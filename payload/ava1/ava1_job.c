#include "ava1_job.h"
#include "ava1_frame.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_op.h"
#include "ava1_thread.h"

static struct {
    pthread_mutex_t mu;
    ava1_job_t *jobs[AVA1_MAX_JOBS];
    /* Ids that left the table but whose destroy may still be running (threads joined,
     * journal closed): a create of the same id must not touch the job's directory until
     * the destroy finished. Cleared when the job's last reference is dropped. */
    uint8_t retiring[AVA1_MAX_JOBS][16];
    unsigned retiring_n;
} T = { .mu = PTHREAD_MUTEX_INITIALIZER };

/* Caller holds T.mu. */
static void retiring_add(const uint8_t id[16]) {
    unsigned i;
    for (i = 0; i < T.retiring_n; i++)
        if (memcmp(T.retiring[i], id, 16) == 0) return;
    /* Full: `create` refuses while the table is full (see there), so a new id cannot be
     * unlisted here — this branch is unreachable by construction. */
    if (T.retiring_n < AVA1_MAX_JOBS) memcpy(T.retiring[T.retiring_n++], id, 16);
}

/* Caller holds T.mu. */
static void retiring_clear(const uint8_t id[16]) {
    unsigned i;
    for (i = 0; i < T.retiring_n; i++)
        if (memcmp(T.retiring[i], id, 16) == 0) {
            memcpy(T.retiring[i], T.retiring[--T.retiring_n], 16);
            return;
        }
}

/* Control-frame bytes queued for jobs, all jobs together (Task 14 fix round 1). */
static uint64_t g_ctl;

int ava1_ctl_take(size_t len) {
    uint64_t cap = ava1_data_cfg()->ctl_cap ? ava1_data_cfg()->ctl_cap : AVA1_CTL_CAP;
    uint64_t cur = __atomic_load_n(&g_ctl, __ATOMIC_RELAXED);
    for (;;) {
        if (cur + len > cap) return -1;
        if (__atomic_compare_exchange_n(&g_ctl, &cur, cur + len, 0, __ATOMIC_RELAXED, __ATOMIC_RELAXED))
            return 0;
    }
}

void ava1_ctl_give(size_t len) { (void)__atomic_sub_fetch(&g_ctl, len, __ATOMIC_RELAXED); }

ava1_job_t *ava1_job_find(const uint8_t id[16]) {
    ava1_job_t *j = NULL;
    int i;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i] && memcmp(T.jobs[i]->id, id, 16) == 0) {
            j = T.jobs[i];
            j->refs++;
            break;
        }
    pthread_mutex_unlock(&T.mu);
    return j;
}

static ava1_job_t *create(const uint8_t id[16], const uint8_t owner[32], const uint8_t *sid) {
    ava1_job_t *j = calloc(1, sizeof *j);
    int i, slot = -1;
    if (!j) return NULL;
    memcpy(j->id, id, 16);
    memcpy(j->owner, owner, 32);
    j->refs = 2; /* the table's and the caller's */
    j->jnl.fd = -1;
    j->log_small = ava1_data_log_small(); /* once: a job is consistently logged or consistently per-file */
    if (sid) {
        memcpy(j->sid, sid, 16);
        j->attached = 1;
    } else {
        j->parked_at_ms = ava1_mono_ms(); /* nobody holds it yet: it ages like a parked job */
    }
    pthread_mutex_init(&j->mu, NULL);
    pthread_mutex_init(&j->jnl_mu, NULL);
    pthread_mutex_init(&j->pack_mu, NULL);
    pthread_cond_init(&j->cv, NULL);
    pthread_mutex_init(&j->cmu, NULL);
    pthread_cond_init(&j->ccv, NULL);
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++) {
        if (T.jobs[i] && memcmp(T.jobs[i]->id, id, 16) == 0) {
            slot = -2; /* raced with another create */
            break;
        }
        if (!T.jobs[i] && slot == -1) slot = i;
    }
    if (slot >= 0)
        for (i = 0; i < (int)T.retiring_n; i++)
            if (memcmp(T.retiring[i], id, 16) == 0) {
                slot = -2; /* the previous job of this id is still being destroyed */
                break;
            }
    /* The retiring table is full: this job's own unlist could not be recorded, and the
     * reopen guard is only complete while every retiring id is listed — refuse rather
     * than let a create whose destroy would silently lose the guard through. */
    if (slot >= 0 && T.retiring_n >= AVA1_MAX_JOBS) slot = -2;
    if (slot >= 0) T.jobs[slot] = j;
    pthread_mutex_unlock(&T.mu);
    if (slot < 0) {
        pthread_mutex_destroy(&j->mu);
        pthread_mutex_destroy(&j->jnl_mu);
        pthread_mutex_destroy(&j->pack_mu);
        pthread_cond_destroy(&j->cv);
        pthread_mutex_destroy(&j->cmu);
        pthread_cond_destroy(&j->ccv);
        free(j);
        return NULL;
    }
    return j;
}

