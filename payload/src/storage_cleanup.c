/* Console clean-up: scan and apply. See include/storage_cleanup.h and include/cleanup_rules.h.
 *
 * Safety, in the order it is applied:
 *   - the scan only looks at the folders cleanup_rules.h names, lstat()s everything (a link is never
 *     followed) and never crosses into another mount than the folder it walks;
 *   - "is this game installed" falls back to "yes" whenever app.db cannot be read, so nothing that
 *     depends on a game being gone (B, E, F) is offered on a console whose title list is unknown;
 *   - the apply takes item ids of one recent scan, re-checks each path against its category's
 *     allowlist (scr_path_allowed) and against what the scan saw (type, size, time), and skips it
 *     while an upload writes there or the game is running; the whole apply is refused while the
 *     console has an install queued (/user/bgft/task);
 *   - an installed update is only ever removed through Sony (uninstall_patch, sony_api_lock), never
 *     by deleting files, and the removal is watched for minutes (installs are asynchronous);
 *   - deletes are unlink/rmdir in place: nothing is moved, nothing is renamed across mounts. */
#include "storage_cleanup.h"
#include "plog.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

#include "ava1_op.h"
#include "cleanup_rules.h"
#include "content_db.h"
#include "fs_jobs.h"
#include "proc_identity.h"
#include "proc_list.h"
#include "register.h"
#include "shell_builtin.h"

#define SC_PATH 512
#define SC_TITLES_MAX 1024
#define SC_WALK_DEPTH 24
/* A Sony update removal is watched this long before it is reported as still going. */
#define SC_PATCH_WAIT_MS (5u * 60u * 1000u)
/* An orphaned update (B): Sony answers 0 even for a title it has no update of (measured on FW 13.60
 * with a planted /user/patch/<id>, nothing removed), so after this wait its files are deleted. */
#define SC_ORPHAN_PATCH_WAIT_MS (30u * 1000u)
/* ps5upload temp files touched this recently may belong to something running: listed, not ticked. */
#define SC_RECENT_S (30 * 60)
/* "Worth a look": a single file at least this big outside game folders. */
#define SC_BIG_FILE (20ull * 1000ull * 1000ull * 1000ull)
#define SC_UPLOAD_ROOTS 16

typedef struct {
    char cat;            /* 'A'..'G' */
    char action;         /* 's' Sony update removal, 'd' delete, 0 none */
    uint8_t is_dir;
    uint8_t def;         /* ticked by default */
    uint8_t game_backup; /* A: the game runs from a backup (a mount stub in user/app) */
    char title_id[10];
    char path[SC_PATH];
    char name[96];
    char state[24];
    char kind[8];
    char version[16];
    char inst_version[16];
    char game_drive[8];
    uint64_t bytes, alloc;
    uint32_t files;
    int64_t mtime;
    /* apply */
    uint8_t res; /* 0 not yet, 1 done, 2 skipped, 3 failed */
    char reason[24];
    uint32_t rc;
} sc_item_t;

typedef struct {
    char tid[10];
    char name[96];
} sc_title_t;

/* ---- state ---- */

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
    uint32_t scan_id;
    int scan_state;      /* 0 none, 1 running, 2 done, 3 cancelled, 4 failed */
    const char *phase;
    uint64_t scan_done_ms;
    volatile int scan_cancel;
    sc_item_t *items;
    int n_items;
    int truncated;
    int appdb_ok;
    int can_sony;
    uint64_t reserve, free_b, total_b;
    char fail[32];
    /* apply */
    uint32_t apply_id;
    int apply_state;     /* 0 none, 1 running, 2 done, 3 cancelled */
    volatile int apply_cancel;
    int apply_current;   /* item index + 1 being removed, 0 none */
    const char *apply_phase;
    uint32_t *apply_ids; /* the requested items, in order */
    int apply_n;
    uint64_t freed;
} G;

/* What the scan thread works with (only it touches these). */
static sc_title_t *g_titles;
static int g_ntitles;

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static uint32_t new_id(void) {
    static uint32_t seq;
    uint32_t v = ((uint32_t)time(NULL) * 2654435761u) ^ (++seq * 40503u) ^ (uint32_t)now_ms();
    return v ? v & 0x7FFFFFFFu : 1u;
}

/* ---- JSON out ---- */

typedef struct {
    char *b;
    size_t cap, len;
    int over;
} jb_t;

static void jb_raw(jb_t *j, const char *s, size_t n) {
    if (j->over || j->len + n + 1 > j->cap) {
        j->over = 1;
        return;
    }
    memcpy(j->b + j->len, s, n);
    j->len += n;
    j->b[j->len] = '\0';
}
static void jb_s(jb_t *j, const char *s) { jb_raw(j, s, strlen(s)); }
static void jb_f(jb_t *j, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void jb_f(jb_t *j, const char *fmt, ...) {
    va_list ap;
    if (j->over) return;
    va_start(ap, fmt);
    int k = vsnprintf(j->b + j->len, j->cap - j->len, fmt, ap);
    va_end(ap);
    if (k < 0 || (size_t)k >= j->cap - j->len) {
        j->over = 1;
        return;
    }
    j->len += (size_t)k;
}
/* "s" with JSON escapes (bytes >= 0x80 pass through: app.db names are UTF-8). */
static void jb_str(jb_t *j, const char *s) {
    char tmp[8];
    jb_raw(j, "\"", 1);
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '"' || *p == '\\') {
            tmp[0] = '\\';
            tmp[1] = (char)*p;
            jb_raw(j, tmp, 2);
        } else if (*p < 0x20) {
            snprintf(tmp, sizeof tmp, "\\u%04x", *p);
            jb_raw(j, tmp, 6);
        } else {
            jb_raw(j, (const char *)p, 1);
        }
    }
    jb_raw(j, "\"", 1);
}

static int reply_err(char *out, size_t cap, const char *tok) {
    int k = snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", tok);
    return k < 0 ? 0 : (int)((size_t)k < cap ? (size_t)k : cap - 1);
}

int sc_reply_error(const char *body, size_t n, char *tok, size_t cap) {
    if (!body || n < 11 || strncmp(body, "{\"ok\":false", 11) != 0) return 0;
    if (!scr_json_str(body, n, "err", tok, cap)) snprintf(tok, cap, "internal");
    return 1;
}

/* ---- filesystem helpers ---- */

static int is_dir_path(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0 && S_ISDIR(st.st_mode);
}
static int exists_path(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0;
}

/* Sizes of everything below `path` (itself included), without following links or leaving the
 * device of the top. `buf` holds the path and is extended in place (SC_PATH bytes). */
typedef struct {
    uint64_t bytes, alloc;
    uint32_t files;
} sc_size_t;

