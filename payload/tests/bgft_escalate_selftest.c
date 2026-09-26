/* The experimental full-ucred-escalation patch-install path (bgft.c) must be
 * default OFF and only ever engage on FW 11+. This pins that gate so a stray
 * default-on or a wrong firmware bound cannot ship silently — the escalation
 * mutates the live payload's credentials, so "when it runs" is the safety
 * contract worth testing even though the escalation itself is kernel-only. */
#include <stdio.h>
#include "../include/bgft_escalate.h"

static int failures = 0;
#define CHECK(expr)                                                    \
    do {                                                               \
        if (!(expr)) {                                                 \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);    \
            failures++;                                                \
        }                                                              \
    } while (0)

int main(void) {
    /* Default OFF: absent env never escalates, on any firmware. */
    CHECK(bgft_full_escalate_enabled(11, NULL) == 0);
    CHECK(bgft_full_escalate_enabled(12, NULL) == 0);

    /* Opt-in only takes effect on FW 11+. */
    CHECK(bgft_full_escalate_enabled(11, "1") == 1);
    CHECK(bgft_full_escalate_enabled(12, "1") == 1);

    /* Below FW 11 the existing path already works — never perturb it, even with
     * the opt-in set. */
    CHECK(bgft_full_escalate_enabled(10, "1") == 0);
    CHECK(bgft_full_escalate_enabled(9, "1") == 0);
    CHECK(bgft_full_escalate_enabled(0, "1") == 0);

    /* Only the exact string "1" enables it — not "10", "1x", "true", or empty,
     * so a truthy-looking value can't turn it on by accident. */
    CHECK(bgft_full_escalate_enabled(11, "0") == 0);
    CHECK(bgft_full_escalate_enabled(11, "") == 0);
    CHECK(bgft_full_escalate_enabled(11, "10") == 0);
    CHECK(bgft_full_escalate_enabled(11, "1x") == 0);
    CHECK(bgft_full_escalate_enabled(11, "true") == 0);

    if (failures) {
        fprintf(stderr, "bgft_escalate_selftest: %d FAILURE(S)\n", failures);
        return 1;
    }
    printf("bgft_escalate_selftest: ALL PASS\n");
    return 0;
}
