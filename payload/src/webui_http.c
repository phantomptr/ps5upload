/* HTTP request parser and response head for the console web UI. */
#include "webui_http.h"

#include <stdio.h>
#include <string.h>

static char lower(char c) { return (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c; }

/* ASCII case-insensitive compare of [a, a+alen) against a lowercase literal. */
static int name_is(const char *a, size_t alen, const char *lit) {
    size_t i;
    if (strlen(lit) != alen) return 0;
    for (i = 0; i < alen; i++)
        if (lower(a[i]) != lit[i]) return 0;
    return 1;
}

static const char *find_crlf(const char *p, const char *end) {
    while (p + 1 < end) {
        if (p[0] == '\r' && p[1] == '\n') return p;
        p++;
    }
    return NULL;
}

static int has_ctl(const char *p, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) {
        unsigned char c = (unsigned char)p[i];
        if ((c < 0x20 && c != '\t') || c == 0x7f) return 1;
    }
    return 0;
}

/* Request line: METHOD SP target SP HTTP/1.x. */
static int parse_request_line(const char *p, size_t n, webui_req_t *out) {
    const char *sp1, *sp2, *q;
    size_t mlen, tlen, plen, qlen;
    if (has_ctl(p, n)) return WEBUI_PARSE_BAD;
    sp1 = memchr(p, ' ', n);
    if (!sp1) return WEBUI_PARSE_BAD;
    mlen = (size_t)(sp1 - p);
    if (mlen == 0 || mlen >= sizeof out->method) return WEBUI_PARSE_BAD;
    {
        size_t i;
        for (i = 0; i < mlen; i++)
            if (p[i] < 'A' || p[i] > 'Z') return WEBUI_PARSE_BAD;
    }
    sp2 = memchr(sp1 + 1, ' ', n - mlen - 1);
    if (!sp2) return WEBUI_PARSE_BAD;
    tlen = (size_t)(sp2 - (sp1 + 1));
    if (tlen == 0 || sp1[1] != '/') return WEBUI_PARSE_BAD;
    {
        size_t vlen = n - (size_t)(sp2 + 1 - p);
        if (vlen != 8 || memcmp(sp2 + 1, "HTTP/1.", 7) != 0 ||
            (sp2[8] != '0' && sp2[8] != '1'))
            return WEBUI_PARSE_BAD;
    }
    q = memchr(sp1 + 1, '?', tlen);
    plen = q ? (size_t)(q - (sp1 + 1)) : tlen;
    qlen = q ? tlen - plen - 1 : 0;
    if (plen > WEBUI_PATH_CAP || qlen > WEBUI_PATH_CAP) return WEBUI_PARSE_TOO_BIG;
    memcpy(out->method, p, mlen);
    out->method[mlen] = '\0';
    memcpy(out->path, sp1 + 1, plen);
    out->path[plen] = '\0';
    if (qlen) memcpy(out->query, q + 1, qlen);
    out->query[qlen] = '\0';
    return WEBUI_PARSE_OK;
}

static int copy_value(char *dst, size_t cap, const char *v, size_t n) {
    if (n >= cap) return WEBUI_PARSE_BAD;
    memcpy(dst, v, n);
    dst[n] = '\0';
    return WEBUI_PARSE_OK;
}

int webui_http_parse(const char *buf, size_t len, webui_req_t *out) {
    return webui_http_parse_cap(buf, len, out, WEBUI_BODY_CAP);
}

