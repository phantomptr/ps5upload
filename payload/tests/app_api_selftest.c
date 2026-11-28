/* Host-side tests for PS5Upload Play's loopback app API (app_api.c): request framing and limits, the
 * Host and Origin checks (DNS rebinding from the PS5's browser), routing and methods, the Remote
 * Play readiness summary, and each route's answer through fake operations. Build: see the root
 * Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "app_api.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

#define PORT 9181u

static const char *READY_RAW =
    "{\"fw_magic\":1,\"has_per_user\":1,\"foreground_uid\":-1,\"account_uid\":268435456,\"account_via\":\"x\","
    "\"user_slot\":1,\"account_id_b64\":\"AAAA\",\"account_id_raw\":1234567,\"account_type\":\"np\","
    "\"service_enabled\":1,\"user_enabled\":1,\"symbols_ok\":1,\"registry_err\":0,\"fg_rc\":0,\"login_rc\":0,"
    "\"login_count\":1,\"signed_in_slot\":1}";

static const char *g_raw;
static int g_enable_calls, g_pin_calls, g_cancel_calls, g_fail_mesh;

static int f_readiness(void *ctx, char *out, size_t cap) {
    (void)ctx;
    if (!g_raw) return -1;
    snprintf(out, cap, "%s", g_raw);
    return (int)strlen(out);
}
static int f_enable(void *ctx, char *out, size_t cap, int *http) {
    (void)ctx;
    (void)http;
    g_enable_calls++;
    return app_api_rp_summary(READY_RAW, strlen(READY_RAW), out, cap);
}
static int f_pin(void *ctx, char *out, size_t cap, int *http) {
    (void)ctx;
    g_pin_calls++;
    if (g_pin_calls == 2) {
        *http = 409;
        return snprintf(out, cap, "{\"error\":\"pin_failed\",\"reason\":\"x\"}");
    }
    return snprintf(out, cap, "{\"state\":\"waiting\",\"pin\":\"12345678\"}");
}
static int f_pin_status(void *ctx, char *out, size_t cap) {
    (void)ctx;
    return snprintf(out, cap, "{\"state\":\"idle\",\"pin\":\"\"}");
}
static int f_cancel(void *ctx, char *out, size_t cap) {
    (void)ctx;
    g_cancel_calls++;
    return snprintf(out, cap, "{\"ok\":true}");
}
static int f_mesh(void *ctx, char *out, size_t cap) {
    (void)ctx;
    if (g_fail_mesh) return -1;
    return snprintf(out, cap, "{\"engine\":null,\"online\":[]}");
}
static int f_fwd(void *ctx, char *out, size_t cap) {
    (void)ctx;
    return snprintf(out, cap, "{\"state\":\"off\"}");
}

static uint32_t g_fwd_peer;
static unsigned g_fwd_port;
static int g_fwd_closes;
static int f_fwd_open(void *ctx, uint32_t peer, unsigned port, char *out, size_t cap, int *http) {
    (void)ctx;
    if (peer == 0x64400063u) { /* 100.64.0.99: not a peer the node knows */
        *http = 409;
        return snprintf(out, cap, "{\"error\":\"unknown_peer\"}");
    }
    g_fwd_peer = peer;
    g_fwd_port = port;
    return snprintf(out, cap, "{\"local_port\":9182}");
}
static int f_fwd_close(void *ctx, char *out, size_t cap) {
    (void)ctx;
    g_fwd_closes++;
    return snprintf(out, cap, "{\"ok\":true}");
}

static const app_api_ops_t OPS = { NULL,    "13.0.0", f_readiness, f_enable,   f_pin,       f_pin_status,
                                   f_cancel, f_mesh,  f_fwd,       f_fwd_open, f_fwd_close };

static void test_tailnet_ip(void) {
    CHECK(app_api_tailnet_ip("100.64.0.1") == 0x64400001u);
    CHECK(app_api_tailnet_ip("100.127.255.254") == 0x647ffffeu);
    CHECK(app_api_tailnet_ip("100.128.0.1") == 0); /* outside 100.64.0.0/10 */
    CHECK(app_api_tailnet_ip("100.63.0.1") == 0);
    CHECK(app_api_tailnet_ip("192.168.1.2") == 0);
    CHECK(app_api_tailnet_ip("127.0.0.1") == 0);
    CHECK(app_api_tailnet_ip("100.64.0") == 0);
    CHECK(app_api_tailnet_ip("100.64.0.1.2") == 0);
    CHECK(app_api_tailnet_ip("100.64.0.256") == 0);
    CHECK(app_api_tailnet_ip("100.64.0.1 ") == 0);
    CHECK(app_api_tailnet_ip("") == 0);
    CHECK(app_api_tailnet_ip(NULL) == 0);
}

