/* Host-side tests for the console page's cheat download store (webui_cheats_store.c): the
 * /api/cheats/store query and file name are checked strictly (a plain base name with the format's
 * extension, never a path), the target folders are made when missing, and the bytes land through
 * wf_receive_file as a same-folder temp + rename: a cut-off body leaves the old file and no temp.
 * The appinfo reply reader prefers CONTENT_VERSION, else the higher of VERSION and APP_VER (#934).
 * File tests run in a fresh mkdtemp directory. Build: see the root Makefile test-payload. */
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include "appinfo_version.h"
#include "webui_cheats_store.h"
#include "webui_files.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static void formats(void) {
    CHECK(webui_cheat_format_ok("json"));
    CHECK(webui_cheat_format_ok("shn"));
    CHECK(webui_cheat_format_ok("mc4"));
    CHECK(!webui_cheat_format_ok("JSON"));
    CHECK(!webui_cheat_format_ok("txt"));
    CHECK(!webui_cheat_format_ok(""));
    CHECK(!webui_cheat_format_ok(NULL));
}

static void names(void) {
    char longname[WEBUI_CHEAT_NAME_MAX + 8];
    CHECK(webui_cheat_name_ok("json", "CUSA15438_01.00.json"));
    CHECK(webui_cheat_name_ok("json", "CUSA09193_01.05_2.json"));
    CHECK(webui_cheat_name_ok("shn", "PPSA01650_01.000.016.shn"));
    CHECK(webui_cheat_name_ok("mc4", "CUSA00002_01.00_c20ae0e8.MC4"));
    CHECK(webui_cheat_name_ok("json", "a-b.json"));
    /* the extension must be the format's */
    CHECK(!webui_cheat_name_ok("json", "CUSA15438_01.00.shn"));
    CHECK(!webui_cheat_name_ok("shn", "CUSA15438_01.00.json"));
    CHECK(!webui_cheat_name_ok("json", "CUSA15438json"));
    CHECK(!webui_cheat_name_ok("json", ".json"));
    CHECK(!webui_cheat_name_ok("json", "json"));
    /* never a path, a hidden file, or a byte outside the set */
    CHECK(!webui_cheat_name_ok("json", "../x.json"));
    CHECK(!webui_cheat_name_ok("json", "a/b.json"));
    CHECK(!webui_cheat_name_ok("json", "a\\b.json"));
    CHECK(!webui_cheat_name_ok("json", "a..b.json"));
    CHECK(!webui_cheat_name_ok("json", ".ps5u-0011.part.json"));
    CHECK(!webui_cheat_name_ok("json", "a b.json"));
    CHECK(!webui_cheat_name_ok("json", "a%2Fb.json"));
    CHECK(!webui_cheat_name_ok("json", "\xc3\xa9.json"));
    CHECK(!webui_cheat_name_ok("json", ""));
    CHECK(!webui_cheat_name_ok("json", NULL));
    CHECK(!webui_cheat_name_ok("txt", "a.txt"));
    /* 128 bytes is the longest */
    memset(longname, 'a', sizeof longname);
    memcpy(longname + WEBUI_CHEAT_NAME_MAX - 5, ".json", 6);
    CHECK(strlen(longname) == WEBUI_CHEAT_NAME_MAX);
    CHECK(webui_cheat_name_ok("json", longname));
    memset(longname, 'a', sizeof longname);
    memcpy(longname + WEBUI_CHEAT_NAME_MAX - 4, ".json", 6);
    CHECK(!webui_cheat_name_ok("json", longname));
}

