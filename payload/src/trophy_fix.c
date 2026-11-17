/* ps5upload — trophy files that still name an old account id (#853). See trophy_fix.h.
 *
 * Where the layout comes from (read 2026-11-17):
 *   - kiikeep/KIK-Trophy app/core.cjs: parseDat/rewriteDat read and rewrite real PS5 TRPTITLE.DAT
 *     files. The account id is the 8 bytes at file offset 0x200 (its Base64 form, as PSN and Remote
 *     Play write it, is those bytes reversed: the file holds it big endian), the user id the 4 bytes
 *     at 0x208, and after changing them it writes the SHA-256 of the first block again at 0x440.
 *   - RafaelNGP/psbox-cloud-gaming src/trophy/trophy_builder.cpp: the same "T2PD" container (0x40
 *     header, 0x400-byte blocks each followed by its SHA-256, an all-zero block with a zero hash).
 *   - The report in #853: fixing the account id in TRPTITLE.DAT by hand brought the trophies back.
 * Nothing here is a Sony call; a game that has its trophy files open must not be running (the engine
 * checks the process list before it asks for a change). */

#include "trophy_fix.h"
#include "app_dump_sha256.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define TF_MAX_DEPTH 6
#define TF_MAX_VISITS 4096
#define TF_MAX_LISTED 128
#define TF_TMP_SUFFIX ".ps5upload-tmp"
#define TF_MANIFEST "manifest.txt"

static uint32_t be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

static uint64_t be64(const uint8_t *p) { return ((uint64_t)be32(p) << 32) | be32(p + 4); }

static void put_be64(uint8_t *p, uint64_t v) {
    for (int i = 7; i >= 0; i--) {
        p[i] = (uint8_t)v;
        v >>= 8;
    }
}

static void put_le64(uint8_t *p, uint64_t v) {
    for (int i = 0; i < 8; i++) {
        p[i] = (uint8_t)v;
        v >>= 8;
    }
}

static int all_zero(const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n; i++)
        if (p[i]) return 0;
    return 1;
}

static void block_hash(const uint8_t *chunk, uint8_t out[32]) {
    if (all_zero(chunk, T2PD_CHUNK)) {
        memset(out, 0, 32);
        return;
    }
    SHA256_CTX c;
    sha256_init(&c);
    sha256_update(&c, chunk, T2PD_CHUNK);
    sha256_final(&c, out);
}

/* The 8 bytes are worth matching anywhere: an id with a zero half (or 0) would match ordinary data. */
static int distinctive(uint64_t id) { return (id >> 32) != 0 && (uint32_t)id != 0; }

static int t2pd_header_ok(const uint8_t *buf, size_t n) {
    if (!buf || n < T2PD_HEAD + T2PD_STRIDE || n > TROPHY_FIX_FILE_MAX) return 0;
    if (memcmp(buf, "T2PD", 4) != 0 || be32(buf + 4) != 0x10000u) return 0;
    if (be64(buf + 8) != T2PD_STRIDE || (n - T2PD_HEAD) % T2PD_STRIDE != 0) return 0;
    return be64(buf + 0x20) == (n - T2PD_HEAD) / T2PD_STRIDE;
}

int trophy_t2pd_valid(const uint8_t *buf, size_t n) {
    if (!t2pd_header_ok(buf, n)) return 0;
    size_t blocks = (n - T2PD_HEAD) / T2PD_STRIDE;
    for (size_t b = 0; b < blocks; b++) {
        const uint8_t *chunk = buf + T2PD_HEAD + b * T2PD_STRIDE;
        uint8_t h[32];
        if (all_zero(chunk, T2PD_CHUNK) && all_zero(chunk + T2PD_CHUNK, 32)) continue;
        SHA256_CTX c;
        sha256_init(&c);
        sha256_update(&c, chunk, T2PD_CHUNK);
        sha256_final(&c, h);
        if (memcmp(h, chunk + T2PD_CHUNK, 32) != 0) return 0;
    }
    return 1;
}

