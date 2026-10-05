#include "ava1_recv.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_apply.h"
#include "ava1_b3.h"
#include "ava1_data.h"
#include "ava1_internal.h"
#include "blake3.h"

#define MAP_ITEMS 2000u /* items per JobMap page: 2,000 ranges of 24 bytes fit a control frame */
#define CREDIT_WANT (64ull << 20)
#define CREDIT_MIN (8ull << 20)

/* ---- replay ------------------------------------------------------------------------ */

typedef struct {
    ava1_job_t *j;
    int opened;
    /* durable-by-log: done files not yet swept, and the pack ranges that hold them (SPEC.md §15.7) */
    ava1_bits_t unswept;
    ava1_pack_ref_t *refs;
    uint32_t nrefs, cap_refs;
} replay_t;

static void replay_free(replay_t *r) {
    ava1_bits_free(&r->unswept);
    free(r->refs);
    r->refs = NULL;
    r->nrefs = r->cap_refs = 0;
}

static void ref_push(replay_t *r, const ava1_pack_ref_t *p) {
    if (r->nrefs == r->cap_refs) {
        uint32_t c = r->cap_refs ? r->cap_refs * 2 : 16;
        ava1_pack_ref_t *q = realloc(r->refs, (size_t)c * sizeof *q);
        if (!q) return; /* a lost ref only fails its files over to a resend */
        r->refs = q;
        r->cap_refs = c;
    }
    r->refs[r->nrefs++] = *p;
}

/* Refs with no unswept file left in their span are dropped. */
static void refs_prune(replay_t *r) {
    uint32_t i, k = 0;
    for (i = 0; i < r->nrefs; i++) {
        uint64_t f;
        int live = 0;
        for (f = r->refs[i].first_file; f < (uint64_t)r->refs[i].first_file + r->refs[i].count && !live; f++)
            live = f < r->j->m.n && ava1_bits_get(&r->unswept, (uint32_t)f);
        if (live) r->refs[k++] = r->refs[i];
    }
    r->nrefs = k;
}

/* Marks the files of FileRun runs unswept (set != 0) or swept. */
static void unswept_runs(replay_t *r, const uint8_t *p, uint32_t len, int set) {
    ava1_r_t it;
    ava1_file_run_t run;
    ava1_r_init(&it, p, len);
    while (ava1_file_run_next(&it, &run) == 1) {
        uint64_t f;
        for (f = run.first; f < (uint64_t)run.first + run.count && f < r->j->m.n; f++) {
            if (set) ava1_bits_set(&r->unswept, (uint32_t)f);
            else ava1_bits_clear(&r->unswept, (uint32_t)f);
        }
    }
}

static void remember(ava1_job_t *j, const ava1_file_range_t *g) {
    ava1_file_range_t *a = realloc(j->last_ranges, (j->last_ranges_n + 1) * sizeof *a);
    if (!a) return; /* the check is a safety net; the commit's root check still guards */
    j->last_ranges = a;
    j->last_ranges[j->last_ranges_n++] = *g;
}

static void forget_ranges(ava1_job_t *j) {
    free(j->last_ranges);
    j->last_ranges = NULL;
    j->last_ranges_n = 0;
}

/* how: 0 adds only; 1 also remembers each range (a batch's); 2 remembers each range's last
 * group (a snapshot's: the batch that wrote them is gone, its tail is what a crash tears). */
static void add_ranges(ava1_job_t *j, const uint8_t *p, uint32_t len, int how) {
    ava1_r_t it;
    ava1_file_range_t g;
    ava1_r_init(&it, p, len);
    while (ava1_file_range_next(&it, &g) == 1) {
        ava1_lfile_t *lf;
        if (g.file_id >= j->m.n || j->m.e[g.file_id].kind != AVA1_ENTRY_FILE || g.len == 0 ||
            !(lf = ava1_lfile_get(j, g.file_id)))
            continue;
        (void)ava1_rset_add(&lf->durable, g.offset, g.offset + g.len);
        if (how == 2) {
            uint64_t end = g.offset + g.len, tail = (end - 1) / AVA1_GROUP_LEN * AVA1_GROUP_LEN;
            if (tail > g.offset) {
                g.len = end - tail;
                g.offset = tail;
            }
        }
        if (how) remember(j, &g);
    }
}

static void add_done(ava1_job_t *j, const uint8_t *p, uint32_t len) {
    ava1_r_t it;
    ava1_file_run_t r;
    ava1_r_init(&it, p, len);
    while (ava1_file_run_next(&it, &r) == 1) {
        uint64_t f;
        for (f = r.first; f < (uint64_t)r.first + r.count && f < j->m.n; f++) {
            if (j->m.e[f].kind != AVA1_ENTRY_FILE) continue;
            ava1_bits_set(&j->done, (uint32_t)f);
            if (j->lf[f]) ava1_rset_clear(&j->lf[f]->durable); /* a done file drops its ranges */
        }
    }
}

static void add_roots(ava1_job_t *j, const uint8_t *p, uint32_t len) {
    ava1_r_t it;
    ava1_root_item_t r;
    ava1_r_init(&it, p, len);
    while (ava1_root_item_next(&it, &r) == 1) {
        ava1_lfile_t *lf;
        if (r.file_id >= j->m.n || j->m.e[r.file_id].kind != AVA1_ENTRY_FILE || !(lf = ava1_lfile_get(j, r.file_id)))
            continue;
        memcpy(lf->root, r.root, 32);
        lf->has_root = lf->root_journaled = 1;
    }
}

static void forget_file(ava1_job_t *j, uint32_t i) {
    ava1_bits_clear(&j->done, i);
    if (j->lf[i]) {
        ava1_rset_clear(&j->lf[i]->durable);
        j->lf[i]->has_root = j->lf[i]->root_journaled = 0;
    }
}

/* The journal of another job, another root or another kind of job is not ours: refusing
 * its Open leaves nothing replayed, and the job starts afresh. */
