/* File transfer pieces for the console page's Files: see include/webui_files.h. */
#include "webui_files.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

#define WF_SLICE_MS 200

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

int wf_rate_ok(wf_rate_t *r, uint64_t now, uint64_t n) {
    uint64_t win = r->window_ms ? r->window_ms : WF_RATE_WINDOW_MS;
    uint64_t min = r->min_bps ? r->min_bps : WF_RATE_MIN_BPS;
    if (!r->start_ms) {
        r->start_ms = now ? now : 1;
        r->bytes = 0;
    }
    r->bytes += n;
    if (now < r->start_ms || now - r->start_ms < win) return 1;
    if (r->bytes * 1000u < min * (now - r->start_ms)) return 0;
    r->start_ms = now;
    r->bytes = 0;
    return 1;
}

const char *wf_error_token(int e) {
    switch (e) {
    case WF_OK: return "ok";
    case WF_E_TOO_BIG: return "too_large";
    case WF_E_BINARY: return "binary";
    case WF_E_CHANGED: return "changed";
    case WF_E_GONE: return "gone";
    case WF_E_EXISTS: return "exists";
    case WF_E_NOT_FILE: return "not_a_file";
    case WF_E_NO_SPACE: return "no_space";
    case WF_E_CROSS: return "cross_mount";
    case WF_E_SHORT: return "incomplete";
    case WF_E_CANCELLED: return "cancelled";
    default: return "io_error";
    }
}

int wf_error_http(int e) {
    switch (e) {
    case WF_OK: return 200;
    case WF_E_TOO_BIG: return 413;
    case WF_E_BINARY: return 415;
    case WF_E_GONE: return 404;
    case WF_E_CHANGED:
    case WF_E_EXISTS:
    case WF_E_NOT_FILE:
    case WF_E_CROSS:
    case WF_E_CANCELLED: return 409;
    case WF_E_NO_SPACE: return 507;
    case WF_E_SHORT: return 400;
    default: return 500;
    }
}

/* ---- socket I/O ---- */

ssize_t wf_recv(int fd, void *buf, size_t cap, unsigned idle_ms, const volatile int *cancel) {
    unsigned waited = 0;
    for (;;) {
        struct pollfd pf;
        int r;
        ssize_t n;
        if (cancel && *cancel) return -1;
        pf.fd = fd;
        pf.events = POLLIN;
        pf.revents = 0;
        r = poll(&pf, 1, WF_SLICE_MS);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return -1;
        if (r == 0) {
            waited += WF_SLICE_MS;
            if (waited >= idle_ms) return -1;
            continue;
        }
        n = recv(fd, buf, cap, 0);
        if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
        return n < 0 ? -1 : n;
    }
}

static int send_rated(int fd, const void *p, size_t n, unsigned idle_ms, wf_rate_t *rate);

int wf_send_all(int fd, const void *p, size_t n, unsigned idle_ms) { return send_rated(fd, p, n, idle_ms, NULL); }

