/* Bounded HTTP server for the console web UI (see webui_server.h).
 *
 * Shape: one accept thread, WEBUI_WORKERS workers, a fixed queue between
 * them. Nothing here can block without a bound:
 *   - the listener is non-blocking and polled in 200 ms slices, so the accept
 *     thread notices stop/rebind requests and never sticks in accept();
 *   - a new connection waits on the accept thread until it sends something, so
 *     an idle one (a browser's speculative connect, or four from anyone on the
 *     LAN) holds no worker; it is closed at its deadline;
 *   - one address cannot take the whole server (WEBUI_PER_SOURCE_MAX);
 *   - a full queue closes the new connection instead of waiting;
 *   - every connection has a deadline of accept time + 5 s; a worker reads in
 *     poll slices up to that deadline and drops an expired connection unread;
 *   - what was turned away is counted and logged every few seconds;
 *   - writes carry SO_SNDTIMEO and a total budget, so a client that stops
 *     reading frees the worker.
 *
 * Lifetime: all state lives in one heap object (srv_t) with a reference per
 * thread plus one for the running server. webui_stop() joins what finishes
 * within 2 s and detaches the rest; a detached worker still inside route()
 * keeps the object alive until it returns. A connection's fd is cleared from
 * the worker slot under the lock BEFORE it is closed, so stop() can never
 * shutdown() a descriptor number that was reused by another subsystem. */
#include "webui_server.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "ava1_thread.h"
#include "proc_identity.h"
#include "webui_auth.h"

#define WEBUI_WORKERS        4
#define WEBUI_QUEUE_DEPTH    32
#define WEBUI_WAITING_MAX    32      /* connections waiting for their first bytes */
#define WEBUI_PER_SOURCE_MAX 16      /* one address's connections anywhere in the server: a browser opens 6 */
#define WEBUI_CONN_MS        5000u   /* read timeout / idle-connection drop */
#define WEBUI_BIND_RETRY_MS  5000u
#define WEBUI_STOP_MS        2000u
#define WEBUI_SLICE_MS       200
#define WEBUI_MIN_SEND_MS    1000u   /* a route that computed long still gets this to write */
#define WEBUI_DRAIN_MS       250u
#define WEBUI_STACK          AVA1_MGMT_STACK  /* routes run mgmt handlers sized for 512 KiB */
#define WEBUI_MAX_ADDRS      8
#define WEBUI_WATCH_MS       5000u   /* the accept thread's listener check (resume, IP change) */
#define WEBUI_BROKEN_MS      5000u   /* a listener failing this long is rebound */
#define WEBUI_RESUME_GAP_MS  10000u  /* a clock gap this long between checks = a resume */
#define WEBUI_THREAD_NAME    "ps5upload-web"
#define WEBUI_BUF_CAP        (WEBUI_HEADER_CAP + WEBUI_BODY_CAP)

typedef struct {
    int fd;
    uint64_t deadline_ms;
    uint32_t src;                   /* the peer's IPv4 address */
} conn_t;

struct srv;

typedef struct {
    struct srv *s;
    int idx;
} worker_arg_t;

typedef struct srv {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int refs;
    int stop;                       /* read lock-free with __atomic in loops */

    conn_t q[WEBUI_QUEUE_DEPTH];
    int qhead, qlen;

    uint16_t port;
    webui_route_fn route;
    int (*stream_for)(const webui_req_t *r);
    webui_stream_fn stream;
    int lan;
    int rebind_req;                 /* accept thread: close and rebind with ->lan */
    int bound;
    unsigned bind_gen;              /* bumped after every bind attempt */
    int listen_fd;                  /* owned by the accept thread once started */

    char token[65];
    char addrs[WEBUI_MAX_ADDRS][INET_ADDRSTRLEN];
    size_t n_addrs;

    pthread_t acc;
    pthread_t w[WEBUI_WORKERS];
    int w_started[WEBUI_WORKERS];
    int w_done[WEBUI_WORKERS];
    int w_fd[WEBUI_WORKERS];        /* connection being served, -1 if none */
    uint32_t w_src[WEBUI_WORKERS];  /* its address */
    unsigned expired;               /* queued connections past their deadline before a worker got to them */
    worker_arg_t wargs[WEBUI_WORKERS];
    char *wbuf[WEBUI_WORKERS];
} srv_t;

/* g_life serializes start/stop/restart; g_mu guards g_srv and g_token. */
static pthread_mutex_t g_life = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static srv_t *g_srv;
static char g_token[65];

static uint64_t mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void nap_ms(int ms) { (void)poll(NULL, 0, ms); }

static int stopping(srv_t *s) { return __atomic_load_n(&s->stop, __ATOMIC_ACQUIRE); }

