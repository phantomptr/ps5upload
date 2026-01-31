#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <errno.h>
#include <dirent.h>
#include <limits.h>
#include <fcntl.h>
#include <pthread.h>
#include <time.h>
#include <signal.h>
#include <stdint.h>
#include <stdatomic.h>

#include "transfer.h"
#include "protocol_defs.h"
#include "notify.h"
#include "lz4.h"
#include "zstd.h"
#include "LzmaLib.h"
#include "config.h"
#include "system_stats.h"

// Optimized for single-process threaded concurrency
#define PACK_BUFFER_SIZE (48 * 1024 * 1024)
#define PACK_QUEUE_DEPTH 4
#define WRITER_THREAD_COUNT 4
#define FILE_WRITE_QUEUE_DEPTH (16 * 1024)
#define FILE_WRITE_BATCH_SIZE (4 * 1024)
#define FILE_WRITE_INTERVAL_MS 250
#define UPLOAD_V2_RECV_BUFFER_SIZE (512 * 1024)

// Buffer pool for large packs
#define POOL_SIZE 16
static uint8_t *g_buffer_pool[POOL_SIZE];
static int g_pool_count = 0;
static pthread_mutex_t g_pool_mutex = PTHREAD_MUTEX_INITIALIZER;
static atomic_size_t g_pack_in_use = 0;
static atomic_int g_active_sessions = 0;

// New structure for individual file write operations
typedef struct {
    char path[PATH_MAX];
    uint8_t *data; // Malloc'd data for the file
    size_t len;
    int chmod_mode; // Mode to apply after write
} FileWriteJob;

// New queue for batching small file writes
static FileWriteJob g_file_write_queue[FILE_WRITE_QUEUE_DEPTH];
static size_t g_file_write_queue_head = 0;
static size_t g_file_write_queue_tail = 0;
static atomic_size_t g_file_write_queue_count = 0;
static pthread_mutex_t g_file_write_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_file_write_queue_not_empty_cond = PTHREAD_COND_INITIALIZER;
static pthread_cond_t g_file_write_queue_not_full_cond = PTHREAD_COND_INITIALIZER;
static int g_file_writer_shutdown = 0;

typedef struct UploadSession {
    struct FrameHeader header;
    size_t header_bytes;
    uint8_t *body;
    size_t body_len;
    size_t body_bytes;
    int error;
    int finalized;
    int use_temp;
    char final_dest_root[PATH_MAX];
    char dest_root[PATH_MAX];
    uint64_t bytes_received;
    uint64_t packs_enqueued;
    uint64_t session_id;
    int chmod_each_file;
    int chmod_final;
} UploadSession;

typedef struct PackJob {
    uint8_t *data;
    size_t len;
    char dest_root[PATH_MAX];
    uint64_t session_id;
    int is_close;
    uint8_t chmod_each_file;
} PackJob;

typedef struct PackQueue {
    PackJob jobs[PACK_QUEUE_DEPTH];
    size_t read_idx;
    size_t write_idx;
    atomic_size_t count;
    int closed;
    pthread_mutex_t mutex;
    pthread_cond_t not_empty;
    pthread_cond_t not_full;
} PackQueue;

static PackQueue g_queues[WRITER_THREAD_COUNT];
static pthread_t g_pack_processor_threads[WRITER_THREAD_COUNT];
static pthread_t g_file_writer_thread;

typedef struct { PackQueue *queue; int index; } ThreadArgs;
static ThreadArgs g_thread_args[WRITER_THREAD_COUNT];

static int g_workers_initialized = 0;
static uint64_t g_session_counter = 0;
static pthread_mutex_t g_session_counter_lock = PTHREAD_MUTEX_INITIALIZER;
static volatile sig_atomic_t g_abort_transfer = 0;
static volatile time_t g_last_transfer_progress = 0;

