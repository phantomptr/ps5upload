/* Host test for when a left-behind extraction request (it can hold a password) is deleted: only once
 * no extractor can still be about to read it. Build: see the root Makefile test-payload. */
#include <stdio.h>

#include "archive_extract.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

int main(void) {
    const time_t t = 1000000;
    /* Written just now: an extractor another helper just started may be reading it. */
    CHECK(!archive_extract_request_stale(t, t));
    CHECK(!archive_extract_request_stale(t, t + 20));
    CHECK(!archive_extract_request_stale(t, t + EXTRACT_REQUEST_STALE_S - 1));
    /* Older than any extractor's start. */
    CHECK(archive_extract_request_stale(t, t + EXTRACT_REQUEST_STALE_S));
    CHECK(archive_extract_request_stale(t, t + 86400));
    /* The clock moved back since: its age is unknown, and the password must not stay. */
    CHECK(archive_extract_request_stale(t, t - 1));
    if (failures) {
        fprintf(stderr, "archive_request_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("archive_request_selftest: ok\n");
    return 0;
}
