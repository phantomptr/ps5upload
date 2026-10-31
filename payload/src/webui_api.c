/* The console web UI's JSON API: see include/webui_api.h.
 *
 * Each route decodes its JSON body (webui_codec.c), encodes the AVA1 request into a buffer,
 * dispatches it through ava1_payload_rpc (the AVA1 server's own entry: data plane first, then
 * the management table with its per-call credential/breadcrumb hooks), and turns the reply
 * back into JSON. Routes run on the web server's worker threads (256 KiB stacks): every reply
 * buffer is on the heap. */
#include "webui_api.h"

#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

#include "archive_extract.h"
#include "ava1_data.h"
#include "ava1_app_gen.h"
#include "ava1_glue.h"
#include "ava1_op.h"
#include "ava1_server.h"
#include "ava1_wire.h"
#include "config.h"
#include "files_handoff.h"
#include "files_prefs.h"
#include "fs_jobs.h"
#include "hw_info.h"
#include "path_policy.h"
#include "proc_identity.h"
#include "smbd_proto.h"
#include "ps5_firmware.h"
#include "storage_cleanup.h"
#include "webui_cheats_store.h"
#include "webui_codec.h"
#include "webui_files.h"
#include "webui_inputlog.h"
#include "webui_offline.h"
#include "webui_pkg.h"
#include "webui_page.h"
#include "webui_server.h"
#include "webui_settings.h"
#include "webui_smb.h"
#include "webui_strings.h"
#include "webui_tile.h"

#define API_OUT_MAX (256u * 1024u)   /* an AVA1 reply's ceiling (the server's RPC_OUT_MAX) */
#define API_LIST_JSON (64u * 1024u)  /* one fs.list page as JSON: entries past it page on */
#define API_JOBS_JSON (16u * 1024u)  /* AVA1_MAX_JOBS (32) entries of ~260 bytes */
#define API_STATUS_JSON (8u * 6u * 1024u) /* one job status: a result of WEBUI_JOB_RESULT_MAX, escaped */
#define XPASS_MAX (512u)            /* an archive password, NUL included */
#define API_PATH (1024u)             /* a path, NUL included (the helper's FS_PATH_MAX) */
#define API_READ_MAX (1024u * 1024u) /* /api/fs/read's longest answer */
#define API_LIST_MAX 256u            /* fs.list entries per call (the helper's own ceiling) */
#define API_CAUSE_MAX 200u
#define API_UPLOAD_ROOTS 8u           /* running uploads named in /api/jobs "uploading" */
#define API_UPLOADS_JSON (API_UPLOAD_ROOTS * (6u * API_PATH + 3u) + 32u)

/* The console page's identity: the owner of the jobs it starts (constraints: fixed value). */
static const uint8_t k_console_id[32] = "ps5upload-console-webui";

/* ---- replies ---- */

static void send_json(int fd, int status, const char *json, size_t len) {
    webui_send(fd, status, "application/json", json, len);
}

/* {"error":"<token>"} with the token escaped (a cause can carry any bytes). */
static void send_error(int fd, int http, const char *token, size_t len) {
    char buf[64 + 6 * API_CAUSE_MAX];
    size_t at;
    if (len > API_CAUSE_MAX) len = API_CAUSE_MAX;
    at = (size_t)snprintf(buf, sizeof buf, "{\"error\":\"");
    at += webui_json_escape(buf + at, sizeof buf - at, token, len);
    /* escape returns the full length: clamp before any `sizeof buf - at` (cannot trigger: 6 bytes
     * per input byte at most, and the buffer holds that) */
    if (at + 3 > sizeof buf) at = sizeof buf - 3;
    memcpy(buf + at, "\"}", 3);
    send_json(fd, http, buf, at + 2);
}

static void send_bad(int fd, const char *token) { send_error(fd, 400, token, strlen(token)); }

static const char *status_token(int st) {
    switch (st) {
    case AVA1_ERR_PROTOCOL: return "bad_request";
    case AVA1_ERR_UNKNOWN_METHOD: return "unavailable";
    case AVA1_ERR_BUSY: return "busy";
    case AVA1_ERR_PATH: return "path_not_allowed";
    case AVA1_ERR_NO_SPACE: return "no_space";
    case AVA1_ERR_UNKNOWN_JOB: return "unknown_job";
    case AVA1_ERR_IO: return "io_error";
    case AVA1_ERR_EXISTS: return "exists";
    case AVA1_ERR_CANCELLED: return "cancelled";
    case AVA1_ERR_CROSS_DEVICE: return "cross_mount";
    default: return "internal";
    }
}

/* An AVA1 error as HTTP: the helper's cause when it gave one (the token inside a legacy
 * {"ok":false,"err":..} body), else a name for the status. A cross-device move is always
 * 409 {"error":"cross_mount"}: the page then copies with move set. */
static void send_status_error(int fd, int st, const uint8_t *cause, size_t len) {
    char c[API_CAUSE_MAX + 1], tok[API_CAUSE_MAX + 1];
    const char *use = c;
    if (st == AVA1_ERR_CROSS_DEVICE) {
        send_error(fd, 409, "cross_mount", 11);
        return;
    }
    if (len > API_CAUSE_MAX) len = API_CAUSE_MAX;
    if (cause && len) memcpy(c, cause, len);
    c[len] = '\0';
    if (len && c[0] == '{' &&
        (webui_json_str(c, len, "err", tok, sizeof tok) == 1 || webui_json_str(c, len, "error", tok, sizeof tok) == 1 ||
         webui_json_str(c, len, "reason", tok, sizeof tok) == 1) &&
        tok[0])
        use = tok;
    if (!use[0]) use = status_token(st);
    send_error(fd, webui_http_status_for(st, c), use, strlen(use));
}

/* ---- dispatch ---- */

/* One AVA1 call into `out` (API_OUT_MAX bytes). */
static int call(uint16_t method, const uint8_t *req, size_t req_len, uint8_t *out, size_t *out_len) {
    int st;
    *out_len = 0;
    st = ava1_payload_rpc(method, req, (uint32_t)req_len, out, API_OUT_MAX, out_len);
    if (*out_len > API_OUT_MAX) {
        *out_len = 0;
        st = AVA1_ERR_INTERNAL;
    }
    return st;
}

/* A MgmtText method: `body` (may be empty: the request is then empty, as the engine sends it) in,
 * the reply text out (a pointer into `out`). */
static int call_text_more(uint16_t method, const char *body, size_t len, uint8_t *out, size_t *out_len,
                          const uint8_t **text, size_t *text_len, int *more);

static int call_text(uint16_t method, const char *body, size_t len, uint8_t *out, size_t *out_len, const uint8_t **text,
                     size_t *text_len) {
    return call_text_more(method, body, len, out, out_len, text, text_len, NULL);
}

/* call_text, and *more (when given) set when a paged method's reply has entries after it. */
static int call_text_more(uint16_t method, const char *body, size_t len, uint8_t *out, size_t *out_len,
                          const uint8_t **text, size_t *text_len, int *more) {
    uint8_t *req = NULL;
    size_t req_len = 0;
    ava1_mgmt_text_t m;
    int st;
    *text = NULL;
    *text_len = 0;
    if (more) *more = 0;
    if (len) {
        ava1_w_t w;
        memset(&m, 0, sizeof m);
        m.body = (const uint8_t *)body;
        m.body_len = (uint32_t)len;
        req = malloc(len + 16);
        if (!req) return AVA1_ERR_INTERNAL;
        ava1_w_init(&w, req, len + 16);
        if (ava1_mgmt_text_encode(&m, &w) != 0) {
            free(req);
            return AVA1_ERR_INTERNAL;
        }
        req_len = w.len;
    }
    st = call(method, req, req_len, out, out_len);
    free(req);
    if (st != AVA1_STATUS_OK) return st;
    if (ava1_mgmt_text_decode(out, *out_len, &m) != 0) {
        *out_len = 0;
        return AVA1_ERR_INTERNAL;
    }
    *text = m.body;
    *text_len = m.body_len;
    if (more) *more = m.has_more && m.more;
    return AVA1_STATUS_OK;
}

/* ---- request fields ---- */

/* A required non-empty string field; on failure answers 400 bad_<key> and returns -1. */
static int need_str(int fd, const char *b, size_t n, const char *key, char *out, size_t cap) {
    char tok[32];
    if (webui_json_str(b, n, key, out, cap) == 1 && out[0]) return 0;
    snprintf(tok, sizeof tok, "bad_%s", key);
    send_bad(fd, tok);
    return -1;
}

/* A required path: need_str, and never the AVA1 trust store or anything below it (the helper's
 * write policy refuses it already; this keeps the page from listing, stat'ing or reading it too).
 * On failure answers 400 / 403 and returns -1. */
static int need_path(int fd, const char *b, size_t n, const char *key, char *out, size_t cap) {
    if (need_str(fd, b, n, key, out, cap) != 0) return -1;
    if (!path_in_protected(out)) return 0;
    send_error(fd, 403, "path_not_allowed", 16);
    return -1;
}

/* An optional unsigned field: `def` when absent; on a bad value answers 400 bad_<key>, -1. */
static int opt_u64(int fd, const char *b, size_t n, const char *key, uint64_t def, uint64_t max, uint64_t *out) {
    char tok[32];
    int rc = webui_json_u64(b, n, key, out);
    if (rc == 0) {
        *out = def;
        return 0;
    }
    if (rc == 1 && *out <= max) return 0;
    snprintf(tok, sizeof tok, "bad_%s", key);
    send_bad(fd, tok);
    return -1;
}

static const char *kind_name(uint8_t k) {
    switch (k) {
    case AVA1_ENTRY_FILE: return "file";
    case AVA1_ENTRY_DIR: return "dir";
    case AVA1_ENTRY_LINK: return "link";
    default: return "other";
    }
}

/* ---- routes ---- */

typedef void (*api_fn)(const char *b, size_t n, uint8_t *out, int fd);

/* snprintf and webui_json_escape return the length they WANTED to write: after a truncation *at
 * would point past the buffer and `cap - *at` would wrap. Keep it on the NUL instead. */
static void clamp_at(size_t *at, size_t cap) {
    if (cap && *at > cap - 1) *at = cap - 1;
}

/* Appends printf output to buf at *at, never past cap (a truncated append is cut short). */
static void put_fmt(char *buf, size_t cap, size_t *at, const char *fmt, ...) __attribute__((format(printf, 4, 5)));
static void put_fmt(char *buf, size_t cap, size_t *at, const char *fmt, ...) {
    va_list ap;
    int k;
    clamp_at(at, cap);
    if (*at + 1 >= cap) return;
    va_start(ap, fmt);
    k = vsnprintf(buf + *at, cap - *at, fmt, ap);
    va_end(ap);
    if (k > 0) *at += (size_t)k;
    clamp_at(at, cap);
}

/* Appends `"key":"<escaped s>"` or `"key":null` (s NULL) to buf at *at. */
static void put_str(char *buf, size_t cap, size_t *at, const char *key, const char *s) {
    if (!s) {
        put_fmt(buf, cap, at, "\"%s\":null", key);
        return;
    }
    put_fmt(buf, cap, at, "\"%s\":\"", key);
    if (*at + 1 >= cap) return;
    *at += webui_json_escape(buf + *at, cap - *at, s, strlen(s));
    clamp_at(at, cap);
    put_fmt(buf, cap, at, "\"");
}

/* NODE_STATUS + HW_INFO (model) + HW_TEMPS + PROC_FOCUS. Each call is in-process and bounded by
 * its own handler; one that fails leaves its fields null instead of failing the route. */
static void r_status(const char *b, size_t n, uint8_t *out, int fd) {
    char version[96], firmware[64], model[128], title[16];
    const uint8_t *t;
    size_t ol, tl, at = 0;
    long soc = 0, cpu = 0, temp = 0;
    uint32_t app_id = 0;
    int have_ver = 0, have_model = 0, have_temp = 0, running = 0;
    (void)b;
    (void)n;
    if (call(AVA1_METHOD_NODE_STATUS, NULL, 0, out, &ol) == AVA1_STATUS_OK) {
        ava1_node_status_t ns;
        if (ava1_node_status_decode(out, ol, &ns) == 0) {
            char kernel[320];
            size_t vl = ns.version_len < sizeof version - 1 ? ns.version_len : sizeof version - 1;
            size_t kl = ns.ps5_kernel_len < sizeof kernel - 1 ? ns.ps5_kernel_len : sizeof kernel - 1;
            memcpy(version, ns.version, vl);
            version[vl] = '\0';
            memcpy(kernel, ns.ps5_kernel, kl);
            kernel[kl] = '\0';
            ps5_firmware_from_kernel(kernel, firmware, sizeof firmware);
            have_ver = 1;
        }
    }
    if (call_text(AVA1_METHOD_HW_INFO, NULL, 0, out, &ol, &t, &tl) == AVA1_STATUS_OK)
        have_model = webui_kv_str((const char *)t, tl, "model", model, sizeof model) == 1 && model[0];
    if (call_text(AVA1_METHOD_HW_TEMPS, NULL, 0, out, &ol, &t, &tl) == AVA1_STATUS_OK) {
        /* 0 means "no reading" (hw_info.c): the SoC junction sensor first, else the CPU's */
        if (webui_kv_int((const char *)t, tl, "soc_temp", &soc) == 1 && soc > 0) temp = soc;
        else if (webui_kv_int((const char *)t, tl, "cpu_temp", &cpu) == 1 && cpu > 0) temp = cpu;
        have_temp = temp > 0;
    }
    if (call_text(AVA1_METHOD_PROC_FOCUS, NULL, 0, out, &ol, &t, &tl) == AVA1_STATUS_OK)
        running = webui_focus_running((const char *)t, tl, title, sizeof title, &app_id) == 1;
    /* every string is copied out of `out` above: it is the JSON buffer now */
    {
        char *j = (char *)out;
        size_t cap = API_OUT_MAX;
        at = 0;
        put_fmt(j, cap, &at, "{");
        put_str(j, cap, &at, "version", have_ver ? version : NULL);
        put_fmt(j, cap, &at, ",");
        put_str(j, cap, &at, "firmware", have_ver ? firmware : NULL);
        put_fmt(j, cap, &at, ",");
        put_str(j, cap, &at, "model", have_model ? model : NULL);
        if (have_temp) put_fmt(j, cap, &at, ",\"temp_c\":%ld,\"running\":", temp);
        else put_fmt(j, cap, &at, ",\"temp_c\":null,\"running\":");
        if (running) {
            put_fmt(j, cap, &at, "{");
            put_str(j, cap, &at, "title_id", title);
            put_fmt(j, cap, &at, ",\"app_id\":%lu,\"name\":null}", (unsigned long)app_id);
        } else {
            put_fmt(j, cap, &at, "null");
        }
        put_fmt(j, cap, &at, "}");
        send_json(fd, 200, j, at);
    }
}

/* The console's users ({"foreground":id,"users":[{"id","name","foreground"}]}, USER_LIST): the Saves
 * screen names each /user/home/<id as %08x> folder by them. */
static void r_users(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_USER_LIST, NULL, 0, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    send_json(fd, 200, (const char *)t, tl);
}

static void r_volumes(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_FS_VOLUMES, NULL, 0, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    send_json(fd, 200, (const char *)t, tl);
}

static void r_fs_list(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], req[API_PATH + 32];
    uint64_t offset, limit;
    ava1_fs_list_t q;
    ava1_w_t w;
    size_t ol;
    char *json;
    int st;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (opt_u64(fd, b, n, "offset", 0, UINT32_MAX, &offset) != 0) return;
    if (opt_u64(fd, b, n, "limit", API_LIST_MAX, UINT64_MAX, &limit) != 0) return;
    if (limit == 0 || limit > API_LIST_MAX) limit = API_LIST_MAX;
    memset(&q, 0, sizeof q);
    q.path = (const uint8_t *)path;
    q.path_len = (uint16_t)strlen(path);
    q.offset = (uint32_t)offset;
    q.limit = (uint16_t)limit;
    ava1_w_init(&w, (uint8_t *)req, sizeof req);
    if (ava1_fs_list_encode(&q, &w) != 0) {
        send_bad(fd, "bad_path");
        return;
    }
    st = call(AVA1_METHOD_FS_LIST, (const uint8_t *)req, w.len, out, &ol);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    json = malloc(API_LIST_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_list_to_json(out, ol, json, API_LIST_JSON) < 0) send_error(fd, 500, "bad_list_reply", 14);
    else send_json(fd, 200, json, strlen(json));
    free(json);
}

