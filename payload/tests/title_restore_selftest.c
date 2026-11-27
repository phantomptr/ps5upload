/* Host self-test of putting an installed package game back on the home screen (title_restore.c, #961).
 *
 *   cc -pthread -Ipayload/include payload/tests/title_restore_selftest.c payload/src/title_restore.c
 *
 * Runs check / prepare / finish on a scratch folder laid out like a console (internal /user, USB
 * extended storage /mnt/ext0, M.2 /mnt/ext1), with app.db / appinfo.db answers set by the test.
 * The scratch folder is removed at the end. */
#include "title_restore.h"

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;
#define CHECK(c)                                                                \
    do {                                                                        \
        if (!(c)) {                                                             \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #c); \
            failures++;                                                         \
        }                                                                       \
    } while (0)

static char root[512];
static int g_appdb, g_appinfo, g_backup_ok = 1;
static uint64_t g_free = 1ull << 40;
static int g_backups;

static int appdb_has(const char *id) {
    (void)id;
    return g_appdb;
}
static int appinfo_has(const char *id) {
    (void)id;
    return g_appinfo;
}
static int backup_db(const char *which, const char *dst, char *err, size_t cap) {
    if (!g_backup_ok) {
        snprintf(err, cap, "test refuses");
        return -1;
    }
    FILE *f = fopen(dst, "wx");
    if (!f) return -1;
    fprintf(f, "copy of %s\n", which);
    fclose(f);
    g_backups++;
    return 0;
}
static int free_bytes(const char *dir, uint64_t *out) {
    (void)dir;
    *out = g_free;
    return 0;
}

static void at(char *buf, size_t cap, const char *path) { snprintf(buf, cap, "%s%s", root, path); }

static void mkdirs(const char *path) {
    char p[1024];
    at(p, sizeof p, path);
    for (char *s = p + strlen(root) + 1; *s; s++) {
        if (*s == '/') {
            *s = 0;
            mkdir(p, 0777);
            *s = '/';
        }
    }
    mkdir(p, 0777);
}

static void put_file(const char *path, const unsigned char *head, size_t n, size_t total) {
    char p[1024], dir[1024];
    snprintf(dir, sizeof dir, "%s", path);
    *strrchr(dir, '/') = 0;
    mkdirs(dir);
    at(p, sizeof p, path);
    FILE *f = fopen(p, "wb");
    fwrite(head, 1, n, f);
    for (size_t i = n; i < total; i++) fputc(0x5A, f);
    fclose(f);
}

static int has(const char *path) {
    char p[1024];
    struct stat st;
    at(p, sizeof p, path);
    return lstat(p, &st) == 0;
}

static long size_of(const char *path) {
    char p[1024];
    struct stat st;
    at(p, sizeof p, path);
    return lstat(p, &st) == 0 ? (long)st.st_size : -1;
}

static void rm(const char *path) {
    char p[1024];
    at(p, sizeof p, path);
    unlink(p);
}

static void rm_tree(const char *p) {
    DIR *d = opendir(p);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            char c[1024];
            snprintf(c, sizeof c, "%s/%s", p, e->d_name);
            struct stat st;
            if (lstat(c, &st) == 0 && S_ISDIR(st.st_mode)) rm_tree(c);
            else unlink(c);
        }
        closedir(d);
    }
    rmdir(p);
}

static const unsigned char FAKE[8] = {0x7F, 'F', 'I', 'H', 0x01, 0x00, 0x03, 0x00};
static const unsigned char RETAIL[8] = {0x7F, 'F', 'I', 'H', 0x01, 0x80, 0x03, 0x00};
static const unsigned char PS4[8] = {0x7F, 'C', 'N', 'T', 0x80, 0x00, 0x00, 0x01};

