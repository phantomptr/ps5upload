#include "ava1_apply.h"
#include "ava1_events.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "ava1_b3.h"
#include "ava1_data.h"
#include "ava1_frame.h" /* AVA1_FLAG_IGNORABLE */
#include "ava1_internal.h"
#include "ava1_platform.h"
#include "ava1_send.h" /* ava1_send_timing_enabled: the shared timing opt-in */
#include "ava1_thread.h"

#define CREDIT_FLUSH (4u << 20)
#define BATCH_BYTES (64ull << 20)
#define BATCH_MS 250u
#define TICK_MS 25u
#define MAX_ITEMS 2000u
#define PATH_CAP AVA1_PATH_CAP

typedef struct {
    ava1_job_t *j;
    uint32_t idx;
} wctx_t;

void (*ava1_apply_hook)(ava1_job_t *j, int point, uint32_t id);
int (*ava1_apply_fault)(ava1_job_t *j, int point, uint32_t id);
int ava1_apply_hold_batches;
#define HOOK(j, point, id) \
    do { \
        if (ava1_apply_hook) ava1_apply_hook((j), (point), (id)); \
    } while (0)

static int is_stopping(ava1_job_t *j) {
    int s;
    pthread_mutex_lock(&j->mu);
    s = j->stopping;
    pthread_mutex_unlock(&j->mu);
    return s;
}

int ava1_apply_jnl_append(ava1_job_t *j, uint8_t kind, const uint8_t *body, size_t len) {
    int rc;
    pthread_mutex_lock(&j->jnl_mu);
    rc = ava1_jnl_append(&j->jnl, kind, body, len);
    pthread_mutex_unlock(&j->jnl_mu);
    return rc;
}

int ava1_sync_dir(const char *dir) {
    int fd = open(dir, O_RDONLY | O_DIRECTORY), rc = 0;
    if (fd < 0) return errno;
    rc = ava1_fsync_retry(fd, NULL, NULL, NULL);
    if (rc == EINVAL || rc == ENOTSUP || rc == EOPNOTSUPP) rc = 0; /* a filesystem that cannot sync a directory */
    close(fd);
    return rc;
}

/* A final rename that failed because something is already where it goes. */
static int in_the_way(int e) { return e == EEXIST || e == ENOTEMPTY || e == ENOTDIR || e == EISDIR; }

#define groups_of ava1_groups_of
#define sync_dir ava1_sync_dir
#define parent_of ava1_parent_of
#define mkparents ava1_mkparents

/* A path that does not fit comes back empty (open() then fails) rather than truncated
 * (which would name some other file). */
void ava1_apply_path(const ava1_job_t *j, uint32_t id, int part, char *out, size_t cap) {
    const char *rel = ava1_mstore_path(&j->m, id);
    int n;
    if (j->flags & AVA1_JF_SINGLE_FILE) n = snprintf(out, cap, "%s%s", j->root, part ? ".ava-part" : "");
    else if (rel) n = snprintf(out, cap, "%s/%s%s", j->base, rel, part && !j->staged ? ".ava-part" : "");
    else n = -1;
    if (cap && (n < 0 || (size_t)n >= cap)) out[0] = 0;
}

static void ob_path(const ava1_job_t *j, uint32_t id, char *out, size_t cap) {
    snprintf(out, cap, "%s/%u.ob", j->dir, id);
}

void ava1_parent_of(const char *p, char *out, size_t cap) {
    const char *s = strrchr(p, '/');
    if (!s) snprintf(out, cap, ".");
    else if (s == p) snprintf(out, cap, "/");
    else snprintf(out, cap, "%.*s", (int)(s - p), p);
}

/* mkdir -p up to the last '/' (and of the whole path when `self`). */
static int mkdirs_to(const char *path, int self, int sync) {
    char p[PATH_CAP], parent[PATH_CAP];
    size_t i, n;
    if (!path[0] || (n = strlen(path)) >= sizeof p) return -ENAMETOOLONG;
    snprintf(p, sizeof p, "%s", path);
    for (i = 1; i <= n; i++) {
        int rc;
        if (i < n ? p[i] != '/' : !self) continue;
        p[i] = 0;
        if (mkdir(p, AVA1_CONSOLE_DIR_MODE) == 0) {
            if (sync) {
                parent_of(p, parent, sizeof parent);
                if ((rc = sync_dir(parent)) != 0) return -rc;
            }
        } else if (errno != EEXIST) {
            return -errno;
        }
        if (i < n) p[i] = '/';
    }
    return 0;
}

int ava1_mkparents(const char *path) { return mkdirs_to(path, 0, 0); }
int ava1_mkdirs(const char *path, int sync) { return mkdirs_to(path, 1, sync); }

/* ---- the large-file index (see ava1_job.h: lfl) ----------------------------------- */

static int lfl_push(ava1_job_t *j, uint32_t id) {
    if (j->lfl_all) return 0; /* every scan walks the whole manifest anyway */
    if (j->lfl_n == j->lfl_cap) {
        uint32_t c = j->lfl_cap ? j->lfl_cap * 2 : 64;
        uint32_t *q = realloc(j->lfl, (size_t)c * sizeof *q);
        if (!q) return -1;
        j->lfl = q;
        j->lfl_cap = c;
    }
    j->lfl[j->lfl_n++] = id;
    return 0;
}

ava1_lfile_t *ava1_lfile_get(ava1_job_t *j, uint32_t id) {
    int fresh = 0;
    if (!j->lf[id]) {
        j->lf[id] = calloc(1, sizeof(ava1_lfile_t));
        if (!j->lf[id]) return NULL;
        j->lf[id]->fd = j->lf[id]->ob_fd = -1;
        fresh = 1;
    }
    if (!j->lf[id]->in_list) {
        if (lfl_push(j, id) != 0) {
            /* Out of memory: the index can no longer be trusted, so the scans take every
             * manifest entry from now on (slower, never wrong). */
            j->lfl_all = 1;
            (void)fresh;
        }
        j->lf[id]->in_list = 1;
    }
    return j->lf[id];
}

void ava1_lflist_rebuild(ava1_job_t *j) {
    uint32_t i;
    j->lfl_n = 0;
    j->lfl_all = 0;
    /* the open-descriptor list follows the files to their new ids */
    j->lfo_n = 0;
    j->lf_fds = 0;
    for (i = 0; j->lf && i < j->m.n; i++) {
        ava1_lfile_t *lf = j->lf[i];
        if (!lf || lf->fd < 0) continue;
        if (j->lfo_n == j->lfo_cap) {
            uint32_t c = j->lfo_cap ? j->lfo_cap * 2 : 16;
            uint32_t *q = realloc(j->lfo, (size_t)c * sizeof *q);
            if (!q) { /* cannot track it: close it, it reopens on demand */
                if (lf->fd >= 0) close(lf->fd);
                if (lf->ob_fd >= 0) close(lf->ob_fd);
                lf->fd = lf->ob_fd = -1;
                if (lf->held) ava1_lf_release(lf->held);
                lf->held = 0;
                continue;
            }
            j->lfo = q;
            j->lfo_cap = c;
        }
        j->lfo[j->lfo_n++] = i;
        j->lf_fds += lf->held;
    }
    for (i = 0; j->lf && i < j->m.n; i++) {
        if (!j->lf[i]) continue;
        j->lf[i]->in_list = 1;
        if (lfl_push(j, i) != 0) {
            j->lfl_all = 1;
            return;
        }
    }
}

void ava1_lflist_reset(ava1_job_t *j, int release) {
    j->lfl_n = 0;
    j->lfl_all = 0;
    if (release) {
        free(j->lfl);
        j->lfl = NULL;
        j->lfl_cap = 0;
    }
}

static int u32cmp(const void *a, const void *b);
static int stopping_cb(void *a);
static void run_commit(ava1_job_t *j, uint32_t id);
static void run_work(ava1_job_t *j, ava1_work_t *w);

/* A lf with nothing left for a batch, a commit or a snapshot to do. */
static int lf_idle(const ava1_job_t *j, uint32_t id, const ava1_lfile_t *lf) {
    return (lf->committed || ava1_bits_get(&j->done, id)) && !lf->written.n && !(lf->has_root && !lf->root_journaled);
}

/* The ids to scan, ascending and unique, as a malloc'd array (caller holds j->mu). `*n` is
 * 0 with NULL when there is nothing to do, or UINT32_MAX with NULL when out of memory. */
static uint32_t *lfl_snapshot(ava1_job_t *j, uint32_t *n) {
    uint32_t *v, k, c = 0;
    *n = 0;
    if (j->lfl_all) {
        if (!j->m.n) return NULL;
        v = malloc((size_t)j->m.n * sizeof *v);
        if (!v) {
            *n = UINT32_MAX;
            return NULL;
        }
        for (k = 0; k < j->m.n; k++)
            if (j->lf[k]) v[c++] = k;
    } else {
        if (!j->lfl_n) return NULL;
        v = malloc((size_t)j->lfl_n * sizeof *v);
        if (!v) {
            *n = UINT32_MAX;
            return NULL;
        }
        memcpy(v, j->lfl, (size_t)j->lfl_n * sizeof *v);
        qsort(v, j->lfl_n, sizeof *v, u32cmp);
        for (k = 0; k < j->lfl_n; k++) {
            if (v[k] >= j->m.n || !j->lf[v[k]]) continue; /* stale: its lf was freed */
            if (c && v[c - 1] == v[k]) continue;
            v[c++] = v[k];
        }
    }
    if (!c) {
        free(v);
        return NULL;
    }
    *n = c;
    return v;
}

/* After a batch: the index keeps only the files that still have work (ids in `v`, sorted). */
static void lfl_prune(ava1_job_t *j, const uint32_t *v, uint32_t n) {
    uint32_t k;
    if (j->lfl_all) return;
    j->lfl_n = 0;
    for (k = 0; k < n; k++) {
        ava1_lfile_t *lf = j->lf[v[k]];
        if (lf_idle(j, v[k], lf)) lf->in_list = 0;
        else j->lfl[j->lfl_n++] = v[k]; /* never more than it held */
    }
}

static uint64_t mono_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
}

static int write_all(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t k = write(fd, p, n);
        if (k < 0) {
            if (errno == EINTR) continue;
            return -errno;
        }
        p += k;
        n -= (size_t)k;
    }
    return 0;
}

static int pwrite_all(int fd, const uint8_t *p, size_t n, uint64_t off) {
    while (n) {
        ssize_t k = pwrite(fd, p, n, (off_t)off);
        if (k < 0) {
            if (errno == EINTR) continue;
            return -errno;
        }
        p += k;
        n -= (size_t)k;
        off += (uint64_t)k;
    }
    return 0;
}

/* ---- messages -------------------------------------------------------------------- */

static void emit_credit(ava1_job_t *j, uint64_t n) {
    ava1_credit_t c;
    uint8_t b[64];
    ava1_w_t w;
    memset(&c, 0, sizeof c);
    memcpy(c.job_id, j->id, 16);
    c.bytes = n;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_credit_encode(&c, &w) == 0) ava1_job_emit(j, AVA1_TYPE_CREDIT, 0, b, w.len);
}

static void emit_retry(ava1_job_t *j, uint32_t id, uint16_t reason) {
    ava1_file_retry_t r;
    uint8_t b[64];
    ava1_w_t w;
    memset(&r, 0, sizeof r);
    memcpy(r.job_id, j->id, 16);
    r.file_id = id;
    r.reason = reason;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_file_retry_encode(&r, &w) == 0) ava1_job_emit(j, AVA1_TYPE_FILE_RETRY, 0, b, w.len);
}

static void emit_done(ava1_job_t *j) {
    ava1_job_done_t d;
    uint8_t b[256];
    ava1_w_t w;
    memset(&d, 0, sizeof d);
    memcpy(d.job_id, j->id, 16);
    d.status = j->final_status;
    d.files = j->files_done;
    d.bytes = j->bytes_durable;
    pthread_mutex_lock(&j->mu);
    if (j->unswept_n || j->usw_n || j->sweeps_inflight) { /* the log is durable; files settle behind JobDone */
        d.has_settling = 1;
        d.settling = 1;
    }
    pthread_mutex_unlock(&j->mu);
    if (j->message[0]) {
        d.has_message = 1;
        d.message = (const uint8_t *)j->message;
        d.message_len = (uint16_t)strlen(j->message);
    }
    ava1_w_init(&w, b, sizeof b);
    if (ava1_job_done_encode(&d, &w) == 0) ava1_job_emit(j, AVA1_TYPE_JOB_DONE, 0, b, w.len);
}

/* Durable{files, ranges}, split so each message stays under a control frame. */
static void emit_durable(ava1_job_t *j, const ava1_file_run_t *runs, uint32_t nr, const ava1_file_range_t *rg,
                         uint32_t ng) {
    uint8_t *fb = malloc(48u * MAX_ITEMS), *rb = malloc(48u * MAX_ITEMS), *out = malloc(64u * 1024u);
    uint32_t i = 0, k = 0;
    if (!fb || !rb || !out) goto done;
    while (i < nr || k < ng || (nr == 0 && ng == 0 && i == 0 && k == 0)) {
        ava1_w_t fw, rw, w;
        ava1_durable_t d;
        uint32_t items = 0;
        ava1_w_init(&fw, fb, 48u * MAX_ITEMS);
        ava1_w_init(&rw, rb, 48u * MAX_ITEMS);
        for (; i < nr && items < MAX_ITEMS; i++, items++) (void)ava1_file_run_append(&fw, &runs[i]);
        for (; k < ng && items < MAX_ITEMS; k++, items++) (void)ava1_file_range_append(&rw, &rg[k]);
        memset(&d, 0, sizeof d);
        memcpy(d.job_id, j->id, 16);
        d.files = fb;
        d.files_len = (uint32_t)fw.len;
        d.ranges = rb;
        d.ranges_len = (uint32_t)rw.len;
        ava1_w_init(&w, out, 64u * 1024u);
        if (ava1_durable_encode(&d, &w) == 0) ava1_job_emit(j, AVA1_TYPE_DURABLE, 0, out, w.len);
        if (nr == 0 && ng == 0) break;
    }
done:
    free(fb);
    free(rb);
    free(out);
}

void ava1_apply_status(ava1_job_t *j) {
    ava1_status_t st;
    char sm[96];
    uint8_t b[192];
    ava1_w_t w;
    uint8_t bn;
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    memset(&st, 0, sizeof st);
    memcpy(st.job_id, j->id, 16);
    pthread_mutex_lock(&j->mu);
    if (j->ticks && j->q_busy_ticks * 2 >= j->ticks)
        bn = (j->want_workers >= cfg->workers_max || j->tune.hold) ? AVA1_BN_DISK : AVA1_BN_WORKERS;
    else bn = AVA1_BN_NETWORK;
    j->ticks = j->q_busy_ticks = 0;
    st.files_done = j->files_done;
    st.files_total = j->m.files;
    st.bytes_received = j->bytes_received;
    st.bytes_durable = j->bytes_durable;
    st.bytes_total = j->m.bytes;
    st.bottleneck = bn;
    st.workers = j->want_workers;
    st.lanes = j->lanes;
    if (j->unswept_n) {
        st.has_unswept = 1;
        st.unswept = j->unswept_n;
    }
    if (j->sweep_err) { /* files cannot be made durable: the sender must hear it (SPEC.md §15.7) */
        st.has_code = 1;
        st.code = AVA1_ERR_IO;
        snprintf(sm, sizeof sm, "%s", j->sweep_msg);
        st.has_current = 1;
        st.current = (const uint8_t *)sm;
        st.current_len = (uint16_t)strlen(sm);
    }
    pthread_mutex_unlock(&j->mu);
    ava1_w_init(&w, b, sizeof b);
    if (ava1_status_encode(&st, &w) == 0) ava1_job_emit(j, AVA1_TYPE_STATUS, AVA1_FLAG_IGNORABLE, b, w.len);
}

size_t ava1_apply_summary(const ava1_job_t *j, char *out, size_t cap) {
    double wall = (double)(mono_us() - j->start_us);
    int n;
    if (wall < 1.0) wall = 1.0;
#define SHARE(us) (100.0 * (double)(us) / wall)
    n = snprintf(out, cap,
                 "[ava1] job %02x%02x%02x%02x: finished in %.0f ms, %llu batches, share of time: scan %.1f%% data fsync "
                 "%.1f%% dirs %.1f%% journal %.1f%% (job thread); commit %.1f%% preallocate %.1f%% (summed over workers)",
                 j->id[0], j->id[1], j->id[2], j->id[3], wall / 1000.0, (unsigned long long)j->tot_batches,
                 SHARE(j->tot_scan_us), SHARE(j->tot_data_us), SHARE(j->tot_dirs_us), SHARE(j->tot_jnl_us),
                 SHARE(__atomic_load_n(&j->tot_commit_us, __ATOMIC_RELAXED)),
                 SHARE(__atomic_load_n(&j->pre_us, __ATOMIC_RELAXED)));
#undef SHARE
    if (n < 0) return 0;
    return (size_t)n < cap ? (size_t)n : (cap ? cap - 1 : 0);
}

/* Every end of a job goes through here, success included (finish() passes AVA1_STATUS_OK):
 * one place sets `finished`, journals Done and sends JobDone, exactly once. */