static void srv_release(srv_t *s) {
    int last, i;
    pthread_mutex_lock(&s->mu);
    last = (--s->refs == 0);
    pthread_mutex_unlock(&s->mu);
    if (!last) return;
    for (i = 0; i < s->qlen; i++) close(s->q[(s->qhead + i) % WEBUI_QUEUE_DEPTH].fd);
    if (s->listen_fd >= 0) close(s->listen_fd);
    for (i = 0; i < WEBUI_WORKERS; i++) free(s->wbuf[i]);
    pthread_cond_destroy(&s->cv);
    pthread_mutex_destroy(&s->mu);
    free(s);
}

static int thread_start(pthread_t *t, void *(*fn)(void *), void *arg) {
    pthread_attr_t attr;
    int rc;
    if (pthread_attr_init(&attr) != 0) return -1;
    if (pthread_attr_setstacksize(&attr, WEBUI_STACK) != 0) {
        pthread_attr_destroy(&attr);
        return -1;
    }
    rc = pthread_create(t, &attr, fn, arg);
    pthread_attr_destroy(&attr);
    return rc == 0 ? 0 : -1;
}

/* The console's IPv4 addresses, for the LAN Host check; -1 when they could not be read
 * (getifaddrs failed: the caller keeps the list it has, so a passing failure never looks like an
 * address change). */
static int own_addrs(char out[WEBUI_MAX_ADDRS][INET_ADDRSTRLEN]) {
    struct ifaddrs *ifa = NULL, *p;
    int n = 0;
    if (getifaddrs(&ifa) != 0) return -1;
    for (p = ifa; p && n < WEBUI_MAX_ADDRS; p = p->ifa_next) {
        if (!p->ifa_addr || p->ifa_addr->sa_family != AF_INET) continue;
        if (!inet_ntop(AF_INET, &((struct sockaddr_in *)(void *)p->ifa_addr)->sin_addr,
                       out[n], INET_ADDRSTRLEN))
            continue;
        n++;
    }
    freeifaddrs(ifa);
    return n;
}

/* ---- listener watch (pure; webui_server.h) ------------------------------ */

int webui_watch_broken(uint64_t err_since, uint64_t now) {
    return err_since != 0 && now >= err_since && now - err_since >= WEBUI_BROKEN_MS;
}

int webui_watch_resumed(uint64_t mono_prev, uint64_t mono_now, int64_t real_prev, int64_t real_now) {
    uint64_t mono = mono_now >= mono_prev ? mono_now - mono_prev : 0;
    int64_t real = real_now - real_prev;
    /* The loop wakes every 200 ms: a long gap on either clock means the console slept (or the
     * thread was frozen); a wall clock that ran far ahead of the monotonic one too (a clock that
     * stops in rest mode). A clock set by hand looks the same: one harmless rebind. */
    if (mono > WEBUI_RESUME_GAP_MS) return 1;
    if (real > (int64_t)mono + (int64_t)WEBUI_RESUME_GAP_MS) return 1;
    return 0;
}

int webui_addrs_differ(const char (*a)[INET_ADDRSTRLEN], size_t na, const char (*b)[INET_ADDRSTRLEN], size_t nb) {
    size_t i, j;
    if (na != nb) return 1;
    for (i = 0; i < na; i++) {
        for (j = 0; j < nb; j++)
            if (strcmp(a[i], b[j]) == 0) break;
        if (j == nb) return 1;
    }
    return 0;
}

int webui_pick_lan_addr(const char (*addrs)[INET_ADDRSTRLEN], size_t n, char out[INET_ADDRSTRLEN]) {
    size_t i;
    int best = -1, best_rank = 0;
    for (i = 0; i < n; i++) {
        const char *a = addrs[i];
        int rank;
        if (strncmp(a, "127.", 4) == 0 || strncmp(a, "0.", 2) == 0) continue;
        /* a private LAN address first, then any other, link-local (no DHCP answer) last */
        if (strncmp(a, "192.168.", 8) == 0 || strncmp(a, "10.", 3) == 0 ||
            (strncmp(a, "172.", 4) == 0 && atoi(a + 4) >= 16 && atoi(a + 4) <= 31))
            rank = 3;
        else if (strncmp(a, "169.254.", 8) == 0)
            rank = 1;
        else
            rank = 2;
        if (rank > best_rank) {
            best_rank = rank;
            best = (int)i;
        }
    }
    if (best < 0) {
        out[0] = '\0';
        return 0;
    }
    memcpy(out, addrs[best], INET_ADDRSTRLEN);
    out[INET_ADDRSTRLEN - 1] = '\0';
    return 1;
}

static int64_t real_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)ts.tv_nsec / 1000000;
}

/* Non-blocking listener on 127.0.0.1 or 0.0.0.0. -1 with errno on failure. */
static int bind_listener(uint16_t port, int lan) {
    struct sockaddr_in a;
    int one = 1, fd, fl, e;
    fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = htonl(lan ? INADDR_ANY : INADDR_LOOPBACK);
    fl = fcntl(fd, F_GETFL, 0);
    if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) != 0 ||
        bind(fd, (struct sockaddr *)&a, sizeof a) != 0 || listen(fd, 64) != 0) {
        e = errno;
        close(fd);
        errno = e;
        return -1;
    }
    return fd;
}