ava1_job_t *ava1_job_create(const uint8_t id[16], const uint8_t owner[32]) { return create(id, owner, NULL); }

ava1_job_t *ava1_job_create_attached(const uint8_t id[16], const uint8_t owner[32], const uint8_t *sid) {
    return create(id, owner, sid);
}

static int listed_locked(const ava1_job_t *j) {
    int i;
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i] == j) return 1;
    return 0;
}

/* Caller holds T.mu. */
static void attach_locked(ava1_job_t *j, const uint8_t sid[16]) {
    pthread_mutex_lock(&j->cmu);
    memcpy(j->sid, sid, 16);
    j->attached = 1;
    pthread_mutex_unlock(&j->cmu);
    j->parked_at_ms = 0;
}

ava1_job_t *ava1_job_find_attach(const uint8_t id[16], const uint8_t sid[16]) {
    ava1_job_t *j = NULL;
    int i;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i] && memcmp(T.jobs[i]->id, id, 16) == 0) {
            j = T.jobs[i];
            j->refs++;
            attach_locked(j, sid);
            break;
        }
    pthread_mutex_unlock(&T.mu);
    return j;
}

int ava1_job_attach_sid(ava1_job_t *j, const uint8_t sid[16]) {
    int ok;
    pthread_mutex_lock(&T.mu);
    ok = listed_locked(j);
    if (ok) attach_locked(j, sid);
    pthread_mutex_unlock(&T.mu);
    return ok ? 0 : -1;
}

void ava1_job_foreach(void (*fn)(ava1_job_t *j, void *ctx), void *ctx) {
    int i;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i]) fn(T.jobs[i], ctx);
    pthread_mutex_unlock(&T.mu);
}

static void free_frames(ava1_inframe_t *f) {
    while (f) {
        ava1_inframe_t *n = f->next;
        if (f->cap) (void)ava1_frame_free(f->body, f->cap);
        else free(f->body);
        free(f);
        f = n;
    }
}

/* Stops threads, closes files, returns credit. Journal and job directory stay. */
static void job_destroy(ava1_job_t *j) {
    uint32_t i;
    ava1_work_t *w;
    pthread_mutex_lock(&j->mu);
    j->stopping = 1;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    if (j->feeder_started) { /* first: it may be handing work to the threads below */
        pthread_mutex_lock(&j->cmu);
        j->feed_stop = 1;
        pthread_cond_broadcast(&j->ccv);
        pthread_mutex_unlock(&j->cmu);
        pthread_join(j->feeder, NULL);
    }
    if (j->thread_started) pthread_join(j->thread, NULL);
    for (i = 0; i < j->nworkers; i++) pthread_join(j->workers[i], NULL);
    /* The role's reader is the only thread left that can enqueue work (a copy's reader
     * applies what it reads, Task 19): join it before the queue below is drained, or an
     * enqueue concurrent with the drain would append to a freed item. */
    if (j->role_free) j->role_free(j);
    while ((w = j->q_head) != NULL) {
        j->q_head = w->next;
        if (w->owned_cap) (void)ava1_frame_free(w->owned, w->owned_cap);
        else free(w->owned);
        free(w);
    }
    for (i = 0; i < j->pend_n; i++)
        if (j->pend_fd[i] >= 0) close(j->pend_fd[i]);
    ava1_pend_release(j->pend_n_fd);
    for (i = 0; i < j->npsegs; i++)
        if (j->psegs[i].fd >= 0) close(j->psegs[i].fd); /* the files stay: recovery sweeps them */
    if (!j->ub_excluded) ava1_unswept_add(-(int64_t)j->unswept_bytes); /* the cross-job cap no longer counts what this job held */
    free(j->psegs);
    free(j->usw);
    free(j->pend_loc);
    free(j->pend_small);
    free(j->pend_fd);
    free(j->pend_root);
    free(j->last_ranges);
    ava1_mstore_free(&j->m_in);
    if (j->lf) {
        for (i = 0; i < j->m.n; i++)
            if (j->lf[i]) {
                if (j->lf[i]->fd >= 0) close(j->lf[i]->fd);
                if (j->lf[i]->ob_fd >= 0) close(j->lf[i]->ob_fd);
                ava1_rset_clear(&j->lf[i]->written);
                ava1_rset_clear(&j->lf[i]->durable);
                free(j->lf[i]);
            }
        free(j->lf);
    }
    free(j->lfl);
    ava1_jnl_close(&j->jnl);
    ava1_bits_free(&j->done);
    ava1_mstore_free(&j->m);
    if (j->credit) ava1_budget_give(j->credit);
    free_frames(j->held_head);
    if (j->in_bytes) ava1_ctl_give(j->in_bytes); /* its inbox's frames were charged */
    free_frames(j->in_head);
    pthread_mutex_destroy(&j->mu);
    pthread_mutex_destroy(&j->jnl_mu);
    pthread_mutex_destroy(&j->pack_mu);
    pthread_cond_destroy(&j->cv);
    pthread_mutex_destroy(&j->cmu);
    pthread_cond_destroy(&j->ccv);
    free(j);
}

