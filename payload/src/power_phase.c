/* The live power phase (power_phase.h): our standby request, a loopback socket probe and the wake
 * watchdog's clock readings decide it; this file pauses the tailnet node to match, keeps stderr.log
 * bounded, and writes one line per transition with both clocks and what the helper holds, each
 * flushed to disk so it survives the console going down. */
#include "power_phase.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "config.h"
#include "hw_info.h"
#include "kernel_rw_lock.h"
#include "sony_api_lock.h"
#include "tailnet_node.h"

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pp_state g_pp = {PP_NORMAL, PP_WHY_NONE, 0, 0, PP_OK_PROBES, 0, 0};
static atomic_int g_phase = PP_NORMAL;

/* Probe failures are logged once per episode, again when the errno or step changes, and at most
 * once a minute otherwise. Under g_mu. */
static int g_fail_logged_err;
static const char *g_fail_logged_step;
static int64_t g_fail_logged_s;
static uint32_t g_fail_count;

/* stderr.log beyond this is rotated at runtime (startup rotates at 512 KiB). Larger while the
 * console is in a transition, so the lines that matter are not split across files. */
#define LOG_ROTATE_BYTES (8L * 1024 * 1024)
#define LOG_ROTATE_QUIET_BYTES (32L * 1024 * 1024)
/* Every transition line, also on its own (pp_log). */
#define POWER_LOG_PATH PS5UPLOAD2_RUNTIME_ROOT "/power.log"
#define POWER_LOG_MAX (256L * 1024)

static int64_t mono_s(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (int64_t)ts.tv_sec;
}

static int64_t real_s(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) return (int64_t)time(NULL);
    return (int64_t)ts.tv_sec;
}

static const char *lock_state(pthread_mutex_t *m) {
    if (pthread_mutex_trylock(m) != 0) return "held";
    pthread_mutex_unlock(m);
    return "free";
}

/* One line, one write(2) (no interleaving with other threads), then fsync when it marks a
 * transition: the page cache does not survive the console crashing or losing power. */
/* One whole write, retried a few times 100 ms apart: around a rest or wake a write can fail for a
 * moment (a "settled" line once went missing from both logs). A partial write is kept as it is. */
static void write_retry(int fd, const char *buf, size_t len) {
    for (int tries = 0; tries < 5; tries++) {
        ssize_t w = write(fd, buf, len);
        if (w >= 0) return;
        if (errno != EINTR) usleep(100 * 1000);
    }
}

static void pp_log(int durable, const char *fmt, ...) {
    char line[640];
    struct tm tm;
    time_t now = (time_t)real_s();
    int n = 0;
    if (gmtime_r(&now, &tm)) n = (int)strftime(line, sizeof line, "[power] %Y-%m-%dT%H:%M:%SZ ", &tm);
    if (n <= 0) n = snprintf(line, sizeof line, "[power] %lld ", (long long)now);
    va_list ap;
    va_start(ap, fmt);
    int m = vsnprintf(line + n, sizeof line - (size_t)n - 1, fmt, ap);
    va_end(ap);
    if (m < 0) m = 0;
    size_t len = (size_t)n + (size_t)m;
    if (len > sizeof line - 2) len = sizeof line - 2;
    line[len++] = '\n';
    write_retry(STDERR_FILENO, line, len);
    if (!durable) return;
    (void)fsync(STDERR_FILENO);
    /* The transitions alone, also in their own small file: stderr.log keeps one .old generation, so
     * two helper starts after a crash can rotate the evidence away. Bounded (POWER_LOG_MAX, one .old). */
    struct stat st;
    if (stat(POWER_LOG_PATH, &st) == 0 && st.st_size > POWER_LOG_MAX) (void)rename(POWER_LOG_PATH, POWER_LOG_PATH ".old");
    int fd = open(POWER_LOG_PATH, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) return;
    /* "[power] <time> pid=<n> <rest>" in ONE write: three writes once left a line without its time. */
    char out[sizeof line + 24];
    size_t head = (size_t)n <= len ? (size_t)n : len;
    int on = snprintf(out, sizeof out, "%.*spid=%d %.*s", (int)head, line, (int)getpid(), (int)(len - head), line + head);
    if (on > 0) write_retry(fd, out, (size_t)on < sizeof out ? (size_t)on : sizeof out - 1);
    (void)fsync(fd);
    close(fd);
}

/* What the helper holds right now, for the transition lines. Never blocks. */
static void snapshot(char *out, size_t cap) {
    struct stat st;
    long log_kib = fstat(STDERR_FILENO, &st) == 0 ? (long)(st.st_size / 1024) : -1;
    snprintf(out, cap, "tailnet=%s fan_pin=%d sony_api_lock=%s kernel_rw_lock=%s log_kib=%ld downs=%u wakes=%u",
             tailnet_node_brief(), hw_fan_pinned_threshold(), lock_state(&sony_api_lock), lock_state(&kernel_rw_lock),
             log_kib, (unsigned)g_pp.downs, (unsigned)g_pp.wakes);
}

/* A loopback UDP socket that sends one byte to itself: no traffic leaves the console, and while the
 * console goes to rest these calls fail the way every socket call does (errno 163). 0, or -1 with
 * the errno and the step that failed. */
