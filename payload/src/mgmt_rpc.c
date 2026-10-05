/* AVA1 management dispatcher: see include/mgmt_rpc.h. */
#include "mgmt_rpc.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ava1_gen.h"
#include "ava1_wire.h"

/* The sink's room for a paged method's whole (unpaged) answer. */
#define MGMT_PAGED_CAPTURE_MAX (1024u * 1024u)
/* MgmtText overhead: u32 length + u16 ext count, plus a `more` ext (tag, length, value). */
#define MGMT_TEXT_OVERHEAD 13u

static struct {
    const mgmt_entry_t *table;
    size_t n;
    void *state;
    void (*enter)(uint16_t);
    void (*leave)(void);
} G;

/* ---- the capture sink ---- */

typedef struct {
    uint8_t *buf; /* capacity cap + 1: always NUL-terminated */
    size_t cap;
    size_t len;
    int have;
    int is_error;
    int overflow;
} capture_t;

static __thread capture_t *g_capture;

int mgmt_capture_active(void) { return g_capture != NULL; }

int mgmt_capture_frame(uint16_t frame_type, const void *body, uint64_t len) {
    capture_t *c = g_capture;
    if (!c) return -1;
    /* The first error frame is the answer; a later frame cannot turn it back into a success. */
    if (c->is_error) return 0;
    if (len > c->cap) {
        c->overflow = 1;
        return -1;
    }
    if (len && body) memcpy(c->buf, body, (size_t)len);
    c->len = (size_t)len;
    c->buf[c->len] = '\0';
    c->have = 1;
    c->is_error = frame_type == MGMT_FRAME_ERROR;
    return 0;
}

void mgmt_reply_free(mgmt_reply_t *r) {
    if (!r) return;
    free(r->body);
    r->body = NULL;
    r->len = 0;
}

/* ---- replies ---- */

int mgmt_reply_error(mgmt_ctx_t *cx, int status, const char *cause) {
    size_t n = cause ? strlen(cause) : 0, i;
    if (n > MGMT_CAUSE_MAX) n = MGMT_CAUSE_MAX;
    if (n > cx->cap) n = cx->cap;
    /* The cause travels as UTF-8 text: keep it printable ASCII whatever the handler sent. */
    for (i = 0; i < n; i++) {
        unsigned char c = (unsigned char)cause[i];
        cx->out[i] = (c < 0x20 || c >= 0x7f) ? '?' : (uint8_t)c;
    }
    cx->out_len = n;
    return status;
}