static int send_rated(int fd, const void *p, size_t n, unsigned idle_ms, wf_rate_t *rate) {
    const char *c = (const char *)p;
    unsigned waited = 0;
    while (n > 0) {
        struct pollfd pf;
        ssize_t w;
        int r;
        pf.fd = fd;
        pf.events = POLLOUT;
        pf.revents = 0;
        r = poll(&pf, 1, WF_SLICE_MS);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return -1;
        if (r == 0) {
            waited += WF_SLICE_MS;
            if (waited >= idle_ms) return -1;
            continue;
        }
        if (pf.revents & (POLLERR | POLLHUP | POLLNVAL)) return -1;
        w = send(fd, c, n, MSG_NOSIGNAL);
        if (w < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
        if (w <= 0) return -1;
        if (rate && !wf_rate_ok(rate, now_ms(), (uint64_t)w)) return -1;
        c += w;
        n -= (size_t)w;
        waited = 0;
    }
    return 0;
}

/* ---- paths ---- */

/* The folder part of `path` ("/a/b" -> "/a", "/b" -> "/"): 0, or -1 when it does not fit. */
static int dir_of(const char *path, char *out, size_t cap) {
    const char *slash = strrchr(path, '/');
    size_t n;
    if (!slash) return -1;
    n = slash == path ? 1 : (size_t)(slash - path);
    if (n + 1 > cap) return -1;
    memcpy(out, path, n);
    out[n] = '\0';
    return 0;
}

int wf_temp_path(const char *target, char *out, size_t cap) {
    static const char hex[] = "0123456789abcdef";
    char dir[4096];
    unsigned char rnd[8];
    char tag[17];
    int i, k;
    if (dir_of(target, dir, sizeof dir) != 0) return -1;
    arc4random_buf(rnd, sizeof rnd);
    for (i = 0; i < 8; i++) {
        tag[i * 2] = hex[rnd[i] >> 4];
        tag[i * 2 + 1] = hex[rnd[i] & 15];
    }
    tag[16] = '\0';
    k = snprintf(out, cap, "%s/.ps5u-%s.part", strcmp(dir, "/") == 0 ? "" : dir, tag);
    return k > 0 && (size_t)k < cap ? 0 : -1;
}

int wf_sweep_stale(const char *dir, time_t now, unsigned max_age_s) {
    char p[4200];
    DIR *d = opendir(dir);
    struct dirent *e;
    unsigned seen = 0;
    int removed = 0;
    if (!d) return 0;
    while ((e = readdir(d)) != NULL && seen++ < WF_SWEEP_MAX) {
        struct stat st;
        size_t nl = strlen(e->d_name);
        if (nl < 11 || strncmp(e->d_name, ".ps5u-", 6) != 0 || strcmp(e->d_name + nl - 5, ".part") != 0) continue;
        if (snprintf(p, sizeof p, "%s/%s", strcmp(dir, "/") == 0 ? "" : dir, e->d_name) >= (int)sizeof p) continue;
        if (lstat(p, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        if (st.st_mtime > now || (uint64_t)(now - st.st_mtime) < max_age_s) continue;
        if (unlink(p) == 0) removed++;
    }
    closedir(d);
    return removed;
}

static int write_all(int fd, const uint8_t *p, size_t n) {
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return errno == ENOSPC || errno == EDQUOT ? WF_E_NO_SPACE : WF_E_IO;
        p += w;
        n -= (size_t)w;
    }
    return WF_OK;
}

/* fsync, with "this filesystem has none" not an error. */
static int sync_fd(int fd) {
    if (fsync(fd) == 0) return 0;
    return errno == EINVAL || errno == ENOTSUP ? 0 : -1;
}

/* Is a rename of `tmp_dev`'s file onto `target` within one device? The target's own device when it
 * exists (lstat: a link is judged where it lives), else its folder's. 1 yes; 0 no or unknown. */
static int same_device_as(dev_t tmp_dev, const char *target) {
    struct stat st;
    char dir[4096];
    if (lstat(target, &st) == 0) return st.st_dev == tmp_dev;
    if (dir_of(target, dir, sizeof dir) != 0 || stat(dir, &st) != 0) return 0;
    return st.st_dev == tmp_dev;
}

/* ---- text save ---- */

int wf_text_ok(const uint8_t *data, size_t n) { return n == 0 || memchr(data, 0, n) == NULL; }

uint32_t wf_mtime_ns(const struct stat *st) {
#if defined(__APPLE__)
    return (uint32_t)st->st_mtimespec.tv_nsec;
#else
    return (uint32_t)st->st_mtim.tv_nsec;
#endif
}

static int pre_matches(const struct stat *st, const wf_text_pre_t *pre) {
    if (!pre || !pre->check) return 1;
    if (pre->have_ns && wf_mtime_ns(st) != pre->mtime_ns) return 0;
    return (uint64_t)st->st_mtime == pre->mtime && (uint64_t)st->st_size == pre->size;
}

int wf_text_save(const char *path, const uint8_t *data, size_t n, const wf_text_pre_t *pre, uint64_t *mtime_out,
                 uint64_t *size_out, uint32_t *mtime_ns_out) {
    char tmp[4200];
    struct stat st, ts;
    int fd, rc;
    mode_t mode;
    if (n > WF_TEXT_MAX) return WF_E_TOO_BIG;
    if (!wf_text_ok(data, n)) return WF_E_BINARY;
    if (pre && pre->create) {
        /* A new file is empty: made in one O_EXCL step, so nothing there is ever replaced. */
        if (n) return WF_E_IO;
        fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0777);
        if (fd < 0) return errno == EEXIST ? WF_E_EXISTS : errno == ENOSPC ? WF_E_NO_SPACE : WF_E_IO;
        (void)fchmod(fd, 0777); /* the umask must not take the bits the helper's files always have */
        if (fstat(fd, &st) != 0) memset(&st, 0, sizeof st);
        close(fd);
        if (mtime_out) *mtime_out = (uint64_t)st.st_mtime;
        if (mtime_ns_out) *mtime_ns_out = wf_mtime_ns(&st);
        if (size_out) *size_out = 0;
        return WF_OK;
    }
    if (lstat(path, &st) != 0) return errno == ENOENT ? WF_E_GONE : WF_E_IO;
    if (!S_ISREG(st.st_mode)) return WF_E_NOT_FILE;
    if (!pre_matches(&st, pre)) return WF_E_CHANGED;
    mode = st.st_mode & 07777;
    if (wf_temp_path(path, tmp, sizeof tmp) != 0) return WF_E_IO;
    fd = open(tmp, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (fd < 0) return errno == ENOSPC ? WF_E_NO_SPACE : WF_E_IO;
    rc = write_all(fd, data, n);
    if (rc == WF_OK && sync_fd(fd) != 0) rc = WF_E_IO;
    if (rc == WF_OK) (void)fchmod(fd, mode);
    if (rc == WF_OK && fstat(fd, &ts) != 0) rc = WF_E_IO;
    close(fd);
    if (rc == WF_OK) {
        /* Again right before the rename: a change while the bytes were written still wins. */
        if (lstat(path, &st) != 0) rc = errno == ENOENT ? WF_E_GONE : WF_E_IO;
        else if (!S_ISREG(st.st_mode)) rc = WF_E_NOT_FILE;
        else if (!pre_matches(&st, pre)) rc = WF_E_CHANGED;
        else if (!same_device_as(ts.st_dev, path)) rc = WF_E_CROSS;
    }
    if (rc == WF_OK && rename(tmp, path) != 0) rc = WF_E_IO;
    if (rc != WF_OK) {
        unlink(tmp);
        return rc;
    }
    if (stat(path, &st) == 0) {
        if (mtime_out) *mtime_out = (uint64_t)st.st_mtime;
        if (mtime_ns_out) *mtime_ns_out = wf_mtime_ns(&st);
        if (size_out) *size_out = (uint64_t)st.st_size;
    }
    return WF_OK;
}

/* ---- upload ---- */

/* A large socket buffer for a bulk transfer (the default is sized for small requests). */
static void bulk_buffer(int fd, int opt) {
    int sz = 4 * 1024 * 1024;
    (void)setsockopt(fd, SOL_SOCKET, opt, &sz, sizeof sz);
}

int wf_receive_file(int fd, const uint8_t *pre, size_t pre_len, uint64_t len, const char *target, int overwrite,
                    unsigned mode, unsigned idle_ms, const volatile int *cancel, uint8_t *buf, size_t cap) {
    char tmp[4200];
    struct stat st, ts;
    uint64_t got = 0;
    size_t used = 0;
    int out, rc = WF_OK;
    wf_rate_t rate;
    memset(&rate, 0, sizeof rate);
    bulk_buffer(fd, SO_RCVBUF);
    if (pre_len > len) pre_len = (size_t)len;
    if (wf_temp_path(target, tmp, sizeof tmp) != 0) return WF_E_IO;
    out = open(tmp, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (out < 0) return errno == ENOSPC ? WF_E_NO_SPACE : WF_E_IO;
    if (pre_len) {
        rc = write_all(out, pre, pre_len);
        got = pre_len;
    }
    while (rc == WF_OK && got < len) {
        size_t want = cap - used;
        ssize_t r;
        if ((uint64_t)want > len - got) want = (size_t)(len - got);
        r = wf_recv(fd, buf + used, want, idle_ms, cancel);
        if (r <= 0 || !wf_rate_ok(&rate, now_ms(), (uint64_t)r)) {
            rc = cancel && *cancel ? WF_E_CANCELLED : WF_E_SHORT;
            break;
        }
        used += (size_t)r;
        got += (uint64_t)r;
        /* Writes in whole buffers: the drive sees large writes whatever the socket hands over. */
        if (used == cap || got == len) {
            rc = write_all(out, buf, used);
            used = 0;
        }
    }
    if (rc == WF_OK && sync_fd(out) != 0) rc = WF_E_IO;
    if (rc == WF_OK) (void)fchmod(out, (mode_t)(mode & 07777));
    if (rc == WF_OK && fstat(out, &ts) != 0) rc = WF_E_IO;
    close(out);
    if (rc == WF_OK && lstat(target, &st) == 0) {
        if (!S_ISREG(st.st_mode)) rc = WF_E_NOT_FILE;
        else if (!overwrite) rc = WF_E_EXISTS;
    }
    if (rc == WF_OK && !same_device_as(ts.st_dev, target)) rc = WF_E_CROSS;
    if (rc == WF_OK && rename(tmp, target) != 0) rc = WF_E_IO;
    if (rc != WF_OK) unlink(tmp);
    return rc;
}

/* ---- download ---- */

static int parse_u64(const char **p, uint64_t *out) {
    const char *s = *p;
    uint64_t v = 0;
    if (*s < '0' || *s > '9') return -1;
    while (*s >= '0' && *s <= '9') {
        if (v > (UINT64_MAX - 9) / 10) return -1;
        v = v * 10 + (uint64_t)(*s - '0');
        s++;
    }
    *p = s;
    *out = v;
    return 0;
}

int wf_parse_range(const char *v, uint64_t size, uint64_t *start, uint64_t *end) {
    const char *p = v;
    uint64_t a = 0, b = 0;
    int have_a, have_b;
    if (!v || strncmp(p, "bytes=", 6) != 0) return 0;
    p += 6;
    while (*p == ' ') p++;
    if (strchr(p, ',')) return 0; /* several ranges: the whole file instead */
    have_a = parse_u64(&p, &a) == 0;
    if (*p != '-') return 0;
    p++;
    have_b = parse_u64(&p, &b) == 0;
    while (*p == ' ') p++;
    if (*p) return 0;
    if (!have_a && !have_b) return 0;
    if (!have_a) {
        /* the last b bytes */
        if (b == 0 || size == 0) return -1;
        *start = b >= size ? 0 : size - b;
        *end = size - 1;
        return 1;
    }
    if (have_b && b < a) return 0;
    if (a >= size) return -1;
    *start = a;
    *end = !have_b || b >= size ? size - 1 : b;
    return 1;
}

size_t wf_disposition(char *out, size_t cap, const char *name) {
    static const char hex[] = "0123456789ABCDEF";
    size_t at, i;
    int k = snprintf(out, cap, "Content-Disposition: attachment; filename=\"");
    if (k < 0 || (size_t)k >= cap) return 0;
    at = (size_t)k;
    /* The plain fallback: ASCII only, quotes and controls replaced. */
    for (i = 0; name[i]; i++) {
        unsigned char c = (unsigned char)name[i];
        if (at + 2 >= cap) return 0;
        out[at++] = (c < 0x20 || c >= 0x7f || c == '"' || c == '\\') ? '_' : (char)c;
    }
    k = snprintf(out + at, cap - at, "\"; filename*=UTF-8''");
    if (k < 0 || (size_t)k >= cap - at) return 0;
    at += (size_t)k;
    for (i = 0; name[i]; i++) {
        unsigned char c = (unsigned char)name[i];
        int plain = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' ||
                    c == '-' || c == '_' || c == '~';
        if (at + 4 >= cap) return 0;
        if (plain) {
            out[at++] = (char)c;
        } else {
            out[at++] = '%';
            out[at++] = hex[c >> 4];
            out[at++] = hex[c & 15];
        }
    }
    if (at + 3 > cap) return 0;
    out[at++] = '\r';
    out[at++] = '\n';
    out[at] = '\0';
    return at;
}

int wf_send_file(int fd, int file_fd, uint64_t start, uint64_t len, unsigned idle_ms, uint8_t *buf, size_t cap) {
    uint64_t done = 0;
    wf_rate_t rate;
    memset(&rate, 0, sizeof rate);
    bulk_buffer(fd, SO_SNDBUF);
    while (done < len) {
        size_t want = cap;
        ssize_t r;
        if ((uint64_t)want > len - done) want = (size_t)(len - done);
        r = pread(file_fd, buf, want, (off_t)(start + done));
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1; /* shrank under us: the Content-Length promised more, so stop short */
        if (send_rated(fd, buf, (size_t)r, idle_ms, &rate) != 0) return -1;
        done += (uint64_t)r;
    }
    return 0;
}

/* ---- tar ---- */

static void octal(char *field, size_t width, uint64_t v) {
    /* width-1 digits, then NUL */
    size_t i = width - 1;
    field[i] = '\0';
    while (i > 0) {
        field[--i] = (char)('0' + (v & 7));
        v >>= 3;
    }
}

static void put_checksum(uint8_t *h) {
    unsigned sum = 0;
    int i;
    memset(h + 148, ' ', 8);
    for (i = 0; i < 512; i++) sum += h[i];
    octal((char *)h + 148, 7, sum); /* six digits and a NUL, then the space already there */
    h[155] = ' ';
}

/* Does `name` fit ustar's name (100) or prefix (155) + '/' + name (100)? Fills the fields when it does. */
static int ustar_name(uint8_t *h, const char *name) {
    size_t n = strlen(name), i;
    if (n <= 100) {
        memcpy(h, name, n);
        return 1;
    }
    /* split at a '/' so the tail is <= 100 and the head <= 155 */
    for (i = n > 101 ? n - 101 : 0; i < n; i++) {
        if (name[i] != '/') continue;
        if (i <= 155 && n - i - 1 <= 100 && n - i - 1 > 0) {
            memcpy(h + 345, name, i);
            memcpy(h, name + i + 1, n - i - 1);
            return 1;
        }
        if (i > 155) break;
    }
    return 0;
}

/* One pax record "<len> key=value\n" at out; its length, or 0 when it does not fit. */
static size_t pax_record(char *out, size_t cap, const char *key, const char *val) {
    size_t body = strlen(key) + 1 + strlen(val) + 1 + 1; /* ' ' key '=' val '\n' */
    size_t len = body + 1, digits;
    int k;
    for (;;) {
        char tmp[24];
        digits = (size_t)snprintf(tmp, sizeof tmp, "%zu", len);
        if (body + digits == len) break;
        len = body + digits;
    }
    if (len >= cap) return 0;
    k = snprintf(out, cap, "%zu %s=%s\n", len, key, val);
    return k > 0 && (size_t)k == len ? len : 0;
}

static void base_header(uint8_t *h, int is_dir, uint64_t size, unsigned mode, uint64_t mtime, char type) {
    octal((char *)h + 100, 8, mode & 07777);
    octal((char *)h + 108, 8, 0);
    octal((char *)h + 116, 8, 0);
    if (size < (1ull << 33)) {
        octal((char *)h + 124, 12, size);
    } else {
        /* base-256 (GNU, star, libarchive): the pax size record is the standard one */
        int i;
        uint64_t v = size;
        h[124] = 0x80;
        for (i = 11; i > 0; i--) {
            h[124 + i] = (uint8_t)(v & 0xff);
            v >>= 8;
        }
    }
    octal((char *)h + 136, 12, mtime > 077777777777ull ? 077777777777ull : mtime);
    h[156] = (uint8_t)(type ? type : (is_dir ? '5' : '0'));
    memcpy(h + 257, "ustar", 6); /* "ustar\0" */
    memcpy(h + 263, "00", 2);
}

size_t wf_tar_header(uint8_t *out, size_t cap, const char *name, int is_dir, uint64_t size, unsigned mode,
                     uint64_t mtime) {
    char full[WF_TAR_PATH_MAX + 2];
    char recs[2 * WF_TAR_PATH_MAX + 128];
    uint8_t *h;
    size_t n = strlen(name), at = 0, rl = 0;
    int need_pax;
    if (n == 0 || n > WF_TAR_PATH_MAX) return 0;
    memcpy(full, name, n);
    if (is_dir && full[n - 1] != '/') full[n++] = '/';
    full[n] = '\0';
    if (is_dir) size = 0;
    if (cap < 512) return 0;
    memset(out, 0, 512);
    need_pax = !ustar_name(out, full) || size >= (1ull << 33);
    if (need_pax) {
        size_t r, blocks;
        char sz[24];
        if (!ustar_name(out, full)) {
            r = pax_record(recs + rl, sizeof recs - rl, "path", full);
            if (!r) return 0;
            rl += r;
        }
        if (size >= (1ull << 33)) {
            snprintf(sz, sizeof sz, "%llu", (unsigned long long)size);
            r = pax_record(recs + rl, sizeof recs - rl, "size", sz);
            if (!r) return 0;
            rl += r;
        }
        blocks = (rl + 511) / 512;
        if (cap < 512 + blocks * 512 + 512) return 0;
        h = out;
        memset(h, 0, 512);
        memcpy(h, "././@PaxHeader", 14);
        base_header(h, 0, rl, 0644, mtime, 'x');
        put_checksum(h);
        memset(out + 512, 0, blocks * 512);
        memcpy(out + 512, recs, rl);
        at = 512 + blocks * 512;
        h = out + at;
        memset(h, 0, 512);
        if (!ustar_name(h, full)) memcpy(h, full, 100); /* the pax path is the real one */
    } else {
        h = out;
        at = 0;
    }
    base_header(h, is_dir, size, mode, mtime, 0);
    put_checksum(h);
    return at + 512;
}

typedef struct {
    int fd;
    unsigned idle;
    uint8_t *buf;
    size_t cap, used;
    int (*skip)(const char *, void *);
    void *skip_ctx;
    wf_rate_t rate;
    dev_t dev;
    uint64_t sent;
    char path[WF_TAR_PATH_MAX + 256];
    size_t cut; /* where the archive names start in path */
    int chunked; /* each flush goes out as one HTTP chunk */
    uint8_t head[WF_TAR_HEADER_MAX];
} tarw_t;

static int tw_flush(tarw_t *w) {
    if (w->used && w->chunked) {
        char size[24];
        int k = snprintf(size, sizeof size, "%zx\r\n", w->used);
        if (send_rated(w->fd, size, (size_t)k, w->idle, NULL) != 0) return -1;
    }
    if (w->used && send_rated(w->fd, w->buf, w->used, w->idle, &w->rate) != 0) return -1;
    if (w->used && w->chunked && send_rated(w->fd, "\r\n", 2, w->idle, NULL) != 0) return -1;
    w->sent += w->used;
    w->used = 0;
    return 0;
}

static int tw_put(tarw_t *w, const void *p, size_t n) {
    const uint8_t *c = (const uint8_t *)p;
    while (n > 0) {
        size_t k = w->cap - w->used;
        if (k > n) k = n;
        memcpy(w->buf + w->used, c, k);
        w->used += k;
        c += k;
        n -= k;
        if (w->used == w->cap && tw_flush(w) != 0) return -1;
    }
    return 0;
}

static int tw_entry_header(tarw_t *w, const struct stat *st, int is_dir, uint64_t size) {
    size_t hl = wf_tar_header(w->head, sizeof w->head, w->path + w->cut, is_dir, size, (unsigned)st->st_mode,
                              (uint64_t)(st->st_mtime > 0 ? st->st_mtime : 0));
    if (!hl) return 1; /* cannot be named: left out */
    return tw_put(w, w->head, hl) == 0 ? 0 : -1;
}

/* One regular file: header, its bytes (zero padded to the header's size), padding to 512. */
static int tw_file(tarw_t *w, const struct stat *lst) {
    struct stat st;
    uint64_t size, done = 0;
    int in = open(w->path, O_RDONLY | O_NONBLOCK | O_NOFOLLOW), rc;
    if (in < 0) return 0;
    if (fstat(in, &st) != 0 || !S_ISREG(st.st_mode) || st.st_dev != lst->st_dev) {
        close(in);
        return 0;
    }
    size = (uint64_t)st.st_size;
    rc = tw_entry_header(w, &st, 0, size);
    if (rc != 0) {
        close(in);
        return rc > 0 ? 0 : -1;
    }
    while (done < size) {
        size_t room = w->cap - w->used;
        ssize_t r;
        if ((uint64_t)room > size - done) room = (size_t)(size - done);
        r = pread(in, w->buf + w->used, room, (off_t)done);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) {
            /* A read error (a failing drive): the download ends here, cut short, as a single
             * file's does. Zeros in its place made a complete-looking archive with a silently
             * damaged file inside. */
            close(in);
            return -1;
        }
        if (r == 0) {
            /* shrank while being read: zeros keep the archive's framing */
            memset(w->buf + w->used, 0, room);
            r = (ssize_t)room;
        }
        w->used += (size_t)r;
        done += (uint64_t)r;
        if (w->used == w->cap && tw_flush(w) != 0) {
            close(in);
            return -1;
        }
    }
    close(in);
    if (size % 512) {
        static const uint8_t zeros[512];
        if (tw_put(w, zeros, 512 - (size_t)(size % 512)) != 0) return -1;
    }
    return 0;
}

static int tw_dir(tarw_t *w, int depth) {
    DIR *d;
    struct dirent *e;
    size_t base = strlen(w->path);
    if (depth > WF_TAR_MAX_DEPTH) return 0;
    d = opendir(w->path);
    if (!d) return 0;
    while ((e = readdir(d)) != NULL) {
        struct stat st;
        size_t nl = strlen(e->d_name);
        int rc = 0;
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (base + 1 + nl > WF_TAR_PATH_MAX) continue;
        w->path[base] = '/';
        memcpy(w->path + base + 1, e->d_name, nl + 1);
        if (lstat(w->path, &st) != 0 || st.st_dev != w->dev || (w->skip && w->skip(w->path, w->skip_ctx))) {
            w->path[base] = '\0';
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            rc = tw_entry_header(w, &st, 1, 0);
            if (rc == 0) rc = tw_dir(w, depth + 1);
            else if (rc > 0) rc = 0;
        } else if (S_ISREG(st.st_mode)) {
            rc = tw_file(w, &st);
        }
        w->path[base] = '\0';
        if (rc < 0) {
            closedir(d);
            return -1;
        }
    }
    closedir(d);
    return 0;
}

int wf_send_tar(int fd, const char *root, int (*skip)(const char *path, void *ctx), void *skip_ctx, unsigned idle_ms,
                uint8_t *buf, size_t cap, uint64_t *sent) {
    return wf_send_tar_http(fd, root, skip, skip_ctx, idle_ms, buf, cap, sent, 0);
}

int wf_send_tar_http(int fd, const char *root, int (*skip)(const char *path, void *ctx), void *skip_ctx,
                     unsigned idle_ms, uint8_t *buf, size_t cap, uint64_t *sent, int chunked) {
    static const uint8_t zeros[1024];
    tarw_t *w;
    struct stat st;
    size_t n = strlen(root);
    const char *slash;
    int rc;
    if (sent) *sent = 0;
    while (n > 1 && root[n - 1] == '/') n--;
    if (n == 0 || n > WF_TAR_PATH_MAX || cap < 65536) return -1;
    if (stat(root, &st) != 0 || !S_ISDIR(st.st_mode)) return -1;
    bulk_buffer(fd, SO_SNDBUF);
    w = (tarw_t *)calloc(1, sizeof *w);
    if (!w) return -1;
    w->fd = fd;
    w->idle = idle_ms;
    w->buf = buf;
    w->cap = cap;
    w->skip = skip;
    w->skip_ctx = skip_ctx;
    w->dev = st.st_dev;
    w->chunked = chunked;
    memcpy(w->path, root, n);
    w->path[n] = '\0';
    slash = strrchr(w->path, '/');
    w->cut = slash && slash[1] ? (size_t)(slash - w->path) + 1 : 0;
    rc = tw_entry_header(w, &st, 1, 0);
    if (rc > 0) rc = -1; /* the root itself cannot be named */
    if (rc == 0) rc = tw_dir(w, 0);
    if (rc == 0) rc = tw_put(w, zeros, sizeof zeros);
    if (rc == 0) rc = tw_flush(w);
    /* The last chunk only after a whole archive: a stream cut before it is a failed download to
     * the browser, where a plain close after a read error was a finished one. */
    if (rc == 0 && chunked) rc = send_rated(fd, "0\r\n\r\n", 5, idle_ms, NULL);
    if (sent) *sent = w->sent;
    free(w);
    return rc;
}