static void set_timeo(int fd, int opt, uint64_t ms) {
    struct timeval tv;
    if (ms == 0) ms = 1; /* 0 would mean "block forever" */
    tv.tv_sec = (time_t)(ms / 1000u);
    tv.tv_usec = (suseconds_t)((ms % 1000u) * 1000u);
    (void)setsockopt(fd, SOL_SOCKET, opt, &tv, sizeof tv);
}

/* ---- responses ---------------------------------------------------------- */

static uint64_t send_budget_ms(int fd) {
    struct timeval tv;
    socklen_t l = sizeof tv;
    uint64_t ms;
    if (getsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, &l) != 0) return WEBUI_CONN_MS;
    ms = (uint64_t)tv.tv_sec * 1000u + (uint64_t)tv.tv_usec / 1000u;
    if (ms == 0 || ms > WEBUI_CONN_MS) ms = WEBUI_CONN_MS;
    return ms;
}

/* Write all of buf before `end` (monotonic ms). 0 on success. */
static int send_all(int fd, const void *buf, size_t len, uint64_t end) {
    const char *p = (const char *)buf;
    while (len > 0) {
        struct pollfd pf;
        uint64_t now = mono_ms();
        ssize_t n;
        int r;
        if (now >= end) return -1;
        pf.fd = fd;
        pf.events = POLLOUT;
        pf.revents = 0;
        r = poll(&pf, 1, (int)(end - now));
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1;
        n = send(fd, p, len, 0);
        if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
        if (n <= 0) return -1;
        p += n;
        len -= (size_t)n;
    }
    return 0;
}

void webui_send(int fd, int status, const char *ctype, const void *body, size_t len) {
    webui_send_ex(fd, status, ctype, body, len, NULL);
}

void webui_send_ex(int fd, int status, const char *ctype, const void *body, size_t len, const char *extra_headers) {
    char head[512];
    uint64_t end = mono_ms() + send_budget_ms(fd);
    int hl = webui_http_head(head, sizeof head, status, ctype, len, extra_headers);
    if (hl < 0) return;
    if (send_all(fd, head, (size_t)hl, end) != 0) return;
    if (len > 0 && body) (void)send_all(fd, body, len, end);
}

void webui_send_json(int fd, int status, const char *json) {
    webui_send(fd, status, "application/json", json, json ? strlen(json) : 0);
}

/* ---- one connection ----------------------------------------------------- */

enum { RD_OK = 0, RD_DROP = -1, RD_STREAM = 1 };

/* Read one request within the deadline. Returns RD_OK, RD_STREAM (the headers are in; the body is
 * the stream handler's to read), RD_DROP (silent close), or an HTTP status (400/413) to answer with. */
static int read_request(srv_t *s, int fd, uint64_t deadline, char *buf, webui_req_t *req,
                        size_t *got_out) {
    size_t got = 0, need = 0;
    int parsed = 0;
    for (;;) {
        struct pollfd pf;
        uint64_t now;
        ssize_t n;
        int r, wait;
        if (got > 0 && !parsed) {
            r = webui_http_parse_cap(buf, got, req, s->stream ? WEBUI_STREAM_BODY_MAX : WEBUI_BODY_CAP);
            if (r == WEBUI_PARSE_BAD) return 400;
            if (r == WEBUI_PARSE_TOO_BIG) return 413;
            if (r == WEBUI_PARSE_OK) {
                if (s->stream && s->stream_for && s->stream_for(req)) {
                    *got_out = got;
                    return RD_STREAM;
                }
                if (req->content_length > WEBUI_BODY_CAP) return 413;
                parsed = 1;
                need = req->header_len + req->content_length;
            }
        }
        if (parsed && got >= need) {
            *got_out = got;
            return RD_OK;
        }
        if (got >= WEBUI_BUF_CAP) return 413;
        if (stopping(s)) return RD_DROP;
        now = mono_ms();
        if (now >= deadline) return RD_DROP;
        wait = (int)(deadline - now);
        if (wait > WEBUI_SLICE_MS) wait = WEBUI_SLICE_MS;
        pf.fd = fd;
        pf.events = POLLIN;
        pf.revents = 0;
        r = poll(&pf, 1, wait);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return RD_DROP;
        if (r == 0) continue;
        n = recv(fd, buf + got, WEBUI_BUF_CAP - got, 0);
        if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
        if (n <= 0) return RD_DROP;
        got += (size_t)n;
    }
}

/* Half-close, then read off whatever the client still sends for a moment, so
 * the close does not turn into a reset that discards the response. */
