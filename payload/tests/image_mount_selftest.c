/* Host test for image_mount.h: which mounts fs.freespace treats as an attached disk image (no
 * working margin) and which nullfs views it follows to their lower mount. */
#include <stdio.h>
#include <string.h>

#include "image_mount.h"
#include "statvfs_avail.h"

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
    /* fs.unmount's later passes: only another image unit may be unmounted after the first layer.
     * What an image was mounted OVER (a drive, a nullfs, a system folder, a sandbox's own mount) is
     * not: those passes used to unmount it too, with MNT_FORCE. */
    CHECK(image_mount_from_is_unit("/dev/lvd0") && image_mount_from_is_unit("/dev/lvd12"));
    CHECK(image_mount_from_is_unit("/dev/md3"));
    CHECK(!image_mount_from_is_unit("/dev/da1p1"));       /* a USB drive */
    CHECK(!image_mount_from_is_unit("/dev/ssd0.user"));   /* the internal drive */
    CHECK(!image_mount_from_is_unit("/dev/nvme1"));       /* the M.2 drive */
    CHECK(!image_mount_from_is_unit("/mnt/ps5upload/G")); /* a nullfs view, even of an image */
    CHECK(!image_mount_from_is_unit("/system_ex/common_ex/lib"));
    CHECK(!image_mount_from_is_unit("tmpfs") && !image_mount_from_is_unit("") && !image_mount_from_is_unit(NULL));
    CHECK(!image_mount_from_is_unit("/dev/lvd") && !image_mount_from_is_unit("/dev/md"));
    CHECK(!image_mount_from_is_unit("/dev/lvdx") && !image_mount_from_is_unit("/dev/mdctl"));

    /* fs.fsck repair: a device with anything mounted from it (itself or a partition) is in use. */
    CHECK(image_mount_from_is_on_device("/dev/da1", "/dev/da1"));
    CHECK(image_mount_from_is_on_device("/dev/da1", "/dev/da1p1"));
    CHECK(image_mount_from_is_on_device("/dev/da1", "/dev/da1s1"));
    CHECK(image_mount_from_is_on_device("/dev/lvd3", "/dev/lvd3"));
    CHECK(!image_mount_from_is_on_device("/dev/da1", "/dev/da12")); /* another drive */
    CHECK(!image_mount_from_is_on_device("/dev/lvd3", "/dev/lvd31"));
    CHECK(!image_mount_from_is_on_device("/dev/md0", "/dev/lvd0"));
    CHECK(!image_mount_from_is_on_device("/dev/da1p1", "/dev/da1"));
    CHECK(!image_mount_from_is_on_device("/dev/da1", "tmpfs") && !image_mount_from_is_on_device("/dev/da1", ""));
    CHECK(!image_mount_from_is_on_device("", "/dev/da1") && !image_mount_from_is_on_device(NULL, "/dev/da1"));
    CHECK(!image_mount_from_is_on_device("/dev/da1", NULL));

    /* Free space from statvfs: the kernel's count is signed and goes below zero on a drive written
     * into its reserve; read as unsigned that is about 16 EiB free. Zero or below is no room. */
    CHECK(statvfs_avail_blocks_to_bytes(10, 4096) == 40960);
    CHECK(statvfs_avail_blocks_to_bytes(0, 4096) == 0);
    CHECK(statvfs_avail_blocks_to_bytes((uint64_t)-1, 4096) == 0);
    CHECK(statvfs_avail_blocks_to_bytes((uint64_t)-70000000, 32768) == 0);
    {
        struct statvfs v;
        memset(&v, 0, sizeof v);
        v.f_frsize = 512;
        v.f_bsize = 4096;
        v.f_bavail = 3;
        CHECK(statvfs_avail_bytes(&v) == 1536);
        v.f_frsize = 0; /* no fragment size: the block size counts */
        CHECK(statvfs_avail_bytes(&v) == 12288);
    }

    /* A tracker file is believed outside /mnt/ps5upload/ only for an image unit: "data.src" is
     * the tracker of /mnt/ps5upload/data AND of /data, and must never make /data ours. */
    CHECK(image_mount_tracker_trusted("/mnt/ps5upload/data", "/dev/lvd3"));
    CHECK(image_mount_tracker_trusted("/mnt/ps5upload/data", "anything"));
    CHECK(image_mount_tracker_trusted("/data/homebrew/PPSA17599", "/dev/lvd3"));
    CHECK(image_mount_tracker_trusted("/mnt/ext1/games/foo", "/dev/md0"));
    CHECK(!image_mount_tracker_trusted("/data", "/dev/ssd0.user"));
    CHECK(!image_mount_tracker_trusted("/user", "/dev/ssd0.user"));
    CHECK(!image_mount_tracker_trusted("/mnt/ext1", "/dev/nvme1"));
    CHECK(!image_mount_tracker_trusted("/mnt/usb0", "/dev/da1p1"));
    CHECK(!image_mount_tracker_trusted("/data", "/data"));
    CHECK(!image_mount_tracker_trusted("/mnt/ps5upload/", "/dev/ssd0.user"));
    CHECK(!image_mount_tracker_trusted("/mnt/ps5upload", "/dev/ssd0.user"));
    CHECK(!image_mount_tracker_trusted("/data", NULL) && !image_mount_tracker_trusted(NULL, "/dev/lvd1"));

    /* A tracker's file name. The forms earlier helpers wrote stay as they were. */
    {
        char key[IMAGE_MOUNT_KEY_MAX + 1], key2[IMAGE_MOUNT_KEY_MAX + 1], small[64] = "x";
        image_mount_tracker_key("/mnt/ps5upload/GTA5", key, sizeof key);
        CHECK(strcmp(key, "GTA5") == 0);
        image_mount_tracker_key("/mnt/ext1/games/foo", key, sizeof key);
        CHECK(strcmp(key, "mnt_2fext1_2fgames_2ffoo") == 0);
        image_mount_tracker_key("/mnt/ext1/foo_bar", key, sizeof key);
        image_mount_tracker_key("/mnt/ext1/foo/bar", key2, sizeof key2);
        /* escaped mount paths, not secrets */
        CHECK(strcmp(key, "mnt_2fext1_2ffoo_5fbar") == 0 && strcmp(key2, "mnt_2fext1_2ffoo_2fbar") == 0); /* gitleaks:allow */
        /* a path below /mnt/ps5upload/<name> is escaped, never "<name>/<more>" */
        image_mount_tracker_key("/mnt/ps5upload/a/b", key, sizeof key);
        CHECK(strcmp(key, "mnt_2fps5upload_2fa_2fb") == 0);
        image_mount_tracker_key("/mnt/ps5upload/", key, sizeof key);
        CHECK(strcmp(key, "mnt_2fps5upload_2f") == 0);
        image_mount_tracker_key(NULL, key, sizeof key);
        CHECK(key[0] == '\0');
        image_mount_tracker_key("/data/x", small, sizeof small); /* a buffer that could not hold every key */
        CHECK(small[0] == '\0');

        /* The longest key that is still the plain one: 251 bytes, "<key>.src" is 255. */
        char mp[1024];
        memset(mp, 'a', sizeof mp);
        mp[0] = '/';
        mp[1 + IMAGE_MOUNT_KEY_MAX] = '\0';
        image_mount_tracker_key(mp, key, sizeof key);
        CHECK(strlen(key) == IMAGE_MOUNT_KEY_MAX && strcmp(key, mp + 1) == 0);

        /* One byte more, and a folder of 28 Japanese letters under /data/homebrew (84 bytes, nine
         * key bytes for each letter): the key still names a file that can be made, holds only
         * file-name-safe bytes, and is the same every time. */
        mp[1 + IMAGE_MOUNT_KEY_MAX] = 'a';
        mp[2 + IMAGE_MOUNT_KEY_MAX] = '\0';
        image_mount_tracker_key(mp, key, sizeof key);
        CHECK(strlen(key) == IMAGE_MOUNT_KEY_HEAD + 34 && strstr(key, "_x") == key + IMAGE_MOUNT_KEY_HEAD);
        char cjk[512] = "/data/homebrew/";
        for (int i = 0; i < 28; i++) strcat(cjk, "\xe3\x82\xb2"); /* U+30B2 */
        image_mount_tracker_key(cjk, key, sizeof key);
        image_mount_tracker_key(cjk, key2, sizeof key2);
        CHECK(strcmp(key, key2) == 0);
        CHECK(strlen(key) + strlen(".src") <= 255 && strlen(key) > 34);
        CHECK(strspn(key, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._") == strlen(key));
        /* the kept part ends between two bytes of the path: "_HH" triplets, then "_x" + 32 hex digits */
        const char *x = strstr(key, "_x");
        CHECK(x != NULL && strlen(x) == 34 && strspn(x + 2, "0123456789abcdef") == 32);
        CHECK(x != NULL && strncmp(key, "data_2fhomebrew_2f_e3_82_b2", 27) == 0 && (x - key - 18) % 3 == 0);

        /* Two long mount points that differ only at the end (they shared one key) have two keys. */
        strcat(cjk, "/1");
        image_mount_tracker_key(cjk, key, sizeof key);
        cjk[strlen(cjk) - 1] = '2';
        image_mount_tracker_key(cjk, key2, sizeof key2);
        CHECK(strcmp(key, key2) != 0 && strncmp(key, key2, IMAGE_MOUNT_KEY_HEAD - 8) == 0);
    }

    /* A caller's mount point is compared with the kernel's name for it, so it is first written the
     * same way: one slash between components, no "." component. What cannot be one is refused. */
    {
        static const struct {
            const char *in, *out; /* out NULL: refused */
        } cases[] = {
            { "/data/games/foo", "/data/games/foo" },
            { "/data//games/foo", "/data/games/foo" },
            { "//data///games////foo", "/data/games/foo" },
            { "/data/./games/./foo", "/data/games/foo" },
            { "/./data/games/foo", "/data/games/foo" },
            { "/data/.hidden/..foo/a.", "/data/.hidden/..foo/a." },
            { "/mnt/usb0", "/mnt/usb0" },
            { "/data/games/foo/", NULL },
            { "/data/games/foo//", NULL },
            { "/data/games/foo/.", NULL },
            { "/data/games/../foo", NULL },
            { "/data/games/foo/..", NULL },
            { "/..", NULL },
            { "/", NULL },
            { "//", NULL },
            { "/.", NULL },
            { "", NULL },
            { "data/games", NULL },
        };
        for (size_t i = 0; i < sizeof cases / sizeof cases[0]; i++) {
            char buf[64];
            snprintf(buf, sizeof buf, "%s", cases[i].in);
            int rc = image_mount_point_normalise(buf);
            if (cases[i].out) CHECK(rc == 0 && strcmp(buf, cases[i].out) == 0);
            else CHECK(rc == -1);
        }
        CHECK(image_mount_point_normalise(NULL) == -1);
    }

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
