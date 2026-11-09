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
    rmdir(dir);
    free(d);
    printf("file_compare_selftest: %s\n", fails ? "FAILED" : "all passed");
    return fails ? 1 : 0;
}
