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
 * into "safe": the caller has to make an explicit choice about it. The
 * safe choice is to let rename() proceed, because the cases that produce
 * UNKNOWN (missing source, missing destination directory) are exactly
 * the cases where rename() fails with an ordinary errno before it can
 * reach the cross-device path. What must never happen is UNKNOWN being
 * silently read as SAME by a caller that assumed a boolean.
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
 * Compares the source file against the destination's PARENT DIRECTORY,
 * not the destination itself — the destination usually does not exist
 * yet, which is the whole point of a rename. */
static inline xdev_result_t xdev_rename_crosses(const char *from,
                                                const char *to,
                                                xdev_dev_fn dev_fn) {
    if (!from || !to || !dev_fn) return XDEV_UNKNOWN;

    unsigned long long dev_from = 0, dev_to = 0;
    if (dev_fn(from, &dev_from) != 0) return XDEV_UNKNOWN;

    char to_dir[512];
    xdev_parent_dir(to, to_dir, sizeof(to_dir));
    if (dev_fn(to_dir, &dev_to) != 0) return XDEV_UNKNOWN;

    return dev_from == dev_to ? XDEV_SAME : XDEV_CROSSES;
}

/* The real device lookup, for payload callers. */
static inline int xdev_stat_dev(const char *path, unsigned long long *out) {
    struct stat st;
    if (!path || !out) return -1;
    if (stat(path, &st) != 0) return -1;
    *out = (unsigned long long)st.st_dev;
    return 0;
}

#endif
