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

/* Replace the string value of `field` IN PLACE in `buf`, a NUL-terminated
 * buffer of `cap` bytes holding `*len` bytes of text. Returns 1 if the field
 * was rewritten, 0 if it is absent (or its value is not a string), -1 if the
 * result does not fit. No second buffer: a fixed scratch here is what used to
 * cap the whole document at 8 KiB. */
static inline int sdk_param_replace_in_place(char *buf, size_t *len, size_t cap,
                                             const char *field,
                                             const char *value) {
    char needle[64];
    int nn = snprintf(needle, sizeof(needle), "\"%s\"", field);
    if (nn < 0 || (size_t)nn >= sizeof(needle)) return 0;

    const char *key = strstr(buf, needle);
    if (!key) return 0;
    const char *colon = strchr(key, ':');
    if (!colon) return 0;
    const char *p = colon + 1;
    while (*p == ' ' || *p == '\t') p++;
    if (*p != '"') return 0;
    p++; /* opening quote of the value */
    const char *close = strchr(p, '"');
    if (!close) return 0;

    size_t head = (size_t)(p - buf);
    size_t tail_off = (size_t)(close - buf);
    if (tail_off > *len) return 0; /* past the text the caller described */
    size_t tail_len = *len - tail_off;
    size_t vlen = strlen(value);
    if (vlen >= cap || head >= cap - vlen || tail_len >= cap - vlen - head) return -1;

    memmove(buf + head + vlen, buf + tail_off, tail_len);
    memcpy(buf + head, value, vlen);
    *len = head + vlen + tail_len;
    buf[*len] = '\0';
    return 1;
}

/* Rewrite both version fields. Returns how many were actually changed —
 * 0 means the document did not contain them, which the caller must
 * report rather than claim success — or -1 when the document or its
 * rewritten form does not fit `out_cap` (`out` is then not to be used).
 *
 * -1, never 0, for "does not fit": this used to copy through an 8 KiB stack
 * buffer and return 0 for anything larger, so a real param.json (they reach
 * ~30 KiB with every language's title in them) read as "no version fields"
 * and was silently left alone. */
static inline int sdk_param_rewrite_json(const char *in, size_t in_len,
                                         uint32_t target_sdk, char *out,
                                         size_t out_cap, size_t *out_len) {
    if (!in || !out || out_cap == 0) return -1;

    char value[32];
    if (sdk_param_format_version(target_sdk, value, sizeof(value)) < 0) return -1;

    static const char *const fields[] = {"sdkVersion",
                                         "requiredSystemSoftwareVersion"};
    if (in_len >= out_cap) return -1;
    memcpy(out, in, in_len);
    out[in_len] = '\0';
    size_t cur_len = in_len;

    int changed = 0;
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); i++) {
        int r = sdk_param_replace_in_place(out, &cur_len, out_cap, fields[i],
                                           value);
        if (r < 0) return -1;
        changed += r;
    }

    if (out_len) *out_len = cur_len;
    return changed;
}

#endif
