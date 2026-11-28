/* The freeing schedule on its own (xfree.h): which parts can go before each unit, and the room the
 * unpack takes at its largest. `make host-test` runs it before tests/run.py, which checks the same
 * rules end to end on real archives. */
#include <stdio.h>
#include <string.h>

#include "xfree.h"

static int failures;

#define CHECK(cond)                                                                \
    do {                                                                           \
        if (!(cond)) {                                                             \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond);             \
            failures++;                                                            \
        }                                                                          \
    } while (0)

/* Parts of these sizes, one after another. */
static void parts(xfvols_t *v, uint64_t *size, uint64_t *start, const uint64_t *sizes, unsigned n) {
    memset(v, 0, sizeof *v);
    v->count = n;
    v->size = size;
    v->start = start;
    for (unsigned i = 0; i < n; i++) {
        size[i] = sizes[i];
        start[i] = v->total;
        v->total += sizes[i];
    }
}

/* Which parts are gone before each unit starts, as the unpack runs them. */
static void schedule(const xfvols_t *v, const uint64_t *lo, size_t n, unsigned *gone_before) {
    uint64_t need[64];
    xf_need(lo, n, need);
    for (size_t u = 0; u < n; u++) gone_before[u] = xf_before(v, need[u]);
}

/* A multi-part zip (or a RAR that is not solid): every file on its own. A part goes once the reader
 * has left it for good, and never while a file that starts in it is still to come. */
static void non_solid_set(void) {
    static const uint64_t sizes[] = {100, 100, 100, 100};
    uint64_t size[4], start[4];
    xfvols_t v;
    parts(&v, size, start, sizes, 4);
    /* five files: at 0, 60 (runs into part 2), 150, 230 (runs across part 3 into 4), 380 */
    static const uint64_t lo[] = {0, 60, 150, 230, 380};
    unsigned g[5];
    schedule(&v, lo, 5, g);
    CHECK(g[0] == 0 && g[1] == 0); /* file 2 still reads part 1 */
    CHECK(g[2] == 1);              /* file 3 starts in part 2: part 1 can go */
    CHECK(g[3] == 2);              /* file 4 starts in part 3 */
    CHECK(g[4] == 3);              /* file 5 is all in part 4 */
    /* At the edge: a unit that starts exactly where a part ends frees it. */
    CHECK(xf_before(&v, 100) == 1 && xf_before(&v, 99) == 0 && xf_before(&v, 400) == 4);
}

/* A 7z of solid blocks across parts: a part goes only when every block with a byte in it is out. */
static void solid_blocks(void) {
    static const uint64_t sizes[] = {100, 100, 100, 100, 100};
    uint64_t size[5], start[5];
    xfvols_t v;
    parts(&v, size, start, sizes, 5);
    /* block A spans parts 1-3 (0..250), block B parts 3-5 (250..480) */
    static const uint64_t lo[] = {XF_NO_READ, 32, 250};
    unsigned g[3];
    schedule(&v, lo, 3, g);
    CHECK(g[0] == 0); /* the folders and empty files read nothing, but block A still needs part 1 */
    CHECK(g[1] == 0);
    CHECK(g[2] == 2); /* block A is done: parts 1 and 2; part 3 holds the start of block B too */
    /* One block over the whole set: nothing goes before the end. */
    static const uint64_t one[] = {32};
    schedule(&v, one, 1, g);
    CHECK(g[0] == 0);
}

/* Units that do not read in order (a zip listed out of order): a part goes only when no unit still
 * to come reads it, whatever order they come in. */
static void out_of_order(void) {
    static const uint64_t sizes[] = {100, 100, 100};
    uint64_t size[3], start[3];
    xfvols_t v;
    parts(&v, size, start, sizes, 3);
    static const uint64_t lo[] = {200, 10, 120};
    unsigned g[3];
    schedule(&v, lo, 3, g);
    CHECK(g[0] == 0 && g[1] == 0); /* the second unit reads part 1 */
    CHECK(g[2] == 1);
}

/* The most room the unpack takes beyond what is free at the start, with the parts going on the way:
 * what is written by each unit's end, less what was deleted before it began. */
static void peak(void) {
    static const uint64_t sizes[] = {100, 100, 100, 100};
    uint64_t size[4], start[4], need[4];
    xfvols_t v;
    parts(&v, size, start, sizes, 4);
    /* four files, one per part, stored (as large unpacked as packed) */
    static const uint64_t lo[] = {0, 100, 200, 300}, bytes[] = {100, 100, 100, 100};
    xf_need(lo, 4, need);
    /* unit 4 writes 400 with parts 1-3 gone: 100 more than at the start, not 400 */
    CHECK(xf_peak(&v, need, bytes, 4) == 100);
    /* the same in one solid block: nothing goes until the end, all 400 */
    static const uint64_t lo1[] = {0}, bytes1[] = {400};
    xf_need(lo1, 1, need);
    CHECK(xf_peak(&v, need, bytes1, 1) == 400);
    /* a game that packs to half its size: 800 unpacked from 400 of parts */
    static const uint64_t bytes2[] = {200, 200, 200, 200};
    xf_need(lo, 4, need);
    CHECK(xf_peak(&v, need, bytes2, 4) == 500);
    /* units that read nothing take no part with them */
    static const uint64_t lo3[] = {XF_NO_READ, XF_NO_READ}, bytes3[] = {0, 0};
    xf_need(lo3, 2, need);
    CHECK(need[0] == XF_NO_READ && xf_before(&v, need[0]) == 4 && xf_peak(&v, need, bytes3, 2) == 0);
}

int main(void) {
    non_solid_set();
    solid_blocks();
    out_of_order();
    peak();
    if (failures) {
        fprintf(stderr, "xfree_test: %d failed\n", failures);
        return 1;
    }
    printf("xfree_test: ok\n");
    return 0;
}