static void r_fs_stat(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], req[API_PATH + 16], json[320];
    ava1_fs_path_t q;
    ava1_fs_stat_t s;
    struct stat ls;
    ava1_w_t w;
    size_t ol;
    int st, k;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    memset(&q, 0, sizeof q);
    q.path = (const uint8_t *)path;
    q.path_len = (uint16_t)strlen(path);
    ava1_w_init(&w, (uint8_t *)req, sizeof req);
    if (ava1_fs_path_encode(&q, &w) != 0) {
        send_bad(fd, "bad_path");
        return;
    }
    st = call(AVA1_METHOD_FS_STAT, (const uint8_t *)req, w.len, out, &ol);
    if (st != AVA1_STATUS_OK) {
        /* "missing_ok":true (the page probing for a free name): a path that is not there is an
         * answer, 200 {"missing":true}, not a 404 the browser logs as an error. */
        int missing_ok = 0;
        char c[API_CAUSE_MAX + 1];
        size_t cl = ol > API_CAUSE_MAX ? API_CAUSE_MAX : ol;
        memcpy(c, out, cl);
        c[cl] = '\0';
        if (webui_json_bool(b, n, "missing_ok", &missing_ok) == 1 && missing_ok && webui_http_status_for(st, c) == 404) {
            send_json(fd, 200, "{\"missing\":true}", 16);
            return;
        }
        send_status_error(fd, st, out, ol);
        return;
    }
    if (ava1_fs_stat_decode(out, ol, &s) != 0) {
        send_error(fd, 500, "bad_stat_reply", 14);
        return;
    }
    /* The entry's identity (device + inode, as strings: an inode can pass 2^53) so the page can tell
     * "the item itself" from a different entry by more than its spelling: exFAT USB is
     * case-insensitive, so /mnt/usb0/Games and /mnt/usb0/games are one folder. stat(), like
     * FS_STAT above (which follows links): a symlink to the destination is the destination, so a
     * paste of that link into it is seen as "into itself". The path is the one FS_STAT accepted. */
    if (stat(path, &ls) == 0)
        /* mtime_ns: the text editor's save precondition (webui_files.h) catches a same-second change too */
        k = snprintf(json, sizeof json,
                     "{\"kind\":\"%s\",\"size\":%llu,\"mtime\":%llu,\"mode\":%lu,\"dev\":\"%llu\",\"ino\":\"%llu\","
                     "\"mtime_ns\":%lu}",
                     kind_name(s.kind), (unsigned long long)s.size, (unsigned long long)s.mtime, (unsigned long)s.mode,
                     (unsigned long long)ls.st_dev, (unsigned long long)ls.st_ino, (unsigned long)wf_mtime_ns(&ls));
    else
        k = snprintf(json, sizeof json, "{\"kind\":\"%s\",\"size\":%llu,\"mtime\":%llu,\"mode\":%lu}", kind_name(s.kind),
                     (unsigned long long)s.size, (unsigned long long)s.mtime, (unsigned long)s.mode);
    if (k < 0 || (size_t)k >= sizeof json) {
        send_error(fd, 500, "bad_stat_reply", 14);
        return;
    }
    send_json(fd, 200, json, (size_t)k);
}

static void r_fs_mkdir(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], req[API_PATH + 32];
    ava1_fs_mkdir_t q;
    ava1_w_t w;
    size_t ol;
    int st;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    memset(&q, 0, sizeof q);
    q.path = (const uint8_t *)path;
    q.path_len = (uint16_t)strlen(path);
    q.mode = 0777;  /* the helper writes 0777 whatever is asked (game files must stay launchable) */
    q.parents = 1;  /* as the engine sends it (mgmt_convert.rs) */
    ava1_w_init(&w, (uint8_t *)req, sizeof req);
    if (ava1_fs_mkdir_encode(&q, &w) != 0) {
        send_bad(fd, "bad_path");
        return;
    }
    st = call(AVA1_METHOD_FS_MKDIR, (const uint8_t *)req, w.len, out, &ol);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, "{}", 2);
}

/* One FS_RENAME (never overwriting): the AVA1 status, the cause in out/ol. -1: does not encode. */
static int rename_once(const char *from, const char *to, uint8_t *out, size_t *ol) {
    char req[2 * API_PATH + 32];
    ava1_fs_rename_t q;
    ava1_w_t w;
    memset(&q, 0, sizeof q);
    q.from = (const uint8_t *)from;
    q.from_len = (uint16_t)strlen(from);
    q.to = (const uint8_t *)to;
    q.to_len = (uint16_t)strlen(to);
    q.overwrite = 0;
    ava1_w_init(&w, (uint8_t *)req, sizeof req);
    if (ava1_fs_rename_encode(&q, &w) != 0) return -1;
    return call(AVA1_METHOD_FS_RENAME, (const uint8_t *)req, w.len, out, ol);
}

/* A rename never crosses a mount (it would panic the kernel): the helper refuses with
 * ERR_CROSS_DEVICE, answered 409 cross_mount, and the page copies with move set instead. An
 * existing target is refused (409 exists): the page does not overwrite by renaming.
 * A change of letter case only ("a.txt" -> "A.txt") on a case-insensitive drive (exFAT USB/
 * images) finds its target "existing" as the source itself (same device + inode): it goes
 * through a temporary name in the same folder (webui_rename_detour), so it never leaves the
 * mount; a failed second step puts the source name back. */
static void r_fs_rename(const char *b, size_t n, uint8_t *out, int fd) {
    char from[API_PATH], to[API_PATH], tmp[API_PATH];
    struct stat fs_, ts_;
    size_t ol = 0;
    int st;
    if (need_path(fd, b, n, "from", from, sizeof from) != 0) return;
    if (need_path(fd, b, n, "to", to, sizeof to) != 0) return;
    if (lstat(from, &fs_) == 0 && lstat(to, &ts_) == 0 && fs_.st_dev == ts_.st_dev && fs_.st_ino == ts_.st_ino) {
        static unsigned seq;
        unsigned salt = ((unsigned)time(NULL) << 8) ^ (unsigned)__atomic_add_fetch(&seq, 1, __ATOMIC_RELAXED);
        struct stat xs;
        int tries;
        for (tries = 0; tries < 8; tries++, salt += 0x9e37u) {
            if (webui_rename_detour(from, to, salt, tmp, sizeof tmp) != 0) break;
            if (lstat(tmp, &xs) != 0) break; /* free */
            tmp[0] = '\0';
        }
        if (tmp[0] && tries < 8) {
            st = rename_once(from, tmp, out, &ol);
            if (st < 0) {
                send_bad(fd, "bad_path");
                return;
            }
            if (st != AVA1_STATUS_OK) {
                send_status_error(fd, st, out, ol);
                return;
            }
            st = rename_once(tmp, to, out, &ol);
            if (st == AVA1_STATUS_OK) {
                send_json(fd, 200, "{}", 2);
                return;
            }
            {
                /* Put it back under its old name; the second step's cause is the answer (kept
                 * aside: the put-back call writes `out`). */
                uint8_t cause[256];
                size_t cl = ol < sizeof cause ? ol : sizeof cause;
                size_t bl = 0;
                if (cl) memcpy(cause, out, cl);
                (void)rename_once(tmp, from, out, &bl);
                if (st < 0) send_bad(fd, "bad_path");
                else send_status_error(fd, st, cause, cl);
            }
            return;
        }
        /* Not one folder (a hard link elsewhere): the plain rename below refuses it (exists). */
    }
    st = rename_once(from, to, out, &ol);
    if (st < 0) send_bad(fd, "bad_path");
    else if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, "{}", 2);
}

/* Raw bytes, at most 1 MiB, in fs.read calls of up to AVA1_FS_READ_MAX each. */
static void r_fs_read(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], req[API_PATH + 32];
    uint64_t offset, len;
    uint8_t *data;
    size_t got = 0;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (opt_u64(fd, b, n, "offset", 0, (uint64_t)INT64_MAX, &offset) != 0) return;
    if (opt_u64(fd, b, n, "len", API_READ_MAX, UINT64_MAX, &len) != 0) return;
    if (len > API_READ_MAX) len = API_READ_MAX;
    data = malloc(len ? (size_t)len : 1);
    if (!data) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    while (got < len) {
        ava1_fs_read_t q;
        ava1_fs_read_result_t r;
        ava1_w_t w;
        size_t ol;
        uint64_t ask = len - got;
        int st;
        if (ask > AVA1_FS_READ_MAX) ask = AVA1_FS_READ_MAX;
        memset(&q, 0, sizeof q);
        q.path = (const uint8_t *)path;
        q.path_len = (uint16_t)strlen(path);
        q.offset = offset + got;
        q.len = (uint32_t)ask;
        q.flags = 0; /* never the unsafe system-partition read */
        ava1_w_init(&w, (uint8_t *)req, sizeof req);
        if (ava1_fs_read_encode(&q, &w) != 0) {
            free(data);
            send_bad(fd, "bad_path");
            return;
        }
        st = call(AVA1_METHOD_FS_READ, (const uint8_t *)req, w.len, out, &ol);
        if (st != AVA1_STATUS_OK) {
            free(data);
            send_status_error(fd, st, out, ol);
            return;
        }
        if (ava1_fs_read_result_decode(out, ol, &r) != 0 || r.data_len > ask) {
            free(data);
            send_error(fd, 500, "bad_read_reply", 14);
            return;
        }
        memcpy(data + got, r.data, r.data_len);
        got += r.data_len;
        if (r.eof || r.data_len == 0) break;
    }
    /* nosniff is on every response (webui_http_head); the sandbox keeps a file opened as a document
     * from running script in this origin, whatever its bytes. */
    webui_send_ex(fd, 200, webui_mime_for(path), data, got, "Content-Security-Policy: sandbox\r\n");
    free(data);
}

static void send_job(int fd, const uint8_t id[16]) {
    char hex[33], json[48];
    int k;
    webui_hex16(id, hex);
    k = snprintf(json, sizeof json, "{\"job\":\"%s\"}", hex);
    send_json(fd, 200, json, (size_t)k);
}

static void r_fs_copy(const char *b, size_t n, uint8_t *out, int fd) {
    char src[API_PATH], dest[API_PATH];
    uint8_t req[2 * API_PATH + 64];
    ava1_job_copy_t q;
    ava1_w_t w;
    size_t ol;
    int move = 0, overwrite = 0, st, rc;
    if (need_path(fd, b, n, "src", src, sizeof src) != 0) return;
    if (need_path(fd, b, n, "dest", dest, sizeof dest) != 0) return;
    rc = webui_json_bool(b, n, "move", &move);
    if (rc < 0) {
        send_bad(fd, "bad_move");
        return;
    }
    /* "overwrite" (off unless sent true): an existing destination FILE is replaced (per file,
     * via .part + rename); on a folder the job merges, so the page only sends it for files
     * (Replace in a paste conflict). Without it the job refuses an existing dest (409). */
    rc = webui_json_bool(b, n, "overwrite", &overwrite);
    if (rc < 0) {
        send_bad(fd, "bad_overwrite");
        return;
    }
    memset(&q, 0, sizeof q);
    arc4random_buf(q.job_id, sizeof q.job_id); /* a fresh job: a known id would resume that job instead */
    q.src = (const uint8_t *)src;
    q.src_len = (uint16_t)strlen(src);
    q.dest = (const uint8_t *)dest;
    q.dest_len = (uint16_t)strlen(dest);
    q.flags = (move ? (uint32_t)AVA1_JF_MOVE : 0u) | (overwrite ? (uint32_t)AVA1_JF_OVERWRITE : 0u);
    ava1_w_init(&w, req, sizeof req);
    if (ava1_job_copy_encode(&q, &w) != 0) {
        send_bad(fd, "bad_path");
        return;
    }
    st = call(AVA1_METHOD_JOB_COPY, req, w.len, out, &ol);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_job(fd, q.job_id);
}

/* A one-path job (delete, chmod, hash, crc32, unpack): op's args are {"path":...} as the engine sends
 * them, then ,"name":... when `name` is set (unpack's Keep both), then `tail` (e.g. ,"mode":"0777"). */
static void start_path_job(int fd, uint8_t *out, int op, const char *path, const char *name, const char *tail) {
    char *args;
    uint8_t *req;
    size_t args_cap = 6 * API_PATH + 6 * 256 + 64, ol;
    ava1_job_run_t q;
    ava1_w_t w;
    int st, len;
    args = malloc(args_cap);
    req = malloc(args_cap + 64);
    if (!args || !req) {
        free(args);
        free(req);
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    /* fs_jobs.c reads it with fsj_json_str (\" \\ \uXXXX understood) */
    len = webui_path_job_args(args, args_cap, path, name, tail);
    memset(&q, 0, sizeof q);
    arc4random_buf(q.job_id, sizeof q.job_id);
    q.op = (uint8_t)op;
    q.args = (const uint8_t *)args;
    q.args_len = len > 0 ? (uint32_t)len : 0;
    ava1_w_init(&w, req, args_cap + 64);
    if (len <= 0 || ava1_job_run_encode(&q, &w) != 0) {
        free(args);
        free(req);
        send_bad(fd, "bad_path");
        return;
    }
    st = call(AVA1_METHOD_JOB_RUN, req, w.len, out, &ol);
    free(args);
    free(req);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_job(fd, q.job_id);
}

static void run_path_job(const char *b, size_t n, uint8_t *out, int fd, int op, const char *tail) {
    char path[API_PATH];
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    start_path_job(fd, out, op, path, NULL, tail);
}

static void r_fs_delete(const char *b, size_t n, uint8_t *out, int fd) {
    run_path_job(b, n, out, fd, AVA1_JOB_OP_DELETE, "");
}

/* "mode" as the page sends it: octal text of 1-4 digits, at most 0777 (no setuid/setgid/sticky from the
 * page). 1 and *mode set, 0 absent, -1 bad. */
static int opt_mode(const char *b, size_t n, unsigned *mode) {
    char m[8];
    size_t i;
    unsigned v = 0;
    int rc = webui_json_str(b, n, "mode", m, sizeof m);
    if (rc == 0) return 0;
    if (rc < 0 || !m[0] || strlen(m) > 4) return -1;
    for (i = 0; m[i]; i++) {
        if (m[i] < '0' || m[i] > '7') return -1;
        v = v * 8 + (unsigned)(m[i] - '0');
    }
    if (v > 0777) return -1;
    *mode = v;
    return 1;
}

/* Permissions. Without "mode": Fix permissions, the PC app's recursive chmod to 0777 (a job). With
 * "mode" ("0755"): that mode, on the item alone (FS_CHMOD, answered at once with {}) or, with
 * "recursive":true, on everything below it too (the CHMOD_R job, as Fix permissions runs). */
static void r_fs_chmod(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], tail[32], req[API_PATH + 32];
    unsigned mode = 0777;
    int has_mode, recursive = 1, rc;
    has_mode = opt_mode(b, n, &mode);
    if (has_mode < 0) {
        send_bad(fd, "bad_mode");
        return;
    }
    if (!has_mode) {
        run_path_job(b, n, out, fd, AVA1_JOB_OP_CHMOD_R, ",\"mode\":\"0777\"");
        return;
    }
    rc = webui_json_bool(b, n, "recursive", &recursive);
    if (rc < 0) {
        send_bad(fd, "bad_recursive");
        return;
    }
    if (rc == 0) recursive = 0; /* a mode applies to the item alone unless asked */
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (recursive) {
        snprintf(tail, sizeof tail, ",\"mode\":\"%04o\"", mode);
        start_path_job(fd, out, AVA1_JOB_OP_CHMOD_R, path, NULL, tail);
        return;
    }
    {
        ava1_fs_chmod_t q;
        ava1_w_t w;
        size_t ol;
        int st;
        memset(&q, 0, sizeof q);
        q.path = (const uint8_t *)path;
        q.path_len = (uint16_t)strlen(path);
        q.mode = mode;
        ava1_w_init(&w, (uint8_t *)req, sizeof req);
        if (ava1_fs_chmod_encode(&q, &w) != 0) {
            send_bad(fd, "bad_path");
            return;
        }
        st = call(AVA1_METHOD_FS_CHMOD, (const uint8_t *)req, w.len, out, &ol);
        if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
        else send_json(fd, 200, "{}", 2);
    }
}

/* Verify: BLAKE3 (HASH) and CRC32 of one file, each a job; the result is read with /api/jobs/status. */
static void r_fs_hash(const char *b, size_t n, uint8_t *out, int fd) { run_path_job(b, n, out, fd, AVA1_JOB_OP_HASH, ""); }

static void r_fs_crc32(const char *b, size_t n, uint8_t *out, int fd) {
    run_path_job(b, n, out, fd, AVA1_JOB_OP_CRC32, "");
}

/* Decompress a .ffpfsc beside it (FFPFSC_UNPACK). Optional "name": the image's file name instead of
 * the one inside (Keep both); the helper still refuses an existing destination and a full drive. */
static void r_fs_unpack(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], name[256];
    int rc;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    rc = webui_json_str(b, n, "name", name, sizeof name);
    if (rc < 0 || (rc == 1 && !webui_unpack_name_ok(name))) {
        send_bad(fd, "bad_name");
        return;
    }
    start_path_job(fd, out, AVA1_JOB_OP_FFPFSC_UNPACK, path, rc == 1 ? name : NULL, "");
}

/* What a decompress would write, before it starts: {"name","size","need","free"|null,"exists",
 * "compressed"}. A source the unpack would refuse answers its cause (403 path, 404 cannot open, 409 not a usable container). */
static void r_fs_unpack_info(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH];
    fsj_unpack_probe_t pr;
    const char *bad;
    char *j = (char *)out;
    size_t at = 0, cap = API_OUT_MAX;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    bad = fsj_ffpfsc_probe(path, &pr);
    if (bad) {
        int http = strstr(bad, "not_allowed") ? 403 : (strstr(bad, "open_failed") ? 404 : 409);
        send_error(fd, http, bad, strlen(bad));
        return;
    }
    put_fmt(j, cap, &at, "{");
    put_str(j, cap, &at, "name", pr.name);
    put_fmt(j, cap, &at, ",\"size\":%llu,\"need\":%llu,", (unsigned long long)pr.size, (unsigned long long)pr.need);
    if (pr.have_free) put_fmt(j, cap, &at, "\"free\":%llu,", (unsigned long long)pr.free);
    else put_fmt(j, cap, &at, "\"free\":null,");
    put_fmt(j, cap, &at, "\"exists\":%s,\"compressed\":%s}", pr.exists ? "true" : "false",
            pr.compressed ? "true" : "false");
    send_json(fd, 200, j, at);
}

