#include "file_compare.h"

#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define CMP_CHUNK (1u << 20)

/* Reads up to n bytes, retrying short reads; how many it got, or -1. */
static ssize_t read_full(int fd, unsigned char *buf, size_t n) {
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(fd, buf + got, n - got);
        if (r < 0) return -1;
        if (r == 0) break;
        got += (size_t)r;
    }
    return (ssize_t)got;
}

int file_contents_differ(const char *a, const char *b) {
    struct stat sa, sb;
    if (stat(a, &sa) != 0 || stat(b, &sb) != 0) return -1;
    if (sa.st_size != sb.st_size) return 1;
    int fa = open(a, O_RDONLY), fb = open(b, O_RDONLY);
    unsigned char *ba = malloc(CMP_CHUNK), *bb = malloc(CMP_CHUNK);
    int rc = -1;
    if (fa < 0 || fb < 0 || !ba || !bb) goto done;
    for (;;) {
        ssize_t ra = read_full(fa, ba, CMP_CHUNK), rb = read_full(fb, bb, CMP_CHUNK);
        if (ra < 0 || rb < 0) goto done;
        if (ra != rb || memcmp(ba, bb, (size_t)ra) != 0) {
            rc = 1;
            goto done;
        }
        if (ra == 0) {
            rc = 0;
            goto done;
        }
    }
done:
    if (fa >= 0) close(fa);
    if (fb >= 0) close(fb);
    free(ba);
    free(bb);
    return rc;
}
