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
