/* See include/webui_queue.h. The state is console_queue.c's; this file starts and watches jobs.
 * No Sony API is called here: the routes it goes through do their own locking. */
#include "webui_queue.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "atomic_file.h"
#include "console_queue.h"
#include "proc_identity.h"
#include "webui_api.h"
#include "webui_codec.h"
#include "webui_pkg.h"
#include "webui_server.h"

#define TICK_MS 1000
/* Another install on the console (or a conversion): tried again this much later. */
#define BUSY_RETRY_MS 15000
#define REPLY_MAX (16u * 1024u)
#define FILE_MAX (256u * 1024u)
#define STAGE_PREFIX "queue-"

static pthread_once_t g_once = PTHREAD_ONCE_INIT;
static uint32_t g_cancel_sent; /* the item whose job was already asked to stop */

static uint64_t mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void sleep_ms(unsigned ms) {
    struct timespec ts = { (time_t)(ms / 1000u), (long)(ms % 1000u) * 1000000L };
    while (nanosleep(&ts, &ts) != 0) {
    }
}

static void save(void) {
    char *buf;
    int n;
    if (!cq_dirty()) return;
    buf = malloc(FILE_MAX);
    if (!buf) return;
    n = cq_serialize(buf, FILE_MAX);
    if (n >= 0 && atomic_file_write(CQ_FILE, CQ_FILE ".tmp", buf, (size_t)n, 0644) == 0) cq_saved();
    else fprintf(stderr, "queue: could not save %s\n", CQ_FILE);
    free(buf);
}

static void load(void) {
    FILE *f = fopen(CQ_FILE, "rb");
    char *buf;
    size_t n;
    int count;
    if (!f) return;
    buf = malloc(FILE_MAX);
    if (!buf) {
        fclose(f);
        return;
    }
    n = fread(buf, 1, FILE_MAX - 1, f);
    fclose(f);
    buf[n] = '\0';
    count = cq_deserialize(buf, n, (int64_t)time(NULL));
    free(buf);
    if (count < 0) fprintf(stderr, "queue: %s is not a queue; starting empty\n", CQ_FILE);
    else if (count > 0) fprintf(stderr, "queue: %d item(s) read%s\n", count, cq_paused() ? " (paused)" : "");
}

/* {"k":"<escaped v>"} pairs into one object. */
static void put(char *out, size_t cap, size_t *pos, const char *k, const char *v) {
    size_t need;
    int w;
    if (*pos >= cap) return;
    w = snprintf(out + *pos, cap - *pos, "%s\"%s\":\"", *pos > 1 ? "," : "", k);
    if (w < 0 || (size_t)w >= cap - *pos) {
        *pos = cap;
        return;
    }
    *pos += (size_t)w;
    need = webui_json_escape(out + *pos, cap - *pos, v, strlen(v));
    if (need >= cap - *pos || cap - *pos - need < 3) {
        *pos = cap;
        return;
    }
    *pos += need;
    out[(*pos)++] = '"';
    out[*pos] = '\0';
}

/* One route call; the HTTP status, the reply's "error" token (or "") into err. */
static int call(const char *route, const char *body, char *reply, char err[CQ_ERR_MAX]) {
    int st = webui_api_call(route, body, reply, REPLY_MAX);
    err[0] = '\0';
    if (st != 200 && webui_json_str(reply, strlen(reply), "error", err, CQ_ERR_MAX) != 1)
        snprintf(err, CQ_ERR_MAX, "%s", st < 0 ? "internal" : "failed");
    return st;
}

static void parent_of(const char *path, char *out, size_t cap) {
    const char *slash = strrchr(path, '/');
    size_t len = slash ? (size_t)(slash - path) : 0;
    if (len >= cap) len = cap - 1;
    memcpy(out, path, len);
    out[len] = '\0';
}

