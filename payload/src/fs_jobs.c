/* Filesystem operations as AVA1 jobs: see include/fs_jobs.h. */
#include "fs_jobs.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_gen.h"
#include "ava1_op.h"
#include "blake3.h"
#include "path_policy.h"

#define MAX_DEPTH 64

uint32_t fsj_test_file_delay_us;
const char *fsj_test_cross_name;

/* The device of an entry, with the test hook's "this one is a mount" applied. */
static dev_t dev_of(const struct stat *st, const char *path) {
    const char *name = fsj_test_cross_name;
    if (name) {
        const char *base = strrchr(path, '/');
        base = base ? base + 1 : path;
        if (strcmp(base, name) == 0) return st->st_dev + 1;
    }
    return st->st_dev;
}

static void failed(const fsj_hooks_t *h, const char *path, const char *what) {
    if (h && h->failed) h->failed(h->arg, path, what);
}

static void test_delay(void) {
    uint32_t us = __atomic_load_n(&fsj_test_file_delay_us, __ATOMIC_RELAXED);
    if (us) usleep(us);
}

static int cancelled(const fsj_hooks_t *h) { return h && h->cancelled && h->cancelled(h->arg); }

static void visited(const fsj_hooks_t *h, uint64_t bytes) {
    test_delay();
    if (h && h->file) h->file(h->arg, bytes);
}

/* ---- request bodies ---- */

/* The value of top-level `key` in a JSON object: a pointer just past the colon and spaces,
 * or NULL. Strings are skipped whole, so a key-like text inside a value never matches. */
static const char *find_value(const char *j, const char *key) {
    size_t klen = strlen(key);
    int depth = 0;
    const char *p = j;
    while (*p) {
        if (*p == '"') {
            const char *s = ++p;
            while (*p && *p != '"') p += (*p == '\\' && p[1]) ? 2 : 1;
            if (!*p) return NULL;
            if (depth == 1 && (size_t)(p - s) == klen && memcmp(s, key, klen) == 0) {
                const char *q = p + 1;
                while (*q == ' ' || *q == '\t' || *q == '\n' || *q == '\r') q++;
                if (*q == ':') {
                    q++;
                    while (*q == ' ' || *q == '\t' || *q == '\n' || *q == '\r') q++;
                    return q;
                }
            }
            p++;
            continue;
        }
        if (*p == '{' || *p == '[') depth++;
        else if (*p == '}' || *p == ']') depth--;
        p++;
    }
    return NULL;
}

int fsj_json_str(const char *json, const char *key, char *out, size_t cap) {
    const char *p = json ? find_value(json, key) : NULL;
    size_t n = 0;
    if (cap) out[0] = '\0';
    if (!p || *p != '"' || !cap) return 0;
    for (p++; *p && *p != '"'; p++) {
        unsigned char c = (unsigned char)*p;
        if (c == '\\' && p[1]) {
            p++;
            switch (*p) {
            case 'n': c = '\n'; break;
            case 't': c = '\t'; break;
            case 'r': c = '\r'; break;
            case 'b': c = '\b'; break;
            case 'f': c = '\f'; break;
            case 'u': {
                unsigned v = 0;
                int k;
                for (k = 1; k <= 4; k++) {
                    char h = p[k];
                    if (h >= '0' && h <= '9') v = v * 16 + (unsigned)(h - '0');
                    else if (h >= 'a' && h <= 'f') v = v * 16 + (unsigned)(h - 'a' + 10);
                    else if (h >= 'A' && h <= 'F') v = v * 16 + (unsigned)(h - 'A' + 10);
                    else return 0;
                }
                p += 4;
                if (v == 0) return 0; /* a NUL would silently cut the path short: refuse the request */
                /* UTF-8 (surrogate pairs are not joined: a path with one is refused as unusable). */
                if (v >= 0xd800 && v < 0xe000) return 0;
                if (v < 0x80) {
                    c = (unsigned char)v;
                } else if (v < 0x800) {
                    if (n + 2 >= cap) return 0;
                    out[n++] = (char)(0xc0 | (v >> 6));
                    c = (unsigned char)(0x80 | (v & 0x3f));
                } else {
                    if (n + 3 >= cap) return 0;
                    out[n++] = (char)(0xe0 | (v >> 12));
                    out[n++] = (char)(0x80 | ((v >> 6) & 0x3f));
                    c = (unsigned char)(0x80 | (v & 0x3f));
                }
                break;
            }
            default: c = (unsigned char)*p; break; /* \" \\ \/ */
            }
        }
        if (n + 1 >= cap) return 0; /* too long: refuse rather than cut a path short */
        out[n++] = (char)c;
    }
    if (*p != '"') return 0;
    out[n] = '\0';
    return 1;
}

