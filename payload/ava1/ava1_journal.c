#include "ava1_journal.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#if defined(__APPLE__) || defined(__FreeBSD__)
#include <sys/sysctl.h>
#endif
#include <time.h>
#include <unistd.h>

#include "ava1_frame.h"
#include "ava1_wire.h"

static const uint8_t MAGIC[8] = { 'A', 'V', 'A', '1', 'J', 'N', 'L', '1' };

static int write_all_fd(int fd, const uint8_t *p, size_t n) {
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

/* ---- fsync with retry (SPEC.md §12.6) ------------------------------------------------ */

/* Tests only (0 in the payload): the next `ava1_fsync_test_fail_n` calls of ava1_fsync_retry
 * fail with this errno instead of reaching the disk. */
int ava1_fsync_test_fail_n, ava1_fsync_test_errno;
unsigned ava1_fsync_retries_total; /* retries made since start (a statistic) */
unsigned ava1_fsync_calls_total;   /* fsync tries made since start (tests count them per batch) */

/* Sony's kernel hands some errors back as 0x8002xxxx instead of an errno; the low 16 bits are the errno. */
static int fsync_errno(int e) {
    return (unsigned)e >= 0x80020000u && (unsigned)e <= 0x8002ffffu ? (int)((unsigned)e & 0xffffu) : e;
}

int ava1_fsync_transient(int e) {
    e = fsync_errno(e);
    /* EIO, ENOSPC, EDQUOT, EBADF, EROFS... are not hiccups: EIO in particular is the kernel
     * saying the data did not reach the drive, and asking again proves nothing. */
    return e == EINTR || e == EAGAIN || e == EBUSY || e == ETIMEDOUT || e == ENOENT || e == ENXIO || e == ENODEV;
}

static int fsync_once(int fd) {
    __atomic_add_fetch(&ava1_fsync_calls_total, 1, __ATOMIC_RELAXED);
    int n = __atomic_load_n(&ava1_fsync_test_fail_n, __ATOMIC_SEQ_CST);
    while (n > 0) {
        if (__atomic_compare_exchange_n(&ava1_fsync_test_fail_n, &n, n - 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST))
            return ava1_fsync_test_errno ? ava1_fsync_test_errno : EIO;
    }
    return fsync(fd) == 0 ? 0 : errno;
}

#define FSYNC_TRIES 5u /* the first call and four retries */

int ava1_fsync_retry(int fd, int (*stopping)(void *), void *arg, int *retried) {
    static const unsigned backoff_ms[FSYNC_TRIES - 1] = { 20, 60, 200, 600 };
    unsigned attempt;
    int e = 0;
    if (retried) *retried = 0;
    for (attempt = 0; attempt < FSYNC_TRIES; attempt++) {
        if (attempt) {
            unsigned ms = backoff_ms[attempt - 1];
            /* in slices a stop can cut */
            while (ms) {
                unsigned step = ms < 20u ? ms : 20u;
                struct timespec ts = { 0, (long)step * 1000000L };
                if (stopping && stopping(arg)) return e;
                nanosleep(&ts, NULL);
                ms -= step;
            }
        }
        e = fsync_once(fd);
        if (e == 0) {
            if (attempt) {
                if (retried) *retried = 1;
                fprintf(stderr, "[ava1] fsync succeeded on retry %u\n", attempt);
            }
            return 0;
        }
        if (!ava1_fsync_transient(e) || attempt + 1 == FSYNC_TRIES) {
            if (attempt) fprintf(stderr, "[ava1] fsync failed after %u retries: errno 0x%x\n", attempt, (unsigned)e);
            return e;
        }
        __atomic_add_fetch(&ava1_fsync_retries_total, 1, __ATOMIC_RELAXED);
        fprintf(stderr, "[ava1] fsync failed (errno 0x%x), retry %u of %u\n", (unsigned)e, attempt + 1,
                FSYNC_TRIES - 1);
    }
    return e;
}

static void put32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static uint32_t get32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

/* len ‖ kind ‖ body ‖ crc(kind ‖ body) in one heap buffer. */
static uint8_t *frame_rec(uint8_t kind, const uint8_t *body, size_t len, size_t *out_len) {
    uint8_t *f = malloc(len + 9);
    if (!f) return NULL;
    put32(f, (uint32_t)(len + 1));
    f[4] = kind;
    if (len) memcpy(f + 5, body, len);
    put32(f + 5 + len, ava1_crc32c(f + 4, len + 1));
    *out_len = len + 9;
    return f;
}

static int sync_dir(const char *dir) {
    int fd = open(dir, O_RDONLY);
    if (fd < 0) return -errno;
    /* The directory fsync is the step that makes a rename durable — the one failure it
     * exists to catch (EIO) must not be swallowed, or callers report durability they
     * do not have. Rust propagates it the same way (SPEC.md §14.1). */
    int e = ava1_fsync_retry(fd, NULL, NULL, NULL), rc = e ? -e : 0;
    close(fd);
    return rc;
}

static void path_in(const char *dir, const char *name, char *out, size_t cap) {
    snprintf(out, cap, "%s/%s", dir, name);
}

/* Bounded copy of `dir` into the journal handle: the stored copy is what compact()
 * later renames through, so a silent truncation would act on the wrong path. */
static int set_dir(ava1_jnl_t *j, const char *dir) {
    int w = snprintf(j->dir, sizeof j->dir, "%s", dir);
    if (w < 0 || (size_t)w >= sizeof j->dir) return -ENAMETOOLONG;
    return 0;
}

/* tmp → fsync → rename → fsync(dir). The caller owns same-directory placement. */
static int write_file_atomic(const char *dir, const char *name, const uint8_t *a, size_t an,
                             const uint8_t *b, size_t bn) {
    char tmp[600], fin[600];
    int fd, rc;
    snprintf(tmp, sizeof tmp, "%s/%s.tmp", dir, name);
    path_in(dir, name, fin, sizeof fin);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -errno;
    rc = write_all_fd(fd, a, an);
    if (rc == 0 && bn) rc = write_all_fd(fd, b, bn);
    if (rc == 0 && (rc = ava1_fsync_retry(fd, NULL, NULL, NULL)) != 0) rc = -rc;
    close(fd);
    if (rc == 0 && rename(tmp, fin) != 0) rc = -errno; /* same directory */
    if (rc == 0) rc = sync_dir(dir);
    return rc;
}

int ava1_jnl_create(ava1_jnl_t *j, const char *dir, const ava1_jnl_open_t *o) {
    uint8_t body[1200];
    ava1_w_t w;
    uint8_t *rec;
    size_t rn;
    int rc;
    char p[600];
    memset(j, 0, sizeof *j);
    j->fd = -1;
    rc = set_dir(j, dir);
    if (rc != 0) return rc;
    if (mkdir(dir, 0755) != 0 && errno != EEXIST) return -errno; /* parent must exist */
    ava1_w_init(&w, body, sizeof body);
    if (ava1_jnl_open_encode(o, &w) != 0) return AVA1_E_SPACE;
    rec = frame_rec(AVA1_JNL_OPEN, body, w.len, &rn);
    if (!rec) return -ENOMEM;
    rc = write_file_atomic(dir, "journal", MAGIC, sizeof MAGIC, rec, rn);
    free(rec);
    if (rc != 0) return rc;
    path_in(dir, "journal", p, sizeof p);
    j->fd = open(p, O_WRONLY | O_APPEND);
    if (j->fd < 0) return -errno;
    j->len = sizeof MAGIC + rn;
    return 0;
}

int ava1_jnl_open(ava1_jnl_t *j, const char *dir, ava1_jnl_visit_fn visit, void *ctx) {
    char p[600];
    struct stat st;
    uint8_t *b;
    size_t at = sizeof MAGIC, n;
    int fd;
    memset(j, 0, sizeof *j);
    j->fd = -1;
    {
        int rc = set_dir(j, dir);
        if (rc != 0) return rc;
    }
    path_in(dir, "journal", p, sizeof p);
    fd = open(p, O_RDWR);
    if (fd < 0) return -errno;
    if (fstat(fd, &st) != 0) {
        int e = errno;
        close(fd);
        return -e;
    }
    n = (size_t)st.st_size;
    b = malloc(n ? n : 1);
    if (!b) {
        close(fd);
        return -ENOMEM;
    }
    if (pread(fd, b, n, 0) != (ssize_t)n || n < sizeof MAGIC ||
        memcmp(b, MAGIC, sizeof MAGIC) != 0) {
        free(b);
        close(fd);
        return AVA1_E_PROTO;
    }
    while (n - at >= 8) {
        uint32_t len = get32(b + at);
        /* A record is 8 + len bytes. `n - at >= 8` makes the subtraction safe, and
         * comparing against the remaining bytes (rather than len + 8, which wraps on a
         * 32-bit host) keeps a hostile length from walking off the buffer. */
        if (len == 0 || len > n - at - 8) break;
        if (ava1_crc32c(b + at + 4, len) != get32(b + at + 4 + len)) break;
        if (visit && visit(ctx, b[at + 4], b + at + 5, len - 1) != 0) break;
        at += 8 + len;
    }
    free(b);
    if (ftruncate(fd, (off_t)at) != 0 || fsync(fd) != 0) {
        int e = errno;
        close(fd);
        return -e;
    }
    close(fd);
    j->fd = open(p, O_WRONLY | O_APPEND);
    if (j->fd < 0) return -errno;
    j->len = at;
    return 0;
}

int ava1_jnl_peek_open(const char *dir, uint8_t *buf, size_t cap, ava1_jnl_open_t *o) {
    char p[600];
    ssize_t n;
    uint32_t len;
    int fd;
    path_in(dir, "journal", p, sizeof p);
    if ((fd = open(p, O_RDONLY)) < 0) return -1;
    n = pread(fd, buf, cap, 0);
    close(fd);
    if (n < (ssize_t)sizeof MAGIC + 8 || memcmp(buf, MAGIC, sizeof MAGIC) != 0) return -1;
    len = get32(buf + sizeof MAGIC);
    if (len < 2 || len > (size_t)n - sizeof MAGIC - 8) return -1;
    if (ava1_crc32c(buf + sizeof MAGIC + 4, len) != get32(buf + sizeof MAGIC + 4 + len)) return -1;
    if (buf[sizeof MAGIC + 4] != AVA1_JNL_OPEN) return -1;
    return ava1_jnl_open_decode(buf + sizeof MAGIC + 5, len - 1, o) == 0 ? 0 : -1;
}

int ava1_jnl_append(ava1_jnl_t *j, uint8_t kind, const uint8_t *body, size_t len) {
    size_t rn;
    uint8_t *rec = frame_rec(kind, body, len, &rn);
    int rc;
    if (!rec) return -ENOMEM;
    rc = write_all_fd(j->fd, rec, rn);
    free(rec);
    /* The record is written; only its fsync is retried (appending it again would duplicate it). */
    if (rc == 0 && (rc = ava1_fsync_retry(j->fd, NULL, NULL, NULL)) != 0) rc = -rc;
    if (rc == 0) j->len += rn;
    return rc;
}

/* The snapshot carries done/ranges/roots but not the job's terminal status, so the
 * Done body is written back after it (NULL when the job is unfinished) — a compaction
 * must never lose state (SPEC.md §14.2). */
int ava1_jnl_compact(ava1_jnl_t *j, const uint8_t *open_body, size_t open_len,
                     const uint8_t *snap_body, size_t snap_len,
                     const uint8_t *done_body, size_t done_len) {
    size_t an, bn, dn = 0;
    uint8_t *a = frame_rec(AVA1_JNL_OPEN, open_body, open_len, &an);
    uint8_t *b = frame_rec(AVA1_JNL_SNAPSHOT, snap_body, snap_len, &bn);
    uint8_t *d = done_body ? frame_rec(AVA1_JNL_DONE, done_body, done_len, &dn) : NULL;
    size_t total = sizeof MAGIC + an + bn + dn;
    uint8_t *all = (a && b && (d || !done_body)) ? malloc(total) : NULL;
    char p[600];
    int rc = -ENOMEM;
    if (all) {
        memcpy(all, MAGIC, sizeof MAGIC);
        memcpy(all + sizeof MAGIC, a, an);
        memcpy(all + sizeof MAGIC + an, b, bn);
        if (d) memcpy(all + sizeof MAGIC + an + bn, d, dn);
        rc = write_file_atomic(j->dir, "journal", all, total, NULL, 0);
    }
    if (rc == 0) {
        close(j->fd);
        path_in(j->dir, "journal", p, sizeof p);
        j->fd = open(p, O_WRONLY | O_APPEND);
        if (j->fd < 0) rc = -errno;
        else j->len = total;
    }
    free(a);
    free(b);
    free(d);
    free(all);
    return rc;
}

void ava1_jnl_close(ava1_jnl_t *j) {
    if (j->fd >= 0) close(j->fd);
    j->fd = -1;
}

int ava1_manifest_file_write(const char *dir, const uint8_t *blob, size_t len) {
    return write_file_atomic(dir, "manifest", blob, len, NULL, 0);
}

int ava1_manifest_file_read(const char *dir, uint8_t **blob, size_t *len) {
    char p[600];
    struct stat st;
    int fd;
    path_in(dir, "manifest", p, sizeof p);
    fd = open(p, O_RDONLY);
    if (fd < 0) return -errno;
    if (fstat(fd, &st) != 0) {
        int e = errno;
        close(fd);
        return -e;
    }
    *len = (size_t)st.st_size;
    *blob = malloc(*len ? *len : 1);
    if (!*blob || pread(fd, *blob, *len, 0) != (ssize_t)*len) {
        free(*blob);
        *blob = NULL;
        close(fd);
        return AVA1_E_IO;
    }
    close(fd);
    return 0;
}

void ava1_job_dir(const char *jobs_dir, const uint8_t job_id[16], char *out, size_t cap) {
    static const char H[] = "0123456789abcdef";
    char hex[33];
    int i;
    for (i = 0; i < 16; i++) {
        hex[2 * i] = H[job_id[i] >> 4];
        hex[2 * i + 1] = H[job_id[i] & 15];
    }
    hex[32] = 0;
    snprintf(out, cap, "%s/%s", jobs_dir, hex);
}

static int rm_tree(const char *p) {
    DIR *d;
    struct dirent *e;
    struct stat st;
    char q[1100];
    /* lstat, not stat: a symlink is removed, never followed — Rust's remove_dir_all has
     * the same rule, and following one here would delete a tree outside the job dir. */
    if (lstat(p, &st) != 0) return -errno;
    if (!S_ISDIR(st.st_mode)) return unlink(p) == 0 ? 0 : -errno;
    d = opendir(p);
    if (!d) return -errno;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        snprintf(q, sizeof q, "%s/%s", p, e->d_name);
        (void)rm_tree(q);
    }
    closedir(d);
    return rmdir(p) == 0 ? 0 : -errno;
}

