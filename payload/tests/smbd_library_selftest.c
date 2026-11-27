/* Host self-test: the game library scan (payload/smb/smbd_library.c) over the real local filesystem
 * (smbd_localfs.c) in a temp folder: packages, folder dumps (PS5 param.json, PS4 param.sfo) and
 * images are found with their details and covers; hidden, metadata and too-deep folders, a dump's
 * inside and the skipped folder are not walked; the limits stop the walk and keep what was found;
 * a rescan replaces the old covers; a cancelled scan writes nothing. Disk images are named and
 * covered by their own sce_sys/param.json and icon0.png (#963), one that cannot be read keeps the
 * title id in its name and says why, and a rescan reads only an image whose size or mtime changed. */
#include <dirent.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>

#include "image_fixtures.h"
#include "pkg_info.h"
#include "smbd_fs.h"
#include "smbd_library.h"
#include "smbd_log.h"
#include "smbd_path.h"

/* The daemon's log (smbd_main.c): the last line kept, for the checks. */
static char g_log[1024];
void smbd_log(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(g_log, sizeof g_log, fmt, ap);
    va_end(ap);
}

static int g_fail;
#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            g_fail++;                                                    \
        }                                                                \
    } while (0)

static char g_root[512];
static char g_out[512];

static void put_be32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}
static void put_le32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}
static void put_le16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

/* A PARAM.SFO of string keys. Returns its length. */
static size_t build_sfo(uint8_t *b, const char *const *keys, const char *const *vals, size_t n) {
    size_t i, koff = 0, doff = 0, key_tab = 20 + n * 16, data_tab;
    for (i = 0; i < n; i++) koff += strlen(keys[i]) + 1;
    data_tab = key_tab + ((koff + 3) & ~(size_t)3);
    memset(b, 0, 4096);
    memcpy(b, "\0PSF", 4);
    put_le32(b + 4, 0x101);
    put_le32(b + 8, (uint32_t)key_tab);
    put_le32(b + 12, (uint32_t)data_tab);
    put_le32(b + 16, (uint32_t)n);
    koff = 0;
    for (i = 0; i < n; i++) {
        uint8_t *e = b + 20 + i * 16;
        size_t kl = strlen(keys[i]) + 1, vl = strlen(vals[i]) + 1, cap = (vl + 3) & ~(size_t)3;
        memcpy(b + key_tab + koff, keys[i], kl);
        put_le16(e, (uint16_t)koff);
        koff += kl;
        put_le16(e + 2, 0x0204);
        put_le32(e + 4, (uint32_t)vl);
        put_le32(e + 8, (uint32_t)cap);
        put_le32(e + 12, (uint32_t)doff);
        memcpy(b + data_tab + doff, vals[i], vl);
        doff += cap;
    }
    return data_tab + doff;
}

static const uint8_t k_png[24] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n', 0, 0, 0, 13, 'I', 'H', 'D', 'R',
                                   0, 0, 0, 1, 0, 0, 0, 1 };

static void mkdirs(const char *rel) {
    char p[2048], *s;
    snprintf(p, sizeof p, "%s/%s", g_root, rel);
    for (s = p + strlen(g_root) + 1; *s; s++)
        if (*s == '/') {
            *s = '\0';
            mkdir(p, 0777);
            *s = '/';
        }
    mkdir(p, 0777);
}

static void put(const char *rel, const void *b, size_t n) {
    char p[2048], dir[1024];
    FILE *f;
    snprintf(dir, sizeof dir, "%s", rel);
    if (strrchr(dir, '/')) {
        *strrchr(dir, '/') = '\0';
        mkdirs(dir);
    }
    snprintf(p, sizeof p, "%s/%s", g_root, rel);
    f = fopen(p, "wb");
    if (!f) {
        perror(p);
        exit(1);
    }
    fwrite(b, 1, n, f);
    fclose(f);
}

