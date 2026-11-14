/* The extractor's output side: safe names, the destination folder, temporary files renamed into
 * place in their own folder, conflicts, free space, progress and cancel. See xjob.h. */
#include "xjob.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/time.h>
#include <unistd.h>

int xj_fail(xjob_t *j, const char *fmt, ...) {
    if (!j->cause[0]) {
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(j->cause, sizeof j->cause, fmt, ap);
        va_end(ap);
    }
    return -1;
}

int xj_failed(const xjob_t *j) { return j->cause[0] != 0; }

int xj_cancelled(xjob_t *j) {
    if (j->cancelled && j->cancelled(j)) {
        xj_fail(j, "fs_extract_cancelled");
        return 1;
    }
    return 0;
}

int xs_safe_name(const char *name, char *out, size_t cap) {
    size_t at = 0;
    const char *p = name;
    if (!name || !cap) return -1;
    /* "/x", "\x" and "C:x" are absolute somewhere; never re-rooted, refused. */
    if (p[0] == '/' || p[0] == '\\') return -1;
    if (((p[0] >= 'a' && p[0] <= 'z') || (p[0] >= 'A' && p[0] <= 'Z')) && p[1] == ':') return -1;
    while (*p) {
        const char *start = p;
        size_t len;
        while (*p && *p != '/' && *p != '\\') p++;
        len = (size_t)(p - start);
        if (*p) p++;
        if (len == 0 || (len == 1 && start[0] == '.')) continue;
        if (len == 2 && start[0] == '.' && start[1] == '.') return -1;
        if (len > 255) return -1;
        if (at + (at ? 1 : 0) + len + 1 > cap) return -1;
        if (at) out[at++] = '/';
        for (size_t i = 0; i < len; i++) {
            unsigned char c = (unsigned char)start[i];
            out[at++] = (c < 0x20 || c == 0x7f) ? '_' : (char)c;
        }
    }
    if (!at) return -2;
    out[at] = 0;
    return 0;
}

static uint64_t free_bytes(xjob_t *j, int *known) {
    struct statvfs sv;
    *known = 1;
    if (j->test_free) return j->test_free > j->written_total ? j->test_free - j->written_total : 0;
    if (statvfs(j->dest[0] ? j->dest : "/", &sv) != 0) {
        *known = 0;
        return 0;
    }
    return (uint64_t)sv.f_bavail * (uint64_t)sv.f_frsize;
}

int xs_check_space(xjob_t *j, uint64_t need) {
    int known;
    uint64_t have = free_bytes(j, &known);
    if (!known) return 0; /* nothing to go on: the writes find out */
    if (have < need + XJ_SPACE_MARGIN) {
        return xj_fail(j, "fs_extract_no_space: needs %llu bytes, %llu free", (unsigned long long)(need + XJ_SPACE_MARGIN),
                       (unsigned long long)have);
    }
    return 0;
}

int xs_open_dest(xjob_t *j) {
    struct stat st;
    if (lstat(j->dest, &st) == 0) {
        if (!j->merge) return xj_fail(j, "fs_extract_destination_exists");
        if (!S_ISDIR(st.st_mode)) return xj_fail(j, "fs_extract_bad_dest: not a folder");
        return 0;
    }
    if (errno != ENOENT) return xj_fail(j, "fs_extract_bad_dest: %s", strerror(errno));
    if (mkdir(j->dest, 0777) != 0) return xj_fail(j, "fs_extract_bad_dest: %s", strerror(errno));
    (void)chmod(j->dest, 0777);
    j->created_dest = 1;
    return 0;
}

/* Makes every folder of `rel` (all components when `all`, else all but the last) under the
 * destination. Never follows a link: an existing component that is not a real folder makes the entry
 * unsafe (1). 0 ok, 1 unsafe, -1 failure (cause set). */
