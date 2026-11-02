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
 * The crash guard (cheats_guard.h) is armed once per session, by the first automatic apply that
 * succeeds; the marker is written only right before a write that could be that first apply.
 * Re-applies and later cheats never arm it again. */

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
} cheats_session_ops_t;

struct cheats_session {
    int      pid;
    char     title[CHEATS_SESSION_TITLE_MAX];
    char     game[CHEATS_SESSION_NAME_MAX];
    cheats_entry_t e[CHEATS_SESSION_MAX_ITEMS];
    int      n;
    int      guard_armed;     /* the guard was told about this session's first apply */
    int      marker_on;       /* the marker was written this pass */
    int      arming;          /* the attempt running now could be the session's first apply */
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

/* Called by ops->apply right before it writes. Writes the crash-guard marker when this could be
 * the session's first apply, once. */
void cheats_session_before_write(cheats_session_t *s);

/* A switch flipped by hand (it applied): on is kept as applied (quietly: the toggle says so
 * itself), off is forgotten. Either way its counters start over. */
void cheats_session_manual(cheats_session_t *s, uint64_t key, int on);

/* The status of the mod at flat index `flat` (CHEATS_ST_NONE when not tracked), and its
 * reason. */
int cheats_session_status(const cheats_session_t *s, int flat, const char **reason);

/* "applied", "waiting", "failed", "changed", or NULL. */
const char *cheats_status_name(int st);

#endif
