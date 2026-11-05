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
#include <time.h>

#define EXTRACT_DIR "/data/ps5upload/extract"

struct ava1_op_ctx;
int archive_extract_op(void *arg, struct ava1_op_ctx *c, const uint8_t *args, size_t n);

/* Registers JOB_OP_ARCHIVE_EXTRACT with ava1_op.c (next to fs_jobs.c's ops). */
void archive_extract_register(void);

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
