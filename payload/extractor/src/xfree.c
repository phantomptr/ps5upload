/* Freeing an archive's space while it is extracted: the schedule and the deletes. See xfree.h. */
#include "xfree.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

void xf_trace(const char *what, const char *name) {
#ifdef __PROSPERO__
    (void)what;
    (void)name;
#else
    const char *path = getenv("XTEST_FREE_TRACE");
    FILE *f;
    if (!path || !*path) return;
    f = fopen(path, "a");
    if (!f) return;
    fprintf(f, "%s %s\n", what, name);
    fclose(f);
#endif
}

unsigned xf_before(const xfvols_t *v, uint64_t at) {
    unsigned k = 0;
    while (k < v->count && v->start[k] + v->size[k] <= at) k++;
    return k;
}

void xf_need(const uint64_t *lo, size_t n, uint64_t *need) {
    uint64_t m = XF_NO_READ;
    for (size_t i = n; i-- > 0;) {
        if (lo[i] < m) m = lo[i];
        need[i] = m;
    }
}

uint64_t xf_peak(const xfvols_t *v, const uint64_t *need, const uint64_t *bytes, size_t n) {
    uint64_t written = 0, peak = 0;
    for (size_t u = 0; u < n; u++) {
        unsigned gone = xf_before(v, need[u]);
        uint64_t freed = 0, at;
        for (unsigned k = 0; k < gone; k++) freed += v->size[k];
        written += bytes[u];
        at = written > freed ? written - freed : 0;
        if (at > peak) peak = at;
    }
    return peak;
}

void xf_free_vols(xfvols_t *v) {
    if (v->path) {
        for (unsigned i = 0; i < v->count; i++) free(v->path[i]);
    }
    free(v->path);
    free(v->size);
    free(v->start);
    memset(v, 0, sizeof *v);
}

int xf_load(xjob_t *j, xfvols_t *v, char *const *paths, unsigned count) {
    memset(v, 0, sizeof *v);
    if (!count) return xj_fail(j, "fs_extract_free_refused: no archive");
    v->path = calloc(count, sizeof *v->path);
    v->size = calloc(count, sizeof *v->size);
    v->start = calloc(count, sizeof *v->start);
    if (!v->path || !v->size || !v->start) {
        xf_free_vols(v);
        return xj_fail(j, "fs_extract_no_memory");
    }
    v->count = count;
    for (unsigned i = 0; i < count; i++) {
        struct stat st;
        char dir[XJ_PATH];
        const char *slash;
        v->path[i] = strdup(paths[i]);
        if (!v->path[i]) {
            xf_free_vols(v);
            return xj_fail(j, "fs_extract_no_memory");
        }
        /* The file itself, never what a link points to, and with no second name that would be
         * damaged with it. */
        if (lstat(paths[i], &st) != 0 || !S_ISREG(st.st_mode)) {
            int err = errno;
            xf_free_vols(v);
            return xj_fail(j, "fs_extract_free_refused: %s is not a plain file%s%s", paths[i], err ? ": " : "",
                           err ? strerror(err) : "");
        }
        if (st.st_nlink > 1) {
            xf_free_vols(v);
            return xj_fail(j, "fs_extract_free_refused: %s has a second name (a hard link)", paths[i]);
        }
        slash = strrchr(paths[i], '/');
        snprintf(dir, sizeof dir, "%.*s", slash ? (int)(slash - paths[i]) : 1, slash ? paths[i] : ".");
        if (!dir[0]) snprintf(dir, sizeof dir, "/");
        if (access(dir, W_OK) != 0) {
            int err = errno;
            xf_free_vols(v);
            return xj_fail(j, "fs_extract_free_refused: its folder can't be written: %s", strerror(err));
        }
        v->size[i] = (uint64_t)st.st_size;
        v->start[i] = v->total;
        v->total += v->size[i];
    }
    j->parts = count;
    j->archive_bytes = v->total;
    return 0;
}