/* Counts (and with `put`, overwrites with `to`) every non-overlapping copy of `pat` in `d`, outside
 * [skip_at, skip_at + 8). Marks the 0x400-byte blocks it touched in `touched`. */
static int replace_all(uint8_t *d, size_t n, const uint8_t pat[8], const uint8_t to[8], size_t skip_at,
                       int put, uint8_t *touched) {
    int hits = 0;
    for (size_t i = 0; i + 8 <= n;) {
        if (i + 8 > skip_at && i < skip_at + 8) {
            i++;
            continue;
        }
        if (memcmp(d + i, pat, 8) == 0) {
            hits++;
            if (put) {
                memcpy(d + i, to, 8);
                touched[i / T2PD_CHUNK] = 1;
                touched[(i + 7) / T2PD_CHUNK] = 1;
            }
            i += 8;
        } else {
            i++;
        }
    }
    return hits;
}

static int count_raw(const uint8_t *buf, size_t n, uint64_t id) {
    uint8_t be[8], le[8];
    put_be64(be, id);
    put_le64(le, id);
    int hits = 0;
    for (size_t i = 0; i + 8 <= n; i++) {
        if (memcmp(buf + i, be, 8) == 0 || memcmp(buf + i, le, 8) == 0) {
            hits++;
            i += 7;
        }
    }
    return hits;
}

int trophy_t2pd_fix(uint8_t *buf, size_t n, uint32_t uid, uint64_t old_id, int have_old,
                    uint64_t new_id, int apply, trophy_t2pd_result_t *r) {
    memset(r, 0, sizeof(*r));
    int extras_ok = have_old && distinctive(old_id) && old_id != new_id;
    if (!trophy_t2pd_valid(buf, n)) {
        if (n >= 4 && memcmp(buf, "T2PD", 4) == 0) return r->state = TF_BAD_HASH;
        if (extras_ok && (r->extra = count_raw(buf, n, old_id)) > 0) return r->state = TF_UNKNOWN;
        return r->state = TF_NONE;
    }
    r->from = be64(buf + T2PD_ACCOUNT_AT);
    if (be32(buf + T2PD_USER_AT) != uid) return r->state = TF_OTHER_USER;

    size_t blocks = (n - T2PD_HEAD) / T2PD_STRIDE;
    size_t dn = blocks * T2PD_CHUNK;
    uint8_t *d = malloc(dn);
    uint8_t *touched = calloc(blocks, 1);
    if (!d || !touched) {
        free(d);
        free(touched);
        return r->state = TF_BAD_HASH;
    }
    for (size_t b = 0; b < blocks; b++)
        memcpy(d + b * T2PD_CHUNK, buf + T2PD_HEAD + b * T2PD_STRIDE, T2PD_CHUNK);
    size_t anchor = T2PD_ACCOUNT_AT - T2PD_HEAD; /* in the data, block 0 */

    if (r->from != new_id && (!have_old || r->from == old_id)) {
        r->anchor = 1;
        if (apply) {
            put_be64(d + anchor, new_id);
            touched[0] = 1;
        }
    }
    if (extras_ok) {
        uint8_t pat[8], to[8];
        put_be64(pat, old_id);
        put_be64(to, new_id);
        r->extra += replace_all(d, dn, pat, to, anchor, apply, touched);
        uint8_t lpat[8], lto[8];
        put_le64(lpat, old_id);
        put_le64(lto, new_id);
        if (memcmp(lpat, pat, 8) != 0) r->extra += replace_all(d, dn, lpat, lto, anchor, apply, touched);
    }
    if (apply) {
        for (size_t b = 0; b < blocks; b++) {
            if (!touched[b]) continue;
            uint8_t *chunk = buf + T2PD_HEAD + b * T2PD_STRIDE;
            memcpy(chunk, d + b * T2PD_CHUNK, T2PD_CHUNK);
            block_hash(chunk, chunk + T2PD_CHUNK);
            r->blocks++;
        }
    }
    free(d);
    free(touched);
    if (r->anchor || r->extra) return r->state = TF_CHANGE;
    return r->state = (r->from == new_id) ? TF_ALREADY : TF_OTHER_ID;
}