/* Extract an archive on the console (ARCHIVE_EXTRACT, run by the extractor payload): {"path", "dest",
 * optional "password", "merge", "replace"}. The job's result: {"path","files","dirs","bytes","skipped",
 * "unsafe","links"}. */
static void r_fs_extract(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], dest[API_PATH], password[XPASS_MAX], *tail;
    size_t at = 0, cap = 3 * API_PATH + 3 * XPASS_MAX + 64;
    int merge = 0, replace = 0, rc;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (need_path(fd, b, n, "dest", dest, sizeof dest) != 0) return;
    rc = webui_json_str(b, n, "password", password, sizeof password);
    if (rc < 0) {
        send_bad(fd, "bad_password");
        return;
    }
    if (webui_json_bool(b, n, "merge", &merge) < 0 || webui_json_bool(b, n, "replace", &replace) < 0) {
        send_bad(fd, "bad_merge");
        return;
    }
    tail = malloc(cap);
    if (!tail) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    /* archive_extract.c reads it back with fsj_json_str (\" \\ \uXXXX understood). */
    at = (size_t)snprintf(tail, cap, ",\"dest\":\"");
    at += webui_json_escape(tail + at, cap - at, dest, strlen(dest));
    if (at + 16 < cap) at += (size_t)snprintf(tail + at, cap - at, "\"%s", rc == 1 ? ",\"password\":\"" : "");
    if (rc == 1 && at < cap) {
        at += webui_json_escape(tail + at, cap - at, password, strlen(password));
        if (at + 2 < cap) at += (size_t)snprintf(tail + at, cap - at, "\"");
    }
    if (at + 40 < cap)
        at += (size_t)snprintf(tail + at, cap - at, ",\"merge\":%s,\"replace\":%s", merge ? "true" : "false",
                               replace ? "true" : "false");
    else
        at = cap; /* did not fit */
    memset(password, 0, sizeof password);
    if (at >= cap) {
        memset(tail, 0, cap);
        free(tail);
        send_bad(fd, "bad_path");
        return;
    }
    start_path_job(fd, out, AVA1_JOB_OP_ARCHIVE_EXTRACT, path, NULL, tail);
    memset(tail, 0, cap);
    free(tail);
}

/* What an extraction would do, before it starts: {"name","dest","size","volumes","free"|null,"exists",
 * "kind"}. A source it would refuse answers its cause (403 path, 404 cannot open, 409 not an archive). */
static void r_fs_extract_info(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH];
    archive_probe_t pr;
    const char *bad;
    char *j = (char *)out;
    size_t at = 0, cap = API_OUT_MAX;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    bad = archive_extract_probe(path, &pr);
    if (bad) {
        int http = strstr(bad, "not_allowed") ? 403 : (strstr(bad, "open_failed") ? 404 : 409);
        send_error(fd, http, bad, strlen(bad));
        return;
    }
    put_fmt(j, cap, &at, "{");
    put_str(j, cap, &at, "name", pr.name);
    put_fmt(j, cap, &at, ",");
    put_str(j, cap, &at, "dest", pr.dest);
    put_fmt(j, cap, &at, ",\"size\":%llu,\"volumes\":%u,", (unsigned long long)pr.size, pr.volumes);
    if (pr.have_free) put_fmt(j, cap, &at, "\"free\":%llu,", (unsigned long long)pr.free);
    else put_fmt(j, cap, &at, "\"free\":null,");
    put_fmt(j, cap, &at, "\"exists\":%s,\"kind\":\"%s\"}", pr.exists ? "true" : "false", pr.kind);
    send_json(fd, 200, j, at);
}

static int is_mine(const uint8_t id[16], void *ctx) {
    (void)ctx;
    return ava1_jobs_owner_is(id, k_console_id);
}

/* "uploading": the destinations of running uploads (a PC's), for the Files badge. Left out when
 * the list cannot be read (the jobs answer stands on its own). */
static void add_uploads(char *json, size_t cap) {
    char (*roots)[AVA1_MAX_PATH + 1] = malloc(API_UPLOAD_ROOTS * sizeof *roots);
    const char *ptrs[API_UPLOAD_ROOTS];
    size_t len = strlen(json);
    int n, i;
    if (!roots) return;
    n = ava1_jobs_upload_roots(roots, API_UPLOAD_ROOTS);
    for (i = 0; i < n; i++) ptrs[i] = roots[i];
    if (len && json[len - 1] == '}') (void)files_uploads_json(ptrs, n, json + len - 1, cap - (len - 1));
    free(roots);
}

static void r_jobs(const char *b, size_t n, uint8_t *out, int fd) {
    size_t ol = 0;
    char *json;
    int st;
    (void)b;
    (void)n;
    st = ava1_jobs_list_all(k_console_id, out, API_OUT_MAX, &ol);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, NULL, 0);
        return;
    }
    json = malloc(API_JOBS_JSON + API_UPLOADS_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_jobs_to_json(out, ol, is_mine, NULL, json, API_JOBS_JSON) < 0) {
        send_error(fd, 500, "bad_jobs_reply", 14);
    } else {
        /* The page has now seen how its finished jobs ended: release them (after a short grace, so
         * a second open page still sees them) instead of letting each hold one of the 32 job slots
         * every owner shares for the full park age. A paired PC's jobs are left alone. */
        ava1_jobs_release_seen(k_console_id);
        add_uploads(json, API_JOBS_JSON + API_UPLOADS_JSON);
        send_json(fd, 200, json, strlen(json));
    }
    free(json);
}

/* Any job, a paired PC's included (the console's own screen may stop what runs on it). May block
 * on a socket write to that job's session: fine here, on a web worker. */
static void r_jobs_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    char hex[40];
    uint8_t id[16];
    int st;
    (void)out;
    if (webui_json_str(b, n, "job", hex, sizeof hex) != 1 || webui_parse_hex16(hex, id) != 0) {
        send_bad(fd, "bad_job");
        return;
    }
    st = ava1_job_cancel_any(id);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, NULL, 0);
    else send_json(fd, 200, "{}", 2);
}

/* One of the page's own operation jobs, read as JOB_STATUS (progress, its message, and once it ended
 * well the operation's result: a hash, a checksum, an unpacked image). A finished job read here is
 * delivered: released after the grace, like the engine's job.status. Another owner's job is 404. */
static void r_jobs_status(const char *b, size_t n, uint8_t *out, int fd) {
    char hex[40], *json;
    uint8_t id[16], req[32];
    ava1_job_ref_t q;
    ava1_w_t w;
    size_t ol;
    int st;
    if (webui_json_str(b, n, "job", hex, sizeof hex) != 1 || webui_parse_hex16(hex, id) != 0) {
        send_bad(fd, "bad_job");
        return;
    }
    memset(&q, 0, sizeof q);
    memcpy(q.job_id, id, 16);
    ava1_w_init(&w, req, sizeof req);
    if (ava1_job_ref_encode(&q, &w) != 0) {
        send_bad(fd, "bad_job");
        return;
    }
    st = call(AVA1_METHOD_JOB_STATUS, req, w.len, out, &ol);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    json = malloc(API_STATUS_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_job_status_to_json(out, ol, json, API_STATUS_JSON) != 0) send_error(fd, 500, "bad_status_reply", 16);
    else send_json(fd, 200, json, strlen(json));
    free(json);
}

/* APP_LIFECYCLE kill, as the engine sends it: {"action":"kill","app_id":N}. */
static void r_app_close(const char *b, size_t n, uint8_t *out, int fd) {
    char req[64];
    const uint8_t *t;
    size_t ol, tl;
    uint64_t app_id;
    int k, st;
    if (webui_json_u64(b, n, "app_id", &app_id) != 1 || app_id == 0 || app_id > UINT32_MAX) {
        send_bad(fd, "bad_app_id");
        return;
    }
    k = snprintf(req, sizeof req, "{\"action\":\"kill\",\"app_id\":%lu}", (unsigned long)app_id);
    st = call_text(AVA1_METHOD_APP_LIFECYCLE, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, (const char *)t, tl);
}

/* ---- packages and payloads on the console (webui_pkg.c) ---- */

#define API_PKG_JSON 4096u

static void r_pkg_info(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], *json;
    const char *cause = "internal";
    int st;
    (void)out;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    json = malloc(API_PKG_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    st = webui_pkg_info_json(path, json, API_PKG_JSON, &cause);
    if (st == 200) send_json(fd, 200, json, strlen(json));
    else send_error(fd, st, cause, strlen(cause));
    free(json);
}

static void r_pkg_icon(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH];
    const char *cause = "internal";
    uint8_t *png = NULL;
    size_t len = 0;
    int st;
    (void)out;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    st = webui_pkg_icon(path, &png, &len, &cause);
    if (st == 200) webui_send_ex(fd, 200, "image/png", png, len, "Content-Security-Policy: sandbox\r\n");
    else send_error(fd, st, cause, strlen(cause));
    free(png);
}

/* {"path"[,"replace":true][,"remove_after":true]}: checked here first (a full game already installed
 * answers 409 pkg_install_already_installed, so the page can ask before Sony removes it), then the
 * job. remove_after (the Library's copy of a package from a share, in WEBUI_PKG_STAGE_DIR) has the
 * job remove the package like its own staging copy. */
static void r_pkg_install(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], tail[48];
    const char *cause = "internal";
    int replace = 0, remove_after = 0, st;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (webui_json_bool(b, n, "replace", &replace) < 0) {
        send_bad(fd, "bad_replace");
        return;
    }
    if (webui_json_bool(b, n, "remove_after", &remove_after) < 0) {
        send_bad(fd, "bad_remove_after");
        return;
    }
    st = webui_pkg_install_check(path, replace, &cause);
    if (st != 200) {
        send_error(fd, st, cause, strlen(cause));
        return;
    }
    snprintf(tail, sizeof tail, "%s%s", replace ? ",\"replace\":true" : "", remove_after ? ",\"remove_after\":1" : "");
    start_path_job(fd, out, AVA1_JOB_OP_PKG_INSTALL, path, NULL, tail);
}

static void r_payload_load(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], json[48];
    const char *cause = "internal";
    uint64_t sent = 0;
    int st, k;
    (void)out;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    st = webui_payload_load(path, &sent, &cause);
    if (st != 200) {
        send_error(fd, st, cause, strlen(cause));
        return;
    }
    k = snprintf(json, sizeof json, "{\"bytes\":%llu}", (unsigned long long)sent);
    send_json(fd, 200, json, (size_t)k);
}


/* ---- games: the installed titles, through the methods the PC app's Games screen calls ----
 *
 * The page builds the installed list the way the engine does (ps5_apps_installed: /user/appmeta, the
 * app folders, app.db and our registrations), from /api/fs/list and the routes below. Launch, uninstall
 * and hide go to the same handlers the engine calls (APP_LAUNCH, APP_UNREGISTER, APP_ICON_SET); those
 * take the Sony API lock themselves (mgmt_table.def MGMT_SONY; launch_title reads the foreground user
 * under it). The page's own tile is never acted on here (Settings removes it), and the console's own
 * apps (NPXS) are never uninstalled or hidden. */

#define API_APPS_INFO_MAX 32u              /* titles one /api/apps/info asks about */
#define API_APPS_INFO_JSON (128u * 1024u) /* 32 appinfo replies of a few hundred bytes, with headroom */

/* A required title id the Games section may act on (webui_app_refusal); else 400 bad_title_id or
 * 403 own_tile / system_title, and -1. */
static int need_app(int fd, const char *b, size_t n, char *out, size_t cap, int destructive) {
    const char *why;
    if (webui_json_str(b, n, "title_id", out, cap) != 1) out[0] = '\0';
    why = webui_app_refusal(out, destructive);
    if (!why) return 0;
    send_error(fd, strcmp(why, "bad_title_id") == 0 ? 400 : 403, why, strlen(why));
    return -1;
}

/* A helper reply that is already the JSON the page reads. */
static void send_text_reply(int fd, int st, uint8_t *out, size_t ol, const uint8_t *t, size_t tl) {
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else if (!tl || t[0] != '{') send_error(fd, 500, "bad_reply", 9);
    else send_json(fd, 200, (const char *)t, tl);
}

/* {offset?}: one page of app.db's titles (APP_DB_QUERY, paged since #473) as the helper answers it:
 * {"apps":[{"title_id","app_id","name"}],"next":N,"more":bool,...} or {"err":..,"apps":[]}. */
static void r_apps_db(const char *b, size_t n, uint8_t *out, int fd) {
    char req[96];
    const uint8_t *t;
    size_t ol, tl;
    uint64_t offset;
    int k, st;
    if (opt_u64(fd, b, n, "offset", 0, UINT32_MAX, &offset) != 0) return;
    k = snprintf(req, sizeof req, "{\"offset\":%lu,\"limit\":500}", (unsigned long)offset);
    st = call_text(AVA1_METHOD_APP_DB_QUERY, req, (size_t)k, out, &ol, &t, &tl);
    send_text_reply(fd, st, out, ol, t, tl);
}

/* The titles we registered from a folder or an image (APP_LIST): {"more":bool,"reply":{"apps":[..]}}.
 * `more` is set when the list did not fit one reply (the page then knows it is not whole). */
static void r_apps_registered(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl, head;
    int st, more = 0;
    char *json;
    (void)b;
    (void)n;
    st = call_text_more(AVA1_METHOD_APP_LIST, NULL, 0, out, &ol, &t, &tl, &more);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (!tl || t[0] != '{') {
        send_error(fd, 500, "bad_reply", 9);
        return;
    }
    json = malloc(tl + 32);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    head = (size_t)snprintf(json, 32, "{\"more\":%s,\"reply\":", more ? "true" : "false");
    memcpy(json + head, t, tl);
    json[head + tl] = '}';
    send_json(fd, 200, json, head + tl + 1);
    free(json);
}

/* The titles whose home-screen icon is hidden (APP_HIDDEN_LIST): {"ok":true,"hidden":[..]}. */
static void r_apps_hidden(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_APP_HIDDEN_LIST, NULL, 0, out, &ol, &t, &tl);
    send_text_reply(fd, st, out, ol, t, tl);
}

/* {title_ids:[..]} (at most API_APPS_INFO_MAX): each title's appinfo.db fields the PC app's game page
 * reads (APP_INFO_QUERY): {"apps":{"<id>":{"ok":true,"rows":[{"key","val"}]..},...}}. A title the
 * query fails for is left out. */
static void r_apps_info(const char *b, size_t n, uint8_t *out, int fd) {
    const char *arr;
    size_t alen, pos = 0, at = 0;
    char tid[32], req[160];
    char *json;
    unsigned count = 0;
    int first = 1;
    if (webui_json_array(b, n, "title_ids", &arr, &alen) != 1) {
        send_bad(fd, "bad_title_ids");
        return;
    }
    json = malloc(API_APPS_INFO_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    put_fmt(json, API_APPS_INFO_JSON, &at, "{\"apps\":{");
    for (;;) {
        const uint8_t *t;
        size_t ol, tl;
        int rc = webui_json_array_str(arr, alen, &pos, tid, sizeof tid), k, st;
        if (rc == 0) break;
        if (rc < 0 || webui_app_refusal(tid, 0) || ++count > API_APPS_INFO_MAX) {
            free(json);
            send_bad(fd, "bad_title_ids");
            return;
        }
        k = snprintf(req, sizeof req,
                     "{\"title_id\":\"%s\",\"keys\":\"CONTENT_VERSION,APP_VER,#_size,#_last_access_time,#_install_time\"}",
                     tid); /* letters and digits only */
        st = call_text(AVA1_METHOD_APP_INFO_QUERY, req, (size_t)k, out, &ol, &t, &tl);
        if (st != AVA1_STATUS_OK || !tl || t[0] != '{') continue;
        /* the whole reply or nothing: a cut one would not parse */
        if (at + strlen(tid) + tl + 16 >= API_APPS_INFO_JSON) continue;
        put_fmt(json, API_APPS_INFO_JSON, &at, "%s\"%s\":", first ? "" : ",", tid);
        memcpy(json + at, t, tl);
        at += tl;
        json[at] = '\0';
        first = 0;
    }
    put_fmt(json, API_APPS_INFO_JSON, &at, "}}");
    send_json(fd, 200, json, at);
    free(json);
}

/* PROCESS_LIST (the PC app's Processes screen): {"ok":true,"procs":[{pid,name,comm,title_id,app_id,memory_mib,
 * threads,kind,is_self}],...}. In-process sysctl walk, no Sony API. */
static void r_proc_list(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_PROC_PROCESS_LIST, NULL, 0, out, &ol, &t, &tl);
    send_text_reply(fd, st, out, ol, t, tl);
}

/* Whether a game with title id `tid` (other != 0: any other game) runs, from PROCESS_LIST. A list that
 * cannot be read counts as running: a guard never passes on a guess. */
static int game_running(uint8_t *out, const char *tid, int other) {
    const uint8_t *t;
    size_t ol, tl;
    if (call_text(AVA1_METHOD_PROC_PROCESS_LIST, NULL, 0, out, &ol, &t, &tl) != AVA1_STATUS_OK) return 1;
    return webui_proc_game_running((const char *)t, tl, tid, other) != 0;
}

/* {title_id}: APP_LAUNCH as the engine sends it. The ack means Sony took the request, not that the game
 * is up (the page watches the process list for it). Refused (409 other_game_running) while another game
 * runs: a launch over a running game closes it and can fail (game-swap on FW 13.60), so the page asks for
 * a Close first. The same title running is fine: launch_title brings it to the front, once. */
