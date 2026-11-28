/* PS5Upload Play's install, the homebrew store app's way (see include/play_app.h for the rules;
 * the engine's twin is engine/crates/ps5upload-engine/src/play_app_install.rs). */
#include "play_app.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "webui_codec.h"

#define PARAM_MAX (256u * 1024u)
#define SMALL_MAX (64u * 1024u)
#define PATH_CAP 1024
#define COPY_BUF (256u * 1024u)
#define ZERO_SHA "0000000000000000000000000000000000000000000000000000000000000000"

/* root + printf(fmt): 0, or -1 when it does not fit. */
static int pathf(char *out, const char *root, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static int pathf(char *out, const char *root, const char *fmt, ...) {
    va_list ap;
    int k = snprintf(out, PATH_CAP, "%s", root ? root : "");
    if (k < 0 || k >= PATH_CAP) return -1;
    va_start(ap, fmt);
    int m = vsnprintf(out + k, PATH_CAP - (size_t)k, fmt, ap);
    va_end(ap);
    return m < 0 || m >= PATH_CAP - k ? -1 : 0;
}

/* The whole of a small file, NUL-terminated (free it); NULL when missing, unreadable or over max. */
static char *slurp(const char *path, size_t max, size_t *len) {
    struct stat st;
    char *buf;
    size_t got = 0;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return NULL;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size < 0 || (uint64_t)st.st_size > max) {
        close(fd);
        return NULL;
    }
    buf = malloc((size_t)st.st_size + 1);
    if (!buf) {
        close(fd);
        return NULL;
    }
    while (got < (size_t)st.st_size) {
        ssize_t r = read(fd, buf + got, (size_t)st.st_size - got);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) break;
        got += (size_t)r;
    }
    close(fd);
    buf[got] = 0;
    *len = got;
    return buf;
}

static int is_dir(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0 && S_ISDIR(st.st_mode);
}

static int exists(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0;
}

/* A string field of a JSON file under root: 1 found (out set), else 0. */
static int file_str(const char *path, size_t max, const char *key, char *out, size_t cap) {
    size_t len = 0;
    char *buf = slurp(path, max, &len);
    int r;
    out[0] = 0;
    if (!buf) return 0;
    r = webui_json_str(buf, len, key, out, cap);
    free(buf);
    if (r != 1) out[0] = 0;
    return r == 1;
}

int play_app_version(const char *root, const char *dir, char *out, size_t cap) {
    char path[PATH_CAP];
    size_t len = 0;
    char *buf;
    int rc;
    if (!dir || !out || !cap) return -1;
    out[0] = 0;
    if (pathf(path, root, "%s/sce_sys/param.json", dir) != 0) return -1;
    if (!exists(path)) return 0;
    buf = slurp(path, PARAM_MAX, &len);
    if (!buf) return -1;
    rc = webui_json_str(buf, len, "contentVersion", out, cap);
    free(buf);
    if (rc != 1 || !out[0]) {
        out[0] = 0;
        return rc < 0 ? -1 : 0;
    }
    return 1;
}

void play_app_human_version(const char *cv, char *out, size_t cap) {
    unsigned a, b, c;
    char tail;
    if (!cap) return;
    if (cv && sscanf(cv, "%u.%u.%u%c", &a, &b, &c, &tail) == 3 && strspn(cv, "0123456789.") == strlen(cv))
        snprintf(out, cap, "%u.%u.%u", a, b, c);
    else
        snprintf(out, cap, "%s", cv ? cv : "");
}

/* path is `base` or below it. */
static int under(const char *path, const char *base) {
    size_t n = strlen(base);
    return strncmp(path, base, n) == 0 && (path[n] == 0 || path[n] == '/');
}

int play_app_drive_root(const char *location, char *out, size_t cap) {
    static const char *const roots[] = { "/data",     "/mnt/ext0", "/mnt/ext1", "/mnt/usb0", "/mnt/usb1", "/mnt/usb2",
                                         "/mnt/usb3", "/mnt/usb4", "/mnt/usb5", "/mnt/usb6", "/mnt/usb7" };
    size_t i;
    if (!location) return -1;
    for (i = 0; i < sizeof roots / sizeof roots[0]; i++)
        if (under(location, roots[i])) {
            snprintf(out, cap, "%s", roots[i]);
            return 0;
        }
    return -1;
}

