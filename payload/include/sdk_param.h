#ifndef PS5UPLOAD_SDK_PARAM_H
#define PS5UPLOAD_SDK_PARAM_H

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* param.json version rewriting for the SDK Version Changer.
 *
 * A PS5 title records the firmware it demands in sce_sys/param.json as
 * `requiredSystemSoftwareVersion`, and the SDK it was built against as
 * `sdkVersion`. Lowering those is what lets a game built for newer
 * firmware start on an older jailbroken console.
 *
 * Split out from sdk_changer.c so the rewrite is testable on the host.
 * The previous in-place implementation could only replace a value when
 * the replacement was byte-for-byte the same length, and silently
 * changed nothing otherwise — while the caller still reported success.
 * Rewriting into an output buffer removes that restriction and makes the
 * changed-count observable.
 *
 * Tests: payload/tests/sdk_param_selftest.c. */

/* The literal param.json stores, e.g. 0x0505000000000000 for FW 5.05. */
static inline int sdk_param_format_version(uint32_t target_sdk, char *out,
                                           size_t cap) {
    if (!out || cap == 0) return -1;
    int n = snprintf(out, cap, "0x%08x00000000", target_sdk);
    if (n < 0 || (size_t)n >= cap) return -1;
    return n;
}

/* Replace the string value of `field` in `in`, appending to `out`.
 * Returns 1 if the field was found and rewritten, 0 if absent. */
static inline int sdk_param_replace_field(const char *in, size_t in_len,
                                          const char *field, const char *value,
                                          char *out, size_t out_cap,
                                          size_t *out_len) {
    char needle[64];
    int nn = snprintf(needle, sizeof(needle), "\"%s\"", field);
    if (nn < 0 || (size_t)nn >= sizeof(needle)) return 0;

    const char *key = strstr(in, needle);
    if (!key) return 0;
    const char *colon = strchr(key, ':');
    if (!colon) return 0;
    const char *p = colon + 1;
    while (*p == ' ' || *p == '\t') p++;
    if (*p != '"') return 0;
    p++; /* opening quote of the value */
    const char *close = strchr(p, '"');
    if (!close) return 0;

    size_t head = (size_t)(p - in);
    size_t tail_off = (size_t)(close - in);
    size_t vlen = strlen(value);
    size_t total = head + vlen + (in_len - tail_off);
    if (total >= out_cap) return 0;

    memcpy(out, in, head);
    memcpy(out + head, value, vlen);
    memcpy(out + head + vlen, in + tail_off, in_len - tail_off);
    if (out_len) *out_len = total;
    return 1;
}

/* Rewrite both version fields. Returns how many were actually changed —
 * 0 means the document did not contain them, which the caller must
 * report rather than claim success. */
static inline int sdk_param_rewrite_json(const char *in, size_t in_len,
                                         uint32_t target_sdk, char *out,
                                         size_t out_cap, size_t *out_len) {
    if (!in || !out || out_cap == 0) return -1;

    char value[32];
    if (sdk_param_format_version(target_sdk, value, sizeof(value)) < 0) return -1;

    static const char *const fields[] = {"sdkVersion",
                                         "requiredSystemSoftwareVersion"};
    /* Ping-pong between two buffers so each field rewrite sees the
     * previous one's output. */
    if (in_len >= out_cap) return -1;
    memcpy(out, in, in_len);
    out[in_len] = '\0';
    size_t cur_len = in_len;

    char scratch[8192];
    int changed = 0;
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); i++) {
        if (cur_len >= sizeof(scratch)) return changed;
        size_t next_len = 0;
        if (sdk_param_replace_field(out, cur_len, fields[i], value, scratch,
                                    sizeof(scratch), &next_len)) {
            if (next_len >= out_cap) return changed;
            memcpy(out, scratch, next_len);
            out[next_len] = '\0';
            cur_len = next_len;
            changed++;
        }
    }

    if (out_len) *out_len = cur_len;
    return changed;
}

#endif
