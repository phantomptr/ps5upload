/* Host-side tests for PS5Upload Play's install (play_app.c), done the homebrew store app's way: the
 * receipt's text, where the app lives (receipt or default), the file order (param.json last), a
 * fresh install (one folder rename), a quick update (overlay, files kept, staging removed), the
 * refusals (journal, backup, something unexpected at the target, no kept copy) and the refresh of
 * the console's sce_sys copies. Every path is under a fresh mkdtemp root. Build: see the root
 * Makefile test-payload. */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "play_app.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static char g_root[256];

/* root + rel, in one of four rotating buffers (two in one call stay apart). */
static const char *at(const char *rel) {
    static char paths[4][512];
    static int next;
    char *p = paths[next++ & 3];
    snprintf(p, 512, "%s%s", g_root, rel);
    return p;
}

static void mkp(const char *rel) {
    char cmd[700];
    snprintf(cmd, sizeof cmd, "mkdir -p '%s'", at(rel));
    if (system(cmd) != 0) exit(2);
}

static void put(const char *rel, const char *text) {
    FILE *f;
    char dir[512], *s;
    snprintf(dir, sizeof dir, "%s", rel);
    if ((s = strrchr(dir, '/')) && s != dir) {
        *s = 0;
        mkp(dir);
    }
    f = fopen(at(rel), "w");
    if (!f) {
        perror(at(rel));
        exit(2);
    }
    fputs(text, f);
    fclose(f);
}

static void rmr(const char *rel) {
    char cmd[700];
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", at(rel));
    if (system(cmd) != 0) exit(2);
}

static int exists(const char *rel) { return access(at(rel), F_OK) == 0; }

static int same_text(const char *rel, const char *text) {
    char buf[600];
    size_t n;
    FILE *f = fopen(at(rel), "r");
    if (!f) return 0;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return !strcmp(buf, text);
}

static int has_text(const char *rel, const char *needle) {
    char buf[2048];
    size_t n;
    FILE *f = fopen(at(rel), "r");
    if (!f) return 0;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return strstr(buf, needle) != NULL;
}

static unsigned mode_of(const char *rel) {
    struct stat st;
    return stat(at(rel), &st) == 0 ? (unsigned)(st.st_mode & 07777) : 0;
}

static ino_t inode(const char *rel) {
    struct stat st;
    return stat(at(rel), &st) == 0 ? st.st_ino : 0;
}

#define KEPT "/data/ps5upload/play/FAKE99261"
#define HB "/data/homebrew/FAKE99261"

static void kept_app(const char *ver, const char *eboot) {
    char param[200];
    rmr(KEPT);
    put(KEPT "/eboot.bin", eboot);
    put(KEPT "/sce_sys/icon0.png", "ICON-" );
    put(KEPT "/sce_sys/about/right.sprx", "SPRX");
    snprintf(param, sizeof param, "{\"titleId\":\"FAKE99261\",\"contentVersion\":\"%s\"}", ver);
    put(KEPT "/sce_sys/param.json", param);
}

static play_app_env_t env(void) {
    play_app_env_t e;
    memset(&e, 0, sizeof e);
    e.root = g_root;
    e.now = 1795000000; /* 2026-11-18T... */
    return e;
}

