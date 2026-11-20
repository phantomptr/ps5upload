#ifndef PS5UPLOAD_UNREGISTER_OUTCOME_H
#define PS5UPLOAD_UNREGISTER_OUTCOME_H

/* What an uninstall request actually achieved, and how the helper reports it (#476).
 *
 * sceAppInstUtilAppUnInstall's return code alone cannot confirm a removal: on FW 13.60 it
 * returns 0 for a title id that was never installed (measured on the Phat, 2026-10-18), and a
 * refusal used to be reported as success anyway, with an "Unregistered" toast on the console
 * while the game stayed. So after the call the helper asks the question the PC app's installed
 * list answers, and only calls the uninstall done when the title would no longer be listed.
 *
 * Header-only and libc-only so the host self-test can include it. */

#include <dirent.h>
#include <inttypes.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef struct {
    int ours;          /* our nullfs registration (a mount.lnk tracker) was torn down */
    int api_called;    /* sceAppInstUtilAppUnInstall ran */
    uint32_t sony_rc;  /* its first return code (0 when it did not run) */
    int still_present; /* the PC app would still list the title, or the home screen still
                        * draws it, after the wait; also set when that could not be checked */
    int unverified;    /* app.db could not be read: whether the home screen still draws the
                        * title is unknown (still_present is set too, for older engines) */
    int smp;           /* ShadowMount+ registered the title from a disk image: its bind and
                        * links were left to it, and it registers the title again on its next
                        * scan while the image is in one of its scan folders (#879) */
    char smp_image[512]; /* that image (its mount_img.lnk), when it named one */
} unregister_outcome_t;

/* Who wrote /user/app/<id>/mount.lnk (#879). `lnk` is its text and `img` mount_img.lnk's; NULL
 * when that file is absent.
 *   - ShadowMount+ (SMP) writes mount.lnk for every title it registers. For a title in a disk
 *     image it points under its image mount root /mnt/shadowmnt, and mount_img.lnk names the
 *     image file itself.
 *   - register_title_from_path writes mount_img.lnk only for a source under /mnt/ps5upload/,
 *     with the same text as mount.lnk.
 * So a mount.lnk under /mnt/shadowmnt, or a mount_img.lnk that differs from it, is SMP's. Before
 * #879 any mount.lnk counted as ours: an uninstall unmounted SMP's bind and deleted its links
 * behind its back, and SMP put the title back on its next scan. A folder SMP registered looks
 * exactly like ours and is still treated as ours, as before. */
typedef enum {
    UNREGISTER_TRACKER_NONE = 0,
    UNREGISTER_TRACKER_OURS = 1,
    UNREGISTER_TRACKER_SMP = 2,
} unregister_tracker_t;

#define UNREGISTER_SMP_MOUNT_ROOT "/mnt/shadowmnt"

static inline unregister_tracker_t unregister_tracker_owner(const char *lnk, const char *img) {
    if (!lnk) return UNREGISTER_TRACKER_NONE;
    const size_t root = sizeof UNREGISTER_SMP_MOUNT_ROOT - 1;
    if (strncmp(lnk, UNREGISTER_SMP_MOUNT_ROOT, root) == 0 &&
        (lnk[root] == '/' || lnk[root] == '\0'))
        return UNREGISTER_TRACKER_SMP;
    if (img && img[0] && strcmp(img, lnk) != 0) return UNREGISTER_TRACKER_SMP;
    return UNREGISTER_TRACKER_OURS;
}

/* Whether unregister may unmount what statfs(dst) reports, dst being /system_ex/app/<id> of the
 * title being removed: only a nullfs mounted at exactly that path (a bind). Nothing mounted there
 * (statfs then names the parent filesystem) or anything else mounted there (Sony's own mount of
 * an installed copy) is left alone. */
static inline int unregister_bind_unmountable(const char *dst, const char *mnt_on,
                                              const char *fstype) {
    return dst && mnt_on && fstype && dst[0] == '/' && strcmp(mnt_on, dst) == 0 &&
           strcmp(fstype, "nullfs") == 0;
}

