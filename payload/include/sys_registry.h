#ifndef PS5UPLOAD2_SYS_REGISTRY_H
#define PS5UPLOAD2_SYS_REGISTRY_H

#include <stdint.h>
#include <stddef.h>

/*
 * Thin payload-side wrapper around Sony's sceRegMgr R/W primitives.
 *
 * Sony's PS5 firmware exposes a key/value registry (`system.dat`) via
 * `libSceRegMgr.sprx`. Same surface PS4 had. The DATE namespace
 * (`SCE_REGMGR_ENT_KEY_DATE_*`) carries timezone, DST policy,
 * auto-sync flag, NTP server settings, date/time format, tzdata
 * version, and an NTP-error counter — the entire control surface
 * for the PS5's clock and timezone UI in Settings → System → Date
 * and Time. As of 2.10.0 ps5upload exposes that surface to users
 * over the desktop UI.
 *
 * Privilege: same envelope as `sys_time.c` — needs the ucred
 * elevation that kstuff installs. Plain webkit/BD-JB payloads see
 * SCE_ERROR_OPERATION_NOT_PERMITTED (or similar) on every call.
 *
 * Defensive symbol resolution: dlsym at first call, NOT link time.
 * Sony's SDK stub library and the runtime SPRX disagree on which
 * registry symbols are exported on which firmware — this is the
 * same trap `sys_time.c` already documented. The wrappers below
 * report SYS_REGISTRY_ERR_NO_SYMBOL when a get/set symbol can't be
 * resolved on this firmware, so the desktop can degrade gracefully
 * (greyed-out field with an explanatory tooltip) rather than crash
 * the payload.
 *
 * Novel territory: as of 2.10.0 we are (best knowledge per the 2.10
 * research) the first public PS5 homebrew to write to the DATE
 * namespace. Read-side is straightforward — those keys have been
 * dumped by ps5-payload-dev/regdump for years. Write-side has been
 * exercised on real hardware for ps5upload's verification matrix;
 * see CHANGELOG and reference_ps5_date_registry_keys.md. Adding new
 * keys: extend this file's wrappers, NOT a fresh dlsym site
 * scattered through runtime.c.
 */

/* SCE registry key IDs we care about, lifted from ps5-payload-dev/
 * regdump's regmgr.h. The numeric layout is Sony's:
 *   high 8 bits  = namespace (0x05 = DATE)
 *   middle bits  = "category" within namespace
 *   low bits     = entry index
 * We hardcode the resolved 32-bit IDs rather than constructing them
 * from macros so a regression against regdump's table is a one-line
 * grep, and so this header has zero external include deps. */
#define SCE_KEY_DATE_TIME_ZONE          0x05010000U  /* int — tz enum index */
#define SCE_KEY_DATE_DATE_FORMAT        0x05020000U  /* int — 0=YMD 1=DMY 2=MDY */
#define SCE_KEY_DATE_TIME_FORMAT        0x05030000U  /* int — 0=24h 1=12h    */
#define SCE_KEY_DATE_SUMMER_TIME        0x05040000U  /* int — 0=off 1=auto 2=on */
#define SCE_KEY_DATE_SET_AUTO           0x05050000U  /* int — 0=manual 1=NTP */
#define SCE_KEY_DATE_IS_SUMMER_TIME     0x05060000U  /* int — RO, current DST */
#define SCE_KEY_DATE_UTC_OFFSET         0x05070000U  /* int — seconds */
#define SCE_KEY_DATE_TIMEZONE_OFFSET    0x05080000U  /* int — minutes */
#define SCE_KEY_DATE_TZDATA_UPDATE      0x05090000U  /* str — e.g. "2023d" */
#define SCE_KEY_DATE_RTC_ERROR_COUNT    0x05190000U  /* int — NTP sync fail counter */

/* Diagnostic err_code sentinels. Same 0xE000_xxxx range as
 * sys_time.h / bgft.h. Chosen so the desktop can distinguish "we
 * couldn't even attempt the call" from "Sony returned an error". */
#define SYS_REGISTRY_ERR_NULL_ARG      0xE0003001U
#define SYS_REGISTRY_ERR_NO_SYMBOL     0xE0003002U
#define SYS_REGISTRY_ERR_BUFFER_TOO_SMALL 0xE0003003U

/* Read an int (4-byte) registry value. Returns 0 on success and
 * writes *out_val. On failure returns -1 with *out_err_code set to
 * one of the SYS_REGISTRY_ERR_* sentinels OR Sony's raw rc cast to
 * uint32. Caller may pass NULL for out_err_code if they don't care. */
int sys_registry_get_int(uint32_t key, int *out_val, uint32_t *out_err_code);

/* Write an int (4-byte) registry value. Same return convention. */
int sys_registry_set_int(uint32_t key, int val, uint32_t *out_err_code);

/* Read a string (NUL-terminated, up to buf_size bytes) registry
 * value. Same return convention. The buffer is always NUL-terminated
 * on success even if Sony's value was max-length without a NUL. */
int sys_registry_get_str(uint32_t key,
                          char *buf, size_t buf_size,
                          uint32_t *out_err_code);

/* Write a string registry value. `len` is the field width Sony
 * expects for the key (e.g. 32 for an account-name slot), NOT
 * strlen(buf) — Sony copies a fixed-width field. Same return
 * convention as the other writers. Added for the profile (offline
 * account) feature, which renames account-name slots. */
int sys_registry_set_str(uint32_t key,
                         const char *buf, size_t len,
                         uint32_t *out_err_code);

/* Read a binary (fixed-width) registry value into `buf`. Used for
 * the 8-byte account-id slots. Same return convention. */
int sys_registry_get_bin(uint32_t key,
                         void *buf, size_t buf_size,
                         uint32_t *out_err_code);

/* Write a binary (fixed-width) registry value. Same return convention. */
int sys_registry_set_bin(uint32_t key,
                         const void *buf, size_t buf_size,
                         uint32_t *out_err_code);

/* Read NTP-derived tick (libSceRtc) — used by the time/state get
 * frame to surface "what time NTP would say it is right now"
 * alongside the wall clock. Returns 0 on success with *out_unix_seconds
 * populated; -1 on any failure (symbol missing, Sony rc non-zero).
 *
 * Note: this reads what the system has CACHED from its last NTP
 * sync; it does NOT trigger a fresh sync. Use the SET_AUTO write
 * for that. The cached value can be stale by hours/days if the
 * console hasn't recently been online. */
int sys_registry_get_ntp_tick_unix(int64_t *out_unix_seconds,
                                    uint32_t *out_err_code);

#endif /* PS5UPLOAD2_SYS_REGISTRY_H */
