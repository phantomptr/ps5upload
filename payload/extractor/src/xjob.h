/* ps5upload-extractor: one archive extraction, shared by the 7-Zip and the RAR back ends.
 *
 * The extractor is its own short-lived payload (never the helper: decompression code linked into the
 * helper crashed it, see payload/extractor/README.md). The helper writes a request, starts this ELF
 * through the console's loader, follows a status file and asks for a stop with a cancel file.
 *
 * Every byte the archive asks to write goes through the sink below (xsink.c), which owns the rules:
 *   - names are relative and stay inside the destination: an absolute name, a `..` component, a drive
 *     letter or an existing symbolic link on the way is refused (counted as `unsafe`, never written);
 *   - links and special files inside the archive are not created (counted as `links`);
 *   - a file is written to a temporary name in its own folder and renamed into place, so the rename
 *     never crosses a mount (a cross-device rename panics the PS5 kernel);
 *   - an existing file is kept (`skipped`) unless the request asked to replace it;
 *   - free space is checked before writing (the archive's total when it is known, each file anyway);
 *   - a stop asked through the cancel file ends the extraction within one buffer.
 *
 * Plain C, no PS5 types: the host test build runs the same code against temporary folders. */
#ifndef PS5UPLOAD_XJOB_H
#define PS5UPLOAD_XJOB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define XJ_PATH 1024
#define XJ_NAME 256
#define XJ_CAUSE 384
#define XJ_PASSWORD 256
/* The top-level names of a listing, and a request's renames of top-level entries ("Extract here"). */
#define XJ_TOP 8192

/* Room kept free on the destination's drive beyond what the files need. */
#define XJ_SPACE_MARGIN (64ull << 20)

typedef struct xjob {
    /* The request. */
    char id[64];
    char archive[XJ_PATH]; /* the archive, or its first volume */
    char dest[XJ_PATH];    /* the folder the entries are written into */
    char password[XJ_PASSWORD];
    int has_password;
    int merge;   /* the destination may exist: write into it */
    int replace; /* an existing file is replaced (else kept and counted as skipped) */
    /* Only name the archive's top-level entries (into `top`); nothing is written, and the
     * destination is neither made nor checked for room. */
    int list;
    /* Renames of top-level entries, "from/to/from/to..." ('/' never occurs in one name). An empty
     * `to` leaves that entry out (counted as skipped). Applied to the name made safe. */
    char top_map[XJ_TOP];
    /* list: the distinct top-level names, '/' separated, those of them that are folders, and
     * whether more did not fit. */
    char top[XJ_TOP];
    char top_dirs[XJ_TOP];
    int top_more;

    /* Progress. A total of 0 means "not known yet". */
    uint64_t files_total, bytes_total;
    uint64_t files_done, bytes_done;
    char current[XJ_NAME];

    /* The outcome. */
    uint64_t files, dirs, bytes, skipped, unsafe, links;
    int created_dest; /* this run made the destination folder */
    char cause[XJ_CAUSE]; /* the first failure: a cause token, then detail */

    /* Hooks. `cancelled` is asked often (it should be cheap); `progress` after each buffer and entry
     * (it throttles itself). Either may be NULL. */
    int (*cancelled)(struct xjob *j);
    void (*progress)(struct xjob *j, int force);
    void *ctx;

    /* Tests only: report this much free space instead of the drive's (0 = the drive's), fail
     * writes with ENOSPC once this many bytes were written (0 = never), slow each write down. */
    uint64_t test_free;
    uint64_t test_write_limit;
    unsigned test_write_delay_us; /* each write waits this long (a slow drive) */
    uint64_t written_total;
} xjob_t;

/* Sets the first failure (later ones keep the first). Returns -1 for `return xj_fail(...)`. */
int xj_fail(xjob_t *j, const char *fmt, ...)
#ifdef __GNUC__
    __attribute__((format(printf, 2, 3)))
#endif
    ;
int xj_failed(const xjob_t *j);
int xj_cancelled(xjob_t *j);

/* ---- the sink ---- */

enum { XE_FILE = 0, XE_DIR = 1, XE_LINK = 2, XE_OTHER = 3 };

typedef struct xentry {
    int fd;
    char final_path[XJ_PATH];
    char tmp_path[XJ_PATH];
    int64_t mtime; /* seconds since the epoch, or -1 */
    uint64_t written;
} xentry_t;

enum { XS_WRITE = 0, XS_SKIP = 1, XS_FAIL = -1 };

/* Prepares the destination: created when it does not exist (merge or not), refused when it exists
 * without `merge` (fs_extract_destination_exists) or is not a real folder. 0 or -1 (cause set). */
int xs_open_dest(xjob_t *j);

/* Checks the room for `need` more bytes on the destination's drive. 0, or -1 with
 * fs_extract_no_space set. */
int xs_check_space(xjob_t *j, uint64_t need);

/* One entry. `name` is the archive's name for it (UTF-8, '/' or '\\' separated). For a file that
 * returns XS_WRITE, write its bytes with xs_write and finish with xs_end. A folder is made at once
 * (XS_SKIP: nothing to write). XS_SKIP for an entry that is not written (unsafe, a link, an existing
 * file kept); XS_FAIL when the extraction must stop (cause set). */
int xs_begin(xjob_t *j, xentry_t *e, const char *name, int kind, uint64_t size, int64_t mtime);
/* 0, or -1 (cause set: no space, write failed, cancelled). */
int xs_write(xjob_t *j, xentry_t *e, const void *buf, size_t n);
/* Commits (ok) or drops (!ok) the entry. 0 or -1 (cause set). Safe to call on a dropped entry. */
int xs_end(xjob_t *j, xentry_t *e, int ok);

/* The name made safe: relative, '/' separated, no `.`/`..`/empty components, control characters
 * replaced. 0; -2 when nothing is left (".", "./"): the destination itself; -1 when it cannot be
 * (absolute, `..`, a drive letter, a component longer than 255 bytes, too long a path). */
int xs_safe_name(const char *name, char *out, size_t cap);

/* The safe name `rel` (in place, `cap` bytes) after the request's top-level renames: 0 keep (maybe
 * renamed), 1 leave the entry out, -1 the new name does not fit. */
int xs_map_top(const xjob_t *j, char *rel, size_t cap);

/* ---- the back ends ---- */

/* The archive's kind from its name: "7z", "zip", "rar", "tar", "gz", "bz2", "xz", "zst", "split", or
 * NULL when the name is not one the extractor takes. */
const char *xj_kind(const char *archive);

/* Extracts j->archive into j->dest. 0 or -1 (j->cause set). */
int xj_run(xjob_t *j);

int x7z_extract(xjob_t *j);   /* everything but RAR: x7z.cpp (7-Zip) */
int xrar_extract(xjob_t *j);  /* RAR 1.5-5: xrar.c (libarchive) */

/* The volumes of a multi-volume RAR beside `first` (x.part1.rar ... or x.rar, x.r00 ...), in order,
 * as one allocated NULL-terminated array (free with xj_free_list), or NULL on no memory. A single
 * archive is a list of one. The list stops at the first absent number: whether the archive goes on
 * past it is only known from the last volume's own header, so a missing one is found while reading. */
char **xj_rar_volumes(const char *first);
/* The name of volume `k` (0 = `first`) of the set `first` starts. 0, or -1. */
int xj_rar_volume_name(const char *first, unsigned k, char *out, size_t cap);
void xj_free_list(char **list);

#ifdef __cplusplus
}
#endif

#endif
