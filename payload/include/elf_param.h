#ifndef PS5UPLOAD_ELF_PARAM_H
#define PS5UPLOAD_ELF_PARAM_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* Locating a title's SDK-version fields inside an ELF.
 *
 * Downgrading those fields is step one of backporting a game to older
 * firmware — without it the system refuses to launch the title. The
 * previous implementation scanned the whole file for two 32-bit magic
 * values and patched every hit, which meant a coincidental match in game
 * data got four bytes overwritten. A 100 MB eboot is ~25M aligned words,
 * so those collisions are not hypothetical.
 *
 * The parameters actually live in dedicated program-header segments, so
 * walking the header table turns a probabilistic search into an exact
 * lookup — and gives the signed-file check for free, because an
 * encrypted SELF has no readable program headers.
 *
 * Offsets follow idlesauce's ps5_elf_sdk_downgrade.py, which is what the
 * scene tooling (BackPork et al.) expects. They are segment-relative:
 * the magic sits at +0x08, so the SDK words at +0x14 / +0x10 are the
 * same fields a magic-relative +0x0C / +0x08 would reach.
 *
 * Tests: payload/tests/elf_param_selftest.c. */

#define PT_SCE_PROCPARAM     0x61000001u
#define PT_SCE_MODULE_PARAM  0x61000002u

#define SCE_PROCESS_PARAM_MAGIC 0x4942524Fu
#define SCE_MODULE_PARAM_MAGIC  0x3C13F4BFu

#define SCE_PARAM_MAGIC_OFFSET   0x08u
#define SCE_PARAM_PS5_SDK_OFFSET 0x14u
#define SCE_PARAM_PS4_SDK_OFFSET 0x10u

typedef enum {
    ELF_PARAM_OK = 0,
    ELF_PARAM_NOT_ELF,      /* no \x7fELF — not something we may patch */
    ELF_PARAM_SIGNED_SELF,  /* encrypted; patching would corrupt it */
    ELF_PARAM_MALFORMED,    /* header table runs outside the file */
    ELF_PARAM_NO_PARAMS,    /* a valid ELF that carries no param segment */
} elf_param_status_t;

/* Where a patchable SDK word lives, and which field it is. */
typedef struct {
    size_t   offset;   /* absolute file offset of the 4-byte value */
    uint32_t seg_type; /* PT_SCE_PROCPARAM or PT_SCE_MODULE_PARAM */
    /* Absolute file offset of the param struct itself. Both SDK fields
     * live here — PS4 at +0x10, PS5 at +0x14 — and a title declares BOTH.
     * `offset` names only the one this segment type prefers, which is why
     * writing through it alone left the other field stale. */
    size_t   param_offset;
} elf_param_site_t;

static inline uint16_t elf_rd16(const unsigned char *p) {
    return (uint16_t)(p[0] | ((uint16_t)p[1] << 8));
}
static inline uint32_t elf_rd32(const unsigned char *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
           ((uint32_t)p[3] << 24);
}
static inline uint64_t elf_rd64(const unsigned char *p) {
    return (uint64_t)elf_rd32(p) | ((uint64_t)elf_rd32(p + 4) << 32);
}

/* SELF container magics. Their presence says a file is WRAPPED, not that
 * it is encrypted — see `elf_self_payload_offset`. */
#define SELF_MAGIC_PS4 0x1D3D154Fu
#define SELF_MAGIC_PS5 0xEEF51454u

/* Where the segment table ends and the payload begins. Verified against
 * real files: eboot.bin, sce_module/libc.prx and several fakelib .sprx
 * all carry 12 entries and start their ELF at 0x1a0. */
#define SELF_HEADER_SIZE  0x20u
#define SELF_ENTRY_SIZE   0x20u
#define SELF_NUM_ENTRIES_OFFSET 0x18u

static inline int elf_has_self_magic(const unsigned char *buf, size_t len) {
    if (len < 4) return 0;
    uint32_t m = elf_rd32(buf);
    return m == SELF_MAGIC_PS4 || m == SELF_MAGIC_PS5;
}