static int replay(void *ctx, uint8_t kind, const uint8_t *body, size_t len) {
    replay_t *r = ctx;
    ava1_job_t *j = r->j;
    if (!r->opened && kind != AVA1_JNL_OPEN) return 1;
    switch (kind) {
    case AVA1_JNL_OPEN: {
        ava1_jnl_open_t o;
        if (r->opened || ava1_jnl_open_decode(body, len, &o) != 0 || memcmp(o.job_id, j->id, 16) != 0 ||
            o.kind != j->kind || o.flags != j->flags || o.root_len != strlen(j->root) ||
            memcmp(o.root, j->root, o.root_len) != 0)
            return 1;
        memcpy(j->manifest_hash, o.manifest_hash, 32);
        j->staged = o.staged & 1;
        j->dest_held = (o.staged & AVA1_STAGED_HELD) != 0;
        r->opened = 1;
        return 0;
    }
    case AVA1_JNL_BATCH: {
        ava1_jnl_batch_t b;
        if (ava1_jnl_batch_decode(body, len, &b) != 0) return 1;
        if (b.ranges_count) forget_ranges(j); /* only the last batch that wrote ranges is checked */
        add_ranges(j, b.ranges, b.ranges_len, 1);
        add_roots(j, b.roots, b.roots_len);
        add_done(j, b.files, b.files_len);
        if (b.has_pack_segment && b.has_pack_offset && b.has_pack_len) {
            /* a logged batch: its files are durable through the log until a JnlSweep says otherwise */
            ava1_pack_ref_t ref;
            ava1_r_t it;
            ava1_file_run_t run;
            uint32_t lo = UINT32_MAX, hi = 0;
            unswept_runs(r, b.files, b.files_len, 1);
            ava1_r_init(&it, b.files, b.files_len);
            while (ava1_file_run_next(&it, &run) == 1) {
                if (run.first < lo) lo = run.first;
                if (run.count && run.first + run.count - 1 > hi) hi = run.first + run.count - 1;
            }
            if (lo != UINT32_MAX) {
                ref.segment = b.pack_segment;
                ref.offset = b.pack_offset;
                ref.len = b.pack_len;
                ref.first_file = lo;
                ref.count = hi - lo + 1;
                ref_push(r, &ref);
            }
        }
        return 0;
    }
    case AVA1_JNL_RESET: {
        ava1_jnl_reset_t x;
        if (ava1_jnl_reset_decode(body, len, &x) != 0) return 1;
        if (x.file_id < j->m.n) {
            forget_file(j, x.file_id);
            ava1_bits_clear(&r->unswept, x.file_id);
        }
        return 0;
    }
    case AVA1_JNL_SWEEP: {
        ava1_jnl_sweep_t x;
        if (ava1_jnl_sweep_decode(body, len, &x) != 0) return 1;
        unswept_runs(r, x.files, x.files_len, 0);
        return 0;
    }
    case AVA1_JNL_SNAPSHOT: {
        ava1_jnl_snapshot_t s;
        uint32_t i;
        if (ava1_jnl_snapshot_decode(body, len, &s) != 0) return 1;
        /* Fail closed: unswept/segments streams that do not parse in full end the replay (a torn record),
         * never read as "everything is swept". */
        {
            uint32_t cnt;
            if ((s.has_unswept && ava1_file_run_count(s.unswept, s.unswept_len, &cnt) != 0) ||
                (s.has_segments && ava1_pack_ref_count(s.segments, s.segments_len, &cnt) != 0))
                return 1;
        }
        for (i = 0; i < j->m.n; i++) forget_file(j, i);
        forget_ranges(j);
        add_ranges(j, s.ranges, s.ranges_len, 2);
        add_roots(j, s.roots, s.roots_len);
        add_done(j, s.done, s.done_len);
        for (i = 0; i < j->m.n; i++) ava1_bits_clear(&r->unswept, i);
        r->nrefs = 0;
        if (s.has_unswept) unswept_runs(r, s.unswept, s.unswept_len, 1);
        if (s.has_segments) {
            ava1_r_t it;
            ava1_pack_ref_t ref;
            ava1_r_init(&it, s.segments, s.segments_len);
            while (ava1_pack_ref_next(&it, &ref) == 1) ref_push(r, &ref);
        }
        return 0;
    }
    case AVA1_JNL_DONE: {
        ava1_jnl_done_t d;
        if (ava1_jnl_done_decode(body, len, &d) != 0) return 1;
        j->replay_done = 1;
        j->replay_status = d.status;
        return 0;
    }
    default:
        return 1;
    }
}

/* files_done and bytes_durable from the state. Caller holds j->mu or owns the job alone. */
static void recount(ava1_job_t *j) {
    uint32_t i;
    j->files_done = 0;
    j->bytes_durable = 0;
    for (i = 0; i < j->m.n; i++) {
        if (j->m.e[i].kind != AVA1_ENTRY_FILE) continue;
        if (ava1_bits_get(&j->done, i)) {
            j->files_done++;
            j->bytes_durable += j->m.e[i].size;
        } else if (j->lf[i]) {
            j->bytes_durable += ava1_rset_covered(&j->lf[i]->durable);
        }
    }
}

static int alloc_state(ava1_job_t *j) {
    j->lf = calloc((size_t)j->m.n + 1, sizeof *j->lf);
    ava1_lflist_reset(j, 0);
    return (j->lf && ava1_bits_init(&j->done, j->m.n) == 0) ? 0 : -1;
}

static void free_lf(ava1_job_t *j, uint32_t id, ava1_lfile_t *lf) {
    if (!lf) return;
    ava1_lf_close_fds(j, id, lf); /* the descriptors and their slots in the large-file budget */
    ava1_rset_clear(&lf->written);
    ava1_rset_clear(&lf->durable);
    free(lf);
}

/* Back to "nothing known": the job starts afresh. */
static void drop_state(ava1_job_t *j) {
    uint32_t i;
    ava1_jnl_close(&j->jnl);
    if (j->lf)
        for (i = 0; i < j->m.n; i++) free_lf(j, i, j->lf[i]);
    free(j->lf);
    j->lf = NULL;
    ava1_lflist_reset(j, 0);
    ava1_bits_free(&j->done);
    memset(&j->done, 0, sizeof j->done);
    ava1_mstore_free(&j->m);
    forget_ranges(j);
    j->have_manifest = j->replay_done = j->staged = j->dest_held = 0;
}

/* Loads <dir>/manifest and replays <dir>/journal. 0 when a usable earlier job was found:
 * the journal opens for this job and root, and was written for exactly this manifest. */
static int load_from_disk(ava1_job_t *j) {
    uint8_t *blob, hash[32];
    size_t len;
    int rc;
    replay_t r;
    memset(&r, 0, sizeof r);
    r.j = j;
    if (ava1_manifest_file_read(j->dir, &blob, &len) != 0) return -1;
    rc = ava1_mstore_from_blob(&j->m, blob, len);
    free(blob);
    if (rc != 0 || alloc_state(j) != 0 || ava1_bits_init(&r.unswept, j->m.n) != 0) {
        replay_free(&r);
        return -1;
    }
    if (ava1_jnl_open(&j->jnl, j->dir, replay, &r) != 0 || !r.opened) {
        replay_free(&r);
        return -1;
    }
    ava1_mstore_hash(&j->m, hash);
    if (memcmp(hash, j->manifest_hash, 32) != 0) { /* a crash between the two writes */
        replay_free(&r);
        return -1;
    }
    j->have_manifest = 1;
    /* Durable-by-log recovery (§4): the files the journal calls done but not yet swept are made again from
     * the pack log where a crash lost them, and swept, before anything is counted or answered. */
    refs_prune(&r);
    snprintf(j->base, sizeof j->base, "%s%s", j->root, j->staged ? ".ava-part" : "");
    if (ava1_pack_recover(j, &r.unswept, r.refs, r.nrefs) != 0) {
        replay_free(&r);
        return -1;
    }
    replay_free(&r);
    recount(j);
    /* Decided once, here, from the replayed journal: a job that already holds done or partial files is a
     * resume, and its sender may hash or skip them for a long time without a frame (review 006). */
    j->resumed = j->files_done > 0 || j->bytes_durable > 0;
    return 0;
}

/* The owner key recovery's throwaway jobs carry: all zero, which no peer's public key is. */
static int owner_is_recovery(const uint8_t owner[32]) {
    static const uint8_t zero[32];
    return memcmp(owner, zero, 32) == 0;
}

static int name_cmp(const void *a, const void *b) { return strcmp((const char *)a, (const char *)b); }

/* One recovery pass (SPEC.md §15.7): job directories under `jobs_dir` that hold a pack log and nobody has
 * open. Each is loaded into a throwaway job exactly as a JobOpen would load it (load_from_disk runs the
 * recovery) and freed again; a directory whose job is listed (create refuses its id) is skipped. Bounded so
 * a start never replays a whole disk of journals: it stops after `max` directories that settled (their log is
 * gone) or 2*max that were tried, and the next pass starts after the last one tried, so a directory that
 * cannot be recovered never starves the ones behind it. While a throwaway is listed a JobOpen for its id is
 * answered BUSY. Returns how many settled. */