int fsj_json_u64(const char *json, const char *key, uint64_t *out) {
    const char *p = json ? find_value(json, key) : NULL;
    uint64_t v = 0;
    int digits = 0;
    if (!p) return 0;
    if (strncmp(p, "true", 4) == 0) {
        *out = 1;
        return 1;
    }
    if (strncmp(p, "false", 5) == 0) {
        *out = 0;
        return 1;
    }
    for (; *p >= '0' && *p <= '9'; p++, digits++) {
        if (v > (UINT64_MAX - 9) / 10) return 0;
        v = v * 10 + (uint64_t)(*p - '0');
    }
    if (!digits) return 0;
    *out = v;
    return 1;
}

/* ---- walkers (moved from runtime.c: rm_rf_op, chmod_rf, recursive_size_inner) ---- */

/* Force-flush the parent directory's metadata after a delete. exFAT (USB) caches directory
 * entries and the allocation bitmap, so freed clusters can stay "used" for minutes after an
 * unlink; fsync of the directory forces the update. Best effort. */
static void fsync_parent_dir(const char *path) {
    char parent[1024];
    const char *slash = strrchr(path, '/');
    size_t plen;
    int dfd;
    if (!slash) return;
    plen = slash == path ? 1 : (size_t)(slash - path);
    if (plen >= sizeof parent) plen = sizeof parent - 1;
    memcpy(parent, path, plen);
    parent[plen] = '\0';
    dfd = open(parent, O_RDONLY);
    if (dfd >= 0) {
        (void)fsync(dfd);
        close(dfd);
    }
}

static int rm_rf_in(const char *path, int depth, const fsj_hooks_t *h, dev_t root_dev) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0, child_failed = 0;

    if (depth > MAX_DEPTH) return -1;
    if (cancelled(h)) return -2;
    if (lstat(path, &st) != 0) {
        /* Already gone is success: a concurrent sweep, or the caller's own earlier delete, may
         * have removed it. */
        if (errno == ENOENT) return 0;
        failed(h, path, "lstat");
        return -1;
    }
    /* Never cross into another device than the root's: a mount below the tree is not ours to empty. */
    if (dev_of(&st, path) != root_dev) {
        failed(h, path, "mount");
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        if (unlink(path) != 0) {
            if (errno == ENOENT) return 0;
            /* EBUSY: Sony's async installer can still hold a just-installed .pkg open when the
             * auto-delete fires. Retry for at most a second before failing the click. */
            if (errno == EBUSY) {
                int freed = 0, i;
                for (i = 0; i < 10; i++) {
                    usleep(100000);
                    if (unlink(path) == 0 || errno == ENOENT) {
                        freed = 1;
                        break;
                    }
                    if (errno != EBUSY) break;
                }
                if (!freed) {
                    failed(h, path, "unlink");
                    return -1;
                }
            } else {
                failed(h, path, "unlink");
                return -1;
            }
        }
        visited(h, S_ISREG(st.st_mode) ? (uint64_t)st.st_size : 0);
        if (st.st_size >= (off_t)(100 * 1024 * 1024)) fsync_parent_dir(path);
        return 0;
    }
    d = opendir(path);
    if (!d) {
        failed(h, path, "opendir");
        return -1;
    }
    while ((e = readdir(d)) != NULL) {
        int n, sub_rc;
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (cancelled(h)) {
            rc = -2;
            break;
        }
        n = snprintf(sub, sizeof sub, "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof sub) {
            failed(h, path, "path too long");
            rc = -1;
            child_failed = 1;
            break;
        }
        sub_rc = rm_rf_in(sub, depth + 1, h, root_dev);
        if (sub_rc == -2) {
            rc = -2;
            break;
        }
        if (sub_rc != 0) {
            rc = -1; /* keep going; best effort */
            child_failed = 1;
        }
    }
    closedir(d);
    /* A cancelled delete leaves the directory the user clicked Stop on, so they can see what
     * is left. ENOENT on rmdir means it is already gone: success. */
    if (rc != -2 && rmdir(path) != 0 && errno != ENOENT) {
        if (!child_failed) failed(h, path, "rmdir"); /* a folder kept only by a failed child is not a new failure */
        rc = -1;
    }
    if (rc == 0) fsync_parent_dir(path);
    return rc;
}

