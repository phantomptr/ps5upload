#ifndef PS5UPLOAD2_INSTANCE_VERDICT_H
#define PS5UPLOAD2_INSTANCE_VERDICT_H

#include <stdint.h>

/*
 * How did the PREVIOUS payload instance end?
 *
 * The ownership record (runtime_write_ownership / runtime_clear_ownership)
 * is already an exit marker: written at startup, unlinked on a graceful
 * exit. What was missing was reading it as a verdict.
 *
 * This matters because SIGKILL cannot be caught. An instance killed by
 * another payload's "replace my predecessor" sweep, or by the OOM killer,
 * leaves no log line of its own — the ONLY evidence is an ownership record
 * it never got to unlink, plus a pid that is no longer alive.
 */
typedef enum {
    PS5UPLOAD2_PRIOR_CLEAN = 0,
    PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY = 1,
    PS5UPLOAD2_PRIOR_WEDGED = 2,
    PS5UPLOAD2_PRIOR_STALE = 3,
    /* Alive when this instance started, then handed over cooperatively. The
     * normal relaunch-over-a-running-helper case. */
    PS5UPLOAD2_PRIOR_REPLACED = 4,
} ps5upload2_prior_verdict_t;

/*
 * `record_present`    — an ownership record existed at startup.
 * `prior_started_at`  — its started_at_unix field, 0 when absent/unreadable.
 * `boottime`          — kern.boottime in the same clock domain, 0 when the
 *                       sysctl is unavailable.
 * `prior_pid_alive`   — kill(pid, 0) succeeded for its recorded pid.
 *
 * Fails safe: any unknowable input yields STALE rather than a confident
 * claim. The ownership file lives on persistent /data and survives reboots,
 * so a record that predates this boot says nothing about this session.
 */
static inline ps5upload2_prior_verdict_t
instance_verdict_classify(int record_present,
                          uint64_t prior_started_at,
                          uint64_t boottime,
                          int prior_pid_alive) {
    if (!record_present) return PS5UPLOAD2_PRIOR_CLEAN;
    if (boottime == 0 || prior_started_at == 0 || prior_started_at < boottime) {
        return PS5UPLOAD2_PRIOR_STALE;
    }
    if (prior_pid_alive) return PS5UPLOAD2_PRIOR_WEDGED;
    return PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY;
}

/*
 * Refine the startup verdict once the takeover's outcome is known.
 *
 * The classifier has to run BEFORE the takeover (afterwards the ownership
 * record belongs to this instance), and at that point a perfectly healthy
 * predecessor is alive — so every ordinary relaunch over a running helper was
 * reported as "wedged". Measured: five relaunches in a row on a FW 5.10
 * console, every one "wedged", every predecessor handing over cleanly. That
 * label then sent a crash investigation after a stuck process that did not
 * exist. "Wedged" now means what it says: alive, and it would NOT hand over.
 */
static inline ps5upload2_prior_verdict_t
instance_verdict_after_takeover(ps5upload2_prior_verdict_t at_startup,
                                int cooperative_takeover_ok) {
    if (at_startup == PS5UPLOAD2_PRIOR_WEDGED && cooperative_takeover_ok) {
        return PS5UPLOAD2_PRIOR_REPLACED;
    }
    return at_startup;
}

/*
 * May the prior pid be killed? (final review: console, outage 2026-10-03)
 *
 * The ownership record's started_at_unix used to be the only proof that the pid belongs to this
 * boot, so a record that read 0 (an instrumented build) left a live helper running beside the new
 * instance. The kernel's own process start time (kinfo_proc ki_start, the same clock domain as
 * kern.boottime) is evidence that needs no record: a live process that started at or after the
 * boot is of this boot, and with the "ps5upload" name it is a helper of ours. The record only
 * backs it up when the kernel time cannot be read or reads implausibly (an offset that does not
 * hold on some firmware must not be trusted).
 */
typedef enum {
    PS5UPLOAD2_REAP_YES = 0,
    PS5UPLOAD2_REAP_NOT_OURS = 1,      /* the process name is not a helper's */
    PS5UPLOAD2_REAP_UNVERIFIABLE = 2,  /* cannot show the pid is of this boot: leave it */
} ps5upload2_reap_t;

/* A start time inside [boottime, now + 60 s]: the sysctl layout offset held and the value is sane. */
static inline int instance_proc_start_plausible(uint64_t start, uint64_t boottime, uint64_t now) {
    return boottime != 0 && start != 0 && start >= boottime && start <= now + 60u;
}

static inline ps5upload2_reap_t instance_reap_decision(int name_is_ours, uint64_t record_started,
                                                       uint64_t boottime, uint64_t proc_start,
                                                       int proc_start_known, uint64_t now) {
    if (!name_is_ours) return PS5UPLOAD2_REAP_NOT_OURS;
    if (boottime == 0) return PS5UPLOAD2_REAP_UNVERIFIABLE;
    if (proc_start_known && instance_proc_start_plausible(proc_start, boottime, now)) return PS5UPLOAD2_REAP_YES;
    if (record_started != 0 && record_started >= boottime) return PS5UPLOAD2_REAP_YES;
    return PS5UPLOAD2_REAP_UNVERIFIABLE;
}

/* Wire name. snake_case: this value crosses the STATUS_ACK JSON boundary
 * into serde on the engine side. */
static inline const char *
instance_verdict_name(ps5upload2_prior_verdict_t v) {
    switch (v) {
        case PS5UPLOAD2_PRIOR_CLEAN:              return "clean";
        case PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY:  return "killed_externally";
        case PS5UPLOAD2_PRIOR_WEDGED:             return "wedged";
        case PS5UPLOAD2_PRIOR_STALE:              return "stale";
        case PS5UPLOAD2_PRIOR_REPLACED:           return "replaced";
    }
    return "unknown";
}

#endif /* PS5UPLOAD2_INSTANCE_VERDICT_H */