/* A PS4 package with PARAM.SFO and icon0.png. */
static void put_pkg(const char *rel) {
    static const char *keys[] = { "APP_VER", "CATEGORY", "TITLE", "TITLE_ID" };
    static const char *vals[] = { "01.02", "gp", "Jak X: Combat Racing", "CUSA07842" };
    static const char cid[] = "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0";
    uint8_t *b = calloc(1, 16384), sfo[4096];
    size_t sl = build_sfo(sfo, keys, vals, 4), data = 0x100 + 2 * 0x20;
    put_be32(b, 0x7F434E54);
    put_be32(b + 0x10, 2);
    put_be32(b + 0x18, 0x100);
    memcpy(b + 0x40, cid, sizeof cid - 1);
    put_be32(b + 0x100, 0x1000);
    put_be32(b + 0x110, (uint32_t)data);
    put_be32(b + 0x114, (uint32_t)sl);
    memcpy(b + data, sfo, sl);
    data += (sl + 15) & ~(size_t)15;
    put_be32(b + 0x120, 0x1200);
    put_be32(b + 0x130, (uint32_t)data);
    put_be32(b + 0x134, sizeof k_png);
    memcpy(b + data, k_png, sizeof k_png);
    data += sizeof k_png;
    put(rel, b, data + 64);
    free(b);
}

static char *slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    char *b;
    long n;
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    n = ftell(f);
    fseek(f, 0, SEEK_SET);
    b = calloc(1, (size_t)n + 1);
    if (fread(b, 1, (size_t)n, f) != (size_t)n) n = 0;
    fclose(f);
    return b;
}

static int count_icons(const char *prefix) {
    char dir[1024];
    DIR *d;
    struct dirent *e;
    int n = 0;
    snprintf(dir, sizeof dir, "%s/icons", g_out);
    d = opendir(dir);
    if (!d) return 0;
    while ((e = readdir(d)) != NULL)
        if (strncmp(e->d_name, prefix, strlen(prefix)) == 0) n++;
    closedir(d);
    return n;
}

static int count_items(const char *json) {
    int n = 0;
    const char *p = json;
    while ((p = strstr(p, "{\"kind\":")) != NULL) {
        n++;
        p++;
    }
    return n;
}

static void build_tree(void) {
    static const char pj[] =
        "{\"titleId\":\"PPSA01234\",\"contentId\":\"EP0001-PPSA01234_00-GAME000000000000\",\"contentVersion\":\"01.000.002\","
        "\"requiredSystemSoftwareVersion\":\"0x0750000000000000\",\"sdkVersion\":\"0x0450000000000000\",\"localizedParameters\":{\"defaultLanguage\":\"en-US\","
        "\"en-US\":{\"titleName\":\"Space \\u00c9xplorer\"}}}";
    static const char *keys[] = { "CATEGORY", "TITLE", "TITLE_ID", "APP_VER", "PUBTOOLINFO" };
    static const char *vals[] = { "gd", "Old Game", "CUSA00001", "01.00", "c_date=20200101,sdk_ver=05050001,st_type=digital50" };
    uint8_t sfo[4096];
    size_t sl = build_sfo(sfo, keys, vals, 5);
    put_pkg("Games/Jak X.pkg");
    put("Games/._Jak X.pkg", "junk", 4);
    put("Games/broken.pkg", "not a package, just text that is long enough to be read as one: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", 160);
    put("Dumps/PPSA01234-app/sce_sys/param.json", pj, sizeof pj - 1);
    put("Dumps/PPSA01234-app/sce_sys/icon0.png", k_png, sizeof k_png);
    put("Dumps/PPSA01234-app/eboot.bin", "elf", 3);
    put("Dumps/PPSA01234-app/fakelib/libSceAgc.sprx", "lib", 3); /* a backported copy */
    put_pkg("Dumps/PPSA01234-app/inside/never.pkg"); /* inside a dump: not walked */
    put("Dumps/CUSA00001/sce_sys/param.sfo", sfo, sl);
    put("Images/PPSA09999 Venus Vacation.exfat", "x", 1);
    put("Images/other.FFPKG", "xy", 2);
    put("Images/readme.txt", "r", 1);
    put(".hidden/hid.exfat", "h", 1);
    put("$RECYCLE.BIN/bin.exfat", "b", 1);
    put("@eaDir/thumb.exfat", "t", 1);
    put("skipme/inner.exfat", "s", 1);
    put("d1/d2/d3/d4/d5/d6/d7/deep.exfat", "d", 1);
    put("d1/d2/d3/d4/d5/d6/ok.exfat", "o", 1);
}