/* Offset of the plaintext ELF inside a SELF wrapper.
 *
 * Returns 0 when the file is not wrapped at all (parse it from the
 * start), a positive offset when a readable ELF follows the wrapper, and
 * -1 when the file is wrapped but its payload cannot be read — a
 * genuinely encrypted retail SELF, which must never be written to.
 *
 * The distinction is the whole point. A *fake*-signed SELF has the same
 * magic as an encrypted one but carries a plaintext ELF inside, and it is
 * the shape every backportable game folder ships. Refusing on magic alone
 * meant the SDK changer skipped every executable and rewrote only
 * param.json, so the console still refused to launch the title:
 *
 *   Prospero SDK version of .../eboot.bin is 0x10000040,
 *   which is newer than system version(0x9600004)
 *
 * Deciding on the payload rather than the wrapper keeps encrypted files
 * refused for the reason that actually matters: nothing readable there. */
static inline int64_t elf_self_payload_offset(const unsigned char *buf,
                                              size_t len) {
    if (!elf_has_self_magic(buf, len)) return 0;
    if (len < SELF_HEADER_SIZE + 2) return -1;
    uint16_t num_entries = elf_rd16(buf + SELF_NUM_ENTRIES_OFFSET);
    uint64_t off = SELF_HEADER_SIZE + (uint64_t)num_entries * SELF_ENTRY_SIZE;
    if (off + 4 > (uint64_t)len) return -1;
    if (memcmp(buf + off, "\x7f" "ELF", 4) != 0) return -1;
    return (int64_t)off;
}

/* Kept for callers that only want the wrapper question answered. A true
 * result no longer implies "unpatchable" — ask
 * `elf_self_payload_offset` for that. */
static inline int elf_is_signed_self(const unsigned char *buf, size_t len) {
    return elf_has_self_magic(buf, len);
}

/* A SELF's segment table: `num_entries` records of 0x20 bytes, each
 * naming the program header it carries and where its bytes actually sit
 * in the file. */
#define SELF_ENTRY_OFFSET_FIELD 0x08u
#define SELF_ENTRY_FILESZ_FIELD 0x10u
#define SELF_ENTRY_ID(flags)    ((uint32_t)(((flags) >> 20) & 0xFFFu))

/* Translate an offset in the ELF's own layout to an offset in the file.
 *
 * For a bare ELF the two are the same. For a SELF they are NOT: the inner
 * ELF header keeps the offsets of the *original* unwrapped file, while
 * the bytes are relocated to wherever the SELF's segment table puts them.
 * Reading the parameters at the ELF's own offset lands on unrelated data
 * — in the title this was found on, on zeroes — which is why the first
 * pass at this reported "0 sites" even after the wrapper was parsed.
 *
 * A segment is located either by its own table entry, or through the
 * PT_LOAD that contains it. Entries are matched on both id and size,
 * because a program header can appear twice with different flags and
 * only the record whose size matches carries the segment's bytes.
 *
 * Returns the file offset, or -1 when the range cannot be mapped or
 * would fall outside the file. */
static inline int64_t elf_self_map_offset(const unsigned char *buf, size_t len,
                                          uint64_t base, const unsigned char *elf,
                                          uint64_t phoff, uint16_t phentsize,
                                          uint16_t phnum, uint16_t want_idx,
                                          uint64_t elf_off, uint64_t need) {
    if (base == 0) {
        /* Not wrapped: the ELF's offsets are the file's offsets. */
        if (elf_off + need > (uint64_t)len) return -1;
        return (int64_t)elf_off;
    }
    uint16_t num_entries = elf_rd16(buf + SELF_NUM_ENTRIES_OFFSET);

    /* Pass 1: an entry naming this program header directly. */
    const unsigned char *ph = elf + phoff + (uint64_t)want_idx * phentsize;
    uint64_t want_filesz = elf_rd64(ph + 0x20);
    for (uint16_t e = 0; e < num_entries; e++) {
        const unsigned char *ent = buf + SELF_HEADER_SIZE + (uint64_t)e * SELF_ENTRY_SIZE;
        if (SELF_ENTRY_ID(elf_rd64(ent)) != want_idx) continue;
        if (elf_rd64(ent + SELF_ENTRY_FILESZ_FIELD) != want_filesz) continue;
        uint64_t at = elf_rd64(ent + SELF_ENTRY_OFFSET_FIELD);
        if (at + need > (uint64_t)len) return -1;
        return (int64_t)at;
    }

    /* Pass 2: the PT_LOAD whose range covers it. */
    for (uint16_t i = 0; i < phnum; i++) {
        const unsigned char *lp = elf + phoff + (uint64_t)i * phentsize;
        if (elf_rd32(lp + 0x00) != 0x00000001u /* PT_LOAD */) continue;
        uint64_t l_off = elf_rd64(lp + 0x08);
        uint64_t l_filesz = elf_rd64(lp + 0x20);
        if (elf_off < l_off || elf_off + need > l_off + l_filesz) continue;
        for (uint16_t e = 0; e < num_entries; e++) {
            const unsigned char *ent =
                buf + SELF_HEADER_SIZE + (uint64_t)e * SELF_ENTRY_SIZE;
            if (SELF_ENTRY_ID(elf_rd64(ent)) != i) continue;
            if (elf_rd64(ent + SELF_ENTRY_FILESZ_FIELD) != l_filesz) continue;
            uint64_t at = elf_rd64(ent + SELF_ENTRY_OFFSET_FIELD) + (elf_off - l_off);
            if (at + need > (uint64_t)len) return -1;
            return (int64_t)at;
        }
    }
    return -1;
}

