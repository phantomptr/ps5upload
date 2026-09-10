#ifndef PS5UPLOAD_FAKELIB_OVERLAY_PATHS_H
#define PS5UPLOAD_FAKELIB_OVERLAY_PATHS_H

#include <stddef.h>
#include <string.h>

static inline int fakelib_overlay_title_from_sandbox(const char *name,
                                                      char *out,
                                                      size_t out_cap) {
    if (!name || !out || out_cap < 10 || strlen(name) != 13) return 0;
    if (memcmp(name, "PPSA", 4) != 0 && memcmp(name, "CUSA", 4) != 0) return 0;
    for (int i = 4; i < 9; i++) if (name[i] < '0' || name[i] > '9') return 0;
    if (name[9] != '_') return 0;
    for (int i = 10; i < 13; i++) if (name[i] < '0' || name[i] > '9') return 0;
    memcpy(out, name, 9);
    out[9] = '\0';
    return 1;
}

static inline int fakelib_overlay_run_dir_ok(const char *name) {
    if (!name || !name[0] || strcmp(name, "app0") == 0 ||
        strcmp(name, ".") == 0 || strcmp(name, "..") == 0) return 0;
    return strchr(name, '/') == NULL && strstr(name, "..") == NULL;
}

static inline int fakelib_overlay_target_matches(const char *mounted_on,
                                                  const char *fstype,
                                                  const char *target) {
    return mounted_on && fstype && target &&
           strcmp(fstype, "unionfs") == 0 && strcmp(mounted_on, target) == 0;
}

#endif
