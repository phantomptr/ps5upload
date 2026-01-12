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

#include "protocol_defs.h"
#include "notify.h"

#define PACK_BUFFER_SIZE (64 * 1024 * 1024) // 64MB buffer for packs
#define PACK_QUEUE_DEPTH 4
#define DISK_WORKER_COUNT 4

typedef struct ConnState {
    char dest_root[PATH_MAX];
    char dir_cache[PATH_MAX];
    FILE *current_fp;
    char current_path[PATH_MAX];
    char current_full_path[PATH_MAX];
    long long total_bytes;
    int total_files;
    uint64_t next_seq;
    uint64_t enqueue_seq;
    uint64_t pending;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
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
} UploadSession;

typedef struct PackJob {
    uint8_t *data;
    size_t len;
    ConnState *state;
    uint64_t seq;
    struct PackJob *next;
} PackJob;

typedef struct PackQueue {
    PackJob *head;
    PackJob *tail;
    size_t count;
    size_t max;
    int closed;
    pthread_mutex_t mutex;
    pthread_cond_t not_empty;
    pthread_cond_t not_full;
} PackQueue;

static PackQueue g_queue;
static pthread_t g_workers[DISK_WORKER_COUNT];
static pthread_once_t g_workers_once = PTHREAD_ONCE_INIT;

static void queue_init(PackQueue *q, size_t max) {
    memset(q, 0, sizeof(*q));
    q->max = max;
    pthread_mutex_init(&q->mutex, NULL);
    pthread_cond_init(&q->not_empty, NULL);
    pthread_cond_init(&q->not_full, NULL);
}

static int queue_push(PackQueue *q, PackJob *job) {
    pthread_mutex_lock(&q->mutex);
    if (q->closed) {
        pthread_mutex_unlock(&q->mutex);
        return -1;
    }
    if (q->count >= q->max) {
        pthread_mutex_unlock(&q->mutex);
        return 1;
    }
    job->next = NULL;
    if (!q->tail) {
        q->head = job;
        q->tail = job;
    } else {
        q->tail->next = job;
        q->tail = job;
    }
    q->count++;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->mutex);
    return 0;
}

static int queue_is_full(PackQueue *q) {
    int full = 0;
    pthread_mutex_lock(&q->mutex);
    full = (q->count >= q->max);
    pthread_mutex_unlock(&q->mutex);
    return full;
}

static PackJob *queue_pop(PackQueue *q) {
    pthread_mutex_lock(&q->mutex);
    while (!q->closed && q->count == 0) {
        pthread_cond_wait(&q->not_empty, &q->mutex);
    }
    if (q->count == 0 && q->closed) {
        pthread_mutex_unlock(&q->mutex);
        return NULL;
    }
    PackJob *job = q->head;
    q->head = job->next;
    if (!q->head) {
        q->tail = NULL;
    }
    q->count--;
    pthread_cond_signal(&q->not_full);
    pthread_mutex_unlock(&q->mutex);
    return job;
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
    if (!state->current_fp) {
        return;
    }
    fclose(state->current_fp);
    chmod(state->current_full_path, 0777);
    state->current_fp = NULL;
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

static void write_pack_locked(ConnState *state, const uint8_t *pack_buf, size_t pack_len) {
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
        snprintf(full_path, sizeof(full_path), "%s/%s", state->dest_root, rel_path);

        char *last_slash = strrchr(full_path, '/');
        if (last_slash) {
            *last_slash = '\0';
            mkdir_recursive(full_path, state->dir_cache);
            *last_slash = '/';
        }

        if (strncmp(rel_path, state->current_path, PATH_MAX) != 0) {
            close_current_file(state);
            strncpy(state->current_path, rel_path, PATH_MAX - 1);
            state->current_path[PATH_MAX - 1] = '\0';
            strncpy(state->current_full_path, full_path, PATH_MAX - 1);
            state->current_full_path[PATH_MAX - 1] = '\0';

            state->current_fp = fopen(full_path, "wb");
            if (!state->current_fp) {
                printf("[FTX] Failed to open %s: %s\n", full_path, strerror(errno));
            } else {
                state->total_files++;
            }
        } else if (!state->current_fp) {
            state->current_fp = fopen(full_path, "ab");
            if (!state->current_fp) {
                printf("[FTX] Failed to reopen %s: %s\n", full_path, strerror(errno));
            }
        }

        if (state->current_fp) {
            fwrite(pack_buf + offset, 1, data_len, state->current_fp);
            state->total_bytes += data_len;
        }

        offset += data_len;
    }
}

