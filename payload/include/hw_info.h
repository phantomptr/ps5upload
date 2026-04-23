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

int hw_temps_get_text(char *out, size_t out_cap, size_t *out_written,
                      const char **err_reason_out);

int hw_power_get_text(char *out, size_t out_cap, size_t *out_written,
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

#endif /* PS5UPLOAD2_HW_INFO_H */
