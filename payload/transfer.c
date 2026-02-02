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
#include <sys/time.h>
#include <signal.h>
#include <stdint.h>
#include <stdatomic.h>

#include "transfer.h"
#include "config.h"
#include <errno.h>
#include "protocol_defs.h"
#include "notify.h"
#include "lz4.h"
#include "zstd.h"
#include "LzmaLib.h"
#include "config.h"
#include "system_stats.h"
#include "protocol.h"

// Optimized for single-process threaded concurrency
#define PACK_BUFFER_SIZE (48 * 1024 * 1024)
#define PACK_QUEUE_DEPTH 4
#define WRITER_THREAD_COUNT 4
#define FILE_WRITE_QUEUE_DEPTH (16 * 1024)
#define FILE_WRITE_INTERVAL_MS 250
#define UPLOAD_RECV_BUFFER_SIZE (512 * 1024)
#define TUNE_PACK_MIN (4 * 1024 * 1024)

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
    uint64_t offset;
    uint64_t total_size;
    uint8_t flags;
    uint64_t pack_id;
    uint64_t session_id;
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

// V4 record flags
#define RECORD_FLAG_HAS_OFFSET 0x01
#define RECORD_FLAG_HAS_TOTAL  0x02
#define RECORD_FLAG_TRUNCATE   0x04

typedef struct UploadSession {
    struct FrameHeader header;
    size_t header_bytes;
    uint8_t *body;
    uint64_t body_len;
    uint64_t body_bytes;
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
    int ack_enabled;
    int client_sock;
} UploadSession;

