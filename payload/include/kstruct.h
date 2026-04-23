#ifndef PS5UPLOAD2_KSTRUCT_H
#define PS5UPLOAD2_KSTRUCT_H

/*
 * Kernel struct offsets + helpers used by code in the payload that
 * touches kernel R/W. Currently only consumed by proc_list.c (for
 * PROC_LIST observability), but kept in a shared header so future
 * kernel-touching modules have one source of truth.
 *
 * We pull firmware-dependent offsets from the SDK where it exposes
 * them (those are resolved at payload startup from
 * `kernel_get_fw_version()` and carry the same offset for every
 * firmware the SDK supports). The SDK's crt1.o fills in the extern
 * `const off_t KERNEL_OFFSET_*` symbols at _start before any of our
 * code runs, so a binary built against SDK v0.38+ runs on PS5 9.x
 * through 12.x without source changes — we just have to use the
 * symbols instead of hardcoded numbers.
 *
 * For the offsets the SDK does NOT expose (notably p_comm, used by
 * proc_list.c to read process names), we fall back to our own table,
 * queried via `ps5upload_p_comm_offset()`. If you bring up support
 * for a newer firmware, add its p_comm offset there — nothing else
 * in this header should need touching.
 */

#include <sys/types.h>
#include <ps5/kernel.h>

/* ── struct proc ──────────────────────────────────────────────────── */
/*
 * `p_list.le_next` is the first field of `struct proc` in FreeBSD 11
 * (LIST_ENTRY is `{le_next, le_prev}`, `le_next` comes first). The SDK
 * doesn't expose a symbol for it because it's "always zero." We keep
 * a named constant so the intent at the call site is readable.
 */
#define PROC_FIELD_NEXT       0x000  /* LIST_ENTRY p_list → struct proc * */

/* Remaining struct-proc offsets come from the SDK (firmware-resolved).
 * `KERNEL_OFFSET_PROC_P_PID`, `KERNEL_OFFSET_PROC_P_UCRED`, etc. are
 * defined as `extern const off_t` in <ps5/kernel.h>. Use them as
 * offsets directly into a proc-pointer read. */

/* ── struct ucred ─────────────────────────────────────────────────── */
/* All exposed by the SDK. Nothing firmware-specific to override here. */

/* ── Well-known auth-ids ──────────────────────────────────────────── */
/* Kernel gates various syscalls on these magic values. Constants, not
 * offsets — independent of firmware version. */
#define AUTHID_PTRACE         0x4800000000010003ULL  /* enables sys_ptrace */
#define AUTHID_WEBSRV         0x4801000000000013ULL  /* sample pattern */

/* ── p_comm (process name) ────────────────────────────────────────── */
/*
 * The SDK does NOT expose an offset for `p_comm`. We maintain our own
 * per-firmware-major table. `ps5upload_p_comm_offset()` consults
 * `kernel_get_fw_version()` at runtime and returns the right offset,
 * or 0 for an unsupported firmware — callers must check and degrade
 * gracefully (emit placeholder names in the PROC_LIST reply instead
 * of reading from a wrong offset, which would return garbage or
 * page-fault).
 */
#define PROC_COMM_LEN         32u
#define PROC_COMM_MAX_USEFUL  19     /* FreeBSD MAXCOMLEN-1 */

/*
 * Returns the byte offset of `char p_comm[PROC_COMM_LEN]` within
 * `struct proc` for the currently-running firmware, or 0 if unknown.
 *
 * Known values:
 *   firmware 9.x-11.x (SDK says 0x09xxxxxx-0x11xxxxxx): 0x59C
 *   firmware 12.x (SDK says 0x12xxxxxx):               TBD — returns 0
 *                                                       until confirmed.
 *
 * Implementation lives in proc_list.c so this header stays
 * allocation-free and doesn't pull in strings/libc.
 */
off_t ps5upload_p_comm_offset(void);

#endif /* PS5UPLOAD2_KSTRUCT_H */