static uint8_t *alloc_pack_buffer(size_t size) {
    if (size > PACK_BUFFER_SIZE) {
        uint8_t *ptr = malloc(size);
        if (ptr) atomic_fetch_add(&g_pack_in_use, 1);
        return ptr;
    }
    pthread_mutex_lock(&g_pool_mutex);
    if (g_pool_count > 0) {
        uint8_t *ptr = g_buffer_pool[--g_pool_count];
        pthread_mutex_unlock(&g_pool_mutex);
        atomic_fetch_add(&g_pack_in_use, 1);
        return ptr;
    }
    pthread_mutex_unlock(&g_pool_mutex);
    void *ptr = NULL;
    if (posix_memalign(&ptr, 4096, PACK_BUFFER_SIZE) != 0) return NULL;
    atomic_fetch_add(&g_pack_in_use, 1);
    return ptr;
}

static void free_pack_buffer(uint8_t *ptr) {
    if (!ptr) return;
    pthread_mutex_lock(&g_pool_mutex);
    if (g_pool_count < POOL_SIZE) {
        g_buffer_pool[g_pool_count++] = ptr;
        pthread_mutex_unlock(&g_pool_mutex);
        atomic_fetch_sub(&g_pack_in_use, 1);
        return;
    }
    pthread_mutex_unlock(&g_pool_mutex);
    free(ptr);
    atomic_fetch_sub(&g_pack_in_use, 1);
}

static int decompress_pack_body(uint32_t frame_type, uint8_t **body, size_t *body_len) {
    if (!body || !*body || !body_len || *body_len < 4) return -1;
    uint32_t raw_len = 0;
    memcpy(&raw_len, *body, 4);
    if (raw_len > PACK_BUFFER_SIZE) return -1;
    uint8_t *out = alloc_pack_buffer(raw_len);
    if (!out) return -1;
    int ok = 0;
    if (frame_type == FRAME_PACK_LZ4) {
        int decoded = LZ4_decompress_safe((const char *)(*body) + 4, (char *)out, (int)(*body_len - 4), (int)raw_len);
        ok = (decoded >= 0 && (uint32_t)decoded == raw_len);
    } else if (frame_type == FRAME_PACK_ZSTD) {
        size_t decoded = ZSTD_decompress(out, raw_len, *body + 4, *body_len - 4);
        ok = !ZSTD_isError(decoded) && decoded == raw_len;
    } else if (frame_type == FRAME_PACK_LZMA) {
        if (*body_len < 17) { free_pack_buffer(out); return -1; }
        SizeT dest_len = (SizeT)raw_len;
        SizeT src_len = (SizeT)(*body_len - 17);
        int res = LzmaUncompress(out, &dest_len, *body + 17, &src_len, *body + 4, LZMA_PROPS_SIZE);
        ok = (res == SZ_OK && dest_len == raw_len);
    }
    if (!ok) { free_pack_buffer(out); return -1; }
    free_pack_buffer(*body);
    *body = out;
    *body_len = raw_len;
    return 0;
}

static void cleanup_buffer_pool(void) {
    pthread_mutex_lock(&g_pool_mutex);
    while (g_pool_count > 0) {
        free(g_buffer_pool[--g_pool_count]);
    }
    pthread_mutex_unlock(&g_pool_mutex);
}

