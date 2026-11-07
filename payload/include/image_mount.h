/* Whether a mount is an attached disk image, from what statfs says about it (fs.freespace's
 * is_image_mount policy). Pure, so the host self-test covers it. */
#ifndef PS5UPLOAD_IMAGE_MOUNT_H
#define PS5UPLOAD_IMAGE_MOUNT_H

#include <string.h>

/* A nullfs is followed through at most this many lower mounts. */
#define IMAGE_MOUNT_MAX_HOPS 4

enum {
    IMAGE_MOUNT_NO = 0,
    IMAGE_MOUNT_YES = 1,
    /* A nullfs view of another path: ask about the mount holding `from` instead. */
    IMAGE_MOUNT_FOLLOW = 2,
};

/* `fstype` and `from` are statfs's f_fstypename and f_mntfromname. An image is an LVD or MD device,
 * or a mount from an image mount root (ShadowMount+'s, our own). A nullfs from any other path (a
 * title's /system_ex/app link to an image mounted at /data/homebrew/<game>) is followed: taken as a
 * drive, it kept a drive's working margin, which on an image with 200 MiB spare left 0 bytes, and
 * every edit to the game was refused ("only 0 bytes are safely allocatable"). */
static inline int image_mount_kind(const char *fstype, const char *from) {
    if (!from || !from[0]) return IMAGE_MOUNT_NO;
    if (strncmp(from, "/dev/lvd", 8) == 0 || strncmp(from, "/dev/md", 7) == 0) return IMAGE_MOUNT_YES;
    if (strncmp(from, "/mnt/shadowmnt/", 15) == 0 || strncmp(from, "/mnt/ps5upload/", 15) == 0)
        return IMAGE_MOUNT_YES;
    if (fstype && strcmp(fstype, "nullfs") == 0 && from[0] == '/' && strncmp(from, "/dev/", 5) != 0)
        return IMAGE_MOUNT_FOLLOW;
    return IMAGE_MOUNT_NO;
}

#endif
