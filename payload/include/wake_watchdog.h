#ifndef PS5UPLOAD2_WAKE_WATCHDOG_H
#define PS5UPLOAD2_WAKE_WATCHDOG_H

#include <stdint.h>

/*
 * Rest-mode wake watchdog.
 *
 * After the PS5 wakes from rest mode (suspend-to-RAM), the kernel resets
 * process credentials. Our payload's ucred elevation becomes stale —
 * Sony APIs silently fail and the fan threshold pin is lost.
 *
 * The watchdog treats a long delay as a possible wake event only when both
 * REALTIME and MONOTONIC clocks advance by the threshold. Requiring two
 * independent signals prevents NTP, manual time changes, and client clock
 * sync from turning a wall-clock adjustment into permission for kernel
 * writes. On detection we:
 *
 *   1. Check if a BigApp (game) is running — if so, skip. The PS5
 *      throttles background threads when a game is foreground, which
 *      can cause a 5s sleep to overrun 60s without any actual suspend.
 *      A real resume from rest kills all games first, so a live BigApp
 *      means we were never actually suspended.
 *   2. Force re-elevation of ucred credentials.
 *   3. Re-apply the persisted fan threshold.
 *   4. Fire a toast so the user sees the re-activation on screen.
 *
 * Mount reconciliation is deliberately excluded: getmntinfo can hang on
 * some firmware/loader combinations and must not run from this detached
 * heuristic thread.
 *
 * MEASURED 2026-08-31, and it bounds what this thread can ever be worth:
 * on FW 5.10 and FW 9.60 the payload does NOT survive rest mode at all. A
 * real standby/wake cycle leaves the ELF loader (9021) listening and the
 * helper's own ports (9113/9114) closed — the process is gone, so no
 * re-escalation can run and the desktop's auto-loader re-sends the payload
 * instead. On those firmwares every branch this thread can reach is
 * therefore a FALSE positive. That asymmetry — unproven upside, proven
 * downside — is why the gate below is deliberately hard to satisfy.
 *
 * A consequence worth stating plainly: if some firmware does keep the
 * process across suspend but does not advance CLOCK_MONOTONIC over it,
 * this gate never fires and recovery defers to that same payload re-send.
 * That is the intended trade. Do not loosen the gate to "fix" it without
 * first proving, on hardware, that a suspended payload survives.
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

/* Pure decision gate used by the watchdog and its host self-test.
 *
 * A wall-clock jump alone is not evidence of rest mode: Sony NTP, the new
 * clock-sync feature, or a manual date correction can all move REALTIME by
 * hours while the process has only slept five seconds. Re-running kernel
 * ucred writes on that weak signal is unsafe, especially on new firmware.
 * Require an independent monotonic suspend-sized gap and a successful proof
 * that no BigApp is running. `bigapp_state`: 0=no app, 1=app, -1=unknown. */
static inline int wake_watchdog_should_recover(int64_t realtime_elapsed,
                                                int64_t monotonic_elapsed,
                                                int bigapp_state) {
    return realtime_elapsed >= 60 && monotonic_elapsed >= 60 && bigapp_state == 0;
}

#endif /* PS5UPLOAD2_WAKE_WATCHDOG_H */