int mgmt_reply_text(mgmt_ctx_t *cx, const char *text, size_t len, int more) {
    ava1_mgmt_text_t m;
    ava1_w_t w;
    memset(&m, 0, sizeof m);
    if (len > AVA1_RPC_TEXT_MAX) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    m.body = (const uint8_t *)text;
    m.body_len = (uint32_t)len;
    if (more >= 0) {
        m.has_more = 1;
        m.more = (uint8_t)(more ? 1 : 0);
    }
    ava1_w_init(&w, cx->out, cx->cap);
    if (ava1_mgmt_text_encode(&m, &w) != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    cx->out_len = w.len;
    return AVA1_STATUS_OK;
}

/* ---- legacy tokens -> statuses ---- */

static int has(const char *s, const char *needle) { return strstr(s, needle) != NULL; }

int mgmt_status_for_token(const char *t) {
    if (!t) return AVA1_ERR_INTERNAL;
    /* Order matters: the first rule that matches wins. */
    if (has(t, "cross_mount") || has(t, "cross_device")) return AVA1_ERR_CROSS_DEVICE;
    /* A path the node's policy refuses (the allowlist) is ERR_PATH; an OS permission refusal is not
     * a path problem. The schema has no permission code, so it is ERR_IO ("the filesystem refused"). */
    if (has(t, "missing")) return AVA1_ERR_PROTOCOL; /* an absent argument (register_src_path_missing) is the peer's, not a refused path */
    if (has(t, "path") || has(t, "not_allowed")) return AVA1_ERR_PATH;
    if (has(t, "denied") || has(t, "permission") || has(t, "eacces") || has(t, "eperm")) return AVA1_ERR_IO;
    if (has(t, "no_space") || has(t, "enospc") || has(t, "disk_full")) return AVA1_ERR_NO_SPACE;
    if (has(t, "already_running") || has(t, "busy") || has(t, "in_progress")) return AVA1_ERR_BUSY;
    if (has(t, "exists") || has(t, "eexist") || has(t, "already")) return AVA1_ERR_EXISTS;
    if (has(t, "unknown_job") || has(t, "no_such_job") || has(t, "job_not_found")) return AVA1_ERR_UNKNOWN_JOB;
    if (has(t, "cancel")) return AVA1_ERR_CANCELLED;
    if (has(t, "required") || has(t, "bad_action") || has(t, "unknown_action")) return AVA1_ERR_PROTOCOL;
    if (has(t, "missing") || has(t, "invalid") || has(t, "malformed") || has(t, "too_large") || has(t, "bad_request") || has(t, "bad_address") ||
        has(t, "body_too"))
        return AVA1_ERR_PROTOCOL;
    if (has(t, "read_failed") || has(t, "write_failed") || has(t, "open_failed") || has(t, "mkdir") || has(t, "_io_") ||
        has(t, "errno"))
        return AVA1_ERR_IO;
    return AVA1_ERR_INTERNAL;
}

static const char *skip_ws(const char *p, const char *end) {
    while (p < end && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) p++;
    return p;
}

/* Skips one JSON value starting at p (string, object, array or scalar); returns the byte after it, or NULL
 * when the text ends first. Strings honour backslash escapes; nesting is counted. */
static const char *skip_value(const char *p, const char *end) {
    int depth = 0;
    while (p < end) {
        char c = *p;
        if (c == '"') {
            for (p++; p < end && *p != '"'; p++)
                if (*p == '\\' && p + 1 < end) p++;
            if (p >= end) return NULL;
            p++;
            if (depth == 0) return p;
            continue;
        }
        if (c == '{' || c == '[') depth++;
        else if (c == '}' || c == ']') {
            if (depth == 0) return p; /* the enclosing container's end: a scalar ended before it */
            if (--depth == 0) return p + 1;
        } else if (depth == 0 && (c == ',' || c == ' ' || c == '\t' || c == '\n' || c == '\r')) {
            return p;
        }
        p++;
    }
    return depth == 0 ? p : NULL;
}

/* The value of the TOP-LEVEL key `key` of the JSON object `json` (keys inside nested objects and text
 * inside strings never match): a pointer to its first byte, or NULL. */
static const char *top_value(const char *json, size_t len, const char *key) {
    const char *end = json + len, *p = skip_ws(json, end);
    size_t kl = strlen(key);
    if (p >= end || *p != '{') return NULL;
    p++;
    for (;;) {
        const char *ks, *ke;
        p = skip_ws(p, end);
        if (p >= end || *p != '"') return NULL; /* '}' (no more keys) or malformed */
        ks = p + 1;
        ke = skip_value(p, end);
        if (!ke) return NULL;
        p = skip_ws(ke, end);
        if (p >= end || *p != ':') return NULL;
        p = skip_ws(p + 1, end);
        if (p >= end) return NULL;
        if ((size_t)(ke - 1 - ks) == kl && memcmp(ks, key, kl) == 0) return p;
        p = skip_value(p, end);
        if (!p) return NULL;
        p = skip_ws(p, end);
        if (p < end && *p == ',') p++;
    }
}

/* Copies a top-level string value into out; 1 when found. */
static int json_string_value(const char *json, size_t len, const char *key, char *out, size_t cap) {
    const char *end = json + len, *p = top_value(json, len, key), *q;
    size_t n = 0;
    if (!p || *p != '"') return 0;
    for (q = p + 1; q < end && *q != '"'; q++) {
        if (*q == '\\' && q + 1 < end) q++;
        if (n + 1 < cap) out[n++] = *q;
    }
    if (cap) out[n] = '\0';
    return 1;
}

int mgmt_legacy_failure(const char *body, size_t len, char *token, size_t token_cap) {
    const char *end = body + len, *v = top_value(body, len, "ok");
    if (token_cap) token[0] = '\0';
    if (!v || end - v < 5 || strncmp(v, "false", 5) != 0) return 0;
    if (token_cap &&
        !json_string_value(body, len, "err", token, token_cap) &&
        !json_string_value(body, len, "error", token, token_cap) &&
        !json_string_value(body, len, "reason", token, token_cap))
        snprintf(token, token_cap, "legacy_failure");
    return 1;
}

/* ---- calling a legacy handler ---- */

/* How a normal frame whose body is {"ok":false,...} is treated:
 *   LC_CONVERT  it becomes an error status (a method's contract);
 *   LC_KEEP     it is the answer (an operation's body, fsck's code, a backup's err text);
 *   LC_PROBE    it is the answer (a probe's negative result), except a malformed request
 *               ("bad_*" token), which stays an error. */
enum { LC_KEEP = 0, LC_CONVERT = 1, LC_PROBE = 2, LC_CONVERT_KEEP = 3 };
/*   LC_CONVERT_KEEP  an error status whose cause is the whole body up to MGMT_KEEP_MAX (the
 *               caller needs its fields: a mount's code, a time.set err_code), else its token. */
#define MGMT_KEEP_MAX 1024u
static int legacy_call(mgmt_ctx_t *cx, mgmt_legacy_fn fn, const char *req, size_t req_len, size_t capture_cap,
                       mgmt_reply_t *rep, int mode) {
    capture_t c;
    char *rq, token[MGMT_CAUSE_MAX + 1];
    int rc;
    memset(&c, 0, sizeof c);
    rep->body = NULL;
    rep->len = 0;
    /* The handler reads its request as a C string: an embedded NUL would silently cut it short. */
    if (req_len && memchr(req, '\0', req_len)) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "request contains NUL");
    rq = malloc(req_len + 1);
    c.buf = malloc(capture_cap + 1);
    if (!rq || !c.buf) {
        free(rq);
        free(c.buf);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "out of memory");
    }
    if (req_len) memcpy(rq, req, req_len);
    rq[req_len] = '\0'; /* every legacy handler reads its request as a C string */
    c.cap = capture_cap;
    g_capture = &c;
    rc = fn(cx->state, -1, 0, rq, req_len);
    g_capture = NULL;
    free(rq);
    if (c.overflow) {
        free(c.buf);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    }
    if (!c.have) {
        free(c.buf);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, rc == 0 ? "handler sent no reply" : "handler failed");
    }
    if (c.is_error) {
        int st = mgmt_status_for_token((const char *)c.buf);
        mgmt_reply_error(cx, st, (const char *)c.buf);
        free(c.buf);
        return st;
    }
    if (mode != LC_KEEP && mgmt_legacy_failure((const char *)c.buf, c.len, token, sizeof token) &&
        !(mode == LC_PROBE && strstr(token, "bad_") == NULL)) {
        int st = mgmt_status_for_token(token);
        if (mode == LC_CONVERT_KEEP) {
            size_t n = c.len < MGMT_KEEP_MAX && c.len <= cx->cap ? c.len : 0;
            if (n) {
                memcpy(cx->out, c.buf, n);
                cx->out_len = n;
            } else {
                mgmt_reply_error(cx, st, token);
            }
        } else {
            /* the whole body (err, errno, reason, codes) when it fits a cause, else its token; a
             * probe's refusal is a malformed request, whose token is the whole story */
            mgmt_reply_error(cx, st, mode != LC_PROBE && c.len <= MGMT_CAUSE_MAX ? (const char *)c.buf : token);
        }
        free(c.buf);
        return st;
    }
    rep->body = c.buf;
    rep->len = c.len;
    return AVA1_STATUS_OK;
}

