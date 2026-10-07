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
#include "authid.h"          /* ps5_detect_firmware_major */

#define INST_PORT     9115
#define INST_VERSION  "1.3.9"
#define BOOT_STEP_MS  500
#define BOOT_STEPS    50     /* 50 * 500ms = 25s */

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
    volatile int     stop;
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

/* ── connection worker ─────────────────────────────────────────────────── */

static void *conn_worker(void *arg) {
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
        case INST_OP_STOP:    olen = inst_reply_ok(out, sizeof(out)); is_stop = 1; break;
        default:              olen = inst_reply_err_str(out, sizeof(out), "bad_request"); break;
    }
    if (olen > 0) {
        (void)send(fd, out, (size_t)olen, MSG_NOSIGNAL);
        (void)send(fd, "\n", 1, MSG_NOSIGNAL);
    }
    close(fd);
    if (is_stop) g_d.stop = 1;
    return NULL;
}

/* ── monitor: retire a serving loopback job once fully covered + idle ──── */

static void *monitor(void *arg) {
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

int main(void) {
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

    /* Bind :9115 first so a probe sees us while we boot-wait. */
    int server_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (server_fd < 0) { notify("PS5Upload installer: socket failed"); return -1; }
    int reuse = 1;
    setsockopt(server_fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons(INST_PORT);
    if (bind(server_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        /* another copy already owns the port — exit quietly */
        close(server_fd);
        return 0;
    }
    if (listen(server_fd, 8) < 0) { close(server_fd); return 0; }

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

    while (!g_d.stop) {
        socklen_t al = sizeof(addr);
        int c = accept(server_fd, (struct sockaddr *)&addr, &al);
        if (c < 0) continue;
        pthread_t t;
        if (pthread_create(&t, NULL, conn_worker, (void *)(intptr_t)c) == 0)
            pthread_detach(t);
        else
            close(c);
    }
    notify("PS5Upload installer stopping");
    close(server_fd);
    return 0;
}
