/* See include/vinput_relay.h.
 *
 * Locking: vinput.c calls the backend with its own lock held; every function here then takes g_mu
 * (vinput's lock -> g_mu, never the other way). The relay thread takes only g_mu, and drops it for
 * the slow start (the loader send, the companion's Init). The companion socket is used only under
 * g_mu, one request at a time, each answer matched by its seq (a late answer is skipped). */
#include "vinput_relay.h"

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
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "proc_identity.h"
#include "vinput_wire.h"

#define IO_ERR (-100)

enum { ST_IDLE, ST_STARTING, ST_RUNNING, ST_STOPPED, ST_UNAVAILABLE };
static const char *const ST_NAME[] = {"idle", "starting", "running", "stopped", "unavailable"};

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_cv = PTHREAD_COND_INITIALIZER;
static vrelay_cfg_t C;
static int g_st = ST_IDLE;
static const char *g_reason;
static int g_fd = -1, g_pid;
static uint8_t g_seq;
static uint8_t g_rbuf[4 * VWIRE_MAX_FRAME];
static size_t g_rhave;
static int g_cm, g_ck;         /* what the companion can make */
static int g_want_m, g_want_k; /* what the session has open (made again after a restart) */
static int g_m_open, g_k_open; /* what the companion has open */
static uint8_t g_buttons, g_mods, g_keys[VINPUT_KEYS];
static int g_fails;
static long long g_retry_at, g_last_reply, g_last_hb, g_running_since, g_unavail_at, g_asked_at = -100000;
static int g_thread_on, g_shutdown;
static pthread_t g_thread;

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void nap_ms(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

#define CFG(f, d) (C.f ? C.f : (d))

void vinput_relay_configure(const vrelay_cfg_t *cfg) {
    pthread_mutex_lock(&g_mu);
    if (cfg)
        C = *cfg;
    else
        memset(&C, 0, sizeof C);
    pthread_mutex_unlock(&g_mu);
}

/* ── the connection ─────────────────────────────────────────────────────────────────────────── */

/* A loopback connection: the fd, -2 when refused, -1 otherwise. */
static int dial(int port, int timeout_ms) {
    struct sockaddr_in a;
    struct pollfd p;
    int fd = socket(AF_INET, SOCK_STREAM, 0), fl, rc, soerr = 0;
    socklen_t sl = sizeof soerr;
    if (fd < 0) return -1;
#ifdef SO_NOSIGPIPE
    {
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
    }
#endif
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    rc = connect(fd, (struct sockaddr *)&a, sizeof a);
    if (rc != 0 && errno != EINPROGRESS) {
        int e = errno;
        close(fd);
        return e == ECONNREFUSED ? -2 : -1;
    }
    if (rc != 0) {
        p.fd = fd;
        p.events = POLLOUT;
        p.revents = 0;
        if (poll(&p, 1, timeout_ms) <= 0 || getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0 || soerr) {
            close(fd);
            return soerr == ECONNREFUSED ? -2 : -1;
        }
    }
    return fd; /* stays non-blocking */
}

static int send_frame(int fd, const uint8_t *b, size_t n, long long until) {
    while (n) {
        ssize_t w = send(fd, b, n, 0);
        if (w > 0) {
            b += w;
            n -= (size_t)w;
            continue;
        }
        if (w < 0 && (errno == EINTR)) continue;
        if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct pollfd p = {fd, POLLOUT, 0};
            long long left = until - now_ms();
            if (left <= 0 || poll(&p, 1, (int)left) <= 0) return -3;
            continue;
        }
        return -1;
    }
    return 0;
}

/* One request on fd and its answer (the same seq), within timeout_ms. The answer's rc with its
 * payload after the rc in out (*out_len bytes); -3 on timeout, IO_ERR when the connection broke. */
