/* Host-side tests for Remote Play registry key derivation.
 *
 * This is plain arithmetic, but it is arithmetic that reads and writes the
 * console's registry. A wrong base does not crash — it silently touches a
 * DIFFERENT system setting, which is far worse than a crash and much
 * harder to notice. The expected values come from Sony's own
 * SCE_REGMGR_ENT_NUM macro as published in ps5-payload-dev's regmgr.h. */
#include <stdio.h>

#include "../include/rp_keys.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

int main(void) {
    /* Global service toggle. */
    CHECK(rp_key_service_enable() == 1098973184u);

    /* Per-user enable: ENT_NUM(slot, 16, 65536, 125859841, 127170561). */
    CHECK(rp_key_user_enable(1) == 125859841u);
    CHECK(rp_key_user_enable(2) == 125859841u + 65536u);
    CHECK(rp_key_user_enable(16) == 125859841u + 15u * 65536u);
    /* Out of range falls back, exactly like Sony's macro — it must never
     * wrap around onto a valid slot's key. */
    CHECK(rp_key_user_enable(0) == 127170561u);
    CHECK(rp_key_user_enable(17) == 127170561u);

    /* Account type, from offact's table. */
    CHECK(rp_key_account_type(1) == 125874183u);
    CHECK(rp_key_account_type(2) == 125874183u + 65536u);
    CHECK(rp_key_account_type(17) == 127184903u);

    /* Registration table, 32 slots. */
    CHECK(rp_key_regist_user_id(1) == 1090584832u);
    CHECK(rp_key_regist_user_id(32) == 1090584832u + 31u * 65536u);
    CHECK(rp_key_regist_user_id(33) == 1092681984u);
    CHECK(rp_key_regist_key(1) == 1090585088u);
    CHECK(rp_key_regist_client_type(1) == 1090585600u);

    /* The four registration sub-tables must stay distinct: they are 256
     * bytes apart, so an off-by-one base would read the neighbouring
     * field and quietly report a pairing secret as a user id. */
    CHECK(rp_key_regist_user_id(5) != rp_key_regist_key(5));
    CHECK(rp_key_regist_key(5) != rp_key_regist_client_type(5));

    /* Firmware gating: the per-user toggle arrived in 10.00. */
    CHECK(!rp_fw_has_per_user_enable(0x09600000u)); /* 9.60  */
    CHECK(!rp_fw_has_per_user_enable(0x05100000u)); /* 5.10  */
    CHECK(rp_fw_has_per_user_enable(0x10000000u));  /* 10.00 */
    CHECK(rp_fw_has_per_user_enable(0x10010000u));  /* 10.01 */
    CHECK(rp_fw_has_per_user_enable(0x12700000u));  /* 12.70 */
    /* Unknown firmware must fail CLOSED. Guessing that a write is safe on
     * a firmware we cannot identify is how you set a key that does not
     * exist there. */
    CHECK(!rp_fw_has_per_user_enable(0u));

    printf("rp_keys_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
