/* See include/console_tasks.h. */
#include "console_tasks.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>

#include "ava1_gen.h"
#include "webui_codec.h"

#define CTASK_KIND_MAX 16u
#define CTASK_NAME_MAX 128u

enum { CT_FREE = 0, CT_RUNNING, CT_DONE, CT_FAILED, CT_CANCELLED, CT_GONE };

typedef struct {
    uint8_t id[16];
    uint8_t state;
    uint8_t cancel_asked;
    char kind[CTASK_KIND_MAX];
    char name[CTASK_NAME_MAX];
    uint32_t files_done, files_total;
    uint64_t bytes_done, bytes_total;
    uint64_t started_ms, ended_ms;
} ctask_t;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static ctask_t g_tasks[CTASKS_SLOTS];

static ctask_t *find_locked(const uint8_t id[16]) {
    unsigned i;
    for (i = 0; i < CTASKS_SLOTS; i++)
        if (g_tasks[i].state != CT_FREE && memcmp(g_tasks[i].id, id, 16) == 0) return &g_tasks[i];
    return NULL;
}

/* A free slot, else the one that ended longest ago; NULL only when all still run. */
static ctask_t *slot_locked(void) {
    ctask_t *best = NULL;
    unsigned i;
    for (i = 0; i < CTASKS_SLOTS; i++) {
        ctask_t *t = &g_tasks[i];
        if (t->state == CT_FREE) return t;
        if (t->state != CT_RUNNING && (!best || t->ended_ms < best->ended_ms)) best = t;
    }
    return best;
}

/* The last name of a path, cut at a whole UTF-8 character when it does not fit. */
static void last_name(const char *path, char *out, size_t cap) {
    size_t len;
    const char *end, *start;
    out[0] = '\0';
    if (!path) return;
    end = path + strlen(path);
    while (end > path && end[-1] == '/') end--;
    start = end;
    while (start > path && start[-1] != '/') start--;
    len = (size_t)(end - start);
    if (len >= cap) {
        len = cap - 1;
        while (len > 0 && ((unsigned char)start[len] & 0xC0u) == 0x80u) len--;
    }
    memcpy(out, start, len);
    out[len] = '\0';
}

void ctasks_started(const uint8_t id[16], const char *kind, const char *path, uint64_t now_ms) {
    ctask_t *t;
    pthread_mutex_lock(&g_mu);
    t = find_locked(id);
    if (!t) t = slot_locked();
    if (t) {
        memset(t, 0, sizeof *t);
        memcpy(t->id, id, 16);
        t->state = CT_RUNNING;
        t->started_ms = now_ms;
        snprintf(t->kind, sizeof t->kind, "%s", kind ? kind : "other");
        last_name(path, t->name, sizeof t->name);
    }
    pthread_mutex_unlock(&g_mu);
}

int ctasks_known(const uint8_t id[16]) {
    int known;
    pthread_mutex_lock(&g_mu);
    known = find_locked(id) != NULL;
    pthread_mutex_unlock(&g_mu);
    return known;
}

int ctasks_cancel_requested(const uint8_t id[16]) {
    ctask_t *t;
    int running;
    pthread_mutex_lock(&g_mu);
    t = find_locked(id);
    running = t && t->state == CT_RUNNING;
    if (running) t->cancel_asked = 1;
    pthread_mutex_unlock(&g_mu);
    return running;
}

void ctasks_cancel_failed(const uint8_t id[16]) {
    ctask_t *t;
    pthread_mutex_lock(&g_mu);
    t = find_locked(id);
    if (t) t->cancel_asked = 0;
    pthread_mutex_unlock(&g_mu);
}

int ctasks_observe(const uint8_t *res, size_t n, uint64_t now_ms) {
    ava1_job_list_result_t r;
    ava1_job_entry_t e;
    ava1_r_t it;
    uint8_t listed[CTASKS_SLOTS];
    unsigned i;
    int rc;
    if (ava1_job_list_result_decode(res, n, &r) != 0) return -1;
    memset(listed, 0, sizeof listed);
    ava1_r_init(&it, r.jobs, r.jobs_len);
    pthread_mutex_lock(&g_mu);
    while ((rc = ava1_job_entry_next(&it, &e)) == 1) {
        ctask_t *t = find_locked(e.job_id);
        if (!t) continue; /* an app's own job */
        listed[t - g_tasks] = 1;
        if (t->state != CT_RUNNING) continue;
        t->files_done = e.files_done;
        t->files_total = e.files_total;
        t->bytes_done = e.bytes_done;
        t->bytes_total = e.bytes_total;
        if (e.state != 0) {
            t->state = e.state == 1 ? CT_DONE : t->cancel_asked ? CT_CANCELLED : CT_FAILED;
            t->ended_ms = now_ms;
        }
    }
    if (rc >= 0)
        for (i = 0; i < CTASKS_SLOTS; i++) {
            ctask_t *t = &g_tasks[i];
            if (t->state != CT_RUNNING || listed[i] || now_ms - t->started_ms < CTASKS_NEW_MS) continue;
            t->state = t->cancel_asked ? CT_CANCELLED : CT_GONE;
            t->ended_ms = now_ms;
        }
    pthread_mutex_unlock(&g_mu);
    return rc < 0 ? -1 : 0;
}

static const char *state_name(uint8_t s) {
    switch (s) {
    case CT_RUNNING: return "running";
    case CT_DONE: return "done";
    case CT_FAILED: return "failed";
    case CT_CANCELLED: return "cancelled";
    default: return "gone";
    }
}

int ctasks_json(char *out, size_t cap, uint64_t now_ms) {
    size_t pos = 0;
    unsigned i;
    int written = 0, failed = 0;
    if (cap) out[0] = '\0';
    pthread_mutex_lock(&g_mu);
    for (i = 0; i < CTASKS_SLOTS && !failed; i++) {
        const ctask_t *t = &g_tasks[i];
        char hex[33], name[CTASK_NAME_MAX * 6 + 1];
        int k;
        if (t->state == CT_FREE) continue;
        if (t->state != CT_RUNNING && now_ms - t->ended_ms > CTASKS_KEEP_MS) continue;
        webui_hex16(t->id, hex);
        webui_json_escape(name, sizeof name, t->name, strlen(t->name));
        k = snprintf(out + pos, cap - pos,
                     "%s{\"id\":\"%s\",\"kind\":\"%s\",\"state\":\"%s\",\"name\":\"%s\",\"files_done\":%lu,"
                     "\"files_total\":%lu,\"bytes_done\":%llu,\"bytes_total\":%llu,\"age_s\":%llu}",
                     written ? "," : "", hex, t->kind, state_name(t->state), name, (unsigned long)t->files_done,
                     (unsigned long)t->files_total, (unsigned long long)t->bytes_done, (unsigned long long)t->bytes_total,
                     t->state == CT_RUNNING ? 0ull : (unsigned long long)((now_ms - t->ended_ms) / 1000u));
        if (k < 0 || (size_t)k >= cap - pos) {
            failed = 1;
            break;
        }
        pos += (size_t)k;
        written++;
    }
    pthread_mutex_unlock(&g_mu);
    if (failed) {
        if (cap) out[0] = '\0';
        return -1;
    }
    return (int)pos;
}

void ctasks_reset(void) {
    pthread_mutex_lock(&g_mu);
    memset(g_tasks, 0, sizeof g_tasks);
    pthread_mutex_unlock(&g_mu);
}