typedef struct PackJob {
    uint8_t *data;
    size_t len;
    char dest_root[PATH_MAX];
    uint64_t session_id;
    uint64_t pack_id;
    uint32_t frame_type;
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
static volatile time_t g_upload_last_log = 0;
static volatile unsigned long long g_upload_bytes_recv = 0;
static atomic_ullong g_upload_bytes_written = 0;
static atomic_ullong g_backpressure_events = 0;
static atomic_ullong g_backpressure_wait_ms = 0;
static volatile time_t g_abort_at = 0;
static uint64_t g_last_session_id = 0;
static uint64_t g_abort_session_id = 0;
static char g_abort_reason[128] = {0};

#define MAX_ACK_SESSIONS 4
#define MAX_PACK_ACKS 512
typedef struct {
    uint64_t session_id;
    int sock;
    int active;
} AckSession;

typedef struct {
    uint64_t session_id;
    uint64_t pack_id;
    uint32_t remaining;
    int active;
} PackAckState;

static AckSession g_ack_sessions[MAX_ACK_SESSIONS];
static PackAckState g_pack_acks[MAX_PACK_ACKS];
static pthread_mutex_t g_ack_mutex = PTHREAD_MUTEX_INITIALIZER;

static void register_ack_session(uint64_t session_id, int sock) {
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_ACK_SESSIONS; i++) {
        if (!g_ack_sessions[i].active) {
            g_ack_sessions[i].session_id = session_id;
            g_ack_sessions[i].sock = sock;
            g_ack_sessions[i].active = 1;
            break;
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

static void unregister_ack_session(uint64_t session_id) {
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_ACK_SESSIONS; i++) {
        if (g_ack_sessions[i].active && g_ack_sessions[i].session_id == session_id) {
            g_ack_sessions[i].active = 0;
            g_ack_sessions[i].sock = -1;
            break;
        }
    }
    for (int i = 0; i < MAX_PACK_ACKS; i++) {
        if (g_pack_acks[i].active && g_pack_acks[i].session_id == session_id) {
            g_pack_acks[i].active = 0;
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

static void abort_active_client_sockets(void) {
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_ACK_SESSIONS; i++) {
        if (g_ack_sessions[i].active && g_ack_sessions[i].sock >= 0) {
            shutdown(g_ack_sessions[i].sock, SHUT_RDWR);
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

static void send_pack_ack(uint64_t session_id, uint64_t pack_id) {
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_ACK_SESSIONS; i++) {
        if (g_ack_sessions[i].active && g_ack_sessions[i].session_id == session_id) {
            struct FrameHeader hdr;
            hdr.magic = MAGIC_FTX1;
            hdr.type = FRAME_PACK_ACK;
            hdr.body_len = 8;
            uint8_t payload[8];
            memcpy(payload, &pack_id, 8);
            send(g_ack_sessions[i].sock, &hdr, sizeof(hdr), 0);
            send(g_ack_sessions[i].sock, payload, sizeof(payload), 0);
            break;
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

static void pack_ack_register(uint64_t session_id, uint64_t pack_id, uint32_t remaining) {
    if (pack_id == 0) return;
    if (remaining == 0) {
        send_pack_ack(session_id, pack_id);
        return;
    }
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_PACK_ACKS; i++) {
        if (!g_pack_acks[i].active) {
            g_pack_acks[i].active = 1;
            g_pack_acks[i].session_id = session_id;
            g_pack_acks[i].pack_id = pack_id;
            g_pack_acks[i].remaining = remaining;
            pthread_mutex_unlock(&g_ack_mutex);
            return;
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

static void pack_ack_complete(uint64_t session_id, uint64_t pack_id) {
    if (pack_id == 0) return;
    pthread_mutex_lock(&g_ack_mutex);
    for (int i = 0; i < MAX_PACK_ACKS; i++) {
        if (g_pack_acks[i].active &&
            g_pack_acks[i].session_id == session_id &&
            g_pack_acks[i].pack_id == pack_id) {
            if (g_pack_acks[i].remaining > 0) {
                g_pack_acks[i].remaining -= 1;
            }
            if (g_pack_acks[i].remaining == 0) {
                g_pack_acks[i].active = 0;
                pthread_mutex_unlock(&g_ack_mutex);
                send_pack_ack(session_id, pack_id);
                return;
            }
            break;
        }
    }
    pthread_mutex_unlock(&g_ack_mutex);
}

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
    if (frame_type == FRAME_PACK_LZ4 || frame_type == FRAME_PACK_LZ4_V3 || frame_type == FRAME_PACK_LZ4_V4) {
        int decoded = LZ4_decompress_safe((const char *)(*body) + 4, (char *)out, (int)(*body_len - 4), (int)raw_len);
        ok = (decoded >= 0 && (uint32_t)decoded == raw_len);
    } else if (frame_type == FRAME_PACK_ZSTD || frame_type == FRAME_PACK_ZSTD_V3 || frame_type == FRAME_PACK_ZSTD_V4) {
        size_t decoded = ZSTD_decompress(out, raw_len, *body + 4, *body_len - 4);
        ok = !ZSTD_isError(decoded) && decoded == raw_len;
    } else if (frame_type == FRAME_PACK_LZMA || frame_type == FRAME_PACK_LZMA_V3 || frame_type == FRAME_PACK_LZMA_V4) {
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

static void queue_destroy(PackQueue *q) {
    pthread_cond_destroy(&q->not_full);
    pthread_cond_destroy(&q->not_empty);
    pthread_mutex_destroy(&q->mutex);
}

static int pack_queue_push(PackQueue *q, uint8_t *data, size_t len, const char *dest_root, uint64_t session_id, uint32_t frame_type, int is_close, int chmod_each_file) {
    pthread_mutex_lock(&q->mutex);
    while (atomic_load(&q->count) >= PACK_QUEUE_DEPTH && !q->closed) {
        struct timeval wait_start;
        gettimeofday(&wait_start, NULL);
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_sec += 5;
        if (pthread_cond_timedwait(&q->not_full, &q->mutex, &ts) == ETIMEDOUT) {
             printf("[FTX] backpressure: pack queue full, waiting...\n");
        }
        struct timeval wait_end;
        gettimeofday(&wait_end, NULL);
        long long waited_us =
            (long long)(wait_end.tv_sec - wait_start.tv_sec) * 1000000LL +
            (long long)(wait_end.tv_usec - wait_start.tv_usec);
        if (waited_us > 0) {
            atomic_fetch_add(&g_backpressure_events, 1);
            atomic_fetch_add(&g_backpressure_wait_ms, (unsigned long long)(waited_us / 1000LL));
        }
    }
    if (q->closed) { pthread_mutex_unlock(&q->mutex); return -1; }
    
    PackJob *job = &q->jobs[q->write_idx];
    job->data = data; job->len = len; strncpy(job->dest_root, dest_root, PATH_MAX - 1);
    job->dest_root[PATH_MAX - 1] = '\0';
    job->session_id = session_id;
    job->frame_type = frame_type;
    job->is_close = is_close;
    job->chmod_each_file = (uint8_t)(chmod_each_file ? 1 : 0);
    
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

// Batch size for file writer - heap allocated to avoid stack overflow
#define FILE_WRITER_BATCH_SIZE 2048

static void *file_writer_thread(void *arg) {
    (void)arg;
    char dir_cache[PATH_MAX] = {0};

    // Heap-allocate the batch to avoid stack overflow
    FileWriteJob *local_batch = malloc(FILE_WRITER_BATCH_SIZE * sizeof(FileWriteJob));
    if (!local_batch) {
        printf("[FTX] FATAL: file_writer_thread failed to allocate batch buffer\n");
        return NULL;
    }

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

        size_t to_process = (queue_count < FILE_WRITER_BATCH_SIZE) ? queue_count : FILE_WRITER_BATCH_SIZE;

        for (size_t i = 0; i < to_process; i++) {
            local_batch[i] = g_file_write_queue[g_file_write_queue_head];
            g_file_write_queue_head = (g_file_write_queue_head + 1) % FILE_WRITE_QUEUE_DEPTH;
        }
        atomic_fetch_sub(&g_file_write_queue_count, to_process);
        job_count = to_process;

        pthread_cond_broadcast(&g_file_write_queue_not_full_cond);
        pthread_mutex_unlock(&g_file_write_mutex);

        // Update progress at start of batch
        g_last_transfer_progress = time(NULL);

        for (size_t i = 0; i < job_count; i++) {
            FileWriteJob *job = &local_batch[i];

            char *last_slash = strrchr(job->path, '/');
            if (last_slash) {
                *last_slash = '\0';
                mkdir_recursive(job->path, dir_cache);
                *last_slash = '/';
            }

            int open_flags = O_WRONLY | O_CREAT;
            if ((job->flags & RECORD_FLAG_HAS_OFFSET) == 0 || (job->flags & RECORD_FLAG_TRUNCATE)) {
                open_flags |= O_TRUNC;
            }
            int fd = open(job->path, open_flags, job->chmod_mode);
            if (fd >= 0) {
                if ((job->flags & RECORD_FLAG_HAS_TOTAL) && job->total_size > 0 && job->offset == 0) {
                    if (lseek(fd, (off_t)(job->total_size - 1), SEEK_SET) >= 0) {
                        (void)write(fd, "", 1);
                    }
                    lseek(fd, 0, SEEK_SET);
                }
                if (job->len > 0) {
                    if (job->flags & RECORD_FLAG_HAS_OFFSET) {
                        ssize_t w = pwrite(fd, job->data, job->len, (off_t)job->offset);
                        (void)w;
                    } else {
                        (void)write(fd, job->data, job->len);
                    }
                }
                close(fd);
                if (job->len > 0) {
                    atomic_fetch_add(&g_upload_bytes_written, (unsigned long long)job->len);
                }
            } else {
                printf("[FTX] Writer: Failed to open %s: %s\n", job->path, strerror(errno));
            }
            free(job->data);
            if (job->pack_id != 0) {
                pack_ack_complete(job->session_id, job->pack_id);
            }

            // Update progress periodically during batch
            if ((i & 0xFF) == 0) {
                g_last_transfer_progress = time(NULL);
            }
        }
    }

    free(local_batch);
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

        // Update progress to keep watchdog happy
        g_last_transfer_progress = time(NULL);

        size_t offset = 0;
        uint32_t record_count = 0;
        uint64_t pack_id = 0;
        if (job.frame_type == FRAME_PACK_V3 ||
            job.frame_type == FRAME_PACK_LZ4_V3 ||
            job.frame_type == FRAME_PACK_ZSTD_V3 ||
            job.frame_type == FRAME_PACK_LZMA_V3 ||
            job.frame_type == FRAME_PACK_V4 ||
            job.frame_type == FRAME_PACK_LZ4_V4 ||
            job.frame_type == FRAME_PACK_ZSTD_V4 ||
            job.frame_type == FRAME_PACK_LZMA_V4) {
            if (job.len < 12) { free_pack_buffer(job.data); continue; }
            memcpy(&pack_id, job.data, 8);
            offset += 8;
            memcpy(&record_count, job.data + offset, 4);
            offset += 4;
            pack_ack_register(job.session_id, pack_id, record_count);
        } else {
            if (job.len < 4) { free_pack_buffer(job.data); continue; }
            memcpy(&record_count, job.data, 4);
            offset += 4;
        }

        for (uint32_t i = 0; i < record_count; i++) {
            pthread_mutex_lock(&g_file_write_mutex);
            while(atomic_load(&g_file_write_queue_count) >= FILE_WRITE_QUEUE_DEPTH - 1 && !g_file_writer_shutdown) {
                g_last_transfer_progress = time(NULL);  // Keep watchdog happy during backpressure
                struct timeval wait_start;
                gettimeofday(&wait_start, NULL);
                struct timespec ts;
                clock_gettime(CLOCK_REALTIME, &ts);
                ts.tv_sec += 5;
                pthread_cond_timedwait(&g_file_write_queue_not_full_cond, &g_file_write_mutex, &ts);
                struct timeval wait_end;
                gettimeofday(&wait_end, NULL);
                long long waited_us =
                    (long long)(wait_end.tv_sec - wait_start.tv_sec) * 1000000LL +
                    (long long)(wait_end.tv_usec - wait_start.tv_usec);
                if (waited_us > 0) {
                    atomic_fetch_add(&g_backpressure_events, 1);
                    atomic_fetch_add(&g_backpressure_wait_ms, (unsigned long long)(waited_us / 1000LL));
                }
            }
            pthread_mutex_unlock(&g_file_write_mutex);

            if (g_file_writer_shutdown || g_abort_transfer) break;

            if (job.len - offset < 2) break;
            uint16_t path_len; memcpy(&path_len, job.data + offset, 2); offset += 2;
            uint16_t flags = 0;
            int is_v4 = (job.frame_type == FRAME_PACK_V4 ||
                         job.frame_type == FRAME_PACK_LZ4_V4 ||
                         job.frame_type == FRAME_PACK_ZSTD_V4 ||
                         job.frame_type == FRAME_PACK_LZMA_V4);
            if (is_v4) {
                if (job.len - offset < 2) break;
                memcpy(&flags, job.data + offset, 2);
                offset += 2;
            }
            if (path_len == 0 || path_len >= PATH_MAX || job.len - offset < (size_t)path_len + 8) break;

            FileWriteJob new_job;
            snprintf(new_job.path, sizeof(new_job.path), "%s/%.*s", job.dest_root, path_len, job.data + offset);
            offset += path_len;

            uint64_t data_len; memcpy(&data_len, job.data + offset, 8); offset += 8;
            uint64_t file_offset = 0;
            uint64_t total_size = 0;
            if (is_v4 && (flags & RECORD_FLAG_HAS_OFFSET)) {
                if (job.len - offset < 8) break;
                memcpy(&file_offset, job.data + offset, 8);
                offset += 8;
            }
            if (is_v4 && (flags & RECORD_FLAG_HAS_TOTAL)) {
                if (job.len - offset < 8) break;
                memcpy(&total_size, job.data + offset, 8);
                offset += 8;
            }
            if (data_len > job.len - offset) break;
            if (data_len > SIZE_MAX) break;

            new_job.len = (size_t)data_len;
            new_job.data = malloc(new_job.len > 0 ? new_job.len : 1);
            if (!new_job.data) { offset += (size_t)data_len; continue; }

            if (new_job.len > 0) memcpy(new_job.data, job.data + offset, new_job.len);
            offset += (size_t)data_len;
            new_job.chmod_mode = job.chmod_each_file ? 0777 : 0666;
            new_job.offset = file_offset;
            new_job.total_size = total_size;
            new_job.flags = (uint8_t)flags;
            new_job.pack_id = pack_id;
            new_job.session_id = job.session_id;

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
    if (g_workers_initialized) {
        printf("[FTX] Workers already initialized, skipping init.\n");
        return 0;
    }
    printf("[FTX] Initializing worker threads (count=%d)...\n", WRITER_THREAD_COUNT);

    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        queue_init(&g_queues[i]);
        g_thread_args[i].queue = &g_queues[i];
        g_thread_args[i].index = i;
        int rc = pthread_create(&g_pack_processor_threads[i], NULL, pack_processor_thread, &g_thread_args[i]);
        if (rc != 0) {
            printf("[FTX] FATAL: pthread_create for pack processor %d failed: %d\n", i, rc);
            return -1;
        }
    }
    printf("[FTX] Pack processor threads created.\n");

    g_file_writer_shutdown = 0;
    int rc = pthread_create(&g_file_writer_thread, NULL, file_writer_thread, NULL);
    if (rc != 0) {
        printf("[FTX] FATAL: pthread_create for file writer failed: %d\n", rc);
        return -1;
    }
    printf("[FTX] File writer thread created.\n");

    g_workers_initialized = 1;
    printf("[FTX] Worker pool initialized successfully.\n");
    return 0;
}

static void cleanup_worker_pool(void) {
    if (!g_workers_initialized) return;

    // Signal all threads to shut down first
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pthread_mutex_lock(&g_queues[i].mutex);
        g_queues[i].closed = 1;
        pthread_cond_broadcast(&g_queues[i].not_empty);
        pthread_cond_broadcast(&g_queues[i].not_full);
        pthread_mutex_unlock(&g_queues[i].mutex);
    }

    pthread_mutex_lock(&g_file_write_mutex);
    g_file_writer_shutdown = 1;
    pthread_cond_broadcast(&g_file_write_queue_not_empty_cond);
    pthread_cond_broadcast(&g_file_write_queue_not_full_cond);
    pthread_mutex_unlock(&g_file_write_mutex);

    // Now, join all threads
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pthread_join(g_pack_processor_threads[i], NULL);
    }
    pthread_join(g_file_writer_thread, NULL);

    // Clean up any remaining jobs in file write queue
    for(size_t i = 0; i < atomic_load(&g_file_write_queue_count); ++i) {
        size_t idx = (g_file_write_queue_head + i) % FILE_WRITE_QUEUE_DEPTH;
        free(g_file_write_queue[idx].data);
    }
    atomic_init(&g_file_write_queue_count, 0);
    g_file_write_queue_head = 0;
    g_file_write_queue_tail = 0;

    // Destroy queue synchronization primitives so they can be re-initialized
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        queue_destroy(&g_queues[i]);
    }

    g_workers_initialized = 0;
}

void transfer_cleanup(void) {
    cleanup_worker_pool();
    cleanup_buffer_pool();
}

void transfer_request_abort(void) {
    transfer_request_abort_with_reason("abort_requested");
}

void transfer_request_abort_with_reason(const char *reason) {
    g_abort_transfer = 1;
    g_abort_at = time(NULL);
    g_abort_session_id = g_last_session_id;
    if (reason && reason[0]) {
        snprintf(g_abort_reason, sizeof(g_abort_reason), "%s", reason);
    } else {
        g_abort_reason[0] = '\0';
    }
    printf("[FTX] Abort requested (%s).\n", g_abort_reason[0] ? g_abort_reason : "unknown");
    g_last_transfer_progress = time(NULL);
    abort_active_client_sockets();
    // Don't block here - let the upload loop exit and cleanup naturally
    // Wake up any threads that might be waiting
    if (g_workers_initialized) {
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
}

int transfer_abort_requested(void) { return g_abort_transfer; }
int transfer_is_active(void) { return atomic_load(&g_active_sessions) > 0; }
time_t transfer_last_progress(void) { return g_last_transfer_progress; }

int transfer_get_stats(TransferStats *out) {
    if (!out) return -1;
    memset(out, 0, sizeof(*out));
    out->pack_in_use = atomic_load(&g_pack_in_use);
    out->active_sessions = atomic_load(&g_active_sessions);
    out->queue_count = atomic_load(&g_file_write_queue_count);
    size_t pack_queue = 0;
    for (int i = 0; i < WRITER_THREAD_COUNT; i++) {
        pack_queue += atomic_load(&g_queues[i].count);
    }
    out->pack_queue_count = pack_queue;
    pthread_mutex_lock(&g_pool_mutex);
    out->pool_count = g_pool_count;
    pthread_mutex_unlock(&g_pool_mutex);
    out->last_progress = g_last_transfer_progress;
    out->abort_requested = g_abort_transfer;
    out->workers_initialized = g_workers_initialized;
    out->abort_at = g_abort_at;
    out->abort_session_id = g_abort_session_id;
    snprintf(out->abort_reason, sizeof(out->abort_reason), "%s", g_abort_reason);
    out->backpressure_events = atomic_load(&g_backpressure_events);
    out->backpressure_wait_ms = atomic_load(&g_backpressure_wait_ms);
    out->bytes_received = g_upload_bytes_recv;
    out->bytes_written = atomic_load(&g_upload_bytes_written);

    static unsigned long long last_recv_bytes = 0;
    static unsigned long long last_write_bytes = 0;
    static unsigned long long last_bp_events = 0;
    static unsigned long long last_bp_wait_ms = 0;
    static struct timeval last_tv = {0};
    unsigned long long delta_bp_events = 0;
    unsigned long long delta_bp_wait_ms = 0;
    struct timeval now;
    if (gettimeofday(&now, NULL) == 0) {
        if (last_tv.tv_sec > 0 || last_tv.tv_usec > 0) {
            long long delta_us =
                (long long)(now.tv_sec - last_tv.tv_sec) * 1000000LL +
                (long long)(now.tv_usec - last_tv.tv_usec);
            if (delta_us > 0) {
                unsigned long long delta_recv = out->bytes_received - last_recv_bytes;
                unsigned long long delta_write = out->bytes_written - last_write_bytes;
                out->recv_rate_bps = (uint64_t)((double)delta_recv * 1000000.0 / (double)delta_us);
                out->write_rate_bps = (uint64_t)((double)delta_write * 1000000.0 / (double)delta_us);
            }
        }
        if (out->backpressure_events >= last_bp_events) {
            delta_bp_events = out->backpressure_events - last_bp_events;
        }
        if (out->backpressure_wait_ms >= last_bp_wait_ms) {
            delta_bp_wait_ms = out->backpressure_wait_ms - last_bp_wait_ms;
        }
        last_tv = now;
        last_recv_bytes = out->bytes_received;
        last_write_bytes = out->bytes_written;
        last_bp_events = out->backpressure_events;
        last_bp_wait_ms = out->backpressure_wait_ms;
    }

    int pressure = 0;
    if (out->queue_count >= (size_t)(FILE_WRITE_QUEUE_DEPTH * 0.9)) {
        pressure += 3;
    } else if (out->queue_count >= (size_t)(FILE_WRITE_QUEUE_DEPTH * 0.75)) {
        pressure += 2;
    } else if (out->queue_count > 0) {
        pressure += 1;
    }
    const size_t pack_queue_max = (size_t)WRITER_THREAD_COUNT * (size_t)PACK_QUEUE_DEPTH;
    if (out->pack_queue_count >= pack_queue_max - 1) {
        pressure += 2;
    } else if (out->pack_queue_count > 0) {
        pressure += 1;
    }
    if (out->pack_in_use >= 12) {
        pressure += 1;
    }
    if (delta_bp_events > 0 || delta_bp_wait_ms > 200) {
        pressure += 1;
    }
    if (out->write_rate_bps > 0 && out->recv_rate_bps > out->write_rate_bps * 3 / 2) {
        pressure += 2;
    }

    if (pressure >= 6) out->tune_level = 3;
    else if (pressure >= 4) out->tune_level = 2;
    else if (pressure >= 2) out->tune_level = 1;
    else out->tune_level = 0;

    uint64_t pack_limit = PACK_BUFFER_SIZE;
    if (out->tune_level == 1) {
        pack_limit = (uint64_t)(PACK_BUFFER_SIZE * 3 / 4);
    } else if (out->tune_level == 2) {
        pack_limit = (uint64_t)(PACK_BUFFER_SIZE / 2);
    } else if (out->tune_level >= 3) {
        pack_limit = (uint64_t)(PACK_BUFFER_SIZE / 4);
    }
    if (pack_limit < TUNE_PACK_MIN) pack_limit = TUNE_PACK_MIN;
    out->recommend_pack_limit = pack_limit;
    out->recommend_pace_ms = out->tune_level == 0 ? 0 : out->tune_level == 1 ? 5 : out->tune_level == 2 ? 20 : 80;
    out->recommend_rate_limit_bps = 0;
    if (out->tune_level >= 2 && out->write_rate_bps > 0) {
        uint64_t rec = out->write_rate_bps + (out->write_rate_bps / 5);
        if (out->recv_rate_bps > 0 && rec > out->recv_rate_bps) {
            rec = out->recv_rate_bps;
        }
        out->recommend_rate_limit_bps = rec;
    }

    static int last_tune_level = -1;
    static uint64_t last_rec_pack = 0;
    static uint64_t last_rec_pace = 0;
    static uint64_t last_rec_rate = 0;
    static time_t last_log_at = 0;
    time_t now_sec = time(NULL);
    if ((out->tune_level != last_tune_level ||
         out->recommend_pack_limit != last_rec_pack ||
         out->recommend_pace_ms != last_rec_pace ||
         out->recommend_rate_limit_bps != last_rec_rate) &&
        (last_log_at == 0 || now_sec - last_log_at >= 2)) {
        payload_log("[TUNE] level=%d pack=%llu pace=%llums rate=%lluB/s queue=%zu packq=%zu backpressure+%llu/%llums recv=%lluB/s write=%lluB/s",
                    out->tune_level,
                    (unsigned long long)out->recommend_pack_limit,
                    (unsigned long long)out->recommend_pace_ms,
                    (unsigned long long)out->recommend_rate_limit_bps,
                    out->queue_count,
                    out->pack_queue_count,
                    (unsigned long long)delta_bp_events,
                    (unsigned long long)delta_bp_wait_ms,
                    (unsigned long long)out->recv_rate_bps,
                    (unsigned long long)out->write_rate_bps);
        last_log_at = now_sec;
        last_tune_level = out->tune_level;
        last_rec_pack = out->recommend_pack_limit;
        last_rec_pace = out->recommend_pace_ms;
        last_rec_rate = out->recommend_rate_limit_bps;
    }
    return 0;
}

int transfer_idle_cleanup(void) {
    if (atomic_load(&g_active_sessions) > 0) return 0;

    // If workers not initialized, just cleanup buffer pool
    if (!g_workers_initialized) {
        cleanup_buffer_pool();
        return 1;
    }

    // Wait briefly for queues to drain naturally
    int wait_cycles = 0;
    while ((atomic_load(&g_pack_in_use) > 0 || atomic_load(&g_file_write_queue_count) > 0) && wait_cycles < 40) {
        sleep_ms(250);
        wait_cycles++;
        g_last_transfer_progress = time(NULL);  // Keep watchdog happy
    }

    // Force cleanup regardless of queue state to prevent leaks
    printf("[FTX] Idle cleanup: shutting down worker pool (pack_in_use=%zu, queue=%zu).\n",
           atomic_load(&g_pack_in_use), atomic_load(&g_file_write_queue_count));
    cleanup_worker_pool();
    cleanup_buffer_pool();
    return 1;
}

static int upload_session_start(UploadSession *session, const char *dest_root) {
    printf("[FTX] upload_session_start: dest=%s workers_init=%d\n", dest_root ? dest_root : "(null)", g_workers_initialized);
    if (!session || !dest_root) return -1;
    if (!g_workers_initialized) {
        printf("[FTX] Workers not initialized, calling init_worker_pool...\n");
        if (init_worker_pool() != 0) {
            printf("[FTX] FATAL: Worker pool init failed.\n");
            return -1;
        }
        printf("[FTX] Worker pool initialized.\n");
    }
    memset(session, 0, sizeof(*session));
    g_abort_transfer = 0;
    pthread_mutex_lock(&g_session_counter_lock);
    session->session_id = ++g_session_counter;
    pthread_mutex_unlock(&g_session_counter_lock);
    strncpy(session->final_dest_root, dest_root, PATH_MAX - 1);
    strncpy(session->dest_root, session->final_dest_root, PATH_MAX - 1);
    mkdir_recursive(session->dest_root, NULL);
    return 0;
}

static int enqueue_pack(UploadSession *session) {
    PackQueue *queue = &g_queues[session->session_id % WRITER_THREAD_COUNT];
    int push_result = pack_queue_push(queue, session->body, (size_t)session->body_len, session->dest_root, session->session_id, session->header.type, 0, session->chmod_each_file);
    if (push_result == -1) {
        printf("[FTX] ERROR: pack_queue_push failed (session=%llu len=%llu)\n",
               (unsigned long long)session->session_id, (unsigned long long)session->body_len);
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
            if (session->header.magic != MAGIC_FTX1) { 
                printf("[FTX] Invalid magic: %08x vs %08x\n", session->header.magic, MAGIC_FTX1);
                session->error = 1; 
                break; 
            }
            if (session->header.type == FRAME_FINISH) { if (done) *done = 1; return 0; }
            if ((session->header.type >= FRAME_PACK && session->header.type <= FRAME_PACK_LZMA) ||
                (session->header.type >= FRAME_PACK_V3 && session->header.type <= FRAME_PACK_LZMA_V3) ||
                (session->header.type >= FRAME_PACK_V4 && session->header.type <= FRAME_PACK_LZMA_V4)) {
                if (session->header.body_len > PACK_BUFFER_SIZE) { session->error = 1; break; }
                session->body_len = session->header.body_len;
                if (session->body_len > SIZE_MAX) {
                    printf("[FTX] ERROR: frame body too large (len=%llu)\n", (unsigned long long)session->body_len);
                    session->error = 1;
                    break;
                }
                session->body = alloc_pack_buffer((size_t)session->body_len);
                session->body_bytes = 0;
                if (!session->body) {
                    printf("[FTX] ERROR: alloc_pack_buffer failed (len=%llu)\n", (unsigned long long)session->body_len);
                    session->error = 1;
                    break;
                }
            } else { session->error = 1; break; }
        }
        if (session->body) {
            size_t need = (size_t)(session->body_len - session->body_bytes);
            size_t take = (len - offset) < need ? (len - offset) : need;
            memcpy(session->body + session->body_bytes, data + offset, take);
            session->body_bytes += take;
            offset += take;

            if (session->body_bytes == session->body_len) {
                if (session->header.type != FRAME_PACK &&
                    session->header.type != FRAME_PACK_V3 &&
                    session->header.type != FRAME_PACK_V4) {
                    if (decompress_pack_body(session->header.type, &session->body, &session->body_len) != 0)
                        { printf("[FTX] ERROR: decompress_pack_body failed (type=%u)\n", session->header.type); session->error = 1; break; }
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
    time_t start = time(NULL);
    size_t last_count = 0;
    time_t last_progress = start;

    while(atomic_load(&g_file_write_queue_count) > 0 && !g_abort_transfer) {
        size_t current = atomic_load(&g_file_write_queue_count);
        time_t now = time(NULL);

        if (current != last_count) {
            last_count = current;
            last_progress = now;
            g_last_transfer_progress = now;
        }

        // Timeout if no progress for 60 seconds
        if (now - last_progress > 60) {
            printf("[FTX] Finalizing timeout: no progress for 60s, %zu files remaining.\n", current);
            break;
        }

        if ((now - start) % 5 == 0) {
            printf("[FTX] Finalizing... waiting for %zu files to be written.\n", current);
        }
        sleep_ms(250);
    }
    if (session->body) { free_pack_buffer(session->body); session->body = NULL; }
    return 0;
}

void upload_session_stats(UploadSession *session, int *files, unsigned long long *bytes) {
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
    g_last_session_id = session->session_id;
    return session;
}

void upload_session_destroy(UploadSession *session) {
    if (!session) return;
    if (!session->finalized) {
        upload_session_finalize(session);
    }
    if (session->body) { free_pack_buffer(session->body); }
    if (session->ack_enabled) {
        unregister_ack_session(session->session_id);
    }
    free(session);
    if (atomic_fetch_sub(&g_active_sessions, 1) <= 1) {
        if (g_abort_transfer) g_abort_transfer = 0;
        transfer_idle_cleanup();
    }
}

void handle_upload_v3(int client_sock, const char *dest_root, int use_temp, int chmod_each_file, int chmod_final) {
    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);
    g_upload_bytes_recv = 0;
    atomic_store(&g_upload_bytes_written, 0);
    atomic_store(&g_backpressure_events, 0);
    atomic_store(&g_backpressure_wait_ms, 0);
    g_upload_last_log = time(NULL);
    printf("[FTX] Upload V3 start: dest=%s mode=%s chmod_each=%d chmod_final=%d\n",
           dest_root ? dest_root : "(null)", use_temp ? "TEMP" : "DIRECT",
           chmod_each_file ? 1 : 0, chmod_final ? 1 : 0);
    UploadSession *session = upload_session_create(dest_root, use_temp);
    if (!session) {
        const char *err = "ERROR: Upload init failed\n";
        send(client_sock, err, strlen(err), 0);
        printf("[FTX] ERROR: Upload init failed\n");
        return;
    }
    session->ack_enabled = 1;
    session->client_sock = client_sock;
    register_ack_session(session->session_id, client_sock);
    session->chmod_each_file = chmod_each_file;
    session->chmod_final = chmod_final;

    struct timeval tv;
    tv.tv_sec = RECV_TIMEOUT_SEC;
    tv.tv_usec = 0;
    setsockopt(client_sock, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    uint8_t *buffer = malloc(UPLOAD_RECV_BUFFER_SIZE);
    if (!buffer) {
        upload_session_destroy(session);
        const char *err = "ERROR: Upload buffer failed\n";
        send(client_sock, err, strlen(err), 0);
        return;
    }
    int done = 0, error = 0;
    while (!done && !error && !g_abort_transfer) {
        ssize_t n = recv(client_sock, buffer, UPLOAD_RECV_BUFFER_SIZE, 0);
        if (n < 0) {
            if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) {
                continue;
            }
            printf("[FTX] Upload recv error: %s\n", strerror(errno));
            error = 1;
            break;
        }
        if (n == 0) {
            printf("[FTX] Upload recv: client closed connection.\n");
            error = 1;
            break;
        }
        g_upload_bytes_recv += (unsigned long long)n;
        g_last_transfer_progress = time(NULL);
        upload_session_feed(session, buffer, (size_t)n, &done, &error);
        time_t now = time(NULL);
        if (now - g_upload_last_log >= 5) {
            g_upload_last_log = now;
            printf("[FTX] Upload recv: %llu bytes, queue=%zu, pack_in_use=%zu, workers=%d\n",
                   g_upload_bytes_recv,
                   atomic_load(&g_file_write_queue_count),
                   atomic_load(&g_pack_in_use),
                   g_workers_initialized);
        }
    }
    free(buffer);

    if (g_abort_transfer) {
        const char *err = "ERROR: Upload aborted\n";
        send(client_sock, err, strlen(err), 0);
        upload_session_finalize(session);
        upload_session_destroy(session);
        return;
    }

    if (error && !g_abort_transfer) {
        const char *err = "ERROR: Upload failed on client side\n";
        send(client_sock, err, strlen(err), 0);
        printf("[FTX] ERROR: Upload failed on client side\n");
        upload_session_destroy(session);
        return;
    }

    upload_session_finalize(session);
    upload_session_destroy(session);

    const char *ok = "OK\n";
    send(client_sock, ok, strlen(ok), 0);
}