static void sleep_ms(unsigned ms) {
    struct timespec ts = { .tv_sec = ms / 1000, .tv_nsec = (long)(ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

static void queue_init(PackQueue *q) {
    memset(q, 0, sizeof(*q));
    atomic_init(&q->count, 0);
    pthread_mutex_init(&q->mutex, NULL);
    pthread_cond_init(&q->not_empty, NULL);
    pthread_cond_init(&q->not_full, NULL);
}

static int pack_queue_push(PackQueue *q, uint8_t *data, size_t len, const char *dest_root, uint64_t session_id, int is_close, int chmod_each_file) {
    pthread_mutex_lock(&q->mutex);
    while (atomic_load(&q->count) >= PACK_QUEUE_DEPTH && !q->closed) {
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_sec += 5;
        if (pthread_cond_timedwait(&q->not_full, &q->mutex, &ts) == ETIMEDOUT) {
             printf("[FTX] backpressure: pack queue full, waiting...\n");
        }
    }
    if (q->closed) { pthread_mutex_unlock(&q->mutex); return -1; }
    
    PackJob *job = &q->jobs[q->write_idx];
    job->data = data; job->len = len; strncpy(job->dest_root, dest_root, PATH_MAX - 1);
    job->dest_root[PATH_MAX - 1] = '\0';
    job->session_id = session_id; job->is_close = is_close; job->chmod_each_file = (uint8_t)(chmod_each_file ? 1 : 0);
    
    q->write_idx = (q->write_idx + 1) % PACK_QUEUE_DEPTH;
    atomic_fetch_add(&q->count, 1);
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->mutex);
    return 0;
}

static int pack_queue_pop(PackQueue *q, PackJob *out) {
    pthread_mutex_lock(&q->mutex);
    while (atomic_load(&q->count) == 0 && !q->closed) {
        pthread_cond_wait(&q->not_empty, &q->mutex);
    }
    if (atomic_load(&q->count) == 0 && q->closed) { pthread_mutex_unlock(&q->mutex); return 0; }
    *out = q->jobs[q->read_idx];
    q->read_idx = (q->read_idx + 1) % PACK_QUEUE_DEPTH;
    atomic_fetch_sub(&q->count, 1);
    pthread_cond_signal(&q->not_full);
    pthread_mutex_unlock(&q->mutex);
    return 1;
}

static int mkdir_recursive(const char *path, char *cache) {
    if (cache && strcmp(path, cache) == 0) return 0;
    char tmp[PATH_MAX]; snprintf(tmp, sizeof(tmp), "%s", path);
    size_t len = strlen(tmp);
    if (tmp[len - 1] == '/') tmp[len - 1] = 0;
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') { *p = 0; if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1; chmod(tmp, 0777); *p = '/'; }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
    chmod(tmp, 0777);
    if (cache) { strncpy(cache, path, PATH_MAX - 1); cache[PATH_MAX-1] = '\0'; }
    return 0;
}

static void *file_writer_thread(void *arg) {
    FileWriteJob local_batch[FILE_WRITE_BATCH_SIZE];
    char dir_cache[PATH_MAX] = {0};
    
    printf("[FTX] Batched file writer thread started.\n");

    while (!g_file_writer_shutdown) {
        size_t job_count = 0;
        pthread_mutex_lock(&g_file_write_mutex);
        
        if (atomic_load(&g_file_write_queue_count) == 0 && !g_file_writer_shutdown) {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += FILE_WRITE_INTERVAL_MS * 1000000L;
            if (ts.tv_nsec >= 1000000000L) {
                ts.tv_sec += 1;
                ts.tv_nsec -= 1000000000L;
            }
            pthread_cond_timedwait(&g_file_write_queue_not_empty_cond, &g_file_write_mutex, &ts);
        }
        
        size_t queue_count = atomic_load(&g_file_write_queue_count);
        if (queue_count == 0) {
            pthread_mutex_unlock(&g_file_write_mutex);
            continue;
        }

        size_t to_process = (queue_count < FILE_WRITE_BATCH_SIZE) ? queue_count : FILE_WRITE_BATCH_SIZE;

        for (size_t i = 0; i < to_process; i++) {
            local_batch[i] = g_file_write_queue[g_file_write_queue_head];
            g_file_write_queue_head = (g_file_write_queue_head + 1) % FILE_WRITE_QUEUE_DEPTH;
        }
        atomic_fetch_sub(&g_file_write_queue_count, to_process);
        job_count = to_process;
        
        pthread_cond_broadcast(&g_file_write_queue_not_full_cond);
        pthread_mutex_unlock(&g_file_write_mutex);

        for (size_t i = 0; i < job_count; i++) {
            FileWriteJob *job = &local_batch[i];
            
            char *last_slash = strrchr(job->path, '/');
            if (last_slash) {
                *last_slash = '\0';
                mkdir_recursive(job->path, dir_cache);
                *last_slash = '/';
            }

            int fd = open(job->path, O_WRONLY | O_CREAT | O_TRUNC, job->chmod_mode);
            if (fd >= 0) {
                if (job->len > 0) write(fd, job->data, job->len);
                close(fd);
            } else {
                 printf("[FTX] Writer: Failed to open %s: %s\n", job->path, strerror(errno));
            }
            free(job->data);
        }
    }
    printf("[FTX] Batched file writer thread stopped.\n");
    return NULL;
}

static void *pack_processor_thread(void *arg) {
    ThreadArgs *thread_args = (ThreadArgs *)arg;
    PackQueue *queue = thread_args->queue;
    
    printf("[FTX] Pack processor thread started (%d)\n", thread_args->index);
    PackJob job;
    while (pack_queue_pop(queue, &job)) {
        if (job.is_close || !job.data) {
            if (job.data) free_pack_buffer(job.data);
            continue;
        }

        size_t offset = 0;
        uint32_t record_count = 0;
        if (job.len < 4) { free_pack_buffer(job.data); continue; }
        memcpy(&record_count, job.data, 4);
        offset += 4;

        for (uint32_t i = 0; i < record_count; i++) {
            pthread_mutex_lock(&g_file_write_mutex);
            while(atomic_load(&g_file_write_queue_count) >= FILE_WRITE_QUEUE_DEPTH - 1 && !g_file_writer_shutdown) {
                printf("[FTX] File write queue is full! Pausing pack processing.\n");
                pthread_cond_wait(&g_file_write_queue_not_full_cond, &g_file_write_mutex);
            }
            pthread_mutex_unlock(&g_file_write_mutex);

            if (g_file_writer_shutdown || g_abort_transfer) break;
            
            if (job.len - offset < 2) break;
            uint16_t path_len; memcpy(&path_len, job.data + offset, 2); offset += 2;
            if (path_len == 0 || path_len >= PATH_MAX || job.len - offset < (size_t)path_len + 8) break;
            
            FileWriteJob new_job;
            snprintf(new_job.path, sizeof(new_job.path), "%s/%.*s", job.dest_root, path_len, job.data + offset);
            offset += path_len;

            uint64_t data_len; memcpy(&data_len, job.data + offset, 8); offset += 8;
            if (data_len > job.len - offset) break;

            new_job.len = (size_t)data_len;
            new_job.data = malloc(new_job.len > 0 ? new_job.len : 1);
            if (!new_job.data) { offset += (size_t)data_len; continue; }

            if (new_job.len > 0) memcpy(new_job.data, job.data + offset, new_job.len);
            offset += (size_t)data_len;
            new_job.chmod_mode = job.chmod_each_file ? 0777 : 0666;

            pthread_mutex_lock(&g_file_write_mutex);
            g_file_write_queue[g_file_write_queue_tail] = new_job;
            g_file_write_queue_tail = (g_file_write_queue_tail + 1) % FILE_WRITE_QUEUE_DEPTH;
            atomic_fetch_add(&g_file_write_queue_count, 1);
            pthread_cond_signal(&g_file_write_queue_not_empty_cond);
            pthread_mutex_unlock(&g_file_write_mutex);
        }
        free_pack_buffer(job.data);
    }
    printf("[FTX] Pack processor thread stopped (%d)\n", thread_args->index);
    return NULL;
}

static int init_worker_pool(void) {
    if (g_workers_initialized) return 0;
    printf("[FTX] Initializing worker threads...\n");
    
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        queue_init(&g_queues[i]);
        g_thread_args[i].queue = &g_queues[i];
        g_thread_args[i].index = i;
        if (pthread_create(&g_pack_processor_threads[i], NULL, pack_processor_thread, &g_thread_args[i]) != 0) return -1;
    }
    
    g_file_writer_shutdown = 0;
    if (pthread_create(&g_file_writer_thread, NULL, file_writer_thread, NULL) != 0) return -1;
    
    g_workers_initialized = 1;
    return 0;
}

static void cleanup_worker_pool(void) {
    if (!g_workers_initialized) return;

    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pthread_mutex_lock(&g_queues[i].mutex);
        g_queues[i].closed = 1;
        pthread_cond_broadcast(&g_queues[i].not_empty);
        pthread_cond_broadcast(&g_queues[i].not_full);
        pthread_mutex_unlock(&g_queues[i].mutex);
    }
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pthread_join(g_pack_processor_threads[i], NULL);
    }
    
    pthread_mutex_lock(&g_file_write_mutex);
    g_file_writer_shutdown = 1;
    pthread_cond_broadcast(&g_file_write_queue_not_empty_cond);
    pthread_cond_broadcast(&g_file_write_queue_not_full_cond);
    pthread_mutex_unlock(&g_file_write_mutex);
    pthread_join(g_file_writer_thread, NULL);

    for(size_t i = 0; i < atomic_load(&g_file_write_queue_count); ++i) {
        size_t idx = (g_file_write_queue_head + i) % FILE_WRITE_QUEUE_DEPTH;
        free(g_file_write_queue[idx].data);
    }
    atomic_init(&g_file_write_queue_count, 0);

    g_workers_initialized = 0;
}