static void queries(void) {
    char fmt[WEBUI_CHEAT_FMT_CAP], name[WEBUI_CHEAT_NAME_MAX + 1];
    CHECK(webui_cheat_store_query("format=json&name=CUSA15438_01.00.json", fmt, name) == NULL);
    CHECK(strcmp(fmt, "json") == 0 && strcmp(name, "CUSA15438_01.00.json") == 0);
    CHECK(webui_cheat_store_query("name=X1234_1.0.shn&format=shn&v=2", fmt, name) == NULL);
    CHECK(strcmp(fmt, "shn") == 0 && strcmp(name, "X1234_1.0.shn") == 0);
    CHECK(strcmp(webui_cheat_store_query("", fmt, name), "bad_format") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=txt&name=a.txt", fmt, name), "bad_format") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json", fmt, name), "bad_name") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json&name=", fmt, name), "bad_name") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json&name=..%2Fx.json", fmt, name), "bad_name") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json&name=a.json&name=b.json", fmt, name), "bad_name") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json&format=shn&name=a.json", fmt, name), "bad_format") == 0);
    CHECK(strcmp(webui_cheat_store_query("format=json&name=a.shn", fmt, name), "bad_name") == 0);
    CHECK(strcmp(webui_cheat_store_query("formatx=json&name=a.json", fmt, name), "bad_format") == 0);
}

static int count_entries(const char *dir) {
    DIR *d = opendir(dir);
    struct dirent *e;
    int n = 0;
    if (!d) return -1;
    while ((e = readdir(d)) != NULL)
        if (e->d_name[0] != '.' || (strcmp(e->d_name, ".") != 0 && strcmp(e->d_name, "..") != 0)) n++;
    closedir(d);
    return n;
}

static int file_is(const char *path, const char *want) {
    char buf[256];
    FILE *f = fopen(path, "rb");
    size_t n;
    if (!f) return 0;
    n = fread(buf, 1, sizeof buf, f);
    fclose(f);
    return n == strlen(want) && memcmp(buf, want, n) == 0;
}

/* `body` (split in two: the part that came with the headers, then the socket) into `target`; `send`
 * bytes of it actually sent before the peer closes. */
static int receive(const char *target, const char *body, size_t send, size_t declared) {
    int sv[2], rc;
    uint8_t buf[64];
    size_t pre = send < 4 ? send : 4;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return -100;
    if (send > pre && write(sv[1], body + pre, send - pre) != (ssize_t)(send - pre)) return -101;
    close(sv[1]);
    rc = wf_receive_file(sv[0], (const uint8_t *)body, pre, declared, target, 1, 0777, 2000, NULL, buf, sizeof buf);
    close(sv[0]);
    return rc;
}

static void store(void) {
    char tmpl[] = "/tmp/ps5u-cheats-store-XXXXXX";
    char root[256], target[512], dir[512];
    const char *root_dir = mkdtemp(tmpl);
    const char *v1 = "{\"name\":\"Game\",\"mods\":[]}";
    const char *v2 = "{\"name\":\"Game v2\",\"mods\":[{\"name\":\"Inf HP\"}]}";
    struct stat st;
    CHECK(root_dir != NULL);
    if (!root_dir) return;
    snprintf(root, sizeof root, "%s/cheats", root_dir);
    snprintf(dir, sizeof dir, "%s/json", root);

    /* refused names never make a folder or a path */
    CHECK(webui_cheat_target(root, "json", "../x.json", target, sizeof target) == -1);
    CHECK(stat(root, &st) != 0);
    CHECK(webui_cheat_target(root, "txt", "x.txt", target, sizeof target) == -1);
    CHECK(webui_cheat_target(root, "json", "x.json", target, 8) == -1);

    /* the root and the format folder are made when missing */
    CHECK(webui_cheat_target(root, "json", "CUSA15438_01.00.json", target, sizeof target) == 0);
    CHECK(stat(dir, &st) == 0 && S_ISDIR(st.st_mode));
    CHECK(strcmp(target + strlen(root), "/json/CUSA15438_01.00.json") == 0);
    /* and found again when there */
    CHECK(webui_cheat_target(root, "json", "CUSA15438_01.00.json", target, sizeof target) == 0);

    /* a full body lands as the file, nothing else left in the folder */
    CHECK(receive(target, v1, strlen(v1), strlen(v1)) == WF_OK);
    CHECK(file_is(target, v1));
    CHECK(count_entries(dir) == 1);

    /* the same name again replaces it */
    CHECK(receive(target, v2, strlen(v2), strlen(v2)) == WF_OK);
    CHECK(file_is(target, v2));
    CHECK(count_entries(dir) == 1);

    /* a body cut short keeps the old file and leaves no temp */
    CHECK(receive(target, v1, 10, strlen(v1)) == WF_E_SHORT);
    CHECK(file_is(target, v2));
    CHECK(count_entries(dir) == 1);

    /* a folder in the file's place is never replaced */
    {
        char other[512];
        CHECK(webui_cheat_target(root, "shn", "CUSA1_1.0.shn", other, sizeof other) == 0);
        CHECK(mkdir(other, 0777) == 0);
        CHECK(receive(other, v1, strlen(v1), strlen(v1)) == WF_E_NOT_FILE);
        rmdir(other);
    }

    /* a link in the format folder's place is refused */
    {
        char mc4[512], t2[512];
        snprintf(mc4, sizeof mc4, "%s/mc4", root);
        CHECK(symlink(root_dir, mc4) == 0);
        CHECK(webui_cheat_target(root, "mc4", "CUSA1_1.0.mc4", t2, sizeof t2) == -1);
        unlink(mc4);
    }

    {
        char cmd[600];
        snprintf(cmd, sizeof cmd, "rm -rf '%s'", root_dir);
        CHECK(system(cmd) == 0);
    }
}