int mgmt_legacy_call(mgmt_ctx_t *cx, mgmt_legacy_fn fn, const char *req, size_t req_len, size_t capture_cap,
                     mgmt_reply_t *rep) {
    return legacy_call(cx, fn, req, req_len, capture_cap, rep, LC_CONVERT);
}

/* ---- job.run operations ---- */

static __thread ava1_op_ctx_t *g_op_cur;

int mgmt_op_cancelled(void) { return g_op_cur && ava1_op_cancelled(g_op_cur); }
void mgmt_op_progress(uint64_t files, uint64_t bytes) {
    if (g_op_cur) ava1_op_add(g_op_cur, files, bytes);
}
void mgmt_op_total(uint64_t files, uint64_t bytes) {
    if (g_op_cur) ava1_op_set_total(g_op_cur, files, bytes);
}

static int op_entry_run(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    const mgmt_op_entry_t *e = arg;
    mgmt_ctx_t cx;
    mgmt_reply_t rep;
    uint8_t cause[MGMT_CAUSE_MAX + 1];
    int rc;
    memset(&cx, 0, sizeof cx);
    cx.state = G.state;
    cx.out = cause;
    cx.cap = MGMT_CAUSE_MAX;
    if (G.enter) G.enter(e->legacy_frame);
    g_op_cur = c;
    rc = legacy_call(&cx, e->fn, (const char *)args, n, AVA1_OP_RESULT_MAX, &rep, LC_KEEP);
    g_op_cur = NULL;
    if (G.leave) G.leave();
    if (rc != AVA1_STATUS_OK) {
        cause[cx.out_len] = '\0';
        ava1_op_message(c, "%s", (const char *)cause);
        return rc;
    }
    rc = ava1_op_set_result(c, rep.body, rep.len);
    mgmt_reply_free(&rep);
    if (rc != 0) {
        ava1_op_message(c, "%s", MGMT_ERR_TRUNCATED);
        return AVA1_ERR_INTERNAL;
    }
    return AVA1_STATUS_OK;
}

