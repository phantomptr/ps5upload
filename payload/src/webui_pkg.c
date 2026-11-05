/* See include/webui_pkg.h. Runs on the web workers (256 KiB stacks) and, for the install, on an
 * AVA1 operation worker (512 KiB): every buffer of any size is on the heap. No Sony API is called
 * here: the installer daemon makes the install call in its own process. */
#include "webui_pkg.h"

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
#include <sys/statvfs.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "ava1_wire.h"
#include "fs_jobs.h"
#include "path_policy.h"
#include "pkg_info.h"
#include "pkg_install_logic.h"
#include "sigpipe_guard.h"
#include "webui_codec.h"
#include "webui_installer.h"

#define IO_BLOCK (64u * 1024u)
#define REPLY_MAX 2048u
/* The loader closes :9021 for a moment when it is replaced (~1 s) and after a failed accept (3 s):
 * a refused connect is retried this long (engine installer_client send_to_loader_retrying). */
#define LOADER_RETRY_MS 8000
#define LOADER_POLL_MS 250
/* After the daemon is sent, how long it may take to answer hello. */
#define DAEMON_UP_MS 20000
/* No byte read by Sony for this long: the install stalled (engine DELIVERY_STALL_MS). */
#define STALL_MS (5 * 60 * 1000)
/* The daemon not answering a job poll for this long: it is gone. */
#define DAEMON_LOST_MS 60000
/* A game's app.pkg appearing after the last byte was read (engine PLAIN_PATH_CONTENT_WAIT). */
#define CONTENT_WAIT_MS 90000

/* One install at a time; payload loads wait for it. */
static int g_installing;
/* The loader takes one ELF per connection: the daemon send and a payload load never interleave. */
static pthread_mutex_t g_loader_mu = PTHREAD_MUTEX_INITIALIZER;

