/* Host-side tests for cheats_session.c (#627): saved cheats apply once the game is ready, are
 * checked and put right again when the game undoes them, and every outcome is visible. The game
 * is a fake: each cheat's location holds the original bytes, the cheat's bytes, or other bytes,
 * and may not be loaded yet. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/cheats_session.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

enum { B_ORIG = 0, B_ON, B_OTHER };

typedef struct {
    int on;           /* switched on */
    int load_after;   /* not loaded until this many passes have run */
    int bytes;        /* B_* */
    int write_fails;
} fake_cheat_t;

typedef struct {
    int engine;
    int held;
    int passes;
    int n;
    fake_cheat_t c[4];
    int applies, writes, marker_sets, marker_clears, guard_calls, notes, warn_notes;
    char last_note[256];
} fake_t;

static fake_t F;

static int f_engine(void *ctx) { return ((fake_t *)ctx)->engine; }
static int f_held(void *ctx, const char *title) { (void)title; return ((fake_t *)ctx)->held; }

static int f_list(void *ctx, const char *title, cheats_item_t *items, int max, char *game, size_t cap) {
    fake_t *f = (fake_t *)ctx;
    (void)title;
    int k = 0;
    for (int i = 0; i < f->n && k < max; i++) {
        if (!f->c[i].on) continue;
        memset(&items[k], 0, sizeof items[k]);
        items[k].key = 1000 + (uint64_t)i;
        items[k].flat = i;
        items[k].slot = i;
        snprintf(items[k].name, sizeof items[k].name, "Cheat %d", i);
        k++;
    }
    snprintf(game, cap, "Shadow of War");
    return k;
}

static int f_apply(void *ctx, cheats_session_t *s, const cheats_item_t *it, int reapply, char *why, size_t cap) {
    fake_t *f = (fake_t *)ctx;
    fake_cheat_t *c = &f->c[it->slot];
    (void)reapply;
    f->applies++;
    if (!c->on || !f->engine) return CHEATS_OUT_GONE;
    if (f->passes < c->load_after) {
        snprintf(why, cap, "the game's code is not loaded yet; try again");
        return CHEATS_OUT_WAITING;
    }
    if (c->bytes == B_ON) return CHEATS_OUT_APPLIED;
    if (c->bytes == B_OTHER) {
        snprintf(why, cap, "cheat does not match this game build");
        return CHEATS_OUT_FAILED;
    }
    cheats_session_before_write(s);
    f->writes++;
    if (c->write_fails) {
        snprintf(why, cap, "write failed");
        return CHEATS_OUT_FAILED;
    }
    c->bytes = B_ON;
    return CHEATS_OUT_APPLIED;
}

static int f_verify(void *ctx, const cheats_item_t *it) {
    fake_cheat_t *c = &((fake_t *)ctx)->c[it->slot];
    if (c->bytes == B_ON) return CHEATS_VERIFY_ON;
    if (c->bytes == B_ORIG) return CHEATS_VERIFY_REVERTED;
    return CHEATS_VERIFY_CHANGED;
}

static void f_marker_set(void *ctx, const char *t) { (void)t; ((fake_t *)ctx)->marker_sets++; }
static void f_marker_clear(void *ctx) { ((fake_t *)ctx)->marker_clears++; }
static void f_guard(void *ctx, const char *t, int pid, uint64_t now) {
    (void)t; (void)pid; (void)now;
    ((fake_t *)ctx)->guard_calls++;
}
static void f_notify(void *ctx, const char *msg, int warn) {
    fake_t *f = (fake_t *)ctx;
    f->notes++;
    if (warn) f->warn_notes++;
    snprintf(f->last_note, sizeof f->last_note, "%s", msg);
}
static void f_log(void *ctx, const char *msg) { (void)ctx; (void)msg; }

static const cheats_session_ops_t OPS = {
    &F, f_engine, f_held, f_list, f_apply, f_verify, f_marker_set, f_marker_clear, f_guard,
    f_notify, f_log,
};

static cheats_session_t S;
static uint64_t now_ms;

static void setup(int n) {
    memset(&F, 0, sizeof F);
    F.engine = 1;
    F.n = n;
    for (int i = 0; i < n; i++) F.c[i].on = 1;
    cheats_session_reset(&S);
    now_ms = 1000000;
}

static int pass_pid(int pid) {
    int r = cheats_session_pass(&S, pid, "CUSA04402", now_ms, 0, &OPS);
    F.passes++;
    now_ms += 3000;
    return r;
}
static int pass(void) { return pass_pid(106); }

static int status(int flat) { return cheats_session_status(&S, flat, NULL); }