/* Starts the copy src -> dest; the job ("smb:<id>" or 32 hex) into job. 0, or the cause in err. */
static int start_copy(const char *src, const char *dest, char *job, char *reply, char err[CQ_ERR_MAX]) {
    char body[2 * CQ_PATH_MAX * 6 + 64], dir[CQ_PATH_MAX], id[CQ_JOB_MAX];
    size_t pos = 1;
    int smb = !strncmp(src, "smb://", 6), st;
    body[0] = '{';
    body[1] = '\0';
    parent_of(dest, dir, sizeof dir);
    if (dir[0]) {
        put(body, sizeof body, &pos, "path", dir);
        if (pos < sizeof body - 2) {
            memcpy(body + pos, "}", 2);
            (void)call("/api/fs/mkdir", body, reply, err); /* one that is there is fine */
        }
        pos = 1;
        body[1] = '\0';
    }
    put(body, sizeof body, &pos, "src", src);
    put(body, sizeof body, &pos, "dest", dest);
    if (pos >= sizeof body - 2) {
        snprintf(err, CQ_ERR_MAX, "path_too_long");
        return -1;
    }
    memcpy(body + pos, "}", 2);
    st = call(smb ? "/api/smb/copy" : "/api/fs/copy", body, reply, err);
    if (st != 200) return -1;
    if (webui_json_str(reply, strlen(reply), "job", id, sizeof id) != 1 || !id[0]) {
        snprintf(err, CQ_ERR_MAX, "bad_reply");
        return -1;
    }
    snprintf(job, CQ_JOB_MAX, "%s%s", smb ? "smb:" : "", id);
    return 0;
}

/* Starts the console's install of the package at `path`. 0 started, 1 the console is busy (try
 * later), -1 failed (err). */
static int start_install(const char *path, int remove_after, char *job, char *reply, char err[CQ_ERR_MAX]) {
    char body[CQ_PATH_MAX * 6 + 64];
    size_t pos = 1;
    int st;
    body[0] = '{';
    body[1] = '\0';
    put(body, sizeof body, &pos, "path", path);
    if (pos >= sizeof body - 24) {
        snprintf(err, CQ_ERR_MAX, "path_too_long");
        return -1;
    }
    snprintf(body + pos, sizeof body - pos, "%s}", remove_after ? ",\"remove_after\":true" : "");
    st = call("/api/pkg/install", body, reply, err);
    if (st == 409 && (!strcmp(err, "pkg_install_busy") || !strcmp(err, "convert_busy"))) return 1;
    if (st != 200) return -1;
    if (webui_json_str(reply, strlen(reply), "job", job, CQ_JOB_MAX) != 1 || !job[0]) {
        snprintf(err, CQ_ERR_MAX, "bad_reply");
        return -1;
    }
    return 0;
}

/* The staging copy of a package from a share is ours to remove when the install never got it. */
static void drop_stage(const cq_item_t *it) {
    if (it->kind == CQ_INSTALL && !strncmp(it->dest, WEBUI_PKG_STAGE_DIR "/" STAGE_PREFIX, sizeof WEBUI_PKG_STAGE_DIR + sizeof STAGE_PREFIX - 1))
        (void)unlink(it->dest);
}

/* The install step of an item: from its staged copy (removed once installed) or where it is. */
static void install_step(cq_item_t *it, char *reply) {
    char job[CQ_JOB_MAX], err[CQ_ERR_MAX];
    int staged = it->dest[0] != '\0', rc;
    rc = start_install(staged ? it->dest : it->src, staged, job, reply, err);
    if (rc == 1) cq_wait(it->id, err, mono_ms() + BUSY_RETRY_MS);
    else if (rc < 0) {
        drop_stage(it);
        cq_finish(it->id, CQ_FAILED, err, (int64_t)time(NULL));
    } else
        cq_set_job(it->id, job, 1);
}

static void begin(cq_item_t *it, char *reply) {
    char job[CQ_JOB_MAX], err[CQ_ERR_MAX];
    if (it->kind == CQ_COPY) {
        if (start_copy(it->src, it->dest, job, reply, err) != 0) cq_finish(it->id, CQ_FAILED, err, (int64_t)time(NULL));
        else cq_set_job(it->id, job, 0);
        return;
    }
    /* an install: one on a share is copied into the staging folder first (as the Library does) */
    if (strncmp(it->src, "smb://", 6) != 0 || it->step == 1) {
        install_step(it, reply);
        return;
    }
    {
        char stage[CQ_PATH_MAX];
        snprintf(stage, sizeof stage, "%s/%s%lu-%lld.pkg", WEBUI_PKG_STAGE_DIR, STAGE_PREFIX, (unsigned long)it->id,
                 (long long)time(NULL));
        cq_set_dest(it->id, stage);
        if (start_copy(it->src, stage, job, reply, err) != 0) {
            (void)unlink(stage);
            cq_finish(it->id, CQ_FAILED, err, (int64_t)time(NULL));
        } else
            cq_set_job(it->id, job, 0);
    }
}

