/* The loopback app API's pure part: request checks, routing and answers (see include/app_api.h).
 * No sockets and no Sony calls here: the operations come in through app_api_ops_t, so the host
 * self-test (payload/tests/app_api_selftest.c) drives every route with fakes. */
#include "app_api.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "webui_codec.h"
#include "webui_http.h"

static char lower(char c) { return (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c; }

static int same_nocase(const char *a, size_t an, const char *lit) {
    size_t i;
    if (strlen(lit) != an) return 0;
    for (i = 0; i < an; i++)
        if (lower(a[i]) != lit[i]) return 0;
    return 1;
}

int app_api_host_ok(const char *host, unsigned port) {
    char want[32];
    const char *colon;
    if (!host || !host[0] || port == 0 || port > 65535) return 0;
    colon = strrchr(host, ':');
    if (!colon) return 0; /* the app always names the port; a bare name is someone else's */
    snprintf(want, sizeof want, "%u", port);
    if (strcmp(colon + 1, want) != 0) return 0;
    return same_nocase(host, (size_t)(colon - host), "127.0.0.1") ||
           same_nocase(host, (size_t)(colon - host), "localhost");
}

int app_api_has_header(const char *buf, size_t header_len, const char *name) {
    size_t i = 0;
    /* skip the request line */
    while (i + 1 < header_len && !(buf[i] == '\r' && buf[i + 1] == '\n')) i++;
    i += 2;
    while (i < header_len) {
        size_t s = i, j;
        while (i + 1 < header_len && !(buf[i] == '\r' && buf[i + 1] == '\n')) i++;
        if (i == s) break; /* the blank line */
        for (j = s; j < i && buf[j] != ':'; j++) {
        }
        if (j < i && same_nocase(buf + s, j - s, name)) return 1;
        i += 2;
    }
    return 0;
}

int app_api_request_ready(const char *buf, size_t len, size_t *total) {
    webui_req_t r;
    size_t i, end = 0;
    int rc;
    for (i = 0; i + 3 < len; i++)
        if (buf[i] == '\r' && buf[i + 1] == '\n' && buf[i + 2] == '\r' && buf[i + 3] == '\n') {
            end = i + 4;
            break;
        }
    if (!end) return len >= APP_API_REQ_MAX ? -2 : 0;
    rc = webui_http_parse_cap(buf, end, &r, APP_API_BODY_MAX);
    if (rc == WEBUI_PARSE_TOO_BIG) return -2;
    if (rc != WEBUI_PARSE_OK) return -1;
    if (r.header_len + r.content_length > APP_API_REQ_MAX) return -2;
    if (total) *total = r.header_len + (size_t)r.content_length;
    return len >= r.header_len + r.content_length ? 1 : 0;
}

/* ---- Remote Play readiness ---- */

/* A signed integer field of a flat JSON object (the helper's own readiness JSON): 1 found, else 0. */
static int json_int(const char *s, size_t n, const char *key, long long *out) {
    char pat[48];
    const char *p = s, *e = s + n;
    long long v = 0;
    int neg = 0, digits = 0;
    size_t pl;
    snprintf(pat, sizeof pat, "\"%s\":", key);
    pl = strlen(pat);
    while (p + pl <= e && memcmp(p, pat, pl) != 0) p++;
    if (p + pl > e) return 0;
    p += pl;
    while (p < e && *p == ' ') p++;
    if (p < e && *p == '-') {
        neg = 1;
        p++;
    }
    while (p < e && *p >= '0' && *p <= '9' && digits < 19) {
        v = v * 10 + (*p - '0');
        p++;
        digits++;
    }
    if (!digits) return 0;
    *out = neg ? -v : v;
    return 1;
}

static int is_object(const char *s, size_t n) {
    while (n && (s[n - 1] == ' ' || s[n - 1] == '\n' || s[n - 1] == '\r')) n--;
    return n >= 2 && s[0] == '{' && s[n - 1] == '}';
}

int app_api_rp_flags(const char *raw, size_t n, int *service_on, int *user_on, int *needs_per_user) {
    long long svc, usr, per;
    if (!raw || !is_object(raw, n)) return -1;
    if (!json_int(raw, n, "service_enabled", &svc) || !json_int(raw, n, "has_per_user", &per)) return -1;
    if (!json_int(raw, n, "user_enabled", &usr)) usr = 0;
    if (service_on) *service_on = svc != 0;
    if (user_on) *user_on = usr != 0;
    if (needs_per_user) *needs_per_user = per != 0;
    return 0;
}

int app_api_rp_summary(const char *raw, size_t n, char *out, size_t cap) {
    int svc, usr, per, signed_in, activated, k;
    long long acct = 0, fg = 0, logins = 0;
    uint64_t id_raw = 0;
    char type[24] = "";
    while (n && (raw[n - 1] == ' ' || raw[n - 1] == '\n' || raw[n - 1] == '\r')) n--;
    if (app_api_rp_flags(raw, n, &svc, &usr, &per) != 0) return -1;
    (void)json_int(raw, n, "account_uid", &acct);
    (void)json_int(raw, n, "foreground_uid", &fg);
    (void)json_int(raw, n, "login_count", &logins);
    (void)webui_json_u64(raw, n, "account_id_raw", &id_raw);
    (void)webui_json_str(raw, n, "account_type", type, sizeof type);
    signed_in = acct > 0 || fg > 0 || logins > 0;
    activated = id_raw != 0 && strcmp(type, "np") == 0;
    k = snprintf(out, cap,
                 "{\"ready\":%s,\"service_on\":%s,\"user_on\":%s,\"needs_user_permission\":%s,"
                 "\"signed_in\":%s,\"psn_activated\":%s,\"readiness\":",
                 svc && (!per || usr) && activated ? "true" : "false", svc ? "true" : "false",
                 usr ? "true" : "false", per ? "true" : "false", signed_in ? "true" : "false",
                 activated ? "true" : "false");
    if (k < 0 || (size_t)k + n + 2 > cap) return -1;
    memcpy(out + k, raw, n);
    out[k + n] = '}';
    out[k + n + 1] = 0;
    return k + (int)n + 1;
}

/* ---- routes ---- */

enum {
    R_VERSION = 1,
    R_STATUS,
    R_RP,
    R_RP_ENABLE,
    R_RP_PIN_REQ,
    R_RP_PIN_GET,
    R_RP_PIN_CANCEL,
    R_MESH
};

static const struct {
    const char *method, *path;
    int id;
} ROUTES[] = {
    { "GET", "/v1/version", R_VERSION },
    { "GET", "/v1/status", R_STATUS },
    { "GET", "/v1/rp", R_RP },
    { "POST", "/v1/rp/enable", R_RP_ENABLE },
    { "POST", "/v1/rp/pin", R_RP_PIN_REQ },
    { "GET", "/v1/rp/pin", R_RP_PIN_GET },
    { "POST", "/v1/rp/pin/cancel", R_RP_PIN_CANCEL },
    { "GET", "/v1/mesh", R_MESH },
};

/* The route for (method, path): its id, 0 unknown path, -1 known path with another method. */
static int route_of(const char *method, const char *path) {
    size_t i;
    int known = 0;
    for (i = 0; i < sizeof ROUTES / sizeof ROUTES[0]; i++) {
        if (strcmp(path, ROUTES[i].path) != 0) continue;
        known = 1;
        if (!strcmp(method, ROUTES[i].method)) return ROUTES[i].id;
    }
    return known ? -1 : 0;
}

static int err_body(char *out, size_t cap, const char *token) {
    int k = snprintf(out, cap, "{\"error\":\"%s\"}", token);
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

/* The readiness summary through ops; -1 when it could not be read. */
static int rp_answer(const app_api_ops_t *ops, char *out, size_t cap) {
    char *raw;
    int n, k;
    if (!ops->rp_readiness) return -1;
    raw = malloc(2048);
    if (!raw) return -1;
    n = ops->rp_readiness(ops->ctx, raw, 2048);
    k = n > 0 && (size_t)n < 2048 ? app_api_rp_summary(raw, (size_t)n, out, cap) : -1;
    free(raw);
    return k;
}

/* {"helper":{"version"},"rp":..,"mesh":..,"forward":..}: a part that cannot be read is null. */
static int status_answer(const app_api_ops_t *ops, char *out, size_t cap) {
    char *part = malloc(APP_API_OUT_MAX), ver[96];
    size_t at = 0;
    int k;
    if (!part) return -1;
    webui_json_escape(ver, sizeof ver, ops->version ? ops->version : "", strlen(ops->version ? ops->version : ""));
    k = snprintf(out, cap, "{\"helper\":{\"version\":\"%s\"},\"rp\":", ver);
    if (k < 0 || (size_t)k >= cap) goto fail;
    at = (size_t)k;
#define APPEND_PART(call, key)                                                         \
    do {                                                                               \
        int pn = (call);                                                               \
        const char *src = pn > 0 && is_object(part, (size_t)pn) ? part : "null";       \
        size_t sl = pn > 0 && is_object(part, (size_t)pn) ? (size_t)pn : 4;            \
        if (at + sl + strlen(key) + 2 >= cap) goto fail;                               \
        memcpy(out + at, src, sl);                                                     \
        at += sl;                                                                      \
        memcpy(out + at, key, strlen(key));                                            \
        at += strlen(key);                                                             \
    } while (0)
    APPEND_PART(rp_answer(ops, part, APP_API_OUT_MAX), ",\"mesh\":");
    APPEND_PART(ops->mesh ? ops->mesh(ops->ctx, part, APP_API_OUT_MAX) : -1, ",\"forward\":");
    APPEND_PART(ops->forward_status ? ops->forward_status(ops->ctx, part, APP_API_OUT_MAX) : -1, "}");
#undef APPEND_PART
    out[at] = 0;
    free(part);
    return (int)at;
fail:
    free(part);
    return -1;
}

static size_t respond(char *out, size_t cap, int status, const char *body, size_t blen) {
    int h = webui_http_head(out, cap, status, "application/json", blen, NULL);
    if (h < 0 || (size_t)h + blen > cap) {
        static const char k500[] = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        if (cap < sizeof k500) return 0;
        memcpy(out, k500, sizeof k500 - 1);
        return sizeof k500 - 1;
    }
    memcpy(out + h, body, blen);
    return (size_t)h + blen;
}

static size_t respond_err(char *out, size_t cap, int status, const char *token) {
    char b[96];
    int k = err_body(b, sizeof b, token);
    return respond(out, cap, status, b, k > 0 ? (size_t)k : 0);
}

size_t app_api_handle(const char *req, size_t len, unsigned port, const app_api_ops_t *ops, char *out, size_t cap) {
    webui_req_t r;
    const char *body;
    size_t blen, resp;
    char *b;
    int id, http = 200, n = -1, rc;
    rc = webui_http_parse_cap(req, len, &r, APP_API_BODY_MAX);
    if (rc == WEBUI_PARSE_TOO_BIG || len > APP_API_REQ_MAX) return respond_err(out, cap, 413, "too_large");
    if (rc != WEBUI_PARSE_OK || r.header_len + r.content_length > len) return respond_err(out, cap, 400, "bad_request");
    if (!app_api_host_ok(r.host, port)) return respond_err(out, cap, 403, "bad_host");
    if (app_api_has_header(req, r.header_len, "origin")) return respond_err(out, cap, 403, "origin_refused");
    id = route_of(r.method, r.path);
    if (id == 0) return respond_err(out, cap, 404, "not_found");
    if (id < 0) return respond_err(out, cap, 405, "method_not_allowed");
    body = req + r.header_len;
    blen = (size_t)r.content_length;
    b = malloc(APP_API_OUT_MAX);
    if (!b) return respond_err(out, cap, 500, "out_of_memory");
    switch (id) {
    case R_VERSION: {
        char ver[96];
        webui_json_escape(ver, sizeof ver, ops->version ? ops->version : "", strlen(ops->version ? ops->version : ""));
        n = snprintf(b, APP_API_OUT_MAX, "{\"helper\":\"%s\",\"api\":%d}", ver, APP_API_VERSION);
        break;
    }
    case R_STATUS: n = status_answer(ops, b, APP_API_OUT_MAX); break;
    case R_RP:
        n = rp_answer(ops, b, APP_API_OUT_MAX);
        if (n < 0) {
            http = 503;
            n = err_body(b, APP_API_OUT_MAX, "rp_unreadable");
        }
        break;
    case R_RP_ENABLE: {
        int confirm = 0;
        if (blen == 0 || webui_json_bool(body, blen, "confirm", &confirm) != 1 || !confirm) {
            http = 400;
            n = err_body(b, APP_API_OUT_MAX, "confirm_required");
        } else if (ops->rp_enable) {
            n = ops->rp_enable(ops->ctx, b, APP_API_OUT_MAX, &http);
        }
        break;
    }
    case R_RP_PIN_REQ:
        if (ops->rp_pin_request) n = ops->rp_pin_request(ops->ctx, b, APP_API_OUT_MAX, &http);
        break;
    case R_RP_PIN_GET:
        if (ops->rp_pin_status) n = ops->rp_pin_status(ops->ctx, b, APP_API_OUT_MAX);
        break;
    case R_RP_PIN_CANCEL:
        if (ops->rp_pin_cancel) n = ops->rp_pin_cancel(ops->ctx, b, APP_API_OUT_MAX);
        break;
    case R_MESH:
        if (ops->mesh) n = ops->mesh(ops->ctx, b, APP_API_OUT_MAX);
        break;
    default: break;
    }
    if (n < 0 || (size_t)n >= APP_API_OUT_MAX || !is_object(b, (size_t)n)) {
        free(b);
        return respond_err(out, cap, 500, "internal");
    }
    resp = respond(out, cap, http, b, (size_t)n);
    free(b);
    return resp;
}
