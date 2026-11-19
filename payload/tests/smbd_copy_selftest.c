/* Host self-test: the SMB daemon's copy state machine (payload/smb/smbd_copy.c) between an
 * in-memory "share" that can drop its connection on cue and the real local filesystem
 * (smbd_localfs.c) under a temp folder. */
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "smbd_copy.h"
#include "smbd_fs.h"
#include "smbd_path.h"

static int g_fail;
#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            g_fail++;                                                    \
        }                                                                \
    } while (0)

/* ---- the in-memory share ---- */

#define MAXN 256
typedef struct {
    int used;
    char path[SMBP_PATH_MAX];
    xkind_t kind;
    uint8_t *data;
    uint64_t size;
} node_t;

typedef struct {
    node_t n[MAXN];
    int connected;
    int reads;             /* pread calls so far */
    int writes;
    int drop_at_read;      /* the read with this number (1-based) drops the connection; 0 = never */
    int drop_at_write;
    int reconnect_fails;   /* this many reconnects fail first (-1: all) */
    int cancel_at_read;    /* sets *cancel on that read */
    volatile int *cancel;
    uint64_t free_bytes;   /* free_space answer; 0 = unknown */
    size_t chunk;
    int open_handles;
} mem_t;

typedef struct {
    int idx;
    int gen; /* the connection it was opened on */
} mfh_t;

static int g_gen = 1;

static node_t *find(mem_t *m, const char *p) {
    int i;
    for (i = 0; i < MAXN; i++)
        if (m->n[i].used && strcmp(m->n[i].path, p) == 0) return &m->n[i];
    return NULL;
}

static node_t *add(mem_t *m, const char *p, xkind_t k, const char *data, size_t size) {
    int i;
    for (i = 0; i < MAXN; i++) {
        if (!m->n[i].used) {
            node_t *x = &m->n[i];
            memset(x, 0, sizeof *x);
            x->used = 1;
            snprintf(x->path, sizeof x->path, "%s", p);
            x->kind = k;
            if (size) {
                x->data = malloc(size);
                memcpy(x->data, data, size);
            }
            x->size = size;
            return x;
        }
    }
    return NULL;
}

static void del(node_t *x) {
    free(x->data);
    memset(x, 0, sizeof *x);
}

#define M(fs) ((mem_t *)(fs)->impl)

static int parent_ok(mem_t *m, const char *p) {
    char par[SMBP_PATH_MAX];
    node_t *x;
    smbp_parent(p, par, sizeof par);
    if (!par[0]) return 1;
    x = find(m, par);
    return x && x->kind == XK_DIR;
}

static int m_stat(xfs_t *fs, const char *p, xstat_t *st) {
    node_t *x;
    if (!M(fs)->connected) return XE_DISCONNECTED;
    if (!p[0]) {
        st->kind = XK_DIR;
        st->size = 0;
        return 0;
    }
    x = find(M(fs), p);
    if (!x) return XE_NOT_FOUND;
    st->kind = x->kind;
    st->size = x->size;
    st->mtime = 0;
    return 0;
}

typedef struct {
    char dir[SMBP_PATH_MAX];
    int at;
} mdir_t;

static int m_opendir(xfs_t *fs, const char *p, void **d) {
    mdir_t *md;
    xstat_t st;
    int rc = m_stat(fs, p, &st);
    if (rc) return rc;
    if (st.kind != XK_DIR) return XE_NOT_DIR;
    md = calloc(1, sizeof *md);
    snprintf(md->dir, sizeof md->dir, "%s", p);
    *d = md;
    return 0;
}

static int m_readdir(xfs_t *fs, void *d, char *name, size_t cap, xstat_t *st) {
    mdir_t *md = d;
    mem_t *m = M(fs);
    if (!m->connected) return XE_DISCONNECTED;
    for (; md->at < MAXN; md->at++) {
        node_t *x = &m->n[md->at];
        char par[SMBP_PATH_MAX];
        if (!x->used) continue;
        smbp_parent(x->path, par, sizeof par);
        if (strcmp(par, md->dir) != 0) continue;
        if (strlen(smbp_base(x->path)) >= cap) { /* like smbd_smbfs.c: an error for this entry */
            md->at++;
            return XE_BAD_NAME;
        }
        snprintf(name, cap, "%s", smbp_base(x->path));
        st->kind = x->kind;
        st->size = x->size;
        md->at++;
        return 1;
    }
    return 0;
}

static void m_closedir(xfs_t *fs, void *d) {
    (void)fs;
    free(d);
}

