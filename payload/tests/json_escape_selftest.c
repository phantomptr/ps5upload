/* Host tests for json_escape_into, the helper's one way of putting a string into a JSON reply. Build:
 * see the root Makefile test-payload. */
#include <stdio.h>
#include <string.h>

#include "json_escape.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static int escaped(const char *in, const char *want) {
    char out[128];
    size_t n = json_escape_into(in, out, sizeof out);
    return n == strlen(want) && strcmp(out, want) == 0;
}

int main(void) {
    char tiny[4], one[1], big[8];

    CHECK(escaped("plain", "plain"));
    CHECK(escaped("", ""));
    /* What fw.spoof.status could carry: a free-form kernel build string. */
    CHECK(escaped("FreeBSD 11.0 \"releases/13.60\" C:\\build",
                  "FreeBSD 11.0 \\\"releases/13.60\\\" C:\\\\build"));
    CHECK(escaped("line\nbreak\ttab", "line?break?tab"));
    CHECK(escaped("caf\xC3\xA9", "caf\xC3\xA9")); /* UTF-8 passes as it is */

    /* Cut at the buffer, never half an escape, always terminated. */
    CHECK(json_escape_into("abcdef", tiny, sizeof tiny) == 2 && strcmp(tiny, "ab") == 0);
    CHECK(json_escape_into("ab\"", tiny, sizeof tiny) == 2 && strcmp(tiny, "ab") == 0);
    CHECK(json_escape_into("a\"", tiny, sizeof tiny) == 3 && strcmp(tiny, "a\\\"") == 0);
    CHECK(json_escape_into("abc\"", big, sizeof big) == 5 && strcmp(big, "abc\\\"") == 0);
    CHECK(json_escape_into("abc", one, sizeof one) == 0 && one[0] == '\0');
    CHECK(json_escape_into("abc", big, 0) == 0);
    CHECK(json_escape_into(NULL, big, sizeof big) == 0 && big[0] == '\0');
    CHECK(json_escape_into("abc", NULL, 8) == 0);

    /* Text that may not be clean UTF-8: whole characters pass, anything else reads '?'. */
    {
        char u[32];
        char cut[6];
        json_escape_utf8_into("caf\xC3\xA9 \xE3\x83\x87 \xF0\x9F\x98\x80", u, sizeof u);
        CHECK(strcmp(u, "caf\xC3\xA9 \xE3\x83\x87 \xF0\x9F\x98\x80") == 0);
        json_escape_utf8_into("a\xFF" "b\x80" "c\xC3", u, sizeof u);
        CHECK(strcmp(u, "a?b?c?") == 0);
        /* A three-byte character cut by the buffer after its second byte. */
        json_escape_utf8_into("ab\xE3\x83\x87", cut, sizeof cut);
        CHECK(strcmp(cut, "ab??") == 0 || strcmp(cut, "ab?") == 0 || strcmp(cut, "ab") == 0);
        for (const char *p = cut; *p; p++) CHECK((unsigned char)*p < 0x80);
        /* Overlong forms, encoded surrogates and code points past U+10FFFF are not valid UTF-8
         * (serde refuses the whole reply over one): each byte of them reads '?'. */
        json_escape_utf8_into("a\xC0\xAF" "b", u, sizeof u);
        CHECK(strcmp(u, "a??b") == 0);
        json_escape_utf8_into("a\xC1\xBF", u, sizeof u);
        CHECK(strcmp(u, "a??") == 0);
        json_escape_utf8_into("a\xE0\x80\xAF" "b", u, sizeof u); /* overlong '/' */
        CHECK(strcmp(u, "a???b") == 0);
        json_escape_utf8_into("a\xED\xA0\x80" "b", u, sizeof u); /* U+D800 */
        CHECK(strcmp(u, "a???b") == 0);
        json_escape_utf8_into("a\xF0\x8F\xBF\xBF" "b", u, sizeof u); /* overlong U+FFFF */
        CHECK(strcmp(u, "a????b") == 0);
        json_escape_utf8_into("a\xF4\x90\x80\x80" "b", u, sizeof u); /* U+110000 */
        CHECK(strcmp(u, "a????b") == 0);
        json_escape_utf8_into("a\xF5\x80\x80\x80", u, sizeof u);
        CHECK(strcmp(u, "a????") == 0);
        /* The edges of each range still pass. */
        json_escape_utf8_into("\xE0\xA0\x80\xED\x9F\xBF\xF0\x90\x80\x80\xF4\x8F\xBF\xBF", u, sizeof u);
        CHECK(strcmp(u, "\xE0\xA0\x80\xED\x9F\xBF\xF0\x90\x80\x80\xF4\x8F\xBF\xBF") == 0);
    }

    if (failures) {
        fprintf(stderr, "json_escape_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("json_escape_selftest: ok\n");
    return 0;
}
