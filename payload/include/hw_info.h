#ifndef PS5UPLOAD2_HW_INFO_H
#define PS5UPLOAD2_HW_INFO_H

#include <stddef.h>
#include <stdint.h>

/*
 * Hardware monitoring APIs — GET_HW_INFO / GET_TEMPS / GET_POWER_INFO.
 *
 * Three frames back the Hardware tab in the client:
 *   - HW_INFO:  static info (model, serial, OS, RAM, CPU count).
 *               Read ONCE at first call and cached forever -- these
 *               values don't change at runtime. Served from cache on
 *               every subsequent call.
 *   - HW_TEMPS: live sensors (CPU temp, SoC temp, CPU freq, power).
 *               1-second cache so a fast-polling UI doesn't hammer
 *               the kernel APIs. Power API has a separate 5-second
 *               throttle because it's measurably heavier than the
 *               others and destabilizes the payload when queried
 *               too often.
 *   - HW_POWER: uptime + operating-time stats via kern.boottime.
 *               No Sony-specific APIs needed, so always-safe.
 *
 * Returns newline-separated "key=value" text bodies (not JSON) to
 * keep the payload parser trivial. JSON would be more structured but
 * adds payload complexity for no real win on these fixed-schema
 * responses.
 *
 * All three APIs are idempotent and thread-safe. Multiple concurrent
 * HW_TEMPS calls from different client connections share the 1-second
 * cache via pthread_mutex_t.
 */

/* out[] must be >= 2048 bytes. Sets *out_written to the text length
 * (not NUL-terminated). Returns 0 on success, -1 on failure. */
int hw_info_get_text(char *out, size_t out_cap, size_t *out_written,
                     const char **err_reason_out);

/* BASIC reading (CPU/SoC temp + CPU clock) — the only sensors safe to
 * fire on an auto-poll. This is what the Dashboard's 5 s tick uses. */
int hw_temps_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out);

/* Per-call selectors for the EXTENDED telemetry. Each newer/risky Sony
 * getter is independently gated so a firmware that wedges on ONE of them
 * (e.g. FW 9.60 Pro hangs on SoC power) can still serve the others. The
 * HW_TEMPS request body selects them: "1" = ALL (back-compat), or any
 * subset of the chars p/u/f/s. An empty body = basic (none). */
#define HW_EXT_POWER  0x1   /* 'p' sceKernelGetSocPowerConsumption  */
#define HW_EXT_USAGE  0x2   /* 'u' sceKernelGetCpuUsageAll          */
#define HW_EXT_FAN    0x4   /* 'f' sceKernelGetCurrentFanDuty       */
#define HW_EXT_SHAPE  0x8   /* 's' sceKernelGetBasicProductShape    */
#define HW_EXT_ALL    (HW_EXT_POWER | HW_EXT_USAGE | HW_EXT_FAN | HW_EXT_SHAPE)

/* `flags` form. flags=0 is identical to hw_temps_get_text (basic). Each
 * HW_EXT_* bit additionally reads that one getter — caller decides which,
 * so a known-wedging call can be excluded per firmware. MUST only carry
 * non-zero flags for an explicit user "Read sensors" request, never on an
 * auto-poll, so a wedge stays a recoverable, user-triggered event. */
int hw_temps_get_text_ex(int flags, char *out, size_t out_cap,
                         size_t *out_written, const char **err_reason_out);

int hw_power_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out);

/* HW_STORAGE — "Console Storage" aggregate matching what PS5 Settings
 * shows. Sums /user effective + /system_data + /system_ex from
 * statfs(2). Distinct from FS_LIST_VOLUMES, which lists per-volume
 * detail; HW_STORAGE is the single-line summary the System tab
 * surfaces.
 *
 * Output keys: total_bytes, free_bytes, used_bytes, reserved_bytes,
 * user_total_bytes, user_free_bytes, user_reserved_bytes,
 * system_data_total_bytes, system_data_free_bytes,
 * system_ex_total_bytes, system_ex_free_bytes.
 *
 * Always succeeds — missing partitions just contribute zero to the
 * total (e.g., /system_ex isn't always mounted on every firmware). */
int hw_storage_get_text(char *out, size_t out_cap, size_t *out_written,
                         const char **err_reason_out);

/* Set the fan-turbo temperature threshold in °C.
 *
 * Mechanism: `open("/dev/icc_fan") → ioctl(0xC01C8F07, &buf) → close`.
 * The device node is exposed by the ICC thermal driver in FreeBSD 11
 * and does NOT require the shellui runtime context that
 * sceKernelGet*Temperature
 * needs, which is why we can fire it from a standalone payload.
 *
 * `threshold_c` is clamped to [HW_FAN_THRESHOLD_MIN, HW_FAN_THRESHOLD_MAX]
 * inside the function — callers don't need to pre-validate. Persists
 * until PS5 reboot.
 *
 * Returns 0 on success; -1 with *err_reason_out set on failure
 * ("icc_fan_open_failed", "icc_fan_ioctl_failed"). */
int hw_fan_set_threshold(uint8_t threshold_c, const char **err_reason_out);

/* Safe clamp bounds. 45 °C floor keeps the fan from running at turbo
 * during normal idle; 80 °C ceiling leaves thermal headroom before
 * Sony's ~95 °C emergency cutoff. */
#define HW_FAN_THRESHOLD_MIN 45
#define HW_FAN_THRESHOLD_MAX 80

/* Returns the currently-pinned threshold in °C, or 0 if the user has
 * never set one this session (in which case the auto-reapply watcher
 * stays dormant on each tick). */
int hw_fan_pinned_threshold(void);

/* Re-arm the auto-reapply watcher with a new pin value. Called
 * internally by `hw_fan_set_threshold` on every successful set so the
 * desktop doesn't need a separate "keep pinned" command. Safe to call
 * before the watcher thread is started — it just stores the value and
 * the thread (which is started lazily on the first set) will pick it
 * up on its next tick. */
void hw_fan_pin_threshold(uint8_t threshold_c);

#endif /* PS5UPLOAD2_HW_INFO_H */
