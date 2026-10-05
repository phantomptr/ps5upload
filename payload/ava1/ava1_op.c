/* Long-running management operations as jobs: see ava1_op.h. */
#include "ava1_op.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_data.h"
#include "ava1_gen.h"
#include "ava1_platform.h"
#include "ava1_thread.h"
#include "ava1_wire.h"

#define MAX_OPS 16

struct ava1_op_ctx {
    /* Progress: the operation writes, a status poll reads (atomics, relaxed). */
    uint64_t files_done, files_total, bytes_done, bytes_total;
    int cancel;
    pthread_mutex_t mu; /* result and message */
    uint8_t *result;
    size_t result_len;
    char message[128];
};

typedef struct {
    ava1_op_ctx_t ctx;
    uint8_t op;
    uint8_t *args;
    size_t args_len;
    ava1_op_fn fn;
    void *fn_arg;
    pthread_t thread;
    int started;
} op_t;

static struct {
    pthread_mutex_t mu; /* registry */
    struct {
        uint8_t op;
        ava1_op_fn fn;
        void *arg;
    } ops[MAX_OPS];
    unsigned n;
} R = { .mu = PTHREAD_MUTEX_INITIALIZER };

/* Serialises job.run (find, count, create) so two calls with one id cannot both create. */
static pthread_mutex_t g_run_mu = PTHREAD_MUTEX_INITIALIZER;
static int g_running;

int ava1_op_register(uint8_t op, ava1_op_fn fn, void *arg) {
    unsigned i;
    int rc = 0;
    pthread_mutex_lock(&R.mu);
    for (i = 0; i < R.n; i++)
        if (R.ops[i].op == op) break;
    if (i == R.n) {
        if (R.n == MAX_OPS) rc = -1;
        else R.n++;
    }
    if (rc == 0) {
        R.ops[i].op = op;
        R.ops[i].fn = fn;
        R.ops[i].arg = arg;
    }
    pthread_mutex_unlock(&R.mu);
    return rc;
}

void ava1_op_unregister_all(void) {
    pthread_mutex_lock(&R.mu);
    R.n = 0;
    pthread_mutex_unlock(&R.mu);
}

static int lookup(uint8_t op, ava1_op_fn *fn, void **arg) {
    unsigned i;
    int found = 0;
    pthread_mutex_lock(&R.mu);
    for (i = 0; i < R.n; i++)
        if (R.ops[i].op == op) {
            *fn = R.ops[i].fn;
            *arg = R.ops[i].arg;
            found = 1;
        }
    pthread_mutex_unlock(&R.mu);
    return found;
}

/* ---- for operations ---- */

int ava1_op_cancelled(const ava1_op_ctx_t *c) { return __atomic_load_n(&c->cancel, __ATOMIC_ACQUIRE); }

void ava1_op_set_total(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes) {
    __atomic_store_n(&c->files_total, files, __ATOMIC_RELAXED);
    __atomic_store_n(&c->bytes_total, bytes, __ATOMIC_RELAXED);
}

void ava1_op_add(ava1_op_ctx_t *c, uint64_t files, uint64_t bytes) {
    if (files) (void)__atomic_add_fetch(&c->files_done, files, __ATOMIC_RELAXED);
    if (bytes) (void)__atomic_add_fetch(&c->bytes_done, bytes, __ATOMIC_RELAXED);
}

void ava1_op_message(ava1_op_ctx_t *c, const char *fmt, ...) {
    va_list ap;
    pthread_mutex_lock(&c->mu);
    va_start(ap, fmt);
    (void)vsnprintf(c->message, sizeof c->message, fmt, ap);
    va_end(ap);
    pthread_mutex_unlock(&c->mu);
}

int ava1_op_set_result(ava1_op_ctx_t *c, const void *body, size_t len) {
    uint8_t *copy = NULL;
    if (len > AVA1_OP_RESULT_MAX) return -1;
    if (len) {
        copy = malloc(len);
        if (!copy) return -1;
        memcpy(copy, body, len);
    }
    pthread_mutex_lock(&c->mu);
    free(c->result);
    c->result = copy;
    c->result_len = len;
    pthread_mutex_unlock(&c->mu);
    return 0;
}

/* ---- the job ---- */

