#define _GNU_SOURCE
#include "loopback.h"
#include "http_range.h"
#include "jobs.h"
#include "pathsafe.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <time.h>
#include <pthread.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>

#define LB_MAX_CONNS   8
#define LB_IO_TIMEOUT  60
#define LB_REQ_MAX     8192
#define LB_SEND_CHUNK  (1024 * 1024)

struct inst_loopback {
    int             listen_fd;
    uint16_t        port;
    volatile int    running;
    pthread_t       listener;
    pthread_mutex_t mutex;
    int             pkg_fd;
    uint64_t        total;
    char            dir[512];      /* directory of pkg_path, for .crc sidecars */
    char            attempt[64];
    char            basename[256];
    time_t          fixed_lm;      /* Last-Modified: fixed, in the past */
    inst_coverage_t coverage;      /* body bytes actually sent */
    double          last_req_mono; /* CLOCK_MONOTONIC of the last request */
    volatile int    workers;
};

static double mono_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static int send_all(int fd, const void *b, size_t len) {
    const char *p = b;
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = send(fd, p + sent, len - sent, MSG_NOSIGNAL);
        if (n < 0) { if (errno == EINTR) continue; return -1; }
        if (n == 0) return -1;
        sent += (size_t)n;
    }
    return 0;
}

/* Send a file's whole content (used for a .crc sidecar). */
static int send_file_bytes(int conn, const char *path, int head_only) {
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) return -1;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    char hdr[256];
    int hlen = snprintf(hdr, sizeof(hdr),
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n"
        "Content-Length: %lld\r\nConnection: close\r\n\r\n",
        (long long)st.st_size);
    if (hlen < 0 || send_all(conn, hdr, (size_t)hlen) != 0) { close(fd); return -1; }
    if (!head_only) {
        /* Heap, never the thread stack: a 1 MiB stack buffer overflows the
         * small worker-thread stack (SIGBUS on host; wedges the console). */
        char *buf = malloc(LB_SEND_CHUNK);
        if (buf) {
            ssize_t n;
            while ((n = read(fd, buf, LB_SEND_CHUNK)) > 0) {
                if (send_all(conn, buf, (size_t)n) != 0) break;
            }
            free(buf);
        }
    }
    close(fd);
    return 0;
}

