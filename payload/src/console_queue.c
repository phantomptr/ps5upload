/* See include/console_queue.h. */
#include "console_queue.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>

#include "webui_codec.h"

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static cq_item_t g_items[CQ_MAX];
static uint32_t g_next_id = 1;
static int64_t g_order_hi, g_order_lo; /* the last order handed out at each end */
static int g_paused, g_dirty;
static char g_reason[CQ_ERR_MAX];

static void copy_str(char *dst, size_t cap, const char *src) { snprintf(dst, cap, "%s", src ? src : ""); }

static int finished(const cq_item_t *it) { return it->state >= CQ_DONE; }

static cq_item_t *find_locked(uint32_t id) {
    unsigned i;
    for (i = 0; i < CQ_MAX; i++)
        if (g_items[i].state != CQ_FREE && g_items[i].id == id) return &g_items[i];
    return NULL;
}

static cq_item_t *running_locked(void) {
    unsigned i;
    for (i = 0; i < CQ_MAX; i++)
        if (g_items[i].state == CQ_RUNNING) return &g_items[i];
    return NULL;
}

/* A free slot, else the finished item that ended longest ago; NULL when all are unfinished. */
static cq_item_t *slot_locked(void) {
    cq_item_t *best = NULL;
    unsigned i;
    for (i = 0; i < CQ_MAX; i++) {
        cq_item_t *it = &g_items[i];
        if (it->state == CQ_FREE) return it;
        if (finished(it) && (!best || it->ended < best->ended)) best = it;
    }
    return best;
}

long cq_add(int kind, const char *src, const char *dest, const char *name, const char *key, int64_t now) {
    cq_item_t *it;
    unsigned i;
    long id;
    if ((kind != CQ_COPY && kind != CQ_INSTALL) || !src || !src[0] || strlen(src) >= CQ_PATH_MAX) return -1;
    if (kind == CQ_COPY && (!dest || !dest[0] || strlen(dest) >= CQ_PATH_MAX)) return -1;
    pthread_mutex_lock(&g_mu);
    for (i = 0; i < CQ_MAX; i++) {
        it = &g_items[i];
        if ((it->state == CQ_PENDING || it->state == CQ_RUNNING) && it->kind == kind && !strcmp(it->src, src) &&
            (kind == CQ_INSTALL || !strcmp(it->dest, dest))) {
            id = (long)it->id;
            pthread_mutex_unlock(&g_mu);
            return id;
        }
    }
    it = slot_locked();
    if (!it) {
        pthread_mutex_unlock(&g_mu);
        return 0;
    }
    memset(it, 0, sizeof *it);
    it->id = g_next_id++;
    if (!g_next_id) g_next_id = 1;
    it->kind = (uint8_t)kind;
    it->state = CQ_PENDING;
    it->order = ++g_order_hi;
    copy_str(it->src, sizeof it->src, src);
    copy_str(it->dest, sizeof it->dest, kind == CQ_COPY ? dest : "");
    copy_str(it->name, sizeof it->name, name);
    copy_str(it->key, sizeof it->key, key);
    it->added = now;
    id = (long)it->id;
    g_dirty = 1;
    pthread_mutex_unlock(&g_mu);
    return id;
}

int cq_cancel(uint32_t id, char job[CQ_JOB_MAX]) {
    cq_item_t *it;
    int rc;
    if (job) job[0] = '\0';
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (!it) rc = 0;
    else if (it->state == CQ_PENDING) {
        it->state = CQ_CANCELLED;
        it->wait[0] = '\0';
        rc = 1;
        g_dirty = 1;
    } else if (it->state == CQ_RUNNING) {
        it->cancel = 1;
        if (job) copy_str(job, CQ_JOB_MAX, it->job);
        rc = 2;
        g_dirty = 1;
    } else
        rc = -1;
    pthread_mutex_unlock(&g_mu);
    return rc;
}

int cq_top(uint32_t id) {
    cq_item_t *it;
    int rc = 0;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_PENDING) {
        it->order = --g_order_lo;
        rc = 1;
        g_dirty = 1;
    }
    pthread_mutex_unlock(&g_mu);
    return rc;
}