typedef enum {
    UNREGISTER_DONE = 0,          /* gone */
    UNREGISTER_STILL_PRESENT = 1, /* Sony accepted, the title is still listed (may be removing) */
    UNREGISTER_REFUSED = 2,       /* Sony returned an error and the title is (or may be) there */
    UNREGISTER_UNAVAILABLE = 3,   /* no uninstall API, and the title is still there */
    UNREGISTER_UNVERIFIED = 4,    /* Sony accepted and nothing is listed, but app.db (what the
                                   * home screen draws) could not be read */
} unregister_verdict_t;

/* The helper tile also has a Sony home/library icon. Removing our appmeta alone does not
 * prove Sony removed that icon. Preserve the staged files when the API refused or is absent. */
static inline int unregister_tile_confirmed(const unregister_outcome_t *o) {
    return o->api_called && o->sony_rc == 0 && !o->still_present;
}

/* A refusal of a package uninstall is final. For our own folder registration the teardown
 * that matters already happened (nullfs, trackers, appmeta), so a Sony refusal only counts
 * when the title is still listed: Sony refuses some titles it barely knows, and reporting
 * those as failures would keep an unregistered folder game on screen as "refused". */
static inline unregister_verdict_t unregister_verdict(const unregister_outcome_t *o) {
    if (o->api_called && o->sony_rc != 0 && (!o->ours || o->still_present)) {
        return UNREGISTER_REFUSED;
    }
    if (!o->still_present) return UNREGISTER_DONE;
    if (!o->api_called) return UNREGISTER_UNAVAILABLE;
    return o->unverified ? UNREGISTER_UNVERIFIED : UNREGISTER_STILL_PRESENT;
}

/* What the console has for one title, in the terms of the engine's installed list
 * (ps5_apps_installed in engine/crates/ps5upload-engine/src/lib.rs):
 *   - every /user/appmeta/<id> folder is listed;
 *   - an app.db row is listed when its app folder exists under /user/app, /mnt/ext0/user/app
 *     or /mnt/ext1/user/app (UNREGISTER_APP_ROOTS below, the engine's roots exactly).
 * When app.db could not be read the engine lists only the appmeta folders, and so does this. */
typedef struct {
    int appmeta_dir; /* /user/appmeta/<id> exists */
    int app_dir;     /* <root>/<id> exists under one of the app roots */
    int appdb_known; /* app.db answered */
    int appdb_row;   /* app.db has a row for the title */
    int appdb_icon_visible; /* app.db has a visible icon row: a home-screen tile */
} unregister_presence_t;

static const char *const UNREGISTER_APP_ROOTS[] = {
    "/user/app",
    "/mnt/ext0/user/app",
    "/mnt/ext1/user/app",
};
#define UNREGISTER_APPMETA_ROOT "/user/appmeta"

static inline int unregister_still_listed(const unregister_presence_t *p) {
    if (p->appmeta_dir) return 1;
    return p->app_dir && p->appdb_known && p->appdb_row;
}

/* Whether an uninstall left the title behind: listed by the PC app, or still a tile on the home
 * screen. The home screen draws app.db's visible icon rows whatever folders are left, so a row
 * without an app folder (folders deleted by hand, #879) is still a game the user sees. */
static inline int unregister_still_present(const unregister_presence_t *p) {
    return unregister_still_listed(p) || (p->appdb_known && p->appdb_icon_visible);
}

/* Nothing is listed, but app.db did not answer: the home screen could not be checked. */
static inline int unregister_presence_unverified(const unregister_presence_t *p) {
    return !unregister_still_present(p) && !p->appdb_known;
}

/* Remove what is under `dir`, then `dir`, at most `depth` folders deep. Uses lstat and never
 * follows a link: a symlink is unlinked itself, a symlinked folder is not entered. Anything
 * that is neither a file, a link nor a folder is left, and so are the folders holding it.
 * Unlink and rmdir only: nothing is renamed, so nothing can cross a mount. */
static inline void unregister_remove_tree(const char *dir, int depth) {
    DIR *d = opendir(dir);
    if (!d) return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        char f[1024];
        int n = snprintf(f, sizeof(f), "%s/%s", dir, e->d_name);
        if (n <= 0 || (size_t)n >= sizeof(f)) continue;
        struct stat st;
        if (lstat(f, &st) != 0) continue;
        if (S_ISREG(st.st_mode) || S_ISLNK(st.st_mode)) {
            (void)unlink(f);
        } else if (S_ISDIR(st.st_mode) && depth > 0) {
            unregister_remove_tree(f, depth - 1);
        }
    }
    closedir(d);
    (void)rmdir(dir);
}