static void tree_in(char *buf, size_t len, dev_t dev, int depth, sc_size_t *sz) {
    struct stat st;
    if (G.scan_cancel && depth > 0) return;
    if (lstat(buf, &st) != 0) return;
    if (st.st_dev != dev) return;
    if (!S_ISDIR(st.st_mode)) {
        sz->bytes += (uint64_t)st.st_size;
        sz->alloc += (uint64_t)st.st_blocks * 512u;
        sz->files++;
        return;
    }
    sz->alloc += (uint64_t)st.st_blocks * 512u;
    if (depth >= SC_WALK_DEPTH) return;
    DIR *d = opendir(buf);
    if (!d) return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        size_t nl = strlen(e->d_name);
        if (len + 1 + nl >= SC_PATH) continue;
        buf[len] = '/';
        memcpy(buf + len + 1, e->d_name, nl + 1);
        tree_in(buf, len + 1 + nl, dev, depth + 1, sz);
        buf[len] = '\0';
    }
    closedir(d);
}

static int tree_size(const char *path, sc_size_t *sz) {
    struct stat st;
    char buf[SC_PATH];
    memset(sz, 0, sizeof *sz);
    if (lstat(path, &st) != 0 || strlen(path) >= SC_PATH) return -1;
    memcpy(buf, path, strlen(path) + 1);
    tree_in(buf, strlen(buf), st.st_dev, 0, sz);
    return 0;
}

/* 1 when the folder has any entry. */
static int dir_has_entries(const char *path) {
    DIR *d = opendir(path);
    if (!d) return 0;
    struct dirent *e;
    int any = 0;
    while ((e = readdir(d)) != NULL)
        if (strcmp(e->d_name, ".") != 0 && strcmp(e->d_name, "..") != 0) {
            any = 1;
            break;
        }
    closedir(d);
    return any;
}

/* The console has an install queued or running (Sony's background-transfer tasks). */
static int bgft_busy(void) {
    return dir_has_entries("/user/bgft/task") || dir_has_entries("/user/bgft/ext0/task") ||
           dir_has_entries("/user/bgft/ext1/task");
}

static int pkg_read_fd(void *ctx, uint64_t off, void *buf, size_t n) {
    int fd = *(int *)ctx;
    size_t got = 0;
    while (got < n) {
        ssize_t r = pread(fd, (char *)buf + got, n - got, (off_t)(off + got));
        if (r <= 0) return -1;
        got += (size_t)r;
    }
    return 0;
}

static int pkg_info(const char *path, uint64_t size, scr_pkg_info_t *pi) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int rc = scr_pkg_read(pkg_read_fd, &fd, size, pi);
    close(fd);
    return rc;
}

/* ---- titles ---- */

static const char *const k_user_roots[] = { "/user", "/mnt/ext0/user", "/mnt/ext1/user" };
#define N_USER_ROOTS (sizeof k_user_roots / sizeof k_user_roots[0])

static int load_titles(void) {
    content_db_app_t *rows = calloc(SC_TITLES_MAX, sizeof *rows);
    g_ntitles = 0;
    if (!rows) return -1;
    int n = content_db_apps(rows, SC_TITLES_MAX, NULL);
    if (n >= 0) {
        g_titles = calloc((size_t)(n ? n : 1), sizeof *g_titles);
        if (!g_titles) n = -1;
        for (int i = 0; i < n; i++) {
            snprintf(g_titles[i].tid, sizeof g_titles[i].tid, "%.9s", rows[i].title_id);
            snprintf(g_titles[i].name, sizeof g_titles[i].name, "%s", rows[i].name);
        }
        if (n > 0) g_ntitles = n;
    }
    free(rows);
    return n < 0 ? -1 : 0;
}

static const sc_title_t *title_row(const char *tid) {
    for (int i = 0; i < g_ntitles; i++)
        if (strcmp(g_titles[i].tid, tid) == 0) return &g_titles[i];
    return NULL;
}

/* Where user/app/<tid> lives: index into k_user_roots, or -1. */
static int app_root_of(const char *tid) {
    char p[96];
    for (size_t i = 0; i < N_USER_ROOTS; i++) {
        snprintf(p, sizeof p, "%s/app/%s", k_user_roots[i], tid);
        if (exists_path(p)) return (int)i;
    }
    return -1;
}

/* 1 installed, 0 gone. Unknown (app.db unreadable and no app folder) reads as installed. */
static int installed(const char *tid) {
    if (title_row(tid)) return 1;
    if (app_root_of(tid) >= 0) return 1;
    return G.appdb_ok ? 0 : 1;
}

/* Installed versions from appinfo.db (CONTENT_VERSION, else APP_VER). */
typedef struct {
    char content[16], app[16];
} sc_ver_t;
static int ver_row(void *ctx, int ncol, const char *const *vals) {
    sc_ver_t *v = ctx;
    if (ncol < 2 || !vals[0] || !vals[1]) return 0;
    if (strcmp(vals[0], "CONTENT_VERSION") == 0) snprintf(v->content, sizeof v->content, "%s", vals[1]);
    else if (strcmp(vals[0], "APP_VER") == 0) snprintf(v->app, sizeof v->app, "%s", vals[1]);
    return 0;
}
static void installed_version(const char *tid, char *out, size_t cap) {
    sc_ver_t v;
    char sql[160];
    memset(&v, 0, sizeof v);
    out[0] = '\0';
    if (!scr_is_title_id(tid)) return;
    snprintf(sql, sizeof sql,
             "SELECT key, val FROM tbl_appinfo WHERE titleId='%s' AND key IN ('CONTENT_VERSION','APP_VER')", tid);
    (void)content_db_select_text(CONTENT_DB_APPINFO, sql, 8, &v, ver_row);
    snprintf(out, cap, "%s", v.content[0] ? v.content : v.app);
}

/* ---- items ---- */

static sc_item_t *item_new(char cat, const char *path, const char *tid) {
    if (G.n_items >= SC_MAX_ITEMS) {
        G.truncated = 1;
        return NULL;
    }
    if (strlen(path) >= SC_PATH) return NULL;
    sc_item_t it;
    memset(&it, 0, sizeof it);
    it.cat = cat;
    snprintf(it.path, sizeof it.path, "%s", path);
    if (tid) {
        snprintf(it.title_id, sizeof it.title_id, "%s", tid);
        const sc_title_t *t = title_row(tid);
        if (t) snprintf(it.name, sizeof it.name, "%s", t->name);
    }
    pthread_mutex_lock(&g_mu);
    G.items[G.n_items] = it;
    sc_item_t *p = &G.items[G.n_items++];
    pthread_mutex_unlock(&g_mu);
    return p;
}