/* True when the job directory holds a pack log (a file named pack.<n>): the only copy of files not yet durable in
 * place, so recovery must reach it before anything deletes it. */
int ava1_dir_has_pack(const char *dir) {
    DIR *dp = opendir(dir);
    struct dirent *de;
    int has = 0;
    while (dp && !has && (de = readdir(dp)) != NULL) has = strncmp(de->d_name, "pack.", 5) == 0;
    if (dp) closedir(dp);
    return has;
}

#define AVA1_PACK_GC_GRACE_S (7 * 86400)
/* A clock before this is a clock that was reset, not the time (the payload did not exist before 2024). */
#define AVA1_GC_MIN_CLOCK 1704067200
/* How many starts must each see a log past its ceiling before it is given up on. */
#define AVA1_GC_STRIKES 3

uint64_t ava1_gc_test_boot_id; /* tests: nonzero replaces the real boot identity */

/* Identifies this boot: kern.boottime seconds (the PS5 kernel moves it with a clock set, so it is only half the
 * guard; the 24 h spacing below is the other half). */
static uint64_t gc_boot_id(void) {
    uint64_t t = __atomic_load_n(&ava1_gc_test_boot_id, __ATOMIC_SEQ_CST);
    if (t) return t;
#if defined(__APPLE__) || defined(__FreeBSD__)
    {
        struct timeval bt;
        size_t len = sizeof bt;
        int mib[2] = {CTL_KERN, KERN_BOOTTIME};
        if (sysctl(mib, 2, &bt, &len, NULL, 0) == 0 && bt.tv_sec > 0) return (uint64_t)bt.tv_sec;
    }
#else
    {
        FILE *f = fopen("/proc/sys/kernel/random/boot_id", "r");
        char b[64];
        uint64_t h = 1469598103934665603ull;
        size_t i, n = 0;
        if (f) {
            n = fread(b, 1, sizeof b, f);
            fclose(f);
        }
        for (i = 0; i < n; i++) h = (h ^ (uint8_t)b[i]) * 1099511628211ull;
        if (n) return h | 1;
    }
#endif
    return 0;
}

