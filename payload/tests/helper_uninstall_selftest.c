/* Host self-test of the "Remove PS5Upload from this PS5" folder delete (helper_uninstall.c).
 *
 *   cc -DHU_HOST_TEST -Ipayload/include payload/tests/helper_uninstall_selftest.c
 *
 * Builds a tree in a temporary folder and checks that everything goes except a kept path, that a
 * link is removed as a name and never followed, and that a folder on another filesystem is never
 * gone into (a second filesystem when the host has one at hand: HU_OTHER_FS names a folder on a
 * different device, which the test plants a link-free mount stand-in for by checking st_dev). */
#define HU_HOST_TEST 1
#include "../src/helper_uninstall.c"

#include <fcntl.h>

static int failures;
#define CHECK(c)                                                                                   \
    do {                                                                                           \
        if (!(c)) {                                                                                \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c);                           \
            failures++;                                                                            \
        }                                                                                          \
    } while (0)

static void put(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
        (void)write(fd, "x", 1);
        close(fd);
    }
}

static int exists(const char *path) {
    struct stat st;
    return lstat(path, &st) == 0;
}

int main(void) {
    char base[] = "/tmp/hu-selftest-XXXXXX";
    char root[512], p[1024], outside[512];
    hu_counts_t c = {0, 0, 0};
    if (!mkdtemp(base)) {
        perror("mkdtemp");
        return 1;
    }
    snprintf(root, sizeof root, "%s/ps5upload", base);
    snprintf(outside, sizeof outside, "%s/outside", base);
    mkdir(root, 0777);
    mkdir(outside, 0777);
    snprintf(p, sizeof p, "%s/victim", outside);
    put(p);

    /* The tree: files, nested folders, a kept folder with content, a link to outside. */
    snprintf(p, sizeof p, "%s/webui.json", root);
    put(p);
    snprintf(p, sizeof p, "%s/cheats", root);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/cheats/a", root);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/cheats/a/b", root);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/cheats/a/b/c", root);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/cheats/a/b/c/x.json", root);
    put(p);
    snprintf(p, sizeof p, "%s/backups", root);
    mkdir(p, 0777);
    snprintf(p, sizeof p, "%s/backups/save.zip", root);
    put(p);
    snprintf(p, sizeof p, "%s/link", root);
    CHECK(symlink(outside, p) == 0);

    {
        char kept[600];
        const char *keep[1];
        snprintf(kept, sizeof kept, "%s/backups", root);
        keep[0] = kept;
        hu_remove_tree(root, keep, 1, &c);
        CHECK(exists(kept));
        snprintf(p, sizeof p, "%s/backups/save.zip", root);
        CHECK(exists(p));
        CHECK(exists(root)); /* holds the kept folder */
        snprintf(p, sizeof p, "%s/webui.json", root);
        CHECK(!exists(p));
        snprintf(p, sizeof p, "%s/cheats", root);
        CHECK(!exists(p));
        snprintf(p, sizeof p, "%s/link", root);
        CHECK(!exists(p));
        snprintf(p, sizeof p, "%s/victim", outside);
        CHECK(exists(p)); /* the link was removed, never followed */
        CHECK(c.kept == 1);
        CHECK(c.failed == 0);
        CHECK(c.removed == 7); /* webui.json, x.json, c, b, a, cheats, link */
    }

    /* Without a keep list everything goes, the root too. */
    memset(&c, 0, sizeof c);
    hu_remove_tree(root, NULL, 0, &c);
    CHECK(!exists(root));
    CHECK(c.failed == 0);
    CHECK(c.removed == 3); /* save.zip, backups, root */

    /* Missing: nothing to do. */
    memset(&c, 0, sizeof c);
    hu_remove_tree(root, NULL, 0, &c);
    CHECK(c.removed == 0 && c.failed == 0 && c.kept == 0);

    /* Another filesystem inside the tree (HU_OTHER_FS: a mounted folder, e.g. a RAM disk) is
     * never entered: its files survive and it counts as kept. */
    {
        const char *other = getenv("HU_OTHER_FS");
        if (other && *other) {
            struct stat a, b;
            char mark[600];
            snprintf(mark, sizeof mark, "%s/must-survive", other);
            put(mark);
            mkdir(root, 0777);
            CHECK(stat(root, &a) == 0 && stat(other, &b) == 0 && a.st_dev != b.st_dev);
            /* The tree reaches it only as a folder of its own: a mount point inside the root. The
             * test cannot mount without privileges, so it walks the other filesystem's own parent
             * with the root's device by calling the walker directly. */
            memset(&c, 0, sizeof c);
            remove_at(other, a.st_dev, 1, NULL, 0, &c);
            CHECK(exists(mark));
            CHECK(c.kept == 1 && c.removed == 0);
            unlink(mark);
            rmdir(root);
        } else {
            fprintf(stderr, "note: HU_OTHER_FS not set; the other-filesystem case was not run\n");
        }
    }

    {
        /* A live mount's source under our folders is kept under both of its names; others are
         * not ours. */
        const char *froms[] = {"/data/ps5upload/tile", "/dev/ssd0.user", "/user/data/ps5upload/mounts/x",
                               "/data/ps5upload2/y", NULL};
        char out[8][HU_KEEP_PATH];
        size_t n = hu_source_keeps(froms, 5, out, 8);
        CHECK(n == 4);
        CHECK(strcmp(out[0], "/data/ps5upload/tile") == 0);
        CHECK(strcmp(out[1], "/user/data/ps5upload/tile") == 0);
        CHECK(strcmp(out[2], "/data/ps5upload/mounts/x") == 0);
        CHECK(strcmp(out[3], "/user/data/ps5upload/mounts/x") == 0);
        CHECK(hu_source_keeps(froms, 5, out, 1) == 1);
    }
    CHECK(hu_mount_under("/data/ps5upload", "/data/ps5upload"));
    CHECK(hu_mount_under("/data/ps5upload/mounts/x", "/data/ps5upload"));
    CHECK(!hu_mount_under("/data/ps5upload2/x", "/data/ps5upload"));
    CHECK(!hu_mount_under("/data", "/data/ps5upload"));

    snprintf(p, sizeof p, "%s/victim", outside);
    unlink(p);
    rmdir(outside);
    rmdir(base);
    if (failures) {
        fprintf(stderr, "helper_uninstall_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("helper_uninstall_selftest: ok\n");
    return 0;
}