static void item_size(sc_item_t *it) {
    struct stat st;
    if (lstat(it->path, &st) != 0) return;
    it->mtime = (int64_t)st.st_mtime;
    it->is_dir = S_ISDIR(st.st_mode) ? 1 : 0;
    if (it->is_dir) {
        sc_size_t sz;
        if (tree_size(it->path, &sz) == 0) {
            it->bytes = sz.bytes;
            it->alloc = sz.alloc;
            it->files = sz.files;
        }
    } else {
        it->bytes = (uint64_t)st.st_size;
        it->alloc = (uint64_t)st.st_blocks * 512u;
        it->files = 1;
    }
}

static void set_phase(const char *p) {
    pthread_mutex_lock(&g_mu);
    G.phase = p;
    int n = G.n_items;
    pthread_mutex_unlock(&g_mu);
    PLOG(PLOG_DEBUG, PLOG_CLEANUP, "[cleanup] scan phase %s (%d items so far)", p, n);
}

/* ---- A / B: installed updates ---- */

static void scan_updates(void) {
    char dir[64], path[96], tmp[SC_PATH];
    for (size_t r = 0; r < N_USER_ROOTS && !G.scan_cancel; r++) {
        snprintf(dir, sizeof dir, "%s/patch", k_user_roots[r]);
        DIR *d = opendir(dir);
        if (!d) continue;
        struct dirent *e;
        while ((e = readdir(d)) != NULL && !G.scan_cancel) {
            if (!scr_is_title_id(e->d_name) || scr_is_system_title(e->d_name)) continue;
            snprintf(path, sizeof path, "%s/%s", dir, e->d_name);
            if (!is_dir_path(path)) continue;
            int inst = installed(e->d_name);
            sc_item_t *it = item_new(inst ? 'A' : 'B', path, e->d_name);
            if (!it) continue;
            item_size(it);
            snprintf(it->kind, sizeof it->kind, "patch");
            /* the update's own version, from its package header */
            snprintf(tmp, sizeof tmp, "%s/patch.pkg", path);
            struct stat st;
            scr_pkg_info_t pi;
            if (lstat(tmp, &st) == 0 && S_ISREG(st.st_mode) && pkg_info(tmp, (uint64_t)st.st_size, &pi) == 0)
                snprintf(it->version, sizeof it->version, "%s", pi.version);
            if (inst) {
                int ar = app_root_of(e->d_name);
                snprintf(it->state, sizeof it->state, "update_installed");
                if (ar >= 0) {
                    char lnk[128];
                    snprintf(it->game_drive, sizeof it->game_drive, "%s", ar == 0 ? "internal" : ar == 1 ? "ext0" : "ext1");
                    snprintf(lnk, sizeof lnk, "%s/app/%s/mount.lnk", k_user_roots[ar], e->d_name);
                    int stub = exists_path(lnk);
                    snprintf(lnk, sizeof lnk, "%s/app/%s/mount_img.lnk", k_user_roots[ar], e->d_name);
                    stub = stub || exists_path(lnk);
                    if (stub) {
                        /* a backup: where its folder or image is, from the link file */
                        it->game_backup = 1;
                        int fd = open(lnk, O_RDONLY);
                        if (fd < 0) {
                            snprintf(lnk, sizeof lnk, "%s/app/%s/mount.lnk", k_user_roots[ar], e->d_name);
                            fd = open(lnk, O_RDONLY);
                        }
                        if (fd >= 0) {
                            ssize_t k = read(fd, tmp, 255);
                            close(fd);
                            if (k > 0) {
                                tmp[k] = '\0';
                                tmp[strcspn(tmp, "\r\n")] = '\0';
                                const char *dv = scr_drive_of(tmp);
                                if (dv[0]) snprintf(it->game_drive, sizeof it->game_drive, "%s", dv);
                            }
                        }
                    } else {
                        /* the base package's version */
                        snprintf(tmp, sizeof tmp, "%s/app/%s/app.pkg", k_user_roots[ar], e->d_name);
                        if (lstat(tmp, &st) == 0 && S_ISREG(st.st_mode) &&
                            pkg_info(tmp, (uint64_t)st.st_size, &pi) == 0)
                            snprintf(it->inst_version, sizeof it->inst_version, "%s", pi.version);
                    }
                }
                it->action = G.can_sony ? 's' : 0;
                it->def = 0;
            } else {
                snprintf(it->state, sizeof it->state, "game_gone");
                it->action = G.can_sony ? 's' : 'd';
                it->def = 1;
            }
        }
        closedir(d);
    }
}

/* ---- C (+ G big files): leftover packages on every drive ---- */

typedef struct {
    const char *root; /* "/data", "/mnt/ext0", "/mnt/usb0" */
    dev_t dev;
    int max_depth;
} sc_walk_t;

static int is_internal_drive(const char *path) { return strcmp(scr_drive_of(path), "internal") == 0; }

static void classify_pkg(sc_item_t *it) {
    scr_pkg_info_t pi;
    size_t pl = strlen(it->path);
    if (pl > 5 && strcmp(it->path + pl - 5, ".part") == 0) {
        snprintf(it->state, sizeof it->state, "partial");
        return;
    }
    if (pkg_info(it->path, it->bytes, &pi) != 0) {
        snprintf(it->state, sizeof it->state, "unreadable");
        return;
    }
    snprintf(it->title_id, sizeof it->title_id, "%s", pi.title_id);
    const sc_title_t *t = title_row(pi.title_id);
    if (t) snprintf(it->name, sizeof it->name, "%s", t->name);
    snprintf(it->version, sizeof it->version, "%s", pi.version);
    snprintf(it->kind, sizeof it->kind, "%s", pi.kind == 'p' ? "patch" : pi.kind == 'd' ? "dlc" : pi.kind == 'b' ? "base" : "");
    int game = title_row(pi.title_id) != NULL || app_root_of(pi.title_id) >= 0;
    int ok = 0;
    if (pi.kind == 'd') {
        char label[17], p[128];
        int found = 0;
        if (scr_label_of_content_id(pi.content_id, label))
            for (size_t r = 0; r < N_USER_ROOTS && !found; r++) {
                snprintf(p, sizeof p, "%s/addcont/%s/%s", k_user_roots[r], pi.title_id, label);
                found = exists_path(p);
            }
        snprintf(it->state, sizeof it->state, "%s", found ? "dlc_installed" : "dlc_not_installed");
        ok = found;
    } else if (!game) {
        snprintf(it->state, sizeof it->state, "not_installed");
    } else if (pi.kind == 'p') {
        /* the installed update's version: its patch.pkg, else appinfo.db */
        char p[128];
        struct stat st;
        scr_pkg_info_t ip;
        it->inst_version[0] = '\0';
        for (size_t r = 0; r < N_USER_ROOTS && !it->inst_version[0]; r++) {
            snprintf(p, sizeof p, "%s/patch/%s/patch.pkg", k_user_roots[r], pi.title_id);
            if (lstat(p, &st) == 0 && S_ISREG(st.st_mode) && pkg_info(p, (uint64_t)st.st_size, &ip) == 0)
                snprintf(it->inst_version, sizeof it->inst_version, "%s", ip.version);
        }
        if (!it->inst_version[0]) installed_version(pi.title_id, it->inst_version, sizeof it->inst_version);
        int c = scr_version_cmp(it->inst_version, pi.version);
        if (c == 0) {
            snprintf(it->state, sizeof it->state, "installed_same");
            ok = 1;
        } else if (c == 1) {
            snprintf(it->state, sizeof it->state, "installed_newer");
            ok = 1;
        } else {
            snprintf(it->state, sizeof it->state, "not_installed_yet");
        }
    } else {
        snprintf(it->state, sizeof it->state, "game_installed");
        ok = 1;
    }
    /* Ticked only on the internal drive: a package collection on a USB or extended drive is
     * usually kept on purpose, even for an installed game. */
    it->def = (uint8_t)(ok && is_internal_drive(it->path));
}

