/* Host proof for the wake watchdog's fail-closed recovery gate. */
#include "wake_watchdog.h"

#include <stdio.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    check(!wake_watchdog_should_recover(5, 5, 0),
          "ordinary poll never recovers");
    check(!wake_watchdog_should_recover(3600, 5, 0),
          "wall-clock/NTP jump alone never performs kernel writes");
    check(!wake_watchdog_should_recover(3600, 3600, 1),
          "BigApp throttle never recovers");
    check(!wake_watchdog_should_recover(3600, 3600, -1),
          "unknown process scan fails closed");
    check(wake_watchdog_should_recover(3600, 3600, 0),
          "two-clock suspend evidence with no BigApp recovers");

    printf("\nwake_watchdog_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
