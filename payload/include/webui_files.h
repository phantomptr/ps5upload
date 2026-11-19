/* File transfer pieces for the console page's Files (webui_api.c is the glue): saving a text file
 * atomically, receiving an upload into a file, sending a file (with a Range) or a folder as a .tar,
 * all over the web server's blocking sockets with an idle timeout instead of the 5 s connection
 * deadline. No path policy here: the routes decide what may be read or written, then call these.
 *
 * Every write goes to a temporary name in the TARGET'S OWN FOLDER and is renamed onto the target
 * only after a same-device check: a rename never crosses a mount (on this kernel that panics the
 * console). A failure or a cancel removes the temporary file; the target is never truncated. */
#ifndef PS5UPLOAD_WEBUI_FILES_H
#define PS5UPLOAD_WEBUI_FILES_H

#include <stddef.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>

#define WF_TEXT_MAX (1024u * 1024u)  /* the longest text the editor saves */
#define WF_IDLE_MS 20000u            /* a transfer that moves no byte this long is dropped */
#define WF_IO_BUF (1024u * 1024u)    /* the heap buffer one transfer reads and writes through */
#define WF_TAR_MAX_DEPTH 64          /* folders deeper than this are left out of a .tar */
#define WF_TAR_PATH_MAX 4096u        /* an entry whose path is longer is left out */
#define WF_RATE_WINDOW_MS 60000u    /* the throughput floor's window */
#define WF_RATE_MIN_BPS 1024u        /* a transfer averaging less over a window is dropped */
#define WF_STALE_PART_S (24u * 3600u) /* a leftover temporary file older than this is swept */
#define WF_SWEEP_MAX 20000u          /* folder entries one sweep looks at, at most */
#define WF_TAR_HEADER_MAX (512u + 2u * WF_TAR_PATH_MAX + 1024u + 512u) /* pax header + its data + the entry's */

enum {
    WF_OK = 0,
    WF_E_TOO_BIG,     /* text over WF_TEXT_MAX */
    WF_E_BINARY,      /* text with a NUL byte */
    WF_E_CHANGED,     /* the file's mtime/size differ from what the editor opened */
    WF_E_GONE,        /* the file is not there any more */
    WF_E_EXISTS,      /* the target exists (and may not be replaced) */
    WF_E_NOT_FILE,    /* the target is a folder, a link or a device */
    WF_E_IO,
    WF_E_NO_SPACE,
    WF_E_CROSS,       /* the temporary file is not on the target's device: never renamed */
    WF_E_SHORT,       /* the upload ended before its declared length (cancelled, or the client went away) */
    WF_E_CANCELLED,
};

/* {"error":<token>} for an WF_E_* code, and its HTTP status. */
const char *wf_error_token(int e);
int wf_error_http(int e);

/* ---- socket I/O with an idle timeout ---- */

/* The throughput floor: a client that trickles a byte now and then never trips the idle timeout, so a
 * transfer must also average WF_RATE_MIN_BPS over every WF_RATE_WINDOW_MS window (zeroed = defaults). */
typedef struct {
    uint64_t start_ms;  /* 0: not started */
    uint64_t bytes;
    uint32_t window_ms; /* 0 = WF_RATE_WINDOW_MS */
    uint32_t min_bps;   /* 0 = WF_RATE_MIN_BPS */
} wf_rate_t;
/* Count `n` bytes moved at `now_ms`: 1 while the transfer keeps up, 0 when a window closed below the floor. */
int wf_rate_ok(wf_rate_t *r, uint64_t now_ms, uint64_t n);

/* Up to `cap` bytes: > 0 bytes read, 0 the peer closed, -1 an error, `idle_ms` without a byte, or
 * *cancel set (checked every 200 ms; may be NULL). */
ssize_t wf_recv(int fd, void *buf, size_t cap, unsigned idle_ms, const volatile int *cancel);
/* All of `n` bytes; 0, or -1 when the peer stops reading for `idle_ms` or goes away. */
int wf_send_all(int fd, const void *p, size_t n, unsigned idle_ms);

/* ---- temporary names ---- */

/* Remove leftover temporary files (".ps5u-*.part", regular files only, judged by lstat: a link is never
 * followed) older than `max_age_s` from `dir` itself (no subfolder), looking at WF_SWEEP_MAX entries at
 * most. A crash or a power cut mid-upload leaves one; the next upload or save in that folder clears it.
 * Returns how many were removed. */
int wf_sweep_stale(const char *dir, time_t now, unsigned max_age_s);

/* "<target's folder>/.ps5u-<16 hex>.part" (random): 0, or -1 when it does not fit `cap`. */
int wf_temp_path(const char *target, char *out, size_t cap);

