/* Commit-apply decision for a direct (single-file) transaction.
 *
 * Extracted from runtime.c's COMMIT_TX handler so it can be exercised on the
 * host. The logic is small but it guards a destructive operation, and it got
 * that wrong once in a way that deleted users' uploads:
 *
 *   A successful COMMIT renames <dest>.ps5up2-tmp -> <dest> and then CLEARS
 *   tmp_path. The transaction entry stays in the table with direct_mode still
 *   set, so a second COMMIT for the same tx (a retried commit whose ack was
 *   lost, or the client's "Retry failed") arrived with tmp_path == "". The
 *   handler unlinked dest_root FIRST and only then attempted the rename, so
 *   the repeat commit deleted the file the first commit had correctly
 *   delivered and reported `rename  -> <dest> failed: No such file or
 *   directory` — with an empty source path. Every retry made it worse.
 *
 * The invariant this encodes: NOTHING may touch the destination until a
 * replacement is known to exist.
 */
#ifndef PS5UPLOAD_COMMIT_APPLY_H
#define PS5UPLOAD_COMMIT_APPLY_H

#include <stdint.h>

typedef enum {
    /* A staged tmp file exists: safe to unlink dest and rename over it. */
    COMMIT_APPLY_PUBLISH = 0,
    /* Nothing staged, but the destination is already present at the expected
     * size — an earlier COMMIT published it. Report success, touch nothing. */
    COMMIT_APPLY_ALREADY_DONE = 1,
    /* Nothing staged AND the destination is missing or the wrong size. A real
     * error, but the destination must still be left alone. */
    COMMIT_APPLY_SOURCE_MISSING = 2,
    /* Staged file exists but is not the size the manifest promised — the
     * transfer is incomplete. Keep the destination as it was. */
    COMMIT_APPLY_SIZE_MISMATCH = 3,
} commit_apply_action_t;

/* Pure decision. `total_bytes == 0` means "size not asserted" (the empty-file
 * case), so a size comparison is skipped in both directions. */
static inline commit_apply_action_t commit_apply_decide(int have_staged,
                                                        uint64_t staged_size,
                                                        int dest_exists,
                                                        uint64_t dest_size,
                                                        uint64_t total_bytes) {
    if (!have_staged) {
        if (dest_exists && (total_bytes == 0 || dest_size == total_bytes)) {
            return COMMIT_APPLY_ALREADY_DONE;
        }
        return COMMIT_APPLY_SOURCE_MISSING;
    }
    if (total_bytes > 0 && staged_size != total_bytes) {
        return COMMIT_APPLY_SIZE_MISMATCH;
    }
    return COMMIT_APPLY_PUBLISH;
}

#endif /* PS5UPLOAD_COMMIT_APPLY_H */
