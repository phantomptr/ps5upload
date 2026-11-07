/* Host test for image_mount.h: which mounts fs.freespace treats as an attached disk image (no
 * working margin) and which nullfs views it follows to their lower mount. */
#include <stdio.h>
#include <string.h>

#include "image_mount.h"

static int fails;
#define CHECK(cond)                                                 \
    do {                                                            \
        if (!(cond)) {                                              \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            fails++;                                                \
        }                                                           \
    } while (0)

/* A fake mount table: the mount holding a path is the longest listed prefix. */
struct fake_mount {
    const char *on, *fstype, *from;
};
static const struct fake_mount *fake_table;
static int looks;
static char first_look[512];

static int fake_look(const char *path, char *fstype, size_t fstype_n, char *from, size_t from_n,
                     void *ctx) {
    const struct fake_mount *best = NULL;
    (void)ctx;
    if (looks++ == 0) snprintf(first_look, sizeof first_look, "%s", path);
    for (const struct fake_mount *m = fake_table; m->on; m++) {
        size_t n = strlen(m->on);
        if (strncmp(path, m->on, n) == 0 && (path[n] == '/' || path[n] == '\0') &&
            (!best || n > strlen(best->on)))
            best = m;
    }
    if (!best) return -1;
    snprintf(fstype, fstype_n, "%s", best->fstype);
    snprintf(from, from_n, "%s", best->from);
    return 0;
}

static int walk(const struct fake_mount *table, const char *path) {
    fake_table = table;
    looks = 0;
    first_look[0] = '\0';
    return image_mount_walk(path, fake_look, NULL);
}

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

    /* the walk: a title link followed to its image, wherever that is mounted */
    static const struct fake_mount table[] = {
        {"/", "ufs", "/dev/ssd0.system"},
        {"/data", "ufs", "/dev/ssd0.user"},
        {"/data/homebrew/G", "exfatfs", "/dev/lvd2"},
        {"/system_ex/app/PPSA00001", "nullfs", "/data/homebrew/G"},
        {"/loop/a", "nullfs", "/loop/b"},
        {"/loop/b", "nullfs", "/loop/a"},
        {"/self", "nullfs", "/self"},
        {NULL, NULL, NULL},
    };
    CHECK(walk(table, "/system_ex/app/PPSA00001/eboot.bin") == 1);
    CHECK(walk(table, "/data/games/G") == 0);
    CHECK(walk(table, "/data/homebrew/G/sce_sys") == 1);
    /* a request path longer than a mount name (88 bytes) is asked about whole */
    char longp[400];
    snprintf(longp, sizeof longp, "/system_ex/app/PPSA00001/%s/%s/%s",
             "a_directory_name_that_is_long_enough_to_matter_here",
             "another_directory_name_that_is_long_enough_to_matter",
             "and_a_file_name_at_the_end_of_it_all.bin");
    CHECK(strlen(longp) > 88);
    CHECK(walk(table, longp) == 1);
    CHECK(strcmp(first_look, longp) == 0);
    /* cycles and self-views end: no more than the hop cap of follows */
    CHECK(walk(table, "/loop/a/x") == 0);
    CHECK(looks == IMAGE_MOUNT_MAX_HOPS + 1);
    CHECK(walk(table, "/self/x") == 0);
    CHECK(looks == 2);
    /* a path no mount holds (statfs failed) */
    CHECK(walk(table, "relative") == 0);
    CHECK(image_mount_walk(NULL, fake_look, NULL) == 0);

    if (fails) return 1;
    printf("image_mount selftest: ok\n");
    return 0;
}
