#ifndef PS5UPLOAD_FAN_SERVO_H
#define PS5UPLOAD_FAN_SERVO_H

#include <stddef.h>
#include <stdint.h>

/* The fan controller's runtime autoservo object and the decisions around it, kept apart from
 * hw_info.c (no device, no Sony API) so a host test can drive them with a fake ioctl.
 *
 * /dev/icc_fan 0xC01C8F08 (GET) and 0xC01C8F07 (SET) both move exactly 28 bytes:
 *   0x00 u16  ICC result
 *   0x02 u8   selector (0 = the primary profile)
 *   0x03 u8   flag
 *   0x04 6 x u32 parameters; byte 5 is the temperature (C) the controller works to hold.
 * SET forwards bytes 2..27 to the controller, so a SET built from zeros with only byte 5 set
 * also zeroes the selector and the other parameters. The safe write is GET, change byte 5 of
 * that copy, SET it whole, GET again and compare (drakmor/fan_target's technical reference). */

#define FAN_SERVO_LEN       28 /* = IOCPARM_LEN of both ioctls: the kernel copies this many */
#define FAN_SERVO_TARGET    5
#define FAN_SERVO_IOCTL_SET 0xC01C8F07UL
#define FAN_SERVO_IOCTL_GET 0xC01C8F08UL
/* Current duty (20 bytes, Q10 duty at 0x02) and extended state (20 bytes: Q8 filtered
 * temperature at 0x10). Both read-only. */
#define FAN_DUTY_IOCTL_GET  0xC0148F02UL
#define FAN_EXT_IOCTL_GET   0xC0148F09UL
#define FAN_DUTY_LEN        20
#define FAN_EXT_LEN         20

/* One ioctl on an open /dev/icc_fan: 0 on success. */
typedef int (*fan_servo_io_fn)(void *ctx, unsigned long req, uint8_t *buf);

enum {
    FAN_SERVO_OK = 0,
    FAN_SERVO_E_GET = -1,         /* the first GET failed: nothing was written */
    FAN_SERVO_E_SET = -2,         /* the SET was refused */
    FAN_SERVO_E_VERIFY = -3,      /* the SET went in but the GET after it failed */
    FAN_SERVO_E_NOT_APPLIED = -4, /* the GET after it shows another target */
};

/* GET with selector 0 into obj (zeroed first). 0, or -1. */
int fan_servo_get(fan_servo_io_fn io, void *ctx, uint8_t obj[FAN_SERVO_LEN]);

/* Set the target to `c`: GET, copy, byte 5 = c, SET the copy, GET again. `before` gets the
 * first GET, `after` the last one (either may be NULL). *others_changed (may be NULL) is 1 when
 * any of bytes 2..27 other than 5 differs between the SET object and the read-back. That is
 * not a failure (the controller owns those bytes); the caller logs it. FAN_SERVO_*. */
int fan_servo_set_target(fan_servo_io_fn io, void *ctx, uint8_t c, uint8_t before[FAN_SERVO_LEN],
                         uint8_t after[FAN_SERVO_LEN], int *others_changed);

/* 56 lowercase hex characters + NUL. */
void fan_servo_hex(const uint8_t obj[FAN_SERVO_LEN], char out[2 * FAN_SERVO_LEN + 1]);

/* ── The guard: what keeps a pinned target in place ───────────────
 *
 * The system puts its own target back whenever it switches fan pattern (home <-> game), so a
 * pinned target is watched: GET every tick, SET only when byte 5 differs. After
 * FAN_GUARD_MAX_FAILS failed SETs in a row it stops writing and leaves the console's own
 * control in charge until the user sets a target again. */
#define FAN_GUARD_MAX_FAILS 5

typedef struct {
    int fails;              /* failed SETs in a row */
    int failing;            /* stopped writing */
    uint32_t restores;      /* targets put back after the system reset them */
    int64_t last_restore_s; /* monotonic seconds of the last one, -1 = never */
} fan_guard_t;

#define FAN_GUARD_INIT { 0, 0, 0, -1 }

typedef enum {
    FAN_WATCH_IDLE,    /* nothing pinned, or the guard gave up: no ioctl at all */
    FAN_WATCH_HOLD,    /* the target is ours: nothing to write */
    FAN_WATCH_RESTORE, /* the target differs: write ours back */
} fan_watch_t;

/* What a tick does with `pinned_c` (0 = nothing pinned) and `current_c` (byte 5 as read now,
 * -1 when the GET failed). */
fan_watch_t fan_watch_decide(const fan_guard_t *g, int pinned_c, int current_c);
/* The outcome of a FAN_WATCH_RESTORE write at monotonic `now_s`. */
void fan_guard_note(fan_guard_t *g, int ok, int64_t now_s);
/* A target the user set (or a restore to the console's own): writing may start again. The
 * restore count stays: it is the session's. */
void fan_guard_clear(fan_guard_t *g);
/* "off" (nothing pinned), "failing" or "ok". */
const char *fan_guard_name(const fan_guard_t *g, int pinned_c);
/* Seconds since the last restore, or -1 (never). */
long fan_guard_last_ago(const fan_guard_t *g, int64_t now_s);

/* ── Policy and decoding ──────────────────────────────────────── */

/* NULL when `c` may be set with the console's own target `stock_c` (0 = not known), else the
 * refusal: "fan_above_stock" (it would make the fans slower than the console's own setting). The
 * [HW_FAN_THRESHOLD_MIN, MAX] clamp is the caller's. */
const char *fan_target_refusal(int c, int stock_c);

/* An HW_FAN_THRESHOLD request: "<c>" or "<c> <reapply_s>" in ASCII, with optional surrounding
 * spaces and a trailing newline (shells add them). <c> is a target in [lo, hi], or 0 for "give the
 * fan back to the console". 0 with both set (*reapply_sec 0 = not given), or -1 for anything else:
 * an invalid request must change nothing, so it never falls back to a default target. */
int fan_threshold_body_parse(const char *body, size_t len, int lo, int hi, int *threshold, int *reapply_sec);

/* fan_reapply.conf: the watcher's tick in seconds. "v3 <s>" is current. Anything older ("v2 <s>"
 * or a bare number, written while the tick was 15 s) reads as `def` when it says 15, the old
 * default, and as itself otherwise. Out of [lo, hi] or unreadable: `def`. The caller writes the
 * value back as v3, so the move to `def` happens once. */
int fan_reapply_parse(const char *text, int def, int lo, int hi);

/* Filtered temperature from an 8F09 reply, in tenths of a C; -1 outside 20..110 C. */
int fan_ext_filtered_tenths(const uint8_t ext[FAN_EXT_LEN]);
/* Q10 duty (1024 = 100 %) as a whole percentage, clamped to 0..100. */
int fan_duty_q10_pct(unsigned duty_q10);

#endif
