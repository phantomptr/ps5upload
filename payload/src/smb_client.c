/* See include/smb_client.h. */
#include "smb_client.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "smbd_json.h"
#include "sigpipe_guard.h"
#include "smbd_proto.h"

#define LOADER_PORT 9021
/* elfldr closes :9021 for about a second while the PC app swaps in its patched build: keep trying. */
#define LOADER_RETRY_MS 8000
#define START_WAIT_MS 12000
#define STOP_WAIT_MS 8000
#define PING_TIMEOUT_MS 2000

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_key[SMBD_KEY_HEX + 1];
static char g_elf_id[17];
/* 1 once a daemon of this build answered (re-checked whenever a connection is refused). */
static int g_checked;

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void nap_ms(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

const char *smbc_elf_id(void) {
    pthread_mutex_lock(&g_mu);
    if (!g_elf_id[0]) {
        uint64_t h = 1469598103934665603ull;
        unsigned int i;
        for (i = 0; i < smbd_elf_len; i++) {
            h ^= smbd_elf[i];
            h *= 1099511628211ull;
        }
        snprintf(g_elf_id, sizeof g_elf_id, "%016llx", (unsigned long long)h);
    }
    pthread_mutex_unlock(&g_mu);
    return g_elf_id;
}

/* ---- the key file ---- */

static int write_key_file(const char *key, const char *elf_id) {
    char tmp[] = SMBD_KEY_PATH ".tmp", text[SMBD_KEY_HEX + 40];
    int fd, k;
    ssize_t w;
    mkdir("/data/ps5upload", 0777);
    mkdir(SMBD_DIR, 0700);
    k = snprintf(text, sizeof text, "%s\n%s\n", key, elf_id);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    (void)fchmod(fd, 0600);
    w = write(fd, text, (size_t)k);
    memset(text, 0, sizeof text);
    if (w != k || fsync(fd) != 0) {
        close(fd);
        unlink(tmp);
        return -1;
    }
    close(fd);
    if (rename(tmp, SMBD_KEY_PATH) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* The key: the file's when it has one (a daemon started by an earlier helper knows only that one),
 * else a new one. Locked by the caller. */
static int key_locked(void) {
    char buf[SMBD_KEY_HEX + 64];
    FILE *f;
    size_t n, i;
    if (g_key[0]) return 0;
    f = fopen(SMBD_KEY_PATH, "r");
    if (f) {
        n = fread(buf, 1, sizeof buf - 1, f);
        fclose(f);
        buf[n] = '\0';
        if (n >= SMBD_KEY_HEX) {
            for (i = 0; i < SMBD_KEY_HEX; i++)
                if (!((buf[i] >= '0' && buf[i] <= '9') || (buf[i] >= 'a' && buf[i] <= 'f'))) break;
            if (i == SMBD_KEY_HEX) {
                memcpy(g_key, buf, SMBD_KEY_HEX);
                g_key[SMBD_KEY_HEX] = '\0';
                memset(buf, 0, sizeof buf);
                return 0;
            }
        }
        memset(buf, 0, sizeof buf);
    }
    {
        uint8_t raw[32];
        static const char d[] = "0123456789abcdef";
        arc4random_buf(raw, sizeof raw);
        for (i = 0; i < 32; i++) {
            g_key[2 * i] = d[raw[i] >> 4];
            g_key[2 * i + 1] = d[raw[i] & 15];
        }
        g_key[SMBD_KEY_HEX] = '\0';
        memset(raw, 0, sizeof raw);
    }
    return write_key_file(g_key, smbc_elf_id());
}

/* ---- sockets ---- */

/* A loopback connection to `port`: the fd, -2 when refused (nothing listens), -1 otherwise. */
static int dial(int port, int timeout_ms) {
    struct sockaddr_in a;
    struct pollfd p;
    int fd = socket(AF_INET, SOCK_STREAM, 0), fl, rc, soerr = 0;
    socklen_t sl = sizeof soerr;
    if (fd < 0) return -1;
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
    fcntl(fd, F_SETFL, fl);
    return fd;
}

static int send_all(int fd, const char *b, size_t n) {
    while (n) {
        ssize_t w = send(fd, b, n, MSG_NOSIGNAL);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        b += w;
        n -= (size_t)w;
    }
    return 0;
}

/* Read one '\n'-terminated line (or up to EOF) within `timeout_ms`. 0 with *out (heap), -1 error,
 * -3 timeout. */
static int read_line(int fd, int timeout_ms, char **out) {
    size_t cap = 4096, len = 0;
    char *buf = malloc(cap);
    long long until = now_ms() + timeout_ms;
    if (!buf) return -1;
    for (;;) {
        struct pollfd p;
        long long left = until - now_ms();
        ssize_t k;
        char *nl;
        if (left <= 0) {
            free(buf);
            return -3;
        }
        p.fd = fd;
        p.events = POLLIN;
        p.revents = 0;
        if (poll(&p, 1, (int)left) <= 0) continue;
        if (len + 1 >= cap) {
            char *nb;
            if (cap >= SMBD_REPLY_MAX + 16) {
                free(buf);
                return -1;
            }
            cap *= 2;
            nb = realloc(buf, cap);
            if (!nb) {
                free(buf);
                return -1;
            }
            buf = nb;
        }
        k = recv(fd, buf + len, cap - len - 1, 0);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) break;
        len += (size_t)k;
        buf[len] = '\0';
        nl = memchr(buf, '\n', len);
        if (nl) {
            *nl = '\0';
            *out = buf;
            return 0;
        }
    }
    buf[len] = '\0';
    if (!len) {
        free(buf);
        return -1;
    }
    *out = buf;
    return 0;
}

/* One exchange with the running daemon: 0 with *reply, -2 refused, -1 failed, -3 timeout. */
static int exchange(const char *fields, size_t len, int timeout_ms, char **reply) {
    size_t cap = SMBD_KEY_HEX + len + 32;
    char *req = malloc(cap), key[SMBD_KEY_HEX + 1];
    int fd, rc, k;
    if (!req) return -1;
    pthread_mutex_lock(&g_mu);
    memcpy(key, g_key, sizeof key);
    pthread_mutex_unlock(&g_mu);
    k = snprintf(req, cap, "{\"key\":\"%s\"%s%.*s}\n", key, len ? "," : "", (int)len, fields);
    memset(key, 0, sizeof key);
    fd = dial(SMBD_PORT, 2000);
    if (fd < 0) {
        memset(req, 0, cap);
        free(req);
        return fd;
    }
    rc = send_all(fd, req, (size_t)k) == 0 ? read_line(fd, timeout_ms, reply) : -1;
    memset(req, 0, cap);
    free(req);
    close(fd);
    return rc;
}

/* The ELF to the loader: connect (retrying a refused port), send it all, half-close, and give the
 * loader a moment to read it. (elfldr then hands this connection to the daemon as its stdio, so it
 * is not closed from that side: the wait is bounded, and the daemon points its stdio elsewhere.) */
static int send_to_loader(void) {
    long long until = now_ms() + LOADER_RETRY_MS;
    int fd;
    char sink[256];
    for (;;) {
        fd = dial(LOADER_PORT, 2000);
        if (fd >= 0) break;
        if (now_ms() >= until) return -1;
        nap_ms(300);
    }
    {
        struct timeval to = {5, 0};
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &to, sizeof to);
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);
    }
    if (send_all(fd, (const char *)smbd_elf, smbd_elf_len) != 0) {
        close(fd);
        return -1;
    }
    shutdown(fd, SHUT_WR);
    {
        long long stop = now_ms() + 1000;
        while (now_ms() < stop) {
            struct pollfd p = {fd, POLLIN, 0};
            if (poll(&p, 1, 500) <= 0) continue;
            if (recv(fd, sink, sizeof sink, 0) <= 0) break;
        }
    }
    close(fd);
    return 0;
}

