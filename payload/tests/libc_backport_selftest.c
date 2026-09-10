/* The libc.prx edit some titles need before they will start on a downgraded
 * firmware. Same length in and out, so it must never move anything. */
#include <stdio.h>
#include <string.h>
#include "../include/libc_backport.h"

static int failures = 0;
#define CHECK(expr)                                                    \
    do {                                                               \
        if (!(expr)) {                                                 \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);    \
            failures++;                                                \
        }                                                              \
    } while (0)

int main(void) {
    /* The find/replace strings are the same length. If that ever stops being
     * true the in-place edit would corrupt every offset after it. */
    CHECK(strlen(LIBC_BACKPORT_FIND) == LIBC_BACKPORT_LEN);
    CHECK(strlen(LIBC_BACKPORT_REPLACE) == LIBC_BACKPORT_LEN);

    /* One occurrence, surrounded by other data that must survive. */
    {
        unsigned char b[64];
        memset(b, 0xAA, sizeof(b));
        memcpy(b + 10, LIBC_BACKPORT_FIND, LIBC_BACKPORT_LEN);
        CHECK(libc_backport_apply(b, sizeof(b)) == 1);
        CHECK(memcmp(b + 10, LIBC_BACKPORT_REPLACE, LIBC_BACKPORT_LEN) == 0);
        for (size_t i = 0; i < 10; i++) CHECK(b[i] == 0xAA);
        for (size_t i = 10 + LIBC_BACKPORT_LEN; i < sizeof(b); i++) CHECK(b[i] == 0xAA);
        CHECK(libc_backport_is_applied(b, sizeof(b)) == 1);
    }

    /* Idempotent: running it again finds nothing left to do. */
    {
        unsigned char b[64];
        memset(b, 0, sizeof(b));
        memcpy(b + 4, LIBC_BACKPORT_FIND, LIBC_BACKPORT_LEN);
        CHECK(libc_backport_apply(b, sizeof(b)) == 1);
        CHECK(libc_backport_apply(b, sizeof(b)) == 0);
    }

    /* Several occurrences, back to back — the cursor advance must not skip
     * one or re-scan inside what it just wrote. */
    {
        unsigned char b[LIBC_BACKPORT_LEN * 3];
        for (int i = 0; i < 3; i++)
            memcpy(b + i * LIBC_BACKPORT_LEN, LIBC_BACKPORT_FIND, LIBC_BACKPORT_LEN);
        CHECK(libc_backport_apply(b, sizeof(b)) == 3);
        for (int i = 0; i < 3; i++)
            CHECK(memcmp(b + i * LIBC_BACKPORT_LEN, LIBC_BACKPORT_REPLACE,
                         LIBC_BACKPORT_LEN) == 0);
    }

    /* A libc that does not carry the symbol is left completely alone — that
     * is the common case (three of the test console's five titles). */
    {
        unsigned char b[64];
        memset(b, 0x5A, sizeof(b));
        unsigned char before[64];
        memcpy(before, b, sizeof(b));
        CHECK(libc_backport_apply(b, sizeof(b)) == 0);
        CHECK(memcmp(b, before, sizeof(b)) == 0);
        CHECK(libc_backport_is_applied(b, sizeof(b)) == 0);
    }

    /* A truncated match at the very end must not be read past. */
    {
        unsigned char b[LIBC_BACKPORT_LEN];
        memcpy(b, LIBC_BACKPORT_FIND, LIBC_BACKPORT_LEN - 1);
        b[LIBC_BACKPORT_LEN - 1] = 0;
        CHECK(libc_backport_apply(b, LIBC_BACKPORT_LEN - 1) == 0);
    }

    CHECK(libc_backport_apply(NULL, 100) == 0);
    CHECK(libc_backport_is_applied(NULL, 100) == 0);

    printf("libc_backport_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