void ava1_apply_fail(ava1_job_t *j, uint16_t status, const char *what, int err, int journal_done) {
    int first;
    int done_written = 0;
    uint64_t credit = 0;
    pthread_mutex_lock(&j->mu);
    first = !j->finished;
    if (first) {
        j->finished = 1;
        j->final_status = status;
        snprintf(j->message, sizeof j->message, "%s%s%s", what, err ? ": " : "", err ? strerror(err) : "");
        /* An ended job takes no more frames (reserve now fails): its credit goes back to
         * the data budget at once, not when the job is finally freed. */
        credit = j->credit;
        j->credit = 0;
    }
    /* Commits already queued or running must finish (a queued one starts, sees `finished` and
     * returns) before Done is appended and JobDone sent: nothing may be journaled or acknowledged
     * after the job's end. Not while stopping: workers no longer run then. */
    while (first && j->commits_inflight && !j->stopping) pthread_cond_wait(&j->cv, &j->mu);
    pthread_mutex_unlock(&j->mu);
    if (!first) return;
    ava1_log_job_event(status == AVA1_STATUS_OK ? "done" : "fail", j, status);
    if (j->start_us) { /* the apply engine ran: where its time went (a download or a move has none) */
        char line[400];
        (void)ava1_apply_summary(j, line, sizeof line);
        fprintf(stderr, "%s\n", line);
    }
    if (__atomic_load_n(&j->pre_files, __ATOMIC_RELAXED))
        fprintf(stderr, "[ava1] job %02x%02x%02x%02x: preallocate took %llu ms for %llu MiB\n", j->id[0], j->id[1],
                j->id[2], j->id[3], (unsigned long long)(__atomic_load_n(&j->pre_us, __ATOMIC_RELAXED) / 1000u),
                (unsigned long long)(__atomic_load_n(&j->pre_bytes, __ATOMIC_RELAXED) >> 20));
    if (credit) ava1_budget_give(credit);
    if (journal_done) {
        ava1_jnl_done_t d;
        uint8_t b[16];
        ava1_w_t w;
        d.status = status;
        ava1_w_init(&w, b, sizeof b);
        if (ava1_jnl_done_encode(&d, &w) == 0 && ava1_apply_jnl_append(j, AVA1_JNL_DONE, b, w.len) == 0)
            done_written = 1;
    }
    pthread_mutex_lock(&j->mu);
    /* A journaled Done(OK) that a restart replayed is itself the proof: the rename, the
     * directory sync and the Done append all succeeded before the crash. */
    j->durable_ok = status == AVA1_STATUS_OK && err == 0 && (done_written || j->replay_done);
    if (status == AVA1_STATUS_OK && journal_done && !done_written) {
        j->final_status = AVA1_ERR_IO;
        snprintf(j->message, sizeof j->message, "journal append failed");
    }
    pthread_mutex_unlock(&j->mu);
    emit_done(j);
}

void ava1_apply_done_again(ava1_job_t *j) {
    if (j->finished) emit_done(j);
}

/* Workers never end the job themselves: they record the first failure here (a nonzero
 * final_status on an unfinished job) and the job thread ends it, so JobDone has one
 * emitter and can never overtake a Durable the job thread is still sending. */
static void worker_fail(ava1_job_t *j, uint16_t status, const char *what, int err) {
    pthread_mutex_lock(&j->mu);
    if (!j->finished && !j->final_status) {
        j->final_status = status;
        snprintf(j->message, sizeof j->message, "%s%s%s", what, err ? ": " : "", err ? strerror(err) : "");
    }
    pthread_mutex_unlock(&j->mu);
}

/* ---- credit ------------------------------------------------------------------------ */

int ava1_apply_reserve(ava1_job_t *j, size_t n) {
    int ok;
    pthread_mutex_lock(&j->mu);
    ok = j->outstanding + n <= j->credit;
    if (ok) j->outstanding += n;
    pthread_mutex_unlock(&j->mu);
    return ok ? 0 : -1;
}

void ava1_apply_unreserve(ava1_job_t *j, size_t n) {
    pthread_mutex_lock(&j->mu);
    j->outstanding -= n;
    pthread_mutex_unlock(&j->mu);
}

static void give_back(ava1_job_t *j, size_t n) {
    uint64_t flush = 0;
    pthread_mutex_lock(&j->mu);
    j->outstanding -= n;
    j->credit_back += n;
    if (j->credit_back >= CREDIT_FLUSH) {
        flush = j->credit_back;
        j->credit_back = 0;
    }
    pthread_mutex_unlock(&j->mu);
    if (flush) emit_credit(j, flush);
}

/* ---- the queue --------------------------------------------------------------------- */

static int enqueue(ava1_job_t *j, ava1_work_t *w, int front) {
    pthread_mutex_lock(&j->mu);
    if (front) {
        w->next = j->q_head;
        j->q_head = w;
        if (!j->q_tail) j->q_tail = w;
    } else {
        w->next = NULL;
        if (j->q_tail) j->q_tail->next = w;
        else j->q_head = w;
        j->q_tail = w;
    }
    j->q_len++;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    return 0;
}

static void release_owned(uint8_t *owned, size_t cap) {
    if (cap) (void)ava1_frame_free(owned, cap);
    else free(owned);
}

int ava1_apply_chunk(ava1_job_t *j, uint8_t *owned, size_t owned_len, uint32_t file_id, uint64_t off,
                     const uint8_t *data, size_t len) {
    return ava1_apply_chunk_pooled(j, owned, owned_len, 0, file_id, off, data, len);
}

int ava1_apply_bundle(ava1_job_t *j, uint8_t *owned, size_t owned_len, const ava1_bundle_t *b) {
    return ava1_apply_bundle_pooled(j, owned, owned_len, 0, b);
}

int ava1_apply_chunk_pooled(ava1_job_t *j, uint8_t *owned, size_t owned_len, size_t owned_cap, uint32_t file_id,
                            uint64_t off, const uint8_t *data, size_t len) {
    ava1_work_t *w;
    uint64_t size;
    /* Inside the file (written so a hostile offset cannot wrap), group-aligned, and a whole
     * number of groups unless it ends the file: write_chunk hashes whole groups out of `data`. */
    if (file_id >= j->m.n || j->m.e[file_id].kind != AVA1_ENTRY_FILE || off % AVA1_GROUP_LEN != 0 ||
        off > (size = j->m.e[file_id].size) || len > size - off ||
        (len % AVA1_GROUP_LEN != 0 && off + len != size)) {
        release_owned(owned, owned_cap);
        give_back(j, owned_len);
        return AVA1_E_PROTO;
    }
    w = calloc(1, sizeof *w);
    if (!w) {
        release_owned(owned, owned_cap);
        give_back(j, owned_len);
        return AVA1_E_IO;
    }
    w->kind = AVA1_W_CHUNK;
    w->owned = owned;
    w->owned_cap = owned_cap;
    w->owned_len = owned_len;
    w->file_id = file_id;
    w->offset = off;
    w->data = data;
    w->len = len;
    return enqueue(j, w, 0);
}

int ava1_apply_bundle_pooled(ava1_job_t *j, uint8_t *owned, size_t owned_len, size_t owned_cap,
                            const ava1_bundle_t *b) {
    ava1_work_t *w;
    ava1_r_t it;
    ava1_bundle_record_t r;
    uint32_t n = 0;
    int k;
    /* The whole bundle up front: every record well formed and naming a file, and as many
     * as it claims. A bad one is refused here, never half-applied by a worker. */
    ava1_r_init(&it, b->records, b->records_len);
    while ((k = ava1_bundle_record_next(&it, &r)) == 1) {
        if (r.file_id >= j->m.n || j->m.e[r.file_id].kind != AVA1_ENTRY_FILE) {
            k = -1;
            break;
        }
        n++;
    }
    if (k != 0 || n != b->records_count) {
        release_owned(owned, owned_cap);
        give_back(j, owned_len);
        return AVA1_E_PROTO;
    }
    w = calloc(1, sizeof *w);
    if (!w) {
        release_owned(owned, owned_cap);
        give_back(j, owned_len);
        return AVA1_E_IO;
    }
    w->kind = AVA1_W_BUNDLE;
    w->owned = owned;
    w->owned_cap = owned_cap;
    w->owned_len = owned_len;
    w->data = b->records;
    w->len = b->records_len;
    return enqueue(j, w, 0);
}

int ava1_apply_root(ava1_job_t *j, uint32_t file_id, const uint8_t root[32]) {
    int rc = 0;
    pthread_mutex_lock(&j->mu);
    if (file_id >= j->m.n || j->m.e[file_id].kind != AVA1_ENTRY_FILE) {
        rc = AVA1_E_PROTO;
    } else {
        if (!ava1_lfile_get(j, file_id)) rc = AVA1_E_IO;
        else if (!j->lf[file_id]->has_root || memcmp(j->lf[file_id]->root, root, 32) != 0) {
            memcpy(j->lf[file_id]->root, root, 32);
            j->lf[file_id]->has_root = 1;
            j->lf[file_id]->root_journaled = 0;
            j->roots_new++;
        }
    }
    pthread_mutex_unlock(&j->mu);
    return rc;
}

int ava1_apply_parallel(ava1_job_t *j, void (*fn)(ava1_job_t *, void *, uint32_t), void *arg, uint32_t n) {
    uint32_t i;
    int dropped = 0;
    pthread_mutex_lock(&j->mu);
    j->calls_left += n;
    pthread_mutex_unlock(&j->mu);
    for (i = 0; i < n; i++) {
        ava1_work_t *w = calloc(1, sizeof *w);
        if (!w) { /* run it here rather than lose it */
            fn(j, arg, i);
            pthread_mutex_lock(&j->mu);
            j->calls_left--;
            pthread_mutex_unlock(&j->mu);
            continue;
        }
        w->kind = AVA1_W_CALL;
        w->fn = fn;
        w->arg = arg;
        w->i = i;
        enqueue(j, w, 1);
    }
    pthread_mutex_lock(&j->mu);
    while (j->calls_left) {
        if (j->stopping) {
            /* Workers no longer take work: drop our items they never started, then wait
             * only for the ones running (they still read `arg`, which the caller frees). */
            ava1_work_t **pp = &j->q_head, *prev = NULL;
            while (*pp) {
                ava1_work_t *w = *pp;
                if (w->kind == AVA1_W_CALL && w->fn == fn && w->arg == arg) {
                    *pp = w->next;
                    if (j->q_tail == w) j->q_tail = prev;
                    j->q_len--;
                    j->calls_left--;
                    dropped = 1;
                    free(w);
                } else {
                    prev = w;
                    pp = &w->next;
                }
            }
            if (!j->calls_left) break;
        }
        pthread_cond_wait(&j->cv, &j->mu);
    }
    pthread_mutex_unlock(&j->mu);
    return dropped ? -1 : 0;
}

/* ---- applying ---------------------------------------------------------------------- */

/* Opens a large file's part file and outboard, preallocating a new part file (review 003
 * §2.1: this used to run under j->mu, so a minutes-long preallocation on a slow drive stalled
 * every other worker and the feeder). It touches no job lock; the caller publishes the
 * descriptors under j->mu. create == 0 (the commit's reopen): a missing part file or outboard
 * is ENOENT, never a new empty file — an empty part would hash to nothing and be renamed over
 * the real file. 0, or an errno with both descriptors closed. */
static int lfile_open_fds(ava1_job_t *j, uint32_t id, int create, int *fdp, int *obp) {
    const ava1_ment_t *e = &j->m.e[id];
    char path[PATH_CAP];
    struct stat st;
    int fd, ob = -1, err;
    *fdp = *obp = -1;
    ava1_apply_path(j, id, 1, path, sizeof path);
    if (!path[0]) return ENAMETOOLONG;
    fd = open(path, O_RDWR | (create ? O_CREAT : 0) | O_NOFOLLOW, 0600);
    if (fd < 0 && create && errno == ENOENT && mkparents(path) == 0) fd = open(path, O_RDWR | O_CREAT | O_NOFOLLOW, 0600);
    if (fd < 0) return errno;
    if (fstat(fd, &st) == 0 && st.st_size == 0 && e->size > 0) {
        /* the old protocol preallocated for the same reason (runtime.c, "a sparse file on PS5 UFS collapses
         * from 60 to 2-3 MiB/s under dirty-buffer throttling"): keep it, and keep ENOSPC first. */
        uint64_t t0, dt;
        int rc;
        HOOK(j, AVA1_HOOK_PREALLOC, id);
        t0 = mono_us();
        rc = ava1_apply_fault ? ava1_apply_fault(j, AVA1_HOOK_PREALLOC, id) : 0; /* tests: an injected errno */
        if (!rc) rc = ava1_platform_preallocate(fd, e->size);
        dt = mono_us() - t0;
        __atomic_add_fetch(&j->pre_us, dt, __ATOMIC_RELAXED);
        __atomic_add_fetch(&j->pre_bytes, e->size, __ATOMIC_RELAXED);
        __atomic_add_fetch(&j->pre_files, 1, __ATOMIC_RELAXED);
        /* more than a second per GiB: say so once per job, so CUTOVER can record it per drive */
        if (dt * (1ull << 30) > 1000000ull * e->size && !__atomic_exchange_n(&j->pre_slow_logged, 1, __ATOMIC_RELAXED))
            fprintf(stderr, "[ava1] job %02x%02x%02x%02x: preallocation on this drive is slow (%llu ms for %llu MiB)\n",
                    j->id[0], j->id[1], j->id[2], j->id[3], (unsigned long long)(dt / 1000u),
                    (unsigned long long)(e->size >> 20));
        if (rc == ENOSPC) {
            close(fd);
            return ENOSPC;
        }
    }
    if (groups_of(e->size) >= 2) {
        ob_path(j, id, path, sizeof path);
        ob = open(path, O_RDWR | (create ? O_CREAT : 0), 0600);
        if (ob < 0) { /* never leave a data fd open without its outboard: CVs would go unwritten */
            err = errno;
            close(fd);
            return err;
        }
        if (fstat(ob, &st) == 0 && (uint64_t)st.st_size < groups_of(e->size) * 32u)
            (void)ftruncate(ob, (off_t)(groups_of(e->size) * 32u));
    }
    *fdp = fd;
    *obp = ob;
    return 0;
}

void ava1_lf_close_fds(ava1_job_t *j, uint32_t id, ava1_lfile_t *lf) {
    uint32_t k;
    if (lf->fd >= 0) close(lf->fd);
    if (lf->ob_fd >= 0) close(lf->ob_fd);
    lf->fd = lf->ob_fd = -1;
    if (lf->held) {
        ava1_lf_release(lf->held);
        if (j->lf_fds >= lf->held) j->lf_fds -= lf->held;
        else j->lf_fds = 0;
        lf->held = 0;
    }
    for (k = 0; k < j->lfo_n; k++)
        if (j->lfo[k] == id) {
            j->lfo[k] = j->lfo[--j->lfo_n];
            break;
        }
}

/* Closes one large file that has nothing a batch, a commit or a writer needs its descriptors for:
 * no bytes written and not yet synced, no write in flight, no commit, no batch fsyncing the
 * descriptors it took (it uses them without j->mu). Reopened by lfile_open when a chunk or the
 * commit comes. Caller holds j->mu. 1 = one closed. */
static int lf_evict_one(ava1_job_t *j) {
    uint32_t k;
    if (j->syncing) return 0;
    for (k = 0; k < j->lfo_n; k++) {
        uint32_t id = j->lfo[k];
        ava1_lfile_t *lf = j->lf[id];
        if (!lf || lf->fd < 0 || lf->opening || lf->committing || lf->committed || lf->writers || lf->written.n) continue;
        /* Has its root AND every byte durable: its commit is about to run and needs them open. A root
         * alone does not mean that: it comes on the control connection and can arrive before the file's
         * last chunks are applied. Those chunks sit in the queue behind the workers waiting here, so such
         * a file can neither commit nor (before this check) be closed, and once a job's whole share was
         * files like it every worker waited for a slot forever (a 35k-file game froze on the console). */
        if (lf->has_root && (j->m.e[id].size == 0 || ava1_rset_covers(&lf->durable, 0, j->m.e[id].size))) continue;
        ava1_lf_close_fds(j, id, lf);
        return 1;
    }
    return 0;
}

/* Takes `need` descriptor slots for a large file about to open: this job's share and the global one.
 * Closes idle files of this job to make room; with none to close, waits (running queued sync stripes,
 * as the small-file gate does: the batch that makes files idle needs workers) and asks the job thread
 * for an early batch. Caller holds j->mu. 0, or EINTR when the job is stopping. */
static int lf_slots(ava1_job_t *j, uint32_t need, int force) {
    int waited = 0;
    if (force) { /* a commit's reopen (a resumed job): it is what gives slots back, so it never waits for one */
        ava1_lf_force_reserve(need);
        j->lf_fds += need;
        return 0;
    }
    for (;;) {
        ava1_work_t *w, **pp, *prev = NULL;
        if (j->stopping) {
            if (waited) j->lf_wait--;
            return EINTR;
        }
        if (j->lf_fds + need <= ava1_lf_job_share()) {
            if (ava1_lf_try_reserve(need)) break;
        }
        if (lf_evict_one(j)) continue;
        if (!waited) {
            waited = 1;
            j->lf_wait++;
        }
        /* Run queued sync stripes and commits ourselves, wherever they sit in the queue: they are what
         * frees slots, and if every worker waited here behind chunk items nobody would run them. */
        for (pp = &j->q_head; (w = *pp) != NULL; prev = w, pp = &w->next)
            if (w->kind == AVA1_W_CALL || w->kind == AVA1_W_COMMIT) break;
        if (w) {
            *pp = w->next;
            if (j->q_tail == w) j->q_tail = prev;
            j->q_len--;
            j->busy++;
            pthread_mutex_unlock(&j->mu);
            if (w->kind == AVA1_W_CALL) {
                w->fn(j, w->arg, w->i);
                pthread_mutex_lock(&j->mu);
                if (--j->calls_left == 0) pthread_cond_broadcast(&j->cv);
            } else {
                run_work(j, w);
                pthread_mutex_lock(&j->mu);
            }
            j->busy--;
            free(w);
        } else {
            pthread_mutex_unlock(&j->mu);
            ava1_platform_sleep_ms(2);
            pthread_mutex_lock(&j->mu);
        }
    }
    if (waited) j->lf_wait--;
    j->lf_fds += need;
    return 0;
}

/* The large file's state with open descriptors. Caller holds j->mu, which is released while
 * the files are opened and preallocated (another worker wanting the same file waits on
 * `opening`) and held again on return. NULL + *err on failure. */
