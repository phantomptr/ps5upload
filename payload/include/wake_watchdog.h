#ifndef PS5UPLOAD2_WAKE_WATCHDOG_H
#define PS5UPLOAD2_WAKE_WATCHDOG_H

#include <stddef.h>
#include <stdint.h>

/*
 * Rest-mode wake watchdog.
 *
 * The helper's process credentials are what the jailbreak's kernel writes
 * made them (runtime_apply_ucred_jailbreak). If the system puts them back
 * while the process lives, Sony APIs and every kernel-R/W feature silently
 * fail until they are written again. The one thing this thread does is notice
 * that and write them again.
 *
 * WHAT ALLOWS A KERNEL WRITE: the ucred itself. A clock jump only says "look":
 * this thread reads its own ucred back and compares it with what the last
 * successful elevation left. Reading is one kernel read per field and changes
 * nothing. If nothing changed there is nothing to do, whatever the clocks say;
 * if something changed, the credentials are wrong now, whatever the reason.
 * That makes both of these safe:
 *
 *   - A clock jump with no rest at all: Sony NTP, the app's clock sync or a
 *     manual date change move REALTIME by hours (#289), and a game in the
 *     foreground throttles background threads so hard that a 5 s sleep can
 *     overrun 60 s on both clocks. The ucred is as it was: no write, no toast.
 *   - A real rest with a game open: the game is suspended, not closed, and
 *     resumes on wake, so "a game is running" says nothing about whether we
 *     slept. This was once the veto here, and it would have refused the most
 *     common rest/wake cycle there is.
 *
 * On a real change we:
 *   1. Force re-elevation of the ucred credentials.
 *   2. Re-apply the persisted fan threshold.
 *   3. Fire a toast so the user sees the re-activation on screen.
 *
 * WHICH CLOCK: either. MEASURED 2026-10-31 on a Pro (FW 13.60): the process
 * survives rest, and CLOCK_MONOTONIC stops across it ("wall clock jumped Ns but
 * monotonic advanced 5s"), so requiring both clocks to jump would never look at
 * all on the firmware where this thread can matter. MEASURED 2026-08-31 on FW
 * 5.10 and 9.60: the payload does not survive rest at all, so there every jump
 * this thread sees is something else, and the ucred compare answers "unchanged".
 *
 * WHEN: never while the console is going to rest or settling after a wake. The
 * jump only marks a look as pending; the look (and any write) runs on the first
 * poll where power_phase.h says NORMAL: the loopback network healthy again and a
 * minute of running time after the wake. Kernel reads and writes racing the
 * system's own suspend/resume are the prime suspect when a console dies in rest.
 *
 * Unknown is not permission: if the ucred cannot be read, or no elevation has
 * succeeded yet (no baseline; the frame dispatcher retries that by itself),
 * nothing is written.
 *
 * Mount reconciliation is deliberately excluded: getmntinfo can hang on
 * some firmware/loader combinations and must not run from this detached
 * thread.
 *
 * Algorithm adapted from elf-arsenal's wake_watchdog_thread (sys.c:1855).
 * Threshold is 60s (raised from elf-arsenal's original 15s for the same
 * reason: PS5 thread throttling under game load).
 *
 * The thread is detached and self-contained — safe to start once at
 * payload boot and never join.
 */

/* Launch the wake watchdog as a detached thread. Safe to call once
 * from main() after the fan threshold restore block. The thread runs
 * for the lifetime of the payload process. */
void start_wake_watchdog(void);

/* The fields runtime_apply_ucred_jailbreak writes, as read back. */
typedef struct {
    uint64_t authid;
    uint8_t caps[16];
    uint8_t attrs[32];
    uint32_t uid, ruid, svuid, rgid, svgid;
} wake_ucred_t;

/* Called at the end of a successful elevation, with kernel_rw_lock held: what the
 * ucred looks like now is what later reads are compared with. */
void wake_watchdog_note_elevated(void);

enum {
    WAKE_UCRED_UNKNOWN = -1, /* could not be read, or nothing to compare with */
    WAKE_UCRED_SAME = 0,
    WAKE_UCRED_CHANGED = 1,
};

/* A jump worth a look at the ucred: either clock moved at least the threshold
 * across one poll (see WHICH CLOCK above). */
#define WAKE_JUMP_THRESH_S 60
static inline int wake_watchdog_should_look(int64_t realtime_elapsed, int64_t monotonic_elapsed) {
    return realtime_elapsed >= WAKE_JUMP_THRESH_S || monotonic_elapsed >= WAKE_JUMP_THRESH_S;
}

/* WAKE_UCRED_SAME / _CHANGED; _UNKNOWN when either side is missing. */
static inline int wake_ucred_compare(const wake_ucred_t *baseline, const wake_ucred_t *now) {
    if (!baseline || !now) return WAKE_UCRED_UNKNOWN;
    if (baseline->authid != now->authid || baseline->uid != now->uid || baseline->ruid != now->ruid ||
        baseline->svuid != now->svuid || baseline->rgid != now->rgid || baseline->svgid != now->svgid)
        return WAKE_UCRED_CHANGED;
    for (size_t i = 0; i < sizeof baseline->caps; i++)
        if (baseline->caps[i] != now->caps[i]) return WAKE_UCRED_CHANGED;
    for (size_t i = 0; i < sizeof baseline->attrs; i++)
        if (baseline->attrs[i] != now->attrs[i]) return WAKE_UCRED_CHANGED;
    return WAKE_UCRED_SAME;
}

/* Pure decision gate used by the watchdog and its host self-test: the kernel
 * writes run only after a jump, and only when the ucred really changed. */
static inline int wake_watchdog_should_recover(int64_t realtime_elapsed, int64_t monotonic_elapsed,
                                               int ucred_state) {
    return wake_watchdog_should_look(realtime_elapsed, monotonic_elapsed) &&
           ucred_state == WAKE_UCRED_CHANGED;
}

#endif /* PS5UPLOAD2_WAKE_WATCHDOG_H */
