/* #889: a running game's temp folder (/app_temp<N>) is a place files may be copied to. */
#include <stdio.h>
#include "../include/app_temp_path.h"

static int fails;
#define CHECK(cond) do { if (!(cond)) { fprintf(stderr, "FAIL %s:%d %s\n", __FILE__, __LINE__, #cond); fails++; } } while (0)

int main(void) {
    CHECK(is_app_temp_path("/app_temp0"));
    CHECK(is_app_temp_path("/app_temp0/"));
    CHECK(is_app_temp_path("/app_temp0/PPSA17221/user513995995/games/com.mojang/behavior_packs"));
    CHECK(is_app_temp_path("/app_temp1/x"));
    CHECK(is_app_temp_path("/app_temp12/x"));
    /* Not that folder. */
    CHECK(!is_app_temp_path("/app_temp"));
    CHECK(!is_app_temp_path("/app_temp/x"));
    CHECK(!is_app_temp_path("/app_tempX/x"));
    CHECK(!is_app_temp_path("/app_temp123/x"));
    CHECK(!is_app_temp_path("/app_temp0x"));
    CHECK(!is_app_temp_path("/app_temp0x/y"));
    CHECK(!is_app_temp_path("app_temp0/x"));
    CHECK(!is_app_temp_path("/system/app_temp0"));
    CHECK(!is_app_temp_path("/app0/x"));
    CHECK(!is_app_temp_path(""));
    CHECK(!is_app_temp_path(NULL));
    if (fails) return 1;
    puts("app_temp_path_selftest: ALL PASS");
    return 0;
}
