#ifndef PS5UPLOAD_INSTALL_GUARD_H
#define PS5UPLOAD_INSTALL_GUARD_H

/* Whether a title id is installed by the console's own installer, on any storage (#884).
 *
 * A folder register stages /user/app/<id>/sce_sys on the INTERNAL drive and then asks Sony's
 * installer to install what is staged there (sceAppInstUtilAppInstallTitleDir, or on FW 13.x
 * sceAppInstUtilAppInstallAll). For a title the console already has installed, from a package,
 * on M.2 or extended storage, that rewrites the title's install record: the console then lists
 * it under Console Storage, looks for its package on the internal drive and cannot start it
 * (GitHub #884: Among Us on M.2, dumped to USB, then Play on the dump). So a register, a
 * re-bind, or an unregister of "our registration" must leave such a title alone, and must check
 * BEFORE anything is written.
 *
 * What counts as installed by the console:
 *   - <volume>/user/app/<id>/app.pkg on the internal drive ("" volume, /user/app) or on any
 *     extended volume (/mnt/ext0 USB extended storage, /mnt/ext1 the M.2 SSD, and any other
 *     /mnt/ext<N> the mount table shows). Our registrations and ShadowMount+'s never write an
 *     app.pkg.
 *   - app.db/appinfo.db naming a package location for the title (`_org_path` on an extended
 *     volume, or an app.pkg path) while it has no tracker of ours: an install on extended
 *     storage that is unplugged right now. Read only when the caller supplies it.
 *
 * Header-only and libc-only so the host self-test can include it. Read-only: stat() only. */

#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

typedef enum {
    INSTALL_GUARD_NONE = 0,
    INSTALL_GUARD_INTERNAL = 1, /* /user/app/<id>/app.pkg */
    INSTALL_GUARD_M2 = 2,       /* /mnt/ext1/user/app/<id>/app.pkg */
    INSTALL_GUARD_USB = 3,      /* /mnt/ext0/user/app/<id>/app.pkg (USB extended storage) */
    INSTALL_GUARD_EXTENDED = 4, /* another /mnt/ext<N> */
    INSTALL_GUARD_RECORD = 5,   /* only the console's records name a package location (its
                                 * storage may be unplugged) */
} install_guard_where_t;

typedef struct {
    install_guard_where_t where;
    char path[512]; /* the app.pkg found, or the recorded location */
} install_guard_hit_t;

/* The volumes always looked at, besides the mount table's: internal, USB extended, M.2. */
static const char *const INSTALL_GUARD_FIXED_VOLUMES[] = {"", "/mnt/ext0", "/mnt/ext1"};

/* A title id we may build paths from: 4 letters + 5 digits. */
static inline int install_guard_valid_id(const char *id) {
    if (!id || strlen(id) != 9) return 0;
    for (int i = 0; i < 4; i++)
        if (!((id[i] >= 'A' && id[i] <= 'Z') || (id[i] >= 'a' && id[i] <= 'z'))) return 0;
    for (int i = 4; i < 9; i++)
        if (id[i] < '0' || id[i] > '9') return 0;
    return 1;
}

/* An extended-storage mount root: exactly /mnt/ext<digits>. */
static inline int install_guard_is_ext_volume(const char *mnt) {
    if (!mnt || strncmp(mnt, "/mnt/ext", 8) != 0) return 0;
    const char *d = mnt + 8;
    if (!*d) return 0;
    for (; *d; d++)
        if (*d < '0' || *d > '9') return 0;
    return 1;
}

static inline install_guard_where_t install_guard_where_of(const char *volume) {
    if (!volume || !volume[0]) return INSTALL_GUARD_INTERNAL;
    if (strcmp(volume, "/mnt/ext1") == 0) return INSTALL_GUARD_M2;
    if (strcmp(volume, "/mnt/ext0") == 0) return INSTALL_GUARD_USB;
    return INSTALL_GUARD_EXTENDED;
}

/* The word the helper's refusals carry: register_title_installed:<word>. */
static inline const char *install_guard_word(install_guard_where_t w) {
    switch (w) {
    case INSTALL_GUARD_INTERNAL: return "internal";
    case INSTALL_GUARD_M2: return "m2";
    case INSTALL_GUARD_USB: return "usb";
    case INSTALL_GUARD_EXTENDED: return "extended";
    case INSTALL_GUARD_RECORD: return "record";
    default: return "none";
    }
}

/* Static reasons (thread-safe to hand back as err_reason_out). */
static inline const char *install_guard_register_reason(install_guard_where_t w) {
    switch (w) {
    case INSTALL_GUARD_INTERNAL: return "register_title_installed:internal";
    case INSTALL_GUARD_M2: return "register_title_installed:m2";
    case INSTALL_GUARD_USB: return "register_title_installed:usb";
    case INSTALL_GUARD_EXTENDED: return "register_title_installed:extended";
    case INSTALL_GUARD_RECORD: return "register_title_installed:record";
    default: return "register_title_installed";
    }
}