static void *disk_worker_main(void *arg) {
    (void)arg;
    for (;;) {
        PackJob *job = queue_pop(&g_queue);
        if (!job) {
            break;
        }

        ConnState *state = job->state;
        pthread_mutex_lock(&state->mutex);
        while (job->seq != state->next_seq) {
            pthread_cond_wait(&state->cond, &state->mutex);
        }

        write_pack_locked(state, job->data, job->len);
        state->next_seq++;
        if (state->pending > 0) {
            state->pending--;
        }
        pthread_cond_broadcast(&state->cond);
        pthread_mutex_unlock(&state->mutex);

        free(job->data);
        free(job);
    }
    return NULL;
}

static void init_worker_pool(void) {
    queue_init(&g_queue, PACK_QUEUE_DEPTH);
    for (int i = 0; i < DISK_WORKER_COUNT; i++) {
        pthread_create(&g_workers[i], NULL, disk_worker_main, NULL);
    }
}

static int upload_session_start(UploadSession *session, const char *dest_root) {
    if (!session || !dest_root) {
        return -1;
    }

    pthread_once(&g_workers_once, init_worker_pool);

    memset(session, 0, sizeof(*session));
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
    session->state.current_fp = NULL;
    session->state.current_path[0] = '\0';
    session->state.current_full_path[0] = '\0';
    session->state.next_seq = 0;
    session->state.enqueue_seq = 0;
    session->state.pending = 0;
    pthread_mutex_init(&session->state.mutex, NULL);
    pthread_cond_init(&session->state.cond, NULL);

    session->header_bytes = 0;
    session->body = NULL;
    session->body_len = 0;
    session->body_bytes = 0;
    session->error = 0;

    return 0;
}

static int enqueue_pack(UploadSession *session) {
    if (queue_is_full(&g_queue)) {
        return 1;
    }

    PackJob *job = malloc(sizeof(*job));
    if (!job) {
        return -1;
    }

    pthread_mutex_lock(&session->state.mutex);
    uint64_t seq = session->state.enqueue_seq++;
    session->state.pending++;
    pthread_mutex_unlock(&session->state.mutex);

    job->data = session->body;
    job->len = session->body_len;
    job->state = &session->state;
    job->seq = seq;

    int push_result = queue_push(&g_queue, job);
    if (push_result == 1) {
        free(job);
        pthread_mutex_lock(&session->state.mutex);
        if (session->state.pending > 0) {
            session->state.pending--;
        }
        pthread_mutex_unlock(&session->state.mutex);
        return 1;
    }
    if (push_result != 0) {
        printf("[FTX] enqueue failed (queue closed)\n");
        free(job);
        pthread_mutex_lock(&session->state.mutex);
        if (session->state.pending > 0) {
            session->state.pending--;
        }
        pthread_mutex_unlock(&session->state.mutex);
        return -1;
    }
    session->body = NULL;
    session->body_len = 0;
    session->body_bytes = 0;
    session->header_bytes = 0;
    session->enqueue_pending = 0;
    session->packs_enqueued++;
    return 0;
}

int upload_session_feed(UploadSession *session, const uint8_t *data, size_t len, int *done, int *error) {
    if (!session || !data) {
        return -1;
    }

    if (done) {
        *done = 0;
    }
    if (error) {
        *error = 0;
    }

    if (session->enqueue_pending) {
        int res = enqueue_pack(session);
        if (res == 1) {
            if (session->bytes_received - session->last_backpressure_log >= (256ULL * 1024 * 1024)) {
                printf("[FTX] backpressure: queue full\n");
                session->last_backpressure_log = session->bytes_received;
            }
            return 0;
        }
        if (res != 0) {
            session->error = 1;
            if (error) {
                *error = 1;
            }
            return 0;
        }
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
            } else if (session->header.type == FRAME_PACK) {
                if (session->header.body_len > PACK_BUFFER_SIZE) {
                    printf("[FTX] pack too large: %llu\n",
                           (unsigned long long)session->header.body_len);
                    session->error = 1;
                } else {
                    session->body_len = session->header.body_len;
                    session->body = malloc(session->body_len);
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
                int res = enqueue_pack(session);
                if (res == 1) {
                    session->enqueue_pending = 1;
                    return 0;
                }
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

    pthread_mutex_lock(&session->state.mutex);
    while (session->state.pending > 0) {
        pthread_cond_wait(&session->state.cond, &session->state.mutex);
    }
    close_current_file(&session->state);
    pthread_mutex_unlock(&session->state.mutex);

    pthread_mutex_destroy(&session->state.mutex);
    pthread_cond_destroy(&session->state.cond);

    if (session->body) {
        free(session->body);
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