static void finish_conn(int fd, char *scratch, size_t cap) {
    uint64_t end = mono_ms() + WEBUI_DRAIN_MS;
    size_t total = 0;
    (void)shutdown(fd, SHUT_WR);
    while (total < 2 * WEBUI_BUF_CAP) {
        struct pollfd pf;
        uint64_t now = mono_ms();
        ssize_t n;
        if (now >= end) break;
        pf.fd = fd;
        pf.events = POLLIN;
        pf.revents = 0;
        if (poll(&pf, 1, (int)(end - now)) <= 0) break;
        n = recv(fd, scratch, cap, 0);
        if (n <= 0) break;
        total += (size_t)n;
    }
}

static void serve(srv_t *s, int idx, conn_t c) {
    char *buf = s->wbuf[idx];
    webui_req_t req;
    webui_auth_cfg_t ac;
    char token[65];
    char addrs[WEBUI_MAX_ADDRS][INET_ADDRSTRLEN];
    const char *addr_ptrs[WEBUI_MAX_ADDRS];
    size_t got = 0, i, n_addrs;
    uint64_t now;
    int rd, is_api, lan;

    set_timeo(c.fd, SO_RCVTIMEO, WEBUI_CONN_MS);
    set_timeo(c.fd, SO_SNDTIMEO, WEBUI_CONN_MS);
    memset(&req, 0, sizeof req);
    rd = read_request(s, c.fd, c.deadline_ms, buf, &req, &got);
    if (rd == RD_DROP) return;

    /* Writes get what is left of the deadline, but at least a short window. */
    now = mono_ms();
    set_timeo(c.fd, SO_SNDTIMEO,
              c.deadline_ms > now + WEBUI_MIN_SEND_MS ? c.deadline_ms - now : WEBUI_MIN_SEND_MS);

    if (rd == 400) {
        webui_send_json(c.fd, 400, "{\"error\":\"bad_request\"}");
        return;
    }
    if (rd == 413) {
        webui_send_json(c.fd, 413, "{\"error\":\"too_big\"}");
        return;
    }

    pthread_mutex_lock(&s->mu);
    memcpy(token, s->token, sizeof token);
    lan = s->lan;
    n_addrs = s->n_addrs;
    memcpy(addrs, s->addrs, sizeof addrs);
    pthread_mutex_unlock(&s->mu);
    for (i = 0; i < n_addrs; i++) addr_ptrs[i] = addrs[i];
    ac.token = token;
    ac.port = s->port;
    ac.lan = lan;
    ac.own_addrs = addr_ptrs;
    ac.n_own = n_addrs;

    is_api = strncmp(req.path, "/api/", 5) == 0 && strcmp(req.path, "/api/token") != 0;
    switch (webui_check(&req, &ac, is_api)) {
    case WEBUI_ALLOW:
        break;
    case WEBUI_DENY_TOKEN:
        webui_send_json(c.fd, 401, "{\"error\":\"token\"}");
        return;
    default:
        webui_send_json(c.fd, 403, "{\"error\":\"forbidden\"}");
        return;
    }
    if (rd == RD_STREAM) {
        /* The bytes past the headers that came in the same reads (never more than the body). */
        size_t pre = got > req.header_len ? got - req.header_len : 0;
        if (pre > req.content_length) pre = (size_t)req.content_length;
        (void)s->stream(&req, (const uint8_t *)buf + req.header_len, pre, c.fd);
        return;
    }
    if (s->route)
        (void)s->route(&req, (const uint8_t *)buf + req.header_len, (size_t)req.content_length, c.fd);
    else
        webui_send_json(c.fd, 404, "{\"error\":\"not_found\"}");
}

static void *worker_main(void *arg) {
    worker_arg_t *wa = (worker_arg_t *)arg;
    srv_t *s = wa->s;
    int idx = wa->idx;
    proc_name_set_self(WEBUI_THREAD_NAME);
    for (;;) {
        conn_t c;
        pthread_mutex_lock(&s->mu);
        while (!s->stop && s->qlen == 0) pthread_cond_wait(&s->cv, &s->mu);
        if (s->stop) {
            pthread_mutex_unlock(&s->mu);
            break;
        }
        c = s->q[s->qhead];
        s->qhead = (s->qhead + 1) % WEBUI_QUEUE_DEPTH;
        s->qlen--;
        s->w_fd[idx] = c.fd;
        s->w_src[idx] = c.src;
        pthread_mutex_unlock(&s->mu);

        if (mono_ms() < c.deadline_ms) {
            serve(s, idx, c);
            finish_conn(c.fd, s->wbuf[idx], WEBUI_BUF_CAP);
        } else {
            pthread_mutex_lock(&s->mu);
            s->expired++;
            pthread_mutex_unlock(&s->mu);
        }
        pthread_mutex_lock(&s->mu);
        s->w_fd[idx] = -1;
        pthread_mutex_unlock(&s->mu);
        close(c.fd);
    }
    pthread_mutex_lock(&s->mu);
    s->w_done[idx] = 1;
    pthread_mutex_unlock(&s->mu);
    srv_release(s);
    return NULL;
}

/* ---- accept thread ------------------------------------------------------ */

