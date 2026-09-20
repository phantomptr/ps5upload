#ifndef PS5UPLOAD2_PROC_IDENTITY_H
#define PS5UPLOAD2_PROC_IDENTITY_H

#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

/*
 * Is this process name one of OURS?
 *
 * Every thread the payload creates shares the "ps5upload" prefix:
 * "ps5upload.elf" (generic workers and main), plus the named helpers
 * "ps5upload-wake", "ps5upload-fan" and "ps5upload-smp".
 *
 * That is only true because every thread entry point calls
 * proc_name_set_self() as its first statement. It is NOT inherited: naming
 * main alone was measured on a Phat 5.10 running v5.31.1 to leave the
 * process listed as "payload.elf", because SYS_thr_set_name names only the
 * CALLING thread and the kernel's representative thread is usually one of
 * the ~15 unnamed workers. Any new pthread_create MUST name its thread too,
 * or it reopens the reap gap described below.
 *
 * A prefix test rather than an exact compare is REQUIRED. The kinfo_proc
 * record returned by sysctl(KERN_PROC_*) carries the name of whichever
 * thread the kernel treats as representative, and that is not reliably the
 * main thread: the SceShellCore FMEM dump in issue #289 listed our process
 * as "ps5upload-wake". An exact compare against the main thread's name is
 * exactly the bug that stopped runtime_reap_prior_instance from reaping its
 * own wedged predecessor.
 *
 * It deliberately does NOT match "payload.elf" — the generic name elfldr
 * gives every raw-streamed payload (elfldr.c:704 -> uri_get_filename falls
 * back to the literal when there is no URI). Matching that would put kstuff,
 * ShadowMountPlus, nanoDNS and every other payload on the console inside our
 * SIGKILL radius.
 *
 * A bare prefix test is NOT enough, though: it also matches anything that
 * merely STARTS WITH our prefix — "ps5uploader.elf", "ps5upload2-tool.elf",
 * "ps5uploadX" — none of which are us. The byte immediately after the
 * prefix must be a delimiter our four real names actually use: '\0'
 * ("ps5upload.elf" truncated to 9 chars by the kernel's short tdname, or an
 * exact 9-char name), '.' ("ps5upload.elf"), or '-' ("ps5upload-wake",
 * "ps5upload-fan", "ps5upload-smp"). This function backs the kill radius of
 * two SIGKILL loops (runtime_reap_prior_instance, runtime_sweep_our_instances),
 * so a loose match here is a loose trigger finger against bystander homebrew.
 */
#define PS5UPLOAD2_PROC_PREFIX     "ps5upload"
#define PS5UPLOAD2_PROC_PREFIX_LEN 9

/* The name every generic payload thread takes. Helpers with a dedicated
 * role pass their own ("ps5upload-fan" and friends) — anything sharing the
 * prefix satisfies proc_name_is_ours(). */
#define PS5UPLOAD2_PROC_NAME "ps5upload.elf"

/* Name the CALLING thread. Must be the first statement of every thread entry
 * point: `-1` means "this thread", so it cannot be done on the creator's
 * behalf from a pthread_create wrapper. Best-effort — a failure here costs
 * identification, never correctness, so the return is deliberately ignored. */
#ifdef SYS_thr_set_name
static inline void proc_name_set_self(const char *name) {
    (void)syscall(SYS_thr_set_name, -1, name);
}
#else
/* Host builds (the proc_identity selftest compiles this header on macOS and
 * Linux) have no SYS_thr_set_name. Naming is a PS5-only concern, so the
 * fallback is a no-op rather than a #error — it keeps the pure-logic
 * proc_name_is_ours() tests buildable off-target. */
static inline void proc_name_set_self(const char *name) {
    (void)name;
}
#endif

static inline int proc_name_is_ours(const char *name) {
    if (!name) return 0;
    if (strncmp(name, PS5UPLOAD2_PROC_PREFIX,
                PS5UPLOAD2_PROC_PREFIX_LEN) != 0) {
        return 0;
    }
    char delim = name[PS5UPLOAD2_PROC_PREFIX_LEN];
    return (delim == '\0' || delim == '.' || delim == '-') ? 1 : 0;
}

#endif /* PS5UPLOAD2_PROC_IDENTITY_H */
