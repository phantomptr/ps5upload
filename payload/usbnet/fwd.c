/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/forward.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md).
 *
 * See fwd.h. One thread accepts on every listener. Each connection has two more, one per
 * direction, so neither waits on the other; their reads and writes wake every TICK_MS to see
 * whether the connection or the run has ended. The far end is a kernel socket to 127.0.0.1.
 * PS5Upload: a fixed pair of rules instead of a saved list, no FTP rewriting, the PROXY line for
 * Minion, and the going-to-rest errno reported to the main loop. */
#include "fwd.h"
#include "fwd_lwip.h"
#include "usbn_log.h"
#include "usbnet_proto.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/time.h>

#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define TICK_MS 1000
#define WAIT_MS 200
#define BACKLOG 32
#define LINKS_MAX 64
#define BUFFER_BYTES (128 * 1024)
#define LINK_STACK (256 * 1024)
#define HALF_OPEN_TICKS 60 /* after the service closed its side, how long a client may stay silent */
#define STOP_WAIT_MS 4000
#define SONY_REST_ERRNO 163

struct rule {
    uint16_t port;
    int on, listening, failed, fd, minion;
    unsigned links;
    uint64_t bytes;
};

struct link {
    int outer, inner; /* lwIP's, the kernel's */
    int rule;
    uint32_t peer;    /* network order */
    uint16_t peer_port;
    int dead, refs, inner_done;
};

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static struct rule g_rules[FWD_RULES] = {
    {USBNET_AVA1_PORT, 1, 0, 0, -1, 0, 0, 0},
    {USBNET_MINION_PORT, 0, 0, 0, -1, 1, 0, 0},
};
static int g_running, g_stop, g_links;
static char g_address[16];
static pthread_t g_thread;
static void (*g_down_hook)(int err);

static long long now_ms(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

void fwd_set_down_hook(void (*hook)(int err))
{
    g_down_hook = hook;
}

static void note_errno(int code)
{
    if (code == SONY_REST_ERRNO && g_down_hook) g_down_hook(code);
}

void fwd_set_minion(int on)
{
    pthread_mutex_lock(&g_lock);
    g_rules[1].on = on != 0;
    pthread_mutex_unlock(&g_lock);
}

void fwd_get_status(struct fwd_status out[FWD_RULES])
{
    int i;

    pthread_mutex_lock(&g_lock);
    for (i = 0; i < FWD_RULES; i++) {
        out[i].port = g_rules[i].port;
        out[i].on = g_rules[i].on;
        out[i].listening = g_rules[i].listening;
        out[i].links = g_rules[i].links;
        out[i].bytes = g_rules[i].bytes;
    }
    pthread_mutex_unlock(&g_lock);
}

static void account(int rule, uint64_t bytes, int ended)
{
    pthread_mutex_lock(&g_lock);
    g_rules[rule].bytes += bytes;
    if (ended) {
        if (g_rules[rule].links) g_rules[rule].links--;
        g_links--;
    }
    pthread_mutex_unlock(&g_lock);
}

/* ---- one connection ------------------------------------------------------------ */

static void note_drop(uint16_t port, const char *dir, int code)
{
    static unsigned long count;
    unsigned long n = __atomic_add_fetch(&count, 1, __ATOMIC_RELAXED);

    if (n <= 8 || !(n & (n - 1))) say("port %u: %s failed, errno %d (%lu so far)", port, dir, code, n);
}

static int gone(struct link *l)
{
    return __atomic_load_n(&l->dead, __ATOMIC_RELAXED) || __atomic_load_n(&g_stop, __ATOMIC_RELAXED);
}

/* Both directions stop: the kernel side wakes at once, lwIP's within a tick. */
static void cut(struct link *l)
{
    __atomic_store_n(&l->dead, 1, __ATOMIC_RELAXED);
    shutdown(l->inner, SHUT_RDWR);
}

static void release(struct link *l)
{
    if (__atomic_sub_fetch(&l->refs, 1, __ATOMIC_ACQ_REL)) return;
    close(l->inner);
    fwd_lwip_close(l->outer, l->dead);
    account(l->rule, 0, 1);
    free(l);
}

static int outer_send(struct link *l, const char *p, size_t n)
{
    while (n) {
        long w = fwd_lwip_write(l->outer, p, n);

        if (w > 0) {
            p += w;
            n -= (size_t)w;
        } else if (w != FWD_LWIP_TIMEOUT || gone(l)) {
            return -1;
        }
    }
    return 0;
}

static int inner_send(struct link *l, const char *p, size_t n)
{
    while (n) {
        ssize_t w = write(l->inner, p, n);

        if (w > 0) {
            p += w;
            n -= (size_t)w;
        } else if (w < 0 && errno == ENOBUFS && !gone(l)) {
            usleep(10000);
        } else if (w == 0 || (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) || gone(l)) {
            if (w < 0) note_errno(errno);
            return -1;
        }
    }
    return 0;
}

static void *outward_main(void *opaque)
{
    struct link *l = opaque;
    char *buffer = malloc(BUFFER_BYTES);
    int idle = 0;
    uint16_t port = g_rules[l->rule].port;

    while (buffer) {
        long n = fwd_lwip_read(l->outer, buffer, BUFFER_BYTES);

        if (n > 0) {
            idle = 0;
            if (inner_send(l, buffer, (size_t)n) != 0) {
                if (!gone(l)) note_drop(port, "write to the console", errno);
                break;
            }
            account(l->rule, (uint64_t)n, 0);
            continue;
        }
        if (n == FWD_LWIP_TIMEOUT) {
            if (gone(l)) break;
            if (__atomic_load_n(&l->inner_done, __ATOMIC_ACQUIRE) && ++idle > HALF_OPEN_TICKS) break;
            continue;
        }
        if (n == 0 || (__atomic_load_n(&l->inner_done, __ATOMIC_ACQUIRE) && (errno == ECONNRESET || errno == ENOTCONN))) {
            shutdown(l->inner, SHUT_WR);
            free(buffer);
            release(l);
            return NULL;
        }
        if (!gone(l)) note_drop(port, "read from the network", errno);
        break;
    }
    free(buffer);
    cut(l);
    release(l);
    return NULL;
}

static int inner_connect(uint16_t port)
{
    struct sockaddr_in to;
    struct timeval timeout = {TICK_MS / 1000, (TICK_MS % 1000) * 1000};
    int fd = socket(AF_INET, SOCK_STREAM, 0), on = 1;

    if (fd < 0) {
        note_errno(errno);
        return -1;
    }
    memset(&to, 0, sizeof to);
    to.sin_family = AF_INET;
    to.sin_port = htons(port);
    to.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, (struct sockaddr *)&to, sizeof to) != 0) {
        int code = errno;
        close(fd);
        note_errno(code);
        errno = code;
        return -1;
    }
