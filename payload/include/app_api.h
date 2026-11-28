/* The helper's loopback app API for PS5Upload Play (FAKE99261), on 127.0.0.1:PLAY_APP_API_PORT.
 *
 * Always on while the helper runs, whatever the Minion switch says (Minion's :9180 server goes away
 * when Minion is off; this one does not), and bound to 127.0.0.1 only, even when Minion's LAN
 * access is on. Loopback is the boundary, so there is no token; on top of it:
 *   - the Host header must be 127.0.0.1:<port> or localhost:<port> (a page in the PS5's browser
 *     that rebinds a DNS name to 127.0.0.1 sends its own name: refused, 403 bad_host);
 *   - a request with an Origin header is refused (403 origin_refused): browsers send one on every
 *     cross-origin fetch and every POST, the app never does;
 *   - a request is at most APP_API_REQ_MAX bytes (413 too_large) and arrives within a short deadline.
 *
 * JSON in and out, versioned paths:
 *   GET  /v1/version          {"helper":"<version>","api":1}
 *   GET  /v1/status           {"helper":{"version"},"rp":<GET /v1/rp or null>,"mesh":<GET /v1/mesh or null>,
 *                              "forward":<the forward's state, app_api_server.c>}
 *   GET  /v1/rp               Remote Play readiness (app_api_rp_summary)
 *   POST /v1/rp/enable        {"confirm":true} -> the readiness after turning Remote Play on (the
 *                             system service, then this user's permission where the firmware has
 *                             one); 400 confirm_required without the flag
 *   POST /v1/rp/pin           a new pairing PIN: {"state":"waiting","pin":"12345678",...}
 *   GET  /v1/rp/pin           the pairing state: {"state":"idle|waiting|paired|failed|timeout","pin",
 *                             "seconds_left","err",...}; this read checks with the console whether a
 *                             device registered (as the engine's rp.status does)
 *   POST /v1/rp/pin/cancel    {"ok":true}
 *   GET  /v1/mesh             the engines connected to this helper (mesh_engines_json)
 *   POST /v1/tailnet/forward  {"peer":"100.x.y.z","port":N[,"proto":"tcp"]} -> {"local_port":9182,
 *                             "peer","port"}: the app's connections to 127.0.0.1:9182 go to that
 *                             tailnet peer's port (tailnet_node_forward_open, the one exception to
 *                             "the PS5 never dials out on the tailnet"); 400 not_tailnet_address,
 *                             bad_port, tcp_only; 409 tailnet_off, unknown_peer, port_busy
 *   GET  /v1/tailnet/forward  {"state":"on","local_port","peer","port"} | {"state":"off"} |
 *                             {"state":"tailnet_off"}
 *   POST /v1/tailnet/forward/close  {"ok":true}
 * Errors are {"error":"<token>"} with a 4xx/5xx status. Wrong method: 405 method_not_allowed;
 * unknown path: 404 not_found.
 *
 * This file is the pure part (request checks, routing, answers), host-tested with fake operations
 * (payload/tests/app_api_selftest.c); app_api_server.c is the listener and the real operations. */
#ifndef PS5UPLOAD_APP_API_H
#define PS5UPLOAD_APP_API_H

#include <stddef.h>
#include <stdint.h>

#define APP_API_VERSION 1
#define APP_API_REQ_MAX 4096u   /* request line + headers + body */
#define APP_API_BODY_MAX 1024u  /* a request body */
#define APP_API_OUT_MAX 16384u  /* an answer's body */
#define APP_API_RESP_MAX (APP_API_OUT_MAX + 512u)

/* What the routes do. Each writes a JSON object into out and returns its length, or -1 (answered
 * 500 internal). *http is 200 on entry; an operation sets another status for an error body. */
typedef struct {
    void *ctx;
    const char *version;    /* the helper's version */
    /* remoteplay_readiness_json's raw JSON */
    int (*rp_readiness)(void *ctx, char *out, size_t cap);
    /* Turn Remote Play on; the readiness after (app_api_rp_summary) or an error body. */
    int (*rp_enable)(void *ctx, char *out, size_t cap, int *http);
    int (*rp_pin_request)(void *ctx, char *out, size_t cap, int *http);
    int (*rp_pin_status)(void *ctx, char *out, size_t cap);
    int (*rp_pin_cancel)(void *ctx, char *out, size_t cap);
    int (*mesh)(void *ctx, char *out, size_t cap);
    /* The forward's state for /v1/status and GET /v1/tailnet/forward (NULL: "forward":null). */
    int (*forward_status)(void *ctx, char *out, size_t cap);
    /* Open the forward to (peer, a tailnet IPv4 in host order, already checked to be in
     * 100.64.0.0/10; port 1..65535). */
    int (*forward_open)(void *ctx, uint32_t peer, unsigned port, char *out, size_t cap, int *http);
    int (*forward_close)(void *ctx, char *out, size_t cap);
} app_api_ops_t;

/* "100.64.0.1" -> host order, 0 when not a dotted IPv4 in 100.64.0.0/10 (Tailscale's range). */
uint32_t app_api_tailnet_ip(const char *s);

/* 1 when host is "127.0.0.1:<port>" or "localhost:<port>" (any letter case), else 0. */
int app_api_host_ok(const char *host, unsigned port);

/* 1 when the header block (buf, up to header_len) has a header named `name` (lowercase), else 0. */
int app_api_has_header(const char *buf, size_t header_len, const char *name);

/* Has buf (len bytes received so far) a whole request? 1 yes (*total = its length with the body),
 * 0 not yet, -1 malformed, -2 over APP_API_REQ_MAX or a body over APP_API_BODY_MAX. */
int app_api_request_ready(const char *buf, size_t len, size_t *total);

/* Remote Play readiness for the app, from remoteplay_readiness_json's raw JSON:
 *   {"ready":bool,"service_on":bool,"user_on":bool,"needs_user_permission":bool,"signed_in":bool,
 *    "psn_activated":bool,"readiness":<raw>}
 * ready = service on, the user permitted where the firmware asks for it, and an activated PSN
 * account. -1 when raw is not readiness JSON or out is too small. */
int app_api_rp_summary(const char *raw, size_t n, char *out, size_t cap);
/* The three flags the enable route needs; 0, or -1 when raw does not say. */
int app_api_rp_flags(const char *raw, size_t n, int *service_on, int *user_on, int *needs_per_user);

/* One whole request (app_api_request_ready said 1) -> a complete HTTP response in out (at least
 * APP_API_RESP_MAX bytes). port is the port the API listens on (the Host check). Returns the
 * response's length. */
size_t app_api_handle(const char *req, size_t len, unsigned port, const app_api_ops_t *ops, char *out, size_t cap);

/* The listener (app_api_server.c): started once at helper start, stopped at exit. */
int app_api_start(void);
void app_api_stop(void);

#endif
