#ifndef PS5UPLOAD_CHEATS_GUARD_H
#define PS5UPLOAD_CHEATS_GUARD_H

/* cheats_guard — what the cheat engine keeps across helper starts (#550), and the guard that
 * stops it re-applying cheats that crash a game. No console calls, so the host self-test
 * (tests/cheats_guard_selftest.c) runs it on a temp folder.
 *
 * The engine switch used to start off with every helper start, so after a reboot the switches
 * read On while nothing was applied. It is remembered now, which takes away the old way out of a
 * cheat that crashes its game at launch (reboot, and the engine is off). The guard replaces it:
 *   - a game that closes within CHEATS_GUARD_WINDOW_MS of its saved cheats being applied, twice
 *     in a row, is paused: its cheats are no longer applied when it starts. Each cheat's first
 *     automatic apply opens (or refreshes) a window, so a cheat that applies a minute in is
 *     watched too. A run that ends without a quick close (the game ran past every window and
 *     closed later) resets the count; surviving one window does not, since a later cheat of the
 *     same run can still be the one that crashes it;
 *   - a marker is written before each automatic apply and removed once the game has run for the
 *     window (or closed). A helper that starts and finds the marker died during that window
 *     (a console crash, or a restart), and that game is paused at once.
 * Switching any of the game's cheats (an explicit choice) lifts its pause. */

#include <stddef.h>
#include <stdint.h>

#define CHEATS_GUARD_WINDOW_MS 30000u
#define CHEATS_GUARD_STRIKES   2
#define CHEATS_GUARD_TITLE_MAX 16

/* ── Files, under the folders cheats.c passes ─────────────────────── */

/* The remembered engine switch, `<root>/engine.json` ({"enabled":true}). 1 or 0; 0 when the file
 * is missing or unreadable (a fresh install starts off, as before). */
int cheats_engine_load(const char *root);
/* Remember the switch (same-folder temp + rename). 0, or -1. */
int cheats_engine_save(const char *root, int on);

/* A paused game: `<state_dir>/<title>.held`. 1 when paused. */
int cheats_held(const char *state_dir, const char *title);
/* Pause (0 or -1) / lift (always fine: a missing file is no pause). */
int cheats_hold(const char *state_dir, const char *title);
void cheats_unhold(const char *state_dir, const char *title);

/* The apply-in-progress marker, `<root>/applying` (the title id). */
int cheats_marker_set(const char *root, const char *title);
void cheats_marker_clear(const char *root);
/* 1 and the title in `out` when a marker was left (it is removed), else 0. */
int cheats_marker_take(const char *root, char *out, size_t cap);

/* ── The guard ───────────────────────────────────────────────────── */

typedef struct {
    char     title[CHEATS_GUARD_TITLE_MAX]; /* the game whose apply is watched ("" none) */
    int      pid;
    uint64_t applied_ms;
    char     strike_title[CHEATS_GUARD_TITLE_MAX];
    int      strikes;                       /* quick closes in a row, of strike_title */
    int      ok_pid;                        /* survived a window; its run has not ended yet */
    char     ok_title[CHEATS_GUARD_TITLE_MAX];
} cheats_guard_t;

enum {
    CHEATS_GUARD_NONE = 0,  /* nothing to do */
    CHEATS_GUARD_SURVIVED,  /* the game ran the window: clear the marker */
    CHEATS_GUARD_STRIKE,    /* it closed inside the window: clear the marker */
    CHEATS_GUARD_HOLD,      /* and that was the last strike: clear the marker, pause the game */
};

/* Saved cheats were just applied to `title` running as `pid` (a window running for it is
 * refreshed). */
void cheats_guard_applied(cheats_guard_t *g, const char *title, int pid, uint64_t now_ms);

/* Each watcher pass: the game running now (`pid` <= 0 for none). The game it returns
 * STRIKE or HOLD for is in g->strike_title. */
int cheats_guard_tick(cheats_guard_t *g, int pid, uint64_t now_ms);

#endif
