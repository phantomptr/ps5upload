/* Host tests for reading a JSON cheat file's string fields (cheats_json.h). Build: see the root
 * Makefile test-payload. */
#include <stdio.h>
#include <string.h>

#include "cheats_json.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static int top_is(const char *json, const char *key, const char *want) {
    char out[128];
    if (cj_top_str(json, key, out, sizeof out) != 0) return 0;
    return strcmp(out, want) == 0;
}

static int top_absent(const char *json, const char *key) {
    char out[128] = "x";
    return cj_top_str(json, key, out, sizeof out) == -1 && out[0] == '\0';
}

static void game_name_is_the_files_own(void) {
    /* The report: the game's name after a cheat's. */
    const char *after = "{\"id\":\"PPSA01234\",\"mods\":[{\"name\":\"God mode\",\"type\":\"checkbox\"}],"
                        "\"name\":\"Game\",\"process\":\"eboot.bin\"}";
    CHECK(top_is(after, "name", "Game"));
    CHECK(top_is(after, "id", "PPSA01234"));
    CHECK(top_is(after, "process", "eboot.bin"));

    /* The usual order still reads. */
    const char *before = "{\"name\":\"Game\",\"id\":\"CUSA00001\",\"mods\":[{\"name\":\"Infinite HP\"}]}";
    CHECK(top_is(before, "name", "Game"));

    /* No name of its own: none, rather than a cheat's (the list then shows the title id). */
    const char *none = "{\"id\":\"PPSA01234\",\"mods\":[{\"name\":\"God mode\"}]}";
    CHECK(top_absent(none, "name"));

    /* Nested in anything else that comes before or after it. */
    const char *other = "{\"meta\":{\"name\":\"not this\"},\"notes\":[\"name\",{\"name\":\"nor this\"}],\"name\":\"Game\"}";
    CHECK(top_is(other, "name", "Game"));

    /* Wrapped one level down, before the mods: the old lookup found it, so this one does. */
    const char *wrapped = "{\"game\":{\"name\":\"Game\",\"id\":\"CUSA00002\"},\"mods\":[{\"name\":\"God mode\"}]}";
    CHECK(top_is(wrapped, "name", "Game"));
    CHECK(top_is(wrapped, "id", "CUSA00002"));

    /* A value or a string that merely says "name" is not the key, nor is a bracket inside a string. */
    const char *tricky = "{\"title\":\"name\",\"x\":\"}{\\\"name\\\":\\\"fake\\\"\",\"name\":\"Game\"}";
    CHECK(top_is(tricky, "name", "Game"));
    const char *spaced = " {\r\n  \"name\" : \"Game\"\r\n}\r\n";
    CHECK(top_is(spaced, "name", "Game"));

    /* Not a string: not a name. */
    CHECK(top_absent("{\"name\":12,\"mods\":[{\"name\":\"God mode\"}]}", "name"));
}

static void slips_do_not_lose_it(void) {
    /* A BOM, a trailing comma, a comment-like line: community files have them. */
    CHECK(top_is("\xEF\xBB\xBF{\"name\":\"Game\",\"mods\":[],}", "name", "Game"));
    CHECK(top_is("{\"mods\":[{\"name\":\"x\",},],\"name\":\"Game\",}", "name", "Game"));
    /* Cut off mid-way: what came first is still there, and nothing reads past the end. */
    CHECK(top_is("{\"name\":\"Game\",\"mods\":[{\"name\":\"x", "name", "Game"));
    CHECK(top_absent("{\"mods\":[{\"name\":\"x", "name"));
    CHECK(top_absent("{\"name", "name"));
    CHECK(top_absent("{\"name\"", "name"));
    CHECK(top_absent("", "name"));
    char out[8];
    CHECK(cj_top_str(NULL, "name", out, sizeof out) == -1);
    CHECK(cj_top_str("{}", "name", out, 0) == -1);
}

static void escapes_decode(void) {
    char out[64];
    /* é, あ, 🙂 (a surrogate pair), and the short escapes. */
    CHECK(cj_str("{\"name\":\"Caf\\u00e9\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "Caf\xC3\xA9") == 0);
    CHECK(cj_str("{\"name\":\"\\u3042\\u3043\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "\xE3\x81\x82\xE3\x81\x83") == 0);
    CHECK(cj_str("{\"name\":\"\\uD83D\\uDE42!\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "\xF0\x9F\x99\x82!") == 0);
    CHECK(cj_str("{\"name\":\"a\\\"b\\\\c\\/d\\ne\\tf\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "a\"b\\c/d\ne\tf") == 0);

    /* The damaged kinds: a lone surrogate and \u0000 read as the replacement character, and a bad
     * escape keeps its letter as it always did. */
    CHECK(cj_str("{\"name\":\"a\\uD83Db\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "a\xEF\xBF\xBD" "b") == 0);
    CHECK(cj_str("{\"name\":\"a\\u0000b\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "a\xEF\xBF\xBD" "b") == 0);
    CHECK(cj_str("{\"name\":\"a\\uZZZZ\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "auZZZZ") == 0);
    CHECK(cj_str("{\"name\":\"a\\u12", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "au12") == 0);
    CHECK(cj_str("{\"name\":\"a\\", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "a\\") == 0);
}

static void cuts_at_the_buffer(void) {
    char out[6];
    /* Plain text is cut at the buffer, NUL-terminated. */
    CHECK(cj_str("{\"name\":\"abcdefgh\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "abcde") == 0);
    /* An escaped character that does not fit is left out whole, not half written. */
    CHECK(cj_str("{\"name\":\"abcd\\u3042\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "abcd") == 0);
    CHECK(cj_str("{\"name\":\"abc\\uD83D\\uDE42\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "abc") == 0);
    char one[1];
    CHECK(cj_str("{\"name\":\"abc\"}", "name", one, sizeof one) == 0);
    CHECK(one[0] == '\0');
}

static void any_depth_for_a_mods_own_fields(void) {
    char out[32];
    CHECK(cj_str("{\"type\":\"checkbox\",\"name\":\"God mode\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "God mode") == 0);
    CHECK(cj_str("{\"memory\":[{\"offset\":\"1F\"}]}", "offset", out, sizeof out) == 0);
    CHECK(strcmp(out, "1F") == 0);
    CHECK(cj_str("{\"a\":1}", "offset", out, sizeof out) == -1);
    /* A longer key that ends the same way is not the key. */
    CHECK(cj_str("{\"module_name\":\"x\",\"name\":\"y\"}", "name", out, sizeof out) == 0);
    CHECK(strcmp(out, "y") == 0);
}

int main(void) {
    game_name_is_the_files_own();
    slips_do_not_lose_it();
    escapes_decode();
    cuts_at_the_buffer();
    any_depth_for_a_mods_own_fields();
    if (failures) {
        fprintf(stderr, "cheats_json_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_json_selftest: ok\n");
    return 0;
}