static void walk_pkgs(const sc_walk_t *w, char *buf, size_t len, size_t root_len, int depth) {
    DIR *d;
    struct dirent *e;
    if (G.scan_cancel || depth > w->max_depth) return;
    d = opendir(buf);
    if (!d) return;
    while ((e = readdir(d)) != NULL && !G.scan_cancel) {
        struct stat st;
        const char *nm = e->d_name;
        size_t nl = strlen(nm);
        if (nm[0] == '.') continue; /* ., .., and hidden system folders on USB drives */
        if (len + 1 + nl >= SC_PATH) continue;
        buf[len] = '/';
        memcpy(buf + len + 1, nm, nl + 1);
        if (lstat(buf, &st) != 0 || st.st_dev != w->dev) goto next;
        if (S_ISDIR(st.st_mode)) {
            if (!scr_walk_skip(w->root, buf + root_len + 1)) walk_pkgs(w, buf, len + 1 + nl, root_len, depth + 1);
            goto next;
        }
        if (!S_ISREG(st.st_mode)) goto next;
        {
            int pkg = (nl > 4 && (strcmp(nm + nl - 4, ".pkg") == 0 || strcmp(nm + nl - 4, ".PKG") == 0)) ||
                      (nl > 9 && strcmp(nm + nl - 9, ".pkg.part") == 0);
            int in_homebrew = strncmp(buf + root_len + 1, "homebrew/", 9) == 0;
            if (pkg && scr_path_allowed('C', buf, "")) {
                sc_item_t *it = item_new('C', buf, NULL);
                if (it) {
                    item_size(it);
                    it->action = 'd';
                    classify_pkg(it);
                }
            } else if (!in_homebrew && nl > 4 && strcasecmp(nm + nl - 4, ".pup") == 0 && is_internal_drive(buf)) {
                sc_item_t *it = item_new('G', buf, NULL);
                if (it) {
                    item_size(it);
                    snprintf(it->state, sizeof it->state, "system_update_file");
                }
            } else if (!in_homebrew && !pkg && (uint64_t)st.st_size >= SC_BIG_FILE) {
                sc_item_t *it = item_new('G', buf, NULL);
                if (it) {
                    item_size(it);
                    snprintf(it->state, sizeof it->state, "large_file");
                }
            }
        }
    next:
        buf[len] = '\0';
    }
    closedir(d);
}

/* The drives the package search and the temp-file search look at: /data, extended drives, USB. */
static int drive_roots(char roots[][16], int max) {
    int n = 0;
    struct stat mnt, st;
    snprintf(roots[n++], 16, "/data");
    if (lstat("/mnt", &mnt) != 0) return n;
    for (int i = 0; i < 2 && n < max; i++) {
        char p[16];
        snprintf(p, sizeof p, "/mnt/ext%d", i);
        if (lstat(p, &st) == 0 && S_ISDIR(st.st_mode) && st.st_dev != mnt.st_dev) snprintf(roots[n++], 16, "%s", p);
    }
    for (int i = 0; i < 8 && n < max; i++) {
        char p[16];
        snprintf(p, sizeof p, "/mnt/usb%d", i);
        if (lstat(p, &st) == 0 && S_ISDIR(st.st_mode) && st.st_dev != mnt.st_dev) snprintf(roots[n++], 16, "%s", p);
    }
    return n;
}

static void scan_packages(char roots[][16], int nroots) {
    char buf[SC_PATH];
    for (int i = 0; i < nroots && !G.scan_cancel; i++) {
        sc_walk_t w;
        struct stat st;
        if (lstat(roots[i], &st) != 0) continue;
        w.root = roots[i];
        w.dev = st.st_dev;
        w.max_depth = strcmp(roots[i], "/data") == 0 ? 3 : 2; /* depth 0 is the root's own entries */
        snprintf(buf, sizeof buf, "%s", roots[i]);
        walk_pkgs(&w, buf, strlen(buf), strlen(buf), 0);
    }
}

/* ---- D: ps5upload temp files ---- */

static int is_mount_point(const char *path) {
    struct statfs *m = NULL;
    int n = mntinfo_snapshot(&m), hit = 0;
    for (int i = 0; i < n && m; i++)
        if (strcmp(m[i].f_mntonname, path) == 0 || (strncmp(m[i].f_mntonname, path, strlen(path)) == 0 &&
                                                     m[i].f_mntonname[strlen(path)] == '/')) {
            hit = 1;
            break;
        }
    free(m);
    return hit;
}

static void scan_temp_dir(const char *dir) {
    struct stat top, st;
    char path[SC_PATH];
    if (lstat(dir, &top) != 0 || !S_ISDIR(top.st_mode)) return;
    DIR *d = opendir(dir);
    if (!d) return;
    struct dirent *e;
    time_t now = time(NULL);
    while ((e = readdir(d)) != NULL && !G.scan_cancel) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if ((size_t)snprintf(path, sizeof path, "%s/%s", dir, e->d_name) >= sizeof path) continue;
        if (lstat(path, &st) != 0 || st.st_dev != top.st_dev || is_mount_point(path)) continue;
        if (!scr_path_allowed('D', path, "")) continue;
        sc_item_t *it = item_new('D', path, NULL);
        if (!it) continue;
        item_size(it);
        it->action = 'd';
        int recent = now - st.st_mtime < SC_RECENT_S;
        snprintf(it->state, sizeof it->state, "%s", recent ? "recent" : "temp");
        it->def = (uint8_t)!recent;
    }
    closedir(d);
}

