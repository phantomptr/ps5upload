#ifndef PS5UPLOAD_CHEATS_PLAN_H
#define PS5UPLOAD_CHEATS_PLAN_H

/* cheats_plan — the pure part of applying one cheat: what each location holds now, which
 * locations are code caves, and the order the writes go in. No console calls, so the host
 * self-test (tests/cheats_plan_selftest.c) runs it on real cheat files.
 *
 * A cheat is applied as a whole or not at all. The old engine wrote whichever locations
 * matched and skipped the rest, so a cheat whose jump landed but whose code cave did not
 * sent the game into int3 padding. */

#include <stddef.h>
#include <stdint.h>

#define CHEAT_PLAN_MAX_BYTES 256
#define CHEAT_PLAN_MAX_LOCS  32  /* MAX_MEM_ENTRIES in cheats.c */
/* An XML game patch (game_patch.h) can have hundreds of lines, each one location or more. */
#define CHEAT_PLAN_MAX_APPLY 8192

/* One location of a cheat, with the bytes read from the game before anything is written. */
typedef struct {
    intptr_t addr;
    int      len;                       /* on, off and cur are all this long */
    uint8_t  on[CHEAT_PLAN_MAX_BYTES];
    uint8_t  off[CHEAT_PLAN_MAX_BYTES];
    uint8_t  cur[CHEAT_PLAN_MAX_BYTES];
    int      keep;                      /* set by cheat_plan_apply: left as it is */
} cheat_loc_t;

/* A byte range another enabled cheat of the same title also writes. */
typedef struct {
    intptr_t addr;
    int      len;
} cheat_range_t;

/* Where the writes go. On the console this is the game's memory; the self-test uses a fake. */
typedef struct {
    void *ctx;
    /* Write len bytes at addr and read them back. 0 = the bytes are there. A failure may leave
     * the range partly written (a write split across two pages can fail on the second). */
    int (*write)(void *ctx, intptr_t addr, const uint8_t *data, size_t len);
    /* 1 while the target process still exists. Checked before every write. */
    int (*alive)(void *ctx);
} cheat_mem_ops_t;

enum {
    CHEAT_LOC_MISMATCH = -1, /* neither the cheat's bytes nor the original: wrong game build */
    CHEAT_LOC_OFF      = 0,  /* holds the original bytes */
    CHEAT_LOC_ON       = 1,  /* holds the cheat's bytes */
};

/* Parse "90 1F-00,00" / "901F0000" into bytes. Separators: space , - : tab CR LF. A trailing
 * odd nibble becomes the low nibble of a last byte. Returns the byte count, or -1 on a
 * non-hex character, an empty string, or more than `max` bytes. */
int cheat_parse_hex(const char *s, uint8_t *out, int max);

/* Bare or 0x-prefixed hex offset. Returns 0 and fills *out, or -1 when it is not hex. */
int cheat_parse_offset(const char *s, uint64_t *out);

/* Which state a location is in, from the bytes read before writing. */
int cheat_loc_state(const cheat_loc_t *l);

/* A code cave: the original bytes are all int3 (0xCC) or all zero padding, so nothing in the
 * game runs there until the cheat's own jump sends it there. */
int cheat_loc_is_cave(const cheat_loc_t *l);

/* The order to write the n locations in. Turning on: caves first, then everything else, each
 * group in file order, so no jump is live before the code it jumps to. Turning off is the exact
 * reverse, so the jumps are restored before their caves go back to padding. The game keeps
 * running while we write, so this order is what keeps it from executing half a cheat.
 * Fills order[0..n-1] with indices into locs. */
void cheat_plan_order(const cheat_loc_t *locs, int n, int turn_on, int *order);

/* Apply or remove one cheat whose locations have `cur` filled in (read from the game before
 * anything is written). All or nothing:
 *   - turning off, a code cave that overlaps one of `keep` (another cheat that is still on and
 *     shares the cave) is left alone;
 *   - any other location holding neither its cheat nor its original bytes refuses the whole
 *     cheat before anything is written;
 *   - writes go in cheat_plan_order(), skipping locations already in the wanted state;
 *   - if a write fails (or the game is gone), the failing location and every one written
 *     before it are put back to `cur`, newest first, and err says whether that worked.
 * Returns 0 when the cheat is in the asked-for state. */
int cheat_plan_apply(cheat_loc_t *locs, int n, int turn_on,
                     const cheat_range_t *keep, int n_keep,
                     const cheat_mem_ops_t *ops, char *err, size_t err_cap);

/* A user-space address the engine is willing to write: above the null page, below the
 * canonical user limit, and not wrapping. */
int cheat_addr_ok(intptr_t addr, int len);

/* Whether a loaded module of the game's process may take a physical write: one of the game's
 * own. A module loaded from its /app0, or dynlib handle 0 when that is its executable (a path
 * ending in eboot.bin, or a path that could not be read, as handle 0 always was). Handle 0 by
 * number alone is not enough: in a PS4 game's process (#920) it is another module, and a
 * physical write there lands in every process that maps the page. */
int cheat_module_is_game(uint64_t handle, const char *path);

#endif /* PS5UPLOAD_CHEATS_PLAN_H */
