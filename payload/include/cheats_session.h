#ifndef PS5UPLOAD_CHEATS_SESSION_H
#define PS5UPLOAD_CHEATS_SESSION_H

/* cheats_session — what the watcher knows about each saved cheat while one game runs (#627).
 * No console calls: cheats.c passes the memory work in as callbacks, so the host self-test
 * (tests/cheats_session_selftest.c) drives it with a fake game.
 *
 * The watcher used to try a game's saved cheats once, the first time it saw the game's process.
 * Early in a launch the game's code is often not loaded yet, the apply failed with "try again",
 * and nothing tried again: the switch read On and nothing was applied. Now each saved cheat has
 * a status for as long as the game runs:
 *   waiting  the game is not ready for it (code or module not loaded, page not readable). Tried
 *            again on every watcher pass for CHEATS_SESSION_FAST_TRIES passes, then every
 *            CHEATS_SESSION_SLOW_EVERY passes, for as long as the game runs.
 *   applied  written and read back. Re-read every CHEATS_SESSION_VERIFY_EVERY passes: back to the
 *            original bytes (the game reloaded its code) is put right again, up to
 *            CHEATS_SESSION_REVERT_MAX times in CHEATS_SESSION_REVERT_WINDOW_MS.
 *   failed   will not be tried again this session (another game version, the game keeps undoing
 *            it, a write that failed). The reason is kept and shown.
 *   changed  the bytes became something that is neither the original nor the cheat: the game
 *            changed it, and it is left alone.
 * A new process (or the game closing, or the engine going off) starts a new session.
 *
 * The crash guard (cheats_guard.h) is armed by each cheat's first automatic apply of the session,
 * so a cheat that only applies a minute in is watched too; a window already running is refreshed.
 * The marker is written right before such a write unless a window is already running (its marker
 * is still there). Re-applies of an applied cheat, switches flipped by hand and explicit re-applies
 * never arm it.
 *
 * An XML patch whose pattern is not found while part of its module could not be read waits (the
 * code may not be loaded yet), but only for CHEATS_SESSION_SCAN_WAITS tries or
 * CHEATS_SESSION_SCAN_WAIT_MS: then it fails as not in the game's code. */

#include <stddef.h>
#include <stdint.h>

#define CHEATS_SESSION_MAX_ITEMS       64
#define CHEATS_SESSION_NAME_MAX        64
#define CHEATS_SESSION_REASON_MAX      112
#define CHEATS_SESSION_TITLE_MAX       16
#define CHEATS_SESSION_FAST_TRIES      20      /* waiting: every pass for about a minute */
#define CHEATS_SESSION_SLOW_EVERY      5       /* then every 5th pass */
#define CHEATS_SESSION_VERIFY_EVERY    2       /* applied: re-read every 2nd pass */
#define CHEATS_SESSION_REVERT_MAX      5       /* more reverts than this in the window: stop */
#define CHEATS_SESSION_REVERT_WINDOW_MS 120000u
#define CHEATS_SESSION_SUMMARY_WAIT_MS 30000u  /* announce what applied even while others wait */
#define CHEATS_SESSION_SCAN_WAITS     20      /* pattern not found in a partly readable module */
#define CHEATS_SESSION_SCAN_WAIT_MS   60000u  /* ... waits at most this many tries / this long */
#define CHEATS_SESSION_SCAN_FAILED    "pattern not found in the game's code"

enum {
    CHEATS_ST_NONE = 0,
    CHEATS_ST_WAITING,
    CHEATS_ST_APPLIED,
    CHEATS_ST_FAILED,
    CHEATS_ST_CHANGED,
};

