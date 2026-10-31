/* PS5Upload installer daemon: TCP :9115, JSON-lines protocol. Loaded onto the
 * console by the :9021 ELF loader as a companion image (never evicts the
 * helper). Self-escalates, bounded AppInstUtil init, then serves hello /
 * install / job / stop. Sony calls are serialized behind one mutex so hello
 * and job answer while an install is in flight. No third-party branding. */
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>
#include <stdint.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <time.h>
#include <pthread.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#include "protocol.h"
#include "pathsafe.h"
#include "jobs.h"
#include "loopback.h"
#include <sys/stat.h>
#include "sony.h"
#include "escalate.h"
#include "listen_guard.h"
#include "thread_name.h"
#include "authid.h"          /* ps5_detect_firmware_major */

#define INST_PORT     9115
#define INST_VERSION  "1.5.0"
#define BOOT_STEP_MS  500
#define BOOT_STEPS    50     /* 50 * 500ms = 25s */
/* A replaced daemon releases :9115 within a poll slice of its `stop`; the new
 * one retries the bind this long before deciding another copy owns the port. */
#define BIND_RETRY_MS 5000u
#define BIND_STEP_MS  250u
/* Connections served at once. Each holds a thread; an install holds one for
 * the whole Sony call, hello/job/stop for a few ms. Past this a connection is
 * answered "too_many_connections" and closed instead of growing threads
 * without bound. */
#define INST_MAX_CONNS 16

/* Sony toast helper. Every message starts "PS5Upload installer". */
typedef struct notify_request {
    char useless1[45];
    char message[3075];
} notify_request_t;
extern int sceKernelSendNotificationRequest(int, notify_request_t *, size_t, int);

static void notify(const char *fmt, ...) {
    notify_request_t req;
    va_list ap;
    bzero(&req, sizeof req);
    va_start(ap, fmt);
    vsnprintf(req.message, sizeof req.message, fmt, ap);
    va_end(ap);
    sceKernelSendNotificationRequest(0, &req, sizeof req, 0);
}

typedef struct {
    pthread_mutex_t  sony_lock;   /* serializes InstallByPackage */
    pthread_mutex_t  state_lock;  /* active job + ring + seq */
    inst_sony_state_t sony;
    inst_job_phase_t active_phase;
    char             active_id[INST_JOBID_MAX];
    inst_loopback_t *active_lb;
    inst_job_ring_t  ring;
    uint32_t         seq;
    int              stopping;    /* stop accepted (under sony_lock): no new installs */
    volatile int     stop;        /* main loop: leave and release :9115 */
    volatile int     conns;       /* live connection workers */
} daemon_t;

static daemon_t g_d;

