/* See include/vinput_companion.h. */
#include "vinput_companion.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* An unauthenticated connection gets this long to present the token. */
#define HELLO_WAIT_MS 2000LL
#define TICK_MS 1000LL
#define STEP_MS 50

typedef struct {
    const vcomp_cfg_t *cfg;
    int m_open, k_open; /* the helper has it open */
    /* Open, but the backend could not make it again (a new foreground user and AddDevice failed):
     * asked again every tick, and on the next OPEN, until it is back. */
    int m_lost, k_lost;
    uint8_t buttons, mods, keys[VINPUT_KEYS];
} vcomp_t;

/* There for input: open and not lost. */
#define M_UP(c) ((c)->m_open && !(c)->m_lost)
#define K_UP(c) ((c)->k_open && !(c)->k_lost)

static long long vc_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void vc_log(const vcomp_t *c, const char *what, int a, int b) {
    if (c->cfg->log) c->cfg->log("%s %d %d", what, a, b);
}

static const uint8_t NO_KEYS[VINPUT_KEYS];

static void forget_held(vcomp_t *c) {
    c->buttons = c->mods = 0;
    memset(c->keys, 0, sizeof c->keys);
}

static void mouse_release(vcomp_t *c) {
    if (!c->m_open) return;
    (void)c->cfg->dev->mouse_send(0, 0, 0, 0, 0);
    c->cfg->dev->mouse_close();
    c->m_open = c->m_lost = 0;
    c->buttons = 0;
}

static void keyboard_release(vcomp_t *c) {
    if (!c->k_open) return;
    (void)c->cfg->dev->keyboard_send(0, NO_KEYS);
    c->cfg->dev->keyboard_close();
    c->k_open = c->k_lost = 0;
    c->mods = 0;
    memset(c->keys, 0, sizeof c->keys);
}

static void release_all(vcomp_t *c, const char *why) {
    if (!c->m_open && !c->k_open) return;
    mouse_release(c);
    keyboard_release(c);
    vc_log(c, why, 0, 0);
}

/* Makes or lets go of one device. The rc. */
static int device(int want, int *is_open, int *lost, int (*open_fn)(void), void (*release)(vcomp_t *),
                  vcomp_t *c) {
    int rc;
    if (!want) {
        release(c);
        return VWIRE_RC_OK;
    }
    if (*is_open && !*lost) return VWIRE_RC_OK;
    rc = open_fn();
    if (rc == VINPUT_OK) {
        *is_open = 1;
        *lost = 0;
    }
    return rc == VINPUT_OK               ? VWIRE_RC_OK
           : rc == VINPUT_E_UNSUPPORTED ? VWIRE_RC_UNSUPPORTED
           : rc == VINPUT_E_NO_USER     ? VWIRE_RC_NO_USER
                                        : VWIRE_RC_FAILED;
}

/* After the backend made its devices again (or failed to): an open device it could not make is
 * lost, and asked for again on each tick until it is back. */
static void remake(vcomp_t *c, int *is_open, int *lost, int (*open_fn)(void), const char *what) {
    if (!*is_open) return;
    *lost = open_fn() != VINPUT_OK;
    if (*lost) vc_log(c, what, 0, 0);
}

static int apply(vcomp_t *c, const uint8_t raw[VINPUT_REPORT_LEN]) {
    int err = VWIRE_RC_OK;
    uint8_t buttons = raw[0], mods = raw[1];
    int dx = (int16_t)vwire_get16(raw + 2), dy = (int16_t)vwire_get16(raw + 4);
    int wheel = (int8_t)raw[6], hwheel = (int8_t)raw[7];
    const uint8_t *keys = raw + 8;
    if (!M_UP(c) && !K_UP(c)) return VWIRE_RC_FAILED;
    if (M_UP(c) && (buttons != c->buttons || dx || dy || wheel || hwheel)) {
        if (c->cfg->dev->mouse_send(buttons, dx, dy, wheel, hwheel) == VINPUT_OK)
            c->buttons = buttons;
        else
            err = VWIRE_RC_FAILED;
    }
    if (K_UP(c) && (mods != c->mods || memcmp(keys, c->keys, VINPUT_KEYS) != 0)) {
        if (c->cfg->dev->keyboard_send(mods, keys) == VINPUT_OK) {
            c->mods = mods;
            memcpy(c->keys, keys, VINPUT_KEYS);
        } else {
            err = VWIRE_RC_FAILED;
        }
    }
    return err;
}

static int send_all(int fd, const uint8_t *b, size_t n) {
    while (n) {
        ssize_t w = send(fd, b, n, 0);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        b += w;
        n -= (size_t)w;
    }
    return 0;
}

static int reply(int fd, const vwire_frame_t *req, int rc, const uint8_t *extra, size_t n) {
    uint8_t p[VWIRE_MAX_PAYLOAD], out[VWIRE_MAX_FRAME];
    int k;
    vwire_put32(p, (uint32_t)rc);
    if (n) memcpy(p + 4, extra, n);
    k = vwire_encode(out, sizeof out, (uint8_t)(req->type | VWIRE_REPLY), req->seq, p, 4 + n);
    return k < 0 ? -1 : send_all(fd, out, (size_t)k);
}

