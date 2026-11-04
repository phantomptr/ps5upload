/* Host test for atomic_file.h: the bytes reach the disk before the rename, and a failure at any step
 * leaves the old file as it was. fsync and rename are counted, so the order is what is checked. Build:
 * see the root Makefile test-payload. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int g_fsyncs, g_fsync_fail, g_renames, g_fsyncs_at_rename;

static int counted_fsync(int fd) {
    g_fsyncs++;
    if (g_fsync_fail) {
        errno = EIO;
        return -1;
    }
    return fsync(fd);
}
static int counted_rename(const char *from, const char *to) {
    g_renames++;
    g_fsyncs_at_rename = g_fsyncs;
    return rename(from, to);
}

#define fsync counted_fsync
#define rename counted_rename
#include "atomic_file.h"
#undef fsync
#undef rename

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static void put(const char *path, const char *text) {
    FILE *f = fopen(path, "w");
    if (!f) abort();
    fputs(text, f);
    fclose(f);
}

static int holds(const char *path, const char *text) {
    char buf[256] = {0};
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    return n == strlen(text) && memcmp(buf, text, n) == 0;
}

static int exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static void reset(void) { g_fsyncs = g_fsync_fail = g_renames = g_fsyncs_at_rename = 0; }

int main(void) {
    char root[] = "/tmp/atomicXXXXXX";
    if (!mkdtemp(root)) return 1;
    char path[64], tmp[80];
    snprintf(path, sizeof(path), "%s/file", root);
    snprintf(tmp, sizeof(tmp), "%s/file.tmp", root);

    /* The bytes are on disk before the name moves. */
    put(path, "old");
    reset();
    CHECK(atomic_file_write(path, tmp, "new", 3, 0644) == 0);
    CHECK(g_fsyncs == 1 && g_renames == 1 && g_fsyncs_at_rename == 1);
    CHECK(holds(path, "new"));
    CHECK(!exists(tmp));

    /* fsync failed: nothing moves, the old file stays, no temp is left. */
    put(path, "old");
    reset();
    g_fsync_fail = 1;
    CHECK(atomic_file_write(path, tmp, "new", 3, 0644) == -1);
    CHECK(g_renames == 0);
    CHECK(holds(path, "old"));
    CHECK(!exists(tmp));

    /* The temp cannot be opened: the old file stays. */
    char nodir[96];
    snprintf(nodir, sizeof(nodir), "%s/missing/file.tmp", root);
    reset();
    CHECK(atomic_file_write(path, nodir, "new", 3, 0644) == -1);
    CHECK(g_renames == 0);
    CHECK(holds(path, "old"));

    /* A link at the temp's name is refused, not written through. */
    char victim[64];
    snprintf(victim, sizeof(victim), "%s/victim", root);
    put(victim, "safe");
    if (symlink(victim, tmp) != 0) abort();
    reset();
    CHECK(atomic_file_write(path, tmp, "new", 3, 0644) == -1);
    CHECK(holds(victim, "safe"));
    CHECK(holds(path, "old"));
    unlink(tmp);

    /* Empty is a valid file. */
    reset();
    CHECK(atomic_file_write(path, tmp, "", 0, 0644) == 0);
    CHECK(holds(path, ""));

    /* stdio: flushed and synced before it is closed. */
    FILE *f = fopen(tmp, "w");
    if (!f) abort();
    fputs("streamed", f);
    reset();
    CHECK(atomic_file_close_stream(f) == 0);
    CHECK(g_fsyncs == 1);
    CHECK(holds(tmp, "streamed"));

    /* stdio: a sync that fails is reported. */
    f = fopen(tmp, "w");
    if (!f) abort();
    fputs("streamed", f);
    reset();
    g_fsync_fail = 1;
    CHECK(atomic_file_close_stream(f) == -1);

    /* stdio: an earlier write that failed is reported, even though the flush has nothing left to do. */
    f = fopen(tmp, "r");
    if (!f) abort();
    (void)fputc('x', f); /* read-only stream: sets the error flag */
    reset();
    CHECK(atomic_file_close_stream(f) == -1);
    CHECK(g_fsyncs == 0);

    unlink(tmp);
    unlink(path);
    unlink(victim);
    rmdir(root);
    if (failures) {
        fprintf(stderr, "atomic_file_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("atomic_file_selftest: ok\n");
    return 0;
}
