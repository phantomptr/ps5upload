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
    /* The destination is one line of job.rec: with a line break in it the sweep after a crash read
     * (and removed) the folder named before the break. */
    CHECK(archive_extract_dest_ok("/data/homebrew/My Game (v1.02)"));
    CHECK(archive_extract_dest_ok("/mnt/usb0/\xE3\x82\xB2\xE3\x83\xBC\xE3\x83\xA0"));
    CHECK(!archive_extract_dest_ok("/data/homebrew\nx"));
    CHECK(!archive_extract_dest_ok("/data/homebrew\rx"));
    CHECK(!archive_extract_dest_ok("/data/home\tbrew"));
    CHECK(!archive_extract_dest_ok("/data/x\x7f"));
    CHECK(!archive_extract_dest_ok("data/x") && !archive_extract_dest_ok("") && !archive_extract_dest_ok(NULL));
    /* A pid from a file: 4294967295 cast to pid_t is -1 (kill(-1): every process), 4294967297 is 1. */
    CHECK(archive_extract_pid_ok(2) && archive_extract_pid_ok(77) && archive_extract_pid_ok(0x7FFFFFFFull));
    CHECK(!archive_extract_pid_ok(0) && !archive_extract_pid_ok(1));
    CHECK(!archive_extract_pid_ok(4294967295ull) && !archive_extract_pid_ok(4294967297ull));
    CHECK(!archive_extract_pid_ok(0x80000000ull) && !archive_extract_pid_ok(~0ull));
    if (failures) {
        fprintf(stderr, "archive_request_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("archive_request_selftest: ok\n");
    return 0;
}