static void r_apps_launch(const char *b, size_t n, uint8_t *out, int fd) {
    char tid[32], req[64];
    const uint8_t *t;
    size_t ol, tl;
    int k, st;
    if (need_app(fd, b, n, tid, sizeof tid, 0) != 0) return;
    if (game_running(out, tid, 1)) {
        send_error(fd, 409, "other_game_running", 18);
        return;
    }
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\"}", tid);
    st = call_text(AVA1_METHOD_APP_LAUNCH, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, "{}", 2);
}

/* {title_id}: APP_UNREGISTER, the PC app's Uninstall. The helper judges the result itself (#476:
 * unregister_outcome.h): {"sony_uninstall_rc":N,"still_present":false} when the title is gone,
 * still_present:true when Sony accepted but its folders are still there (a big game takes minutes:
 * the page keeps checking), an error ("uninstall_refused:0x..", "uninstall_unavailable") otherwise.
 * Refused (409 game_running) while that game runs. */
static void r_apps_uninstall(const char *b, size_t n, uint8_t *out, int fd) {
    char tid[32], req[64];
    const uint8_t *t;
    size_t ol, tl;
    int k, st;
    if (need_app(fd, b, n, tid, sizeof tid, 1) != 0) return;
    if (game_running(out, tid, 0)) {
        send_error(fd, 409, "game_running", 12);
        return;
    }
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\"}", tid);
    st = call_text(AVA1_METHOD_APP_UNREGISTER, req, (size_t)k, out, &ol, &t, &tl);
    send_text_reply(fd, st, out, ol, t, tl);
}

/* {path}: APP_REGISTER, the PC app's Register of a game folder (the Library offers it after it copied
 * a folder dump to the console) -> {"title_id","title_name","used_nullfs"}; a refusal is the
 * helper's register_* token. */
static void r_apps_register(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], *req;
    const uint8_t *t;
    size_t ol, tl, k;
    int st;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    req = malloc(2 * API_PATH + 32);
    if (!req) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    k = (size_t)snprintf(req, 2 * API_PATH + 32, "{\"src_path\":\"");
    k += webui_json_escape(req + k, 2 * API_PATH + 32 - k, path, strlen(path));
    if (k + 3 > 2 * API_PATH + 32) {
        free(req);
        send_bad(fd, "bad_path");
        return;
    }
    memcpy(req + k, "\"}", 3);
    st = call_text(AVA1_METHOD_APP_REGISTER, req, k + 2, out, &ol, &t, &tl);
    free(req);
    send_text_reply(fd, st, out, ol, t, tl);
}

/* {title_id, hidden}: APP_ICON_SET (#407): only app.db's visible flag changes; the title stays
 * installed. A refusal ({"ok":false,"err":..}) is 409 with the helper's reason. */
static void r_apps_icon(const char *b, size_t n, uint8_t *out, int fd) {
    char tid[32], req[96], err[API_CAUSE_MAX];
    const uint8_t *t;
    size_t ol, tl;
    int hidden, ok = 0, k, st;
    if (need_app(fd, b, n, tid, sizeof tid, 1) != 0) return;
    if (webui_json_bool(b, n, "hidden", &hidden) != 1) {
        send_bad(fd, "bad_hidden");
        return;
    }
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\",\"hidden\":\"%s\"}", tid, hidden ? "1" : "0");
    st = call_text(AVA1_METHOD_APP_ICON_SET, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (webui_json_bool((const char *)t, tl, "ok", &ok) == 1 && ok) {
        send_json(fd, 200, hidden ? "{\"hidden\":true}" : "{\"hidden\":false}", hidden ? 15 : 16);
        return;
    }
    if (webui_json_str((const char *)t, tl, "err", err, sizeof err) != 1 || !err[0]) snprintf(err, sizeof err, "icon_failed");
    send_error(fd, 409, err, strlen(err));
}

/* {pid}: PROCESS_KILL (SIGKILL), as the PC app's Processes screen sends it, for a game or a payload only
 * (webui_proc_kill_refusal: never the helper, never a system process, never a pid the list does not
 * show). 409 kill_failed with the errno text when the kernel says no. */
