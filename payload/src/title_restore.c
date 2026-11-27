/* ps5upload — putting an installed package game back on the home screen (#961). See title_restore.h.
 *
 * Only file operations: stat, mkdir, a same-device rename (checked first with cross_device.h),
 * unlink of our own copy, and the small record file. No Sony call (the install itself is the
 * engine's, through the installer daemon), so no sony_api_lock. One restore operation at a time
 * (g_restore_lock): two finishes of one title must not both move its package. */

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

#include "cross_device.h"
#include "install_guard.h"
#include "statvfs_avail.h"
#include "title_restore.h"

#define TR_PATH 640

static pthread_mutex_t g_restore_lock = PTHREAD_MUTEX_INITIALIZER;

typedef struct {
    char vol[32];      /* "" internal, else /mnt/ext<N> */
    char app_dir[96];  /* <vol>/user/app/<id> (internal: /user/app/<id>) */
    char pkg[128];     /* <app_dir>/app.pkg */
    char patch[128];   /* <vol>/user/patch/<id>/patch.pkg */
    char aside[128];   /* <vol>/ps5upload/restore/<id> (internal: /user/ps5upload/restore/<id>) */
    char root[32];     /* the volume's root, for free space: <vol> or /user */
} tr_vol_t;

typedef struct {
    const char *state;
    const char *reason; /* a word for logs and the UI, may be NULL */
    int vol;            /* index of the volume with the package (or the pending record), -1 none */
    uint64_t size, patch_size, free_volume, free_internal;
    int have_patch;     /* a patch.pkg on the package's drive */
    const char *kind;   /* "fake" | "ps4" | "unknown" */
    int free_known;
} tr_info_t;

static const char *pfx(const title_restore_env_t *env) {
    return env && env->prefix ? env->prefix : "";
}

/* <prefix><path> into buf; 0, or -1 when it does not fit. */
static int real_path(const title_restore_env_t *env, const char *path, char *buf, size_t cap) {
    int n = snprintf(buf, cap, "%s%s", pfx(env), path);
    return (n > 0 && (size_t)n < cap) ? 0 : -1;
}

static int exists(const title_restore_env_t *env, const char *path, struct stat *st) {
    char p[TR_PATH];
    struct stat local;
    if (real_path(env, path, p, sizeof p) != 0) return 0;
    return lstat(p, st ? st : &local) == 0;
}

/* The volumes: internal, ext0, ext1, then the mount table's other /mnt/ext<N>. */
static int tr_volumes(const title_restore_env_t *env, const char *id, tr_vol_t *v, int max) {
    const char *names[TITLE_RESTORE_MAX_VOLUMES];
    int n = 0;
    names[n++] = "";
    names[n++] = "/mnt/ext0";
    names[n++] = "/mnt/ext1";
    for (int i = 0; env && env->extra_volumes && i < env->nextra && n < TITLE_RESTORE_MAX_VOLUMES; i++) {
        const char *e = env->extra_volumes[i];
        if (!install_guard_is_ext_volume(e) || strlen(e) >= sizeof v[0].vol) continue;
        int dup = 0;
        for (int k = 0; k < n; k++)
            if (strcmp(names[k], e) == 0) dup = 1;
        if (!dup) names[n++] = e;
    }
    if (n > max) n = max;
    for (int i = 0; i < n; i++) {
        const char *vol = names[i];
        snprintf(v[i].vol, sizeof v[i].vol, "%s", vol);
        snprintf(v[i].app_dir, sizeof v[i].app_dir, "%s/user/app/%s", vol, id);
        snprintf(v[i].pkg, sizeof v[i].pkg, "%s/app.pkg", v[i].app_dir);
        snprintf(v[i].patch, sizeof v[i].patch, "%s/user/patch/%s/patch.pkg", vol, id);
        if (vol[0])
            snprintf(v[i].aside, sizeof v[i].aside, "%s/ps5upload/restore/%s", vol, id);
        else
            snprintf(v[i].aside, sizeof v[i].aside, "/user/ps5upload/restore/%s", id);
        snprintf(v[i].root, sizeof v[i].root, "%s", vol[0] ? vol : "/user");
    }
    return n;
}

