/* The loopback app API's listener and its real operations (see include/app_api.h).
 *
 * Shape: one thread, one connection at a time. The only client is PS5Upload Play on this console,
 * which asks a few small questions now and then, so nothing here needs workers:
 *   - the listener is 127.0.0.1 only (never the wildcard address, whatever Minion's LAN setting says), non-
 *     blocking, polled in 200 ms slices so stop and rebind are noticed;
 *   - a connection has APP_API_CONN_MS from accept to the end of its request (a request is at most
 *     APP_API_REQ_MAX bytes) and APP_API_SEND_MS to take its answer; a silent client is closed at
 *     its deadline and holds the thread no longer;
 *   - after rest mode (a clock gap) or when the listener keeps failing (the network going away
 *     under it), the listener is closed and bound again: a helper that survives rest otherwise
 *     keeps a deaf socket;
 *   - every request runs inside the AVA1 local-RPC gate, as Minion's routes do, so the helper's
 *     exit waits for a Remote Play call in flight and refuses new ones (503 stopping).
 * The Remote Play operations are remoteplay.c's own entry points, which take sony_api_lock for the
 * whole call (mesh and readiness reads included); nothing here calls a Sony API itself. */
#include "app_api.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "ava1_server.h"
#include "ava1_thread.h"
#include "config.h"
#include "mesh.h"
#include "proc_identity.h"
#include "remoteplay.h"
#include "tailnet_node.h"
#include "webui_server.h"

#define APP_API_CONN_MS 3000u
#define APP_API_SEND_MS 2000u
#define APP_API_SLICE_MS 200
#define APP_API_WATCH_MS 5000u
#define APP_API_BIND_RETRY_MS 2000u

static pthread_t g_thr;
static int g_started;
static int g_stop; /* __atomic */
static char *g_req, *g_resp;

static uint64_t mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static int64_t real_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)ts.tv_nsec / 1000000;
}

static int stopping(void) { return __atomic_load_n(&g_stop, __ATOMIC_ACQUIRE); }

/* ---- the operations ---- */

static int op_rp_readiness(void *ctx, char *out, size_t cap) {
    (void)ctx;
    return remoteplay_readiness_json(out, cap);
}