static int read_token(const char *path, uint8_t tok[VWIRE_TOKEN_LEN]) {
    char buf[VWIRE_TOKEN_HEX + 16];
    ssize_t n;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    n = read(fd, buf, sizeof buf);
    close(fd);
    unlink(path);
    if (n < VWIRE_TOKEN_HEX) return -1;
    n = vwire_token_from_hex(buf, (size_t)n, tok);
    memset(buf, 0, sizeof buf);
    return (int)n;
}

/* The pid in the file, or 0. */
static long pid_in(const char *path) {
    char s[24];
    ssize_t n;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    n = read(fd, s, sizeof s - 1);
    close(fd);
    if (n <= 0) return 0;
    s[n] = '\0';
    return strtol(s, NULL, 10);
}

/* Removes the pid file if it is still ours (a later companion may have written its own). */
static void unlink_pid(const char *path) {
    if (path[0] && pid_in(path) == (long)getpid()) unlink(path);
}

static void write_pid(const char *path) {
    char s[24];
    int k, fd;
    if (!path[0]) return;
    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return;
    k = snprintf(s, sizeof s, "%ld\n", (long)getpid());
    if (write(fd, s, (size_t)k) != k) { /* best effort */
    }
    close(fd);
}

static int bind_loopback(int port) {
    struct sockaddr_in a;
    int one = 1, fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(fd, (struct sockaddr *)&a, sizeof a) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

/* One request from the authenticated helper. -1 when the connection should go. */
static int handle(vcomp_t *c, int fd, const vwire_frame_t *f) {
    uint8_t x[8];
    int mouse = 0, keyboard = 0;
    switch (f->type) {
    case VWIRE_HELLO: /* again: answered like the first */
        c->cfg->dev->caps(&mouse, &keyboard);
        vwire_put16(x, VWIRE_VERSION);
        vwire_put32(x + 2, (uint32_t)getpid());
        x[6] = (uint8_t)!!mouse;
        x[7] = (uint8_t)!!keyboard;
        return reply(fd, f, VWIRE_RC_OK, x, 8);
    case VWIRE_CAPS:
        c->cfg->dev->caps(&mouse, &keyboard);
        x[0] = (uint8_t)!!mouse;
        x[1] = (uint8_t)!!keyboard;
        x[2] = (uint8_t)M_UP(c);
        x[3] = (uint8_t)K_UP(c);
        return reply(fd, f, VWIRE_RC_OK, x, 4);
    case VWIRE_OPEN: {
        int rm, rk;
        if (f->len < 2) return reply(fd, f, VWIRE_RC_BAD_REQUEST, NULL, 0);
        rm = device(f->payload[0], &c->m_open, &c->m_lost, c->cfg->dev->mouse_open, mouse_release, c);
        rk = device(f->payload[1], &c->k_open, &c->k_lost, c->cfg->dev->keyboard_open, keyboard_release, c);
        x[0] = (uint8_t)M_UP(c);
        x[1] = (uint8_t)K_UP(c);
        vc_log(c, "open", x[0], x[1]);
        return reply(fd, f, rm ? rm : rk, x, 2);
    }
    case VWIRE_REPORT:
        if (f->len != VINPUT_REPORT_LEN) return reply(fd, f, VWIRE_RC_BAD_REQUEST, NULL, 0);
        return reply(fd, f, apply(c, f->payload), NULL, 0);
    case VWIRE_HB:
        return reply(fd, f, VWIRE_RC_OK, NULL, 0);
    case VWIRE_CLOSE:
        release_all(c, "close");
        return reply(fd, f, VWIRE_RC_OK, NULL, 0);
    default:
        return reply(fd, f, VWIRE_RC_BAD_REQUEST, NULL, 0);
    }
}

int vcomp_run(const vcomp_cfg_t *cfg) {
    vcomp_t c;
    uint8_t tok[VWIRE_TOKEN_LEN], buf[4 * VWIRE_MAX_FRAME];
    size_t have = 0;
    int lfd, cfd = -1, authed = 0, ever_authed = 0, code = VCOMP_EXIT_DONE;
    const int port = cfg->port ? cfg->port : VWIRE_PORT;
    const long long hb_ms = cfg->hb_timeout_ms ? cfg->hb_timeout_ms : VWIRE_HB_TIMEOUT_MS;
    const long long idle_ms = cfg->idle_exit_ms ? cfg->idle_exit_ms : VWIRE_IDLE_EXIT_MS;
    const long long auth_ms = cfg->auth_wait_ms ? cfg->auth_wait_ms : VWIRE_AUTH_WAIT_MS;
    const char *pid_path = cfg->pid_path ? cfg->pid_path : VWIRE_PID_PATH;
    long long start, last_rx = 0, conn_at = 0, idle_since, next_tick;

    memset(&c, 0, sizeof c);
    c.cfg = cfg;
    if (read_token(cfg->token_path ? cfg->token_path : VWIRE_TOKEN_PATH, tok) != 0) {
        vc_log(&c, "no token", 0, 0);
        return VCOMP_EXIT_NO_TOKEN;
    }
    lfd = bind_loopback(port);
    if (lfd < 0) {
        vc_log(&c, "port busy", port, errno);
        return VCOMP_EXIT_BUSY;
    }
    /* Written once the port is ours (bound) and before Init, so a helper can end an Init that
     * hangs; not left behind when listen fails. */
    write_pid(pid_path);
    if (cfg->dev_init) cfg->dev_init();
    if (listen(lfd, 4) != 0) {
        close(lfd);
        unlink_pid(pid_path);
        return VCOMP_EXIT_BUSY;
    }
    start = idle_since = vc_now_ms();
    next_tick = start + TICK_MS;
    vc_log(&c, "listening", port, (int)getpid());

    for (;;) {
        struct pollfd p[2];
        int np = 1;
        long long now;
        if (cfg->stop && __atomic_load_n(cfg->stop, __ATOMIC_ACQUIRE)) break;
        if (cfg->hang && __atomic_load_n(cfg->hang, __ATOMIC_ACQUIRE)) {
            usleep(STEP_MS * 1000);
            continue;
        }
        p[0].fd = lfd;
        p[0].events = POLLIN;
        p[0].revents = 0;
        if (cfd >= 0) {
            p[1].fd = cfd;
            p[1].events = POLLIN;
            p[1].revents = 0;
            np = 2;
        }
        if (poll(p, (nfds_t)np, STEP_MS) < 0 && errno != EINTR) usleep(STEP_MS * 1000);
        now = vc_now_ms();

        if (p[0].revents & POLLIN) {
            int nfd = accept(lfd, NULL, NULL);
            if (nfd >= 0) {
                if (cfd >= 0) {
                    close(nfd); /* one helper at a time */
                } else {
                    cfd = nfd;
                    conn_at = now;
                    authed = 0;
                    have = 0;
                }
            }
        }
        if (np == 2 && cfd >= 0 && (p[1].revents & (POLLIN | POLLHUP | POLLERR))) {
            ssize_t k = recv(cfd, buf + have, sizeof buf - have, 0);
            if (k <= 0 && !(k < 0 && errno == EINTR)) {
                close(cfd);
                cfd = -1;
                if (authed) {
                    release_all(&c, "helper left");
                    break;
                }
                continue;
            }
            if (k > 0) have += (size_t)k;
            for (;;) {
                vwire_frame_t f;
                int used = vwire_decode(buf, have, &f), bad = 0;
                if (used == 0) break;
                if (used < 0) {
                    bad = 1;
                } else if (!authed) {
                    if (f.type != VWIRE_HELLO || f.len < 2 + VWIRE_TOKEN_LEN ||
                        !vwire_token_equal(f.payload + 2, tok)) {
                        (void)reply(cfd, &f, VWIRE_RC_BAD_TOKEN, NULL, 0);
                        bad = 1;
                    } else {
                        authed = ever_authed = 1;
                        last_rx = now;
                        vc_log(&c, "helper connected", vwire_get16(f.payload), 0);
                        if (handle(&c, cfd, &f) != 0) bad = 1;
                    }
                } else {
                    last_rx = now;
                    if (handle(&c, cfd, &f) != 0) bad = 1;
                }
                if (bad) {
                    close(cfd);
                    cfd = -1;
                    have = 0;
                    if (authed) {
                        release_all(&c, "helper connection broken");
                        code = VCOMP_EXIT_DONE;
                        goto out;
                    }
                    break;
                }
                memmove(buf, buf + used, have - (size_t)used);
                have -= (size_t)used;
            }
        }
        if (cfd >= 0 && !authed && now - conn_at > HELLO_WAIT_MS) {
            close(cfd);
            cfd = -1;
        }
        if (authed && (c.m_open || c.k_open) && now - last_rx > hb_ms) release_all(&c, "heartbeat timeout");
        if (c.m_open || c.k_open) idle_since = now;
        if (!ever_authed && now - start > auth_ms) {
            vc_log(&c, "nobody came", 0, 0);
            break;
        }
        if (!c.m_open && !c.k_open && now - idle_since > idle_ms) {
            vc_log(&c, "idle", 0, 0);
            break;
        }
        if (now >= next_tick) {
            next_tick = now + TICK_MS;
            if (cfg->dev_tick && cfg->dev_tick() == 1) {
                forget_held(&c);
                c.m_lost = c.k_lost = 1; /* checked below: made again, or not */
            }
            if (c.m_lost) remake(&c, &c.m_open, &c.m_lost, cfg->dev->mouse_open, "mouse not made again");
            if (c.k_lost) remake(&c, &c.k_open, &c.k_lost, cfg->dev->keyboard_open, "keyboard not made again");
        }
    }
out:
    release_all(&c, "exit");
    if (cfd >= 0) close(cfd);
    close(lfd);
    unlink_pid(pid_path);
    memset(tok, 0, sizeof tok);
    return code;
}
