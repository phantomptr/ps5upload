#ifndef PS5UPLOAD2_SYS_TIME_H
#define PS5UPLOAD2_SYS_TIME_H

#include <stdint.h>

/*
 * PS5 system clock get/set, via SCE's sceSystemServiceGetCurrentDateTime
 * and sceSystemServiceSetCurrentDateTime.
 *
 * Both calls go through SceShellCore IPC. On a vanilla webkit / BD-JB
 * payload context the IPC is rejected because the caller's authid +
 * capabilities don't include the system-service privilege. On a
 * kstuff-loaded payload the loader has already escalated this
 * process's ucred to uid=0 with the right cap bits — at which point
 * the IPC accepts and the set call actually changes the clock.
 *
 * On firmwares where the libSceSystemService SDK stub exists but
 * the runtime SPRX doesn't implement the underlying syscall, the
 * set call returns rc=0 but the clock doesn't move. The
 * `out_prior_unix` / `out_new_unix` outputs let the desktop detect
 * that case by comparing them — if they're equal AND the requested
 * time was different, the call was a stub no-op.
 *
 * Defensive symbol resolution: we dlsym both functions at first call,
 * not at link time. If either symbol is absent (SDK link order quirk,
 * NID-only export on some FWs) we surface SYS_TIME_ERR_NO_SYMBOL
 * instead of crashing the payload.
 *
 * Thread-safety: the dlsym cache is initialised at first call; the
 * underlying SCE functions are documented thread-safe.
 */

/* Mirrors Sony's SceDateTime layout exactly — stable across every
 * firmware ps5upload has been tested on (4.03 → 9.60+). All fields
 * UTC. `microsecond` is part of the struct in Sony's header but we
 * always pass 0 because the SCE set call rounds to whole seconds
 * internally anyway. */
typedef struct sce_datetime {
    uint16_t year;        /* e.g. 2026 */
    uint16_t month;       /* 1-12 */
    uint16_t day;         /* 1-31 */
    uint16_t hour;        /* 0-23 */
    uint16_t minute;      /* 0-59 */
    uint16_t second;      /* 0-59 (no leap seconds — Sony rounds those out) */
    uint32_t microsecond; /* 0-999999, ignored on set */
} sce_datetime_t;

/* Diagnostic err_code sentinels we surface in the OUT param. Picked
 * in the 0xE0000000+ range so they don't collide with Sony's err_code
 * space (Sony uses 0x80xxxxxx negative-as-unsigned), with our own
 * BGFT_ERR_* (which use 0xE000_1xxx), or with the standard zero/rc
 * semantics. */
#define SYS_TIME_ERR_NULL_ARG     0xE0002001U /* caller passed NULL */
#define SYS_TIME_ERR_NO_SYMBOL    0xE0002002U /* dlsym returned NULL */

/* Read the PS5's current system date/time (UTC). On success writes
 * *out and returns 0; on failure writes *out_err_code with one of
 * the SYS_TIME_ERR_* sentinels OR the raw int return from SCE
 * (uint32 cast). */
int sys_time_get(sce_datetime_t *out, uint32_t *out_err_code);

/* Set the PS5's system date/time (UTC). Returns 0 on success, -1 on
 * failure. *out_err_code follows the same convention as sys_time_get.
 *
 * out_prior_unix / out_new_unix are best-effort outputs the caller
 * can compare to detect the "rc=0 but clock didn't actually move"
 * stub-no-op case some firmwares produce. Either may be -1 if the
 * companion sys_time_get failed (e.g. symbol absent — the set still
 * runs in that case, we just can't verify).
 *
 * Either of out_prior_unix / out_new_unix may be NULL if the caller
 * doesn't care about that side.
 *
 * Why prior + new (instead of just new): some firmwares' set call
 * returns rc=0 even when the underlying syscall did nothing. Having
 * the prior epoch in the response lets the desktop UI show the
 * actual drift before/after so the user sees whether the set
 * actually took. */
int sys_time_set(const sce_datetime_t *dt,
                 uint32_t *out_err_code,
                 int64_t *out_prior_unix,
                 int64_t *out_new_unix);

#endif /* PS5UPLOAD2_SYS_TIME_H */