static void r_proc_kill(const char *b, size_t n, uint8_t *out, int fd) {
    char req[48], reason[API_CAUSE_MAX];
    const uint8_t *t;
    size_t ol, tl;
    uint64_t pid;
    int ok = 0, k, st;
    const char *why;
    if (webui_json_u64(b, n, "pid", &pid) != 1 || pid > 0x7fffffffu) {
        send_bad(fd, "bad_pid");
        return;
    }
    st = call_text(AVA1_METHOD_PROC_PROCESS_LIST, NULL, 0, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    why = webui_proc_kill_refusal((const char *)t, tl, pid);
    if (why) {
        send_error(fd, strcmp(why, "not_running") == 0 ? 404 : 403, why, strlen(why));
        return;
    }
    k = snprintf(req, sizeof req, "{\"pid\":%lu}", (unsigned long)pid);
    st = call_text(AVA1_METHOD_PROC_KILL, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (webui_json_bool((const char *)t, tl, "ok", &ok) == 1 && ok) {
        send_json(fd, 200, "{}", 2);
        return;
    }
    if (webui_json_str((const char *)t, tl, "reason", reason, sizeof reason) != 1 || !reason[0])
        snprintf(reason, sizeof reason, "kill_failed");
    send_error(fd, 409, reason, strlen(reason));
}

/* ---- volumes: eject a USB drive, unmount a mounted image ---- */

/* USB_EJECT {"mount_point":"/mnt/usbN","force":"0"|"1"} as the engine sends it (fs_ops.rs usb_eject). The
 * helper accepts /mnt/usb0-7 only and answers {"ok":false,"err":"usb_eject_busy"} while the drive is held
 * open; that reaches the page as 409 usb_eject_busy, and it may ask again with force (sync, then a forced
 * unmount). */
static void r_volumes_eject(const char *b, size_t n, uint8_t *out, int fd) {
    char path[64], req[128];
    const uint8_t *t;
    size_t ol, tl;
    int force = 0, k, st;
    if (webui_json_str(b, n, "path", path, sizeof path) != 1 || strncmp(path, "/mnt/usb", 8) != 0 || path[8] < '0' ||
        path[8] > '7' || path[9] != '\0') {
        send_bad(fd, "bad_path");
        return;
    }
    if (webui_json_bool(b, n, "force", &force) < 0) {
        send_bad(fd, "bad_force");
        return;
    }
    k = snprintf(req, sizeof req, "{\"mount_point\":\"%s\",\"force\":\"%s\"}", path, force ? "1" : "0");
    st = call_text(AVA1_METHOD_USB_EJECT, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, "{}", 2);
}

/* FS_UNMOUNT {"mount_point":..} (fs_ops.rs fs_unmount): an image the helper mounted. The helper does NOT
 * refuse a busy mount: fs_mount_try_unmount (runtime.c) escalates to MNT_FORCE on EBUSY, so a game
 * running from the image would lose it mid-play. Ruling R20: refused here while any game runs (the same
 * PROC_FOCUS reading /api/status shows: 409 game_running), and the console's own volumes are never
 * unmounted from the page (403 system_volume; webui_unmount_refusal). The helper itself still refuses
 * mounts it did not make (fs_unmount_not_our_mount). */
static void r_volumes_unmount(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], req[API_PATH * 2 + 32];
    const uint8_t *t;
    size_t ol, tl, at = 0;
    int st;
    const char *why;
    char title[16];
    uint32_t app_id = 0;
    int running = 0;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (!webui_unmount_refusal(path, 0)) {
        /* a failed focus read counts as running: never force an unmount on a guess */
        running = call_text(AVA1_METHOD_PROC_FOCUS, NULL, 0, out, &ol, &t, &tl) != AVA1_STATUS_OK ||
                  webui_focus_running((const char *)t, tl, title, sizeof title, &app_id) != 0;
    }
    why = webui_unmount_refusal(path, running);
    if (why) {
        send_error(fd, strcmp(why, "game_running") == 0 ? 409 : 403, why, strlen(why));
        return;
    }
    put_fmt(req, sizeof req, &at, "{");
    put_str(req, sizeof req, &at, "mount_point", path);
    put_fmt(req, sizeof req, &at, "}");
    if (at + 1 >= sizeof req) {
        send_bad(fd, "bad_path");
        return;
    }
    st = call_text(AVA1_METHOD_FS_UNMOUNT, req, at, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) send_status_error(fd, st, out, ol);
    else send_json(fd, 200, "{}", 2);
}

/* ---- the fan (Fan page): read and set it the way the desktop does ---- */

#define API_FAN_JSON 512u /* webui_fan_status_json's object is under 300 bytes */

/* HW_TEMPS "f" (the basic sensors plus the fan duty, the one extended read the page needs) as
 * webui_fan_status_json's object. */
static void send_fan_status(uint8_t *out, int fd) {
    char json[API_FAN_JSON];
    const uint8_t *t;
    size_t ol, tl;
    int st, k;
    st = call_text(AVA1_METHOD_HW_TEMPS, "f", 1, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    k = webui_fan_status_json((const char *)t, tl, json, sizeof json);
    if (k < 0) send_error(fd, 500, "bad_fan_reply", 13);
    else send_json(fd, 200, json, (size_t)k);
}

static void r_fan(const char *b, size_t n, uint8_t *out, int fd) {
    (void)b;
    (void)n;
    send_fan_status(out, fd);
}

/* {"target_c":N} (HW_FAN_THRESHOLD_MIN..MAX) or {"stock":true}: HW_FAN_THRESHOLD "N", or "0" for the
 * console's own setting (hw_fan_restore_stock), as the engine sends it; then the status read after it.
 * The helper's refusals reach the page as 409 fan_above_stock (higher than the console's own) and
 * 409 fan_restore_needs_restart (ours is dropped; the console's own value comes back at its next
 * fan-pattern switch or restart); a write it could not verify as icc_fan_*. */
static void r_fan_set(const char *b, size_t n, uint8_t *out, int fd) {
    char req[16], cause[API_CAUSE_MAX + 1];
    const uint8_t *t;
    size_t ol, tl;
    uint64_t c = 0;
    int stock = 0, k, st;
    if (webui_json_bool(b, n, "stock", &stock) < 0) {
        send_bad(fd, "bad_stock");
        return;
    }
    if (!stock && (webui_json_u64(b, n, "target_c", &c) != 1 || c < HW_FAN_THRESHOLD_MIN || c > HW_FAN_THRESHOLD_MAX)) {
        send_bad(fd, "bad_target_c");
        return;
    }
    k = snprintf(req, sizeof req, "%u", stock ? 0u : (unsigned)c);
    st = call_text(AVA1_METHOD_HW_FAN_THRESHOLD, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        size_t cl = ol < API_CAUSE_MAX ? ol : API_CAUSE_MAX;
        memcpy(cause, out, cl);
        cause[cl] = '\0';
        if (strcmp(cause, "fan_above_stock") == 0 || strcmp(cause, "fan_restore_needs_restart") == 0)
            send_error(fd, 409, cause, cl);
        else
            send_status_error(fd, st, out, ol);
        return;
    }
    send_fan_status(out, fd);
}

/* ---- file manager prefs (files_prefs.c): pinned folders, recent folders, the last folder ---- */

static void send_prefs_error(int fd, int rc) {
    switch (rc) {
    case FILES_PREFS_E_BAD_PATH: send_bad(fd, "bad_path"); break;
    case FILES_PREFS_E_BAD_OP: send_bad(fd, "bad_op"); break;
    case FILES_PREFS_E_FULL: send_error(fd, 409, "prefs_full", 10); break;
    case FILES_PREFS_E_NOMEM: send_error(fd, 500, "out_of_memory", 13); break;
    default: send_error(fd, 500, "save_failed", 11); break;
    }
}

/* {} -> {"v":1,"favorites":[..],"recent":[..],"last":".."|null}. A garbled file answers empty lists. */
static void r_files_prefs(const char *b, size_t n, uint8_t *out, int fd) {
    int len;
    (void)b;
    if (n > FILES_PREFS_REQ_MAX) {
        send_error(fd, 413, "too_large", 9);
        return;
    }
    len = files_prefs_get(FILES_PREFS_PATH, (char *)out, API_OUT_MAX);
    if (len < 0) send_prefs_error(fd, len);
    else send_json(fd, 200, (const char *)out, (size_t)len);
}

/* {"op":"pin"|"unpin"|"visit"|"forget","path":".."} -> the new prefs, as /api/files/prefs answers. */
static void r_files_prefs_set(const char *b, size_t n, uint8_t *out, int fd) {
    char op[16], path[API_PATH];
    int o, len;
    if (n > FILES_PREFS_REQ_MAX) {
        send_error(fd, 413, "too_large", 9);
        return;
    }
    if (webui_json_str(b, n, "op", op, sizeof op) != 1 || (o = files_prefs_op_from_name(op)) == 0) {
        send_bad(fd, "bad_op");
        return;
    }
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    len = files_prefs_update(FILES_PREFS_PATH, o, path, (char *)out, API_OUT_MAX);
    if (len < 0) send_prefs_error(fd, len);
    else send_json(fd, 200, (const char *)out, (size_t)len);
}

/* ---- Show on TV / Show on PC (files_handoff.c) ---- */

/* {"for":"console"} -> {"path":"..","age_ms":N} (the PC asked the TV to open it), or {}. */
static void r_files_handoff_take(const char *b, size_t n, uint8_t *out, int fd) {
    char side[16];
    int len;
    if (n > FILES_HANDOFF_REQ_MAX) {
        send_error(fd, 413, "too_large", 9);
        return;
    }
    if (webui_json_str(b, n, "for", side, sizeof side) != 1 || files_handoff_side(side) != FILES_HANDOFF_TO_CONSOLE) {
        send_bad(fd, "bad_side");
        return;
    }
    len = files_handoff_take_json(FILES_HANDOFF_TO_CONSOLE, files_handoff_now_ms(), (char *)out, API_OUT_MAX);
    if (len < 0) send_error(fd, 500, "internal", 8);
    else send_json(fd, 200, (const char *)out, (size_t)len);
}

/* {"to":"pc","path":".."} -> {}: the PC app's File System opens it (it polls). */
static void r_files_handoff_put(const char *b, size_t n, uint8_t *out, int fd) {
    char side[16], path[API_PATH];
    (void)out;
    if (n > FILES_HANDOFF_REQ_MAX) {
        send_error(fd, 413, "too_large", 9);
        return;
    }
    if (webui_json_str(b, n, "to", side, sizeof side) != 1 || files_handoff_side(side) != FILES_HANDOFF_TO_PC) {
        send_bad(fd, "bad_side");
        return;
    }
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (files_handoff_put(FILES_HANDOFF_TO_PC, path, files_handoff_now_ms()) != 0) send_bad(fd, "bad_path");
    else send_json(fd, 200, "{}", 2);
}

/* ---- console clean-up (storage_cleanup.c): the same calls the PC app makes ---- */

static void send_cleanup(int fd, int len, const uint8_t *out) {
    char tok[48];
    int http = 500;
    if (len <= 0) {
        send_error(fd, 500, "internal", 8);
        return;
    }
    if (!sc_reply_error((const char *)out, (size_t)len, tok, sizeof tok)) {
        send_json(fd, 200, (const char *)out, (size_t)len);
        return;
    }
    if (strcmp(tok, "busy") == 0 || strcmp(tok, "install_running") == 0 || strcmp(tok, "scan_not_done") == 0) http = 409;
    else if (strcmp(tok, "unknown_scan") == 0 || strcmp(tok, "unknown_apply") == 0) http = 404;
    else if (strcmp(tok, "scan_expired") == 0) http = 410;
    else if (strcmp(tok, "bad_request") == 0) http = 400;
    send_error(fd, http, tok, strlen(tok));
}

static void r_cleanup_scan(const char *b, size_t n, uint8_t *out, int fd) {
    (void)b;
    (void)n;
    send_cleanup(fd, sc_scan_start((char *)out, API_OUT_MAX), out);
}
static void r_cleanup_status(const char *b, size_t n, uint8_t *out, int fd) {
    send_cleanup(fd, sc_scan_status(b, n, (char *)out, API_OUT_MAX), out);
}
static void r_cleanup_apply(const char *b, size_t n, uint8_t *out, int fd) {
    send_cleanup(fd, sc_apply_start(b, n, (char *)out, API_OUT_MAX), out);
}
static void r_cleanup_apply_status(const char *b, size_t n, uint8_t *out, int fd) {
    send_cleanup(fd, sc_apply_status(b, n, (char *)out, API_OUT_MAX), out);
}
static void r_cleanup_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    send_cleanup(fd, sc_cancel(b, n, (char *)out, API_OUT_MAX), out);
}

/* ---- settings ---- */

/* The live settings. g_set_mu is held across a save, so two /api/settings/set calls never write
 * the file at once and memory always matches what is on disk. */
static pthread_mutex_t g_set_mu = PTHREAD_MUTEX_INITIALIZER;
static webui_settings_t g_set = { 0, 1, 1, 0 };
/* Whether webui.json was on the console when the helper started, or has been saved since: a PC that
 * turned Minion off re-applies that when the file is gone (the folder was wiped), and only then. */
static int g_saved = 0;
/* Serializes the LAN rebinds and the server starts/stops for `enabled`: each one reads the newest
 * setting under it, so the last one to run applies the last value saved, whatever order the
 * threads ran in. */
static pthread_mutex_t g_rebind_mu = PTHREAD_MUTEX_INITIALIZER;

void webui_api_settings_init(const webui_settings_t *s, int saved) {
    pthread_mutex_lock(&g_set_mu);
    g_set = *s;
    g_saved = saved ? 1 : 0;
    pthread_mutex_unlock(&g_set_mu);
}

/* tile_busy / tile_failed: the tile switch is applied on the tile thread after this reply (Sony calls
 * take seconds), so the page re-reads /api/settings until tile_busy is false and then says so when
 * the last ensure/remove failed (webui_tile_state). tile_result: what the helper last did to the
 * tile ("added", "removed", "hidden", "failed" or ""; webui_tile_result_name). */
/* `addr`: with LAN access on, the console's address a phone opens ("Continue on your phone"; the
 * page builds http://<addr>:<port>/ from it); "" when off or there is none. Dotted IPv4 only.
 * `enabled`: PS5Upload Minion as a whole (false only in the reply that turns it off: the page
 * stops right after). `start_cheats`: the page opens on Cheats instead of Home (#584). */
static void send_settings(int fd, const webui_settings_t *s) {
    char json[320], addr[INET_ADDRSTRLEN];
    int busy, failed, k;
    webui_tile_result_t result;
    webui_tile_state(&busy, &failed, &result);
    if (!s->lan || !webui_lan_addr(addr)) addr[0] = '\0';
    k = snprintf(json, sizeof json,
                 "{\"lan\":%s,\"tile\":%s,\"enabled\":%s,\"start_cheats\":%s,\"tile_busy\":%s,\"tile_failed\":%s,"
                 "\"tile_result\":\"%s\",\"version\":\"%s\",\"addr\":\"%s\"}",
                 s->lan ? "true" : "false", s->tile ? "true" : "false", s->enabled ? "true" : "false",
                 s->start_cheats ? "true" : "false", busy ? "true" : "false", failed ? "true" : "false",
                 webui_tile_result_name(result), PS5UPLOAD2_VERSION, addr);
    if (k < 0 || (size_t)k >= sizeof json) {
        send_error(fd, 500, "bad_settings_reply", 18);
        return;
    }
    send_json(fd, 200, json, (size_t)k);
}

static void r_settings(const char *b, size_t n, uint8_t *out, int fd) {
    webui_settings_t s;
    (void)b;
    (void)n;
    (void)out;
    pthread_mutex_lock(&g_set_mu);
    s = g_set;
    pthread_mutex_unlock(&g_set_mu);
    send_settings(fd, &s);
}

/* Rebinds the listener for the saved `lan`. Runs on its own thread, started after the reply was
 * written: webui_restart waits up to 1.5 s for the bind, which must not hold a web worker, and the
 * page that asked keeps working (the token survives webui_restart; open connections are not
 * touched, only the listener). */
static void *rebind_main(void *arg) {
    int lan;
    (void)arg;
    proc_name_set_self("ps5upload-web");
    pthread_mutex_lock(&g_rebind_mu);
    pthread_mutex_lock(&g_set_mu);
    lan = g_set.lan;
    pthread_mutex_unlock(&g_set_mu);
    if (webui_restart(lan) != 0) fprintf(stderr, "webui: rebind for lan=%d not bound yet (the server keeps retrying)\n", lan);
    pthread_mutex_unlock(&g_rebind_mu);
    return NULL;
}

static void spawn_lan_apply(void) {
    pthread_attr_t attr;
    pthread_t t;
    int rc = -1;
    if (pthread_attr_init(&attr) == 0) {
        if (pthread_attr_setstacksize(&attr, 256u * 1024u) == 0 &&
            pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0)
            rc = pthread_create(&t, &attr, rebind_main, NULL);
        pthread_attr_destroy(&attr);
    }
    if (rc != 0) fprintf(stderr, "webui: cannot start the rebind thread (%d); LAN applies at the next start\n", rc);
}

/* Starts the :9180 server with the saved LAN setting (main.c at helper start, and turning Minion
 * back on). 0 when it runs (or already ran), -1 when it could not start (logged). */
int webui_api_server_start(void) {
    webui_cfg_t wc;
    int lan;
    if (webui_running()) return 0;
    pthread_mutex_lock(&g_set_mu);
    lan = g_set.lan;
    pthread_mutex_unlock(&g_set_mu);
    memset(&wc, 0, sizeof wc);
    wc.port = WEBUI_PORT;
    wc.lan = lan;
    wc.route = webui_api_route;
    wc.stream_for = webui_api_stream_for;
    wc.stream = webui_api_stream;
    if (webui_start(&wc) != 0) {
        if (webui_running()) return 0; /* another start won the race */
        fprintf(stderr, "webui: server did not start\n");
        return -1;
    }
    return 0;
}

/* Starts or stops the server for the saved `enabled`. Runs on its own thread, started after the
 * reply was written: webui_stop joins the web workers (up to 2 s), which must never happen on a
 * web worker (the page that turned Minion off is answered first) or on an AVA1 thread. */
static void *server_apply_main(void *arg) {
    int enabled;
    (void)arg;
    proc_name_set_self("ps5upload-web");
    pthread_mutex_lock(&g_rebind_mu);
    pthread_mutex_lock(&g_set_mu);
    enabled = g_set.enabled;
    pthread_mutex_unlock(&g_set_mu);
    if (enabled) {
        if (webui_api_server_start() == 0) fprintf(stderr, "webui: PS5Upload Minion turned on\n");
    } else if (webui_running()) {
        webui_stop();
        fprintf(stderr, "webui: PS5Upload Minion turned off; the page is no longer served\n");
    }
    pthread_mutex_unlock(&g_rebind_mu);
    return NULL;
}

static void spawn_server_apply(void) {
    pthread_attr_t attr;
    pthread_t t;
    int rc = -1;
    if (pthread_attr_init(&attr) == 0) {
        if (pthread_attr_setstacksize(&attr, 256u * 1024u) == 0 &&
            pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0)
            rc = pthread_create(&t, &attr, server_apply_main, NULL);
        pthread_attr_destroy(&attr);
    }
    if (rc != 0) fprintf(stderr, "webui: cannot start the server thread (%d); Minion on/off applies at the next start\n", rc);
}

/* Caller holds g_set_mu; `s` is saved and set. Asks the tile thread for what the change means:
 * the tile wanted (Minion on and tile on) and it was not, or `retry` (the caller sent the tile
 * setting again): add it; not wanted and it was, or `retry`: remove it; Minion just turned off
 * with the tile already off: sweep what may be left of it (#480). Returns 1 when `enabled`
 * changed: the caller then calls spawn_server_apply (after its reply). */
static int apply_locked(const webui_settings_t *old, const webui_settings_t *s, int retry) {
    int was = webui_settings_tile_wanted(old), want = webui_settings_tile_wanted(s);
    if (want && (retry || !was)) webui_tile_request(1, WEBUI_PORT);
    else if (!want && (was || retry)) webui_tile_request(0, WEBUI_PORT);
    else if (!s->enabled && old->enabled) webui_tile_sweep_request();
    return s->enabled != old->enabled;
}

/* {lan?, tile?, enabled?, start_cheats?}: saved first (the file is the truth at the next start), then answered
 * with the new settings, then applied: a `lan` change rebinds the listener on a thread of its own,
 * a `tile` change (or tile:true again) registers or removes the tile on the tile thread, and
 * enabled:false (Settings -> Turn off PS5Upload Minion) removes the tile and then stops this
 * server, after the reply. A failed save changes nothing. */
static void r_settings_set(const char *b, size_t n, uint8_t *out, int fd) {
    webui_settings_t old, s;
    int lan, tile, enabled, start_cheats, has_lan, has_tile, has_enabled, has_start, server, changed;
    (void)out;
    has_lan = webui_json_bool(b, n, "lan", &lan);
    if (has_lan < 0) {
        send_bad(fd, "bad_lan");
        return;
    }
    has_tile = webui_json_bool(b, n, "tile", &tile);
    if (has_tile < 0) {
        send_bad(fd, "bad_tile");
        return;
    }
    has_enabled = webui_json_bool(b, n, "enabled", &enabled);
    if (has_enabled < 0) {
        send_bad(fd, "bad_enabled");
        return;
    }
    has_start = webui_json_bool(b, n, "start_cheats", &start_cheats);
    if (has_start < 0) {
        send_bad(fd, "bad_start_cheats");
        return;
    }
    pthread_mutex_lock(&g_set_mu);
    old = s = g_set;
    if (has_lan) s.lan = lan;
    if (has_tile) s.tile = tile;
    if (has_enabled) s.enabled = enabled;
    if (has_start) s.start_cheats = start_cheats;
    changed = s.lan != old.lan || s.tile != old.tile || s.enabled != old.enabled || s.start_cheats != old.start_cheats;
    if (changed && webui_settings_save(WEBUI_SETTINGS_PATH, &s) != 0) {
        pthread_mutex_unlock(&g_set_mu);
        send_error(fd, 500, "save_failed", 11);
        return;
    }
    g_set = s;
    if (changed) g_saved = 1;
    /* Keep tile requests in saved-setting order, including calls from the paired PC.
     * tile:true also retries a failed ensure; removal runs only on a change. */
    server = apply_locked(&old, &s, has_tile && tile);
    pthread_mutex_unlock(&g_set_mu);
    send_settings(fd, &s); /* written in full (or the deadline passed) before the rebind below runs */
    if (server) spawn_server_apply();
    else if (s.lan != old.lan) spawn_lan_apply();
}

int webui_api_tile(const char *body, size_t len, char *out, size_t cap) {
    webui_settings_t old, s;
    int tile = 0, enabled = 0, lan = 0, remove = 0, busy, failed, n, server = 0, removed = -1;
    webui_tile_result_t result;
    int has = webui_json_bool(body, len, "tile", &tile);
    int has_enabled = webui_json_bool(body, len, "enabled", &enabled);
    int has_lan = webui_json_bool(body, len, "lan", &lan);
    int has_remove = webui_json_bool(body, len, "remove", &remove);
    if (has < 0) return snprintf(out, cap, "{\"ok\":false,\"err\":\"bad_tile\"}");
    if (has_enabled < 0) return snprintf(out, cap, "{\"ok\":false,\"err\":\"bad_enabled\"}");
    if (has_lan < 0) return snprintf(out, cap, "{\"ok\":false,\"err\":\"bad_lan\"}");
    if (has_remove < 0 || (has_remove && !remove)) return snprintf(out, cap, "{\"ok\":false,\"err\":\"bad_remove\"}");
    /* remove: Minion off (saved first, so a helper restart midway still keeps it off), then its own
     * files deleted; the tile is removed by the tile thread as for any turn-off. */
    if (has_remove) {
        has_enabled = 1;
        enabled = 0;
    }
    pthread_mutex_lock(&g_set_mu);
    old = s = g_set;
    if (has || has_enabled || has_lan) {
        if (has) s.tile = tile;
        if (has_enabled) s.enabled = enabled;
        if (has_lan) s.lan = lan;
        if (s.tile != old.tile || s.enabled != old.enabled || s.lan != old.lan || (has_remove && !g_saved)) {
            if (webui_settings_save(WEBUI_SETTINGS_PATH, &s) != 0) {
                pthread_mutex_unlock(&g_set_mu);
                return snprintf(out, cap, "{\"ok\":false,\"err\":\"save_failed\"}");
            }
            g_saved = 1;
        }
        g_set = s;
        /* `has` also retries an interrupted registration/removal with an unchanged setting; a
         * remove also sweeps again when Minion was already off (leftovers from an older helper). */
        server = apply_locked(&old, &s, has);
        if (has_remove && !old.enabled && !has) webui_tile_sweep_request();
    }
    if (has_remove) {
        /* Minion's own files: the page's Files favourites and recent folders, its controller input
         * log, and the game Library cache the SMB daemon writes for it. webui.json stays (off).
         * Shared data (saved network shares, cheats, backups) belongs to the PC app too: kept. */
        static const char *const leftovers[] = { FILES_PREFS_PATH, WEBUI_INPUTLOG_PATH, SMBD_LIB_DIR };
        if (webui_minion_forget(leftovers, sizeof leftovers / sizeof leftovers[0], &removed) != 0) {
            pthread_mutex_unlock(&g_set_mu);
            if (server) spawn_server_apply();
            return snprintf(out, cap, "{\"ok\":false,\"err\":\"remove_failed\"}");
        }
    }
    webui_tile_state(&busy, &failed, &result);
    n = snprintf(out, cap,
        "{\"tile\":%s,\"enabled\":%s,\"lan\":%s,\"saved\":%s,\"tile_busy\":%s,\"tile_failed\":%s,"
        "\"tile_result\":\"%s\",\"port\":%d,\"version\":\"%s\"",
        s.tile ? "true" : "false", s.enabled ? "true" : "false", s.lan ? "true" : "false",
        g_saved ? "true" : "false", busy ? "true" : "false", failed ? "true" : "false",
        webui_tile_result_name(result), (int)WEBUI_PORT, PS5UPLOAD2_VERSION);
    if (n > 0 && (size_t)n < cap && removed >= 0) n += snprintf(out + n, cap - (size_t)n, ",\"removed\":%d", removed);
    if (n > 0 && (size_t)n < cap) n += snprintf(out + n, cap - (size_t)n, "}");
    pthread_mutex_unlock(&g_set_mu);
    /* An AVA1 thread: the server start/stop (up to 2 s) and a LAN rebind run on their own thread. */
    if (server) spawn_server_apply();
    else if (s.lan != old.lan) spawn_lan_apply();
    return n < 0 || (size_t)n >= cap ? -1 : n;
}

void webui_api_tile_forget(void) {
    webui_settings_t s;
    pthread_mutex_lock(&g_set_mu);
    s = g_set;
    s.tile = 0;
    if (webui_settings_save(WEBUI_SETTINGS_PATH, &s) != 0) fprintf(stderr, "webui: cannot save tile:false\n");
    g_set = s; /* the tile is off either way */
    pthread_mutex_unlock(&g_set_mu);
}

int webui_api_tile_off(void) {
    webui_settings_t s;
    unsigned gen;
    int rc = 0;
    pthread_mutex_lock(&g_set_mu);
    s = g_set;
    s.tile = 0;
    /* Persist first. A failed save must not look successful or re-enable the tile on boot. */
    if (webui_settings_save(WEBUI_SETTINGS_PATH, &s) != 0) {
        fprintf(stderr, "webui: cannot save tile:false\n");
        pthread_mutex_unlock(&g_set_mu);
        return -1;
    }
    g_set = s;
    /* Asked for under g_set_mu (requests stay in saved-setting order), waited for without it: the
     * page's /api/settings and the PC's tile poll take it, and would otherwise hang behind the Sony
     * calls (#480). A toggle made meanwhile supersedes this request; the wait reports that as -1. */
    gen = webui_tile_request(0, WEBUI_PORT);
    pthread_mutex_unlock(&g_set_mu);
    rc = webui_tile_remove_wait(gen);
    return rc;
}

/* ---- cheats: the running game's cheats, through the methods the engine calls (cheats.rs) ---- */

#define API_CHEATS_JSON (96u * 1024u) /* CHEATS_GET's 64 KiB reply, re-escaped */

/* A required title id the cheat methods can take (webui_cheats_title_ok); else 400 bad_title_id. */
static int need_title(int fd, const char *b, size_t n, char *out, size_t cap) {
    if (webui_json_str(b, n, "title_id", out, cap) == 1 && webui_cheats_title_ok(out)) return 0;
    send_bad(fd, "bad_title_id");
    return -1;
}

/* CHEATS_STATUS (empty request): {"engine":bool,"game":{"title_id":..}|null}. Cheap (the helper
 * caches the running game): the page polls it. */
static void r_cheats_status(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    char json[96];
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_CHEATS_STATUS, NULL, 0, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (webui_cheats_status_to_json((const char *)t, tl, json, sizeof json) != 0) send_error(fd, 500, "bad_cheats_reply", 16);
    else send_json(fd, 200, json, strlen(json));
}

/* {title_id}: CHEATS_GET {"title_id":..}, as {"mods":[{index,name,desc,button,on}],"truncated":bool}. */
static void r_cheats_list(const char *b, size_t n, uint8_t *out, int fd) {
    char title[32], req[64], err[API_CAUSE_MAX];
    const uint8_t *t;
    size_t ol, tl;
    char *json;
    int st, k;
    if (need_title(fd, b, n, title, sizeof title) != 0) return;
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\"}", title); /* letters and digits only */
    st = call_text(AVA1_METHOD_CHEATS_GET, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    json = malloc(API_CHEATS_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_cheats_mods_to_json((const char *)t, tl, json, API_CHEATS_JSON, err, sizeof err) < 0) {
        if (err[0]) send_error(fd, 500, err, strlen(err));
        else send_error(fd, 500, "bad_cheats_reply", 16);
    } else {
        send_json(fd, 200, json, strlen(json));
    }
    free(json);
}

static int cheats_error_http(const char *token) {
    if (strcmp(token, "game_not_running") == 0 || strcmp(token, "module_missing") == 0 ||
        strcmp(token, "game_busy") == 0)
        return 409;
    if (strcmp(token, "no_cheats") == 0) return 404;
    if (strcmp(token, "bad_index") == 0) return 400;
    return 500;
}

/* {title_id, index, on}: CHEATS_TOGGLE as the engine sends it. The helper refuses unless that game
 * is running (it patches the live process) and turns its cheat engine on with any switch-on. A
 * refusal is {"error":"game_not_running"|"no_cheats"|"bad_index"|"module_missing"|"toggle_failed"}. */
static void r_cheats_toggle(const char *b, size_t n, uint8_t *out, int fd) {
    char title[32], req[96];
    const uint8_t *t;
    size_t ol, tl;
    uint64_t index;
    int on, st, k;
    if (need_title(fd, b, n, title, sizeof title) != 0) return;
    if (webui_json_u64(b, n, "index", &index) != 1 || index > 0x7fffffffu) {
        send_bad(fd, "bad_index");
        return;
    }
    if (webui_json_bool(b, n, "on", &on) != 1) {
        send_bad(fd, "bad_on");
        return;
    }
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\",\"index\":%lu,\"on\":%s}", title, (unsigned long)index,
                 on ? "true" : "false");
    st = call_text(AVA1_METHOD_CHEATS_TOGGLE, req, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        /* the handler's whole {"ok":false,"err":..} body is the cause (mgmt_call_text_keep) */
        if (ol && out[0] == '{') {
            const char *tok = webui_cheats_toggle_error((const char *)out, ol);
            send_error(fd, cheats_error_http(tok), tok, strlen(tok));
        } else {
            send_status_error(fd, st, out, ol);
        }
        return;
    }
    {
        const char *ok = on ? "{\"ok\":true,\"on\":true}" : "{\"ok\":true,\"on\":false}";
        send_json(fd, 200, ok, strlen(ok));
    }
}