static ava1_lfile_t *lfile_open(ava1_job_t *j, uint32_t id, int *err, int create) {
    ava1_lfile_t *lf = ava1_lfile_get(j, id);
    int fd, ob, rc;
    uint32_t need;
    *err = 0;
    if (!lf) {
        *err = ENOMEM;
        return NULL;
    }
    while (lf->opening && !j->stopping) pthread_cond_wait(&j->cv, &j->mu);
    if (lf->fd >= 0) return lf;
    if (lf->opening) { /* stopping while another thread still opens it */
        *err = EINTR;
        return NULL;
    }
    lf->opening = 1;
    need = ava1_groups_of(j->m.e[id].size) >= 2 ? 2u : 1u;
    rc = lf_slots(j, need, !create); /* may release j->mu while it waits; this lf stays `opening` */
    if (rc) {
        lf->opening = 0;
        pthread_cond_broadcast(&j->cv);
        *err = rc;
        return NULL;
    }
    pthread_mutex_unlock(&j->mu);
    rc = lfile_open_fds(j, id, create, &fd, &ob);
    pthread_mutex_lock(&j->mu);
    lf->opening = 0;
    pthread_cond_broadcast(&j->cv);
    if (rc == 0 && j->lfo_n == j->lfo_cap) {
        uint32_t c = j->lfo_cap ? j->lfo_cap * 2 : 16;
        uint32_t *q = realloc(j->lfo, (size_t)c * sizeof *q);
        if (q) {
            j->lfo = q;
            j->lfo_cap = c;
        } else {
            close(fd);
            if (ob >= 0) close(ob);
            rc = ENOMEM;
        }
    }
    if (rc) {
        ava1_lf_release(need); /* the slots lf_slots took */
        j->lf_fds -= need;
        *err = rc;
        return NULL;
    }
    lf->fd = fd;
    lf->ob_fd = ob;
    lf->held = (uint8_t)need;
    j->lfo[j->lfo_n++] = id;
    return lf;
}

/* fsync after a chunk's pwrite: 0, or -errno. A retried fsync may have succeeded on pages the
 * kernel dropped, so then the chunk is read back and compared with what was received (the batch
 * path does the same with reread_ranges). */
static int chunk_sync(ava1_job_t *j, int fd, const uint8_t *d, size_t len, uint64_t off) {
    int retried = 0, e = ava1_fsync_retry(fd, stopping_cb, j, &retried);
    uint8_t *buf;
    size_t got = 0;
    int rc = 0;
    if (e) return -e;
    if (!retried) return 0;
    if (!(buf = malloc(len ? len : 1))) return -ENOMEM;
    while (got < len) {
        ssize_t k = pread(fd, buf + got, len - got, (off_t)(off + got));
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) {
            rc = k < 0 ? -errno : -EIO;
            break;
        }
        got += (size_t)k;
    }
    if (!rc && memcmp(buf, d, len) != 0) rc = -EIO;
    free(buf);
    return rc;
}

static int write_chunk(ava1_job_t *j, uint32_t id, uint64_t off, const uint8_t *d, size_t len) {
    ava1_lfile_t *lf;
    int err, fd, ob;
    uint64_t size = j->m.e[id].size, g;
    pthread_mutex_lock(&j->mu);
    if (ava1_bits_get(&j->done, id) || (j->lf[id] && (j->lf[id]->committed || j->lf[id]->committing))) {
        pthread_mutex_unlock(&j->mu);
        return 0; /* a late duplicate */
    }
    lf = lfile_open(j, id, &err, 1);
    if (lf && (lf->committed || lf->committing || ava1_bits_get(&j->done, id))) { /* committed while we opened it */
        pthread_mutex_unlock(&j->mu);
        return 0;
    }
    /* Our own descriptors: the job thread may close lf's at commit while we write, and a
     * reused descriptor number would then take these bytes into some other file. */
    fd = lf ? dup(lf->fd) : -1;
    ob = lf && lf->ob_fd >= 0 ? dup(lf->ob_fd) : -1;
    if (lf && (fd < 0 || (lf->ob_fd >= 0 && ob < 0))) err = errno;
    else if (lf) lf->writers++; /* until the range is recorded: lf_evict_one keeps its descriptors open */
    pthread_mutex_unlock(&j->mu);
    if (!lf || err) {
        if (fd >= 0) close(fd);
        if (ob >= 0) close(ob);
        return -err;
    }
    err = pwrite_all(fd, d, len, off);
    if (err == 0 && ob >= 0) {
        for (g = off / AVA1_GROUP_LEN; g * AVA1_GROUP_LEN < off + len; g++) {
            uint64_t gs = g * AVA1_GROUP_LEN, glen = size - gs < AVA1_GROUP_LEN ? size - gs : AVA1_GROUP_LEN;
            uint8_t cv[32];
            ava1_b3_group_cv(d + (gs - off), (size_t)glen, g, cv);
            if ((err = pwrite_all(ob, cv, 32, g * 32u)) != 0) break;
        }
    }
    /* A drive too slow for batch fsyncs (see slow_drive_check): flush this chunk now, so the
     * device streams and each flush is short. The batch fsync that follows finds little to do. */
    if (err == 0 && __atomic_load_n(&j->perchunk, __ATOMIC_RELAXED)) err = chunk_sync(j, fd, d, len, off);
    close(fd);
    if (ob >= 0) close(ob);
    if (err) {
        pthread_mutex_lock(&j->mu);
        lf->writers--;
        pthread_mutex_unlock(&j->mu);
        return err;
    }
    pthread_mutex_lock(&j->mu);
    lf->writers--;
    if (!lf->committed) (void)ava1_rset_add(&lf->written, off, off + len); /* else: a late duplicate */
    j->unsynced_bytes += len;
    j->bytes_received += len;
    j->applied_since_tune++;
    pthread_mutex_unlock(&j->mu);
    return 0;
}

/* ---- durable-by-log: the pack log (SPEC.md §15.7) ----------------------------------- */

static void put32le(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static uint32_t get32le(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

#define PACK_MAGIC "AVA1PCK1"
#define PACK_HDR 8u
#define PACK_REC_MAX (1u << 26) /* a record longer than this is not one of ours */
#define SWEEP_BATCH 64u

static void pack_path(const ava1_job_t *j, uint32_t seg, char *out, size_t cap) {
    snprintf(out, cap, "%s/pack.%u", j->dir, seg);
}

/* One pack record for a small file: u32le len ‖ kind 1 ‖ BundleRecord ‖ u32le crc32c(kind ‖ body),
 * len = 1 + body length. Malloc'd; *n is its whole length. */
static uint8_t *pack_frame(const ava1_bundle_record_t *r, size_t *n) {
    size_t cap = (size_t)r->data_len + 256u;
    uint8_t *buf = malloc(cap + 16u);
    ava1_w_t w;
    uint32_t len;
    if (!buf) return NULL;
    ava1_w_init(&w, buf + 5, cap);
    if (ava1_bundle_record_encode(r, &w) != 0) {
        free(buf);
        return NULL;
    }
    len = (uint32_t)(1 + w.len);
    put32le(buf, len);
    buf[4] = 1;
    put32le(buf + 4 + len, ava1_crc32c(buf + 4, len));
    *n = (size_t)len + 8u;
    return buf;
}

/* The job's unswept bytes and the cross-job counter move together, except while the job is excluded from the
 * cap (a sticky sweep error, or a recovery pass's throwaway): then only the job's own count moves, so a stuck
 * job's bytes never keep other jobs waiting. Caller holds j->mu. */
static void ub_delta_locked(ava1_job_t *j, int64_t d) {
    if (d >= 0) {
        j->unswept_bytes += (uint64_t)d;
    } else {
        uint64_t sub = (uint64_t)(-d);
        if (sub > j->unswept_bytes) sub = j->unswept_bytes;
        j->unswept_bytes -= sub;
        d = -(int64_t)sub;
    }
    if (!j->ub_excluded) ava1_unswept_add(d);
}

static void ub_exclude_locked(ava1_job_t *j, int on) {
    if (on == j->ub_excluded) return;
    ava1_unswept_add(on ? -(int64_t)j->unswept_bytes : (int64_t)j->unswept_bytes);
    j->ub_excluded = on;
}

/* Drops one file's claim on its segment (caller holds j->mu); a closed segment nobody claims is
 * deleted (I3: its files are swept and the sweep is journaled, or they never were journaled). */
static void pack_unref_locked(ava1_job_t *j, uint32_t seg, uint32_t len) {
    ava1_pseg_t *p;
    if (seg >= j->npsegs) return;
    p = &j->psegs[seg];
    if (p->nusw) p->nusw--;
    ub_delta_locked(j, -(int64_t)len);
    if (p->closed && !p->nusw && !p->removed) {
        char path[PATH_CAP];
        if (p->fd >= 0) close(p->fd);
        p->fd = -1;
        p->removed = 1;
        pack_path(j, seg, path, sizeof path);
        (void)unlink(path);
    }
}

/* Makes room for segment numbers below `n` (all closed and removed until a caller fills one).
 * Caller holds j->mu. 0, or -1 when out of memory. */
static int psegs_reserve_locked(ava1_job_t *j, uint32_t n) {
    if (n > (1u << 20)) return -1; /* a million segments of 64 MiB: not a log, a corrupt number */
    if (n > j->psegs_cap) {
        uint32_t c = j->psegs_cap ? j->psegs_cap : 8;
        ava1_pseg_t *q;
        while (c < n) c *= 2; /* n <= 2^20: this ends, and c stays far below 2^31 */
        if (!(q = realloc(j->psegs, (size_t)c * sizeof *q))) return -1;
        j->psegs = q;
        j->psegs_cap = c;
    }
    while (j->npsegs < n) {
        ava1_pseg_t *p = &j->psegs[j->npsegs++];
        memset(p, 0, sizeof *p);
        p->fd = -1;
        p->closed = p->removed = 1;
    }
    return 0;
}

/* Starts segment `n` (the next number): the file with its magic, its directory entry synced so the
 * journal can name it. Caller holds pack_mu. 0 or -errno. */
static int pack_roll(ava1_job_t *j) {
    char path[PATH_CAP];
    uint32_t n;
    int fd, rc;
    pthread_mutex_lock(&j->mu);
    n = j->npsegs;
    if (n && !j->psegs[n - 1].removed) j->psegs[n - 1].closed = 1;
    if (psegs_reserve_locked(j, n + 1) != 0) {
        pthread_mutex_unlock(&j->mu);
        return -ENOMEM;
    }
    if (n && j->psegs[n - 1].closed && !j->psegs[n - 1].nusw && !j->psegs[n - 1].removed) {
        if (j->psegs[n - 1].fd >= 0) close(j->psegs[n - 1].fd); /* an empty one: nothing to keep */
        pack_path(j, n - 1, path, sizeof path);
        (void)unlink(path);
        j->psegs[n - 1].fd = -1;
        j->psegs[n - 1].removed = 1;
    }
    pthread_mutex_unlock(&j->mu);
    pack_path(j, n, path, sizeof path);
    fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) return -errno;
    if ((rc = pwrite_all(fd, (const uint8_t *)PACK_MAGIC, PACK_HDR, 0)) != 0 || (rc = -sync_dir(j->dir)) != 0) {
        close(fd);
        (void)unlink(path);
        return rc;
    }
    pthread_mutex_lock(&j->mu);
    j->psegs[n].fd = fd;
    j->psegs[n].tail = PACK_HDR;
    j->psegs[n].nusw = 0;
    j->psegs[n].dirty = 1; /* the magic is not yet synced */
    j->psegs[n].closed = j->psegs[n].removed = 0;
    pthread_mutex_unlock(&j->mu);
    return 0;
}

/* Appends one small file's record to the log (one pwrite at the tail). The tail offset is taken
 * and written under pack_mu, so every record below a written one is itself written and a batch's
 * byte range is a run of whole records. The file counts against its segment and the unswept cap
 * from here until it is swept (or dropped with pack_unref_locked). 0, or -errno. */
static int pack_append(ava1_job_t *j, const ava1_bundle_record_t *r, ava1_ploc_t *loc) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    size_t n;
    uint8_t *rec = pack_frame(r, &n);
    uint32_t seg;
    uint64_t off;
    int fd, rc = 0, roll;
    if (!rec) return -ENOMEM;
    pthread_mutex_lock(&j->pack_mu);
    pthread_mutex_lock(&j->mu);
    roll = !j->npsegs || j->psegs[j->npsegs - 1].closed ||
           (j->psegs[j->npsegs - 1].tail > PACK_HDR && j->psegs[j->npsegs - 1].tail + n > cfg->pack_segment);
    pthread_mutex_unlock(&j->mu);
    if (roll && (rc = pack_roll(j)) != 0) goto out;
    pthread_mutex_lock(&j->mu);
    seg = j->npsegs - 1;
    fd = j->psegs[seg].fd;
    off = j->psegs[seg].tail;
    j->psegs[seg].tail += n;
    j->psegs[seg].nusw++;
    ub_delta_locked(j, (int64_t)n);
    pthread_mutex_unlock(&j->mu);
    if ((rc = pwrite_all(fd, rec, n, off)) != 0) {
        pthread_mutex_lock(&j->mu);
        pack_unref_locked(j, seg, (uint32_t)n);
        pthread_mutex_unlock(&j->mu);
        goto out;
    }
    pthread_mutex_lock(&j->mu);
    j->psegs[seg].dirty = 1;
    pthread_mutex_unlock(&j->mu);
    loc->seg = seg;
    loc->off = off;
    loc->len = (uint32_t)n;
out:
    pthread_mutex_unlock(&j->pack_mu);
    free(rec);
    return rc;
}

static int sweep_step(ava1_job_t *j, int force);

/* The open-file budget (ava1_data.h): a slot is taken before a small file is opened and given
 * back once its fd is closed. While the share is used up, a worker runs queued sync stripes
 * (the batch that frees slots needs workers) or waits; the job thread syncs early (pend_full). */
static int pend_gate_stopping(void *a) {
    ava1_job_t *j = a;
    int st;
    pthread_mutex_lock(&j->mu);
    st = j->stopping;
    pthread_mutex_unlock(&j->mu);
    return st;
}

static void pend_gate_idle(void *a) {
    ava1_job_t *j = a;
    ava1_work_t *w;
    pthread_mutex_lock(&j->mu);
    w = j->q_head;
    if (w && w->kind == AVA1_W_CALL) {
        j->q_head = w->next;
        if (!j->q_head) j->q_tail = NULL;
        j->q_len--;
        pthread_mutex_unlock(&j->mu);
        w->fn(j, w->arg, w->i);
        pthread_mutex_lock(&j->mu);
        if (--j->calls_left == 0) pthread_cond_broadcast(&j->cv);
        free(w);
        pthread_mutex_unlock(&j->mu);
    } else {
        pthread_mutex_unlock(&j->mu);
        ava1_platform_sleep_ms(2);
    }
}

/* Small files wait here (holding their fd) until a sync batch covers them. */
static void pend_add(ava1_job_t *j, uint32_t id, int fd, const uint8_t root[32], const ava1_ploc_t *loc) {
    pthread_mutex_lock(&j->mu);
    while (j->pend_n >= AVA1_PEND_MAX && !j->stopping) {
        /* Run sync stripes ourselves: if every worker waited here, nobody would. */
        ava1_work_t *w = j->q_head;
        if (w && w->kind == AVA1_W_CALL) {
            j->q_head = w->next;
            if (!j->q_head) j->q_tail = NULL;
            j->q_len--;
            pthread_mutex_unlock(&j->mu);
            w->fn(j, w->arg, w->i);
            pthread_mutex_lock(&j->mu);
            if (--j->calls_left == 0) pthread_cond_broadcast(&j->cv);
            free(w);
        } else {
            pthread_cond_wait(&j->cv, &j->mu);
        }
    }
    if (j->pend_n == j->pend_cap) {
        uint32_t c = j->pend_cap ? j->pend_cap * 2 : 256;
        uint32_t *a = realloc(j->pend_small, c * sizeof *a);
        int *b = a ? realloc(j->pend_fd, c * sizeof *b) : NULL;
        uint8_t(*r)[32] = b ? realloc(j->pend_root, (size_t)c * sizeof *r) : NULL;
        ava1_ploc_t *pl = r ? realloc(j->pend_loc, (size_t)c * sizeof *pl) : NULL;
        if (a) j->pend_small = a;
        if (b) j->pend_fd = b;
        if (r) j->pend_root = r;
        if (pl) j->pend_loc = pl;
        if (a && b && r && pl) j->pend_cap = c;
    }
    if (j->pend_n < j->pend_cap) {
        j->pend_small[j->pend_n] = id;
        memcpy(j->pend_root[j->pend_n], root, 32);
        if (loc) j->pend_loc[j->pend_n] = *loc;
        else memset(&j->pend_loc[j->pend_n], 0, sizeof j->pend_loc[0]);
        j->pend_fd[j->pend_n++] = fd;
        if (fd >= 0) j->pend_n_fd++;
    } else if (fd >= 0) {
        close(fd); /* out of memory: the file will be in no batch and is sent again on resume */
        ava1_pend_release(1);
    } else if (loc) {
        pack_unref_locked(j, loc->seg, loc->len);
    }
    pthread_mutex_unlock(&j->mu);
}

/* The durable-by-log path (SPEC.md §15.7): the record goes to the pack log, the file is written with no
 * fsync and no descriptor kept; the batch fsyncs the log once and the sweep makes the file durable in
 * place later. 0 or -errno. */
static int apply_record_logged(ava1_job_t *j, const ava1_bundle_record_t *r, const ava1_ment_t *e, const char *path,
                               const uint8_t root[32]) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_ploc_t loc;
    int fd, rc;
    /* Backpressure (§3.4): past the unswept cap this worker sweeps (or runs queued sync stripes the
     * batch that would free room needs) instead of writing, so disk use stays bounded. */
    for (;;) {
        uint64_t b;
        int stop;
        pthread_mutex_lock(&j->mu);
        b = j->unswept_bytes;
        stop = j->stopping || j->finished || j->final_status;
        pthread_mutex_unlock(&j->mu);
        if (stop) return 0;
        /* The cross-job cap gates a job only past its own share (4 MiB, or half the cap when that is smaller):
         * a job that holds little is never held up by other jobs' bytes, however stuck they are. */
        {
            uint64_t need = r->data_len + 128u, floor = cfg->unswept_total / 2 < (4ull << 20) ? cfg->unswept_total / 2 : (4ull << 20);
            if (b + need <= cfg->unswept_max && (b < floor || ava1_unswept_total() + need <= cfg->unswept_total)) break;
        }
        if (sweep_step(j, 1) <= 0) pend_gate_idle(j);
    }
    if ((rc = pack_append(j, r, &loc)) != 0) return rc;
    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0 && errno == ENOENT && mkparents(path) == 0) fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) {
        rc = -errno;
        goto fail;
    }
    if ((rc = write_all(fd, r->data, r->data_len)) != 0) {
        close(fd);
        goto fail;
    }
    (void)fchmod(fd, AVA1_CONSOLE_FILE_MODE);
    ava1_platform_set_mtime(fd, path, e->mtime);
    close(fd);
    pthread_mutex_lock(&j->mu);
    j->bytes_received += r->data_len;
    j->applied_since_tune++;
    pthread_mutex_unlock(&j->mu);
    pend_add(j, r->file_id, -1, root, &loc);
    return 0;
