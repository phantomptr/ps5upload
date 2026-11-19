/* Host test for when a console user may be deleted (user_guard.h). Build: see the root Makefile
 * test-payload. */
#include <stdio.h>
#include <string.h>

#include "user_guard.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static int is(const char *got, const char *want) {
    if (!got || !want) return got == want;
    return strcmp(got, want) == 0;
}

int main(void) {
    const int ids[4] = {111, 222, -1, -1};
    const int none[4] = {-1, -1, -1, -1};

    /* The user in front of the console. */
    CHECK(is(user_delete_refusal(111, 0, 111, 0, ids, 4), "user_signed_in"));
    /* Signed in, but someone else is in the foreground. */
    CHECK(is(user_delete_refusal(222, 0, 111, 0, ids, 4), "user_signed_in"));
    /* Signed in, nobody in the foreground (that call fails then). */
    CHECK(is(user_delete_refusal(222, -1, -1, 0, ids, 4), "user_signed_in"));
    /* Not signed in: this is who the delete is for. */
    CHECK(is(user_delete_refusal(333, 0, 111, 0, ids, 4), NULL));
    CHECK(is(user_delete_refusal(333, -1, -1, 0, none, 4), NULL));
    /* The foreground call failing leaves a stale value that means nothing. */
    CHECK(is(user_delete_refusal(333, -1, 333, 0, ids, 4), NULL));
    /* The list cannot be read: no telling, and a delete cannot be undone. */
    CHECK(is(user_delete_refusal(333, 0, 111, -1, none, 4), "user_state_unknown"));
    /* But the foreground user is known to be signed in even then. */
    CHECK(is(user_delete_refusal(111, 0, 111, -1, none, 4), "user_signed_in"));

    /* User ids read without loss: unsigned, or the same bits signed as an engine's i32 holds them. */
    {
        uint32_t u = 7;
        CHECK(user_id_parse("2415919104}", &u) == 1 && u == 0x90000000u);
        CHECK(user_id_parse("-1879048192,", &u) == 1 && u == 0x90000000u); /* the same user, signed */
        CHECK(user_id_parse(" 395971800", &u) == 1 && u == 0x179a0cd8u);
        CHECK(user_id_parse("4294967295", &u) == 1 && u == 0xFFFFFFFFu);
        CHECK(user_id_parse("-2147483648", &u) == 1 && u == 0x80000000u);
        CHECK(user_id_parse("0", &u) == 1 && u == 0);
        u = 7;
        /* Never wrapped onto another user: 2^32 + 5 is not user 5. */
        CHECK(user_id_parse("4294967301", &u) == 0 && u == 7);
        CHECK(user_id_parse("-2147483649", &u) == 0);
        CHECK(user_id_parse("12.5", &u) == 0 && user_id_parse("1e9", &u) == 0);
        CHECK(user_id_parse("\"5\"", &u) == 0 && user_id_parse("", &u) == 0 && user_id_parse("-", &u) == 0);
        CHECK(user_id_parse(NULL, &u) == 0);
        /* What the helper writes for an older engine's i32, and back. */
        CHECK(user_id_as_i32(0x90000000u) == -1879048192LL && user_id_as_i32(0x179a0cd8u) == 395971800LL);
        CHECK(user_id_as_i32(0x7FFFFFFFu) == 2147483647LL && user_id_as_i32(0x80000000u) == -2147483648LL);
        /* Two users that differ only in the top bit stay two (the old mask made them one). */
        CHECK(user_id_as_i32(0x90000001u) != user_id_as_i32(0x10000001u));
        /* The delete's own check: a user above 0x7fffffff signed in is refused like any other. */
        const int hi[2] = {(int)user_id_as_i32(0x90000000u), -1};
        CHECK(is(user_delete_refusal((int)user_id_as_i32(0x90000000u), -1, -1, 0, hi, 2), "user_signed_in"));
    }

    if (failures) {
        fprintf(stderr, "user_guard_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("user_guard_selftest: ok\n");
    return 0;
}
