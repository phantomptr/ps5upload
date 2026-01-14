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

#include "protocol_defs.h"
#include "notify.h"
#include "lz4.h"

// Optimized for single-process threaded concurrency
#define PACK_BUFFER_SIZE (16 * 1024 * 1024)   // 16MB buffer (Increased for throughput)
#define PACK_QUEUE_DEPTH 4                    // 4 * 16MB = 64MB heap usage

#define POOL_SIZE 16
static uint8_t *g_buffer_pool[POOL_SIZE];
static int g_pool_count = 0;
static pthread_mutex_t g_pool_mutex = PTHREAD_MUTEX_INITIALIZER;

static uint8_t *alloc_pack_buffer(size_t size) {
    if (size > PACK_BUFFER_SIZE) {
        return malloc(size);
    }
    
    pthread_mutex_lock(&g_pool_mutex);
    if (g_pool_count > 0) {
        uint8_t *ptr = g_buffer_pool[--g_pool_count];
        pthread_mutex_unlock(&g_pool_mutex);
        return ptr;
    }
    pthread_mutex_unlock(&g_pool_mutex);
    
    void *ptr = NULL;
    if (posix_memalign(&ptr, 4096, PACK_BUFFER_SIZE) != 0) {
        return NULL; // Fallback to NULL if OOM
    }
    return ptr;
}

static void free_pack_buffer(uint8_t *ptr) {
    if (!ptr) return;
    
    pthread_mutex_lock(&g_pool_mutex);
    if (g_pool_count < POOL_SIZE) {
        g_buffer_pool[g_pool_count++] = ptr;
        pthread_mutex_unlock(&g_pool_mutex);
        return;
    }
    pthread_mutex_unlock(&g_pool_mutex);
    
    free(ptr);
}

static void cleanup_buffer_pool(void) {
    pthread_mutex_lock(&g_pool_mutex);
    while (g_pool_count > 0) {
        free(g_buffer_pool[--g_pool_count]);
    }
    pthread_mutex_unlock(&g_pool_mutex);
}

#ifndef O_DIRECT
#define O_DIRECT 0
#endif

typedef struct ConnState {
    char dest_root[PATH_MAX];
    char dir_cache[PATH_MAX];
    int current_fd;
    char current_path[PATH_MAX];
    char current_full_path[PATH_MAX];
    long long total_bytes;
    int total_files;
} ConnState;

typedef struct UploadSession {
    ConnState state;
    struct FrameHeader header;
    size_t header_bytes;
    uint8_t *body;
    size_t body_len;
    size_t body_bytes;
    int error;
    int finalized;
    int use_temp;
    char final_dest_root[PATH_MAX];
    char temp_root[PATH_MAX];
    char session_root[PATH_MAX];
    uint64_t bytes_received;
    uint64_t packs_enqueued;
    uint64_t last_log_bytes;
    int enqueue_pending;
    uint64_t last_backpressure_log;
    uint64_t session_id;
} UploadSession;

typedef struct PackJob {
    uint8_t *data; // Owned pointer
    size_t len;
    char dest_root[PATH_MAX];
    uint64_t session_id;
    int is_close;
} PackJob;

typedef struct PackQueue {
    PackJob jobs[PACK_QUEUE_DEPTH];
    size_t read_idx;
    size_t write_idx;
    size_t count;
    int closed;
    pthread_mutex_t mutex;
    pthread_cond_t not_empty;
    pthread_cond_t not_full;
} PackQueue;

static PackQueue g_queue;
static pthread_t g_writer_thread;
static int g_workers_initialized = 0;
static long long g_total_bytes = 0;
static int g_total_files = 0;
static uint64_t g_session_counter = 0;
static pthread_mutex_t g_session_counter_lock = PTHREAD_MUTEX_INITIALIZER;

typedef struct {
    pthread_mutex_t mutex;
    uint64_t bytes;
    uint64_t last_ms;
} GlobalLogState;

static GlobalLogState g_log_state;