#define AVA1_GC_STRIKE_SPACING_S 86400

/* A start has seen `dir` (a job directory with a log) past its ceiling. A strike counts only when it is from
 * another boot than the previous strike AND at least 24 h after it (by the wall stamp stored with it), so a
 * helper re-sent three times in a row, or a clock set wrong, cannot add up to a deletion. <dir>/gc.strikes
 * holds "count boot stamp". Returns the count (0 when it cannot count: never give up). The directory's mtime is
 * put back, since it is the age being counted. */
static int gc_strike(const char *dir, const struct stat *st, int64_t now_unix) {
    char fp[760], buf[96];
    int fd, n = 0;
    unsigned long long pboot = 0;
    long long pstamp = 0;
    uint64_t boot = gc_boot_id();
    ssize_t k;
    if (!boot) return 0;
    snprintf(fp, sizeof fp, "%s/gc.strikes", dir);
    fd = open(fp, O_RDWR | O_CREAT, 0600);
    if (fd < 0) return 0;
    k = read(fd, buf, sizeof buf - 1);
    if (k > 0) {
        buf[k] = '\0';
        if (sscanf(buf, "%d %llu %lld", &n, &pboot, &pstamp) != 3 || n < 0 || n > 1000) n = 0, pboot = 0, pstamp = 0;
    }
    if (n == 0 || (pboot != boot && now_unix - pstamp >= AVA1_GC_STRIKE_SPACING_S)) {
        n++;
        k = snprintf(buf, sizeof buf, "%d %llu %lld", n, (unsigned long long)boot, (long long)now_unix);
        if (lseek(fd, 0, SEEK_SET) == 0 && ftruncate(fd, 0) == 0) (void)write(fd, buf, (size_t)k);
    }
    close(fd);
    {
        struct timeval tv[2];
        tv[0].tv_sec = tv[1].tv_sec = st->st_mtime;
        tv[0].tv_usec = tv[1].tv_usec = 0;
        (void)utimes(dir, tv);
    }
    return n;
}