/* ── JSON out ─────────────────────────────────────────────────────────── */

typedef struct {
    char *p;
    size_t cap, len;
    int full;
} tf_out_t;

static void outf(tf_out_t *o, const char *fmt, ...) {
    if (o->full) return;
    va_list ap;
    va_start(ap, fmt);
    int k = vsnprintf(o->p + o->len, o->cap - o->len, fmt, ap);
    va_end(ap);
    if (k < 0 || (size_t)k >= o->cap - o->len) {
        o->full = 1;
        return;
    }
    o->len += (size_t)k;
}

static void out_str(tf_out_t *o, const char *s) {
    outf(o, "\"");
    for (; *s && !o->full; s++) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\')
            outf(o, "\\%c", c);
        else if (c < 0x20)
            outf(o, "\\u%04x", c);
        else
            outf(o, "%c", c);
    }
    outf(o, "\"");
}

static const char *state_name(int s, int dry) {
    switch (s) {
    case TF_CHANGE: return dry ? "would_change" : "changed";
    case TF_ALREADY: return "already";
    case TF_OTHER_ID: return "other_id";
    case TF_OTHER_USER: return "other_user";
    case TF_BAD_HASH: return "bad_hash";
    case TF_UNKNOWN: return "unknown_format";
    default: return "none";
    }
}

/* ── files ────────────────────────────────────────────────────────────── */

static int mkdirs(const char *path) {
    char tmp[512];
    if (snprintf(tmp, sizeof tmp, "%s", path) >= (int)sizeof tmp) return -1;
    for (char *p = tmp + 1; *p; p++) {
        if (*p != '/') continue;
        *p = '\0';
        if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
        *p = '/';
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
    struct stat st;
    return (lstat(tmp, &st) == 0 && S_ISDIR(st.st_mode)) ? 0 : -1;
}

/* The whole regular file (never through a link), at most TROPHY_FIX_FILE_MAX bytes. */
static uint8_t *read_all(const char *path, size_t *n, struct stat *st) {
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return NULL;
    if (fstat(fd, st) != 0 || !S_ISREG(st->st_mode) || st->st_size < 0 ||
        (uint64_t)st->st_size > TROPHY_FIX_FILE_MAX) {
        close(fd);
        return NULL;
    }
    size_t want = (size_t)st->st_size;
    uint8_t *buf = malloc(want ? want : 1);
    size_t got = 0;
    while (buf && got < want) {
        ssize_t k = read(fd, buf + got, want - got);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) break;
        got += (size_t)k;
    }
    close(fd);
    if (!buf || got != want) {
        free(buf);
        return NULL;
    }
    *n = want;
    return buf;
}

static int write_all(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t k = write(fd, p, n);
        if (k < 0 && errno == EINTR) continue;
        if (k <= 0) return -1;
        p += k;
        n -= (size_t)k;
    }
    return 0;
}

/* A new file (never over an existing one, never through a link), on disk when this returns 0. */
static int write_new(const char *path, const uint8_t *p, size_t n) {
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0644);
    if (fd < 0) return -1;
    int rc = write_all(fd, p, n);
    if (rc == 0 && fsync(fd) != 0) rc = -1;
    if (close(fd) != 0) rc = -1;
    if (rc != 0) unlink(path);
    return rc;
}

/* Replace `path` with these bytes: a temp beside it (the same folder, so never another filesystem),
 * with the mode and owner the file had, on disk before the rename. */