static int exchange(int fd, uint8_t *rbuf, size_t *rhave, uint8_t type, uint8_t seq, const uint8_t *payload,
                    size_t len, int timeout_ms, uint8_t *out, size_t *out_len) {
    uint8_t frame[VWIRE_MAX_FRAME];
    long long until = now_ms() + timeout_ms;
    int k = vwire_encode(frame, sizeof frame, type, seq, payload, len), s;
    if (k < 0) return IO_ERR;
    s = send_frame(fd, frame, (size_t)k, until);
    if (s == -3) return -3;
    if (s != 0) return IO_ERR;
    for (;;) {
        vwire_frame_t f;
        int used;
        while ((used = vwire_decode(rbuf, *rhave, &f)) > 0) {
            memmove(rbuf, rbuf + used, *rhave - (size_t)used);
            *rhave -= (size_t)used;
            if (f.seq != seq || f.type != (uint8_t)(type | VWIRE_REPLY) || f.len < 4) continue; /* a late one */
            if (out_len) {
                *out_len = f.len - 4u;
                if (out) memcpy(out, f.payload + 4, f.len - 4u);
            }
            return (int)(int32_t)vwire_get32(f.payload);
        }
        if (used < 0) return IO_ERR;
        {
            struct pollfd p = {fd, POLLIN, 0};
            long long left = until - now_ms();
            ssize_t r;
            if (left <= 0) return -3;
            if (poll(&p, 1, (int)left) <= 0) continue;
            r = recv(fd, rbuf + *rhave, 4 * VWIRE_MAX_FRAME - *rhave, 0);
            if (r < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
            if (r <= 0) return IO_ERR;
            *rhave += (size_t)r;
        }
    }
}

static void wake_thread_locked(void);

/* The companion is gone (or silent: killed). Lock held. */
static void lost_locked(int kill_it, const char *why) {
    long long now = now_ms();
    int wanted = g_want_m || g_want_k || now - g_asked_at < 5000;
    fprintf(stderr, "[vinput] companion pid %d %s\n", g_pid, why);
    if (kill_it && g_pid > 0 && C.kill) C.kill(C.ctx, g_pid);
    if (g_fd >= 0) close(g_fd);
    g_fd = -1;
    g_pid = 0;
    g_rhave = 0;
    g_m_open = g_k_open = 0;
    if (!wanted && !kill_it) {
        g_st = ST_IDLE; /* it went idle: started again when a session opens */
        return;
    }
    g_fails++;
    if (g_fails >= CFG(max_fails, 3)) {
        g_st = ST_UNAVAILABLE;
        g_reason = "vinput_companion_failed";
        g_unavail_at = now;
        return;
    }
    g_st = ST_STOPPED;
    g_retry_at = now + (long long)CFG(backoff_ms, 1000) * (1LL << (g_fails - 1));
    wake_thread_locked();
}

/* A request to the running companion. Lock held. The rc, or IO_ERR (then it is gone). */
static int call_locked(uint8_t type, const uint8_t *payload, size_t len, uint8_t *out, size_t *out_len) {
    int rc;
    if (g_st != ST_RUNNING || g_fd < 0) return IO_ERR;
    g_seq++;
    rc = exchange(g_fd, g_rbuf, &g_rhave, type, g_seq, payload, len, CFG(call_timeout_ms, 200), out, out_len);
    if (rc == -3) {
        if (now_ms() - g_last_reply >= CFG(silent_ms, 1000)) lost_locked(1, "is silent: killed");
        return IO_ERR;
    }
    if (rc == IO_ERR) {
        lost_locked(0, "closed the connection");
        return IO_ERR;
    }
    g_last_reply = now_ms();
    return rc;
}

/* OPEN with what is wanted now. Lock held. */
static int open_locked(int m, int k) {
    uint8_t p[2] = {(uint8_t)!!m, (uint8_t)!!k}, out[8];
    size_t n = 0;
    int rc = call_locked(VWIRE_OPEN, p, 2, out, &n);
    if (rc == IO_ERR) return VINPUT_E_STARTING;
    if (n >= 2) {
        g_m_open = out[0];
        g_k_open = out[1];
    }
    return rc == VWIRE_RC_OK ? VINPUT_OK : rc == VWIRE_RC_UNSUPPORTED ? VINPUT_E_UNSUPPORTED : VINPUT_E_FAILED;
}

/* ── starting it ────────────────────────────────────────────────────────────────────────────── */

static int write_token(const char *path, const uint8_t tok[VWIRE_TOKEN_LEN]) {
    char hex[VWIRE_TOKEN_HEX + 1], tmp[256];
    int fd, ok;
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    vwire_token_to_hex(tok, hex);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    (void)fchmod(fd, 0600);
    ok = write(fd, hex, VWIRE_TOKEN_HEX) == VWIRE_TOKEN_HEX;
    memset(hex, 0, sizeof hex);
    close(fd);
    if (!ok || rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

static int read_pid(const char *path) {
    char s[24];
    ssize_t n;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    n = read(fd, s, sizeof s - 1);
    close(fd);
    if (n <= 0) return 0;
    s[n] = '\0';
    return atoi(s);
}

typedef struct {
    int fd, pid, mouse, keyboard;
    uint8_t rbuf[4 * VWIRE_MAX_FRAME];
    size_t rhave;
} started_t;

/* Starts the companion and says hello (no lock held). 0 with *s, VRELAY_NO_LOADER, or -1. */
static int start_companion(started_t *s) {
    const char *token_path = C.token_path ? C.token_path : VWIRE_TOKEN_PATH;
    const char *pid_path = C.pid_path ? C.pid_path : VWIRE_PID_PATH;
    const int port = CFG(port, VWIRE_PORT);
    uint8_t tok[VWIRE_TOKEN_LEN], hello[2 + VWIRE_TOKEN_LEN], out[16];
    long long until;
    int rc, pid;
    memset(s, 0, sizeof *s);
    s->fd = -1;
    arc4random_buf(tok, sizeof tok);
    mkdir(VWIRE_DIR, 0777);
    unlink(pid_path);
    if (write_token(token_path, tok) != 0) return -1;
    rc = C.launch ? C.launch(C.ctx) : -1;
    if (rc != 0) {
        unlink(token_path);
        memset(tok, 0, sizeof tok);
        return rc == VRELAY_NO_LOADER ? VRELAY_NO_LOADER : -1;
    }
    vwire_put16(hello, VWIRE_VERSION);
    memcpy(hello + 2, tok, VWIRE_TOKEN_LEN);
    memset(tok, 0, sizeof tok);
    until = now_ms() + CFG(connect_wait_ms, 8000);
    while (now_ms() < until) {
        size_t n = 0;
        int fd = dial(port, 500);
        if (fd < 0) {
            nap_ms(100);
            continue;
        }
        s->rhave = 0;
        rc = exchange(fd, s->rbuf, &s->rhave, VWIRE_HELLO, 0, hello, sizeof hello, 1000, out, &n);
        if (rc == VWIRE_RC_OK && n >= 8) {
            s->fd = fd;
            s->pid = (int)vwire_get32(out + 2);
            s->mouse = out[6];
            s->keyboard = out[7];
            memset(hello, 0, sizeof hello);
            return 0;
        }
        close(fd); /* another instance (it refuses our token) on its way out, or not ready */
        nap_ms(100);
    }
    memset(hello, 0, sizeof hello);
    unlink(token_path);
    /* It started but never answered (a hung Init): end it. */
    pid = read_pid(pid_path);
    if (pid > 0 && C.kill) {
        fprintf(stderr, "[vinput] companion pid %d never answered: killed\n", pid);
        C.kill(C.ctx, pid);
    }
    return -1;
}

/* ── the relay thread: starts, keeps, and starts again ──────────────────────────────────────── */

static void *relay_main(void *arg) {
    (void)arg;
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    pthread_mutex_lock(&g_mu);
    while (!g_shutdown) {
        long long now = now_ms();
        int wanted = g_want_m || g_want_k || now - g_asked_at < 5000;
        if (g_st == ST_STARTING || (g_st == ST_STOPPED && wanted && now >= g_retry_at)) {
            started_t s;
            int rc;
            g_st = ST_STARTING;
            pthread_mutex_unlock(&g_mu);
            rc = start_companion(&s);
            pthread_mutex_lock(&g_mu);
            now = now_ms();
            if (g_shutdown) {
                if (rc == 0) close(s.fd);
                break;
            }
            if (rc == 0) {
                g_fd = s.fd;
                g_pid = s.pid;
                g_cm = s.mouse;
                g_ck = s.keyboard;
                memcpy(g_rbuf, s.rbuf, s.rhave);
                g_rhave = s.rhave;
                g_st = ST_RUNNING;
                g_reason = NULL;
                g_last_reply = g_last_hb = g_running_since = now;
                fprintf(stderr, "[vinput] companion pid %d up (mouse %d, keyboard %d)\n", g_pid, g_cm, g_ck);
                /* A restart: the devices the session had are made again, nothing held. */
                if (g_want_m || g_want_k) (void)open_locked(g_want_m, g_want_k);
                g_buttons = g_mods = 0;
                memset(g_keys, 0, sizeof g_keys);
            } else if (rc == VRELAY_NO_LOADER) {
                fprintf(stderr, "[vinput] no payload loader on :9021\n");
                g_st = ST_UNAVAILABLE;
                g_reason = "vinput_needs_loader";
                g_unavail_at = now;
            } else {
                g_fails++;
                fprintf(stderr, "[vinput] companion did not start (%d in a row)\n", g_fails);
                if (g_fails >= CFG(max_fails, 3)) {
                    g_st = ST_UNAVAILABLE;
                    g_reason = "vinput_companion_failed";
                    g_unavail_at = now;
                } else {
                    g_st = ST_STOPPED;
                    g_retry_at = now + (long long)CFG(backoff_ms, 1000) * (1LL << (g_fails - 1));
                }
            }
            continue;
        }
        if (g_st == ST_RUNNING) {
            if (now - g_last_hb >= CFG(hb_ms, 300)) {
                g_last_hb = now;
                (void)call_locked(VWIRE_HB, NULL, 0, NULL, NULL);
            }
            if (g_st == ST_RUNNING && g_fails && now - g_running_since >= CFG(healthy_ms, 30000)) g_fails = 0;
            /* It ended on its own (idle, or killed from outside)? */
            if (g_st == ST_RUNNING && g_fd >= 0) {
                struct pollfd p = {g_fd, POLLIN, 0};
                if (poll(&p, 1, 0) > 0) {
                    ssize_t r = recv(g_fd, g_rbuf + g_rhave, sizeof g_rbuf - g_rhave, 0);
                    if (r == 0 || (r < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR))
                        lost_locked(0, "ended");
                    else if (r > 0)
                        g_rhave += (size_t)r;
                    if (g_rhave == sizeof g_rbuf) g_rhave = 0; /* nothing unasked should come */
                }
            }
        } else if (!wanted && g_st != ST_STARTING) {
            break; /* nothing to do: started again with the next session */
        }
        {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += 50 * 1000000L;
            if (ts.tv_nsec >= 1000000000L) {
                ts.tv_sec++;
                ts.tv_nsec -= 1000000000L;
            }
            pthread_cond_timedwait(&g_cv, &g_mu, &ts);
        }
    }
    g_thread_on = 0;
    /* Ended on its own: nobody joins it (vinput_relay_reset joins one it asked to end). */
    if (!g_shutdown) pthread_detach(pthread_self());
    pthread_mutex_unlock(&g_mu);
    return NULL;
}

static void wake_thread_locked(void) {
    pthread_attr_t attr;
    if (g_thread_on) {
        pthread_cond_signal(&g_cv);
        return;
    }
    if (g_shutdown || pthread_attr_init(&attr) != 0) return;
    (void)pthread_attr_setstacksize(&attr, 128u * 1024u);
    if (pthread_create(&g_thread, &attr, relay_main, NULL) == 0) g_thread_on = 1;
    pthread_attr_destroy(&attr);
}

/* A session wants the companion. VINPUT_E_STARTING, or VINPUT_E_UNSUPPORTED while it cannot be.
 * Lock held. */
/* "unavailable" is asked again after a while: a loader may have been started since (or whatever
 * stopped the companion has passed). Lock held. */
static int unavailable_over_locked(long long now) {
    long long wait = strcmp(g_reason ? g_reason : "", "vinput_needs_loader") == 0 ? CFG(loader_retry_ms, 10000)
                                                                                 : CFG(healthy_ms, 30000);
    return now - g_unavail_at >= wait;
}

static int ask_start_locked(void) {
    long long now = now_ms();
    g_asked_at = now;
    if (g_st == ST_UNAVAILABLE) {
        if (!unavailable_over_locked(now)) return VINPUT_E_UNSUPPORTED;
        g_fails = 0;
        g_reason = NULL;
        g_st = ST_IDLE;
    }
    if (g_st == ST_IDLE) g_st = ST_STARTING;
    wake_thread_locked();
    return VINPUT_E_STARTING;
}

/* ── the backend ────────────────────────────────────────────────────────────────────────────── */

static void r_caps(int *mouse, int *keyboard) {
    pthread_mutex_lock(&g_mu);
    if (g_st == ST_RUNNING) {
        *mouse = g_cm;
        *keyboard = g_ck;
    } else if (g_st == ST_UNAVAILABLE && !unavailable_over_locked(now_ms())) {
        *mouse = *keyboard = 0;
    } else {
        *mouse = *keyboard = 1; /* not known until it runs: it can be started */
    }
    pthread_mutex_unlock(&g_mu);
}

static int r_open(int mouse) {
    int rc;
    pthread_mutex_lock(&g_mu);
    if (g_st != ST_RUNNING) {
        rc = ask_start_locked();
    } else {
        rc = open_locked(mouse ? 1 : g_want_m, mouse ? g_want_k : 1);
        if (mouse && g_m_open) {
            g_want_m = 1;
            rc = VINPUT_OK;
        } else if (!mouse && g_k_open) {
            g_want_k = 1;
            rc = VINPUT_OK;
        } else if (rc == VINPUT_OK) {
            rc = VINPUT_E_FAILED;
        }
    }
    pthread_mutex_unlock(&g_mu);
    return rc;
}

static void r_close(int mouse) {
    pthread_mutex_lock(&g_mu);
    if (mouse)
        g_want_m = 0;
    else
        g_want_k = 0;
    if (g_st == ST_RUNNING) (void)open_locked(g_want_m, g_want_k);
    if (mouse)
        g_buttons = 0;
    else {
        g_mods = 0;
        memset(g_keys, 0, sizeof g_keys);
    }
    pthread_mutex_unlock(&g_mu);
}

static int r_mouse_open(void) {
    return r_open(1);
}
static void r_mouse_close(void) {
    r_close(1);
}
static int r_keyboard_open(void) {
    return r_open(0);
}
static void r_keyboard_close(void) {
    r_close(0);
}

/* One report with the other device's state as last sent. Lock held. */
static int report_locked(uint8_t buttons, int dx, int dy, int wheel, int hwheel, uint8_t mods,
                         const uint8_t keys[VINPUT_KEYS]) {
    uint8_t r[VINPUT_REPORT_LEN];
    int rc;
    if (g_st != ST_RUNNING) return g_st == ST_UNAVAILABLE ? VINPUT_E_FAILED : VINPUT_E_STARTING;
    r[0] = buttons;
    r[1] = mods;
    vwire_put16(r + 2, (uint16_t)(int16_t)dx);
    vwire_put16(r + 4, (uint16_t)(int16_t)dy);
    r[6] = (uint8_t)(int8_t)wheel;
    r[7] = (uint8_t)(int8_t)hwheel;
    memcpy(r + 8, keys, VINPUT_KEYS);
    rc = call_locked(VWIRE_REPORT, r, sizeof r, NULL, NULL);
    if (rc == IO_ERR) return VINPUT_E_STARTING;
    return rc == VWIRE_RC_OK ? VINPUT_OK : VINPUT_E_FAILED;
}

static int r_mouse_send(uint8_t buttons, int dx, int dy, int wheel, int hwheel) {
    int rc;
    pthread_mutex_lock(&g_mu);
    rc = report_locked(buttons, dx, dy, wheel, hwheel, g_mods, g_keys);
    if (rc == VINPUT_OK) g_buttons = buttons;
    pthread_mutex_unlock(&g_mu);
    return rc;
}

static int r_keyboard_send(uint8_t mods, const uint8_t keys[VINPUT_KEYS]) {
    int rc;
    pthread_mutex_lock(&g_mu);
    rc = report_locked(g_buttons, 0, 0, 0, 0, mods, keys);
    if (rc == VINPUT_OK) {
        g_mods = mods;
        memcpy(g_keys, keys, VINPUT_KEYS);
    }
    pthread_mutex_unlock(&g_mu);
    return rc;
}

static const char *r_status(const char **reason) {
    const char *s;
    pthread_mutex_lock(&g_mu);
    s = ST_NAME[g_st];
    *reason = g_st == ST_UNAVAILABLE ? g_reason : NULL;
    pthread_mutex_unlock(&g_mu);
    return s;
}

static const vinput_backend_t RELAY = {
    "relay", r_caps, r_mouse_open, r_mouse_close, r_mouse_send, r_keyboard_open, r_keyboard_close,
    r_keyboard_send, r_status,
};

const vinput_backend_t *vinput_relay_backend(void) {
    return &RELAY;
}

int vinput_relay_pid(void) {
    int pid;
    pthread_mutex_lock(&g_mu);
    pid = g_st == ST_RUNNING ? g_pid : 0;
    pthread_mutex_unlock(&g_mu);
    return pid;
}

void vinput_relay_reset(void) {
    int join;
    pthread_mutex_lock(&g_mu);
    g_shutdown = 1;
    join = g_thread_on;
    pthread_cond_broadcast(&g_cv);
    pthread_mutex_unlock(&g_mu);
    if (join) pthread_join(g_thread, NULL);
    pthread_mutex_lock(&g_mu);
    if (g_fd >= 0) close(g_fd);
    g_fd = -1;
    g_pid = 0;
    g_rhave = 0;
    g_st = ST_IDLE;
    g_reason = NULL;
    g_cm = g_ck = g_want_m = g_want_k = g_m_open = g_k_open = 0;
    g_buttons = g_mods = 0;
    memset(g_keys, 0, sizeof g_keys);
    g_fails = 0;
    g_retry_at = g_last_reply = g_last_hb = g_running_since = g_unavail_at = g_asked_at = 0;
    g_asked_at = -100000;
    g_thread_on = 0;
    g_shutdown = 0;
    pthread_mutex_unlock(&g_mu);
}