fail:
    pthread_mutex_lock(&j->mu);
    pack_unref_locked(j, loc.seg, loc.len);
    pthread_mutex_unlock(&j->mu);
    return rc;
}

/* 0, -errno, or BAD_RECORD: a record naming no file (the rest of its bundle is dropped).
 * Positive so it can never collide with a -errno. */
#define BAD_RECORD 1

static int apply_record(ava1_job_t *j, const ava1_bundle_record_t *r) {
    const ava1_ment_t *e;
    uint8_t root[32];
    char path[PATH_CAP];
    int fd, rc;
    if (r->file_id >= j->m.n || j->m.e[r->file_id].kind != AVA1_ENTRY_FILE) return BAD_RECORD;
    e = &j->m.e[r->file_id];
    if (r->data_len != e->size) {
        emit_retry(j, r->file_id, AVA1_RETRY_CHANGED);
        return 0;
    }
    ava1_b3_hash(r->data, r->data_len, root);
    if (memcmp(root, r->root, 32) != 0) {
        emit_retry(j, r->file_id, AVA1_RETRY_VERIFY);
        return 0;
    }
    ava1_apply_path(j, r->file_id, 0, path, sizeof path);
    /* Right before the O_TRUNC: a duplicate must not truncate a file that is already
     * durable, or written and waiting for its batch. */
    pthread_mutex_lock(&j->mu);
    rc = ava1_bits_get(&j->done, r->file_id);
    for (fd = 0; !rc && (uint32_t)fd < j->pend_n; fd++) rc = j->pend_small[fd] == r->file_id;
    pthread_mutex_unlock(&j->mu);
    if (rc) return 0;
    if (j->log_small) return apply_record_logged(j, r, e, path, root);
    if (!ava1_pend_reserve(pend_gate_stopping, pend_gate_idle, j)) return 0; /* stopping */
    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0 && errno == ENOENT && mkparents(path) == 0) fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) {
        rc = -errno;
        ava1_pend_release(1);
        return rc;
    }
    if ((rc = write_all(fd, r->data, r->data_len)) != 0) {
        close(fd);
        ava1_pend_release(1);
        return rc;
    }
    (void)fchmod(fd, AVA1_CONSOLE_FILE_MODE);
    ava1_platform_set_mtime(fd, path, e->mtime);
    pthread_mutex_lock(&j->mu);
    j->bytes_received += r->data_len;
    j->applied_since_tune++;
    pthread_mutex_unlock(&j->mu);
    pend_add(j, r->file_id, fd, root, NULL);
    return 0;
}

/* A queued sweep (job_main queues one when files are due): sweeps until none is due. */
static void run_sweep(ava1_job_t *j) {
    int n, rounds = 0;
    do {
        int force;
        pthread_mutex_lock(&j->mu);
        force = j->finished || j->final_status;
        pthread_mutex_unlock(&j->mu);
        n = sweep_step(j, force);
    } while (n > 0 && ++rounds < 16 && !is_stopping(j));
    pthread_mutex_lock(&j->mu);
    j->sweep_queued = 0;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

static void run_work(ava1_job_t *j, ava1_work_t *w) {
    int rc = 0;
    if (w->kind == AVA1_W_COMMIT) {
        run_commit(j, w->file_id);
        return;
    }
    if (w->kind == AVA1_W_SWEEP) {
        run_sweep(j);
        return;
    }
    if (w->kind == AVA1_W_CHUNK) {
        rc = write_chunk(j, w->file_id, w->offset, w->data, w->len);
    } else if (w->kind == AVA1_W_BUNDLE) {
        ava1_r_t it;
        ava1_bundle_record_t r;
        int k;
        ava1_r_init(&it, w->data, w->len);
        while (rc == 0 && (k = ava1_bundle_record_next(&it, &r)) == 1) rc = apply_record(j, &r);
    }
    if (rc == -ENOSPC) worker_fail(j, AVA1_ERR_NO_SPACE, "the destination drive is full", ENOSPC);
    else if (rc == BAD_RECORD) worker_fail(j, AVA1_ERR_PROTOCOL, "a bundle record names no file", 0);
    else if (rc < 0) worker_fail(j, AVA1_ERR_IO, "write failed", -rc);
    release_owned(w->owned, w->owned_cap);
    give_back(j, w->owned_len);
}

static void *worker_main(void *arg) {
    wctx_t *c = arg;
    ava1_job_t *j = c->j;
    uint32_t idx = c->idx;
    free(c);
    pthread_mutex_lock(&j->mu);
    for (;;) {
        ava1_work_t *w;
        while (!j->stopping && (!j->q_head || idx >= j->want_workers)) pthread_cond_wait(&j->cv, &j->mu);
        if (j->stopping) break;
        w = j->q_head;
        j->q_head = w->next;
        if (!j->q_head) j->q_tail = NULL;
        j->q_len--;
        j->busy++;
        pthread_mutex_unlock(&j->mu);
        if (w->kind == AVA1_W_CALL) {
            w->fn(j, w->arg, w->i);
            pthread_mutex_lock(&j->mu);
            if (--j->calls_left == 0) pthread_cond_broadcast(&j->cv);
            pthread_mutex_unlock(&j->mu);
        } else {
            run_work(j, w);
        }
        free(w);
        pthread_mutex_lock(&j->mu);
        j->busy--;
    }
    pthread_mutex_unlock(&j->mu);
    return NULL;
}

static int add_workers(ava1_job_t *j, uint8_t n) {
    while (j->nworkers < n && j->nworkers < 16) {
        wctx_t *c = malloc(sizeof *c);
        if (!c) return -1;
        c->j = j;
        c->idx = j->nworkers;
        if (ava1_thread_start(worker_main, c, &j->workers[j->nworkers]) != 0) {
            free(c);
            return -1;
        }
        j->nworkers++;
    }
    return 0;
}

/* ---- sync batches ------------------------------------------------------------------ */

/* One large file's share of a batch, kept so its ranges can be re-read after a retried fsync. */
typedef struct {
    uint32_t id, fd_idx, rg_first, rg_n;
    int fd, ob_fd, ob_idx; /* ob_idx < 0: no outboard */
} chk_t;

typedef struct {
    ava1_job_t *job;
    int *fds;
    const uint32_t *ids;          /* the first n_small fds belong to these small files */
    const uint8_t (*roots)[32];   /* ... and carry these BLAKE3 roots */
    uint8_t *retried;             /* per fd: a retry is what made its fsync succeed */
    uint32_t n, n_small, stripes;
    int err, cut; /* cut: a stripe gave up because the job is stopping */
} fdlist_t;

static int stopping_cb(void *a) { return is_stopping(a); }

/* A small file whose fsync needed a retry is read back and compared with the root it arrived
 * with: if the kernel dropped dirty pages on the failed attempt, the retry succeeds on
 * nothing, and this is where it shows. 0, or an errno. */
static int reread_small(ava1_job_t *j, uint32_t id, const uint8_t root[32]) {
    uint64_t size = j->m.e[id].size, got = 0;
    uint8_t *buf = malloc(size ? (size_t)size : 1), h[32];
    char path[PATH_CAP];
    int rc = 0, fd;
    if (!buf) return ENOMEM;
    ava1_apply_path(j, id, 0, path, sizeof path); /* the pending descriptor is write-only */
    fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) {
        free(buf);
        return errno;
    }
    while (got < size) {
        ssize_t k = pread(fd, buf + got, (size_t)(size - got), (off_t)got);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) {
            rc = k < 0 ? errno : EIO;
            break;
        }
        got += (uint64_t)k;
    }
    if (!rc) {
        ava1_b3_hash(buf, (size_t)size, h);
        if (memcmp(h, root, 32) != 0) rc = EIO;
    }
    close(fd);
    free(buf);
    return rc;
}

static void sync_stripe(ava1_job_t *j, void *arg, uint32_t i) {
    fdlist_t *l = arg;
    uint32_t k;
    uint32_t delay = ava1_data_cfg()->fsync_delay_us;
    for (k = i; k < l->n; k += l->stripes) {
        uint32_t ms = delay ? (delay / 1000u ? delay / 1000u : 1u) : 0;
        int e, retried = 0;
        while (ms && !is_stopping(j)) { /* a slow disk, in slices a stop can cut */
            uint32_t step = ms < 50u ? ms : 50u;
            ava1_platform_sleep_ms(step);
            ms -= step;
        }
        if (is_stopping(j)) {
            __atomic_store_n(&l->cut, 1, __ATOMIC_RELAXED);
            return;
        }
        e = ava1_fsync_retry(l->fds[k], stopping_cb, j, &retried);
        if (e) {
            if (is_stopping(j)) __atomic_store_n(&l->cut, 1, __ATOMIC_RELAXED);
            else __atomic_store_n(&l->err, e, __ATOMIC_RELAXED);
        } else if (retried) {
            l->retried[k] = 1;
            if (k < l->n_small && (e = reread_small(l->job, l->ids[k], l->roots[k])) != 0)
                __atomic_store_n(&l->err, e, __ATOMIC_RELAXED);
        }
    }
}

/* A large file whose data or outboard fsync needed a retry: every range of this batch is read
 * back and each group's chaining value compared with the outboard's. 0, or an errno. */
static int reread_ranges(const chk_t *c, const ava1_file_range_t *rg, uint64_t size) {
    uint32_t r;
    uint8_t *buf = malloc(AVA1_GROUP_LEN);
    int rc = 0;
    if (!buf) return ENOMEM;
    for (r = c->rg_first; r < c->rg_first + c->rg_n && !rc; r++) {
        uint64_t g, end = rg[r].offset + rg[r].len;
        for (g = rg[r].offset / AVA1_GROUP_LEN; g * AVA1_GROUP_LEN < end && !rc; g++) {
            uint64_t gs = g * AVA1_GROUP_LEN, glen = size - gs < AVA1_GROUP_LEN ? size - gs : AVA1_GROUP_LEN, got = 0;
            uint8_t cv[32], want[32];
            while (got < glen) {
                ssize_t k = pread(c->fd, buf + got, (size_t)(glen - got), (off_t)(gs + got));
                if (k < 0 && errno == EINTR) continue;
                if (k <= 0) {
                    rc = k < 0 ? errno : EIO;
                    break;
                }
                got += (uint64_t)k;
            }
            if (rc) break;
            if (c->ob_fd < 0) continue; /* a one-group file: its root is the CV, checked at commit */
            ava1_b3_group_cv(buf, (size_t)glen, g, cv);
            if (pread(c->ob_fd, want, 32, (off_t)(g * 32u)) != 32 || memcmp(cv, want, 32) != 0) rc = EIO;
        }
    }
    free(buf);
    return rc;
}

typedef struct {
    uint32_t id;
    ava1_ploc_t loc;
} idloc_t;
static int idloc_cmp(const void *a, const void *b) {
    uint32_t x = ((const idloc_t *)a)->id, y = ((const idloc_t *)b)->id;
    return x < y ? -1 : x > y;
}
static int u32cmp(const void *a, const void *b) {
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return x < y ? -1 : x > y;
}

/* Tests: stop dead between two steps of the durability chain, as a power cut would. */
void ava1_apply_crash(ava1_job_t *j) {
    pthread_mutex_lock(&j->mu);
    j->stopping = 1;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

static int dirent_cmp(const void *a, const void *b) {
    return strcmp(((const ava1_dirent_t *)a)->dir, ((const ava1_dirent_t *)b)->dir);
}

/* A set of distinct directories to fsync, striped over the workers like the data fsyncs
 * (review 003 §3.3 item 1: they used to run one after another on the job thread, ~3 ms each,
 * tens of seconds in prepare and a good share of every batch on a game-sized tree). The
 * directories are independent descriptors; each sync is the same call as before, so nothing
 * about what is durable when changes. */
typedef struct {
    const ava1_dirent_t *d;
    uint32_t n, stripes;
    int hook, crash; /* the test hook point (0: none); crash after the first sync (tests) */
    int err, cut;
} dirset_t;

static void dir_stripe(ava1_job_t *j, void *arg, uint32_t i) {
    dirset_t *l = arg;
    uint32_t k;
    for (k = i; k < l->n; k += l->stripes) {
        int e;
        if (is_stopping(j) || __atomic_load_n(&l->cut, __ATOMIC_RELAXED)) {
            __atomic_store_n(&l->cut, 1, __ATOMIC_RELAXED);
            return;
        }
        if ((e = sync_dir(l->d[k].dir)) != 0) {
            __atomic_store_n(&l->err, e, __ATOMIC_RELAXED);
            return;
        }
        if (l->hook) HOOK(j, l->hook, l->d[k].id);
        if (l->crash) { /* tests: the power goes after one directory of the batch */
            __atomic_store_n(&l->cut, 1, __ATOMIC_RELAXED);
            ava1_apply_crash(j);
            return;
        }
    }
}

int ava1_sync_dirset(ava1_job_t *j, ava1_dirent_t *d, uint32_t n, int hook_point) {
    uint32_t i, m = 0;
    int rc = 0;
    dirset_t l;
    if (n) qsort(d, n, sizeof *d, dirent_cmp);
    for (i = 0; i < n; i++) { /* distinct only, in place; the repeats' strings go now */
        if (m && strcmp(d[i].dir, d[m - 1].dir) == 0) {
            free(d[i].dir);
            continue;
        }
        d[m++] = d[i];
    }
    memset(&l, 0, sizeof l);
    l.d = d;
    l.n = m;
    l.hook = hook_point;
    l.crash = hook_point == AVA1_HOOK_BATCH_DIR_SYNCED && ava1_data_cfg()->crash_at == AVA1_CRASH_MID_DIRS;
    l.stripes = j->want_workers < m ? j->want_workers : m;
    if (l.stripes >= 2 && m > 2) {
        if (ava1_apply_parallel(j, dir_stripe, &l, l.stripes) != 0) rc = -1; /* stopping */
    } else {
        l.stripes = 1;
        dir_stripe(j, &l, 0);
    }
    if (l.err) rc = l.err;
    else if (!rc && (l.cut || (m && is_stopping(j)))) rc = -1;
    for (i = 0; i < m; i++) free(d[i].dir);
    return rc;
}

/* ---- durable-by-log: sweep and recovery --------------------------------------------- */

/* Reads the record at (seg, off, len) from the pack, checks its frame and decodes it. `*buf` (malloc'd,
 * the caller frees) owns `r`'s data. 0, or an errno. */
static int pack_read(ava1_job_t *j, uint32_t seg, uint64_t off, uint32_t len, uint8_t **buf, ava1_bundle_record_t *r) {
    int fd;
    uint8_t *b;
    uint32_t body;
    if (len < 9 || len > PACK_REC_MAX) return EIO;
    pthread_mutex_lock(&j->mu);
    fd = seg < j->npsegs ? j->psegs[seg].fd : -1;
    pthread_mutex_unlock(&j->mu);
    if (fd < 0) return ENOENT;
    if (!(b = malloc(len))) return ENOMEM;
    {
        size_t got = 0;
        while (got < len) {
            ssize_t k = pread(fd, b + got, len - got, (off_t)(off + got));
            if (k < 0 && errno == EINTR) continue;
            if (k <= 0) {
                free(b);
                return k < 0 ? errno : EIO;
            }
            got += (size_t)k;
        }
    }
    body = get32le(b);
    if (body + 8u != len || b[4] != 1 || get32le(b + 4 + body) != ava1_crc32c(b + 4, body) ||
        ava1_bundle_record_decode(b + 5, body - 1, r) != 0) {
        free(b);
        return EIO;
    }
    *buf = b;
    return 0;
}

/* Writes a small file from its log record (the part a crash or a lost page cache left out). */
static int write_small(ava1_job_t *j, const ava1_ment_t *e, uint32_t id, const ava1_bundle_record_t *r) {
    char path[PATH_CAP];
    int fd, rc;
    ava1_apply_path(j, id, 0, path, sizeof path);
    if (!path[0]) return ENAMETOOLONG;
    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0 && errno == ENOENT && mkparents(path) == 0) fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) return errno;
    if ((rc = write_all(fd, r->data, r->data_len)) != 0) {
        close(fd);
        return -rc;
    }
    (void)fchmod(fd, AVA1_CONSOLE_FILE_MODE);
    ava1_platform_set_mtime(fd, path, e->mtime);
    close(fd);
    return 0;
}

/* The file at `path` is a regular file of `size` bytes whose BLAKE3 is `root`. */
static int small_matches(const char *path, uint64_t size, const uint8_t root[32]) {
    struct stat st;
    uint8_t *buf, h[32];
    int fd = open(path, O_RDONLY | O_NOFOLLOW), ok = 0;
    size_t got = 0;
    if (fd < 0) return 0;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || (uint64_t)st.st_size != size || size > PACK_REC_MAX ||
        !(buf = malloc(size ? (size_t)size : 1))) {
        close(fd);
        return 0;
    }
    while (got < size) {
        ssize_t k = pread(fd, buf + got, (size_t)(size - got), (off_t)got);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) break;
        got += (size_t)k;
    }
    if (got == size) {
        ava1_b3_hash(buf, (size_t)size, h);
        ok = memcmp(h, root, 32) == 0;
    }
    free(buf);
    close(fd);
    return ok;
}

/* Makes one logged file durable in place: opens it (re-making it from its record when it is missing
 * or the wrong size), fsyncs it. A retried fsync may have lost pages, so the file is re-made and
 * synced again. 0, or an errno. */
