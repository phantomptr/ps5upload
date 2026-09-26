/* The SDK version is a PAIR. Writing one half is what let a title pass the
 * launch gate and then die: its eboot still declared PS4:12090001 on a
 * FW 9.60 console. */
#include <stdio.h>
#include "../include/sdk_pairs.h"

static int failures = 0;
#define CHECK(expr)                                                    \
    do {                                                               \
        if (!(expr)) {                                                 \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);    \
            failures++;                                                \
        }                                                              \
    } while (0)

int main(void) {
    sdk_pair_t p;

    /* The pair every working backport on the test console carries. */
    CHECK(sdk_pair_lookup(0x04000031u, &p) == 1);
    CHECK(p.ps5 == 0x04000031u);
    CHECK(p.ps4 == 0x09040001u);

    /* The FW 10 pair — exactly what an un-backported title ships with. */
    CHECK(sdk_pair_lookup(0x10000040u, &p) == 1);
    CHECK(p.ps4 == 0x12090001u);

    /* ps5upload's own UI produced 0x09600000 for "9.60", which is not a
     * real SDK version. It must still resolve to the FW 9 pair rather
     * than being written verbatim. */
    CHECK(sdk_pair_lookup(0x09600000u, &p) == 1);
    CHECK(p.ps5 == 0x09000040u);
    CHECK(p.ps4 == 0x11590001u);

    /* Same for a bare major with no build number. */
    CHECK(sdk_pair_lookup(0x04000000u, &p) == 1);
    CHECK(p.ps5 == 0x04000031u);

    /* Unknown firmware: refuse. Writing a guess would put half a pair on
     * disk, which is the failure mode this table exists to prevent. */
    CHECK(sdk_pair_lookup(0x99000000u, &p) == 0);
    CHECK(sdk_pair_lookup(0x00000000u, &p) == 0);
    CHECK(sdk_pair_lookup(0x04000031u, NULL) == 0);

    /* Every entry must round-trip, and no two may share a major — the
     * fallback lookup would be ambiguous. */
    for (int i = 0; i < SDK_PAIRS_COUNT; i++) {
        sdk_pair_t got;
        CHECK(sdk_pair_lookup(SDK_PAIRS[i].ps5, &got) == 1);
        CHECK(got.ps4 == SDK_PAIRS[i].ps4);
        for (int j = i + 1; j < SDK_PAIRS_COUNT; j++) {
            CHECK(((SDK_PAIRS[i].ps5 >> 24) & 0xff) !=
                  ((SDK_PAIRS[j].ps5 >> 24) & 0xff));
        }
    }

    printf("sdk_pairs_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
