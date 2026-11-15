/* Convert a game on the console (JOB_OP_CONSOLE_CONVERT): the helper's side.
 *
 * The conversion is not done in the helper. It runs in its own short-lived payload,
 * ps5upload-convert (engine/crates/ps5upload-convert, built by payload/convert/build.sh), which
 * this ELF carries as bytes and starts through the console's ELF loader (127.0.0.1:9021) for each
 * job, the way the archive extractor is started (archive_extract.h): a crash or a memory spike in
 * a conversion ends that program, not the helper. The two talk through files in CONVERT_DIR:
 *
 *   request.json  written here (0600), read and deleted by the converter at start
 *   status.json   rewritten by the converter through a temporary file and a rename in that folder
 *   cancel        created here when Stop is asked
 *   job.rec       this helper's note of a running job, for the next start's sweep
 *   convert.log   the converter's stdout/stderr (the last run)
 *
 * JOB_OP_CONSOLE_CONVERT {"path":"<game folder or .exfat/.ffpkg/.ffpfs image>"
 *   [,"action":"inspect"|"build"][,"dest":"<output>"][,"format":"exfat"|"ffpkg"|"ffpfs"|"folder"]
 *   [,"zstd":true[,"zstd_level":1..22]][,"verify":"fast"|"full"]}
 * "inspect" (the default) reads what the source is; the job's result is the converter's answer
 * ({"path","format","size","files","title_id","title",...}). "build" writes `dest` in `format`
 * (through `<dest>.partial` beside it, renamed in place at the end). A `dest` that exists is
 * refused. Stop asks the converter to stop and, if it has not within CONVERT_KILL_AFTER_MS, kills
 * it; a failed or stopped build's `.partial` is removed. Result of a build: {"path":"<dest>"}.
 * One job at a time (convert_busy); a build is also refused while a package install or an
 * archive extraction runs (the three would share the drive and the console's memory). */
#ifndef PS5UPLOAD_CONSOLE_CONVERT_H
#define PS5UPLOAD_CONSOLE_CONVERT_H

#include <stddef.h>
#include <stdint.h>
#include <time.h>

/* payload/convert/entry.c names the same folder for the converter. */
#define CONVERT_DIR "/data/ps5upload/convert"

/* The most of status.json the helper reads. The converter keeps its status far below this (an
 * inspection is about 1 KiB); one this large is not a status. */
#define CONVERT_STATUS_MAX 16384u
/* The largest converter answer passed on as the job's result: the console page's job status
 * carries at most WEBUI_JOB_RESULT_MAX (4096) bytes. */
#define CONVERT_RESULT_MAX 4000u

struct ava1_op_ctx;
int console_convert_op(void *arg, struct ava1_op_ctx *c, const uint8_t *args, size_t n);

/* Registers JOB_OP_CONSOLE_CONVERT with ava1_op.c (next to archive_extract_register). */
void console_convert_register(void);

/* A helper that died mid-conversion leaves the converter's unfinished output behind: at the next
 * start it is removed (unless the old helper or its converter still runs). */
void console_convert_sweep(void);

/* 1 while a build runs in this helper (archive_extract.c refuses to start beside one; an
 * inspection does not count). */
int console_convert_building(void);

/* A request no converter read is left only by a helper that died between writing it and the
 * converter's start. Older than this it cannot be waiting for one (the helper gives a converter
 * 20 s to report), so it goes; so does one dated in the future (the clock moved). The rule is
 * the extractor's (archive_extract_request_stale). */
#define CONVERT_REQUEST_STALE_S 60
static inline int console_convert_request_stale(time_t mtime, time_t now) {
    return now < mtime || now - mtime >= CONVERT_REQUEST_STALE_S;
}

/* ---- the files' formats (console_convert_wire.c: no I/O, host-tested) ---- */

/* What a build asks for, already checked by the op. */
typedef struct {
    const char *dest;   /* NULL for an inspection */
    const char *format; /* "exfat" | "ffpkg" | "ffpfs" | "folder" */
    int zstd;
    int full_verify;
    int zstd_level; /* 1..22 with zstd; 0: the converter's default */
} cc_build_t;

/* request.json for job `id`: {"id","op","path"} and, with `b`, {"dest","format","zstd","verify"}
 * (and "zstd_level" when zstd is on and a level was chosen).
 * Returns the length, or -1 when it does not fit `cap`. */
int cc_request_json(char *out, size_t cap, const char *id, const char *op, const char *path, const cc_build_t *b);

typedef struct {
    char state[16]; /* "running" | "done" | "failed" */
    char op[16];
    char cause[64];   /* a failure's token (convert_*) */
    char detail[400]; /* a failure's words, for the log */
    char stage[24];   /* a build's current stage */
    uint64_t pid, bytes_done, bytes_total;
    const char *result; /* the "result" object inside the parsed buffer, or NULL */
    size_t result_len;
} cc_status_t;

/* Parses the `n` bytes read from status.json. 1 when it is one complete JSON object that carries
 * job `id` and a state; 0 otherwise: another job's, empty, cut short (the helper may read it while
 * it is being replaced, or get a short read), or not JSON at all. A 0 means "nothing new yet". */
int cc_status_parse(const char *buf, size_t n, const char *id, cc_status_t *s);

/* job.rec: "<converter pid> <helper pid>\n<dest>\n" (`dest` "" for an inspection). */
int cc_record_format(char *out, size_t cap, unsigned long long xpid, long hpid, const char *dest);
/* 1 parsed, 0 not a record. */
int cc_record_parse(const char *buf, unsigned long long *xpid, long *hpid, char *dest, size_t cap);

/* The format an output path's extension stands for ("exfat", "ffpkg", "ffpfs"), or NULL. */
const char *cc_image_format(const char *path);
/* 1 when `level` is a zstd level a build may be asked for (zstd's own range; the page offers 1, 5, 19). */
static inline int cc_zstd_level_ok(unsigned long long level) { return level >= 1 && level <= 22; }
/* 1 when `format` is one a build writes. */
int cc_format_ok(const char *format);
/* 1 when `inner` is `outer` itself or below it (both normalised). */
int cc_path_within(const char *outer, const char *inner);

#ifdef CONSOLE_CONVERT_SELFTEST
/* The host test runs the op against a folder of its own and a stand-in for the loader. */
extern const char *console_convert_test_dir;
extern int (*console_convert_test_launch)(void);
#endif

#endif
