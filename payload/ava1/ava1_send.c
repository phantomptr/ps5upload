#include "ava1_send.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "ava1_apply.h"
#include "ava1_b3.h"
#include "ava1_events.h"
#include "ava1_data.h"
#include "ava1_platform.h"
#include "ava1_server.h"
#include "ava1_thread.h"

#define READ_AHEAD (32u << 20)

static uint64_t now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
}

uint32_t ava1_send_test_fail_sends;
uint32_t ava1_send_test_fail_writer_starts;
uint64_t ava1_send_test_chunk_bytes;

/* ---- the reader --------------------------------------------------------------------- */

static int src_path(const ava1_reader_t *r, uint32_t id, char *out, size_t cap) {
    if (r->single) return snprintf(out, cap, "%s", r->src_root) >= (int)cap ? -1 : 0;
    return snprintf(out, cap, "%s/%s", r->src_root, ava1_mstore_path(&r->j->m, id)) >= (int)cap ? -1 : 0;
}

static int pread_all(int fd, uint8_t *p, size_t n, uint64_t off) {
    while (n) {
        ssize_t k = pread(fd, p, n, (off_t)off);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) return k == 0 ? -EIO : -errno;
        p += k;
        n -= (size_t)k;
        off += (uint64_t)k;
    }
    return 0;
}

/* Flushes the bundle being built: encodes Bundle{job, records} and hands it over. */
static int flush_bundle(ava1_reader_t *r, ava1_w_t *recs) {
    ava1_bundle_t b;
    ava1_w_t w;
    size_t cap = recs->len + 64;
    uint8_t *msg;
    if (recs->len == 0) return 0;
    msg = malloc(cap);
    if (!msg) return -ENOMEM;
    memset(&b, 0, sizeof b);
    memcpy(b.job_id, r->j->id, 16);
    b.records = recs->buf;
    b.records_len = (uint32_t)recs->len;
    ava1_w_init(&w, msg, cap);
    if (ava1_bundle_encode(&b, &w) != 0) {
        free(msg);
        return -EIO;
    }
    recs->len = 0;
    return r->put(r->ctx, AVA1_TYPE_BUNDLE, msg, w.len);
}

static int send_chunk(ava1_reader_t *r, uint32_t id, uint64_t off, const uint8_t *data, size_t len) {
    ava1_chunk_t c;
    ava1_w_t w;
    size_t cap = len + 64;
    uint8_t *msg = malloc(cap);
    if (!msg) return -ENOMEM;
    memset(&c, 0, sizeof c);
    memcpy(c.job_id, r->j->id, 16);
    c.file_id = id;
    c.offset = off;
    c.data = data;
    c.data_len = (uint32_t)len;
    ava1_w_init(&w, msg, cap);
    if (ava1_chunk_encode(&c, &w) != 0) {
        free(msg);
        return -EIO;
    }
    return r->put(r->ctx, AVA1_TYPE_CHUNK, msg, w.len);
}

/* Reads a large file in runs of same "send" state (a durable range is not re-read for
 * sending, but every group's CV is still computed so the root can be produced even for
 * durable-only runs), at most one chunk long each. */
static int read_large(ava1_reader_t *r, uint32_t id, int fd, uint8_t *buf) {
    uint64_t size = r->j->m.e[id].size, n = (size + AVA1_GROUP_LEN - 1) / AVA1_GROUP_LEN, g = 0;
    const ava1_rset_t *dur = r->durable ? r->durable[id] : NULL;
    uint8_t (*cvs)[32] = n >= 2 ? malloc((size_t)n * 32u) : NULL;
    uint8_t root[32];
    int rc = 0;
    if (n >= 2 && !cvs) return -ENOMEM;
    while (g < n && rc == 0 && !*r->stop) {
        /* A run of groups with the same "send" state, at most one chunk long. */
        uint64_t off = g * AVA1_GROUP_LEN, len = 0, k;
        int send = !(dur && ava1_rset_covers(dur, off, off + (size - off < AVA1_GROUP_LEN ? size - off : AVA1_GROUP_LEN)));
        while (g < n && len < r->chunk) {
            uint64_t gs = g * AVA1_GROUP_LEN, gl = size - gs < AVA1_GROUP_LEN ? size - gs : AVA1_GROUP_LEN;
            int s2 = !(dur && ava1_rset_covers(dur, gs, gs + gl));
            if (s2 != send) break;
            len += gl;
            g++;
        }
        if ((rc = pread_all(fd, buf, (size_t)len, off)) != 0) break;
        for (k = 0; n >= 2 && k * AVA1_GROUP_LEN < len; k++) {
            uint64_t gl = len - k * AVA1_GROUP_LEN < AVA1_GROUP_LEN ? len - k * AVA1_GROUP_LEN : AVA1_GROUP_LEN;
            ava1_b3_group_cv(buf + k * AVA1_GROUP_LEN, (size_t)gl, off / AVA1_GROUP_LEN + k, cvs[off / AVA1_GROUP_LEN + k]);
        }
        if (n < 2) ava1_b3_hash(buf, (size_t)len, root);
        if (send) rc = send_chunk(r, id, off, buf, (size_t)len);
    }
    if (rc == 0 && !*r->stop) {
        if (n >= 2) ava1_b3_root_from_cvs((const uint8_t (*)[32])cvs, n, root);
        else if (size == 0) ava1_b3_hash(buf, 0, root);
        r->root(r->ctx, id, root);
    }
    free(cvs);
    return rc;
}