int fsj_rm_rf(const char *path, int depth, const fsj_hooks_t *h) {
    struct stat st;
    if (lstat(path, &st) != 0) return errno == ENOENT ? 0 : -1;
    return rm_rf_in(path, depth, h, dev_of(&st, path));
}

static int chmod_rf_in(const char *path, unsigned mode, int depth, const fsj_hooks_t *h, dev_t root_dev) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    char sub[1024];
    int rc = 0;

    if (depth > MAX_DEPTH) return -1;
    if (cancelled(h)) return -2;
    if (lstat(path, &st) != 0) {
        failed(h, path, "lstat");
        return -1;
    }
    if (dev_of(&st, path) != root_dev) {
        failed(h, path, "mount");
        return -1;
    }
    /* A link that leads into the trust store is not chmod'ed (chmod follows it). */
    if (S_ISLNK(st.st_mode) && path_in_protected(path)) return 0;
    /* chmod first, then descend: a partial failure still updated the top. */
    if (chmod(path, (mode_t)mode) != 0) {
        failed(h, path, "chmod");
        rc = -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        visited(h, 0); /* progress counts what the size walk counted: non-directories */
        return rc;
    }
    d = opendir(path);
    if (!d) {
        failed(h, path, "opendir");
        return -1;
    }
    while ((e = readdir(d)) != NULL) {
        int n, sub_rc;
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        n = snprintf(sub, sizeof sub, "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof sub) {
            failed(h, path, "path too long");
            rc = -1;
            break;
        }
        sub_rc = chmod_rf_in(sub, mode, depth + 1, h, root_dev);
        if (sub_rc == -2) {
            rc = -2;
            break;
        }
        if (sub_rc != 0) rc = -1;
    }
    closedir(d);
    return rc;
}

int fsj_chmod_rf(const char *path, unsigned mode, int depth, const fsj_hooks_t *h) {
    struct stat st;
    if (lstat(path, &st) != 0) return -1;
    return chmod_rf_in(path, mode, depth, h, dev_of(&st, path));
}

static int tree_size_in(const char *path, uint64_t *bytes, uint64_t *files, int depth, const fsj_hooks_t *h,
                        dev_t root_dev) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    int rc = 0;
    if (depth > MAX_DEPTH) return -1;
    if (cancelled(h)) return -2;
    if (lstat(path, &st) != 0) return -1;
    /* A nested mount is not counted (the removal and the chmod skip it and report it). */
    if (dev_of(&st, path) != root_dev) return 0;
    if (!S_ISDIR(st.st_mode)) {
        if (S_ISREG(st.st_mode)) *bytes += (uint64_t)st.st_size;
        if (files) (*files)++;
        return 0;
    }
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        char sub[1024];
        int sub_rc;
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        /* A truncated child path means the work will hit the same limit; report it now rather
         * than let the totals lie. */
        if (snprintf(sub, sizeof sub, "%s/%s", path, e->d_name) >= (int)sizeof sub) {
            rc = -1;
            break;
        }
        sub_rc = tree_size_in(sub, bytes, files, depth + 1, h, root_dev);
        if (sub_rc != 0) {
            rc = sub_rc;
            break;
        }
    }
    closedir(d);
    return rc;
}