/* {enabled}: CHEATS_ENGINE_SET, as the PC app sends it: the helper's cheat engine on or off, which it
 * remembers across restarts (#550). On, it applies the running game's switched-on cheats within a few
 * seconds. {"ok":true,"enabled":bool}. */
static void r_cheats_engine(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    int on, st;
    if (webui_json_bool(b, n, "enabled", &on) != 1) {
        send_bad(fd, "bad_enabled");
        return;
    }
    const char *req = on ? "{\"enabled\":true}" : "{\"enabled\":false}";
    st = call_text(AVA1_METHOD_CHEATS_ENGINE_SET, req, strlen(req), out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    {
        const char *ok = on ? "{\"ok\":true,\"enabled\":true}" : "{\"ok\":true,\"enabled\":false}";
        send_json(fd, 200, ok, strlen(ok));
    }
}

/* {title_id}: the game's installed version, read like the PC app reads it (APP_INFO_QUERY's
 * CONTENT_VERSION, else APP_VER): {"version":"01.05"} or {"version":null} when appinfo.db does not
 * say (the page then lists every version's cheats). Never an error for an unknown version. */
static void r_cheats_version(const char *b, size_t n, uint8_t *out, int fd) {
    char title[32], req[96], ver[32], json[64];
    const uint8_t *t;
    size_t ol, tl;
    int st, k;
    if (need_title(fd, b, n, title, sizeof title) != 0) return;
    k = snprintf(req, sizeof req, "{\"title_id\":\"%s\",\"keys\":\"CONTENT_VERSION,APP_VER\"}", title);
    st = call_text(AVA1_METHOD_APP_INFO_QUERY, req, (size_t)k, out, &ol, &t, &tl);
    if (st == AVA1_STATUS_OK && webui_cheat_appinfo_version((const char *)t, tl, ver, sizeof ver) == 1)
        k = snprintf(json, sizeof json, "{\"version\":\"%s\"}", ver); /* digits and dots only */
    else
        k = snprintf(json, sizeof json, "{\"version\":null}");
    send_json(fd, 200, json, (size_t)k);
}

/* ---- search: the helper's file index (INDEX_START / STATUS / SEARCH / CANCEL, runtime.c) ----
 *
 * One index for the whole helper (its own thread walks the roots: at most 200,000 files and 32 MiB of
 * paths, ten folders deep, dot names skipped; INDEX_CANCEL stops it between folders). The helper does
 * not say which roots an index covers or that it was stopped: the page's last start is remembered here
 * (by the started_at INDEX_STATUS gives it) so /api/index/status can tell the page whether the index
 * it finds is the one it asked for. The AVA1 trust store's files are never answered. */

#define API_INDEX_RESULTS_MAX 500u           /* results one search answers */
#define API_INDEX_ASK 500u                   /* INDEX_SEARCH's limit (it filters "under" first) */
#define API_INDEX_JSON (256u * 1024u)        /* 500 results with long, escaped paths fit easily */
#define API_INDEX_STATUS_JSON (WEBUI_INDEX_ROOTS_MAX * (6u * WEBUI_INDEX_ROOT_CAP + 3u) + 160u) /* 8 roots, escaped */

static pthread_mutex_t g_idx_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_idx_roots[WEBUI_INDEX_ROOTS_MAX][WEBUI_INDEX_ROOT_CAP];
static int g_idx_nroots = 0;
static uint64_t g_idx_started_at = 0; /* 0: no index the page started */
static int g_idx_cancelled = 0;

static int hide_protected(const char *path, void *ctx) {
    (void)ctx;
    return path_in_protected(path);
}

/* INDEX_STATUS's reply text, and its started_at (0 when absent). */
static int index_status_text(uint8_t *out, const uint8_t **t, size_t *tl, uint64_t *started_at) {
    size_t ol;
    int st = call_text(AVA1_METHOD_INDEX_STATUS, NULL, 0, out, &ol, t, tl);
    if (st != AVA1_STATUS_OK) return st;
    if (webui_json_u64((const char *)*t, *tl, "started_at", started_at) != 1) *started_at = 0;
    return AVA1_STATUS_OK;
}

/* {roots:["/data",..]} (1-8 roots): a new index, replacing the one there. 409 busy while one is built. */
static void r_index_start(const char *b, size_t n, uint8_t *out, int fd) {
    char roots[WEBUI_INDEX_ROOTS_MAX][WEBUI_INDEX_ROOT_CAP], body[WEBUI_INDEX_ROOTS_MAX * (WEBUI_INDEX_ROOT_CAP + 3) + 16];
    char err[64];
    const char *ptrs[WEBUI_INDEX_ROOTS_MAX];
    const char *arr;
    const uint8_t *t;
    size_t alen, pos = 0, ol, tl;
    uint64_t started_at = 0;
    int nroots = 0, rc, len, st, started = 0;
    if (webui_json_array(b, n, "roots", &arr, &alen) != 1) {
        send_bad(fd, "bad_roots");
        return;
    }
    for (;;) {
        char root[WEBUI_INDEX_ROOT_CAP + 1];
        rc = webui_json_array_str(arr, alen, &pos, root, sizeof root);
        if (rc == 0) break;
        if (rc < 0 || nroots >= WEBUI_INDEX_ROOTS_MAX || !webui_index_root_ok(root)) {
            send_bad(fd, "bad_roots");
            return;
        }
        if (path_in_protected(root)) {
            send_error(fd, 403, "path_not_allowed", 16);
            return;
        }
        memcpy(roots[nroots], root, strlen(root) + 1);
        ptrs[nroots] = roots[nroots];
        nroots++;
    }
    len = nroots ? webui_index_start_body(body, sizeof body, ptrs, nroots) : -1;
    if (len <= 0) {
        send_bad(fd, "bad_roots");
        return;
    }
    st = call_text(AVA1_METHOD_INDEX_START, body, (size_t)len, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (webui_json_bool((const char *)t, tl, "started", &started) != 1 || !started) {
        if (webui_json_str((const char *)t, tl, "err", err, sizeof err) != 1 || !err[0]) snprintf(err, sizeof err, "index_failed");
        if (strcmp(err, "already_building") == 0) send_error(fd, 409, "busy", 4);
        else send_error(fd, 500, err, strlen(err));
        return;
    }
    /* the index just started is this one: remember what it covers */
    if (index_status_text(out, &t, &tl, &started_at) != AVA1_STATUS_OK) started_at = 0;
    pthread_mutex_lock(&g_idx_mu);
    memcpy(g_idx_roots, roots, sizeof roots);
    g_idx_nroots = nroots;
    g_idx_started_at = started_at;
    g_idx_cancelled = 0;
    pthread_mutex_unlock(&g_idx_mu);
    send_json(fd, 200, "{\"started\":true}", 16);
}

/* {} -> {"phase","files","truncated","cancelled","roots"|null}: roots null when the index there is not
 * one the page started (never built, or another client's). */
static void r_index_status(const char *b, size_t n, uint8_t *out, int fd) {
    const char *ptrs[WEBUI_INDEX_ROOTS_MAX];
    char roots[WEBUI_INDEX_ROOTS_MAX][WEBUI_INDEX_ROOT_CAP];
    char *json;
    const uint8_t *t;
    size_t tl;
    uint64_t started_at = 0;
    int st, i, nroots = 0, cancelled = 0, ours = 0;
    (void)b;
    (void)n;
    st = index_status_text(out, &t, &tl, &started_at);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, NULL, 0);
        return;
    }
    pthread_mutex_lock(&g_idx_mu);
    if (g_idx_started_at != 0 && g_idx_started_at == started_at) {
        ours = 1;
        nroots = g_idx_nroots;
        cancelled = g_idx_cancelled;
        memcpy(roots, g_idx_roots, sizeof roots);
    }
    pthread_mutex_unlock(&g_idx_mu);
    for (i = 0; i < nroots; i++) ptrs[i] = roots[i];
    json = malloc(API_INDEX_STATUS_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_index_status_to_json((const char *)t, tl, ours ? ptrs : NULL, nroots, cancelled, json, API_INDEX_STATUS_JSON) != 0)
        send_error(fd, 500, "bad_index_reply", 15);
    else
        send_json(fd, 200, json, strlen(json));
    free(json);
}

/* {query, under?}: files whose NAME matches `query` (the helper's glob: * and ?, ASCII case-insensitive,
 * the whole name), under the folder `under` when given. -> {"results":[{"path","size"}],"truncated"}. */
static void r_index_search(const char *b, size_t n, uint8_t *out, int fd) {
    char query[WEBUI_INDEX_ROOT_CAP + 1], under[API_PATH], body[2 * WEBUI_INDEX_ROOT_CAP + 64], *json;
    const uint8_t *t;
    size_t ol, tl;
    unsigned ask;
    int rc, st, k;
    if (webui_json_str(b, n, "query", query, sizeof query) != 1 || !webui_index_query_ok(query)) {
        send_bad(fd, "bad_query");
        return;
    }
    rc = webui_json_str(b, n, "under", under, sizeof under);
    if (rc < 0 || (rc == 1 && !webui_index_root_ok(under))) {
        send_bad(fd, "bad_under");
        return;
    }
    if (rc == 0) under[0] = '\0';
    webui_index_collapse_stars(query);
    ask = API_INDEX_ASK;
    /* checked: no quote, no backslash. "under" makes the helper filter before its limit applies (an
     * older helper ignores it: the filter below still holds, and the reply says truncated). */
    if (under[0])
        k = snprintf(body, sizeof body, "{\"query\":\"%s\",\"under\":\"%s\",\"limit\":%u}", query, under, ask);
    else
        k = snprintf(body, sizeof body, "{\"query\":\"%s\",\"limit\":%u}", query, ask);
    if (k <= 0 || (size_t)k >= sizeof body) {
        send_bad(fd, "bad_query");
        return;
    }
    st = call_text(AVA1_METHOD_INDEX_SEARCH, body, (size_t)k, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    json = malloc(API_INDEX_JSON);
    if (!json) {
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    if (webui_index_results_to_json((const char *)t, tl, under[0] ? under : NULL, API_INDEX_RESULTS_MAX, ask,
                                    hide_protected, NULL, json, API_INDEX_JSON) < 0)
        send_error(fd, 500, "bad_index_reply", 15);
    else
        send_json(fd, 200, json, strlen(json));
    free(json);
}

/* {} -> {}: the build stops before its next folder (what it found stays searchable, marked cancelled). */
static void r_index_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    const uint8_t *t;
    size_t ol, tl;
    uint64_t started_at = 0;
    int st;
    (void)b;
    (void)n;
    st = call_text(AVA1_METHOD_INDEX_CANCEL, NULL, 0, out, &ol, &t, &tl);
    if (st != AVA1_STATUS_OK) {
        send_status_error(fd, st, out, ol);
        return;
    }
    if (index_status_text(out, &t, &tl, &started_at) == AVA1_STATUS_OK) {
        pthread_mutex_lock(&g_idx_mu);
        if (g_idx_started_at != 0 && g_idx_started_at == started_at) g_idx_cancelled = 1;
        pthread_mutex_unlock(&g_idx_mu);
    }
    send_json(fd, 200, "{}", 2);
}

/* ---- Files from another device: text save, upload, download (webui_files.c does the I/O) ----
 *
 * None of these reaches a helper method: they work on the file system directly, under the helper's own
 * policy (ava1_data_cfg: may_write for anything written, may_read for anything sent, the trust store
 * never). Streamed bodies (an upload, a text save) are read by the route itself (webui_api_stream), with
 * an idle timeout instead of the 5 s connection deadline, so they hold a web worker only while bytes
 * move. At most ONE upload and ONE download run at a time (409 busy otherwise), and a text save (one at a
 * time too) is refused 409 busy while both of those run: of the four workers, at least two stay free for
 * everything else. Every transfer also has a throughput floor (webui_files.h wf_rate_t), so a client that
 * trickles bytes cannot hold a worker forever either. */

#define XFER_RESERVE_MS 30000LL   /* an upload begun but not sent is forgotten after this */
#define XFER_TICKET_MS 60000LL    /* a download link works this long after it was made or last used */
#define XFER_TEXT_HEAD 8192u      /* a text save's first line (its JSON) at most */
#define XFER_TEXT_TOTAL_MS 60000u /* a text save's whole body within this */
#define XFER_IDLE_TEXT_MS 10000u

static long long mono_ms(void);

static int policy_write(const char *p) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    return !path_in_protected(p) && cfg && cfg->may_write && cfg->may_write(p);
}

static int policy_read(const char *p) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    return !path_in_protected(p) && cfg && cfg->may_read && cfg->may_read(p, 0);
}

/* The tar walk's filter: the trust store's forms, resolved once per archive (path_protected_forms);
 * the walk's root is canonical and it follows no link, so each entry is a plain prefix test. */
typedef struct {
    char lex[PATH_MAX];
    char canon[PATH_MAX];
} protected_forms_t;

static int skip_protected(const char *p, void *ctx) {
    const protected_forms_t *f = (const protected_forms_t *)ctx;
    return path_under_forms(p, f->lex, f->canon);
}

/* An answer to a client still sending its body: read off (and drop) a bounded part of it first, so the
 * close that follows the answer is less likely to reset the connection before the page reads the code. */
#define XFER_DRAIN_BYTES (64u * 1024u)
#define XFER_DRAIN_MS 2000
static void drain_then(int fd, uint64_t body_left) {
    uint8_t sink[16384];
    long long until = mono_ms() + XFER_DRAIN_MS;
    uint64_t want = body_left < XFER_DRAIN_BYTES ? body_left : XFER_DRAIN_BYTES;
    while (want > 0 && mono_ms() < until) {
        ssize_t k = wf_recv(fd, sink, want < sizeof sink ? (size_t)want : sizeof sink,
                            (unsigned)(until - mono_ms() > 0 ? until - mono_ms() : 1), NULL);
        if (k <= 0) break;
        want -= (uint64_t)k;
    }
}

static void send_wf(int fd, int e) {
    const char *tok = wf_error_token(e);
    send_error(fd, wf_error_http(e), tok, strlen(tok));
}

static int folder_of(const char *path, char *out, size_t cap) {
    const char *slash = strrchr(path, '/');
    size_t n;
    if (!slash) return -1;
    n = slash == path ? 1 : (size_t)(slash - path);
    if (n + 1 > cap) return -1;
    memcpy(out, path, n);
    out[n] = '\0';
    return 0;
}

static pthread_mutex_t g_xfer_mu = PTHREAD_MUTEX_INITIALIZER;
enum { UP_FREE = 0, UP_RESERVED, UP_RECEIVING };
static struct {
    int state;
    uint8_t id[16];
    char path[API_PATH];
    uint64_t size;
    int overwrite;
    long long expires;
    volatile int cancel;
} g_up;
enum { DL_NONE = 0, DL_READY, DL_SENDING };
static struct {
    int state;
    uint8_t id[16];
    char path[API_PATH];
    int is_dir;
    long long expires;
} g_dl;
static pthread_mutex_t g_text_mu = PTHREAD_MUTEX_INITIALIZER;

/* {path, size, overwrite?} -> {"id":".."}: checks what the upload would do before any byte is sent. The
 * file then comes as POST /api/upload/data?id=<id> (the raw bytes, Content-Length = size) within 30 s. */