int cq_clear_finished(void) {
    unsigned i;
    int n = 0;
    pthread_mutex_lock(&g_mu);
    for (i = 0; i < CQ_MAX; i++)
        if (g_items[i].state != CQ_FREE && finished(&g_items[i])) {
            memset(&g_items[i], 0, sizeof g_items[i]);
            n++;
        }
    if (n) g_dirty = 1;
    pthread_mutex_unlock(&g_mu);
    return n;
}

void cq_set_paused(int paused, const char *reason) {
    pthread_mutex_lock(&g_mu);
    g_paused = paused ? 1 : 0;
    copy_str(g_reason, sizeof g_reason, paused ? reason : "");
    g_dirty = 1;
    pthread_mutex_unlock(&g_mu);
}

int cq_paused(void) {
    int p;
    pthread_mutex_lock(&g_mu);
    p = g_paused;
    pthread_mutex_unlock(&g_mu);
    return p;
}

/* The pending item first in line, or NULL. */
static cq_item_t *head_locked(void) {
    cq_item_t *best = NULL;
    unsigned i;
    for (i = 0; i < CQ_MAX; i++) {
        cq_item_t *it = &g_items[i];
        if (it->state == CQ_PENDING && (!best || it->order < best->order)) best = it;
    }
    return best;
}

int cq_take_next(cq_item_t *out, int64_t now, uint64_t now_ms) {
    cq_item_t *it;
    int rc = 0;
    pthread_mutex_lock(&g_mu);
    if (!g_paused && !running_locked()) {
        it = head_locked();
        /* strict order: one that waits holds the rest */
        if (it && it->retry_at_ms <= now_ms) {
            it->state = CQ_RUNNING;
            if (!it->started) it->started = now;
            it->wait[0] = '\0';
            it->job[0] = '\0';
            it->bytes_done = it->bytes_total = 0;
            if (out) *out = *it;
            rc = 1;
            g_dirty = 1;
        }
    }
    pthread_mutex_unlock(&g_mu);
    return rc;
}

int cq_running(cq_item_t *out) {
    cq_item_t *it;
    int rc = 0;
    pthread_mutex_lock(&g_mu);
    it = running_locked();
    if (it) {
        if (out) *out = *it;
        rc = 1;
    }
    pthread_mutex_unlock(&g_mu);
    return rc;
}

void cq_set_job(uint32_t id, const char *job, int step) {
    cq_item_t *it;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_RUNNING) {
        copy_str(it->job, sizeof it->job, job);
        it->step = (uint8_t)step;
        it->bytes_done = it->bytes_total = 0;
        g_dirty = 1;
    }
    pthread_mutex_unlock(&g_mu);
}

void cq_set_dest(uint32_t id, const char *dest) {
    cq_item_t *it;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_RUNNING) {
        copy_str(it->dest, sizeof it->dest, dest);
        g_dirty = 1;
    }
    pthread_mutex_unlock(&g_mu);
}

void cq_progress(uint32_t id, uint64_t done, uint64_t total) {
    cq_item_t *it;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_RUNNING) {
        it->bytes_done = done;
        it->bytes_total = total;
    }
    pthread_mutex_unlock(&g_mu);
}

void cq_finish(uint32_t id, int state, const char *error, int64_t now) {
    cq_item_t *it;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_RUNNING) {
        if (state == CQ_FAILED && it->cancel) state = CQ_CANCELLED;
        it->state = (uint8_t)(state == CQ_DONE || state == CQ_CANCELLED ? state : CQ_FAILED);
        copy_str(it->error, sizeof it->error, it->state == CQ_FAILED ? (error && error[0] ? error : "failed") : "");
        it->ended = now;
        it->job[0] = '\0';
        it->wait[0] = '\0';
        g_dirty = 1;
    }
    pthread_mutex_unlock(&g_mu);
}

void cq_wait(uint32_t id, const char *why, uint64_t at_ms) {
    cq_item_t *it;
    pthread_mutex_lock(&g_mu);
    it = find_locked(id);
    if (it && it->state == CQ_RUNNING) {
        if (it->cancel) {
            it->state = CQ_CANCELLED;
            it->ended = it->started;
        } else {
            it->state = CQ_PENDING;
            it->order = --g_order_lo; /* still first in line */
            it->retry_at_ms = at_ms;
            copy_str(it->wait, sizeof it->wait, why);
        }
        it->job[0] = '\0';
        g_dirty = 1;
    }
    pthread_mutex_unlock(&g_mu);
}