static double uptime_seconds(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void make_job_id(char *out, size_t cap) {
    pthread_mutex_lock(&g_d.state_lock);
    uint32_t s = ++g_d.seq;
    pthread_mutex_unlock(&g_d.state_lock);
    snprintf(out, cap, "%ld-%u", (long)time(NULL), s);
}

/* ── dispatch: each returns a JSON object length in `out`, or -1. ─────── */

static int do_hello(char *out, size_t cap) {
    pthread_mutex_lock(&g_d.state_lock);
    int esc = g_d.sony.escalated;
    uint32_t irc = g_d.sony.init_rc;
    int done = g_d.sony.init_done;
    int fw = g_d.sony.fw_major;
    pthread_mutex_unlock(&g_d.state_lock);
    const char *state = done ? "ready" : "init_failed";
    char fwbuf[16];
    snprintf(fwbuf, sizeof(fwbuf), "%d", fw);
    return inst_reply_hello(out, cap, INST_VERSION, fwbuf, state, irc, esc);
}

static int do_job(const inst_request_t *req, char *out, size_t cap) {
    inst_job_phase_t ph; uint32_t code; uint64_t bs, tot;
    pthread_mutex_lock(&g_d.state_lock);
    /* if it's the active loopback job, refresh live bytes first */
    if (g_d.active_lb && strcmp(g_d.active_id, req->job) == 0) {
        bs = inst_loopback_bytes_served(g_d.active_lb);
        tot = inst_loopback_total(g_d.active_lb);
        inst_ring_put(&g_d.ring, req->job, g_d.active_phase, 0, bs, tot);
    }
    int found = inst_ring_get(&g_d.ring, req->job, &ph, &code, &bs, &tot);
    pthread_mutex_unlock(&g_d.state_lock);
    if (!found) return inst_reply_err_str(out, cap, "unknown_job");
    return inst_reply_job(out, cap, inst_job_phase_str(ph), bs, tot, code);
}

static int do_install(const inst_request_t *req, char *out, size_t cap) {
    if (req->src == INST_SRC_PATH && !inst_path_is_safe(req->path))
        return inst_reply_err_str(out, cap, "bad_path");

    /* ensure init (retry once on demand) */
    pthread_mutex_lock(&g_d.state_lock);
    int done = g_d.sony.init_done;
    pthread_mutex_unlock(&g_d.state_lock);
    if (!done) {
        int rc = inst_sony_init(&g_d.sony);
        pthread_mutex_lock(&g_d.state_lock);
        g_d.sony.init_done = (rc == 0);
        g_d.sony.init_rc = (uint32_t)rc;
        done = (rc == 0);
        pthread_mutex_unlock(&g_d.state_lock);
        if (!done)
            return inst_reply_err_not_ready(out, cap, (uint32_t)rc);
    }

    /* Admission: only a serving loopback job blocks a new install. Take the
     * sony_lock FIRST, then decide admission under state_lock. The check must
     * be inside the sony_lock, not before it: active_phase is not set to
     * SERVING until an accepted loopback install finishes below, so a
     * pre-lock check would let a second install slip in while the first's
     * Sony call is still in flight — starting a second loopback (leaking the
     * first) and issuing a duplicate InstallByPackage. Serialising the check
     * behind sony_lock closes that window. */
    pthread_mutex_lock(&g_d.sony_lock);
    if (g_d.stopping) {
        /* A stop was accepted: this daemon is on its way out. */
        pthread_mutex_unlock(&g_d.sony_lock);
        return inst_reply_err_str(out, cap, "stopping");
    }
    pthread_mutex_lock(&g_d.state_lock);
    inst_job_phase_t active = g_d.active_phase;
    char busy_id[INST_JOBID_MAX];
    snprintf(busy_id, sizeof(busy_id), "%s", g_d.active_id);
    int active_complete = 0;
    if (g_d.active_lb && active == INST_JOB_SERVING) {
        uint64_t tot = inst_loopback_total(g_d.active_lb);
        active_complete = (tot == 0) || (inst_loopback_bytes_served(g_d.active_lb) >= tot);
    }
    if (!inst_admit_install(active, active_complete)) {
        pthread_mutex_unlock(&g_d.state_lock);
        pthread_mutex_unlock(&g_d.sony_lock);
        return inst_reply_err_busy(out, cap, busy_id);
    }
    /* Sony has read every byte of the previous loopback job: retire it now
     * (instead of after its idle window) so this install can start its own. */
    inst_loopback_t *retire = NULL;
    if (active == INST_JOB_SERVING && active_complete) {
        uint64_t tot = inst_loopback_total(g_d.active_lb);
        inst_ring_put(&g_d.ring, g_d.active_id, INST_JOB_DONE, 0, tot, tot);
        retire = g_d.active_lb;
        g_d.active_lb = NULL;
        g_d.active_phase = INST_JOB_DONE;
    }
    pthread_mutex_unlock(&g_d.state_lock);
    if (retire) inst_loopback_stop(retire);   /* join outside state_lock */

    char job_id[INST_JOBID_MAX];
    make_job_id(job_id, sizeof(job_id));

    inst_loopback_t *lb = NULL;
    inst_install_result_t res = inst_sony_install(req, &g_d.sony, &lb, job_id);
    pthread_mutex_lock(&g_d.state_lock);
    if (res.accepted) {
        if (lb) {                          /* live loopback serving job */
            g_d.active_lb = lb;
            g_d.active_phase = INST_JOB_SERVING;
            snprintf(g_d.active_id, sizeof(g_d.active_id), "%s", job_id);
            inst_ring_put(&g_d.ring, job_id, INST_JOB_SERVING, 0, 0,
                          inst_loopback_total(lb));
        } else {                           /* url or path: accepted, no serving */
            inst_ring_put(&g_d.ring, job_id, INST_JOB_ACCEPTED, 0, 0, 0);
        }
    } else {
        inst_ring_put(&g_d.ring, job_id, INST_JOB_FAILED, res.code, 0, 0);
    }
    pthread_mutex_unlock(&g_d.state_lock);
    pthread_mutex_unlock(&g_d.sony_lock);

    if (res.accepted)
        return inst_reply_install_ok(out, cap, job_id, res.via);
    return inst_reply_err_sony(out, cap, res.code, res.hint);
}

/* `stop`: refused as busy while an install call is in flight (the Sony lock
 * is held) or a loopback job is still being read; exiting then would fail
 * that install. The engine keeps the running daemon and upgrades later.
 * Returns the reply length; *accepted is set when the daemon will exit. */
static int do_stop(char *out, size_t cap, int *accepted) {
    *accepted = 0;
    int in_flight = (pthread_mutex_trylock(&g_d.sony_lock) != 0);
    pthread_mutex_lock(&g_d.state_lock);
    int complete = 1;
    if (g_d.active_lb && g_d.active_phase == INST_JOB_SERVING) {
        uint64_t tot = inst_loopback_total(g_d.active_lb);
        complete = (tot == 0) || (inst_loopback_bytes_served(g_d.active_lb) >= tot);
    }
    int allowed = inst_stop_allowed(in_flight, g_d.active_phase, complete);
    char busy_id[INST_JOBID_MAX];
    snprintf(busy_id, sizeof(busy_id), "%s", g_d.active_id);
    pthread_mutex_unlock(&g_d.state_lock);
    if (!in_flight) {
        if (allowed) g_d.stopping = 1;   /* under sony_lock: no install starts after this */
        pthread_mutex_unlock(&g_d.sony_lock);
    }
    if (!allowed) {
        fprintf(stderr, "[installer] stop refused: install in flight (job %s)\n",
                busy_id[0] ? busy_id : "-");
        return inst_reply_err_busy(out, cap, busy_id);
    }
    *accepted = 1;
    return inst_reply_ok(out, cap);
}

/* ── connection worker ─────────────────────────────────────────────────── */

static void *conn_worker(void *arg) {
    inst_thread_name(INST_THREAD_CONN);
    int fd = (int)(intptr_t)arg;
    struct timeval tv = { 5, 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
    int yes = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));

    char buf[INST_REQ_MAX + 1];
    size_t len = 0;
    while (len < INST_REQ_MAX) {
        ssize_t n = recv(fd, buf + len, INST_REQ_MAX - len, 0);
        if (n <= 0) break;
        len += (size_t)n;
        buf[len] = '\0';
        if (memchr(buf, '\n', len)) break;   /* one line per request */
    }
    /* trim at newline */
    char *nl = memchr(buf, '\n', len);
    if (nl) { *nl = '\0'; len = (size_t)(nl - buf); }

    inst_request_t req;
    inst_parse_request(buf, len, &req);

    char out[512];
    int olen = -1;
    int is_stop = 0;
    if (req.error) {
        olen = inst_reply_err_str(out, sizeof(out), req.error);
    } else switch (req.op) {
        case INST_OP_HELLO:   olen = do_hello(out, sizeof(out)); break;
        case INST_OP_INSTALL: olen = do_install(&req, out, sizeof(out)); break;
        case INST_OP_JOB:     olen = do_job(&req, out, sizeof(out)); break;
        case INST_OP_STOP:    olen = do_stop(out, sizeof(out), &is_stop); break;
        default:              olen = inst_reply_err_str(out, sizeof(out), "bad_request"); break;
    }
    if (olen > 0) {
        (void)send(fd, out, (size_t)olen, MSG_NOSIGNAL);
        (void)send(fd, "\n", 1, MSG_NOSIGNAL);
    }
    close(fd);
    /* After the reply is out: the main loop notices within a poll slice,
     * closes :9115 and exits, so a replacement can bind at once. */
    if (is_stop) {
        fprintf(stderr, "[installer] stop accepted; releasing :%d\n", INST_PORT);
        g_d.stop = 1;
    }
    inst_slot_release(&g_d.conns);
    return NULL;
}

