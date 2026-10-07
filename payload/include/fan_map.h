#ifndef PS5UPLOAD_FAN_MAP_H
#define PS5UPLOAD_FAN_MAP_H

/* Pure curve -> temperature mapping (no hardware, no Sony API), kept apart from
 * fan_curve.c so the host-built ava1-ctest suite can pin it.
 *
 * What /dev/icc_fan ioctl 0xC01C8F07 takes is ONE temperature: the one the
 * firmware's own fan control works to hold. It is not a curve. Lower means
 * louder: under load the fans run as hard as it takes to stay there.
 *
 * The console's own value is NOT 60 C, as this file said until it was read
 * back: it is 91 C on FW 13.60 (CFI-1115A and CFI-7019, ioctl 0xC01C8F08). So a
 * curve "capped at stock 60" ran the fans far harder than the console does by
 * itself, whatever the curve looked like (#400, after #354). */

/* The most we set (HW_FAN_THRESHOLD_MAX, checked in fan_curve.c). */
#define FAN_MAP_MAX_C   80
/* Floor, equal to HW_FAN_THRESHOLD_MIN (checked in fan_curve.c). */
#define FAN_MAP_MIN_C   45
/* "Leave it to the console": restore its own value and stop overriding it. */
#define FAN_MAP_CONSOLE_OWN 0

/* The temperature (C) for `{"points":[{"temp_c":N,"duty_pct":N},...]}`: the
 * lowest one at which the user asked for 100% duty, floored at FAN_MAP_MIN_C.
 * A curve that never asks for 100%, or only above FAN_MAP_MAX_C, maps to
 * FAN_MAP_CONSOLE_OWN. Returns -1 when the body has no readable point. */
int fan_map_threshold(const char *points_json);

#endif
