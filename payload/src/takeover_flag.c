/* Flag-file takeover between AVA1-era instances: see include/takeover_flag.h. */
#include "takeover_flag.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#if defined(__APPLE__) || defined(__FreeBSD__)
#include <sys/sysctl.h> /* KERN_ARND; glibc >= 2.32 has no such header and the use below is #ifdef KERN_ARND (review 007: Linux host build) */
#endif
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define FLAG_NAME "takeover"
#define FLAG_TMP "takeover.tmp"

static int flag_path(char *out, size_t cap, const char *dir, const char *name) {
    int n = snprintf(out, cap, "%s/%s", dir, name);
    return (n < 0 || (size_t)n >= cap) ? -1 : 0;
}

int takeover_flag_write(const char *dir, uint64_t nonce) {
    char tmp[300], dst[300], body[32];
    if (!dir || flag_path(tmp, sizeof tmp, dir, FLAG_TMP) || flag_path(dst, sizeof dst, dir, FLAG_NAME))
        return -1;
    int n = snprintf(body, sizeof body, "%llu\n", (unsigned long long)nonce);
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    int ok = write(fd, body, (size_t)n) == n;
    ok = (close(fd) == 0) && ok;
    /* Same directory, so never a cross-mount rename. */
    if (!ok || rename(tmp, dst) != 0) {
        (void)unlink(tmp);
        return -1;
    }
    return 0;
}

int takeover_flag_read(const char *dir, uint64_t *nonce) {
    char path[300], body[32];
    if (!dir || !nonce || flag_path(path, sizeof path, dir, FLAG_NAME)) return -1;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, body, sizeof body - 1);
    close(fd);
    if (n <= 0) return -1;
    body[n] = '\0';
    char *end = NULL;
    errno = 0;
    unsigned long long v = strtoull(body, &end, 10);
    if (errno != 0 || end == body || v == 0) return -1;
    *nonce = (uint64_t)v;
    return 0;
}

void takeover_flag_unlink(const char *dir) {
    char path[300];
    if (dir && flag_path(path, sizeof path, dir, FLAG_NAME) == 0) (void)unlink(path);
}

void takeover_flag_identity(const char *dir, takeover_flag_id_t *out) {
    char path[300];
    struct stat st;
    memset(out, 0, sizeof *out);
    if (!dir || flag_path(path, sizeof path, dir, FLAG_NAME) || stat(path, &st) != 0) return;
    if (takeover_flag_read(dir, &out->nonce) != 0) return;
    out->present = 1;
    out->ino = (uint64_t)st.st_ino;
#ifdef __APPLE__
    out->mtime_ns = (int64_t)st.st_mtimespec.tv_sec * 1000000000LL + st.st_mtimespec.tv_nsec;
#else
    out->mtime_ns = (int64_t)st.st_mtim.tv_sec * 1000000000LL + st.st_mtim.tv_nsec;
#endif
}

int takeover_flag_asks_us_to_exit(const char *dir, uint64_t my_nonce, const takeover_flag_id_t *stale) {
    takeover_flag_id_t now;
    takeover_flag_identity(dir, &now);
    if (!now.present || now.nonce == my_nonce) return 0;
    if (stale && stale->present && stale->nonce == now.nonce && stale->ino == now.ino &&
        stale->mtime_ns == now.mtime_ns)
        return 0;
    return 1;
}

int takeover_nonce_new(uint64_t *nonce) {
    uint64_t v = 0;
    int ok = 0;
#ifdef KERN_ARND
    {
        int mib[2] = {CTL_KERN, KERN_ARND};
        size_t len = sizeof v;
        ok = sysctl(mib, 2, &v, &len, NULL, 0) == 0 && len == sizeof v;
    }
#endif
    if (!ok) {
        int fd = open("/dev/urandom", O_RDONLY);
        if (fd >= 0) {
            ok = read(fd, &v, sizeof v) == (ssize_t)sizeof v;
            close(fd);
        }
    }
    if (!ok) {
        /* Last resort: mix the monotonic clock and the pid. Only distinctness matters here. */
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        v = ((uint64_t)ts.tv_nsec << 20) ^ (uint64_t)ts.tv_sec ^ ((uint64_t)getpid() << 40);
    }
    if (v == 0) v = 1;
    *nonce = v;
    return 0;
}

int takeover_port_responding(int port) {
    struct sockaddr_in a;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return 0;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int up = connect(fd, (struct sockaddr *)&a, sizeof a) == 0;
    close(fd);
    return up;
}

int takeover_wait_port_free(int port, int max_ms, int interval_ms) {
    struct timespec t0, now;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    if (interval_ms < 1) interval_ms = 1;
    for (;;) {
        long waited;
        if (!takeover_port_responding(port)) return 0;
        clock_gettime(CLOCK_MONOTONIC, &now);
        waited = (long)(now.tv_sec - t0.tv_sec) * 1000 + (now.tv_nsec - t0.tv_nsec) / 1000000;
        if (waited >= max_ms) return -1;
        usleep((useconds_t)interval_ms * 1000u);
    }
}

int takeover_gate_start(int port, int max_ms, int interval_ms, void (*start)(void *), void *ctx) {
    if (takeover_wait_port_free(port, max_ms, interval_ms) != 0) return 0;
    start(ctx);
    return 1;
}

int takeover_flag_request(const char *dir, uint64_t my_nonce, const int *ports, int nports,
                          int attempts, int interval_us) {
    if (takeover_flag_write(dir, my_nonce) != 0) return -1;
    for (int i = 0; i < attempts; i++) {
        int busy = 0;
        for (int p = 0; p < nports; p++) busy |= takeover_port_responding(ports[p]);
        if (!busy) {
            takeover_flag_unlink(dir);
            return 0;
        }
        usleep((useconds_t)interval_us);
    }
    return -1;
}

typedef struct {
    char dir[240];
    uint64_t my_nonce;
    int period_ms;
    void (*on_newer)(void);
    takeover_flag_id_t stale;
} poll_ctx_t;

static void *poll_thread(void *arg) {
    poll_ctx_t c = *(poll_ctx_t *)arg;
    free(arg);
    for (;;) {
        struct timespec ts = {c.period_ms / 1000, (long)(c.period_ms % 1000) * 1000000L};
        nanosleep(&ts, NULL);
        if (takeover_flag_asks_us_to_exit(c.dir, c.my_nonce, &c.stale)) {
            c.on_newer();
            return NULL;
        }
    }
}

int takeover_flag_poll_start(const char *dir, uint64_t my_nonce, int period_ms, void (*on_newer)(void)) {
    if (!dir || !on_newer || period_ms <= 0 || strlen(dir) >= sizeof(((poll_ctx_t *)0)->dir)) return -1;
    poll_ctx_t *c = calloc(1, sizeof *c);
    if (!c) return -1;
    memcpy(c->dir, dir, strlen(dir) + 1);
    c->my_nonce = my_nonce;
    c->period_ms = period_ms;
    c->on_newer = on_newer;
    /* Whatever is there now is from before this instance: remember it as stale and remove it. */
    takeover_flag_identity(dir, &c->stale);
    takeover_flag_unlink(dir);
    pthread_t t;
    pthread_attr_t a;
    if (pthread_attr_init(&a) != 0) {
        free(c);
        return -1;
    }
    (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&t, &a, poll_thread, c);
    pthread_attr_destroy(&a);
    if (rc != 0) {
        free(c);
        return -1;
    }
    return 0;
}