/* ── monitor: retire a serving loopback job once fully covered + idle ──── */

static void *monitor(void *arg) {
    inst_thread_name(INST_THREAD_MONITOR);
    (void)arg;
    while (!g_d.stop) {
        inst_loopback_t *retire = NULL;
        char id[INST_JOBID_MAX];
        pthread_mutex_lock(&g_d.state_lock);
        if (g_d.active_lb && g_d.active_phase == INST_JOB_SERVING) {
            uint64_t bs = inst_loopback_bytes_served(g_d.active_lb);
            uint64_t tot = inst_loopback_total(g_d.active_lb);
            int complete = (tot == 0) || (bs >= tot);
            double idle = inst_loopback_idle_seconds(g_d.active_lb);
            inst_ring_put(&g_d.ring, g_d.active_id, INST_JOB_SERVING, 0, bs, tot);
            if (inst_job_should_finish(complete, idle)) {
                retire = g_d.active_lb;
                snprintf(id, sizeof(id), "%s", g_d.active_id);
                inst_ring_put(&g_d.ring, g_d.active_id, INST_JOB_DONE, 0, tot, tot);
                g_d.active_lb = NULL;
                g_d.active_phase = INST_JOB_DONE;
            }
        }
        pthread_mutex_unlock(&g_d.state_lock);
        if (retire) inst_loopback_stop(retire);   /* join outside the lock */
        struct timespec ts = { 5, 0 };
        nanosleep(&ts, NULL);
    }
    return NULL;
}