int play_app_location_ok(const char *loc) {
    const char *p;
    char drive[32];
    size_t n;
    if (!loc || loc[0] != '/' || (n = strlen(loc)) >= PLAY_APP_LOCATION_MAX || loc[n - 1] == '/') return 0;
    for (p = loc; *p; p++)
        if ((unsigned char)*p < 0x20 || *p == 0x7f || *p == '\\' || *p == '"') return 0;
    /* every part: not empty, not "." or ".." */
    for (p = loc + 1;;) {
        const char *e = strchr(p, '/');
        size_t pl = e ? (size_t)(e - p) : strlen(p);
        if (pl == 0 || (pl == 1 && p[0] == '.') || (pl == 2 && p[0] == '.' && p[1] == '.')) return 0;
        if (!e) break;
        p = e + 1;
    }
    return play_app_drive_root(loc, drive, sizeof drive) == 0;
}

int play_app_target(const char *root, char *location, size_t cap) {
    char path[PATH_CAP], loc[PLAY_APP_LOCATION_MAX], tid[32];
    snprintf(location, cap, "%s", PLAY_APP_DEFAULT_LOCATION);
    if (pathf(path, root, "%s", PLAY_APP_STORE_RECEIPT) != 0) return 0;
    if (!file_str(path, SMALL_MAX, "titleId", tid, sizeof tid) || strcmp(tid, PLAY_APP_TITLE_ID) != 0) return 0;
    if (!file_str(path, SMALL_MAX, "location", loc, sizeof loc) || !play_app_location_ok(loc)) return 0;
    if (pathf(path, root, "%s/%s/sce_sys/param.json", loc, PLAY_APP_TITLE_ID) != 0) return 0;
    if (!file_str(path, PARAM_MAX, "titleId", tid, sizeof tid) || strcmp(tid, PLAY_APP_TITLE_ID) != 0) return 0;
    snprintf(location, cap, "%s", loc);
    return 1;
}

static int sha_ok(const char *s) {
    return s && strlen(s) == 64 && strspn(s, "0123456789abcdefABCDEF") == 64;
}