static void test_pure(void) {
    char out[600], v[64];
    char *rel[] = { "sce_sys/param.json", "eboot.bin", "sce_sys/icon0.png", "a/b.txt", "sce_sys/about/right.sprx" };
    play_app_human_version("13.000.000", v, sizeof v);
    CHECK(!strcmp(v, "13.0.0"));
    play_app_human_version("13.001.020", v, sizeof v);
    CHECK(!strcmp(v, "13.1.20"));
    play_app_human_version("beta-1", v, sizeof v);
    CHECK(!strcmp(v, "beta-1"));
    CHECK(play_app_receipt_text(out, sizeof out, "/data/homebrew", "13.000.000",
                                "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789", 1795000000) > 0);
    CHECK(!strcmp(out, "{\"schema\":1,\"titleId\":\"FAKE99261\",\"location\":\"/data/homebrew\",\"contentVersion\":\"13.000.000\","
                       "\"releaseTag\":\"v13.0.0\",\"sha256\":\"abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789\","
                       "\"installedAt\":\"2026-11-18T11:06:40Z\"}\n"));
    CHECK(play_app_receipt_text(out, sizeof out, "/mnt/ext1/homebrew", "13.000.000", "short", 0) > 0);
    CHECK(strstr(out, "\"sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\"") != NULL);
    CHECK(strstr(out, "\"installedAt\":\"1970-01-01T00:00:00Z\"") != NULL);
    CHECK(play_app_receipt_text(out, sizeof out, "/data/../etc", "13.000.000", NULL, 0) == -1);
    CHECK(play_app_receipt_text(out, 40, "/data/homebrew", "13.000.000", NULL, 0) == -1);
    /* drive roots and clean locations */
    CHECK(play_app_drive_root("/data/homebrew", v, sizeof v) == 0 && !strcmp(v, "/data"));
    CHECK(play_app_drive_root("/mnt/usb3/apps", v, sizeof v) == 0 && !strcmp(v, "/mnt/usb3"));
    CHECK(play_app_drive_root("/mnt/ext0", v, sizeof v) == 0 && !strcmp(v, "/mnt/ext0"));
    CHECK(play_app_drive_root("/database", v, sizeof v) == -1);
    CHECK(play_app_drive_root("/mnt/usb8/x", v, sizeof v) == -1);
    CHECK(play_app_location_ok("/data/homebrew"));
    CHECK(!play_app_location_ok("/data/homebrew/"));
    CHECK(!play_app_location_ok("data/homebrew"));
    CHECK(!play_app_location_ok("/data//homebrew"));
    CHECK(!play_app_location_ok("/data/./homebrew"));
    CHECK(!play_app_location_ok("/data/../user"));
    CHECK(!play_app_location_ok("/system/homebrew"));
    /* the order things go into place: by path, param.json last */
    play_app_order(rel, 5);
    CHECK(!strcmp(rel[0], "a/b.txt") && !strcmp(rel[1], "eboot.bin") && !strcmp(rel[2], "sce_sys/about/right.sprx") &&
          !strcmp(rel[3], "sce_sys/icon0.png") && !strcmp(rel[4], "sce_sys/param.json"));
    CHECK(play_app_has_nocase("x  ShadowMountPlus.elf", 22, "shadowmountplus"));
    CHECK(!play_app_has_nocase("shadowmount", 11, "shadowmountplus"));
}

static void test_target(void) {
    char loc[256];
    rmr("/data");
    rmr("/mnt");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 0 && !strcmp(loc, "/data/homebrew"));
    /* a receipt naming a folder that holds the app: it lives there */
    put("/data/prosperostore/receipts/FAKE99261.json",
        "{\"schema\":1,\"titleId\":\"FAKE99261\",\"location\":\"/mnt/ext1/homebrew\",\"contentVersion\":\"13.000.000\"}");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 0); /* the folder is not there */
    put("/mnt/ext1/homebrew/FAKE99261/sce_sys/param.json", "{\"titleId\":\"FAKE99261\",\"contentVersion\":\"13.000.000\"}");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 1 && !strcmp(loc, "/mnt/ext1/homebrew"));
    CHECK(play_app_store_managed(g_root, "/mnt/ext1/homebrew", "13.000.000") == 1);
    CHECK(play_app_store_managed(g_root, "/mnt/ext1/homebrew", "13.001.000") == 0);
    /* another title's param.json there, another title's receipt, an unclean location: default */
    put("/mnt/ext1/homebrew/FAKE99261/sce_sys/param.json", "{\"titleId\":\"FAKE00001\"}");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 0 && !strcmp(loc, "/data/homebrew"));
    put("/mnt/ext1/homebrew/FAKE99261/sce_sys/param.json", "{\"titleId\":\"FAKE99261\"}");
    put("/data/prosperostore/receipts/FAKE99261.json", "{\"titleId\":\"FAKE00001\",\"location\":\"/mnt/ext1/homebrew\"}");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 0);
    put("/data/prosperostore/receipts/FAKE99261.json", "{\"titleId\":\"FAKE99261\",\"location\":\"/mnt/ext1/../ext1/homebrew\"}");
    CHECK(play_app_target(g_root, loc, sizeof loc) == 0);
    rmr("/data");
    rmr("/mnt");
}