static inline const char *install_guard_unregister_reason(install_guard_where_t w) {
    switch (w) {
    case INSTALL_GUARD_INTERNAL: return "unregister_title_installed:internal";
    case INSTALL_GUARD_M2: return "unregister_title_installed:m2";
    case INSTALL_GUARD_USB: return "unregister_title_installed:usb";
    case INSTALL_GUARD_EXTENDED: return "unregister_title_installed:extended";
    case INSTALL_GUARD_RECORD: return "unregister_title_installed:record";
    default: return "unregister_title_installed";
    }
}

/* Look for <prefix><volume>/user/app/<id>/app.pkg on the fixed volumes, then on `extra`
 * (mount points from the mount table; anything not /mnt/ext<N> is skipped). `prefix` is "" on
 * the console and a scratch folder in the host test. First hit wins; 1 found, 0 not. */
static inline int install_guard_find_pkg(const char *prefix, const char *title_id,
                                         const char *const *extra, int nextra,
                                         install_guard_hit_t *out) {
    char p[640];
    struct stat st;
    if (out) {
        out->where = INSTALL_GUARD_NONE;
        out->path[0] = '\0';
    }
    if (!install_guard_valid_id(title_id)) return 0;
    if (!prefix) prefix = "";
    int nfixed = (int)(sizeof INSTALL_GUARD_FIXED_VOLUMES / sizeof INSTALL_GUARD_FIXED_VOLUMES[0]);
    for (int i = 0; i < nfixed + (extra ? nextra : 0); i++) {
        const char *vol = i < nfixed ? INSTALL_GUARD_FIXED_VOLUMES[i] : extra[i - nfixed];
        if (i >= nfixed) {
            if (!install_guard_is_ext_volume(vol)) continue;
            int dup = 0;
            for (int k = 1; k < nfixed; k++)
                if (strcmp(vol, INSTALL_GUARD_FIXED_VOLUMES[k]) == 0) dup = 1;
            if (dup) continue;
        }
        int n = snprintf(p, sizeof p, "%s%s/user/app/%s/app.pkg", prefix, vol, title_id);
        if (n <= 0 || (size_t)n >= sizeof p) continue;
        /* Any kind of entry: a package is a file, but nothing else of that name belongs there. */
        if (stat(p, &st) != 0) continue;
        if (out) {
            out->where = install_guard_where_of(vol);
            snprintf(out->path, sizeof out->path, "%s/user/app/%s/app.pkg", vol, title_id);
        }
        return 1;
    }
    return 0;
}

/* Whether a recorded install location (appinfo.db `_org_path`) is a package install: on an
 * extended volume, or naming an app.pkg. Our registrations and ShadowMount+'s stage on the
 * internal /user/app and never have a package. */
static inline int install_guard_record_is_package(const char *org_path) {
    if (!org_path || !org_path[0]) return 0;
    if (strncmp(org_path, "/mnt/ext", 8) == 0) return 1;
    size_t n = strlen(org_path);
    return n >= 8 && strcmp(org_path + n - 8, "/app.pkg") == 0;
}

/* Whether /user/app/<id> holds a tracker of a folder registration (ours: mount.lnk, or the
 * helper tile's ps5upload.lnk; ShadowMount+'s mount.lnk). */
static inline int install_guard_has_tracker(const char *prefix, const char *title_id) {
    static const char *const names[] = {"mount.lnk", "ps5upload.lnk"};
    char p[640];
    struct stat st;
    if (!install_guard_valid_id(title_id)) return 0;
    if (!prefix) prefix = "";
    for (size_t i = 0; i < sizeof names / sizeof names[0]; i++) {
        int n = snprintf(p, sizeof p, "%s/user/app/%s/%s", prefix, title_id, names[i]);
        if (n > 0 && (size_t)n < sizeof p && stat(p, &st) == 0) return 1;
    }
    return 0;
}

/* The whole check. `org_path` is appinfo.db's `_org_path` for the title, or NULL when it could
 * not be read (then only the packages on mounted storage count). 1 = installed by the console
 * (`out` says where), 0 = not. */
static inline int install_guard_check(const char *prefix, const char *title_id,
                                      const char *const *extra, int nextra, const char *org_path,
                                      install_guard_hit_t *out) {
    install_guard_hit_t local;
    if (!out) out = &local;
    if (install_guard_find_pkg(prefix, title_id, extra, nextra, out)) return 1;
    if (install_guard_valid_id(title_id) && install_guard_record_is_package(org_path) &&
        !install_guard_has_tracker(prefix, title_id)) {
        out->where = INSTALL_GUARD_RECORD;
        snprintf(out->path, sizeof out->path, "%s", org_path);
        return 1;
    }
    return 0;
}

#endif
