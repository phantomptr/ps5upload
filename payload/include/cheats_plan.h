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
 * ending in eboot.bin, or a path that could not be read, as handle 0 always was), or a module
 * under another handle whose path ends in eboot.bin outside /system* and /preinst (#946). Handle 0 by
 * number alone is not enough: in a PS4 game's process (#920) it is another module, and a
 * physical write there lands in every process that maps the page. */
int cheat_module_is_game(uint64_t handle, const char *path);

/* Whether an XML game patch may take the module with dynlib `handle`, loaded from `path`, as the
 * base its lines are placed in and bounded by: cheat_module_is_game's rule, so a PS4 game's handle
 * 0 that is not its eboot.bin is refused whatever way the write would go (physical or debugger).
 * 1, or 0 with `why` (may be NULL) saying so. */
int cheat_patch_base_ok(uint64_t handle, const char *path, char *why, size_t why_cap);

/* Where a PS4 executable is loaded: the address GoldHEN-style PS4 cheat offsets are relative to,
 * and where a debugger saw Rayman Legends' (CUSA00284) code on a PS5 (#920). */
#define CHEAT_PS4_EXE_BASE 0x400000ULL

/* What the game process's module list says about dynlib handle 0 (cheats.c module_info). */
typedef struct {
    int      found; /* handle 0 is in the list */
    uint64_t base;  /* its mapbase */
    uint64_t size;  /* its mapsize */
    char     path[256];
} cheat_exe_seen_t;

enum {
    CHEAT_EXE_NONE        = 0, /* no base: the game's code is not there (yet) */
    CHEAT_EXE_HANDLE0     = 1, /* handle 0's load address */
    CHEAT_EXE_PS4_DEFAULT = 2, /* a PS4 game whose module list gives no usable base: 0x400000 */
    CHEAT_EXE_NAMED       = 3, /* a PS4 game: the module named eboot.bin, under another handle */
};

/* The address a game's cheat offsets are relative to: its executable's load address.
 *
 * A PS5 game: handle 0's load address, or none (0) while it has none.
 * A PS4 game (`ps4`): handle 0's load address when handle 0 is the game's executable at a usable
 * address; else the load address of the module named eboot.bin (`eboot`, may be NULL: the list
 * has none, or it is handle 0) when usable; otherwise CHEAT_PS4_EXE_BASE, never 0 (#946). 12.6.26 (#920) fell back to 0x400000
 * only when handle 0 was some other module, and only when one 16-byte read at 0x400000 worked,
 * so a handle 0 named eboot.bin whose list entry gives a load address of 0, or that one read
 * failing, still left 0 and every toggle said "the game's code is not loaded yet" without
 * reading a single cheat location. Taking 0x400000 writes nothing by itself: every location is
 * read first and must hold the cheat's original bytes, and a physical write must still pass the
 * module check (cheat_module_is_game).
 *
 * What handle 0 of a PS4 game's process really is has not been seen: the "336 KiB module at
 * 0x6988c000" #920 went by (and the "eboot.bin at 0x400000, 344064 bytes" of #946) came from
 * PROC_MODULES, which lists the helper's own process (runtime.c handle_proc_modules), not the
 * game's. cheats.c log_game_modules writes the game's list to the helper's log when this falls
 * back, so a bug report shows it.
 *
 * `how` (may be NULL) gets a CHEAT_EXE_* value. */
uint64_t cheat_exe_base_pick(int ps4, const cheat_exe_seen_t *h0, const cheat_exe_seen_t *eboot,
                             int *how);

/* What handle 0 is, for an error or the log: "eboot.bin at 0x0, 0x1a4000 bytes", "not in the
 * module list". Only the file name of its path (a sandbox path names the title and nothing
 * else, but the name is all that is needed). */
void cheat_exe_describe(const cheat_exe_seen_t *h0, char *out, size_t cap);

#endif /* PS5UPLOAD_CHEATS_PLAN_H */
