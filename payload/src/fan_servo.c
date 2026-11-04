#include "fan_servo.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int fan_servo_get(fan_servo_io_fn io, void *ctx, uint8_t obj[FAN_SERVO_LEN]) {
    memset(obj, 0, FAN_SERVO_LEN); /* selector 0 at byte 2 */
    return io(ctx, FAN_SERVO_IOCTL_GET, obj) == 0 ? 0 : -1;
}

int fan_servo_set_target(fan_servo_io_fn io, void *ctx, uint8_t c, uint8_t before[FAN_SERVO_LEN],
                         uint8_t after[FAN_SERVO_LEN], int *others_changed) {
    uint8_t cur[FAN_SERVO_LEN], req[FAN_SERVO_LEN], back[FAN_SERVO_LEN];
    if (others_changed) *others_changed = 0;
    if (fan_servo_get(io, ctx, cur) != 0) return FAN_SERVO_E_GET;
    if (before) memcpy(before, cur, FAN_SERVO_LEN);
    memcpy(req, cur, FAN_SERVO_LEN);
    req[FAN_SERVO_TARGET] = c;
    /* SET writes its reply into the buffer it was given: send a copy so `req` stays what we sent */
    memcpy(back, req, FAN_SERVO_LEN);
    if (io(ctx, FAN_SERVO_IOCTL_SET, back) != 0) return FAN_SERVO_E_SET;
    if (fan_servo_get(io, ctx, back) != 0) return FAN_SERVO_E_VERIFY;
    if (after) memcpy(after, back, FAN_SERVO_LEN);
    if (others_changed) {
        for (int i = 2; i < FAN_SERVO_LEN; i++) {
            if (i != FAN_SERVO_TARGET && back[i] != req[i]) {
                *others_changed = 1;
                break;
            }
        }
    }
    return back[FAN_SERVO_TARGET] == c ? FAN_SERVO_OK : FAN_SERVO_E_NOT_APPLIED;
}

void fan_servo_hex(const uint8_t obj[FAN_SERVO_LEN], char out[2 * FAN_SERVO_LEN + 1]) {
    static const char k_hex[] = "0123456789abcdef";
    for (int i = 0; i < FAN_SERVO_LEN; i++) {
        out[2 * i] = k_hex[obj[i] >> 4];
        out[2 * i + 1] = k_hex[obj[i] & 15];
    }
    out[2 * FAN_SERVO_LEN] = '\0';
}

fan_watch_t fan_watch_decide(const fan_guard_t *g, int pinned_c, int current_c) {
    if (pinned_c <= 0 || g->failing) return FAN_WATCH_IDLE;
    /* A failed GET proves nothing, and writing blind is what this replaces: wait for the next tick. */
    if (current_c < 0 || current_c == pinned_c) return FAN_WATCH_HOLD;
    return FAN_WATCH_RESTORE;
}

void fan_guard_note(fan_guard_t *g, int ok, int64_t now_s) {
    if (ok) {
        g->fails = 0;
        g->restores++;
        g->last_restore_s = now_s;
        return;
    }
    if (++g->fails >= FAN_GUARD_MAX_FAILS) g->failing = 1;
}

void fan_guard_clear(fan_guard_t *g) {
    g->fails = 0;
    g->failing = 0;
}

const char *fan_guard_name(const fan_guard_t *g, int pinned_c) {
    if (pinned_c <= 0) return "off";
    return g->failing ? "failing" : "ok";
}

long fan_guard_last_ago(const fan_guard_t *g, int64_t now_s) {
    if (g->last_restore_s < 0) return -1;
    return now_s >= g->last_restore_s ? (long)(now_s - g->last_restore_s) : 0;
}

const char *fan_target_refusal(int c, int stock_c) {
    /* A target above the console's own lets the chip run hotter than Sony's control would. */
    if (stock_c > 0 && c > stock_c) return "fan_above_stock";
    return NULL;
}

int fan_threshold_body_parse(const char *body, size_t len, int lo, int hi, int *threshold, int *reapply_sec) {
    char buf[32], *end;
    long v;
    if (!body || len == 0 || len >= sizeof buf) return -1;
    memcpy(buf, body, len);
    buf[len] = '\0';
    if (strlen(buf) != len) return -1; /* an embedded NUL */
    errno = 0;
    v = strtol(buf, &end, 10);
    if (end == buf || errno != 0) return -1;
    if (v != 0 && (v < lo || v > hi)) return -1;
    *threshold = (int)v;
    *reapply_sec = 0;
    while (*end == ' ' || *end == '\t') end++;
    if (*end >= '0' && *end <= '9') {
        long r = strtol(end, &end, 10);
        if (errno != 0 || r > 0x7fffffffL) return -1;
        *reapply_sec = (int)r;
    }
    while (*end == ' ' || *end == '\t' || *end == '\r' || *end == '\n') end++;
    return *end == '\0' ? 0 : -1;
}

int fan_reapply_parse(const char *text, int def, int lo, int hi) {
    int v = 0;
    if (!text) return def;
    if (sscanf(text, "v3 %d", &v) == 1) return v < lo || v > hi ? def : v;
    /* Older files: "v2 <s>" was what the loader wanted, a bare "<s>" what the saver wrote, so the
     * value was 15 (the old default) unless someone chose another. */
    if (sscanf(text, "v2 %d", &v) != 1 && sscanf(text, "%d", &v) != 1) return def;
    if (v == 15 || v < lo || v > hi) return def;
    return v;
}

int fan_ext_filtered_tenths(const uint8_t ext[FAN_EXT_LEN]) {
    uint32_t q8;
    memcpy(&q8, ext + 0x10, sizeof q8);
    /* Q8 -> tenths, rounded */
    uint64_t tenths = ((uint64_t)q8 * 10u + 128u) / 256u;
    if (tenths < 200u || tenths > 1100u) return -1;
    return (int)tenths;
}

int fan_duty_q10_pct(unsigned duty_q10) {
    unsigned pct = (duty_q10 * 100u + 512u) / 1024u;
    return pct > 100u ? 100 : (int)pct;
}