static void scan_temp(char roots[][16], int nroots) {
    char p[64];
    for (int i = 0; i < nroots && !G.scan_cancel; i++) {
        snprintf(p, sizeof p, "%s/ps5upload/pkg_temp", roots[i]);
        scan_temp_dir(p);
        snprintf(p, sizeof p, "%s/ps5upload/tests", roots[i]);
        scan_temp_dir(p);
    }
    scan_temp_dir("/mnt/ps5upload/.unpack");
}

/* ---- E: Sony download files of games that are gone ---- */

static void scan_downloads(void) {
    char p[96];
    DIR *d = opendir("/user/download");
    if (!d) return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && !G.scan_cancel) {
        struct stat st;
        if (!scr_is_title_id(e->d_name) || scr_is_system_title(e->d_name)) continue;
        if (installed(e->d_name)) continue;
        snprintf(p, sizeof p, "/user/download/%s/download0.dat", e->d_name);
        if (lstat(p, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        sc_item_t *it = item_new('E', p, e->d_name);
        if (!it) continue;
        item_size(it);
        it->action = 'd';
        snprintf(it->state, sizeof it->state, "game_gone");
        it->def = 0;
    }
    closedir(d);
}

/* ---- F (+ G add-ons): leftovers of removed games ---- */

static void scan_leftovers(void) {
    char dir[96], p[SC_PATH];
    for (size_t r = 0; r < N_USER_ROOTS && !G.scan_cancel; r++) {
        snprintf(dir, sizeof dir, "%s/appmeta", k_user_roots[r]);
        DIR *d = opendir(dir);
        if (d) {
            struct dirent *e;
            while ((e = readdir(d)) != NULL && !G.scan_cancel) {
                if (!scr_is_title_id(e->d_name) || scr_is_system_title(e->d_name) || installed(e->d_name)) continue;
                snprintf(p, sizeof p, "%s/%s", dir, e->d_name);
                if (!is_dir_path(p)) continue;
                sc_item_t *it = item_new('F', p, e->d_name);
                if (!it) continue;
                item_size(it);
                it->action = 'd';
                it->def = 1;
                snprintf(it->state, sizeof it->state, "appmeta_orphan");
            }
            closedir(d);
        }
        snprintf(dir, sizeof dir, "%s/addcont", k_user_roots[r]);
        d = opendir(dir);
        if (d) {
            struct dirent *e;
            while ((e = readdir(d)) != NULL && !G.scan_cancel) {
                if (!scr_is_title_id(e->d_name) || scr_is_system_title(e->d_name) || installed(e->d_name)) continue;
                snprintf(p, sizeof p, "%s/%s", dir, e->d_name);
                if (!is_dir_path(p)) continue;
                sc_item_t *it = item_new('G', p, e->d_name);
                if (!it) continue;
                item_size(it);
                snprintf(it->state, sizeof it->state, "addcont_orphan");
            }
            closedir(d);
        }
    }
    static const char *const playgo[] = { "/user/playgo/app", "/user/playgo/patch", "/user/playgo/addcont" };
    for (size_t i = 0; i < sizeof playgo / sizeof playgo[0] && !G.scan_cancel; i++) {
        DIR *d = opendir(playgo[i]);
        if (!d) continue;
        struct dirent *e;
        while ((e = readdir(d)) != NULL && !G.scan_cancel) {
            char tid[10];
            if (!scr_tid_of_content_id(e->d_name, tid) || scr_is_system_title(tid) || installed(tid)) continue;
            if ((size_t)snprintf(p, sizeof p, "%s/%s", playgo[i], e->d_name) >= sizeof p) continue;
            if (!is_dir_path(p) || !scr_path_allowed('F', p, tid)) continue;
            sc_item_t *it = item_new('F', p, tid);
            if (!it) continue;
            item_size(it);
            it->action = 'd';
            it->def = 1;
            snprintf(it->state, sizeof it->state, "playgo_orphan");
        }
        closedir(d);
    }
}

/* ---- G: the same game twice in homebrew/ ---- */

typedef struct {
    char tid[10];
    char path[SC_PATH];
} sc_hb_t;

static void scan_duplicates(char roots[][16], int nroots) {
    enum { MAXHB = 256 };
    sc_hb_t *hb = calloc(MAXHB, sizeof *hb);
    int n = 0;
    if (!hb) return;
    for (int i = 0; i < nroots && !G.scan_cancel; i++) {
        char dir[48];
        snprintf(dir, sizeof dir, "%s/homebrew", roots[i]);
        DIR *d = opendir(dir);
        if (!d) continue;
        struct dirent *e;
        while ((e = readdir(d)) != NULL && n < MAXHB) {
            char tid[10];
            if (strlen(e->d_name) < 9) continue;
            memcpy(tid, e->d_name, 9);
            tid[9] = '\0';
            if (!scr_is_title_id(tid)) continue;
            if ((size_t)snprintf(hb[n].path, sizeof hb[n].path, "%s/%s", dir, e->d_name) >= sizeof hb[n].path) continue;
            memcpy(hb[n].tid, tid, 10);
            n++;
        }
        closedir(d);
    }
    for (int i = 0; i < n && !G.scan_cancel; i++) {
        int dup = 0;
        for (int k = 0; k < n; k++)
            if (k != i && strcmp(hb[k].tid, hb[i].tid) == 0) dup = 1;
        if (!dup) continue;
        sc_item_t *it = item_new('G', hb[i].path, hb[i].tid);
        if (!it) continue;
        item_size(it);
        snprintf(it->state, sizeof it->state, "duplicate");
    }
    free(hb);
}

/* ---- the scan thread ---- */

static void *scan_main(void *arg) {
    char roots[12][16];
    (void)arg;
    proc_name_set_self("ps5upload-clean");
    set_phase("updates");
    int ok = load_titles() == 0;
    pthread_mutex_lock(&g_mu);
    G.appdb_ok = ok;
    G.can_sony = uninstall_patch_available();
    pthread_mutex_unlock(&g_mu);
    fprintf(stderr, "[cleanup] scan: %d titles from app.db (%s), patch removal %s\n", g_ntitles,
            ok ? "read" : "unreadable", G.can_sony ? "available" : "not exported");
    int nroots = drive_roots(roots, 12);
    scan_updates();
    set_phase("packages");
    if (!G.scan_cancel) scan_packages(roots, nroots);
    set_phase("temp");
    if (!G.scan_cancel) scan_temp(roots, nroots);
    set_phase("downloads");
    if (!G.scan_cancel) scan_downloads();
    set_phase("leftovers");
    if (!G.scan_cancel) scan_leftovers();
    set_phase("duplicates");
    if (!G.scan_cancel) scan_duplicates(roots, nroots);
    {
        struct statvfs v;
        if (statvfs("/user", &v) == 0) {
            pthread_mutex_lock(&g_mu);
            G.reserve = ((uint64_t)v.f_bfree - (uint64_t)v.f_bavail) * (uint64_t)v.f_frsize;
            G.free_b = (uint64_t)v.f_bavail * (uint64_t)v.f_frsize;
            G.total_b = (uint64_t)v.f_blocks * (uint64_t)v.f_frsize;
            pthread_mutex_unlock(&g_mu);
        }
    }
    free(g_titles);
    g_titles = NULL;
    g_ntitles = 0;
    pthread_mutex_lock(&g_mu);
    G.scan_state = G.scan_cancel ? 3 : 2;
    G.phase = "done";
    G.scan_done_ms = now_ms();
    fprintf(stderr, "[cleanup] scan %u %s: %d items%s\n", G.scan_id, G.scan_cancel ? "stopped" : "done", G.n_items,
            G.truncated ? " (truncated)" : "");
    pthread_mutex_unlock(&g_mu);
    return NULL;
}

static int spawn(void *(*fn)(void *)) {
    pthread_attr_t a;
    pthread_t t;
    int rc = -1;
    if (pthread_attr_init(&a) == 0) {
        (void)pthread_attr_setstacksize(&a, 512u * 1024u);
        (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
        rc = pthread_create(&t, &a, fn, NULL);
        pthread_attr_destroy(&a);
    }
    return rc;
}

int sc_scan_start(char *out, size_t cap) {
    pthread_mutex_lock(&g_mu);
    if (G.scan_state == 1 || G.apply_state == 1) {
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "busy");
    }
    sc_item_t *items = calloc(SC_MAX_ITEMS, sizeof *items);
    if (!items) {
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "out_of_memory");
    }
    free(G.items);
    G.items = items;
    G.n_items = 0;
    G.truncated = 0;
    G.scan_id = new_id();
    G.scan_state = 1;
    G.scan_cancel = 0;
    G.phase = "updates";
    G.reserve = G.free_b = G.total_b = 0;
    G.apply_state = 0;
    G.apply_id = 0;
    uint32_t id = G.scan_id;
    if (spawn(scan_main) != 0) {
        G.scan_state = 4;
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "no_thread");
    }
    pthread_mutex_unlock(&g_mu);
    int k = snprintf(out, cap, "{\"ok\":true,\"scan_id\":%u}", id);
    return k < 0 ? 0 : k;
}

