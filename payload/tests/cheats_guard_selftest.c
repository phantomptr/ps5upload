/* Host-side tests for cheats_guard.c (#550): the engine switch is remembered across helper starts,
 * and a game whose saved cheats keep crashing it is paused instead of re-applied forever. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "../src/cheats_guard.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static void write_file(const char *dir, const char *name, const char *text) {
    char path[512];
    snprintf(path, sizeof(path), "%s/%s", dir, name);
    FILE *f = fopen(path, "w");
    if (!f) return;
    fputs(text, f);
    fclose(f);
}

static void test_engine(const char *dir) {
    CHECK(cheats_engine_load(dir) == 0); /* a fresh install starts off, as before */
    CHECK(cheats_engine_save(dir, 1) == 0);
    CHECK(cheats_engine_load(dir) == 1); /* the next helper start reads it back */
    CHECK(cheats_engine_save(dir, 0) == 0);
    CHECK(cheats_engine_load(dir) == 0); /* an explicit Off is remembered too */
    write_file(dir, "engine.json", "{ \"enabled\" : true }");
    CHECK(cheats_engine_load(dir) == 1);
    write_file(dir, "engine.json", "garbage");
    CHECK(cheats_engine_load(dir) == 0);
    write_file(dir, "engine.json", "");
    CHECK(cheats_engine_load(dir) == 0);
    CHECK(cheats_engine_load("/nonexistent/dir") == 0);
    CHECK(cheats_engine_save("/nonexistent/dir", 1) == -1);
}

static void test_hold(const char *dir) {
    CHECK(cheats_held(dir, "CUSA04402") == 0);
    CHECK(cheats_hold(dir, "CUSA04402") == 0);
    CHECK(cheats_held(dir, "CUSA04402") == 1);
    CHECK(cheats_held(dir, "PPSA01234") == 0);
    cheats_unhold(dir, "CUSA04402");
    CHECK(cheats_held(dir, "CUSA04402") == 0);
    cheats_unhold(dir, "CUSA04402"); /* lifting no pause is fine */
    /* Never a path out of the folder. */
    CHECK(cheats_hold(dir, "../x") == -1);
    CHECK(cheats_hold(dir, "") == -1);
    CHECK(cheats_held(dir, "../x") == 0);
}

static void test_marker(const char *dir) {
    char t[CHEATS_GUARD_TITLE_MAX];
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 0 && t[0] == '\0');
    CHECK(cheats_marker_set(dir, "CUSA04402") == 0);
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 1 && strcmp(t, "CUSA04402") == 0);
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 0); /* taking it removed it */
    CHECK(cheats_marker_set(dir, "CUSA04402") == 0);
    cheats_marker_clear(dir);
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 0);
    write_file(dir, "applying", "../../etc\n");
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 0); /* a garbled marker pauses nothing */
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 0); /* and is gone */
    write_file(dir, "applying", "PPSA01234\n");
    CHECK(cheats_marker_take(dir, t, sizeof(t)) == 1 && strcmp(t, "PPSA01234") == 0);
}