static int put_file(const char *path, const uint8_t *p, size_t n, mode_t mode, uid_t owner, gid_t group) {
    char tmp[600];
    if (snprintf(tmp, sizeof tmp, "%s" TF_TMP_SUFFIX, path) >= (int)sizeof tmp) return -1;
    (void)unlink(tmp); /* a leftover of ours; a link there is removed as a name, never followed */
    int fd = open(tmp, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (fd < 0) return -1;
    int rc = write_all(fd, p, n);
    if (rc == 0 && fchown(fd, owner, group) != 0) rc = -1;
    if (rc == 0 && fchmod(fd, mode & 07777) != 0) rc = -1;
    if (rc == 0 && fsync(fd) != 0) rc = -1;
    if (close(fd) != 0) rc = -1;
    if (rc == 0 && rename(tmp, path) != 0) rc = -1;
    if (rc != 0) unlink(tmp);
    return rc;
}

/* ── the walk ─────────────────────────────────────────────────────────── */

typedef struct {
    const char *home;       /* <home_root>/<uid> */
    const char *backup_dir; /* <backup_root>/<uid> */
    const char *stamp;
    char backup[64];        /* the backup folder's name, once made */
    uint32_t uid;
    uint64_t old_id, new_id;
    int have_old, dry;
    dev_t dev;
    int visits, scanned, changed, already, left, failed, listed, more;
    tf_out_t *o;
} tf_walk_t;

static int ensure_backup(tf_walk_t *w) {
    if (w->backup[0]) return 0;
    if (mkdirs(w->backup_dir) != 0) return -1;
    for (int i = 1; i <= 50; i++) {
        char name[64], dir[600];
        if (i == 1)
            snprintf(name, sizeof name, "%s", w->stamp);
        else
            snprintf(name, sizeof name, "%s-%d", w->stamp, i);
        snprintf(dir, sizeof dir, "%s/%s", w->backup_dir, name);
        if (mkdir(dir, 0777) == 0) {
            char m[640], head[160];
            snprintf(m, sizeof m, "%s/" TF_MANIFEST, dir);
            int hn = snprintf(head, sizeof head, "ps5upload-trophy-backup 1\nuid %08x\nold 0x%016llx %d\nnew 0x%016llx\n",
                              w->uid, (unsigned long long)w->old_id, w->have_old,
                              (unsigned long long)w->new_id);
            if (write_new(m, (const uint8_t *)head, (size_t)hn) != 0) return -1;
            snprintf(w->backup, sizeof w->backup, "%s", name);
            return 0;
        }
        if (errno != EEXIST) return -1;
    }
    return -1;
}

/* Keeps the file's bytes as they were: <backup>/<rel>, then a line in the manifest. */
static int backup_file(tf_walk_t *w, const char *rel, const uint8_t *p, size_t n) {
    if (ensure_backup(w) != 0) return -1;
    char dst[1200], dir[1200], m[700];
    snprintf(dst, sizeof dst, "%s/%s/%s", w->backup_dir, w->backup, rel);
    snprintf(dir, sizeof dir, "%s", dst);
    char *slash = strrchr(dir, '/');
    if (!slash) return -1;
    *slash = '\0';
    if (mkdirs(dir) != 0 || write_new(dst, p, n) != 0) return -1;
    snprintf(m, sizeof m, "%s/%s/" TF_MANIFEST, w->backup_dir, w->backup);
    int fd = open(m, O_WRONLY | O_APPEND | O_NOFOLLOW);
    if (fd < 0) return -1;
    char line[600];
    int ln = snprintf(line, sizeof line, "f %s\n", rel);
    int rc = (ln > 0 && (size_t)ln < sizeof line) ? write_all(fd, (const uint8_t *)line, (size_t)ln) : -1;
    if (rc == 0 && fsync(fd) != 0) rc = -1;
    close(fd);
    return rc;
}

static void list_file(tf_walk_t *w, const char *rel, const char *state, const trophy_t2pd_result_t *r) {
    if (w->listed >= TF_MAX_LISTED) {
        w->more = 1;
        return;
    }
    outf(w->o, "%s{\"path\":", w->listed ? "," : "");
    out_str(w->o, rel);
    outf(w->o, ",\"state\":\"%s\",\"from\":\"0x%016llx\",\"anchor\":%s,\"extra\":%d}", state,
         (unsigned long long)r->from, r->anchor ? "true" : "false", r->extra);
    w->listed++;
}

static void do_file(tf_walk_t *w, const char *path, const char *rel) {
    size_t n = 0;
    struct stat st;
    uint8_t *buf = read_all(path, &n, &st);
    if (!buf) return;
    w->scanned++;
    trophy_t2pd_result_t r;
    int s = trophy_t2pd_fix(buf, n, w->uid, w->old_id, w->have_old, w->new_id, 0, &r);
    if (s == TF_NONE) {
        free(buf);
        return;
    }
    if (s == TF_ALREADY) w->already++;
    if (s != TF_CHANGE) {
        if (s != TF_ALREADY) w->left++;
        list_file(w, rel, state_name(s, w->dry), &r);
        free(buf);
        return;
    }
    if (w->dry) {
        w->changed++;
        list_file(w, rel, state_name(s, 1), &r);
        free(buf);
        return;
    }
    const char *why = NULL;
    uint8_t *orig = malloc(n);
    if (!orig) {
        why = "failed";
    } else {
        memcpy(orig, buf, n);
        trophy_t2pd_fix(buf, n, w->uid, w->old_id, w->have_old, w->new_id, 1, &r);
        struct stat now;
        if (!trophy_t2pd_valid(buf, n))
            why = "failed";
        else if (lstat(path, &now) != 0 || now.st_ino != st.st_ino || now.st_size != st.st_size ||
                 now.st_mtime != st.st_mtime)
            why = "changed_meanwhile";
        else if (backup_file(w, rel, orig, n) != 0)
            why = "backup_failed";
        else if (put_file(path, buf, n, st.st_mode, st.st_uid, st.st_gid) != 0)
            why = "write_failed";
    }
    free(orig);
    free(buf);
    if (why) {
        w->failed++;
        list_file(w, rel, why, &r);
    } else {
        w->changed++;
        list_file(w, rel, state_name(TF_CHANGE, 0), &r);
    }
}

static void walk(tf_walk_t *w, const char *dir, const char *rel, int depth) {
    if (depth > TF_MAX_DEPTH) return;
    DIR *d = opendir(dir);
    if (!d) return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && w->visits < TF_MAX_VISITS) {
        const char *nm = e->d_name;
        if (!strcmp(nm, ".") || !strcmp(nm, "..")) continue;
        size_t nl = strlen(nm);
        if (nl >= sizeof(TF_TMP_SUFFIX) - 1 && !strcmp(nm + nl - (sizeof(TF_TMP_SUFFIX) - 1), TF_TMP_SUFFIX))
            continue;
        char path[600], sub[520];
        if (snprintf(path, sizeof path, "%s/%s", dir, nm) >= (int)sizeof path) continue;
        if (snprintf(sub, sizeof sub, "%s/%s", rel, nm) >= (int)sizeof sub) continue;
        struct stat st;
        if (lstat(path, &st) != 0 || st.st_dev != w->dev) continue; /* links are S_ISLNK: neither below */
        w->visits++;
        if (S_ISDIR(st.st_mode))
            walk(w, path, sub, depth + 1);
        else if (S_ISREG(st.st_mode) && (uint64_t)st.st_size <= TROPHY_FIX_FILE_MAX)
            do_file(w, path, sub);
    }
    closedir(d);
}

