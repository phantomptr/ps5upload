/* See smbd_library.h. Runs on a job thread (512 KiB stack): every path and listing is on the heap. */
#include "smbd_library.h"

#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "pkg_info.h"
#include "smbd_json.h"
#include "smbd_path.h"

typedef struct {
    char *name;
    xkind_t kind;
    uint64_t size;
} lent_t;

typedef struct {
    xfs_t *fs;
    lib_limits_t lim;
    copy_progress_t *pr;
    const char *source;
    const char *out_dir;
    const char *skip;
    char stamp[24];
    sj_buf_t items;
    unsigned nitems;
    unsigned entries;
    time_t deadline;
    uint64_t icon_bytes;
    const char *stopped; /* why the walk ended early, or NULL */
    int fail;            /* a negative xerr_t: the walk was abandoned (the source went away), no result */
} scan_t;

int lib_source_ok(const char *s) {
    size_t i;
    if (!s || !s[0] || strlen(s) >= LIB_SOURCE_MAX) return 0;
    for (i = 0; s[i]; i++) {
        char c = s[i];
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-')) return 0;
    }
    return 1;
}

static int is_upper(char c) { return c >= 'A' && c <= 'Z'; }
static int is_digit(char c) { return c >= '0' && c <= '9'; }
static int is_alnum(char c) { return is_upper(c) || is_digit(c) || (c >= 'a' && c <= 'z'); }

int lib_title_id_in(const char *name, char out[10]) {
    size_t n = strlen(name), i, k;
    for (i = 0; i + 9 <= n; i++) {
        if (i > 0 && is_alnum(name[i - 1])) continue;
        for (k = 0; k < 4 && is_upper(name[i + k]); k++) {
        }
        if (k < 4) continue;
        for (; k < 9 && is_digit(name[i + k]); k++) {
        }
        if (k < 9 || (i + 9 < n && is_digit(name[i + 9]))) continue;
        memcpy(out, name + i, 9);
        out[9] = '\0';
        return 1;
    }
    out[0] = '\0';
    return 0;
}

static int ends_with_ci(const char *s, const char *suffix) {
    size_t n = strlen(s), m = strlen(suffix), i;
    if (n <= m) return 0;
    for (i = 0; i < m; i++) {
        char c = s[n - m + i];
        if (c >= 'A' && c <= 'Z') c = (char)(c + 32);
        if (c != suffix[i]) return 0;
    }
    return 1;
}

static int is_image(const char *name) {
    return ends_with_ci(name, ".exfat") || ends_with_ci(name, ".ffpkg") || ends_with_ci(name, ".ffpfs") ||
           ends_with_ci(name, ".ffpfsc");
}

/* Folders a NAS, Windows or macOS keeps for itself. */
static int skip_folder(const char *name) {
    static const char *const k[] = { "$RECYCLE.BIN", "System Volume Information", "@eaDir", "#recycle", "#snapshot",
                                     "lost+found", "Thumbs", NULL };
    int i;
    if (name[0] == '.' || name[0] == '$') return 1;
    for (i = 0; k[i]; i++)
        if (strcmp(name, k[i]) == 0) return 1;
    return 0;
}

static int over(scan_t *s) {
    if (s->stopped || s->fail) return 1;
    if (s->pr->cancel) return 1;
    if (time(NULL) > s->deadline) s->stopped = "time";
    else if (s->entries >= s->lim.entries_max) s->stopped = "entries";
    else if (s->nitems >= s->lim.items_max) s->stopped = "items";
    return s->stopped != NULL;
}

/* ---- reading ---- */

typedef struct {
    xfs_t *fs;
    void *fh;
    size_t chunk;
} rd_t;

static int rd_exact(void *ctx, uint64_t off, void *buf, size_t len) {
    rd_t *r = ctx;
    size_t got = 0;
    while (got < len) {
        size_t want = len - got;
        long long k;
        if (want > r->chunk) want = r->chunk;
        k = r->fs->ops->pread(r->fs, r->fh, (uint8_t *)buf + got, want, off + got);
        if (k <= 0) return -1;
        got += (size_t)k;
    }
    return 0;
}

static size_t chunk_of(xfs_t *fs) {
    size_t c = fs->ops->chunk ? fs->ops->chunk(fs) : 0;
    return c ? c : COPY_CHUNK_MAX;
}

