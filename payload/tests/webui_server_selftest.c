/* Host-side tests for the console web UI server: real sockets on an
 * ephemeral loopback port and a stub router answering {"ok":true}.
 * Covers the properties that keep the helper alive: silent, half-sent and
 * flooding clients never stall other requests, and a stop/start rebinds the
 * port with a new token. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "webui_server.h"

/* The server's connection deadline and listener-watch period. The Makefile builds this test and
 * webui_server.c with shorter ones (-DWEBUI_CONN_MS / -DWEBUI_WATCH_MS) so the timeout paths run in
 * a fraction of the time; every wait below is measured from them. The defaults are the helper's. */
#ifndef WEBUI_CONN_MS
#define WEBUI_CONN_MS 5000u
#endif
#ifndef WEBUI_WATCH_MS
#define WEBUI_WATCH_MS 5000u
#endif
#define CONN_MS ((int)WEBUI_CONN_MS)
#define CONN_S (CONN_MS / 1000.0)

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

static uint16_t g_port;

static int stub_route(const webui_req_t *r, const uint8_t *body, size_t body_len, int fd) {
    (void)body; (void)body_len;
    if (strcmp(r->path, "/api/slow") == 0) {
        /* A long download to a slow reader: holds its worker until the client goes (10 s at most). */
        char b[256];
        struct pollfd p = { fd, POLLIN, 0 };
        while (poll(&p, 1, 10000) > 0 && recv(fd, b, sizeof b, 0) > 0) {}
    }
    webui_send_json(fd, 200, "{\"ok\":true}");
    return 0;
}

/* Requests that may hold a worker for long besides the streamed ones (the helper's downloads). */
static int long_for(const webui_req_t *r) { return strcmp(r->path, "/api/slow") == 0; }

/* Streamed route: reads the rest of the body itself (with its own timeout), answers {"n":<bytes>}. */
static int stream_for(const webui_req_t *r) { return strcmp(r->path, "/api/stream") == 0; }

