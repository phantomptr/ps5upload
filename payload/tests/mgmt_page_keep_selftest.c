/* Host selftest for the paged methods' kept answer: pages after the first come from one walk,
 * and a new look, another method or another user never gets a stale list. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/mgmt_page_keep.c"

static int a_fn, b_fn;
#define NO_USER UINT64_MAX

static int has(const void *fn, uint64_t user, uint64_t now, const char *want) {
    char *out = NULL;
    size_t len = 0;
    if (!mgmt_page_keep_take(fn, user, now, &out, &len)) return 0;
    int same = len == strlen(want) && memcmp(out, want, len) == 0;
    free(out);
    return same;
}

int main(void) {
    assert(!has(&a_fn, NO_USER, 0, ""));

    mgmt_page_keep_store(&a_fn, NO_USER, "[1,2]", 5, 1000);
    assert(has(&a_fn, NO_USER, 1000, "[1,2]"));
    assert(has(&a_fn, NO_USER, 1000 + MGMT_PAGE_KEEP_MS, "[1,2]"));

    /* Another method, another user: walked for themselves, and what is kept stays. */
    assert(!has(&b_fn, NO_USER, 1000, "[1,2]"));
    assert(!has(&a_fn, 7, 1000, "[1,2]"));
    assert(has(&a_fn, NO_USER, 1001, "[1,2]"));

    /* Too old, or a clock that went back: gone. */
    assert(!has(&a_fn, NO_USER, 1001 + MGMT_PAGE_KEEP_MS, "[1,2]"));
    assert(!has(&a_fn, NO_USER, 1002, "[1,2]"));
    mgmt_page_keep_store(&a_fn, NO_USER, "[3]", 3, 5000);
    assert(!has(&a_fn, NO_USER, 4000, "[3]"));
    assert(!has(&a_fn, NO_USER, 5000, "[3]"));

    /* A new store replaces; a clear empties. */
    mgmt_page_keep_store(&a_fn, 7, "[4]", 3, 100);
    mgmt_page_keep_store(&b_fn, 7, "[5]", 3, 100);
    assert(!has(&a_fn, 7, 100, "[4]"));
    assert(has(&b_fn, 7, 100, "[5]"));
    mgmt_page_keep_clear();
    assert(!has(&b_fn, 7, 100, "[5]"));

    puts("mgmt_page_keep: ok");
    return 0;
}