int mgmt_rpc_install_ops(const mgmt_op_entry_t *table, size_t n) {
    size_t i;
    if (!table) return -1;
    for (i = 0; i < n; i++)
        if (table[i].op == 0 || !table[i].fn || ava1_op_register(table[i].op, op_entry_run, (void *)&table[i]) != 0)
            return -1;
    return 0;
}

/* The text capacity this call can carry: the reply buffer minus the MgmtText encoding. */
static size_t text_cap(const mgmt_ctx_t *cx) {
    size_t room = cx->cap > MGMT_TEXT_OVERHEAD ? cx->cap - MGMT_TEXT_OVERHEAD : 0;
    return room < AVA1_RPC_TEXT_MAX ? room : AVA1_RPC_TEXT_MAX;
}

/* The request's MgmtText body, or empty for an empty request. 0, or an AVA1 status. */
static int text_request(mgmt_ctx_t *cx, const uint8_t *req, uint32_t n, const char **body, uint32_t *body_len) {
    *body = "";
    *body_len = 0;
    if (n) {
        ava1_mgmt_text_t m;
        if (ava1_mgmt_text_decode(req, n, &m) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad MgmtText request");
        *body = (const char *)m.body;
        *body_len = m.body_len;
    }
    return AVA1_STATUS_OK;
}

int mgmt_text_call(mgmt_ctx_t *cx, const uint8_t *req, uint32_t req_len, mgmt_legacy_fn fn, int has_body) {
    const char *b = "";
    uint32_t bl = 0;
    mgmt_reply_t rep;
    int rc;
    if (has_body && (rc = text_request(cx, req, req_len, &b, &bl)) != AVA1_STATUS_OK) return rc;
    rc = mgmt_legacy_call(cx, fn, b, bl, text_cap(cx), &rep);
    if (rc != AVA1_STATUS_OK) return rc;
    rc = mgmt_reply_text(cx, (const char *)rep.body, rep.len, -1);
    mgmt_reply_free(&rep);
    return rc;
}

/* ---- runner helpers named in mgmt_table.def ---- */

int mgmt_call_text(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    return mgmt_text_call(cx, req, n, fn, 1);
}

/* A clamped tail (log.klog, log.syslog; SPEC.md section 7.3): the handler may answer far more
 * text than one reply carries (kern.msgbuf: up to 1 MiB). The reply is the LAST text_cap bytes
 * (the newest lines are the ones a bug report needs), starting at a line boundary when the first
 * line in the window is cut, never in the middle of a UTF-8 sequence, with `more` = 1 when older
 * text was left out. A short answer is returned whole with `more` = 0. */
int mgmt_tail_window(const char *text, size_t len, size_t cap, size_t *start) {
    size_t s, i, scan;
    if (len <= cap) {
        *start = 0;
        return 0;
    }
    s = len - cap;
    scan = cap < 4096 ? cap : 4096;
    for (i = s + 1; i <= s + scan; i++)
        if (text[i - 1] == '\n') {
            s = i;
            goto aligned;
        }
    /* no line break near the cut: at least do not start inside a UTF-8 sequence */
    while (s < len && ((unsigned char)text[s] & 0xC0u) == 0x80u) s++;
aligned:
    *start = s;
    return 1;
}

int mgmt_call_tail(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    const char *b;
    uint32_t bl;
    mgmt_reply_t rep;
    size_t start = 0;
    int rc, clipped;
    if ((rc = text_request(cx, req, n, &b, &bl)) != AVA1_STATUS_OK) return rc;
    rc = mgmt_legacy_call(cx, fn, b, bl, MGMT_PAGED_CAPTURE_MAX, &rep);
    if (rc != AVA1_STATUS_OK) return rc;
    clipped = mgmt_tail_window((const char *)rep.body, rep.len, text_cap(cx), &start);
    rc = mgmt_reply_text(cx, (const char *)rep.body + start, rep.len - start, clipped);
    mgmt_reply_free(&rep);
    return rc;
}

/* A probe (net.reach): its negative answers are the measurement, so {"ok":false,...} with
 * the timeout, errno and elapsed time travels as a successful MgmtText. Only a malformed
 * request ("bad_request", "bad_address") is an error status. */
int mgmt_call_probe(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    const char *b;
    uint32_t bl;
    mgmt_reply_t rep;
    int rc;
    if ((rc = text_request(cx, req, n, &b, &bl)) != AVA1_STATUS_OK) return rc;
    rc = legacy_call(cx, fn, b, bl, text_cap(cx), &rep, LC_PROBE);
    if (rc != AVA1_STATUS_OK) return rc;
    rc = mgmt_reply_text(cx, (const char *)rep.body, rep.len, -1);
    mgmt_reply_free(&rep);
    return rc;
}

/* A MgmtText method whose failure body is data the caller needs: the whole {"ok":false,...} body is
 * the error cause (status from its "err" token), e.g. a mount's code or time.set's err_code. */
int mgmt_call_text_keep(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    const char *b;
    uint32_t bl;
    mgmt_reply_t rep;
    int rc;
    if ((rc = text_request(cx, req, n, &b, &bl)) != AVA1_STATUS_OK) return rc;
    rc = legacy_call(cx, fn, b, bl, text_cap(cx), &rep, LC_CONVERT_KEEP);
    if (rc != AVA1_STATUS_OK) return rc;
    rc = mgmt_reply_text(cx, (const char *)rep.body, rep.len, -1);
    mgmt_reply_free(&rep);
    return rc;
}

/* A method whose answer is empty (node.shutdown): the handler's body is dropped. */
int mgmt_call_empty(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    mgmt_reply_t rep;
    int rc;
    (void)req;
    (void)n;
    rc = mgmt_legacy_call(cx, fn, "", 0, 4096, &rep);
    if (rc != AVA1_STATUS_OK) return rc;
    mgmt_reply_free(&rep);
    cx->out_len = 0;
    return AVA1_STATUS_OK;
}

/* A paged text method: the request body is {"offset":N,"limit":M} (both optional); the reply
 * is the handler's JSON array cut to that window, `more` set when entries remain. */
int mgmt_call_paged(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    const char *b;
    uint32_t bl;
    uint64_t offset = 0, limit = 0;
    mgmt_reply_t rep;
    char *page;
    size_t page_len = 0, cap = text_cap(cx);
    int rc, more = 0, o_rc, l_rc;
    if ((rc = text_request(cx, req, n, &b, &bl)) != AVA1_STATUS_OK) return rc;
    {
        /* The window is read from a C string: the body is not NUL-terminated here. */
        char *z = malloc((size_t)bl + 1);
        if (!z) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "out of memory");
        memcpy(z, b, bl);
        z[bl] = '\0';
        o_rc = mgmt_json_u64(z, "offset", &offset);
        l_rc = mgmt_json_u64(z, "limit", &limit);
        free(z);
        if (o_rc < 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad offset");
        if (l_rc < 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad limit");
    }
    rc = mgmt_legacy_call(cx, fn, b, bl, MGMT_PAGED_CAPTURE_MAX, &rep);
    if (rc != AVA1_STATUS_OK) return rc;
    page = malloc(cap + 1);
    if (!page) {
        mgmt_reply_free(&rep);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "out of memory");
    }
    rc = mgmt_page_json_array((const char *)rep.body, rep.len, offset, limit, page, cap, &page_len, &more);
    mgmt_reply_free(&rep);
    if (rc != AVA1_STATUS_OK) {
        free(page);
        return mgmt_reply_error(cx, rc, rc == AVA1_ERR_INTERNAL ? MGMT_ERR_TRUNCATED : "bad paged reply");
    }
    rc = mgmt_reply_text(cx, page, page_len, more);
    free(page);
    return rc;
}

/* fs.mkdir: FsMkdir{path, mode, parents}. The legacy handler always creates the missing
 * parents (mkdir -p) with 0777 and ignores `mode`/`parents`; Task 4 decides whether to honour
 * them. The reply is empty. */
int mgmt_call_fs_mkdir(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    ava1_fs_mkdir_t q;
    mgmt_reply_t rep;
    char *json;
    size_t cap;
    int w, rc;
    if (ava1_fs_mkdir_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsMkdir request");
    cap = (size_t)q.path_len * 6 + 16;
    json = malloc(cap);
    if (!json) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "out of memory");
    memcpy(json, "{\"path\":\"", 9);
    w = mgmt_json_escape((const char *)q.path, q.path_len, json + 9, cap - 9);
    if (w < 0) {
        free(json);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "out of memory");
    }
    memcpy(json + 9 + w, "\"}", 3);
    rc = mgmt_legacy_call(cx, fn, json, 9 + (size_t)w + 2, 4096, &rep);
    free(json);
    if (rc != AVA1_STATUS_OK) return rc;
    mgmt_reply_free(&rep);
    cx->out_len = 0;
    return AVA1_STATUS_OK;
}