static int make_parents(xjob_t *j, const char *rel, int all) {
    char path[XJ_PATH];
    size_t base = strlen(j->dest);
    struct stat st;
    if (base + 1 + strlen(rel) + 1 > sizeof path) return 1;
    memcpy(path, j->dest, base);
    path[base] = '/';
    strcpy(path + base + 1, rel);
    for (char *p = path + base + 1;; p++) {
        int last = *p == 0;
        if (*p != '/' && !last) continue;
        if (last && !all) break;
        *p = 0;
        if (lstat(path, &st) == 0) {
            if (!S_ISDIR(st.st_mode)) return 1;
        } else if (errno == ENOENT) {
            if (mkdir(path, 0777) != 0 && errno != EEXIST) {
                return xj_fail(j, "fs_extract_write_failed: %s: %s", rel, strerror(errno));
            }
            (void)chmod(path, 0777);
            if (lstat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return 1;
            j->dirs++;
        } else {
            return xj_fail(j, "fs_extract_write_failed: %s: %s", rel, strerror(errno));
        }
        if (last) break;
        *p = '/';
    }
    return 0;
}

static void set_current(xjob_t *j, const char *rel) {
    const char *slash = strrchr(rel, '/');
    snprintf(j->current, sizeof j->current, "%s", slash ? slash + 1 : rel);
}

static unsigned tmp_seq;

int xs_begin(xjob_t *j, xentry_t *e, const char *name, int kind, uint64_t size, int64_t mtime) {
    char rel[XJ_PATH];
    struct stat st;
    int rc;
    memset(e, 0, sizeof *e);
    e->fd = -1;
    if (xj_failed(j) || xj_cancelled(j)) return XS_FAIL;
    if (kind == XE_LINK || kind == XE_OTHER) {
        j->links++;
        return XS_SKIP;
    }
    rc = xs_safe_name(name, rel, sizeof rel);
    if (rc == -2) return XS_SKIP; /* "./": the destination itself */
    if (rc != 0 || strlen(j->dest) + 1 + strlen(rel) + 32 > sizeof e->final_path) {
        j->unsafe++;
        return XS_SKIP;
    }
    set_current(j, rel);
    if (kind == XE_DIR) {
        rc = make_parents(j, rel, 1);
        if (rc < 0) return XS_FAIL;
        if (rc > 0) j->unsafe++;
        if (j->progress) j->progress(j, 0);
        return XS_SKIP;
    }
    rc = make_parents(j, rel, 0);
    if (rc < 0) return XS_FAIL;
    if (rc > 0) {
        j->unsafe++;
        return XS_SKIP;
    }
    snprintf(e->final_path, sizeof e->final_path, "%s/%s", j->dest, rel);
    if (lstat(e->final_path, &st) == 0) {
        /* A folder is never replaced by a file; anything else only when asked. A link in its place is
         * replaced as a name (the rename swaps the link itself, never what it points to). */
        if (S_ISDIR(st.st_mode) || !j->replace) {
            j->skipped++;
            j->files_done++;
            return XS_SKIP;
        }
    }
    if (size && xs_check_space(j, size) != 0) return XS_FAIL;
    {
        char *slash = strrchr(e->final_path, '/');
        size_t dir_len = (size_t)(slash - e->final_path);
        snprintf(e->tmp_path, sizeof e->tmp_path, "%.*s/.ps5u-x-%ld-%u.part", (int)dir_len, e->final_path,
                 (long)getpid(), ++tmp_seq);
    }
    e->fd = open(e->tmp_path, O_WRONLY | O_CREAT | O_EXCL | O_TRUNC, 0666);
    if (e->fd < 0) {
        int err = errno;
        e->tmp_path[0] = 0;
        if (err == ENOSPC) return xj_fail(j, "fs_extract_no_space: %s", rel);
        return xj_fail(j, "fs_extract_write_failed: %s: %s", rel, strerror(err));
    }
    e->mtime = mtime;
    return XS_WRITE;
}

/* Decoders hand over small pieces (libarchive's RAR readers about 28 KiB on average, many under
 * 64 KiB), and on the PS5 each write to the drive costs far more than the copy: #826 measured about
 * 10 MB/s for a RAR set that decodes at well over 100 MiB/s. So a file's bytes are gathered here and
 * written in runs of up to 4 MiB, like ffpfsc_unpack.c. One file is open at a time, so one buffer;
 * without the memory for it the bytes are written as they come. */
#define XS_WBUF (4u << 20)
static uint8_t *wbuf;
static size_t wfill;
static int wbuf_tried;

static int write_out(xjob_t *j, xentry_t *e, const uint8_t *p, size_t n) {
    while (n) {
        size_t take = n;
        ssize_t w;
        if (j->test_write_limit && j->written_total + take > j->test_write_limit) {
            if (j->written_total >= j->test_write_limit) {
                errno = ENOSPC;
                w = -1;
                goto werr;
            }
            take = (size_t)(j->test_write_limit - j->written_total);
        }
        w = write(e->fd, p, take);
        if (w < 0 && errno == EINTR) continue;
    werr:
        if (w <= 0) {
            int err = w < 0 ? errno : EIO;
            if (err == ENOSPC || err == EDQUOT) {
                int known;
                uint64_t have = free_bytes(j, &known);
                return xj_fail(j, "fs_extract_no_space: %s, %llu free", j->current, (unsigned long long)have);
            }
            return xj_fail(j, "fs_extract_write_failed: %s: %s", j->current, strerror(err));
        }
        p += w;
        n -= (size_t)w;
        j->written_total += (uint64_t)w;
    }
    return 0;
}

static int flush_out(xjob_t *j, xentry_t *e) {
    size_t n = wfill;
    wfill = 0;
    return n ? write_out(j, e, wbuf, n) : 0;
}

int xs_write(xjob_t *j, xentry_t *e, const void *buf, size_t n) {
    const uint8_t *p = buf;
    if (e->fd < 0) return xj_failed(j) ? -1 : 0;
    if (xj_cancelled(j)) return -1;
    if (j->test_write_delay_us) usleep(j->test_write_delay_us);
    if (!wbuf && !wbuf_tried) {
        wbuf_tried = 1;
        wbuf = malloc(XS_WBUF);
    }
    if (!wbuf) {
        if (write_out(j, e, p, n) != 0) return -1;
        e->written += n;
    }
    while (wbuf && n) {
        size_t take = XS_WBUF - wfill < n ? XS_WBUF - wfill : n;
        memcpy(wbuf + wfill, p, take);
        wfill += take;
        p += take;
        n -= take;
        e->written += take;
        if (wfill == XS_WBUF && flush_out(j, e) != 0) return -1;
    }
    if (j->progress) j->progress(j, 0);
    return 0;
}

int xs_end(xjob_t *j, xentry_t *e, int ok) {
    int rc = 0;
    if (e->fd >= 0) {
        /* What is still gathered goes out first; a dropped entry's bytes are thrown away. */
        if (ok && flush_out(j, e) != 0) {
            ok = 0;
            rc = -1;
        }
        wfill = 0;
        if (close(e->fd) != 0 && ok) {
            ok = 0;
            rc = xj_fail(j, "fs_extract_write_failed: %s: %s", j->current, strerror(errno));
        }
        e->fd = -1;
        if (ok && e->mtime >= 0) {
            struct timeval tv[2];
            tv[0].tv_sec = tv[1].tv_sec = (time_t)e->mtime;
            tv[0].tv_usec = tv[1].tv_usec = 0;
            (void)utimes(e->tmp_path, tv);
        }
        if (ok) (void)chmod(e->tmp_path, 0777);
        /* Same folder as the final name: the rename can never cross a mount. */
        if (ok && rename(e->tmp_path, e->final_path) != 0) {
            rc = xj_fail(j, "fs_extract_write_failed: %s: %s", j->current, strerror(errno));
            ok = 0;
        }
        if (!ok) unlink(e->tmp_path);
        else {
            j->files++;
            j->files_done++;
            j->bytes += e->written;
        }
        e->tmp_path[0] = 0;
    }
    if (j->progress) j->progress(j, 0);
    return rc;
}