uint32_t ava1_recv_recover_pass(const char *jobs_dir, uint32_t max) {
    static pthread_mutex_t pass_mu = PTHREAD_MUTEX_INITIALIZER;
    static uint32_t cursor;
    enum { MAX_CAND = 512 };
    static const uint8_t zero_owner[32];
    char (*names)[33] = NULL;
    uint32_t n = 0, settled = 0, tried = 0, start, k;
    DIR *dp;
    struct dirent *de;
    if (pthread_mutex_trylock(&pass_mu) != 0) return 0; /* another pass is running */
    if (!(names = malloc(MAX_CAND * sizeof *names))) {
        pthread_mutex_unlock(&pass_mu);
        return 0;
    }
    dp = opendir(jobs_dir);
    while (dp && n < MAX_CAND && (de = readdir(dp)) != NULL) {
        char dir[sizeof ((ava1_job_t *)0)->dir];
        uint8_t id[16];
        size_t i;
        if (strlen(de->d_name) != 32) continue;
        for (i = 0; i < 16; i++) {
            unsigned v;
            if (sscanf(de->d_name + 2 * i, "%2x", &v) != 1) break;
            id[i] = (uint8_t)v;
        }
        if (i != 16) continue;
        ava1_job_dir(jobs_dir, id, dir, sizeof dir);
        if (ava1_dir_has_pack(dir)) memcpy(names[n++], de->d_name, 33);
    }
    if (dp) closedir(dp);
    qsort(names, n, sizeof *names, name_cmp);
    start = n ? cursor % n : 0;
    for (k = 0; k < n && settled < max && tried < 2u * max && ava1_data_running(); k++) {
        uint32_t at = (start + k) % n;
        uint8_t id[16], buf[AVA1_MAX_PATH + 256];
        char dir[sizeof ((ava1_job_t *)0)->dir];
        ava1_jnl_open_t o;
        ava1_job_t *j;
        size_t i;
        for (i = 0; i < 16; i++) {
            unsigned v;
            (void)sscanf(names[at] + 2 * i, "%2x", &v);
            id[i] = (uint8_t)v;
        }
        ava1_job_dir(jobs_dir, id, dir, sizeof dir);
        cursor = at + 1; /* the next pass starts behind this one, whatever came of it */
        tried++;
        if (ava1_jnl_peek_open(dir, buf, sizeof buf, &o) != 0 || o.kind != AVA1_JOB_UPLOAD || o.root_len >= AVA1_MAX_PATH) {
            fprintf(stderr, "[ava1] recovery: %s holds a log but its journal cannot be opened; left for the GC ceiling\n", names[at]);
            continue;
        }
        if (!(j = ava1_job_create(id, zero_owner))) continue; /* listed: a real session owns it */
        j->kind = o.kind;
        j->flags = o.flags;
        memcpy(j->root, o.root, o.root_len);
        j->root[o.root_len] = 0;
        snprintf(j->dir, sizeof j->dir, "%s", dir);
        j->ub_excluded = 1; /* a throwaway's log bytes are no other job's business (the cross-job cap) */
        (void)load_from_disk(j); /* recovers; a job that is not ours or has nothing unswept changes nothing */
        ava1_job_free_one(j->id);
        ava1_job_put(j);
        if (!ava1_dir_has_pack(dir)) settled++;
        else fprintf(stderr, "[ava1] recovery: %s still holds its log; tried again later\n", names[at]);
    }
    free(names);
    pthread_mutex_unlock(&pass_mu);
    return settled;
}

/* ---- messages ---------------------------------------------------------------------- */

/* Bytes the drive already holds for this job's unfinished large files (the JobMap `held`
 * extension, design 015/02): the allocated blocks of the part file of every file the receiver
 * tracks, at most that file's size. A sender checking free space credits them: a part file is
 * preallocated whole, so its undurable tail is on the drive already. Only what the file system
 * reports is counted, and only for files this job tracks (a stray file of that name is not
 * ours to credit). The stats run outside j->mu. */