void ava1_job_put(ava1_job_t *j) {
    int last;
    pthread_mutex_lock(&T.mu);
    last = --j->refs == 0;
    pthread_mutex_unlock(&T.mu);
    if (last) {
        uint8_t id[16];
        memcpy(id, j->id, 16);
        job_destroy(j); /* threads joined, journal closed: only now may the id reopen */
        pthread_mutex_lock(&T.mu);
        retiring_clear(id);
        pthread_mutex_unlock(&T.mu);
    }
}

static void *put_main(void *arg) {
    ava1_job_put(arg);
    return NULL;
}

void ava1_job_put_nowait(ava1_job_t *j) {
    int done;
    pthread_mutex_lock(&T.mu);
    done = j->refs > 1;
    if (done) j->refs--;
    pthread_mutex_unlock(&T.mu);
    if (done) return;
    if (ava1_data_spawn(put_main, j) != 0) ava1_job_put(j); /* no thread: free it here */
}

static void unlist(ava1_job_t *j) {
    int i;
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i] == j) {
            T.jobs[i] = NULL;
            retiring_add(j->id); /* the destroy may still be running: the id stays taken */
            return;
        }
}

void ava1_job_park(ava1_job_t *j) {
    uint64_t now = ava1_mono_ms();
    pthread_mutex_lock(&T.mu);
    pthread_mutex_lock(&j->cmu);
    j->attached = 0;
    pthread_mutex_unlock(&j->cmu);
    j->parked_at_ms = now;
    pthread_mutex_unlock(&T.mu);
}

void ava1_job_park_session(const uint8_t sid[16]) {
    int i;
    uint64_t now = ava1_mono_ms();
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++) {
        ava1_job_t *j = T.jobs[i];
        if (j && j->attached && memcmp(j->sid, sid, 16) == 0) {
            pthread_mutex_lock(&j->cmu);
            j->attached = 0;
            pthread_mutex_unlock(&j->cmu);
            j->parked_at_ms = now;
        }
    }
    pthread_mutex_unlock(&T.mu);
}

void ava1_job_reap(uint64_t now_ms) {
    ava1_job_t *gone[AVA1_MAX_JOBS];
    int i, n = 0;
    uint64_t age = ava1_data_cfg()->park_ms ? ava1_data_cfg()->park_ms : AVA1_PARK_MS;
    uint64_t done_age = age < 10000u ? age : 10000u;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++) {
        ava1_job_t *j = T.jobs[i];
        int fin;
        /* Only a parked job can be collected: detached with a park stamp. A job created
         * detached is stamped at creation, so one nobody ever attaches ages too; a job
         * attached (find_attach, under this lock) has no stamp. `finished` is read without
         * j->mu: that lock can be held across disk I/O, and waiting for it under T.mu would
         * stall every reader thread's job lookup. It only ever goes 0 -> 1, so a stale read
         * reaps later. */
        if (!j || j->attached || j->parked_at_ms == 0) continue;
        fin = __atomic_load_n(&j->finished, __ATOMIC_ACQUIRE);
        /* Files still settling (durable-by-log) keep their job alive for the full park age: it owns the
         * sweep, and the engine polls its Status for `unswept`. */
        if (fin && __atomic_load_n(&j->unswept_n, __ATOMIC_RELAXED) && now_ms - j->parked_at_ms <= age) continue;
        /* past the park age a settling job is destroyed anyway (a stuck one must not hold a table slot
         * forever): its directory and log stay, and housekeeping's recovery pass finishes it. */
        /* The receiver marks finished before the copy role removes its source. A move
         * must remain listed throughout that delete phase, however long it takes. */
        if (j->kind == AVA1_JOB_COPY && __atomic_load_n(&j->copy_move, __ATOMIC_ACQUIRE) &&
            !__atomic_load_n(&j->copy_delete_done, __ATOMIC_ACQUIRE)) continue;
        /* A local job (JOB_COPY) is a writer with no session. Its park stamp is set at
         * creation and again when it ends, so `!fin` protects a running copy (whose stamp is
         * the creation one) from the age rule, and the window the operator can query is the
         * full park age from the moment it ended. */
        /* An operation job (job.run, ava1_op.c) is the same: stamped when it ends, so its result
         * can be read for the full park age. */
        if (j->kind == AVA1_JOB_OPKIND
                ? (fin && now_ms - j->parked_at_ms > ava1_op_keep_ms(__atomic_load_n(&j->op_delivered, __ATOMIC_ACQUIRE)))
                : j->kind == AVA1_JOB_COPY
                ? (fin && now_ms - j->parked_at_ms > age)
                : (now_ms - j->parked_at_ms > age || (fin && now_ms - j->parked_at_ms > done_age))) {
            gone[n++] = j;
            unlist(j);
        }
    }
    pthread_mutex_unlock(&T.mu);
    for (i = 0; i < n; i++) ava1_job_put(gone[i]); /* the table's reference */
}