/* What one apply attempt came to. */
enum {
    CHEATS_OUT_APPLIED = 0, /* the cheat's bytes are there (written and read back, or already) */
    CHEATS_OUT_WAITING,     /* the game is not ready; nothing was written */
    CHEATS_OUT_FAILED,      /* will not work this session; nothing left written */
    CHEATS_OUT_GONE,        /* it is no longer switched on (or the engine went off) */
    CHEATS_OUT_WAITING_SCAN, /* waiting: its pattern is not in the part of the module that could
                              * be read (bounded, see CHEATS_SESSION_SCAN_WAITS) */
};

/* What a re-read of an applied cheat found. */
enum {
    CHEATS_VERIFY_ON = 0,   /* still the cheat's bytes */
    CHEATS_VERIFY_REVERTED, /* the original bytes again (some or all of them) */
    CHEATS_VERIFY_CHANGED,  /* something else */
    CHEATS_VERIFY_UNKNOWN,  /* could not be read now; look again later */
};

/* One switched-on cheat (or always-on patch) of the running game, as cheats.c lists it. */
typedef struct {
    uint64_t key;                          /* stable across passes: file + entry */
    int      flat;                         /* its index in the mod list; -1 for a patch */
    int      slot;                         /* cheats.c's own handle, passed back to it */
    char     name[CHEATS_SESSION_NAME_MAX];
} cheats_item_t;

typedef struct {
    cheats_item_t item;
    int      status;
    char     reason[CHEATS_SESSION_REASON_MAX];
    int      tries;                        /* waiting attempts */
    int      skip;                         /* passes to skip before the next attempt / re-read */
    int      ever_applied;                 /* announced (or applied by hand): never again */
    int      quiet;                        /* switched on by hand: no announcement */
    int      n_reverts;
    uint64_t revert_ms[CHEATS_SESSION_REVERT_MAX];
    int      seen;
    int      scan_waits;                   /* CHEATS_OUT_WAITING_SCAN results */
    uint64_t scan_since;                   /* the first of them */
} cheats_entry_t;

typedef struct cheats_session cheats_session_t;

typedef struct {
    void *ctx;
    int (*engine_on)(void *ctx);
    int (*held)(void *ctx, const char *title);
    /* The game's switched-on cheats now. Fills up to `max`, and the game's own name ("" none). */
    int (*list)(void *ctx, const char *title, cheats_item_t *items, int max, char *game,
                size_t game_cap);
    /* Apply one. `reapply` is 1 when it was applied before and the game undid it. It calls
     * cheats_session_before_write(s) right before its first write. Returns CHEATS_OUT_*, with
     * `why` set for WAITING and FAILED. */
    int (*apply)(void *ctx, cheats_session_t *s, const cheats_item_t *it, int reapply, char *why,
                 size_t why_cap);
    int (*verify)(void *ctx, const cheats_item_t *it); /* CHEATS_VERIFY_* */
    void (*marker_set)(void *ctx, const char *title);
    void (*marker_clear)(void *ctx);
    void (*guard_applied)(void *ctx, const char *title, int pid, uint64_t now_ms);
    void (*notify)(void *ctx, const char *msg, int warn);
    void (*log)(void *ctx, const char *msg);
    /* 1 while the crash guard watches a window for `title` running as `pid` (its marker is
     * written). NULL: never. */
    int (*guard_watching)(void *ctx, const char *title, int pid);
} cheats_session_ops_t;

struct cheats_session {
    int      pid;
    char     title[CHEATS_SESSION_TITLE_MAX];
    char     game[CHEATS_SESSION_NAME_MAX];
    cheats_entry_t e[CHEATS_SESSION_MAX_ITEMS];
    int      n;
    int      no_guard;        /* this pass is an explicit re-apply: nothing arms the guard */
    int      armed_pass;      /* an apply armed (or refreshed) the guard this pass */
    int      marker_on;       /* the marker was written this pass */
    int      arming;          /* the attempt running now is a cheat's first automatic apply */
    int      held_told;
    int      summary_sent;
    int      announce;        /* first applies not yet announced */
    uint64_t announce_since;
    const cheats_session_ops_t *ops;
};

