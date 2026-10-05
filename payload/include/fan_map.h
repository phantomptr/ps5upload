#ifndef PS5UPLOAD_FAN_MAP_H
#define PS5UPLOAD_FAN_MAP_H

/* Pure curve -> threshold mapping (no hardware, no Sony API), kept apart from
 * fan_curve.c so the host-built ava1-ctest suite can pin it.
 *
 * What /dev/icc_fan ioctl 0xC01C8F07 takes is ONE temperature: the point at
 * which the firmware's own fan control goes to turbo (stock 60 C; the
 * ps5-fan-threshold / elf-arsenal references both describe it that way). It is
 * not a curve and not a target to hold. Passing the curve's lowest temperature
 * (what v4.1..v5.41 did) therefore makes the fans run flat out from that
 * temperature up: issue #354. */

/* Stock firmware threshold. A curve never raises the turbo point above this:
 * the console may run its fans harder than the curve asks, never softer than
 * stock, so a curve cannot cost thermal headroom. */
#define FAN_MAP_STOCK_C 60
/* Floor, equal to HW_FAN_THRESHOLD_MIN (checked in fan_curve.c). */
#define FAN_MAP_MIN_C   45

/* The turbo threshold (C) for `{"points":[{"temp_c":N,"duty_pct":N},...]}`:
 * the lowest temperature at which the user asked for 100% duty, clamped to
 * [FAN_MAP_MIN_C, FAN_MAP_STOCK_C]. A curve that never asks for 100% maps to
 * FAN_MAP_STOCK_C. Returns -1 when the body has no readable point. */
int fan_map_threshold(const char *points_json);

#endif
