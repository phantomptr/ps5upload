/* Host test for image_mount.h: which mounts fs.freespace treats as an attached disk image (no
 * working margin) and which nullfs views it follows to their lower mount. */
#include <stdio.h>

#include "image_mount.h"

static int fails;
#define CHECK(cond)                                                 \
    do {                                                            \
        if (!(cond)) {                                              \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            fails++;                                                \
        }                                                           \
    } while (0)

int main(void) {
    /* images */
    CHECK(image_mount_kind("exfatfs", "/dev/lvd2") == IMAGE_MOUNT_YES);
    CHECK(image_mount_kind("ufs", "/dev/md3") == IMAGE_MOUNT_YES);
    CHECK(image_mount_kind("nullfs", "/mnt/shadowmnt/G_ca51a0d7") == IMAGE_MOUNT_YES);
    CHECK(image_mount_kind("nullfs", "/mnt/ps5upload/G") == IMAGE_MOUNT_YES);
    /* drives */
    CHECK(image_mount_kind("ufs", "/dev/ssd0.user") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("ufs", "/dev/nvme1") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("exfatfs", "/dev/da1p1") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("tmpfs", "tmpfs") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("ufs", "") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("ufs", NULL) == IMAGE_MOUNT_NO);
    /* a title link to an image mounted at a chosen path, or /data's own nullfs: followed */
    CHECK(image_mount_kind("nullfs", "/data/homebrew/G") == IMAGE_MOUNT_FOLLOW);
    CHECK(image_mount_kind("nullfs", "/user/data") == IMAGE_MOUNT_FOLLOW);
    /* a path-shaped source on another fs is not a view to follow */
    CHECK(image_mount_kind("ufs", "/data/homebrew/G") == IMAGE_MOUNT_NO);
    CHECK(image_mount_kind("nullfs", "/dev/ssd0.user") == IMAGE_MOUNT_NO);
    if (fails) return 1;
    printf("image_mount selftest: ok\n");
    return 0;
}