int fsj_tree_size(const char *path, uint64_t *bytes, uint64_t *files, int depth, const fsj_hooks_t *h) {
    struct stat st;
    if (lstat(path, &st) != 0) return -1;
    return tree_size_in(path, bytes, files, depth, h, dev_of(&st, path));
}

/* ---- paths ---- */

int fsj_normalize_path(char *path) {
    char *out = path, *p = path;
    if (path[0] != '/') return -1;
    while (*p) {
        char *start;
        size_t n;
        while (*p == '/') p++;
        if (!*p) break;
        start = p;
        while (*p && *p != '/') p++;
        n = (size_t)(p - start);
        if (n == 1 && start[0] == '.') continue;
        if (n == 2 && start[0] == '.' && start[1] == '.') return -1;
        *out++ = '/';
        memmove(out, start, n); /* out never passes start: components only shrink */
        out += n;
    }
    if (out == path) *out++ = '/';
    *out = '\0';
    return 0;
}

int fsj_path_is_root(const char *path) {
    int comps = 0;
    const char *p = path;
    int mnt = strncmp(path, "/mnt", 4) == 0 && (path[4] == '/' || path[4] == '\0');
    while (*p) {
        while (*p == '/') p++;
        if (!*p) break;
        comps++;
        while (*p && *p != '/') p++;
    }
    return mnt ? comps < 3 : comps < 2;
}

int fsj_copy_atomic(const char *src, const char *dst, const fsj_hooks_t *h) {
    static unsigned g_seq;
    enum { BLK = 64 * 1024 };
    char tmp[1100];
    struct stat st;
    struct timespec times[2];
    char *buf;
    int sfd, dfd, rc = 0;
    ssize_t n;
    unsigned seq = __atomic_add_fetch(&g_seq, 1, __ATOMIC_RELAXED);
    struct timespec now;
    struct stat dst_st;
    (void)clock_gettime(CLOCK_MONOTONIC, &now);
    /* Next to dst: the final rename stays in one folder, so it can never cross a device. The
     * suffix (pid, clock, counter) keeps a stale temp left by a crash from ever colliding. */
    if (snprintf(tmp, sizeof tmp, "%s.part%d.%llx.%u", dst, (int)getpid(),
                 (unsigned long long)now.tv_sec * 1000000000ull + (unsigned long long)now.tv_nsec, seq) >= (int)sizeof tmp)
        return -1;
    sfd = open(src, O_RDONLY);
    if (sfd < 0) return -1;
    if (fstat(sfd, &st) != 0) {
        close(sfd);
        return -1;
    }
    dfd = open(tmp, O_WRONLY | O_CREAT | O_EXCL, 0644);
    buf = malloc(BLK);
    if (dfd < 0 || !buf) {
        if (dfd >= 0) {
            close(dfd);
            unlink(tmp);
        }
        close(sfd);
        free(buf);
        return -1;
    }
    for (;;) {
        ssize_t off = 0;
        n = read(sfd, buf, BLK);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) {
            rc = -1;
            break;
        }
        if (n == 0) break;
        if (cancelled(h)) {
            rc = -2;
            break;
        }
        while (off < n) {
            ssize_t w = write(dfd, buf + off, (size_t)(n - off));
            if (w <= 0) {
                rc = -1;
                break;
            }
            off += w;
        }
        if (rc) break;
        if (h && h->file) h->file(h->arg, (uint64_t)n);
        test_delay();
    }
    free(buf);
    close(sfd);
    if (rc == 0 && fsync(dfd) != 0) rc = -1;
    if (close(dfd) != 0 && rc == 0) rc = -1;
    if (rc == 0) {
        /* The temp file was created 0644: give it the source's mode (a 0755 file stays 0755);
         * a source with no permission bits falls back to the destination's current mode. */
        mode_t m = st.st_mode & 07777;
        if (m == 0 && stat(dst, &dst_st) == 0) m = dst_st.st_mode & 07777;
        if (m != 0 && chmod(tmp, m) != 0) rc = -1;
    }
    if (rc == 0) {
        times[0].tv_sec = st.st_atime;
        times[0].tv_nsec = 0;
        times[1].tv_sec = st.st_mtime;
        times[1].tv_nsec = 0;
        (void)utimensat(AT_FDCWD, tmp, times, 0);
        if (rename(tmp, dst) != 0) rc = -1; /* same folder: never a cross-device rename */
    }
    if (rc != 0) unlink(tmp); /* only the temporary file; dst was never touched */
    return rc;
}