static int sweep_one(ava1_job_t *j, const ava1_usw_t *u) {
    const ava1_ment_t *e = &j->m.e[u->id];
    char path[PATH_CAP];
    struct stat st;
    int fd, rc, retried = 0, again;
    ava1_apply_path(j, u->id, 0, path, sizeof path);
    if (!path[0]) return ENAMETOOLONG;
    if (ava1_apply_fault && (rc = ava1_apply_fault(j, AVA1_HOOK_SWEEP_FILE, u->id)) != 0) return rc; /* tests */
    for (again = 0; again < 2; again++) {
        int bad;
        fd = open(path, O_RDONLY | O_NOFOLLOW);
        bad = fd < 0 || fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || (uint64_t)st.st_size != e->size || again;
        if (!bad) {
            /* Right size is not right content (zero-filled blocks after a power cut, a damaged page): check the
             * bytes against the record's BLAKE3 root before the file is fsynced and its record released
             * (review 007 #8). A record that cannot be read here skips the check; it never fails a sound file. */
            ava1_bundle_record_t cr;
            uint8_t *cbuf = NULL;
            if (pack_read(j, u->seg, u->off, u->len, &cbuf, &cr) == 0) {
                bad = cr.file_id != u->id || !small_matches(path, e->size, cr.root);
                free(cbuf);
            }
        }
        if (bad) {
            ava1_bundle_record_t r;
            uint8_t *buf = NULL;
            if (fd >= 0) close(fd);
            if ((rc = pack_read(j, u->seg, u->off, u->len, &buf, &r)) != 0) return rc;
            if (r.file_id != u->id) rc = EIO;
            else rc = write_small(j, e, u->id, &r);
            free(buf);
            if (rc) return rc;
            fd = open(path, O_RDONLY | O_NOFOLLOW);
            if (fd < 0) return errno;
        }
        retried = 0;
        rc = ava1_fsync_retry(fd, stopping_cb, j, &retried);
        close(fd);
        if (rc) return rc;
        if (!retried) return 0;
    }
    return 0;
}

/* Puts files a failed sweep took back at the front of the queue, so they are tried again (caller holds
 * j->mu). Grows the queue if the room in front of its head is gone. */
static void usw_push_front_locked(ava1_job_t *j, const ava1_usw_t *take, uint32_t k) {
    if (j->usw_head >= k) {
        j->usw_head -= k;
        memcpy(j->usw + j->usw_head, take, (size_t)k * sizeof *take);
        j->usw_n += k;
        return;
    }
    if ((size_t)j->usw_n + k > j->usw_cap) {
        uint32_t c = j->usw_cap ? j->usw_cap : 256;
        ava1_usw_t *q;
        while (c < j->usw_n + k) c *= 2;
        if (!(q = realloc(j->usw, (size_t)c * sizeof *q))) return; /* cannot track them: the log still holds them */
        j->usw = q;
        j->usw_cap = c;
    }
    memmove(j->usw + k, j->usw + j->usw_head, (size_t)j->usw_n * sizeof *j->usw);
    memcpy(j->usw, take, (size_t)k * sizeof *take);
    j->usw_head = 0;
    j->usw_n += k;
}

#define SWEEP_FAIL_MAX 5u /* consecutive failures before the error is sticky and a running job fails */

/* One sweep: up to 64 files (all that are old enough; every one when `force`) made durable in place,
 * their parent directories synced, then the sweep journaled; only then do the files stop holding
 * their pack segments (I3). Returns the number swept, 0 when none was due, or -errno (the job is
 * then failing). Runs on a worker, so its directory syncs are serial (a worker never waits on
 * other workers' stripes). */
static int sweep_step(ava1_job_t *j, int force) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_usw_t take[SWEEP_BATCH];
    ava1_dirent_t d[SWEEP_BATCH];
    uint32_t k = 0, i, nd = 0, ids[SWEEP_BATCH];
    uint64_t now = ava1_mono_ms();
    int rc = 0;
    pthread_mutex_lock(&j->mu);
    while (k < SWEEP_BATCH && k < j->usw_n && (force || now - j->usw[j->usw_head + k].t_ms >= cfg->sweep_age_ms)) {
        take[k] = j->usw[j->usw_head + k];
        k++;
    }
    j->usw_head += k;
    j->usw_n -= k;
    if (k) j->sweeps_inflight++;
    pthread_mutex_unlock(&j->mu);
    if (!k) return 0;
    for (i = 0; i < k && !rc; i++) {
        char p[PATH_CAP], parent[PATH_CAP];
        if (is_stopping(j)) {
            rc = -1;
            break;
        }
        if ((rc = sweep_one(j, &take[i])) != 0) break;
        ava1_apply_path(j, take[i].id, 0, p, sizeof p);
        parent_of(p, parent, sizeof parent);
        if ((d[nd].dir = strdup(parent))) d[nd++].id = take[i].id;
        ids[i] = take[i].id;
    }
    if (!rc && nd) {
        qsort(d, nd, sizeof *d, dirent_cmp);
        for (i = 0; i < nd && !rc; i++)
            if (!i || strcmp(d[i].dir, d[i - 1].dir) != 0) rc = sync_dir(d[i].dir);
    }
    for (i = 0; i < nd; i++) free(d[i].dir);
    if (!rc && cfg->crash_at == AVA1_CRASH_MID_SWEEP) {
        ava1_apply_crash(j);
        rc = -1;
    }
    if (!rc) { /* the sweep record: runs of the (sorted) ids */
        uint8_t fb[SWEEP_BATCH * 16u + 16u], body[SWEEP_BATCH * 16u + 64u];
        ava1_w_t fw, w;
        ava1_jnl_sweep_t sw;
        ava1_file_run_t run;
        uint32_t nr = 0;
        qsort(ids, k, sizeof ids[0], u32cmp);
        ava1_w_init(&fw, fb, sizeof fb);
        for (i = 0; i < k; i++) {
            if (nr && run.first + run.count == ids[i]) {
                run.count++;
                continue;
            }
            if (nr) (void)ava1_file_run_append(&fw, &run);
            run.first = ids[i];
            run.count = 1;
            nr++;
        }
        if (nr) (void)ava1_file_run_append(&fw, &run);
        memset(&sw, 0, sizeof sw);
        sw.files = fb;
        sw.files_len = (uint32_t)fw.len;
        ava1_w_init(&w, body, sizeof body);
        if (ava1_jnl_sweep_encode(&sw, &w) != 0 || ava1_apply_jnl_append(j, AVA1_JNL_SWEEP, body, w.len) != 0) rc = EIO;
        else HOOK(j, AVA1_HOOK_SWEPT, UINT32_MAX);
    }
    if (!rc && cfg->crash_at == AVA1_CRASH_AFTER_SWEEP) {
        ava1_apply_crash(j);
        rc = -1;
    }
    pthread_mutex_lock(&j->mu);
    if (!rc) {
        for (i = 0; i < k; i++) pack_unref_locked(j, take[i].seg, take[i].len);
        j->unswept_n -= k;
        if (j->sweep_err) fprintf(stderr, "[ava1] job %02x%02x%02x%02x: sweeping works again\n", j->id[0], j->id[1], j->id[2], j->id[3]);
        j->sweep_fail_n = 0;
        j->sweep_err = 0;
        j->sweep_msg[0] = 0;
        ub_exclude_locked(j, 0);
    } else if (!j->stopping) {
        /* A failed sweep loses nothing: the files go back to the front of the queue and are tried again
         * after a backoff. After SWEEP_FAIL_MAX in a row the error is sticky (Status reports it) and a job
         * that is still running fails; an ended job keeps retrying. */
        usw_push_front_locked(j, take, k);
        j->sweep_fail_n++;
        j->sweep_retry_ms = ava1_mono_ms() + (100u << (j->sweep_fail_n < 6 ? j->sweep_fail_n - 1 : 5));
        if (j->sweep_fail_n >= SWEEP_FAIL_MAX && rc > 0) {
            int first = !j->sweep_err;
            j->sweep_err = rc;
            ub_exclude_locked(j, 1); /* its bytes stop counting against the other jobs' cap */
            snprintf(j->sweep_msg, sizeof j->sweep_msg, "making files durable on the console failed: %s", strerror(rc));
            if (first)
                fprintf(stderr, "[ava1] job %02x%02x%02x%02x: %s\n", j->id[0], j->id[1], j->id[2], j->id[3], j->sweep_msg);
        }
    }
    j->sweeps_inflight--;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    if (rc > 0 && j->sweep_fail_n >= SWEEP_FAIL_MAX && !j->finished) worker_fail(j, AVA1_ERR_IO, "making files durable failed", rc);
    return rc ? -(rc > 0 ? rc : EINTR) : (int)k;
}

/* Every logged file durable in place before this returns (0), or an errno; EINTR when the job is
 * stopping (the caller must not fail or journal anything then). A failing sweep is tried again a few
 * times, with a backoff, before it gives up. `unswept_n` is the truth: the queue can look empty while a
 * failed sweep's files are still counted. */
int ava1_pack_drain(ava1_job_t *j) {
    uint32_t fails = 0;
    for (;;) {
        uint32_t left, queued, inflight;
        int n = sweep_step(j, 1);
        if (n < 0) {
            if (n == -EINTR || is_stopping(j)) return EINTR;
            if (++fails >= SWEEP_FAIL_MAX) return -n;
            { /* the backoff, in slices a data-layer stop can cut (recovery drains run on a background thread) */
                uint32_t ms = 50u << (fails - 1);
                while (ms && ava1_data_running()) {
                    uint32_t step = ms < 20u ? ms : 20u;
                    ava1_platform_sleep_ms(step);
                    ms -= step;
                }
                if (!ava1_data_running()) return EINTR;
            }
            continue;
        }
        pthread_mutex_lock(&j->mu);
        left = j->unswept_n;
        queued = j->usw_n;
        inflight = j->sweeps_inflight;
        pthread_mutex_unlock(&j->mu);
        if (!left) return 0;
        if (n == 0) {
            if (!queued && !inflight) return EIO; /* counted files nobody holds: never say "settled" */
            ava1_platform_sleep_ms(2); /* another thread's sweep is finishing */
        }
    }
}

/* Deletes every pack file in the job directory and forgets the segment table (caller: nothing is
 * unswept any more). */
static void pack_forget_all(ava1_job_t *j) {
    char path[PATH_CAP];
    uint32_t i;
    pthread_mutex_lock(&j->mu);
    for (i = 0; i < j->npsegs; i++) {
        if (j->psegs[i].fd >= 0) close(j->psegs[i].fd);
        j->psegs[i].fd = -1;
        if (!j->psegs[i].removed) {
            pack_path(j, i, path, sizeof path);
            (void)unlink(path);
            j->psegs[i].removed = 1;
        }
    }
    j->npsegs = 0;
    ub_delta_locked(j, -(int64_t)j->unswept_bytes);
    pthread_mutex_unlock(&j->mu);
}

int ava1_pack_recover(ava1_job_t *j, const ava1_bits_t *unswept, const ava1_pack_ref_t *refs, uint32_t nrefs) {
    uint32_t i, r, nfound = 0, nlost = 0;
    ava1_bits_t found;
    int rc = 0;
    if (!ava1_bits_count(unswept)) {
        /* nothing unswept: a crashed run's stray segments hold nothing journaled */
        DIR *dp = opendir(j->dir);
        struct dirent *de;
        while (dp && (de = readdir(dp)) != NULL)
            if (strncmp(de->d_name, "pack.", 5) == 0) {
                char p[PATH_CAP];
                snprintf(p, sizeof p, "%s/%s", j->dir, de->d_name);
                (void)unlink(p);
            }
        if (dp) closedir(dp);
        return 0;
    }
    if (ava1_bits_init(&found, j->m.n) != 0) return -1;
    for (r = 0; r < nrefs; r++) {
        char path[PATH_CAP];
        struct stat st;
        int fd;
        uint64_t pos = refs[r].offset, end = refs[r].offset + refs[r].len;
        uint32_t seg = refs[r].segment;
        pack_path(j, seg, path, sizeof path);
        fd = open(path, O_RDONLY | O_NOFOLLOW);
        if (fd < 0) continue; /* a missing segment fails every file it held */
        if (fstat(fd, &st) == 0 && end > (uint64_t)st.st_size) end = (uint64_t)st.st_size;
        pthread_mutex_lock(&j->mu);
        if (psegs_reserve_locked(j, seg + 1) == 0 && j->psegs[seg].removed) {
            j->psegs[seg].fd = fd;
            j->psegs[seg].removed = j->psegs[seg].closed = 0;
            j->psegs[seg].tail = (uint64_t)st.st_size;
            fd = -1;
        }
        pthread_mutex_unlock(&j->mu);
        if (fd >= 0) close(fd); /* the segment was already opened by an earlier ref */
        while (pos + 9 <= end) {
            uint8_t hdr[4], *buf = NULL;
            ava1_bundle_record_t rec;
            uint32_t body;
            int fd2;
            pthread_mutex_lock(&j->mu);
            fd2 = j->psegs[seg].fd;
            pthread_mutex_unlock(&j->mu);
            if (fd2 < 0 || pread(fd2, hdr, 4, (off_t)pos) != 4) break;
            body = get32le(hdr);
            if (body < 1 || body > PACK_REC_MAX || pos + 8u + body > end) break;
            if (pack_read(j, seg, pos, body + 8u, &buf, &rec) != 0) break; /* the torn tail ends the range */
            if (rec.file_id < j->m.n && ava1_bits_get(unswept, rec.file_id) && !ava1_bits_get(&found, rec.file_id)) {
                uint8_t h[32];
                ava1_b3_hash(rec.data, rec.data_len, h);
                if (memcmp(h, rec.root, 32) == 0 && rec.data_len == j->m.e[rec.file_id].size) {
                    char fp[PATH_CAP];
                    ava1_usw_t u;
                    ava1_apply_path(j, rec.file_id, 0, fp, sizeof fp);
                    if (!small_matches(fp, rec.data_len, rec.root) && write_small(j, &j->m.e[rec.file_id], rec.file_id, &rec) != 0) {
                        free(buf);
                        pos += 8u + body;
                        continue; /* cannot re-make it: left to the reset below */
                    }
                    ava1_bits_set(&found, rec.file_id);
                    nfound++;
                    u.id = rec.file_id;
                    u.seg = seg;
                    u.off = pos;
                    u.len = body + 8u;
                    u.t_ms = 0;
                    pthread_mutex_lock(&j->mu);
                    if (j->usw_head + j->usw_n == j->usw_cap) {
                        uint32_t c = j->usw_cap ? j->usw_cap * 2 : 256;
                        ava1_usw_t *q = realloc(j->usw, (size_t)c * sizeof *q);
                        if (q) {
                            j->usw = q;
                            j->usw_cap = c;
                        }
                    }
                    if (j->usw_head + j->usw_n < j->usw_cap) {
                        j->usw[j->usw_head + j->usw_n++] = u;
                        j->psegs[seg].nusw++;
                        j->unswept_n++;
                        ub_delta_locked(j, (int64_t)u.len);
                    } else {
                        rc = -1;
                    }
                    pthread_mutex_unlock(&j->mu);
                }
            }
            free(buf);
            pos += 8u + body;
        }
    }
    /* A file whose record cannot be found or read is not done: forget it, it is sent again. */
    for (i = 0; i < j->m.n && !rc; i++) {
        if (!ava1_bits_get(unswept, i) || ava1_bits_get(&found, i)) continue;
        {
            ava1_jnl_reset_t x;
            uint8_t b[16];
            ava1_w_t w;
            x.file_id = i;
            ava1_w_init(&w, b, sizeof b);
            if (ava1_jnl_reset_encode(&x, &w) != 0 || ava1_apply_jnl_append(j, AVA1_JNL_RESET, b, w.len) != 0) rc = -1;
            else {
                ava1_bits_clear(&j->done, i);
                nlost++;
            }
        }
    }
    ava1_bits_free(&found);
    if (!rc && ava1_pack_drain(j) != 0) rc = -1;
    if (!rc) {
        pack_forget_all(j);
        {
            DIR *dp = opendir(j->dir);
            struct dirent *de;
            while (dp && (de = readdir(dp)) != NULL)
                if (strncmp(de->d_name, "pack.", 5) == 0) {
                    char p[PATH_CAP];
                    snprintf(p, sizeof p, "%s/%s", j->dir, de->d_name);
                    (void)unlink(p);
                }
            if (dp) closedir(dp);
        }
    }
    fprintf(stderr, "[ava1] job %02x%02x%02x%02x: recovered %u logged files, %u lost (resent)%s\n", j->id[0], j->id[1],
            j->id[2], j->id[3], nfound, nlost, rc ? "; recovery failed" : "");
    return rc;
}

/* Syncs, once each, the directories that gained an entry in this batch: the small files'
 * (every one was just created or truncated) and the part files opened for the first time.
 * 0, an errno, or -1 when the job is stopping. */
static int sync_new_dirs(ava1_job_t *j, const uint32_t *small, uint32_t n_small, const uint32_t *large,
                         uint32_t n_large) {
    ava1_dirent_t *d = calloc((size_t)n_small + n_large + 1, sizeof *d);
    char p[PATH_CAP], parent[PATH_CAP];
    uint32_t i, n = 0;
    int rc = 0;
    if (!d) return ENOMEM;
    for (i = 0; i < n_small + n_large && !rc; i++) {
        uint32_t id = i < n_small ? small[i] : large[i - n_small];
        ava1_apply_path(j, id, i >= n_small, p, sizeof p);
        if (!p[0]) continue; /* its write failed already */
        parent_of(p, parent, sizeof parent);
        if (!(d[n].dir = strdup(parent))) rc = ENOMEM;
        else d[n++].id = id;
    }
    if (rc) {
        for (i = 0; i < n; i++) free(d[i].dir);
    } else {
        rc = ava1_sync_dirset(j, d, n, AVA1_HOOK_BATCH_DIR_SYNCED);
    }
    free(d);
    return rc;
}

/* Review 003 §6: after a batch's data fsync took `data_ms`, would the sender have run dry? The
 * intake rate is measured over the time data was flowing (from the end of the last batch to the
 * start of this one; the sender is parked on credit while an fsync runs, so including that would
 * understate it). When the fsync takes longer than the credit window holds data at that rate,
 * the job switches, for good, to an fsync after every chunk. */
static void slow_drive_check(ava1_job_t *j, uint64_t bytes, uint64_t flow_ms, uint64_t data_ms) {
    uint64_t credit, window_ms;
    if (__atomic_load_n(&j->perchunk, __ATOMIC_RELAXED) || bytes < (1ull << 20) || flow_ms < 50 || data_ms < 200) return;
    pthread_mutex_lock(&j->mu);
    credit = j->credit;
    pthread_mutex_unlock(&j->mu);
    if (!credit) return;
    window_ms = credit / (bytes / flow_ms ? bytes / flow_ms : 1u); /* bytes per ms */
    if (data_ms <= window_ms) return;
    if (__atomic_exchange_n(&j->perchunk, 1, __ATOMIC_RELAXED)) return;
    fprintf(stderr,
            "[ava1] job %02x%02x%02x%02x: slow drive: a batch fsync took %llu ms for %llu MiB, the %llu MiB window holds "
            "%llu ms at the current rate; fsync per chunk from now on\n",
            j->id[0], j->id[1], j->id[2], j->id[3], (unsigned long long)data_ms, (unsigned long long)(bytes >> 20),
            (unsigned long long)(credit >> 20), (unsigned long long)window_ms);
}