static void serve_one(struct inst_loopback *lb, int conn, const char *req, size_t rlen) {
    inst_http_req_t h;
    if (inst_http_parse_head(req, rlen, &h) != 0) {
        char b[128]; int n = inst_http_hdr_404(b, sizeof(b), 0);
        if (n > 0) send_all(conn, b, (size_t)n);
        return;
    }
    if (!h.is_get && !h.is_head) {
        char b[128]; int n = inst_http_hdr_404(b, sizeof(b), 0);
        if (n > 0) send_all(conn, b, (size_t)n);
        return;
    }

    /* record request time for the idle clock */
    pthread_mutex_lock(&lb->mutex);
    lb->last_req_mono = mono_now();
    pthread_mutex_unlock(&lb->mutex);

    /* expected active path: /<attempt>/<basename> */
    char expect[512];
    snprintf(expect, sizeof(expect), "/%s/%s", lb->attempt, lb->basename);

    if (strcmp(h.target, expect) != 0) {
        /* Maybe a .crc sidecar under /<attempt>/<name>.crc */
        char prefix[128];
        snprintf(prefix, sizeof(prefix), "/%s/", lb->attempt);
        size_t plen = strlen(prefix);
        const char *name = NULL;
        if (strncmp(h.target, prefix, plen) == 0) name = h.target + plen;
        size_t nlen = name ? strlen(name) : 0;
        int is_crc = name && nlen > 4 && strcmp(name + nlen - 4, ".crc") == 0
                     && strchr(name, '/') == NULL;
        if (is_crc) {
            char side[600];
            snprintf(side, sizeof(side), "%s/%s", lb->dir, name);
            if (send_file_bytes(conn, side, h.is_head) == 0) return;
        }
        char b[128]; int n = inst_http_hdr_404(b, sizeof(b), 0);
        if (n > 0) send_all(conn, b, (size_t)n);
        return;
    }

    uint64_t total = lb->total;
    char rangev[128];
    uint64_t start = 0, end = (total > 0) ? total - 1 : 0;
    int has_range = inst_http_header(req, "Range", rangev, sizeof(rangev));
    inst_range_result_t rr = has_range
        ? inst_parse_range(rangev, total, &start, &end)
        : INST_RANGE_NONE;

    char hdr[1024];
    int hlen;
    time_t now = time(NULL);
    if (rr == INST_RANGE_UNSAT) {
        hlen = inst_http_hdr_416(hdr, sizeof(hdr), total, 0);
        if (hlen > 0) send_all(conn, hdr, (size_t)hlen);
        return;
    } else if (rr == INST_RANGE_OK) {
        hlen = inst_http_hdr_206(hdr, sizeof(hdr), start, end, total, now, lb->fixed_lm, 0);
    } else {
        start = 0; end = (total > 0) ? total - 1 : 0;
        hlen = inst_http_hdr_200(hdr, sizeof(hdr), total, now, lb->fixed_lm, 0);
    }
    if (hlen <= 0 || send_all(conn, hdr, (size_t)hlen) != 0) return;
    if (h.is_head || total == 0) return;

    /* body — heap buffer, never the thread stack (see send_file_bytes). */
    char *buf = malloc(LB_SEND_CHUNK);
    if (!buf) return;
    uint64_t off = start;
    while (off <= end) {
        uint64_t remain = end - off + 1;
        size_t want = remain > LB_SEND_CHUNK ? LB_SEND_CHUNK : (size_t)remain;
        ssize_t n = pread(lb->pkg_fd, buf, want, (off_t)off);
        if (n <= 0) break;
        if (send_all(conn, buf, (size_t)n) != 0) break;
        pthread_mutex_lock(&lb->mutex);
        inst_coverage_add(&lb->coverage, off, off + (uint64_t)n - 1);
        pthread_mutex_unlock(&lb->mutex);
        off += (uint64_t)n;
    }
    free(buf);
}

typedef struct { struct inst_loopback *lb; int conn; } worker_arg_t;