/* ---- the operations ---- */

static int may_write(const char *p) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    /* Review S2: a root that IS the AVA1 trust store, is below it, or is an ANCESTOR of it (/data/ps5upload,
     * /data, /) is refused for every walking/destructive op: the walk would reach identity and peers. */
    return cfg->may_write && cfg->may_write(p) && !path_tree_op_refused(p);
}

/* "/a/b/c" -> "/a/b"; "/c" -> "/". */
static void parent_of(const char *path, char *out, size_t cap) {
    const char *slash = strrchr(path, '/');
    size_t n = (slash && slash != path) ? (size_t)(slash - path) : 1;
    if (n >= cap) n = cap - 1;
    if (!slash || slash == path) {
        out[0] = '/';
        out[1] = '\0';
        return;
    }
    memcpy(out, path, n);
    out[n] = '\0';
}

/* Collects what a walk could not do: the first path and how many entries. */
typedef struct {
    ava1_op_ctx_t *c;
    char first[600];
    char what[16];
    uint64_t n;
} fail_t;

static int fail_cancelled(void *arg) { return ava1_op_cancelled(((fail_t *)arg)->c); }
static void fail_file(void *arg, uint64_t bytes) { ava1_op_add(((fail_t *)arg)->c, 1, bytes); }
static void fail_note(void *arg, const char *path, const char *what) {
    fail_t *f = arg;
    if (f->n++ == 0) {
        snprintf(f->first, sizeof f->first, "%s", path);
        snprintf(f->what, sizeof f->what, "%s", what);
    }
}

/* The cause of a partly failed walk: `<token>: <what> <first path> (<n> left)`, cut to the
 * status line, keeping the END of a long path (the part that tells folders apart). */
static void fail_message(const fail_t *f, const char *token, const char *unit) {
    char shown[110];
    size_t l = strlen(f->first), keep = sizeof shown - 1;
    if (l > keep) {
        memcpy(shown, "...", 3);
        memcpy(shown + 3, f->first + l - (keep - 3), keep - 3);
        shown[keep] = '\0';
    } else {
        memcpy(shown, f->first, l + 1);
    }
    ava1_op_message((ava1_op_ctx_t *)f->c, "%s: %s %s (%llu %s)", token, f->what, shown, (unsigned long long)f->n, unit);
}

/* DELETE {"path"}. The path is normalised first (trailing slashes, `//`, `/./` all spell the
 * same folder, and the guards below must see that folder, not its spelling). It refuses a root
 * or a top folder (`/data`, `/mnt/usb0`), anything outside the writable roots, and a mount point:
 * a path on another device than its parent, or a device that cannot be told (fail closed). The
 * tree is counted first so progress has a total, then removed; the walk never enters a nested
 * mount and a partial failure names its first path and how many entries are left. */