/* Reads the running item's job and moves it along. */
static void watch(cq_item_t *it, char *reply) {
    char body[CQ_JOB_MAX + 16], state[16], err[CQ_ERR_MAX];
    int smb = !strncmp(it->job, "smb:", 4), st;
    const char *id = smb ? it->job + 4 : it->job;
    uint64_t done = 0, total = 0;
    if (!it->job[0]) {
        /* between steps (a restart of the runner, a lost start): start the step again */
        if (it->cancel) cq_finish(it->id, CQ_CANCELLED, NULL, (int64_t)time(NULL));
        else begin(it, reply);
        return;
    }
    snprintf(body, sizeof body, "{\"job\":\"%s\"}", id);
    if (it->cancel && g_cancel_sent != it->id) {
        g_cancel_sent = it->id;
        (void)call(smb ? "/api/smb/cancel" : "/api/jobs/cancel", body, reply, err);
    }
    st = call(smb ? "/api/smb/job" : "/api/jobs/status", body, reply, err);
    if (st == 404 || (st != 200 && !strcmp(err, "unknown_job"))) {
        if (it->step == 0) drop_stage(it);
        cq_finish(it->id, it->cancel ? CQ_CANCELLED : CQ_FAILED, "job_lost", (int64_t)time(NULL));
        return;
    }
    if (st != 200) return; /* passing trouble (the daemon starting): read again next tick */
    if (webui_json_str(reply, strlen(reply), "state", state, sizeof state) != 1) return;
    (void)webui_json_u64(reply, strlen(reply), "bytes_done", &done);
    (void)webui_json_u64(reply, strlen(reply), "bytes_total", &total);
    if (!strcmp(state, "running")) {
        cq_progress(it->id, done, total);
        return;
    }
    if (!strcmp(state, "done")) {
        if (it->kind == CQ_INSTALL && it->step == 0) {
            /* the package is on the console: now the install */
            cq_set_job(it->id, "", 1);
            it->step = 1;
            if (!it->cancel) install_step(it, reply);
            else {
                drop_stage(it);
                cq_finish(it->id, CQ_CANCELLED, NULL, (int64_t)time(NULL));
            }
            return;
        }
        cq_finish(it->id, CQ_DONE, NULL, (int64_t)time(NULL));
        return;
    }
    /* failed or cancelled: the cause is the job's error (a share copy) or message (an operation) */
    if (webui_json_str(reply, strlen(reply), smb ? "error" : "message", err, sizeof err) != 1 || !err[0])
        snprintf(err, sizeof err, "%s", !strcmp(state, "cancelled") ? "cancelled" : "failed");
    else {
        char *cut = strpbrk(err, ": ");
        if (cut) *cut = '\0';
    }
    if (it->kind == CQ_INSTALL && it->step == 1 && (!strcmp(err, "pkg_install_busy") || !strcmp(err, "convert_busy"))) {
        cq_wait(it->id, err, mono_ms() + BUSY_RETRY_MS);
        return;
    }
    if (it->step == 0) drop_stage(it);
    cq_finish(it->id, !strcmp(state, "cancelled") || it->cancel ? CQ_CANCELLED : CQ_FAILED, err, (int64_t)time(NULL));
}

static void *runner(void *arg) {
    cq_item_t *it = malloc(sizeof *it);
    char *reply = malloc(REPLY_MAX);
    (void)arg;
    proc_name_set_self("ps5upload-queue");
    if (!it || !reply) {
        fprintf(stderr, "queue: out of memory; the queue does not run\n");
        free(it);
        free(reply);
        return NULL;
    }
    for (;;) {
        reply[0] = '\0';
        if (cq_running(it)) watch(it, reply);
        else if (cq_take_next(it, (int64_t)time(NULL), mono_ms())) begin(it, reply);
        save();
        sleep_ms(TICK_MS);
    }
    return NULL;
}

static void start_once(void) {
    pthread_t t;
    pthread_attr_t attr;
    load();
    if (pthread_attr_init(&attr) != 0) return;
    (void)pthread_attr_setstacksize(&attr, 256u * 1024u);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&t, &attr, runner, NULL) != 0) fprintf(stderr, "queue: runner did not start\n");
    pthread_attr_destroy(&attr);
}

void webui_queue_start(void) { (void)pthread_once(&g_once, start_once); }

/* ---- routes ---- */

static void send_err(int fd, int status, const char *token) {
    char buf[96];
    snprintf(buf, sizeof buf, "{\"error\":\"%s\"}", token);
    webui_send_json(fd, status, buf);
}