static void test_fresh_and_update(void) {
    play_app_env_t e = env();
    play_app_result_t r;
    const char *err = "x";
    char v[32];
    ino_t dir_ino;
    rmr("/data");
    rmr("/user");
    /* no kept copy yet */
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "not_staged"));
    /* fresh: one rename of the staged folder to /data/homebrew/FAKE99261, everything 0777 */
    kept_app("13.000.000", "EBOOT-1");
    put("/data/ps5upload/play/FAKE99261.sha256", "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    CHECK(play_app_install(&e, &r, &err) == 0 && err == NULL);
    CHECK(!r.update && !strcmp(r.location, "/data/homebrew") && !strcmp(r.version, "13.000.000"));
    CHECK(same_text(HB "/eboot.bin", "EBOOT-1") && same_text(HB "/sce_sys/about/right.sprx", "SPRX"));
    CHECK(mode_of(HB) == 0777 && mode_of(HB "/sce_sys") == 0777 && mode_of(HB "/eboot.bin") == 0777);
    CHECK(!exists("/data/prosperostore/staging/FAKE99261"));
    CHECK(same_text("/data/prosperostore/receipts/FAKE99261.json",
                    "{\"schema\":1,\"titleId\":\"FAKE99261\",\"location\":\"/data/homebrew\",\"contentVersion\":\"13.000.000\","
                    "\"releaseTag\":\"v13.0.0\",\"sha256\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\","
                    "\"installedAt\":\"2026-11-18T11:06:40Z\"}\n"));
    CHECK(!exists("/data/prosperostore/receipts/FAKE99261.json.tmp"));
    CHECK(exists(KEPT "/eboot.bin")); /* the kept copy stays */
    /* quick update: same folder, files overlaid, a file the new version lacks stays */
    dir_ino = inode(HB);
    put(HB "/settings.dat", "KEEP");
    kept_app("13.001.000", "EBOOT-2");
    unlink(at("/data/ps5upload/play/FAKE99261.sha256"));
    mkp("/user/appmeta/FAKE99261");
    mkp("/user/app/FAKE99261/sce_sys");
    put("/user/app/FAKE99261/icon0.png", "OLD");
    CHECK(play_app_install(&e, &r, &err) == 0);
    CHECK(r.update && inode(HB) == dir_ino);
    CHECK(same_text(HB "/eboot.bin", "EBOOT-2") && same_text(HB "/settings.dat", "KEEP"));
    CHECK(play_app_version(g_root, HB, v, sizeof v) == 1 && !strcmp(v, "13.001.000"));
    CHECK(!exists("/data/prosperostore/staging/FAKE99261"));
    CHECK(has_text("/data/prosperostore/receipts/FAKE99261.json", "\"contentVersion\":\"13.001.000\""));
    CHECK(has_text("/data/prosperostore/receipts/FAKE99261.json", "\"sha256\":\"00000000000000000000000000000000"));
    /* the console's copies of sce_sys follow */
    CHECK(has_text("/user/appmeta/FAKE99261/param.json", "13.001.000"));
    CHECK(same_text("/user/app/FAKE99261/sce_sys/icon0.png", "ICON-"));
    CHECK(same_text("/user/app/FAKE99261/icon0.png", "ICON-"));
    /* install.json */
    CHECK(play_app_write_install_json(g_root, "13.001.000", "/data/homebrew", "shadowmount", 1795000000) == 0);
    CHECK(same_text("/data/ps5upload/play/install.json",
                    "{\"version\":\"13.001.000\",\"location\":\"/data/homebrew\",\"registered_by\":\"shadowmount\","
                    "\"installed_at\":\"2026-11-18T11:06:40Z\"}\n"));
}

static void test_refusals(void) {
    play_app_env_t e = env();
    play_app_result_t r;
    const char *err = NULL;
    rmr("/data");
    kept_app("13.000.000", "E");
    /* a store transaction (any title) */
    put("/data/prosperostore/journal.json", "{\"titleId\":\"OTHER0001\"}");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "store_busy"));
    CHECK(play_app_store_busy(g_root) == 1);
    unlink(at("/data/prosperostore/journal.json"));
    /* something unexpected where the folder goes */
    put(HB, "a file");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "unexpected_target"));
    unlink(at(HB));
    put(HB "/readme.txt", "another app's folder");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "unexpected_target"));
    CHECK(same_text(HB "/readme.txt", "another app's folder"));
    rmr(HB);
    /* an update while the store's clean-update backup is there */
    CHECK(play_app_install(&e, &r, &err) == 0);
    mkp("/data/prosperostore/backup/FAKE99261");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "store_backup_present"));
    rmr("/data/prosperostore/backup");
    /* an update over a folder where a file should be: nothing moved, param.json untouched */
    kept_app("13.002.000", "E3");
    rmr(HB "/sce_sys/icon0.png");
    mkp(HB "/sce_sys/icon0.png/x");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "unexpected_target"));
    CHECK(has_text(HB "/sce_sys/param.json", "13.000.000"));
    CHECK(same_text(HB "/eboot.bin", "E"));
    CHECK(!exists("/data/prosperostore/staging/FAKE99261"));
    /* an incomplete kept copy (no param.json yet) */
    rmr(KEPT "/sce_sys/param.json");
    CHECK(play_app_install(&e, &r, &err) == -1 && !strcmp(err, "not_staged"));
}

int main(void) {
    snprintf(g_root, sizeof g_root, "/tmp/play-app-selftest-XXXXXX");
    if (!mkdtemp(g_root)) {
        perror("mkdtemp");
        return 2;
    }
    umask(022);
    test_pure();
    test_target();
    test_fresh_and_update();
    test_refusals();
    {
        char cmd[400];
        snprintf(cmd, sizeof cmd, "rm -rf '%s'", g_root);
        if (system(cmd) != 0) fprintf(stderr, "could not remove %s\n", g_root);
    }
    if (failures) {
        fprintf(stderr, "%d play app check(s) failed\n", failures);
        return 1;
    }
    printf("play app selftest ok\n");
    return 0;
}