static void versions(void) {
    char v[32];
    const char *ps5 =
        "{\"ok\":true,\"title_id\":\"PPSA01650\",\"rows\":[{\"key\":\"APP_VER\",\"val\":\"01.00\"},"
        "{\"key\":\"CONTENT_VERSION\",\"val\":\"01.000.016\"}],\"source\":\"x\"}";
    const char *ps4 = "{\"ok\":true,\"rows\":[{\"key\":\"APP_VER\",\"val\":\" 01.05 \"}]}";
    const char *none = "{\"ok\":true,\"rows\":[]}";
    const char *junk = "{\"ok\":true,\"rows\":[{\"key\":\"CONTENT_VERSION\",\"val\":\"abc\"},{\"key\":\"APP_VER\",\"val\":\"1.\\\"}\"}]}";
    const char *nested = "{\"ok\":true,\"rows\":[{\"key\":\"X\",\"val\":\"{]}\"},{\"key\":\"APP_VER\",\"val\":\"02.10\"}]}";
    CHECK(webui_cheat_appinfo_version(ps5, strlen(ps5), v, sizeof v) == 1 && strcmp(v, "01.000.016") == 0);
    CHECK(webui_cheat_appinfo_version(ps4, strlen(ps4), v, sizeof v) == 1 && strcmp(v, "01.05") == 0);
    CHECK(webui_cheat_appinfo_version(none, strlen(none), v, sizeof v) == 0 && v[0] == '\0');
    CHECK(webui_cheat_appinfo_version(junk, strlen(junk), v, sizeof v) == 0);
    CHECK(webui_cheat_appinfo_version(nested, strlen(nested), v, sizeof v) == 1 && strcmp(v, "02.10") == 0);
    CHECK(webui_cheat_appinfo_version("{\"ok\":false,\"error\":\"x\"}", 25, v, sizeof v) == 0);
    CHECK(webui_cheat_appinfo_version(ps5, strlen(ps5), v, 4) == 0); /* does not fit */

    /* #934: an updated PS4 game keeps APP_VER at the base's 01.00 and says its update in VERSION. */
    const char *rdr2 = "{\"ok\":true,\"title_id\":\"CUSA03041\",\"rows\":[{\"key\":\"APP_VER\",\"val\":\"01.00\"},"
                       "{\"key\":\"VERSION\",\"val\":\"01.32\"}],\"source\":\"sqlite\",\"error\":null}";
    const char *app_higher = "{\"ok\":true,\"rows\":[{\"key\":\"VERSION\",\"val\":\"01.00\"},{\"key\":\"APP_VER\",\"val\":\"01.09\"}]}";
    const char *only_version = "{\"ok\":true,\"rows\":[{\"key\":\"VERSION\",\"val\":\"01.26\"}]}";
    const char *ps5_with_version = "{\"ok\":true,\"rows\":[{\"key\":\"VERSION\",\"val\":\"01.32\"},"
                                   "{\"key\":\"CONTENT_VERSION\",\"val\":\"01.500.000\"}]}";
    CHECK(webui_cheat_appinfo_version(rdr2, strlen(rdr2), v, sizeof v) == 1 && strcmp(v, "01.32") == 0);
    CHECK(webui_cheat_appinfo_version(app_higher, strlen(app_higher), v, sizeof v) == 1 && strcmp(v, "01.09") == 0);
    CHECK(webui_cheat_appinfo_version(only_version, strlen(only_version), v, sizeof v) == 1 && strcmp(v, "01.26") == 0);
    CHECK(webui_cheat_appinfo_version(ps5_with_version, strlen(ps5_with_version), v, sizeof v) == 1 &&
          strcmp(v, "01.500.000") == 0);
}