static const char *scan_state_name(int s) {
    switch (s) {
    case 1: return "running";
    case 2: return "done";
    case 3: return "cancelled";
    case 4: return "failed";
    default: return "none";
    }
}

/* The unsigned number at "key" in a body of `n` bytes (not NUL-terminated); 0 when absent or bad. */
static uint32_t num_field(const char *body, size_t n, const char *key) {
    size_t kl = strlen(key);
    if (!body) return 0;
    for (size_t i = 0; i + kl + 2 < n; i++) {
        if (body[i] != '"' || memcmp(body + i + 1, key, kl) != 0 || body[i + 1 + kl] != '"') continue;
        size_t j = i + 2 + kl;
        while (j < n && body[j] == ' ') j++;
        if (j >= n || body[j] != ':') return 0;
        j++;
        while (j < n && body[j] == ' ') j++;
        uint64_t v = 0;
        int digits = 0;
        while (j < n && body[j] >= '0' && body[j] <= '9' && digits < 11) {
            v = v * 10u + (uint64_t)(body[j] - '0');
            j++;
            digits++;
        }
        return digits && v <= 0xFFFFFFFFu ? (uint32_t)v : 0;
    }
    return 0;
}

static void json_item(jb_t *j, const sc_item_t *it, int idx) {
    char cat[2] = { it->cat, 0 };
    jb_f(j, "{\"id\":%d,\"cat\":\"%s\",\"path\":", idx + 1, cat);
    jb_str(j, it->path);
    if (it->title_id[0]) jb_f(j, ",\"title_id\":\"%s\"", it->title_id);
    if (it->name[0]) {
        jb_s(j, ",\"name\":");
        jb_str(j, it->name);
    }
    jb_f(j, ",\"drive\":\"%s\",\"state\":\"%s\"", scr_drive_of(it->path), it->state);
    if (it->kind[0]) jb_f(j, ",\"kind\":\"%s\"", it->kind);
    if (it->version[0]) {
        jb_s(j, ",\"version\":");
        jb_str(j, it->version);
    }
    if (it->inst_version[0]) {
        jb_s(j, ",\"installed_version\":");
        jb_str(j, it->inst_version);
    }
    if (it->game_drive[0]) jb_f(j, ",\"game_drive\":\"%s\"", it->game_drive);
    if (it->game_backup) jb_s(j, ",\"game_backup\":true");
    jb_f(j,
         ",\"bytes\":%llu,\"alloc_bytes\":%llu,\"files\":%u,\"is_dir\":%s,\"mtime\":%lld,\"default\":%s,"
         "\"action\":\"%s\"}",
         (unsigned long long)it->bytes, (unsigned long long)it->alloc, it->files, it->is_dir ? "true" : "false",
         (long long)it->mtime, it->def ? "true" : "false",
         it->action == 's' ? "sony" : it->action == 'd' ? "delete" : "none");
}

int sc_scan_status(const char *body, size_t n, char *out, size_t cap) {
    uint32_t id = num_field(body, n, "scan_id");
    jb_t j = { out, cap, 0, 0 };
    pthread_mutex_lock(&g_mu);
    if (!id || id != G.scan_id || G.scan_state == 0) {
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "unknown_scan");
    }
    uint64_t age = G.scan_state == 1 ? 0 : now_ms() - G.scan_done_ms;
    jb_f(&j,
         "{\"ok\":true,\"scan_id\":%u,\"state\":\"%s\",\"phase\":\"%s\",\"age_ms\":%llu,\"appdb\":%s,"
         "\"can_remove_updates\":%s,\"install_running\":%s,\"found\":%d,\"truncated\":%s,\"reserve_bytes\":%llu,"
         "\"free_bytes\":%llu,\"total_bytes\":%llu",
         G.scan_id, scan_state_name(G.scan_state), G.phase ? G.phase : "", (unsigned long long)age,
         G.appdb_ok ? "true" : "false", G.can_sony ? "true" : "false", bgft_busy() ? "true" : "false", G.n_items,
         G.truncated ? "true" : "false", (unsigned long long)G.reserve, (unsigned long long)G.free_b,
         (unsigned long long)G.total_b);
    if (G.scan_state == 2 || G.scan_state == 3) {
        jb_s(&j, ",\"items\":[");
        for (int i = 0; i < G.n_items; i++) {
            if (i) jb_s(&j, ",");
            json_item(&j, &G.items[i], i);
        }
        jb_s(&j, "]");
    }
    jb_s(&j, "}");
    pthread_mutex_unlock(&g_mu);
    if (j.over) return reply_err(out, cap, "reply_too_large");
    return (int)j.len;
}