static const char *home_root(const trophy_fix_roots_t *r) {
    return (r && r->home_root) ? r->home_root : TROPHY_FIX_HOME_ROOT;
}

static const char *backup_root(const trophy_fix_roots_t *r) {
    return (r && r->backup_root) ? r->backup_root : TROPHY_FIX_BACKUP_ROOT;
}

static int fail_json(tf_out_t *o, const char *err) {
    o->len = 0;
    o->full = 0;
    outf(o, "{\"ok\":false,\"err\":\"%s\"}", err);
    return o->full ? -1 : (int)o->len;
}

int trophy_fix_run(const trophy_fix_roots_t *roots, uint32_t uid, uint64_t old_id, int have_old,
                   uint64_t new_id, int dry_run, const char *stamp, char *out, size_t cap) {
    tf_out_t o = {out, cap, 0, 0};
    if (!out || cap < 64) return -1;
    if (uid == 0 || new_id == 0) return fail_json(&o, "bad_request");
    if (!stamp || !*stamp || strspn(stamp, "0123456789-") != strlen(stamp) || strlen(stamp) > 32)
        return fail_json(&o, "bad_request");
    char home[300], bdir[300];
    snprintf(home, sizeof home, "%s/%08x", home_root(roots), uid);
    snprintf(bdir, sizeof bdir, "%s/%08x", backup_root(roots), uid);
    struct stat hs;
    if (lstat(home, &hs) != 0 || !S_ISDIR(hs.st_mode)) return fail_json(&o, "no_such_user");

    tf_walk_t w;
    memset(&w, 0, sizeof w);
    w.home = home;
    w.backup_dir = bdir;
    w.stamp = stamp;
    w.uid = uid;
    w.old_id = have_old ? old_id : 0;
    w.have_old = have_old;
    w.new_id = new_id;
    w.dry = dry_run;
    w.dev = hs.st_dev;
    w.o = &o;
    outf(&o, "{\"ok\":true,\"uid\":%u,\"dry_run\":%s,\"old\":", uid, dry_run ? "true" : "false");
    if (have_old)
        outf(&o, "\"0x%016llx\"", (unsigned long long)old_id);
    else
        outf(&o, "null");
    outf(&o, ",\"new\":\"0x%016llx\",\"files\":[", (unsigned long long)new_id);
    /* PS5 games' trophies (trophy2) and PS4 games' (trophy): only the user's own folders. */
    static const char *const tops[] = {"trophy2", "trophy"};
    for (size_t i = 0; i < sizeof tops / sizeof tops[0]; i++) {
        char top[400];
        snprintf(top, sizeof top, "%s/%s", home, tops[i]);
        struct stat ts;
        if (lstat(top, &ts) != 0 || !S_ISDIR(ts.st_mode) || ts.st_dev != w.dev) continue;
        walk(&w, top, tops[i], 1);
    }
    outf(&o, "],\"more\":%s,\"backup\":", w.more ? "true" : "false");
    out_str(&o, w.backup);
    outf(&o, ",\"scanned\":%d,\"changed\":%d,\"already\":%d,\"left\":%d,\"failed\":%d}", w.scanned,
         w.changed, w.already, w.left, w.failed);
    if (o.full) return fail_json(&o, "reply_too_large");
    return (int)o.len;
}