static int free_of(const title_restore_env_t *env, const char *dir, uint64_t *out) {
    char p[TR_PATH];
    *out = 0;
    if (real_path(env, dir, p, sizeof p) != 0) return -1;
    if (env && env->free_bytes) return env->free_bytes(p, out);
    struct statvfs sv;
    if (statvfs(p, &sv) != 0) return -1;
    *out = statvfs_avail_bytes(&sv);
    return 0;
}

/* What the package's first bytes say: "fake" (a PS5 package signed for debug), "retail" (a PS5
 * package with the retail byte: refused), "ps4" (a PS4 package: its header does not tell), or
 * "unknown" (the installed file starts some other way; the installer decides). */
static const char *pkg_kind(const title_restore_env_t *env, const char *pkg) {
    char p[TR_PATH];
    unsigned char h[8] = {0};
    if (real_path(env, pkg, p, sizeof p) != 0) return "unknown";
    int fd = open(p, O_RDONLY);
    if (fd < 0) return "unknown";
    ssize_t got = read(fd, h, sizeof h);
    close(fd);
    if (got < 8) return "unknown";
    if (h[0] == 0x7F && h[1] == 'F' && h[2] == 'I' && h[3] == 'H') return h[5] == 0x00 ? "fake" : "retail";
    if (h[0] == 0x7F && h[1] == 'C' && h[2] == 'N' && h[3] == 'T') return "ps4";
    return "unknown";
}

static int record_path(const tr_vol_t *v, char *buf, size_t cap) {
    int n = snprintf(buf, cap, "%s/%s", v->aside, TITLE_RESTORE_RECORD);
    return (n > 0 && (size_t)n < cap) ? 0 : -1;
}

/* The volume holding a pending restore of this title (its record), or -1. */
static int find_pending(const title_restore_env_t *env, const tr_vol_t *v, int nv) {
    char rec[TR_PATH];
    for (int i = 0; i < nv; i++)
        if (record_path(&v[i], rec, sizeof rec) == 0 && exists(env, rec, NULL)) return i;
    return -1;
}

/* The deepest existing ancestor of `path` (console path), for a device comparison before the
 * folders exist. */
static int nearest_dev(const title_restore_env_t *env, const char *path, unsigned long long *dev) {
    char p[TR_PATH];
    if (real_path(env, path, p, sizeof p) != 0) return -1;
    size_t floor = strlen(pfx(env));
    for (;;) {
        struct stat st;
        if (stat(p, &st) == 0) {
            *dev = (unsigned long long)st.st_dev;
            return 0;
        }
        char *s = strrchr(p, '/');
        if (!s || (size_t)(s - p) < floor) return -1;
        if ((size_t)(s - p) == floor) {
            /* the root itself */
            if (floor == 0) snprintf(p, sizeof p, "/");
            else *s = '\0';
            if (stat(p, &st) != 0) return -1;
            *dev = (unsigned long long)st.st_dev;
            return 0;
        }
        *s = '\0';
    }
}

