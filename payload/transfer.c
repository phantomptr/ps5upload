#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <pthread.h>

#include "protocol_defs.h"
#include "notify.h"

#define PACK_BUFFER_SIZE (128 * 1024 * 1024) // 128MB buffer for packs
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
    while (!q->closed && q->count >= q->max) {
        pthread_cond_wait(&q->not_full, &q->mutex);
    }
    if (q->closed) {
        pthread_mutex_unlock(&q->mutex);
        return -1;
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
                return -1;
            }
            chmod(tmp, 0777);
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
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
    strncpy(session->state.dest_root, dest_root, PATH_MAX - 1);
    session->state.dest_root[PATH_MAX - 1] = '\0';
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

    if (mkdir_recursive(session->state.dest_root, session->state.dir_cache) != 0) {
        session->error = 1;
        return -1;
    }
    return 0;
}

static int enqueue_pack(UploadSession *session, uint8_t *pack_buf, size_t pack_len) {
    PackJob *job = malloc(sizeof(*job));
    if (!job) {
        return -1;
    }

    pthread_mutex_lock(&session->state.mutex);
    uint64_t seq = session->state.enqueue_seq++;
    session->state.pending++;
    pthread_mutex_unlock(&session->state.mutex);

    job->data = pack_buf;
    job->len = pack_len;
    job->state = &session->state;
    job->seq = seq;

    if (queue_push(&g_queue, job) != 0) {
        free(job);
        pthread_mutex_lock(&session->state.mutex);
        if (session->state.pending > 0) {
            session->state.pending--;
        }
        pthread_mutex_unlock(&session->state.mutex);
        return -1;
    }
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

    size_t offset = 0;
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
                session->error = 1;
            } else if (session->header.type == FRAME_FINISH) {
                if (done) {
                    *done = 1;
                }
            } else if (session->header.type == FRAME_PACK) {
                if (session->header.body_len > PACK_BUFFER_SIZE) {
                    session->error = 1;
                } else {
                    session->body_len = session->header.body_len;
                    session->body = malloc(session->body_len);
                    session->body_bytes = 0;
                    if (!session->body) {
                        session->error = 1;
                    }
                }
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
                if (enqueue_pack(session, session->body, session->body_len) != 0) {
                    session->error = 1;
                    if (error) {
                        *error = 1;
                    }
                    return 0;
                }
                session->body = NULL;
                session->body_len = 0;
                session->body_bytes = 0;
                session->header_bytes = 0;
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

static void upload_session_finish(UploadSession *session) {
    if (!session) {
        return;
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

UploadSession *upload_session_create(const char *dest_root) {
    UploadSession *session = malloc(sizeof(*session));
    if (!session) {
        return NULL;
    }
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
    upload_session_finish(session);
    free(session);
}

void handle_upload_v2(int client_sock, const char *dest_root) {
    printf("[FTX] Starting V2 Upload to %s\n", dest_root);

    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);

    UploadSession *session = upload_session_create(dest_root);
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
