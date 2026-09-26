#ifndef PS5UPLOAD_FAN_CURVE_H
#define PS5UPLOAD_FAN_CURVE_H

#include <stdint.h>
#include <stddef.h>

/* Set the fan curve.
 *
 * `points_json` is a JSON body of the form:
 *   {"points":[{"temp_c":N,"duty_pct":N},...]}
 *
 * The curve is persisted to /data/ps5upload/fan_curve.json AND applied
 * to hardware via hw_fan_set_threshold(): the first point's temperature
 * becomes the pinned fan threshold, so hw_info's watcher keeps re-applying
 * it across the firmware's per-game-launch fan resets (a one-shot ioctl
 * would silently revert).
 *
 * On failure, `err` is filled with a human-readable reason (if err_cap > 0).
 * Returns 0 on success, -1 on error. */
int fan_curve_set(const char *points_json, char *err, size_t err_cap);

/* Read back the stored fan curve.
 *
 * Writes the raw JSON from /data/ps5upload/fan_curve.json into `buf`.
 * If no curve has been saved, writes {"points":[]}.
 *
 * Returns 0 on success, -1 on error. */
int fan_curve_get(char *buf, size_t cap);

#endif