static op_t *op_of(ava1_job_t *j) { return __atomic_load_n((op_t **)&j->role, __ATOMIC_ACQUIRE); }

static void *op_main(void *arg) {
    ava1_job_t *j = arg;
    op_t *o = op_of(j);
    /* (int): the error code is an unsigned constant and fn returns int; gcc 13 -Werror=sign-compare
     * rejects the mixed-sign ?: (review 007: the ctest build on a gcc 13 host). */
    int rc = ava1_op_cancelled(&o->ctx) ? (int)AVA1_ERR_CANCELLED : o->fn(o->fn_arg, &o->ctx, o->args, o->args_len);
    pthread_mutex_lock(&j->mu);
    j->final_status = (uint16_t)rc;
    pthread_mutex_lock(&o->ctx.mu);
    if (rc != AVA1_STATUS_OK && !o->ctx.message[0])
        snprintf(o->ctx.message, sizeof o->ctx.message, rc == AVA1_ERR_CANCELLED ? "cancelled" : "failed");
    snprintf(j->message, sizeof j->message, "%s", o->ctx.message);
    pthread_mutex_unlock(&o->ctx.mu);
    pthread_mutex_unlock(&j->mu);
    __atomic_store_n(&j->parked_at_ms, ava1_mono_ms(), __ATOMIC_RELEASE);
    __atomic_store_n(&j->finished, 1, __ATOMIC_RELEASE);
    (void)__atomic_sub_fetch(&g_running, 1, __ATOMIC_RELAXED);
    return NULL;
}

/* role_free: stop and join the worker (the job is being destroyed), free the op. */
static void op_free(ava1_job_t *j) {
    op_t *o = op_of(j);
    if (!o) return;
    __atomic_store_n(&o->ctx.cancel, 1, __ATOMIC_RELEASE);
    if (o->started) pthread_join(o->thread, NULL);
    free(o->args);
    free(o->ctx.result);
    pthread_mutex_destroy(&o->ctx.mu);
    free(o);
    __atomic_store_n((op_t **)&j->role, NULL, __ATOMIC_RELEASE);
}

int ava1_op_encode_status(ava1_job_t *j, uint8_t *out, size_t cap, size_t *out_len) {
    ava1_status_t st;
    ava1_w_t w;
    op_t *o = op_of(j);
    int rc, fin;
    memset(&st, 0, sizeof st);
    memcpy(st.job_id, j->id, 16);
    st.has_state = 1;
    if (!o) { /* between creation and the worker's start: running */
        ava1_w_init(&w, out, cap);
        rc = ava1_status_encode(&st, &w);
        *out_len = w.len;
        return rc == 0 ? AVA1_STATUS_OK : AVA1_ERR_INTERNAL;
    }
    /* finished is stored after the final status and message, so reading it first (acquire)
     * makes them visible: a status never says "finished" with an old message. */
    fin = __atomic_load_n(&j->finished, __ATOMIC_ACQUIRE);
    st.files_done = (uint32_t)__atomic_load_n(&o->ctx.files_done, __ATOMIC_RELAXED);
    st.files_total = (uint32_t)__atomic_load_n(&o->ctx.files_total, __ATOMIC_RELAXED);
    st.bytes_received = st.bytes_durable = __atomic_load_n(&o->ctx.bytes_done, __ATOMIC_RELAXED);
    st.bytes_total = __atomic_load_n(&o->ctx.bytes_total, __ATOMIC_RELAXED);
    st.state = !fin ? 0 : (j->final_status == AVA1_STATUS_OK ? 1 : 2);
    pthread_mutex_lock(&o->ctx.mu);
    if (o->ctx.message[0]) {
        st.has_current = 1;
        st.current = (const uint8_t *)o->ctx.message;
        st.current_len = (uint16_t)strlen(o->ctx.message);
    }
    if (fin && j->final_status == AVA1_STATUS_OK && o->ctx.result_len) {
        st.has_result = 1;
        st.result = o->ctx.result;
        st.result_len = (uint32_t)o->ctx.result_len;
    }
    if (fin && j->final_status != AVA1_STATUS_OK) {
        st.has_code = 1;
        st.code = j->final_status;
    }
    ava1_w_init(&w, out, cap);
    rc = ava1_status_encode(&st, &w);
    pthread_mutex_unlock(&o->ctx.mu);
    *out_len = w.len;
    return rc == 0 ? AVA1_STATUS_OK : AVA1_ERR_INTERNAL;
}

