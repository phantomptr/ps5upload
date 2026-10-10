#ifndef PS5UPLOAD2_RUNTIME_H
#define PS5UPLOAD2_RUNTIME_H

#include <stdint.h>
#include <stddef.h>
#include <pthread.h>

#include "instance_verdict.h"

typedef struct {
    uint64_t instance_id;
    uint64_t takeover_nonce; /* random; names this instance in the takeover flag file (takeover_flag.h) */
    /* Mutex guarding the counters below, which node.status reads while the management handlers
     * (on AVA1 worker threads) update them. Keep critical sections short. */
    pthread_mutex_t state_mtx;
    int shutdown_requested;
    int startup_reason;
    /* How the PREVIOUS instance ended, classified once at startup before
     * runtime_write_ownership overwrites the record. Holds a
     * ps5upload2_prior_verdict_t. Reported by node.status so the client and
     * the bug bundle can show it: an externally SIGKILLed predecessor is
     * otherwise completely invisible. */
    int prior_verdict;
    int takeover_requested;
    uint64_t started_at_unix;
    /* The prior instance's ownership record as read at startup, before the takeover made it
     * unlink the file. The reap backs a fresh (possibly lost) read with it (review 010). */
    int prior_rec_pid;
    uint64_t prior_rec_started;
    uint64_t command_count;
    char ownership_path[256];
} runtime_state_t;

#define PS5UPLOAD2_STARTUP_FRESH 1
#define PS5UPLOAD2_STARTUP_TAKEOVER 2

int runtime_init(runtime_state_t *state);
int runtime_write_ownership(const runtime_state_t *state);
int runtime_clear_ownership(const runtime_state_t *state);
/* Best-effort SIGKILL of a previous instance that crashed and lingered (the
 * cooperative takeover only handles a healthy old instance). Call AFTER
 * runtime_try_takeover and BEFORE runtime_write_ownership. See runtime.c. */
void runtime_reap_prior_instance(runtime_state_t *state);
/* Classify how the previous instance ended and store it on `state`.
 * MUST be called after runtime_init (which fills ownership_path) and
 * BEFORE runtime_write_ownership overwrites the prior record. MUST also be
 * called before any worker thread starts (mgmt thread, shutdown watchdog,
 * etc.): it writes state->prior_verdict WITHOUT holding state_mtx, while
 * handle_status_frame reads it under that mutex. That is only safe because
 * today's one call site runs on the main thread long before any other
 * thread that could read prior_verdict exists — a later call, or a second
 * call from a worker thread, would race. This is a threading contract, not
 * locking: no lock has been added here on purpose. */
void runtime_classify_prior_instance(runtime_state_t *state);
/* LAST RESORT. SIGKILL every process whose name carries our own
 * "ps5upload" prefix, except this one. Returns how many were killed.
 *
 * Only ever called after BOTH the cooperative TAKEOVER_REQUEST handshake
 * AND the pid-based reap have failed. The graceful path stays primary
 * because it calls runtime_mark_active_transactions(..., "interrupted")
 * first, which tears the journal down cleanly so upload resume survives;
 * a SIGKILL skips all of that.
 *
 * Unlike the pid-based reap this does NOT need an ownership record, which
 * is the case it exists for: a predecessor whose record was lost or
 * overwritten is otherwise unreachable and the new payload just exits.
 *
 * Deliberately has NO boot-session guard, unlike runtime_reap_prior_instance.
 * That guard exists there because a pid comes from a persisted file that
 * survives reboots. Here every pid comes from a live KERN_PROC_PROC sysctl
 * snapshot taken at call time — the live snapshot IS the boot-session proof,
 * so there is nothing for a started_at/boottime check to add. See the
 * comment at the top of the implementation for the full reasoning. */
int runtime_sweep_our_instances(void);
/* Arm a detached watchdog that force-`_exit()`s the process if the graceful
 * shutdown wedges, so a stuck shutdown can't leave an orphan. Call once when
 * shutdown begins (after runtime_wait_for_shutdown returns).
 *
 * `state` is used ONLY to clear the ownership record before the forced
 * `_exit()` — a shutdown that wedges past this watchdog is a deliberate exit
 * we caused, not an external kill, and the ownership record must not
 * outlive it (see instance_verdict.h: a leftover record + dead pid is
 * indistinguishable from `killed_externally` to the next instance). May be
 * NULL to skip that step. */