static void inspect(const title_restore_env_t *env, const char *id, tr_vol_t *v, int nv, tr_info_t *in) {
    memset(in, 0, sizeof *in);
    in->vol = -1;
    in->kind = "unknown";
    int pending = find_pending(env, v, nv);
    if (pending >= 0) {
        in->vol = pending;
        in->state = "pending";
        return;
    }
    int hits = 0;
    for (int i = 0; i < nv; i++) {
        struct stat st;
        if (exists(env, v[i].pkg, &st)) {
            if (hits++ == 0) {
                in->vol = i;
                in->size = (uint64_t)st.st_size;
            }
        }
    }
    if (hits == 0) {
        in->state = "not_installed";
        return;
    }
    if (hits > 1) {
        in->state = "ambiguous";
        in->reason = "the same title's app.pkg is on two drives";
        return;
    }
    const tr_vol_t *pv = &v[in->vol];
    {
        struct stat st;
        if (!exists(env, pv->pkg, &st) || !S_ISREG(st.st_mode)) {
            in->state = "not_installed";
            in->reason = "app.pkg is not a file";
            return;
        }
    }
    /* A folder registration (ours or ShadowMount+'s) is not a package install. */
    if (install_guard_has_tracker(pfx(env), id)) {
        in->state = "folder_registration";
        return;
    }
    int db = env && env->appdb_has ? env->appdb_has(id) : -1;
    if (db < 0) {
        in->state = "db_unreadable";
        in->reason = "app.db";
        return;
    }
    if (db > 0) {
        in->state = "registered";
        return;
    }
    int info = env && env->appinfo_has ? env->appinfo_has(id) : -1;
    if (info < 0) {
        in->state = "db_unreadable";
        in->reason = "appinfo.db";
        return;
    }
    if (info > 0) {
        in->state = "record_elsewhere";
        return;
    }
    in->kind = pkg_kind(env, pv->pkg);
    if (strcmp(in->kind, "retail") == 0) {
        in->state = "retail";
        return;
    }
    {
        struct stat st;
        if (exists(env, pv->patch, &st) && S_ISREG(st.st_mode)) {
            in->have_patch = 1;
            in->patch_size = (uint64_t)st.st_size;
        }
    }
    /* The installer writes a whole new copy where Settings › Storage › Installation location says:
     * the package's own drive or the console's. One of them needs the room. */
    int fv = free_of(env, pv->root, &in->free_volume);
    int fi = free_of(env, "/user", &in->free_internal);
    in->free_known = fv == 0 || fi == 0;
    if (in->free_known && !((fv == 0 && in->free_volume >= in->size) || (fi == 0 && in->free_internal >= in->size))) {
        in->state = "no_room";
        return;
    }
    /* The move aside must stay on the package's device. */
    {
        unsigned long long dpkg = 0, daside = 0;
        char p[TR_PATH];
        struct stat st;
        if (real_path(env, pv->pkg, p, sizeof p) != 0 || lstat(p, &st) != 0 ||
            nearest_dev(env, pv->aside, &daside) != 0) {
            in->state = "aside_cross_device";
            in->reason = "the drive could not be checked";
            return;
        }
        dpkg = (unsigned long long)st.st_dev;
        if (dpkg != daside) {
            in->state = "aside_cross_device";
            return;
        }
    }
    in->state = "restorable";
}

/* ---- JSON out ---- */

typedef struct {
    char *b;
    size_t cap, n;
    int over;
} tr_out_t;

static void put(tr_out_t *o, const char *fmt, ...) {
    if (o->over) return;
    va_list ap;
    va_start(ap, fmt);
    int k = vsnprintf(o->b + o->n, o->cap - o->n, fmt, ap);
    va_end(ap);
    if (k < 0 || (size_t)k >= o->cap - o->n) {
        o->over = 1;
        return;
    }
    o->n += (size_t)k;
}

static int done(tr_out_t *o) { return o->over ? -1 : (int)o->n; }

static const char *where_word(const tr_vol_t *v) {
    return install_guard_word(install_guard_where_of(v->vol));
}

static void put_info(tr_out_t *o, const char *id, const tr_vol_t *v, const tr_info_t *in) {
    put(o, "\"title_id\":\"%s\",\"state\":\"%s\"", id, in->state);
    if (in->reason) put(o, ",\"reason\":\"%s\"", in->reason);
    if (in->vol < 0) return;
    const tr_vol_t *pv = &v[in->vol];
    put(o, ",\"where\":\"%s\",\"volume\":\"%s\",\"aside\":\"%s\"", where_word(pv), pv->root, pv->aside);
    if (strcmp(in->state, "pending") == 0) return;
    put(o, ",\"pkg\":\"%s\",\"size\":%llu,\"kind\":\"%s\"", pv->pkg, (unsigned long long)in->size, in->kind);
    if (in->have_patch)
        put(o, ",\"patch\":\"%s\",\"patch_size\":%llu", pv->patch, (unsigned long long)in->patch_size);
    if (in->free_known)
        put(o, ",\"free_volume\":%llu,\"free_internal\":%llu", (unsigned long long)in->free_volume,
            (unsigned long long)in->free_internal);
}

/* ---- the record ---- */

typedef struct {
    int app;   /* app.pkg was moved aside */
    int patch; /* patch.pkg was moved aside */
    char backup[256];
} tr_record_t;

