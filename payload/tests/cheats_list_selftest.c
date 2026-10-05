/* Host-side tests for the Cheats title list (final review #8): a large pack, invalid UTF-8
 * names, the reply ceiling, the per-file cache, and titles that have two formats. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>

#include "../src/cheats_list.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ── A strict-enough JSON validator (strings must be valid UTF-8) ── */

static const char *jp;
static int jv_value(void);

static void jv_ws(void) {
    while (*jp == ' ' || *jp == '\n' || *jp == '\t' || *jp == '\r') jp++;
}

static int jv_string(void) {
    if (*jp != '"') return 0;
    jp++;
    while (*jp && *jp != '"') {
        unsigned char c = (unsigned char)*jp;
        if (c < 0x20) return 0;
        if (c == '\\') {
            jp++;
            if (!strchr("\"\\/bfnrtu", *jp) || !*jp) return 0;
            jp++;
        } else if (c < 0x80) {
            jp++;
        } else {
            int n = cheats_utf8_char_len(jp);
            if (!n) return 0;
            jp += n;
        }
    }
    if (*jp != '"') return 0;
    jp++;
    return 1;
}

static int jv_value(void) {
    jv_ws();
    if (*jp == '{') {
        jp++;
        jv_ws();
        if (*jp == '}') { jp++; return 1; }
        for (;;) {
            jv_ws();
            if (!jv_string()) return 0;
            jv_ws();
            if (*jp++ != ':') return 0;
            if (!jv_value()) return 0;
            jv_ws();
            if (*jp == ',') { jp++; continue; }
            if (*jp == '}') { jp++; return 1; }
            return 0;
        }
    }
    if (*jp == '[') {
        jp++;
        jv_ws();
        if (*jp == ']') { jp++; return 1; }
        for (;;) {
            if (!jv_value()) return 0;
            jv_ws();
            if (*jp == ',') { jp++; continue; }
            if (*jp == ']') { jp++; return 1; }
            return 0;
        }
    }
    if (*jp == '"') return jv_string();
    if (!strncmp(jp, "true", 4)) { jp += 4; return 1; }
    if (!strncmp(jp, "false", 5)) { jp += 5; return 1; }
    if ((*jp >= '0' && *jp <= '9') || *jp == '-') {
        while ((*jp >= '0' && *jp <= '9') || *jp == '-' || *jp == '.') jp++;
        return 1;
    }
    return 0;
}

static int json_valid(const char *s) {
    jp = s;
    if (!jv_value()) return 0;
    jv_ws();
    return *jp == '\0';
}

static int count_of(const char *hay, const char *needle) {
    int n = 0;
    for (const char *p = hay; (p = strstr(p, needle)); p += strlen(needle)) n++;
    return n;
}

/* ── Fixture ── */

static char root[256];
static char dirs[3][300];
static char state_dir[300];
static const char *g_name = "Game";
static int g_mods = 3;

static void put(const char *dir, const char *file, const char *body) {
    char p[600];
    snprintf(p, sizeof p, "%s/%s", dir, file);
    FILE *f = fopen(p, "w");
    if (!f) { perror(p); exit(2); }
    fputs(body, f);
    fclose(f);
}

static int fake_load(const char *path, int format, char *name, size_t cap, int *mods) {
    (void)format;
    /* a file whose body says "bad" does not parse */
    FILE *f = fopen(path, "r");
    char body[64] = "";
    if (f) { if (!fgets(body, sizeof body, f)) body[0] = 0; fclose(f); }
    if (!strncmp(body, "bad", 3)) return -1;
    if (!strncmp(body, "name=", 5)) {
        size_t n = strcspn(body + 5, "\n");
        if (n >= cap) n = cap - 1;
        memcpy(name, body + 5, n);
        name[n] = '\0';
    } else {
        snprintf(name, cap, "%s %s", g_name, strrchr(path, '/') + 1);
    }
    *mods = g_mods;
    return 0;
}

static void setup(void) {
    snprintf(root, sizeof root, "/tmp/ps5up-cheatlist-%d", (int)getpid());
    mkdir(root, 0777);
    const char *sub[3] = {"json", "shn", "mc4"};
    for (int i = 0; i < 3; i++) {
        snprintf(dirs[i], sizeof dirs[i], "%s/%s", root, sub[i]);
        mkdir(dirs[i], 0777);
    }
    snprintf(state_dir, sizeof state_dir, "%s/state", root);
    mkdir(state_dir, 0777);
}

static void wipe(void) {
    char cmd[400];
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", root);
    if (system(cmd) != 0) { /* best effort */ }
}

static void clear_files(void) {
    char cmd[800];
    snprintf(cmd, sizeof cmd, "rm -f '%s'/json/* '%s'/shn/* '%s'/mc4/* '%s'/state/*", root, root, root, root);
    if (system(cmd) != 0) { /* best effort */ }
    cheats_list_cache_reset();
}

static cheats_list_cfg_t cfg(void) {
    cheats_list_cfg_t c;
    memset(&c, 0, sizeof c);
    for (int i = 0; i < 3; i++) c.dirs[i] = dirs[i];
    c.state_dir = state_dir;
    c.load = fake_load;
    return c;
}

