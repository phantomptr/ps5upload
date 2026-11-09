#ifndef PS5UPLOAD2_POWER_PHASE_H
#define PS5UPLOAD2_POWER_PHASE_H

/*
 * Where the console is in a rest/wake cycle, as the helper can tell from inside.
 *
 * Measured on FW 13.60: the helper process survives rest frozen. For the ~25 s (Phat: ~80 s) before
 * it freezes, socket calls fail with errno 163 (Sony's own, beyond FreeBSD's ELAST); in rest the
 * monotonic clock stops; on wake the wall clock jumps ahead of it. Nothing tells a process "the
 * console is going to rest" or "it just woke", so this module infers it:
 *
 *   NORMAL      nothing going on.
 *   GOING_DOWN  our own standby request (power.control), or a loopback socket probe failing the way
 *               it does while the console goes to rest. Left without a wake only after the probe
 *               has been healthy again for a while (the request was ignored, or it was a blip).
 *   WAKING      the clocks jumped apart (the process was frozen). Stays until the network probe is
 *               healthy and a minute of running time has passed.
 *
 * What it is for: anything that is not needed to answer the user and that touches hardware or the
 * kernel (the fan re-apply, the wake watchdog's ucred look and re-elevation) waits while the phase
 * is not NORMAL, and the tailnet node stays paused (no sockets) while the network is not usable.
 * Nothing here ever does a kernel or hardware call by itself.
 *
 * The decisions are pure (static inline below) so the host self-test runs them; power_phase.c
 * keeps the one live state, runs the probe, and logs each transition with both clocks.
 */

#include <errno.h>
#include <stdint.h>

enum {
    PP_NORMAL = 0,
    PP_GOING_DOWN = 1,
    PP_WAKING = 2,
};

enum {
    PP_WHY_NONE = 0,
    PP_WHY_STANDBY = 1, /* power.control standby from this helper */
    PP_WHY_NET = 2,     /* the loopback probe failed with a "network is going away" errno */
};

/* After a wake: a minute of running time before fan and kernel work (monotonic seconds). */
#define PP_SETTLE_S 60
/* Across one wake-watchdog poll, the wall clock running this far ahead of the monotonic one means
 * the process was frozen (13.60: the monotonic clock stops in rest). Same gap AVA1 rebinds on. A
 * thread the system throttled under a game moves both clocks alike and is not a wake. */
#define PP_FROZEN_GAP_S 10
/* A failing probe that was not followed by a rest: back to NORMAL this long after the last failure. */
#define PP_NET_HOLD_S 30
/* Our own standby request that never put the console to rest. */
#define PP_STANDBY_HOLD_S 180
/* Healthy probes in a row before the network counts as back. */
#define PP_OK_PROBES 2
/* The errno the PS5 returns from socket calls while it goes to rest (0xA3). */
#define PP_ERRNO_SONY_REST 163

typedef struct {
    int phase;
    int why;           /* PP_WHY_*, meaningful in GOING_DOWN */
    int64_t down_s;    /* monotonic s of the last going-down signal */
    int64_t wake_s;    /* monotonic s when the last wake was seen */
    int ok_streak;     /* healthy probes in a row */
    uint32_t downs;    /* going-down episodes seen */
    uint32_t wakes;    /* wakes seen */
} pp_state;

static inline void pp_init(pp_state *s) {
    s->phase = PP_NORMAL;
    s->why = PP_WHY_NONE;
    s->down_s = 0;
    s->wake_s = 0;
    s->ok_streak = PP_OK_PROBES; /* a fresh helper starts with a working network */
    s->downs = 0;
    s->wakes = 0;
}

/* Either clock moving this far across one 5 s poll: the process was frozen. Measured on the Pro
 * (13.60, 2026-11-09), the clocks disagree from rest to rest: once both moved 411 s, once the
 * monotonic clock moved 284 s while the wall clock moved 5 s (reset on wake). A thread a game
 * throttles stalls seconds, not a minute; a wall clock set forward by a minute or more while
 * running only holds background work for the settle minute, which is harmless. */
#define PP_FROZEN_POLL_S 60

/* Did the clocks' movement across one poll say the process was frozen? */
static inline int pp_clocks_say_frozen(int64_t real_elapsed_s, int64_t mono_elapsed_s) {
    if (real_elapsed_s - mono_elapsed_s > PP_FROZEN_GAP_S) return 1;
    return real_elapsed_s > PP_FROZEN_POLL_S || mono_elapsed_s > PP_FROZEN_POLL_S;
}