/* node.status: NodeStatus (SPEC.md section 7.3) built from the legacy handler's JSON. The legacy
 * keys the typed body drops (runtime_port, shutdown, takeover_requested and the old transaction
 * counters) are ignored. An absent number is 0, as an older payload's missing field was to the client. */
static int json_bool(const char *json, const char *key) {
    char needle[40];
    const char *p, *end = json + strlen(json);
    if (snprintf(needle, sizeof needle, "\"%s\"", key) >= (int)sizeof needle) return 0;
    p = strstr(json, needle);
    if (!p) return 0;
    p = skip_ws(p + strlen(needle), end);
    if (p >= end || *p != ':') return 0;
    p = skip_ws(p + 1, end);
    return p < end && *p == 't';
}

static uint64_t json_num(const char *json, const char *key) {
    uint64_t v = 0;
    return mgmt_json_u64(json, key, &v) == 1 ? v : 0;
}

int mgmt_call_node_status(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx, mgmt_legacy_fn fn) {
    mgmt_reply_t rep;
    ava1_node_status_t st;
    ava1_w_t w;
    char version[96], kernel[320], prior[40];
    uint64_t v;
    int rc;
    (void)req;
    (void)n;
    rc = mgmt_legacy_call(cx, fn, "", 0, 4096, &rep);
    if (rc != AVA1_STATUS_OK) return rc;
    memset(&st, 0, sizeof st);
    version[0] = kernel[0] = prior[0] = '\0';
    (void)json_string_value((const char *)rep.body, rep.len, "version", version, sizeof version);
    (void)json_string_value((const char *)rep.body, rep.len, "ps5_kernel", kernel, sizeof kernel);
    st.version = (const uint8_t *)version;
    st.version_len = (uint16_t)strlen(version);
    st.ps5_kernel = (const uint8_t *)kernel;
    st.ps5_kernel_len = (uint16_t)strlen(kernel);
    st.instance_id = json_num((const char *)rep.body, "instance_id");
    st.started_at_unix = json_num((const char *)rep.body, "started_at_unix");
    st.command_count = json_num((const char *)rep.body, "command_count");
    v = json_num((const char *)rep.body, "startup_reason");
    st.startup_reason = (uint16_t)(v > 0xffff ? 0xffff : v);
    st.ucred_elevated = (uint8_t)json_bool((const char *)rep.body, "ucred_elevated");
    v = json_num((const char *)rep.body, "max_transfer_streams");
    st.max_transfer_streams = (uint8_t)(v > 0xff ? 0xff : v);
    v = json_num((const char *)rep.body, "fan_threshold");
    st.fan_threshold = (uint16_t)(v > 0xffff ? 0xffff : v);
    v = json_num((const char *)rep.body, "fan_reapply_sec");
    st.fan_reapply_sec = (uint16_t)(v > 0xffff ? 0xffff : v);
    if (json_string_value((const char *)rep.body, rep.len, "prior_instance", prior, sizeof prior)) {
        st.has_prior_instance = 1;
        st.prior_instance = (const uint8_t *)prior;
        st.prior_instance_len = (uint16_t)strlen(prior);
    }
    mgmt_reply_free(&rep);
    ava1_w_init(&w, cx->out, cx->cap);
    if (ava1_node_status_encode(&st, &w) != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "bad status reply");
    cx->out_len = w.len;
    return AVA1_STATUS_OK;
}

