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

    if (failures) {
        fprintf(stderr, "user_guard_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("user_guard_selftest: ok\n");
    return 0;
}