int cq_dirty(void) {
    int d;
    pthread_mutex_lock(&g_mu);
    d = g_dirty;
    pthread_mutex_unlock(&g_mu);
    return d;
}

void cq_saved(void) {
    pthread_mutex_lock(&g_mu);
    g_dirty = 0;
    pthread_mutex_unlock(&g_mu);
}

static const char *state_name(uint8_t s) {
    switch (s) {
    case CQ_PENDING: return "pending";
    case CQ_RUNNING: return "running";
    case CQ_DONE: return "done";
    case CQ_FAILED: return "failed";
    default: return "cancelled";
    }
}

static int state_of(const char *s) {
    if (!strcmp(s, "pending")) return CQ_PENDING;
    if (!strcmp(s, "running")) return CQ_RUNNING;
    if (!strcmp(s, "done")) return CQ_DONE;
    if (!strcmp(s, "failed")) return CQ_FAILED;
    if (!strcmp(s, "cancelled")) return CQ_CANCELLED;
    return CQ_FREE;
}

/* Appends `"key":"<escaped>"` (with a leading comma unless first). -1 when it does not fit. */
static int put_str(char *out, size_t cap, size_t *pos, const char *key, const char *s, int first) {
    size_t need;
    int k = snprintf(out + *pos, cap - *pos, "%s\"%s\":\"", first ? "" : ",", key);
    if (k < 0 || (size_t)k >= cap - *pos) return -1;
    *pos += (size_t)k;
    need = webui_json_escape(out + *pos, cap - *pos, s, strlen(s));
    if (need >= cap - *pos || cap - *pos - need < 2) return -1;
    *pos += need;
    out[(*pos)++] = '"';
    out[*pos] = '\0';
    return 0;
}

/* One item as a JSON object. `live`: with what only the page needs (bytes, elapsed). */
static int item_json(const cq_item_t *it, char *out, size_t cap, size_t *pos, int live, int64_t now) {
    int k = snprintf(out + *pos, cap - *pos, "{\"id\":%lu,\"kind\":\"%s\",\"state\":\"%s\",\"step\":%u,\"order\":%lld,",
                     (unsigned long)it->id, it->kind == CQ_COPY ? "copy" : "install", state_name(it->state),
                     (unsigned)it->step, (long long)it->order);
    if (k < 0 || (size_t)k >= cap - *pos) return -1;
    *pos += (size_t)k;
    if (put_str(out, cap, pos, "src", it->src, 1) || put_str(out, cap, pos, "dest", it->dest, 0) ||
        put_str(out, cap, pos, "name", it->name, 0) || put_str(out, cap, pos, "key", it->key, 0) ||
        put_str(out, cap, pos, "job", it->job, 0) || put_str(out, cap, pos, "error", it->error, 0) ||
        put_str(out, cap, pos, "wait", it->wait, 0))
        return -1;
    if (live) {
        int64_t end = it->state == CQ_RUNNING ? now : it->ended;
        int64_t ran = it->started && end > it->started ? end - it->started : 0;
        k = snprintf(out + *pos, cap - *pos,
                     ",\"added\":%lld,\"started\":%lld,\"ended\":%lld,\"elapsed_s\":%lld,\"bytes_done\":%llu,"
                     "\"bytes_total\":%llu,\"cancel\":%s}",
                     (long long)it->added, (long long)it->started, (long long)it->ended, (long long)ran,
                     (unsigned long long)it->bytes_done, (unsigned long long)it->bytes_total,
                     it->cancel ? "true" : "false");
    } else {
        k = snprintf(out + *pos, cap - *pos, ",\"added\":%lld,\"started\":%lld,\"ended\":%lld,\"cancel\":%u}",
                     (long long)it->added, (long long)it->started, (long long)it->ended, (unsigned)it->cancel);
    }
    if (k < 0 || (size_t)k >= cap - *pos) return -1;
    *pos += (size_t)k;
    return 0;
}

