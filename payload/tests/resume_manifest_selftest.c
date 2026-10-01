/* Host-side test for the resume-manifest decision.
 *
 * The case that matters is `reduced_manifest_is_stale`: a payload that stayed
 * up through a dropped folder upload kept the original manifest on resume,
 * so the cursor counted against the old shard numbering and the upload could
 * never be resumed (see resume_manifest.h). The rest pin down when the held
 * manifest, and with it the cursor, must be kept.
 */
#include <stdio.h>
#include <string.h>

#include "../include/resume_manifest.h"

static int failures = 0;

#define CHECK_EQ(got, want)                                                 \
    do {                                                                    \
        int g_ = (got), w_ = (want);                                        \
        if (g_ != w_) {                                                     \
            fprintf(stderr, "FAIL line %d: got %d want %d\n",               \
                    __LINE__, g_, w_);                                      \
            failures++;                                                     \
        }                                                                   \
    } while (0)

/* Same JSON shape the engine sends: the original three-file upload, then the
 * reduced manifest after big.bin promoted, renumbered from shard 1. */
static const char FULL[] =
    "{\"dest_root\":\"/data/homebrew/GAME\",\"file_count\":3,"
    "\"total_bytes\":201326592,\"total_shards\":3,\"files\":["
    "{\"path\":\"/data/homebrew/GAME/big.bin\",\"size\":67108864,"
    "\"shard_start\":1,\"shard_count\":1},"
    "{\"path\":\"/data/homebrew/GAME/mid.bin\",\"size\":67108864,"
    "\"shard_start\":2,\"shard_count\":1},"
    "{\"path\":\"/data/homebrew/GAME/end.bin\",\"size\":67108864,"
    "\"shard_start\":3,\"shard_count\":1}]}";
static const char REDUCED[] =
    "{\"dest_root\":\"/data/homebrew/GAME\",\"file_count\":2,"
    "\"total_bytes\":134217728,\"total_shards\":2,\"files\":["
    "{\"path\":\"/data/homebrew/GAME/mid.bin\",\"size\":67108864,"
    "\"shard_start\":1,\"shard_count\":1},"
    "{\"path\":\"/data/homebrew/GAME/end.bin\",\"size\":67108864,"
    "\"shard_start\":2,\"shard_count\":1}]}";

#define LEN(s) ((uint64_t)(sizeof(s) - 1))

int main(void) {
    char copy[sizeof(FULL)];
    memcpy(copy, FULL, sizeof(FULL));

    /* The engine's retry within one attempt resends the exact same manifest
     * from a separate buffer: keep it, the cursor is valid. */
    CHECK_EQ(resume_manifest_is_stale(FULL, LEN(FULL), copy, LEN(FULL)), 0);

    /* THE REGRESSION. A later attempt reconciled and sent a reduced,
     * renumbered manifest while the payload still held the original. */
    CHECK_EQ(resume_manifest_is_stale(FULL, LEN(FULL), REDUCED, LEN(REDUCED)),
             1);

    /* Same length, different content (one shard_start renumbered). A length
     * check alone would keep the stale cursor. */
    copy[strstr(copy, "\"shard_start\":3") - copy + 14] = '4';
    CHECK_EQ(resume_manifest_is_stale(FULL, LEN(FULL), copy, LEN(FULL)), 1);

    /* Nothing held: a payload restart, which the caller's rebuild handles. */
    CHECK_EQ(resume_manifest_is_stale(NULL, 0, REDUCED, LEN(REDUCED)), 0);

    /* No manifest in the BeginTx: an older client that does not resend it on
     * resume. The held copy is all there is, so keep it. */
    CHECK_EQ(resume_manifest_is_stale(FULL, LEN(FULL), NULL, 0), 0);
    CHECK_EQ(resume_manifest_is_stale(FULL, LEN(FULL), REDUCED, 0), 0);

    if (failures) {
        fprintf(stderr, "resume_manifest_selftest: %d FAILURE(S)\n", failures);
        return 1;
    }
    printf("resume_manifest_selftest: ALL PASS\n");
    return 0;
}