void transfer_cleanup(void) {
    cleanup_worker_pool();
    cleanup_buffer_pool();
}

void transfer_request_abort(void) { 
    g_abort_transfer = 1; 
    // Wake up any waiting threads so they can terminate
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pthread_mutex_lock(&g_queues[i].mutex);
        pthread_cond_broadcast(&g_queues[i].not_empty);
        pthread_cond_broadcast(&g_queues[i].not_full);
        pthread_mutex_unlock(&g_queues[i].mutex);
    }
    pthread_mutex_lock(&g_file_write_mutex);
    pthread_cond_broadcast(&g_file_write_queue_not_empty_cond);
    pthread_cond_broadcast(&g_file_write_queue_not_full_cond);
    pthread_mutex_unlock(&g_file_write_mutex);
}
int transfer_abort_requested(void) { return g_abort_transfer; }
int transfer_is_active(void) { return atomic_load(&g_active_sessions) > 0; }
time_t transfer_last_progress(void) { return g_last_transfer_progress; }
int transfer_get_stats(TransferStats *out) { return 0; }
int transfer_idle_cleanup(void) { return 0; }

static int upload_session_start(UploadSession *session, const char *dest_root) {
    if (!session || !dest_root) return -1;
    if (!g_workers_initialized) {
        if (init_worker_pool() != 0) return -1;
    }
    memset(session, 0, sizeof(*session));
    g_abort_transfer = 0;
    pthread_mutex_lock(&g_session_counter_lock);
    session->session_id = ++g_session_counter;
    pthread_mutex_unlock(&g_session_counter_lock);
    strncpy(session->final_dest_root, dest_root, PATH_MAX - 1);
    
    // Simplified: always write directly to the final destination for now.
    // The batching writer provides enough of a buffer that temp folders may not be needed.
    strncpy(session->dest_root, session->final_dest_root, PATH_MAX - 1);
    mkdir_recursive(session->dest_root, NULL);
    return 0;
}