/* Close the listener so the loop binds a fresh one (s->mu held). */
static void drop_listener_locked(srv_t *s) {
    if (s->listen_fd >= 0) close(s->listen_fd);
    s->listen_fd = -1;
    s->bound = 0;
}

/* Every WEBUI_WATCH_MS: after a resume from rest mode, or (LAN access on) when the console's
 * addresses changed (DHCP, another network), refresh the Host allowlist and rebind, as Payload
 * Manager's watchdog does. One getifaddrs per check, on this thread: no thread, no allocation that
 * outlives the call. */
static void watch_listener(srv_t *s, uint64_t *mono_prev, int64_t *real_prev) {
    uint64_t mono = mono_ms();
    int64_t real = real_ms();
    int resumed, lan, changed = 0;
    char addrs[WEBUI_MAX_ADDRS][INET_ADDRSTRLEN];
    int n = -1;
    resumed = webui_watch_resumed(*mono_prev, mono, *real_prev, real);
    *mono_prev = mono;
    *real_prev = real;
    pthread_mutex_lock(&s->mu);
    lan = s->lan;
    pthread_mutex_unlock(&s->mu);
    if (lan) n = own_addrs(addrs);
    pthread_mutex_lock(&s->mu);
    if (lan && s->lan && n >= 0 &&
        webui_addrs_differ((const char (*)[INET_ADDRSTRLEN])s->addrs, s->n_addrs,
                           (const char (*)[INET_ADDRSTRLEN])addrs, (size_t)n)) {
        memcpy(s->addrs, addrs, sizeof addrs);
        s->n_addrs = (size_t)n;
        changed = 1;
    }
    if ((resumed || changed) && s->listen_fd >= 0) drop_listener_locked(s);
    pthread_mutex_unlock(&s->mu);
    if (resumed) fprintf(stderr, "webui: resumed (clock gap); rebinding port %u\n", (unsigned)s->port);
    if (changed) fprintf(stderr, "webui: console addresses changed (%d now); rebinding port %u\n", n, (unsigned)s->port);
}

/* What the accept thread turned away since it last said so. */
typedef struct {
    unsigned queue_full, per_source, idle_evicted;
} drops_t;

/* One line every WEBUI_WATCH_MS at most, only when something was turned away: a console page that
 * stops answering must leave a trace of why. */
static void report_drops(srv_t *s, drops_t *d) {
    unsigned expired;
    pthread_mutex_lock(&s->mu);
    expired = s->expired;
    s->expired = 0;
    pthread_mutex_unlock(&s->mu);
    if (d->queue_full || d->per_source || d->idle_evicted || expired)
        fprintf(stderr,
                "webui: connections turned away: %u queue full, %u over %d from one address, "
                "%u idle evicted, %u expired in the queue\n",
                d->queue_full, d->per_source, WEBUI_PER_SOURCE_MAX, d->idle_evicted, expired);
    memset(d, 0, sizeof *d);
}

/* To a worker, or closed when the queue is full: never wait. */
static void hand_to_worker(srv_t *s, const conn_t *c, drops_t *d) {
    int queued = 0;
    pthread_mutex_lock(&s->mu);
    if (!s->stop && s->qlen < WEBUI_QUEUE_DEPTH) {
        s->q[(s->qhead + s->qlen) % WEBUI_QUEUE_DEPTH] = *c;
        s->qlen++;
        pthread_cond_signal(&s->cv);
        queued = 1;
    }
    pthread_mutex_unlock(&s->mu);
    if (!queued) {
        close(c->fd);
        d->queue_full++;
    }
}

/* The connections `src` has anywhere in the server. */
static int source_count(srv_t *s, const conn_t *waiting, int n_waiting, uint32_t src) {
    int n = 0, i;
    for (i = 0; i < n_waiting; i++)
        if (waiting[i].src == src) n++;
    pthread_mutex_lock(&s->mu);
    for (i = 0; i < s->qlen; i++)
        if (s->q[(s->qhead + i) % WEBUI_QUEUE_DEPTH].src == src) n++;
    for (i = 0; i < WEBUI_WORKERS; i++)
        if (s->w_fd[i] >= 0 && s->w_src[i] == src) n++;
    pthread_mutex_unlock(&s->mu);
    return n;
}

/* After a poll over `pfs` (pfs[1 + i] is waiting[i]): what has something to read, or was closed, goes
 * to a worker; what reached its deadline without a byte is closed here. */
static int sweep_waiting(srv_t *s, conn_t *waiting, int n_waiting, const struct pollfd *pfs, drops_t *d) {
    uint64_t now = mono_ms();
    int i, keep = 0;
    for (i = 0; i < n_waiting; i++) {
        if (pfs[1 + i].revents) hand_to_worker(s, &waiting[i], d);
        else if (now >= waiting[i].deadline_ms) close(waiting[i].fd);
        else waiting[keep++] = waiting[i];
    }
    return keep;
}

