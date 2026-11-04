/* Bounded HTTP server for the console web UI, running inside the helper.
 *
 * One accept thread and WEBUI_WORKERS workers. Accepted sockets go through a
 * bounded queue (depth 32; a connection that does not fit is closed at once).
 * Every connection carries a deadline of accept time + 5 s: reads and writes
 * stop there, and a worker that dequeues an already-expired connection closes
 * it without reading. A connection that has sent nothing waits on the accept
 * thread and holds no worker; a slow or hostile client that has holds one for at
 * most 5 s and can never stall the accept thread. One address gets 16 connections
 * at most. */
#ifndef PS5UPLOAD_WEBUI_SERVER_H
#define PS5UPLOAD_WEBUI_SERVER_H

#include <netinet/in.h>
#include <stddef.h>
#include <stdint.h>
#include "webui_http.h"

/* Writes a complete response to fd (use webui_send / webui_send_json).
 * Runs on a worker after the Host/Origin/token checks passed. Unknown paths
 * answer 404 and wrong methods 405. The return value is ignored. */
typedef int (*webui_route_fn)(const webui_req_t *r, const uint8_t *body, size_t body_len, int fd);

/* A streamed request (an upload, a text save over 64 KiB): its body may be up to
 * WEBUI_STREAM_BODY_MAX and is NOT read by the server. After the Host/Origin/token checks passed,
 * the handler gets the body bytes that arrived with the headers (`pre`, `pre_len`) and reads the
 * remaining content_length - pre_len itself, with its own idle timeout (the 5 s connection deadline
 * covers the request line and headers only). It must write a complete response. */
typedef int (*webui_stream_fn)(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd);

#define WEBUI_STREAM_BODY_MAX (1ull << 42) /* 4 TiB: larger than any file a console drive holds */

typedef struct {
    uint16_t port;
    int lan;              /* 0: bind 127.0.0.1; 1: bind 0.0.0.0 */
    webui_route_fn route;
    /* Optional (NULL: nothing is streamed). 1 = this request (by method + path) is a streamed one. */
    int (*stream_for)(const webui_req_t *r);
    webui_stream_fn stream;
} webui_cfg_t;

/* New token, bind, start the threads. Returns 0, or -1 when already running or
 * the threads cannot start. A bind failure is NOT an error: it is logged and
 * retried every 5 s by the accept thread (a helper takeover means the old
 * instance may still hold the port for a moment). */
int webui_start(const webui_cfg_t *cfg);

/* 1 while the server is started (webui_start .. webui_stop), else 0. */
int webui_running(void);

/* Close the listener and the queued connections, wake the workers and join
 * them within 2 s. A worker still inside route() after that is detached and
 * finishes on its own; it keeps the server state it needs alive. */
void webui_stop(void);

/* Rebind the listener for a LAN setting change. Safe to call from a route
 * handler (it never waits on the workers). The token is kept. Returns 0 when
 * the new listener is bound, -1 when it is not (yet): the accept thread keeps
 * retrying every 5 s. */
int webui_restart(int lan);

/* The listener heals itself (the accept thread, no other thread): every 5 s it checks for a resume
 * from rest mode (a gap of 10 s or more on either clock) and, with LAN access on, for a change in
 * the console's IPv4 addresses (DHCP, another network: the Host allowlist is refreshed); either one
 * rebinds the listener (the token is kept). A listener whose poll or accept keeps failing for 5 s
 * is rebound too. */

/* The console's LAN address a phone should open (a private one first, never loopback), from the
 * running server's list; 1 and `out` set when LAN access is on and there is one, else 0 and "". */
int webui_lan_addr(char out[INET_ADDRSTRLEN]);

/* The watch's decisions, pure (tests): */
/* Did the console sleep between two checks? (monotonic and wall-clock ms at each) */
int webui_watch_resumed(uint64_t mono_prev, uint64_t mono_now, int64_t real_prev, int64_t real_now);
/* Has a listener failing since `err_since` (0 = not failing) failed long enough to rebind? */
int webui_watch_broken(uint64_t err_since, uint64_t now);
/* Do two address lists differ (as sets)? */
int webui_addrs_differ(const char (*a)[INET_ADDRSTRLEN], size_t na, const char (*b)[INET_ADDRSTRLEN], size_t nb);
/* The address to show for LAN access: private ranges first, then others, link-local last; never
 * 127.x or 0.x. 1 and `out` set, else 0 and "". */
int webui_pick_lan_addr(const char (*addrs)[INET_ADDRSTRLEN], size_t n, char out[INET_ADDRSTRLEN]);

/* The token for this start ("" when the server is not running). */
const char *webui_token(void);
/* The token copied under the server's lock (webui_token() reads it unsynchronized): use this one
 * from any thread that may race a start, stop or restart. */
void webui_token_copy(char out[65]);

/* Write a complete response (head + body) within the connection deadline. */
void webui_send(int fd, int status, const char *ctype, const void *body, size_t len);
void webui_send_json(int fd, int status, const char *json);
/* webui_send with extra header lines (each a complete "Name: value\r\n"), e.g. a CSP. */
void webui_send_ex(int fd, int status, const char *ctype, const void *body, size_t len, const char *extra_headers);

#endif