/* A non-blocking listener on :9115, or -1 (errno kept). Non-blocking so a
 * connection that resets between poll() and accept() cannot block the loop. */
static int open_listener(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    int reuse = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons(INST_PORT);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0 || listen(fd, 8) < 0) {
        int e = errno;
        close(fd);
        errno = e;
        return -1;
    }
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl >= 0) (void)fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    return fd;
}

/* Hand one accepted connection to a worker, or turn it away when full. */
static void dispatch(int c) {
    /* BSD hands the listener's O_NONBLOCK to the accepted socket; the worker
     * expects a blocking one bounded by its timeouts. */
    int fl = fcntl(c, F_GETFL, 0);
    if (fl >= 0) (void)fcntl(c, F_SETFL, fl & ~O_NONBLOCK);
    if (!inst_slot_take(&g_d.conns, INST_MAX_CONNS)) {
        static const char k_busy[] = "{\"ok\":false,\"error\":\"too_many_connections\"}\n";
        (void)send(c, k_busy, sizeof(k_busy) - 1, MSG_NOSIGNAL);
        close(c);
        return;
    }
    pthread_t t;
    if (pthread_create(&t, NULL, conn_worker, (void *)(intptr_t)c) == 0) {
        pthread_detach(t);
    } else {
        inst_slot_release(&g_d.conns);
        close(c);
    }
}