/* Does a failing socket call with this errno say "the network is going away"? Resource limits
 * (EMFILE, ENOBUFS, ...) and retries (EINTR, EAGAIN) do not. */
static inline int pp_errno_means_down(int e) {
    return e == PP_ERRNO_SONY_REST || e == ENETDOWN || e == ENETUNREACH || e == EHOSTDOWN || e == EADDRNOTAVAIL;
}

/* 1 when the phase changed. */
static inline int pp_on_standby(pp_state *s, int64_t now_s) {
    int changed = s->phase != PP_GOING_DOWN || s->why != PP_WHY_STANDBY;
    if (s->phase != PP_GOING_DOWN) s->downs++;
    s->phase = PP_GOING_DOWN;
    s->why = PP_WHY_STANDBY;
    s->down_s = now_s;
    s->ok_streak = 0;
    return changed;
}

/* The clocks jumped apart: the process was frozen. 1 (always a transition worth logging). */
static inline int pp_on_wake(pp_state *s, int64_t now_s) {
    s->phase = PP_WAKING;
    s->why = PP_WHY_NONE;
    s->wake_s = now_s;
    s->ok_streak = 0;
    s->wakes++;
    return 1;
}

/* Time passing with no new signal. 1 when the phase changed (back to NORMAL). */
static inline int pp_on_tick(pp_state *s, int64_t now_s) {
    if (s->ok_streak < PP_OK_PROBES) return 0;
    if (s->phase == PP_GOING_DOWN) {
        int64_t hold = s->why == PP_WHY_STANDBY ? PP_STANDBY_HOLD_S : PP_NET_HOLD_S;
        if (now_s - s->down_s < hold) return 0;
    } else if (s->phase == PP_WAKING) {
        if (now_s - s->wake_s < PP_SETTLE_S) return 0;
    } else {
        return 0;
    }
    s->phase = PP_NORMAL;
    s->why = PP_WHY_NONE;
    return 1;
}

/* One probe result: ok, or the errno it failed with. 1 when the phase changed. While WAKING a
 * failure only says the network is not back yet (or the console is going down again): the phase
 * stays WAKING, so the settle minute still counts from the wake, and the network is unusable. */
static inline int pp_on_probe(pp_state *s, int ok, int err, int64_t now_s) {
    if (ok) {
        if (s->ok_streak < 1000) s->ok_streak++;
        return pp_on_tick(s, now_s);
    }
    if (!pp_errno_means_down(err)) return 0; /* out of descriptors etc.: not a power transition */
    s->ok_streak = 0;
    if (s->phase == PP_NORMAL) {
        s->phase = PP_GOING_DOWN;
        s->why = PP_WHY_NET;
        s->down_s = now_s;
        s->downs++;
        return 1;
    }
    if (s->phase == PP_GOING_DOWN) s->down_s = now_s;
    return 0;
}

/* Hold off hardware and kernel work (fan re-apply, ucred look). */
static inline int pp_quiet(const pp_state *s) { return s->phase != PP_NORMAL; }

/* May networking that is not needed to answer the user (the tailnet node) run? */
static inline int pp_net_usable(const pp_state *s) {
    return s->phase == PP_NORMAL || (s->phase == PP_WAKING && s->ok_streak >= PP_OK_PROBES);
}

static inline const char *pp_phase_name(int phase) {
    return phase == PP_GOING_DOWN ? "going_down" : phase == PP_WAKING ? "waking" : "normal";
}

static inline const char *pp_why_name(int why) {
    return why == PP_WHY_STANDBY ? "standby_request" : why == PP_WHY_NET ? "socket_errors" : "none";
}

/* ── The live state (power_phase.c, console build) ─────────────────────── */

/* Our power.control standby is about to run. Safe from any thread. */
void power_phase_note_standby(void);
/* Called by the wake watchdog after each poll with the clocks' movement across it. Runs the probe,
 * updates the phase (a wake when pp_clocks_say_frozen), pauses/resumes the tailnet node, rotates
 * stderr.log when it grew too big, and logs transitions. Returns the phase after the update. */
int power_phase_poll(int64_t real_elapsed_s, int64_t mono_elapsed_s);
/* 1 while fan and kernel work should wait (any phase but NORMAL). Lock-free, any thread. */
int power_phase_quiet(void);
/* Current phase (PP_*), lock-free. */
int power_phase_current(void);

#endif /* PS5UPLOAD2_POWER_PHASE_H */