/* Collects job directories idle for more than max_age_s by `now_unix`, a WALL clock (file mtimes are wall
 * time). The wall clock is the user's to set, so (final review: console):
 *   - before 2024 it is a reset clock: nothing is collected;
 *   - a job stamped in the future (clock moved back) is skipped, and only it;
 *   - a directory with a pack log (the only copy of files not yet durable in place) is never collected on
 *     that clock alone: a clock moved forward would age every log at once. It is given up on only when
 *     AVA1_GC_STRIKES strikes, each from a different boot and 24 h after the last (re-sends within a boot add
 *     none), and it says so. */
int ava1_jobs_gc(const char *jobs_dir, int64_t now_unix, int64_t max_age_s) {
    DIR *d = opendir(jobs_dir);
    struct dirent *e;
    int n = 0;
    if (!d) return errno == ENOENT ? 0 : -errno;
    if (now_unix < AVA1_GC_MIN_CLOCK) {
        fprintf(stderr, "[ava1] gc: the clock reads before 2024; collecting nothing\n");
        closedir(d);
        return 0;
    }
    while ((e = readdir(d)) != NULL) {
        char p[700], jp[720];
        struct stat st, js;
        int64_t last;
        if (e->d_name[0] == '.') continue;
        snprintf(p, sizeof p, "%s/%s", jobs_dir, e->d_name);
        if (stat(p, &st) != 0 || !S_ISDIR(st.st_mode)) continue;
        last = (int64_t)st.st_mtime;
        snprintf(jp, sizeof jp, "%s/journal", p);
        if (stat(jp, &js) == 0 && (int64_t)js.st_mtime > last) last = (int64_t)js.st_mtime;
        if (last > now_unix + 60) { /* the clock was moved back past this one: skip it, only it */
            fprintf(stderr, "[ava1] gc: %s is stamped in the future; leaving it\n", e->d_name);
            continue;
        }
        if (now_unix - last > max_age_s) {
            int packed = ava1_dir_has_pack(p);
            if (packed) {
                /* a week past the normal age, and only after several starts agreed */
                if (now_unix - last <= max_age_s + AVA1_PACK_GC_GRACE_S) continue;
                if (gc_strike(p, &st, now_unix) < AVA1_GC_STRIKES) continue;
                fprintf(stderr, "[ava1] gc: giving up on %s: its log was never recovered\n", e->d_name);
            }
            if (rm_tree(p) == 0) n++;
        }
    }
    closedir(d);
    return n;
}