static int err_into(char *out, size_t cap, int *http, int status, const char *token) {
    int k = snprintf(out, cap, "{\"error\":\"%s\"}", token);
    *http = status;
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

/* What the engine's pairing does before a PIN (rp_regist.rs): the system service first, then this
 * user's permission where the firmware has one (FW 10.00+). remoteplay_enable re-reads the console,
 * so the answer is the console's own. */
static int op_rp_enable(void *ctx, char *out, size_t cap, int *http) {
    char raw[1024];
    int n, svc = 0, usr = 0, per = 0;
    (void)ctx;
    n = remoteplay_readiness_json(raw, sizeof raw);
    if (n <= 0 || (size_t)n >= sizeof raw || app_api_rp_flags(raw, (size_t)n, &svc, &usr, &per) != 0)
        return err_into(out, cap, http, 503, "rp_unreadable");
    if (!svc) {
        n = remoteplay_enable(0, raw, sizeof raw);
        if (n == -3) return err_into(out, cap, http, 409, "no_user");
        if (n < 0 || (size_t)n >= sizeof raw) return err_into(out, cap, http, 500, "enable_failed");
        if (app_api_rp_flags(raw, (size_t)n, &svc, &usr, &per) != 0) return err_into(out, cap, http, 500, "enable_failed");
    }
    if (per && !usr) {
        n = remoteplay_enable(1, raw, sizeof raw);
        if (n == -2) return err_into(out, cap, http, 409, "unsupported_fw");
        if (n == -3) return err_into(out, cap, http, 409, "no_user");
        if (n < 0 || (size_t)n >= sizeof raw) return err_into(out, cap, http, 500, "enable_failed");
    }
    n = app_api_rp_summary(raw, strlen(raw), out, cap);
    return n < 0 ? err_into(out, cap, http, 500, "enable_failed") : n;
}

/* A new PIN; the answer carries it (read without probing the registration, like rp.request). */
static int op_rp_pin_request(void *ctx, char *out, size_t cap, int *http) {
    char snap[192] = "", why[288] = "";
    int k;
    (void)ctx;
    if (remoteplay_request(NULL) != 0) {
        (void)remoteplay_last_error_json(why, sizeof why); /* already JSON-escaped */
        k = snprintf(out, cap, "{\"error\":\"pin_failed\",\"reason\":\"%s\"}", why);
        *http = 409;
        return k < 0 || (size_t)k >= cap ? -1 : k;
    }
    k = remoteplay_pin_snapshot(snap, sizeof snap);
    if (k <= 1 || snap[0] != '{') return err_into(out, cap, http, 500, "pin_unreadable");
    /* {"pin","account_id"} -> {"state":"waiting","pin","account_id"} */
    k = snprintf(out, cap, "{\"state\":\"waiting\",%s", snap + 1);
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

static int op_rp_pin_status(void *ctx, char *out, size_t cap) {
    int k;
    (void)ctx;
    if (remoteplay_get_status(out, cap) != 0) {
        k = snprintf(out, cap, "{\"state\":\"failed\"}");
        return k < 0 || (size_t)k >= cap ? -1 : k;
    }
    return (int)strlen(out);
}

static int op_rp_pin_cancel(void *ctx, char *out, size_t cap) {
    (void)ctx;
    (void)remoteplay_cancel();
    return snprintf(out, cap, "{\"ok\":true}");
}

static int op_mesh(void *ctx, char *out, size_t cap) {
    (void)ctx;
    return mesh_engines_json(out, cap);
}

/* PS5Upload Play's Tailscale forward: tailnet_node.c holds the one exception to "the PS5 never
 * dials out on the tailnet" and its rules. */
static int op_forward_status(void *ctx, char *out, size_t cap) {
    (void)ctx;
    return tailnet_node_forward_json(out, cap);
}

static int op_forward_open(void *ctx, uint32_t peer, unsigned port, char *out, size_t cap, int *http) {
    unsigned lport = 0;
    const char *err = "failed";
    char ip[16];
    int k;
    (void)ctx;
    if (tailnet_node_forward_open(peer, port, &lport, &err) != 0)
        return err_into(out, cap, http, strcmp(err, "failed") == 0 ? 500 : 409, err);
    snprintf(ip, sizeof ip, "%u.%u.%u.%u", peer >> 24, (peer >> 16) & 255u, (peer >> 8) & 255u, peer & 255u);
    k = snprintf(out, cap, "{\"local_port\":%u,\"peer\":\"%s\",\"port\":%u}", lport, ip, port);
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

static int op_forward_close(void *ctx, char *out, size_t cap) {
    (void)ctx;
    tailnet_node_forward_close();
    return snprintf(out, cap, "{\"ok\":true}");
}

static const app_api_ops_t OPS = {
    NULL,           PS5UPLOAD2_VERSION, op_rp_readiness,   op_rp_enable,    op_rp_pin_request, op_rp_pin_status,
    op_rp_pin_cancel, op_mesh,          op_forward_status, op_forward_open, op_forward_close,
};

/* ---- the listener ---- */

static int listen_loopback(unsigned port) {
    struct sockaddr_in a;
    int fd = socket(AF_INET, SOCK_STREAM, 0), one = 1, fl;
    if (fd < 0) return -1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); /* never the wildcard address */
    if (bind(fd, (struct sockaddr *)&a, sizeof a) != 0 || listen(fd, 4) != 0) {
        close(fd);
        return -1;
    }
    fl = fcntl(fd, F_GETFL, 0);
    if (fl >= 0) (void)fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    return fd;
}

static void send_all(int fd, const char *p, size_t n, uint64_t deadline) {
    while (n > 0 && !stopping()) {
        struct pollfd pf;
        uint64_t now = mono_ms();
        ssize_t w;
        if (now >= deadline) return;
        pf.fd = fd;
        pf.events = POLLOUT;
        pf.revents = 0;
        if (poll(&pf, 1, (int)(deadline - now)) <= 0) return;
        w = send(fd, p, n, 0);
        if (w < 0) {
            if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) continue;
            return;
        }
        p += w;
        n -= (size_t)w;
    }
}

static void answer_raw(int fd, int status, const char *token) {
    char b[256];
    int k = snprintf(b, sizeof b,
                     "HTTP/1.1 %d %s\r\nContent-Type: application/json\r\nContent-Length: %u\r\nConnection: close\r\n"
                     "Cache-Control: no-store\r\n\r\n{\"error\":\"%s\"}",
                     status, status == 413 ? "Payload Too Large" : status == 503 ? "Service Unavailable" : "Bad Request",
                     (unsigned)(strlen(token) + 12), token);
    if (k > 0 && (size_t)k < sizeof b) send_all(fd, b, (size_t)k, mono_ms() + APP_API_SEND_MS);
}

/* One connection, start to end, within its deadlines. */
static void serve(int fd) {
    uint64_t deadline = mono_ms() + APP_API_CONN_MS;
    size_t got = 0, total = 0, resp;
    int ready = 0, fl;
#ifdef SO_NOSIGPIPE
    {
        int one = 1;
        (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
    }
#endif
    fl = fcntl(fd, F_GETFL, 0);
    if (fl >= 0) (void)fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    while (!ready && !stopping()) {
        struct pollfd pf;
        uint64_t now = mono_ms();
        ssize_t r;
        if (now >= deadline) return; /* silent or slow: closed unanswered */
        pf.fd = fd;
        pf.events = POLLIN;
        pf.revents = 0;
        if (poll(&pf, 1, (int)(deadline - now < APP_API_SLICE_MS ? deadline - now : APP_API_SLICE_MS)) <= 0) continue;
        r = recv(fd, g_req + got, APP_API_REQ_MAX - got, 0);
        if (r == 0) return;
        if (r < 0) {
            if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) continue;
            return;
        }
        got += (size_t)r;
        switch (app_api_request_ready(g_req, got, &total)) {
        case 1: ready = 1; break;
        case 0:
            if (got >= APP_API_REQ_MAX) {
                answer_raw(fd, 413, "too_large");
                return;
            }
            break;
        case -2: answer_raw(fd, 413, "too_large"); return;
        default: answer_raw(fd, 400, "bad_request"); return;
        }
    }
    if (!ready) return;
    /* Counted with the AVA1 workers' RPCs: the exit waits for this call and refuses new ones. */
    if (ava1_server_local_rpc_enter() != 0) {
        answer_raw(fd, 503, "stopping");
        return;
    }
    resp = app_api_handle(g_req, total, PLAY_APP_API_PORT, &OPS, g_resp, APP_API_RESP_MAX);
    ava1_server_local_rpc_leave();
    if (resp) send_all(fd, g_resp, resp, mono_ms() + APP_API_SEND_MS);
}

static void nap_ms(int ms) {
    while (ms > 0 && !stopping()) {
        (void)poll(NULL, 0, ms < APP_API_SLICE_MS ? ms : APP_API_SLICE_MS);
        ms -= APP_API_SLICE_MS;
    }
}

static void *app_api_main(void *arg) {
    int lfd = -1, logged_fail = 0;
    uint64_t err_since = 0, watch_at = mono_ms(), mono_prev = mono_ms();
    int64_t real_prev = real_ms();
    (void)arg;
    proc_name_set_self("ps5upload-appapi");
    while (!stopping()) {
        struct pollfd pf;
        int r;
        if (mono_ms() - watch_at >= APP_API_WATCH_MS) {
            uint64_t m = mono_ms();
            int64_t t = real_ms();
            watch_at = m;
            if (webui_watch_resumed(mono_prev, m, real_prev, t) && lfd >= 0) {
                fprintf(stderr, "[appapi] resumed (clock gap); rebinding 127.0.0.1:%u\n", (unsigned)PLAY_APP_API_PORT);
                close(lfd);
                lfd = -1;
            }
            mono_prev = m;
            real_prev = t;
        }
        if (lfd < 0) {
            lfd = listen_loopback(PLAY_APP_API_PORT);
            if (lfd < 0) {
                if (!logged_fail++) fprintf(stderr, "[appapi] bind 127.0.0.1:%u failed (errno %d); retrying\n",
                                            (unsigned)PLAY_APP_API_PORT, errno);
                nap_ms((int)APP_API_BIND_RETRY_MS);
                continue;
            }
            fprintf(stderr, "[appapi] listening on 127.0.0.1:%u\n", (unsigned)PLAY_APP_API_PORT);
            logged_fail = 0;
            err_since = 0;
        }
        pf.fd = lfd;
        pf.events = POLLIN;
        pf.revents = 0;
        r = poll(&pf, 1, APP_API_SLICE_MS);
        if (r == 0) {
            err_since = 0;
            continue;
        }
        if (r < 0 || (pf.revents & (POLLERR | POLLHUP | POLLNVAL))) {
            if (r < 0 && errno == EINTR) continue;
            if (!err_since) err_since = mono_ms();
            else if (webui_watch_broken(err_since, mono_ms())) {
                fprintf(stderr, "[appapi] listener failing; rebinding\n");
                close(lfd);
                lfd = -1;
                err_since = 0;
                continue;
            }
            nap_ms(50);
            continue;
        }
        r = accept(lfd, NULL, NULL);
        if (r < 0) {
            /* Never leave the loop on an accept error (errno 163 in rest once killed a helper). */
            int e = errno;
            if (e != EAGAIN && e != EWOULDBLOCK && e != EINTR && e != ECONNABORTED && e != EMFILE && e != ENFILE &&
                e != ENOBUFS && e != ENOMEM) {
                if (!err_since) err_since = mono_ms();
                else if (webui_watch_broken(err_since, mono_ms())) {
                    fprintf(stderr, "[appapi] accept failing (errno %d); rebinding\n", e);
                    close(lfd);
                    lfd = -1;
                    err_since = 0;
                    continue;
                }
            }
            nap_ms(50);
            continue;
        }
        err_since = 0;
        serve(r);
        close(r);
    }
    if (lfd >= 0) close(lfd);
    return NULL;
}

int app_api_start(void) {
    pthread_attr_t attr;
    int rc;
    if (g_started) return 0;
    g_req = malloc(APP_API_REQ_MAX + 1);
    g_resp = malloc(APP_API_RESP_MAX);
    if (!g_req || !g_resp) {
        free(g_req);
        free(g_resp);
        g_req = g_resp = NULL;
        return -1;
    }
    __atomic_store_n(&g_stop, 0, __ATOMIC_RELEASE);
    if (pthread_attr_init(&attr) != 0) return -1;
    /* Remote Play's entry points run here, sized like the management handlers they are. */
    (void)pthread_attr_setstacksize(&attr, AVA1_MGMT_STACK);
    rc = pthread_create(&g_thr, &attr, app_api_main, NULL);
    pthread_attr_destroy(&attr);
    if (rc != 0) return -1;
    g_started = 1;
    return 0;
}

void app_api_stop(void) {
    if (!g_started) return;
    /* The thread sees it within a slice and closes its listener. Never joined: a Remote Play call
     * in flight can take seconds, and the exit watchdog is what bounds that, not this. Its buffers
     * stay with it (the process is about to end). */
    __atomic_store_n(&g_stop, 1, __ATOMIC_RELEASE);
    pthread_detach(g_thr);
    g_started = 0;
}
