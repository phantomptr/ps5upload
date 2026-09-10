#ifndef PS5UPLOAD_SDK_PAIRS_H
#define PS5UPLOAD_SDK_PAIRS_H

#include <stdint.h>

/* The SDK version a title declares is a PAIR, not one number.
 *
 * Every param segment carries a PS4 value at +0x10 and a PS5 value at
 * +0x14, and the kernel reports both when a game starts:
 *
 *     [KERNEL] INFO: SDK vesion: PS4:09040001 PPR:04000031
 *
 * ps5upload used to write ONE of them, chosen by segment type — the PS5
 * field for a process param, the PS4 field for a module param. That left
 * the other field at whatever the title shipped with. Measured on a
 * FW 9.60 console: a title patched that way passed the launch gate and
 * then died, because its eboot still declared PS4:12090001 (FW 12.09).
 * Writing both fields cleared every error from the kernel log.
 *
 * The pairings are not derivable — they are a fixed table. These match
 * idlesauce's ps5_elf_sdk_downgrade.py (via Nazky/Auto-Backpork), and
 * were confirmed against working backported titles on hardware, whose
 * eboot AND every sce_module PRX all carry (0x04000031, 0x09040001).
 *
 * Tests: payload/tests/sdk_pairs_selftest.c */

typedef struct {
    uint32_t ps5; /* written at param + 0x14 */
    uint32_t ps4; /* written at param + 0x10 */
} sdk_pair_t;

static const sdk_pair_t SDK_PAIRS[] = {
    {0x01000050u, 0x07590001u}, /* FW 1  */
    {0x02000009u, 0x08050001u}, /* FW 2  */
    {0x03000027u, 0x08540001u}, /* FW 3  */
    {0x04000031u, 0x09040001u}, /* FW 4  — what shipped backports use */
    {0x05000033u, 0x09590001u}, /* FW 5  */
    {0x06000038u, 0x10090001u}, /* FW 6  */
    {0x07000038u, 0x10590001u}, /* FW 7  */
    {0x08000041u, 0x11090001u}, /* FW 8  */
    {0x09000040u, 0x11590001u}, /* FW 9  */
    {0x10000040u, 0x12090001u}, /* FW 10 */
};

#define SDK_PAIRS_COUNT ((int)(sizeof(SDK_PAIRS) / sizeof(SDK_PAIRS[0])))

/* Resolve a requested PS5 SDK value to its pair.
 *
 * Matches exactly first, then falls back to the firmware major in the top
 * byte — so a caller that asks for "9.60" as 0x09600000 still lands on the
 * FW 9 pair rather than writing a value that exists in no Sony table.
 * (ps5upload's own UI produced exactly that number for years.)
 *
 * Returns 1 and fills `out` on success, 0 when the request names no known
 * firmware — in which case the caller must not write anything, because
 * half a pair is what caused the bug this table exists to fix. */
static inline int sdk_pair_lookup(uint32_t requested_ps5, sdk_pair_t *out) {
    if (!out) return 0;
    for (int i = 0; i < SDK_PAIRS_COUNT; i++) {
        if (SDK_PAIRS[i].ps5 == requested_ps5) {
            *out = SDK_PAIRS[i];
            return 1;
        }
    }
    uint32_t major = (requested_ps5 >> 24) & 0xffu;
    for (int i = 0; i < SDK_PAIRS_COUNT; i++) {
        if (((SDK_PAIRS[i].ps5 >> 24) & 0xffu) == major) {
            *out = SDK_PAIRS[i];
            return 1;
        }
    }
    return 0;
}

#endif