/* ---- apply ---- */

static void finish_item(sc_item_t *it, int res, const char *reason, uint32_t rc) {
    pthread_mutex_lock(&g_mu);
    it->res = (uint8_t)res;
    snprintf(it->reason, sizeof it->reason, "%s", reason ? reason : "");
    it->rc = rc;
    if (res == 1) G.freed += it->alloc;
    pthread_mutex_unlock(&g_mu);
    /* res: 1 removed, 2 skipped, 3 failed (failures at info): the reason
     * and Sony's code are what a report about "Clean-up did nothing" needs. */
    PLOG(res == 3 ? PLOG_INFO : PLOG_DEBUG, PLOG_CLEANUP, "[cleanup] item %c %s: result %d %s rc=0x%08X", it->cat,
         it->path, res, reason ? reason : "", (unsigned)rc);
}

static void set_apply_phase(int cur, const char *phase) {
    pthread_mutex_lock(&g_mu);
    G.apply_current = cur;
    G.apply_phase = phase;
    pthread_mutex_unlock(&g_mu);
}

/* An upload is writing at or below `path` (or into a folder that contains it). */
static int upload_touches(const char *path) {
    char (*roots)[AVA1_MAX_PATH + 1] = calloc(SC_UPLOAD_ROOTS, sizeof *roots);
    int hit = 0;
    if (!roots) return 1;
    int n = ava1_jobs_upload_roots(roots, SC_UPLOAD_ROOTS);
    size_t pl = strlen(path);
    for (int i = 0; i < n && !hit; i++) {
        size_t rl = strlen(roots[i]);
        if (strncmp(roots[i], path, pl) == 0 && (roots[i][pl] == '\0' || roots[i][pl] == '/')) hit = 1;
        if (strncmp(path, roots[i], rl) == 0 && (path[rl] == '\0' || path[rl] == '/')) hit = 1;
    }
    free(roots);
    return hit;
}

/* Still what the scan saw: same type, and the same size (a file: and time). */
static int unchanged(const sc_item_t *it) {
    struct stat st;
    if (lstat(it->path, &st) != 0) return 0;
    if (it->is_dir != (S_ISDIR(st.st_mode) ? 1 : 0)) return 0;
    if (S_ISLNK(st.st_mode)) return 0;
    if (!it->is_dir) return (uint64_t)st.st_size == it->bytes && (int64_t)st.st_mtime == it->mtime;
    sc_size_t sz;
    if (tree_size(it->path, &sz) != 0) return 0;
    return sz.bytes == it->bytes;
}

/* Waits up to `ms` for `path` to disappear. 1 gone, 0 still there, -1 Stop was asked meanwhile
 * (#714: Stop used to wait out the whole 5 minutes of every update). */
static int wait_gone(const char *path, uint32_t ms) {
    uint64_t end = now_ms() + ms;
    while (now_ms() < end) {
        if (!exists_path(path)) return 1;
        if (G.apply_cancel) return -1;
        usleep(500u * 1000u);
    }
    return !exists_path(path);
}

static void apply_one(sc_item_t *it, int idx) {
    const char *why = NULL;
    if (!it->action) {
        finish_item(it, 2, "not_removable", 0);
        return;
    }
    if (!scr_path_allowed(it->cat, it->path, it->title_id)) {
        fprintf(stderr, "[cleanup] refusing %s: not allowed for category %c\n", it->path, it->cat);
        finish_item(it, 3, "path_not_allowed", 0);
        return;
    }
    if (!exists_path(it->path)) {
        finish_item(it, 1, "already_gone", 0);
        return;
    }
    if (!unchanged(it)) why = "changed";
    else if (upload_touches(it->path)) why = "in_use";
    else if ((it->cat == 'A' || it->cat == 'B' || it->cat == 'F' || it->cat == 'E') && it->title_id[0] &&
             proc_find_pid_by_title_id(it->title_id) > 0)
        why = "game_running";
    else if (it->cat == 'E' && bgft_busy())
        why = "install_running";
    else if ((it->cat == 'B' || it->cat == 'E' || it->cat == 'F') && content_db_has_title(it->title_id) != 0)
        why = "game_installed"; /* came back (or app.db cannot say) since the scan */
    if (why) {
        finish_item(it, 2, why, 0);
        return;
    }
    set_apply_phase(idx + 1, "removing");
    fprintf(stderr, "[cleanup] removing %c %s\n", it->cat, it->path);
    if (it->action == 's') {
        uint32_t rc = 0;
        int r = uninstall_patch(it->title_id, &rc);
        if (r == 0) {
            set_apply_phase(idx + 1, "waiting");
            int gone = wait_gone(it->path, it->cat == 'A' ? SC_PATCH_WAIT_MS : SC_ORPHAN_PATCH_WAIT_MS);
            if (gone > 0) {
                finish_item(it, 1, "", 0);
                return;
            }
            if (gone < 0) {
                /* Sony may still finish removing it; a later scan shows which */
                finish_item(it, 2, "cancelled", 0);
                return;
            }
            if (it->cat == 'A' || content_db_has_title(it->title_id) != 0) {
                finish_item(it, 3, "still_present", 0);
                return;
            }
            fprintf(stderr, "[cleanup] Sony left the update of %s (no such title); deleting its files\n", it->title_id);
        } else if (it->cat == 'A') {
            /* never a plain delete of an installed game's update */
            finish_item(it, 3, r == -2 ? "not_supported" : "sony_refused", rc);
            return;
        } else {
            fprintf(stderr, "[cleanup] Sony declined the update of %s (0x%08X); deleting its files\n", it->title_id,
                    (unsigned)rc);
        }
    }
    int rc;
    if (it->is_dir) rc = fsj_rm_rf(it->path, 0, NULL);
    else rc = unlink(it->path) == 0 || errno == ENOENT ? 0 : -1;
    int e = errno;
    if (rc == 0 && it->cat == 'E') {
        /* the title's download folder goes too when nothing else is in it */
        char dir[SC_PATH];
        snprintf(dir, sizeof dir, "%s", it->path);
        char *slash = strrchr(dir, '/');
        if (slash) {
            *slash = '\0';
            (void)rmdir(dir);
        }
    }
    if (rc == 0 && !exists_path(it->path)) finish_item(it, 1, "", 0);
    else finish_item(it, 3, "delete_failed", (uint32_t)e);
}