int main(void) {
    inst_thread_name(INST_THREAD_MAIN);
    /* Keep stderr somewhere readable: launched by a loader, it otherwise
     * goes to a socket that is already closed, and an install refusal left
     * no trace a bug report could collect. Line-buffered, appended. */
    mkdir("/data/ps5upload", 0777);
    if (freopen("/data/ps5upload/installer.log", "a", stderr) != NULL)
        setvbuf(stderr, NULL, _IOLBF, 0);
    fprintf(stderr, "=== ps5upload installer %s start ===\n", INST_VERSION);
    memset(&g_d, 0, sizeof(g_d));
    pthread_mutex_init(&g_d.sony_lock, NULL);
    pthread_mutex_init(&g_d.state_lock, NULL);
    inst_ring_reset(&g_d.ring);
    g_d.active_phase = INST_JOB_NONE;

    /* Bind :9115 first so a probe sees us while we boot-wait. A daemon being
     * replaced lets go of the port within a poll slice of its `stop`, so keep
     * trying for a few seconds before deciding another copy owns it. */
    int server_fd = -1;
    uint64_t bind_until = inst_mono_ms() + BIND_RETRY_MS;
    while ((server_fd = open_listener()) < 0 && inst_mono_ms() < bind_until)
        inst_nap_ms(BIND_STEP_MS);
    if (server_fd < 0) {
        /* another copy still owns the port — exit quietly */
        fprintf(stderr, "[installer] :%d still busy after %u ms (errno %d); another copy runs, exiting\n",
                INST_PORT, (unsigned)BIND_RETRY_MS, errno);
        return 0;
    }

    /* Boot wait only on a fresh boot (uptime <= 120s). */
    if (inst_should_boot_wait(uptime_seconds())) {
        struct timespec ts = { 0, BOOT_STEP_MS * 1000000L };
        for (int i = 0; i < BOOT_STEPS; i++) nanosleep(&ts, NULL);
    }

    g_d.sony.fw_major = ps5_detect_firmware_major();
    g_d.sony.escalated = (inst_escalate_self() == 0);
    if (!g_d.sony.escalated)
        notify("PS5Upload installer: escalation failed; install may not work");
    (void)inst_sony_init(&g_d.sony);

    pthread_t mon;
    pthread_create(&mon, NULL, monitor, NULL);
    pthread_detach(mon);

    notify("PS5Upload installer listening on port %d", INST_PORT);

    /* Never leave this loop except on `stop`, and never spin: an accept or
     * poll error backs off, and a listener failing for INST_LISTEN_BROKEN_MS
     * (rest mode can break it) is closed and bound again. An errno this loop
     * does not know (Sony's 163 once) is treated the same way. The poll slice
     * bounds how long an accepted `stop` takes to release the port. */
    uint64_t err_since = 0;
    while (!g_d.stop) {
        if (server_fd < 0) {
            server_fd = open_listener();
            if (server_fd < 0) {
                inst_nap_ms(500);
                continue;
            }
            fprintf(stderr, "[installer] listener rebuilt on :%d\n", INST_PORT);
            err_since = 0;
        }
        struct pollfd pf = { server_fd, POLLIN, 0 };
        int r = poll(&pf, 1, INST_LISTEN_SLICE_MS);
        if (r == 0) { err_since = 0; continue; }
        if (r < 0 || (pf.revents & (POLLERR | POLLHUP | POLLNVAL))) {
            int e = (r < 0) ? errno : 0;
            if (r < 0 && e == EINTR) continue;
            if (!err_since) err_since = inst_mono_ms();
            else if (inst_listen_broken(err_since, inst_mono_ms())) {
                fprintf(stderr, "[installer] listener failing for %u ms (errno %d); rebinding\n",
                        (unsigned)INST_LISTEN_BROKEN_MS, e);
                close(server_fd);
                server_fd = -1;
                continue;
            }
            inst_nap_ms(INST_LISTEN_BACKOFF_MS);
            continue;
        }
        int c = accept(server_fd, NULL, NULL);
        if (c < 0) {
            int e = errno;
            inst_accept_action_t act = inst_accept_classify(e);
            if (act == INST_ACCEPT_BROKEN) {
                if (!err_since) err_since = inst_mono_ms();
                else if (inst_listen_broken(err_since, inst_mono_ms())) {
                    fprintf(stderr, "[installer] accept failing for %u ms (errno %d); rebinding\n",
                            (unsigned)INST_LISTEN_BROKEN_MS, e);
                    close(server_fd);
                    server_fd = -1;
                    continue;
                }
            }
            /* Out of descriptors (BUSY) backs off too, but is not counted
             * toward a rebind: the listener is fine, the process is full. */
            if (act != INST_ACCEPT_RETRY) inst_nap_ms(INST_LISTEN_BACKOFF_MS);
            continue;
        }
        err_since = 0;
        dispatch(c);
    }
    if (server_fd >= 0) close(server_fd);
    fprintf(stderr, "[installer] stopped; :%d released\n", INST_PORT);
    notify("PS5Upload installer stopping");
    return 0;
}