/* A pack fsync that needed a retry may have succeeded on pages the kernel dropped: every record of the
 * batch is read back and its CRC and BLAKE3 root checked (replacing reread_small for logged files). */
static int reread_pack_batch(ava1_job_t *j, const uint32_t *ids, const ava1_ploc_t *loc, const uint8_t (*roots)[32],
                             uint32_t n) {
    uint32_t i;
    for (i = 0; i < n; i++) {
        ava1_bundle_record_t r;
        uint8_t *buf = NULL, h[32];
        int rc;
        if (!loc[i].len) continue;
        if ((rc = pack_read(j, loc[i].seg, loc[i].off, loc[i].len, &buf, &r)) != 0) return rc;
        ava1_b3_hash(r.data, r.data_len, h);
        rc = (r.file_id != ids[i] || memcmp(h, roots[i], 32) != 0) ? EIO : 0;
        free(buf);
        if (rc) return rc;
    }
    return 0;
}

/* Appends one JnlBatch: small-file runs, large-file ranges and roots, and for a logged batch the pack range
 * (`pack_len` != 0) of its records. 0, or nonzero when it could not be journaled. */
static int append_batch(ava1_job_t *j, const ava1_file_run_t *runs, uint32_t nr, const ava1_file_range_t *rg, uint32_t ng,
                        const ava1_root_item_t *roots, uint32_t nroots, uint32_t seg, uint64_t off, uint64_t pack_len) {
    size_t cap = 96u + 16u * nr + 32u * ng + 48u * nroots;
    ava1_w_t fw, rw, ow, w;
    uint8_t *fb = malloc(16u * nr + 8), *rb = malloc(32u * ng + 8), *ob = malloc(48u * nroots + 8), *body = malloc(cap);
    ava1_jnl_batch_t b;
    uint32_t i;
    int rc;
    if (!fb || !rb || !ob || !body) {
        free(fb);
        free(rb);
        free(ob);
        free(body);
        return ENOMEM;
    }
    ava1_w_init(&fw, fb, 16u * nr + 8);
    ava1_w_init(&rw, rb, 32u * ng + 8);
    ava1_w_init(&ow, ob, 48u * nroots + 8);
    for (i = 0; i < nr; i++) (void)ava1_file_run_append(&fw, &runs[i]);
    for (i = 0; i < ng; i++) (void)ava1_file_range_append(&rw, &rg[i]);
    for (i = 0; i < nroots; i++) (void)ava1_root_item_append(&ow, &roots[i]);
    memset(&b, 0, sizeof b);
    b.files = fb;
    b.files_len = (uint32_t)fw.len;
    b.ranges = rb;
    b.ranges_len = (uint32_t)rw.len;
    b.roots = ob;
    b.roots_len = (uint32_t)ow.len;
    if (pack_len) {
        b.has_pack_segment = b.has_pack_offset = b.has_pack_len = 1;
        b.pack_segment = seg;
        b.pack_offset = off;
        b.pack_len = pack_len;
    }
    ava1_w_init(&w, body, cap);
    rc = ava1_jnl_batch_encode(&b, &w) == 0 ? ava1_apply_jnl_append(j, AVA1_JNL_BATCH, body, w.len) : -1;
    free(fb);
    free(rb);
    free(ob);
    free(body);
    return rc ? EIO : 0;
}

typedef struct {
    uint32_t seg, id;
    uint64_t off, len;
} lent_t;

static int lent_cmp(const void *a, const void *b) {
    const lent_t *x = a, *y = b;
    if (x->seg != y->seg) return x->seg < y->seg ? -1 : 1;
    return x->off < y->off ? -1 : x->off > y->off;
}

/* The durability chain (SPEC.md §12.6), in this order and no other: (1) data fsync, then
 * the directories that gained entries, (2) journal append + fsync, (3) state, then Durable. */
static void sync_batch(ava1_job_t *j) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    uint32_t *ids, n_small, i, nr = 0, ng = 0, nroots = 0, cap_g = 0, nlf = 0, nnew = 0;
    uint32_t *snap = NULL, nsnap = 0, k; /* the large files with work (lfl_snapshot) */
    uint8_t (*sroots)[32];
    uint8_t *retried = NULL;
    chk_t *chk = NULL;
    uint32_t nchk = 0;
    uint64_t u0 = mono_us(), u1 = 0, u2 = 0, u3 = 0;
    uint32_t nfiles;
    uint32_t *newlf = NULL; /* large files whose part file's directory entry is not yet synced */
    int *sfds, rc;
    ava1_file_run_t *runs = NULL;
    ava1_file_range_t *rg = NULL;
    ava1_root_item_t *roots = NULL;
    fdlist_t l;
    uint64_t t0 = ava1_mono_ms(), new_bytes = 0, bytes_in = 0;
    ava1_ploc_t *ploc = NULL;       /* durable-by-log: where each pending small file's record is */
    uint32_t *fids = NULL, nfd = 0, npk = 0, pk_first = 0, ndirty, held;
    uint8_t (*froots)[32] = NULL;   /* the pending files that hold a descriptor (the per-file fsync path) */
    int logmode = 0;
    lent_t *lg = NULL;
    memset(&l, 0, sizeof l);
    pthread_mutex_lock(&j->mu);
    ids = j->pend_small;
    sfds = j->pend_fd;
    sroots = j->pend_root;
    ploc = j->pend_loc;
    n_small = nfiles = j->pend_n;
    j->pend_small = NULL;
    j->pend_fd = NULL;
    j->pend_root = NULL;
    j->pend_loc = NULL;
    j->pend_n = j->pend_cap = 0;
    held = j->pend_n_fd; /* every pending fd holds a reservation; `nfd` is only set once the lists exist */
    j->pend_n_fd = 0;
    logmode = n_small && ploc && ploc[0].len;
    for (ndirty = 0, k = 0; k < j->npsegs; k++)
        if (j->psegs[k].dirty && !j->psegs[k].removed) ndirty++;
    bytes_in = j->bytes_received;
    snap = lfl_snapshot(j, &nsnap);
    if (nsnap == UINT32_MAX) {
        pthread_mutex_unlock(&j->mu);
        ava1_apply_fail(j, AVA1_ERR_IO, "out of memory in a sync batch", ENOMEM, 0);
        goto out;
    }
    for (k = 0; k < nsnap; k++) {
        ava1_lfile_t *lf = j->lf[snap[k]];
        if (lf->committed || lf->committing || lf->fd < 0) {
            ava1_rset_clear(&lf->written); /* a late duplicate's range: nothing left to sync */
        } else if (lf->written.n) {
            cap_g += (uint32_t)lf->written.n;
            nlf++;
        }
        if (lf->has_root && !lf->root_journaled) nroots++;
    }
    l.fds = malloc(((size_t)n_small + 2u * nlf + ndirty + 1u) * sizeof *l.fds);
    fids = malloc(((size_t)n_small + 1u) * sizeof *fids);
    froots = malloc(((size_t)n_small + 1u) * sizeof *froots);
    lg = malloc(((size_t)n_small + 1u) * sizeof *lg);
    rg = malloc(((size_t)cap_g + 1u) * sizeof *rg);
    roots = malloc(((size_t)nroots + 1u) * sizeof *roots);
    runs = malloc(((size_t)n_small + 1u) * sizeof *runs);
    newlf = malloc(((size_t)nlf + 1u) * sizeof *newlf);
    retried = calloc((size_t)n_small + 2u * nlf + ndirty + 1u, 1);
    chk = malloc(((size_t)nlf + 1u) * sizeof *chk);
    if (!l.fds || !rg || !roots || !runs || !newlf || !retried || !chk || !fids || !froots || !lg ||
        (ava1_apply_fault && ava1_apply_fault(j, AVA1_HOOK_BATCH_ALLOC, 0))) {
        pthread_mutex_unlock(&j->mu);
        ava1_apply_fail(j, AVA1_ERR_IO, "out of memory in a sync batch", ENOMEM, 0);
        goto out;
    }
    for (i = 0; i < n_small; i++) {
        if (sfds[i] < 0) continue; /* logged: no descriptor, its record is in the pack */
        l.fds[l.n++] = sfds[i];
        fids[nfd] = ids[i];
        memcpy(froots[nfd], sroots[i], 32);
        nfd++;
    }
    pk_first = l.n;
    for (k = 0; k < j->npsegs; k++) /* the pack segments written since the last batch: one fsync each */
        if (j->psegs[k].dirty && !j->psegs[k].removed) {
            j->psegs[k].dirty = 0;
            l.fds[l.n++] = j->psegs[k].fd;
            npk++;
        }
    nroots = 0;
    for (k = 0; k < nsnap; k++) {
        ava1_lfile_t *lf = j->lf[snap[k]];
        size_t r;
        i = snap[k];
        if (lf->written.n) {
            chk[nchk].id = i;
            chk[nchk].fd = lf->fd;
            chk[nchk].ob_fd = lf->ob_fd;
            chk[nchk].fd_idx = l.n;
            chk[nchk].ob_idx = lf->ob_fd >= 0 ? (int)l.n + 1 : -1;
            chk[nchk].rg_first = ng;
            chk[nchk].rg_n = (uint32_t)lf->written.n;
            nchk++;
            for (r = 0; r < lf->written.n; r++) {
                rg[ng].file_id = i;
                rg[ng].offset = lf->written.v[2 * r];
                rg[ng].len = lf->written.v[2 * r + 1] - lf->written.v[2 * r];
                new_bytes += rg[ng].len;
                ng++;
            }
            ava1_rset_clear(&lf->written);
            l.fds[l.n++] = lf->fd;
            if (lf->ob_fd >= 0) l.fds[l.n++] = lf->ob_fd;
            if (!lf->dir_synced) newlf[nnew++] = i;
        }
        if (lf->has_root && !lf->root_journaled) {
            roots[nroots].file_id = i;
            memcpy(roots[nroots].root, lf->root, 32);
            nroots++;
        }
    }
    j->roots_new = 0;
    j->unsynced_bytes = 0;
    lfl_prune(j, snap, nsnap);
    j->syncing = 1; /* the descriptors in `l` and `chk` are used without j->mu until the sync is done */
    u1 = mono_us();
    pthread_cond_broadcast(&j->cv); /* workers waiting for pend space */
    pthread_mutex_unlock(&j->mu);

    /* 1. data sync, spread over the workers */
    l.stripes = j->want_workers ? j->want_workers : 1;
    l.job = j;
    l.ids = fids;
    l.roots = (const uint8_t(*)[32])froots;
    l.retried = retried;
    l.n_small = nfd;
    /* A stop mid-sync: some data may be unsynced, so nothing is journaled or acknowledged. */
    if ((l.n && ava1_apply_parallel(j, sync_stripe, &l, l.stripes) != 0) || l.cut || is_stopping(j)) goto out;
    for (k = 0; k < nchk && !l.err; k++) {
        if (retried[chk[k].fd_idx] || (chk[k].ob_idx >= 0 && retried[chk[k].ob_idx]))
            l.err = reread_ranges(&chk[k], rg, j->m.e[chk[k].id].size);
    }
    for (k = 0; k < npk && !l.err; k++)
        if (retried[pk_first + k]) l.err = reread_pack_batch(j, ids, ploc, (const uint8_t(*)[32])sroots, n_small);
    if (l.err) {
        ava1_apply_fail(j, AVA1_ERR_IO, "fsync failed", l.err, 0);
        goto out;
    }
    pthread_mutex_lock(&j->mu);
    j->syncing = 0; /* the descriptors are not needed any more: idle files may be closed again */
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    HOOK(j, AVA1_HOOK_BATCH_SYNCED, UINT32_MAX);
    u2 = mono_us();
    slow_drive_check(j, bytes_in > j->rate_bytes0 ? bytes_in - j->rate_bytes0 : 0, t0 - j->last_batch_end_ms, (u2 - u1) / 1000u);
    if (cfg->crash_at == AVA1_CRASH_AFTER_DATA) {
        ava1_apply_crash(j);
        goto out;
    }
    /* A new file's bytes are durable, its name only once its directory is synced. */
    if ((rc = sync_new_dirs(j, fids, nfd, newlf, nnew)) != 0) {
        if (rc > 0) ava1_apply_fail(j, AVA1_ERR_IO, "syncing a folder failed", rc, 0);
        goto out; /* rc < 0: stopped */
    }
    u3 = mono_us();
    pthread_mutex_lock(&j->mu);
    for (i = 0; i < nnew; i++)
        if (j->lf[newlf[i]]) j->lf[newlf[i]]->dir_synced = 1;
    pthread_mutex_unlock(&j->mu);
    if (cfg->crash_at == AVA1_CRASH_AFTER_SYNC) {
        ava1_apply_crash(j);
        goto out;
    }

    /* 2. journal */
    if (n_small) {
        /* Sorted ids, with each logged file's record location kept beside its id: sorting `ids` alone left
         * `ploc` in arrival order, so the sweep queue and the per-segment journal ranges paired ids with other
         * files' records (review 007 #8 found it: the sweep's re-make read the wrong record and failed). */
        idloc_t *pr = ploc && logmode ? malloc((size_t)n_small * sizeof *pr) : NULL;
        if (pr) {
            for (i = 0; i < n_small; i++) {
                pr[i].id = ids[i];
                pr[i].loc = ploc[i];
            }
            qsort(pr, n_small, sizeof *pr, idloc_cmp);
            for (i = 0; i < n_small; i++) {
                ids[i] = pr[i].id;
                ploc[i] = pr[i].loc;
            }
            free(pr);
        } else if (ploc && logmode) {
            ava1_apply_fail(j, AVA1_ERR_IO, "out of memory in a sync batch", ENOMEM, 0);
            goto out;
        } else {
            qsort(ids, n_small, sizeof *ids, u32cmp);
        }
    }
    for (i = 0; i < n_small; i++) {
        if (nr && runs[nr - 1].first + runs[nr - 1].count == ids[i]) runs[nr - 1].count++;
        else if (!nr || runs[nr - 1].first + runs[nr - 1].count < ids[i]) {
            runs[nr].first = ids[i];
            runs[nr].count = 1;
            nr++;
        }
    }
    if (!logmode) {
        if (append_batch(j, runs, nr, rg, ng, roots, nroots, 0, 0, 0) != 0) {
            ava1_apply_fail(j, AVA1_ERR_IO, "journal append failed", EIO, 0);
            goto out;
        }
    } else {
        /* One JnlBatch per pack segment the batch's records sit in (one, unless a segment rolled):
         * the files of the group as runs plus the byte range of their records. The first also carries
         * the large files' ranges and roots. The pack was fsynced above, so these records are the
         * proof for I1 (SPEC.md §15.7). */
        uint32_t g0 = 0, first = 1;
        ava1_file_run_t *gr = malloc(((size_t)n_small + 1u) * sizeof *gr);
        uint32_t *gids = malloc(((size_t)n_small + 1u) * sizeof *gids);
        if (!gr || !gids) {
            free(gr);
            free(gids);
            ava1_apply_fail(j, AVA1_ERR_IO, "out of memory in a sync batch", ENOMEM, 0);
            goto out;
        }
        for (i = 0; i < n_small; i++) {
            lg[i].seg = ploc[i].seg;
            lg[i].off = ploc[i].off;
            lg[i].len = ploc[i].len;
            lg[i].id = ids[i];
        }
        qsort(lg, n_small, sizeof *lg, lent_cmp);
        while (g0 < n_small) {
            uint32_t g1 = g0, ngr = 0, ng_ids = 0;
            uint64_t lo = lg[g0].off, hi = 0;
            int e;
            while (g1 < n_small && lg[g1].seg == lg[g0].seg) {
                gids[ng_ids++] = lg[g1].id;
                if (lg[g1].off + lg[g1].len > hi) hi = lg[g1].off + lg[g1].len;
                g1++;
            }
            qsort(gids, ng_ids, sizeof *gids, u32cmp);
            for (i = 0; i < ng_ids; i++) {
                if (ngr && gr[ngr - 1].first + gr[ngr - 1].count == gids[i]) gr[ngr - 1].count++;
                else {
                    gr[ngr].first = gids[i];
                    gr[ngr].count = 1;
                    ngr++;
                }
            }
            e = append_batch(j, gr, ngr, first ? rg : NULL, first ? ng : 0, first ? roots : NULL, first ? nroots : 0,
                             lg[g0].seg, lo, hi - lo);
            first = 0;
            if (e != 0) {
                free(gr);
                free(gids);
                ava1_apply_fail(j, AVA1_ERR_IO, "journal append failed", EIO, 0);
                goto out;
            }
            g0 = g1;
        }
        free(gr);
        free(gids);
    }
    HOOK(j, AVA1_HOOK_BATCH_JOURNALED, UINT32_MAX);
    if (cfg->crash_at == AVA1_CRASH_AFTER_JOURNAL) {
        ava1_apply_crash(j);
        goto out;
    }

    /* 3. state, then Durable */
    pthread_mutex_lock(&j->mu);
    for (i = 0; i < n_small; i++) {
        if (!ava1_bits_get(&j->done, ids[i])) {
            ava1_bits_set(&j->done, ids[i]);
            j->files_done++;
            j->bytes_durable += j->m.e[ids[i]].size;
        }
        if (logmode) { /* done, but only durable through the log until the sweep reaches it */
            ava1_usw_t u;
            if (j->usw_head + j->usw_n == j->usw_cap) {
                if (j->usw_head) {
                    memmove(j->usw, j->usw + j->usw_head, (size_t)j->usw_n * sizeof *j->usw);
                    j->usw_head = 0;
                } else {
                    uint32_t c = j->usw_cap ? j->usw_cap * 2 : 256;
                    ava1_usw_t *q = realloc(j->usw, (size_t)c * sizeof *q);
                    if (q) {
                        j->usw = q;
                        j->usw_cap = c;
                    }
                }
            }
            u.id = ids[i];
            u.seg = ploc[i].seg;
            u.off = ploc[i].off;
            u.len = ploc[i].len;
            u.t_ms = ava1_mono_ms();
            if (j->usw_head + j->usw_n < j->usw_cap) {
                j->usw[j->usw_head + j->usw_n++] = u;
                j->unswept_n++;
            } else {
                pack_unref_locked(j, u.seg, u.len); /* cannot track it: it is swept at once below */
            }
        }
    }
    for (i = 0; i < ng; i++) {
        ava1_lfile_t *lf = j->lf[rg[i].file_id];
        if (lf) (void)ava1_rset_add(&lf->durable, rg[i].offset, rg[i].offset + rg[i].len);
    }
    for (i = 0; i < nroots; i++)
        if (j->lf[roots[i].file_id] && memcmp(j->lf[roots[i].file_id]->root, roots[i].root, 32) == 0)
            j->lf[roots[i].file_id]->root_journaled = 1;
    j->bytes_durable += new_bytes;
    pthread_mutex_unlock(&j->mu);
    for (i = 0; i < n_small; i++)
        if (sfds[i] >= 0) close(sfds[i]);
    ava1_pend_release(held);
    held = 0;
    nfd = 0;
    n_small = 0;
    if (nr || ng) emit_durable(j, runs, nr, rg, ng);
    {
        uint64_t u4 = mono_us();
        j->st_batches++;
        j->st_files += nfiles;
        j->st_scan_us += u1 - u0;
        j->st_data_us += u2 - u1;
        j->st_dirs_us += u3 - u2;
        j->st_jnl_us += u4 - u3;
        j->tot_batches++;
        j->tot_files += nfiles;
        j->tot_scan_us += u1 - u0;
        j->tot_data_us += u2 - u1;
        j->tot_dirs_us += u3 - u2;
        j->tot_jnl_us += u4 - u3;
    }
    if (ava1_jnl_len(&j->jnl) > AVA1_JNL_COMPACT_AT) {
        uint64_t c0 = mono_us();
        /* A compaction waits for commits in flight and is retried each batch; a job that always has
         * one would never compact. Past twice the limit, drain them first (new commits are queued
         * only by this thread, so the wait ends). */
        if (ava1_jnl_len(&j->jnl) > 2ull * AVA1_JNL_COMPACT_AT) {
            pthread_mutex_lock(&j->mu);
            while ((j->commits_inflight || j->sweeps_inflight) && !j->stopping) pthread_cond_wait(&j->cv, &j->mu);
            pthread_mutex_unlock(&j->mu);
        }
        (void)ava1_apply_compact(j);
        j->st_compact_us += mono_us() - c0;
        j->st_compacts++;
    }
    pthread_mutex_lock(&j->mu);
    j->rate_bytes0 = j->bytes_received;
    pthread_mutex_unlock(&j->mu);
    j->last_batch_end_ms = ava1_mono_ms();
    {
        uint64_t dt = ava1_mono_ms() - t0;
        if (dt > 1500 && j->batch_max > 16) j->batch_max /= 2;
        /* past AVA1_PEND_MAX the count trigger could never fire: pend_add caps there */
        else if (dt < 500 && j->batch_max < AVA1_PEND_MAX) j->batch_max *= 2;
    }
