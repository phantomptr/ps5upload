/* JOB_OP_APP_DUMP: the dump itself (see include/app_dump.h). Pure file work, built for the host
 * tests too (payload/tests/app_dump_selftest.c); the job glue is app_dump.c, the console-only SELF
 * decryption app_dump_self.c.
 *
 * Ported from ps5-app-dumper (https://github.com/EchoStretch/ps5-app-dumper): ps5_dumper.c,
 * ps4_dumper.c, decrypt.c, utils.c (copy_dir_recursive_tracked, read_npwr_id) and app_scan.c (the
 * DLC folders). Changes: progress, cancel and errors go to the AVA1 job instead of notifications
 * and a log; a copy that fails fails the dump; the free space is checked first; names that come
 * from a package or an image may not leave the dump folder.
 *
 * Copyright (C) 2025 EchoStretch, slopmaster33
 * Copyright (C) 2026 PhantomPtr (the port)
 *
 * This program is free software; you can redistribute it and/or modify it under the terms of the
 * GNU General Public License as published by the Free Software Foundation; either version 3, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY;
 * without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See
 * the GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License along with this program; see
 * the file LICENSE. If not, see <http://www.gnu.org/licenses/>. */
#include "app_dump.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include "statvfs_avail.h"
#include <time.h>
#include <unistd.h>

#include "app_dump_int.h"
#include "webui_codec.h"

const char *app_dump_root = "";

#define AD_MAX_DEPTH 48
/* sce_sys extras (package entries, appmeta, trophies) on top of the counted trees. */
#define AD_SPACE_MARGIN (64ull * 1024 * 1024)
#define AD_PART ".ps5u-part"

/* ---- shared helpers (app_dump_int.h) ---- */

static uint8_t *g_buf;

uint8_t *ad_buf(void) {
    if (!g_buf) g_buf = malloc(AD_BUF_SIZE);
    return g_buf;
}

void ad_buf_release(void) {
    free(g_buf);
    g_buf = NULL;
}

int ad_cancelled(const app_dump_hooks_t *h) { return h && h->cancelled && h->cancelled(h->arg); }

void ad_add(const app_dump_hooks_t *h, uint64_t files, uint64_t bytes) {
    if (h && h->add && (files || bytes)) h->add(h->arg, files, bytes);
}

static void note(const app_dump_hooks_t *h, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void note(const app_dump_hooks_t *h, const char *fmt, ...) {
    char msg[300];
    va_list ap;
    if (!h || !h->note) return;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof msg, fmt, ap);
    va_end(ap);
    h->note(h->arg, msg);
}

static int is_dir(const char *p) {
    struct stat st;
    return stat(p, &st) == 0 && S_ISDIR(st.st_mode);
}

static int is_file(const char *p) {
    struct stat st;
    return stat(p, &st) == 0 && S_ISREG(st.st_mode);
}

int ad_mkdirs(const char *path) {
    char tmp[APP_DUMP_PATH_MAX + 128];
    size_t n = strlen(path);
    char *p;
    if (n == 0 || n >= sizeof tmp) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(tmp, path, n + 1);
    for (p = tmp + 1;; p++) {
        if (*p == '/' || *p == '\0') {
            char c = *p;
            *p = '\0';
            /* An existing folder is never mkdir()ed: a read-only mount point answers EROFS. */
            if (!is_dir(tmp) && mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
            *p = c;
            if (!c) break;
        }
    }
    return is_dir(tmp) ? 0 : -1;
}

int ad_mkparent(const char *file) {
    char dir[APP_DUMP_PATH_MAX + 128];
    const char *slash = strrchr(file, '/');
    if (!slash || slash == file) return 0;
    if ((size_t)(slash - file) >= sizeof dir) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(dir, file, (size_t)(slash - file));
    dir[slash - file] = '\0';
    return ad_mkdirs(dir);
}

int ad_write_all(int fd, const void *buf, size_t n) {
    const uint8_t *p = buf;
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) {
            if (w == 0) errno = EIO;
            return -1;
        }
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

int ad_pwrite_all(int fd, const void *buf, size_t n, off_t off) {
    const uint8_t *p = buf;
    while (n) {
        ssize_t w = pwrite(fd, p, n, off);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) {
            if (w == 0) errno = EIO;
            return -1;
        }
        p += w;
        n -= (size_t)w;
        off += w;
    }
    return 0;
}

int ad_pread_all(int fd, void *buf, size_t n, off_t off) {
    uint8_t *p = buf;
    while (n) {
        ssize_t r = pread(fd, p, n, off);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) {
            if (r == 0) errno = EIO; /* the file ended early */
            return -1;
        }
        p += r;
        n -= (size_t)r;
        off += r;
    }
    return 0;
}

int ad_copy_range(int in_fd, uint64_t off, uint64_t size, const char *dst, const app_dump_hooks_t *h, int track) {
    uint8_t *buf = ad_buf();
    uint64_t done = 0;
    int out, rc = 0;
    if (!buf) {
        errno = ENOMEM;
        return -1;
    }
    out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out < 0) return -1;
    while (done < size) {
        size_t n = size - done > AD_BUF_SIZE ? AD_BUF_SIZE : (size_t)(size - done);
        /* A single file runs to tens of GB: a stop has to reach it here. */
        if (ad_cancelled(h)) {
            rc = -2;
            break;
        }
        if (ad_pread_all(in_fd, buf, n, (off_t)(off + done)) != 0 || ad_write_all(out, buf, n) != 0) {
            rc = -1;
            break;
        }
        done += n;
        if (track) ad_add(h, 0, n);
    }
    if (close(out) != 0 && rc == 0) rc = -1; /* a full drive can surface only here */
    return rc;
}

