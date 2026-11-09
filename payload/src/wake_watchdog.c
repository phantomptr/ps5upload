/*
 * wake_watchdog.c — re-escalation when the system put our credentials back.
 *
 * A jump on either clock is the cue to read the process's ucred back; only a
 * ucred that really differs from what the last elevation left is re-elevated,
 * so a clock change or a throttled thread never leads to a kernel write.
 *
 * See wake_watchdog.h for the full design rationale.
 */

#include "wake_watchdog.h"
#include "plog.h"
#include "kernel_rw_lock.h"
#include "proc_identity.h"
#include "config.h"
#include "runtime.h"
#include "hw_info.h"
#include "power_phase.h"

#include <ps5/kernel.h>

#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <stdlib.h>
#include <pthread.h>

/* Poll interval. WAKE_POLL_S: how often we sample both clocks. 5s keeps CPU
 * overhead negligible.
 *
 * WAKE_JUMP_THRESH_S (wake_watchdog.h): the gap that earns a look at the ucred.
 * Was 15s in elf-arsenal's early code; 60s because the PS5 throttles background
 * threads hard when a game is foreground. A look writes nothing, so this only
 * keeps kernel reads rare. */
#define WAKE_POLL_S    5

/* ── The ucred, as it is and as we left it ───────────────────────────── */

/* Set under kernel_rw_lock by wake_watchdog_note_elevated, read under it here. */
static wake_ucred_t g_baseline;
static int g_baseline_ok;

/* Caller holds kernel_rw_lock. The SDK's getters: caps and attrs report a failed read (-1); the
 * others return 0 or -1, which cannot be told from a value, so a read counts when those two do. */
static int ucred_read_locked(wake_ucred_t *out) {
    pid_t me = getpid();
    memset(out, 0, sizeof *out);
    if (kernel_get_ucred_caps(me, out->caps) != 0 || kernel_get_ucred_attrs(me, out->attrs) != 0) return -1;
    out->authid = kernel_get_ucred_authid(me);
    out->uid = (uint32_t)kernel_get_ucred_uid(me);
    out->ruid = (uint32_t)kernel_get_ucred_ruid(me);
    out->svuid = (uint32_t)kernel_get_ucred_svuid(me);
    out->rgid = (uint32_t)kernel_get_ucred_rgid(me);
    out->svgid = (uint32_t)kernel_get_ucred_svgid(me);
    return 0;
}

void wake_watchdog_note_elevated(void) {
    g_baseline_ok = ucred_read_locked(&g_baseline) == 0;
}

/* Inside kernel_rw_lock, so no authid swap (register.c, ptrace_remote.c) is ever seen half-way. */
static int ucred_state(void) {
    wake_ucred_t now;
    int state = WAKE_UCRED_UNKNOWN;
    pthread_mutex_lock(&kernel_rw_lock);
    if (g_baseline_ok && ucred_read_locked(&now) == 0) state = wake_ucred_compare(&g_baseline, &now);
    pthread_mutex_unlock(&kernel_rw_lock);
    return state;
}

/* ── Watchdog thread ─────────────────────────────────────────────────── */