out:
    pthread_mutex_lock(&j->mu);
    j->syncing = 0;
    pthread_mutex_unlock(&j->mu);
    for (i = 0; i < n_small; i++)
        if (sfds[i] >= 0) close(sfds[i]);
    ava1_pend_release(held); /* an early `goto out` leaves nfd at 0 with the reservations still taken */
    free(ploc);
    free(fids);
    free(froots);
    free(lg);
    free(snap);
    free(ids);
    free(sfds);
    free(sroots);
    free(retried);
    free(chk);
    free(l.fds);
    free(rg);
    free(roots);
    free(runs);
    free(newlf);
}

int ava1_apply_compact(ava1_job_t *j) {
    ava1_jnl_open_t o;
    ava1_jnl_snapshot_t s;
    ava1_w_t ow, fw, rw, tw, sw;
    size_t nr = 0, cap;
    uint32_t *snap, nsnap, k;
    uint8_t ob[1200], *fb, *rb, *tb, *sb, *ub, *sg;
    size_t extra;
    memset(&o, 0, sizeof o);
    memcpy(o.job_id, j->id, 16);
    memcpy(o.manifest_hash, j->manifest_hash, 32);
    o.kind = j->kind;
    o.flags = j->flags;
    o.staged = (uint8_t)((j->staged ? 1 : 0) | (j->dest_held ? AVA1_STAGED_HELD : 0));
    o.root = (const uint8_t *)j->root;
    o.root_len = (uint16_t)strlen(j->root);
    ava1_w_init(&ow, ob, sizeof ob);
    if (ava1_jnl_open_encode(&o, &ow) != 0) return -1;
    pthread_mutex_lock(&j->jnl_mu); /* no append races the rewrite; lock order jnl_mu, mu */
    pthread_mutex_lock(&j->mu);
    if (j->commits_inflight || j->sweeps_inflight) { /* a commit between its rename and its journal record would be
                                * dropped from the snapshot: try again after the batch */
        pthread_mutex_unlock(&j->mu);
        pthread_mutex_unlock(&j->jnl_mu);
        return -1;
    }
    snap = lfl_snapshot(j, &nsnap);
    if (nsnap == UINT32_MAX) { /* out of memory: keep the longer journal, it is still whole */
        pthread_mutex_unlock(&j->mu);
        pthread_mutex_unlock(&j->jnl_mu);
        return -1;
    }
    for (k = 0; k < nsnap; k++) nr += j->lf[snap[k]]->durable.n + 1;
    cap = 16u * (size_t)j->m.n + 8;
    fb = malloc(cap);
    rb = malloc(32u * nr + 8);
    tb = malloc(48u * (size_t)j->m.n + 8);
    extra = 24u * (size_t)j->usw_n + 64u * (size_t)j->npsegs + 64u;
    sb = malloc(cap + 32u * nr + 48u * (size_t)j->m.n + 64 + extra);
    ub = malloc(24u * (size_t)j->usw_n + 16u);
    sg = malloc(64u * (size_t)j->npsegs + 16u);
    if (fb && rb && tb && sb && ub && sg) {
        ava1_w_init(&fw, fb, cap);
        ava1_w_init(&rw, rb, 32u * nr + 8);
        ava1_w_init(&tw, tb, 48u * (size_t)j->m.n + 8);
        (void)ava1_bits_append_runs(&j->done, &fw);
        for (k = 0; k < nsnap; k++) {
            uint32_t i = snap[k];
            ava1_lfile_t *lf = j->lf[i];
            size_t r;
            if (ava1_bits_get(&j->done, i)) continue;
            for (r = 0; r < lf->durable.n; r++) {
                ava1_file_range_t g;
                g.file_id = i;
                g.offset = lf->durable.v[2 * r];
                g.len = lf->durable.v[2 * r + 1] - g.offset;
                (void)ava1_file_range_append(&rw, &g);
            }
            if (lf->has_root && lf->root_journaled) {
                ava1_root_item_t r;
                r.file_id = i;
                memcpy(r.root, lf->root, 32);
                (void)ava1_root_item_append(&tw, &r);
            }
        }
        /* the files done but not yet swept, and where their records are (SPEC.md §15.7) */
        memset(&s, 0, sizeof s);
        if (j->usw_n) {
            ava1_w_t uw, pw;
            uint32_t *uids = malloc((size_t)j->usw_n * sizeof *uids), n, nrun = 0, sidx;
            ava1_file_run_t run = { 0, 0 };
            if (uids) {
                for (n = 0; n < j->usw_n; n++) uids[n] = j->usw[j->usw_head + n].id;
                qsort(uids, j->usw_n, sizeof *uids, u32cmp);
                ava1_w_init(&uw, ub, 24u * (size_t)j->usw_n + 16u);
                for (n = 0; n < j->usw_n; n++) {
                    if (nrun && run.first + run.count == uids[n]) {
                        run.count++;
                        continue;
                    }
                    if (nrun) (void)ava1_file_run_append(&uw, &run);
                    run.first = uids[n];
                    run.count = 1;
                    nrun++;
                }
                if (nrun) (void)ava1_file_run_append(&uw, &run);
                s.has_unswept = 1;
                s.unswept = ub;
                s.unswept_len = (uint32_t)uw.len;
                ava1_w_init(&pw, sg, 64u * (size_t)j->npsegs + 16u);
                for (sidx = 0; sidx < j->npsegs; sidx++) { /* one ref per segment: the span of its unswept records */
                    ava1_pack_ref_t ref;
                    int any = 0;
                    memset(&ref, 0, sizeof ref);
                    for (n = 0; n < j->usw_n; n++) {
                        const ava1_usw_t *u = &j->usw[j->usw_head + n];
                        uint32_t last;
                        if (u->seg != sidx) continue;
                        if (!any) {
                            ref.segment = sidx;
                            ref.offset = u->off;
                            ref.len = u->off + u->len;
                            ref.first_file = last = u->id;
                            ref.count = 1;
                            any = 1;
                            (void)last;
                        } else {
                            uint64_t end = ref.len, uend = u->off + u->len;
                            uint32_t hi = ref.first_file + ref.count - 1;
                            if (u->off < ref.offset) ref.offset = u->off;
                            if (uend > end) end = uend;
                            ref.len = end;
                            if (u->id < ref.first_file) {
                                ref.count += ref.first_file - u->id;
                                ref.first_file = u->id;
                            } else if (u->id > hi) {
                                ref.count += u->id - hi;
                            }
                        }
                    }
                    if (any) {
                        ref.len -= ref.offset; /* len held the end until here */
                        (void)ava1_pack_ref_append(&pw, &ref);
                    }
                }
                s.has_segments = 1;
                s.segments = sg;
                s.segments_len = (uint32_t)pw.len;
            }
            free(uids);
        }
        s.done = fb;
        s.done_len = (uint32_t)fw.len;
        s.ranges = rb;
        s.ranges_len = (uint32_t)rw.len;
        s.roots = tb;
        s.roots_len = (uint32_t)tw.len;
        ava1_w_init(&sw, sb, cap + 32u * nr + 48u * (size_t)j->m.n + 64 + extra);
        if (ava1_jnl_snapshot_encode(&s, &sw) == 0) (void)ava1_jnl_compact(&j->jnl, ob, ow.len, sb, sw.len, NULL, 0);
    }
    pthread_mutex_unlock(&j->mu);
    pthread_mutex_unlock(&j->jnl_mu);
    free(snap);
    free(fb);
    free(rb);
    free(tb);
    free(sb);
    free(ub);
    free(sg);
    return 0;
}

/* ---- commit ------------------------------------------------------------------------ */

void ava1_apply_reset(ava1_job_t *j, uint32_t id, uint16_t reason) {
    ava1_jnl_reset_t r;
    uint8_t b[16];
    ava1_w_t w;
    ava1_lfile_t *lf = j->lf[id];
    r.file_id = id;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_jnl_reset_encode(&r, &w) == 0) (void)ava1_apply_jnl_append(j, AVA1_JNL_RESET, b, w.len);
    pthread_mutex_lock(&j->mu);
    j->bytes_durable -= ava1_rset_covered(&lf->durable);
    ava1_rset_clear(&lf->written);
    ava1_rset_clear(&lf->durable);
    lf->has_root = lf->root_journaled = 0;
    lf->committing = 0; /* a commit that found its bytes wrong starts the file over */
    pthread_mutex_unlock(&j->mu);
    if (lf->ob_fd >= 0) {
        uint64_t n = groups_of(j->m.e[id].size) * 32u;
        (void)ftruncate(lf->ob_fd, 0);
        (void)ftruncate(lf->ob_fd, (off_t)n);
    }
    if (reason) emit_retry(j, id, reason);
}

static int read_root(ava1_job_t *j, uint32_t id, uint8_t root[32]) {
    ava1_lfile_t *lf = j->lf[id];
    uint64_t size = j->m.e[id].size, n = groups_of(size);
    uint8_t *buf;
    ssize_t k;
    if (n >= 2) {
        buf = malloc((size_t)n * 32u);
        if (!buf) return -ENOMEM;
        k = pread(lf->ob_fd, buf, (size_t)n * 32u, 0);
        if (k == (ssize_t)(n * 32u)) ava1_b3_root_from_cvs((const uint8_t (*)[32])buf, n, root);
    } else {
        buf = malloc(size ? (size_t)size : 1);
        if (!buf) return -ENOMEM;
        k = pread(lf->fd, buf, (size_t)size, 0);
        if (k == (ssize_t)size) ava1_b3_hash(buf, (size_t)size, root);
        else k = -1;
        if (k >= 0) k = (ssize_t)(n * 32u);
    }
    free(buf);
    return k == (ssize_t)(n * 32u) ? 0 : -EIO;
}

/* A commit runs on a worker, and workers never end the job themselves (see worker_fail): the
 * failure is recorded and the job thread ends it, journaling Done when `journal_done`. */
static void commit_fail(ava1_job_t *j, uint16_t status, const char *what, int err, int journal_done) {
    pthread_mutex_lock(&j->mu);
    if (!j->finished && !j->final_status) {
        j->final_status = status;
        j->fail_journal = journal_done;
        snprintf(j->message, sizeof j->message, "%s%s%s", what, err ? ": " : "", err ? strerror(err) : "");
    }
    pthread_mutex_unlock(&j->mu);
}

static void commit_large(ava1_job_t *j, uint32_t id) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_lfile_t *lf = j->lf[id];
    const ava1_ment_t *e = &j->m.e[id];
    char part[PATH_CAP], fin[PATH_CAP], parent[PATH_CAP], ob[600];
    uint8_t root[32], want[32];
    int err = 0;
    pthread_mutex_lock(&j->mu);
    if (j->finished || j->final_status) { /* the job already ended or a sibling failed: do nothing more */
        lf->committing = 0;
        pthread_mutex_unlock(&j->mu);
        return;
    }
    if (cfg->crash_at == AVA1_CRASH_BEFORE_COMMIT) {
        pthread_mutex_unlock(&j->mu);
        ava1_apply_crash(j);
        return;
    }
    if (lf->fd < 0 && !lfile_open(j, id, &err, 0)) {
        pthread_mutex_unlock(&j->mu);
        if (err == ENOENT) { /* its bytes are gone: start the file over */
            ava1_apply_reset(j, id, AVA1_RETRY_IO);
            return;
        }
        commit_fail(j, AVA1_ERR_IO, "reopen for commit failed", err, 0);
        return;
    }
    memcpy(want, lf->root, 32); /* ava1_apply_root may replace it from another thread */
    pthread_mutex_unlock(&j->mu);
    if (read_root(j, id, root) != 0) {
        commit_fail(j, AVA1_ERR_IO, "reading the outboard failed", EIO, 0);
        return;
    }
    if (memcmp(root, want, 32) != 0) {
        ava1_apply_reset(j, id, AVA1_RETRY_VERIFY);
        return;
    }
    ava1_apply_path(j, id, 1, part, sizeof part);
    ava1_apply_path(j, id, 0, fin, sizeof fin);
    if (!part[0] || !fin[0]) {
        commit_fail(j, AVA1_ERR_IO, "path too long", ENAMETOOLONG, 0);
        return;
    }
    HOOK(j, AVA1_HOOK_COMMIT_VERIFIED, id);
    /* From here on a late duplicate chunk is dropped (write_chunk checks `committed`), and
     * one that slipped in before this point is forgotten: the fd is about to close. */
    pthread_mutex_lock(&j->mu);
    lf->committed = 1;
    ava1_rset_clear(&lf->written);
    pthread_mutex_unlock(&j->mu);
    /* Truncate (preallocate may have reserved more) before the mtime: on Linux ftruncate
     * itself sets the mtime. The fsync makes all three durable. */
    (void)fchmod(lf->fd, AVA1_CONSOLE_FILE_MODE);
    if (ftruncate(lf->fd, (off_t)e->size) != 0) {
        commit_fail(j, AVA1_ERR_IO, "final truncate failed", errno, 0);
        return;
    }
    ava1_platform_set_mtime(lf->fd, part, e->mtime);
    if ((err = ava1_fsync_retry(lf->fd, stopping_cb, j, NULL)) != 0) {
        commit_fail(j, AVA1_ERR_IO, "final sync failed", err, 0);
        return;
    }
    pthread_mutex_lock(&j->mu);
    ava1_lf_close_fds(j, id, lf); /* closes both and gives the slots back */
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    if (strcmp(part, fin) != 0) {
        parent_of(fin, parent, sizeof parent);
        /* Same directory by construction; checked anyway (SPEC.md §12.6, the kernel panic). */
        {
            /* Fail closed: only a definite 1 may reach rename(). -1 (a stat failed) is not "same";
             * a false refusal costs a retry, a false allow costs a kernel panic (review 007 #4). */
            int sd = cfg->same_device ? cfg->same_device(part, parent) : -1; /* no hook = unknown */
            if (sd == 0) {
                commit_fail(j, AVA1_ERR_CROSS_DEVICE, "the destination is on another drive", 0, 1);
                return;
            }
            if (sd != 1) {
                commit_fail(j, AVA1_ERR_IO, "could not verify the destination drive", errno, 1);
                return;
            }
        }
        if (rename(part, fin) != 0) {
            int e = errno;
            if (in_the_way(e)) commit_fail(j, AVA1_ERR_EXISTS, "something is already where the file goes", e, 1);
            else commit_fail(j, AVA1_ERR_IO, "rename into place failed", e, 1);
            return;
        }
        HOOK(j, AVA1_HOOK_RENAMED, id);
        if ((err = sync_dir(parent)) != 0) {
            commit_fail(j, AVA1_ERR_IO, "syncing the folder failed", err, 1);
            return;
        }
        HOOK(j, AVA1_HOOK_DIR_SYNCED, id);
        if (cfg->crash_at == AVA1_CRASH_COMMIT_RENAMED) {
            ava1_apply_crash(j);
            return;
        }
    }
    {
        ava1_jnl_batch_t b;
        ava1_file_run_t r = { id, 1 };
        uint8_t rb[32], body[64];
        ava1_w_t rw, w;
        ava1_w_init(&rw, rb, sizeof rb);
        (void)ava1_file_run_append(&rw, &r);
        memset(&b, 0, sizeof b);
        b.files = rb;
        b.files_len = (uint32_t)rw.len;
        ava1_w_init(&w, body, sizeof body);
        if (ava1_jnl_batch_encode(&b, &w) != 0 || ava1_apply_jnl_append(j, AVA1_JNL_BATCH, body, w.len) != 0) {
            commit_fail(j, AVA1_ERR_IO, "journal append failed", EIO, 0);
            return;
        }
        HOOK(j, AVA1_HOOK_JOURNALED, id);
        /* only once the commit is journaled: until then a replay may still need the CVs */
        ob_path(j, id, ob, sizeof ob);
        (void)unlink(ob);
        HOOK(j, AVA1_HOOK_OB_UNLINKED, id);
        pthread_mutex_lock(&j->mu);
        ava1_bits_set(&j->done, id);
        j->files_done++;
        pthread_mutex_unlock(&j->mu);
        emit_durable(j, &r, 1, NULL, 0);
    }
}