static void version_pick(void) {
    CHECK(strcmp(appinfo_version_pick("01.000.016", "01.32", "01.00"), "01.000.016") == 0);
    CHECK(strcmp(appinfo_version_pick(NULL, "01.32", "01.00"), "01.32") == 0);
    CHECK(strcmp(appinfo_version_pick("", "01.00", "01.09"), "01.09") == 0);
    CHECK(strcmp(appinfo_version_pick(NULL, "01.05", "01.05"), "01.05") == 0);
    CHECK(strcmp(appinfo_version_pick(NULL, "", "01.00"), "01.00") == 0);
    CHECK(strcmp(appinfo_version_pick(NULL, "01.26", NULL), "01.26") == 0);
    CHECK(strcmp(appinfo_version_pick(NULL, NULL, NULL), "") == 0);
    CHECK(appinfo_version_cmp("01.32", "01.05") > 0);
    CHECK(appinfo_version_cmp("1.10", "1.9") > 0);
    CHECK(appinfo_version_cmp("01.05", "1.5") == 0);
    CHECK(appinfo_version_cmp("01.00", "01.00.001") < 0);
    CHECK(appinfo_version_cmp(" 01.05", "01.05") == 0);
    CHECK(appinfo_version_cmp("abc", "01.00") < 0);
    CHECK(appinfo_version_cmp("", "") == 0);
    CHECK(appinfo_version_cmp("99999999999999999999", "1") > 0); /* no overflow */
}

/* A downloaded file's game, and where its saved switches are: always the upper-case title id. */
static void titles(void) {
    char t[10], p[256];
    CHECK(webui_cheat_file_title("CUSA15438_01.00.json", t) && strcmp(t, "CUSA15438") == 0);
    CHECK(webui_cheat_file_title("ppsa01650.shn", t) && strcmp(t, "PPSA01650") == 0);
    CHECK(webui_cheat_file_title("Ppsa01650_2.mc4", t) && strcmp(t, "PPSA01650") == 0);
    CHECK(!webui_cheat_file_title("PPSA0165.json", t) && t[0] == '\0');
    CHECK(!webui_cheat_file_title("PPSA016501.json", t));
    CHECK(!webui_cheat_file_title("1PSA01650.json", t));
    CHECK(!webui_cheat_file_title("PPSA01650-x.json", t));
    CHECK(!webui_cheat_file_title("mygame.json", t));
    CHECK(!webui_cheat_file_title("", t));
    CHECK(webui_cheat_state_path("/data/ps5upload/cheats", "PPSA01650", p, sizeof p) == 0);
    CHECK(strcmp(p, "/data/ps5upload/cheats/state/PPSA01650.json") == 0);
    CHECK(webui_cheat_state_path("/r", "ppsa01650", p, sizeof p) == -1); /* only the upper-case name */
    CHECK(webui_cheat_state_path("/r", "../x/../y", p, sizeof p) == -1);
    CHECK(webui_cheat_state_path("/r", "PPSA01650", p, 10) == -1);
}

int main(void) {
    formats();
    names();
    titles();
    queries();
    store();
    versions();
    version_pick();
    if (failures) {
        fprintf(stderr, "webui_cheats_store_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("webui_cheats_store_selftest: ok\n");
    return 0;
}