typedef struct SessionWriterState {
    uint64_t session_id;
    char current_path[PATH_MAX];
    char current_full_path[PATH_MAX];
    char dir_cache[PATH_MAX];
    char last_dest_root[PATH_MAX];
    int current_fd;
    struct SessionWriterState *next;
} SessionWriterState;

static SessionWriterState *g_writer_states = NULL;
static pthread_mutex_t g_writer_state_mutex = PTHREAD_MUTEX_INITIALIZER;

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000ULL);
}

static void queue_init(PackQueue *q) {
    memset(q, 0, sizeof(*q));
    q->read_idx = 0;
    q->write_idx = 0;
    q->count = 0;
    q->closed = 0;
    pthread_mutex_init(&q->mutex, NULL);
    pthread_cond_init(&q->not_empty, NULL);
    pthread_cond_init(&q->not_full, NULL);
}

static int queue_push(PackQueue *q, uint8_t *data, size_t len, const char *dest_root, uint64_t session_id, int is_close) {
    pthread_mutex_lock(&q->mutex);
    while (q->count >= PACK_QUEUE_DEPTH && !q->closed) {
        pthread_cond_wait(&q->not_full, &q->mutex);
    }
    if (q->closed) {
        pthread_mutex_unlock(&q->mutex);
        return -1;
    }

    PackJob *job = &q->jobs[q->write_idx];
    job->data = data;
    job->len = len;
    strncpy(job->dest_root, dest_root, PATH_MAX - 1);
    job->dest_root[PATH_MAX - 1] = '\0';
    job->session_id = session_id;
    job->is_close = is_close;

    q->write_idx = (q->write_idx + 1) % PACK_QUEUE_DEPTH;
    q->count++;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->mutex);
    return 0;
}

static int queue_pop(PackQueue *q, PackJob *out) {
    pthread_mutex_lock(&q->mutex);
    while (q->count == 0 && !q->closed) {
        pthread_cond_wait(&q->not_empty, &q->mutex);
    }
    if (q->count == 0 && q->closed) {
        pthread_mutex_unlock(&q->mutex);
        return 0;
    }

    *out = q->jobs[q->read_idx];
    
    q->read_idx = (q->read_idx + 1) % PACK_QUEUE_DEPTH;
    q->count--;
    pthread_cond_signal(&q->not_full);
    pthread_mutex_unlock(&q->mutex);
    return 1;
}

// Helper to create directories recursively with caching
static int mkdir_recursive(const char *path, char *cache) {
    if (cache && strcmp(path, cache) == 0) {
        return 0; // Already created this directory
    }

    char tmp[PATH_MAX];
    char *p = NULL;
    size_t len;

    snprintf(tmp, sizeof(tmp), "%s", path);
    len = strlen(tmp);
    if (tmp[len - 1] == '/') {
        tmp[len - 1] = 0;
    }

    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
                printf("[FTX] mkdir failed: %s (%s)\n", tmp, strerror(errno));
                return -1;
            }
            chmod(tmp, 0777);
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
        printf("[FTX] mkdir failed: %s (%s)\n", tmp, strerror(errno));
        return -1;
    }
    chmod(tmp, 0777);

    if (cache) {
        strncpy(cache, path, PATH_MAX - 1);
        cache[PATH_MAX - 1] = '\0';
    }
    return 0;
}

static void close_current_file(ConnState *state) {
    if (state->current_fd < 0) {
        return;
    }
#ifdef POSIX_FADV_DONTNEED
    posix_fadvise(state->current_fd, 0, 0, POSIX_FADV_DONTNEED);
#endif
    close(state->current_fd);
    chmod(state->current_full_path, 0777);
    state->current_fd = -1;
    state->current_path[0] = '\0';
    state->current_full_path[0] = '\0';
}

static int path_is_dir(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) {
        return 0;
    }
    return S_ISDIR(st.st_mode);
}