static int stream_route(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd) {
    static char buf[65536];
    char json[64];
    uint64_t got = pre_len;
    (void)pre;
    while (got < r->content_length) {
        struct pollfd p = { fd, POLLIN, 0 };
        ssize_t n;
        if (poll(&p, 1, 10000) <= 0) break;
        n = recv(fd, buf, sizeof buf, 0);
        if (n <= 0) break;
        got += (uint64_t)n;
    }
    snprintf(json, sizeof json, "{\"n\":%llu}", (unsigned long long)got);
    webui_send_json(fd, 200, json);
    return 0;
}

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void sleep_ms(int ms) {
    struct timespec ts = { ms / 1000, (long)(ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

/* An ephemeral port nobody holds right now. */
static uint16_t free_port(void) {
    struct sockaddr_in a;
    socklen_t al = sizeof a;
    int s = socket(AF_INET, SOCK_STREAM, 0);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (s < 0 || bind(s, (struct sockaddr *)&a, sizeof a) != 0 ||
        getsockname(s, (struct sockaddr *)&a, &al) != 0) {
        if (s >= 0) close(s);
        return 0;
    }
    close(s);
    return ntohs(a.sin_port);
}

static int dial(void) {
    struct sockaddr_in a;
    int s = socket(AF_INET, SOCK_STREAM, 0);
    if (s < 0) return -1;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(g_port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(s, (struct sockaddr *)&a, sizeof a) != 0) {
        close(s);
        return -1;
    }
    return s;
}

/* Read until EOF or timeout. Returns bytes read, -1 on timeout. */
static int read_all(int s, char *out, size_t cap, int timeout_ms) {
    size_t got = 0;
    double end = now_s() + timeout_ms / 1000.0;
    for (;;) {
        struct pollfd p = { s, POLLIN, 0 };
        int left = (int)((end - now_s()) * 1000.0);
        ssize_t n;
        if (left <= 0) return -1;
        if (poll(&p, 1, left) <= 0) continue;
        n = recv(s, out + got, cap - 1 - got, 0);
        if (n <= 0) break;
        got += (size_t)n;
        if (got >= cap - 1) break;
    }
    out[got] = '\0';
    return (int)got;
}

/* Send one request on a fresh connection; return the HTTP status or -1. */
static int request(const char *path, const char *token, char *resp, size_t cap) {
    char req[512];
    int s = dial(), n, status = -1;
    if (s < 0) return -1;
    n = snprintf(req, sizeof req,
                 "GET %s HTTP/1.1\r\nHost: 127.0.0.1:%u\r\n%s%s%s\r\n",
                 path, (unsigned)g_port, token ? "X-PS5U-Token: " : "",
                 token ? token : "", token ? "\r\n" : "");
    if (send(s, req, (size_t)n, 0) == n && read_all(s, resp, cap, 7000) > 0)
        (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    return status;
}

static int start_server(void) {
    webui_cfg_t c;
    memset(&c, 0, sizeof c);
    c.port = g_port;
    c.lan = 0;
    c.route = stub_route;
    c.stream_for = stream_for;
    c.stream = stream_route;
    c.long_for = long_for;
    return webui_start(&c);
}

static void serves_api_with_token(void) {
    char resp[2048];
    CHECK(request("/api/x", webui_token(), resp, sizeof resp) == 200);
    CHECK(strstr(resp, "{\"ok\":true}") != NULL);
    CHECK(strstr(resp, "Connection: close") != NULL);
}

static void api_without_token_401(void) {
    char resp[2048];
    CHECK(request("/api/x", NULL, resp, sizeof resp) == 401);
    CHECK(strstr(resp, "{\"error\":\"token\"}") != NULL);
    /* The page itself and the token endpoint need no token. */
    CHECK(request("/", NULL, resp, sizeof resp) == 200);
    CHECK(request("/api/token", NULL, resp, sizeof resp) == 200);
}

static void bad_host_403(void) {
    char req[256], resp[2048];
    int s = dial(), n, status = -1;
    CHECK(s >= 0);
    if (s < 0) return;
    n = snprintf(req, sizeof req, "GET /api/x HTTP/1.1\r\nHost: evil.example:%u\r\nX-PS5U-Token: %s\r\n\r\n",
                 (unsigned)g_port, webui_token());
    CHECK(send(s, req, (size_t)n, 0) == n);
    if (read_all(s, resp, sizeof resp, 7000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 403);
    CHECK(strstr(resp, "{\"error\":\"forbidden\"}") != NULL);
}

static void garbage_400(void) {
    char resp[2048];
    int s = dial(), status = -1;
    CHECK(s >= 0);
    if (s < 0) return;
    CHECK(send(s, "NOT HTTP AT ALL\r\n\r\n", 19, 0) == 19);
    if (read_all(s, resp, sizeof resp, 7000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 400);
}

static void too_big_413(void) {
    char req[256], resp[2048];
    int s = dial(), n, status = -1;
    CHECK(s >= 0);
    if (s < 0) return;
    n = snprintf(req, sizeof req,
                 "POST /api/x HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nContent-Length: 999999\r\n\r\n",
                 (unsigned)g_port);
    CHECK(send(s, req, (size_t)n, 0) == n);
    if (read_all(s, resp, sizeof resp, 7000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 413);
}

/* A streamed route takes a body far over the 64 KiB cap, and past the header deadline (5 s in the
 * helper) when the client sends slowly; without the token it is refused before any body byte is
 * read. A request that is not streamed still gets 413 for the same length (too_big_413). */
static void streamed_body(void) {
    static char chunk[65536];
    char req[512], resp[2048];
    const size_t total = 16u * sizeof chunk;
    size_t sent = 0;
    int s, n, status = -1;
    memset(chunk, 'x', sizeof chunk);
    s = dial();
    CHECK(s >= 0);
    if (s < 0) return;
    n = snprintf(req, sizeof req,
                 "POST /api/stream HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\nContent-Length: %zu\r\n\r\n",
                 (unsigned)g_port, webui_token(), total);
    CHECK(send(s, req, (size_t)n, 0) == n);
    while (sent < total / 2) {
        ssize_t w = send(s, chunk, sizeof chunk, 0);
        if (w <= 0) break;
        sent += (size_t)w;
    }
    sleep_ms(CONN_MS + 1000); /* past the connection's deadline */
    while (sent < total) {
        size_t k = total - sent < sizeof chunk ? total - sent : sizeof chunk;
        ssize_t w = send(s, chunk, k, 0);
        if (w <= 0) break;
        sent += (size_t)w;
    }
    if (read_all(s, resp, sizeof resp, 7000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 200);
    snprintf(req, sizeof req, "{\"n\":%zu}", total);
    CHECK(strstr(resp, req) != NULL);

    s = dial();
    CHECK(s >= 0);
    if (s < 0) return;
    n = snprintf(req, sizeof req, "POST /api/stream HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nContent-Length: %zu\r\n\r\n",
                 (unsigned)g_port, total);
    CHECK(send(s, req, (size_t)n, 0) == n);
    status = -1;
    if (read_all(s, resp, sizeof resp, 7000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 401);
}

/* Open a request that then holds its worker: a download to a reader that never reads (GET
 * /api/slow) or an upload that never sends its body (POST /api/stream). The socket, or -1. */
static int hold(int upload) {
    char req[512];
    int s = dial(), n;
    if (s < 0) return -1;
    if (upload)
        n = snprintf(req, sizeof req,
                     "POST /api/stream HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\n"
                     "Content-Length: 1000000\r\n\r\n",
                     (unsigned)g_port, webui_token());
    else
        n = snprintf(req, sizeof req, "GET /api/slow HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\n\r\n",
                     (unsigned)g_port, webui_token());
    if (send(s, req, (size_t)n, 0) != n) {
        close(s);
        return -1;
    }
    return s;
}

/* Transfers never take every worker: with three running (uploads and downloads alike, each holding
 * its worker for as long as its client keeps it open), a fourth is told 503 busy at once, and the
 * page's own requests are still answered at once. Before, four slow transfers held all four workers
 * and every other request waited in the queue until it expired. */
static void transfers_leave_a_worker(void) {
    int held[3], i, s, n, status;
    char req[512], resp[2048];
    double t0;
    held[0] = hold(1);
    held[1] = hold(0);
    held[2] = hold(0);
    for (i = 0; i < 3; i++) CHECK(held[i] >= 0);
    sleep_ms(500); /* the workers have them */

    for (i = 0; i < 2; i++) {
        t0 = now_s();
        s = hold(i == 0);
        CHECK(s >= 0);
        if (s < 0) continue;
        status = -1;
        if (read_all(s, resp, sizeof resp, 3000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
        close(s);
        CHECK(status == 503);
        CHECK(strstr(resp, "{\"error\":\"busy\"}") != NULL);
        CHECK(strstr(resp, "Retry-After: 2\r\n") != NULL);
        CHECK(now_s() - t0 < 1.0);
    }
    t0 = now_s();
    CHECK(request("/api/x", webui_token(), resp, sizeof resp) == 200);
    CHECK(now_s() - t0 < 1.0);

    /* Once they end, a transfer runs again. */
    for (i = 0; i < 3; i++)
        if (held[i] >= 0) close(held[i]);
    sleep_ms(500);
    s = dial();
    CHECK(s >= 0);
    if (s < 0) return;
    n = snprintf(req, sizeof req,
                 "POST /api/stream HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\nContent-Length: 3\r\n\r\nabc",
                 (unsigned)g_port, webui_token());
    CHECK(send(s, req, (size_t)n, 0) == n);
    status = -1;
    if (read_all(s, resp, sizeof resp, 3000) > 0) (void)sscanf(resp, "HTTP/1.1 %d", &status);
    close(s);
    CHECK(status == 200);
    CHECK(strstr(resp, "{\"n\":3}") != NULL);
}

/* Idle connections hold no worker: four from anyone on the LAN (or a browser's speculative connects)
 * used to keep the page from answering for as long as they kept coming. A request behind them is
 * answered at once. */
static void idle_connections_take_no_worker(void) {
    int idle[8], i, st;
    char resp[2048];
    double t0;
    for (i = 0; i < 8; i++) idle[i] = dial();
    for (i = 0; i < 8; i++) CHECK(idle[i] >= 0);
    sleep_ms(300);
    t0 = now_s();
    st = request("/api/x", webui_token(), resp, sizeof resp);
    CHECK(st == 200);
    CHECK(now_s() - t0 < 1.0);
    for (i = 0; i < 8; i++)
        if (idle[i] >= 0) close(idle[i]);
}

/* An idle connection is closed at its deadline, by nobody's worker. */
static void idle_connection_closed_at_deadline(void) {
    char buf[64];
    int s = dial(), n;
    double t0 = now_s();
    CHECK(s >= 0);
    if (s < 0) return;
    n = read_all(s, buf, sizeof buf, CONN_MS + 2000);
    CHECK(n == 0);                  /* closed, with nothing said */
    CHECK(now_s() - t0 > CONN_S - 1.0 && now_s() - t0 < CONN_S + 1.5);
    close(s);
}

/* Four half-sent requests do occupy every worker (they said something); a request behind them is still
 * served once their deadline (5 s in the helper) drops them. */
static void silent_client_dropped_and_next_served(void) {
    int idle[4], i, st;
    char resp[2048];
    double t0 = now_s();
    for (i = 0; i < 4; i++) {
        idle[i] = dial();
        CHECK(idle[i] >= 0);
        if (idle[i] >= 0) CHECK(send(idle[i], "G", 1, 0) == 1);
    }
    /* Let the workers take them first, so the request below is
     * queued behind them with its own (later) deadline. */
    sleep_ms(1000);
    st = request("/api/x", webui_token(), resp, sizeof resp);
    CHECK(st == 200);
    CHECK(now_s() - t0 < CONN_S + 2.5);
    for (i = 0; i < 4; i++)
        if (idle[i] >= 0) close(idle[i]);
}

/* 1 when the server has closed s (EOF or a reset), 0 when it still holds it. */
static int closed_by_server(int s, int wait_ms) {
    struct pollfd p = { s, POLLIN, 0 };
    char b;
    if (poll(&p, 1, wait_ms) <= 0) return 0;
    return recv(s, &b, 1, MSG_PEEK) <= 0;
}

/* One address cannot take the whole server: past its 16 connections the next are closed at once, and
 * another address is still served. Once it lets go, it is served again. */
static void one_address_cannot_fill_the_server(void) {
    enum { N = 30 };
    int idle[N], i, st, closed = 0;
    char resp[2048];
    double t0;
    for (i = 0; i < N; i++) idle[i] = dial();
    for (i = 0; i < N; i++) CHECK(idle[i] >= 0);
    sleep_ms(300);
    for (i = 0; i < N; i++)
        if (idle[i] >= 0 && closed_by_server(idle[i], 50)) closed++;
    CHECK(closed == N - 16);

    /* The same address, over its limit: turned away, at once, not queued. */
    st = request("/api/x", webui_token(), resp, sizeof resp);
    CHECK(st == -1);

    /* Another address (Linux routes all of 127/8; macOS has only 127.0.0.1). */
    {
        struct sockaddr_in me, to;
        int s = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(s >= 0);
        memset(&me, 0, sizeof me);
        me.sin_family = AF_INET;
        me.sin_addr.s_addr = htonl(0x7f000002);
        if (s >= 0 && bind(s, (struct sockaddr *)&me, sizeof me) == 0) {
            char req[512];
            int n = snprintf(req, sizeof req,
                             "GET /api/x HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\n\r\n",
                             (unsigned)g_port, webui_token());
            memset(&to, 0, sizeof to);
            to.sin_family = AF_INET;
            to.sin_port = htons(g_port);
            to.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
            t0 = now_s();
            st = -1;
            if (connect(s, (struct sockaddr *)&to, sizeof to) == 0 && send(s, req, (size_t)n, 0) == n &&
                read_all(s, resp, sizeof resp, 3000) > 0)
                (void)sscanf(resp, "HTTP/1.1 %d", &st);
            CHECK(st == 200);
            CHECK(now_s() - t0 < 1.0);
        }
        if (s >= 0) close(s);
    }

    /* It lets go: the count comes down and it is served again. */
    for (i = 0; i < N; i++)
        if (idle[i] >= 0) close(idle[i]);
    sleep_ms(300);
    st = request("/api/x", webui_token(), resp, sizeof resp);
    CHECK(st == 200);
}

/* A request that never finishes its headers is dropped within the timeout. */
static void half_request_dropped_within_timeout(void) {
    char buf[256];
    int s = dial(), n;
    double t0 = now_s();
    CHECK(s >= 0);
    if (s < 0) return;
    CHECK(send(s, "GET /api/x HTTP/1.1\r\nHost: 127.0.0.1", 36, 0) == 36);
    n = read_all(s, buf, sizeof buf, CONN_MS + 2000);
    CHECK(n >= 0);              /* closed (EOF), not still open 2 s past the deadline */
    CHECK(now_s() - t0 < CONN_S + 2.0);
    close(s);
}

/* 40 idle connections: more than queue depth + workers. The overflow is
 * closed at once and a fresh request retried once per second is answered
 * within the deadline + 2 s; the accept thread never blocks on the full queue. */
static void queue_overflow_closes_not_blocks(void) {
    int idle[40], i, ok = 0;
    char resp[2048];
    double t0 = now_s();
    for (i = 0; i < 40; i++) idle[i] = dial();
    for (i = 0; i < 40; i++) CHECK(idle[i] >= 0);
    while (now_s() - t0 < CONN_S + 2.0) {
        double t1 = now_s();
        int s = dial();
        if (s >= 0) {
            char req[512];
            int n = snprintf(req, sizeof req,
                             "GET /api/x HTTP/1.1\r\nHost: 127.0.0.1:%u\r\nX-PS5U-Token: %s\r\n\r\n",
                             (unsigned)g_port, webui_token());
            int st = -1;
            if (send(s, req, (size_t)n, 0) == n && read_all(s, resp, sizeof resp, 1000) > 0)
                (void)sscanf(resp, "HTTP/1.1 %d", &st);
            close(s);
            if (st == 200) { ok = 1; break; }
        }
        if (now_s() - t1 < 1.0) sleep_ms((int)((1.0 - (now_s() - t1)) * 1000.0));
    }
    CHECK(ok);
    CHECK(now_s() - t0 < CONN_S + 2.5);
    for (i = 0; i < 40; i++)
        if (idle[i] >= 0) close(idle[i]);
}

static void stop_then_start_rebinds_with_new_token(void) {
    char t1[65], resp[2048];
    double t0;
    snprintf(t1, sizeof t1, "%s", webui_token());
    CHECK(strlen(t1) == 64);
    t0 = now_s();
    webui_stop();
    CHECK(now_s() - t0 < 2.5);
    CHECK(webui_token()[0] == '\0');
    CHECK(request("/api/x", t1, resp, sizeof resp) == -1); /* nothing listening */
    CHECK(start_server() == 0);
    CHECK(strlen(webui_token()) == 64);
    CHECK(strcmp(t1, webui_token()) != 0);
    CHECK(request("/api/x", webui_token(), resp, sizeof resp) == 200);
    CHECK(request("/api/x", t1, resp, sizeof resp) == 401);
}

/* Stop while workers are blocked on half-sent requests, with more connections queued and idle ones
 * waiting: still within 2 s. */
static void stop_while_workers_busy(void) {
    int idle[10], i;
    double t0;
    for (i = 0; i < 10; i++) {
        idle[i] = dial();
        if (i < 6 && idle[i] >= 0) CHECK(send(idle[i], "G", 1, 0) == 1);
    }
    sleep_ms(200);
    t0 = now_s();
    webui_stop();
    CHECK(now_s() - t0 < 2.5);
    for (i = 0; i < 10; i++)
        if (idle[i] >= 0) close(idle[i]);
    CHECK(webui_start(NULL) == -1);
    CHECK(start_server() == 0);
    CHECK(start_server() == -1); /* already running */
}

static void restart_keeps_token_and_serves(void) {
    char t1[65], resp[2048];
    snprintf(t1, sizeof t1, "%s", webui_token());
    CHECK(webui_restart(0) == 0);
    CHECK(strcmp(t1, webui_token()) == 0);
    CHECK(request("/api/x", webui_token(), resp, sizeof resp) == 200);
}

/* The listener watch's decisions (pure). */
static void watch_decisions(void) {
    char a[3][INET_ADDRSTRLEN] = { "127.0.0.1", "192.0.2.99", "169.254.3.4" };
    char b[3][INET_ADDRSTRLEN] = { "192.0.2.99", "169.254.3.4", "127.0.0.1" };
    char c[2][INET_ADDRSTRLEN] = { "127.0.0.1", "192.0.2.120" };
    char d[2][INET_ADDRSTRLEN] = { "127.0.0.1", "169.254.9.9" };
    char e[1][INET_ADDRSTRLEN] = { "127.0.0.1" };
    char f[2][INET_ADDRSTRLEN] = { "8.8.4.4", "172.20.1.2" };
    char out[INET_ADDRSTRLEN];

    /* resume: a normal 5 s check is not one; a 10 s+ gap on either clock is */
    CHECK(webui_watch_resumed(1000, 6000, 50000, 55000) == 0);
    CHECK(webui_watch_resumed(1000, 6200, 50000, 55300) == 0); /* jitter */
    CHECK(webui_watch_resumed(1000, 31000, 50000, 80000) == 1); /* the thread was frozen */
    CHECK(webui_watch_resumed(1000, 6000, 50000, 3650000) == 1); /* monotonic stopped in rest mode */
    CHECK(webui_watch_resumed(6000, 1000, 0, 0) == 0);          /* never underflows */

    /* broken listener: only after 5 s of failures */
    CHECK(webui_watch_broken(0, 999999) == 0);
    CHECK(webui_watch_broken(1000, 5999) == 0);
    CHECK(webui_watch_broken(1000, 6000) == 1);

    /* address sets */
    CHECK(webui_addrs_differ((const char (*)[INET_ADDRSTRLEN])a, 3, (const char (*)[INET_ADDRSTRLEN])b, 3) == 0);
    CHECK(webui_addrs_differ((const char (*)[INET_ADDRSTRLEN])a, 3, (const char (*)[INET_ADDRSTRLEN])c, 2) == 1);
    CHECK(webui_addrs_differ((const char (*)[INET_ADDRSTRLEN])c, 2, (const char (*)[INET_ADDRSTRLEN])d, 2) == 1); /* DHCP moved */
    CHECK(webui_addrs_differ((const char (*)[INET_ADDRSTRLEN])e, 0, (const char (*)[INET_ADDRSTRLEN])e, 0) == 0);

    /* the address a phone opens */
    CHECK(webui_pick_lan_addr((const char (*)[INET_ADDRSTRLEN])a, 3, out) == 1 && strcmp(out, "192.0.2.99") == 0);
    CHECK(webui_pick_lan_addr((const char (*)[INET_ADDRSTRLEN])d, 2, out) == 1 && strcmp(out, "169.254.9.9") == 0);
    CHECK(webui_pick_lan_addr((const char (*)[INET_ADDRSTRLEN])e, 1, out) == 0 && out[0] == '\0');
    CHECK(webui_pick_lan_addr((const char (*)[INET_ADDRSTRLEN])f, 2, out) == 1 && strcmp(out, "172.20.1.2") == 0);
}

/* The watch runs on the accept thread every WEBUI_WATCH_MS (5 s in the helper): across a check the
 * server keeps serving (loopback: nothing to rebind), and LAN mode off means no address for a phone. */
static void watch_cycle_keeps_serving(void) {
    char resp[2048], addr[INET_ADDRSTRLEN];
    sleep_ms((int)WEBUI_WATCH_MS + 600);
    CHECK(request("/api/x", webui_token(), resp, sizeof resp) == 200);
    CHECK(webui_lan_addr(addr) == 0 && addr[0] == '\0');
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    g_port = free_port();
    CHECK(g_port != 0);
    CHECK(start_server() == 0);
    serves_api_with_token();
    api_without_token_401();
    bad_host_403();
    garbage_400();
    too_big_413();
    streamed_body();
    transfers_leave_a_worker();
    idle_connections_take_no_worker();
    idle_connection_closed_at_deadline();
    silent_client_dropped_and_next_served();
    half_request_dropped_within_timeout();
    one_address_cannot_fill_the_server();
    queue_overflow_closes_not_blocks();
    stop_then_start_rebinds_with_new_token();
    stop_while_workers_busy();
    restart_keeps_token_and_serves();
    watch_decisions();
    watch_cycle_keeps_serving();
    webui_stop();
    if (failures) {
        fprintf(stderr, "webui_server_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("webui_server_selftest: ok\n");
    return 0;
}