int ad_safe_relpath(const char *name) {
    const char *p = name;
    if (!name || !*name || *name == '/') return 0;
    while (*p) {
        const char *e = strchr(p, '/');
        size_t len = e ? (size_t)(e - p) : strlen(p);
        if (len == 0 || (len == 1 && p[0] == '.') || (len == 2 && p[0] == '.' && p[1] == '.')) return 0;
        if (!e) break;
        p = e + 1;
        if (!*p) return 0; /* a trailing slash */
    }
    return 1;
}

/* A console path under app_dump_root. 0, or -1 when it does not fit. */
static int cpath(char *out, size_t cap, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static int cpath(char *out, size_t cap, const char *fmt, ...) {
    va_list ap;
    int n = snprintf(out, cap, "%s", app_dump_root);
    int m;
    if (n < 0 || (size_t)n >= cap) return -1;
    va_start(ap, fmt);
    m = vsnprintf(out + n, cap - (size_t)n, fmt, ap);
    va_end(ap);
    return m < 0 || (size_t)m >= cap - (size_t)n ? -1 : 0;
}

static int join(char *out, size_t cap, const char *a, const char *b) {
    int n = snprintf(out, cap, "%s/%s", a, b);
    if (n < 0 || (size_t)n >= cap) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

/* ---- args, names, result ---- */

int app_dump_title_ok(const char *id) {
    int i;
    if (!id || strlen(id) != 9) return 0;
    for (i = 0; i < 4; i++)
        if (id[i] < 'A' || id[i] > 'Z') return 0;
    for (; i < 9; i++)
        if (id[i] < '0' || id[i] > '9') return 0;
    return 1;
}

int app_dump_is_ps4(const char *id) { return strncmp(id, "CUSA", 4) == 0; }

const char *app_dump_parse_args(const char *json, size_t n, app_dump_args_t *a) {
    int v;
    memset(a, 0, sizeof *a);
    if (webui_json_str(json, n, "title_id", a->title_id, sizeof a->title_id) != 1 || !app_dump_title_ok(a->title_id))
        return "dump_bad_title_id";
    if (webui_json_str(json, n, "dest", a->dest, sizeof a->dest) != 1 || a->dest[0] != '/') return "dump_bad_dest";
    if ((v = webui_json_bool(json, n, "decrypt", &a->decrypt)) < 0) return "dump_bad_args";
    if (v == 0) a->decrypt = 0;
    if ((v = webui_json_bool(json, n, "fself", &a->fself)) < 0) return "dump_bad_args";
    if (v == 0) a->fself = 0;
    if ((v = webui_json_bool(json, n, "dlc", &a->dlc)) < 0) return "dump_bad_args";
    if (v == 0) a->dlc = 1;
    return NULL;
}

int app_dump_out_path(const app_dump_args_t *a, char *out, size_t cap) {
    size_t dl = strlen(a->dest);
    int n;
    while (dl > 1 && a->dest[dl - 1] == '/') dl--;
    n = snprintf(out, cap, "%.*s/%s%s", (int)dl, a->dest, a->title_id, app_dump_is_ps4(a->title_id) ? "" : "-app");
    return n < 0 || (size_t)n >= cap ? -1 : 0;
}

int app_dump_dlc_name_ok(const char *name, const char *title_id) {
    char mark[24];
    size_t len = strlen(name);
    if (len < 20 || len >= 96 || strchr(name, '/')) return 0;
    snprintf(mark, sizeof mark, "-%s_00-", title_id);
    if (!strstr(name, mark)) return 0;
    return strcmp(name + len - 3, "-ac") == 0;
}

static int put_str(char *out, size_t cap, size_t *at, const char *s) {
    size_t need;
    if (*at + 2 >= cap) return -1;
    out[(*at)++] = '"';
    need = webui_json_escape(out + *at, cap - *at, s, strlen(s));
    if (need + 2 >= cap - *at) return -1;
    *at += need;
    out[(*at)++] = '"';
    out[*at] = '\0';
    return 0;
}

static int put_raw(char *out, size_t cap, size_t *at, const char *fmt, ...) __attribute__((format(printf, 4, 5)));
static int put_raw(char *out, size_t cap, size_t *at, const char *fmt, ...) {
    va_list ap;
    int n;
    if (*at >= cap) return -1;
    va_start(ap, fmt);
    n = vsnprintf(out + *at, cap - *at, fmt, ap);
    va_end(ap);
    if (n < 0 || (size_t)n >= cap - *at) return -1;
    *at += (size_t)n;
    return 0;
}

int app_dump_result_json(const app_dump_result_t *r, const char *title_id, long long finished, int info,
                         char *out, size_t cap) {
    size_t at = 0;
    unsigned i;
    if (put_raw(out, cap, &at, "{") != 0) return -1;
    if (info && (put_raw(out, cap, &at, "\"title_id\":") != 0 || put_str(out, cap, &at, title_id) != 0 ||
                 put_raw(out, cap, &at, ",") != 0))
        return -1;
    if (put_raw(out, cap, &at, "\"path\":") != 0 || put_str(out, cap, &at, r->path) != 0) return -1;
    if (put_raw(out, cap, &at, ",\"files\":%llu,\"bytes\":%llu,\"decrypted\":%llu,\"decrypt_failed\":%llu,\"decrypt_skipped\":",
                (unsigned long long)r->files, (unsigned long long)r->bytes, (unsigned long long)r->decrypted,
                (unsigned long long)r->decrypt_failed) != 0 ||
        put_str(out, cap, &at, r->decrypt_skipped) != 0 || put_raw(out, cap, &at, ",\"dlc\":[") != 0)
        return -1;
    for (i = 0; i < r->dlc_count && i < APP_DUMP_MAX_DLC; i++)
        if ((i && put_raw(out, cap, &at, ",") != 0) || put_str(out, cap, &at, r->dlc[i]) != 0) return -1;
    if (put_raw(out, cap, &at, "],\"fw\":") != 0 || put_str(out, cap, &at, r->fw) != 0) return -1;
    if (info && put_raw(out, cap, &at, ",\"finished\":%lld", finished) != 0) return -1;
    if (put_raw(out, cap, &at, "}") != 0) return -1;
    return (int)at;
}

static int write_text(const char *path, const char *text, size_t n) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    int rc;
    if (fd < 0) return -1;
    rc = ad_write_all(fd, text, n);
    if (close(fd) != 0) rc = -1;
    return rc;
}

int app_dump_write_info(const app_dump_args_t *a, const app_dump_result_t *r, long long finished) {
    char path[APP_DUMP_PATH_MAX + 32];
    char *body = malloc(16384);
    int n, rc = -1;
    if (!body) return -1;
    n = app_dump_result_json(r, a->title_id, finished, 1, body, 16384 - 1);
    if (n >= 0 && join(path, sizeof path, r->path, "dump-info.json") == 0) {
        body[n++] = '\n';
        rc = write_text(path, body, (size_t)n);
    }
    free(body);
    return rc;
}

/* ---- trees ---- */

typedef struct {
    const app_dump_args_t *a;
    const app_dump_hooks_t *h;
    app_dump_result_t *r;
    int decrypt_on; /* decrypt asked and this firmware can */
    int fself_on;
    const char *root_out; /* the folder decrypted/ goes under */
} dump_t;

/* snprintf that may cut its output short on purpose (a message, a name already bounded). */
static void fmt_cut(char *dst, size_t cap, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void fmt_cut(char *dst, size_t cap, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(dst, cap, fmt, ap);
    va_end(ap);
}

static int fail_io(dump_t *d, const char *what, const char *path) {
    int e = errno;
    if (!d->r->cause[0]) fmt_cut(d->r->cause, sizeof d->r->cause, "dump_io_failed: %s %s: %s", what, path, strerror(e));
    errno = e;
    return -1;
}

static int is_self_name(const char *name) {
    const char *ext = strrchr(name, '.');
    return ext && (!strcasecmp(ext, ".elf") || !strcasecmp(ext, ".self") || !strcasecmp(ext, ".prx") ||
                   !strcasecmp(ext, ".sprx") || !strcasecmp(ext, ".bin"));
}

typedef struct {
    uint64_t files, bytes, self_files, self_bytes;
} sizes_t;

/* Files and bytes under `dir` (and the SELF candidates among them). 0, -1 too deep/unreadable
 * entries are simply not counted, -2 cancelled. */
static int size_tree(const app_dump_hooks_t *h, const char *dir, int depth, sizes_t *s) {
    char path[APP_DUMP_PATH_MAX];
    DIR *dp;
    struct dirent *e;
    struct stat st;
    int rc = 0;
    if (depth > AD_MAX_DEPTH || !(dp = opendir(dir))) return 0;
    while ((e = readdir(dp)) != NULL) {
        if (ad_cancelled(h)) {
            rc = -2;
            break;
        }
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (join(path, sizeof path, dir, e->d_name) != 0 || stat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            if ((rc = size_tree(h, path, depth + 1, s)) != 0) break;
        } else if (S_ISREG(st.st_mode)) {
            s->files++;
            s->bytes += (uint64_t)st.st_size;
            if (is_self_name(e->d_name)) {
                s->self_files++;
                s->self_bytes += (uint64_t)st.st_size;
            }
        }
    }
    closedir(dp);
    return rc;
}

/* One file. 0, -1 (cause set), -2 cancelled. */
static int copy_file(dump_t *d, const char *src, const char *dst, int track) {
    struct stat st;
    int fd = open(src, O_RDONLY), rc;
    if (fd < 0) return fail_io(d, "open", src);
    if (fstat(fd, &st) != 0) {
        close(fd);
        return fail_io(d, "stat", src);
    }
    if (!S_ISREG(st.st_mode)) {
        close(fd);
        errno = EINVAL;
        return fail_io(d, "stat", src);
    }
    rc = ad_copy_range(fd, 0, (uint64_t)st.st_size, dst, d->h, track);
    close(fd);
    if (rc == -1) return fail_io(d, "copy", dst);
    if (rc == 0) {
        d->r->files++;
        d->r->bytes += (uint64_t)st.st_size;
        if (track) ad_add(d->h, 1, 0);
    }
    return rc;
}

/* `src` copied into `dst` (made). 0, -1 (cause set), -2 cancelled. */
static int copy_tree(dump_t *d, const char *src, const char *dst, int depth, int track) {
    char sp[APP_DUMP_PATH_MAX], dp_[APP_DUMP_PATH_MAX];
    DIR *dp;
    struct dirent *e;
    struct stat st;
    int rc = 0;
    if (depth > AD_MAX_DEPTH) {
        errno = ELOOP;
        return fail_io(d, "walk", src);
    }
    if (ad_mkdirs(dst) != 0) return fail_io(d, "mkdir", dst);
    if (!(dp = opendir(src))) return fail_io(d, "opendir", src);
    while ((e = readdir(dp)) != NULL) {
        if (ad_cancelled(d->h)) {
            rc = -2;
            break;
        }
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (join(sp, sizeof sp, src, e->d_name) != 0 || join(dp_, sizeof dp_, dst, e->d_name) != 0) {
            rc = fail_io(d, "path", src);
            break;
        }
        if (stat(sp, &st) != 0) {
            rc = fail_io(d, "stat", sp);
            break;
        }
        if (S_ISDIR(st.st_mode)) rc = copy_tree(d, sp, dp_, depth + 1, track);
        else if (S_ISREG(st.st_mode)) rc = copy_file(d, sp, dp_, track);
        if (rc != 0) break;
    }
    closedir(dp);
    return rc;
}

/* An extra that may be missing or unreadable: only a stop counts. */
static int extra(int rc) { return rc == -2 ? -2 : 0; }

static int extra_tree(dump_t *d, const char *src, const char *dst) {
    char keep[sizeof d->r->cause];
    int rc;
    if (!is_dir(src)) return 0;
    memcpy(keep, d->r->cause, sizeof keep);
    rc = copy_tree(d, src, dst, 0, 0);
    if (rc == -1) fprintf(stderr, "app_dump: %s (kept going)\n", d->r->cause);
    memcpy(d->r->cause, keep, sizeof keep);
    return extra(rc);
}

static int extra_file(dump_t *d, const char *src, const char *dst) {
    char keep[sizeof d->r->cause];
    int rc;
    if (!is_file(src)) return 0;
    memcpy(keep, d->r->cause, sizeof keep);
    rc = ad_mkparent(dst) == 0 ? copy_file(d, src, dst, 0) : -1;
    if (rc == -1) fprintf(stderr, "app_dump: %s -> %s failed (kept going)\n", src, dst);
    memcpy(d->r->cause, keep, sizeof keep);
    return extra(rc);
}

/* ---- SELF decryption (decrypt.c's process_file / decrypt_and_process_all) ---- */

static int cancel_tramp(void *arg) { return ad_cancelled((const app_dump_hooks_t *)arg); }

/* 0 go on, 1 stop decrypting (the firmware turned out unsupported), -2 cancelled. */
static int decrypt_file(dump_t *d, const char *in_path, const char *out_path) {
    char part[APP_DUMP_PATH_MAX + 16], dec[APP_DUMP_PATH_MAX], fpart[APP_DUMP_PATH_MAX + 16];
    const char *rel = out_path + strlen(d->root_out);
    struct stat st;
    int in, out, rc, ret = 0;
    while (*rel == '/') rel++;
    note(d->h, "decrypting %s", rel);
    in = open(in_path, O_RDONLY);
    if (in < 0 || fstat(in, &st) != 0) {
        if (in >= 0) close(in);
        d->r->decrypt_failed++;
        return 0;
    }
    if (d->fself_on) {
        /* The plain ELF is kept under decrypted/, the dump gets the fake-signed SELF. */
        if (snprintf(dec, sizeof dec, "%s/decrypted/%s", d->root_out, rel) >= (int)sizeof dec) goto failed_in;
    } else {
        snprintf(dec, sizeof dec, "%s", out_path);
    }
    snprintf(part, sizeof part, "%s" AD_PART, dec);
    if (ad_mkparent(part) != 0) goto failed_in;
    out = open(part, O_RDWR | O_CREAT | O_TRUNC, 0666);
    if (out < 0) goto failed_in;
    rc = app_dump_platform.decrypt_self ? app_dump_platform.decrypt_self(in, out, cancel_tramp, (void *)d->h)
                                        : APP_DUMP_SELF_UNSUPPORTED;
    close(in);
    if (close(out) != 0 && rc == APP_DUMP_SELF_OK) rc = APP_DUMP_SELF_FAILED;
    /* rename() only within one folder: never across mounts. */
    if (rc == APP_DUMP_SELF_OK && rename(part, dec) != 0) rc = APP_DUMP_SELF_FAILED;
    if (rc != APP_DUMP_SELF_OK) (void)unlink(part);
    switch (rc) {
    case APP_DUMP_SELF_OK:
        d->r->decrypted++;
        if (d->fself_on) {
            snprintf(fpart, sizeof fpart, "%s" AD_PART, out_path);
            if (app_dump_elf2fself(dec, fpart) == 0 && rename(fpart, out_path) == 0) break;
            (void)unlink(fpart);
            /* As ps5-app-dumper does: no fself, the dump keeps the decrypted ELF. */
            fprintf(stderr, "app_dump: elf2fself %s failed; the dump keeps the decrypted ELF\n", dec);
            if (extra_file(d, dec, out_path) == -2) ret = -2;
        }
        break;
    case APP_DUMP_SELF_NOT_SELF: break;
    case APP_DUMP_SELF_CANCELLED: ret = -2; break;
    case APP_DUMP_SELF_UNSUPPORTED:
        snprintf(d->r->decrypt_skipped, sizeof d->r->decrypt_skipped, "dump_decrypt_unsupported_fw");
        ret = 1;
        break;
    case APP_DUMP_SELF_TOO_BIG:
        fprintf(stderr, "app_dump: dump_decrypt_too_big: %s left encrypted\n", in_path);
        d->r->decrypt_failed++;
        break;
    default:
        fprintf(stderr, "app_dump: decrypting %s failed; left encrypted\n", in_path);
        d->r->decrypt_failed++;
        break;
    }
    ad_add(d->h, 1, (uint64_t)st.st_size);
    return ret;
failed_in:
    close(in);
    d->r->decrypt_failed++;
    ad_add(d->h, 1, (uint64_t)st.st_size);
    return 0;
}

/* Every SELF candidate under `src` (the mounted title) decrypted over its copy under `dst`. */
static int decrypt_tree(dump_t *d, const char *src, const char *dst, int depth) {
    char sp[APP_DUMP_PATH_MAX], dp_[APP_DUMP_PATH_MAX];
    DIR *dp;
    struct dirent *e;
    struct stat st;
    int rc = 0;
    if (!d->decrypt_on || depth > AD_MAX_DEPTH || !(dp = opendir(src))) return 0;
    while ((e = readdir(dp)) != NULL && d->decrypt_on) {
        if (ad_cancelled(d->h)) {
            rc = -2;
            break;
        }
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (join(sp, sizeof sp, src, e->d_name) != 0 || join(dp_, sizeof dp_, dst, e->d_name) != 0) continue;
        if (stat(sp, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            rc = decrypt_tree(d, sp, dp_, depth + 1);
        } else if (S_ISREG(st.st_mode) && is_self_name(e->d_name)) {
            rc = decrypt_file(d, sp, dp_);
            if (rc == 1) {
                d->decrypt_on = 0;
                rc = 0;
            }
        }
        if (rc != 0) break;
    }
    closedir(dp);
    return rc;
}

/* ---- package entries, appmeta, trophies ---- */

static const char *const k_app_pkgs[] = {"/user/app/%s/app.pkg", "/mnt/ext1/user/app/%s/app.pkg",
                                         "/mnt/ext0/user/app/%s/app.pkg", NULL};
static const char *const k_patch_pkgs[] = {"/user/patch/%s/patch.pkg", "/mnt/ext1/user/patch/%s/patch.pkg",
                                           "/mnt/ext0/user/patch/%s/patch.pkg", NULL};

/* The first package of `tmpls` (formatted with id) that exists. 1 found. */
static int find_pkg(char *out, size_t cap, const char *const *tmpls, const char *id, int want_ps4) {
    int i;
    for (i = 0; tmpls[i]; i++) {
        if (cpath(out, cap, tmpls[i], id) != 0 || !is_file(out)) continue;
        if (want_ps4 && !app_dump_is_ps4_pkg(out)) continue;
        return 1;
    }
    return 0;
}

int app_dump_read_npwr(const char *npbind, char *out, size_t cap) {
    unsigned char buf[4096];
    ssize_t n;
    size_t i, j;
    int fd = open(npbind, O_RDONLY);
    if (!out || cap < 2 || fd < 0) {
        if (fd >= 0) close(fd);
        return -1;
    }
    n = read(fd, buf, sizeof buf);
    close(fd);
    if (n < 9) return -1;
    for (i = 0; i + 9 <= (size_t)n; i++) {
        if (memcmp(buf + i, "NPWR", 4) != 0) continue;
        for (j = 0; j < cap - 1 && i + j < (size_t)n && isprint(buf[i + j]) && buf[i + j] != '/'; j++)
            out[j] = (char)buf[i + j];
        out[j] = '\0';
        return 0;
    }
    return -1;
}

/* ps5_dumper.c steps 7-9: appmeta (user, then system) into sce_sys, then the trophy and UDS files
 * that npbind.dat names. */
static int ps5_extras(dump_t *d, const char *out) {
    char src[APP_DUMP_PATH_MAX], dst[APP_DUMP_PATH_MAX], npbind[APP_DUMP_PATH_MAX], npwr[32];
    const char *id = d->a->title_id;
    if (join(dst, sizeof dst, out, "sce_sys") != 0) return 0;
    if (cpath(src, sizeof src, "/user/appmeta/%s", id) == 0 && extra_tree(d, src, dst) == -2) return -2;
    if (cpath(src, sizeof src, "/system_data/priv/appmeta/%s", id) == 0 && extra_tree(d, src, dst) == -2) return -2;
    if (join(dst, sizeof dst, out, "sce_sys/trophy2") == 0) (void)ad_mkdirs(dst);
    if (join(dst, sizeof dst, out, "sce_sys/uds") == 0) (void)ad_mkdirs(dst);
    if (cpath(npbind, sizeof npbind, "/system_data/priv/appmeta/%s/trophy2/npbind.dat", id) != 0) return 0;
    if (!is_file(npbind) && cpath(npbind, sizeof npbind, "/system_data/priv/appmeta/%s/uds/npbind.dat", id) != 0)
        return 0;
    if (app_dump_read_npwr(npbind, npwr, sizeof npwr) != 0 || !npwr[0]) return 0;
    if (cpath(src, sizeof src, "/user/trophy2/nobackup/conf/%s/TROPHY.UCP", npwr) == 0 &&
        join(dst, sizeof dst, out, "sce_sys/trophy2/trophy00.ucp") == 0 && extra_file(d, src, dst) == -2)
        return -2;
    if (cpath(src, sizeof src, "/user/np_uds/nobackup/conf/%s/uds.ucp", npwr) == 0 &&
        join(dst, sizeof dst, out, "sce_sys/uds/uds00.ucp") == 0 && extra_file(d, src, dst) == -2)
        return -2;
    return 0;
}

static int ps5_dump(dump_t *d, const char *out) {
    char pkg[APP_DUMP_PATH_MAX], src[APP_DUMP_PATH_MAX];
    const char *id = d->a->title_id;
    int rc;
    if (find_pkg(pkg, sizeof pkg, k_app_pkgs, id, 0)) {
        note(d->h, "extracting the package's sce_sys");
        if (app_dump_unpkg_ps5(pkg, out, d->h) == -2) return -2;
    }
    if (cpath(src, sizeof src, "/mnt/sandbox/pfsmnt/%s-app0", id) != 0) return fail_io(d, "path", id);
    note(d->h, "copying");
    if ((rc = copy_tree(d, src, out, 0, 1)) != 0) return rc;
    note(d->h, "copying appmeta and trophies");
    if ((rc = ps5_extras(d, out)) != 0) return rc;
    if (d->decrypt_on) return decrypt_tree(d, src, out, 0);
    return 0;
}

static int ps4_meta(dump_t *d, const char *out) {
    static const char *const names[] = {"nptitle.dat", "npbind.dat"};
    char src[APP_DUMP_PATH_MAX], dst[APP_DUMP_PATH_MAX];
    unsigned i;
    for (i = 0; i < 2; i++) {
        if (cpath(src, sizeof src, "/system_data/priv/appmeta/%s/%s", d->a->title_id, names[i]) != 0 ||
            snprintf(dst, sizeof dst, "%s/sce_sys/%s", out, names[i]) >= (int)sizeof dst)
            continue;
        if (extra_file(d, src, dst) == -2) return -2;
    }
    return 0;
}

/* One block of ps4_dumper.c: the package's sce_sys, nptitle/npbind, the nested image (or, with
 * none, the mounted tree; ps5-app-dumper copies nothing then), then its SELFs decrypted. */
static int ps4_block(dump_t *d, const char *out, const char *const *pkgs, const char *suffix) {
    char pkg[APP_DUMP_PATH_MAX], img[APP_DUMP_PATH_MAX], mnt[APP_DUMP_PATH_MAX], sce[APP_DUMP_PATH_MAX];
    const char *id = d->a->title_id;
    int rc;
    if (find_pkg(pkg, sizeof pkg, pkgs, id, 1)) {
        note(d->h, "extracting the %s package's sce_sys", suffix);
        if (app_dump_unpkg_ps4(pkg, out, d->h) == -2) return -2;
    }
    if (join(sce, sizeof sce, out, "sce_sys") != 0 || ad_mkdirs(sce) != 0) return fail_io(d, "mkdir", sce);
    if ((rc = ps4_meta(d, out)) != 0) return rc;
    if (cpath(img, sizeof img, "/mnt/sandbox/pfsmnt/%s-%s-nest/pfs_image.dat", id, suffix) != 0 ||
        cpath(mnt, sizeof mnt, "/mnt/sandbox/pfsmnt/%s-%s", id, suffix) != 0)
        return fail_io(d, "path", id);
    note(d->h, "copying %s", suffix);
    if (is_file(img)) {
        uint64_t f = 0, b = 0;
        rc = app_dump_unpfs(img, out, 0, d->h, &f, &b);
        if (rc == -1) return fail_io(d, "unpfs", img);
        if (rc == -2) return -2;
        d->r->files += f;
        d->r->bytes += b;
    } else if (is_dir(mnt)) {
        if ((rc = copy_tree(d, mnt, out, 0, 1)) != 0) return rc;
    }
    if (d->decrypt_on && is_dir(mnt)) return decrypt_tree(d, mnt, out, 0);
    return 0;
}

static int ps4_dump(dump_t *d, const char *out) {
    char npbind[APP_DUMP_PATH_MAX], npwr[32], src[APP_DUMP_PATH_MAX], dst[APP_DUMP_PATH_MAX];
    int rc;
    if ((rc = ps4_block(d, out, k_app_pkgs, "app0")) != 0) return rc;
    if ((rc = ps4_block(d, out, k_patch_pkgs, "patch0")) != 0) return rc;
    if (join(npbind, sizeof npbind, out, "sce_sys/npbind.dat") == 0 &&
        app_dump_read_npwr(npbind, npwr, sizeof npwr) == 0 && npwr[0] &&
        cpath(src, sizeof src, "/user/trophy/conf/%s/TROPHY.TRP", npwr) == 0 &&
        join(dst, sizeof dst, out, "sce_sys/trophy/trophy00.trp") == 0)
        return extra_file(d, src, dst);
    return 0;
}

/* ps5_dumper.c's dump_dlc_folder + dump_dlc_metadata: the mounted DLC, then its ac.pkg's sce_sys
 * and its appmeta. */
static int dlc_dump(dump_t *d, const char *name, const char *out) {
    static const char *const pkgs[] = {"/user/addcont/%s/%.*s/ac.pkg", "/mnt/ext1/user/addcont/%s/%.*s/ac.pkg",
                                       "/mnt/ext0/user/addcont/%s/%.*s/ac.pkg", NULL};
    char src[APP_DUMP_PATH_MAX], pkg[APP_DUMP_PATH_MAX], sce[APP_DUMP_PATH_MAX];
    const char *id = d->a->title_id, *label = strstr(name, "_00-");
    int label_len, i, rc;
    if (cpath(src, sizeof src, "/mnt/sandbox/pfsmnt/%s", name) != 0) return fail_io(d, "path", name);
    note(d->h, "copying DLC %s", name);
    if ((rc = copy_tree(d, src, out, 0, 1)) != 0) return rc;
    /* The label between _00- and -ac is what the content is called. */
    label = label ? label + 4 : name;
    label_len = (int)strlen(label) - 3;
    if (label_len <= 0) return 0;
    for (i = 0; pkgs[i]; i++) {
        if (cpath(pkg, sizeof pkg, pkgs[i], id, label_len, label) != 0 || !is_file(pkg)) continue;
        rc = app_dump_is_ps4_pkg(pkg) ? app_dump_unpkg_ps4(pkg, out, d->h) : app_dump_unpkg_ps5(pkg, out, d->h);
        if (rc == -2) return -2;
        break;
    }
    if (cpath(src, sizeof src, "/user/appmeta/addcont/%s/%.*s", id, label_len, label) == 0 &&
        join(sce, sizeof sce, out, "sce_sys") == 0)
        return extra_tree(d, src, sce);
    return 0;
}

/* ---- the run ---- */

static void mark_incomplete(const char *out, const char *why) {
    char path[APP_DUMP_PATH_MAX + 32], text[400];
    int n;
    if (!is_dir(out) || join(path, sizeof path, out, "dump-incomplete.txt") != 0) return;
    n = snprintf(text, sizeof text,
                 "This dump did not finish (%s). Files may be missing or cut short: dump the title again.\n", why);
    if (n > 0 && (size_t)n < sizeof text && write_text(path, text, (size_t)n) != 0)
        fprintf(stderr, "app_dump: could not write %s\n", path);
}

int app_dump_run(const app_dump_args_t *a, const app_dump_hooks_t *h, app_dump_result_t *r) {
    char sand[APP_DUMP_PATH_MAX], dlcs[APP_DUMP_MAX_DLC][96], path[APP_DUMP_PATH_MAX];
    unsigned ndlc = 0, i;
    int ps4 = app_dump_is_ps4(a->title_id), rc = 0, status, main_done = 0;
    uint32_t fw;
    sizes_t sz;
    struct statvfs sv;
    dump_t d;

    memset(r, 0, sizeof *r);
    memset(&sz, 0, sizeof sz);
    memset(&d, 0, sizeof d);
    d.a = a;
    d.h = h;
    d.r = r;
    fw = app_dump_platform.fw_version ? app_dump_platform.fw_version() : 0;
    if (fw) snprintf(r->fw, sizeof r->fw, "0x%08X", (unsigned)fw);

    if (!app_dump_title_ok(a->title_id)) {
        snprintf(r->cause, sizeof r->cause, "dump_bad_title_id");
        return APP_DUMP_E_IO;
    }
    if (cpath(sand, sizeof sand, "/mnt/sandbox/pfsmnt/%s-app0", a->title_id) != 0 || !is_dir(sand)) {
        snprintf(r->cause, sizeof r->cause, "dump_not_running");
        return APP_DUMP_E_NOT_RUNNING;
    }
    if (app_dump_out_path(a, r->path, sizeof r->path) != 0) {
        snprintf(r->cause, sizeof r->cause, "dump_io_failed: the destination path is too long");
        return APP_DUMP_E_IO;
    }
    {
        struct stat st;
        if (lstat(r->path, &st) == 0) {
            snprintf(r->cause, sizeof r->cause, "dump_exists");
            return APP_DUMP_E_EXISTS;
        }
    }

    if (a->decrypt) {
        if (app_dump_platform.decrypt_supported && app_dump_platform.decrypt_supported()) d.decrypt_on = 1;
        else snprintf(r->decrypt_skipped, sizeof r->decrypt_skipped, "dump_decrypt_unsupported_fw");
    }
    d.fself_on = d.decrypt_on && a->fself;
    d.root_out = r->path;

    /* The DLC mounted with the title (app_scan.c's app_find_dlc, by the folder listing). One whose
     * folder exists in dest already is left alone. */
    if (a->dlc && cpath(path, sizeof path, "/mnt/sandbox/pfsmnt") == 0) {
        DIR *dp = opendir(path);
        struct dirent *e;
        while (dp && (e = readdir(dp)) != NULL && ndlc < APP_DUMP_MAX_DLC) {
            char od[APP_DUMP_PATH_MAX];
            struct stat st;
            unsigned k;
            int dup = 0;
            if (!app_dump_dlc_name_ok(e->d_name, a->title_id)) continue;
            for (k = 0; k < ndlc; k++) dup |= strcmp(dlcs[k], e->d_name) == 0;
            if (dup || snprintf(od, sizeof od, "%s/%s", a->dest, e->d_name) >= (int)sizeof od) continue;
            if (lstat(od, &st) == 0) {
                fprintf(stderr, "app_dump: %s exists; that DLC is not dumped again\n", od);
                continue;
            }
            fmt_cut(dlcs[ndlc++], sizeof dlcs[0], "%s", e->d_name); /* app_dump_dlc_name_ok: < 96 */
        }
        if (dp) closedir(dp);
    }

    /* What it takes, before anything is written. */
    note(h, "counting");
    if (ps4) {
        static const char *const sfx[] = {"app0", "patch0"};
        for (i = 0; i < 2 && rc == 0; i++) {
            char img[APP_DUMP_PATH_MAX], mnt[APP_DUMP_PATH_MAX];
            sizes_t ss;
            memset(&ss, 0, sizeof ss);
            if (cpath(img, sizeof img, "/mnt/sandbox/pfsmnt/%s-%s-nest/pfs_image.dat", a->title_id, sfx[i]) != 0 ||
                cpath(mnt, sizeof mnt, "/mnt/sandbox/pfsmnt/%s-%s", a->title_id, sfx[i]) != 0)
                continue;
            if (is_file(img)) {
                rc = app_dump_unpfs(img, NULL, 1, h, &sz.files, &sz.bytes);
                if (rc == -1) rc = 0; /* an unreadable image fails at the copy, with its cause */
                if (rc == 0 && d.decrypt_on) rc = size_tree(h, mnt, 0, &ss);
            } else {
                rc = size_tree(h, mnt, 0, &ss);
                sz.files += ss.files;
                sz.bytes += ss.bytes;
            }
            sz.self_files += ss.self_files;
            sz.self_bytes += ss.self_bytes;
        }
    } else {
        rc = size_tree(h, sand, 0, &sz);
    }
    for (i = 0; i < ndlc && rc == 0; i++) {
        sizes_t ss;
        memset(&ss, 0, sizeof ss);
        if (cpath(path, sizeof path, "/mnt/sandbox/pfsmnt/%s", dlcs[i]) == 0) rc = size_tree(h, path, 0, &ss);
        sz.files += ss.files;
        sz.bytes += ss.bytes;
    }
    if (rc == -2) {
        snprintf(r->cause, sizeof r->cause, "dump_cancelled");
        return APP_DUMP_E_CANCELLED;
    }
    if (!d.decrypt_on) sz.self_files = sz.self_bytes = 0;

    if (ad_mkdirs(a->dest) != 0) {
        fail_io(&d, "mkdir", a->dest);
        return APP_DUMP_E_IO;
    }
    if (statvfs(a->dest, &sv) == 0) {
        uint64_t avail = statvfs_avail_bytes(&sv);
        uint64_t need = sz.bytes + (d.fself_on ? sz.self_bytes : 0) + AD_SPACE_MARGIN;
        if (need > avail) {
            fprintf(stderr, "app_dump: %s needs %llu MiB, %s has %llu MiB free\n", a->title_id,
                    (unsigned long long)(need >> 20), a->dest, (unsigned long long)(avail >> 20));
            snprintf(r->cause, sizeof r->cause, "dump_no_space");
            return APP_DUMP_E_NO_SPACE;
        }
    }
    if (h && h->set_total) h->set_total(h->arg, sz.files + sz.self_files, sz.bytes + sz.self_bytes);

    if (ad_mkdirs(r->path) != 0) {
        fail_io(&d, "mkdir", r->path);
        return APP_DUMP_E_IO;
    }
    rc = ps4 ? ps4_dump(&d, r->path) : ps5_dump(&d, r->path);
    /* The title is complete before its DLC start: a later stop or failure leaves it described. */
    if (rc == 0 && app_dump_write_info(a, r, (long long)time(NULL)) != 0)
        fprintf(stderr, "app_dump: could not write %s/dump-info.json\n", r->path);
    main_done = rc == 0;
    for (i = 0; i < ndlc && rc == 0; i++) {
        char od[APP_DUMP_PATH_MAX];
        if (join(od, sizeof od, a->dest, dlcs[i]) != 0) continue;
        if ((rc = dlc_dump(&d, dlcs[i], od)) == 0) fmt_cut(r->dlc[r->dlc_count++], sizeof r->dlc[0], "%s", dlcs[i]);
        else mark_incomplete(od, rc == -2 ? "stopped" : r->cause);
    }
    ad_buf_release();

    if (rc == -2) {
        snprintf(r->cause, sizeof r->cause, "dump_cancelled");
        if (!main_done) mark_incomplete(r->path, "stopped");
        status = APP_DUMP_E_CANCELLED;
    } else if (rc != 0) {
        if (!r->cause[0]) snprintf(r->cause, sizeof r->cause, "dump_io_failed");
        if (!main_done) mark_incomplete(r->path, r->cause);
        status = APP_DUMP_E_IO;
    } else {
        status = APP_DUMP_OK;
        if (ndlc && app_dump_write_info(a, r, (long long)time(NULL)) != 0)
            fprintf(stderr, "app_dump: could not write %s/dump-info.json\n", r->path);
    }
    return status;
}