static int remove_tree(const char *path) {
    DIR *dir = opendir(path);
    if (!dir) {
        if (errno == ENOENT) {
            return 0;
        }
        return -1;
    }

    struct dirent *entry;
    char child[PATH_MAX];
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        snprintf(child, sizeof(child), "%s/%s", path, entry->d_name);

        struct stat st;
        if (stat(child, &st) != 0) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (remove_tree(child) != 0) {
                closedir(dir);
                return -1;
            }
        } else {
            if (unlink(child) != 0) {
                closedir(dir);
                return -1;
            }
        }
    }
    closedir(dir);
    return rmdir(path);
}

static int copy_file(const char *src, const char *dst) {
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0) {
        return -1;
    }
    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0777);
    if (out_fd < 0) {
        close(in_fd);
        return -1;
    }

    char *buf = malloc(1024 * 1024);
    if (!buf) {
        close(in_fd);
        close(out_fd);
        return -1;
    }

    int result = 0;
    for (;;) {
        ssize_t n = read(in_fd, buf, 1024 * 1024);
        if (n == 0) {
            break;
        }
        if (n < 0) {
            result = -1;
            break;
        }
        ssize_t written = 0;
        while (written < n) {
            ssize_t w = write(out_fd, buf + written, n - written);
            if (w < 0) {
                result = -1;
                break;
            }
            written += w;
        }
        if (result != 0) {
            break;
        }
    }

    free(buf);
    close(in_fd);
    close(out_fd);
    return result;
}

static int copy_tree(const char *src, const char *dst) {
    DIR *dir = opendir(src);
    if (!dir) {
        return -1;
    }
    if (mkdir(dst, 0777) != 0 && errno != EEXIST) {
        closedir(dir);
        return -1;
    }
    chmod(dst, 0777);

    struct dirent *entry;
    char src_path[PATH_MAX];
    char dst_path[PATH_MAX];
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        snprintf(src_path, sizeof(src_path), "%s/%s", src, entry->d_name);
        snprintf(dst_path, sizeof(dst_path), "%s/%s", dst, entry->d_name);

        struct stat st;
        if (stat(src_path, &st) != 0) {
            closedir(dir);
            return -1;
        }
        if (S_ISDIR(st.st_mode)) {
            if (copy_tree(src_path, dst_path) != 0) {
                closedir(dir);
                return -1;
            }
        } else {
            if (copy_file(src_path, dst_path) != 0) {
                closedir(dir);
                return -1;
            }
            chmod(dst_path, 0777);
        }
    }
    closedir(dir);
    return 0;
}

static int move_tree(const char *src, const char *dst) {
    if (rename(src, dst) == 0) {
        return 0;
    }
    if (errno != EXDEV) {
        return -1;
    }
    if (copy_tree(src, dst) != 0) {
        return -1;
    }
    if (remove_tree(src) != 0) {
        return -1;
    }
    return 0;
}

static int ensure_parent_dir(const char *path) {
    char parent[PATH_MAX];
    strncpy(parent, path, sizeof(parent) - 1);
    parent[sizeof(parent) - 1] = '\0';
    char *slash = strrchr(parent, '/');
    if (!slash || slash == parent) {
        return 0;
    }
    *slash = '\0';
    return mkdir_recursive(parent, NULL);
}

static SessionWriterState *get_writer_state(uint64_t session_id, const char *dest_root) {
    pthread_mutex_lock(&g_writer_state_mutex);
    SessionWriterState *state = g_writer_states;
    while (state) {
        if (state->session_id == session_id) {
            if (strcmp(dest_root, state->last_dest_root) != 0) {
                if (state->current_fd >= 0) {
                    close(state->current_fd);
                    chmod(state->current_full_path, 0777);
                    state->current_fd = -1;
                }
                state->current_path[0] = '\0';
                state->current_full_path[0] = '\0';
                state->dir_cache[0] = '\0';
                strncpy(state->last_dest_root, dest_root, PATH_MAX - 1);
                state->last_dest_root[PATH_MAX - 1] = '\0';
            }
            pthread_mutex_unlock(&g_writer_state_mutex);
            return state;
        }
        state = state->next;
    }

    state = malloc(sizeof(*state));
    if (!state) {
        pthread_mutex_unlock(&g_writer_state_mutex);
        return NULL;
    }
    memset(state, 0, sizeof(*state));
    state->session_id = session_id;
    state->current_fd = -1;
    strncpy(state->last_dest_root, dest_root, PATH_MAX - 1);
    state->last_dest_root[PATH_MAX - 1] = '\0';
    state->next = g_writer_states;
    g_writer_states = state;
    pthread_mutex_unlock(&g_writer_state_mutex);
    return state;
}