/* The reply for `o`: 1 = ACK body, 0 = ERROR reason, -1 = did not fit. `*written` is its
 * length. An ACK keeps `sony_uninstall_rc` for older engines, which read only that field. */
static inline void unregister_json_escape(const char *s, char *out, size_t cap) {
    size_t j = 0;
    if (cap == 0) return;
    for (; s && *s; s++) {
        unsigned char c = (unsigned char)*s;
        char tmp[8];
        int k;
        if (c == '"' || c == '\\') k = snprintf(tmp, sizeof tmp, "\\%c", c);
        else if (c < 0x20) k = snprintf(tmp, sizeof tmp, "\\u%04x", c);
        else k = snprintf(tmp, sizeof tmp, "%c", c);
        if (k <= 0 || j + (size_t)k >= cap) {
            out[0] = '\0'; /* a cut path would name another file: none at all */
            return;
        }
        memcpy(out + j, tmp, (size_t)k);
        j += (size_t)k;
    }
    out[j] = '\0';
}

/* An ACK's fields after still_present: "unverified", and ShadowMount+'s "managed_by" + "image".
 * Older engines ignore them. */
static inline int unregister_reply_extra(const unregister_outcome_t *o, char *out, size_t cap) {
    char img[1100];
    unregister_json_escape(o->smp ? o->smp_image : "", img, sizeof img);
    int n = snprintf(out, cap, "%s%s%s%s", o->unverified ? ",\"unverified\":true" : "",
                     o->smp ? ",\"managed_by\":\"shadowmount\",\"image\":\"" : "",
                     o->smp ? img : "", o->smp ? "\"" : "");
    return (n < 0 || (size_t)n >= cap) ? -1 : 0;
}

static inline int unregister_reply(const unregister_outcome_t *o, char *body, size_t cap,
                                   size_t *written) {
    int n;
    int ack = 1;
    char extra[1200];
    *written = 0;
    if (unregister_reply_extra(o, extra, sizeof extra) != 0) return -1;
    switch (unregister_verdict(o)) {
    case UNREGISTER_DONE:
        /* Sony's code rides along: for our folder registration it can be a refusal the
         * teardown made moot, which the engine logs. */
        n = snprintf(body, cap, "{\"sony_uninstall_rc\":%" PRIu32 ",\"still_present\":false%s}",
                     o->api_called ? o->sony_rc : 0u, extra);
        break;
    case UNREGISTER_STILL_PRESENT:
    case UNREGISTER_UNVERIFIED:
        /* An older engine reads still_present only: an unchecked result never reads as gone. */
        n = snprintf(body, cap, "{\"sony_uninstall_rc\":0,\"still_present\":true%s}", extra);
        break;
    case UNREGISTER_REFUSED:
        n = snprintf(body, cap, "uninstall_refused:0x%08" PRIX32, o->sony_rc);
        ack = 0;
        break;
    default:
        n = snprintf(body, cap, "uninstall_unavailable");
        ack = 0;
        break;
    }
    if (n < 0 || (size_t)n >= cap) return -1;
    *written = (size_t)n;
    return ack;
}

/* The console toast for `o`. A pkg install is "uninstalled"; our folder registration is
 * "unregistered" (its source files stay). */
static inline void unregister_toast(const unregister_outcome_t *o, const char *title_id,
                                    char *out, size_t cap) {
    switch (unregister_verdict(o)) {
    case UNREGISTER_DONE:
        if (o->smp)
            snprintf(out, cap, "Removed %s: ShadowMount+ adds it back while its image is there",
                     title_id);
        else
            snprintf(out, cap, "%s %s", o->ours ? "Unregistered" : "Uninstalled", title_id);
        break;
    case UNREGISTER_STILL_PRESENT:
        snprintf(out, cap, "Removing %s: still on the console", title_id);
        break;
    case UNREGISTER_UNVERIFIED:
        snprintf(out, cap, "Removing %s: the home screen could not be checked", title_id);
        break;
    case UNREGISTER_REFUSED:
        snprintf(out, cap, "PS5 refused to uninstall %s (0x%08" PRIX32 ")", title_id, o->sony_rc);
        break;
    default:
        snprintf(out, cap, "Could not uninstall %s", title_id);
        break;
    }
}

#endif