void ava1_job_free_all(void) {
    ava1_job_t *gone[AVA1_MAX_JOBS];
    int i, n = 0;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i]) {
            gone[n++] = T.jobs[i];
            T.jobs[i] = NULL;
            retiring_add(gone[n - 1]->id);
        }
    pthread_mutex_unlock(&T.mu);
    for (i = 0; i < n; i++) ava1_job_put(gone[i]);
}

void ava1_job_free_one(const uint8_t id[16]) {
    ava1_job_t *j = NULL;
    int i;
    pthread_mutex_lock(&T.mu);
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (T.jobs[i] && memcmp(T.jobs[i]->id, id, 16) == 0) {
            j = T.jobs[i];
            T.jobs[i] = NULL;
            retiring_add(j->id); /* like unlist: the destroy may still be running */
            break;
        }
    pthread_mutex_unlock(&T.mu);
    if (j) ava1_job_put(j);
}

int ava1_job_retire(ava1_job_t *j) {
    int ok;
    pthread_mutex_lock(&T.mu);
    /* Listed, and held only by the table and the caller: nobody else can be running it. An
     * unlisted job's two references are the caller's and someone else's. */
    ok = listed_locked(j) && j->refs == 2;
    if (ok) {
        unlist(j);
        j->refs = 1;
    }
    pthread_mutex_unlock(&T.mu);
    if (!ok) {
        ava1_job_put(j);
        return -1;
    }
    ava1_job_put(j); /* the last reference: stops and joins its threads now */
    return 0;
}

void ava1_job_emit(ava1_job_t *j, uint8_t type, uint8_t flags, const uint8_t *body, size_t len) {
    ava1_emit_fn fn;
    pthread_mutex_lock(&j->cmu);
    fn = j->emit;
    pthread_mutex_unlock(&j->cmu);
    if (fn) fn(j, type, flags, body, len);
}

/* ---- test driver (fix round 2, R4) ------------------------------------------------- */

/* The retiring table full (AVA1_MAX_JOBS ids unlisted, every destroy still pending behind
 * a held reference): a create must refuse — pre-fix it succeeded and its own unlist would
 * have silently lost the reopen guard. 0, or the failed step. */
int ava1_test_retiring_full_blocks_create(void) {
    static const uint8_t OWNER[32] = { 1 };
    ava1_data_cfg_t dc;
    ava1_job_t *held[AVA1_MAX_JOBS];
    ava1_job_t *again;
    uint8_t id[16];
    int i, rc = 0;
    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "%s", "/tmp/ava1-retiring-unused");
    memset(held, 0, sizeof held);
    if (ava1_data_start(&dc) != 0) return -100;
    /* 32 detached jobs (parked from creation), each held by the caller: once the reaper
     * unlists them all, every destroy is pending and the retiring table is full. */
    for (i = 0; i < AVA1_MAX_JOBS && !rc; i++) {
        memset(id, (int)(0x51u + i), 16);
        if (!(held[i] = ava1_job_create(id, OWNER))) rc = -1;
    }
    if (!rc) {
        ava1_job_reap(ava1_mono_ms() + 3600u * 1000u); /* unlists all 32: the table fills */
        memset(id, 0x71, 16);
        if ((again = ava1_job_create(id, OWNER))) {
            rc = -2; /* a create succeeded while its unlist could never be recorded */
            ava1_job_put(again);
        }
    }
    for (i = 0; i < AVA1_MAX_JOBS; i++)
        if (held[i]) ava1_job_put(held[i]); /* destroyed now: the table drains */
    if (!rc) {
        memset(id, 0x71, 16);
        if (!(again = ava1_job_create(id, OWNER))) rc = -3; /* the drained table takes it */
        else ava1_job_put(again);
    }
    ava1_data_stop();
    return rc;
}
