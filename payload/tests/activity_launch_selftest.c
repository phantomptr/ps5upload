/* Host selftest for play-time launch/resume bookkeeping.
 *
 * Reloading the helper while a game was running counted as a fresh
 * launch, so "times played" climbed every time the payload was sent and
 * drifted upward with no relation to how often the game was actually
 * started. Observed directly: a PS2 title went from launches=1 to
 * launches=2 across one reload, with no relaunch.
 *
 * The rule is now "only count launches we witnessed": the first poll
 * after start adopts whatever is already running, and only later polls
 * can record a launch.
 *
 * The arithmetic is copied rather than included, because activity.c
 * pulls in PS5 headers that do not exist on the host — same approach as
 * the other selftests here. What this pins is the accounting: that a
 * resume banks an open session instead of dropping or double-counting
 * it, and that it never increments the launch count. */
#include <stdio.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    uint64_t launches;
    uint64_t total_seconds;
    int64_t  last_launch_ts;
    int64_t  last_seen_ts;
    int64_t  session_started_ts;
} entry_t;

#define LAUNCH_DEBOUNCE_SEC 5

static void record_launch(entry_t *e, int64_t now) {
    if (e->session_started_ts > 0) {
        if (now >= e->last_launch_ts && now - e->last_launch_ts < LAUNCH_DEBOUNCE_SEC) {
            e->last_seen_ts = now;
            return;
        }
        if (e->last_seen_ts > e->session_started_ts)
            e->total_seconds += (uint64_t)(e->last_seen_ts - e->session_started_ts);
        e->session_started_ts = 0;
    }
    e->launches++;
    e->session_started_ts = now;
    e->last_launch_ts = now;
    e->last_seen_ts = now;
}

static void record_resume(entry_t *e, int64_t now) {
    if (e->session_started_ts > 0 && e->last_seen_ts > e->session_started_ts) {
        e->total_seconds += (uint64_t)(e->last_seen_ts - e->session_started_ts);
    }
    e->session_started_ts = now;
    e->last_seen_ts = now;
}

static int failures = 0;
static void check(int cond, const char *what) {
    if (!cond) { printf("  FAIL: %s\n", what); failures++; }
}

int main(void) {
    /* A real launch we witnessed. */
    entry_t e = {0};
    record_launch(&e, 1000);
    check(e.launches == 1, "a witnessed launch counts");

    /* Played for 300s, saved along the way. */
    e.last_seen_ts = 1300;

    /* The helper is reloaded. The game never stopped, so the first poll
     * after start adopts it. This is the regression: it must not count. */
    record_resume(&e, 1400);
    check(e.launches == 1, "a reload mid-session does not count as a launch");
    check(e.total_seconds == 300,
          "the open session is banked on resume, not lost");
    check(e.session_started_ts == 1400, "the session clock restarts on resume");

    /* Keep playing, then genuinely relaunch later. */
    e.last_seen_ts = 1500;
    record_launch(&e, 2000);
    check(e.launches == 2, "a later real launch still counts");
    check(e.total_seconds == 400,
          "the resumed session's time is banked too, not double-counted");

    /* Repeated reloads must not inflate the count however many times. */
    for (int i = 0; i < 10; i++) {
        e.last_seen_ts = 2000 + (i + 1) * 10;
        record_resume(&e, 2000 + (i + 1) * 10);
    }
    check(e.launches == 2, "ten reloads add zero launches");

    /* An adopted game must be visible while it is being played.
     *
     * Launches and banked seconds are only written when a session ends,
     * so a game adopted at startup has zero of both. Filtering rows on
     * those alone hid the game the user was actually playing: it showed
     * as "playing now" with no row to show for it. An open session is
     * enough to report. */
    entry_t adopted = {0};
    record_resume(&adopted, 9000);
    check(adopted.launches == 0 && adopted.total_seconds == 0,
          "an adopted game starts with nothing banked");
    check(!(adopted.launches == 0 && adopted.total_seconds == 0 &&
            adopted.session_started_ts == 0),
          "but an open session makes it reportable");

    /* A genuinely empty entry stays hidden. */
    entry_t empty = {0};
    check(empty.launches == 0 && empty.total_seconds == 0 &&
          empty.session_started_ts == 0,
          "an entry with no history and no session is still skipped");

    /* The debounce still protects against a double-fire launch. */
    entry_t d = {0};
    record_launch(&d, 5000);
    record_launch(&d, 5002);   /* inside the debounce window */
    check(d.launches == 1, "a bounced launch is not counted twice");

    /* The clock moved back mid-session (this app can set it, and NTP can step it). The span is
     * negative: cast to unsigned it was saved as 18 quintillion seconds. And a launch after the step
     * is a launch, not a bounce: the debounce only applies when the clock ran forward. */
    entry_t back = {0};
    record_launch(&back, 10000);
    back.last_seen_ts = 10600;
    record_launch(&back, 3000);        /* an hour-odd earlier than the session's start */
    check(back.total_seconds == 600, "the time played before the step is banked");
    check(back.launches == 2, "a launch after the clock moved back still counts");
    back.last_seen_ts = 2000;          /* seen before its own session began */
    back.last_launch_ts = 2000;
    record_launch(&back, 2010);
    check(back.total_seconds == 600, "a span that runs backwards adds nothing");
    check(back.total_seconds < 1000000, "the total never underflows");

    if (failures == 0) {
        printf("✓ play-time counts only launches it witnessed\n");
        return 0;
    }
    printf("✗ %d failure(s)\n", failures);
    return 1;
}
