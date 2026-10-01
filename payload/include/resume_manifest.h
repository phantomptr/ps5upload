/* Resume-manifest decision for a multi-file transaction.
 *
 * Extracted from runtime.c's BEGIN_TX handler so it can be exercised on the
 * host. The engine reconciles before every cross-attempt resume and sends a
 * REDUCED manifest: the files already promoted to their final path are
 * dropped and the rest are renumbered from shard 1. The resume cursor
 * (shards_received) is a count against whichever manifest the entry holds, so
 * the two must always come from the same manifest.
 *
 * After a payload restart the entry holds no manifest and BEGIN_TX adopts the
 * reduced one. When the payload stayed up through the drop, the entry still
 * held the ORIGINAL manifest and kept it, so the cursor kept counting against
 * the old numbering. Seen on hardware: a 298-file, 2158-shard folder upload
 * dropped at shard 1182 while the payload stayed up. The first resume sent a
 * 159-file manifest; the engine skipped reduced shards 1..1182 it had never
 * sent and COMMIT failed shards_incomplete (1182 of 2158). Every later resume
 * sent a 139-file, 1175-shard manifest and the engine refused it with
 * "last_acked_shard=1182 > total_shards=1175", so the upload could not be
 * resumed at all.
 */
#ifndef PS5UPLOAD_RESUME_MANIFEST_H
#define PS5UPLOAD_RESUME_MANIFEST_H

#include <stdint.h>
#include <string.h>

/* Pure decision: 1 when a resume BEGIN_TX carries a manifest that differs from
 * the one the entry still holds, so the held copy and the cursor counted
 * against it must be replaced. 0 when nothing is held (the restart path
 * rebuilds it), when the client sent no manifest (an older client that does
 * not resend it on resume), or when both are byte-identical (the engine's
 * retry within one attempt, where the cursor is still valid). */
static inline int resume_manifest_is_stale(const char *held,
                                           uint64_t held_len,
                                           const char *incoming,
                                           uint64_t incoming_len) {
    if (!held || !incoming || incoming_len == 0) {
        return 0;
    }
    if (held_len != incoming_len) {
        return 1;
    }
    return memcmp(held, incoming, (size_t)incoming_len) != 0;
}

#endif /* PS5UPLOAD_RESUME_MANIFEST_H */