static int net_probe(int *err, const char **step) {
    struct sockaddr_in a;
    socklen_t alen = sizeof a;
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s < 0) {
        *err = errno;
        *step = "socket";
        return -1;
    }
    /* Never wait here: as the console goes to rest a blocking send can sit until the wake. */
    int fl = fcntl(s, F_GETFL, 0);
    if (fl >= 0) (void)fcntl(s, F_SETFL, fl | O_NONBLOCK);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int rc = -1;
    char b = 'p';
    if (bind(s, (struct sockaddr *)&a, sizeof a) != 0) {
        *step = "bind";
    } else if (getsockname(s, (struct sockaddr *)&a, &alen) != 0) {
        *step = "getsockname";
    } else if (sendto(s, &b, 1, MSG_DONTWAIT, (struct sockaddr *)&a, sizeof a) != 1) {
        *step = "sendto";
    } else {
        rc = 0;
    }
    if (rc != 0) *err = errno;
    close(s);
    return rc;
}

/* Keep stderr.log bounded while the helper runs: rename it to .old and reopen (same directory, so
 * the rename never crosses a device). Only when fd 2 really is that file. */
static void log_size_guard(int quiet) {
    struct stat st, cur;
    const char *path = PS5UPLOAD2_RUNTIME_ROOT "/stderr.log";
    if (fstat(STDERR_FILENO, &st) != 0 || !S_ISREG(st.st_mode)) return;
    if (st.st_size < (quiet ? LOG_ROTATE_QUIET_BYTES : LOG_ROTATE_BYTES)) return;
    if (stat(path, &cur) != 0 || cur.st_ino != st.st_ino || cur.st_dev != st.st_dev) return;
    if (rename(path, PS5UPLOAD2_RUNTIME_ROOT "/stderr.log.old") != 0) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) return;
    dup2(fd, STDOUT_FILENO);
    dup2(fd, STDERR_FILENO);
    if (fd != STDOUT_FILENO && fd != STDERR_FILENO) close(fd);
    pp_log(0, "stderr.log reached %lld KiB: the earlier part is stderr.log.old", (long long)(st.st_size / 1024));
}

/* The tailnet node follows the phase; a node busy starting or stopping is asked again next poll.
 * Caller holds g_mu. */
static void apply_tailnet_locked(void) {
    (void)tailnet_node_set_paused(!pp_net_usable(&g_pp));
}

static void log_transition_locked(const char *what, int64_t real_elapsed, int64_t mono_elapsed) {
    char snap[256];
    snapshot(snap, sizeof snap);
    pp_log(1, "%s: phase=%s why=%s mono=%llds clocks_moved real=%+llds mono=%+llds | %s", what,
           pp_phase_name(g_pp.phase), pp_why_name(g_pp.why), (long long)mono_s(), (long long)real_elapsed,
           (long long)mono_elapsed, snap);
}

void power_phase_note_standby(void) {
    pthread_mutex_lock(&g_mu);
    pp_on_standby(&g_pp, mono_s());
    atomic_store(&g_phase, g_pp.phase);
    apply_tailnet_locked();
    log_transition_locked("standby requested by this helper", 0, 0);
    pthread_mutex_unlock(&g_mu);
}

int power_phase_poll(int64_t real_elapsed_s, int64_t mono_elapsed_s) {
    int jumped = pp_clocks_say_frozen(real_elapsed_s, mono_elapsed_s);
    int err = 0;
    const char *step = "";
    /* Outside the lock: the standby path must never wait on a socket call. */
    int ok = net_probe(&err, &step) == 0;
    int64_t now = mono_s();

    pthread_mutex_lock(&g_mu);
    int before = g_pp.phase;
    if (jumped) {
        pp_on_wake(&g_pp, now);
        g_fail_logged_step = NULL;
        g_fail_count = 0;
    }
    int changed = pp_on_probe(&g_pp, ok, err, now);
    atomic_store(&g_phase, g_pp.phase);
    /* Pause first: a slow fsync below must not delay taking the tailnet's sockets down. */
    apply_tailnet_locked();

    if (!ok) {
        g_fail_count++;
        if (!g_fail_logged_step || err != g_fail_logged_err || strcmp(step, g_fail_logged_step) != 0 ||
            now - g_fail_logged_s >= 60) {
            pp_log(g_fail_logged_step == NULL, "loopback probe failed at %s: errno %d (0x%x)%s, %u in a row", step, err,
                   (unsigned)err, err == PP_ERRNO_SONY_REST ? " = the PS5's going-to-rest errno" : "",
                   (unsigned)g_fail_count);
            g_fail_logged_err = err;
            g_fail_logged_step = step;
            g_fail_logged_s = now;
        }
    } else if (g_fail_logged_step) {
        pp_log(0, "loopback probe healthy again after %u failures", (unsigned)g_fail_count);
        g_fail_logged_step = NULL;
        g_fail_count = 0;
    }

    if (jumped) {
        log_transition_locked("woke (process was frozen)", real_elapsed_s, mono_elapsed_s);
    } else if (changed || before != g_pp.phase) {
        log_transition_locked(g_pp.phase == PP_GOING_DOWN ? "going to rest?" : "settled", real_elapsed_s, mono_elapsed_s);
    }
    int phase = g_pp.phase;
    pthread_mutex_unlock(&g_mu);

    log_size_guard(phase != PP_NORMAL);
    return phase;
}

int power_phase_quiet(void) { return atomic_load(&g_phase) != PP_NORMAL; }

int power_phase_current(void) { return atomic_load(&g_phase); }
