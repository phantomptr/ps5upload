#include "protocol.h"

#include <string.h>
#include <stdio.h>
#include <stdarg.h>
#include <inttypes.h>

/* ── minimal flat-object JSON string-field reader ─────────────────────── */

/* Decode a JSON string starting at *p (which must point at the opening
 * quote). Writes the decoded bytes into out (cap), NUL-terminates, and
 * advances *p past the closing quote. Returns 0 on success, -1 on
 * malformed input or overflow. Handles \" \\ \/ \n \t \r \b \f and passes
 * other bytes through; \uXXXX is rejected (never sent by our clients). */
static int json_decode_string(const char **p, const char *end,
                              char *out, size_t cap) {
    const char *s = *p;
    if (s >= end || *s != '"') return -1;
    s++;
    size_t o = 0;
    while (s < end && *s != '"') {
        char c = *s++;
        if (c == '\\') {
            if (s >= end) return -1;
            char e = *s++;
            switch (e) {
                case '"':  c = '"';  break;
                case '\\': c = '\\'; break;
                case '/':  c = '/';  break;
                case 'n':  c = '\n'; break;
                case 't':  c = '\t'; break;
                case 'r':  c = '\r'; break;
                case 'b':  c = '\b'; break;
                case 'f':  c = '\f'; break;
                default:   return -1;
            }
        }
        if (o + 1 >= cap) return -1;
        out[o++] = c;
    }
    if (s >= end || *s != '"') return -1;
    out[o] = '\0';
    *p = s + 1;
    return 0;
}

/* Find `"key"` at an object-key position and, if the value is a string,
 * decode it into out. Returns 1 found, 0 absent, -1 malformed/overflow.
 * Only scans the flat top-level object (good enough for our requests). */
static int json_get_string(const char *buf, size_t len, const char *key,
                           char *out, size_t cap) {
    const char *p = buf;
    const char *end = buf + len;
    size_t klen = strlen(key);
    while (p < end) {
        if (*p != '"') { p++; continue; }
        /* p is at the opening quote of a string token; find its closing
         * quote so we always advance past the whole token — never landing
         * inside one, which would let a value's bytes look like a key. */
        const char *tok = p + 1;
        while (tok < end && *tok != '"') {
            if (*tok == '\\' && tok + 1 < end) tok++;
            tok++;
        }
        /* A token is our key only when its text matches exactly AND it sits
         * in key position, i.e. the next non-space byte is ':'. A value
         * string that happens to equal `key` fails the colon test and is
         * skipped like any other token. */
        if ((size_t)(tok - (p + 1)) == klen && memcmp(p + 1, key, klen) == 0) {
            const char *v = (tok < end) ? tok + 1 : end;
            while (v < end && (*v == ' ' || *v == '\t')) v++;
            if (v < end && *v == ':') {
                v++;
                while (v < end && (*v == ' ' || *v == '\t')) v++;
                if (v < end && *v == '"') {
                    return json_decode_string(&v, end, out, cap) == 0 ? 1 : -1;
                }
                return 0; /* key present but value not a string */
            }
        }
        p = (tok < end) ? tok + 1 : end;
    }
    return 0;
}

void inst_parse_request(const char *buf, size_t len, inst_request_t *req) {
    memset(req, 0, sizeof(*req));
    req->op = INST_OP_UNKNOWN;
    req->src = INST_SRC_NONE;

    if (buf == NULL || len == 0 || len > INST_REQ_MAX) {
        req->error = "bad_request";
        return;
    }

    char op[32];
    int got_op = json_get_string(buf, len, "op", op, sizeof(op));
    if (got_op != 1) { req->error = "bad_request"; return; }

    if (strcmp(op, "hello") == 0) {
        req->op = INST_OP_HELLO;
        req->error = NULL;
        return;
    }
    if (strcmp(op, "stop") == 0) {
        req->op = INST_OP_STOP;
        req->error = NULL;
        return;
    }
    if (strcmp(op, "job") == 0) {
        req->op = INST_OP_JOB;
        if (json_get_string(buf, len, "job", req->job, sizeof(req->job)) != 1) {
            req->error = "bad_request";
            return;
        }
        req->error = NULL;
        return;
    }
    if (strcmp(op, "install") == 0) {
        req->op = INST_OP_INSTALL;
        int have_url  = json_get_string(buf, len, "url",  req->url,  sizeof(req->url));
        int have_path = json_get_string(buf, len, "path", req->path, sizeof(req->path));
        (void)json_get_string(buf, len, "name_hint", req->name_hint,
                              sizeof(req->name_hint));
        if (have_url < 0 || have_path < 0) { req->error = "bad_request"; return; }
        if (have_url == 1 && have_path == 1) { req->error = "bad_request"; return; }
        if (have_url == 0 && have_path == 0) { req->error = "bad_request"; return; }
        req->src = (have_url == 1) ? INST_SRC_URL : INST_SRC_PATH;
        req->error = NULL;
        return;
    }

    req->error = "bad_request"; /* unknown op */
}

/* ── reply builders ───────────────────────────────────────────────────── */

/* snprintf wrapper: returns n on success, -1 if truncated or error. */
static int emit(char *out, size_t cap, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(out, cap, fmt, ap);
    va_end(ap);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

int inst_reply_hello(char *out, size_t cap, const char *version,
                     const char *fw, const char *state,
                     uint32_t init_rc, int escalated) {
    return emit(out, cap,
        "{\"ok\":true,\"version\":\"%s\",\"fw\":\"%s\",\"state\":\"%s\","
        "\"init_rc\":%" PRIu32 ",\"escalated\":%s}",
        version, fw, state, init_rc, escalated ? "true" : "false");
}

int inst_reply_install_ok(char *out, size_t cap, const char *job,
                          const char *via) {
    return emit(out, cap, "{\"ok\":true,\"job\":\"%s\",\"via\":\"%s\"}", job, via);
}

int inst_reply_job(char *out, size_t cap, const char *phase,
                   uint64_t bytes_served, uint64_t total, uint32_t code) {
    return emit(out, cap,
        "{\"ok\":true,\"phase\":\"%s\",\"bytes_served\":%" PRIu64
        ",\"total\":%" PRIu64 ",\"code\":%" PRIu32 "}",
        phase, bytes_served, total, code);
}

int inst_reply_ok(char *out, size_t cap) {
    return emit(out, cap, "{\"ok\":true}");
}

int inst_reply_err_busy(char *out, size_t cap, const char *job) {
    return emit(out, cap, "{\"ok\":false,\"error\":\"busy\",\"job\":\"%s\"}", job);
}

int inst_reply_err_not_ready(char *out, size_t cap, uint32_t init_rc) {
    return emit(out, cap,
        "{\"ok\":false,\"error\":\"not_ready\",\"init_rc\":%" PRIu32 "}", init_rc);
}

int inst_reply_err_str(char *out, size_t cap, const char *error) {
    return emit(out, cap, "{\"ok\":false,\"error\":\"%s\"}", error);
}

int inst_reply_err_sony(char *out, size_t cap, uint32_t code, const char *hint) {
    if (hint && hint[0] != '\0')
        return emit(out, cap,
            "{\"ok\":false,\"code\":%" PRIu32 ",\"hint\":\"%s\"}", code, hint);
    return emit(out, cap, "{\"ok\":false,\"code\":%" PRIu32 "}", code);
}

int inst_should_boot_wait(double uptime_seconds) {
    return uptime_seconds <= 120.0 ? 1 : 0;
}
