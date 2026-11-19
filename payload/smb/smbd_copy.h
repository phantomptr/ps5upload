/* The copy state machine: one file or one folder tree from a source filesystem to a destination
 * filesystem (SMB -> console, console -> SMB), with progress, cancel, conflict rules and recovery
 * from a server that drops mid-copy. Both sides are behind xfs_t, so the host test drives it with
 * an in-memory filesystem that can fail on cue (payload/tests/smbd_copy_selftest.c).
 *
 * Rules:
 * - A file is written to "<dest>.ps5u-part" in the destination's own folder and renamed over
 *   <dest> when it is complete (same folder: never a cross-device rename). A failure or a cancel
 *   removes the part file; the destination is never left half-written.
 * - An existing destination file is refused (XE_EXISTS) unless `overwrite`; an existing folder is
 *   refused at the top unless `overwrite` (then it is merged: files in it are replaced).
 * - A read or write that fails as XE_DISCONNECTED / XE_TIMEOUT / XE_IO on a side that can
 *   reconnect is retried after reconnecting (COPY_RETRIES times, waits 1 s, 2 s, 4 s), carrying on
 *   from the same offset; then the copy fails with that error.
 * - Links and other special entries are skipped. Folder trees deeper than COPY_MAX_DEPTH are refused.
 * - Buffers are bounded: one chunk of at most COPY_CHUNK_MAX, on the heap. */
#ifndef PS5UPLOAD_SMBD_COPY_H
#define PS5UPLOAD_SMBD_COPY_H

#include <stddef.h>
#include <stdint.h>

#define COPY_CHUNK_MAX (4u * 1024u * 1024u)
#define COPY_MAX_DEPTH 48
#define COPY_RETRIES 3
#define COPY_DIR_ENTRIES_MAX 100000u /* one folder's entries, read before it is copied */

typedef enum { XK_FILE = 1, XK_DIR = 2, XK_OTHER = 3 } xkind_t;

typedef struct {
    xkind_t kind;
    uint64_t size;
    uint64_t mtime; /* seconds since the epoch */
} xstat_t;

typedef struct xfs xfs_t;

/* One side of a copy. Every call returns 0 / a count, or a negative xerr_t. */
typedef struct {
    int (*stat)(xfs_t *fs, const char *path, xstat_t *st);
    int (*opendir)(xfs_t *fs, const char *path, void **dir);
    /* 1 = an entry (name and st set), 0 = the end, < 0 = an error. "." and ".." are never returned.
     * XE_BAD_NAME: one entry whose name does not fit in `cap` (or whose path is too long); the next
     * call goes on with the next entry. A copy fails on it (nothing is left out unseen); a listing
     * or a scan may skip it. */
    int (*readdir)(xfs_t *fs, void *dir, char *name, size_t cap, xstat_t *st);
    void (*closedir)(xfs_t *fs, void *dir);
    int (*mkdir)(xfs_t *fs, const char *path);
    int (*open_read)(xfs_t *fs, const char *path, void **fh);
    /* resume = 0: create or truncate; 1: open the existing file for writing at any offset. */
    int (*open_write)(xfs_t *fs, const char *path, int resume, void **fh);
    long long (*pread)(xfs_t *fs, void *fh, void *buf, size_t n, uint64_t off);
    long long (*pwrite)(xfs_t *fs, void *fh, const void *buf, size_t n, uint64_t off);
    /* Flushes a written file; a handle is always released, even when this fails. */
    int (*close)(xfs_t *fs, void *fh);
    /* `from` and `to` are in the same folder. replace = 1: an existing `to` (a file) is replaced. */
    int (*rename)(xfs_t *fs, const char *from, const char *to, int replace);
    int (*unlink)(xfs_t *fs, const char *path);
    int (*rmdir)(xfs_t *fs, const char *path);
    /* Free bytes where `path` lives: 0 with *out set, or < 0 (unknown: the copy goes ahead). */
    int (*free_space)(xfs_t *fs, const char *path, uint64_t *out);
    /* Drop the connection and connect again (every open handle is then invalid). NULL: the side
     * cannot (a local disk). */
    int (*reconnect)(xfs_t *fs);
    /* The largest read or write the side does in one call (0: COPY_CHUNK_MAX). */
    size_t (*chunk)(xfs_t *fs);
} xfs_ops_t;

struct xfs {
    const xfs_ops_t *ops;
    void *impl;
};

/* Progress, written by the copy and read by any thread (plain loads: a torn read only shows a
 * slightly stale number). */
typedef struct {
    volatile uint64_t bytes_done;
    volatile uint64_t bytes_total;
    volatile uint32_t files_done;
    volatile uint32_t files_total;
    volatile int scanning;        /* 1 while the source tree is being counted */
    volatile int cancel;          /* set by anyone: the copy stops at the next chunk */
    volatile uint32_t reconnects; /* how often a dropped connection was recovered */
    char current[256];            /* the file being copied (its name), best-effort */
} copy_progress_t;

typedef struct {
    int overwrite;
    /* Leave out macOS metadata (smbp_skip_download) while walking a folder. */
    int skip_mac_metadata;
    /* Free space the destination must keep beyond what the copy writes. */
    uint64_t space_margin;
    /* Waits between reconnect tries; tests pass a no-op. NULL = sleep(seconds). */
    void (*sleep_s)(unsigned seconds);
} copy_opts_t;

/* Copy `src_path` on `src` to `dst_path` on `dst` (the full destination name, not its folder).
 * 0, or a negative xerr_t; `failed` (cap bytes) names the entry the failure was on, when there was one. */
int copy_run(xfs_t *src, const char *src_path, xfs_t *dst, const char *dst_path, const copy_opts_t *opts,
             copy_progress_t *pr, char *failed, size_t failed_cap);

/* Remove `path` on `fs`, a file or a whole folder tree (for Replace of a folder on a share).
 * 0 or a negative xerr_t. Stops at the next entry when pr->cancel is set. */
int copy_remove_tree(xfs_t *fs, const char *path, copy_progress_t *pr);

#endif