static void r_upload_begin(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], dir[API_PATH], hex[33], json[64];
    struct stat st;
    struct statvfs vf;
    uint64_t size;
    int overwrite = 0;
    long long now = mono_ms();
    (void)out;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (webui_json_u64(b, n, "size", &size) != 1 || size > WEBUI_STREAM_BODY_MAX) {
        send_bad(fd, "bad_size");
        return;
    }
    if (webui_json_bool(b, n, "overwrite", &overwrite) < 0) {
        send_bad(fd, "bad_overwrite");
        return;
    }
    if (folder_of(path, dir, sizeof dir) != 0 || !policy_write(path)) {
        send_error(fd, 403, "path_not_allowed", 16);
        return;
    }
    if (stat(dir, &st) != 0 || !S_ISDIR(st.st_mode)) {
        send_error(fd, 404, "no_folder", 9);
        return;
    }
    (void)wf_sweep_stale(dir, time(NULL), WF_STALE_PART_S); /* temps a crash left behind */
    if (lstat(path, &st) == 0) {
        if (!S_ISREG(st.st_mode)) {
            send_error(fd, 409, "not_a_file", 10);
            return;
        }
        if (!overwrite) {
            send_error(fd, 409, "exists", 6);
            return;
        }
    }
    /* The bytes go to a temporary file first: the whole size must be free even when it replaces one. */
    if (statvfs(dir, &vf) == 0 && (uint64_t)vf.f_bavail * (uint64_t)vf.f_frsize < size) {
        send_error(fd, 507, "no_space", 8);
        return;
    }
    pthread_mutex_lock(&g_xfer_mu);
    if (g_up.state == UP_RECEIVING || (g_up.state == UP_RESERVED && g_up.expires > now)) {
        pthread_mutex_unlock(&g_xfer_mu);
        send_error(fd, 409, "busy", 4);
        return;
    }
    g_up.state = UP_RESERVED;
    arc4random_buf(g_up.id, sizeof g_up.id);
    memcpy(g_up.path, path, strlen(path) + 1);
    g_up.size = size;
    g_up.overwrite = overwrite;
    g_up.expires = now + XFER_RESERVE_MS;
    g_up.cancel = 0;
    webui_hex16(g_up.id, hex);
    pthread_mutex_unlock(&g_xfer_mu);
    snprintf(json, sizeof json, "{\"id\":\"%s\"}", hex);
    send_json(fd, 200, json, strlen(json));
}

/* {id}: an upload begun and not yet sent is forgotten; one being received stops (its temporary file
 * is removed, the final name never appears). Unknown ids answer {} too. */
static void r_upload_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    char hex[40];
    uint8_t id[16];
    (void)out;
    if (webui_json_str(b, n, "id", hex, sizeof hex) != 1 || webui_parse_hex16(hex, id) != 0) {
        send_bad(fd, "bad_id");
        return;
    }
    pthread_mutex_lock(&g_xfer_mu);
    if (memcmp(id, g_up.id, 16) == 0) {
        if (g_up.state == UP_RESERVED) g_up.state = UP_FREE;
        else if (g_up.state == UP_RECEIVING) g_up.cancel = 1;
    }
    pthread_mutex_unlock(&g_xfer_mu);
    send_json(fd, 200, "{}", 2);
}

/* The upload's bytes (stream): ?id=<hex> from /api/upload/begin, Content-Length its size. */
static void s_upload_data(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd) {
    uint8_t id[16], *buf;
    char path[API_PATH], json[64];
    uint64_t size;
    int overwrite, rc;
    long long now = mono_ms();
    uint64_t left = r->content_length > pre_len ? r->content_length - pre_len : 0;
    if (strncmp(r->query, "id=", 3) != 0 || webui_parse_hex16(r->query + 3, id) != 0) {
        drain_then(fd, left);
        send_bad(fd, "bad_id");
        return;
    }
    pthread_mutex_lock(&g_xfer_mu);
    if (g_up.state != UP_RESERVED || memcmp(id, g_up.id, 16) != 0 || g_up.expires <= now) {
        pthread_mutex_unlock(&g_xfer_mu);
        drain_then(fd, left);
        send_error(fd, 409, "not_begun", 9);
        return;
    }
    if (r->content_length != g_up.size) {
        pthread_mutex_unlock(&g_xfer_mu);
        drain_then(fd, left);
        send_bad(fd, "bad_size");
        return;
    }
    g_up.state = UP_RECEIVING;
    g_up.cancel = 0;
    memcpy(path, g_up.path, sizeof path);
    size = g_up.size;
    overwrite = g_up.overwrite;
    pthread_mutex_unlock(&g_xfer_mu);
    buf = policy_write(path) ? malloc(WF_IO_BUF) : NULL;
    if (!buf) rc = policy_write(path) ? WF_E_IO : -1;
    else rc = wf_receive_file(fd, pre, pre_len, size, path, overwrite, 0777, WF_IDLE_MS, &g_up.cancel, buf, WF_IO_BUF);
    free(buf);
    pthread_mutex_lock(&g_xfer_mu);
    g_up.state = UP_FREE;
    memset(g_up.id, 0, sizeof g_up.id);
    pthread_mutex_unlock(&g_xfer_mu);
    if (rc == -1) {
        send_error(fd, 403, "path_not_allowed", 16);
    } else if (rc != WF_OK) {
        send_wf(fd, rc);
    } else {
        snprintf(json, sizeof json, "{\"size\":%llu}", (unsigned long long)size);
        send_json(fd, 200, json, strlen(json));
    }
}

/* A text save (stream): the body is one line of JSON {path, create?, mtime?, size?}, '\n', then the text
 * (at most WF_TEXT_MAX bytes, no NUL). Without create, mtime and size are the file as the editor read it:
 * a file changed since answers 409 changed. Answers {"mtime","size"} of the saved file. */