#define ID "PPSA01234"
#define PKG "/mnt/ext1/user/app/" ID "/app.pkg"
#define PATCH "/mnt/ext1/user/patch/" ID "/patch.pkg"
#define ASIDE "/mnt/ext1/ps5upload/restore/" ID
#define REC ASIDE "/restore.txt"

static title_restore_env_t env;
static char out[8192];

static int run(const char *op, const char *id, int flags) {
    return title_restore_run(&env, op, id, flags, "20261126-101500", out, sizeof out);
}

static int says(const char *s) { return strstr(out, s) != NULL; }

static void fresh(void) {
    rm_tree(root);
    mkdir(root, 0777);
    mkdirs("/user/app");
    mkdirs("/mnt/ext0/user");
    mkdirs("/mnt/ext1/user/app");
    put_file(PKG, FAKE, sizeof FAKE, 4096);
    put_file(PATCH, FAKE, sizeof FAKE, 1024);
    g_appdb = 0;
    g_appinfo = 0;
    g_backup_ok = 1;
    g_free = 1ull << 40;
    g_backups = 0;
}

int main(void) {
    const char *tmp = getenv("TMPDIR");
    snprintf(root, sizeof root, "%s/ps5u-title-restore-%d", tmp && tmp[0] ? tmp : "/tmp", (int)getpid());
    memset(&env, 0, sizeof env);
    env.prefix = root;
    env.appdb_has = appdb_has;
    env.appinfo_has = appinfo_has;
    env.backup_db = backup_db;
    env.free_bytes = free_bytes;

    /* ---- check: what each console state answers, read-only ---- */
    fresh();
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"restorable\"") && says("\"kind\":\"fake\"") &&
          says("\"where\":\"m2\"") && says("\"pkg\":\"" PKG "\"") && says("\"patch\":\"" PATCH "\"") &&
          says("\"aside\":\"" ASIDE "\"") && says("\"size\":4096"));
    CHECK(has(PKG) && !has(ASIDE));
    CHECK(run("check", "ppsa01234", 0) > 0 && says("\"title_id\":\"PPSA01234\"") && says("restorable"));
    g_appdb = 1;
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"registered\""));
    g_appdb = -1;
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"db_unreadable\"") && says("\"reason\":\"app.db\""));
    g_appdb = 0;
    g_appinfo = 1;
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"record_elsewhere\""));
    g_appinfo = -1;
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"db_unreadable\"") && says("appinfo.db"));
    g_appinfo = 0;
    g_free = 100;
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"no_room\""));
    g_free = 1ull << 40;
    put_file(PKG, RETAIL, sizeof RETAIL, 4096);
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"retail\""));
    put_file(PKG, PS4, sizeof PS4, 4096);
    CHECK(run("check", ID, 0) > 0 && says("restorable") && says("\"kind\":\"ps4\""));
    put_file(PKG, (const unsigned char *)"\0\0\0\0\0\0\0\0", 8, 4096);
    CHECK(run("check", ID, 0) > 0 && says("restorable") && says("\"kind\":\"unknown\""));
    put_file(PKG, FAKE, sizeof FAKE, 4096);
    put_file("/user/app/" ID "/mount.lnk", (const unsigned char *)"/data/x", 7, 7);
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"folder_registration\""));
    rm("/user/app/" ID "/mount.lnk");
    put_file("/user/app/" ID "/app.pkg", FAKE, sizeof FAKE, 64);
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"ambiguous\""));
    rm("/user/app/" ID "/app.pkg");
    CHECK(run("check", "PPSA99999", 0) > 0 && says("\"state\":\"not_installed\""));
    CHECK(run("check", "NPXS20001", 0) > 0 && says("\"state\":\"system_title\""));
    CHECK(run("check", "../etc/x", 0) > 0 && says("\"state\":\"invalid_title\""));
    CHECK(run("prepare", "NPXS20001", 0) > 0 && says("\"ok\":false") && says("restore_system_title"));
    printf("✓ check: restorable only for a package on a drive that no database knows; every other state named\n");

    /* ---- prepare refusals touch nothing ---- */
    g_appdb = 1;
    CHECK(run("prepare", ID, 0) > 0 && says("\"ok\":false") && says("\"err\":\"restore_registered\""));
    CHECK(has(PKG) && !has(ASIDE) && g_backups == 0);
    g_appdb = 0;
    g_backup_ok = 0;
    CHECK(run("prepare", ID, 0) > 0 && says("restore_backup_failed"));
    CHECK(has(PKG) && has(PATCH) && !has(ASIDE));
    g_backup_ok = 1;
    printf("✓ prepare refuses a registered title and a failed database backup before anything moves\n");

    /* ---- prepare, then an install that failed: rolled back ---- */
    CHECK(run("prepare", ID, 0) > 0 && says("\"ok\":true") && says("\"state\":\"prepared\"") &&
          says("\"install_path\":\"" ASIDE "/app.pkg\"") && says("\"patch_aside\":\"" ASIDE "/patch.pkg\"") &&
          says("\"backup\":\"/data/ps5upload/backup/restore/" ID "-20261126-101500\""));
    CHECK(!has(PKG) && !has(PATCH) && size_of(ASIDE "/app.pkg") == 4096 && size_of(ASIDE "/patch.pkg") == 1024 &&
          has(REC));
    CHECK(has("/data/ps5upload/backup/restore/" ID "-20261126-101500/app.db") &&
          has("/data/ps5upload/backup/restore/" ID "-20261126-101500/appinfo.db") && g_backups == 2);
    CHECK(run("check", ID, 0) > 0 && says("\"state\":\"pending\""));
    CHECK(run("prepare", ID, 0) > 0 && says("restore_pending"));
    /* not asked to put it back, not on the home screen: nothing moves */
    CHECK(run("finish", ID, 0) > 0 && says("\"state\":\"waiting\"") && says("not_on_home_screen"));
    CHECK(!has(PKG) && has(ASIDE "/app.pkg"));
    /* the place taken (a half-written install): kept */
    put_file(PKG, FAKE, sizeof FAKE, 16);
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK) > 0 && says("\"state\":\"unsettled\"") && says("place_taken"));
    CHECK(size_of(PKG) == 16 && has(ASIDE "/app.pkg"));
    rm(PKG);
    /* recorded without a package: never moved by us */
    g_appdb = 1;
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK) > 0 && says("unsettled") && says("recorded_without_package"));
    CHECK(!has(PKG) && has(ASIDE "/app.pkg"));
    g_appdb = -1;
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK) > 0 && says("db_unreadable"));
    g_appdb = 0;
    /* the installer may have removed the title's folder: made again */
    {
        char p[1024];
        at(p, sizeof p, "/mnt/ext1/user/app/" ID);
        rmdir(p);
    }
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK) > 0 && says("\"state\":\"rolled_back\"") &&
          says("\"patch\":\"" PATCH "\""));
    CHECK(size_of(PKG) == 4096 && size_of(PATCH) == 1024 && !has(ASIDE));
    CHECK(run("finish", ID, 0) > 0 && says("nothing_pending"));
    CHECK(run("check", ID, 0) > 0 && says("restorable"));
    printf("✓ a failed restore puts the package and its update back, only into a free place, never over a record\n");

    /* ---- prepare, then an install that put it back on the same drive ---- */
    CHECK(run("prepare", ID, 0) > 0 && says("prepared"));
    put_file(PKG, FAKE, sizeof FAKE, 3000); /* the installer's own copy (it may differ in size) */
    g_appdb = 1;
    CHECK(run("finish", ID, 0) > 0 && says("\"state\":\"waiting\"") && says("install_may_be_running"));
    CHECK(has(ASIDE "/app.pkg"));
    /* "Put back" while the console lists the game: nothing moves, nothing is deleted */
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK | TITLE_RESTORE_INSTALL_ENDED) > 0 && says("unsettled") &&
          says("on_home_screen"));
    CHECK(size_of(PKG) == 3000 && has(ASIDE "/app.pkg") && !has(PATCH));
    CHECK(run("finish", ID, TITLE_RESTORE_INSTALL_ENDED) > 0 && says("\"state\":\"restored\"") &&
          says("\"installed\":\"" PKG "\"") && says("\"patch\":\"" PATCH "\""));
    CHECK(size_of(PKG) == 3000 && size_of(PATCH) == 1024 && !has(ASIDE));
    CHECK(run("check", ID, 0) > 0 && says("registered"));
    printf("✓ a restore that worked deletes only the copy aside, after the install ended, and puts the update back\n");

    /* ---- the installer chose the console's own drive ---- */
    fresh();
    CHECK(run("prepare", ID, 0) > 0 && says("prepared"));
    put_file("/user/app/" ID "/app.pkg", FAKE, sizeof FAKE, 4096);
    g_appdb = 1;
    CHECK(run("finish", ID, TITLE_RESTORE_INSTALL_ENDED) > 0 && says("restored") &&
          says("\"installed_where\":\"internal\""));
    CHECK(has("/user/app/" ID "/app.pkg") && !has(ASIDE "/app.pkg") && has(PATCH));
    printf("✓ the game installed on another drive counts as restored\n");

    /* ---- the update already back in its place: kept aside, reported ---- */
    fresh();
    CHECK(run("prepare", ID, 0) > 0 && says("prepared"));
    put_file(PKG, FAKE, sizeof FAKE, 4096);
    put_file(PATCH, FAKE, sizeof FAKE, 77);
    g_appdb = 1;
    CHECK(run("finish", ID, TITLE_RESTORE_INSTALL_ENDED) > 0 && says("restored") &&
          says("\"patch_kept\":\"" ASIDE "/patch.pkg\""));
    CHECK(size_of(PATCH) == 77 && has(ASIDE "/patch.pkg") && has(REC));
    printf("✓ an update never replaces a file in its place; the record stays while one is kept aside\n");

    /* ---- internal drive: the aside folder is on /user ---- */
    fresh();
    rm(PKG);
    rm(PATCH);
    put_file("/user/app/" ID "/app.pkg", FAKE, sizeof FAKE, 512);
    CHECK(run("prepare", ID, 0) > 0 && says("prepared") && says("\"where\":\"internal\"") &&
          says("\"install_path\":\"/user/ps5upload/restore/" ID "/app.pkg\""));
    CHECK(!has("/user/app/" ID "/app.pkg") && size_of("/user/ps5upload/restore/" ID "/app.pkg") == 512);
    CHECK(run("finish", ID, TITLE_RESTORE_ROLLBACK) > 0 && says("rolled_back"));
    CHECK(size_of("/user/app/" ID "/app.pkg") == 512);
    printf("✓ the internal drive's package moves within /user and back\n");

    /* ---- a record without a moved package (interrupted before the move) is cleared ---- */
    fresh();
    put_file(REC, (const unsigned char *)"app=1\npatch=0\nbackup=\n", 22, 22);
    CHECK(run("finish", ID, 0) > 0 && says("nothing_pending"));
    CHECK(!has(REC) && has(PKG));
    CHECK(run("finish", ID, 0) > 0 && says("nothing_pending"));
    CHECK(run("bogus", ID, 0) > 0 && says("bad_request"));
    printf("✓ an interrupted prepare leaves nothing to undo\n");

    /* ---- a reply that does not fit is -1, not a cut JSON ---- */
    {
        char small[80];
        CHECK(title_restore_run(&env, "check", ID, 0, "x", small, sizeof small) == -1);
    }

    rm_tree(root);
    if (failures) {
        fprintf(stderr, "title_restore_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("title_restore_selftest: ok\n");
    return 0;
}