int play_app_receipt_text(char *out, size_t cap, const char *location, const char *cv, const char *sha256, time_t now) {
    char loc[2 * PLAY_APP_LOCATION_MAX], ver[2 * PLAY_APP_VERSION_MAX], human[PLAY_APP_VERSION_MAX], sha[65], at[32];
    struct tm tm;
    size_t i;
    int k;
    if (!location || !cv || !cv[0] || !play_app_location_ok(location) || strlen(cv) >= PLAY_APP_VERSION_MAX) return -1;
    webui_json_escape(loc, sizeof loc, location, strlen(location));
    webui_json_escape(ver, sizeof ver, cv, strlen(cv));
    play_app_human_version(cv, human, sizeof human);
    snprintf(sha, sizeof sha, "%s", sha_ok(sha256) ? sha256 : ZERO_SHA);
    for (i = 0; sha[i]; i++)
        if (sha[i] >= 'A' && sha[i] <= 'F') sha[i] = (char)(sha[i] - 'A' + 'a');
    if (!gmtime_r(&now, &tm) || strftime(at, sizeof at, "%Y-%m-%dT%H:%M:%SZ", &tm) == 0) return -1;
    {
        char hum[2 * PLAY_APP_VERSION_MAX];
        webui_json_escape(hum, sizeof hum, human, strlen(human));
        k = snprintf(out, cap,
                     "{\"schema\":1,\"titleId\":\"%s\",\"location\":\"%s\",\"contentVersion\":\"%s\",\"releaseTag\":\"v%s\","
                     "\"sha256\":\"%s\",\"installedAt\":\"%s\"}\n",
                     PLAY_APP_TITLE_ID, loc, ver, hum, sha, at);
    }
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

int play_app_store_managed(const char *root, const char *location, const char *installed) {
    char path[PATH_CAP], loc[PLAY_APP_LOCATION_MAX], ver[PLAY_APP_VERSION_MAX];
    if (!installed || !installed[0] || pathf(path, root, "%s", PLAY_APP_STORE_RECEIPT) != 0) return 0;
    return file_str(path, SMALL_MAX, "location", loc, sizeof loc) && !strcmp(loc, location) &&
           file_str(path, SMALL_MAX, "contentVersion", ver, sizeof ver) && !strcmp(ver, installed);
}

int play_app_store_busy(const char *root) {
    char path[PATH_CAP];
    return pathf(path, root, "%s", PLAY_APP_STORE_JOURNAL) == 0 && exists(path);
}

int play_app_has_nocase(const char *s, size_t n, const char *needle) {
    size_t nl = strlen(needle), i, j;
    if (!s || nl == 0 || n < nl) return 0;
    for (i = 0; i + nl <= n; i++) {
        for (j = 0; j < nl; j++) {
            char c = s[i + j];
            if (c >= 'A' && c <= 'Z') c = (char)(c + 32);
            if (c != needle[j]) break;
        }
        if (j == nl) return 1;
    }
    return 0;
}

/* ---- the tree ---- */

typedef struct {
    char **rel;     /* paths relative to the tree's root */
    uint8_t *dir;   /* 1 a folder, 0 a file */
    uint64_t total; /* bytes of the files */
    int n;
} tree_t;

static int is_param(const char *rel) { return strcmp(rel, "sce_sys/param.json") == 0; }

void play_app_order(char **rel, int n) {
    int i, j;
    /* insertion sort: by path, sce_sys/param.json last */
    for (i = 1; i < n; i++) {
        char *r = rel[i];
        for (j = i - 1; j >= 0; j--) {
            int a = is_param(rel[j]), b = is_param(r);
            if (a < b || (a == b && strcmp(rel[j], r) <= 0)) break;
            rel[j + 1] = rel[j];
        }
        rel[j + 1] = r;
    }
}

static void tree_free(tree_t *t) {
    int i;
    for (i = 0; i < t->n; i++) free(t->rel[i]);
    free(t->rel);
    free(t->dir);
    memset(t, 0, sizeof *t);
}

/* Folders (first, parents before children) and regular files under base/rel; symlinks and anything
 * else are skipped. 0, or -1 with *err. */
static int walk(const char *base, const char *rel, int depth, tree_t *t, const char **err) {
    char path[PATH_CAP];
    DIR *d;
    struct dirent *e;
    if (depth > PLAY_APP_DEPTH_MAX) {
        *err = "too_many_files";
        return -1;
    }
    if (snprintf(path, sizeof path, "%s%s%s", base, rel[0] ? "/" : "", rel) >= (int)sizeof path || !(d = opendir(path))) {
        *err = "not_staged";
        return -1;
    }
    while ((e = readdir(d)) != NULL) {
        char sub[PATH_CAP], full[PATH_CAP];
        struct stat st;
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (snprintf(sub, sizeof sub, "%s%s%s", rel, rel[0] ? "/" : "", e->d_name) >= (int)sizeof sub ||
            snprintf(full, sizeof full, "%s/%s", base, sub) >= (int)sizeof full || lstat(full, &st) != 0) {
            closedir(d);
            *err = "not_staged";
            return -1;
        }
        if (!S_ISDIR(st.st_mode) && !S_ISREG(st.st_mode)) continue;
        if (t->n >= PLAY_APP_FILES_MAX) {
            closedir(d);
            *err = "too_many_files";
            return -1;
        }
        if (!(t->rel[t->n] = strdup(sub))) {
            closedir(d);
            *err = "not_staged";
            return -1;
        }
        t->dir[t->n] = S_ISDIR(st.st_mode) ? 1 : 0;
        if (!t->dir[t->n]) t->total += (uint64_t)st.st_size;
        t->n++;
        if (S_ISDIR(st.st_mode) && walk(base, sub, depth + 1, t, err) != 0) {
            closedir(d);
            return -1;
        }
    }
    closedir(d);
    return 0;
}

/* rm -rf that never follows a symlink; depth-limited. 0 / -1. */
static int rm_rf(const char *path, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    int rc = 0;
    if (lstat(path, &st) != 0) return errno == ENOENT ? 0 : -1;
    if (!S_ISDIR(st.st_mode)) return unlink(path);
    if (depth > PLAY_APP_DEPTH_MAX + 2 || !(d = opendir(path))) return -1;
    while ((e = readdir(d)) != NULL) {
        char sub[PATH_CAP];
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (snprintf(sub, sizeof sub, "%s/%s", path, e->d_name) >= (int)sizeof sub || rm_rf(sub, depth + 1) != 0) rc = -1;
    }
    closedir(d);
    return rmdir(path) == 0 ? rc : -1;
}

/* mkdir -p, each folder it makes chmod 0777. 0 / -1. */
static int mkdirs(const char *path) {
    char p[PATH_CAP];
    size_t i;
    if (snprintf(p, sizeof p, "%s", path) >= (int)sizeof p) return -1;
    for (i = 1; p[i]; i++) {
        if (p[i] != '/') continue;
        p[i] = 0;
        if (!is_dir(p) && (mkdir(p, 0777) != 0 && errno != EEXIST)) return -1;
        p[i] = '/';
    }
    if (!is_dir(p)) {
        if (mkdir(p, 0777) != 0 && errno != EEXIST) return -1;
        (void)chmod(p, 0777);
    }
    return is_dir(p) ? 0 : -1;
}

static int copy_file(const char *src, const char *dst, uint8_t *buf, uint64_t *done, uint64_t total,
                     const play_app_env_t *env) {
    int in = open(src, O_RDONLY), out;
    if (in < 0) return -1;
    out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0777);
    if (out < 0) {
        close(in);
        return -1;
    }
    for (;;) {
        ssize_t r = read(in, buf, COPY_BUF), w = 0;
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) goto fail;
        if (r == 0) break;
        while (w < r) {
            ssize_t k = write(out, buf + w, (size_t)(r - w));
            if (k < 0 && errno == EINTR) continue;
            if (k <= 0) goto fail;
            w += k;
        }
        if (done) {
            *done += (uint64_t)r;
            if (env && env->progress) env->progress(env->ctx, *done, total);
        }
    }
    /* On disk before it is renamed into place. */
    if (fsync(out) != 0) goto fail;
    close(in);
    return close(out) == 0 ? 0 : -1;