/* ---- a share: the local tree behind share-relative paths ("" = the share's top), with one folder
 * whose listing can fail the way a dropped or refusing server would ---- */

typedef struct {
    xfs_t *local;
    const char *drop; /* a share-relative folder whose opendir answers drop_rc, or NULL */
    int drop_rc;
    int opens;        /* files opened for reading */
} share_t;

#define SH(fs) ((share_t *)(fs)->impl)

static void share_abs(const char *rel, char *out, size_t cap) {
    if (rel[0]) snprintf(out, cap, "%s/%s", g_root, rel);
    else snprintf(out, cap, "%s", g_root);
}
static int sh_stat(xfs_t *fs, const char *p, xstat_t *st) {
    char a[1024];
    share_abs(p, a, sizeof a);
    return SH(fs)->local->ops->stat(SH(fs)->local, a, st);
}
static int sh_opendir(xfs_t *fs, const char *p, void **d) {
    char a[1024];
    if (SH(fs)->drop && strcmp(p, SH(fs)->drop) == 0) return SH(fs)->drop_rc;
    share_abs(p, a, sizeof a);
    return SH(fs)->local->ops->opendir(SH(fs)->local, a, d);
}
static int sh_readdir(xfs_t *fs, void *d, char *n, size_t cap, xstat_t *st) {
    return SH(fs)->local->ops->readdir(SH(fs)->local, d, n, cap, st);
}
static void sh_closedir(xfs_t *fs, void *d) { SH(fs)->local->ops->closedir(SH(fs)->local, d); }
static int sh_open_read(xfs_t *fs, const char *p, void **fh) {
    char a[1024];
    share_abs(p, a, sizeof a);
    SH(fs)->opens++;
    return SH(fs)->local->ops->open_read(SH(fs)->local, a, fh);
}
static long long sh_pread(xfs_t *fs, void *fh, void *b, size_t n, uint64_t off) {
    return SH(fs)->local->ops->pread(SH(fs)->local, fh, b, n, off);
}
static int sh_close(xfs_t *fs, void *fh) { return SH(fs)->local->ops->close(SH(fs)->local, fh); }
static size_t sh_chunk(xfs_t *fs) { return SH(fs)->local->ops->chunk ? SH(fs)->local->ops->chunk(SH(fs)->local) : 0; }

static const xfs_ops_t k_share_ops = { sh_stat, sh_opendir, sh_readdir, sh_closedir, NULL,    sh_open_read, NULL, sh_pread,
                                       NULL,    sh_close,   NULL,       NULL,        NULL,    NULL,         NULL, sh_chunk };

static void title_ids(void) {
    char t[10];
    CHECK(lib_title_id_in("PPSA09999 Venus.exfat", t) == 1 && strcmp(t, "PPSA09999") == 0);
    CHECK(lib_title_id_in("[CUSA12345] x", t) == 1 && strcmp(t, "CUSA12345") == 0);
    CHECK(lib_title_id_in("xPPSA09999", t) == 0);  /* glued to a word */
    CHECK(lib_title_id_in("PPSA099991", t) == 0);  /* ten digits */
    CHECK(lib_title_id_in("ppsa09999", t) == 0);
    CHECK(lib_title_id_in("Game.exfat", t) == 0 && t[0] == '\0');
    CHECK(lib_source_ok("console") && lib_source_ok("3fa2c1d0") && !lib_source_ok("") && !lib_source_ok("../x") &&
          !lib_source_ok("a/b") && !lib_source_ok("0123456789012345678901234567890123"));
}