int ava1_read_files(ava1_reader_t *r, const uint32_t *ids, uint32_t count) {
    uint32_t i, total = ids ? count : r->j->m.n;
    size_t bcap = (size_t)r->bundle + AVA1_MAX_PATH + 128;
    uint8_t *buf = malloc(r->chunk > r->cutoff ? r->chunk : r->cutoff), *bundle = malloc(bcap);
    ava1_w_t recs;
    char path[AVA1_MAX_PATH + 600];
    int rc = 0;
    if (!buf || !bundle) {
        free(buf);
        free(bundle);
        return -ENOMEM;
    }
    ava1_w_init(&recs, bundle, bcap);
    for (i = 0; i < total && rc == 0 && !*r->stop; i++) {
        uint32_t id = ids ? ids[i] : i;
        const ava1_ment_t *e = &r->j->m.e[id];
        int fd;
        if (e->kind != AVA1_ENTRY_FILE || (r->skip && ava1_bits_get(r->skip, id))) continue;
        if (src_path(r, id, path, sizeof path) != 0) {
            rc = -ENAMETOOLONG;
            break;
        }
        fd = ava1_open_read_safe(path, ava1_data_cfg()->refuse_link);
        if (fd < 0) {
            rc = fd;
            break;
        }
        if (e->size < r->cutoff) {
            ava1_bundle_record_t br;
            if ((rc = pread_all(fd, buf, (size_t)e->size, 0)) == 0) {
                memset(&br, 0, sizeof br);
                br.file_id = id;
                ava1_b3_hash(buf, (size_t)e->size, br.root);
                br.data = buf;
                br.data_len = (uint32_t)e->size;
                if (recs.len + e->size + 64 > r->bundle) rc = flush_bundle(r, &recs);
                if (rc == 0) rc = ava1_bundle_record_append(&recs, &br);
            }
        } else {
            rc = flush_bundle(r, &recs); /* keep file order for ordered receivers */
            if (rc == 0) rc = read_large(r, id, fd, buf);
        }
        close(fd);
    }
    if (rc == 0 && !*r->stop) rc = flush_bundle(r, &recs);
    free(buf);
    free(bundle);
    return rc;
}

/* ---- the network sender ------------------------------------------------------------ */

/* A lane frame. Window rule (SPEC.md §12.3): picking a frame charges the window; the
 * charge comes back with the receiver's Credit after its apply, or here only when the
 * frame provably never left — its send failed (a failed send writes nothing, or breaks
 * the connection mid-frame, so the receiver cannot have admitted it). A lane's death
 * requeues its frames for a re-send but keeps their charge: they may be on the wire. */
typedef struct sframe {
    struct sframe *next;
    uint8_t *msg;
    size_t len;
    uint8_t type;
    uint8_t sending; /* between the pick and the settle: the writer owns it */
    uint8_t acked;   /* a Received came while it was being sent: the writer frees it */
    uint32_t seq;
    uint32_t gen; /* its lane's generation at the pick */
    uint16_t lane;
} sframe_t;

typedef struct {
    uint64_t credit, queued;
    sframe_t *ready, *ready_tail, *inflight;
    uint32_t next_seq, next_page;
    int pages_done, have_map, reading, stop, unsafe_read, single;
    ava1_bits_t skip;
    ava1_rset_t **durable;
    uint32_t *retry;
    uint32_t retry_n;
    pthread_t reader;
    int reader_started;
    pthread_t writer[AVA1_MAX_LANES + 1];
    int writer_up[AVA1_MAX_LANES + 1];      /* 0 down, 1 running, 2 start requested */
    int writer_started[AVA1_MAX_LANES + 1]; /* a joinable thread exists */
    uint32_t gen[AVA1_MAX_LANES + 1];       /* bumped by every death of the lane id */
    uint8_t wsid[AVA1_MAX_LANES + 1][16];   /* the session the lane belongs to */
    /* Stage timers (microseconds, CLOCK_MONOTONIC), printed once when the job is freed:
     * where a download's time goes. The sums are across the job's lanes. */
    pthread_mutex_t pump_mu; /* serialises the tick's and the map's thread starts and page emission */
    uint64_t t_open, walk_us, t_first_send, t_last_send, reader_end, reader_wait_us;
    uint64_t send_us, idle_us, sent_frames, sent_bytes;
} snd_t;

static snd_t *S_(ava1_job_t *j) { return (snd_t *)j->role; }

static int put_frame(void *ctx, uint8_t type, uint8_t *msg, size_t len) {
    ava1_job_t *j = ctx;
    snd_t *s = S_(j);
    sframe_t *f = calloc(1, sizeof *f);
    if (!f) {
        free(msg);
        return -ENOMEM;
    }
    f->msg = msg;
    f->len = len;
    f->type = type;
    pthread_mutex_lock(&j->mu);
    if (s->queued >= READ_AHEAD) {
        uint64_t w0 = now_us();
        while (s->queued >= READ_AHEAD && !s->stop && !j->stopping) pthread_cond_wait(&j->cv, &j->mu);
        s->reader_wait_us += now_us() - w0;
    }
    if (s->stop || j->stopping) { /* the job is ending: the read-ahead bound holds to the end */
        pthread_mutex_unlock(&j->mu);
        free(msg);
        free(f);
        return -ECANCELED;
    }
    if (type == AVA1_TYPE_CHUNK) __atomic_add_fetch(&ava1_send_test_chunk_bytes, len, __ATOMIC_SEQ_CST);
    if (s->ready_tail) s->ready_tail->next = f;
    else s->ready = f;
    s->ready_tail = f;
    s->queued += len;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    return 0;
}