/* The report: the code is not loaded for the first passes; it applies on the first ready pass. */
static void test_waits_then_applies(void) {
    setup(1);
    F.c[0].load_after = 4;
    for (int i = 0; i < 4; i++) {
        CHECK(pass() == 0);
        CHECK(status(0) == CHEATS_ST_WAITING);
    }
    const char *why = NULL;
    CHECK(cheats_session_status(&S, 0, &why) == CHEATS_ST_WAITING && strstr(why, "not loaded"));
    CHECK(F.marker_sets == 0 && F.guard_calls == 0 && F.notes == 0); /* nothing written yet */
    CHECK(pass() == 1);
    CHECK(status(0) == CHEATS_ST_APPLIED);
    CHECK(F.writes == 1 && F.marker_sets == 1 && F.marker_clears == 0 && F.guard_calls == 1);
    CHECK(F.notes == 1 && strcmp(F.last_note, "1 cheat applied to Shadow of War") == 0);
    /* nothing more to do while it stays applied */
    for (int i = 0; i < 10; i++) CHECK(pass() == 0);
    CHECK(F.writes == 1 && F.notes == 1 && F.guard_calls == 1);
}

/* Bytes that are neither the original nor the cheat: failed, said once, never tried again. */
static void test_wrong_version(void) {
    setup(1);
    F.c[0].bytes = B_OTHER;
    pass();
    CHECK(status(0) == CHEATS_ST_FAILED);
    CHECK(F.applies == 1 && F.writes == 0 && F.warn_notes == 1);
    CHECK(strstr(F.last_note, "Cheat 0: couldn't apply") == F.last_note);
    for (int i = 0; i < 30; i++) pass();
    CHECK(F.applies == 1 && F.notes == 1 && F.guard_calls == 0 && F.marker_sets == 0);
}

/* Applied, then the game reloads its code: put back, without a second announcement or guard. */
static void test_reverted_is_reapplied(void) {
    setup(1);
    pass();
    CHECK(status(0) == CHEATS_ST_APPLIED && F.writes == 1);
    F.c[0].bytes = B_ORIG;
    pass();
    pass(); /* re-read every second pass */
    CHECK(status(0) == CHEATS_ST_APPLIED && F.writes == 2 && F.c[0].bytes == B_ON);
    CHECK(F.guard_calls == 1 && F.marker_sets == 1 && F.notes == 1);
}

/* The game keeps undoing it: stop after the limit, with the reason. */
static void test_reverts_bounded(void) {
    setup(1);
    pass();
    int writes_before = F.writes;
    for (int i = 0; i < 40 && status(0) == CHEATS_ST_APPLIED; i++) {
        F.c[0].bytes = B_ORIG;
        pass();
    }
    const char *why = NULL;
    CHECK(cheats_session_status(&S, 0, &why) == CHEATS_ST_FAILED);
    CHECK(strcmp(why, "the game keeps undoing it") == 0);
    CHECK(F.writes - writes_before == CHEATS_SESSION_REVERT_MAX);
    int w = F.writes;
    for (int i = 0; i < 10; i++) pass();
    CHECK(F.writes == w);
    CHECK(F.warn_notes == 1);

    /* spread out past the window, reverts are fine */
    setup(1);
    pass();
    for (int i = 0; i < 12; i++) {
        F.c[0].bytes = B_ORIG;
        now_ms += 60000;
        pass();
        pass();
    }
    CHECK(status(0) == CHEATS_ST_APPLIED && F.writes == 13);
}

/* Something else wrote there: left alone, said once. */
static void test_changed_left_alone(void) {
    setup(1);
    pass();
    F.c[0].bytes = B_OTHER;
    pass();
    pass();
    CHECK(status(0) == CHEATS_ST_CHANGED && F.writes == 1);
    CHECK(F.warn_notes == 1 && strstr(F.last_note, "left it alone"));
    for (int i = 0; i < 10; i++) pass();
    CHECK(F.writes == 1 && F.c[0].bytes == B_OTHER && F.warn_notes == 1);
}

static void test_held_and_engine_off(void) {
    setup(2);
    F.held = 1;
    for (int i = 0; i < 5; i++) pass();
    CHECK(F.applies == 0 && F.notes == 1 && strstr(F.last_note, "paused"));
    CHECK(status(0) == CHEATS_ST_NONE);
    F.held = 0; /* a switch flipped lifts the pause: the next pass applies */
    pass();
    CHECK(status(0) == CHEATS_ST_APPLIED && status(1) == CHEATS_ST_APPLIED);

    setup(2);
    F.engine = 0;
    for (int i = 0; i < 5; i++) pass();
    CHECK(F.applies == 0 && F.notes == 0 && S.n == 0);
    F.engine = 1;
    pass();
    CHECK(F.writes == 2 && F.guard_calls == 1 && F.notes == 1);
    CHECK(strcmp(F.last_note, "2 cheats applied to Shadow of War") == 0);
    F.engine = 0; /* off mid-game: forgotten */
    pass();
    CHECK(S.n == 0 && status(0) == CHEATS_ST_NONE);
}

