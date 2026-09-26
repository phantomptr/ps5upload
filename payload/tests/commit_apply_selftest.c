/* Host-side test for the direct-commit apply decision.
 *
 * Every case here is a real failure mode, not a permutation for its own sake.
 * The one that matters most is `recommit_after_success_is_idempotent`: that
 * exact sequence deleted users' uploads in the field and reported
 * `rename  -> <dest> failed: No such file or directory` (note the empty
 * source), reproduced on hardware before the fix and again after it.
 */
#include <stdio.h>

#include "../include/commit_apply.h"

static int failures = 0;

#define CHECK_EQ(got, want)                                                 \
    do {                                                                    \
        commit_apply_action_t g_ = (got), w_ = (want);                      \
        if (g_ != w_) {                                                     \
            fprintf(stderr, "FAIL line %d: got %d want %d\n",               \
                    __LINE__, (int)g_, (int)w_);                            \
            failures++;                                                     \
        }                                                                   \
    } while (0)

int main(void) {
    const uint64_t SIZE = 50331648; /* the 48 MiB file used in the hw repro */

    /* Normal path: staged tmp is present and the right size. */
    CHECK_EQ(commit_apply_decide(1, SIZE, 0, 0, SIZE), COMMIT_APPLY_PUBLISH);
    /* Publishing over an existing destination is still a publish. */
    CHECK_EQ(commit_apply_decide(1, SIZE, 1, SIZE, SIZE), COMMIT_APPLY_PUBLISH);

    /* THE REGRESSION. Second COMMIT for a tx that already published: no
     * staged file, destination present and correct. Must be idempotent —
     * anything else unlinks a file the user successfully uploaded. */
    CHECK_EQ(commit_apply_decide(0, 0, 1, SIZE, SIZE),
             COMMIT_APPLY_ALREADY_DONE);

    /* Nothing staged and no destination: a real error, but the caller must
     * still not touch dest. */
    CHECK_EQ(commit_apply_decide(0, 0, 0, 0, SIZE),
             COMMIT_APPLY_SOURCE_MISSING);
    /* Nothing staged and the destination is the wrong size — do not claim
     * success just because a file happens to be sitting there. */
    CHECK_EQ(commit_apply_decide(0, 0, 1, SIZE - 1, SIZE),
             COMMIT_APPLY_SOURCE_MISSING);

    /* Truncated staged file: refuse, destination preserved. */
    CHECK_EQ(commit_apply_decide(1, SIZE - 1, 1, SIZE, SIZE),
             COMMIT_APPLY_SIZE_MISMATCH);
    /* Overlong staged file is equally wrong. */
    CHECK_EQ(commit_apply_decide(1, SIZE + 1, 0, 0, SIZE),
             COMMIT_APPLY_SIZE_MISMATCH);

    /* total_bytes == 0 means "size not asserted" (empty-file transfers), so
     * size is not compared in either direction. */
    CHECK_EQ(commit_apply_decide(1, 0, 0, 0, 0), COMMIT_APPLY_PUBLISH);
    CHECK_EQ(commit_apply_decide(0, 0, 1, 0, 0), COMMIT_APPLY_ALREADY_DONE);
    /* A zero-byte destination still counts as published when the manifest
     * asserts no size — but not when it asserts a real one. */
    CHECK_EQ(commit_apply_decide(0, 0, 1, 0, SIZE),
             COMMIT_APPLY_SOURCE_MISSING);

    if (failures) {
        fprintf(stderr, "commit_apply_selftest: %d FAILURE(S)\n", failures);
        return 1;
    }
    printf("commit_apply_selftest: ALL PASS\n");
    return 0;
}
