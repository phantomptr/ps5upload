#ifndef PS5UPLOAD_CROSS_DEVICE_H
#define PS5UPLOAD_CROSS_DEVICE_H

#include <stddef.h>
#include <string.h>
#include <sys/stat.h>

/* Guard against the cross-device rename that panics this kernel.
 *
 * POSIX says rename(2) across filesystems fails with EXDEV. On the PS5
 * it does not: moving a file from a USB mount to the internal SSD (or
 * between two USB mounts) takes the console down hard — black screen,
 * power-cord pull, reproducible. So the payload must decide BEFORE
 * calling rename() whether both ends live on the same device, and refuse
 * rather than let the kernel find out.
 *
 * Callers that hold a user-supplied destination path MUST consult this.
 * The three call sites are FS_MOVE (runtime.c), the shell's `mv`
 * (shell_builtin.c), and FTP RNFR/RNTO (ftp_server.c) — anywhere a
 * remote client picks both paths. Renames of our own tmp files into
 * their final name in the SAME directory are safe by construction and
 * do not need it.
 *
 * `XDEV_UNKNOWN` is deliberately a third value rather than being folded
 * into "safe", and it is FAIL CLOSED: a caller proceeds to rename() only on
 * XDEV_SAME. Use xdev_rename_is_safe() rather than comparing to CROSSES.
 * (An earlier version of this note said UNKNOWN should let rename() proceed
 * because the usual causes fail with an ordinary errno; that reasoned from
 * the common case and left the one error that costs a kernel panic
 * fail-open - review 007 #4.)
 *
 * Header-only so the payload and the host-built selftest share one
 * implementation — same pattern as hw_guard.h and appdb_scan.h.
 * Tests: payload/tests/cross_device_selftest.c. */

typedef enum {
    XDEV_SAME = 0,    /* both ends on one device — rename() is safe */
    XDEV_CROSSES = 1, /* different devices — rename() would PANIC */
    XDEV_UNKNOWN = 2  /* could not determine; see the note above */
} xdev_result_t;

/* Look up the device id for a path. Returns 0 and writes `*out` on
 * success, non-zero if the path could not be stat'd. Injected so the
 * selftest can describe a mount table without needing real mounts. */
typedef int (*xdev_dev_fn)(const char *path, unsigned long long *out);

/* Write the directory containing `path` into `buf`.
 *
 * Always produces a stat-able path: "." for a bare relative name or an
 * empty input, "/" for a file sitting directly in the root. Returning
 * an empty string here would be a bug with teeth — stat("") fails, the
 * result becomes UNKNOWN, and the guard waves through the very rename
 * it exists to stop. */
static inline void xdev_parent_dir(const char *path, char *buf,
                                   size_t buflen) {
    if (!buf || buflen == 0) return;
    if (!path || !*path) {
        snprintf(buf, buflen, ".");
        return;
    }

    size_t len = strlen(path);
    /* Ignore a trailing slash so "/data/dir/" yields "/data/dir" rather
     * than the directory's own parent. */
    while (len > 1 && path[len - 1] == '/') len--;

    const char *slash = NULL;
    for (size_t i = len; i > 0; i--) {
        if (path[i - 1] == '/') {
            slash = path + (i - 1);
            break;
        }
    }
    if (!slash) {
        snprintf(buf, buflen, ".");
        return;
    }
    if (slash == path) {
        snprintf(buf, buflen, "/");
        return;
    }

    size_t dlen = (size_t)(slash - path);
    if (dlen >= buflen) dlen = buflen - 1;
    memcpy(buf, path, dlen);
    buf[dlen] = '\0';
}

/* Would rename(from, to) cross a device boundary?
 *
 * Compares the source against the destination's PARENT DIRECTORY, not the
 * destination itself — the destination usually does not exist yet, which is
 * the whole point of a rename.
 *
 * The SOURCE is judged by its own device (`from_dev`, lstat: a symbolic link
 * is the thing rename() moves, and it lives on its directory's device). A
 * stat() here would judge the link by its TARGET: a link on device A pointing
 * at a file on device B, renamed into a directory on B, would read as B == B
 * and rename() would move the link across devices — the kernel panic. The
 * destination directory is followed (`dir_dev`, stat), as rename() resolves it. */
static inline xdev_result_t xdev_rename_crosses_l(const char *from,
                                                  const char *to,
                                                  xdev_dev_fn from_dev,
                                                  xdev_dev_fn dir_dev) {
    if (!from || !to || !from_dev || !dir_dev) return XDEV_UNKNOWN;

    unsigned long long dev_from = 0, dev_to = 0;
    if (from_dev(from, &dev_from) != 0) return XDEV_UNKNOWN;

    /* The parent is judged WHOLE: a path too long for the buffer would be clamped by xdev_parent_dir and
     * stat a different, nonexistent directory (a parent on another mount cut off), so it is UNKNOWN, which
     * every caller refuses (review 007 #4, final review fs #1). 4096 covers every path the payload accepts
     * (FS_PATH_MAX is 1024). */
    char to_dir[4096];
    if (strlen(to) >= sizeof(to_dir)) return XDEV_UNKNOWN;
    xdev_parent_dir(to, to_dir, sizeof(to_dir));
    if (dir_dev(to_dir, &dev_to) != 0) return XDEV_UNKNOWN;

    return dev_from == dev_to ? XDEV_SAME : XDEV_CROSSES;
}

/* The only answer that lets a rename go ahead. */
static inline int xdev_rename_is_safe(xdev_result_t r) { return r == XDEV_SAME; }

/* One lookup for both ends (the selftest's fake mount table). Real callers use
 * xdev_rename_crosses_l with xdev_lstat_dev for the source. */
static inline xdev_result_t xdev_rename_crosses(const char *from,
                                                const char *to,
                                                xdev_dev_fn dev_fn) {
    return xdev_rename_crosses_l(from, to, dev_fn, dev_fn);
}

/* The real device lookup, for payload callers. */
static inline int xdev_stat_dev(const char *path, unsigned long long *out) {
    struct stat st;
    if (!path || !out) return -1;
    if (stat(path, &st) != 0) return -1;
    *out = (unsigned long long)st.st_dev;
    return 0;
}

/* The same without following a final symbolic link: the device of the link
 * itself. This is the lookup for a rename's SOURCE. */
static inline int xdev_lstat_dev(const char *path, unsigned long long *out) {
    struct stat st;
    if (!path || !out) return -1;
    if (lstat(path, &st) != 0) return -1;
    *out = (unsigned long long)st.st_dev;
    return 0;
}

#endif