/* ---- text save ---- */

typedef struct {
    int create;      /* a new, empty file: refused when anything is there (no data allowed) */
    int check;       /* compare mtime/size with the file as it is now (the editor's copy) */
    uint64_t mtime;  /* seconds */
    uint64_t size;
    int have_ns;     /* mtime_ns given too: same-second changes of the same size are caught as well */
    uint32_t mtime_ns;
} wf_text_pre_t;

/* The nanoseconds part of a stat's mtime. */
uint32_t wf_mtime_ns(const struct stat *st);

/* Save `data` as `path`: refused over WF_TEXT_MAX or with a NUL byte; written to a temporary file in
 * the same folder, fsync'd, given the old file's permissions, renamed onto it (a create makes an empty
 * file with O_EXCL instead). The precondition is checked before writing and again right before the
 * rename. On success the new mtime/size are stored (either pointer may be NULL). */
int wf_text_save(const char *path, const uint8_t *data, size_t n, const wf_text_pre_t *pre, uint64_t *mtime_out,
                 uint64_t *size_out, uint32_t *mtime_ns_out);
/* 1 when `data` is text the editor saves (no NUL byte), else 0. */
int wf_text_ok(const uint8_t *data, size_t n);

/* ---- upload: an HTTP body into a file ---- */

/* `len` body bytes (`pre` came with the headers, the rest is read from `fd`) into a temporary file
 * beside `target`, then renamed onto it: WF_E_EXISTS when the target appeared and `overwrite` is 0,
 * WF_E_NOT_FILE when it is a folder or a link. `buf`/`cap` is the caller's heap buffer. `mode` is
 * given to the new file. Any failure removes the temporary file. */
int wf_receive_file(int fd, const uint8_t *pre, size_t pre_len, uint64_t len, const char *target, int overwrite,
                    unsigned mode, unsigned idle_ms, const volatile int *cancel, uint8_t *buf, size_t cap);

/* ---- download ---- */

/* A Range header value against a file of `size` bytes: 0 = no usable range (send it all; also
 * several ranges or a bad syntax, which RFC 9110 lets a server ignore), 1 = [*start, *end] (both
 * inclusive), -1 = not satisfiable (416). */
int wf_parse_range(const char *v, uint64_t size, uint64_t *start, uint64_t *end);

/* `Content-Disposition: attachment; filename="<ASCII>"; filename*=UTF-8''<percent-encoded>\r\n`
 * for `name`. Bytes written (no NUL counted), or 0 when it does not fit. */
size_t wf_disposition(char *out, size_t cap, const char *name);

/* `len` bytes of `file_fd` from `start` to the socket. 0, or -1 (read error, client gone). */
int wf_send_file(int fd, int file_fd, uint64_t start, uint64_t len, unsigned idle_ms, uint8_t *buf, size_t cap);

/* One tar entry's header (ustar; a pax 'x' header first when the name does not fit ustar or the size
 * needs more than 11 octal digits). `name` is the entry's path inside the archive (a folder's without
 * the trailing '/', it is added). Bytes written (a multiple of 512), or 0 when `cap` is too small. */
size_t wf_tar_header(uint8_t *out, size_t cap, const char *name, int is_dir, uint64_t size, unsigned mode,
                     uint64_t mtime);

/* `root` (a folder) as a .tar stream on `fd`: its own name first, then everything below it on the same
 * device, depth first. Links, devices, other mounts, and paths `skip` refuses (may be NULL) are left
 * out, and so are entries that cannot be opened. `root` should be canonical (skip then sees canonical
 * paths: nothing below it is reached through a link). A file that shrinks while it is read is padded with
 * zeros to the size its header gave. 0 when the archive was sent in full, -1 when the client went away
 * or the root cannot be read. `buf`/`cap` (>= 64 KiB) is the caller's heap buffer. */
int wf_send_tar(int fd, const char *root, int (*skip)(const char *path, void *ctx), void *skip_ctx, unsigned idle_ms,
                uint8_t *buf, size_t cap, uint64_t *sent);

/* wf_send_tar as the body of an HTTP/1.1 reply. With `chunked` the archive goes out in chunks
 * ("Transfer-Encoding: chunked" is the caller's header) and the closing chunk is written only when
 * all of it was sent. A folder download has no Content-Length, and a body that simply ends when the
 * connection closes looks complete to a browser: a download cut by a read error on the drive was
 * saved as a finished .tar. `sent` counts the archive's bytes, not the chunk framing. */
int wf_send_tar_http(int fd, const char *root, int (*skip)(const char *path, void *ctx), void *skip_ctx,
                     unsigned idle_ms, uint8_t *buf, size_t cap, uint64_t *sent, int chunked);

#endif