static void s_save_text(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd) {
    char path[API_PATH], json[128];
    uint8_t *body;
    const uint8_t *nl;
    size_t got, head_len;
    uint64_t total = r->content_length, mt = 0, sz = 0;
    uint32_t mns = 0;
    long long until = mono_ms() + XFER_TEXT_TOTAL_MS;
    wf_text_pre_t pc;
    int create = 0, rc;
    if (total > WF_TEXT_MAX + XFER_TEXT_HEAD) {
        drain_then(fd, total - pre_len);
        send_error(fd, 413, "too_large", 9);
        return;
    }
    {
        int both;
        pthread_mutex_lock(&g_xfer_mu);
        both = g_up.state == UP_RECEIVING && g_dl.state == DL_SENDING;
        pthread_mutex_unlock(&g_xfer_mu);
        if (both || pthread_mutex_trylock(&g_text_mu) != 0) {
            drain_then(fd, total - pre_len);
            send_error(fd, 409, "busy", 4);
            return;
        }
    }
    body = malloc(total ? (size_t)total : 1);
    if (!body) {
        pthread_mutex_unlock(&g_text_mu);
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    got = pre_len > total ? (size_t)total : pre_len;
    memcpy(body, pre, got);
    while (got < total && mono_ms() < until) {
        ssize_t k = wf_recv(fd, body + got, (size_t)total - got, XFER_IDLE_TEXT_MS, NULL);
        if (k <= 0) break;
        got += (size_t)k;
    }
    if (got < total) {
        free(body);
        pthread_mutex_unlock(&g_text_mu);
        send_error(fd, 400, "incomplete", 10);
        return;
    }
    nl = memchr(body, '\n', got < XFER_TEXT_HEAD ? got : XFER_TEXT_HEAD);
    if (!nl) {
        free(body);
        pthread_mutex_unlock(&g_text_mu);
        send_bad(fd, "bad_request");
        return;
    }
    head_len = (size_t)(nl - body);
    memset(&pc, 0, sizeof pc);
    rc = need_path(fd, (const char *)body, head_len, "path", path, sizeof path);
    if (rc == 0 && webui_json_bool((const char *)body, head_len, "create", &create) < 0) {
        send_bad(fd, "bad_create");
        rc = -1;
    }
    if (rc == 0 && !create &&
        (webui_json_u64((const char *)body, head_len, "mtime", &pc.mtime) != 1 ||
         webui_json_u64((const char *)body, head_len, "size", &pc.size) != 1)) {
        send_bad(fd, "bad_precondition");
        rc = -1;
    }
    if (rc == 0 && !create) {
        uint64_t ns = 0;
        int r2 = webui_json_u64((const char *)body, head_len, "mtime_ns", &ns);
        if (r2 < 0 || ns >= 1000000000ull) {
            send_bad(fd, "bad_precondition");
            rc = -1;
        } else if (r2 == 1) {
            pc.have_ns = 1;
            pc.mtime_ns = (uint32_t)ns;
        }
    }
    if (rc == 0 && !policy_write(path)) {
        send_error(fd, 403, "path_not_allowed", 16);
        rc = -1;
    }
    if (rc == 0) {
        char dir[API_PATH];
        if (folder_of(path, dir, sizeof dir) == 0) (void)wf_sweep_stale(dir, time(NULL), WF_STALE_PART_S);
        pc.create = create;
        pc.check = !create;
        rc = wf_text_save(path, nl + 1, got - head_len - 1, &pc, &mt, &sz, &mns);
        if (rc != WF_OK) {
            send_wf(fd, rc);
        } else {
            snprintf(json, sizeof json, "{\"mtime\":%llu,\"size\":%llu,\"mtime_ns\":%lu}", (unsigned long long)mt,
                     (unsigned long long)sz, (unsigned long)mns);
            send_json(fd, 200, json, strlen(json));
        }
    }
    free(body);
    pthread_mutex_unlock(&g_text_mu);
}

/* One cheat store at a time: two pages saving the same name would race their renames. */
static pthread_mutex_t g_cheat_mu = PTHREAD_MUTEX_INITIALIZER;
#define CHEAT_STORE_BUF (256u * 1024u)

/* A cheat file the page downloaded from a community repo (stream): ?format=json|shn|mc4&name=<file>,
 * the raw bytes as the body (1..WEBUI_CHEAT_FILE_MAX). Stored as WEBUI_CHEATS_ROOT/<format>/<name>
 * (folders made when missing) through a same-folder temp + rename, replacing a file of that name. The
 * cheat engine reads its folders on every list, so the game's cheats show on the next read. Answers
 * {"ok":true,"format":..,"name":..,"size":N,"replaced":bool} (replaced: a file of that name was there). */
static void s_cheats_store(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd) {
    char fmt[WEBUI_CHEAT_FMT_CAP], name[WEBUI_CHEAT_NAME_MAX + 1], target[API_PATH], dir[API_PATH], json[256];
    const char *bad;
    uint8_t *buf;
    uint64_t total = r->content_length;
    uint64_t left = total > pre_len ? total - pre_len : 0;
    struct stat old;
    int rc, replaced;
    bad = webui_cheat_store_query(r->query, fmt, name);
    if (bad) {
        drain_then(fd, left);
        send_bad(fd, bad);
        return;
    }
    if (total == 0) {
        send_bad(fd, "empty");
        return;
    }
    if (total > WEBUI_CHEAT_FILE_MAX) {
        drain_then(fd, left);
        send_error(fd, 413, "too_large", 9);
        return;
    }
    if (pthread_mutex_trylock(&g_cheat_mu) != 0) {
        drain_then(fd, left);
        send_error(fd, 409, "busy", 4);
        return;
    }
    buf = malloc(CHEAT_STORE_BUF);
    if (!buf || webui_cheat_target(WEBUI_CHEATS_ROOT, fmt, name, target, sizeof target) != 0) {
        const char *why = buf ? "io_error" : "out_of_memory";
        pthread_mutex_unlock(&g_cheat_mu);
        free(buf);
        drain_then(fd, left);
        send_error(fd, 500, why, strlen(why));
        return;
    }
    if (folder_of(target, dir, sizeof dir) == 0) (void)wf_sweep_stale(dir, time(NULL), WF_STALE_PART_S);
    replaced = lstat(target, &old) == 0 && S_ISREG(old.st_mode); /* stores are serialized (g_cheat_mu) */
    rc = wf_receive_file(fd, pre, pre_len, total, target, 1, 0777, WF_IDLE_MS, NULL, buf, CHEAT_STORE_BUF);
    free(buf);
    pthread_mutex_unlock(&g_cheat_mu);
    if (rc != WF_OK) {
        send_wf(fd, rc);
        return;
    }
    rc = snprintf(json, sizeof json, "{\"ok\":true,\"format\":\"%s\",\"name\":\"%s\",\"size\":%llu,\"replaced\":%s}", fmt,
                  name, (unsigned long long)total, replaced ? "true" : "false"); /* both checked: [A-Za-z0-9._-] only */
    send_json(fd, 200, json, (size_t)rc);
}

/* A URL path segment: unreserved bytes as they are, the rest %XX. */
static size_t url_segment(char *out, size_t cap, const char *s) {
    static const char hex[] = "0123456789ABCDEF";
    size_t at = 0;
    for (; *s; s++) {
        unsigned char c = (unsigned char)*s;
        int plain = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' ||
                    c == '-' || c == '_' || c == '~';
        if (at + 4 >= cap) return 0;
        if (plain) out[at++] = (char)c;
        else {
            out[at++] = '%';
            out[at++] = hex[c >> 4];
            out[at++] = hex[c & 15];
        }
    }
    out[at] = '\0';
    return at;
}

/* {path} -> {"url":"/dl/<ticket>/<name>","name":"..","size":N|null}: a link the browser downloads with a
 * plain navigation (it cannot carry the token header; the ticket stands in for it: random, for this item
 * only, 60 s after it was made or last used). A file is sent with its length and Range; a folder as a
 * .tar streamed as it is read (no size known ahead). */
static void r_download_begin(const char *b, size_t n, uint8_t *out, int fd) {
    char path[API_PATH], hex[33], name[300], *seg, *json;
    const char *base;
    struct stat st;
    size_t at = 0, cap = 6u * 300u + 3u * 300u + 160u;
    int is_dir;
    long long now = mono_ms();
    (void)out;
    if (need_path(fd, b, n, "path", path, sizeof path) != 0) return;
    if (!policy_read(path)) {
        send_error(fd, 403, "path_not_allowed", 16);
        return;
    }
    if (stat(path, &st) != 0) {
        send_error(fd, 404, "gone", 4);
        return;
    }
    is_dir = S_ISDIR(st.st_mode);
    if (!is_dir && !S_ISREG(st.st_mode)) {
        send_error(fd, 409, "not_a_file", 10);
        return;
    }
    if (is_dir && path_tree_op_refused(path)) {
        send_error(fd, 403, "path_not_allowed", 16);
        return;
    }
    base = strrchr(path, '/');
    base = base && base[1] ? base + 1 : "ps5";
    if (snprintf(name, sizeof name, "%s%s", base, is_dir ? ".tar" : "") >= (int)sizeof name) {
        send_bad(fd, "bad_path");
        return;
    }
    pthread_mutex_lock(&g_xfer_mu);
    if (g_dl.state == DL_SENDING) {
        pthread_mutex_unlock(&g_xfer_mu);
        send_error(fd, 409, "busy", 4);
        return;
    }
    g_dl.state = DL_READY;
    arc4random_buf(g_dl.id, sizeof g_dl.id);
    memcpy(g_dl.path, path, strlen(path) + 1);
    g_dl.is_dir = is_dir;
    g_dl.expires = now + XFER_TICKET_MS;
    webui_hex16(g_dl.id, hex);
    pthread_mutex_unlock(&g_xfer_mu);
    seg = malloc(3u * sizeof name + 1);
    json = malloc(cap);
    if (!seg || !json || !url_segment(seg, 3u * sizeof name + 1, name)) {
        free(seg);
        free(json);
        send_error(fd, 500, "out_of_memory", 13);
        return;
    }
    put_fmt(json, cap, &at, "{\"url\":\"/dl/%s/%s\",", hex, seg);
    put_str(json, cap, &at, "name", name);
    if (is_dir) put_fmt(json, cap, &at, ",\"size\":null}");
    else put_fmt(json, cap, &at, ",\"size\":%llu}", (unsigned long long)st.st_size);
    send_json(fd, 200, json, at);
    free(seg);
    free(json);
}

/* A download response head (no Content-Length when `len` is negative: a .tar ends when the connection
 * does). */
static int send_dl_head(int fd, int status, const char *ctype, long long len, const char *extra, const char *name) {
    char head[2048], disp[1400], cl[48];
    int k;
    if (!wf_disposition(disp, sizeof disp, name)) disp[0] = '\0';
    if (len >= 0) snprintf(cl, sizeof cl, "Content-Length: %lld\r\n", len);
    else cl[0] = '\0';
    k = snprintf(head, sizeof head,
                 "HTTP/1.1 %d %s\r\nContent-Type: %s\r\n%s%s%sCache-Control: no-store\r\n"
                 "X-Content-Type-Options: nosniff\r\nContent-Security-Policy: sandbox\r\nConnection: close\r\n\r\n",
                 status, status == 206 ? "Partial Content" : status == 416 ? "Range Not Satisfiable" : "OK", ctype, cl,
                 extra, disp);
    if (k <= 0 || (size_t)k >= sizeof head) return -1;
    return wf_send_all(fd, head, (size_t)k, WF_IDLE_MS);
}

/* GET /dl/<ticket>/<name>: the item /api/download/begin named. */
static void serve_download(const webui_req_t *r, int fd) {
    char path[API_PATH], hexs[33], name[300], extra[160];
    const char *base;
    uint8_t id[16], *buf = NULL;
    struct stat st;
    int is_dir, in = -1;
    long long now = mono_ms();
    if (strcmp(r->method, "GET") != 0) {
        webui_send_json(fd, 405, "{\"error\":\"method_not_allowed\"}");
        return;
    }
    if (strlen(r->path) < 4 + 32 || (r->path[4 + 32] != '/' && r->path[4 + 32] != '\0')) {
        webui_send_json(fd, 404, "{\"error\":\"not_found\"}");
        return;
    }
    memcpy(hexs, r->path + 4, 32);
    hexs[32] = '\0';
    if (webui_parse_hex16(hexs, id) != 0) {
        webui_send_json(fd, 404, "{\"error\":\"not_found\"}");
        return;
    }
    pthread_mutex_lock(&g_xfer_mu);
    if (g_dl.state != DL_READY || memcmp(id, g_dl.id, 16) != 0 || g_dl.expires <= now) {
        pthread_mutex_unlock(&g_xfer_mu);
        webui_send_json(fd, g_dl.state == DL_SENDING && memcmp(id, g_dl.id, 16) == 0 ? 409 : 404,
                        "{\"error\":\"link_expired\"}");
        return;
    }
    g_dl.state = DL_SENDING;
    memcpy(path, g_dl.path, sizeof path);
    is_dir = g_dl.is_dir;
    pthread_mutex_unlock(&g_xfer_mu);

    base = strrchr(path, '/');
    base = base && base[1] ? base + 1 : "ps5";
    snprintf(name, sizeof name, "%s%s", base, is_dir ? ".tar" : "");
    buf = policy_read(path) ? malloc(WF_IO_BUF) : NULL;
    if (!buf) {
        webui_send_json(fd, 403, "{\"error\":\"path_not_allowed\"}");
    } else if (is_dir) {
        uint64_t sent = 0;
        protected_forms_t *pf = malloc(sizeof *pf);
        char *canon = malloc(PATH_MAX);
        /* The walk starts from the canonical root (it follows no link below it), so the trust store's
         * forms are resolved once here instead of a realpath per entry. */
        if (!pf || !canon || !realpath(path, canon) || stat(canon, &st) != 0 || !S_ISDIR(st.st_mode) ||
            path_tree_op_refused(canon)) {
            webui_send_json(fd, 404, "{\"error\":\"gone\"}");
        } else {
            path_protected_forms(pf->lex, pf->canon);
            if (send_dl_head(fd, 200, "application/x-tar", -1, "", name) == 0)
                (void)wf_send_tar(fd, canon, skip_protected, pf, WF_IDLE_MS, buf, WF_IO_BUF, &sent);
        }
        free(pf);
        free(canon);
    } else {
        uint64_t a = 0, z = 0;
        int rg;
        in = open(path, O_RDONLY | O_NONBLOCK);
        if (in < 0 || fstat(in, &st) != 0 || !S_ISREG(st.st_mode)) {
            webui_send_json(fd, 404, "{\"error\":\"gone\"}");
        } else {
            rg = wf_parse_range(r->range, (uint64_t)st.st_size, &a, &z);
            if (rg < 0) {
                snprintf(extra, sizeof extra, "Content-Range: bytes */%llu\r\n", (unsigned long long)st.st_size);
                (void)send_dl_head(fd, 416, "application/octet-stream", 0, extra, name);
            } else {
                if (rg == 0) {
                    a = 0;
                    z = st.st_size ? (uint64_t)st.st_size - 1 : 0;
                    snprintf(extra, sizeof extra, "Accept-Ranges: bytes\r\n");
                } else {
                    snprintf(extra, sizeof extra, "Accept-Ranges: bytes\r\nContent-Range: bytes %llu-%llu/%llu\r\n",
                             (unsigned long long)a, (unsigned long long)z, (unsigned long long)st.st_size);
                }
                {
                    uint64_t len = st.st_size ? z - a + 1 : 0;
                    if (send_dl_head(fd, rg ? 206 : 200, "application/octet-stream", (long long)len, extra, name) == 0)
                        (void)wf_send_file(fd, in, a, len, WF_IDLE_MS, buf, WF_IO_BUF);
                }
            }
        }
        if (in >= 0) close(in);
    }
    free(buf);
    pthread_mutex_lock(&g_xfer_mu);
    if (g_dl.state == DL_SENDING && memcmp(id, g_dl.id, 16) == 0) {
        g_dl.state = DL_READY; /* a resume (Range) may follow */
        g_dl.expires = mono_ms() + XFER_TICKET_MS;
    }
    pthread_mutex_unlock(&g_xfer_mu);
}

/* ---- input log ---- */

/* One append at a time (the file is a ring rewritten in place), and a rate gate: a page that
 * ignores its one-a-second rule is told 429 instead of making the helper write. Its last batch
 * ({"final":true}, as Controls closes or the page is hidden) may follow a regular one sooner, once
 * per window (webui_inputlog_gate_admit). */
static pthread_mutex_t g_inlog_mu = PTHREAD_MUTEX_INITIALIZER;
static webui_inputlog_gate g_inlog_gate = WEBUI_INPUTLOG_GATE_INIT;
#define INLOG_MIN_GAP_MS 500 /* half the page's period: its timer jitter never trips it */
#define INLOG_LINES_CAP (8u * 1024u)

static long long mono_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/* {events:["<line>",...]}: appended to WEBUI_INPUTLOG_PATH (webui_inputlog.c checks every line).
 * `out` (API_OUT_MAX, heap) holds the lines, then serves as the ring rewrite's scratch. */
static void r_input_log(const char *b, size_t n, uint8_t *out, int fd) {
    const char *arr;
    size_t alen;
    long lines;
    int rc, final = 0;
    _Static_assert(INLOG_LINES_CAP + WEBUI_INPUTLOG_CAP <= API_OUT_MAX, "input log scratch must fit the route buffer");
    if (n > WEBUI_INPUTLOG_BODY_MAX) {
        send_error(fd, 413, "too_large", 9);
        return;
    }
    if (webui_json_array(b, n, "events", &arr, &alen) != 1) {
        send_bad(fd, "bad_events");
        return;
    }
    lines = webui_inputlog_lines(arr, alen, (char *)out, INLOG_LINES_CAP);
    if (lines < 0) {
        send_bad(fd, "bad_events");
        return;
    }
    if (webui_json_bool(b, n, "final", &final) != 1) final = 0;
    pthread_mutex_lock(&g_inlog_mu);
    if (!webui_inputlog_gate_admit(&g_inlog_gate, mono_ms(), final != 0, INLOG_MIN_GAP_MS)) {
        pthread_mutex_unlock(&g_inlog_mu);
        send_error(fd, 429, "too_many", 8);
        return;
    }
    rc = webui_inputlog_append(WEBUI_INPUTLOG_PATH, (const char *)out, (size_t)lines, WEBUI_INPUTLOG_CAP,
                               (char *)out + INLOG_LINES_CAP, API_OUT_MAX - INLOG_LINES_CAP);
    pthread_mutex_unlock(&g_inlog_mu);
    if (rc != 0) send_error(fd, 500, "write_failed", 12);
    else send_json(fd, 200, "{\"ok\":true}", 11);
}

/* The allowlist: nothing else is reachable. */
static const struct {
    const char *path;
    api_fn fn;
} k_routes[] = {
    { "/api/status", r_status },
    { "/api/users", r_users },
    { "/api/volumes", r_volumes },
    { "/api/volumes/eject", r_volumes_eject },
    { "/api/volumes/unmount", r_volumes_unmount },
    { "/api/fan", r_fan },
    { "/api/fan/set", r_fan_set },
    { "/api/files/prefs", r_files_prefs },
    { "/api/files/prefs/set", r_files_prefs_set },
    { "/api/files/handoff/take", r_files_handoff_take },
    { "/api/files/handoff/put", r_files_handoff_put },
    { "/api/fs/list", r_fs_list },
    { "/api/fs/stat", r_fs_stat },
    { "/api/fs/mkdir", r_fs_mkdir },
    { "/api/fs/rename", r_fs_rename },
    { "/api/fs/read", r_fs_read },
    { "/api/fs/copy", r_fs_copy },
    { "/api/fs/delete", r_fs_delete },
    { "/api/fs/chmod", r_fs_chmod },
    { "/api/fs/hash", r_fs_hash },
    { "/api/fs/crc32", r_fs_crc32 },
    { "/api/fs/unpack", r_fs_unpack },
    { "/api/fs/unpack/info", r_fs_unpack_info },
    { "/api/fs/extract", r_fs_extract },
    { "/api/fs/extract/info", r_fs_extract_info },
    { "/api/upload/begin", r_upload_begin },
    { "/api/upload/cancel", r_upload_cancel },
    { "/api/download/begin", r_download_begin },
    { "/api/jobs", r_jobs },
    { "/api/jobs/status", r_jobs_status },
    { "/api/jobs/cancel", r_jobs_cancel },
    { "/api/app/close", r_app_close },
    { "/api/apps/db", r_apps_db },
    { "/api/apps/registered", r_apps_registered },
    { "/api/apps/hidden", r_apps_hidden },
    { "/api/apps/info", r_apps_info },
    { "/api/apps/launch", r_apps_launch },
    { "/api/apps/uninstall", r_apps_uninstall },
    { "/api/apps/icon", r_apps_icon },
    { "/api/proc/list", r_proc_list },
    { "/api/proc/kill", r_proc_kill },
    { "/api/pkg/info", r_pkg_info },
    { "/api/pkg/icon", r_pkg_icon },
    { "/api/pkg/install", r_pkg_install },
    { "/api/payload/load", r_payload_load },
    { "/api/settings", r_settings },
    { "/api/settings/set", r_settings_set },
    { "/api/input-log", r_input_log },
    { "/api/cheats/status", r_cheats_status },
    { "/api/cheats/list", r_cheats_list },
    { "/api/cheats/toggle", r_cheats_toggle },
    { "/api/cheats/engine", r_cheats_engine },
    { "/api/cheats/version", r_cheats_version },
    { "/api/index/start", r_index_start },
    { "/api/index/status", r_index_status },
    { "/api/index/search", r_index_search },
    { "/api/index/cancel", r_index_cancel },
    /* network shares (webui_smb.h) */
    { "/api/smb/shares", webui_smb_shares },
    { "/api/smb/discover", webui_smb_discover },
    { "/api/smb/save", webui_smb_save },
    { "/api/smb/remove", webui_smb_remove },
    { "/api/smb/list", webui_smb_list },
    { "/api/smb/stat", webui_smb_stat },
    { "/api/smb/mkdir", webui_smb_mkdir },
    { "/api/smb/copy", webui_smb_copy },
    { "/api/smb/delete", webui_smb_delete },
    { "/api/smb/job", webui_smb_job },
    { "/api/smb/cancel", webui_smb_cancel },
    { "/api/smb/jobs", webui_smb_jobs },
    { "/api/library/scan", webui_library_scan },
    { "/api/apps/register", r_apps_register },
    { "/api/cleanup/scan", r_cleanup_scan },
    { "/api/cleanup/status", r_cleanup_status },
    { "/api/cleanup/apply", r_cleanup_apply },
    { "/api/cleanup/apply_status", r_cleanup_apply_status },
    { "/api/cleanup/cancel", r_cleanup_cancel },
};

/* The page's own headers: same-origin fetches plus the two GitHub hosts the cheat finder reads
 * (raw.githubusercontent.com for the repos' index and cheat files, api.github.com for the one repo
 * listed through the git-trees API; both answer CORS-open, and only GET), inline script/style (the page is one file),
 * no-cache rather than no-store (revalidated on every visit, but storable: the appcache keeps it as
 * the offline copy; webui_offline.h),
 * data:/blob: images; nosniff is on every response already. Never framed (frame-ancestors plus
 * X-Frame-Options for older engines): with LAN access on, another site could otherwise frame the
 * page, whose token works inside the frame, and click its buttons for the user. */
#define PAGE_HEADERS                                                                       \
    "Content-Encoding: gzip\r\n"                                                          \
    "Cache-Control: no-cache\r\n"                                                         \
    "Content-Security-Policy: default-src 'self' 'unsafe-inline' data: blob:; "                \
    "connect-src 'self' https://raw.githubusercontent.com https://api.github.com; "        \
    "frame-ancestors 'none'\r\n"                                                          \
    "X-Frame-Options: DENY\r\n"
/* webui_send_ex builds the head in 512 bytes and sends nothing when it does not fit: the fixed
 * part (status line, Content-Type text/html, a 10-digit length, Connection, Cache-Control,
 * nosniff, final CRLF) is under 200 bytes, so the extra lines get at most 300. */
_Static_assert(sizeof(PAGE_HEADERS) <= 300, "PAGE_HEADERS must fit webui_send_ex's 512-byte head");

/* One language's strings (webui_strings.h): gzip'd JSON, cached for a year. The page asks with
 * ?v=<hash of the strings> (the query is not read here), so every build is a new URL. */
#define STRINGS_HEADERS                                       \
    "Content-Encoding: gzip\r\n"                              \
    "Cache-Control: public, max-age=31536000, immutable\r\n"
_Static_assert(sizeof(STRINGS_HEADERS) <= 300, "STRINGS_HEADERS must fit webui_send_ex's 512-byte head");

#define STRINGS_PREFIX "/strings/"

/* The offline copy's files (webui_offline.h): checked on every visit. */
#define OFFLINE_HEADERS "Cache-Control: no-cache\r\n"

/* GET /, GET /api/token, GET /strings/<lang>.json, GET /sw.js and GET /cache.appcache: the server
 * checked Host/Origin but no token (is_api is 0 for all of them). None reaches a helper method, so they stay outside the in-flight RPC
 * gate. The token goes out as JSON with no CORS headers: another site's page cannot read the reply,
 * and loaded through a <script> tag it executes nothing. Never serve it as JavaScript. A strings
 * path names one of the embedded languages exactly (webui_strings_find) or answers 404. */
static int serve_static(const webui_req_t *r, int fd) {
    int page = strcmp(r->path, "/") == 0;
    int strings = strncmp(r->path, STRINGS_PREFIX, sizeof STRINGS_PREFIX - 1) == 0;
    webui_offline_file_t off;
    int offline = webui_offline_find(r->path, &off);
    if (!page && !strings && !offline && strcmp(r->path, "/api/token") != 0) return 0;
    if (strcmp(r->method, "GET") != 0) {
        webui_send_json(fd, 405, "{\"error\":\"method_not_allowed\"}");
        return 1;
    }
    if (offline) {
        webui_send_ex(fd, 200, off.ctype, off.data, off.len, OFFLINE_HEADERS);
    } else if (strings) {
        const webui_strings_entry_t *e = webui_strings_find(webui_strings_table, webui_strings_count, r->path);
        if (!e) webui_send_json(fd, 404, "{\"error\":\"not_found\"}");
        else webui_send_ex(fd, 200, "application/json", e->data, e->len, STRINGS_HEADERS);
    } else if (page) {
        webui_send_ex(fd, 200, "text/html; charset=utf-8", webui_page_gz, webui_page_gz_len, PAGE_HEADERS);
    } else {
        char tok[65], json[96];
        int n;
        webui_token_copy(tok);
        if (!tok[0]) {
            webui_send_json(fd, 503, "{\"error\":\"stopping\"}");
            return 1;
        }
        n = snprintf(json, sizeof json, "{\"token\":\"%s\"}", tok);
        send_json(fd, 200, json, (size_t)n);
    }
    return 1;
}

/* The streamed routes (their bodies are read by the route: webui_server.h webui_stream_fn), POST only.
 * Part of the allowlist like k_routes. */
static const struct {
    const char *path;
    void (*fn)(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd);
} k_streams[] = {
    { "/api/fs/save-text", s_save_text },
    { "/api/upload/data", s_upload_data },
    { "/api/cheats/store", s_cheats_store },
};

int webui_api_stream_for(const webui_req_t *r) {
    size_t i;
    if (strcmp(r->method, "POST") != 0) return 0;
    for (i = 0; i < sizeof k_streams / sizeof k_streams[0]; i++)
        if (strcmp(r->path, k_streams[i].path) == 0) return 1;
    return 0;
}

int webui_api_stream(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd) {
    size_t i;
    for (i = 0; i < sizeof k_streams / sizeof k_streams[0]; i++) {
        if (strcmp(r->path, k_streams[i].path) == 0) {
            k_streams[i].fn(r, pre, pre_len, fd);
            return 0;
        }
    }
    webui_send_json(fd, 404, "{\"error\":\"not_found\"}");
    return 0;
}

int webui_api_route(const webui_req_t *r, const uint8_t *body, size_t body_len, int fd) {
    size_t i;
    uint8_t *out;
    if (serve_static(r, fd)) return 0;
    /* A download link (/api/download/begin): Host/Origin were checked; the ticket stands in for the
     * token a plain navigation cannot send. Outside the RPC gate: it reaches no helper method. */
    if (strncmp(r->path, "/dl/", 4) == 0) {
        serve_download(r, fd);
        return 0;
    }
    for (i = 0; i < sizeof k_routes / sizeof k_routes[0]; i++)
        if (strcmp(r->path, k_routes[i].path) == 0) break;
    if (i == sizeof k_routes / sizeof k_routes[0]) {
        webui_send_json(fd, 404, "{\"error\":\"not_found\"}");
        return 0;
    }
    if (strcmp(r->method, "POST") != 0) {
        webui_send_json(fd, 405, "{\"error\":\"method_not_allowed\"}");
        return 0;
    }
    /* Counted with the AVA1 workers' RPCs, so the exit (ava1_payload_stop) waits for this call
     * before it stops the data layer; refused once the exit has begun. Every route reaches the
     * helper (mgmt, data layer, job table), so the whole route is inside the gate. */
    if (ava1_server_local_rpc_enter() != 0) {
        webui_send_json(fd, 503, "{\"error\":\"stopping\"}");
        return 0;
    }
    /* Every call acts as the console page, never as whichever device this thread served last. */
    ava1_server_set_rpc_peer(k_console_id);
    out = malloc(API_OUT_MAX);
    if (!out) {
        ava1_server_local_rpc_leave();
        webui_send_json(fd, 500, "{\"error\":\"out_of_memory\"}");
        return 0;
    }
    k_routes[i].fn((const char *)body, body_len, out, fd);
    free(out);
    ava1_server_local_rpc_leave();
    return 0;
}