int main(void) {
    char tmpl[] = "/tmp/ps5u-lib-XXXXXX", *base, roots[1200], skip[600], json_path[700];
    char *j;
    xfs_t fs;
    copy_progress_t pr;
    lib_limits_t lim;
    int rc;
    title_ids();
    base = mkdtemp(tmpl);
    if (!base) return 1;
    snprintf(g_root, sizeof g_root, "%s/root", base);
    snprintf(g_out, sizeof g_out, "%s/out", base);
    mkdir(g_root, 0777);
    build_tree();
    localfs_init(&fs);
    snprintf(roots, sizeof roots, "%s\n%s/not-there", g_root, base);
    snprintf(skip, sizeof skip, "%s/skipme", g_root);
    snprintf(json_path, sizeof json_path, "%s/console.json", g_out);

    /* the full scan */
    memset(&pr, 0, sizeof pr);
    rc = lib_scan(&fs, "console", roots, skip, g_out, NULL, &pr);
    CHECK(rc == 0);
    j = slurp(json_path);
    CHECK(j != NULL);
    if (j) {
        CHECK(strstr(j, "\"source\":\"console\"") && strstr(j, "\"complete\":true") && strstr(j, "\"stopped\":null"));
        CHECK(count_items(j) == 6);
        CHECK(strstr(j, "\"kind\":\"pkg\"") && strstr(j, "\"title\":\"Jak X: Combat Racing\"") &&
              strstr(j, "\"title_id\":\"CUSA07842\"") && strstr(j, "\"type\":\"patch\"") && strstr(j, "\"version\":\"01.02\"") &&
              strstr(j, "\"platform\":\"ps4\"") && strstr(j, "/Games/Jak X.pkg\""));
        CHECK(strstr(j, "\"kind\":\"folder\"") && strstr(j, "\"title\":\"Space \xc3\x89xplorer\"") &&
              strstr(j, "\"title_id\":\"PPSA01234\"") && strstr(j, "\"version\":\"01.000.002\"") && strstr(j, "\"fw\":\"7.50\"") &&
              strstr(j, "\"name\":\"PPSA01234-app\""));
        CHECK(strstr(j, "\"title\":\"Old Game\"") && strstr(j, "\"title_id\":\"CUSA00001\""));
        /* the SDK each was built with; the dump holding fakelib/ is a backport, the other is not */
        CHECK(strstr(j, "\"fw\":\"7.50\",\"sdk\":\"4.50\",\"backport\":true") != NULL);
        CHECK(strstr(j, "\"sdk\":\"5.05\",\"backport\":false") != NULL);
        CHECK(!strstr(j, "libSceAgc"));
        CHECK(strstr(j, "\"name\":\"PPSA09999 Venus Vacation.exfat\"") && strstr(j, "\"title_id\":\"PPSA09999\"") &&
              strstr(j, "\"name\":\"other.FFPKG\"") && strstr(j, "\"name\":\"ok.exfat\""));
        CHECK(!strstr(j, "never.pkg") && !strstr(j, "broken") && !strstr(j, "._Jak") && !strstr(j, "hid.exfat") &&
              !strstr(j, "bin.exfat") && !strstr(j, "thumb.exfat") && !strstr(j, "inner.exfat") &&
              !strstr(j, "deep.exfat") && !strstr(j, "readme"));
        free(j);
    }
    CHECK(count_icons("console-") == 2); /* the package's and the PS5 dump's */
    CHECK(pr.files_total == 6 && pr.files_done > 10 && !pr.scanning);

    /* a limit stops the walk and keeps what it found; the old scan's covers go */
    memset(&pr, 0, sizeof pr);
    memset(&lim, 0, sizeof lim);
    lim.items_max = 2;
    sleep(1); /* a new stamp */
    rc = lib_scan(&fs, "console", roots, skip, g_out, &lim, &pr);
    CHECK(rc == 0);
    j = slurp(json_path);
    if (j) {
        CHECK(strstr(j, "\"complete\":false") && strstr(j, "\"stopped\":\"items\"") && count_items(j) == 2);
        free(j);
    }
    CHECK(count_icons("console-") <= 2);

    memset(&pr, 0, sizeof pr);
    memset(&lim, 0, sizeof lim);
    lim.entries_max = 3;
    CHECK(lib_scan(&fs, "console", roots, skip, g_out, &lim, &pr) == 0);
    j = slurp(json_path);
    if (j) {
        CHECK(strstr(j, "\"stopped\":\"entries\"") != NULL);
        free(j);
    }

    /* a cancelled scan writes nothing and leaves the last result */
    memset(&pr, 0, sizeof pr);
    pr.cancel = 1;
    CHECK(lib_scan(&fs, "console", roots, skip, g_out, NULL, &pr) == XE_CANCELLED);
    j = slurp(json_path);
    CHECK(j && strstr(j, "\"stopped\":\"entries\""));
    free(j);

    /* a share: its root is "" (saved without a start folder) and is walked all the same; the old
     * loop never ran for an empty root, so such a share scanned to 0 items, "complete" */
    {
        share_t sh = { &fs, NULL, 0, 0 };
        xfs_t share = { &k_share_ops, &sh };
        char share_json[700], *before = NULL;
        int icons;
        snprintf(share_json, sizeof share_json, "%s/share1.json", g_out);
        memset(&pr, 0, sizeof pr);
        rc = lib_scan(&share, "share1", "", NULL, g_out, NULL, &pr);
        CHECK(rc == 0);
        before = slurp(share_json);
        CHECK(before != NULL);
        if (before) {
            CHECK(strstr(before, "\"complete\":true") && count_items(before) >= 6 && strstr(before, "Games/Jak X.pkg\"") &&
                  strstr(before, "\"title_id\":\"PPSA01234\""));
        }
        icons = count_icons("share1-");
        CHECK(icons == 2);

        /* the share drops while a folder is listed: the scan is abandoned with the cause and the
         * last result and its covers stay, instead of a partial listing written as complete */
        sh.drop = "Dumps";
        sh.drop_rc = XE_DISCONNECTED;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share1", "", NULL, g_out, NULL, &pr) == XE_DISCONNECTED);
        j = slurp(share_json);
        CHECK(j && before && strcmp(j, before) == 0);
        CHECK(count_icons("share1-") == icons);
        free(j);
        sh.drop_rc = XE_TIMEOUT;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share1", "", NULL, g_out, NULL, &pr) == XE_TIMEOUT);

        /* one folder the server refuses inside a working share is passed over */
        sh.drop_rc = XE_ACCESS;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share1", "", NULL, g_out, NULL, &pr) == 0);
        j = slurp(share_json);
        CHECK(j && strstr(j, "\"complete\":true") && !strstr(j, "PPSA01234") && strstr(j, "Games/Jak X.pkg\""));
        free(j);

        /* the start folder itself cannot be read: reported, never an empty result */
        sh.drop = "";
        sh.drop_rc = XE_ACCESS;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share1", "", NULL, g_out, NULL, &pr) == XE_ACCESS);
        free(before);
    }

    /* disk images on a share (#963) */
    {
        share_t sh = { &fs, NULL, 0, 0 };
        xfs_t share = { &k_share_ops, &sh };
        const char *pj = fx_param_json("PPSA23012", "Call of Duty: Modern Warfare II");
        char share_json[700], abs[1200], *icon_name = NULL;
        size_t n;
        uint8_t *img = fx_exfat((const uint8_t *)pj, strlen(pj), 0, 6000, 1, &n);
        struct timeval tv[2];
        put("Lib/PPSA23012.exfat", img, n);
        put("Lib/copy two/Modern Warfare.exfat", img, n); /* no id in the name: the image's own */
        put("Lib/PPSA11111.ffpfsc", "not an image at all", 19);
        free(img);
        snprintf(share_json, sizeof share_json, "%s/share2.json", g_out);
        memset(&pr, 0, sizeof pr);
        g_log[0] = '\0';
        CHECK(lib_scan(&share, "share2", "Lib", NULL, g_out, NULL, &pr) == 0);
        CHECK(sh.opens == 3);
        CHECK(strstr(g_log, "3 images, 2 read, 0 from the last scan, 1 not read") != NULL);
        j = slurp(share_json);
        CHECK(j != NULL);
        if (j) {
            const char *a = strstr(j, "\"path\":\"Lib/PPSA23012.exfat\""), *b = strstr(j, "\"path\":\"Lib/copy two/Modern Warfare.exfat\"");
            const char *c = strstr(j, "\"path\":\"Lib/PPSA11111.ffpfsc\"");
            CHECK(count_items(j) == 3 && a && b && c);
            if (a && b && c) {
                CHECK(strstr(a, "\"title\":\"Call of Duty: Modern Warfare II\",\"title_id\":\"PPSA23012\"") == a + strlen("\"path\":\"Lib/PPSA23012.exfat\",\"name\":\"PPSA23012.exfat\",") );
                CHECK(strstr(b, "\"title_id\":\"PPSA23012\"") && strstr(b, "\"content_id\":\"UP9000-PPSA23012_00-GAME000000000000\""));
                CHECK(strstr(a, "\"meta\":\"read\",\"mtime\":") && strstr(a, "\"icon\":\"share2-"));
                /* not readable here: the id from its name, and why */
                CHECK(strstr(c, "\"title\":\"\",\"title_id\":\"PPSA11111\"") && strstr(c, "\"meta\":\"img_unknown\"") &&
                      strstr(c, "\"icon\":\"\""));
                icon_name = strstr(a, "\"icon\":\"") + 8;
                icon_name = strndup(icon_name, (size_t)(strchr(icon_name, '"') - icon_name));
            }
            free(j);
        }
        CHECK(count_icons("share2-") == 2);

        /* unchanged: nothing is opened again, titles and covers stay (the covers under new names) */
        sleep(1);
        sh.opens = 0;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share2", "Lib", NULL, g_out, NULL, &pr) == 0);
        CHECK(sh.opens == 0);
        CHECK(strstr(g_log, "3 images, 0 read, 3 from the last scan, 0 not read") != NULL);
        j = slurp(share_json);
        if (j) {
            CHECK(strstr(j, "\"title\":\"Call of Duty: Modern Warfare II\"") && strstr(j, "\"meta\":\"img_unknown\"") &&
                  strstr(j, "\"icon\":\"share2-"));
            CHECK(icon_name && !strstr(j, icon_name)); /* a new stamp */
            free(j);
        }
        CHECK(count_icons("share2-") == 2);

        /* a changed mtime: that image is read again, the others are not */
        snprintf(abs, sizeof abs, "%s/Lib/PPSA23012.exfat", g_root);
        gettimeofday(&tv[0], NULL);
        tv[0].tv_sec += 100;
        tv[1] = tv[0];
        CHECK(utimes(abs, tv) == 0);
        sh.opens = 0;
        memset(&pr, 0, sizeof pr);
        CHECK(lib_scan(&share, "share2", "Lib", NULL, g_out, NULL, &pr) == 0);
        CHECK(sh.opens == 1);
        CHECK(strstr(g_log, "3 images, 1 read, 2 from the last scan, 0 not read") != NULL);
        CHECK(count_icons("share2-") == 2);
        free(icon_name);
    }

    /* a bad source id is refused before anything is read */
    memset(&pr, 0, sizeof pr);
    CHECK(lib_scan(&fs, "../x", roots, skip, g_out, NULL, &pr) == XE_BAD_REQUEST);

    {
        char cmd[700];
        snprintf(cmd, sizeof cmd, "rm -rf '%s'", base);
        if (system(cmd) != 0) fprintf(stderr, "could not remove %s\n", base);
    }
    if (g_fail) {
        fprintf(stderr, "%d failure(s)\n", g_fail);
        return 1;
    }
    printf("smbd_library selftest: ok\n");
    return 0;
}