static int m_mkdir(xfs_t *fs, const char *p) {
    mem_t *m = M(fs);
    if (!m->connected) return XE_DISCONNECTED;
    if (find(m, p)) return XE_EXISTS;
    if (!parent_ok(m, p)) return XE_NOT_FOUND;
    add(m, p, XK_DIR, NULL, 0);
    return 0;
}

static int m_open(xfs_t *fs, const char *p, int write, int trunc, void **fh) {
    mem_t *m = M(fs);
    node_t *x;
    mfh_t *h;
    if (!m->connected) return XE_DISCONNECTED;
    x = find(m, p);
    if (!x) {
        if (!write) return XE_NOT_FOUND;
        if (!parent_ok(m, p)) return XE_NOT_FOUND;
        x = add(m, p, XK_FILE, NULL, 0);
    } else if (x->kind == XK_DIR) {
        return XE_IS_DIR;
    } else if (trunc) {
        free(x->data);
        x->data = NULL;
        x->size = 0;
    }
    h = calloc(1, sizeof *h);
    h->idx = (int)(x - m->n);
    h->gen = g_gen;
    m->open_handles++;
    *fh = h;
    return 0;
}

static int m_open_read(xfs_t *fs, const char *p, void **fh) { return m_open(fs, p, 0, 0, fh); }
static int m_open_write(xfs_t *fs, const char *p, int resume, void **fh) { return m_open(fs, p, 1, !resume, fh); }

static long long m_pread(xfs_t *fs, void *fh, void *buf, size_t n, uint64_t off) {
    mem_t *m = M(fs);
    mfh_t *h = fh;
    node_t *x;
    m->reads++;
    if (m->cancel_at_read && m->reads == m->cancel_at_read && m->cancel) *m->cancel = 1;
    if (m->drop_at_read && m->reads == m->drop_at_read) m->connected = 0;
    if (!m->connected || h->gen != g_gen) return XE_DISCONNECTED;
    x = &m->n[h->idx];
    if (off >= x->size) return 0;
    if (n > x->size - off) n = (size_t)(x->size - off);
    memcpy(buf, x->data + off, n);
    return (long long)n;
}

static long long m_pwrite(xfs_t *fs, void *fh, const void *buf, size_t n, uint64_t off) {
    mem_t *m = M(fs);
    mfh_t *h = fh;
    node_t *x;
    m->writes++;
    if (m->drop_at_write && m->writes == m->drop_at_write) m->connected = 0;
    if (!m->connected || h->gen != g_gen) return XE_DISCONNECTED;
    x = &m->n[h->idx];
    if (off + n > x->size) {
        x->data = realloc(x->data, (size_t)(off + n));
        if (off > x->size) memset(x->data + x->size, 0, (size_t)(off - x->size));
        x->size = off + n;
    }
    /* a short write once in a while, to exercise the loop */
    if (n > 3 && (m->writes % 5) == 0) n = n / 2;
    memcpy(x->data + off, buf, n);
    return (long long)n;
}

static int m_close(xfs_t *fs, void *fh) {
    M(fs)->open_handles--;
    free(fh);
    return 0;
}

static int m_rename(xfs_t *fs, const char *from, const char *to, int replace) {
    mem_t *m = M(fs);
    node_t *a, *b;
    if (!m->connected) return XE_DISCONNECTED;
    a = find(m, from);
    if (!a) return XE_NOT_FOUND;
    b = find(m, to);
    if (b) {
        if (!replace) return XE_EXISTS;
        del(b);
    }
    snprintf(a->path, sizeof a->path, "%s", to);
    return 0;
}

static int m_unlink(xfs_t *fs, const char *p) {
    mem_t *m = M(fs);
    node_t *x;
    if (!m->connected) return XE_DISCONNECTED;
    x = find(m, p);
    if (!x) return XE_NOT_FOUND;
    if (x->kind == XK_DIR) return XE_IS_DIR;
    del(x);
    return 0;
}

static int m_rmdir(xfs_t *fs, const char *p) {
    mem_t *m = M(fs);
    node_t *x;
    int i;
    if (!m->connected) return XE_DISCONNECTED;
    x = find(m, p);
    if (!x) return XE_NOT_FOUND;
    for (i = 0; i < MAXN; i++) {
        char par[SMBP_PATH_MAX];
        if (!m->n[i].used) continue;
        smbp_parent(m->n[i].path, par, sizeof par);
        if (strcmp(par, p) == 0) return XE_EXISTS;
    }
    del(x);
    return 0;
}