/* ---- JSON helpers ---- */

int mgmt_json_escape(const char *s, size_t n, char *out, size_t cap) {
    static const char hex[] = "0123456789abcdef";
    size_t i, o = 0;
    for (i = 0; i < n; i++) {
        unsigned char c = (unsigned char)s[i];
        if (c == '"' || c == '\\') {
            if (o + 2 >= cap) return -1;
            out[o++] = '\\';
            out[o++] = (char)c;
        } else if (c < 0x20) {
            if (o + 6 >= cap) return -1;
            out[o++] = '\\';
            out[o++] = 'u';
            out[o++] = '0';
            out[o++] = '0';
            out[o++] = hex[c >> 4];
            out[o++] = hex[c & 15];
        } else {
            if (o + 1 >= cap) return -1;
            out[o++] = (char)c;
        }
    }
    if (o >= cap) return -1;
    out[o] = '\0';
    return (int)o;
}

int mgmt_json_u64(const char *json, const char *key, uint64_t *out) {
    char needle[40];
    const char *p, *end;
    uint64_t v = 0;
    int digits = 0;
    if (snprintf(needle, sizeof needle, "\"%s\"", key) >= (int)sizeof needle) return -1;
    p = strstr(json, needle);
    if (!p) return 0;
    end = json + strlen(json);
    p = skip_ws(p + strlen(needle), end);
    if (p >= end || *p != ':') return -1;
    p = skip_ws(p + 1, end);
    while (p < end && *p >= '0' && *p <= '9') {
        uint64_t d = (uint64_t)(*p - '0');
        if (v > (UINT64_MAX - d) / 10) return -1; /* overflow */
        v = v * 10 + d;
        p++;
        digits++;
    }
    if (!digits) return -1;
    /* an integer: not 1.5, 1e3 or a bare word glued on */
    if (p < end && (*p == '.' || *p == 'e' || *p == 'E' || *p == '_' || (*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z')))
        return -1;
    *out = v;
    return 1;
}

int mgmt_page_json_array(const char *json, size_t len, uint64_t offset, uint64_t limit, char *out, size_t cap,
                         size_t *out_len, int *more) {
    size_t i, lb = len, rb = len, start = 0, o;
    uint64_t idx = 0, written = 0, total = 0;
    int in_str = 0, depth = 0, have_el = 0;
    *out_len = 0;
    *more = 0;
    for (i = 0; i < len; i++) {
        if (json[i] == '"') in_str = !in_str; /* the prefix before '[' has no escaped quotes */
        else if (json[i] == '[' && !in_str) {
            lb = i;
            break;
        }
    }
    for (i = len; i > 0; i--)
        if (json[i - 1] == ']') {
            rb = i - 1;
            break;
        }
    if (lb >= len || rb >= len || rb < lb) return AVA1_ERR_INTERNAL;
    /* prefix through '[' */
    if (lb + 1 > cap) return AVA1_ERR_INTERNAL;
    memcpy(out, json, lb + 1);
    o = lb + 1;
    in_str = 0;
    start = lb + 1;
    for (i = lb + 1; i <= rb; i++) {
        int boundary = 0;
        char ch = i < rb ? json[i] : ',';
        if (i < rb) {
            if (in_str) {
                if (ch == '\\') i++;
                else if (ch == '"') in_str = 0;
                continue;
            }
            if (ch == '"') {
                in_str = 1;
                have_el = 1;
                continue;
            }
            if (ch == '{' || ch == '[') {
                depth++;
                have_el = 1;
                continue;
            }
            if (ch == '}' || ch == ']') {
                depth--;
                continue;
            }
            if (ch == ',' && depth == 0) boundary = 1;
            else if (ch != ' ' && ch != '\t' && ch != '\n' && ch != '\r') have_el = 1;
        } else {
            boundary = 1; /* the closing ']' ends the last element */
        }
        if (!boundary) continue;
        if (have_el) {
            size_t a = start, b = i;
            while (a < b && (json[a] == ' ' || json[a] == '\n' || json[a] == '\t' || json[a] == '\r')) a++;
            while (b > a && (json[b - 1] == ' ' || json[b - 1] == '\n' || json[b - 1] == '\t' || json[b - 1] == '\r')) b--;
            total++;
            if (idx >= offset && (limit == 0 || written < limit) && !*more) {
                size_t el = b - a, need = el + (written ? 1 : 0);
                /* keep room for the suffix ("]}" and whatever the handler closed with) */
                if (o + need + (len - rb) > cap) {
                    if (written == 0) return AVA1_ERR_INTERNAL; /* one element does not fit: never clip it */
                    *more = 1;
                } else {
                    if (written) out[o++] = ',';
                    memcpy(out + o, json + a, el);
                    o += el;
                    written++;
                }
            } else if (idx >= offset) {
                *more = 1;
            }
            idx++;
        }
        start = i + 1;
        have_el = 0;
    }
    if (o + (len - rb) > cap) return AVA1_ERR_INTERNAL;
    memcpy(out + o, json + rb, len - rb);
    o += len - rb;
    *out_len = o;
    if (offset + written < total) *more = 1;
    return AVA1_STATUS_OK;
}

/* ---- the table ---- */

int mgmt_rpc_install(const mgmt_entry_t *table, size_t n, void *state, void (*enter)(uint16_t),
                     void (*leave)(void)) {
    size_t i, j;
    if (!table && n) return -1;
    for (i = 0; i < n; i++) {
        if (!table[i].run) return -1;
        for (j = i + 1; j < n; j++)
            if (table[i].method == table[j].method) return -1;
    }
    G.table = table;
    G.n = n;
    G.state = state;
    G.enter = enter;
    G.leave = leave;
    return 0;
}

static const mgmt_entry_t *find_entry(uint16_t method) {
    size_t i;
    for (i = 0; i < G.n; i++)
        if (G.table[i].method == method) return &G.table[i];
    return NULL;
}

int mgmt_rpc_installed(void) { return G.n > 0; }

int mgmt_rpc_handles(uint16_t method) { return find_entry(method) != NULL; }

int mgmt_rpc_dispatch(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap,
                      size_t *out_len) {
    const mgmt_entry_t *e = find_entry(method);
    mgmt_ctx_t cx;
    int rc;
    *out_len = 0;
    if (!e || (e->flags & MGMT_LONG)) {
        static const char u[] = "unknown method";
        size_t n = sizeof u - 1 < cap ? sizeof u - 1 : cap;
        memcpy(out, u, n);
        *out_len = n;
        return AVA1_ERR_UNKNOWN_METHOD;
    }
    memset(&cx, 0, sizeof cx);
    cx.state = G.state;
    cx.out = out;
    cx.cap = cap;
    cx.entry = e;
    if (G.enter) G.enter(e->legacy_frame);
    rc = e->run(body, len, &cx);
    if (G.leave) G.leave();
    *out_len = cx.out_len;
    return rc;
}