static char g_resp[APP_API_RESP_MAX];

/* The response to `req` (sent whole); *status set from its first line. */
static const char *ask(const char *req, int *status) {
    size_t n = app_api_handle(req, strlen(req), PORT, &OPS, g_resp, sizeof g_resp);
    g_resp[n < sizeof g_resp ? n : sizeof g_resp - 1] = 0;
    *status = n > 12 ? atoi(g_resp + 9) : 0;
    {
        const char *b = strstr(g_resp, "\r\n\r\n");
        return b ? b + 4 : "";
    }
}

static const char *get(const char *path, int *status) {
    static char req[512];
    snprintf(req, sizeof req, "GET %s HTTP/1.1\r\nHost: 127.0.0.1:9181\r\n\r\n", path);
    return ask(req, status);
}

static const char *post(const char *path, const char *body, int *status) {
    static char req[2048];
    snprintf(req, sizeof req, "POST %s HTTP/1.1\r\nHost: localhost:9181\r\nContent-Length: %u\r\n\r\n%s", path,
             (unsigned)strlen(body), body);
    return ask(req, status);
}

static int has(const char *hay, const char *needle) { return strstr(hay, needle) != NULL; }

static void test_host(void) {
    CHECK(app_api_host_ok("127.0.0.1:9181", PORT));
    CHECK(app_api_host_ok("localhost:9181", PORT));
    CHECK(app_api_host_ok("LocalHost:9181", PORT));
    CHECK(!app_api_host_ok("127.0.0.1", PORT));          /* no port: not the app */
    CHECK(!app_api_host_ok("127.0.0.1:9180", PORT));
    CHECK(!app_api_host_ok("127.0.0.1:91811", PORT));
    CHECK(!app_api_host_ok("evil.example:9181", PORT)); /* a rebound DNS name */
    CHECK(!app_api_host_ok("127.0.0.1.nip.io:9181", PORT));
    CHECK(!app_api_host_ok("localhost.:9181", PORT));
    CHECK(!app_api_host_ok("192.168.1.5:9181", PORT));
    CHECK(!app_api_host_ok("", PORT));
    CHECK(!app_api_host_ok(NULL, PORT));
}

static void test_framing(void) {
    size_t total = 0;
    char big[APP_API_REQ_MAX + 64];
    const char *r = "POST /v1/rp/enable HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nContent-Length: 15\r\n\r\n{\"confirm\":true}";
    CHECK(app_api_request_ready("GET /v1/rp HTTP/1.1\r\n", 21, &total) == 0);
    CHECK(app_api_request_ready(r, strlen(r) - 3, &total) == 0); /* body not all here */
    CHECK(app_api_request_ready(r, strlen(r) - 1, &total) == 1 && total == strlen(r) - 1);
    CHECK(app_api_request_ready("BAD\r\n\r\n", 7, &total) == -1);
    /* A body over the cap, or a request over the cap, is refused before it is read. */
    snprintf(big, sizeof big, "POST /v1/rp/pin HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nContent-Length: %u\r\n\r\n",
             (unsigned)APP_API_BODY_MAX + 1);
    CHECK(app_api_request_ready(big, strlen(big), &total) == -2);
    memset(big, 'a', sizeof big);
    memcpy(big, "GET /", 5);
    CHECK(app_api_request_ready(big, APP_API_REQ_MAX, &total) == -2);
    CHECK(app_api_request_ready(big, APP_API_REQ_MAX - 1, &total) == 0);
}

