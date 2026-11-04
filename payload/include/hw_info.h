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
 *               the kernel APIs, and one reader at a time that nobody
 *               waits for (hw_temps_flight.h). Besides the values it
 *               says how the read went: cpu_temp_rc and soc_temp_rc
 *               (the getters' return codes, or fault / missing / range /
 *               none), read_ms, and stale=1 when another thread's read
 *               was still running, so these are the last values it had.
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
 * cache and never wait for each other's Sony calls.
 */

/* out[] must be >= 2048 bytes. Sets *out_written to the text length
 * (not NUL-terminated). Returns 0 on success, -1 on failure. */
int hw_info_get_text(char *out, size_t out_cap, size_t *out_written,
                     const char **err_reason_out);

/* Per-call selectors for the EXTENDED telemetry. Each newer/risky Sony
 * getter is independently gated so a firmware that wedges on ONE of them
 * (e.g. FW 9.60 Pro hangs on SoC power) can still serve the others. The
 * HW_TEMPS request body selects them: "1" = ALL (back-compat), or any
 * subset of the chars p/u/f/s/c/d. An empty body = basic (none). */
#define HW_EXT_POWER  0x1   /* 'p' sceKernelGetSocPowerConsumption  */
#define HW_EXT_USAGE  0x2   /* 'u' sceKernelGetCpuUsageAll          */
#define HW_EXT_FAN    0x4   /* 'f' sceKernelGetCurrentFanDuty       */
#define HW_EXT_SHAPE  0x8   /* 's' sceKernelGetBasicProductShape    */
#define HW_EXT_CLOCKS 0x10  /* 'c' GPU + per-core CPU clock (/dev/mp1, mp1_clock.h) */
#define HW_EXT_DEBUG  0x20  /* 'd' with 'c': also the raw mp1 words (mp1_clk_raw, mp1_core_raw) */
/* "1" selects HW_EXT_ALL, which deliberately leaves out CLOCKS and DEBUG: the SMU read is
 * new and unverified on most firmware, so it happens only when 'c' is sent explicitly. */
#define HW_EXT_ALL    (HW_EXT_POWER | HW_EXT_USAGE | HW_EXT_FAN | HW_EXT_SHAPE)

/* flags=0 is the BASIC reading (CPU/SoC temp + CPU clock), the only
 * sensors safe to fire on an auto-poll. Each HW_EXT_* bit additionally reads that one getter — caller decides which,
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
 * Mechanism: `open("/dev/icc_fan")`, GET the 28-byte profile (0xC01C8F08), change the target
 * byte, SET it whole (0xC01C8F07), GET again and compare (fan_servo.h).
 * The device node is exposed by the ICC thermal driver in FreeBSD 11
 * and does NOT require the shellui runtime context that
 * sceKernelGet*Temperature
 * needs, which is why we can fire it from a standalone payload.
 *
 * `threshold_c` is clamped to [HW_FAN_THRESHOLD_MIN, HW_FAN_THRESHOLD_MAX]
 * inside the function — callers don't need to pre-validate. Pinned and
 * persisted: the watcher puts it back whenever the system resets it.
 *
 * Returns 0 on success; -1 with *err_reason_out set on failure
 * ("icc_fan_open_failed", "icc_fan_read_failed" (nothing written),
 * "icc_fan_ioctl_failed", "icc_fan_verify_failed", "icc_fan_not_applied",
 * "fan_above_stock" (higher than the console's own target: fans slower
 * than stock)). A user set also lets a guard that gave up write again. */
int hw_fan_set_threshold(uint8_t threshold_c, const char **err_reason_out);

/* The temperature the controller is working to right now (read back from /dev/icc_fan), or
 * -1 when it cannot be read. */
int hw_fan_read_target(void);

/* The console's own value: what the controller held before this helper first changed it
 * (remembered in fan_stock.conf), or 0 when it was never seen. */
int hw_fan_stock_target(void);

/* Whether a reading of `current_c` is the console's own value rather than ours (`ours_c` is
 * what this helper last applied or has on file, 0 = nothing). */
int hw_fan_is_stock_reading(int current_c, int ours_c);

/* Give the fan back to the console: apply its own value, stop re-applying ours and forget
 * the saved threshold. 0 on success (also when nothing of ours was in force); -1 with
 * *err_reason_out: "fan_restore_needs_restart" (ours was dropped, but the console's value was
 * never seen, so it returns at the next restart), "icc_fan_ioctl_failed",
 * "icc_fan_not_applied". */
int hw_fan_restore_stock(const char **err_reason_out);

/* Safe clamp bounds. 45 °C floor keeps the fan from running at turbo
 * during normal idle; the 85 °C ceiling is the mildest drakmor/fan_target
 * build and still under the console's own 91 °C (anything above the
 * console's own value is refused besides: hw_fan_set_threshold). */
#define HW_FAN_THRESHOLD_MIN 45
#define HW_FAN_THRESHOLD_MAX 85

/* Current fan duty as Q10 PWM (1024 = 100 %) from /dev/icc_fan 0xC0148F02, or -1. */
int hw_fan_read_duty_q10(void);

/* The fan lines of HW_TEMPS, appended as key=value lines: fan_pinned_c, fan_target_c (-1
 * unreadable), fan_stock_c (0 never seen), fan_min_c, fan_max_c, fan_restores (targets put back
 * after the system reset them, this session), fan_last_restore_s (seconds ago, -1 never),
 * fan_guard (off|ok|failing), fan_filtered_c (the controller's filtered temperature, one
 * decimal, -1 unreadable or outside 20..110), fan_servo (the 28-byte profile as 56 hex
 * characters, empty when unreadable). snprintf's return. */
int hw_fan_status_text(char *out, size_t cap);

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

/* Load the persisted fan threshold from
 * /data/ps5upload/fan_threshold.conf. Returns the °C value, or 0 if
 * no valid persisted file exists. Call once at boot (after
 * runtime_ensure_directories) to restore the user's last setting —
 * a zero return means "nothing pinned, leave fan at firmware default". */
int hw_fan_load_persisted(void);

/* Watcher tick — how often the watcher reads the target back and, when
 * the system reset it, writes the pinned one again (in seconds). Default
 * 2 s: a read costs one ioctl and a write happens only after a reset.
 * Persisted across reboots. */
int hw_fan_reapply_interval(void);
void hw_fan_set_reapply_interval(int seconds);

/* Load the persisted reapply interval from
 * /data/ps5upload/fan_reapply.conf. Returns the seconds value, or
 * FAN_REAPPLY_DEFAULT_SEC (2) if no valid persisted file exists (a file
 * from the 15 s builds that says 15 reads as 2: fan_reapply_parse).
 * Call once at boot alongside hw_fan_load_persisted. */
int hw_fan_load_reapply_interval(void);

#endif /* PS5UPLOAD2_HW_INFO_H */