static int m_free(xfs_t *fs, const char *p, uint64_t *out) {
    (void)p;
    if (!M(fs)->free_bytes) return XE_IO;
    *out = M(fs)->free_bytes;
    return 0;
}

static int m_reconnect(xfs_t *fs) {
    mem_t *m = M(fs);
    if (m->reconnect_fails < 0) return XE_UNREACHABLE;
    if (m->reconnect_fails > 0) {
        m->reconnect_fails--;
        return XE_UNREACHABLE;
    }
    m->connected = 1;
    g_gen++; /* every handle of the old connection is now dead */
    return 0;
}

static size_t m_chunk(xfs_t *fs) { return M(fs)->chunk; }

static const xfs_ops_t k_mem = {
    m_stat, m_opendir, m_readdir, m_closedir, m_mkdir, m_open_read, m_open_write, m_pread, m_pwrite, m_close,
    m_rename, m_unlink, m_rmdir, m_free, m_reconnect, m_chunk,
};

static void mem_init(xfs_t *fs, mem_t *m) {
    memset(m, 0, sizeof *m);
    m->connected = 1;
    m->chunk = 7; /* tiny chunks: many reads per file */
    fs->ops = &k_mem;
    fs->impl = m;
}

static void mem_free(mem_t *m) {
    int i;
    for (i = 0; i < MAXN; i++)
        if (m->n[i].used) del(&m->n[i]);
}

/* ---- helpers ---- */

static char g_tmp[512];

static void no_sleep(unsigned s) { (void)s; }

static void rm_rf(const char *p) {
    struct stat st;
    if (lstat(p, &st) != 0) return;
    if (S_ISDIR(st.st_mode)) {
        DIR *d = opendir(p);
        struct dirent *e;
        while (d && (e = readdir(d))) {
            char c[2048];
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            snprintf(c, sizeof c, "%s/%s", p, e->d_name);
            rm_rf(c);
        }
        if (d) closedir(d);
        rmdir(p);
    } else {
        unlink(p);
    }
}

static int read_file(const char *p, char *buf, size_t cap) {
    FILE *f = fopen(p, "rb");
    size_t n;
    if (!f) return -1;
    n = fread(buf, 1, cap - 1, f);
    fclose(f);
    buf[n] = '\0';
    return (int)n;
}

static void write_file(const char *p, const char *s) {
    FILE *f = fopen(p, "wb");
    fputs(s, f);
    fclose(f);
}

static int exists(const char *p) {
    struct stat st;
    return lstat(p, &st) == 0;
}

/* Any "*.ps5u-part" left under p. */
static int parts_under(const char *p) {
    struct stat st;
    int n = 0;
    if (lstat(p, &st) != 0) return 0;
    if (S_ISDIR(st.st_mode)) {
        DIR *d = opendir(p);
        struct dirent *e;
        while (d && (e = readdir(d))) {
            char c[2048];
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            snprintf(c, sizeof c, "%s/%s", p, e->d_name);
            n += parts_under(c);
        }
        if (d) closedir(d);
        return n;
    }
    return strstr(p, SMBP_PART_SUFFIX) != NULL;
}

static int mem_parts(mem_t *m) {
    int i, n = 0;
    for (i = 0; i < MAXN; i++)
        if (m->n[i].used && strstr(m->n[i].path, SMBP_PART_SUFFIX)) n++;
    return n;
}

static void lp(char *out, const char *rel) { snprintf(out, 1024, "%s/%s", g_tmp, rel); }

static const char k_big[] = "The quick brown fox jumps over the lazy dog, 0123456789 and then some more bytes.";

static void share_tree(mem_t *m) {
    add(m, "Games", XK_DIR, NULL, 0);
    add(m, "Games/中文 文件夹", XK_DIR, NULL, 0);
    add(m, "Games/中文 文件夹/存档.bin", XK_FILE, k_big, sizeof k_big - 1);
    add(m, "Games/中文 文件夹/empty.txt", XK_FILE, "", 0);
    add(m, "Games/top.txt", XK_FILE, "top", 3);
    add(m, "Games/._top.txt", XK_FILE, "apple", 5);
    add(m, "Games/.DS_Store", XK_FILE, "ds", 2);
    add(m, "Games/link", XK_OTHER, NULL, 0);
    add(m, "one.bin", XK_FILE, k_big, sizeof k_big - 1);
}

/* ---- tests ---- */