fail:
    close(in);
    close(out);
    return -1;
}

/* The one rename of this file: only when `from` and the folder `to` goes in are on one device (a
 * rename across devices panics the PS5's kernel). 0, -1 rename failed, -2 another device. */
static int same_dev_rename(const char *from, const char *to) {
    char parent[PATH_CAP];
    struct stat a, b;
    char *slash;
    if (snprintf(parent, sizeof parent, "%s", to) >= (int)sizeof parent || !(slash = strrchr(parent, '/'))) return -1;
    if (slash == parent) slash[1] = 0;
    else *slash = 0;
    if (lstat(from, &a) != 0 || stat(parent, &b) != 0) return -1;
    if (a.st_dev != b.st_dev) return -2;
    return rename(from, to) == 0 ? 0 : -1;
}

/* Write `text` to path through path.tmp (same folder). 0 / -1. */
static int write_via_tmp(const char *path, const char *text) {
    char tmp[PATH_CAP];
    size_t n = strlen(text), off = 0;
    int fd;
    if (snprintf(tmp, sizeof tmp, "%s.tmp", path) >= (int)sizeof tmp) return -1;
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    while (off < n) {
        ssize_t w = write(fd, text + off, n - off);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) {
            close(fd);
            unlink(tmp);
            return -1;
        }
        off += (size_t)w;
    }
    if (fsync(fd) != 0 || close(fd) != 0 || same_dev_rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* The target folder exists and holds FAKE99261's param.json. */
static int holds_app(const char *dir) {
    char path[PATH_CAP], tid[32];
    return snprintf(path, sizeof path, "%s/sce_sys/param.json", dir) < (int)sizeof path &&
           file_str(path, PARAM_MAX, "titleId", tid, sizeof tid) && !strcmp(tid, PLAY_APP_TITLE_ID);
}

/* Update only, best effort: the console's copies of sce_sys follow the new files. */
static void refresh_registered(const char *root, const char *target, uint8_t *buf) {
    char sys[PATH_CAP], meta[PATH_CAP], appsys[PATH_CAP], icon[PATH_CAP];
    DIR *d;
    struct dirent *e;
    int have_meta, have_appsys;
    if (snprintf(sys, sizeof sys, "%s/sce_sys", target) >= (int)sizeof sys || pathf(meta, root, "%s", PLAY_APP_APPMETA_DIR) != 0 ||
        pathf(appsys, root, "%s/sce_sys", PLAY_APP_USER_APP_DIR) != 0 || pathf(icon, root, "%s/icon0.png", PLAY_APP_USER_APP_DIR) != 0)
        return;
    have_meta = is_dir(meta);
    have_appsys = is_dir(appsys);
    if ((d = opendir(sys)) != NULL) {
        while ((e = readdir(d)) != NULL) {
            char src[PATH_CAP], dst[PATH_CAP];
            struct stat st;
            if (snprintf(src, sizeof src, "%s/%s", sys, e->d_name) >= (int)sizeof src || lstat(src, &st) != 0 || !S_ISREG(st.st_mode))
                continue;
            if (have_meta && snprintf(dst, sizeof dst, "%s/%s", meta, e->d_name) < (int)sizeof dst)
                (void)copy_file(src, dst, buf, NULL, 0, NULL);
            if (have_appsys && snprintf(dst, sizeof dst, "%s/%s", appsys, e->d_name) < (int)sizeof dst)
                (void)copy_file(src, dst, buf, NULL, 0, NULL);
        }
        closedir(d);
    }
    if (exists(icon)) {
        char src[PATH_CAP];
        if (snprintf(src, sizeof src, "%s/icon0.png", sys) < (int)sizeof src && exists(src))
            (void)copy_file(src, icon, buf, NULL, 0, NULL);
    }
}

int play_app_install(const play_app_env_t *env, play_app_result_t *res, const char **err) {
    char kept[PATH_CAP], target[PATH_CAP], drive[32], work[PATH_CAP], staging[PATH_CAP], path[PATH_CAP], path2[PATH_CAP];
    char receipt[PLAY_APP_RECEIPT_MAX], sha[80] = "";
    const char *root = env && env->root ? env->root : "", *dummy;
    tree_t t;
    uint8_t *buf = NULL;
    uint64_t done = 0;
    struct stat st;
    int i, rc = -1, r;
    if (!err) err = &dummy;
    memset(res, 0, sizeof *res);
    memset(&t, 0, sizeof t);
    if (play_app_store_busy(root)) {
        *err = "store_busy";
        return -1;
    }
    /* The kept copy, complete (param.json is its last file). */
    *err = "not_staged";
    if (pathf(kept, root, "%s", PLAY_APP_KEPT_DIR) != 0 || play_app_version(root, PLAY_APP_KEPT_DIR, res->version, sizeof res->version) != 1)
        return -1;
    (void)play_app_target(root, res->location, sizeof res->location);
    if (play_app_drive_root(res->location, drive, sizeof drive) != 0 ||
        pathf(target, root, "%s/%s", res->location, PLAY_APP_TITLE_ID) != 0 || pathf(work, root, "%s/prosperostore", drive) != 0 ||
        snprintf(staging, sizeof staging, "%s/staging/%s", work, PLAY_APP_TITLE_ID) >= (int)sizeof staging) {
        *err = "unexpected_target";
        return -1;
    }
    /* Fresh or update: whatever is at the target must be this app's folder. */
    if (lstat(target, &st) == 0) {
        if (!S_ISDIR(st.st_mode) || !holds_app(target)) {
            *err = "unexpected_target";
            return -1;
        }
        res->update = 1;
        if (snprintf(path, sizeof path, "%s/backup/%s", work, PLAY_APP_TITLE_ID) < (int)sizeof path && exists(path)) {
            *err = "store_backup_present";
            return -1;
        }
    }
    t.rel = calloc(PLAY_APP_FILES_MAX, sizeof *t.rel);
    t.dir = calloc(PLAY_APP_FILES_MAX, 1);
    buf = malloc(COPY_BUF);
    if (!t.rel || !t.dir || !buf) {
        *err = "staging_failed";
        goto out;
    }
    if (walk(kept, "", 0, &t, err) != 0) goto out;
    /* 1. Staging: a fresh copy of the kept one, every folder and file 0777. */
    *err = "staging_failed";
    if (rm_rf(staging, 0) != 0 || mkdirs(staging) != 0 || chmod(staging, 0777) != 0) goto out;
    for (i = 0; i < t.n; i++) {
        if (snprintf(path, sizeof path, "%s/%s", staging, t.rel[i]) >= (int)sizeof path) goto out;
        if (t.dir[i]) {
            if ((mkdir(path, 0777) != 0 && errno != EEXIST) || chmod(path, 0777) != 0) goto out;
            continue;
        }
        if (snprintf(path2, sizeof path2, "%s/%s", kept, t.rel[i]) >= (int)sizeof path2 ||
            copy_file(path2, path, buf, &done, t.total, env) != 0 || chmod(path, 0777) != 0) {
            fprintf(stderr, "[play-app] staging %s failed (errno %d)\n", t.rel[i], errno);
            goto out;
        }
    }
    if (!res->update) {
        /* 2a. Fresh: one rename of the whole folder. */
        if (pathf(path, root, "%s", res->location) != 0 || mkdirs(path) != 0) {
            *err = "place_failed";
            goto out;
        }
        r = same_dev_rename(staging, target);
        if (r != 0) {
            *err = r == -2 ? "cross_device" : "place_failed";
            fprintf(stderr, "[play-app] placing the folder failed (%s, errno %d)\n", *err, errno);
            goto out;
        }
    } else {
        /* 2b. Quick update: folders made, then each file over its target, param.json last. */
        char **files = calloc((size_t)t.n + 1, sizeof *files);
        int nf = 0;
        if (!files) goto out;
        for (i = 0; i < t.n; i++) {
            if (!t.dir[i]) {
                files[nf++] = t.rel[i];
                continue;
            }
            if (snprintf(path, sizeof path, "%s/%s", target, t.rel[i]) >= (int)sizeof path ||
                (mkdir(path, 0777) != 0 && errno != EEXIST) || !is_dir(path) || chmod(path, 0777) != 0) {
                free(files);
                *err = is_dir(path) || !exists(path) ? "place_failed" : "unexpected_target";
                goto out;
            }
        }
        play_app_order(files, nf);
        /* Nothing is moved before every target path has been checked. */
        for (i = 0; i < nf; i++)
            if (snprintf(path, sizeof path, "%s/%s", target, files[i]) >= (int)sizeof path ||
                (lstat(path, &st) == 0 && !S_ISREG(st.st_mode))) {
                free(files);
                *err = "unexpected_target";
                goto out;
            }
        for (i = 0; i < nf; i++) {
            (void)snprintf(path, sizeof path, "%s/%s", staging, files[i]);
            (void)snprintf(path2, sizeof path2, "%s/%s", target, files[i]);
            r = same_dev_rename(path, path2);
            if (r != 0) {
                fprintf(stderr, "[play-app] placing %s failed (errno %d)\n", files[i], errno);
                free(files);
                *err = r == -2 ? "cross_device" : "place_failed";
                goto out;
            }
        }
        free(files);
        (void)rm_rf(staging, 0);
    }
    /* 3. The receipt, as the store writes it. */
    if (pathf(path, root, "%s", PLAY_APP_SHA_FILE) == 0) {
        size_t len = 0;
        char *s = slurp(path, 256, &len);
        if (s) {
            while (len && (s[len - 1] == '\n' || s[len - 1] == '\r' || s[len - 1] == ' ')) s[--len] = 0;
            snprintf(sha, sizeof sha, "%s", s);
            free(s);
        }
    }
    if (play_app_receipt_text(receipt, sizeof receipt, res->location, res->version, sha, env ? env->now : time(NULL)) < 0 ||
        pathf(path, root, "%s", PLAY_APP_STORE_RECEIPTS) != 0 || mkdirs(path) != 0 ||
        pathf(path, root, "%s", PLAY_APP_STORE_RECEIPT) != 0 || write_via_tmp(path, receipt) != 0) {
        *err = "receipt_failed";
        goto out;
    }
    /* 4. Update: the console's copies of sce_sys. */
    if (res->update) refresh_registered(root, target, buf);
    rc = 0;
    *err = NULL;
out:
    if (rc != 0) (void)rm_rf(staging, 0);
    free(buf);
    tree_free(&t);
    return rc;
}

int play_app_write_install_json(const char *root, const char *version, const char *location, const char *registered_by,
                                time_t now) {
    char path[PATH_CAP], text[1024], v[2 * PLAY_APP_VERSION_MAX], l[2 * PLAY_APP_LOCATION_MAX], at[32];
    struct tm tm;
    int k;
    if (!version || !location || !registered_by || !gmtime_r(&now, &tm) ||
        strftime(at, sizeof at, "%Y-%m-%dT%H:%M:%SZ", &tm) == 0)
        return -1;
    webui_json_escape(v, sizeof v, version, strlen(version));
    webui_json_escape(l, sizeof l, location, strlen(location));
    k = snprintf(text, sizeof text, "{\"version\":\"%s\",\"location\":\"%s\",\"registered_by\":\"%s\",\"installed_at\":\"%s\"}\n",
                 v, l, strcmp(registered_by, "shadowmount") == 0 ? "shadowmount" : "ps5upload", at);
    if (k < 0 || (size_t)k >= sizeof text) return -1;
    if (pathf(path, root, "/data/ps5upload/play") != 0 || mkdirs(path) != 0) return -1;
    if (pathf(path, root, "%s", PLAY_APP_INSTALL_JSON) != 0) return -1;
    return write_via_tmp(path, text);
}
