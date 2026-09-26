/* Host self-test for the installer hint table (pure). */
#include <stdio.h>
#include <string.h>
#include <stdint.h>

#include "../installer/hints.h"

static int failures = 0;
#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

int main(void) {
    /* Known patch-relevant codes get a non-empty, branding-free hint. */
    const char *h;
    h = inst_install_hint(0x80A30004u); /* base not installed */
    CHECK(h != NULL && strstr(h, "base") != NULL);
    h = inst_install_hint(0x80A30002u); /* no space */
    CHECK(h != NULL && strstr(h, "space") != NULL);
    h = inst_install_hint(0x80A3000Cu); /* game running */
    CHECK(h != NULL);
    h = inst_install_hint(0x80A3000Du); /* firmware too old */
    CHECK(h != NULL);
    h = inst_install_hint(0x80A3000Fu); /* patch != base */
    CHECK(h != NULL);
    h = inst_install_hint(0x80A30011u); /* wrong base version */
    CHECK(h != NULL);
    h = inst_install_hint(0x80A30019u); /* invalid patch pkg */
    CHECK(h != NULL);
    /* An unknown code has no hint. */
    CHECK(inst_install_hint(0x80B2150Fu) == NULL);
    CHECK(inst_install_hint(0u) == NULL);

    /* No leftover branding in any returned hint. */
    for (uint32_t c = 0x80A30000u; c <= 0x80A30020u; c++) {
        const char *s = inst_install_hint(c);
        if (!s) continue;
        CHECK(strstr(s, "ezremote") == NULL);
        CHECK(strstr(s, "etaHEN") == NULL);
        CHECK(strstr(s, "onionHEN") == NULL);
    }
    printf("installer_hints_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
