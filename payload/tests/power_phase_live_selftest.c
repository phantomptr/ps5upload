/* power_phase.c itself on the host: the loopback probe is healthy on a working machine, our standby
 * request pauses the tailnet node at once, a clock gap is a wake, the node comes back once the
 * network is, and the fan/kernel hold lasts the settle minute. The node and the fan are stubs that
 * record what they were asked. Every [power] line goes to stderr; /data does not exist here, so
 * power.log and the stderr.log rotation are skipped. */
#include "power_phase.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

pthread_mutex_t sony_api_lock = PTHREAD_MUTEX_INITIALIZER;
pthread_mutex_t kernel_rw_lock = PTHREAD_MUTEX_INITIALIZER;

static int g_paused, g_pause_calls;
int tailnet_node_set_paused(int paused) {
    g_pause_calls++;
    g_paused = paused;
    return 0;
}
const char *tailnet_node_brief(void) { return g_paused ? "on,paused" : "on"; }
static int g_down_pending;
int tailnet_node_take_down(int *err, char *what, size_t cap) {
    if (!g_down_pending) return 0;
    g_down_pending = 0;
    *err = 163;
    snprintf(what, cap, "network going away (errno 163 on udp receive)");
    return 1;
}
int hw_fan_pinned_threshold(void) { return 0; }

static int failures;
static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    printf("power_phase_live_selftest\n");
    check(power_phase_poll(5, 5) == PP_NORMAL && !power_phase_quiet(), "a working machine: the probe is healthy");
    check(!g_paused, "tailnet left running");

    power_phase_note_standby();
    check(power_phase_current() == PP_GOING_DOWN && power_phase_quiet(), "standby request: going down");
    check(g_paused, "standby request: tailnet paused at once");

    /* the sony lock held across the standby call shows in the transition line (by eye) */
    pthread_mutex_lock(&sony_api_lock);
    check(power_phase_poll(3600, 5) == PP_WAKING, "an hour of wall clock over 5 s of monotonic: woke");
    pthread_mutex_unlock(&sony_api_lock);
    check(g_paused, "one healthy probe after the wake: tailnet still paused");
    power_phase_poll(5, 5);
    check(!g_paused && power_phase_quiet(), "two healthy probes: tailnet back, fan/kernel work still held");

    /* the settle minute runs on the monotonic clock: wait it out for real would take a minute, so
     * check only that a poll right away keeps holding */
    check(power_phase_poll(5, 5) == PP_WAKING, "inside the settle minute: still waking");

    /* a throttled thread (both clocks overran, under a minute) is not a wake; past a minute both
     * clocks moving is a rest on FW 13.60 (measured), see pp_clocks_say_frozen */
    int calls = g_pause_calls;
    power_phase_poll(45, 45);
    check(g_pause_calls >= calls && !g_paused, "a throttled poll does not pause the tailnet");

    /* the tailnet core closed its sockets by itself (a rest the console started on its own): the next
     * poll is going down and keeps the node paused; a freeze after it is a wake as usual */
    g_down_pending = 1;
    check(power_phase_poll(5, 5) == PP_GOING_DOWN && g_paused, "the core closed itself: going down, node kept paused");
    check(!g_down_pending, "the signal is taken once");
    check(power_phase_poll(5, 5) == PP_GOING_DOWN && g_paused, "healthy probes inside the hold: still paused");
    g_down_pending = 1;
    check(power_phase_poll(1800, 5) == PP_WAKING && g_paused, "closed, then frozen: woke, paused until the network is back");

    printf("power_phase_live_selftest: %s\n", failures ? "FAILED" : "ok");
    return failures ? 1 : 0;
}
