/* Gating for the experimental full-ucred-escalation patch-install path
 * (bgft.c). Pure and header-only so it can be unit-tested on the host without
 * the Sony/kernel dependencies bgft.c pulls in.
 *
 * The escalation itself (snapshot -> root+sandbox-escape+caps -> install ->
 * restore) lives in bgft.c because it is all kernel calls; only the DECISION
 * to attempt it is testable, and that is here. */
#ifndef BGFT_ESCALATE_H
#define BGFT_ESCALATE_H

#include <stddef.h>

/* Attempt the full escalation only when BOTH hold:
 *   - firmware major >= 11 (the only firmware where the main payload's
 *     authid-only swap is known to be insufficient for a PS4 patch; below it
 *     the existing path already works, so do not perturb it), and
 *   - the operator opted in with PS5UPLOAD_FULL_ESCALATE=1.
 *
 * Default OFF: a NULL/absent/any-other env value returns 0, so shipped
 * behaviour is unchanged until a hardware session enables it. */
static inline int bgft_full_escalate_enabled(int fw_major, const char *env_val) {
    if (fw_major < 11) return 0;
    return env_val != NULL && env_val[0] == '1' && env_val[1] == '\0';
}

#endif /* BGFT_ESCALATE_H */
