/* Extract an archive on the console (JOB_OP_ARCHIVE_EXTRACT): the helper's side.
 *
 * The decoding is not done in the helper. Decompression code linked into the helper crashed it (a
 * static zlib: SIGBUS within minutes, once a console freeze), so it runs in its own short-lived
 * payload, payload/extractor (7-Zip and libarchive), which this ELF carries as bytes and starts
 * through the console's ELF loader (127.0.0.1:9021) for each extraction. The two talk through
 * files in EXTRACT_DIR: the request (0600, deleted by the extractor once read; it can hold a
 * password), the status the extractor rewrites while it runs, and a cancel file. See
 * payload/extractor/README.md.
 *
 * JOB_OP_ARCHIVE_EXTRACT {"path":"<archive or its first volume>","dest":"<folder>"[,"password":"..."]
 *   [,"merge":true][,"replace":true]}: writes the archive's entries under `dest`. A `dest` that
 * exists is refused unless "merge"; existing files are kept (counted as skipped) unless "replace".
 * Progress is the extractor's (bytes and files when the archive says them); Stop asks it to stop
 * and, if it does not within EXTRACT_KILL_AFTER_MS, kills it. On a failure or a stop, a `dest` this
 * run created is removed again. Result: {"path","files","dirs","bytes","skipped","unsafe","links"}.
 * One extraction at a time (fs_extract_busy). */
#ifndef PS5UPLOAD_ARCHIVE_EXTRACT_H
#define PS5UPLOAD_ARCHIVE_EXTRACT_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <time.h>

#define EXTRACT_DIR "/data/ps5upload/extract"

struct ava1_op_ctx;
int archive_extract_op(void *arg, struct ava1_op_ctx *c, const uint8_t *args, size_t n);

/* Registers JOB_OP_ARCHIVE_EXTRACT with ava1_op.c (next to fs_jobs.c's ops). */
void archive_extract_register(void);

/* 1 while an extraction runs in this helper (console_convert.c refuses to build beside one). */
int archive_extract_running(void);

/* A helper that died mid-extraction leaves its extractor's work behind: at the next start, a folder
 * that run created is removed (unless the old helper or its extractor still runs). */
void archive_extract_sweep(void);

/* The request can hold the archive's password in clear. The extractor deletes it the moment it has read
 * it and the helper when the extractor never starts, so one is left only by a helper that died in
 * between (a crash, a power cut). Older than this it cannot be waiting for an extractor (the helper
 * gives one 20 s to report), so it goes; so does one dated in the future (the clock moved, its age is
 * unknown). A younger one may belong to an extractor another helper instance just started. */
#define EXTRACT_REQUEST_STALE_S 60
static inline int archive_extract_request_stale(time_t mtime, time_t now) {
    return now < mtime || now - mtime >= EXTRACT_REQUEST_STALE_S;
}

/* job.rec keeps the destination on a line of its own ("<extractor pid> <helper pid> <created>\n
 * <dest>\n") for the sweep after a helper died mid-extraction, which removes the folder the
 * extraction made. A destination with a line break in it read back as its first line only: for
 * "/data/homebrew\nx" the sweep removed /data/homebrew. So a destination with any control byte
 * is refused before anything starts (the converter's record has the same rule). */
static inline int archive_extract_dest_ok(const char *dest) {
    const unsigned char *p = (const unsigned char *)dest;
    if (!p || p[0] != '/') return 0;
    for (; *p; p++)
        if (*p < 0x20 || *p == 0x7f) return 0;
    return 1;
}

/* A process id read from a file (status.json, job.rec) that may be signalled: a real one. The
 * cast to pid_t of a larger number wraps: 4294967295 is -1, and kill(-1) is every process. */
static inline int archive_extract_pid_ok(unsigned long long pid) { return pid > 1 && pid <= 0x7FFFFFFFull; }

/* Whether the sweep leaves the folder of a dead helper's extraction alone: the extractor is its own
 * process and went on without the helper, and its last status (of that same extractor, by pid) says
 * it finished. That folder is a complete extraction, the person may have been using it since, and
 * removing it at the next start (or at the next extraction, days later) deleted good files. Any
 * other state (failed, still "running" when it died, another extractor's status, none) is the
 * half-made folder the sweep is for. */
static inline int archive_extract_sweep_keeps(unsigned long long rec_pid, unsigned long long status_pid,
                                              const char *status_state) {
    return rec_pid != 0 && status_pid == rec_pid && status_state && strcmp(status_state, "done") == 0;
}

/* Deletes a request left behind (archive_extract_request_stale). Run at the start, by archive_extract_sweep,
 * and again a minute later for one too young to judge then. */
void archive_extract_sweep_request(void);

/* What an extraction of `path` would do, read without starting it (the console page asks first). */
typedef struct {
    char name[256];  /* the default folder's name */
    char dest[1024]; /* <archive's folder>/<name> */
    const char *kind;
    uint64_t size;    /* all volumes found */
    unsigned volumes;
    uint64_t free;
    int have_free, exists;
} archive_probe_t;
/* NULL, or the cause token the extraction would fail with. */
const char *archive_extract_probe(const char *path, archive_probe_t *p);

#endif
