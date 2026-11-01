/* Host selftest for play_pick(): which running title gets the play time.
 *
 * The tracker credited the first title process in the kernel's list, so a
 * game left suspended in the background collected the hours of the game
 * actually being played. Each case below is one way that went wrong. */
#include <stdio.h>
#include <string.h>

#include "play_pick.h"

static int failures = 0;
static void check(int cond, const char *what) {
    if (!cond) { printf("  FAIL: %s\n", what); failures++; }
}

static play_proc_t P(int pid, const char *tid, uint64_t rt, int stopped) {
    play_proc_t p;
    memset(&p, 0, sizeof(p));
    p.pid = pid;
    snprintf(p.title_id, sizeof(p.title_id), "%s", tid);
    p.runtime_us = rt;
    p.stopped = stopped;
    return p;
}

#define SEC 1000000ull

int main(void) {
    const uint64_t iv = 30 * SEC;

    /* The reported bug: the suspended game is listed first. */
    play_proc_t prev1[] = {P(100, "PPSA00001", 50 * SEC, 0), P(200, "PPSA00002", 900 * SEC, 0)};
    play_proc_t now1[]  = {P(100, "PPSA00001", 50 * SEC + 2000, 0), P(200, "PPSA00002", 960 * SEC, 0)};
    check(play_pick(now1, 2, prev1, 2, iv) == 1, "the game using the CPU wins over the one listed first");

    /* Everything suspended (dashboard, or the console asleep). */
    play_proc_t now2[] = {P(100, "PPSA00001", 50 * SEC + 1000, 0), P(200, "PPSA00002", 900 * SEC + 1000, 0)};
    check(play_pick(now2, 2, prev1, 2, iv) == -1, "nothing being played credits nobody");

    /* A stopped process never counts, even with CPU time behind it. */
    play_proc_t now3[] = {P(100, "PPSA00001", 80 * SEC, 1)};
    play_proc_t prev3[] = {P(100, "PPSA00001", 50 * SEC, 0)};
    check(play_pick(now3, 1, prev3, 1, iv) == -1, "a stopped process is not played");

    /* A game just started next to a suspended one: no delta yet, but it is
     * the only new title, so it is the one. */
    play_proc_t now4[] = {P(100, "PPSA00001", 50 * SEC + 500, 0), P(300, "PPSA00003", 4 * SEC, 0)};
    check(play_pick(now4, 2, prev1, 2, iv) == 1, "a freshly started game is picked");

    /* Two unmeasured titles (helper just started): no guessing. */
    check(play_pick(now1, 2, NULL, 0, iv) == -1, "two unmeasured titles pick nobody");
    check(play_pick(now1, 1, NULL, 0, iv) == 0, "one unmeasured title is picked");

    /* A pid reused by another title is not the old measurement. */
    play_proc_t prev5[] = {P(100, "PPSA00001", 900 * SEC, 0)};
    play_proc_t now5[]  = {P(100, "PPSA00009", 1 * SEC, 0)};
    check(play_pick(now5, 1, prev5, 1, iv) == 0, "a reused pid counts as a new title");
    check(!play_seen_before(prev5, 1, &now5[0]), "a reused pid is not a returning title");
    check(play_seen_before(prev1, 2, &now1[1]), "the same pid and title is a returning title");

    /* Below the threshold: 2% of a core over 30 s is 0.6 s. */
    play_proc_t now6[] = {P(100, "PPSA00001", 50 * SEC + 590000, 0)};
    play_proc_t now7[] = {P(100, "PPSA00001", 50 * SEC + 610000, 0)};
    check(play_pick(now6, 1, prev1, 2, iv) == -1, "under 2% of a core is idle");
    check(play_pick(now7, 1, prev1, 2, iv) == 0, "over 2% of a core is played");

    if (failures) { printf("play_pick selftest: %d failure(s)\n", failures); return 1; }
    printf("play_pick selftest: ok\n");
    return 0;
}
