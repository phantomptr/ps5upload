/* Two helpers sent together: the one that waited leaves the start to one that already serves
 * (start_dedupe.h). Plain files and one loopback connection; no Sony call, no fork, no rename. */
#include "start_dedupe.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

long long start_dedupe_mono_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000L;
}

int serving_record_format(char *buf, size_t n, const serving_rec_t *r) {
    int len;
    if (!buf || !r || r->pid <= 0 || !r->version[0] || r->mono_ms <= 0) return -1;
    len = snprintf(buf, n, "pid=%d\nversion=%s\nmono_ms=%lld\n", r->pid, r->version, r->mono_ms);
    return (len < 0 || (size_t)len >= n) ? -1 : len;
}

int serving_record_parse(const char *text, size_t len, serving_rec_t *r) {
    const char *p = text, *end = text + len;
    int have_pid = 0, have_ver = 0, have_mono = 0;
    memset(r, 0, sizeof *r);
    while (p < end) {
        const char *nl = memchr(p, '\n', (size_t)(end - p));
        char line[96];
        size_t l;
        if (!nl) break; /* a line still being written does not count */
        l = (size_t)(nl - p);
        if (l < sizeof line) {
            memcpy(line, p, l);
            line[l] = '\0';
            if (strncmp(line, "pid=", 4) == 0) {
                char *e;
                long v = strtol(line + 4, &e, 10);
                if (*e == '\0' && v > 0 && v < 0x7fffffff) {
                    r->pid = (int)v;
                    have_pid = 1;
                }
            } else if (strncmp(line, "version=", 8) == 0 && l - 8 > 0 && l - 8 < sizeof r->version) {
                memcpy(r->version, line + 8, l - 8);
                r->version[l - 8] = '\0';
                have_ver = 1;
            } else if (strncmp(line, "mono_ms=", 8) == 0) {
                char *e;
                long long v = strtoll(line + 8, &e, 10);
                if (*e == '\0' && v > 0) {
                    r->mono_ms = v;
                    have_mono = 1;
                }
            }
        }
        p = nl + 1;
    }
    return have_pid && have_ver && have_mono;
}

static void record_path(const char *dir, char *out, size_t cap) { snprintf(out, cap, "%s/serving", dir); }

int serving_record_write(const char *dir, const serving_rec_t *r) {
    char path[512], buf[128];
    int len = serving_record_format(buf, sizeof buf, r), fd, ok;
    if (len < 0 || !dir) return -1;
    record_path(dir, path, sizeof path);
    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    ok = write(fd, buf, (size_t)len) == (ssize_t)len;
    close(fd);
    return ok ? 0 : -1;
}

int serving_record_read(const char *dir, serving_rec_t *r) {
    char path[512], buf[256];
    ssize_t n;
    int fd;
    memset(r, 0, sizeof *r);
    if (!dir) return 0;
    record_path(dir, path, sizeof path);
    fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return 0;
    return serving_record_parse(buf, (size_t)n, r);
}

void serving_record_clear(const char *dir, int pid) {
    serving_rec_t r;
    char path[512];
    /* Only our own: a newer helper's record must survive our exit. */
    if (!serving_record_read(dir, &r) || r.pid != pid) return;
    record_path(dir, path, sizeof path);
    (void)unlink(path);
}

/* "12.6.15-rc.1" -> {12, 6, 15}, pre = "rc.1". 0, or -1 when it is not a version. */
static int parse_version(const char *v, long n[3], const char **pre) {
    int i;
    const char *p = v;
    if (!v) return -1;
    if (*p == 'v' || *p == 'V') p++;
    n[0] = n[1] = n[2] = 0;
    *pre = NULL;
    for (i = 0; i < 3; i++) {
        char *e;
        if (*p < '0' || *p > '9') return -1;
        n[i] = strtol(p, &e, 10);
        p = e;
        if (i < 2) {
            if (*p == '.') {
                p++;
                continue;
            }
            if (i == 1 && (*p == '\0' || *p == '-')) break; /* "12.6" */
            return -1;
        }
    }
    if (*p == '-') {
        if (!p[1]) return -1;
        *pre = p + 1;
        return 0;
    }
    return *p == '\0' ? 0 : -1;
}