static int enqueue_pack(UploadSession *session) {
    PackQueue *queue = &g_queues[session->session_id % WRITER_THREAD_COUNT];
    int push_result = pack_queue_push(queue, session->body, session->body_len, session->dest_root, session->session_id, 0, session->chmod_each_file);
    if (push_result == -1) {
        free_pack_buffer(session->body);
        session->body = NULL;
        return -1;
    }
    session->body = NULL;
    session->packs_enqueued++;
    return 0;
}

int upload_session_feed(UploadSession *session, const uint8_t *data, size_t len, int *done, int *error) {
    if (!session || g_abort_transfer) { if (error) *error = 1; return 0; }
    size_t offset = 0;
    if (len > 0) g_last_transfer_progress = time(NULL);

    while (offset < len) {
        if (session->header_bytes < sizeof(struct FrameHeader)) {
            size_t need = sizeof(struct FrameHeader) - session->header_bytes;
            size_t take = (len - offset) < need ? (len - offset) : need;
            memcpy(((uint8_t *)&session->header) + session->header_bytes, data + offset, take);
            session->header_bytes += take;
            offset += take;
            if (session->header_bytes < sizeof(struct FrameHeader)) continue;
            if (session->header.magic != MAGIC_FTX1) { session->error = 1; break; }
            if (session->header.type == FRAME_FINISH) { if (done) *done = 1; return 0; }
            if (session->header.type >= FRAME_PACK && session->header.type <= FRAME_PACK_LZMA) {
                if (session->header.body_len > PACK_BUFFER_SIZE) { session->error = 1; break; }
                session->body_len = session->header.body_len;
                session->body = alloc_pack_buffer(session->body_len);
                session->body_bytes = 0;
                if (!session->body) { session->error = 1; break; }
            } else { session->error = 1; break; }
        }
        if (session->body) {
            size_t need = session->body_len - session->body_bytes;
            size_t take = (len - offset) < need ? (len - offset) : need;
            memcpy(session->body + session->body_bytes, data + offset, take);
            session->body_bytes += take;
            offset += take;

            if (session->body_bytes == session->body_len) {
                if (session->header.type != FRAME_PACK) {
                    if (decompress_pack_body(session->header.type, &session->body, &session->body_len) != 0)
                        { session->error = 1; break; }
                }
                if (enqueue_pack(session) != 0) { session->error = 1; break; }
                session->header_bytes = 0;
            }
        } else {
             session->header_bytes = 0;
        }
    }
    if (session->error && error) *error = 1;
    return 0;
}

