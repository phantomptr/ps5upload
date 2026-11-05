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

static inline const char *user_delete_refusal(int uid, int rc_fg, int foreground, int rc_list, const int *ids,
                                              size_t n_ids) {
    if (rc_fg == 0 && uid == foreground) return "user_signed_in";
    if (rc_list != 0) return "user_state_unknown";
    for (size_t i = 0; i < n_ids; i++)
        if (ids[i] == uid) return "user_signed_in";
    return NULL;
}

#endif /* PS5UPLOAD_USER_GUARD_H */