/* Signals and returns: this runs on a session's reader or an RPC worker, which must not
 * wait on a worker thread. The engine polls job.status, which reports `state 2 / ERR_CANCELLED`
 * as soon as the operation stops (a delete, chmod, hash, crc32 or backup within one entry or
 * block; fsck, cleanup and sdk.scan are one system call and only honour a cancel before they start). */
void ava1_op_cancel(ava1_job_t *j) {
    op_t *o = op_of(j);
    if (!o) return;
    __atomic_store_n(&o->ctx.cancel, 1, __ATOMIC_RELEASE);
}

/* The operations whose repeat gives the same outcome: their finished job is released as soon as
 * its terminal status was read (the engine has the answer; keeping 8 or 32 of them would let a
 * loop of hashes fill the job table). A backup snapshot or restore is kept for the done-age
 * instead, because a re-run after a lost reply would take a second snapshot. */
static int releasable(const op_t *o) { return o->op != AVA1_JOB_OP_BACKUP_SNAPSHOT && o->op != AVA1_JOB_OP_BACKUP_RESTORE; }

void (*ava1_op_test_pre_encode)(ava1_job_t *j);

int ava1_op_finished_before(ava1_job_t *j) { return __atomic_load_n(&j->finished, __ATOMIC_ACQUIRE) != 0; }

uint64_t ava1_op_keep_ms(int delivered) {
    uint64_t age = ava1_data_cfg()->park_ms ? ava1_data_cfg()->park_ms : AVA1_PARK_MS;
    uint64_t keep = delivered ? AVA1_OP_GRACE_MS : AVA1_OP_DONE_AGE_MS;
    return age < keep ? age : keep;
}

void ava1_op_status_delivered(ava1_job_t *j, int was_finished) {
    op_t *o = op_of(j);
    uint8_t id[16];
    uint64_t now;
    if (!o || !was_finished || !releasable(o)) return;
    now = ava1_mono_ms();
    if (!__atomic_load_n(&j->op_delivered, __ATOMIC_ACQUIRE)) {
        /* The first terminal delivery starts the grace: restamp, then flag (the reaper reads the
         * flag first, so it never sees the flag with the old stamp). */
        __atomic_store_n(&j->parked_at_ms, now, __ATOMIC_RELEASE);
        __atomic_store_n(&j->op_delivered, 1, __ATOMIC_RELEASE);
        return;
    }
    if (now - __atomic_load_n(&j->parked_at_ms, __ATOMIC_ACQUIRE) < ava1_op_keep_ms(1)) return;
    memcpy(id, j->id, 16);
    ava1_job_free_one(id); /* unlists; the caller's reference still holds it until it returns */
}

typedef struct {
    uint8_t id[AVA1_MAX_JOBS][16];
    uint64_t at[AVA1_MAX_JOBS];
    unsigned n;
} evict_t;

static void evict_one(ava1_job_t *j, void *ctx) {
    evict_t *e = ctx;
    if (j->kind != AVA1_JOB_OPKIND || !__atomic_load_n(&j->op_delivered, __ATOMIC_ACQUIRE) ||
        !__atomic_load_n(&j->finished, __ATOMIC_ACQUIRE) || e->n >= AVA1_MAX_JOBS) return;
    memcpy(e->id[e->n], j->id, 16);
    e->at[e->n++] = __atomic_load_n(&j->parked_at_ms, __ATOMIC_ACQUIRE);
}

/* The table is full: free the slot of the operation whose terminal status was delivered longest
 * ago, grace or not (a loop of hashes must never fill the table). 1 when one was freed. */
static int evict_delivered(void) {
    evict_t *e = calloc(1, sizeof *e);
    unsigned i, best = 0;
    int ok = 0;
    if (!e) return 0;
    ava1_job_foreach(evict_one, e);
    if (e->n) {
        for (i = 1; i < e->n; i++)
            if (e->at[i] < e->at[best]) best = i;
        ava1_job_free_one(e->id[best]);
        ok = 1;
    }
    free(e);
    return ok;
}