/* Collect every patchable SDK-version site. Returns how many were found
 * (at most `max`), or -1 with `status` set when the file must not be
 * touched at all. */
static inline int elf_find_param_sites(const unsigned char *buf, size_t len,
                                       elf_param_site_t *out, int max,
                                       elf_param_status_t *status) {
    if (status) *status = ELF_PARAM_OK;
    if (!buf || !out || max <= 0) {
        if (status) *status = ELF_PARAM_MALFORMED;
        return -1;
    }
    /* A wrapped file is parsed from its payload; everything below is
     * relative to `elf`, and every offset handed back is absolute. */
    int64_t base = elf_self_payload_offset(buf, len);
    if (base < 0) {
        if (status) *status = ELF_PARAM_SIGNED_SELF;
        return -1;
    }
    const unsigned char *elf = buf + base;
    size_t elen = len - (size_t)base;

    if (elen < 0x40 || memcmp(elf, "\x7f" "ELF", 4) != 0) {
        if (status) *status = ELF_PARAM_NOT_ELF;
        return -1;
    }

    uint64_t phoff = elf_rd64(elf + 0x20);
    uint16_t phentsize = elf_rd16(elf + 0x36);
    uint16_t phnum = elf_rd16(elf + 0x38);
    if (phentsize < 0x28 || phoff == 0 ||
        phoff + (uint64_t)phentsize * phnum > (uint64_t)elen) {
        if (status) *status = ELF_PARAM_MALFORMED;
        return -1;
    }

    int found = 0;
    for (uint16_t i = 0; i < phnum && found < max; i++) {
        const unsigned char *ph = elf + phoff + (uint64_t)i * phentsize;
        uint32_t p_type = elf_rd32(ph + 0x00);
        if (p_type != PT_SCE_PROCPARAM && p_type != PT_SCE_MODULE_PARAM) continue;

        uint64_t p_offset = elf_rd64(ph + 0x08);
        uint64_t p_filesz = elf_rd64(ph + 0x20);
        uint32_t want_magic = (p_type == PT_SCE_PROCPARAM)
                                  ? SCE_PROCESS_PARAM_MAGIC
                                  : SCE_MODULE_PARAM_MAGIC;
        uint32_t sdk_off = (p_type == PT_SCE_PROCPARAM) ? SCE_PARAM_PS5_SDK_OFFSET
                                                        : SCE_PARAM_PS4_SDK_OFFSET;

        /* The segment must actually hold the struct we expect. */
        if (p_filesz < sdk_off + 4) continue;
        int64_t at = elf_self_map_offset(buf, len, (uint64_t)base, elf, phoff,
                                         phentsize, phnum, i, p_offset, p_filesz);
        if (at < 0) continue;
        if (elf_rd32(buf + at + SCE_PARAM_MAGIC_OFFSET) != want_magic) continue;

        /* A file offset, so a wrapped file is patched in its parameters
         * and not somewhere inside the container. */
        out[found].offset = (size_t)((uint64_t)at + sdk_off);
        out[found].param_offset = (size_t)at;
        out[found].seg_type = p_type;
        found++;
    }

    if (found == 0 && status) *status = ELF_PARAM_NO_PARAMS;
    return found;
}

#endif