int webui_pkg_installing(void) { return __atomic_load_n(&g_installing, __ATOMIC_ACQUIRE); }

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void sleep_ms(unsigned ms) {
    struct timespec ts = { (time_t)(ms / 1000), (long)(ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

/* ---- reading a package ---- */

static int fd_read(void *ctx, uint64_t off, void *buf, size_t len) {
    int fd = *(int *)ctx;
    size_t got = 0;
    while (got < len) {
        ssize_t n = pread(fd, (char *)buf + got, len - got, (off_t)(off + got));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return -1;
        got += (size_t)n;
    }
    return 0;
}

/* A readable regular file the page may name: normalised into `norm`, open. -1 with *cause and *http. */
static int open_checked(const char *path, char *norm, size_t cap, struct stat *st, int *http, const char **cause) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    int fd;
    if (snprintf(norm, cap, "%s", path) >= (int)cap || fsj_normalize_path(norm) != 0) {
        *http = 400;
        *cause = "bad_path";
        return -1;
    }
    if (path_in_protected(norm) || !cfg->may_read || !cfg->may_read(norm, 0)) {
        *http = 403;
        *cause = "path_not_allowed";
        return -1;
    }
    fd = open(norm, O_RDONLY);
    if (fd < 0) {
        *http = errno == ENOENT ? 404 : 403;
        *cause = errno == ENOENT ? "not_found" : "read_failed";
        return -1;
    }
    if (fstat(fd, st) != 0 || !S_ISREG(st->st_mode)) {
        close(fd);
        *http = 400;
        *cause = "not_a_file";
        return -1;
    }
    return fd;
}

static int read_pkg(const char *path, char *norm, size_t cap, pkg_info_t *info, uint64_t *size, int *fd_out,
                    const char **cause) {
    struct stat st;
    int http = 0, fd;
    const char *bad;
    if (!pkg_ends_with_ci(path, ".pkg")) {
        *cause = "pkg_not_pkg";
        return 400;
    }
    fd = open_checked(path, norm, cap, &st, &http, cause);
    if (fd < 0) return http;
    *size = (uint64_t)st.st_size;
    bad = pkg_info_read(fd_read, &fd, *size, info);
    if (bad) {
        close(fd);
        *cause = bad;
        return strcmp(bad, "pkg_no_memory") == 0 ? 500 : 400;
    }
    if (fd_out) *fd_out = fd;
    else close(fd);
    return 200;
}

/* ---- what is installed ---- */

static int file_nonempty(const char *p) {
    struct stat st;
    return stat(p, &st) == 0 && S_ISREG(st.st_mode) && st.st_size > 0;
}

static int path_exists(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0;
}

static int valid_title_id(const char *t) {
    size_t i;
    if (strlen(t) != 9) return 0;
    for (i = 0; i < 4; i++)
        if (t[i] < 'A' || t[i] > 'Z') return 0;
    for (; i < 9; i++)
        if (t[i] < '0' || t[i] > '9') return 0;
    return 1;
}

/* The title's app.pkg on internal storage or an extended drive (engine pkg_install probe_app_pkg_at). */
static int app_installed(const char *tid) {
    static const char *const roots[] = { "", "/mnt/ext0", "/mnt/ext1" };
    char p[160];
    size_t i;
    if (!valid_title_id(tid)) return 0;
    for (i = 0; i < sizeof roots / sizeof roots[0]; i++) {
        snprintf(p, sizeof p, "%s/user/app/%s/app.pkg", roots[i], tid);
        if (file_nonempty(p)) return 1;
    }
    return 0;
}

/* ShadowMount+ registered the title from a disk image (its links in /user/app/<id>): Sony's
 * installer would install over a live image registration (memory: smp-install-collision). */
static int image_mounted(const char *tid) {
    char p[160];
    if (!valid_title_id(tid)) return 0;
    snprintf(p, sizeof p, "/user/app/%s/mount.lnk", tid);
    if (path_exists(p)) return 1;
    snprintf(p, sizeof p, "/user/app/%s/mount_img.lnk", tid);
    return path_exists(p);
}

static int is_game(const pkg_info_t *p) { return strcmp(pkg_info_kind(p), "game") == 0; }

/* The guard both the route and the job apply. 200, or an HTTP status with *cause. */
static int install_guard(const pkg_info_t *p, int replace, const char **cause) {
    if (is_game(p) && image_mounted(p->title_id)) {
        *cause = "pkg_install_image_mounted";
        return 409;
    }
    if (is_game(p) && !replace && app_installed(p->title_id)) {
        *cause = "pkg_install_already_installed";
        return 409;
    }
    return 200;
}

/* ---- /api/pkg/info, /api/pkg/icon ---- */

static void put_str(char *out, size_t cap, size_t *at, const char *key, const char *v, int comma) {
    int k;
    if (*at + 1 >= cap) return;
    k = snprintf(out + *at, cap - *at, "%s\"%s\":\"", comma ? "," : "", key);
    if (k < 0 || (size_t)k >= cap - *at) {
        *at = cap - 1;
        return;
    }
    *at += (size_t)k;
    *at += webui_json_escape(out + *at, cap - *at, v, strlen(v));
    if (*at + 2 >= cap) {
        *at = cap - 1;
        return;
    }
    out[(*at)++] = '"';
    out[*at] = '\0';
}

int webui_pkg_info_json(const char *path, char *out, size_t cap, const char **cause) {
    char norm[1024];
    pkg_info_t info;
    uint64_t size = 0;
    size_t at = 0;
    int st, k;
    st = read_pkg(path, norm, sizeof norm, &info, &size, NULL, cause);
    if (st != 200) return st;
    if (cap < 64) {
        *cause = "internal";
        return 500;
    }
    out[at++] = '{';
    out[at] = '\0';
    put_str(out, cap, &at, "title", info.title, 0);
    put_str(out, cap, &at, "title_id", info.title_id, 1);
    put_str(out, cap, &at, "content_id", info.content_id, 1);
    put_str(out, cap, &at, "version", info.app_ver, 1);
    put_str(out, cap, &at, "category", info.category, 1);
    put_str(out, cap, &at, "kind", pkg_info_kind(&info), 1);
    put_str(out, cap, &at, "platform", info.platform, 1);
    put_str(out, cap, &at, "fw", info.fw, 1);
    k = snprintf(out + at, cap - at,
                 ",\"size\":%llu,\"fake\":%s,\"icon\":%s,\"installed\":%s,\"image_mounted\":%s,\"staged\":%s,"
                 "\"installing\":%s}",
                 (unsigned long long)size, info.fih && info.fake ? "true" : "false", info.icon_len ? "true" : "false",
                 is_game(&info) && app_installed(info.title_id) ? "true" : "false",
                 is_game(&info) && image_mounted(info.title_id) ? "true" : "false",
                 pkg_needs_staging(norm) ? "true" : "false", webui_pkg_installing() ? "true" : "false");
    if (k < 0 || (size_t)k >= cap - at) {
        *cause = "internal";
        return 500;
    }
    return 200;
}

int webui_pkg_icon(const char *path, uint8_t **png, size_t *len, const char **cause) {
    char norm[1024];
    pkg_info_t info;
    uint64_t size = 0;
    int fd = -1, st;
    *png = NULL;
    *len = 0;
    st = read_pkg(path, norm, sizeof norm, &info, &size, &fd, cause);
    if (st != 200) return st;
    if (!info.icon_len) {
        close(fd);
        *cause = "pkg_no_icon";
        return 404;
    }
    *png = malloc(info.icon_len);
    if (!*png) {
        close(fd);
        *cause = "out_of_memory";
        return 500;
    }
    if (fd_read(&fd, info.icon_off, *png, info.icon_len) != 0) {
        close(fd);
        free(*png);
        *png = NULL;
        *cause = "read_failed";
        return 500;
    }
    close(fd);
    *len = info.icon_len;
    return 200;
}

int webui_pkg_install_check(const char *path, int replace, const char **cause) {
    char norm[1024];
    pkg_info_t info;
    uint64_t size = 0;
    int st = read_pkg(path, norm, sizeof norm, &info, &size, NULL, cause);
    if (st != 200) return st;
    if (webui_pkg_installing()) {
        *cause = "pkg_install_busy";
        return 409;
    }
    return install_guard(&info, replace, cause);
}

/* ---- local sockets: the loader (:9021) and the installer daemon (:9115) ---- */

/* A connected TCP socket to 127.0.0.1:port, or -1 with errno. */
static int connect_local(int port, int timeout_s) {
    struct sockaddr_in a;
    struct timeval tv = { timeout_s, 0 };
    int s = socket(AF_INET, SOCK_STREAM, 0);
    if (s < 0) return -1;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    (void)setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    (void)setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    if (connect(s, (struct sockaddr *)&a, sizeof a) != 0) {
        int e = errno;
        close(s);
        errno = e;
        return -1;
    }
    return s;
}

static int send_all(int s, const void *b, size_t n) {
    const char *p = b;
    while (n) {
        ssize_t w = send(s, p, n, MSG_NOSIGNAL);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

/* Streams an image to the loader: from `mem`, or (mem NULL) `len` bytes of `fd` from offset 0.
 * The caller holds g_loader_mu. A refused connect is retried (LOADER_RETRY_MS); nothing is ever
 * opened without sending at once (a loader handed a silent connection waits on it). Half-closes,
 * as the engine does: EOF is the loader's "run it". 0, or -1 with *cause. */
static int loader_send(const uint8_t *mem, int fd, uint64_t len, const char **cause) {
    long long deadline = now_ms() + LOADER_RETRY_MS;
    uint8_t *buf = NULL;
    uint64_t sent = 0;
    int s;
    for (;;) {
        s = connect_local(WEBUI_LOADER_PORT, 30);
        if (s >= 0) break;
        if (errno == ECONNREFUSED && now_ms() < deadline) {
            sleep_ms(LOADER_POLL_MS);
            continue;
        }
        *cause = "loader_unreachable";
        return -1;
    }
    if (!mem) {
        buf = malloc(IO_BLOCK);
        if (!buf) {
            close(s);
            *cause = "out_of_memory";
            return -1;
        }
    }
    while (sent < len) {
        size_t n = len - sent > IO_BLOCK ? IO_BLOCK : (size_t)(len - sent);
        const uint8_t *p = mem ? mem + sent : buf;
        if (!mem && fd_read(&fd, sent, buf, n) != 0) {
            *cause = "read_failed";
            break;
        }
        if (send_all(s, p, n) != 0) {
            *cause = "loader_send_failed";
            break;
        }
        sent += n;
    }
    free(buf);
    (void)shutdown(s, SHUT_WR);
    close(s);
    return sent == len ? 0 : -1;
}

/* One JSON line to the daemon, its one-line answer back (NUL-terminated, newline cut). 0, or -1
 * (not listening: errno ECONNREFUSED; or no answer). */
static int daemon_talk(const char *req, char *reply, size_t cap, int timeout_s) {
    size_t got = 0;
    int s = connect_local(WEBUI_INSTALLER_PORT, timeout_s);
    if (s < 0) return -1;
    if (send_all(s, req, strlen(req)) != 0 || send_all(s, "\n", 1) != 0) {
        close(s);
        return -1;
    }
    while (got + 1 < cap) {
        ssize_t n = recv(s, reply + got, cap - 1 - got, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        got += (size_t)n;
        if (memchr(reply, '\n', got)) break;
    }
    close(s);
    reply[got] = '\0';
    {
        char *nl = strchr(reply, '\n');
        if (nl) *nl = '\0';
    }
    return got ? 0 : -1;
}

static int reply_ok(const char *r) {
    int ok = 0;
    return webui_json_bool(r, strlen(r), "ok", &ok) == 1 && ok;
}

/* The daemon answering hello with at least the embedded version: 1; an older one: 2; none: 0. */
static int daemon_hello(char *reply, size_t cap) {
    char ver[32];
    if (daemon_talk("{\"op\":\"hello\"}", reply, cap, 5) != 0 || !reply_ok(reply)) return 0;
    if (webui_json_str(reply, strlen(reply), "version", ver, sizeof ver) != 1) return 0;
    return pkg_version_older(ver, webui_installer_version) ? 2 : 1;
}

/* The daemon up and current: 0, or -1 with *cause. Sends the embedded one when needed. */
static int ensure_daemon(const char **cause) {
    char *reply = malloc(REPLY_MAX);
    long long until;
    int h;
    if (!reply) {
        *cause = "out_of_memory";
        return -1;
    }
    h = daemon_hello(reply, REPLY_MAX);
    if (h == 1) {
        free(reply);
        return 0;
    }
    if (h == 2) {
        /* An older daemon: stop it (it finishes a serving job's reads first only if asked
         * again later; the engine replaces it the same way), wait for its port to close. */
        (void)daemon_talk("{\"op\":\"stop\"}", reply, REPLY_MAX, 5);
        until = now_ms() + 5000;
        while (now_ms() < until) {
            int s = connect_local(WEBUI_INSTALLER_PORT, 2);
            if (s < 0) break;
            close(s);
            sleep_ms(250);
        }
    }
    pthread_mutex_lock(&g_loader_mu);
    if (loader_send(webui_installer_elf, -1, webui_installer_elf_len, cause) != 0) {
        pthread_mutex_unlock(&g_loader_mu);
        free(reply);
        return -1;
    }
    pthread_mutex_unlock(&g_loader_mu);
    until = now_ms() + DAEMON_UP_MS;
    while (now_ms() < until) {
        sleep_ms(500);
        if (daemon_hello(reply, REPLY_MAX) == 1) {
            free(reply);
            return 0;
        }
    }
    free(reply);
    *cause = "pkg_install_daemon_not_up";
    return -1;
}

/* ---- the install job ---- */

typedef struct {
    ava1_op_ctx_t *c;
} stage_ctx_t;

static int stage_cancelled(void *arg) { return ava1_op_cancelled(((stage_ctx_t *)arg)->c); }
static void stage_file(void *arg, uint64_t bytes) { ava1_op_add(((stage_ctx_t *)arg)->c, 0, bytes); }

/* Copies the package to WEBUI_PKG_STAGE_DIR/<name>. 0, -1 with a message set, -2 cancelled. */
static int stage_copy(ava1_op_ctx_t *c, const char *src, uint64_t size, const pkg_info_t *info, char *dst, size_t cap) {
    char name[64];
    struct statvfs sv;
    stage_ctx_t sc = { c };
    fsj_hooks_t h;
    int rc;
    pkg_stage_name(info->content_id, arc4random(), name, sizeof name);
    (void)mkdir("/user/data/ps5upload", 0777);
    (void)mkdir(WEBUI_PKG_STAGE_DIR, 0777);
    if (snprintf(dst, cap, "%s/%s", WEBUI_PKG_STAGE_DIR, name) >= (int)cap) {
        ava1_op_message(c, "pkg_install_internal");
        return -1;
    }
    if (statvfs(WEBUI_PKG_STAGE_DIR, &sv) == 0) {
        uint64_t avail = (uint64_t)sv.f_bavail * (uint64_t)sv.f_frsize;
        if (avail < size + WEBUI_PKG_STAGE_MARGIN) {
            ava1_op_message(c, "pkg_install_no_space: %llu", (unsigned long long)(size + WEBUI_PKG_STAGE_MARGIN));
            return -1;
        }
    }
    memset(&h, 0, sizeof h);
    h.cancelled = stage_cancelled;
    h.file = stage_file;
    h.arg = &sc;
    ava1_op_message(c, "pkg_install_staging");
    rc = fsj_copy_atomic(src, dst, &h);
    if (rc == -2) return -2;
    if (rc != 0) {
        ava1_op_message(c, "pkg_install_stage_failed");
        return -1;
    }
    return 0;
}

/* The daemon's answer to install: the job id (1), or a failure message set (0). */
static int start_install(ava1_op_ctx_t *c, const char *path, const char *hint, char *job, size_t cap) {
    char *req = malloc(4096), *reply = malloc(REPLY_MAX), err[48];
    size_t at;
    uint64_t code = 0;
    int ok = 0;
    if (!req || !reply) {
        free(req);
        free(reply);
        ava1_op_message(c, "pkg_install_internal");
        return 0;
    }
    at = (size_t)snprintf(req, 4096, "{\"op\":\"install\",\"path\":\"");
    at += webui_json_escape(req + at, 4096 - at, path, strlen(path));
    if (at + 128 < 4096) {
        snprintf(req + at, 4096 - at, "\",\"name_hint\":\"%s\"}", hint);
        if (daemon_talk(req, reply, REPLY_MAX, 120) != 0) {
            ava1_op_message(c, "pkg_install_daemon_lost");
        } else if (reply_ok(reply) && webui_json_str(reply, strlen(reply), "job", job, cap) == 1 && job[0]) {
            ok = 1;
        } else if (webui_json_u64(reply, strlen(reply), "code", &code) == 1) {
            ava1_op_message(c, "pkg_install_refused: 0x%08llX", (unsigned long long)code);
        } else if (webui_json_str(reply, strlen(reply), "error", err, sizeof err) == 1) {
            ava1_op_message(c, "pkg_install_daemon_%s", err);
        } else {
            ava1_op_message(c, "pkg_install_daemon_lost");
        }
    } else {
        ava1_op_message(c, "pkg_install_internal");
    }
    free(req);
    free(reply);
    return ok;
}

/* Follows the daemon's job until Sony has read every byte. 1 complete, 0 failed (message set). */
static int watch_delivery(ava1_op_ctx_t *c, const char *job) {
    char req[128], *reply = malloc(REPLY_MAX), phase[24];
    uint64_t served = 0, total = 0, code = 0, shown = 0;
    long long last_move = now_ms(), last_answer = now_ms();
    int result = -1;
    if (!reply) {
        ava1_op_message(c, "pkg_install_internal");
        return 0;
    }
    snprintf(req, sizeof req, "{\"op\":\"job\",\"job\":\"%s\"}", job);
    while (result < 0) {
        sleep_ms(1000);
        if (daemon_talk(req, reply, REPLY_MAX, 5) == 0 && reply_ok(reply) &&
            webui_json_str(reply, strlen(reply), "phase", phase, sizeof phase) == 1) {
            size_t rl = strlen(reply);
            uint64_t s = 0;
            last_answer = now_ms();
            (void)webui_json_u64(reply, rl, "total", &total);
            (void)webui_json_u64(reply, rl, "code", &code);
            if (webui_json_u64(reply, rl, "bytes_served", &s) == 1 && s > served) {
                served = s;
                last_move = now_ms();
            }
            if (served > shown) {
                ava1_op_add(c, 0, served - shown);
                shown = served;
            }
            if (strcmp(phase, "failed") == 0) {
                ava1_op_message(c, "pkg_install_refused: 0x%08llX", (unsigned long long)code);
                result = 0;
            } else if (strcmp(phase, "done") == 0 || strcmp(phase, "accepted") == 0 || (total > 0 && served >= total)) {
                result = 1;
            } else if (now_ms() - last_move >= STALL_MS) {
                ava1_op_message(c, "pkg_install_stalled");
                result = 0;
            }
        } else if (now_ms() - last_answer >= DAEMON_LOST_MS) {
            ava1_op_message(c, "pkg_install_daemon_lost");
            result = 0;
        }
    }
    free(reply);
    return result;
}

static int op_pkg_install(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char *src = malloc(1024), *norm = malloc(1024), *staged = malloc(1024), *job = malloc(64), result[512];
    pkg_info_t info;
    uint64_t size = 0, replace = 0, remove_after = 0;
    const char *cause = NULL;
    const char *install_path;
    int st, status = AVA1_ERR_IO, delivered = 0, took = 0, rc;
    size_t at = 0;
    (void)arg;
    (void)n;
    if (!src || !norm || !staged || !job) {
        ava1_op_message(c, "pkg_install_internal");
        status = AVA1_ERR_INTERNAL;
        goto out;
    }
    staged[0] = '\0';
    job[0] = '\0';
    if (!fsj_json_str((const char *)args, "path", src, 1024) || !src[0]) {
        ava1_op_message(c, "pkg_install_missing_path");
        status = AVA1_ERR_PROTOCOL;
        goto out;
    }
    (void)fsj_json_u64((const char *)args, "replace", &replace);
    (void)fsj_json_u64((const char *)args, "remove_after", &remove_after);
    st = read_pkg(src, norm, 1024, &info, &size, NULL, &cause);
    if (st != 200) {
        ava1_op_message(c, "%s", cause);
        status = st == 403 ? AVA1_ERR_PATH : AVA1_ERR_IO;
        goto out;
    }
    if (remove_after && !pkg_needs_staging(norm) && !strncmp(norm, WEBUI_PKG_STAGE_DIR "/", sizeof WEBUI_PKG_STAGE_DIR) &&
        !strchr(norm + sizeof WEBUI_PKG_STAGE_DIR, '/')) {
        /* the Library's copy of a package from a share: removed like a staging copy, also when the
         * install is refused (only a file directly in the staging folder, never any other package) */
        snprintf(staged, 1024, "%s", norm);
    }
    {
        int expect = 0;
        if (!__atomic_compare_exchange_n(&g_installing, &expect, 1, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
            ava1_op_message(c, "pkg_install_busy");
            status = AVA1_ERR_BUSY;
            goto out;
        }
        took = 1;
    }
    if (install_guard(&info, replace != 0, &cause) != 200) {
        ava1_op_message(c, "%s", cause);
        status = AVA1_ERR_EXISTS;
        goto out;
    }
    install_path = norm;
    ava1_op_set_total(c, 0, pkg_needs_staging(norm) ? 2 * size : size);
    if (pkg_needs_staging(norm)) {
        rc = stage_copy(c, norm, size, &info, staged, 1024);
        if (rc == -2) {
            staged[0] = '\0';
            ava1_op_message(c, "pkg_install_cancelled");
            status = AVA1_ERR_CANCELLED;
            goto out;
        }
        if (rc != 0) {
            staged[0] = '\0';
            goto out;
        }
        install_path = staged;
    }
    if (ava1_op_cancelled(c)) {
        ava1_op_message(c, "pkg_install_cancelled");
        status = AVA1_ERR_CANCELLED;
        goto out;
    }
    ava1_op_message(c, "pkg_install_starting");
    if (ensure_daemon(&cause) != 0) {
        ava1_op_message(c, "%s", cause);
        goto out;
    }
    /* From here Sony owns the install: a Stop no longer stops it (it cannot be stopped, and
     * cutting the source mid-read would leave a broken title). */
    if (!start_install(c, install_path, valid_title_id(info.title_id) ? info.title_id : "", job, 64)) goto out;
    ava1_op_message(c, "pkg_install_installing");
    delivered = watch_delivery(c, job);
    if (!delivered) goto out;
    {
        const char *verdict = "delivered";
        if (is_game(&info) && valid_title_id(info.title_id)) {
            long long until = now_ms() + CONTENT_WAIT_MS;
            ava1_op_message(c, "pkg_install_finishing");
            while (now_ms() < until && !app_installed(info.title_id)) sleep_ms(2000);
            if (app_installed(info.title_id)) verdict = "installed";
        }
        at = (size_t)snprintf(result, sizeof result, "{\"verdict\":\"%s\",\"title_id\":\"%s\",\"kind\":\"%s\",\"content_id\":\"",
                              verdict, valid_title_id(info.title_id) ? info.title_id : "", pkg_info_kind(&info));
        if (at < sizeof result) at += webui_json_escape(result + at, sizeof result - at, info.content_id, strlen(info.content_id));
        if (at + 3 <= sizeof result) {
            memcpy(result + at, "\"}", 3);
            (void)ava1_op_set_result(c, result, at + 2);
        }
        ava1_op_message(c, "pkg_install_%s", verdict);
        status = AVA1_STATUS_OK;
    }
out:
    /* The staging copy goes once Sony has read all of it; on a failure after the daemon took it,
     * it stays for a retry (the PC app's stale sweep of pkg_temp removes it later). A failure before
     * that (no daemon, a refusal) leaves nothing Sony is reading: remove it. */
    if (staged && staged[0] && (delivered || !job || !job[0])) (void)unlink(staged);
    if (took) __atomic_store_n(&g_installing, 0, __ATOMIC_RELEASE);
    free(src);
    free(norm);
    free(staged);
    free(job);
    return status;
}

void webui_pkg_register_ops(void) { (void)ava1_op_register(AVA1_JOB_OP_PKG_INSTALL, op_pkg_install, NULL); }

/* ---- /api/payload/load ---- */

/* 1 when the file carries the ps5upload helper's marker. -1 on a read error. */
static int is_helper_image(int fd, uint64_t size) {
    uint8_t *buf = malloc(IO_BLOCK);
    char carry[16];
    size_t have = 0;
    uint64_t off = 0;
    int hit = 0;
    if (!buf) return -1;
    while (off < size && !hit) {
        size_t n = size - off > IO_BLOCK ? IO_BLOCK : (size_t)(size - off);
        if (fd_read(&fd, off, buf, n) != 0) {
            free(buf);
            return -1;
        }
        hit = pkg_scan_marker(buf, n, carry, &have);
        off += n;
    }
    free(buf);
    return hit;
}

int webui_payload_load(const char *path, uint64_t *sent, const char **cause) {
    char norm[1024];
    struct stat st;
    uint8_t magic[4];
    int http = 0, fd, helper;
    *sent = 0;
    if (!pkg_is_payload_name(path)) {
        *cause = "payload_not_payload";
        return 400;
    }
    fd = open_checked(path, norm, sizeof norm, &st, &http, cause);
    if (fd < 0) return http;
    if (st.st_size < 64 || fd_read(&fd, 0, magic, 4) != 0 || memcmp(magic, "\x7f" "ELF", 4) != 0) {
        close(fd);
        *cause = "payload_not_elf";
        return 400;
    }
    if ((uint64_t)st.st_size > WEBUI_PAYLOAD_MAX) {
        close(fd);
        *cause = "payload_too_large";
        return 413;
    }
    helper = is_helper_image(fd, (uint64_t)st.st_size);
    if (helper != 0) {
        close(fd);
        *cause = helper > 0 ? "payload_is_helper" : "read_failed";
        return helper > 0 ? 409 : 500;
    }
    if (webui_pkg_installing()) {
        close(fd);
        *cause = "pkg_install_busy";
        return 409;
    }
    pthread_mutex_lock(&g_loader_mu);
    if (loader_send(NULL, fd, (uint64_t)st.st_size, cause) != 0) {
        pthread_mutex_unlock(&g_loader_mu);
        close(fd);
        return strcmp(*cause, "loader_unreachable") == 0 ? 502 : 500;
    }
    pthread_mutex_unlock(&g_loader_mu);
    close(fd);
    *sent = (uint64_t)st.st_size;
    return 200;
}