int start_dedupe_version_cmp(const char *a, const char *b) {
    long x[3], y[3];
    const char *xp, *yp;
    int i, c;
    if (parse_version(a, x, &xp) != 0 || parse_version(b, y, &yp) != 0) return -2;
    for (i = 0; i < 3; i++)
        if (x[i] != y[i]) return x[i] < y[i] ? -1 : 1;
    if (!xp && !yp) return 0;
    if (xp && !yp) return -1;
    if (!xp && yp) return 1;
    c = strcmp(xp, yp);
    return c < 0 ? -1 : c > 0 ? 1 : 0;
}

int start_dedupe_should_stand_down(const serving_rec_t *rec, int rec_complete, const char *mine,
                                   long long now_mono_ms, int self_pid, int pid_alive) {
    long long age;
    int c;
    if (!rec || !rec_complete || !mine) return 0;
    if (rec->pid == self_pid || !pid_alive) return 0;
    c = start_dedupe_version_cmp(rec->version, mine);
    if (c < 0) return 0; /* older, or a version that does not read: take over as always */
    age = now_mono_ms - rec->mono_ms;
    return age >= 0 && age <= START_DEDUPE_RECENT_MS;
}

/* CRC32C (Castagnoli, reflected), for the 16-byte AVA1 frame header (SPEC.md section 2). */
static uint32_t crc32c(const uint8_t *p, size_t n) {
    uint32_t c = 0xFFFFFFFFu;
    size_t i;
    int k;
    for (i = 0; i < n; i++) {
        c ^= p[i];
        for (k = 0; k < 8; k++) c = (c >> 1) ^ (0x82F63B78u & (0u - (c & 1u)));
    }
    return c ^ 0xFFFFFFFFu;
}

static void put32(uint8_t *b, uint32_t v) {
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
    b[2] = (uint8_t)(v >> 16);
    b[3] = (uint8_t)(v >> 24);
}

int start_dedupe_probe_serves(int port, int timeout_ms) {
    struct sockaddr_in a;
    uint8_t hdr[16], got[16];
    size_t have = 0;
    long long until = start_dedupe_mono_ms() + timeout_ms;
    int fd, flags, ok = 0;
    /* "A1", type Hs1 (0x01), flags 0, channel 0, body_len 0, CRC32C of bytes 0..11. An Hs1 with no
     * body does not decode, so the server answers Error(protocol) and closes: before any handshake,
     * no pairing code shown, nothing counted against pairing. */
    memset(hdr, 0, sizeof hdr);
    hdr[0] = 'A';
    hdr[1] = '1';
    hdr[2] = 0x01;
    put32(hdr + 12, crc32c(hdr, 12));
    fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return 0;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    flags = fcntl(fd, F_GETFL, 0);
    if (flags >= 0) (void)fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    if (connect(fd, (struct sockaddr *)&a, sizeof a) != 0 && errno != EINPROGRESS) goto out;
    {
        struct pollfd pw = {fd, POLLOUT, 0};
        int err = 0;
        socklen_t el = sizeof err;
        if (poll(&pw, 1, timeout_ms) <= 0) goto out;
        if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &el) != 0 || err != 0) goto out;
    }
    if (send(fd, hdr, sizeof hdr, 0) != (ssize_t)sizeof hdr) goto out;
    while (have < sizeof got) {
        long long left = until - start_dedupe_mono_ms();
        struct pollfd pr = {fd, POLLIN, 0};
        ssize_t n;
        if (left <= 0 || poll(&pr, 1, (int)left) <= 0) goto out;
        n = recv(fd, got + have, sizeof got - have, 0);
        if (n < 0 && (errno == EINTR || errno == EAGAIN)) continue;
        if (n <= 0) goto out;
        have += (size_t)n;
    }
    /* Any well-formed frame header back is a server that read ours and answered (an Error, 0x0b,
     * in practice). */
    ok = got[0] == 'A' && got[1] == '1' && crc32c(got, 12) == ((uint32_t)got[12] | (uint32_t)got[13] << 8 |
                                                              (uint32_t)got[14] << 16 | (uint32_t)got[15] << 24);
out:
    close(fd);
    return ok;
}