/* Runs on a worker (or inline when the queue item could not be allocated). */
static void run_commit(ava1_job_t *j, uint32_t id) {
    uint64_t t0 = mono_us();
    commit_large(j, id);
    __atomic_add_fetch(&j->st_commit_us, mono_us() - t0, __ATOMIC_RELAXED);
    __atomic_add_fetch(&j->tot_commit_us, mono_us() - t0, __ATOMIC_RELAXED);
    pthread_mutex_lock(&j->mu);
    j->commits_inflight--;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

/* commit_large on a worker: queued here, run by run_work. The queue item names the file;
 * `committing` (set under mu when it is queued) keeps the next scan from queueing it again. */
void ava1_apply_commit_ready(ava1_job_t *j) {
    uint32_t *snap, n, k;
    pthread_mutex_lock(&j->mu);
    snap = lfl_snapshot(j, &n);
    pthread_mutex_unlock(&j->mu);
    if (n == UINT32_MAX) return; /* out of memory: the next batch asks again */
    for (k = 0; k < n && !j->finished; k++) {
        uint32_t i = snap[k];
        ava1_lfile_t *lf;
        int ready;
        ava1_work_t *w;
        pthread_mutex_lock(&j->mu);
        lf = j->lf[i]; /* the same lf the snapshot saw: only a commit or a reset frees one, never this scan */
        ready = lf && !lf->committed && !lf->committing && !ava1_bits_get(&j->done, i) && lf->has_root &&
                lf->root_journaled && !lf->written.n &&
                (j->m.e[i].size == 0 || ava1_rset_covers(&lf->durable, 0, j->m.e[i].size));
        if (ready) {
            lf->committing = 1;
            j->commits_inflight++;
        }
        pthread_mutex_unlock(&j->mu);
        if (!ready) continue;
        w = calloc(1, sizeof *w);
        if (!w) { /* run it here rather than lose it */
            run_commit(j, i);
            continue;
        }
        w->kind = AVA1_W_COMMIT;
        w->file_id = i;
        enqueue(j, w, 0);
    }
    free(snap);
}

/* ---- finishing --------------------------------------------------------------------- */

static void finish(ava1_job_t *j) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    char parent[PATH_CAP];
    struct stat st;
    int e;
    /* A console copy or move: a move deletes its source once the job ends (SPEC.md §15.5), and what was
     * copied must be durable in place by then, not only in the log. */
    if (j->kind == AVA1_JOB_COPY && (e = ava1_pack_drain(j)) != 0) {
        /* EINTR: the job is stopping. Nothing was decided: journaling a terminal failure now would make a
         * resume see a failed job. */
        if (e != EINTR) ava1_apply_fail(j, AVA1_ERR_IO, "making the copied files durable failed", e, 1);
        return;
    }
    if (j->staged && !(j->flags & AVA1_JF_SINGLE_FILE)) {
        /* The sweep addresses files by path and the rename below moves them: settle first (the one
         * place the tail of a staged upload waits; merges and single files settle behind JobDone). */
        if ((e = ava1_pack_drain(j)) != 0) {
            if (e != EINTR) ava1_apply_fail(j, AVA1_ERR_IO, "making the last files durable failed", e, 1);
            return; /* EINTR: stopping, so no Done is journaled (the resume finishes the job) */
        }
        parent_of(j->root, parent, sizeof parent);
        /* A held root is our own empty lock folder (SPEC.md §11.6): the rename below
         * replaces it only while it is still empty. */
        if (!j->dest_held && stat(j->root, &st) == 0) {
            ava1_apply_fail(j, AVA1_ERR_EXISTS, "the destination appeared during the upload; the files are in .ava-part", 0, 1);
            return;
        }
        {
            int sd = cfg->same_device ? cfg->same_device(j->base, parent) : -1; /* no hook = unknown */ /* only a definite 1 may rename (review 007 #4) */
            if (sd == 0) {
                ava1_apply_fail(j, AVA1_ERR_CROSS_DEVICE, "the destination is on another drive", 0, 1);
                return;
            }
            if (sd != 1) {
                ava1_apply_fail(j, AVA1_ERR_IO, "could not verify the destination drive", errno, 1);
                return;
            }
        }
        if (rename(j->base, j->root) != 0) {
            e = errno;
            if (in_the_way(e)) ava1_apply_fail(j, AVA1_ERR_EXISTS, "the destination is not empty; the files are in .ava-part", e, 1);
            else ava1_apply_fail(j, AVA1_ERR_IO, "renaming the finished folder failed", e, 1);
            return;
        }
        HOOK(j, AVA1_HOOK_RENAMED, UINT32_MAX);
        e = ava1_apply_fault ? ava1_apply_fault(j, AVA1_HOOK_DIR_SYNCED, UINT32_MAX) : 0;
        if (!e) e = sync_dir(parent);
        if (e) {
            /* The tree is in place and complete; only the rename's own durability is in
             * doubt. Reporting ERR_IO would make the sender resend a finished tree. */
            ava1_apply_fail(j, j->kind == AVA1_JOB_COPY ? AVA1_ERR_IO : AVA1_STATUS_OK,
                            "the folder is in place; syncing its parent failed", e, 1);
            return;
        }
        HOOK(j, AVA1_HOOK_DIR_SYNCED, UINT32_MAX);
        if (cfg->crash_at == AVA1_CRASH_STAGED_RENAMED) {
            ava1_apply_crash(j);
            return;
        }
    }
    ava1_apply_fail(j, AVA1_STATUS_OK, "", 0, 1); /* the success path: see ava1_apply_fail */
}

void ava1_apply_finish_landed(ava1_job_t *j) {
    char parent[PATH_CAP];
    int err;
    parent_of(j->root, parent, sizeof parent);
    err = sync_dir(parent);
    if (err) ava1_apply_fail(j, AVA1_ERR_IO, "syncing the landed folder failed", err, 1);
    else ava1_apply_fail(j, AVA1_STATUS_OK, "", 0, 1);
}

int ava1_apply_quiesce(ava1_job_t *j) {
    uint32_t i;
    int pend, ok, rc = 0;
    pthread_mutex_lock(&j->mu);
    while ((j->q_len || j->busy) && !j->stopping) {
        /* A worker may be waiting in pend_add for room: make it, or nobody ever will. */
        int sync = j->pend_n != 0, can = j->prepared && !j->finished && !j->final_status;
        if (sync && !can) {
            for (i = 0; i < j->pend_n; i++) {
                if (j->pend_fd[i] >= 0) close(j->pend_fd[i]); /* unsynced: sent again */
                else pack_unref_locked(j, j->pend_loc[i].seg, j->pend_loc[i].len);
            }
            ava1_pend_release(j->pend_n_fd);
            j->pend_n = j->pend_n_fd = 0;
            pthread_cond_broadcast(&j->cv);
        }
        pthread_mutex_unlock(&j->mu);
        if (sync && can) sync_batch(j);
        else ava1_platform_sleep_ms(2);
        pthread_mutex_lock(&j->mu);
    }
    pend = j->pend_n || j->unsynced_bytes || j->roots_new;
    ok = j->prepared && !j->finished && !j->final_status && !j->stopping;
    pthread_mutex_unlock(&j->mu);
    if (pend && ok) sync_batch(j);
    /* a changed manifest renumbers files: nothing may stay unswept, or the queue would name other files */
    if (ok) rc = ava1_pack_drain(j);
    /* Whatever could not be made durable is dropped: it is not in the map, so it is sent again. */
    pthread_mutex_lock(&j->mu);
    for (i = 0; i < j->pend_n; i++) {
        if (j->pend_fd[i] >= 0) close(j->pend_fd[i]);
        else pack_unref_locked(j, j->pend_loc[i].seg, j->pend_loc[i].len);
    }
    ava1_pend_release(j->pend_n_fd);
    j->pend_n = j->pend_n_fd = 0;
    if (j->lf) {
        uint32_t *snap, n, k;
        snap = lfl_snapshot(j, &n);
        if (n == UINT32_MAX) { /* out of memory: clear them all, as before the index */
            for (i = 0; i < j->m.n; i++)
                if (j->lf[i]) ava1_rset_clear(&j->lf[i]->written);
        } else {
            for (k = 0; k < n; k++) ava1_rset_clear(&j->lf[snap[k]]->written);
        }
        free(snap);
    }
    j->unsynced_bytes = 0;
    pthread_mutex_unlock(&j->mu);
    return rc;
}

static int all_done(ava1_job_t *j) {
    int d;
    pthread_mutex_lock(&j->mu);
    d = j->prepared && !j->finished && !j->final_status && j->files_done >= j->m.files && j->pend_n == 0 && j->q_len == 0 && j->busy == 0;
    pthread_mutex_unlock(&j->mu);
    return d;
}

static void tune_workers(ava1_job_t *j, uint64_t now) {
    uint8_t want;
    double secs = (double)(now - j->tune_ms) / 1000.0;
    pthread_mutex_lock(&j->mu);
    want = ava1_wtune_step(&j->tune, secs > 0 ? j->applied_since_tune / secs : 0,
                           j->tune_ticks && j->tune_busy * 2 >= j->tune_ticks);
    j->applied_since_tune = 0;
    j->tune_ticks = j->tune_busy = 0;
    pthread_mutex_unlock(&j->mu);
    j->tune_ms = now;
    if (want > j->nworkers) (void)add_workers(j, want);
    pthread_mutex_lock(&j->mu);
    j->want_workers = want <= j->nworkers ? want : j->nworkers;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

/* One line of stderr.log per ten seconds of batches: where this job's time goes. */
static void log_stats(ava1_job_t *j, uint64_t now) {
    double b = (double)j->st_batches;
    j->st_log_ms = now;
    fprintf(stderr,
            "[ava1] job %02x%02x%02x%02x: %u/%u files, %llu batches (%.0f files each), per batch ms: "
            "scan %.2f data %.1f dirs %.1f journal %.1f commit %.1f; %llu compactions %.1f ms total; "
            "%u large in flight\n",
            j->id[0], j->id[1], j->id[2], j->id[3], j->files_done, j->m.files,
            (unsigned long long)j->st_batches, (double)j->st_files / b, (double)j->st_scan_us / b / 1000.0,
            (double)j->st_data_us / b / 1000.0, (double)j->st_dirs_us / b / 1000.0,
            (double)j->st_jnl_us / b / 1000.0, (double)j->st_commit_us / b / 1000.0,
            (unsigned long long)j->st_compacts, (double)j->st_compact_us / 1000.0, j->lfl_n);
    j->st_batches = j->st_files = j->st_data_us = j->st_dirs_us = j->st_jnl_us = j->st_scan_us = 0;
    j->st_commit_us = j->st_compact_us = j->st_compacts = 0;
}

/* Durable-by-log (§3.3): queues a sweep when a logged file is old enough (every one once the job has
 * ended: nothing more is coming), and removes the pack segments when the last file is swept. */
static void maybe_sweep(ava1_job_t *j, uint64_t now) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    int want = 0, settle = 0;
    ava1_work_t *w;
    pthread_mutex_lock(&j->mu);
    if (!j->stopping && j->usw_n && !j->sweep_queued && now >= j->sweep_retry_ms &&
        (j->finished || j->final_status || now - j->usw[j->usw_head].t_ms >= cfg->sweep_age_ms)) {
        j->sweep_queued = want = 1;
    }
    if (j->finished && !j->usw_n && !j->unswept_n && !j->sweeps_inflight && !j->settled && j->npsegs) settle = 1;
    pthread_mutex_unlock(&j->mu);
    if (settle) {
        pack_forget_all(j);
        pthread_mutex_lock(&j->mu);
        j->settled = 1;
        pthread_mutex_unlock(&j->mu);
        ava1_apply_status(j); /* the last one: unswept is gone, a waiting sender may report */
    }
    if (!want) return;
    w = calloc(1, sizeof *w);
    if (!w) {
        pthread_mutex_lock(&j->mu);
        j->sweep_queued = 0;
        pthread_mutex_unlock(&j->mu);
        return;
    }
    w->kind = AVA1_W_SWEEP;
    enqueue(j, w, 0);
}

/* Review 006 #2: 1 when a receiving job has made no progress for its deadline although its sender is
 * attached and owes bytes. A Ping is a byte, so the connection's liveness cannot see a sender whose data
 * pump is wedged (a source read stuck on a network share); this can. The clock only runs while we are
 * waiting on the sender alone: nothing queued, applying or waiting for a sync batch (a slow drive is
 * never a stall), and restarts on every frame, root, write or batch (the signature below). A resumed job (decided once, see `resumed`) gets the long limit: its sender may hash what it will not resend for a
 * long time without producing a frame. */
static int progress_stalled(ava1_job_t *j, uint64_t now) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    uint64_t sig, frames, gen;
    int owed, ready, attached;
    pthread_mutex_lock(&j->cmu);
    frames = j->frames_in;
    ready = j->ready;
    attached = j->attached;
    gen = j->att_gen;
    pthread_mutex_unlock(&j->cmu);
    pthread_mutex_lock(&j->mu);
    /* Once per attach, not per re-arm: a reattached job that already holds durable work is a resume. */
    if (gen != j->prog_gen) {
        j->prog_gen = gen;
        if (j->bytes_durable || j->files_done) j->resumed = 1;
    }
    owed = j->kind == AVA1_JOB_UPLOAD && !j->role && j->prepared && !j->finished && !j->final_status &&
           !j->stopping && j->files_done + j->pend_n < j->m.files && !j->q_len && !j->busy && !j->pend_n &&
           !j->unsynced_bytes && !j->roots_new;
    sig = frames + j->bytes_received + j->bytes_durable + j->files_done;
    pthread_mutex_unlock(&j->mu);
    if (!owed || !ready || !attached) {
        j->prog_armed = 0;
        return 0;
    }
    if (!j->prog_armed || sig != j->prog_sig) {
        if (!j->prog_armed) {
            uint32_t fresh = cfg->progress_ms ? cfg->progress_ms : AVA1_PROGRESS_MS;
            uint32_t resume = cfg->resume_progress_ms ? cfg->resume_progress_ms : AVA1_RESUME_PROGRESS_MS;
            pthread_mutex_lock(&j->mu);
            j->prog_limit_ms = j->resumed ? resume : fresh;
            pthread_mutex_unlock(&j->mu);
        }
        j->prog_armed = 1;
        j->prog_sig = sig;
        j->prog_at_ms = now;
        return 0;
    }
    return now - j->prog_at_ms >= j->prog_limit_ms;
}

static void *job_main(void *arg) {
    ava1_job_t *j = arg;
    for (;;) {
        uint64_t now, flush;
        int ev, batch, failed, fail_journal = 0;
        uint16_t fail_status = 0;
        char fail_msg[sizeof j->message];
        ava1_platform_sleep_ms(TICK_MS);
        now = ava1_mono_ms();
        pthread_mutex_lock(&j->mu);
        if (j->stopping) {
            pthread_mutex_unlock(&j->mu);
            break;
        }
        j->ticks++;
        j->tune_ticks++;
        if (j->q_len) {
            j->q_busy_ticks++;
            j->tune_busy++;
        }
        ev = j->ev_end || j->ev_resume;
        flush = j->credit_back;
        j->credit_back = 0;
        failed = !j->finished && j->final_status != 0; /* a worker's failure (worker_fail) */
        if (failed) {
            fail_status = j->final_status;
            fail_journal = j->fail_journal;
            memcpy(fail_msg, j->message, sizeof fail_msg);
        }
        batch = j->prepared && !j->finished && !failed && !__atomic_load_n(&ava1_apply_hold_batches, __ATOMIC_SEQ_CST) &&
                ((j->pend_n && (j->pend_n >= j->batch_max || ava1_pend_full())) || j->unsynced_bytes >= BATCH_BYTES ||
                 (j->lf_wait && (j->unsynced_bytes || j->roots_new)) || /* workers wait for a descriptor slot */
                 ((j->pend_n || j->unsynced_bytes || j->roots_new) && now - j->last_batch_ms >= BATCH_MS));
        pthread_mutex_unlock(&j->mu);
        if (failed) ava1_apply_fail(j, fail_status, fail_msg, 0, fail_journal);
        else if (progress_stalled(j, now)) {
            fprintf(stderr, "[ava1] job %02x%02x%02x%02x: no progress for %u s while the sender is connected\n",
                    j->id[0], j->id[1], j->id[2], j->id[3], j->prog_limit_ms / 1000u);
            /* The journal stays open: the sender resumes it. */
            ava1_apply_fail(j, AVA1_ERR_STALLED, "progress stalled: the sender sent no data", 0, 0);
        }
        if (ev && j->on_events) j->on_events(j);
        if (j->on_tick) j->on_tick(j);
        if (flush) emit_credit(j, flush);
        maybe_sweep(j, now);
        if (batch) {
            sync_batch(j);
            j->last_batch_ms = now;
            if (!j->stopping) ava1_apply_commit_ready(j);
        }
        if (j->timing && j->st_batches && now - j->st_log_ms >= 10000) log_stats(j, now);
        if (all_done(j)) finish(j);
        if (now - j->status_ms >= 250) {
            j->status_ms = now;
            /* while files settle behind JobDone the sender still reads Status (its `unswept`) */
            if (!j->finished || j->unswept_n) ava1_apply_status(j);
        }
        if (now - j->tune_ms >= 2000 && j->prepared && !j->finished) tune_workers(j, now);
    }
    return NULL;
}

int ava1_apply_start(ava1_job_t *j) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_wtune_init(&j->tune, cfg->workers_start, cfg->workers_min, cfg->workers_max);
    j->batch_max = 256;
    j->start_us = mono_us();
    j->timing = ava1_send_timing_enabled();
    j->tune_ms = j->status_ms = j->last_batch_ms = j->last_batch_end_ms = ava1_mono_ms();
    if (add_workers(j, cfg->workers_start) != 0 && j->nworkers == 0) return -1;
    j->want_workers = j->nworkers;
    if (ava1_thread_start(job_main, j, &j->thread) != 0) return -1;
    j->thread_started = 1;
    return 0;
}
