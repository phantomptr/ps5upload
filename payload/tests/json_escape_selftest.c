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

    if (failures) {
        fprintf(stderr, "json_escape_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("json_escape_selftest: ok\n");
    return 0;
}