/* The drive `path` is on, or the nearest folder above it that exists. 0 when none can be read. */
static int dev_of(const char *path, dev_t *dev) {
    char p[XJ_PATH];
    struct stat st;
    snprintf(p, sizeof p, "%s", path);
    for (;;) {
        char *slash;
        if (stat(p, &st) == 0) {
            *dev = st.st_dev;
            return 1;
        }
        slash = strrchr(p, '/');
        if (!slash) return 0;
        if (slash == p) {
            if (!p[1]) return 0;
            p[1] = 0;
        } else {
            *slash = 0;
        }
    }
}

int xf_same_drive(const char *archive, const char *dest) {
    dev_t a, d;
    return dev_of(archive, &a) && dev_of(dest, &d) && a == d;
}

static void count_freed(xjob_t *j, const xfvols_t *v, unsigned k) {
    j->parts_freed++;
    j->bytes_freed += v->size[k];
    j->freed_any = 1;
}

/* Said in the status before the first delete: should this process die between a delete and its next
 * status, the helper still knows the folder holds the only copy of what is gone. */
static void about_to_delete(xjob_t *j) {
    if (j->deleting) return;
    j->deleting = 1;
    if (j->progress) j->progress(j, 1);
}

int xf_release(xjob_t *j, xfvols_t *v, unsigned upto) {
    if (upto > v->count) upto = v->count;
    if (upto <= v->freed || v->stuck) return 0;
    /* Everything unpacked from these parts is on the drive before they go. */
    if (xs_barrier(j) != 0) return -1;
    about_to_delete(j);
    while (v->freed < upto) {
        unsigned k = v->freed;
        if (v->drop) v->drop(v->drop_ctx, k);
        if (unlink(v->path[k]) != 0 && errno != ENOENT) {
            fprintf(stderr, "free: could not delete %s (%s); keeping the rest until the end\n", v->path[k],
                    strerror(errno));
            v->stuck = 1;
            break;
        }
        fprintf(stderr, "free: deleted %s (%llu bytes)\n", v->path[k], (unsigned long long)v->size[k]);
        xf_trace("freed", strrchr(v->path[k], '/') ? strrchr(v->path[k], '/') + 1 : v->path[k]);
        count_freed(j, v, k);
        v->freed++;
    }
    if (j->progress) j->progress(j, 1);
    return 0;
}

int xf_finish(xjob_t *j, xfvols_t *v) {
    if (xs_barrier(j) != 0) return -1;
    if (v->freed < v->count) about_to_delete(j);
    for (unsigned k = v->freed; k < v->count; k++) {
        if (v->drop) v->drop(v->drop_ctx, k);
        if (unlink(v->path[k]) != 0 && errno != ENOENT) {
            fprintf(stderr, "free: could not delete %s (%s)\n", v->path[k], strerror(errno));
            j->free_not_removed++;
            continue;
        }
        xf_trace("freed", strrchr(v->path[k], '/') ? strrchr(v->path[k], '/') + 1 : v->path[k]);
        count_freed(j, v, k);
    }
    v->freed = v->count;
    return 0;
}

void xf_abandon(xjob_t *j, xfvols_t *v) {
    if (!j->parts_freed) return;
    for (unsigned k = 0; k < v->count; k++) {
        char to[XJ_PATH + 32];
        struct stat st;
        const char *slash;
        if (lstat(v->path[k], &st) != 0 || !S_ISREG(st.st_mode)) continue;
        snprintf(to, sizeof to, "%s%s", v->path[k], XF_PARTIAL_SUFFIX);
        for (unsigned n = 2; lstat(to, &st) == 0 && n < 1000; n++)
            snprintf(to, sizeof to, "%s%s-%u", v->path[k], XF_PARTIAL_SUFFIX, n);
        /* Same folder, same drive: this rename can't cross a mount. */
        if (rename(v->path[k], to) != 0) snprintf(to, sizeof to, "%s", v->path[k]);
        if (!j->parts_left) {
            slash = strrchr(to, '/');
            snprintf(j->left_first, sizeof j->left_first, "%s", slash ? slash + 1 : to);
        }
        j->parts_left++;
    }
}
