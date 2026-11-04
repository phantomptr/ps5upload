/* Host proof for the wake watchdog's gate: kernel writes only when the ucred really changed. */
#include "wake_watchdog.h"

#include <stdio.h>
#include <string.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

static wake_ucred_t elevated(void) {
    wake_ucred_t u;
    memset(&u, 0, sizeof u);
    u.authid = 0x4800000000000006ull;
    memset(u.caps, 0xff, sizeof u.caps);
    u.attrs[3] = 0x80;
    return u;
}

static void gate(void) {
    /* Nothing to look at on an ordinary poll, and no write whatever the ucred says. */
    check(!wake_watchdog_should_look(5, 5), "ordinary poll does not read the ucred");
    check(!wake_watchdog_should_recover(5, 5, WAKE_UCRED_CHANGED), "ordinary poll never recovers");

    /* A clock moved with no rest: NTP, the app's clock sync, a date change (#289). */
    check(wake_watchdog_should_look(3600, 5), "wall-clock jump earns a look");
    check(!wake_watchdog_should_recover(3600, 5, WAKE_UCRED_SAME),
          "wall-clock jump with the same ucred performs no kernel write");

    /* A game in the foreground throttled this thread: both clocks overran, nothing slept. */
    check(!wake_watchdog_should_recover(90, 90, WAKE_UCRED_SAME),
          "a throttled thread during a game performs no kernel write");

    /* A real rest with a game open: the game is suspended and resumes, and it is no veto. */
    check(wake_watchdog_should_recover(3600, 3600, WAKE_UCRED_CHANGED),
          "rest counted by both clocks, ucred changed: recovers");
    check(wake_watchdog_should_recover(3600, 5, WAKE_UCRED_CHANGED),
          "rest with CLOCK_MONOTONIC stopped (FW 13.60), ucred changed: recovers");
    check(wake_watchdog_should_recover(-30, 3600, WAKE_UCRED_CHANGED),
          "monotonic jump alone earns a look too (the wall clock was set back meanwhile)");

    /* Unknown is not permission. */
    check(!wake_watchdog_should_recover(3600, 3600, WAKE_UCRED_UNKNOWN),
          "an unreadable ucred fails closed");
    check(!wake_watchdog_should_look(-3600, 5), "a clock set back does not earn a look");
}

static void compare(void) {
    wake_ucred_t a = elevated(), b = elevated();
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_SAME, "identical: same");
    check(wake_ucred_compare(NULL, &b) == WAKE_UCRED_UNKNOWN, "no baseline: unknown");
    check(wake_ucred_compare(&a, NULL) == WAKE_UCRED_UNKNOWN, "no reading: unknown");

    b = elevated();
    b.authid = 0x2000000000000000ull;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "authid put back: changed");
    b = elevated();
    b.authid = 0;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "authid cleared: changed");
    b = elevated();
    b.caps[15] = 0;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "a capability byte dropped: changed");
    b = elevated();
    b.attrs[3] = 0;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "the ptrace attribute dropped: changed");
    b = elevated();
    b.attrs[31] = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "the last attribute byte differs: changed");
    b = elevated();
    b.uid = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "uid: changed");
    b = elevated();
    b.ruid = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "ruid: changed");
    b = elevated();
    b.svuid = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "svuid: changed");
    b = elevated();
    b.rgid = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "rgid: changed");
    b = elevated();
    b.svgid = 1;
    check(wake_ucred_compare(&a, &b) == WAKE_UCRED_CHANGED, "svgid: changed");
}

int main(void) {
    gate();
    compare();
    printf("\nwake_watchdog_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