static void test_checks(void) {
    int st;
    const char *b;
    b = ask("GET /v1/version HTTP/1.1\r\nHost: evil.example:9181\r\n\r\n", &st);
    CHECK(st == 403 && has(b, "bad_host"));
    b = ask("GET /v1/version HTTP/1.1\r\n\r\n", &st);
    CHECK(st == 403 && has(b, "bad_host"));
    b = ask("GET /v1/version HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nOrigin: http://127.0.0.1:9181\r\n\r\n", &st);
    CHECK(st == 403 && has(b, "origin_refused"));
    b = ask("GET /v1/version HTTP/1.1\r\nHost: 127.0.0.1:9181\r\norigin: null\r\n\r\n", &st);
    CHECK(st == 403 && has(b, "origin_refused"));
    b = ask("GET /v1/version HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nOrigin:\r\n\r\n", &st);
    CHECK(st == 403 && has(b, "origin_refused")); /* even empty */
    b = ask("GET /v1/version HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nX-Origin: a\r\n\r\n", &st);
    CHECK(st == 200);
    b = ask("GET /v1/version HTTP/1.1\r\nHost: 127.0.0.1:9181\r\nTransfer-Encoding: chunked\r\n\r\n", &st);
    CHECK(st == 400);
    b = get("/v1/nope", &st);
    CHECK(st == 404 && has(b, "not_found"));
    b = get("/v1/rp/enable", &st);
    CHECK(st == 405 && has(b, "method_not_allowed"));
    b = post("/v1/status", "{}", &st);
    CHECK(st == 405);
    b = get("/v1/version?x=1", &st); /* a query string is ignored */
    CHECK(st == 200);
    /* No CORS headers in any answer: a page cannot read one. */
    get("/v1/version", &st);
    CHECK(!has(g_resp, "Access-Control"));
    CHECK(has(g_resp, "Content-Type: application/json\r\n"));
}

static void test_rp_summary(void) {
    char out[2048];
    const char *off =
        "{\"has_per_user\":1,\"foreground_uid\":-1,\"account_uid\":0,\"account_id_raw\":0,\"account_type\":\"\","
        "\"service_enabled\":0,\"user_enabled\":0,\"login_count\":0}";
    const char *signed_only = "{\"has_per_user\":0,\"foreground_uid\":5,\"account_uid\":0,\"account_id_raw\":0,"
                              "\"account_type\":\"\",\"service_enabled\":1,\"user_enabled\":0,\"login_count\":-1}";
    int svc, usr, per;
    CHECK(app_api_rp_summary(READY_RAW, strlen(READY_RAW), out, sizeof out) > 0);
    CHECK(has(out, "{\"ready\":true,\"service_on\":true,\"user_on\":true,\"needs_user_permission\":true,"
                   "\"signed_in\":true,\"psn_activated\":true,\"readiness\":{\"fw_magic\":1"));
    CHECK(out[strlen(out) - 1] == '}' && out[strlen(out) - 2] == '}');
    CHECK(app_api_rp_summary(off, strlen(off), out, sizeof out) > 0);
    CHECK(has(out, "{\"ready\":false,\"service_on\":false,\"user_on\":false,\"needs_user_permission\":true,"
                   "\"signed_in\":false,\"psn_activated\":false"));
    CHECK(app_api_rp_summary(signed_only, strlen(signed_only), out, sizeof out) > 0);
    CHECK(has(out, "\"ready\":false,\"service_on\":true") && has(out, "\"needs_user_permission\":false,\"signed_in\":true"));
    CHECK(app_api_rp_summary("{}", 2, out, sizeof out) == -1);
    CHECK(app_api_rp_summary("nope", 4, out, sizeof out) == -1);
    CHECK(app_api_rp_summary(READY_RAW, strlen(READY_RAW), out, 64) == -1);
    CHECK(app_api_rp_flags(off, strlen(off), &svc, &usr, &per) == 0 && !svc && !usr && per);
}