int webui_http_parse_cap(const char *buf, size_t len, webui_req_t *out, uint64_t body_cap) {
    const char *end = buf + len, *p, *eol, *term = NULL;
    size_t scan, i;
    int rc, have_cl = 0, have_host = 0, have_origin = 0, have_token = 0, have_range = 0;

    memset(out, 0, sizeof *out);

    /* Locate the blank line within the header cap. */
    scan = len < WEBUI_HEADER_CAP ? len : WEBUI_HEADER_CAP;
    for (i = 0; i + 3 < scan; i++) {
        if (buf[i] == '\r' && buf[i + 1] == '\n' && buf[i + 2] == '\r' &&
            buf[i + 3] == '\n') {
            term = buf + i;
            break;
        }
    }
    /* Reject an over-long request line as soon as its end is known. */
    eol = find_crlf(buf, end);
    if (!term) {
        if (len > WEBUI_HEADER_CAP) return WEBUI_PARSE_TOO_BIG;
        if (!eol) {
            /* No request line yet: bound by the longest legal line, i.e.
             * method(7) SP path(1024) '?' query(1024) SP HTTP/1.x(8), plus
             * a possible lone '\r' of the CRLF. Must match
             * parse_request_line so the verdict never depends on how the
             * bytes were segmented. */
            if (len > 7 + 1 + WEBUI_PATH_CAP + 1 + WEBUI_PATH_CAP + 1 + 8 + 1)
                return WEBUI_PARSE_TOO_BIG;
            return WEBUI_PARSE_MORE;
        }
    }
    if (!eol) return WEBUI_PARSE_BAD;
    rc = parse_request_line(buf, (size_t)(eol - buf), out);
    if (rc != WEBUI_PARSE_OK) return rc;
    if (!term) return WEBUI_PARSE_MORE;

    out->header_len = (size_t)(term - buf) + 4;
    p = eol + 2;
    while (p < term + 2) {
        const char *colon, *v, *ve;
        size_t nlen;
        eol = find_crlf(p, term + 4);
        if (!eol || eol == p) break;
        if (*p == ' ' || *p == '\t') return WEBUI_PARSE_BAD; /* obs-fold */
        if (has_ctl(p, (size_t)(eol - p))) return WEBUI_PARSE_BAD;
        colon = memchr(p, ':', (size_t)(eol - p));
        if (!colon || colon == p) return WEBUI_PARSE_BAD;
        nlen = (size_t)(colon - p);
        v = colon + 1;
        ve = eol;
        while (v < ve && (*v == ' ' || *v == '\t')) v++;
        while (ve > v && (ve[-1] == ' ' || ve[-1] == '\t')) ve--;

        if (name_is(p, nlen, "host")) {
            if (have_host) return WEBUI_PARSE_BAD;
            have_host = 1;
            rc = copy_value(out->host, sizeof out->host, v, (size_t)(ve - v));
        } else if (name_is(p, nlen, "origin")) {
            if (have_origin) return WEBUI_PARSE_BAD;
            have_origin = 1;
            rc = copy_value(out->origin, sizeof out->origin, v, (size_t)(ve - v));
        } else if (name_is(p, nlen, "x-ps5u-token")) {
            if (have_token) return WEBUI_PARSE_BAD;
            have_token = 1;
            rc = copy_value(out->token, sizeof out->token, v, (size_t)(ve - v));
        } else if (name_is(p, nlen, "range")) {
            if (have_range) return WEBUI_PARSE_BAD;
            have_range = 1;
            /* longer than any range a browser sends: ignored (the whole file), never a 400 */
            if ((size_t)(ve - v) < sizeof out->range) rc = copy_value(out->range, sizeof out->range, v, (size_t)(ve - v));
            else {
                out->range[0] = '\0';
                rc = WEBUI_PARSE_OK;
            }
        } else if (name_is(p, nlen, "transfer-encoding")) {
            return WEBUI_PARSE_BAD;
        } else if (name_is(p, nlen, "content-length")) {
            uint64_t n = 0;
            const char *d;
            if (have_cl || v == ve) return WEBUI_PARSE_BAD;
            have_cl = 1;
            for (d = v; d < ve; d++) {
                if (*d < '0' || *d > '9') return WEBUI_PARSE_BAD;
                n = n * 10 + (uint64_t)(*d - '0');
                if (n > body_cap) {
                    /* keep validating digits so garbage is BAD, not TOO_BIG */
                    n = body_cap + 1;
                }
            }
            if (n > body_cap) return WEBUI_PARSE_TOO_BIG;
            out->content_length = n;
            rc = WEBUI_PARSE_OK;
        } else {
            rc = WEBUI_PARSE_OK;
        }
        if (rc != WEBUI_PARSE_OK) return rc;
        p = eol + 2;
    }
    return WEBUI_PARSE_OK;
}

static const char *status_text(int status) {
    switch (status) {
    case 200: return "OK";
    case 204: return "No Content";
    case 206: return "Partial Content";
    case 400: return "Bad Request";
    case 401: return "Unauthorized";
    case 403: return "Forbidden";
    case 404: return "Not Found";
    case 405: return "Method Not Allowed";
    case 408: return "Request Timeout";
    case 413: return "Payload Too Large";
    case 409: return "Conflict";
    case 410: return "Gone";
    case 414: return "URI Too Long";
    case 416: return "Range Not Satisfiable";
    case 429: return "Too Many Requests";
    case 431: return "Request Header Fields Too Large";
    case 500: return "Internal Server Error";
    case 503: return "Service Unavailable";
    case 507: return "Insufficient Storage";
    default:  return "Unknown";
    }
}

/* Does a header block ("Name: value\r\n"...) have a line starting with `name` (lowercase, ending
 * in ':'), compared without regard to case? */
static int has_line(const char *h, const char *name) {
    size_t n = strlen(name);
    while (h && *h) {
        size_t i = 0;
        while (i < n && h[i] && lower(h[i]) == name[i]) i++;
        if (i == n) return 1;
        h = strstr(h, "\r\n");
        if (h) h += 2;
    }
    return 0;
}

int webui_http_head(char *out, size_t cap, int status, const char *content_type,
                    size_t body_len, const char *extra_headers) {
    int n, own_cache;
    size_t used;
    if (!out || cap == 0) return -1;
    /* no-store unless the extra headers carry their own Cache-Control line (the page, the strings
     * files, the offline copy), in any letter case. */
    own_cache = has_line(extra_headers, "cache-control:");
    n = snprintf(out, cap,
                 "HTTP/1.1 %d %s\r\n"
                 "Content-Type: %s\r\n"
                 "Content-Length: %lu\r\n"
                 "Connection: close\r\n"
                 "%s"
                 "X-Content-Type-Options: nosniff\r\n",
                 status, status_text(status),
                 content_type ? content_type : "application/octet-stream",
                 (unsigned long)body_len, own_cache ? "" : "Cache-Control: no-store\r\n");
    if (n < 0 || (size_t)n >= cap) return -1;
    used = (size_t)n;
    if (extra_headers && *extra_headers) {
        size_t el = strlen(extra_headers);
        if (used + el >= cap) return -1;
        memcpy(out + used, extra_headers, el);
        used += el;
    }
    if (used + 2 >= cap) return -1;
    out[used++] = '\r';
    out[used++] = '\n';
    out[used] = '\0';
    return (int)used;
}
