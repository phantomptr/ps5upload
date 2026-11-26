/* file_contents_differ: the save write-back's last check (#770). */
#include "file_compare.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int fails;
#define CHECK(c) do { if (!(c)) { fprintf(stderr, "FAIL %s:%d %s\n", __FILE__, __LINE__, #c); fails++; } } while (0)

static void put(const char *p, const unsigned char *d, size_t n) {
    FILE *f = fopen(p, "wb");
    if (!f) { perror(p); exit(2); }
    fwrite(d, 1, n, f);
    fclose(f);
}

int main(void) {
    char dir[] = "/tmp/fcmpXXXXXX";
    if (!mkdtemp(dir)) return 2;
    char a[256], b[256];
    snprintf(a, sizeof a, "%s/a", dir);
    snprintf(b, sizeof b, "%s/b", dir);
    /* Bigger than one chunk, so the edit is in the second read. */
    size_t n = (3u << 20) + 17;
    unsigned char *d = malloc(n);
    for (size_t i = 0; i < n; i++) d[i] = (unsigned char)(i * 31u);
    put(a, d, n);
    put(b, d, n);
    CHECK(file_contents_differ(a, b) == 0);
    d[(1u << 20) + 5] ^= 0x40; /* one byte, same size: what a save edit looks like */
    put(b, d, n);
    CHECK(file_contents_differ(a, b) == 1);
    put(b, d, n - 1);
    CHECK(file_contents_differ(a, b) == 1);
    char missing[256];
    snprintf(missing, sizeof missing, "%s/none", dir);
    CHECK(file_contents_differ(a, missing) == -1);
    put(a, (const unsigned char *)"", 0);
    put(b, (const unsigned char *)"", 0);
    CHECK(file_contents_differ(a, b) == 0);
    unlink(a);
    unlink(b);

    /* keep_aside: two saves' edits kept under the same image name both stay. */
    char copy[256], kept[300], first[300];
    snprintf(copy, sizeof copy, "%s/sdimg_sce_sdmemory", dir);
    put(copy, (const unsigned char *)"first", 5);
    CHECK(keep_aside(copy, kept, sizeof kept) == 0);
    snprintf(first, sizeof first, "%s.unsaved", copy);
    CHECK(strcmp(kept, first) == 0);
    put(copy, (const unsigned char *)"second", 6);
    CHECK(keep_aside(copy, kept, sizeof kept) == 0);
    char second[300];
    snprintf(second, sizeof second, "%s.2.unsaved", copy);
    CHECK(strcmp(kept, second) == 0);
    CHECK(access(copy, F_OK) != 0);
    {
        char got[16] = {0};
        FILE *f = fopen(first, "rb");
        CHECK(f && fread(got, 1, sizeof got - 1, f) == 5 && strcmp(got, "first") == 0);
        if (f) fclose(f);
    }
    /* Nothing to keep: it says so and names the path. */
    CHECK(keep_aside(copy, kept, sizeof kept) == -1);
    CHECK(strcmp(kept, copy) == 0);
    /* A name that does not fit is never cut to some other file's name. */
    put(copy, (const unsigned char *)"x", 1);
    char tiny[8];
    CHECK(keep_aside(copy, tiny, sizeof tiny) == -1);
    CHECK(access(copy, F_OK) == 0);
    unlink(copy);
    unlink(first);
    unlink(second);
    rmdir(dir);
    free(d);
    printf("file_compare_selftest: %s\n", fails ? "FAILED" : "all passed");
    return fails ? 1 : 0;
}