int ava1_op_run_rpc(const uint8_t *body, uint32_t len, const uint8_t owner[32], uint8_t *out, size_t cap,
                    size_t *out_len) {
    ava1_job_run_t r;
    ava1_op_fn fn = NULL;
    void *fn_arg = NULL;
    ava1_job_t *j;
    op_t *o;
    int rc;
    if (ava1_job_run_decode(body, len, &r) != 0) return AVA1_ERR_PROTOCOL;
    if (r.args_len > AVA1_OP_ARGS_MAX) {
        ava1_rpc_msg(out, cap, out_len, "args_too_large");
        return AVA1_ERR_PROTOCOL;
    }
    pthread_mutex_lock(&g_run_mu);
    j = ava1_job_find(r.job_id);
    if (j) { /* the same job again: whatever state it is in, that is the answer */
        o = op_of(j);
        if (memcmp(j->owner, owner, 32) != 0) {
            rc = AVA1_ERR_UNKNOWN_JOB;
        } else if (j->kind != AVA1_JOB_OPKIND || !o || o->op != r.op || o->args_len != r.args_len ||
                   (r.args_len && memcmp(o->args, r.args, r.args_len) != 0)) {
            ava1_rpc_msg(out, cap, out_len, "a job with this id has different parameters");
            rc = AVA1_ERR_PROTOCOL;
        } else {
            int fin = ava1_op_finished_before(j);
            rc = ava1_op_encode_status(j, out, cap, out_len);
            if (rc == AVA1_STATUS_OK) ava1_op_status_delivered(j, fin);
        }
        ava1_job_put(j);
        pthread_mutex_unlock(&g_run_mu);
        return rc;
    }
    if (!lookup(r.op, &fn, &fn_arg)) {
        pthread_mutex_unlock(&g_run_mu);
        ava1_rpc_msg(out, cap, out_len, "unknown_op");
        return AVA1_ERR_PROTOCOL;
    }
    if (__atomic_load_n(&g_running, __ATOMIC_RELAXED) >= AVA1_OP_MAX_RUNNING) {
        pthread_mutex_unlock(&g_run_mu);
        ava1_rpc_msg(out, cap, out_len, "too many operations running");
        return AVA1_ERR_BUSY;
    }
    j = ava1_job_create(r.job_id, owner);
    if (!j && evict_delivered()) j = ava1_job_create(r.job_id, owner);
    if (!j) {
        pthread_mutex_unlock(&g_run_mu);
        ava1_rpc_msg(out, cap, out_len, "the job table is full");
        return AVA1_ERR_BUSY;
    }
    o = calloc(1, sizeof *o);
    if (o) {
        pthread_mutex_init(&o->ctx.mu, NULL);
        o->op = r.op;
        o->fn = fn;
        o->fn_arg = fn_arg;
        o->args_len = r.args_len;
        o->args = malloc(r.args_len + 1);
    }
    if (!o || !o->args) {
        if (o) {
            pthread_mutex_destroy(&o->ctx.mu);
            free(o);
        }
        ava1_job_free_one(r.job_id);
        ava1_job_put(j);
        pthread_mutex_unlock(&g_run_mu);
        ava1_rpc_msg(out, cap, out_len, "out of memory");
        return AVA1_ERR_INTERNAL;
    }
    if (r.args_len) memcpy(o->args, r.args, r.args_len);
    o->args[r.args_len] = 0; /* ops read their arguments as C text */
    pthread_mutex_lock(&j->mu);
    j->kind = AVA1_JOB_OPKIND;
    j->role_free = op_free;
    pthread_mutex_unlock(&j->mu);
    (void)__atomic_add_fetch(&g_running, 1, __ATOMIC_RELAXED);
    __atomic_store_n((op_t **)&j->role, o, __ATOMIC_RELEASE);
    if (ava1_thread_start_stack(op_main, j, &o->thread, AVA1_MGMT_STACK) != 0) {
        (void)__atomic_sub_fetch(&g_running, 1, __ATOMIC_RELAXED);
        ava1_job_free_one(r.job_id); /* unlists; the put below destroys it (op_free finds no thread) */
        ava1_job_put(j);
        pthread_mutex_unlock(&g_run_mu);
        ava1_rpc_msg(out, cap, out_len, "cannot start a thread");
        return AVA1_ERR_INTERNAL;
    }
    o->started = 1;
    {
        int fin = ava1_op_finished_before(j); /* before encoding: see ava1_op_status_delivered */
        if (ava1_op_test_pre_encode) ava1_op_test_pre_encode(j);
        rc = ava1_op_encode_status(j, out, cap, out_len);
        if (rc == AVA1_STATUS_OK) ava1_op_status_delivered(j, fin); /* an op that already finished */
    }
    ava1_job_put(j);
    pthread_mutex_unlock(&g_run_mu);
    return rc;
}