static void *apply_main(void *arg) {
    (void)arg;
    proc_name_set_self("ps5upload-clean");
    for (int i = 0;; i++) {
        pthread_mutex_lock(&g_mu);
        int n = G.apply_n, cancel = G.apply_cancel;
        uint32_t id = i < n ? G.apply_ids[i] : 0;
        pthread_mutex_unlock(&g_mu);
        if (i >= n) break;
        sc_item_t *it = &G.items[id - 1];
        if (cancel) {
            finish_item(it, 2, "cancelled", 0);
            continue;
        }
        apply_one(it, (int)id - 1);
    }
    pthread_mutex_lock(&g_mu);
    G.apply_state = G.apply_cancel ? 3 : 2;
    G.apply_current = 0;
    G.apply_phase = "done";
    fprintf(stderr, "[cleanup] apply %u finished: %llu bytes freed\n", G.apply_id, (unsigned long long)G.freed);
    pthread_mutex_unlock(&g_mu);
    return NULL;
}

int sc_apply_start(const char *body, size_t n, char *out, size_t cap) {
    uint32_t sid = num_field(body, n, "scan_id");
    uint32_t *ids = calloc(SC_MAX_ITEMS, sizeof *ids);
    if (!ids) return reply_err(out, cap, "out_of_memory");
    int k = scr_parse_id_list(body, n, "item_ids", ids, SC_MAX_ITEMS);
    if (k <= 0) {
        free(ids);
        return reply_err(out, cap, "bad_request");
    }
    pthread_mutex_lock(&g_mu);
    const char *err = NULL;
    if (G.apply_state == 1 || G.scan_state == 1) err = "busy";
    else if (!sid || sid != G.scan_id || G.scan_state == 0) err = "unknown_scan";
    else if (G.scan_state != 2) err = "scan_not_done";
    else if (now_ms() - G.scan_done_ms > SC_SCAN_MAX_AGE_MS) err = "scan_expired";
    else if (bgft_busy()) err = "install_running";
    if (!err) {
        /* valid, distinct, and not already removed by an earlier apply of this scan */
        int m = 0;
        for (int i = 0; i < k; i++) {
            int dup = 0;
            if (ids[i] == 0 || (int)ids[i] > G.n_items) {
                err = "bad_request";
                break;
            }
            for (int q = 0; q < m; q++)
                if (ids[q] == ids[i]) dup = 1;
            if (!dup) ids[m++] = ids[i];
        }
        k = m;
    }
    if (err) {
        pthread_mutex_unlock(&g_mu);
        free(ids);
        return reply_err(out, cap, err);
    }
    for (int i = 0; i < G.n_items; i++) {
        int asked = 0;
        for (int q = 0; q < k; q++)
            if ((int)ids[q] == i + 1) asked = 1;
        if (asked) {
            G.items[i].res = 0;
            G.items[i].reason[0] = '\0';
            G.items[i].rc = 0;
        }
    }
    free(G.apply_ids);
    G.apply_ids = ids;
    G.apply_n = k;
    G.apply_id = new_id();
    G.apply_state = 1;
    G.apply_cancel = 0;
    G.apply_current = 0;
    G.apply_phase = "starting";
    G.freed = 0;
    uint32_t aid = G.apply_id;
    if (spawn(apply_main) != 0) {
        G.apply_state = 0;
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "no_thread");
    }
    pthread_mutex_unlock(&g_mu);
    fprintf(stderr, "[cleanup] apply %u of scan %u: %d items\n", aid, sid, k);
    int w = snprintf(out, cap, "{\"ok\":true,\"apply_id\":%u}", aid);
    return w < 0 ? 0 : w;
}

int sc_apply_status(const char *body, size_t n, char *out, size_t cap) {
    uint32_t id = num_field(body, n, "apply_id");
    jb_t j = { out, cap, 0, 0 };
    pthread_mutex_lock(&g_mu);
    /* No id: the latest one, so a screen that was not watching can find a running apply. */
    if (!id) id = G.apply_id;
    if (!id || id != G.apply_id || G.apply_state == 0) {
        pthread_mutex_unlock(&g_mu);
        return reply_err(out, cap, "unknown_apply");
    }
    jb_f(&j, "{\"ok\":true,\"apply_id\":%u,\"scan_id\":%u,\"state\":\"%s\",\"current\":%d,\"phase\":\"%s\",\"total\":%d",
         G.apply_id, G.scan_id, G.apply_state == 1 ? "running" : G.apply_state == 3 ? "cancelled" : "done",
         G.apply_current, G.apply_phase ? G.apply_phase : "", G.apply_n);
    jb_s(&j, ",\"done\":[");
    int first = 1;
    for (int q = 0; q < G.apply_n; q++) {
        const sc_item_t *it = &G.items[G.apply_ids[q] - 1];
        if (it->res != 1) continue;
        jb_f(&j, "%s%u", first ? "" : ",", G.apply_ids[q]);
        first = 0;
    }
    for (int pass = 2; pass <= 3; pass++) {
        jb_s(&j, pass == 2 ? "],\"skipped\":[" : "],\"failed\":[");
        first = 1;
        for (int q = 0; q < G.apply_n; q++) {
            const sc_item_t *it = &G.items[G.apply_ids[q] - 1];
            if (it->res != pass) continue;
            jb_f(&j, "%s{\"id\":%u,\"reason\":\"%s\"", first ? "" : ",", G.apply_ids[q], it->reason);
            if (it->rc) jb_f(&j, ",\"code\":\"0x%08X\"", (unsigned)it->rc);
            jb_s(&j, "}");
            first = 0;
        }
    }
    jb_f(&j, "],\"freed_bytes\":%llu}", (unsigned long long)G.freed);
    pthread_mutex_unlock(&g_mu);
    if (j.over) return reply_err(out, cap, "reply_too_large");
    return (int)j.len;
}

int sc_cancel(const char *body, size_t n, char *out, size_t cap) {
    uint32_t id = num_field(body, n, "id");
    int hit = 0;
    pthread_mutex_lock(&g_mu);
    if (id && id == G.scan_id && G.scan_state == 1) {
        G.scan_cancel = 1;
        hit = 1;
    } else if (id && id == G.apply_id && G.apply_state == 1) {
        G.apply_cancel = 1;
        hit = 1;
    } else if (id && (id == G.scan_id || id == G.apply_id)) {
        hit = 1; /* already finished: nothing to stop */
    }
    pthread_mutex_unlock(&g_mu);
    if (!hit) return reply_err(out, cap, "unknown_scan");
    int k = snprintf(out, cap, "{\"ok\":true}");
    return k < 0 ? 0 : k;
}