int upload_session_backpressure(UploadSession *session) {
    if (!session) return 0;
    return atomic_load(&g_file_write_queue_count) >= FILE_WRITE_QUEUE_DEPTH - 1;
}

static int upload_session_finish(UploadSession *session) {
    if (!session) return -1;
    while(atomic_load(&g_file_write_queue_count) > 0 && !g_abort_transfer) {
        printf("[FTX] Finalizing... waiting for %zu files to be written.\n", atomic_load(&g_file_write_queue_count));
        sleep_ms(250);
    }
    if (session->body) { free_pack_buffer(session->body); session->body = NULL; }
    return 0;
}

void upload_session_stats(UploadSession *session, int *files, long long *bytes) {
    if (files) *files = 0;
    if (bytes) *bytes = 0;
}

int upload_session_finalize(UploadSession *session) {
    if (!session || session->finalized) return session->error ? -1 : 0;
    if (upload_session_finish(session) != 0) session->error = 1;
    printf("[FTX] finalize done (ok=%d)\n", session->error ? 0 : 1);
    session->finalized = 1;
    return session->error ? -1 : 0;
}

UploadSession *upload_session_create(const char *dest_root, int use_temp) {
    UploadSession *session = malloc(sizeof(*session));
    if (!session) return NULL;
    atomic_fetch_add(&g_active_sessions, 1);
    g_last_transfer_progress = time(NULL);
    session->use_temp = use_temp;
    if (upload_session_start(session, dest_root) != 0) {
        atomic_fetch_sub(&g_active_sessions, 1);
        free(session);
        return NULL;
    }
    return session;
}

void upload_session_destroy(UploadSession *session) {
    if (!session) return;
    if (!session->finalized) {
        upload_session_finalize(session);
    }
    if (session->body) { free_pack_buffer(session->body); }
    free(session);
    if (atomic_fetch_sub(&g_active_sessions, 1) <= 1 && g_abort_transfer) {
        g_abort_transfer = 0;
    }
    transfer_idle_cleanup();
}

void handle_upload_v2(int client_sock, const char *dest_root, int use_temp, int chmod_each_file, int chmod_final) {
    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);
    UploadSession *session = upload_session_create(dest_root, use_temp);
    if (!session) { 
        const char *err = "ERROR: Upload init failed\n";
        send(client_sock, err, strlen(err), 0);
        return; 
    }
    session->chmod_each_file = chmod_each_file;
    session->chmod_final = chmod_final;
    uint8_t *buffer = malloc(UPLOAD_V2_RECV_BUFFER_SIZE);
    if (!buffer) {
        upload_session_destroy(session);
        const char *err = "ERROR: Upload buffer failed\n";
        send(client_sock, err, strlen(err), 0);
        return;
    }
    int done = 0, error = 0;
    while (!done && !error) {
        ssize_t n = recv(client_sock, buffer, UPLOAD_V2_RECV_BUFFER_SIZE, 0);
        if (n <= 0) { error = 1; break; }
        upload_session_feed(session, buffer, (size_t)n, &done, &error);
    }
    free(buffer);
    
    if (error && !g_abort_transfer) {
        const char *err = "ERROR: Upload failed on client side\n";
        send(client_sock, err, strlen(err), 0);
    } else {
        const char *ok = "SUCCESS\n";
        send(client_sock, ok, strlen(ok), 0);
    }
    upload_session_destroy(session);
}