/* A new process is a fresh session, guard and announcement included. */
static void test_new_pid(void) {
    setup(1);
    pass_pid(106);
    CHECK(F.guard_calls == 1);
    F.c[0].bytes = B_ORIG; /* the new process starts with its own code */
    pass_pid(0);           /* the game closed */
    CHECK(S.n == 0);
    pass_pid(200);
    CHECK(status(0) == CHEATS_ST_APPLIED && F.writes == 2 && F.guard_calls == 2 && F.notes == 2);
    CHECK(F.marker_sets == 2);
}

/* The guard counts once: from the first apply, not from later cheats or re-applies. */
static void test_guard_once(void) {
    setup(2);
    F.c[1].load_after = 3;
    pass();
    CHECK(status(0) == CHEATS_ST_APPLIED && status(1) == CHEATS_ST_WAITING);
    CHECK(F.guard_calls == 1 && F.marker_sets == 1);
    CHECK(F.notes == 0); /* still waiting for cheat 1, and not 30 s yet */
    for (int i = 0; i < 3; i++) pass();
    CHECK(status(1) == CHEATS_ST_APPLIED);
    CHECK(F.guard_calls == 1 && F.marker_sets == 1);
    CHECK(F.notes == 1 && strcmp(F.last_note, "2 cheats applied to Shadow of War") == 0);

    /* one never loads: what applied is announced after 30 s, the late one on its own later */
    setup(2);
    F.c[1].load_after = 30;
    for (int i = 0; i < 11; i++) pass();
    CHECK(F.notes == 1 && strcmp(F.last_note, "1 cheat applied to Shadow of War") == 0);
    for (int i = 0; i < 40; i++) pass();
    CHECK(status(1) == CHEATS_ST_APPLIED);
    CHECK(F.notes == 2 && strcmp(F.last_note, "1 more cheat applied to Shadow of War") == 0);
    CHECK(F.guard_calls == 1);
}

/* A write that fails does not leave the marker behind. */
static void test_marker_cleared_on_failed_write(void) {
    setup(1);
    F.c[0].write_fails = 1;
    pass();
    CHECK(status(0) == CHEATS_ST_FAILED);
    CHECK(F.marker_sets == 1 && F.marker_clears == 1 && F.guard_calls == 0);
}

/* After a minute of waiting it is tried every 5th pass, for as long as the game runs. */
static void test_waiting_backs_off(void) {
    setup(1);
    F.c[0].load_after = 1000;
    for (int i = 0; i < CHEATS_SESSION_FAST_TRIES; i++) pass();
    CHECK(F.applies == CHEATS_SESSION_FAST_TRIES);
    for (int i = 0; i < 50; i++) pass();
    CHECK(F.applies == CHEATS_SESSION_FAST_TRIES + 10);
    F.c[0].load_after = 0;
    for (int i = 0; i < CHEATS_SESSION_SLOW_EVERY; i++) pass();
    CHECK(status(0) == CHEATS_ST_APPLIED);
}

/* Switches flipped by hand: on is applied quietly, off is forgotten; Off/On starts it over. */
static void test_manual(void) {
    setup(1);
    F.c[0].bytes = B_OTHER;
    pass();
    CHECK(status(0) == CHEATS_ST_FAILED);
    F.c[0].on = 0;
    cheats_session_manual(&S, 1000, 0);
    CHECK(status(0) == CHEATS_ST_NONE);
    F.c[0].on = 1;
    F.c[0].bytes = B_ON; /* the toggle wrote it */
    cheats_session_manual(&S, 1000, 1);
    pass();
    CHECK(status(0) == CHEATS_ST_APPLIED && F.notes == 1); /* only the failure, no summary */

    /* switched off elsewhere: dropped at the next pass */
    F.c[0].on = 0;
    pass();
    CHECK(S.n == 0);
}

/* An explicit re-apply: at once, announced, even paused, never watched by the guard. */
static void test_force(void) {
    setup(1);
    F.held = 1;
    cheats_session_pass(&S, 106, "CUSA04402", now_ms, 1, &OPS);
    CHECK(status(0) == CHEATS_ST_APPLIED && F.guard_calls == 0 && F.marker_sets == 0);
    CHECK(strcmp(F.last_note, "1 cheat applied to Shadow of War") == 0);
    F.c[0].on = 0;
    cheats_session_pass(&S, 106, "CUSA04402", now_ms, 1, &OPS);
    CHECK(strcmp(F.last_note, "No cheats enabled for Shadow of War") == 0);
}

int main(void) {
    test_waits_then_applies();
    test_wrong_version();
    test_reverted_is_reapplied();
    test_reverts_bounded();
    test_changed_left_alone();
    test_held_and_engine_off();
    test_new_pid();
    test_guard_once();
    test_marker_cleared_on_failed_write();
    test_waiting_backs_off();
    test_manual();
    test_force();
    CHECK(strcmp(cheats_status_name(CHEATS_ST_WAITING), "waiting") == 0);
    CHECK(cheats_status_name(CHEATS_ST_NONE) == NULL);
    if (failures) {
        fprintf(stderr, "cheats_session_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_session_selftest: ok\n");
    return 0;
}
