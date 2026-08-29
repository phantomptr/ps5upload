/*
 * Host self-test for the sys_time settimeofday fallback decision.
 *
 * Some firmwares export sceSystemServiceSetCurrentDateTime but the
 * runtime SPRX behind it does nothing: the call returns rc=0 and the
 * clock never moves. Others don't export the symbol at all. In both
 * cases the payload can still set the clock through libkernel's
 * settimeofday, which is the path the ps5-date-time-sync reference
 * uses exclusively.
 *
 * Deciding *whether* to take that fallback is the part worth pinning
 * down, so it lives in its own pure predicate. This test exercises it
 * directly and never calls settimeofday — running the real thing on a
 * developer's machine would set the host clock.
 */

#include <stdint.h>
#include <stdio.h>

#include "sys_time.h"

static int failures;

#define CHECK(expr)                                                        \
    do {                                                                   \
        if (!(expr)) {                                                     \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #expr); \
            failures++;                                                    \
        }                                                                  \
    } while (0)

/* A representative target: 2026-05-15 23:30:00 UTC. */
#define TARGET 1778887800LL

int main(void) {
    /* No symbol on this firmware — the Sony path cannot run at all,
     * so the fallback is the only way to set the clock. */
    CHECK(sys_time_needs_fallback(0, 0, TARGET, -1) == 1);

    /* Sony rejected the call (typically the 0x80A2xxxx authid family
     * on a non-elevated loader). Worth trying the syscall directly. */
    CHECK(sys_time_needs_fallback(1, -1, TARGET, -1) == 1);

    /* Sony accepted and the clock actually moved — no fallback. */
    CHECK(sys_time_needs_fallback(1, 0, TARGET, TARGET) == 0);

    /* The stub no-op: rc=0, but the clock is still where it was.
     * This is the case the whole fallback exists for. */
    CHECK(sys_time_needs_fallback(1, 0, TARGET, TARGET - 90000) == 1);

    /* rc=0 but we could not read the clock back (get-side symbol
     * missing). We have no evidence the set failed, so we trust it
     * rather than writing the clock a second time. */
    CHECK(sys_time_needs_fallback(1, 0, TARGET, -1) == 0);

    /* Tolerance boundary. A few seconds of slop is expected: the
     * read-back happens after the set, and the payload's clock API is
     * whole-second granular. */
    CHECK(sys_time_needs_fallback(1, 0, TARGET,
                                  TARGET + SYS_TIME_SET_TOLERANCE_SEC) == 0);
    CHECK(sys_time_needs_fallback(1, 0, TARGET,
                                  TARGET + SYS_TIME_SET_TOLERANCE_SEC + 1) == 1);
    /* Symmetric — the clock can land either side of the target. */
    CHECK(sys_time_needs_fallback(1, 0, TARGET,
                                  TARGET - SYS_TIME_SET_TOLERANCE_SEC) == 0);
    CHECK(sys_time_needs_fallback(1, 0, TARGET,
                                  TARGET - SYS_TIME_SET_TOLERANCE_SEC - 1) == 1);

    /* Extreme inputs must not overflow the difference. `target` is
     * request-controlled (it arrives as JSON from the desktop), so
     * `observed - target` in int64 can wrap — signed overflow is
     * undefined behaviour in C, not merely a wrong answer. The engine
     * hit this same bug in its own stub-no-op heuristic and fixed it
     * by widening; the payload has to do the equivalent. Both of these
     * are enormous mismatches, so both must report "fall back". */
    CHECK(sys_time_needs_fallback(1, 0, INT64_MIN, INT64_MAX) == 1);
    CHECK(sys_time_needs_fallback(1, 0, INT64_MAX, 0) == 1);

    /* The inverse conversion, used by the read-side fallback.
     *
     * Hardware finding (FW 9.60): sceSystemServiceGetCurrentDateTime is
     * not exported at all — TIME_GET returns SYS_TIME_ERR_NO_SYMBOL and
     * the console cannot report its own clock. Reading it through
     * gettimeofday instead means converting a unix epoch back into
     * Sony's date struct, which is this. */
    sce_datetime_t dt;

    CHECK(sys_time_unix_to_sce(0, &dt) == 0);
    CHECK(dt.year == 1970 && dt.month == 1 && dt.day == 1);
    CHECK(dt.hour == 0 && dt.minute == 0 && dt.second == 0);

    CHECK(sys_time_unix_to_sce(TARGET, &dt) == 0);
    CHECK(dt.year == 2026 && dt.month == 5 && dt.day == 15);
    CHECK(dt.hour == 23 && dt.minute == 30 && dt.second == 0);

    /* Leap day — 2024-02-29 12:34:56 UTC. */
    CHECK(sys_time_unix_to_sce(1709210096LL, &dt) == 0);
    CHECK(dt.year == 2024 && dt.month == 2 && dt.day == 29);
    CHECK(dt.hour == 12 && dt.minute == 34 && dt.second == 56);

    /* Round-trips against the forward conversion the payload already
     * uses, so the two cannot drift apart. */
    CHECK(sys_time_unix_to_sce(TARGET, &dt) == 0);
    CHECK(sys_time_sce_to_unix(&dt) == TARGET);

    /* Rejected rather than silently producing a 1969 date. */
    CHECK(sys_time_unix_to_sce(-1, &dt) == -1);
    CHECK(sys_time_unix_to_sce(0, NULL) == -1);

    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    printf("sys_time fallback self-test OK\n");
    return 0;
}