static void *accept_main(void *arg) {
    srv_t *s = (srv_t *)arg;
    unsigned fails = 0;
    uint64_t next_watch = mono_ms() + WEBUI_WATCH_MS, mono_prev = mono_ms(), err_since = 0;
    int64_t real_prev = real_ms();
    conn_t waiting[WEBUI_WAITING_MAX];
    struct pollfd pfs[1 + WEBUI_WAITING_MAX];
    int n_waiting = 0, i;
    drops_t drops;
    memset(&drops, 0, sizeof drops);
    proc_name_set_self(WEBUI_THREAD_NAME);
    while (!stopping(s)) {
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof peer;
        conn_t c;
        int fd, r, lan, fl, listener;

        if (mono_ms() >= next_watch) {
            watch_listener(s, &mono_prev, &real_prev);
            report_drops(s, &drops);
            next_watch = mono_ms() + WEBUI_WATCH_MS;
        }

        pthread_mutex_lock(&s->mu);
        if (s->rebind_req) {
            s->rebind_req = 0;
            drop_listener_locked(s);
            fails = 0;
        }
        lan = s->lan;
        fd = s->listen_fd;
        pthread_mutex_unlock(&s->mu);

        if (fd < 0) {
            uint64_t until;
            fd = bind_listener(s->port, lan);
            int e = errno;
            pthread_mutex_lock(&s->mu);
            s->listen_fd = fd;
            s->bound = fd >= 0;
            s->bind_gen++;
            pthread_cond_broadcast(&s->cv);
            pthread_mutex_unlock(&s->mu);
            if (fd >= 0) {
                if (fails) fprintf(stderr, "webui: bound port %u after %u retries\n", (unsigned)s->port, fails);
                fails = 0;
                continue;
            }
            /* First failure and then once a minute: the log must not grow every 5 s forever. */
            if (fails % 12 == 0)
                fprintf(stderr, "webui: bind port %u (%s) failed: errno %d; retrying every 5 s\n",
                        (unsigned)s->port, lan ? "lan" : "loopback", e);
            fails++;
            until = mono_ms() + WEBUI_BIND_RETRY_MS;
            while (!stopping(s) && mono_ms() < until) {
                int req;
                pthread_mutex_lock(&s->mu);
                req = s->rebind_req;
                pthread_mutex_unlock(&s->mu);
                if (req) break;
                nap_ms(WEBUI_SLICE_MS);
            }
            continue;
        }

        pfs[0].fd = fd;
        pfs[0].events = POLLIN;
        pfs[0].revents = 0;
        for (i = 0; i < n_waiting; i++) {
            pfs[1 + i].fd = waiting[i].fd;
            pfs[1 + i].events = POLLIN;
            pfs[1 + i].revents = 0;
        }
        r = poll(pfs, (nfds_t)(1 + n_waiting), WEBUI_SLICE_MS);
        if (r >= 0) n_waiting = sweep_waiting(s, waiting, n_waiting, pfs, &drops);
        listener = pfs[0].revents;
        if (r == 0 || (r > 0 && listener == 0)) {
            err_since = 0; /* a quiet, healthy listener */
            continue;
        }
        if (r < 0 || (listener & (POLLERR | POLLHUP | POLLNVAL))) {
            if (r < 0 && errno == EINTR) continue;
            /* A broken listener (the network went away under it): rebind once it has failed for
             * WEBUI_BROKEN_MS, instead of failing every slice forever. */
            if (!err_since) err_since = mono_ms();
            else if (webui_watch_broken(err_since, mono_ms())) {
                fprintf(stderr, "webui: listener on port %u failing for %u ms; rebinding\n", (unsigned)s->port,
                        (unsigned)WEBUI_BROKEN_MS);
                pthread_mutex_lock(&s->mu);
                if (s->listen_fd == fd) drop_listener_locked(s);
                pthread_mutex_unlock(&s->mu);
                err_since = 0;
                continue;
            }
            nap_ms(50);
            continue;
        }
        r = accept(fd, (struct sockaddr *)&peer, &peer_len);
        if (r < 0) {
            /* Never leave the loop on an accept error (a stray errno once killed a helper's
             * server); back off briefly on the ones that would otherwise spin, and rebind when they
             * go on for WEBUI_BROKEN_MS. */
            int e = errno;
            if (e == EMFILE || e == ENFILE || e == ENOBUFS || e == ENOMEM) {
                /* Out of descriptors or buffers: the listener is fine, the process or system is
                 * busy. Back off; a rebind would not help and must not be counted toward one. */
                nap_ms(50);
            } else if (e != EAGAIN && e != EWOULDBLOCK && e != EINTR && e != ECONNABORTED) {
                if (!err_since) err_since = mono_ms();
                else if (webui_watch_broken(err_since, mono_ms())) {
                    fprintf(stderr, "webui: accept on port %u failing for %u ms (errno %d); rebinding\n",
                            (unsigned)s->port, (unsigned)WEBUI_BROKEN_MS, e);
                    pthread_mutex_lock(&s->mu);
                    if (s->listen_fd == fd) drop_listener_locked(s);
                    pthread_mutex_unlock(&s->mu);
                    err_since = 0;
                    continue;
                }
                nap_ms(50);
            }
            continue;
        }
        err_since = 0;
        /* BSD hands the listener's O_NONBLOCK to the accepted socket; workers expect blocking
         * sockets bounded by timeouts and poll. */
        fl = fcntl(r, F_GETFL, 0);
        if (fl >= 0) (void)fcntl(r, F_SETFL, fl & ~O_NONBLOCK);
#ifdef SO_NOSIGPIPE
        {
            int one = 1;
            (void)setsockopt(r, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
        }
#endif
        c.fd = r;
        c.deadline_ms = mono_ms() + WEBUI_CONN_MS;
        c.src = peer_len >= sizeof peer && peer.sin_family == AF_INET ? ntohl(peer.sin_addr.s_addr) : 0;
        if (source_count(s, waiting, n_waiting, c.src) >= WEBUI_PER_SOURCE_MAX) {
            close(r);
            drops.per_source++;
            continue;
        }
        /* It waits here until it sends something, on the next poll if it already has. */
        if (n_waiting == WEBUI_WAITING_MAX) {
            close(waiting[0].fd); /* the oldest, the closest to its deadline */
            memmove(waiting, waiting + 1, sizeof waiting[0] * (WEBUI_WAITING_MAX - 1));
            n_waiting--;
            drops.idle_evicted++;
        }
        waiting[n_waiting++] = c;
    }
    for (i = 0; i < n_waiting; i++) close(waiting[i].fd);
    pthread_mutex_lock(&s->mu);
    if (s->listen_fd >= 0) close(s->listen_fd);
    s->listen_fd = -1;
    s->bound = 0;
    pthread_mutex_unlock(&s->mu);
    srv_release(s);
    return NULL;
}

/* ---- lifecycle ---------------------------------------------------------- */

static void stop_locked(srv_t *s) {
    uint64_t end;
    int i, all;
    pthread_mutex_lock(&s->mu);
    __atomic_store_n(&s->stop, 1, __ATOMIC_RELEASE);
    pthread_cond_broadcast(&s->cv);
    /* Wake a worker blocked writing to a client that stopped reading. */
    for (i = 0; i < WEBUI_WORKERS; i++)
        if (s->w_fd[i] >= 0) (void)shutdown(s->w_fd[i], SHUT_RDWR);
    /* Queued connections were never served: close them now. */
    while (s->qlen > 0) {
        close(s->q[s->qhead].fd);
        s->qhead = (s->qhead + 1) % WEBUI_QUEUE_DEPTH;
        s->qlen--;
    }
    pthread_mutex_unlock(&s->mu);

    /* The accept thread polls in 200 ms slices; joining it frees the port. */
    pthread_join(s->acc, NULL);

    end = mono_ms() + WEBUI_STOP_MS;
    for (;;) {
        all = 1;
        pthread_mutex_lock(&s->mu);
        for (i = 0; i < WEBUI_WORKERS; i++)
            if (s->w_started[i] && !s->w_done[i]) all = 0;
        pthread_mutex_unlock(&s->mu);
        if (all || mono_ms() >= end) break;
        nap_ms(20);
    }
    for (i = 0; i < WEBUI_WORKERS; i++) {
        int done;
        if (!s->w_started[i]) continue;
        pthread_mutex_lock(&s->mu);
        done = s->w_done[i];
        pthread_mutex_unlock(&s->mu);
        if (done) {
            pthread_join(s->w[i], NULL);
        } else {
            fprintf(stderr, "webui: worker %d still inside a request after 2 s; detached\n", i);
            pthread_detach(s->w[i]);
        }
    }
    srv_release(s); /* the running server's own reference */
}

int webui_start(const webui_cfg_t *cfg) {
    srv_t *s;
    int i, e;
    if (!cfg) return -1;
    pthread_mutex_lock(&g_life);
    if (g_srv) {
        pthread_mutex_unlock(&g_life);
        return -1;
    }
    s = (srv_t *)calloc(1, sizeof *s);
    if (!s) goto fail_nomem;
    for (i = 0; i < WEBUI_WORKERS; i++) {
        s->wbuf[i] = (char *)malloc(WEBUI_BUF_CAP);
        s->w_fd[i] = -1;
        if (!s->wbuf[i]) {
            while (i >= 0) free(s->wbuf[i--]);
            free(s);
            goto fail_nomem;
        }
    }
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
    s->refs = 1;
    s->port = cfg->port;
    s->route = cfg->route;
    s->stream_for = cfg->stream_for;
    s->stream = cfg->stream;
    s->lan = cfg->lan ? 1 : 0;
    webui_token_new(s->token);
    if (s->lan) {
        int got = own_addrs(s->addrs);
        s->n_addrs = got > 0 ? (size_t)got : 0;
    }

    /* First bind here so the server answers as soon as start returns; on failure the accept
     * thread retries (a takeover's old instance may still hold the port for a moment). */
    s->listen_fd = bind_listener(s->port, s->lan);
    e = errno;
    s->bound = s->listen_fd >= 0;
    if (s->listen_fd < 0)
        fprintf(stderr, "webui: bind port %u failed: errno %d; retrying every 5 s\n", (unsigned)s->port, e);

    s->refs++;
    if (thread_start(&s->acc, accept_main, s) != 0) {
        s->refs--;
        srv_release(s);
        fprintf(stderr, "webui: accept thread did not start\n");
        pthread_mutex_unlock(&g_life);
        return -1;
    }
    for (i = 0; i < WEBUI_WORKERS; i++) {
        s->wargs[i].s = s;
        s->wargs[i].idx = i;
        pthread_mutex_lock(&s->mu);
        s->refs++;
        pthread_mutex_unlock(&s->mu);
        if (thread_start(&s->w[i], worker_main, &s->wargs[i]) != 0) {
            pthread_mutex_lock(&s->mu);
            s->refs--;
            pthread_mutex_unlock(&s->mu);
            fprintf(stderr, "webui: worker %d did not start\n", i);
            continue; /* fewer workers still serve; zero is handled below */
        }
        s->w_started[i] = 1;
    }
    for (i = 0; i < WEBUI_WORKERS && !s->w_started[i]; i++) {
    }
    if (i == WEBUI_WORKERS) {
        stop_locked(s);
        pthread_mutex_unlock(&g_life);
        return -1;
    }

    pthread_mutex_lock(&g_mu);
    g_srv = s;
    memcpy(g_token, s->token, sizeof g_token);
    pthread_mutex_unlock(&g_mu);
    if (s->bound)
        fprintf(stderr, "webui: serving on %s:%u\n", s->lan ? "0.0.0.0" : "127.0.0.1", (unsigned)s->port);
    pthread_mutex_unlock(&g_life);
    return 0;

fail_nomem:
    fprintf(stderr, "webui: out of memory\n");
    pthread_mutex_unlock(&g_life);
    return -1;
}

int webui_running(void) {
    int on;
    pthread_mutex_lock(&g_mu);
    on = g_srv != NULL;
    pthread_mutex_unlock(&g_mu);
    return on;
}

void webui_stop(void) {
    srv_t *s;
    pthread_mutex_lock(&g_life);
    pthread_mutex_lock(&g_mu);
    s = g_srv;
    g_srv = NULL;
    memset(g_token, 0, sizeof g_token);
    pthread_mutex_unlock(&g_mu);
    if (s) stop_locked(s);
    pthread_mutex_unlock(&g_life);
}

int webui_restart(int lan) {
    srv_t *s;
    char addrs[WEBUI_MAX_ADDRS][INET_ADDRSTRLEN];
    size_t n = 0;
    unsigned gen;
    uint64_t end;
    int bound = 0, got = -1;

    if (lan) {
        got = own_addrs(addrs);
        n = got > 0 ? (size_t)got : 0;
    }
    pthread_mutex_lock(&g_life);
    s = g_srv;
    if (!s) {
        pthread_mutex_unlock(&g_life);
        return -1;
    }
    pthread_mutex_lock(&s->mu);
    s->lan = lan ? 1 : 0;
    if (!lan || got >= 0) { /* a failed read keeps the list it had */
        s->n_addrs = n;
        if (n) memcpy(s->addrs, addrs, sizeof addrs);
    }
    s->rebind_req = 1;
    gen = s->bind_gen;
    s->refs++;
    pthread_mutex_unlock(&s->mu);
    pthread_mutex_unlock(&g_life);

    /* Wait briefly for the accept thread's attempt; it keeps retrying on its own after. */
    end = mono_ms() + 1500u;
    for (;;) {
        int done;
        pthread_mutex_lock(&s->mu);
        done = s->bind_gen != gen && !s->rebind_req;
        bound = s->bound;
        pthread_mutex_unlock(&s->mu);
        if (done || stopping(s) || mono_ms() >= end) break;
        nap_ms(20);
    }
    srv_release(s);
    return bound ? 0 : -1;
}

const char *webui_token(void) { return g_token; }

void webui_token_copy(char out[65]) {
    pthread_mutex_lock(&g_mu);
    memcpy(out, g_token, 65);
    pthread_mutex_unlock(&g_mu);
    out[64] = '\0';
}

int webui_lan_addr(char out[INET_ADDRSTRLEN]) {
    srv_t *s;
    int ok = 0;
    out[0] = '\0';
    pthread_mutex_lock(&g_mu);
    s = g_srv;
    if (s) {
        pthread_mutex_lock(&s->mu);
        if (s->lan) ok = webui_pick_lan_addr((const char (*)[INET_ADDRSTRLEN])s->addrs, s->n_addrs, out);
        pthread_mutex_unlock(&s->mu);
    }
    pthread_mutex_unlock(&g_mu);
    return ok;
}