/* The whole of a small file (at most `max` bytes) into a heap buffer. */
static uint8_t *read_small(xfs_t *fs, const char *path, size_t max, size_t *len) {
    xstat_t st;
    rd_t r;
    uint8_t *b;
    if (fs->ops->stat(fs, path, &st) != 0 || st.kind != XK_FILE || st.size == 0 || st.size > max) return NULL;
    b = malloc((size_t)st.size + 1);
    if (!b) return NULL;
    r.fs = fs;
    r.chunk = chunk_of(fs);
    if (fs->ops->open_read(fs, path, &r.fh) != 0) {
        free(b);
        return NULL;
    }
    if (rd_exact(&r, 0, b, (size_t)st.size) != 0) {
        fs->ops->close(fs, r.fh);
        free(b);
        return NULL;
    }
    fs->ops->close(fs, r.fh);
    b[st.size] = 0;
    *len = (size_t)st.size;
    return b;
}

/* ---- covers ---- */

static int write_file(const char *path, const uint8_t *b, size_t n) {
    char tmp[SMBP_PATH_MAX + 16];
    int fd;
    size_t off = 0;
    if (snprintf(tmp, sizeof tmp, "%s.tmp", path) >= (int)sizeof tmp) return -1;
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    while (off < n) {
        ssize_t w = write(fd, b + off, n - off);
        if (w <= 0) {
            close(fd);
            unlink(tmp);
            return -1;
        }
        off += (size_t)w;
    }
    if (close(fd) != 0 || rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* Saves a PNG as this scan's next cover; its file name into `name` ("" when it was not kept). */
static void keep_icon(scan_t *s, const uint8_t *png, size_t n, char *name, size_t cap) {
    static const uint8_t sig[8] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n' };
    char path[SMBP_PATH_MAX];
    name[0] = '\0';
    if (n < 8 || n > LIB_ICON_MAX || memcmp(png, sig, 8) != 0) return;
    if (s->icon_bytes + n > s->lim.icon_budget) return;
    snprintf(name, cap, "%s-%s-%u.png", s->source, s->stamp, s->nitems);
    if (snprintf(path, sizeof path, "%s/icons/%s", s->out_dir, name) >= (int)sizeof path || write_file(path, png, n) != 0) {
        name[0] = '\0';
        return;
    }
    s->icon_bytes += n;
}

/* ---- items ---- */

static void put_item(scan_t *s, const char *kind, const char *path, const char *name, const pkg_info_t *p,
                     uint64_t size, const char *icon, int backport) {
    sj_buf_t *b = &s->items;
    if (s->nitems) sj_raw(b, ",");
    sj_raw(b, "{");
    sj_kv_str(b, 0, "kind", kind);
    sj_kv_str(b, 1, "path", path);
    sj_kv_str(b, 1, "name", name);
    sj_kv_str(b, 1, "title", p->title);
    sj_kv_str(b, 1, "title_id", p->title_id);
    sj_kv_str(b, 1, "content_id", p->content_id);
    sj_kv_str(b, 1, "version", p->app_ver);
    sj_kv_str(b, 1, "category", p->category);
    sj_kv_str(b, 1, "type", p->category[0] ? pkg_info_kind(p) : "game");
    sj_kv_str(b, 1, "platform", p->platform);
    sj_kv_str(b, 1, "fw", p->fw);
    sj_kv_str(b, 1, "sdk", p->sdk);
    sj_fmt(b, ",\"backport\":%s,\"fake\":%s,\"size\":%llu", backport ? "true" : "false", p->fake ? "true" : "false",
           (unsigned long long)size);
    sj_kv_str(b, 1, "icon", icon);
    sj_raw(b, "}");
    s->nitems++;
    s->pr->files_total = s->nitems;
}

static void add_pkg(scan_t *s, const char *path, const char *name, uint64_t size) {
    pkg_info_t *p = calloc(1, sizeof *p);
    rd_t r;
    char icon[96] = "";
    if (!p) return;
    r.fs = s->fs;
    r.chunk = chunk_of(s->fs);
    if (s->fs->ops->open_read(s->fs, path, &r.fh) != 0) {
        free(p);
        return;
    }
    if (pkg_info_read(rd_exact, &r, size, p) == NULL) {
        if (p->icon_len && p->icon_len <= LIB_ICON_MAX && s->icon_bytes + p->icon_len <= s->lim.icon_budget) {
            uint8_t *png = malloc(p->icon_len);
            if (png && rd_exact(&r, p->icon_off, png, p->icon_len) == 0) keep_icon(s, png, p->icon_len, icon, sizeof icon);
            free(png);
        }
        s->fs->ops->close(s->fs, r.fh);
        put_item(s, "pkg", path, name, p, size, icon, 0);
    } else {
        s->fs->ops->close(s->fs, r.fh); /* not a package (a split part, something else named .pkg) */
    }
    free(p);
}

static void add_image(scan_t *s, const char *path, const char *name, uint64_t size) {
    pkg_info_t p;
    memset(&p, 0, sizeof p);
    lib_title_id_in(name, p.title_id);
    if (strncmp(p.title_id, "PPSA", 4) == 0) memcpy(p.platform, "ps5", 4);
    else if (strncmp(p.title_id, "CUSA", 4) == 0) memcpy(p.platform, "ps4", 4);
    put_item(s, "image", path, name, &p, size, "", 0);
}

/* `path` is a folder dump when it holds sce_sys/param.json or param.sfo: listed, 1. 0 when not.
 * `backport`: it also holds a fakelib/ folder (a backported copy). */
static int try_dump(scan_t *s, const char *path, int backport) {
    char *f = malloc(SMBP_PATH_MAX), *meta_path = malloc(SMBP_PATH_MAX);
    pkg_info_t *p = calloc(1, sizeof *p);
    uint8_t *meta = NULL;
    size_t n = 0;
    int is_json = 1, found = 0;
    if (!f || !p || !meta_path) goto out;
    if (smbp_join(path, "sce_sys/param.json", meta_path, SMBP_PATH_MAX) != 0) goto out;
    meta = read_small(s->fs, meta_path, PKG_INFO_MAX_META, &n);
    if (!meta) {
        is_json = 0;
        if (smbp_join(path, "sce_sys/param.sfo", meta_path, SMBP_PATH_MAX) != 0) goto out;
        meta = read_small(s->fs, meta_path, PKG_INFO_MAX_META, &n);
    }
    if (!meta) goto out;
    pkg_info_from_meta(meta, n, is_json, p);
    free(meta);
    meta = NULL;
    {
        char icon[96] = "";
        size_t il = 0;
        uint8_t *png = NULL;
        if (smbp_join(path, "sce_sys/icon0.png", f, SMBP_PATH_MAX) == 0 && s->icon_bytes < s->lim.icon_budget)
            png = read_small(s->fs, f, LIB_ICON_MAX, &il);
        if (png) keep_icon(s, png, il, icon, sizeof icon);
        free(png);
        put_item(s, "folder", path, path[0] ? smbp_base(path) : "", p, 0, icon, backport);
    }
    found = 1;
out:
    free(meta);
    free(f);
    free(meta_path);
    free(p);
    return found;
}

/* ---- the walk ---- */

static void free_ents(lent_t *v, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) free(v[i].name);
    free(v);
}

static int read_dir(scan_t *s, const char *path, lent_t **out, size_t *count) {
    void *d = NULL;
    lent_t *v = NULL;
    size_t n = 0, cap = 0;
    char *name = malloc(SMBP_NAME_MAX);
    xstat_t st;
    int rc;
    if (!name) return XE_NOMEM;
    rc = s->fs->ops->opendir(s->fs, path, &d);
    if (rc != 0) {
        free(name);
        return rc;
    }
    while ((rc = s->fs->ops->readdir(s->fs, d, name, SMBP_NAME_MAX, &st)) > 0 || rc == XE_BAD_NAME) {
        if (rc == XE_BAD_NAME) continue; /* a name too long to hold is no game folder the scan can use */
        if (n >= LIB_DIR_ENTRIES_MAX) {
            /* One folder holds more than a scan reads: the result is partial, and says so. */
            if (!s->stopped) s->stopped = "entries";
            break;
        }
        if (s->entries >= s->lim.entries_max || s->pr->cancel) break;
        s->entries++;
        s->pr->files_done = s->entries;
        if (n == cap) {
            size_t nc = cap ? cap * 2 : 64;
            lent_t *nv = realloc(v, nc * sizeof *nv);
            if (!nv) {
                rc = XE_NOMEM;
                break;
            }
            v = nv;
            cap = nc;
        }
        v[n].name = strdup(name);
        if (!v[n].name) {
            rc = XE_NOMEM;
            break;
        }
        v[n].kind = st.kind;
        v[n].size = st.size;
        n++;
    }
    s->fs->ops->closedir(s->fs, d);
    free(name);
    if (rc < 0) {
        free_ents(v, n);
        return rc;
    }
    *out = v;
    *count = n;
    return 0;
}

static void walk(scan_t *s, const char *path, unsigned depth) {
    lent_t *v = NULL;
    size_t n = 0, i;
    char *full;
    int rc;
    if (over(s)) return;
    snprintf(s->pr->current, sizeof s->pr->current, "%s", path);
    rc = read_dir(s, path, &v, &n);
    if (rc != 0) {
        /* The source itself went away (the share dropped, timed out, lost its session) or the
         * root cannot be read at all: the scan is abandoned and the last result kept, rather
         * than a partial listing written as complete. A folder that cannot be read inside an
         * otherwise fine source is passed over. */
        if (rc == XE_DISCONNECTED || rc == XE_TIMEOUT || rc == XE_IO || rc == XE_AUTH || rc == XE_NOMEM ||
            rc == XE_UNREACHABLE || (depth == 0 && (rc == XE_ACCESS || rc == XE_NOT_FOUND)))
            s->fail = rc;
        return;
    }
    for (i = 0; i < n; i++)
        if (v[i].kind == XK_DIR && strcmp(v[i].name, "sce_sys") == 0) break;
    if (i < n) {
        size_t j;
        int fakelib = 0;
        for (j = 0; j < n && !fakelib; j++) fakelib = v[j].kind == XK_DIR && strcmp(v[j].name, "fakelib") == 0;
        if (!try_dump(s, path, fakelib)) i = n;
    }
    if (i < n) {
        free_ents(v, n);
        return;
    }
    full = malloc(SMBP_PATH_MAX);
    for (i = 0; full && i < n && !over(s); i++) {
        const lent_t *e = &v[i];
        if (e->name[0] == '.') continue; /* hidden, and macOS "._x" metadata */
        if (smbp_join(path, e->name, full, SMBP_PATH_MAX) != 0) continue;
        if (e->kind == XK_DIR) {
            if (skip_folder(e->name) || strcmp(e->name, "sce_sys") == 0) continue;
            if (s->skip && strcmp(full, s->skip) == 0) continue;
            if (depth + 1 > s->lim.depth_max) continue;
            walk(s, full, depth + 1);
        } else if (e->kind == XK_FILE) {
            if (ends_with_ci(e->name, ".pkg")) add_pkg(s, full, e->name, e->size);
            else if (is_image(e->name)) add_image(s, full, e->name, e->size);
        }
    }
    free(full);
    free_ents(v, n);
}

/* ---- the result ---- */

/* Removes this source's covers: those of other scans (ours = 0: the new result does not name them),
 * or this scan's own (ours = 1: nothing was written). */
static void drop_icons(const char *out_dir, const char *source, const char *stamp, int ours) {
    char dir[SMBP_PATH_MAX], keep[64], mine[48], *p;
    DIR *d;
    struct dirent *e;
    snprintf(dir, sizeof dir, "%s/icons", out_dir);
    snprintf(mine, sizeof mine, "%s-", source);
    snprintf(keep, sizeof keep, "%s-%s-", source, stamp);
    d = opendir(dir);
    if (!d) return;
    p = malloc(SMBP_PATH_MAX);
    while (p && (e = readdir(d)) != NULL) {
        if (strncmp(e->d_name, mine, strlen(mine)) != 0 || (strncmp(e->d_name, keep, strlen(keep)) == 0) != ours) continue;
        /* "<source>-<stamp>-<n>.png": another source whose id starts with ours has a different shape */
        if (!strchr(e->d_name + strlen(mine), '-')) continue;
        if (snprintf(p, SMBP_PATH_MAX, "%s/%s", dir, e->d_name) < (int)SMBP_PATH_MAX) unlink(p);
    }
    free(p);
    closedir(d);
}

int lib_scan(xfs_t *fs, const char *source, const char *roots, const char *skip_prefix, const char *out_dir,
             const lib_limits_t *lim, copy_progress_t *pr) {
    scan_t *s;
    char *root, *path;
    const char *at;
    sj_buf_t out;
    static unsigned counter;
    int rc = 0;
    if (!lib_source_ok(source) || !roots || !out_dir) return XE_BAD_REQUEST;
    s = calloc(1, sizeof *s);
    root = malloc(SMBP_PATH_MAX);
    path = malloc(SMBP_PATH_MAX + 64);
    if (!s || !root || !path) {
        free(s);
        free(root);
        free(path);
        return XE_NOMEM;
    }
    s->fs = fs;
    s->pr = pr;
    s->source = source;
    s->out_dir = out_dir;
    s->skip = skip_prefix;
    s->lim.depth_max = lim && lim->depth_max ? lim->depth_max : LIB_DEPTH_MAX;
    s->lim.entries_max = lim && lim->entries_max ? lim->entries_max : LIB_ENTRIES_MAX;
    s->lim.items_max = lim && lim->items_max ? lim->items_max : LIB_ITEMS_MAX;
    s->lim.time_max_s = lim && lim->time_max_s ? lim->time_max_s : LIB_TIME_MAX_S;
    s->lim.icon_budget = lim && lim->icon_budget ? lim->icon_budget : LIB_ICON_BUDGET;
    s->deadline = time(NULL) + s->lim.time_max_s;
    snprintf(s->stamp, sizeof s->stamp, "%lx%x", (unsigned long)time(NULL), ++counter & 0xfff);
    sj_init(&s->items, LIB_RESULT_MAX);
    (void)mkdir(out_dir, 0777);
    snprintf(path, SMBP_PATH_MAX + 64, "%s/icons", out_dir);
    (void)mkdir(path, 0777);
    pr->scanning = 1;
    /* Every root once, the first always: "" alone (a share saved without a start folder) is the
     * share's top and must be walked too, which a `while (*at)` loop never did. */
    for (at = roots;;) {
        const char *nl = strchr(at, '\n');
        size_t l = nl ? (size_t)(nl - at) : strlen(at);
        xstat_t st;
        if (l < SMBP_PATH_MAX) {
            memcpy(root, at, l);
            root[l] = '\0';
            /* "" is a share's top; a console root must be there and be a folder */
            if (!root[0] || (fs->ops->stat(fs, root, &st) == 0 && st.kind == XK_DIR)) walk(s, root, 0);
        }
        at += l;
        if (*at != '\n' || at[1] == '\0' || over(s)) break;
        at++;
    }
    pr->scanning = 0;
    if (pr->cancel) {
        rc = XE_CANCELLED;
        goto done;
    }
    if (s->fail) {
        rc = s->fail; /* abandoned: nothing written, the old result and its covers stay */
        goto done;
    }
    sj_init(&out, LIB_RESULT_MAX + 4096);
    sj_raw(&out, "{");
    sj_kv_str(&out, 0, "source", source);
    sj_fmt(&out, ",\"scanned\":%lld,\"complete\":%s", (long long)time(NULL), s->stopped ? "false" : "true");
    if (s->stopped) sj_kv_str(&out, 1, "stopped", s->stopped);
    else sj_raw(&out, ",\"stopped\":null");
    sj_fmt(&out, ",\"entries\":%u,\"items\":[", s->entries);
    if (s->items.buf) sj_raw(&out, s->items.buf);
    sj_raw(&out, "]}");
    if (out.err || s->items.err || !out.buf) {
        rc = XE_NOMEM;
    } else {
        snprintf(path, SMBP_PATH_MAX + 64, "%s/%s.json", out_dir, source);
        rc = write_file(path, (const uint8_t *)out.buf, out.len) == 0 ? 0 : XE_IO;
    }
    sj_free(&out);
done:
    drop_icons(out_dir, source, s->stamp, rc != 0);
    sj_free(&s->items);
    free(s);
    free(root);
    free(path);
    return rc;
}