static void put_root(void *ctx, uint32_t id, const uint8_t root[32]) {
    ava1_job_t *j = ctx;
    ava1_file_root_t r;
    uint8_t b[96];
    ava1_w_t w;
    memset(&r, 0, sizeof r);
    memcpy(r.job_id, j->id, 16);
    r.file_id = id;
    memcpy(r.root, root, 32);
    ava1_w_init(&w, b, sizeof b);
    if (ava1_file_root_encode(&r, &w) == 0) ava1_job_emit(j, AVA1_TYPE_FILE_ROOT, 0, b, w.len);
}

static void *reader_main(void *arg) {
    ava1_job_t *j = arg;
    snd_t *s = S_(j);
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_reader_t r;
    int rc;
    memset(&r, 0, sizeof r);
    r.j = j;
    r.src_root = j->src;
    r.single = s->single;
    r.cutoff = cfg->cutoff ? cfg->cutoff : (256u << 10); /* JobOpen carries no cutoff (C6) */
    r.chunk = 4u << 20;
    r.bundle = 1u << 20;
    r.skip = &s->skip;
    r.durable = s->durable;
    r.put = put_frame;
    r.root = put_root;
    r.ctx = j;
    r.stop = &s->stop;
    rc = ava1_read_files(&r, NULL, 0);
    s->reader_end = now_us();
    /* FileRetry: the engine asked for whole files again. */
    while (rc == 0 && !s->stop && !j->stopping) {
        uint32_t *ids = NULL, n = 0;
        pthread_mutex_lock(&j->mu);
        if (s->retry_n) {
            ids = s->retry;
            n = s->retry_n;
            s->retry = NULL;
            s->retry_n = 0;
        } else {
            pthread_cond_wait(&j->cv, &j->mu);
        }
        pthread_mutex_unlock(&j->mu);
        if (ids) {
            r.skip = NULL;    /* a retry is a full re-read of the named files (C8) */
            r.durable = NULL;
            rc = ava1_read_files(&r, ids, n);
            free(ids);
        }
    }
    if (rc != 0 && rc != -ECANCELED) ava1_apply_fail(j, AVA1_ERR_IO, "reading the source failed", -rc, 0);
    return NULL;
}

typedef struct {
    ava1_job_t *j;
    uint16_t lane;
    uint32_t gen;
    uint8_t sid[16];
} wctx2_t;

static uint64_t sat_add(uint64_t a, uint64_t b) { return a > UINT64_MAX - b ? UINT64_MAX : a + b; }

static void unlink_inflight(snd_t *s, sframe_t *f) {
    sframe_t **pp;
    for (pp = &s->inflight; *pp; pp = &(*pp)->next)
        if (*pp == f) {
            *pp = f->next;
            return;
        }
}

/* Back to the front of the ready list (caller holds mu): the next pick re-sends it. */
static void requeue_front(snd_t *s, sframe_t *f) {
    f->next = s->ready;
    s->ready = f;
    if (!s->ready_tail) s->ready_tail = f;
    s->queued += f->len;
}

/* The writer's pick (caller holds mu): the first ready frame that fits the window — a
 * front frame that does not fit never blocks a smaller one behind it (the head-of-line
 * credit stall) — charged and recorded in flight on `lane`; NULL when nothing fits. */
static sframe_t *pick_locked(snd_t *s, uint16_t lane) {
    sframe_t **pp, *f = NULL, *prev = NULL;
    for (pp = &s->ready; *pp; prev = *pp, pp = &(*pp)->next)
        if ((*pp)->len <= s->credit) {
            f = *pp;
            break;
        }
    if (!f) return NULL;
    *pp = f->next;
    if (s->ready_tail == f) s->ready_tail = prev;
    s->queued -= f->len;
    s->credit -= f->len;
    f->seq = ++s->next_seq;
    f->lane = lane;
    f->gen = s->gen[lane];
    f->sending = 1;
    f->acked = 0;
    f->next = s->inflight;
    s->inflight = f;
    return f;
}

/* The writer's resolution of one send that returned `rc` (caller holds mu). Returns 1
 * the writer goes on, 0 its lane generation is over (the lane died meanwhile), -1 the
 * frame can never be sent (the job cannot go on). */
static int settle_locked(snd_t *s, sframe_t *f, int rc) {
    int dead = s->gen[f->lane] != f->gen;
    f->sending = 0;
    if (f->acked) { /* the receiver has it; Received already took it off the in-flight list */
        free(f->msg);
        free(f);
        return dead ? 0 : 1;
    }
    if (rc != 0) {
        /* Never delivered: the charge comes back here, and the frame goes again. */
        unlink_inflight(s, f);
        s->credit = sat_add(s->credit, f->len);
        requeue_front(s, f);
    } else if (dead) {
        /* Sent on a lane that died meanwhile (the sweep left it to us): it may be on the
         * wire, so it is re-sent with its charge held, like the sweep's frames. */
        unlink_inflight(s, f);
        requeue_front(s, f);
    }
    if (dead) return 0;
    return rc == AVA1_E_TOOLONG ? -1 : 1;
}

