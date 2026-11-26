/* Host tests for utf8_cut.h: a delete or chmod that partly failed names its first path on the
 * status line, cut to the line's room from the end (fs_jobs.c fail_message) and to its buffer from
 * the start (fail_note). Both cuts were byte cuts and could split a character of a Japanese or
 * accented name. Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <string.h>

#include "utf8_cut.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* 1 when `s` (`n` bytes) is whole, well-formed UTF-8 (lead and continuation bytes only; the
 * cuts never make overlong forms, so the shape is what matters here). */
static int whole_utf8(const char *s, size_t n) {
    size_t i = 0;
    while (i < n) {
        unsigned char c = (unsigned char)s[i];
        size_t need = c < 0x80 ? 1 : (c >= 0xC2 && c <= 0xDF) ? 2 : (c >= 0xE0 && c <= 0xEF) ? 3 : (c >= 0xF0 && c <= 0xF4) ? 4 : 0;
        size_t k;
        if (need == 0 || i + need > n) return 0;
        for (k = 1; k < need; k++)
            if (!utf8_is_cont((unsigned char)s[i + k])) return 0;
        i += need;
    }
    return 1;
}

/* fail_message's cut: "..." and the end of `p` that fits `keep` bytes, from a whole character. */
static void shown_tail(const char *p, char *shown, size_t cap) {
    size_t l = strlen(p), keep = cap - 1;
    if (l > keep) {
        size_t from = utf8_tail_start(p, l, keep - 3);
        memcpy(shown, "...", 3);
        memcpy(shown + 3, p + from, l - from + 1);
    } else {
        memcpy(shown, p, l + 1);
    }
}

int main(void) {
    char path[600], shown[110], first[64];
    size_t pad, i, n;

    /* The tail starts on a character for every alignment of 2-, 3- and 4-byte characters. */
    for (pad = 0; pad < 4; pad++) {
        const char *chars[] = {"\xC3\xA9", "\xE6\x97\xA5", "\xF0\x9F\x8E\xAE"};
        for (i = 0; i < 3; i++) {
            size_t k;
            snprintf(path, sizeof path, "/data/%.*s", (int)pad, "xyz");
            for (k = 0; k < 60; k++) strcat(path, chars[i]);
            strcat(path, "/save.dat");
            shown_tail(path, shown, sizeof shown);
            n = strlen(shown);
            CHECK(n <= sizeof shown - 1);
            CHECK(strncmp(shown, "...", 3) == 0);
            CHECK(whole_utf8(shown, n));
            CHECK(strcmp(shown + n - 9, "/save.dat") == 0);
            /* As much as fits: never more than a character's bytes short. */
            CHECK(n + 3 >= sizeof shown - 1);
        }
    }
    /* Short text is kept whole; ASCII is cut exactly. */
    shown_tail("/data/a/b", shown, sizeof shown);
    CHECK(strcmp(shown, "/data/a/b") == 0);
    CHECK(utf8_tail_start("abcdef", 6, 4) == 2);
    CHECK(utf8_tail_start("abc", 3, 0) == 3);
    CHECK(utf8_tail_start("abc", 3, 10) == 0);
    /* "a" + U+00E9: keeping 1 byte would start inside the character, so nothing of it is kept. */
    CHECK(utf8_tail_start("a\xC3\xA9", 3, 1) == 3);
    CHECK(utf8_tail_start("a\xC3\xA9", 3, 2) == 1);

    /* The front cut (snprintf into the first-path buffer) drops a character cut short. */
    for (pad = 0; pad < 4; pad++) {
        size_t k;
        snprintf(path, sizeof path, "/%.*s", (int)pad, "xyz");
        for (k = 0; k < 40; k++) strcat(path, "\xE6\x97\xA5");
        snprintf(first, sizeof first, "%s", path);
        first[utf8_whole_prefix(first, strlen(first))] = '\0';
        n = strlen(first);
        CHECK(whole_utf8(first, n));
        CHECK(n + 3 > sizeof first - 1);
        CHECK(strncmp(first, path, n) == 0);
    }
    CHECK(utf8_whole_prefix("ab", 2) == 2);
    CHECK(utf8_whole_prefix("", 0) == 0);
    CHECK(utf8_whole_prefix("a\xC3\xA9", 3) == 3);
    CHECK(utf8_whole_prefix("a\xC3", 2) == 1);
    CHECK(utf8_whole_prefix("a\xE6\x97", 3) == 1);
    CHECK(utf8_whole_prefix("a\xF0\x9F\x8E", 4) == 1);
    CHECK(utf8_whole_prefix("a\xF0\x9F\x8E\xAE", 5) == 5);
    /* Not UTF-8 to begin with (a stray continuation byte, Latin-1): left as it is. */
    CHECK(utf8_whole_prefix("a\xA9", 2) == 2);
    CHECK(utf8_whole_prefix("\x80\x80\x80\x80\x80", 5) == 5);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("utf8_cut_selftest: ok\n");
    return 0;
}
