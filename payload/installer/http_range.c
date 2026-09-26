#define _GNU_SOURCE  /* strcasestr on glibc (CI host); harmless on macOS/BSD */
#include "http_range.h"

#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <ctype.h>
#include <stdarg.h>
#include <inttypes.h>

inst_range_result_t inst_parse_range(const char *rv, uint64_t total,
                                     uint64_t *start, uint64_t *end) {
    if (rv == NULL) return INST_RANGE_NONE;
    const char *b = strstr(rv, "bytes=");
    if (b == NULL) return INST_RANGE_NONE;
    b += 6;
    /* multi-range -> let caller serve the whole file */
    if (strchr(b, ',') != NULL) return INST_RANGE_NONE;
    const char *dash = strchr(b, '-');
    if (dash == NULL) return INST_RANGE_NONE;

    uint64_t s, e;
    if (dash == b) {
        /* suffix: bytes=-n */
        char *ep = NULL;
        unsigned long long n = strtoull(dash + 1, &ep, 10);
        if (ep == dash + 1) return INST_RANGE_NONE; /* no digits -> malformed */
        if (total == 0 || n == 0) return INST_RANGE_UNSAT;
        if (n > total) n = total;
        s = total - n;
        e = total - 1;
    } else {
        char *ep = NULL;
        unsigned long long a = strtoull(b, &ep, 10);
        if (ep != dash) return INST_RANGE_NONE; /* junk before dash */
        s = a;
        if (*(dash + 1) >= '0' && *(dash + 1) <= '9') {
            e = strtoull(dash + 1, NULL, 10);
        } else {
            e = (total > 0) ? total - 1 : 0; /* open-ended */
        }
    }
    if (total == 0 || s >= total || s > e) return INST_RANGE_UNSAT;
    if (e >= total) e = total - 1;
    *start = s;
    *end = e;
    return INST_RANGE_OK;
}

int inst_http_header(const char *req, const char *name, char *out, size_t cap) {
    size_t nlen = strlen(name);
    const char *p = req;
    /* skip the request line */
    const char *eol = strstr(p, "\r\n");
    if (eol == NULL) return 0;
    p = eol + 2;
    while (*p && !(p[0] == '\r' && p[1] == '\n')) {
        const char *line = p;
        const char *le = strstr(line, "\r\n");
        if (le == NULL) le = line + strlen(line);
        if ((size_t)(le - line) > nlen && line[nlen] == ':') {
            int match = 1;
            for (size_t i = 0; i < nlen; i++) {
                if (tolower((unsigned char)line[i]) !=
                    tolower((unsigned char)name[i])) { match = 0; break; }
            }
            if (match) {
                const char *v = line + nlen + 1;
                while (v < le && (*v == ' ' || *v == '\t')) v++;
                size_t vlen = (size_t)(le - v);
                if (vlen >= cap) vlen = cap - 1;
                memcpy(out, v, vlen);
                out[vlen] = '\0';
                return 1;
            }
        }
        if (*le == '\0') break;
        p = le + 2;
    }
    return 0;
}

int inst_http_percent_decode(const char *in, char *out, size_t cap) {
    size_t o = 0;
    for (const char *p = in; *p; p++) {
        char c = *p;
        if (c == '%') {
            if (!isxdigit((unsigned char)p[1]) || !isxdigit((unsigned char)p[2]))
                return -1;
            char h[3] = { p[1], p[2], 0 };
            c = (char)strtol(h, NULL, 16);
            p += 2;
        }
        if (o + 1 >= cap) return -1;
        out[o++] = c;
    }
    if (o >= cap) return -1;
    out[o] = '\0';
    return 0;
}

