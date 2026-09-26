/* Host-side test for the cross-device rename guard.
 *
 * This guard exists because of a hard console crash, not a style rule:
 * on this kernel `rename()` across two mounts does NOT return EXDEV the
 * way POSIX promises. It panics — black screen, power-cord pull. So
 * every rename() the payload performs on user-supplied paths has to
 * prove both ends live on the same device BEFORE calling it.
 *
 * The device lookup is injected so this can run on the host: the tests
 * describe a fake mount table rather than needing two real mounts. */
#include <stdio.h>
#include <string.h>

#include "../include/cross_device.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

#define CHECK_STR(got, want)                                            \
    do {                                                                \
        if (strcmp((got), (want)) != 0) {                               \
            fprintf(stderr, "FAIL line %d: got \"%s\" want \"%s\"\n",   \
                    __LINE__, (got), (want));                           \
            failures++;                                                 \
        }                                                               \
    } while (0)

/* A fake mount table: prefix → device id. Anything unmatched is
 * "cannot determine", which the guard must treat as its own case. */
static int fake_dev(const char *path, unsigned long long *out) {
    if (strncmp(path, "/mnt/usb0", 9) == 0) { *out = 10; return 0; }
    if (strncmp(path, "/mnt/ext0", 9) == 0) { *out = 11; return 0; }
    if (strncmp(path, "/data", 5) == 0)     { *out = 20; return 0; }
    if (strncmp(path, "/", 1) == 0 && strncmp(path, "/gone", 5) != 0) {
        *out = 20; /* the internal SSD holds everything else */
        return 0;
    }
    return -1; /* no such path */
}

int main(void) {
    char buf[64];

    /* ── parent directory extraction ───────────────────────────────── */
    xdev_parent_dir("/mnt/usb0/games/x.pkg", buf, sizeof(buf));
    CHECK_STR(buf, "/mnt/usb0/games");

    /* A file at the root: the parent is the root, not the empty string. */
    xdev_parent_dir("/file.pkg", buf, sizeof(buf));
    CHECK_STR(buf, "/");

    /* No slash at all — relative name, so the parent is the cwd. */
    xdev_parent_dir("file.pkg", buf, sizeof(buf));
    CHECK_STR(buf, ".");

    /* Trailing slash: rename(from, "/data/dir/") names the entry "dir"
     * inside "/data", so the containing directory — the one whose device
     * decides where the entry lands — is "/data", not "/data/dir". The
     * slash must also never leave the parent empty. */
    xdev_parent_dir("/data/dir/", buf, sizeof(buf));
    CHECK_STR(buf, "/data");

    /* Empty input must still produce a usable path, never a bare NUL
     * that a later stat() would treat as the cwd by accident. */
    xdev_parent_dir("", buf, sizeof(buf));
    CHECK_STR(buf, ".");

    /* Truncation must not run off the end of a short buffer. */
    {
        char tiny[8];
        xdev_parent_dir("/mnt/usb0/games/x.pkg", tiny, sizeof(tiny));
        CHECK(strlen(tiny) < sizeof(tiny));
    }

    /* ── the guard itself ──────────────────────────────────────────── */

    /* Same mount: safe, rename() may proceed. */
    CHECK(xdev_rename_crosses("/mnt/usb0/a.pkg", "/mnt/usb0/b.pkg", fake_dev)
          == XDEV_SAME);

    /* Same mount, different subdirectory: still safe. */
    CHECK(xdev_rename_crosses("/mnt/usb0/a.pkg", "/mnt/usb0/sub/b.pkg", fake_dev)
          == XDEV_SAME);

    /* THE PANIC CASE: USB → internal SSD. Must be refused. */
    CHECK(xdev_rename_crosses("/mnt/usb0/a.pkg", "/data/b.pkg", fake_dev)
          == XDEV_CROSSES);

    /* And the reverse direction. */
    CHECK(xdev_rename_crosses("/data/a.pkg", "/mnt/usb0/b.pkg", fake_dev)
          == XDEV_CROSSES);

    /* Two different external mounts are still two devices. */
    CHECK(xdev_rename_crosses("/mnt/usb0/a.pkg", "/mnt/ext0/b.pkg", fake_dev)
          == XDEV_CROSSES);

    /* The destination FILE need not exist — only its directory does.
     * Renaming to a new name in the same dir is the common case and
     * must not be misread as "unknown". */
    CHECK(xdev_rename_crosses("/data/a.pkg", "/data/brand-new.pkg", fake_dev)
          == XDEV_SAME);

    /* A missing source is not a device question; report unknown and let
     * rename() fail with a normal errno rather than guessing. */
    CHECK(xdev_rename_crosses("/gone/a.pkg", "/data/b.pkg", fake_dev)
          == XDEV_UNKNOWN);

    /* A destination whose PARENT does not exist, likewise. */
    CHECK(xdev_rename_crosses("/data/a.pkg", "/gone/sub/b.pkg", fake_dev)
          == XDEV_UNKNOWN);

    /* NULL inputs must not crash and must not report "safe". */
    CHECK(xdev_rename_crosses(NULL, "/data/b.pkg", fake_dev) == XDEV_UNKNOWN);
    CHECK(xdev_rename_crosses("/data/a.pkg", NULL, fake_dev) == XDEV_UNKNOWN);
    CHECK(xdev_rename_crosses("/data/a.pkg", "/data/b.pkg", NULL) == XDEV_UNKNOWN);

    /* Most important negative: UNKNOWN must be distinct from SAME, so a
     * caller can never accidentally treat "couldn't tell" as "safe to
     * rename". If these ever collapse to one value the guard is useless. */
    CHECK(XDEV_SAME != XDEV_UNKNOWN);
    CHECK(XDEV_CROSSES != XDEV_UNKNOWN);
    CHECK(XDEV_SAME != XDEV_CROSSES);

    printf("cross_device_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