static int op_delete(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char path[1024], parent[1024];
    uint64_t bytes = 0, files = 0;
    struct stat st;
    fail_t f;
    fsj_hooks_t h, walk;
    int rc, dev;
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    (void)arg;
    (void)n;
    memset(&f, 0, sizeof f);
    f.c = c;
    h.cancelled = fail_cancelled;
    h.file = fail_file;
    h.failed = fail_note;
    h.arg = &f;
    walk = h;
    walk.file = NULL;
    walk.failed = NULL;
    if (!fsj_json_str((const char *)args, "path", path, sizeof path) || !path[0]) {
        ava1_op_message(c, "fs_delete_missing_path");
        return AVA1_ERR_PROTOCOL;
    }
    if (fsj_normalize_path(path) != 0 || fsj_path_is_root(path) || !may_write(path)) {
        ava1_op_message(c, "fs_delete_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    parent_of(path, parent, sizeof parent);
    dev = cfg->same_device ? cfg->same_device(path, parent) : -1;
    if (dev != 1) { /* 0: a mount point; -1: cannot tell, which is not "safe" */
        ava1_op_message(c, dev == 0 ? "fs_delete_path_is_mount_point" : "fs_delete_device_unknown");
        return AVA1_ERR_PATH;
    }
    if (lstat(path, &st) != 0 && errno == ENOENT) return AVA1_STATUS_OK; /* nothing to do */
    ava1_op_message(c, "counting");
    rc = fsj_tree_size(path, &bytes, &files, 0, &walk);
    if (rc == -2) {
        ava1_op_message(c, "fs_delete_cancelled");
        return AVA1_ERR_CANCELLED;
    }
    if (rc != 0) {
        ava1_op_message(c, "fs_delete_walk_failed");
        return AVA1_ERR_IO;
    }
    ava1_op_set_total(c, files, bytes);
    ava1_op_message(c, "deleting");
    rc = fsj_rm_rf(path, 0, &h);
    if (rc == -2) {
        ava1_op_message(c, "fs_delete_cancelled");
        return AVA1_ERR_CANCELLED;
    }
    if (rc != 0) {
        if (f.n) fail_message(&f, "fs_delete_failed", "left");
        else ava1_op_message(c, "fs_delete_failed");
        return AVA1_ERR_IO;
    }
    ava1_op_message(c, "%s", "");
    return AVA1_STATUS_OK;
}

/* CHMOD_R {"path","mode":"0755"}: the recursive form of fs.chmod. The path is normalised; the
 * walk stays on the root's device and names its first failure. */
static int op_chmod_r(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char path[1024], mode_s[16];
    uint64_t bytes = 0, files = 0, mode;
    fail_t f;
    fsj_hooks_t h, walk;
    int rc;
    (void)arg;
    (void)n;
    memset(&f, 0, sizeof f);
    f.c = c;
    h.cancelled = fail_cancelled;
    h.file = fail_file;
    h.failed = fail_note;
    h.arg = &f;
    walk = h;
    walk.file = NULL;
    walk.failed = NULL;
    if (!fsj_json_str((const char *)args, "path", path, sizeof path) || !path[0]) {
        ava1_op_message(c, "fs_chmod_missing_path");
        return AVA1_ERR_PROTOCOL;
    }
    if (fsj_normalize_path(path) != 0 || !may_write(path)) {
        ava1_op_message(c, "fs_chmod_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    if (!fsj_json_str((const char *)args, "mode", mode_s, sizeof mode_s) || !mode_s[0]) {
        ava1_op_message(c, "fs_chmod_missing_mode");
        return AVA1_ERR_PROTOCOL;
    }
    mode = strtoull(mode_s, NULL, 8); /* octal text, as the legacy handler read it */
    if (mode > 07777) mode = 07777;
    if ((rc = fsj_tree_size(path, &bytes, &files, 0, &walk)) == 0) ava1_op_set_total(c, files, bytes);
    else if (rc == -2) {
        ava1_op_message(c, "fs_chmod_cancelled");
        return AVA1_ERR_CANCELLED;
    }
    /* A failed count (a link loop, an unreadable folder) is not fatal: the chmod walk is the
     * authority and reports its own failure; the total just stays 0. */
    rc = fsj_chmod_rf(path, (unsigned)mode, 0, &h);
    if (rc == -2) {
        ava1_op_message(c, "fs_chmod_cancelled");
        return AVA1_ERR_CANCELLED;
    }
    if (rc != 0) {
        if (f.n) fail_message(&f, "fs_chmod_failed", "failed");
        else ava1_op_message(c, "fs_chmod_failed");
        return AVA1_ERR_IO;
    }
    return AVA1_STATUS_OK;
}

/* Opens `path` for reading under the read policy. Returns the fd or -1 with the cause
 * and status set. */
static int open_for_read(ava1_op_ctx_t *c, const char *args, const char *tag, char *path, size_t cap, struct stat *st,
                         int *status) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    uint64_t unsafe = 0;
    int fd;
    (void)fsj_json_u64(args, "unsafe", &unsafe);
    if (!fsj_json_str(args, "path", path, cap) || !path[0]) {
        ava1_op_message(c, "%s_bad_path", tag);
        *status = AVA1_ERR_PATH;
        return -1;
    }
    if (path[0] != '/' || !cfg->may_read || !cfg->may_read(path, unsafe != 0) || path_tree_op_refused(path)) {
        ava1_op_message(c, "%s_bad_path", tag);
        *status = AVA1_ERR_PATH;
        return -1;
    }
    if (stat(path, st) != 0) {
        ava1_op_message(c, "%s_stat_failed", tag);
        *status = AVA1_ERR_IO;
        return -1;
    }
    if (!S_ISREG(st->st_mode)) {
        ava1_op_message(c, "%s_not_regular_file", tag);
        *status = AVA1_ERR_PATH;
        return -1;
    }
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        ava1_op_message(c, "%s_open_failed", tag);
        *status = AVA1_ERR_IO;
    }
    return fd;
}

enum { READ_BUF = 64 * 1024 };

/* HASH {"path"}: BLAKE3 of one file, 64 KiB at a time. Result {"path","size","hash"}. */
static int op_hash(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    static const char hexchars[] = "0123456789abcdef";
    char path[1024], esc[2100], hex[BLAKE3_OUT_LEN * 2 + 1], resp[2300];
    struct stat st;
    uint8_t digest[BLAKE3_OUT_LEN];
    unsigned char *buf = malloc(READ_BUF);
    blake3_hasher *hasher = malloc(sizeof *hasher);
    int fd, status = AVA1_STATUS_OK, len;
    size_t i, o = 0;
    (void)arg;
    (void)n;
    if (!buf || !hasher) {
        free(buf);
        free(hasher);
        ava1_op_message(c, "fs_hash_oom");
        return AVA1_ERR_INTERNAL;
    }
    fd = open_for_read(c, (const char *)args, "fs_hash", path, sizeof path, &st, &status);
    if (fd < 0) {
        free(buf);
        free(hasher);
        return status;
    }
    ava1_op_set_total(c, 1, (uint64_t)st.st_size);
    blake3_hasher_init(hasher);
    for (;;) {
        ssize_t r = read(fd, buf, READ_BUF);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) {
            ava1_op_message(c, "fs_hash_read_failed");
            status = AVA1_ERR_IO;
            break;
        }
        if (r == 0) break;
        blake3_hasher_update(hasher, buf, (size_t)r);
        ava1_op_add(c, 0, (uint64_t)r);
        test_delay();
        if (ava1_op_cancelled(c)) {
            ava1_op_message(c, "fs_hash_cancelled");
            status = AVA1_ERR_CANCELLED;
            break;
        }
    }
    close(fd);
    if (status == AVA1_STATUS_OK) {
        blake3_hasher_finalize(hasher, digest, BLAKE3_OUT_LEN);
        for (i = 0; i < BLAKE3_OUT_LEN; i++) {
            hex[2 * i] = hexchars[(digest[i] >> 4) & 0xf];
            hex[2 * i + 1] = hexchars[digest[i] & 0xf];
        }
        hex[BLAKE3_OUT_LEN * 2] = '\0';
        for (i = 0; path[i] && o + 7 < sizeof esc; i++) {
            if (path[i] == '"' || path[i] == '\\') esc[o++] = '\\';
            if ((unsigned char)path[i] < 0x20) {
                o += (size_t)snprintf(esc + o, sizeof esc - o, "\\u%04x", (unsigned)(unsigned char)path[i]);
                continue;
            }
            esc[o++] = path[i];
        }
        esc[o] = '\0';
        len = snprintf(resp, sizeof resp, "{\"path\":\"%s\",\"size\":%llu,\"hash\":\"%s\"}", esc,
                       (unsigned long long)st.st_size, hex);
        if (len < 0 || (size_t)len >= sizeof resp || ava1_op_set_result(c, resp, (size_t)len) != 0) {
            ava1_op_message(c, "fs_hash_reply_too_large");
            status = AVA1_ERR_INTERNAL;
        }
    }
    free(buf);
    free(hasher);
    return status;
}