#ifdef SO_NOSIGPIPE
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
#endif
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on);
    return fd;
}

static int spawn(void *(*main)(void *), void *argument)
{
    pthread_attr_t attr;
    pthread_t thread;
    int result;

    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, LINK_STACK);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    result = pthread_create(&thread, &attr, main, argument);
    pthread_attr_destroy(&attr);
    return result;
}

/* Connects to the console's port, then carries what it sends back to the client. */
static void *inward_main(void *opaque)
{
    struct link *l = opaque;
    uint16_t port = g_rules[l->rule].port;
    char *buffer;

    l->inner = inner_connect(port);
    if (l->inner < 0) {
        note_drop(port, "connect to the console", errno);
        fwd_lwip_close(l->outer, 1); /* the client sees the refusal the service gave */
        account(l->rule, 0, 1);
        free(l);
        return NULL;
    }
    if (g_rules[l->rule].minion) {
        char line[USBNET_PROXY_LINE_MAX + 1];
        char address[16];
        size_t n;

        pthread_mutex_lock(&g_lock);
        memcpy(address, g_address, sizeof address);
        pthread_mutex_unlock(&g_lock);
        n = fwd_proxy_line(line, sizeof line, l->peer, l->peer_port, address, port);
        if (!n || inner_send(l, line, n) != 0) {
            close(l->inner);
            fwd_lwip_close(l->outer, 1);
            account(l->rule, 0, 1);
            free(l);
            return NULL;
        }
    }
    l->refs = 2;
    if (spawn(outward_main, l) != 0) {
        l->refs = 1;
        cut(l);
        release(l);
        return NULL;
    }
    buffer = malloc(BUFFER_BYTES);
    while (buffer) {
        ssize_t n = read(l->inner, buffer, BUFFER_BYTES);

        if (n > 0) {
            if (outer_send(l, buffer, (size_t)n) != 0) {
                if (!gone(l)) note_drop(port, "write to the network", errno);
                break;
            }
            account(l->rule, (uint64_t)n, 0);
            continue;
        }
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) {
            if (gone(l)) break;
            continue;
        }
        if (n == 0 && !gone(l)) {
            __atomic_store_n(&l->inner_done, 1, __ATOMIC_RELEASE);
            fwd_lwip_shutdown(l->outer);
            free(buffer);
            release(l);
            return NULL;
        }
        if (n < 0 && !gone(l)) {
            note_errno(errno);
            note_drop(port, "read from the console", errno);
        }
        break;
    }
    free(buffer);
    cut(l);
    release(l);
    return NULL;
}