static void t_download_file(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char d[1024], buf[256], failed[256];
    int rc;
    mem_init(&src, &m);
    share_tree(&m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    lp(d, "one.bin");
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0);
    CHECK(read_file(d, buf, sizeof buf) == (int)sizeof k_big - 1 && strcmp(buf, k_big) == 0);
    CHECK(pr.bytes_done == sizeof k_big - 1 && pr.bytes_total == sizeof k_big - 1);
    CHECK(pr.files_done == 1 && pr.files_total == 1);
    CHECK(parts_under(g_tmp) == 0);
    CHECK(m.open_handles == 0);
    /* again without overwrite: refused, file untouched */
    write_file(d, "mine");
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_EXISTS);
    CHECK(read_file(d, buf, sizeof buf) == 4 && strcmp(buf, "mine") == 0);
    /* with overwrite: replaced */
    o.overwrite = 1;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0 && read_file(d, buf, sizeof buf) > 0 && strcmp(buf, k_big) == 0);
    CHECK(parts_under(g_tmp) == 0);
    /* a missing source names itself */
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "nope.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_NOT_FOUND && strcmp(failed, "nope.bin") == 0);
    unlink(d);
    mem_free(&m);
}

static void t_download_tree(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char d[1024], f[1024], buf[256], failed[256];
    int rc;
    mem_init(&src, &m);
    share_tree(&m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    o.skip_mac_metadata = 1;
    lp(d, "游戏");
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "Games", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0);
    CHECK(pr.files_total == 3 && pr.files_done == 3);
    CHECK(pr.bytes_total == sizeof k_big - 1 + 3 && pr.bytes_done == pr.bytes_total);
    lp(f, "游戏/中文 文件夹/存档.bin");
    CHECK(read_file(f, buf, sizeof buf) > 0 && strcmp(buf, k_big) == 0);
    lp(f, "游戏/中文 文件夹/empty.txt");
    CHECK(exists(f) && read_file(f, buf, sizeof buf) == 0);
    lp(f, "游戏/._top.txt");
    CHECK(!exists(f));
    lp(f, "游戏/.DS_Store");
    CHECK(!exists(f));
    lp(f, "游戏/link");
    CHECK(!exists(f));
    CHECK(parts_under(g_tmp) == 0);
    /* the folder exists now: refused at the top without overwrite, merged with it */
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "Games", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_EXISTS);
    lp(f, "游戏/top.txt");
    write_file(f, "old");
    o.overwrite = 1;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "Games", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0 && read_file(f, buf, sizeof buf) == 3 && strcmp(buf, "top") == 0);
    rm_rf(d);
    mem_free(&m);
}

static void t_drop_and_resume(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char d[1024], buf[256], failed[256];
    int rc;
    mem_init(&src, &m);
    share_tree(&m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    lp(d, "resumed.bin");
    m.drop_at_read = 4; /* mid-file */
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0);
    CHECK(pr.reconnects == 1);
    CHECK(read_file(d, buf, sizeof buf) == (int)sizeof k_big - 1 && strcmp(buf, k_big) == 0);
    CHECK(pr.bytes_done == sizeof k_big - 1); /* nothing counted twice */
    CHECK(parts_under(g_tmp) == 0);
    unlink(d);

    /* a share that never comes back: the copy fails cleanly, no part file, no destination */
    m.connected = 1;
    m.reads = 0;
    m.drop_at_read = 3;
    m.reconnect_fails = -1;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_DISCONNECTED);
    CHECK(!exists(d));
    CHECK(parts_under(g_tmp) == 0);
    CHECK(strcmp(failed, "one.bin") == 0);

    /* comes back on the third try */
    m.connected = 1;
    m.reads = 0;
    m.drop_at_read = 2;
    m.reconnect_fails = 2;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == 0 && pr.reconnects == 1);
    unlink(d);
    mem_free(&m);
}

static void t_cancel(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char d[1024], failed[256];
    int rc;
    mem_init(&src, &m);
    share_tree(&m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    lp(d, "cancelled");
    memset(&pr, 0, sizeof pr);
    m.cancel = &pr.cancel;
    m.cancel_at_read = 5;
    rc = copy_run(&src, "Games", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_CANCELLED);
    CHECK(parts_under(g_tmp) == 0);
    CHECK(m.open_handles == 0);
    rm_rf(d);
    mem_free(&m);
}

static void t_no_space(void) {
    xfs_t src, dst;
    mem_t ms, md;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char failed[256];
    int rc;
    mem_init(&src, &ms);
    share_tree(&ms);
    mem_init(&dst, &md);
    md.free_bytes = 10;
    o.sleep_s = no_sleep;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "one.bin", &dst, "one.bin", &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_NO_SPACE);
    CHECK(!find(&md, "one.bin"));
    mem_free(&ms);
    mem_free(&md);
}