static void *wake_watchdog_thread_fn(void *arg) {
    (void)arg;
    proc_name_set_self("ps5upload-wake");

    /* Once: what a bug report needs to trust the later lines. Right after the elevation this has to
     * read "unchanged" ("unreadable" before kstuff is loaded: the baseline comes with the elevation). */
    {
        int state = ucred_state();
        fprintf(stderr, "wake_watchdog: ucred %s at start\n",
                state == WAKE_UCRED_SAME ? "unchanged" : state == WAKE_UCRED_CHANGED ? "CHANGED" : "unreadable");
    }

    /* A jump seen, its ucred look not done yet: the look waits until the console has settled. */
    int look_pending = 0;
    int64_t pend_real = 0, pend_mono = 0;
    /* Elapsed is measured from the END of the previous poll, not just across the sleep: a rest that
     * begins while this thread is inside a poll (the probe's socket call as the network goes away, a
     * log flush) would otherwise fall between two measurements and never be seen. Measured on the Pro
     * (12.4.1, 2026-11-09): a 7-minute rest from the console's menu left no "woke" line at all. */
    struct timespec last_real, last_mono;
    while (clock_gettime(CLOCK_REALTIME, &last_real) != 0 || clock_gettime(CLOCK_MONOTONIC, &last_mono) != 0)
        sleep(WAKE_POLL_S);
    for (;;) {
        struct timespec real1, mono1;
        sleep(WAKE_POLL_S);
        if (clock_gettime(CLOCK_REALTIME, &real1) != 0 ||
            clock_gettime(CLOCK_MONOTONIC, &mono1) != 0) {
            continue;
        }
        int64_t real_elapsed = (int64_t)real1.tv_sec - (int64_t)last_real.tv_sec;
        int64_t mono_elapsed = (int64_t)mono1.tv_sec - (int64_t)last_mono.tv_sec;
        last_real = real1;
        last_mono = mono1;

        PLOG(PLOG_TRACE, PLOG_WAKE, "wake_watchdog: slept %llds (monotonic %llds)", (long long)real_elapsed,
             (long long)mono_elapsed);
        /* Every poll: is the console going to rest, frozen-and-woken, or settled (power_phase.h)? This
         * also pauses the tailnet node while the network is going away or not back yet. */
        int phase = power_phase_poll(real_elapsed, mono_elapsed);
        {
            /* A poll that ran long: say where the time went (the next poll counts it as elapsed). */
            struct timespec after;
            if (clock_gettime(CLOCK_MONOTONIC, &after) == 0 && (int64_t)after.tv_sec - (int64_t)mono1.tv_sec >= 10)
                fprintf(stderr, "wake_watchdog: a power check took %llds\n",
                        (long long)((int64_t)after.tv_sec - (int64_t)mono1.tv_sec));
        }

        if (wake_watchdog_should_look(real_elapsed, mono_elapsed)) {
            if (!look_pending && phase != PP_NORMAL)
                fprintf(stderr, "wake_watchdog: clocks jumped %llds (monotonic %llds); ucred look waits until the "
                        "console has settled\n", (long long)real_elapsed, (long long)mono_elapsed);
            look_pending = 1;
            pend_real = real_elapsed;
            pend_mono = mono_elapsed;
        }
        /* Normal case: no jump to look into. */
        if (!look_pending) continue;
        /* Never a kernel read or write while the console is going to rest, or in the minute after a
         * wake (power_phase.h): the system is suspending or resuming underneath us then. */
        if (phase != PP_NORMAL) continue;
        look_pending = 0;
        real_elapsed = pend_real;
        mono_elapsed = pend_mono;

        /* A jump is a cue, not proof: a clock correction or a throttled thread
         * looks the same as a rest. The ucred decides. */
        int state = ucred_state();
        PLOG(PLOG_DEBUG, PLOG_WAKE, "wake_watchdog: clocks apart (%llds vs %llds), ucred state %d",
             (long long)real_elapsed, (long long)mono_elapsed, state);
        if (!wake_watchdog_should_recover(real_elapsed, mono_elapsed, state)) {
            fprintf(stderr,
                    "wake_watchdog: clocks jumped %llds (monotonic %llds), ucred %s; "
                    "no re-escalation\n",
                    (long long)real_elapsed, (long long)mono_elapsed,
                    state == WAKE_UCRED_SAME ? "unchanged" : "unreadable");
            continue;
        }

        fprintf(stderr,
                "wake_watchdog: clocks jumped %llds (monotonic %llds) and the ucred changed; "
                "re-escalating\n",
                (long long)real_elapsed, (long long)mono_elapsed);

        /* 1. Force re-elevation. Clear the cached success flag so
         *    runtime_apply_ucred_jailbreak() actually runs the full
         *    kernel-write sequence instead of early-outing. */
        g_ucred_elevation_rc = -1;
        runtime_apply_ucred_jailbreak();
        if (g_ucred_elevation_rc != 0) {
            fprintf(stderr,
                    "wake_watchdog: ucred re-escalation failed — stopping recovery\n");
            continue;
        }

        /* 2. Re-apply fan threshold if one was pinned (the phase is NORMAL here: settled). */
        int pinned = hw_fan_pinned_threshold();
        if (pinned >= HW_FAN_THRESHOLD_MIN && pinned <= HW_FAN_THRESHOLD_MAX) {
            const char *err = NULL;
            if (hw_fan_set_threshold((uint8_t)pinned, &err) == 0) {
                fprintf(stderr,
                        "wake_watchdog: fan threshold re-applied (%d°C)\n",
                        pinned);
            } else {
                fprintf(stderr,
                        "wake_watchdog: fan re-apply failed (%s)\n",
                        err ? err : "unknown");
            }
        }

        /* Mount reconciliation is intentionally NOT run from this detached
         * thread. main.c already avoids getmntinfo at startup because it can
         * hang on some firmware/loader combinations; a wake heuristic must
         * not reintroduce that same failure path in the background. Mounts
         * reconcile on explicit UI operations instead. */

        /* 3. Toast so the user sees we recovered. */
        pop_notification("PS5Upload: re-activated after rest mode");

        fprintf(stderr, "wake_watchdog: re-escalation complete\n");
    }

    return NULL;
}

void start_wake_watchdog(void) {
    pthread_t t;
    pthread_attr_t a;
    if (pthread_attr_init(&a) != 0) return;
    (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&t, &a, wake_watchdog_thread_fn, NULL) != 0) {
        fprintf(stderr, "wake_watchdog: failed to start thread\n");
    }
    pthread_attr_destroy(&a);
}
