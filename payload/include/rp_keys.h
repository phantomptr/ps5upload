#ifndef PS5UPLOAD_RP_KEYS_H
#define PS5UPLOAD_RP_KEYS_H

#include <stdint.h>

/* Remote Play registry keys, derived with Sony's entry-number formula.
 *
 * Header-only so the payload and a host-built selftest share one
 * implementation — the same pattern as hw_guard.h and appdb_scan.h.
 * The bases come from ps5-payload-dev's regmgr.h.
 *
 * Why these are pinned by tests: getting a base wrong does not crash. It
 * silently reads or writes a different system setting, and the symptom
 * shows up later as some unrelated console behaviour changing. Tests are
 * the only cheap way to notice.
 *
 * Tests: payload/tests/rp_keys_selftest.c */

#define RP_KEY_SERVICE_ENABLE 1098973184u

/* Sony's SCE_REGMGR_ENT_NUM. Out-of-range slots return the fallback key
 * rather than wrapping onto a valid slot's entry. */
static inline uint32_t rp_ent_num(uint32_t slot, uint32_t max, uint32_t stride,
                                  uint32_t base, uint32_t fallback) {
    if (slot < 1 || slot > max) return fallback;
    return (slot - 1) * stride + base;
}

/* The system-wide "Enable Remote Play" toggle. */
static inline uint32_t rp_key_service_enable(void) {
    return RP_KEY_SERVICE_ENABLE;
}

/* Per-user Remote Play permission.
 *
 * FW 10.00 added "select which users can access your console through
 * Remote Play", and this is the key behind that setting. On a console
 * with several users, pairing can succeed while sessions are refused
 * because this is unset for the account being used. Writing it on older
 * firmware is meaningless, so gate on rp_fw_has_per_user_enable(). */
static inline uint32_t rp_key_user_enable(uint32_t slot) {
    return rp_ent_num(slot, 16, 65536, 125859841u, 127170561u);
}

/* Account type for a user slot: "np" once activated, empty otherwise.
 * Not a Remote Play key as such, but Remote Play readiness depends on it —
 * an unactivated account cannot pair. Base from ps5-payload-dev's offact. */
static inline uint32_t rp_key_account_type(uint32_t slot) {
    return rp_ent_num(slot, 16, 65536, 125874183u, 127184903u);
}

/* Paired-device table: 32 registration records.
 *
 * Only user_id and client_type are exposed here. regist_key and aes_key
 * are pairing secrets — deliberately not surfaced, because nothing in the
 * UI needs them and they should not travel over the wire. */
static inline uint32_t rp_key_regist_user_id(uint32_t slot) {
    return rp_ent_num(slot, 32, 65536, 1090584832u, 1092681984u);
}

static inline uint32_t rp_key_regist_key(uint32_t slot) {
    return rp_ent_num(slot, 32, 65536, 1090585088u, 1092682240u);
}

static inline uint32_t rp_key_regist_client_type(uint32_t slot) {
    return rp_ent_num(slot, 32, 65536, 1090585600u, 1092682752u);
}

/* Does this firmware have per-user Remote Play permissions?
 *
 * Firmware magic is BCD-ish: 9.60 is 0x09600000, 10.00 is 0x10000000,
 * 12.70 is 0x12700000. A magic of 0 means we could not read it, so return
 * 0 and let callers fail closed — writing a key that may not exist on an
 * unidentified firmware is exactly the kind of guess to avoid. */
static inline int rp_fw_has_per_user_enable(unsigned int fw_magic) {
    if (fw_magic == 0u) return 0;
    return fw_magic >= 0x10000000u;
}

#endif