static uint32_t g_crc_table[256];
static pthread_once_t g_crc_once = PTHREAD_ONCE_INIT;

static void crc_init(void) {
    int i, j;
    for (i = 0; i < 256; i++) {
        uint32_t v = (uint32_t)i;
        for (j = 0; j < 8; j++) v = (v & 1) ? (0xedb88320u ^ (v >> 1)) : (v >> 1);
        g_crc_table[i] = v;
    }
}

/* CRC32 {"path"}: result {"crc32":N,"size":N}. The buffer is on the heap: the handler's
 * 64 KiB stack array is what the stack audit exists to keep out of a management thread. */
static int op_crc32(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char path[1024], resp[64];
    struct stat st;
    unsigned char *buf = malloc(READ_BUF);
    uint32_t crc = 0xffffffffu;
    uint64_t total = 0;
    int fd, status = AVA1_STATUS_OK, len;
    (void)arg;
    (void)n;
    if (!buf) {
        ava1_op_message(c, "crc32_oom");
        return AVA1_ERR_INTERNAL;
    }
    fd = open_for_read(c, (const char *)args, "crc32", path, sizeof path, &st, &status);
    if (fd < 0) {
        free(buf);
        return status;
    }
    pthread_once(&g_crc_once, crc_init);
    ava1_op_set_total(c, 1, (uint64_t)st.st_size);
    for (;;) {
        ssize_t r = read(fd, buf, READ_BUF), i;
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) {
            ava1_op_message(c, "crc32_read_failed");
            status = AVA1_ERR_IO;
            break;
        }
        if (r == 0) break;
        for (i = 0; i < r; i++) crc = g_crc_table[(crc ^ buf[i]) & 0xff] ^ (crc >> 8);
        total += (uint64_t)r;
        ava1_op_add(c, 0, (uint64_t)r);
        test_delay();
        if (ava1_op_cancelled(c)) {
            ava1_op_message(c, "crc32_cancelled");
            status = AVA1_ERR_CANCELLED;
            break;
        }
    }
    close(fd);
    free(buf);
    if (status != AVA1_STATUS_OK) return status;
    len = snprintf(resp, sizeof resp, "{\"crc32\":%u,\"size\":%llu}", crc ^ 0xffffffffu, (unsigned long long)total);
    (void)ava1_op_set_result(c, resp, (size_t)len);
    return AVA1_STATUS_OK;
}

void fsj_register_ops(void) {
    (void)ava1_op_register(AVA1_JOB_OP_DELETE, op_delete, NULL);
    (void)ava1_op_register(AVA1_JOB_OP_CHMOD_R, op_chmod_r, NULL);
    (void)ava1_op_register(AVA1_JOB_OP_HASH, op_hash, NULL);
    (void)ava1_op_register(AVA1_JOB_OP_CRC32, op_crc32, NULL);
}
