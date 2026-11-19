#ifndef PS5UPLOAD_USER_GUARD_H
#define PS5UPLOAD_USER_GUARD_H

/* Whether a console user may be deleted right now. Sony's own UI refuses a user who is signed in, and
 * the delete can also wipe that user's saves, so this refuses first rather than find out what
 * sceUserServiceDestroyUser does to a live user.
 *
 * rc_fg / foreground: sceUserServiceGetForegroundUser (it fails when nobody is in the foreground, which
 * says nothing about the others). rc_list / ids: sceUserServiceGetLoginUserIdList, unused slots -1. If
 * the list cannot be read there is no telling, and a delete cannot be undone: no. */
#include <stddef.h>
#include <stdint.h>

static inline const char *user_delete_refusal(int uid, int rc_fg, int foreground, int rc_list, const int *ids,
                                              size_t n_ids) {
    if (rc_fg == 0 && uid == foreground) return "user_signed_in";
    if (rc_list != 0) return "user_state_unknown";
    for (size_t i = 0; i < n_ids; i++)
        if (ids[i] == uid) return "user_signed_in";
    return NULL;
}

/* A user id as JSON carries it, read without loss: the id unsigned (0..4294967295), or the same 32
 * bits signed (-2147483648..-1) as an engine that holds ids in an i32 sends and reads them. 1 with the
 * id in *out; 0 when `p` does not start with such a whole number. A number that does not fit is
 * refused, never wrapped or masked onto another user's id. */
static inline int user_id_parse(const char *p, uint32_t *out) {
    if (!p || !out) return 0;
    while (*p == ' ' || *p == '\t') p++;
    int neg = 0;
    if (*p == '-') {
        neg = 1;
        p++;
    }
    if (*p < '0' || *p > '9') return 0;
    uint64_t v = 0;
    for (; *p >= '0' && *p <= '9'; p++) {
        v = v * 10u + (uint64_t)(*p - '0');
        if (v > 0xFFFFFFFFull) return 0;
    }
    if (*p == '.' || *p == 'e' || *p == 'E') return 0; /* not a whole number */
    if (neg) {
        if (v > 0x80000000ull) return 0;
        *out = (uint32_t)(0x100000000ull - v); /* -0 is 0 */
    } else {
        *out = (uint32_t)v;
    }
    return 1;
}

/* The id as the helper writes it where an older engine reads an i32 (saves list, user.delete reply):
 * the same 32 bits, signed. user_id_parse reads it back to the same id. */
static inline long long user_id_as_i32(uint32_t id) {
    return id >= 0x80000000u ? (long long)id - 0x100000000ll : (long long)id;
}

#endif /* PS5UPLOAD_USER_GUARD_H */
