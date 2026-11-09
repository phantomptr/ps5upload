/* Host proof for the power phase (power_phase.h): while the console goes to rest and until it has
 * settled after a wake, fan and kernel work hold off and the tailnet node has no sockets. */
#include "power_phase.h"

#include <stdio.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

/* One wake-watchdog poll (5 s): the clocks' movement, then the probe. Mirrors power_phase_poll. */
static void poll_once(pp_state *s, int64_t *now, int64_t real_el, int64_t mono_el, int ok, int err) {
    *now += mono_el;
    if (pp_clocks_say_frozen(real_el, mono_el)) pp_on_wake(s, *now);
    pp_on_probe(s, ok, err, *now);
}

static void fresh(void) {
    pp_state s;
    pp_init(&s);
    check(s.phase == PP_NORMAL && !pp_quiet(&s) && pp_net_usable(&s), "a fresh helper is settled and online");
    check(!pp_on_probe(&s, 1, 0, 5) && s.phase == PP_NORMAL, "healthy probes change nothing");
}

static void clocks(void) {
    check(!pp_clocks_say_frozen(5, 5), "an ordinary poll is not a wake");
    check(!pp_clocks_say_frozen(45, 45), "a thread throttled under a game (both clocks, under a minute) is not a wake");
    check(pp_clocks_say_frozen(411, 411), "FW 13.60 Pro, measured: both clocks moved 411 s over a rest");
    check(pp_clocks_say_frozen(5, 284), "FW 13.60 Pro, measured: monotonic moved 284 s, wall clock 5 s (reset on wake)");
    check(pp_clocks_say_frozen(3600, 5), "FW 13.60 rest: wall clock ahead, monotonic stopped");
    check(pp_clocks_say_frozen(20, 5), "a short rest (15 s gap) still counts");
    check(!pp_clocks_say_frozen(14, 5), "a few seconds of drift does not");
    check(!pp_clocks_say_frozen(-3600, 5), "a clock set back is not a wake");
}

static void errnos(void) {
    check(pp_errno_means_down(PP_ERRNO_SONY_REST), "errno 163 means the network is going away");
    check(pp_errno_means_down(ENETDOWN) && pp_errno_means_down(ENETUNREACH), "ENETDOWN/ENETUNREACH too");
    check(!pp_errno_means_down(EMFILE) && !pp_errno_means_down(ENOBUFS) && !pp_errno_means_down(EINTR),
          "running out of descriptors or buffers is not a power transition");
}

/* Our own standby: quiet at once, through the freeze, and for a settle minute after the wake. */
static void standby_cycle(void) {
    pp_state s;
    int64_t now = 1000;
    pp_init(&s);
    check(pp_on_standby(&s, now) && s.phase == PP_GOING_DOWN, "standby request: going down");
    check(pp_quiet(&s) && !pp_net_usable(&s), "standby request: no fan/kernel work, tailnet paused");
    /* ~25 s of errno 163 before the freeze */
    for (int i = 0; i < 5; i++) poll_once(&s, &now, 5, 5, 0, PP_ERRNO_SONY_REST);
    check(s.phase == PP_GOING_DOWN && s.why == PP_WHY_STANDBY, "errno 163 while going down keeps it there");
    /* frozen for an hour; the first poll after the wake sees the gap; the network is not back yet */
    poll_once(&s, &now, 3600, 5, 0, PP_ERRNO_SONY_REST);
    check(s.phase == PP_WAKING && s.wakes == 1, "the clock gap marks the wake");
    check(pp_quiet(&s) && !pp_net_usable(&s), "just woke, network not back: everything held");
    poll_once(&s, &now, 5, 5, 1, 0);
    check(!pp_net_usable(&s), "one healthy probe is not enough for the tailnet");
    poll_once(&s, &now, 5, 5, 1, 0);
    check(pp_net_usable(&s) && pp_quiet(&s), "two healthy probes: tailnet back, fan/kernel still held");
    int polls = 0;
    while (s.phase != PP_NORMAL && polls < 100) {
        poll_once(&s, &now, 5, 5, 1, 0);
        polls++;
    }
    check(s.phase == PP_NORMAL, "settles");
    check(now - s.wake_s >= PP_SETTLE_S && now - s.wake_s < PP_SETTLE_S + 10,
          "fan/kernel work resumes about a minute of running time after the wake, not earlier");
}

/* The console goes to rest by itself (power button, auto rest): only the probe can tell. */
static void outside_standby(void) {
    pp_state s;
    int64_t now = 0;
    pp_init(&s);
    poll_once(&s, &now, 5, 5, 0, PP_ERRNO_SONY_REST);
    check(s.phase == PP_GOING_DOWN && s.why == PP_WHY_NET && !pp_net_usable(&s),
          "first errno 163 from the probe: going down, tailnet paused within one poll");
    poll_once(&s, &now, 7200, 5, 1, 0);
    check(s.phase == PP_WAKING, "frozen, then woken");
}

/* A blip that is not a rest: back to normal once the probe has been healthy for a while. */
static void blip(void) {
    pp_state s;
    int64_t now = 0;
    pp_init(&s);
    poll_once(&s, &now, 5, 5, 0, ENETDOWN);
    poll_once(&s, &now, 5, 5, 1, 0);
    poll_once(&s, &now, 5, 5, 1, 0);
    check(s.phase == PP_GOING_DOWN, "two healthy probes right after: still holding");
    for (int i = 0; i < 10 && s.phase != PP_NORMAL; i++) poll_once(&s, &now, 5, 5, 1, 0);
    check(s.phase == PP_NORMAL && now - s.down_s >= PP_NET_HOLD_S, "back to normal after the hold");
    /* a probe failing with a resource errno never counts */
    poll_once(&s, &now, 5, 5, 0, EMFILE);
    check(s.phase == PP_NORMAL, "EMFILE from the probe is not a transition");
}

/* power.control standby that the console ignored: not quiet forever. */
static void ignored_standby(void) {
    pp_state s;
    int64_t now = 0;
    pp_init(&s);
    pp_on_standby(&s, now);
    for (int i = 0; i < 30; i++) poll_once(&s, &now, 5, 5, 1, 0);
    check(s.phase == PP_GOING_DOWN, "two and a half minutes on: still holding (rest can take ~1.5 min)");
    for (int i = 0; i < 10; i++) poll_once(&s, &now, 5, 5, 1, 0);
    check(s.phase == PP_NORMAL, "after the standby hold: normal again");
}

/* Maintenance wakes in rest: the console wakes, works a little and goes back down, again and again. */
static void dark_wakes(void) {
    pp_state s;
    int64_t now = 0;
    pp_init(&s);
    pp_on_standby(&s, now);
    for (int cycle = 0; cycle < 50; cycle++) {
        poll_once(&s, &now, 1800, 5, 1, 0);                      /* woke */
        poll_once(&s, &now, 5, 5, 1, 0);                         /* network back: tailnet back */
        poll_once(&s, &now, 5, 5, 0, PP_ERRNO_SONY_REST);        /* going down again */
        if (!pp_quiet(&s) || pp_net_usable(&s)) break;
    }
    check(pp_quiet(&s) && !pp_net_usable(&s), "a wake that goes straight back down never reaches fan/kernel work");
    check(s.wakes == 50, "every wake counted");
}

int main(void) {
    printf("power_phase_selftest\n");
    fresh();
    clocks();
    errnos();
    standby_cycle();
    outside_standby();
    blip();
    ignored_standby();
    dark_wakes();
    printf("power_phase_selftest: %s\n", failures ? "FAILED" : "ok");
    return failures ? 1 : 0;
}