static int good_name(const char *name) {
    size_t n = name ? strlen(name) : 0;
    return n > 0 && n <= 40 && strspn(name, "0123456789-") == n;
}

int trophy_fix_backups_json(const trophy_fix_roots_t *roots, uint32_t uid, char *out, size_t cap) {
    tf_out_t o = {out, cap, 0, 0};
    if (!out || cap < 64) return -1;
    char bdir[300];
    snprintf(bdir, sizeof bdir, "%s/%08x", backup_root(roots), uid);
    outf(&o, "{\"ok\":true,\"uid\":%u,\"backups\":[", uid);
    /* Names are timestamps: sort them newest first. */
    char names[64][48];
    int count = 0;
    DIR *d = opendir(bdir);
    struct dirent *e;
    while (d && (e = readdir(d)) != NULL && count < 64) {
        if (good_name(e->d_name)) snprintf(names[count++], sizeof names[0], "%s", e->d_name);
    }
    if (d) closedir(d);
    for (int i = 1; i < count; i++)
        for (int j = i; j > 0 && strcmp(names[j - 1], names[j]) < 0; j--) {
            char t[48];
            memcpy(t, names[j], sizeof t);
            memcpy(names[j], names[j - 1], sizeof t);
            memcpy(names[j - 1], t, sizeof t);
        }
    int first = 1;
    for (int i = 0; i < count; i++) {
        char m[400];
        snprintf(m, sizeof m, "%s/%s/" TF_MANIFEST, bdir, names[i]);
        FILE *f = fopen(m, "r");
        if (!f) continue;
        char line[600];
        int files = 0;
        while (fgets(line, sizeof line, f))
            if (line[0] == 'f' && line[1] == ' ') files++;
        fclose(f);
        outf(&o, "%s{\"name\":\"%s\",\"files\":%d}", first ? "" : ",", names[i], files);
        first = 0;
    }
    outf(&o, "]}");
    if (o.full) return fail_json(&o, "reply_too_large");
    return (int)o.len;
}