/* Indexes of the used slots in run order: finished (by end), running, pending (by order). */
static unsigned ordered_locked(unsigned idx[CQ_MAX]) {
    unsigned n = 0, i, j;
    for (i = 0; i < CQ_MAX; i++)
        if (g_items[i].state != CQ_FREE) idx[n++] = i;
    for (i = 1; i < n; i++) {
        unsigned v = idx[i];
        for (j = i; j > 0; j--) {
            const cq_item_t *a = &g_items[idx[j - 1]], *b = &g_items[v];
            int ra = finished(a) ? 0 : a->state == CQ_RUNNING ? 1 : 2;
            int rb = finished(b) ? 0 : b->state == CQ_RUNNING ? 1 : 2;
            int64_t ka = ra == 0 ? a->ended : a->order, kb = rb == 0 ? b->ended : b->order;
            if (ra < rb || (ra == rb && (ka < kb || (ka == kb && a->id <= b->id)))) break;
            idx[j] = idx[j - 1];
        }
        idx[j] = v;
    }
    return n;
}

int cq_json(char *out, size_t cap, int64_t now) {
    unsigned idx[CQ_MAX], n, i;
    size_t pos = 0;
    int k, rc = 0;
    if (cap) out[0] = '\0';
    pthread_mutex_lock(&g_mu);
    k = snprintf(out, cap, "{\"paused\":%s,", g_paused ? "true" : "false");
    if (k < 0 || (size_t)k >= cap) rc = -1;
    else {
        pos = (size_t)k;
        if (put_str(out, cap, &pos, "reason", g_reason, 1) != 0) rc = -1;
    }
    if (!rc) {
        k = snprintf(out + pos, cap - pos, ",\"items\":[");
        if (k < 0 || (size_t)k >= cap - pos) rc = -1;
        else pos += (size_t)k;
    }
    n = ordered_locked(idx);
    for (i = 0; !rc && i < n; i++) {
        if (i) {
            if (cap - pos < 2) rc = -1;
            else out[pos++] = ',';
        }
        if (!rc && item_json(&g_items[idx[i]], out, cap, &pos, 1, now) != 0) rc = -1;
    }
    pthread_mutex_unlock(&g_mu);
    if (rc || cap - pos < 3) {
        if (cap) out[0] = '\0';
        return -1;
    }
    memcpy(out + pos, "]}", 3);
    return (int)(pos + 2);
}

int cq_serialize(char *out, size_t cap) {
    unsigned idx[CQ_MAX], n, i;
    size_t pos = 0;
    int k, rc = 0;
    if (cap) out[0] = '\0';
    pthread_mutex_lock(&g_mu);
    k = snprintf(out, cap, "{\"v\":1,\"next_id\":%lu,\"order_hi\":%lld,\"order_lo\":%lld,\"paused\":%d,",
                 (unsigned long)g_next_id, (long long)g_order_hi, (long long)g_order_lo, g_paused);
    if (k < 0 || (size_t)k >= cap) rc = -1;
    else {
        pos = (size_t)k;
        if (put_str(out, cap, &pos, "reason", g_reason, 1) != 0 || cap - pos < 3) rc = -1;
        else {
            memcpy(out + pos, "}\n", 3);
            pos += 2;
        }
    }
    n = ordered_locked(idx);
    for (i = 0; !rc && i < n; i++) {
        if (item_json(&g_items[idx[i]], out, cap, &pos, 0, 0) != 0 || cap - pos < 2) rc = -1;
        else {
            out[pos++] = '\n';
            out[pos] = '\0';
        }
    }
    pthread_mutex_unlock(&g_mu);
    if (rc) {
        if (cap) out[0] = '\0';
        return -1;
    }
    return (int)pos;
}

static int64_t num(const char *line, size_t n, const char *key) {
    uint64_t v = 0;
    if (webui_json_u64(line, n, key, &v) != 1) return 0;
    return (int64_t)v;
}

/* Reads one negative-or-positive integer field (order may be below zero). */
static int64_t snum(const char *line, size_t n, const char *key) {
    char pat[40];
    const char *p;
    long long v = 0;
    snprintf(pat, sizeof pat, "\"%s\":", key);
    p = strstr(line, pat);
    if (!p || (size_t)(p - line) >= n) return 0;
    if (sscanf(p + strlen(pat), "%lld", &v) != 1) return 0;
    return (int64_t)v;
}