int main(void) {
    setup();
    static char buf[256 * 1024];
    size_t w = 0;
    cheats_list_cfg_t c = cfg();

    /* utf-8 */
    CHECK(cheats_utf8_char_len("a") == 1);
    CHECK(cheats_utf8_char_len("\xC3\xA9") == 2);
    CHECK(cheats_utf8_char_len("\xE3\x81\x82") == 3);
    CHECK(cheats_utf8_char_len("\xF0\x9F\x8E\xAE") == 4);
    CHECK(cheats_utf8_char_len("\xE9") == 0);              /* Latin-1 e-acute */
    CHECK(cheats_utf8_char_len("\xC0\x80") == 0);          /* overlong */
    CHECK(cheats_utf8_char_len("\xED\xA0\x80") == 0);      /* surrogate */
    CHECK(cheats_utf8_char_len("\xF4\x90\x80\x80") == 0);  /* > U+10FFFF */
    CHECK(cheats_utf8_char_len("\xE3\x81") == 0);          /* cut mid-character */
    CHECK(cheats_utf8_char_len("") == 0);
    char s[32];
    cheats_utf8_sanitize("Caf\xE9 \xE3\x81", s, sizeof s);
    CHECK(strcmp(s, "Caf? ??") == 0);
    /* never ends in half a character */
    cheats_utf8_sanitize("ab\xE3\x81\x82", s, 5);
    CHECK(strcmp(s, "ab") == 0);

    /* 1000 titles, listed once each, valid JSON, not truncated */
    for (int i = 0; i < 1000; i++) {
        char f[64];
        snprintf(f, sizeof f, "CUSA%05d_01.%02d.json", i, i % 50);
        put(dirs[0], f, "x");
    }
    put(state_dir, "CUSA00007.json", "{\"0\":true,\"2\":true}");
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(w == strlen(buf));
    CHECK(json_valid(buf));
    CHECK(count_of(buf, "\"title_id\"") == 1000);
    CHECK(strstr(buf, "\"truncated\":false") != NULL);
    CHECK(strstr(buf, "\"title_id\":\"CUSA00007\",\"name\":\"Game CUSA00007_01.07.json\","
                      "\"version\":\"01.07\",\"formats\":[\"json\"],\"enabled\":2") != NULL);
    CHECK(strstr(buf, "\"title_id\":\"CUSA00008\"") != NULL);
    CHECK(strstr(buf, "\"title_id\":\"CUSA00008\",\"name\":\"Game CUSA00008_01.08.json\","
                      "\"version\":\"01.08\",\"formats\":[\"json\"],\"enabled\":0") != NULL);

    /* the second call parses nothing; a changed file parses once */
    unsigned long first = cheats_list_cache_loads();
    CHECK(first == 1000);
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(cheats_list_cache_loads() == first);
    put(dirs[0], "CUSA00003_01.03.json", "xx"); /* new size */
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(cheats_list_cache_loads() == first + 1);
    CHECK(count_of(buf, "\"title_id\"") == 1000);

    /* the reply ceiling: a valid truncated list, never broken JSON */
    static char small[64 * 1024];
    CHECK(cheats_list_build(&c, small, sizeof small, &w) == 0);
    CHECK(json_valid(small));
    CHECK(strstr(small, "\"truncated\":true") != NULL);
    int got = count_of(small, "\"title_id\"");
    CHECK(got > 100 && got < 1000);
    CHECK(w < sizeof small);
    for (size_t cap = 256; cap < 2000; cap += 97) { /* tiny buffers too */
        char tiny[2000];
        CHECK(cheats_list_build(&c, tiny, cap, &w) == 0);
        CHECK(json_valid(tiny));
        CHECK(w < cap);
    }
    CHECK(cheats_list_build(&c, small, 100, &w) == -1);

    /* invalid UTF-8 names are replaced, so the document parses */
    clear_files();
    put(dirs[0], "CUSA10001_01.00.json", "name=Caf\xE9 \x82\xA0 \xF0\x9F");
    put(dirs[1], "CUSA10002_01.00.shn", "name=\xE3\x81\x82\xE3\x81 ok");
    put(dirs[2], "CUSA10003_01.00.mc4", "name=quote\" back\\ done");
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(json_valid(buf));
    CHECK(strstr(buf, "\"name\":\"Caf? ?? ??\"") != NULL);
    CHECK(strstr(buf, "\"name\":\"\xE3\x81\x82?? ok\"") != NULL);
    CHECK(strstr(buf, "quote\\\" back\\\\ done") != NULL);

    /* a name that is the title id (or a parse failure) falls back to the title id */
    clear_files();
    c.usable_name = NULL;
    put(dirs[0], "CUSA20001_01.00.json", "bad");
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(json_valid(buf));
    CHECK(strstr(buf, "\"name\":\"CUSA20001\"") != NULL);
    unsigned long l = cheats_list_cache_loads();
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(cheats_list_cache_loads() == l); /* a failed parse is cached too */

    /* more than 256 titles that have two formats: each listed once (the old dedupe table held 256) */
    clear_files();
    for (int i = 0; i < 300; i++) {
        char f[64];
        snprintf(f, sizeof f, "PPSA%05d_01.00.json", i);
        put(dirs[0], f, "x");
        snprintf(f, sizeof f, "PPSA%05d_01.00.shn", i);
        put(dirs[1], f, "x");
    }
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(json_valid(buf));
    CHECK(count_of(buf, "\"title_id\"") == 300);
    CHECK(count_of(buf, "\"formats\":[\"json\",\"shn\"]") == 300);

    /* running game flag */
    c.running = 1;
    c.running_title = "PPSA00005";
    CHECK(cheats_list_build(&c, buf, sizeof buf, &w) == 0);
    CHECK(strstr(buf, "\"title_id\":\"PPSA00005\"") != NULL);
    CHECK(count_of(buf, "\"running\":true") == 1);
    CHECK(strstr(buf, "\"game_running\":true,\"game_title_id\":\"PPSA00005\"") != NULL);

    wipe();
    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_list selftest: ok\n");
    return 0;
}