static int write_record(const title_restore_env_t *env, const tr_vol_t *v, const tr_record_t *r) {
    char rec[TR_PATH], p[TR_PATH];
    if (record_path(v, rec, sizeof rec) != 0 || real_path(env, rec, p, sizeof p) != 0) return -1;
    char tmp[TR_PATH + 8];
    snprintf(tmp, sizeof tmp, "%s.tmp", p);
    FILE *f = fopen(tmp, "w");
    if (!f) return -1;
    int ok = fprintf(f, "app=%d\npatch=%d\nbackup=%s\n", r->app, r->patch, r->backup) > 0;
    ok = fflush(f) == 0 && ok;
    fsync(fileno(f));
    ok = fclose(f) == 0 && ok;
    /* our own tmp file into its name in the same folder */
    if (!ok || rename(tmp, p) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

static int read_record(const title_restore_env_t *env, const tr_vol_t *v, tr_record_t *r) {
    char rec[TR_PATH], p[TR_PATH], line[256];
    memset(r, 0, sizeof *r);
    if (record_path(v, rec, sizeof rec) != 0 || real_path(env, rec, p, sizeof p) != 0) return -1;
    FILE *f = fopen(p, "r");
    if (!f) return -1;
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = '\0';
        if (strncmp(line, "app=", 4) == 0) r->app = atoi(line + 4) == 1;
        else if (strncmp(line, "patch=", 6) == 0) r->patch = atoi(line + 6) == 1;
        else if (strncmp(line, "backup=", 7) == 0 && !strchr(line + 7, '"') && !strchr(line + 7, '\\'))
            snprintf(r->backup, sizeof r->backup, "%s", line + 7);
    }
    fclose(f);
    return 0;
}

static void remove_record(const title_restore_env_t *env, const tr_vol_t *v) {
    char rec[TR_PATH], p[TR_PATH];
    if (record_path(v, rec, sizeof rec) == 0 && real_path(env, rec, p, sizeof p) == 0) unlink(p);
    /* The folder only when empty: rmdir never removes anything else. */
    if (real_path(env, v->aside, p, sizeof p) == 0) rmdir(p);
}

/* mkdir -p of a console path (prefix applied), 0 when it exists as a folder afterwards. */
static int mkdirs(const title_restore_env_t *env, const char *path) {
    char p[TR_PATH];
    if (real_path(env, path, p, sizeof p) != 0) return -1;
    size_t floor = strlen(pfx(env));
    for (char *s = p + floor + 1; *s; s++) {
        if (*s != '/') continue;
        *s = '\0';
        if (mkdir(p, 0777) != 0 && errno != EEXIST) {
            *s = '/';
            return -1;
        }
        *s = '/';
    }
    if (mkdir(p, 0777) != 0 && errno != EEXIST) return -1;
    struct stat st;
    return stat(p, &st) == 0 && S_ISDIR(st.st_mode) ? 0 : -1;
}

/* A rename that this kernel survives: only when both ends are proven to be on one device
 * (cross_device.h; anything but XDEV_SAME is refused), and never over an existing entry. */
static int safe_move(const title_restore_env_t *env, const char *from, const char *to) {
    char f[TR_PATH], t[TR_PATH];
    struct stat st;
    if (real_path(env, from, f, sizeof f) != 0 || real_path(env, to, t, sizeof t) != 0) return -1;
    if (lstat(t, &st) == 0) return -1;
    if (!xdev_rename_is_safe(xdev_rename_crosses_l(f, t, xdev_lstat_dev, xdev_stat_dev))) return -1;
    return rename(f, t) == 0 ? 0 : -1;
}

static void aside_file(const tr_vol_t *v, const char *name, char *buf, size_t cap) {
    snprintf(buf, cap, "%s/%s", v->aside, name);
}

/* ---- the three operations ---- */

static int op_check(const title_restore_env_t *env, const char *id, tr_out_t *o) {
    tr_vol_t v[TITLE_RESTORE_MAX_VOLUMES];
    tr_info_t in;
    int nv = tr_volumes(env, id, v, TITLE_RESTORE_MAX_VOLUMES);
    inspect(env, id, v, nv, &in);
    put(o, "{\"ok\":true,");
    put_info(o, id, v, &in);
    put(o, "}");
    return done(o);
}

static int op_prepare(const title_restore_env_t *env, const char *id, const char *stamp, tr_out_t *o) {
    tr_vol_t v[TITLE_RESTORE_MAX_VOLUMES];
    tr_info_t in;
    int nv = tr_volumes(env, id, v, TITLE_RESTORE_MAX_VOLUMES);
    inspect(env, id, v, nv, &in);
    if (strcmp(in.state, "restorable") != 0) {
        put(o, "{\"ok\":false,\"err\":\"restore_%s\",", in.state);
        put_info(o, id, v, &in);
        put(o, "}");
        return done(o);
    }
    const tr_vol_t *pv = &v[in.vol];
    char aside_pkg[TR_PATH], aside_patch[TR_PATH];
    aside_file(pv, "app.pkg", aside_pkg, sizeof aside_pkg);
    aside_file(pv, "patch.pkg", aside_patch, sizeof aside_patch);

    /* 1. The databases, copied aside before anything changes (the installer writes them next). */
    tr_record_t rec;
    memset(&rec, 0, sizeof rec);
    {
        const char *root = env && env->backup_root ? env->backup_root : TITLE_RESTORE_BACKUP_ROOT;
        char dir[200], dst[TR_PATH], err[160] = {0};
        int ok = stamp && stamp[0] && !strchr(stamp, '/') && !strstr(stamp, "..");
        /* A folder of its own: an earlier restore's backup (the same second) is never written into. */
        for (int k = 1; ok; k++) {
            int n = k == 1 ? snprintf(dir, sizeof dir, "%s/%s-%s", root, id, stamp)
                           : snprintf(dir, sizeof dir, "%s/%s-%s-%d", root, id, stamp, k);
            if (n <= 0 || n >= (int)sizeof dir || k > 50) ok = 0;
            else if (!exists(env, dir, NULL)) break;
        }
        int made = ok && mkdirs(env, dir) == 0;
        ok = ok && made;
        static const char *const dbs[] = {"app.db", "appinfo.db"};
        for (size_t i = 0; ok && i < sizeof dbs / sizeof dbs[0]; i++) {
            char con[TR_PATH];
            snprintf(con, sizeof con, "%s/%s", dir, dbs[i]);
            ok = real_path(env, con, dst, sizeof dst) == 0 && env && env->backup_db &&
                 env->backup_db(dbs[i], dst, err, sizeof err) == 0;
        }
        if (!ok) {
            /* A half backup is no backup: its files and its (our own, new) folder go. */
            for (size_t i = 0; made && i < sizeof dbs / sizeof dbs[0]; i++) {
                char con[TR_PATH];
                snprintf(con, sizeof con, "%s/%s", dir, dbs[i]);
                if (real_path(env, con, dst, sizeof dst) == 0) unlink(dst);
            }
            if (made && real_path(env, dir, dst, sizeof dst) == 0) rmdir(dst);
            fprintf(stderr, "[restore] %s: database backup failed (%s): nothing moved\n", id, err);
            put(o, "{\"ok\":false,\"err\":\"restore_backup_failed\",");
            put_info(o, id, v, &in);
            put(o, "}");
            return done(o);
        }
        snprintf(rec.backup, sizeof rec.backup, "%s", dir);
    }

    /* 2. The aside folder, empty of packages, and the record (written before any move, so an
     *    interruption is found again by the next check). */
    if (mkdirs(env, pv->aside) != 0 || exists(env, aside_pkg, NULL) || exists(env, aside_patch, NULL)) {
        put(o, "{\"ok\":false,\"err\":\"restore_aside_busy\",");
        put_info(o, id, v, &in);
        put(o, "}");
        return done(o);
    }
    rec.app = 1;
    rec.patch = in.have_patch;
    if (write_record(env, pv, &rec) != 0) {
        remove_record(env, pv);
        put(o, "{\"ok\":false,\"err\":\"restore_move_failed\",\"reason\":\"record\",");
        put_info(o, id, v, &in);
        put(o, "}");
        return done(o);
    }

    /* 3. The package aside: a same-device rename, nothing copied. */
    if (safe_move(env, pv->pkg, aside_pkg) != 0) {
        int e = errno;
        remove_record(env, pv);
        fprintf(stderr, "[restore] %s: moving %s aside failed (errno %d): nothing changed\n", id, pv->pkg, e);
        put(o, "{\"ok\":false,\"err\":\"restore_move_failed\",");
        put_info(o, id, v, &in);
        put(o, "}");
        return done(o);
    }
    /* 4. The update too, so the installer cannot clear it; when it will not move, it stays
     *    where it is and the record says so. */
    if (rec.patch && safe_move(env, pv->patch, aside_patch) != 0) {
        rec.patch = 0;
        (void)write_record(env, pv, &rec);
    }
    fprintf(stderr, "[restore] %s: %s moved aside to %s%s; databases backed up in %s\n", id, pv->pkg, aside_pkg,
            rec.patch ? " with its update" : "", rec.backup);
    put(o, "{\"ok\":true,\"title_id\":\"%s\",\"state\":\"prepared\",\"where\":\"%s\",\"volume\":\"%s\","
           "\"aside\":\"%s\",\"pkg\":\"%s\",\"install_path\":\"%s\",\"size\":%llu,\"kind\":\"%s\",\"backup\":\"%s\"",
        id, where_word(pv), pv->root, pv->aside, pv->pkg, aside_pkg, (unsigned long long)in.size, in.kind, rec.backup);
    if (rec.patch) put(o, ",\"patch\":\"%s\",\"patch_aside\":\"%s\"", pv->patch, aside_patch);
    put(o, "}");
    return done(o);
}

/* An app.pkg in any <volume>/user/app/<id>: where the installer put the game. -1 none. */
static int installed_somewhere(const title_restore_env_t *env, const tr_vol_t *v, int nv) {
    for (int i = 0; i < nv; i++) {
        struct stat st;
        if (exists(env, v[i].pkg, &st) && S_ISREG(st.st_mode)) return i;
    }
    return -1;
}

/* The update back where it was: only into a free place. 1 put back, 0 kept aside (or none). */
static int patch_back(const title_restore_env_t *env, const tr_vol_t *pv, const char *aside_patch) {
    if (!exists(env, aside_patch, NULL)) return 0;
    char dir[TR_PATH];
    xdev_parent_dir(pv->patch, dir, sizeof dir);
    if (mkdirs(env, dir) != 0) return 0;
    return safe_move(env, aside_patch, pv->patch) == 0;
}

static int op_finish(const title_restore_env_t *env, const char *id, int flags, tr_out_t *o) {
    tr_vol_t v[TITLE_RESTORE_MAX_VOLUMES];
    int nv = tr_volumes(env, id, v, TITLE_RESTORE_MAX_VOLUMES);
    int at = find_pending(env, v, nv);
    if (at < 0) {
        put(o, "{\"ok\":true,\"title_id\":\"%s\",\"state\":\"nothing_pending\"}", id);
        return done(o);
    }
    const tr_vol_t *pv = &v[at];
    tr_record_t rec;
    (void)read_record(env, pv, &rec);
    char aside_pkg[TR_PATH], aside_patch[TR_PATH];
    aside_file(pv, "app.pkg", aside_pkg, sizeof aside_pkg);
    aside_file(pv, "patch.pkg", aside_patch, sizeof aside_patch);
    int have_aside = exists(env, aside_pkg, NULL);
    int have_patch_aside = exists(env, aside_patch, NULL);
    put(o, "{\"ok\":true,\"title_id\":\"%s\",\"where\":\"%s\",\"volume\":\"%s\",\"aside\":\"%s\",\"pkg\":\"%s\"",
        id, where_word(pv), pv->root, pv->aside, pv->pkg);
    if (rec.backup[0]) put(o, ",\"backup\":\"%s\"", rec.backup);

    if (!have_aside) {
        /* Interrupted before the move (only the record was written), or already finished. */
        int back = have_patch_aside ? patch_back(env, pv, aside_patch) : 0;
        if (!have_patch_aside || back) remove_record(env, pv);
        put(o, ",\"state\":\"nothing_pending\"}");
        return done(o);
    }
    int db = env && env->appdb_has ? env->appdb_has(id) : -1;
    int landed = installed_somewhere(env, v, nv);
    if ((flags & TITLE_RESTORE_ROLLBACK) && db == 1) {
        /* Asked to put it back, but the console has a record of the title now: its files are never
         * moved by us, and the copy aside is not deleted on a put-back either. */
        put(o, ",\"state\":\"unsettled\",\"reason\":\"%s\",\"kept\":\"%s\"}",
            landed >= 0 ? "on_home_screen" : "recorded_without_package", aside_pkg);
        return done(o);
    }
    if (db == 1 && landed >= 0 && !(flags & TITLE_RESTORE_INSTALL_ENDED)) {
        /* Recorded, with a package in place, but nobody said the install ended: the installer may
         * still be reading our copy. Nothing is deleted. */
        put(o, ",\"state\":\"waiting\",\"reason\":\"install_may_be_running\",\"installed\":\"%s\",\"kept\":\"%s\"}",
            v[landed].pkg, aside_pkg);
        return done(o);
    }
    if (db == 1 && landed >= 0) {
        /* Restored: the console has its record and its own copy. Ours goes. */
        char p[TR_PATH];
        int gone = real_path(env, aside_pkg, p, sizeof p) == 0 && unlink(p) == 0;
        int back = have_patch_aside ? patch_back(env, pv, aside_patch) : 0;
        if (gone && (!have_patch_aside || back)) remove_record(env, pv);
        fprintf(stderr, "[restore] %s: restored (installed at %s)%s\n", id, v[landed].pkg,
                have_patch_aside && !back ? "; its update kept aside" : "");
        put(o, ",\"state\":\"restored\",\"installed\":\"%s\",\"installed_where\":\"%s\"", v[landed].pkg,
            where_word(&v[landed]));
        if (have_patch_aside) put(o, back ? ",\"patch\":\"%s\"" : ",\"patch_kept\":\"%s\"", back ? pv->patch : aside_patch);
        if (!gone) put(o, ",\"kept\":\"%s\"", aside_pkg);
        put(o, "}");
        return done(o);
    }
    if (db < 0) {
        put(o, ",\"state\":\"unsettled\",\"reason\":\"db_unreadable\",\"kept\":\"%s\"}", aside_pkg);
        return done(o);
    }
    if (!(flags & TITLE_RESTORE_ROLLBACK)) {
        /* Not (yet) on the home screen and not asked to put it back: an install may still be
         * writing. Nothing is touched. */
        put(o, ",\"state\":\"waiting\",\"reason\":\"%s\",\"kept\":\"%s\"}",
            db == 1 ? "recorded_without_package" : "not_on_home_screen", aside_pkg);
        return done(o);
    }
    if (db == 1) {
        /* The console has a record of the title: its files are never moved by us. */
        put(o, ",\"state\":\"unsettled\",\"reason\":\"recorded_without_package\",\"kept\":\"%s\"}", aside_pkg);
        return done(o);
    }
    /* Roll back: the package goes back only into a free place. */
    if (exists(env, pv->pkg, NULL)) {
        put(o, ",\"state\":\"unsettled\",\"reason\":\"place_taken\",\"kept\":\"%s\"}", aside_pkg);
        return done(o);
    }
    if (mkdirs(env, pv->app_dir) != 0 || safe_move(env, aside_pkg, pv->pkg) != 0) {
        put(o, ",\"state\":\"unsettled\",\"reason\":\"move_back_failed\",\"kept\":\"%s\"}", aside_pkg);
        return done(o);
    }
    int back = have_patch_aside ? patch_back(env, pv, aside_patch) : 0;
    if (!have_patch_aside || back) remove_record(env, pv);
    fprintf(stderr, "[restore] %s: not restored; %s put back\n", id, pv->pkg);
    put(o, ",\"state\":\"rolled_back\"");
    if (have_patch_aside) put(o, back ? ",\"patch\":\"%s\"" : ",\"patch_kept\":\"%s\"", back ? pv->patch : aside_patch);
    put(o, "}");
    return done(o);
}

int title_restore_run(const title_restore_env_t *env, const char *op, const char *title_id, int flags,
                      const char *stamp, char *out, size_t cap) {
    if (!out || cap < 64) return -1;
    tr_out_t o = {out, cap, 0, 0};
    out[0] = '\0';
    const char *refused = install_guard_register_id_refusal(title_id);
    if (refused) {
        const char *st = strcmp(refused, "register_system_title") == 0 ? "system_title" : "invalid_title";
        if (!op || !op[0] || strcmp(op, "check") == 0)
            put(&o, "{\"ok\":true,\"state\":\"%s\"}", st);
        else
            put(&o, "{\"ok\":false,\"err\":\"restore_%s\",\"state\":\"%s\"}", st, st);
        return done(&o);
    }
    char id[16];
    snprintf(id, sizeof id, "%s", title_id);
    for (char *c = id; *c; c++)
        if (*c >= 'a' && *c <= 'z') *c = (char)(*c - 'a' + 'A');
    if (!op || !op[0] || strcmp(op, "check") == 0) return op_check(env, id, &o);
    pthread_mutex_lock(&g_restore_lock);
    int n;
    if (strcmp(op, "prepare") == 0) n = op_prepare(env, id, stamp, &o);
    else if (strcmp(op, "finish") == 0) n = op_finish(env, id, flags, &o);
    else {
        put(&o, "{\"ok\":false,\"err\":\"bad_request\"}");
        n = done(&o);
    }
    pthread_mutex_unlock(&g_restore_lock);
    return n;
}