static void test_guard(void) {
    cheats_guard_t g;
    memset(&g, 0, sizeof(g));
    const uint64_t W = CHEATS_GUARD_WINDOW_MS;

    CHECK(cheats_guard_tick(&g, 100, 0) == CHEATS_GUARD_NONE); /* nothing applied yet */

    /* The game runs past the window: survived, and stays quiet after. */
    cheats_guard_applied(&g, "CUSA04402", 100, 1000);
    CHECK(cheats_guard_tick(&g, 100, 4000) == CHEATS_GUARD_NONE);
    CHECK(cheats_guard_tick(&g, 100, 1000 + W) == CHEATS_GUARD_SURVIVED);
    CHECK(cheats_guard_tick(&g, 100, 1000 + 2 * W) == CHEATS_GUARD_NONE);

    /* Closed inside the window: one strike; relaunched and closed again: paused. */
    cheats_guard_applied(&g, "CUSA04402", 200, 100000);
    CHECK(cheats_guard_tick(&g, 0, 105000) == CHEATS_GUARD_STRIKE);
    CHECK(strcmp(g.strike_title, "CUSA04402") == 0);
    cheats_guard_applied(&g, "CUSA04402", 201, 110000);
    CHECK(cheats_guard_tick(&g, 0, 112000) == CHEATS_GUARD_HOLD);
    CHECK(strcmp(g.strike_title, "CUSA04402") == 0);

    /* A relaunch straight away (a new pid, no tick without a game in between) counts too. */
    memset(&g, 0, sizeof(g));
    cheats_guard_applied(&g, "CUSA04402", 300, 0);
    CHECK(cheats_guard_tick(&g, 301, 3000) == CHEATS_GUARD_STRIKE);
    cheats_guard_applied(&g, "CUSA04402", 301, 3000);
    CHECK(cheats_guard_tick(&g, 302, 6000) == CHEATS_GUARD_HOLD);

    /* A run that survives in between resets the count. */
    memset(&g, 0, sizeof(g));
    cheats_guard_applied(&g, "CUSA04402", 400, 0);
    CHECK(cheats_guard_tick(&g, 0, 1000) == CHEATS_GUARD_STRIKE);
    cheats_guard_applied(&g, "CUSA04402", 401, 2000);
    CHECK(cheats_guard_tick(&g, 401, 2000 + W) == CHEATS_GUARD_SURVIVED);
    cheats_guard_applied(&g, "CUSA04402", 402, 100000);
    CHECK(cheats_guard_tick(&g, 0, 101000) == CHEATS_GUARD_STRIKE); /* one again, not two */

    /* A later cheat's window in the same run (#627 review): surviving the first window does not
     * clear the count, closing inside the second one is a strike; twice is a pause. */
    memset(&g, 0, sizeof(g));
    for (int run = 0; run < 2; run++) {
        int pid = 600 + run;
        uint64_t t0 = 1000000u * (uint64_t)(run + 1);
        cheats_guard_applied(&g, "CUSA04402", pid, t0 + 3000);
        CHECK(cheats_guard_tick(&g, pid, t0 + 3000 + W) == CHEATS_GUARD_SURVIVED);
        CHECK(cheats_guard_tick(&g, pid, t0 + 50000) == CHEATS_GUARD_NONE);
        cheats_guard_applied(&g, "CUSA04402", pid, t0 + 60000);
        CHECK(cheats_guard_tick(&g, 0, t0 + 66000) ==
              (run == 0 ? CHEATS_GUARD_STRIKE : CHEATS_GUARD_HOLD));
    }
    /* ...but a run that plays past its windows and then closes does clear it */
    memset(&g, 0, sizeof(g));
    cheats_guard_applied(&g, "CUSA04402", 700, 0);
    CHECK(cheats_guard_tick(&g, 0, 1000) == CHEATS_GUARD_STRIKE);
    cheats_guard_applied(&g, "CUSA04402", 701, 10000);
    CHECK(cheats_guard_tick(&g, 701, 10000 + W) == CHEATS_GUARD_SURVIVED);
    CHECK(g.strikes == 1);                                 /* the run is not over yet */
    CHECK(cheats_guard_tick(&g, 0, 500000) == CHEATS_GUARD_NONE); /* closed later */
    CHECK(g.strikes == 0);

    /* Quick closes of two different games are not "in a row" for either. */
    memset(&g, 0, sizeof(g));
    cheats_guard_applied(&g, "CUSA04402", 500, 0);
    CHECK(cheats_guard_tick(&g, 0, 1000) == CHEATS_GUARD_STRIKE);
    cheats_guard_applied(&g, "PPSA01234", 501, 2000);
    CHECK(cheats_guard_tick(&g, 0, 3000) == CHEATS_GUARD_STRIKE);
    CHECK(strcmp(g.strike_title, "PPSA01234") == 0);
}

int main(void) {
    char dir[] = "/tmp/ps5upload-cheats-guard-XXXXXX";
    if (!mkdtemp(dir)) {
        perror("mkdtemp");
        return 1;
    }
    test_engine(dir);
    test_hold(dir);
    test_marker(dir);
    test_guard();
    char cmd[600];
    snprintf(cmd, sizeof(cmd), "rm -rf '%s'", dir);
    if (system(cmd) != 0) fprintf(stderr, "could not remove %s\n", dir);
    if (failures) {
        fprintf(stderr, "cheats_guard_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_guard_selftest: ok\n");
    return 0;
}
