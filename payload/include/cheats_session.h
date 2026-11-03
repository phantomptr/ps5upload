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
 * CHEATS_SESSION_SCAN_WAIT_MS: then it fails as not in the game's code.
 *
 * Start-time patches (#627 follow-up). Some games read a setting once in their first seconds (Red
 * Dead Redemption 2 reads its frame-rate mode once), so a patch written 3-9 s after launch reads
 * back as applied and does nothing. When the watcher sees a game start (its exec, or a look that
 * found no game just before), cheats_session_launch() opens a start-up window of
 * CHEATS_SESSION_FAST_MS: while any cheat still waits for the game's code, a fast pass
 * (cheats_session_fast_pass) tries only the waiting ones every CHEATS_SESSION_FAST_EVERY_MS,
 * without listing the cheat files again. Full passes (list, verify, self-heal) keep their
 * CHEATS_SESSION_FULL_EVERY_MS cadence; cheats_sched_next() says which pass is due.
 *
 * Each cheat's first automatic apply records how long after the launch it landed (and when its
 * code was first ready). Later than CHEATS_SESSION_LATE_MS, or with the launch not seen (the helper
 * or the engine started mid-game), it is marked late: a start-time patch may have come too late
 * and the game has to be restarted with it on. */

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
#define CHEATS_SESSION_FAST_MS        60000u  /* after a launch: fast passes at most this long */
#define CHEATS_SESSION_FAST_EVERY_MS  5u      /* ... this far apart while a cheat waits */
#define CHEATS_SESSION_FULL_EVERY_MS  3000u   /* list, verify, put back: this often */
#define CHEATS_SESSION_IDLE_EVERY_MS  500u    /* engine on, no game: look for one this often */
#define CHEATS_SESSION_LATE_MS        3000u   /* applied later than this after launch: late */

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
    int      attempts;                     /* apply attempts, fast passes included */
    int      timed;                        /* its first automatic apply was timed: */
    int64_t  after_ms;                     /*   ms from launch to applied, -1 launch not seen */
    int64_t  ready_ms;                     /*   ms from launch to the attempt that applied it */
    int      late;                         /*   may have come too late for a start-time patch */
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
    /* The clock now, for the timing of an apply (an apply can take a while). NULL: the pass's. */
    uint64_t (*now_ms)(void *ctx);
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
    /* The launch (cheats_session_launch). */
    uint64_t launch_ms;       /* when the game started (0: not seen) */
    int      launch_seen;
    uint64_t fast_until;      /* the start-up window ends (0: none) */
    int      listed;          /* a full pass has looked at its cheats (listed, or paused) */
    int      in_fast;         /* the pass running now is a fast pass */
    int      fast_passes;
    int      fast_over;       /* the window's end was logged */
    int      wrote;           /* the attempt running now wrote (cheats_session_before_write) */
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

/* The watcher saw `pid` start running `title` (its exec, or a look that found it right after one
 * that found no game): a new session whose launch was at `at_ms`, with `seen` 0 when the start was
 * not seen (`at_ms` is then ignored). `fast` opens the start-up window (cheats_session_fast). A
 * second exec of the same process starts over too: its code is new. */
void cheats_session_launch(cheats_session_t *s, int pid, const char *title, uint64_t at_ms,
                           int seen, int fast, uint64_t now_ms);

/* A fast pass: only the cheats still waiting for the game's code (not a pattern scan, which is
 * costly and bounded by tries) are tried, nothing is listed or re-read. Call it when
 * cheats_sched_next() says so. Returns how many were applied for the first time. */
int cheats_session_fast_pass(cheats_session_t *s, uint64_t now_ms,
                             const cheats_session_ops_t *ops);

/* 1 while the start-up window is open and a cheat still waits (or nothing is listed yet). */
int cheats_session_fast(const cheats_session_t *s, uint64_t now_ms);

/* The watcher's clock: when it last ran a full and a fast pass (0: never). */
typedef struct {
    uint64_t last_full_ms;
    uint64_t last_fast_ms;
} cheats_sched_t;

enum {
    CHEATS_NEXT_WAIT = 0, /* nothing due: wait *wait_ms (an exec event may come sooner) */
    CHEATS_NEXT_FAST,     /* a fast pass */
    CHEATS_NEXT_FULL,     /* a full pass (and, with no game, a look for one) */
};

/* Which pass is due now. Full: every CHEATS_SESSION_FULL_EVERY_MS, or CHEATS_SESSION_IDLE_EVERY_MS
 * while the engine is on and no game is known. Fast: CHEATS_SESSION_FAST_EVERY_MS after the last
 * fast pass ended, while cheats_session_fast(). */
int cheats_sched_next(const cheats_sched_t *k, const cheats_session_t *s, int engine_on,
                      uint64_t now_ms, uint64_t *wait_ms);

/* The timing of the mod at flat index `flat`'s first automatic apply: 1 with *after_ms (-1 when
 * the launch was not seen) and *late set, 0 when it was not timed (not applied, by hand). */
int cheats_session_timing(const cheats_session_t *s, int flat, int64_t *after_ms, int *late);

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

/* The one answer to "is cheat `flat` on" that the watcher (still_on), the mod list
 * (cheats_load_state) and the turn-off cave protection (collect_enabled_ranges) all use: the
 * sidecar text `state` (NULL: it could not be read, kept on), else the file's own flag, minus an
 * explicit or pending off. */
int cheats_effective_on(const char *state, int flat, int from_file, int pending_off);

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