int cq_deserialize(const char *text, size_t n, int64_t now) {
    const char *p = text, *end = text + n, *nl;
    int count = 0, interrupted_install = 0;
    cq_item_t *slot = g_items;
    char state[16], kind[16];
    if (!text || !n || text[0] != '{' || !strstr(text, "\"v\":1")) return -1;
    pthread_mutex_lock(&g_mu);
    memset(g_items, 0, sizeof g_items);
    nl = memchr(p, '\n', (size_t)(end - p));
    if (!nl) nl = end;
    g_next_id = (uint32_t)num(p, (size_t)(nl - p), "next_id");
    if (!g_next_id) g_next_id = 1;
    g_order_hi = snum(p, (size_t)(nl - p), "order_hi");
    g_order_lo = snum(p, (size_t)(nl - p), "order_lo");
    g_paused = num(p, (size_t)(nl - p), "paused") ? 1 : 0;
    if (webui_json_str(p, (size_t)(nl - p), "reason", g_reason, sizeof g_reason) != 1) g_reason[0] = '\0';
    p = nl < end ? nl + 1 : end;
    while (p < end && slot < g_items + CQ_MAX) {
        size_t len;
        nl = memchr(p, '\n', (size_t)(end - p));
        if (!nl) nl = end;
        len = (size_t)(nl - p);
        if (len && p[0] == '{' && webui_json_str(p, len, "state", state, sizeof state) == 1 &&
            webui_json_str(p, len, "kind", kind, sizeof kind) == 1 &&
            webui_json_str(p, len, "src", slot->src, sizeof slot->src) == 1 && slot->src[0]) {
            int st = state_of(state);
            slot->id = (uint32_t)num(p, len, "id");
            slot->kind = !strcmp(kind, "copy") ? CQ_COPY : !strcmp(kind, "install") ? CQ_INSTALL : 0;
            if (st != CQ_FREE && slot->id && slot->kind) {
                if (webui_json_str(p, len, "dest", slot->dest, sizeof slot->dest) != 1) slot->dest[0] = '\0';
                if (webui_json_str(p, len, "name", slot->name, sizeof slot->name) != 1) slot->name[0] = '\0';
                if (webui_json_str(p, len, "key", slot->key, sizeof slot->key) != 1) slot->key[0] = '\0';
                if (webui_json_str(p, len, "error", slot->error, sizeof slot->error) != 1) slot->error[0] = '\0';
                slot->order = snum(p, len, "order");
                slot->step = (uint8_t)(num(p, len, "step") ? 1 : 0);
                slot->added = num(p, len, "added");
                slot->started = num(p, len, "started");
                slot->ended = num(p, len, "ended");
                slot->state = (uint8_t)st;
                if (st == CQ_RUNNING) {
                    /* it was running when the helper stopped: nobody knows how it ended */
                    slot->state = CQ_FAILED;
                    copy_str(slot->error, sizeof slot->error, "interrupted");
                    slot->ended = now;
                    if (slot->kind == CQ_INSTALL) interrupted_install = 1;
                }
                if (slot->id >= g_next_id) g_next_id = slot->id + 1;
                if (slot->order > g_order_hi) g_order_hi = slot->order;
                if (slot->order < g_order_lo) g_order_lo = slot->order;
                slot++;
                count++;
            } else {
                memset(slot, 0, sizeof *slot);
            }
        } else {
            memset(slot, 0, sizeof *slot);
        }
        p = nl < end ? nl + 1 : end;
    }
    if (interrupted_install) {
        g_paused = 1;
        copy_str(g_reason, sizeof g_reason, "interrupted_install");
    }
    g_dirty = interrupted_install || count != 0;
    pthread_mutex_unlock(&g_mu);
    return count;
}

void cq_reset(void) {
    pthread_mutex_lock(&g_mu);
    memset(g_items, 0, sizeof g_items);
    g_next_id = 1;
    g_order_hi = g_order_lo = 0;
    g_paused = g_dirty = 0;
    g_reason[0] = '\0';
    pthread_mutex_unlock(&g_mu);
}