/* ---- listening -------------------------------------------------------------------- */

/* With the lock: a listener for every rule that is on, and none for any other. */
static void reconcile(void)
{
    int i;

    for (i = 0; i < FWD_RULES; i++) {
        struct rule *r = &g_rules[i];

        if (!r->on || g_stop) {
            if (r->fd >= 0) fwd_lwip_close(r->fd, 0);
            r->fd = -1;
            r->listening = 0;
            r->failed = 0;
            continue;
        }
        if (r->fd >= 0) continue;
        r->fd = fwd_lwip_listen(r->port, BACKLOG);
        if (r->fd < 0) {
            if (!r->failed) say("port %u could not listen, errno %d", r->port, errno);
            r->failed = 1;
            continue;
        }
        r->listening = 1;
        r->failed = 0;
        say("port %u: listening on the adapter", r->port);
    }
}

static void take(int rule)
{
    struct link *k;
    uint32_t peer = 0;
    uint16_t peer_port = 0;
    int fd;

    pthread_mutex_lock(&g_lock);
    if (g_rules[rule].fd < 0 || (fd = fwd_lwip_accept(g_rules[rule].fd, &peer, &peer_port, TICK_MS)) < 0) {
        pthread_mutex_unlock(&g_lock);
        return;
    }
    k = g_links < LINKS_MAX ? calloc(1, sizeof *k) : NULL;
    if (!k) {
        pthread_mutex_unlock(&g_lock);
        fwd_lwip_close(fd, 1);
        return;
    }
    g_links++;
    g_rules[rule].links++;
    pthread_mutex_unlock(&g_lock);
    k->outer = fd;
    k->inner = -1;
    k->rule = rule;
    k->peer = peer;
    k->peer_port = peer_port;
    if (spawn(inward_main, k) != 0) {
        fwd_lwip_close(fd, 1);
        account(rule, 0, 1);
        free(k);
    }
}

static void *accept_main(void *unused)
{
    (void)unused;
    while (!__atomic_load_n(&g_stop, __ATOMIC_RELAXED)) {
        int fds[FWD_RULES], map[FWD_RULES], ready[FWD_RULES], n = 0, i;

        pthread_mutex_lock(&g_lock);
        reconcile();
        for (i = 0; i < FWD_RULES; i++)
            if (g_rules[i].fd >= 0) {
                fds[n] = g_rules[i].fd;
                map[n++] = i;
            }
        pthread_mutex_unlock(&g_lock);
        if (fwd_lwip_wait(fds, n, WAIT_MS, ready) <= 0) continue;
        for (i = 0; i < n; i++)
            if (ready[i]) take(map[i]);
    }
    pthread_mutex_lock(&g_lock);
    reconcile(); /* g_stop: every listener closed */
    pthread_mutex_unlock(&g_lock);
    return NULL;
}

int fwd_start(const char *address)
{
    pthread_attr_t attr;
    int result;

    pthread_mutex_lock(&g_lock);
    if (g_running) {
        pthread_mutex_unlock(&g_lock);
        return 0;
    }
    snprintf(g_address, sizeof g_address, "%s", address);
    g_stop = 0;
    pthread_mutex_unlock(&g_lock);
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, LINK_STACK);
    result = pthread_create(&g_thread, &attr, accept_main, NULL);
    pthread_attr_destroy(&attr);
    if (result) return -1;
    pthread_mutex_lock(&g_lock);
    g_running = 1;
    pthread_mutex_unlock(&g_lock);
    return 0;
}

/* Every listener closed; open connections are cut and given up to STOP_WAIT_MS to end. */
void fwd_stop(void)
{
    long long until;

    pthread_mutex_lock(&g_lock);
    if (!g_running) {
        pthread_mutex_unlock(&g_lock);
        return;
    }
    __atomic_store_n(&g_stop, 1, __ATOMIC_RELAXED);
    pthread_mutex_unlock(&g_lock);
    pthread_join(g_thread, NULL);
    until = now_ms() + STOP_WAIT_MS;
    for (;;) {
        int left;

        pthread_mutex_lock(&g_lock);
        left = g_links;
        pthread_mutex_unlock(&g_lock);
        if (!left || now_ms() >= until) {
            if (left) say("%d connections still open at stop", left);
            break;
        }
        usleep(20000);
    }
    pthread_mutex_lock(&g_lock);
    g_running = 0;
    pthread_mutex_unlock(&g_lock);
}