/* ---- job.list ---- */

typedef struct {
    const uint8_t *owner;
    ava1_job_entry_t e[AVA1_MAX_JOBS];
    unsigned n;
} list_t;

static void list_one(ava1_job_t *j, void *ctx) {
    list_t *l = ctx;
    ava1_job_entry_t *e;
    int fin;
    if (memcmp(j->owner, l->owner, 32) != 0 || l->n >= AVA1_MAX_JOBS) return;
    e = &l->e[l->n++];
    memset(e, 0, sizeof *e);
    memcpy(e->job_id, j->id, 16);
    e->kind = j->kind;
    fin = __atomic_load_n(&j->finished, __ATOMIC_ACQUIRE);
    if (j->kind == AVA1_JOB_OPKIND) {
        op_t *o = op_of(j);
        if (o) {
            e->files_done = (uint32_t)__atomic_load_n(&o->ctx.files_done, __ATOMIC_RELAXED);
            e->files_total = (uint32_t)__atomic_load_n(&o->ctx.files_total, __ATOMIC_RELAXED);
            e->bytes_done = __atomic_load_n(&o->ctx.bytes_done, __ATOMIC_RELAXED);
            e->bytes_total = __atomic_load_n(&o->ctx.bytes_total, __ATOMIC_RELAXED);
        }
    } else {
        /* Plain reads of counters the job thread updates under its own lock: this walk runs
         * under the table lock and must not wait for that one (it can be held across disk I/O).
         * A value a moment old is fine for a listing. */
        e->files_done = __atomic_load_n(&j->files_done, __ATOMIC_RELAXED);
        e->files_total = j->have_manifest ? j->m.files : j->m_in.files;
        e->bytes_done = __atomic_load_n(&j->bytes_durable, __ATOMIC_RELAXED);
        e->bytes_total = j->have_manifest ? j->m.bytes : j->m_in.bytes;
        if (j->kind == AVA1_JOB_COPY && __atomic_load_n(&j->copy_move, __ATOMIC_ACQUIRE) &&
            !__atomic_load_n(&j->copy_delete_done, __ATOMIC_ACQUIRE))
            fin = 0; /* a move is running until its source is gone */
    }
    e->state = !fin ? 0 : (j->final_status == AVA1_STATUS_OK ? 1 : 2);
}

int ava1_op_list_rpc(const uint8_t owner[32], uint8_t *out, size_t cap, size_t *out_len) {
    list_t *l = calloc(1, sizeof *l);
    uint8_t *blob;
    ava1_w_t bw, w;
    ava1_job_list_result_t res;
    unsigned i;
    int rc;
    if (!l) return AVA1_ERR_INTERNAL;
    l->owner = owner;
    ava1_job_foreach(list_one, l);
    blob = malloc((size_t)AVA1_MAX_JOBS * 64u + 16u);
    if (!blob) {
        free(l);
        return AVA1_ERR_INTERNAL;
    }
    ava1_w_init(&bw, blob, (size_t)AVA1_MAX_JOBS * 64u + 16u);
    for (i = 0; i < l->n; i++) (void)ava1_job_entry_append(&bw, &l->e[i]);
    memset(&res, 0, sizeof res);
    res.jobs = blob;
    res.jobs_len = (uint32_t)bw.len;
    res.jobs_count = l->n;
    ava1_w_init(&w, out, cap);
    rc = (bw.err == 0 && ava1_job_list_result_encode(&res, &w) == 0) ? AVA1_STATUS_OK : AVA1_ERR_INTERNAL;
    *out_len = w.len;
    free(blob);
    free(l);
    return rc;
}