/* {"ok":true,...,"elf":"<id>"}: 1 ours, 0 another build, -1 not a daemon answer. */
static int ping_is_ours(const char *reply) {
    char elf[32];
    size_t n = strlen(reply);
    int ok = 0;
    if (sj_bool(reply, n, "ok", &ok) != 1 || !ok) return -1;
    if (sj_str(reply, n, "elf", elf, sizeof elf) != 1) return 0;
    return strcmp(elf, smbc_elf_id()) == 0;
}

/* Make sure our daemon answers. Called with g_mu NOT held; serialized by g_start_mu. */
static pthread_mutex_t g_start_mu = PTHREAD_MUTEX_INITIALIZER;

static int ensure_daemon(char *err, size_t err_cap) {
    char *reply = NULL;
    int rc, ours;
    long long until;
    pthread_mutex_lock(&g_start_mu);
    pthread_mutex_lock(&g_mu);
    rc = key_locked();
    pthread_mutex_unlock(&g_mu);
    if (rc != 0) {
        snprintf(err, err_cap, "smb_daemon_unavailable");
        pthread_mutex_unlock(&g_start_mu);
        return -1;
    }
    rc = exchange("\"op\":\"ping\"", 11, PING_TIMEOUT_MS, &reply);
    if (rc == 0) {
        ours = ping_is_ours(reply);
        free(reply);
        reply = NULL;
        if (ours == 1) {
            g_checked = 1;
            pthread_mutex_unlock(&g_start_mu);
            return 0;
        }
        if (ours == 0) {
            /* another build: ask it to go, wait for the port to free */
            if (exchange("\"op\":\"quit\"", 11, PING_TIMEOUT_MS, &reply) == 0) free(reply);
            reply = NULL;
            until = now_ms() + STOP_WAIT_MS;
            while (now_ms() < until) {
                int fd = dial(SMBD_PORT, 500);
                if (fd == -2) break;
                if (fd >= 0) close(fd);
                nap_ms(250);
            }
        } else {
            /* something answered that is not our daemon (or refused our key) */
            snprintf(err, err_cap, "smb_daemon_unavailable");
            pthread_mutex_unlock(&g_start_mu);
            return -1;
        }
    }
    /* (re)write the key file with this build's id, then start it */
    pthread_mutex_lock(&g_mu);
    rc = write_key_file(g_key, smbc_elf_id());
    pthread_mutex_unlock(&g_mu);
    if (rc != 0 || send_to_loader() != 0) {
        snprintf(err, err_cap, rc != 0 ? "smb_daemon_unavailable" : "smb_loader_unavailable");
        pthread_mutex_unlock(&g_start_mu);
        return -1;
    }
    fprintf(stderr, "[smb] daemon sent to the loader (%u bytes)\n", smbd_elf_len);
    until = now_ms() + START_WAIT_MS;
    while (now_ms() < until) {
        nap_ms(250);
        if (exchange("\"op\":\"ping\"", 11, PING_TIMEOUT_MS, &reply) == 0) {
            ours = ping_is_ours(reply);
            free(reply);
            reply = NULL;
            if (ours == 1) {
                g_checked = 1;
                pthread_mutex_unlock(&g_start_mu);
                return 0;
            }
        }
    }
    snprintf(err, err_cap, "smb_daemon_unavailable");
    pthread_mutex_unlock(&g_start_mu);
    return -1;
}