static void t_upload(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char s[1024], f[1024], failed[256];
    node_t *x;
    int rc;
    mem_init(&dst, &m);
    localfs_init(&src);
    o.sleep_s = no_sleep;
    lp(s, "up");
    mkdir(s, 0777);
    lp(f, "up/sub");
    mkdir(f, 0777);
    lp(f, "up/sub/naïve.txt");
    write_file(f, k_big);
    lp(f, "up/a.txt");
    write_file(f, "alpha");
    lp(f, "up/sl");
    (void)symlink("/etc/passwd", f); /* a link is never followed */
    m.drop_at_write = 3;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, s, &dst, "Backup/up", &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_NOT_FOUND); /* "Backup" is not there */
    add(&m, "Backup", XK_DIR, NULL, 0);
    m.connected = 1;
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, s, &dst, "Backup/up", &o, &pr, failed, sizeof failed);
    CHECK(rc == 0);
    CHECK(pr.reconnects >= 1);
    x = find(&m, "Backup/up/sub/naïve.txt");
    CHECK(x && x->size == sizeof k_big - 1 && memcmp(x->data, k_big, x->size) == 0);
    x = find(&m, "Backup/up/a.txt");
    CHECK(x && x->size == 5 && memcmp(x->data, "alpha", 5) == 0);
    CHECK(!find(&m, "Backup/up/sl"));
    CHECK(mem_parts(&m) == 0);
    /* remove the tree on the share */
    memset(&pr, 0, sizeof pr);
    rc = copy_remove_tree(&dst, "Backup/up", &pr);
    CHECK(rc == 0 && !find(&m, "Backup/up") && !find(&m, "Backup/up/a.txt") && find(&m, "Backup"));
    CHECK(copy_remove_tree(&dst, "Backup/nothing", &pr) == XE_NOT_FOUND);
    rm_rf(s);
    mem_free(&m);
}

static void t_depth(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char p[SMBP_PATH_MAX] = "", d[1024], failed[256];
    int i, rc;
    mem_init(&src, &m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    for (i = 0; i < COPY_MAX_DEPTH + 3; i++) {
        size_t l = strlen(p);
        snprintf(p + l, sizeof p - l, "%sd", l ? "/" : "");
        add(&m, p, XK_DIR, NULL, 0);
    }
    lp(d, "deep");
    memset(&pr, 0, sizeof pr);
    rc = copy_run(&src, "d", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_TOO_DEEP);
    rm_rf(d);
    mem_free(&m);
}

/* A name longer than 255 bytes on the share (100 CJK characters are 300 UTF-8 bytes): the folder
 * copy fails and names the folder, never reports done with the file missing. */
static void t_long_name(void) {
    xfs_t src, dst;
    mem_t m;
    copy_progress_t pr;
    copy_opts_t o = {0};
    char d[1024], longp[SMBP_PATH_MAX], failed[256];
    int i, rc;
    mem_init(&src, &m);
    localfs_init(&dst);
    o.sleep_s = no_sleep;
    add(&m, "Long", XK_DIR, NULL, 0);
    add(&m, "Long/a.txt", XK_FILE, "a", 1);
    snprintf(longp, sizeof longp, "Long/");
    for (i = 0; i < 100; i++) strcat(longp, "\xe6\x96\x87"); /* 文 */
    add(&m, longp, XK_FILE, "b", 1);
    lp(d, "long");
    memset(&pr, 0, sizeof pr);
    failed[0] = '\0';
    rc = copy_run(&src, "Long", &dst, d, &o, &pr, failed, sizeof failed);
    CHECK(rc == XE_BAD_NAME);
    CHECK(strcmp(failed, "Long") == 0);
    rm_rf(d);
    mem_free(&m);
}

int main(void) {
    snprintf(g_tmp, sizeof g_tmp, "/tmp/ps5upload-smbd-copy-%d", (int)getpid());
    rm_rf(g_tmp);
    mkdir(g_tmp, 0777);
    t_download_file();
    t_download_tree();
    t_drop_and_resume();
    t_cancel();
    t_no_space();
    t_upload();
    t_depth();
    t_long_name();
    rm_rf(g_tmp);
    if (g_fail) {
        fprintf(stderr, "%d check(s) failed\n", g_fail);
        return 1;
    }
    printf("smbd copy self-test: ok\n");
    return 0;
}