int inst_http_parse_head(const char *req, size_t len, inst_http_req_t *out) {
    (void)len;
    memset(out, 0, sizeof(*out));
    char raw_target[INST_HTTP_TARGET_MAX];
    char version[16];
    /* request line: METHOD SP TARGET SP VERSION */
    if (sscanf(req, "%7s %1023s %15s", out->method, raw_target, version) != 3)
        return -1;
    out->is_get  = (strcmp(out->method, "GET") == 0);
    out->is_head = (strcmp(out->method, "HEAD") == 0);
    /* strip query */
    char *q = strchr(raw_target, '?');
    if (q) *q = '\0';
    if (inst_http_percent_decode(raw_target, out->target, sizeof(out->target)) != 0)
        return -1;
    /* keep-alive basis: HTTP/1.1 default unless Connection: close */
    char conn[64];
    int has_conn = inst_http_header(req, "Connection", conn, sizeof(conn));
    int wants_close = has_conn && (strcasestr(conn, "close") != NULL);
    int wants_keep  = has_conn && (strcasestr(conn, "keep-alive") != NULL);
    int http11 = (strcmp(version, "HTTP/1.1") == 0);
    out->keep_alive = !wants_close && (wants_keep || http11);
    return 0;
}

int inst_http_imf_date(time_t t, char *out, size_t cap) {
    struct tm tmv;
    if (gmtime_r(&t, &tmv) == NULL) return -1;
    size_t n = strftime(out, cap, "%a, %d %b %Y %H:%M:%S GMT", &tmv);
    return n > 0 ? 0 : -1;
}

static int hdr_emit(char *out, size_t cap, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(out, cap, fmt, ap);
    va_end(ap);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

static const char *conn_tok(int keep_alive) {
    return keep_alive ? "keep-alive" : "close";
}

int inst_http_hdr_200(char *out, size_t cap, uint64_t total,
                      time_t date, time_t fixed_lm, int keep_alive) {
    char d[64], lm[64];
    if (inst_http_imf_date(date, d, sizeof(d)) != 0) return -1;
    if (inst_http_imf_date(fixed_lm, lm, sizeof(lm)) != 0) return -1;
    return hdr_emit(out, cap,
        "HTTP/1.1 200 OK\r\n"
        "Date: %s\r\n"
        "Last-Modified: %s\r\n"
        "Content-Type: application/octet-stream\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: %" PRIu64 "\r\n"
        "Connection: %s\r\n\r\n",
        d, lm, total, conn_tok(keep_alive));
}

int inst_http_hdr_206(char *out, size_t cap, uint64_t start, uint64_t end,
                      uint64_t total, time_t date, time_t fixed_lm, int keep_alive) {
    char d[64], lm[64];
    if (inst_http_imf_date(date, d, sizeof(d)) != 0) return -1;
    if (inst_http_imf_date(fixed_lm, lm, sizeof(lm)) != 0) return -1;
    uint64_t clen = end - start + 1;
    return hdr_emit(out, cap,
        "HTTP/1.1 206 Partial Content\r\n"
        "Date: %s\r\n"
        "Last-Modified: %s\r\n"
        "Content-Type: application/octet-stream\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: %" PRIu64 "\r\n"
        "Content-Range: bytes %" PRIu64 "-%" PRIu64 "/%" PRIu64 "\r\n"
        "Connection: %s\r\n\r\n",
        d, lm, clen, start, end, total, conn_tok(keep_alive));
}

int inst_http_hdr_416(char *out, size_t cap, uint64_t total, int keep_alive) {
    return hdr_emit(out, cap,
        "HTTP/1.1 416 Range Not Satisfiable\r\n"
        "Content-Range: bytes */%" PRIu64 "\r\n"
        "Accept-Ranges: bytes\r\n"
        "Content-Length: 0\r\n"
        "Connection: %s\r\n\r\n",
        total, conn_tok(keep_alive));
}

int inst_http_hdr_404(char *out, size_t cap, int keep_alive) {
    return hdr_emit(out, cap,
        "HTTP/1.1 404 Not Found\r\n"
        "Content-Length: 0\r\n"
        "Connection: %s\r\n\r\n",
        conn_tok(keep_alive));
}