static void *worker(void *arg) {
    worker_arg_t *wa = arg;
    struct inst_loopback *lb = wa->lb;
    int conn = wa->conn;
    free(wa);

    struct timeval tv = { LB_IO_TIMEOUT, 0 };
    setsockopt(conn, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(conn, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    char req[LB_REQ_MAX + 1];
    size_t rlen = 0;
    while (rlen < LB_REQ_MAX) {
        ssize_t n = recv(conn, req + rlen, LB_REQ_MAX - rlen, 0);
        if (n <= 0) break;
        rlen += (size_t)n;
        req[rlen] = '\0';
        if (strstr(req, "\r\n\r\n")) break;
    }
    if (rlen > 0) serve_one(lb, conn, req, rlen);

    close(conn);
    __sync_sub_and_fetch(&lb->workers, 1);
    return NULL;
}

static void *listener(void *arg) {
    struct inst_loopback *lb = arg;
    while (lb->running) {
        struct sockaddr_in cli;
        socklen_t cl = sizeof(cli);
        int conn = accept(lb->listen_fd, (struct sockaddr *)&cli, &cl);
        if (conn < 0) { if (lb->running) continue; break; }
        int yes = 1;
        setsockopt(conn, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));
        if (__sync_add_and_fetch(&lb->workers, 1) > LB_MAX_CONNS) {
            __sync_sub_and_fetch(&lb->workers, 1);
            close(conn);
            continue;
        }
        worker_arg_t *wa = malloc(sizeof(*wa));
        if (!wa) { __sync_sub_and_fetch(&lb->workers, 1); close(conn); continue; }
        wa->lb = lb; wa->conn = conn;
        pthread_t t;
        if (pthread_create(&t, NULL, worker, wa) == 0) {
            pthread_detach(t);
        } else {
            __sync_sub_and_fetch(&lb->workers, 1);
            free(wa);
            close(conn);
        }
    }
    return NULL;
}

int inst_loopback_start(inst_loopback_t **out, const char *pkg_path,
                        const char *attempt, const char *base, uint16_t *out_port) {
    struct stat st;
    if (stat(pkg_path, &st) != 0 || !S_ISREG(st.st_mode)) return -1;
    struct inst_loopback *lb = calloc(1, sizeof(*lb));
    if (!lb) return -1;
    pthread_mutex_init(&lb->mutex, NULL);
    inst_coverage_reset(&lb->coverage);
    lb->total = (uint64_t)st.st_size;
    /* A fixed date well in the past, the same one the engine's pkg-host
     * sends. With Last-Modified = the job's start (≈ the response Date) the
     * response is not cacheable, and on FW 13.60 Sony's installer then takes
     * its patch path (DbgGetPatchInfo → DbgCancelPatch 0x80B21401) and
     * refuses with 0x80B2116F — every "Upload & install". Measured on a 13.60
     * Pro: the same link without it refused, with it installed, back to back. */
    lb->fixed_lm = (time_t)1735689600;  /* Wed, 01 Jan 2025 00:00:00 GMT */
    lb->last_req_mono = mono_now();
    snprintf(lb->attempt, sizeof(lb->attempt), "%s", attempt);
    snprintf(lb->basename, sizeof(lb->basename), "%s", base);
    /* directory of pkg_path */
    snprintf(lb->dir, sizeof(lb->dir), "%s", pkg_path);
    char *slash = strrchr(lb->dir, '/');
    if (slash) *slash = '\0'; else strcpy(lb->dir, ".");

    lb->pkg_fd = open(pkg_path, O_RDONLY);
    if (lb->pkg_fd < 0) { free(lb); return -1; }

    lb->listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (lb->listen_fd < 0) { close(lb->pkg_fd); free(lb); return -1; }
    int one = 1;
    setsockopt(lb->listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); /* 127.0.0.1 ONLY */
    a.sin_port = 0;                              /* ephemeral */
    if (bind(lb->listen_fd, (struct sockaddr *)&a, sizeof(a)) != 0 ||
        listen(lb->listen_fd, LB_MAX_CONNS) != 0) {
        close(lb->listen_fd); close(lb->pkg_fd); free(lb); return -1;
    }
    socklen_t al = sizeof(a);
    getsockname(lb->listen_fd, (struct sockaddr *)&a, &al);
    lb->port = ntohs(a.sin_port);
    lb->running = 1;
    if (pthread_create(&lb->listener, NULL, listener, lb) != 0) {
        close(lb->listen_fd); close(lb->pkg_fd); free(lb); return -1;
    }
    *out = lb;
    *out_port = lb->port;
    return 0;
}

uint64_t inst_loopback_bytes_served(inst_loopback_t *lb) {
    pthread_mutex_lock(&lb->mutex);
    uint64_t v = inst_coverage_bytes(&lb->coverage);
    pthread_mutex_unlock(&lb->mutex);
    return v;
}

uint64_t inst_loopback_total(inst_loopback_t *lb) { return lb->total; }

double inst_loopback_idle_seconds(inst_loopback_t *lb) {
    pthread_mutex_lock(&lb->mutex);
    double idle = mono_now() - lb->last_req_mono;
    pthread_mutex_unlock(&lb->mutex);
    return idle;
}

void inst_loopback_stop(inst_loopback_t *lb) {
    if (!lb) return;
    lb->running = 0;
    shutdown(lb->listen_fd, SHUT_RDWR);
    close(lb->listen_fd);
    pthread_join(lb->listener, NULL);
    /* let any in-flight detached workers drain briefly */
    for (int i = 0; i < 200 && lb->workers > 0; i++) usleep(10000);
    close(lb->pkg_fd);
    pthread_mutex_destroy(&lb->mutex);
    free(lb);
}