int smbc_request(const char *fields, size_t len, int timeout_ms, int may_start, char **reply, char *err,
                 size_t err_cap) {
    int rc;
    *reply = NULL;
    if (err_cap) err[0] = '\0';
    if (!may_start) {
        pthread_mutex_lock(&g_mu);
        rc = key_locked();
        pthread_mutex_unlock(&g_mu);
        rc = rc == 0 ? exchange(fields, len, timeout_ms, reply) : -1;
        if (rc == 0) return 0;
        snprintf(err, err_cap, rc == -3 ? "smb_daemon_timeout" : rc == -2 ? "smb_daemon_not_running" : "smb_daemon_unavailable");
        return -1;
    }
    if (!g_checked && ensure_daemon(err, err_cap) != 0) return -1;
    rc = exchange(fields, len, timeout_ms, reply);
    if (rc == -2 || (rc == -1 && !*reply)) {
        /* it went away (idle exit, a crash, rest mode): start it again, once */
        if (rc == -1) nap_ms(200);
        g_checked = 0;
        if (ensure_daemon(err, err_cap) != 0) return -1;
        rc = exchange(fields, len, timeout_ms, reply);
    }
    if (rc == 0) return 0;
    snprintf(err, err_cap, rc == -3 ? "smb_daemon_timeout" : "smb_daemon_unavailable");
    return -1;
}
