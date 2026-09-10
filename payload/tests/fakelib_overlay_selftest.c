#include <stdio.h>
#include <string.h>
#include "../include/fakelib_overlay_paths.h"

static int failures;
#define CHECK(x) do { if (!(x)) { fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #x); failures++; } } while (0)

int main(void) {
    char title[10];
    CHECK(fakelib_overlay_title_from_sandbox("PPSA25411_000", title, sizeof(title)) == 1);
    CHECK(strcmp(title, "PPSA25411") == 0);
    CHECK(fakelib_overlay_title_from_sandbox("CUSA00900_017", title, sizeof(title)) == 1);
    CHECK(fakelib_overlay_title_from_sandbox("NPXS40087_000", title, sizeof(title)) == 0);
    CHECK(fakelib_overlay_title_from_sandbox("PPSA25411_00", title, sizeof(title)) == 0);

    CHECK(fakelib_overlay_target_matches(
        "/mnt/sandbox/PPSA25411_003/abc/common/lib", "unionfs",
        "/mnt/sandbox/PPSA25411_003/abc/common/lib") == 1);
    CHECK(fakelib_overlay_target_matches(
        "/mnt/sandbox/PPSA25411_003/abc/common/lib", "ufs",
        "/mnt/sandbox/PPSA25411_003/abc/common/lib") == 0);
    CHECK(fakelib_overlay_target_matches(
        "/mnt/sandbox/PPSA25411_003/other/common/lib", "unionfs",
        "/mnt/sandbox/PPSA25411_003/abc/common/lib") == 0);

    CHECK(fakelib_overlay_run_dir_ok("abc123") == 1);
    CHECK(fakelib_overlay_run_dir_ok("app0") == 0);
    CHECK(fakelib_overlay_run_dir_ok("../escape") == 0);
    CHECK(fakelib_overlay_run_dir_ok("common/lib") == 0);

    printf("fakelib_overlay_selftest: %s\n", failures ? "FAILED" : "ALL PASS");
    return failures ? 1 : 0;
}