static void test_routes(void) {
    int st;
    const char *b;
    g_raw = READY_RAW;
    b = get("/v1/version", &st);
    CHECK(st == 200 && !strcmp(b, "{\"helper\":\"13.0.0\",\"api\":1}"));
    b = get("/v1/rp", &st);
    CHECK(st == 200 && has(b, "\"ready\":true"));
    b = get("/v1/status", &st);
    CHECK(st == 200 && has(b, "{\"helper\":{\"version\":\"13.0.0\"},\"rp\":{\"ready\":true"));
    CHECK(has(b, "},\"mesh\":{\"engine\":null,\"online\":[]},\"forward\":{\"state\":\"off\"}}"));
    /* A part that cannot be read is null; the rest still answers. */
    g_raw = NULL;
    g_fail_mesh = 1;
    b = get("/v1/status", &st);
    CHECK(st == 200 && !strcmp(b, "{\"helper\":{\"version\":\"13.0.0\"},\"rp\":null,\"mesh\":null,\"forward\":{\"state\":\"off\"}}"));
    b = get("/v1/rp", &st);
    CHECK(st == 503 && has(b, "rp_unreadable"));
    b = get("/v1/mesh", &st);
    CHECK(st == 500 && has(b, "internal"));
    g_fail_mesh = 0;
    g_raw = READY_RAW;
    /* enable needs the confirm flag */
    b = post("/v1/rp/enable", "", &st);
    CHECK(st == 400 && has(b, "confirm_required") && g_enable_calls == 0);
    b = post("/v1/rp/enable", "{\"confirm\":false}", &st);
    CHECK(st == 400 && has(b, "confirm_required") && g_enable_calls == 0);
    b = post("/v1/rp/enable", "{\"confirm\":\"yes\"}", &st);
    CHECK(st == 400 && g_enable_calls == 0);
    b = post("/v1/rp/enable", "{\"confirm\":true}", &st);
    CHECK(st == 200 && has(b, "\"service_on\":true") && g_enable_calls == 1);
    b = post("/v1/rp/pin", "", &st);
    CHECK(st == 200 && has(b, "\"pin\":\"12345678\""));
    b = post("/v1/rp/pin", "", &st);
    CHECK(st == 409 && has(b, "pin_failed"));
    b = get("/v1/rp/pin", &st);
    CHECK(st == 200 && has(b, "\"state\":\"idle\""));
    b = post("/v1/rp/pin/cancel", "", &st);
    CHECK(st == 200 && has(b, "\"ok\":true") && g_cancel_calls == 1);
    b = get("/v1/mesh", &st);
    CHECK(st == 200 && has(b, "\"engine\":null"));
}

static void test_forward(void) {
    int st;
    const char *b;
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":19113}", &st);
    CHECK(st == 200 && has(b, "\"local_port\":9182") && g_fwd_peer == 0x64650203u && g_fwd_port == 19113);
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":19113,\"proto\":\"tcp\"}", &st);
    CHECK(st == 200);
    g_fwd_peer = 0;
    /* Never anything but a tailnet address, a real port, TCP. */
    b = post("/v1/tailnet/forward", "{\"peer\":\"192.168.1.10\",\"port\":19113}", &st);
    CHECK(st == 400 && has(b, "not_tailnet_address") && g_fwd_peer == 0);
    b = post("/v1/tailnet/forward", "{\"peer\":\"127.0.0.1\",\"port\":9120}", &st);
    CHECK(st == 400 && has(b, "not_tailnet_address") && g_fwd_peer == 0);
    b = post("/v1/tailnet/forward", "{\"port\":19113}", &st);
    CHECK(st == 400 && has(b, "not_tailnet_address"));
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":0}", &st);
    CHECK(st == 400 && has(b, "bad_port"));
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":65536}", &st);
    CHECK(st == 400 && has(b, "bad_port"));
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":\"80\"}", &st);
    CHECK(st == 400 && has(b, "bad_port"));
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.101.2.3\",\"port\":9296,\"proto\":\"udp\"}", &st);
    CHECK(st == 400 && has(b, "tcp_only") && g_fwd_peer == 0);
    b = post("/v1/tailnet/forward", "", &st);
    CHECK(st == 400);
    /* The node's own answer comes through (a peer it does not know). */
    b = post("/v1/tailnet/forward", "{\"peer\":\"100.64.0.99\",\"port\":19113}", &st);
    CHECK(st == 409 && has(b, "unknown_peer"));
    b = get("/v1/tailnet/forward", &st);
    CHECK(st == 200 && !strcmp(b, "{\"state\":\"off\"}"));
    b = post("/v1/tailnet/forward/close", "", &st);
    CHECK(st == 200 && has(b, "\"ok\":true") && g_fwd_closes == 1);
    b = get("/v1/tailnet/forward/close", &st);
    CHECK(st == 405);
}

int main(void) {
    test_host();
    test_framing();
    test_checks();
    test_rp_summary();
    test_routes();
    test_tailnet_ip();
    test_forward();
    if (failures) {
        fprintf(stderr, "%d app API check(s) failed\n", failures);
        return 1;
    }
    printf("app api selftest ok\n");
    return 0;
}