void runtime_arm_shutdown_watchdog(const runtime_state_t *state, int exit_code);
int runtime_ensure_directories(void);
/* 2.2.52 Tier-1 staging sweep. Removes *.pkg files in
 * PS5UPLOAD2_PKG_TEMP_DIR with mtime older than 24 h — orphans
 * left by desktop crashes mid-install. Call once at payload init,
 * after runtime_ensure_directories. Failure-tolerant: opendir/stat
 * errors are silent so payload still starts even if the dir was
 * never created (e.g. read-only /data, fresh PS5 startup). */
void runtime_sweep_stale_pkg_temp(void);
int runtime_try_takeover(runtime_state_t *state);

/* Asks this instance to exit (node.shutdown, the old shutdown frame, the takeover flag file). */
void runtime_request_shutdown(runtime_state_t *state, const char *why);
/* Blocks until something asks this instance to exit (node.shutdown, the takeover flag file). The
 * AVA1 server runs on its own threads: the main thread has nothing else to serve. */
void runtime_wait_for_shutdown(runtime_state_t *state);

/* Pop a system toast on the PS5 UI (top-right corner). Defined in
 * main.c. Used by mount/unmount/register/launch handlers in
 * runtime.c so users see status on the PS5 even when the desktop
 * client is closed. Empty/NULL message is a no-op. The kernel API
 * caps message length at ~3 KiB internally, but in practice we
 * format ~100-character strings. */
void pop_notification(const char *message);

/* (Re-)apply the full ucred jailbreak: uid/ruid/svuid=0, all-FF
 * sceCaps, sceAttr=0x80000000, debugger authid, root vnode for
 * rootdir + jaildir. Idempotent and safe to call multiple times.
 * Sets `g_ucred_elevation_rc` to the aggregate result (0 = full
 * elevation succeeded, non-zero = at least one kernel write
 * failed — typically because kernel R/W isn't available yet).
 * Called once at startup from main.c, and again lazily from
 * shellui_rpc_init() if the prior elevation attempt failed —
 * lets users load kstuff after the payload was already running
 * and have sensors/launch start working without a reboot. */
void runtime_apply_ucred_jailbreak(void);

/* Installs the AVA1 management table (mgmt_rpc.h) over this runtime. 0, or -1. Call before
 * the AVA1 server starts. */
int runtime_mgmt_install(runtime_state_t *state);

/* `volatile` matches the definition in main.c — without it, the
 * compiler is allowed to cache reads across function calls into a
 * register, which on a multi-thread frame dispatcher (where every
 * connection thread re-runs runtime_apply_ucred_jailbreak() and
 * mutates this) means a reader could observe a stale -1 long after
 * elevation succeeded, or vice versa. The qualifier is the cheapest
 * fix that preserves the existing int-shape extern across all
 * call sites. */
extern volatile int g_ucred_elevation_rc;

/* Writable-roots allowlist check. Returns 1 if the path starts with an
 * allowed root (/data, /user, /mnt/extN, /mnt/usbN, /mnt/ps5upload/<name>,
 * /mnt/shadowmnt) and contains no ".."/"." components that escape it.
 * Also resolves symlinks via realpath() and re-validates the canonical
 * form. Used by every destructive FS handler and backup.c's restore
 * path. NOTE: realpath() fails for not-yet-existing paths (write/mkdir
 * targets) — in that case the lexical check result is returned as-is
 * (there's no symlink to follow yet). */
int is_path_allowed(const char *p);

/* The read half of the unsafe-read rule: `/system*` paths the hard-coded management handlers
 * may read when the request asks for it. `is_path_allowed` OR this is the pair every
 * unsafe read must satisfy — the same expression runtime.c uses. */
int is_safe_unsafe_read_path(const char *p);

/* The legacy frame number this thread is currently dispatching, or 0 when it is
 * not inside a request. Thread-local and written only by its own thread, so
 * the fatal-signal handler can read it without a lock.
 *
 * Why it exists: when the helper dies, the persisted stderr.log used to name
 * the faulting call only if it was one of the fault-guarded hardware getters.
 * A crash anywhere else left no trace of what was running. Recording the
 * frame type turns "the helper disconnected" into "the helper died on
 * SIGSEGV while serving frame 68" for EVERY request. */
extern __thread volatile unsigned int g_inflight_frame_type;

#endif