/* A manifest path: below trophy2/ or trophy/, no "..", no empty part. */
static int good_rel(const char *rel) {
    if (strncmp(rel, "trophy2/", 8) != 0 && strncmp(rel, "trophy/", 7) != 0) return 0;
    if (strstr(rel, "//") || strstr(rel, "/../") || strstr(rel, "/./")) return 0;
    size_t n = strlen(rel);
    if (n < 3 || rel[n - 1] == '/' || (n >= 3 && !strcmp(rel + n - 3, "/.."))) return 0;
    return 1;
}

int trophy_fix_restore(const trophy_fix_roots_t *roots, uint32_t uid, const char *name, char *out,
                       size_t cap) {
    tf_out_t o = {out, cap, 0, 0};
    if (!out || cap < 64) return -1;
    if (uid == 0 || !good_name(name)) return fail_json(&o, "bad_request");
    char bdir[400], home[300], m[460];
    snprintf(bdir, sizeof bdir, "%s/%08x/%s", backup_root(roots), uid, name);
    snprintf(home, sizeof home, "%s/%08x", home_root(roots), uid);
    snprintf(m, sizeof m, "%s/" TF_MANIFEST, bdir);
    FILE *f = fopen(m, "r");
    if (!f) return fail_json(&o, "no_such_backup");
    char line[600];
    int restored = 0, failed = 0, header = 0;
    while (fgets(line, sizeof line, f)) {
        size_t ln = strlen(line);
        if (ln == 0 || line[ln - 1] != '\n') {
            failed++;
            break;
        }
        line[ln - 1] = '\0';
        if (!strcmp(line, "ps5upload-trophy-backup 1")) {
            header = 1;
            continue;
        }
        if (!header || line[0] != 'f' || line[1] != ' ') continue;
        const char *rel = line + 2;
        if (!good_rel(rel)) {
            failed++;
            continue;
        }
        char src[1100], dst[1100], parent[1100];
        snprintf(src, sizeof src, "%s/%s", bdir, rel);
        snprintf(dst, sizeof dst, "%s/%s", home, rel);
        snprintf(parent, sizeof parent, "%s", dst);
        char *slash = strrchr(parent, '/');
        if (slash) *slash = '\0';
        size_t n = 0;
        struct stat bs, ps, ts;
        uint8_t *buf = read_all(src, &n, &bs);
        mode_t mode = 0644;
        uid_t owner = 0;
        gid_t group = 0;
        int ok = buf && lstat(parent, &ps) == 0 && S_ISDIR(ps.st_mode);
        if (ok) {
            owner = ps.st_uid;
            group = ps.st_gid;
            if (lstat(dst, &ts) == 0) {
                if (!S_ISREG(ts.st_mode))
                    ok = 0; /* a link or a folder where the file was: left alone */
                else {
                    mode = ts.st_mode;
                    owner = ts.st_uid;
                    group = ts.st_gid;
                }
            }
        }
        if (ok && put_file(dst, buf, n, mode, owner, group) == 0)
            restored++;
        else
            failed++;
        free(buf);
    }
    fclose(f);
    if (!header) return fail_json(&o, "no_such_backup");
    outf(&o, "{\"ok\":%s,\"uid\":%u,\"backup\":\"%s\",\"restored\":%d,\"failed\":%d}",
         failed == 0 ? "true" : "false", uid, name, restored, failed);
    return o.full ? -1 : (int)o.len;
}