static uint64_t held_bytes(ava1_job_t *j) {
    uint32_t *ids, n = 0, i;
    uint64_t sum = 0;
    if (!(ids = malloc(sizeof *ids * (j->m.n ? j->m.n : 1u)))) return 0;
    pthread_mutex_lock(&j->mu);
    for (i = 0; i < j->m.n; i++)
        if (j->lf[i] && j->m.e[i].kind == AVA1_ENTRY_FILE && !ava1_bits_get(&j->done, i)) ids[n++] = i;
    pthread_mutex_unlock(&j->mu);
    for (i = 0; i < n; i++) {
        char p[AVA1_PATH_CAP];
        struct stat st;
        uint64_t held, size = j->m.e[ids[i]].size;
        ava1_apply_path(j, ids[i], 1, p, sizeof p);
        if (!p[0] || lstat(p, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        held = (uint64_t)st.st_blocks * 512u;
        sum += held < size ? held : size;
    }
    free(ids);
    return sum;
}

/* JobMap pages: done runs, then the durable ranges of the other files, MAP_ITEMS a page;
 * `last` on the final one. A failed map is one page. */
static void emit_map(ava1_job_t *j, uint16_t status, const char *msg) {
    uint8_t *fb = malloc(16u * MAP_ITEMS), *rb = malloc(32u * MAP_ITEMS), *out = malloc(64u * 1024u);
    uint32_t i = 0, gi = 0;
    size_t gk = 0;
    uint64_t held = 0;
    if (!fb || !rb || !out) goto done;
    if (status == AVA1_STATUS_OK) held = held_bytes(j);
    for (;;) {
        ava1_w_t fw, rw, w;
        ava1_job_map_t m;
        uint32_t items = 0;
        ava1_w_init(&fw, fb, 16u * MAP_ITEMS);
        ava1_w_init(&rw, rb, 32u * MAP_ITEMS);
        pthread_mutex_lock(&j->mu);
        while (status == AVA1_STATUS_OK && i < j->m.n && items < MAP_ITEMS) {
            ava1_file_run_t r;
            if (!ava1_bits_get(&j->done, i)) {
                i++;
                continue;
            }
            r.first = i;
            while (i < j->m.n && ava1_bits_get(&j->done, i)) i++;
            r.count = i - r.first;
            (void)ava1_file_run_append(&fw, &r);
            items++;
        }
        while (status == AVA1_STATUS_OK && i >= j->m.n && gi < j->m.n && items < MAP_ITEMS) {
            ava1_lfile_t *lf = j->lf[gi];
            ava1_file_range_t g;
            uint64_t size = j->m.e[gi].size, s, e;
            if (!lf || ava1_bits_get(&j->done, gi) || gk >= lf->durable.n) {
                gi++;
                gk = 0;
                continue;
            }
            /* Whole groups only (SPEC.md §12.2): the sender resends what is not listed. */
            s = (lf->durable.v[2 * gk] + AVA1_GROUP_LEN - 1) / AVA1_GROUP_LEN * AVA1_GROUP_LEN;
            e = lf->durable.v[2 * gk + 1];
            if (e < size) e = e / AVA1_GROUP_LEN * AVA1_GROUP_LEN;
            gk++;
            if (e <= s) continue;
            g.file_id = gi;
            g.offset = s;
            g.len = e - s;
            (void)ava1_file_range_append(&rw, &g);
            items++;
        }
        pthread_mutex_unlock(&j->mu);
        memset(&m, 0, sizeof m);
        memcpy(m.job_id, j->id, 16);
        m.status = status;
        m.last = (uint8_t)(status != AVA1_STATUS_OK || (i >= j->m.n && gi >= j->m.n));
        m.done = fb;
        m.done_len = (uint32_t)fw.len;
        m.partial = rb;
        m.partial_len = (uint32_t)rw.len;
        if (m.last && held > 0) {
            m.has_held = 1;
            m.held = held;
        }
        if (msg && *msg) {
            m.has_message = 1;
            m.message = (const uint8_t *)msg;
            m.message_len = (uint16_t)strlen(msg);
        }
        ava1_w_init(&w, out, 64u * 1024u);
        if (ava1_job_map_encode(&m, &w) == 0) ava1_job_emit(j, AVA1_TYPE_JOB_MAP, 0, out, w.len);
        if (m.last) break;
    }
done:
    free(fb);
    free(rb);
    free(out);
}

/* ---- open -------------------------------------------------------------------------- */

static void recv_events(ava1_job_t *j);

static ava1_job_t *refuse(ava1_job_open_ack_t *ack, uint16_t status, char *msg, size_t cap, const char *why) {
    ack->status = status;
    if (cap) snprintf(msg, cap, "%s", why);
    return NULL;
}

/* What this receiver understands: anything else in a JobOpen is a protocol error. */
static int known_open(const ava1_recv_spec_t *s) {
    const uint32_t flags = AVA1_JF_SINGLE_FILE | AVA1_JF_ORDERED | AVA1_JF_UNSAFE_READ | AVA1_JF_MOVE;
    return s->kind >= AVA1_JOB_UPLOAD && s->kind <= AVA1_JOB_COPY && s->policy <= AVA1_POLICY_VERIFY &&
           !(s->flags & ~flags);
}

/* The job a journal's Open describes is this JobOpen's job. */
static int same_job(const ava1_jnl_open_t *o, const ava1_recv_spec_t *s) {
    return memcmp(o->job_id, s->id, 16) == 0 && o->kind == s->kind && o->flags == s->flags &&
           o->root_len == strlen(s->root) && memcmp(o->root, s->root, o->root_len) == 0;
}

ava1_job_t *ava1_recv_open(const ava1_recv_spec_t *s, ava1_job_open_ack_t *ack, char *msg, size_t cap) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    ava1_job_t *j;
    struct stat st;
    int peeked;
    uint8_t peek_staged;
    memset(ack, 0, sizeof *ack);
    memcpy(ack->job_id, s->id, 16);
    if (cap) msg[0] = 0;
    if (!known_open(s)) return refuse(ack, AVA1_ERR_PROTOCOL, msg, cap, "unknown job kind, policy or flags");
    /* An absolute path whose rest follows the manifest path rules (SPEC.md §11.2): no
     * trailing '/', no empty, "." or ".." component. */
    if (!s->root || s->root[0] != '/' || strlen(s->root) > AVA1_MAX_PATH || /* j->root holds no more */
        !ava1_path_ok((const uint8_t *)s->root + 1, strlen(s->root) - 1))
        return refuse(ack, AVA1_ERR_PATH, msg, cap, "the destination is not a valid path");
    if (!cfg->may_write || !cfg->may_write(s->root))
        return refuse(ack, AVA1_ERR_PATH, msg, cap, "writing there is not allowed");
    if (s->entries > AVA1_MAX_ENTRIES) return refuse(ack, AVA1_ERR_PROTOCOL, msg, cap, "the manifest has too many entries");
    j = ava1_job_find(s->id);
    if (j && owner_is_recovery(j->owner)) {
        /* recovery's throwaway job (housekeeping, seconds at most): the sender retries on BUSY and then
         * resumes a settled job, instead of being told the job belongs to another device */
        ava1_job_put(j);
        return refuse(ack, AVA1_ERR_BUSY, msg, cap, "the console is finishing this job's files; try again");
    }
    if (j && memcmp(j->owner, s->owner, 32) != 0) {
        ava1_job_put(j);
        return refuse(ack, AVA1_ERR_UNKNOWN_JOB, msg, cap, "this job belongs to another device");
    }
    if (j) {
        int dead, other;
        pthread_mutex_lock(&j->mu);
        /* A job that failed in memory (disk full, I/O) or was stopped restarts from its
         * journal. One that ended for good stays and answers again. */
        dead = j->stopping || (j->finished && j->final_status != AVA1_STATUS_OK && j->final_status != AVA1_ERR_EXISTS &&
                               j->final_status != AVA1_ERR_CROSS_DEVICE);
        other = j->kind != s->kind || j->flags != s->flags || strcmp(j->root, s->root) != 0;
        pthread_mutex_unlock(&j->mu);
        if (other) {
            ava1_job_put(j);
            return refuse(ack, AVA1_ERR_PROTOCOL, msg, cap, "this job was opened for another destination");
        }
        /* Its directory is reused only once its threads are gone: never while an old session
         * still holds it. */
        if (dead && ava1_job_retire(j) != 0)
            return refuse(ack, AVA1_ERR_BUSY, msg, cap, "the job's previous session is still closing");
        if (dead) j = NULL;
    }
    if (j) { /* re-attach: a new session for a job still in memory */
        pthread_mutex_lock(&j->mu);
        j->emit = s->emit;
        j->emit_ctx = s->emit_ctx;
        j->policy = s->policy;
        ava1_mstore_free(&j->m_in);
        if (s->entries) (void)ava1_mstore_reserve(&j->m_in, s->entries);
        j->ev_end = j->ev_resume = 0;
        /* The new sender starts from what is free now. Credit freed while detached is
         * already counted here, so it must not also be flushed as a Credit message. */
        ack->credit = j->credit > j->outstanding ? j->credit - j->outstanding : 0;
        j->credit_back = 0;
        ack->staged = (uint8_t)j->staged;
        ack->workers = j->want_workers;
        pthread_mutex_unlock(&j->mu);
        return j;
    }
    {
        /* The journal on disk, read without touching it: a JobOpen for another root (or
         * kind) is refused and the real job's journal kept as it is. */
        char dir[sizeof j->dir];
        uint8_t buf[AVA1_MAX_PATH + 256];
        ava1_jnl_open_t o;
        ava1_job_dir(cfg->jobs_dir, s->id, dir, sizeof dir);
        peeked = ava1_jnl_peek_open(dir, buf, sizeof buf, &o) == 0;
        if (peeked && !same_job(&o, s))
            return refuse(ack, AVA1_ERR_PROTOCOL, msg, cap, "this job was opened for another destination");
        peek_staged = peeked ? o.staged : 0;
    }
    j = ava1_job_create_attached(s->id, s->owner, s->sid);
    if (!j) return refuse(ack, AVA1_ERR_BUSY, msg, cap, "too many jobs, or this job is still closing; try again");
    if (!j->log_small && s->kind == AVA1_JOB_UPLOAD)
        fprintf(stderr, "[ava1] job %02x%02x%02x%02x: durable-by-log OFF (%s)\n", s->id[0], s->id[1], s->id[2], s->id[3],
                ava1_data_log_small_flagged() ? "debug flag" : "config");
    j->kind = s->kind;
    j->policy = s->policy;
    j->flags = s->flags;
    j->emit = s->emit;
    j->emit_ctx = s->emit_ctx;
    snprintf(j->root, sizeof j->root, "%s", s->root);
    (void)ava1_mkdirs(cfg->jobs_dir, 0);
    ava1_job_dir(cfg->jobs_dir, s->id, j->dir, sizeof j->dir);
    if (load_from_disk(j) != 0) {
        drop_state(j); /* no usable progress on disk: a new job */
        if (peeked) {
            /* Its journal is this job's, only its progress is not usable (a crash between
             * the manifest write and the journal): the staging choice and the hold on
             * <root> still stand, or our own lock folder would turn into a merge target. */
            j->staged = peek_staged & 1;
            j->dest_held = (peek_staged & AVA1_STAGED_HELD) != 0;
        } else {
            j->staged = !(s->flags & AVA1_JF_SINGLE_FILE) && stat(s->root, &st) != 0;
        }
        (void)ava1_mkdirs(j->dir, 0);
    }
    snprintf(j->base, sizeof j->base, "%s%s", j->root, j->staged ? ".ava-part" : "");
    if (s->entries && ava1_mstore_reserve(&j->m_in, s->entries) != 0) {
        ava1_job_free_one(j->id);
        ava1_job_put(j);
        return refuse(ack, AVA1_ERR_BUSY, msg, cap, "not enough memory for the manifest");
    }
    j->credit = ava1_budget_take(CREDIT_WANT, CREDIT_MIN); /* returned when the job ends or is freed */
    if (!j->credit) {
        ava1_job_free_one(j->id);
        ava1_job_put(j);
        return refuse(ack, AVA1_ERR_BUSY, msg, cap, "the console is busy with other transfers");
    }
    j->on_events = recv_events;
    if (ava1_apply_start(j) != 0) {
        ava1_job_free_one(j->id);
        ava1_job_put(j);
        return refuse(ack, AVA1_ERR_INTERNAL, msg, cap, "cannot start the job's threads");
    }
    ack->credit = j->credit;
    ack->staged = (uint8_t)j->staged;
    ack->workers = j->want_workers;
    return j;
}

int ava1_recv_page(ava1_job_t *j, const ava1_manifest_page_t *p) {
    int rc;
    pthread_mutex_lock(&j->mu);
    rc = ava1_mstore_add_page(&j->m_in, p);
    pthread_mutex_unlock(&j->mu);
    return rc;
}

int ava1_recv_end(ava1_job_t *j, const ava1_manifest_end_t *e) {
    pthread_mutex_lock(&j->mu);
    j->end_files = e->files;
    j->end_bytes = e->bytes;
    memcpy(j->end_hash, e->manifest_hash, 32);
    j->ev_end = 1;
    pthread_mutex_unlock(&j->mu);
    return 0;
}

int ava1_recv_resume(ava1_job_t *j, const uint8_t hash[32]) {
    pthread_mutex_lock(&j->mu);
    memcpy(j->end_hash, hash, 32);
    j->ev_resume = 1;
    pthread_mutex_unlock(&j->mu);
    return 0;
}

void ava1_recv_cancel(ava1_job_t *j) {
    pthread_mutex_lock(&j->mu);
    j->stopping = 1;
    if (j->kind == AVA1_JOB_COPY) j->discard_parts = 1; /* a copy is never resumed: its part files go (final review #9) */
    pthread_cond_broadcast(&j->cv);
    pthread_mutex_unlock(&j->mu);
    ava1_job_free_one(j->id); /* the journal stays: a later JobOpen resumes */
}

/* ---- a changed manifest ------------------------------------------------------------ */

static uint64_t fnv(const char *s) {
    uint64_t h = 1469598103934665603ull;
    while (*s) h = (h ^ (uint8_t)*s++) * 1099511628211ull;
    return h;
}

/* Carries progress from the old manifest (j->m) to `in` by path (SPEC.md §11.5): a file
 * keeps it only when kind, size and mtime are unchanged. Its outboard follows it to the new
 * id (two renames in the job directory, so no name is overwritten). A changed file that had
 * progress loses its old bytes and is marked in `changed` for a journaled reset. */
/* Removes one of our own leftovers and remembers its folder for a sync. */
static void drop_path(const char *p, int dir, ava1_dirent_t *d, uint32_t *nd) {
    char parent[AVA1_PATH_CAP];
    if (!p[0] || (dir ? rmdir(p) : unlink(p)) != 0) return;
    ava1_parent_of(p, parent, sizeof parent);
    if ((d[*nd].dir = strdup(parent)) != NULL) d[(*nd)++].id = 0;
}

static int remap(ava1_job_t *j, ava1_mstore_t *in, ava1_bits_t *changed) {
    const uint32_t NONE = UINT32_MAX;
    uint32_t cap = 1, i, *slot, *o2n, nd = 0, k, kept = 0;
    ava1_lfile_t **nlf;
    ava1_bits_t ndone, seen;
    ava1_dirent_t *d;
    char a[600], b[600], p[AVA1_PATH_CAP];
    int rc, ours = j->staged && !(j->flags & AVA1_JF_SINGLE_FILE); /* the whole tree is ours */
    while (cap < 2u * j->m.n + 2u) cap *= 2;
    slot = calloc(cap, sizeof *slot);
    o2n = malloc(((size_t)j->m.n + 1) * sizeof *o2n);
    d = calloc(2 * (size_t)j->m.n + 2, sizeof *d); /* up to two removals an entry, + the job dir */
    nlf = calloc((size_t)in->n + 1, sizeof *nlf);
    memset(&ndone, 0, sizeof ndone);
    memset(&seen, 0, sizeof seen);
    if (!slot || !o2n || !d || !nlf || ava1_bits_init(&ndone, in->n) != 0 || ava1_bits_init(&seen, j->m.n) != 0 ||
        ava1_bits_init(changed, in->n) != 0) {
        free(slot);
        free(o2n);
        free(d);
        free(nlf);
        ava1_bits_free(&ndone);
        ava1_bits_free(&seen);
        return -1;
    }
    for (i = 0; i < j->m.n; i++) {
        uint32_t h = (uint32_t)fnv(ava1_mstore_path(&j->m, i)) & (cap - 1);
        while (slot[h]) h = (h + 1) & (cap - 1);
        slot[h] = i + 1;
        o2n[i] = NONE;
    }
    for (i = 0; i < in->n; i++) {
        const ava1_ment_t *ne = &in->e[i];
        const char *path = ava1_mstore_path(in, i);
        uint32_t h = (uint32_t)fnv(path) & (cap - 1), o;
        for (; (o = slot[h]) != 0; h = (h + 1) & (cap - 1)) {
            const ava1_ment_t *oe = &j->m.e[o - 1];
            if (strcmp(ava1_mstore_path(&j->m, o - 1), path) != 0) continue;
            ava1_bits_set(&seen, o - 1);
            if (oe->kind == ne->kind && oe->size == ne->size && oe->mtime == ne->mtime) {
                o2n[o - 1] = i;
                if (ava1_bits_get(&j->done, o - 1)) ava1_bits_set(&ndone, i);
                if (j->lf[o - 1]) {
                    nlf[i] = j->lf[o - 1];
                    j->lf[o - 1] = NULL;
                    ava1_rset_clear(&nlf[i]->written); /* never synced: not in the map, sent again */
                    snprintf(a, sizeof a, "%s/%u.ob", j->dir, o - 1);
                    snprintf(b, sizeof b, "%s/%u.ob.m", j->dir, i);
                    (void)rename(a, b); /* same directory */
                }
            } else if (oe->kind == AVA1_ENTRY_FILE && (j->lf[o - 1] || ava1_bits_get(&j->done, o - 1))) {
                /* Never splice old and new bytes: the old part file goes now. */
                free_lf(j, o - 1, j->lf[o - 1]);
                j->lf[o - 1] = NULL;
                ava1_apply_path(j, o - 1, 1, p, sizeof p);
                drop_path(p, 0, d, &nd);
                if (ne->kind == AVA1_ENTRY_FILE) ava1_bits_set(changed, i);
            }
            break;
        }
    }
    /* Entries the new manifest no longer has: their part files go, and inside a staged tree
     * (ours alone) their finished files and emptied folders too, or the final rename would
     * deliver them. In place, a finished file is the user's now and stays. Children first. */
    for (i = j->m.n; i-- > 0;) {
        if (ava1_bits_get(&seen, i)) continue;
        if (j->m.e[i].kind == AVA1_ENTRY_FILE) {
            if (j->lf[i]) {
                ava1_apply_path(j, i, 1, p, sizeof p);
                drop_path(p, 0, d, &nd);
            }
            if (ours && ava1_bits_get(&j->done, i)) {
                ava1_apply_path(j, i, 0, p, sizeof p);
                drop_path(p, 0, d, &nd);
            }
        } else if (ours) {
            ava1_apply_path(j, i, 0, p, sizeof p);
            drop_path(p, 1, d, &nd);
        }
    }
    /* Second step of the outboard renames; drop the outboards nobody carried. */
    for (i = 0; i < j->m.n; i++) {
        snprintf(a, sizeof a, "%s/%u.ob", j->dir, i);
        (void)unlink(a);
        free_lf(j, i, j->lf[i]);
    }
    for (i = 0; i < in->n; i++)
        if (nlf[i]) {
            snprintf(a, sizeof a, "%s/%u.ob.m", j->dir, i);
            snprintf(b, sizeof b, "%s/%u.ob", j->dir, i);
            (void)rename(a, b);
        }
    if ((d[nd].dir = strdup(j->dir)) != NULL) d[nd++].id = 0;
    for (k = 0; k < nd;) { /* a folder removed itself needs no sync (its parent gets one) */
        struct stat st;
        if (lstat(d[k].dir, &st) == 0) {
            k++;
            continue;
        }
        free(d[k].dir);
        d[k] = d[--nd];
    }
    /* the renames and removals are durable before the journal names the new ids */
    rc = ava1_sync_dirset(j, d, nd, 0);
    /* The resume check follows each file to its new id (M1): a torn tail is still caught. */
    for (k = 0; k < j->last_ranges_n; k++) {
        uint32_t f = j->last_ranges[k].file_id;
        if (f < j->m.n && o2n[f] != NONE) {
            j->last_ranges[kept] = j->last_ranges[k];
            j->last_ranges[kept++].file_id = o2n[f];
        }
    }
    j->last_ranges_n = kept;
    free(slot);
    free(o2n);
    free(d);
    ava1_bits_free(&seen);
    pthread_mutex_lock(&j->mu);
    free(j->lf);
    ava1_bits_free(&j->done);
    ava1_mstore_free(&j->m);
    j->m = *in;
    memset(in, 0, sizeof *in);
    j->lf = nlf;
    j->done = ndone;
    ava1_lflist_rebuild(j); /* the files moved to new ids */
    pthread_mutex_unlock(&j->mu);
    return rc ? -1 : 0;
}

/* ---- prepare ----------------------------------------------------------------------- */

/* Takes <root> for a staged job: mkdir(<root>) is the lock that keeps anyone else's folder
 * from being replaced by the final rename (SPEC.md §11.6). A journaled hold recognises the
 * empty folder as ours on resume. */
static uint16_t take_dest(ava1_job_t *j, int fresh, char *msg, size_t cap) {
    char parent[AVA1_PATH_CAP];
    struct stat st;
    int rc;
    if (!j->staged || (j->flags & AVA1_JF_SINGLE_FILE)) return AVA1_STATUS_OK;
    ava1_parent_of(j->root, parent, sizeof parent);
    if (ava1_mkdirs(parent, 1) != 0) {
        snprintf(msg, cap, "cannot create the folder that holds the destination");
        return AVA1_ERR_IO;
    }
    if (mkdir(j->root, 0755) == 0) {
        if ((rc = ava1_sync_dir(parent)) != 0) {
            snprintf(msg, cap, "syncing the destination's folder failed: %s", strerror(rc));
            return AVA1_ERR_IO;
        }
    } else if (errno != EEXIST) {
        snprintf(msg, cap, "cannot create the destination: %s", strerror(errno));
        return AVA1_ERR_IO;
    } else if (!j->dest_held || lstat(j->root, &st) != 0 || !S_ISDIR(st.st_mode)) {
        snprintf(msg, cap, "the destination already exists");
        return AVA1_ERR_EXISTS;
    }
    if (!j->dest_held) {
        j->dest_held = 1;
        if (!fresh) ava1_apply_compact(j); /* records the hold */
    }
    return AVA1_STATUS_OK;
}

typedef struct {
    uint32_t stripes;
    ava1_bits_t hit;
    pthread_mutex_t mu;
} policy_t;

/* The file at `p` is a regular file whose BLAKE3 (its root, SPEC.md §13.1) is `want`. */
static int hashes_to(const char *p, const uint8_t want[32]) {
    blake3_hasher h;
    uint8_t *buf = malloc(1u << 20), out[32];
    int fd = open(p, O_RDONLY | O_NOFOLLOW), ok = 0;
    ssize_t k = -1;
    if (buf && fd >= 0) {
        blake3_hasher_init(&h);
        while ((k = read(fd, buf, 1u << 20)) > 0) blake3_hasher_update(&h, buf, (size_t)k);
        blake3_hasher_finalize(&h, out, 32);
        ok = k == 0 && memcmp(out, want, 32) == 0;
    }
    if (fd >= 0) close(fd);
    free(buf);
    return ok;
}

/* Forgets a file's part in progress: part file, outboard and state (it is done another way). */
static void drop_part(ava1_job_t *j, uint32_t id) {
    char p[AVA1_PATH_CAP], f[AVA1_PATH_CAP], ob[600];
    ava1_lfile_t *lf;
    ava1_apply_path(j, id, 1, p, sizeof p);
    ava1_apply_path(j, id, 0, f, sizeof f);
    if (p[0] && strcmp(p, f) != 0) (void)unlink(p);
    snprintf(ob, sizeof ob, "%s/%u.ob", j->dir, id);
    (void)unlink(ob);
    pthread_mutex_lock(&j->mu);
    lf = j->lf[id];
    j->lf[id] = NULL;
    if (lf) ava1_lf_close_fds(j, id, lf);
    pthread_mutex_unlock(&j->mu);
    free_lf(j, id, lf);
}

static int file_matches(ava1_job_t *j, uint32_t id) {
    const ava1_ment_t *e = &j->m.e[id];
    const uint8_t *want;
    char p[AVA1_PATH_CAP];
    struct stat st;
    ava1_apply_path(j, id, 0, p, sizeof p);
    if (!p[0] || lstat(p, &st) != 0 || !S_ISREG(st.st_mode) || (uint64_t)st.st_size != e->size) return 0;
    if (j->policy == AVA1_POLICY_SKIP_EXISTING) return (uint64_t)st.st_mtime == e->mtime;
    if (j->policy == AVA1_POLICY_VERIFY && (want = ava1_mstore_root(&j->m, id)) != NULL) return hashes_to(p, want);
    return 0;
}

static void policy_stripe(ava1_job_t *j, void *arg, uint32_t s) {
    policy_t *p = arg;
    uint32_t i;
    for (i = s; i < j->m.n; i += p->stripes) {
        if (j->m.e[i].kind != AVA1_ENTRY_FILE || ava1_bits_get(&j->done, i)) continue;
        if (file_matches(j, i)) {
            pthread_mutex_lock(&p->mu);
            ava1_bits_set(&p->hit, i);
            pthread_mutex_unlock(&p->mu);
        }
    }
}

static int journal_files(ava1_job_t *j, const ava1_bits_t *b) {
    size_t cap = 12u * (size_t)j->m.n + 16;
    uint8_t *fb = malloc(cap), *body = malloc(cap + 64);
    ava1_w_t fw, w;
    ava1_jnl_batch_t jb;
    int rc = -1;
    if (fb && body) {
        ava1_w_init(&fw, fb, cap);
        if (ava1_bits_append_runs(b, &fw) == 0) {
            memset(&jb, 0, sizeof jb);
            jb.files = fb;
            jb.files_len = (uint32_t)fw.len;
            ava1_w_init(&w, body, cap + 64);
            if (ava1_jnl_batch_encode(&jb, &w) == 0) rc = ava1_apply_jnl_append(j, AVA1_JNL_BATCH, body, w.len);
        }
    }
    free(fb);
    free(body);
    return rc;
}

/* §13.4: re-hash the durable groups of the last journaled batch from the part file and
 * compare them with the outboard; a file whose bytes differ starts over (silently). */
static void resume_check(ava1_job_t *j) {
    uint8_t *buf = malloc(AVA1_GROUP_LEN);
    uint32_t k;
    char p[AVA1_PATH_CAP], obp[600];
    if (!buf) return;
    for (k = 0; k < j->last_ranges_n; k++) {
        ava1_file_range_t g = j->last_ranges[k];
        ava1_lfile_t *lf;
        uint64_t size, gi;
        int fd, ob, bad = 0;
        if (g.file_id >= j->m.n || ava1_bits_get(&j->done, g.file_id) || !(lf = j->lf[g.file_id])) continue;
        size = j->m.e[g.file_id].size;
        if (ava1_groups_of(size) < 2) continue; /* one-group files are hashed whole at commit */
        ava1_apply_path(j, g.file_id, 1, p, sizeof p);
        snprintf(obp, sizeof obp, "%s/%u.ob", j->dir, g.file_id);
        fd = p[0] ? open(p, O_RDONLY | O_NOFOLLOW) : -1;
        if (fd < 0 && errno == ENOENT) continue; /* reconcile() decides what a missing part means */
        ob = open(obp, O_RDONLY);
        if (fd < 0 || ob < 0) bad = 1;
        for (gi = g.offset / AVA1_GROUP_LEN; !bad && gi * AVA1_GROUP_LEN < g.offset + g.len && gi * AVA1_GROUP_LEN < size; gi++) {
            uint64_t gs = gi * AVA1_GROUP_LEN, glen = size - gs < AVA1_GROUP_LEN ? size - gs : AVA1_GROUP_LEN;
            uint8_t cv[32], want[32];
            if (!ava1_rset_covers(&lf->durable, gs, gs + glen)) continue; /* reset since, or not whole */
            if (pread(fd, buf, (size_t)glen, (off_t)gs) != (ssize_t)glen || pread(ob, want, 32, (off_t)(gi * 32u)) != 32) {
                bad = 1;
                break;
            }
            ava1_b3_group_cv(buf, (size_t)glen, gi, cv);
            bad = memcmp(cv, want, 32) != 0;
        }
        if (fd >= 0) close(fd);
        if (ob >= 0) close(ob);
        if (bad && lf->durable.n) ava1_apply_reset(j, g.file_id, 0);
    }
    free(buf);
    forget_ranges(j);
}

/* A file whose every range and root are durable but that is not done was mid-commit when
 * the job stopped. Its part file still there: the commit runs again after the map. Gone:
 * the rename happened, so the file in place is it if it hashes to the root — journal it
 * done. A file with durable ranges whose part file is gone, or anything else, starts over.
 * The commit itself never recreates a missing part. */
static uint16_t reconcile(ava1_job_t *j, char *msg, size_t cap) {
    ava1_bits_t ok;
    uint32_t i;
    int any = 0;
    if (ava1_bits_init(&ok, j->m.n) != 0) {
        snprintf(msg, cap, "out of memory");
        return AVA1_ERR_INTERNAL;
    }
    for (i = 0; i < j->m.n; i++) {
        ava1_lfile_t *lf = j->lf[i];
        const ava1_ment_t *e = &j->m.e[i];
        char p[AVA1_PATH_CAP], f[AVA1_PATH_CAP];
        uint8_t root[32];
        struct stat st;
        int whole;
        if (e->kind != AVA1_ENTRY_FILE || !lf || ava1_bits_get(&j->done, i)) continue;
        pthread_mutex_lock(&j->mu);
        whole = lf->has_root && lf->root_journaled && (e->size == 0 || ava1_rset_covers(&lf->durable, 0, e->size));
        memcpy(root, lf->root, 32);
        pthread_mutex_unlock(&j->mu);
        if (!whole && !lf->durable.n) continue;
        ava1_apply_path(j, i, 1, p, sizeof p);
        ava1_apply_path(j, i, 0, f, sizeof f);
        if (p[0] && lstat(p, &st) == 0 && S_ISREG(st.st_mode)) continue; /* whole: commit_ready */
        /* The part file is gone: only a finished rename explains that for a whole file. */
        if (whole && p[0] && f[0] && strcmp(p, f) != 0 && lstat(f, &st) == 0 && S_ISREG(st.st_mode) &&
            (uint64_t)st.st_size == e->size && hashes_to(f, root)) {
            ava1_bits_set(&ok, i);
            any = 1;
        } else {
            ava1_apply_reset(j, i, 0);
        }
    }
    if (any) {
        if (journal_files(j, &ok) != 0) {
            ava1_bits_free(&ok);
            snprintf(msg, cap, "journal append failed");
            return AVA1_ERR_IO;
        }
        for (i = 0; i < j->m.n; i++)
            if (ava1_bits_get(&ok, i)) {
                pthread_mutex_lock(&j->mu);
                ava1_bits_set(&j->done, i);
                pthread_mutex_unlock(&j->mu);
                drop_part(j, i); /* only its outboard is left */
            }
    }
    ava1_bits_free(&ok);
    return AVA1_STATUS_OK;
}

/* The base folder and every directory entry (merge mode refuses one that is a symlink),
 * the parents of what was created synced; then the policies and the resume check. */
static uint16_t prepare(ava1_job_t *j, char *msg, size_t cap) {
    uint32_t i, nd = 0;
    char p[AVA1_PATH_CAP], parent[AVA1_PATH_CAP];
    struct stat st;
    uint16_t status;
    int rc;
    if ((status = take_dest(j, 0, msg, cap)) != AVA1_STATUS_OK) return status;
    if (!(j->flags & AVA1_JF_SINGLE_FILE)) {
        ava1_dirent_t *d = calloc((size_t)j->m.n + 1, sizeof *d);
        if (!d) {
            snprintf(msg, cap, "out of memory");
            return AVA1_ERR_INTERNAL;
        }
        if (ava1_mkdirs(j->base, 1) != 0) {
            snprintf(msg, cap, "cannot create the destination");
            free(d);
            return AVA1_ERR_IO;
        }
        status = AVA1_STATUS_OK;
        for (i = 0; i < j->m.n && status == AVA1_STATUS_OK; i++) {
            const char *rel = ava1_mstore_path(&j->m, i);
            if (j->m.e[i].kind != AVA1_ENTRY_DIR || !rel) continue;
            if (snprintf(p, sizeof p, "%s/%s", j->base, rel) >= (int)sizeof p) {
                snprintf(msg, cap, "a path is too long");
                status = AVA1_ERR_PATH;
            } else if (lstat(p, &st) == 0) {
                if (S_ISLNK(st.st_mode) && !j->staged) {
                    snprintf(msg, cap, "%.100s is a symbolic link", rel);
                    status = AVA1_ERR_PATH;
                } else if (!S_ISDIR(st.st_mode)) {
                    snprintf(msg, cap, "%.100s is not a folder", rel);
                    status = AVA1_ERR_PATH;
                }
            } else if (mkdir(p, 0755) == 0 || (errno == ENOENT && ava1_mkdirs(p, 1) == 0)) {
                ava1_parent_of(p, parent, sizeof parent);
                if (!(d[nd].dir = strdup(parent))) status = AVA1_ERR_INTERNAL;
                else d[nd++].id = i;
            } else {
                snprintf(msg, cap, "cannot create %.100s", rel);
                status = AVA1_ERR_IO;
            }
        }
        rc = ava1_sync_dirset(j, d, nd, AVA1_HOOK_PREP_DIR_SYNCED); /* frees the strings */
        free(d);
        if (status != AVA1_STATUS_OK) return status;
        if (rc) {
            snprintf(msg, cap, "syncing the new folders failed");
            return AVA1_ERR_IO;
        }
    } else {
        ava1_parent_of(j->root, parent, sizeof parent);
        if (ava1_mkdirs(parent, 1) != 0) {
            snprintf(msg, cap, "cannot create the destination's folder");
            return AVA1_ERR_IO;
        }
    }
    /* Policies apply where the target can already exist: merge mode or a single file. */
    if (j->policy != AVA1_POLICY_REPLACE && (!j->staged || (j->flags & AVA1_JF_SINGLE_FILE))) {
        policy_t pol;
        memset(&pol, 0, sizeof pol);
        pol.stripes = j->want_workers ? j->want_workers : 1;
        pthread_mutex_init(&pol.mu, NULL);
        if (ava1_bits_init(&pol.hit, j->m.n) == 0) {
            if (ava1_apply_parallel(j, policy_stripe, &pol, pol.stripes) == 0 && ava1_bits_count(&pol.hit)) {
                if (journal_files(j, &pol.hit) != 0) {
                    snprintf(msg, cap, "journal append failed");
                    status = AVA1_ERR_IO;
                } else {
                    for (i = 0; i < j->m.n; i++)
                        if (ava1_bits_get(&pol.hit, i)) {
                            pthread_mutex_lock(&j->mu);
                            ava1_bits_set(&j->done, i);
                            pthread_mutex_unlock(&j->mu);
                            if (j->lf[i]) drop_part(j, i); /* the existing file won */
                        }
                }
            }
            ava1_bits_free(&pol.hit);
        }
        pthread_mutex_destroy(&pol.mu);
        if (status != AVA1_STATUS_OK) return status;
    }
    resume_check(j);
    return reconcile(j, msg, cap);
}

/* ---- the job thread's events ------------------------------------------------------- */

/* A held staged job whose .ava-part is gone, whose <root> is a folder and whose files are
 * all done: the final rename happened. */
static int staged_tree_landed(ava1_job_t *j) {
    struct stat st;
    int all;
    if (!j->staged || !j->dest_held || (j->flags & AVA1_JF_SINGLE_FILE)) return 0;
    if (lstat(j->base, &st) == 0 || errno != ENOENT) return 0;
    if (lstat(j->root, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;
    pthread_mutex_lock(&j->mu);
    recount(j);
    /* With no files, "all done" says nothing about the tree: prepare must make its folders. */
    all = j->m.files > 0 && j->files_done >= j->m.files;
    if (all) j->prepared = 1;
    pthread_mutex_unlock(&j->mu);
    return all;
}

/* The map for a manifest that is in place: prepared once, then answered from the state. */
static void answer(ava1_job_t *j) {
    char msg[160] = "";
    uint16_t st;
    uint32_t i;
    if (j->replay_done) {
        /* The journal says the job ended: every byte was durable. All done, then the
         * journaled status — the idempotent answer to a sender that lost the first JobDone. */
        pthread_mutex_lock(&j->mu);
        for (i = 0; i < j->m.n; i++)
            if (j->m.e[i].kind == AVA1_ENTRY_FILE) ava1_bits_set(&j->done, i);
        recount(j);
        j->prepared = 1;
        pthread_mutex_unlock(&j->mu);
        emit_map(j, AVA1_STATUS_OK, NULL);
        ava1_apply_fail(j, j->replay_status, "", 0, 0); /* the journal already has its Done */
        return;
    }
    if (!j->prepared && staged_tree_landed(j)) {
        /* Stopped between the staging rename and its journaled Done: the tree is in place. */
        emit_map(j, AVA1_STATUS_OK, NULL);
        ava1_apply_finish_landed(j);
        return;
    }
    if (!j->prepared && (st = prepare(j, msg, sizeof msg)) != AVA1_STATUS_OK) {
        emit_map(j, st, msg);
        /* An existing destination is final for this job; anything else may succeed on a retry. */
        ava1_apply_fail(j, st, msg, 0, st == AVA1_ERR_EXISTS && j->jnl.fd >= 0);
        return;
    }
    pthread_mutex_lock(&j->mu);
    recount(j);
    j->prepared = 1;
    pthread_mutex_unlock(&j->mu);
    emit_map(j, AVA1_STATUS_OK, NULL);
    /* Files every byte and root of which are durable commit now: no sync batch may come to
     * trigger it (nothing is left to send). */
    ava1_apply_commit_ready(j);
}

/* A new or changed manifest becomes the job's: written, then journaled. */
static uint16_t adopt(ava1_job_t *j, ava1_mstore_t *in, const uint8_t hash[32], char *msg, size_t cap) {
    ava1_bits_t changed;
    uint8_t *blob;
    size_t len;
    uint32_t i;
    uint16_t st;
    int fresh = !j->have_manifest;
    memset(&changed, 0, sizeof changed);
    pthread_mutex_lock(&j->mu);
    j->prepared = 0;
    pthread_mutex_unlock(&j->mu);
    if (fresh) {
        pthread_mutex_lock(&j->mu);
        j->m = *in;
        memset(in, 0, sizeof *in);
        pthread_mutex_unlock(&j->mu);
        if (alloc_state(j) != 0) {
            snprintf(msg, cap, "out of memory");
            return AVA1_ERR_INTERNAL;
        }
    } else if (remap(j, in, &changed) != 0) {
        ava1_bits_free(&changed);
        snprintf(msg, cap, "carrying the progress over failed");
        return AVA1_ERR_IO;
    }
    if (ava1_data_cfg()->crash_at == AVA1_CRASH_MID_REMAP && !fresh) {
        ava1_bits_free(&changed);
        ava1_apply_crash(j); /* tests: the renames are done, the manifest and journal are not */
        return AVA1_STATUS_OK;
    }
    memcpy(j->manifest_hash, hash, 32);
    j->have_manifest = 1;
    j->replay_done = 0;
    if (ava1_mstore_blob(&j->m, &blob, &len) != 0) {
        ava1_bits_free(&changed);
        snprintf(msg, cap, "out of memory");
        return AVA1_ERR_INTERNAL;
    }
    st = ava1_manifest_file_write(j->dir, blob, len) == 0 ? AVA1_STATUS_OK : AVA1_ERR_IO;
    free(blob);
    if (st != AVA1_STATUS_OK) {
        ava1_bits_free(&changed);
        snprintf(msg, cap, "writing the job's manifest failed");
        return st;
    }
    if (fresh) {
        ava1_jnl_open_t o;
        if ((st = take_dest(j, 1, msg, cap)) != AVA1_STATUS_OK) return st;
        if (ava1_data_cfg()->crash_at == AVA1_CRASH_AFTER_TAKE) {
            ava1_apply_crash(j);
            return AVA1_STATUS_OK;
        }
        memset(&o, 0, sizeof o);
        memcpy(o.job_id, j->id, 16);
        memcpy(o.manifest_hash, hash, 32);
        o.kind = j->kind;
        o.flags = j->flags;
        o.staged = (uint8_t)((j->staged ? 1 : 0) | (j->dest_held ? AVA1_STAGED_HELD : 0));
        o.root = (const uint8_t *)j->root;
        o.root_len = (uint16_t)strlen(j->root);
        if (ava1_jnl_create(&j->jnl, j->dir, &o) != 0) {
            snprintf(msg, cap, "creating the job's journal failed");
            return AVA1_ERR_IO;
        }
    } else {
        ava1_apply_compact(j); /* Open (the new hash) + Snapshot of the carried state */
        for (i = 0; i < changed.n; i++)
            if (ava1_bits_get(&changed, i)) {
                ava1_lfile_t *lf;
                pthread_mutex_lock(&j->mu);
                lf = ava1_lfile_get(j, i);
                pthread_mutex_unlock(&j->mu);
                if (lf) ava1_apply_reset(j, i, 0); /* before any of its ranges is asked for */
            }
        ava1_bits_free(&changed);
    }
    return AVA1_STATUS_OK;
}

static void recv_events(ava1_job_t *j) {
    int end, res;
    ava1_mstore_t in;
    uint32_t efiles;
    uint64_t ebytes;
    uint8_t ehash[32], hash[32];
    char msg[160] = "";
    uint16_t st;
    pthread_mutex_lock(&j->mu);
    end = j->ev_end;
    res = j->ev_resume;
    j->ev_end = j->ev_resume = 0;
    in = j->m_in;
    memset(&j->m_in, 0, sizeof j->m_in);
    efiles = j->end_files;
    ebytes = j->end_bytes;
    memcpy(ehash, j->end_hash, 32);
    pthread_mutex_unlock(&j->mu);
    if (!end) ava1_mstore_free(&in);
    if (j->finished) { /* an ended job answers with how it ended */
        ava1_mstore_free(&in);
        emit_map(j, j->final_status, j->message);
        ava1_apply_done_again(j);
        return;
    }
    if (!end) {
        if (!res) return;
        if (j->have_manifest && memcmp(ehash, j->manifest_hash, 32) == 0) answer(j);
        else emit_map(j, AVA1_ERR_UNKNOWN_JOB, "send the manifest again");
        return;
    }
    /* No entries is a valid manifest (an empty folder): it declares zero files. */
    ava1_mstore_hash(&in, hash);
    if (in.files != efiles || in.bytes != ebytes || memcmp(hash, ehash, 32) != 0) {
        ava1_mstore_free(&in);
        emit_map(j, AVA1_ERR_PROTOCOL, "the manifest does not match its end");
        return;
    }
    if (j->have_manifest && memcmp(hash, j->manifest_hash, 32) == 0) {
        ava1_mstore_free(&in); /* the same manifest: the replayed state stands */
    } else {
        int stopped;
        /* frames of an earlier session name the old ids; and files not yet durable in place must settle first,
         * or the sweep queue would name other files once the ids move (SPEC.md §15.7) */
        if (ava1_apply_quiesce(j) != 0) {
            pthread_mutex_lock(&j->mu);
            stopped = j->stopping;
            pthread_mutex_unlock(&j->mu);
            ava1_mstore_free(&in);
            if (stopped) return; /* a stop or a test crash point */
            snprintf(msg, sizeof msg, "files are still being made durable on the console; try again");
            emit_map(j, AVA1_ERR_IO, msg);
            ava1_apply_fail(j, AVA1_ERR_IO, msg, 0, 0);
            return;
        }
        st = adopt(j, &in, hash, msg, sizeof msg);
        ava1_mstore_free(&in);
        pthread_mutex_lock(&j->mu);
        stopped = j->stopping;
        pthread_mutex_unlock(&j->mu);
        if (stopped) return; /* a test crash point */
        if (st != AVA1_STATUS_OK) {
            emit_map(j, st, msg);
            ava1_apply_fail(j, st, msg, 0, 0);
            return;
        }
    }
    answer(j);
}