static int id_of(const char *b, size_t n, uint32_t *id) {
    uint64_t v = 0;
    if (webui_json_u64(b, n, "id", &v) != 1 || !v || v > UINT32_MAX) return -1;
    *id = (uint32_t)v;
    return 0;
}

void webui_queue_get(const char *b, size_t n, uint8_t *out, int fd) {
    char *json = malloc(FILE_MAX);
    int k;
    (void)b;
    (void)n;
    (void)out;
    webui_queue_start();
    if (!json) {
        send_err(fd, 500, "out_of_memory");
        return;
    }
    k = cq_json(json, FILE_MAX, (int64_t)time(NULL));
    if (k < 0) send_err(fd, 500, "too_long");
    else webui_send(fd, 200, "application/json", json, (size_t)k);
    free(json);
}

void webui_queue_add(const char *b, size_t n, uint8_t *out, int fd) {
    char kind[16], *src = malloc(CQ_PATH_MAX), *dest = malloc(CQ_PATH_MAX), *key = malloc(CQ_KEY_MAX), name[CQ_NAME_MAX];
    long id;
    int k;
    (void)out;
    webui_queue_start();
    if (!src || !dest || !key) {
        send_err(fd, 500, "out_of_memory");
        goto done;
    }
    if (webui_json_str(b, n, "kind", kind, sizeof kind) != 1 || (strcmp(kind, "copy") && strcmp(kind, "install")) ||
        webui_json_str(b, n, "src", src, CQ_PATH_MAX) != 1 || (src[0] != '/' && strncmp(src, "smb://", 6))) {
        send_err(fd, 400, "bad_request");
        goto done;
    }
    if (webui_json_str(b, n, "dest", dest, CQ_PATH_MAX) != 1) dest[0] = '\0';
    if (!strcmp(kind, "copy") && dest[0] != '/') {
        send_err(fd, 400, "bad_dest");
        goto done;
    }
    if (webui_json_str(b, n, "name", name, sizeof name) != 1) name[0] = '\0';
    if (webui_json_str(b, n, "key", key, CQ_KEY_MAX) != 1) key[0] = '\0';
    id = cq_add(!strcmp(kind, "copy") ? CQ_COPY : CQ_INSTALL, src, dest, name, key, (int64_t)time(NULL));
    if (id == 0) send_err(fd, 409, "queue_full");
    else if (id < 0) send_err(fd, 400, "bad_request");
    else {
        char json[48];
        k = snprintf(json, sizeof json, "{\"id\":%ld}", id);
        webui_send(fd, 200, "application/json", json, (size_t)k);
        save();
    }
done:
    free(src);
    free(dest);
    free(key);
}

void webui_queue_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    uint32_t id;
    int rc;
    (void)out;
    if (id_of(b, n, &id) != 0) {
        send_err(fd, 400, "bad_id");
        return;
    }
    /* a running item's job is stopped by the runner on its next tick */
    rc = cq_cancel(id, NULL);
    if (rc == 0) send_err(fd, 404, "unknown_item");
    else if (rc < 0) send_err(fd, 409, "ended");
    else {
        webui_send_json(fd, 200, "{}");
        save();
    }
}

void webui_queue_top(const char *b, size_t n, uint8_t *out, int fd) {
    uint32_t id;
    (void)out;
    if (id_of(b, n, &id) != 0) {
        send_err(fd, 400, "bad_id");
        return;
    }
    if (!cq_top(id)) send_err(fd, 409, "not_pending");
    else {
        webui_send_json(fd, 200, "{}");
        save();
    }
}

void webui_queue_clear(const char *b, size_t n, uint8_t *out, int fd) {
    char json[32];
    int k;
    (void)b;
    (void)n;
    (void)out;
    k = snprintf(json, sizeof json, "{\"cleared\":%d}", cq_clear_finished());
    webui_send(fd, 200, "application/json", json, (size_t)k);
    save();
}

void webui_queue_pause(const char *b, size_t n, uint8_t *out, int fd) {
    int paused = 0;
    (void)out;
    if (webui_json_bool(b, n, "paused", &paused) != 1) {
        send_err(fd, 400, "bad_paused");
        return;
    }
    webui_queue_start();
    cq_set_paused(paused, "");
    webui_send_json(fd, 200, "{}");
    save();
}

int webui_queue_json(char *out, size_t cap) { return cq_json(out, cap, (int64_t)time(NULL)); }