static void release_writer_state(uint64_t session_id) {
    pthread_mutex_lock(&g_writer_state_mutex);
    SessionWriterState **prev = &g_writer_states;
    SessionWriterState *state = g_writer_states;
    while (state) {
        if (state->session_id == session_id) {
            if (state->current_fd >= 0) {
                close(state->current_fd);
                chmod(state->current_full_path, 0777);
                state->current_fd = -1;
            }
            *prev = state->next;
            free(state);
            pthread_mutex_unlock(&g_writer_state_mutex);
            return;
        }
        prev = &state->next;
        state = state->next;
    }
    pthread_mutex_unlock(&g_writer_state_mutex);
}

// Clean up all orphaned writer states - call periodically or on memory pressure
static void cleanup_all_writer_states(void) {
    pthread_mutex_lock(&g_writer_state_mutex);
    SessionWriterState *state = g_writer_states;
    while (state) {
        SessionWriterState *next = state->next;
        if (state->current_fd >= 0) {
            close(state->current_fd);
            chmod(state->current_full_path, 0777);
        }
        free(state);
        state = next;
    }
    g_writer_states = NULL;
    pthread_mutex_unlock(&g_writer_state_mutex);
}

static void write_pack_data(uint64_t session_id, const char *dest_root, const uint8_t *pack_buf, size_t pack_len, int is_close) {
    if (is_close) {
        release_writer_state(session_id);
        return;
    }
    SessionWriterState *state = get_writer_state(session_id, dest_root);
    if (!state) {
        return;
    }
    size_t offset = 0;
    uint32_t record_count = 0;

    if (pack_len < 4) {
        return;
    }
    memcpy(&record_count, pack_buf, 4);
    offset += 4;

    for (uint32_t i = 0; i < record_count; i++) {
        if (offset + 2 > pack_len) break;

        uint16_t path_len;
        memcpy(&path_len, pack_buf + offset, 2);
        offset += 2;

        if (offset + path_len + 8 > pack_len) break;

        char rel_path[PATH_MAX];
        memcpy(rel_path, pack_buf + offset, path_len);
        rel_path[path_len] = '\0';
        offset += path_len;

        uint64_t data_len;
        memcpy(&data_len, pack_buf + offset, 8);
        offset += 8;

        if (offset + data_len > pack_len) break;

        char full_path[PATH_MAX];
        snprintf(full_path, sizeof(full_path), "%s/%s", dest_root, rel_path);

        char *last_slash = strrchr(full_path, '/');
        if (last_slash) {
            *last_slash = '\0';
            mkdir_recursive(full_path, state->dir_cache);
            *last_slash = '/';
        }

        if (strncmp(rel_path, state->current_path, PATH_MAX) != 0) {
            if (state->current_fd >= 0) {
                close(state->current_fd);
                chmod(state->current_full_path, 0777);
                state->current_fd = -1;
            }
            strncpy(state->current_path, rel_path, PATH_MAX - 1);
            state->current_path[PATH_MAX - 1] = '\0';
            strncpy(state->current_full_path, full_path, PATH_MAX - 1);
            state->current_full_path[PATH_MAX - 1] = '\0';

            state->current_fd = open(full_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
            if (state->current_fd < 0) {
                printf("[FTX] Writer: Failed to open %s: %s\n", full_path, strerror(errno));
            } else {
                g_total_files++;
            }
        } else if (state->current_fd < 0) {
            state->current_fd = open(full_path, O_WRONLY | O_APPEND, 0666);
            if (state->current_fd < 0) {
                printf("[FTX] Writer: Failed to reopen %s: %s\n", full_path, strerror(errno));
            }
        }

        if (state->current_fd >= 0) {
            ssize_t written = write(state->current_fd, pack_buf + offset, data_len);
            if (written > 0) {
                g_total_bytes += written;
            }
            
            pthread_mutex_lock(&g_log_state.mutex);
            g_log_state.bytes += written;
            uint64_t now = now_ms();
            if (g_log_state.last_ms == 0) {
                g_log_state.last_ms = now;
                g_log_state.bytes = 0;
            } else if (now - g_log_state.last_ms >= 2000) {
                double seconds = (now - g_log_state.last_ms) / 1000.0;
                double mbps = (g_log_state.bytes / (1024.0 * 1024.0)) / seconds;
                printf("[FTX] disk write rate: %.2f MB/s\n", mbps);
                g_log_state.last_ms = now;
                g_log_state.bytes = 0;
            }
            pthread_mutex_unlock(&g_log_state.mutex);
        }

        offset += data_len;
    }
}

static void *disk_writer_thread(void *arg) {
    (void)arg;
    printf("[FTX] Disk writer thread started\n");
    
    // Init log state
    pthread_mutex_init(&g_log_state.mutex, NULL);
    g_log_state.bytes = 0;
    g_log_state.last_ms = 0;

    PackJob job;
    while (queue_pop(&g_queue, &job)) {
        write_pack_data(job.session_id, job.dest_root, job.data, job.len, job.is_close);
        
        // Ownership was transferred to us, so we free it
        if (job.data) {
            free_pack_buffer(job.data);
        }
    }
    
    printf("[FTX] Disk writer thread stopped\n");
    return NULL;
}

static void init_worker_pool(void) {
    if (g_workers_initialized) {
        return;
    }

    printf("[FTX] Initializing writer thread...\n");
    queue_init(&g_queue);

    if (pthread_create(&g_writer_thread, NULL, disk_writer_thread, NULL) != 0) {
        printf("[FTX] Failed to create writer thread: %s\n", strerror(errno));
        exit(1);
    }
    
    g_workers_initialized = 1;
}

static void cleanup_worker_pool(void) {
    if (!g_workers_initialized) {
        return;
    }

    pthread_mutex_lock(&g_queue.mutex);
    g_queue.closed = 1;
    pthread_cond_broadcast(&g_queue.not_empty);
    pthread_mutex_unlock(&g_queue.mutex);

    pthread_join(g_writer_thread, NULL);
    g_workers_initialized = 0;
}

void transfer_cleanup(void) {
    cleanup_worker_pool();
    cleanup_all_writer_states();
    cleanup_buffer_pool();
    // Reset global counters
    g_total_bytes = 0;
    g_total_files = 0;
}

static int upload_session_start(UploadSession *session, const char *dest_root) {
    if (!session || !dest_root) {
        return -1;
    }

    if (!g_workers_initialized) {
        init_worker_pool();
    }

    // Reset global counters for this session to avoid accumulation across transfers
    g_total_bytes = 0;
    g_total_files = 0;

    memset(session, 0, sizeof(*session));
    pthread_mutex_lock(&g_session_counter_lock);
    session->session_id = ++g_session_counter;
    pthread_mutex_unlock(&g_session_counter_lock);
    strncpy(session->final_dest_root, dest_root, PATH_MAX - 1);
    session->final_dest_root[PATH_MAX - 1] = '\0';
    if (session->use_temp) {
        const char *base = "/data";
        if (path_is_dir("/mnt/usb0")) {
            base = "/mnt/usb0";
        } else if (path_is_dir("/mnt/ext1")) {
            base = "/mnt/ext1";
        }

        snprintf(session->temp_root, sizeof(session->temp_root), "%s/ps5upload/temp", base);
        if (mkdir_recursive(session->temp_root, NULL) != 0) {
            printf("[FTX] temp mkdir failed: %s (%s)\n", session->temp_root, strerror(errno));
            session->error = 1;
            return -1;
        }

        static pthread_mutex_t counter_lock = PTHREAD_MUTEX_INITIALIZER;
        static unsigned counter = 0;
        pthread_mutex_lock(&counter_lock);
        unsigned local = ++counter;
        pthread_mutex_unlock(&counter_lock);

        snprintf(session->session_root, sizeof(session->session_root), "%s/session_%d_%u",
                 session->temp_root, (int)getpid(), local);
        if (mkdir_recursive(session->session_root, NULL) != 0) {
            printf("[FTX] session mkdir failed: %s (%s)\n", session->session_root, strerror(errno));
            session->error = 1;
            return -1;
        }

        strncpy(session->state.dest_root, session->session_root, PATH_MAX - 1);
        session->state.dest_root[PATH_MAX - 1] = '\0';
    } else {
        strncpy(session->state.dest_root, session->final_dest_root, PATH_MAX - 1);
        session->state.dest_root[PATH_MAX - 1] = '\0';
        if (mkdir_recursive(session->state.dest_root, NULL) != 0) {
            printf("[FTX] dest mkdir failed: %s (%s)\n", session->state.dest_root, strerror(errno));
            session->error = 1;
            return -1;
        }
    }
    session->state.dir_cache[0] = '\0';
    session->state.current_fd = -1;
    session->state.current_path[0] = '\0';
    session->state.current_full_path[0] = '\0';

    session->header_bytes = 0;
    session->body = NULL;
    session->body_len = 0;
    session->body_bytes = 0;
    session->error = 0;

    return 0;
}

static int enqueue_pack(UploadSession *session) {
    // Transfer ownership of session->body to queue
    int push_result = queue_push(&g_queue, session->body, session->body_len, session->state.dest_root, session->session_id, 0);
    if (push_result == -1) {
        printf("[FTX] enqueue failed (queue closed)\n");
        // Queue rejected, we still own the buffer - free it to avoid leak
        free_pack_buffer(session->body);
        session->body = NULL;
        return -1;
    }

    session->body = NULL; // Ownership transferred
    session->body_len = 0;
    session->body_bytes = 0;
    session->header_bytes = 0;
    session->enqueue_pending = 0;
    session->packs_enqueued++;
    return 0;
}

int upload_session_feed(UploadSession *session, const uint8_t *data, size_t len, int *done, int *error) {
    if (!session) {
        return -1;
    }

    if (done) {
        *done = 0;
    }
    if (error) {
        *error = 0;
    }

    size_t offset = 0;
    session->bytes_received += len;
    if (session->bytes_received - session->last_log_bytes >= (512ULL * 1024 * 1024)) {
        printf("[FTX] recv %.2f GB, packs %llu\n",
               session->bytes_received / (1024.0 * 1024.0 * 1024.0),
               (unsigned long long)session->packs_enqueued);
        session->last_log_bytes = session->bytes_received;
    }
    while (offset < len) {
        if (session->header_bytes < sizeof(struct FrameHeader)) {
            size_t need = sizeof(struct FrameHeader) - session->header_bytes;
            size_t take = (len - offset) < need ? (len - offset) : need;
            memcpy(((uint8_t *)&session->header) + session->header_bytes, data + offset, take);
            session->header_bytes += take;
            offset += take;

            if (session->header_bytes < sizeof(struct FrameHeader)) {
                continue;
            }

            if (session->header.magic != MAGIC_FTX1) {
                printf("[FTX] invalid magic: %08x\n", session->header.magic);
                session->error = 1;
            } else if (session->header.type == FRAME_FINISH) {
                if (done) {
                    *done = 1;
                }
            } else if (session->header.type == FRAME_PACK || session->header.type == FRAME_PACK_LZ4) {
                if (session->header.body_len > PACK_BUFFER_SIZE) {
                    printf("[FTX] pack too large: %llu\n",
                           (unsigned long long)session->header.body_len);
                    session->error = 1;
                } else {
                    session->body_len = session->header.body_len;
                    session->body = alloc_pack_buffer(session->body_len);
                    
                    session->body_bytes = 0;
                    if (!session->body) {
                        printf("[FTX] OOM allocating pack buffer (%llu)\n",
                               (unsigned long long)session->body_len);
                        session->error = 1;
                    }
                }
            } else {
                printf("[FTX] unknown frame type: %u\n", session->header.type);
                session->error = 1;
            }

            if (session->error) {
                if (error) {
                    *error = 1;
                }
                return 0;
            }

            if (session->header.type == FRAME_FINISH) {
                session->header_bytes = 0;
                if (done) {
                    *done = 1;
                }
                return 0;
            }
        }

        if (session->body) {
            size_t need = session->body_len - session->body_bytes;
            size_t take = (len - offset) < need ? (len - offset) : need;
            memcpy(session->body + session->body_bytes, data + offset, take);
            session->body_bytes += take;
            offset += take;

            if (session->body_bytes == session->body_len) {
                if (session->header.type == FRAME_PACK_LZ4) {
                    if (session->body_len < 4) {
                        free_pack_buffer(session->body);
                        session->body = NULL;
                        session->error = 1;
                        if (error) {
                            *error = 1;
                        }
                        return 0;
                    }
                    uint32_t raw_len = 0;
                    memcpy(&raw_len, session->body, 4);
                    if (raw_len > PACK_BUFFER_SIZE) {
                        free_pack_buffer(session->body);
                        session->body = NULL;
                        session->error = 1;
                        if (error) {
                            *error = 1;
                        }
                        return 0;
                    }
                    void *out = alloc_pack_buffer(raw_len);
                    if (!out) {
                        free_pack_buffer(session->body);
                        session->body = NULL;
                        session->error = 1;
                        if (error) {
                            *error = 1;
                        }
                        return 0;
                    }
                    int decoded = LZ4_decompress_safe((const char *)session->body + 4, (char *)out,
                                                     (int)session->body_len - 4, (int)raw_len);
                    if (decoded < 0 || (uint32_t)decoded != raw_len) {
                        free_pack_buffer(out);
                        free_pack_buffer(session->body);
                        session->body = NULL;
                        session->error = 1;
                        if (error) {
                            *error = 1;
                        }
                        return 0;
                    }
                    free_pack_buffer(session->body);
                    session->body = out;
                    session->body_len = raw_len;
                    session->body_bytes = raw_len;
                }

                int res = enqueue_pack(session);
                if (res != 0) {
                    session->error = 1;
                    if (error) {
                        *error = 1;
                    }
                    return 0;
                }
            }
        } else if (session->header_bytes == sizeof(struct FrameHeader)) {
            session->header_bytes = 0;
        }
    }

    if (session->error && error) {
        *error = 1;
    }
    return 0;
}

int upload_session_backpressure(UploadSession *session) {
    if (!session) {
        return 0;
    }
    return session->enqueue_pending ? 1 : 0;
}

static int upload_session_finish(UploadSession *session) {
    if (!session) {
        return -1;
    }

    if (queue_push(&g_queue, NULL, 0, session->state.dest_root, session->session_id, 1) == -1) {
        return -1;
    }

    pthread_mutex_lock(&g_queue.mutex);
    while (g_queue.count > 0 && !g_queue.closed) {
        pthread_cond_wait(&g_queue.not_full, &g_queue.mutex);
    }
    pthread_mutex_unlock(&g_queue.mutex);

    close_current_file(&session->state);

    session->state.total_bytes = g_total_bytes;
    session->state.total_files = g_total_files;

    if (session->body) {
        free_pack_buffer(session->body);
        session->body = NULL;
    }
    return 0;
}

void upload_session_stats(UploadSession *session, int *files, long long *bytes) {
    if (!session) {
        return;
    }
    if (files) {
        *files = session->state.total_files;
    }
    if (bytes) {
        *bytes = session->state.total_bytes;
    }
}

int upload_session_finalize(UploadSession *session) {
    if (!session) {
        return -1;
    }
    if (session->finalized) {
        return session->error ? -1 : 0;
    }

    printf("[FTX] finalize start (temp=%d)\n", session->use_temp);
    if (upload_session_finish(session) != 0) {
        session->error = 1;
    }

    if (!session->error && session->use_temp) {
        if (path_is_dir(session->final_dest_root)) {
            if (remove_tree(session->final_dest_root) != 0) {
                printf("[FTX] Failed to remove existing dest: %s (%s)\n",
                       session->final_dest_root, strerror(errno));
                session->error = 1;
            }
        }

        if (!session->error && ensure_parent_dir(session->final_dest_root) != 0) {
            printf("[FTX] Failed to create dest parent: %s (%s)\n",
                   session->final_dest_root, strerror(errno));
            session->error = 1;
        }

        if (!session->error && move_tree(session->session_root, session->final_dest_root) != 0) {
            printf("[FTX] Failed to move temp to dest: %s -> %s (%s)\n",
                   session->session_root, session->final_dest_root, strerror(errno));
            session->error = 1;
        }

        if (session->error) {
            remove_tree(session->session_root);
        }

        (void)rmdir(session->temp_root);
        char temp_parent[PATH_MAX];
        snprintf(temp_parent, sizeof(temp_parent), "%s/ps5upload", (path_is_dir("/mnt/usb0") ? "/mnt/usb0" :
            (path_is_dir("/mnt/ext1") ? "/mnt/ext1" : "/data")));
        (void)rmdir(temp_parent);
    }

    printf("[FTX] finalize done (ok=%d)\n", session->error ? 0 : 1);
    session->finalized = 1;
    return session->error ? -1 : 0;
}

UploadSession *upload_session_create(const char *dest_root, int use_temp) {
    UploadSession *session = malloc(sizeof(*session));
    if (!session) {
        return NULL;
    }
    session->use_temp = use_temp ? 1 : 0;
    if (upload_session_start(session, dest_root) != 0) {
        free(session);
        return NULL;
    }
    return session;
}

void upload_session_destroy(UploadSession *session) {
    if (!session) {
        return;
    }
    if (!session->finalized) {
        upload_session_finalize(session);
    }
    // Ensure writer state is released even if session wasn't properly finalized
    release_writer_state(session->session_id);
    // Free any remaining body buffer
    if (session->body) {
        free_pack_buffer(session->body);
        session->body = NULL;
    }
    free(session);
}

void handle_upload_v2(int client_sock, const char *dest_root) {
    printf("[FTX] Starting V2 Upload to %s\n", dest_root);

    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);

    UploadSession *session = upload_session_create(dest_root, 1);
    if (!session) {
        const char *err = "ERROR: Upload init failed\n";
        send(client_sock, err, strlen(err), 0);
        return;
    }

    uint8_t buffer[64 * 1024];
    int done = 0;
    int error = 0;

    while (!done && !error) {
        ssize_t n = recv(client_sock, buffer, sizeof(buffer), 0);
        if (n <= 0) {
            error = 1;
            break;
        }
        upload_session_feed(session, buffer, (size_t)n, &done, &error);
    }

    if (error) {
        upload_session_destroy(session);
        const char *err = "ERROR: Upload failed\n";
        send(client_sock, err, strlen(err), 0);
        notify_error("PS5 Upload", "Upload failed");
        return;
    }

    int files = 0;
    long long bytes = 0;
    upload_session_stats(session, &files, &bytes);
    upload_session_destroy(session);

    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %lld\n", files, bytes);
    send(client_sock, response, strlen(response), 0);

    char msg[128];
    snprintf(msg, sizeof(msg), "Transfer complete: %d files", files);
    notify_success("PS5 Upload", msg);
}