/* Forget everything (no game, a new game, the engine off). */
void cheats_session_reset(cheats_session_t *s);

/* Start the session for `pid` running `title`, unless it is the one already kept. */
void cheats_session_begin(cheats_session_t *s, int pid, const char *title);

/* One watcher pass for the game running as `pid` (<= 0 for none). `force` is an explicit
 * re-apply from the app: a fresh session, applied even with the engine off or the game paused,
 * announced at once, and not watched by the crash guard (like a switch flipped by hand).
 * Returns how many cheats were applied for the first time this pass. */
int cheats_session_pass(cheats_session_t *s, int pid, const char *title, uint64_t now_ms,
                        int force, const cheats_session_ops_t *ops);

/* Called by ops->apply right before it writes. Writes the crash-guard marker when this is a
 * cheat's first automatic apply and no guard window is running, once per pass. */
void cheats_session_before_write(cheats_session_t *s);

/* A switch flipped by hand (it applied): on is kept as applied (quietly: the toggle says so
 * itself), off is forgotten. Either way its counters start over. */
void cheats_session_manual(cheats_session_t *s, uint64_t key, int on);

/* The status of the mod at flat index `flat` (CHEATS_ST_NONE when not tracked), and its
 * reason. */
int cheats_session_status(const cheats_session_t *s, int flat, const char **reason);

/* "applied", "waiting", "failed", "changed", or NULL. */
const char *cheats_status_name(int st);

/* ── The saved switches (<state dir>/<title>.json) ─────────────────
 * {"<flat index>":true,...}, and "<flat index>":false for a cheat its file switches on itself
 * ("_sonic_enabled":true) that the player turned off: the player's choice wins over the file. */
enum {
    CHEATS_SW_UNSET = 0, /* no key: the cheat file's own flag decides */
    CHEATS_SW_ON,
    CHEATS_SW_OFF,
    CHEATS_SW_UNKNOWN,   /* the sidecar could not be read just now */
};

/* What the sidecar text `buf` says about `flat` (CHEATS_SW_UNSET/ON/OFF). */
int cheats_state_get(const char *buf, int flat);

/* The sidecar with `flat` set to `value` (CHEATS_SW_*; UNSET removes its key) and every other key
 * of `old` (indices below `max_keys`) kept. Length written, or -1 when it does not fit. */
int cheats_state_build(const char *old, int flat, int value, int max_keys, char *out, size_t cap);

/* Whether a cheat is switched on: an off not saved yet (`pending_off`) wins, then the sidecar,
 * then (no key) the file's own flag. A sidecar that cannot be read keeps it on (it was on when
 * listed). */
int cheats_switch_on(int saved, int from_file, int pending_off);

/* Switches turned off by hand whose off could not be saved: kept for the game process they were
 * turned off in, so the watcher does not put them back on this session. */
#define CHEATS_OFFS_MAX 64
typedef struct {
    int  pid;
    char title[CHEATS_SESSION_TITLE_MAX];
    int  n;
    int  flat[CHEATS_OFFS_MAX];
} cheats_offs_t;

void cheats_offs_add(cheats_offs_t *o, int pid, const char *title, int flat);
void cheats_offs_del(cheats_offs_t *o, const char *title, int flat);
int cheats_offs_has(const cheats_offs_t *o, const char *title, int flat);
/* The game running now (pid <= 0: none): offs kept for another process are forgotten. */
void cheats_offs_session(cheats_offs_t *o, int pid, const char *title);

/* ── Recorded locations (XML patches) ──────────────────────────────
 * A module as it was when a patch's locations were recorded, or as it is now. */
typedef struct {
    uint64_t base, size;
} cheats_module_t;

/* 1 when a location recorded against `was` may be written again: the module is loaded now
 * (`now` non-NULL) at the same base and size, and [addr, addr + len) is inside it. */
int cheats_loc_still_ours(const cheats_module_t *was, const cheats_module_t *now, uint64_t addr,
                          int len);

#endif