static int take_one(uint32_t *n) {
    uint32_t v = __atomic_load_n(n, __ATOMIC_SEQ_CST);
    while (v && !__atomic_compare_exchange_n(n, &v, v - 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
    }
    return v != 0;
}

static void *writer_main(void *arg) {
    wctx2_t c = *(wctx2_t *)arg;
    ava1_job_t *j = c.j;
    uint16_t lane = c.lane;
    snd_t *s = S_(j);
    free(arg);
    pthread_mutex_lock(&j->mu);
    for (;;) {
        sframe_t *f = NULL;
        int rc, k;
        uint64_t w0, t1;
        w0 = now_us();
        while (!s->stop && !j->stopping && s->writer_up[lane] == 1 && s->gen[lane] == c.gen &&
               !(f = pick_locked(s, lane)))
            pthread_cond_wait(&j->cv, &j->mu);
        s->idle_us += now_us() - w0;
        if (!f) break;
        pthread_cond_broadcast(&j->cv); /* the reader may continue */
        pthread_mutex_unlock(&j->mu);
        /* The copying send keeps f->msg as plaintext, so a requeued frame can go out again
         * on another lane under that lane's keys. */
        t1 = now_us();
        rc = take_one(&ava1_send_test_fail_sends) ? AVA1_E_IO
                                                   : ava1_server_send(c.sid, lane, f->type, 0, f->seq, f->msg, f->len);
        pthread_mutex_lock(&j->mu);
        {
            uint64_t t2 = now_us();
            s->send_us += t2 - t1;
            if (rc == 0) {
                if (!s->t_first_send) s->t_first_send = t1;
                s->t_last_send = t2;
                s->sent_frames++;
                s->sent_bytes += f->len;
            }
        }
        k = settle_locked(s, f, rc);
        pthread_cond_broadcast(&j->cv); /* a requeued frame or returned credit: any lane may go */
        if (k < 0) {
            pthread_mutex_unlock(&j->mu);
            ava1_apply_fail(j, AVA1_ERR_INTERNAL, "a lane frame is too large to send", 0, 0);
            return NULL;
        }
        if (k == 0) break;
        if (rc != 0) {
            /* A send error on a live lane: either the connection broke (its lane-change
             * follows and ends this writer) or it was transient (no memory). Either way
             * the frame is requeued; back off briefly instead of spinning on it. */
            pthread_mutex_unlock(&j->mu);
            ava1_platform_sleep_ms(10);
            pthread_mutex_lock(&j->mu);
        }
    }
    pthread_mutex_unlock(&j->mu);
    return NULL;
}

/* A lane died (caller holds mu): its writer's generation ends, and its frames are
 * requeued for a re-send with their charge held (they may be on the wire). A frame the
 * writer is still sending is left to it: the writer settles it by its send's result. */
static void lane_down_locked(snd_t *s, uint16_t lane) {
    sframe_t **pp = &s->inflight;
    s->writer_up[lane] = 0;
    s->gen[lane]++;
    while (*pp) {
        sframe_t *f = *pp;
        if (f->lane != lane || f->sending) {
            pp = &f->next;
            continue;
        }
        *pp = f->next;
        requeue_front(s, f);
    }
}

/* A lane of session `sid` is up (caller holds mu). A writer still marked for another
 * session's lane of the same id (a re-attached job whose old lanes never reported their
 * death to it) is retired first. */
static void lane_up_locked(snd_t *s, uint16_t lane, const uint8_t sid[16]) {
    if (s->writer_up[lane] && memcmp(s->wsid[lane], sid, 16) == 0) return;
    if (s->writer_up[lane]) lane_down_locked(s, lane);
    memcpy(s->wsid[lane], sid, 16);
    s->writer_up[lane] = 2; /* "start me": the job thread starts the writer (on_tick) */
}

static void lane_change(ava1_job_t *j, uint16_t lane, int up) {
    snd_t *s = S_(j);
    if (lane == 0 || lane > AVA1_MAX_LANES) return;
    /* Called under the job table lock (so j->sid is stable): only mark state and signal;
     * never join here. */
    pthread_mutex_lock(&j->mu);
    if (!up) lane_down_locked(s, lane);
    else lane_up_locked(s, lane, j->sid);
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

static void emit_page_or_end(ava1_job_t *j) {
    snd_t *s = S_(j);
    uint8_t *out = malloc(64u * 1024u);
    size_t len;
    if (!out) return;
    while (s->next_page < j->m.n) {
        if (ava1_mstore_page(&j->m, j->id, &s->next_page, out, 64u * 1024u, &len) == 0)
            ava1_job_emit(j, AVA1_TYPE_MANIFEST_PAGE, 0, out, len);
        else break; /* transient (no memory): retried next tick, not a tight spin */
    }
    if (s->next_page >= j->m.n) { /* m.n == 0: no page, just the end */
        ava1_manifest_end_t e;
        ava1_w_t w;
        memset(&e, 0, sizeof e);
        memcpy(e.job_id, j->id, 16);
        e.files = j->m.files;
        e.bytes = j->m.bytes;
        ava1_mstore_hash(&j->m, e.manifest_hash);
        ava1_w_init(&w, out, 64u * 1024u);
        if (ava1_manifest_end_encode(&e, &w) == 0) ava1_job_emit(j, AVA1_TYPE_MANIFEST_END, 0, out, w.len);
        s->pages_done = 1;
    }
    free(out);
}

/* Starts the reader (once the map is in) and every writer marked "start me". The job
 * tick does it (25 ms granularity) and so does the arrival of the map, so the first data
 * frame does not wait for the next tick. `may_join` is the tick's: a lane id whose previous
 * writer is still to be joined is left to it (the connection thread never waits on a job).
 * Caller holds pump_mu. */
static void start_threads(ava1_job_t *j, int may_join) {
    snd_t *s = S_(j);
    uint16_t l;
    pthread_mutex_lock(&j->mu);
    if (s->have_map && !s->reader_started) {
        s->reader_started = ava1_thread_start(reader_main, j, &s->reader) == 0;
    }
    for (l = 1; l <= AVA1_MAX_LANES; l++) {
        wctx2_t *c;
        if (s->writer_up[l] != 2) continue;
        if (s->writer_started[l]) { /* the previous writer of this lane id has exited or is exiting */
            if (!may_join) continue;
            pthread_mutex_unlock(&j->mu);
            pthread_join(s->writer[l], NULL);
            pthread_mutex_lock(&j->mu);
            s->writer_started[l] = 0;
            if (s->writer_up[l] != 2) continue; /* the lane died while we joined */
        }
        /* A writer that cannot start leaves the lane at 2: the next tick tries again. */
        if (!(c = malloc(sizeof *c))) continue;
        c->j = j;
        c->lane = l;
        c->gen = s->gen[l];
        memcpy(c->sid, s->wsid[l], 16);
        if (!take_one(&ava1_send_test_fail_writer_starts) && ava1_thread_start(writer_main, c, &s->writer[l]) == 0) {
            s->writer_started[l] = 1;
            s->writer_up[l] = 1;
        } else {
            free(c);
        }
    }
    pthread_mutex_unlock(&j->mu);
}

static int attached_now(ava1_job_t *j) {
    int attached;
    pthread_mutex_lock(&j->cmu);
    attached = j->attached;
    pthread_mutex_unlock(&j->cmu);
    return attached;
}

static void on_tick(ava1_job_t *j) {
    snd_t *s = S_(j);
    /* C2: nothing may be emitted before the data layer attaches the job — net_emit would
     * drop the pages while next_page advanced, and the manifest would be lost. */
    if (!attached_now(j)) return;
    pthread_mutex_lock(&s->pump_mu);
    if (!s->pages_done) emit_page_or_end(j);
    start_threads(j, 1);
    pthread_mutex_unlock(&s->pump_mu);
}

/* The receiver's map just arrived: start the reader and the writers now rather than at the
 * next tick (up to 25 ms of the job's critical path). Best effort: if the tick holds
 * pump_mu it starts them itself. */
static void kick(ava1_job_t *j) {
    snd_t *s = S_(j);
    if (!attached_now(j)) return;
    if (pthread_mutex_trylock(&s->pump_mu) != 0) return;
    start_threads(j, 0);
    pthread_mutex_unlock(&s->pump_mu);
}

/* A Received (caller holds mu): the frame to free, or NULL. A frame its writer is still
 * sending is only marked (the writer frees it); a seq not in flight (a frame requeued
 * when its lane died, or a duplicate) changes nothing — its charge was held all along and
 * comes back with the receiver's Credit. */
static sframe_t *received_locked(ava1_job_t *j, snd_t *s, uint32_t seq) {
    sframe_t *f;
    for (f = s->inflight; f; f = f->next)
        if (f->seq == seq) break;
    if (!f) return NULL;
    unlink_inflight(s, f);
    j->bytes_received += f->len;
    if (f->sending) {
        f->acked = 1;
        return NULL;
    }
    return f;
}

/* More window from the receiver (caller holds mu). Credit frames are incremental (R2);
 * the bytes are peer-controlled, so the add saturates instead of wrapping. */
static void credit_locked(snd_t *s, uint64_t n) { s->credit = sat_add(s->credit, n); }

static int on_frame(ava1_job_t *j, uint8_t type, const uint8_t *body, size_t len) {
    snd_t *s = S_(j);
    switch (type) {
    case AVA1_TYPE_JOB_MAP: {
        ava1_job_map_t m;
        ava1_r_t it;
        ava1_file_run_t r;
        ava1_file_range_t g;
        if (ava1_job_map_decode(body, len, &m) != 0) return 1;
        if (m.status != AVA1_STATUS_OK) {
            /* A reader-thread hook never ends a job itself (C4): the job thread does. */
            ava1_data_fail_soon(j, m.status, "the receiver refused the job");
            return 0;
        }
        pthread_mutex_lock(&j->mu);
        ava1_r_init(&it, m.done, m.done_len);
        while (ava1_file_run_next(&it, &r) == 1) {
            uint32_t f;
            for (f = r.first; f < r.first + r.count && f < j->m.n; f++) ava1_bits_set(&s->skip, f);
        }
        ava1_r_init(&it, m.partial, m.partial_len);
        while (ava1_file_range_next(&it, &g) == 1) {
            if (g.file_id >= j->m.n) continue;
            if (!s->durable[g.file_id]) s->durable[g.file_id] = calloc(1, sizeof(ava1_rset_t));
            if (s->durable[g.file_id]) (void)ava1_rset_add(s->durable[g.file_id], g.offset, g.offset + g.len);
        }
        if (m.last) s->have_map = 1;
        pthread_mutex_unlock(&j->mu);
        if (m.last) kick(j);
        return 0;
    }
    case AVA1_TYPE_RECEIVED: {
        ava1_received_t r;
        sframe_t *f;
        if (ava1_received_decode(body, len, &r) != 0) return 1;
        pthread_mutex_lock(&j->mu);
        f = received_locked(j, s, r.seq);
        pthread_mutex_unlock(&j->mu);
        if (f) {
            free(f->msg);
            free(f);
        }
        return 0;
    }
    case AVA1_TYPE_CREDIT: {
        ava1_credit_t c;
        if (ava1_credit_decode(body, len, &c) != 0) return 1;
        pthread_mutex_lock(&j->mu);
        credit_locked(s, c.bytes);
        pthread_cond_broadcast(&j->cv);
        pthread_mutex_unlock(&j->mu);
        return 0;
    }
    case AVA1_TYPE_FILE_RETRY: {
        ava1_file_retry_t r;
        uint32_t *a;
        if (ava1_file_retry_decode(body, len, &r) != 0 || r.file_id >= j->m.n) return 1;
        pthread_mutex_lock(&j->mu);
        a = realloc(s->retry, (s->retry_n + 1) * sizeof *a);
        if (a) {
            s->retry = a;
            s->retry[s->retry_n++] = r.file_id;
        }
        pthread_cond_broadcast(&j->cv);
        pthread_mutex_unlock(&j->mu);
        return 0;
    }
    case AVA1_TYPE_JOB_OPEN_ACK: {
        /* Sending to another console (SPEC.md §18): we opened the job there, and its answer
         * carries the window. A download never gets one (it sends its own). */
        ava1_job_open_ack_t a;
        if (ava1_job_open_ack_decode(body, len, &a) != 0) return 1;
        if (a.status != AVA1_STATUS_OK) {
            char why[sizeof j->message];
            snprintf(why, sizeof why, "the other console refused: %.*s", a.has_message ? (int)a.message_len : 0,
                     a.has_message ? (const char *)a.message : "");
            ava1_data_fail_soon(j, a.status, why);
            return 0;
        }
        pthread_mutex_lock(&j->mu);
        credit_locked(s, a.credit);
        pthread_cond_broadcast(&j->cv);
        pthread_mutex_unlock(&j->mu);
        return 0;
    }
    case AVA1_TYPE_JOB_DONE:
    case AVA1_TYPE_JOB_CANCEL:
        /* The peer concluded the job: stop, never answer with a JobDone of our own (C10). */
        ava1_log_job_event(type == AVA1_TYPE_JOB_DONE ? "peer-done" : "peer-cancel", j, AVA1_STATUS_OK);
        pthread_mutex_lock(&j->mu);
        if (!j->finished) { /* how it ended there is how it ended: a status reader asks us */
            ava1_job_done_t d;
            if (type == AVA1_TYPE_JOB_DONE && ava1_job_done_decode(body, len, &d) == 0) {
                j->final_status = d.status;
                j->files_done = d.files;
                j->bytes_durable = d.bytes;
                if (d.status != AVA1_STATUS_OK)
                    snprintf(j->message, sizeof j->message, "%.*s", d.has_message ? (int)d.message_len : 0,
                             d.has_message ? (const char *)d.message : "");
            } else if (type == AVA1_TYPE_JOB_CANCEL) {
                j->final_status = AVA1_ERR_CANCELLED;
                snprintf(j->message, sizeof j->message, "cancelled by the receiver");
            }
            __atomic_store_n(&j->parked_at_ms, ava1_mono_ms(), __ATOMIC_RELEASE);
        }
        s->stop = 1;
        j->finished = 1;
        pthread_cond_broadcast(&j->cv);
        pthread_mutex_unlock(&j->mu);
        return 0;
    default:
        return 0; /* Durable, Status: progress the engine shows; nothing to do here */
    }
}

/* The stage-timer line is opt-in (review L2), like the Rust side's PS5UPLOAD_AVA1_TIMING: the
 * console has no environment to set, so a flag file in the debug directory turns it on (the host
 * build, which runs this code in ava1-ctest, also honours the environment variable). Read once per
 * job. */
#ifndef AVA1_TIMING_FLAG
#define AVA1_TIMING_FLAG "/data/ps5upload/debug/ava1-timing"
#endif
int ava1_send_timing_enabled(void) {
    return getenv("PS5UPLOAD_AVA1_TIMING") != NULL || access(AVA1_TIMING_FLAG, F_OK) == 0;
}

static void role_free(ava1_job_t *j) {
    snd_t *s = S_(j);
    sframe_t *f;
    uint32_t i;
    if (!s) return;
    pthread_mutex_lock(&j->mu);
    s->stop = 1;
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    if (s->reader_started) pthread_join(s->reader, NULL);
    for (i = 1; i <= AVA1_MAX_LANES; i++)
        if (s->writer_started[i]) pthread_join(s->writer[i], NULL); /* each exits on stop */
    if (s->sent_frames && ava1_send_timing_enabled()) {
        uint64_t t0 = s->t_open;
        fprintf(stderr,
                "ava1 send: walk=%llums first_send=+%llums last_send=+%llums reader_end=+%llums files=%u "
                "frames=%llu bytes=%llu reader_wait=%llums send=%llums writer_idle=%llums\n",
                (unsigned long long)(s->walk_us / 1000), (unsigned long long)((s->t_first_send - t0) / 1000),
                (unsigned long long)((s->t_last_send - t0) / 1000),
                (unsigned long long)(s->reader_end ? (s->reader_end - t0) / 1000 : 0), j->m.n,
                (unsigned long long)s->sent_frames, (unsigned long long)s->sent_bytes,
                (unsigned long long)(s->reader_wait_us / 1000), (unsigned long long)(s->send_us / 1000),
                (unsigned long long)(s->idle_us / 1000));
    }
    while ((f = s->ready) != NULL) {
        s->ready = f->next;
        free(f->msg);
        free(f);
    }
    while ((f = s->inflight) != NULL) {
        s->inflight = f->next;
        free(f->msg);
        free(f);
    }
    for (i = 0; s->durable && i < j->m.n; i++)
        if (s->durable[i]) {
            ava1_rset_clear(s->durable[i]);
            free(s->durable[i]);
        }
    free(s->durable);
    free(s->retry);
    ava1_bits_free(&s->skip);
    pthread_mutex_destroy(&s->pump_mu);
    free(s);
    j->role = NULL;
}

ava1_job_t *ava1_send_open(const ava1_job_open_t *o, const uint8_t peer[32], ava1_job_open_ack_t *ack, char *msg,
                           size_t cap) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_job_t *j;
    snd_t *s;
    struct stat st;
    char root[AVA1_MAX_PATH + 1];
    int rc;
    memset(ack, 0, sizeof *ack);
    memcpy(ack->job_id, o->job_id, 16);
    msg[0] = 0;
    memcpy(root, o->root, o->root_len);
    root[o->root_len] = 0;
    if (!cfg->may_read || !cfg->may_read(root, (o->flags & AVA1_JF_UNSAFE_READ) != 0)) {
        ack->status = AVA1_ERR_PATH;
        snprintf(msg, cap, "reading %s is not allowed", root);
        return NULL;
    }
    if (stat(root, &st) != 0) {
        ack->status = AVA1_ERR_PATH;
        snprintf(msg, cap, "%s: %s", root, strerror(errno));
        return NULL;
    }
    ava1_job_free_one(o->job_id); /* a sender's state lives only in memory: start over */
    j = ava1_job_create(o->job_id, peer);
    s = j ? calloc(1, sizeof *s) : NULL;
    if (s) pthread_mutex_init(&s->pump_mu, NULL);
    if (!j || !s) {
        if (j) {
            ava1_job_free_one(o->job_id);
            ava1_job_put(j);
        }
        if (s) pthread_mutex_destroy(&s->pump_mu);
        free(s);
        ack->status = AVA1_ERR_BUSY;
        return NULL;
    }
    j->kind = AVA1_JOB_DOWNLOAD;
    j->flags = o->flags;
    snprintf(j->src, sizeof j->src, "%s", root);
    j->role = s;
    j->role_free = role_free;
    s->single = !S_ISDIR(st.st_mode);
    s->t_open = now_us();
    rc = s->single ? ava1_mstore_single(&j->m, root) : ava1_mstore_walk_deny(&j->m, root, AVA1_WALK_FOLLOW, cfg->refuse_link);
    s->walk_us = now_us() - s->t_open;
    s->credit = o->has_credit ? o->credit : (16u << 20);
    if (rc != 0 || ava1_bits_init(&s->skip, j->m.n) != 0 ||
        !(s->durable = calloc((size_t)j->m.n + 1, sizeof *s->durable)) ||
        !(j->lf = calloc((size_t)j->m.n + 1, sizeof *j->lf)) || ava1_bits_init(&j->done, j->m.n) != 0) {
        ack->status = rc == AVA1_E_BADPATH ? AVA1_ERR_PATH : AVA1_ERR_IO;
        snprintf(msg, cap, "cannot read %s", root);
        ava1_job_free_one(o->job_id);
        ava1_job_put(j);
        return NULL;
    }
    j->on_frame = on_frame;
    j->on_lane_change = lane_change;
    j->on_tick = on_tick;
    if (ava1_apply_start(j) != 0) {
        ack->status = AVA1_ERR_INTERNAL;
        ava1_job_free_one(o->job_id);
        ava1_job_put(j);
        return NULL;
    }
    ack->status = AVA1_STATUS_OK;
    return j;
}

void ava1_send_start(ava1_job_t *j) {
    snd_t *s = S_(j);
    uint16_t lanes[AVA1_MAX_LANES];
    uint8_t sid[16];
    int n, i, attached;
    uint16_t l;
    pthread_mutex_lock(&j->cmu);
    attached = j->attached;
    memcpy(sid, j->sid, 16);
    pthread_mutex_unlock(&j->cmu);
    if (!attached) return;
    /* Lanes already up when the job was (re-)attached never produced a lane-change event
     * for it. */
    n = ava1_server_lanes(sid, lanes);
    pthread_mutex_lock(&j->mu);
    /* A re-attach (a wire Resume): writers of an earlier session's lanes are retired —
     * their frames requeued with their charge held — whether or not that session's lane
     * deaths ever reached the job (it was parked first). */
    for (l = 1; l <= AVA1_MAX_LANES; l++)
        if (s->writer_up[l] && memcmp(s->wsid[l], sid, 16) != 0) lane_down_locked(s, l);
    for (i = 0; i < n; i++) lane_up_locked(s, lanes[i], sid);
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
}

/* ---- tests only: the window and the queues, driven step by step ------------------- */

#define TEST_TAKEN 64
static ava1_job_t *g_tj;
static sframe_t *g_taken[TEST_TAKEN];

void ava1_send_test_begin(uint64_t credit) {
    snd_t *s = calloc(1, sizeof *s);
    if (s) pthread_mutex_init(&s->pump_mu, NULL);
    g_tj = calloc(1, sizeof *g_tj);
    memset(g_taken, 0, sizeof g_taken);
    pthread_mutex_init(&g_tj->mu, NULL);
    pthread_cond_init(&g_tj->cv, NULL);
    s->credit = credit;
    g_tj->role = s;
}

void ava1_send_test_end(void) {
    snd_t *s = S_(g_tj);
    sframe_t *f;
    uint32_t i;
    for (i = 0; i < TEST_TAKEN; i++) /* a frame between pick and settle is on neither list */
        if (g_taken[i]) {
            sframe_t *p;
            int listed = 0;
            for (p = s->ready; p; p = p->next) listed |= p == g_taken[i];
            for (p = s->inflight; p; p = p->next) listed |= p == g_taken[i];
            if (!listed) {
                free(g_taken[i]->msg);
                free(g_taken[i]);
            }
        }
    while ((f = s->ready) != NULL) {
        s->ready = f->next;
        free(f->msg);
        free(f);
    }
    while ((f = s->inflight) != NULL) {
        s->inflight = f->next;
        free(f->msg);
        free(f);
    }
    pthread_mutex_destroy(&s->pump_mu);
    free(s);
    pthread_cond_destroy(&g_tj->cv);
    pthread_mutex_destroy(&g_tj->mu);
    free(g_tj);
    g_tj = NULL;
}

/* The reader's put of one `len`-byte frame: put_frame's return. */
int ava1_send_test_put(uint64_t len) {
    uint8_t *m = malloc(len ? (size_t)len : 1);
    if (!m) return -ENOMEM;
    return put_frame(g_tj, AVA1_TYPE_BUNDLE, m, (size_t)len);
}

/* The lane's writer picks its next frame: the frame's seq, 0 when it takes none. */
uint32_t ava1_send_test_take(uint16_t lane) {
    snd_t *s = S_(g_tj);
    sframe_t *f = NULL;
    uint32_t i;
    pthread_mutex_lock(&g_tj->mu);
    if (s->writer_up[lane] == 1) f = pick_locked(s, lane);
    pthread_mutex_unlock(&g_tj->mu);
    if (!f) return 0;
    for (i = 0; i < TEST_TAKEN; i++)
        if (!g_taken[i]) {
            g_taken[i] = f;
            break;
        }
    return f->seq;
}

/* The writer's send of frame `seq` returned `rc`: settle_locked's answer. */
int ava1_send_test_settle(uint32_t seq, int rc) {
    snd_t *s = S_(g_tj);
    uint32_t i;
    int k = -2;
    for (i = 0; i < TEST_TAKEN; i++)
        if (g_taken[i] && g_taken[i]->seq == seq) {
            sframe_t *f = g_taken[i];
            g_taken[i] = NULL;
            pthread_mutex_lock(&g_tj->mu);
            k = settle_locked(s, f, rc);
            pthread_mutex_unlock(&g_tj->mu);
            break;
        }
    return k;
}

/* A lane event; an up lane's writer counts as started (what on_tick does). */
void ava1_send_test_lane(uint16_t lane, int up) {
    snd_t *s = S_(g_tj);
    lane_change(g_tj, lane, up);
    pthread_mutex_lock(&g_tj->mu);
    if (up && s->writer_up[lane] == 2) s->writer_up[lane] = 1;
    pthread_mutex_unlock(&g_tj->mu);
}

void ava1_send_test_received(uint32_t seq) {
    sframe_t *f;
    uint32_t i;
    pthread_mutex_lock(&g_tj->mu);
    f = received_locked(g_tj, S_(g_tj), seq);
    pthread_mutex_unlock(&g_tj->mu);
    if (f) {
        for (i = 0; i < TEST_TAKEN; i++)
            if (g_taken[i] == f) g_taken[i] = NULL;
        free(f->msg);
        free(f);
    }
}

void ava1_send_test_credit(uint64_t n) {
    pthread_mutex_lock(&g_tj->mu);
    credit_locked(S_(g_tj), n);
    pthread_mutex_unlock(&g_tj->mu);
}

void ava1_send_test_stopping(void) {
    pthread_mutex_lock(&g_tj->mu);
    g_tj->stopping = 1;
    pthread_cond_broadcast(&g_tj->cv);
    pthread_mutex_unlock(&g_tj->mu);
}

/* out: credit, ready frames, queued bytes, in-flight frames. */
void ava1_send_test_state(uint64_t out[4]) {
    snd_t *s = S_(g_tj);
    sframe_t *f;
    pthread_mutex_lock(&g_tj->mu);
    out[0] = s->credit;
    out[1] = 0;
    for (f = s->ready; f; f = f->next) out[1]++;
    out[2] = s->queued;
    out[3] = 0;
    for (f = s->inflight; f; f = f->next) out[3]++;
    pthread_mutex_unlock(&g_tj->mu);
}
