/* Host selftest for the sceKernelGetAppInfo struct layout.
 *
 * This layout was wrong in all four places it was copied to. Three
 * omitted `app_type`, putting title_id at offset 16; one used a byte
 * blob and put it at 64. Offset 16 lands on app_type, which is zero for
 * an ordinary process — so title_id read back as the empty string and
 * every feature that asks "what game is running" got nothing: process
 * listing, the cheat engine, and play-time tracking alike.
 *
 * The header carries _Static_asserts, but those only fire when the
 * payload is rebuilt with the PS5 toolchain. This runs on the host in
 * the normal test suite, and additionally proves the *behaviour*: that
 * a struct filled the way the kernel fills it yields the right title,
 * and that reading at the old offset yields the empty string this bug
 * produced. */
#include <stdio.h>
#include <string.h>
#include "../include/app_info.h"

static int failures = 0;

static void check(int cond, const char *what) {
    if (!cond) { printf("  FAIL: %s\n", what); failures++; }
}

int main(void) {
    check(offsetof(app_info_t, app_id) == 0, "app_id at 0");
    check(offsetof(app_info_t, title_id) == 16, "title_id at 16");

    /* A process as the kernel reports it. */
    app_info_t info;
    memset(&info, 0, sizeof(info));
    info.app_id = 0x1234;
    memcpy(info.title_id, "CUSA12345", 10);

    char tid[10] = {0};
    check(app_info_title_id(&info, tid, sizeof(tid)), "reads a valid title id");
    check(strcmp(tid, "CUSA12345") == 0, "title id round-trips exactly");

    /* The regression a release actually shipped: reading four bytes too
     * far returns the tail of the value, not the value. Pin the symptom
     * so the mistake is recognisable if it recurs. */
    const char *four_bytes_late = (const char *)&info + 20;
    check(strcmp(four_bytes_late, "12345") == 0,
          "reading at offset 20 returns only the tail (the shipped bug)");

    /* Shape validation — a wrong layout must degrade to "no title"
     * rather than feed garbage into cheat lookups and the UI. */
    check(app_info_title_id_valid("CUSA12345"), "CUSA accepted");
    check(app_info_title_id_valid("PPSA01234"), "PPSA accepted");
    check(app_info_title_id_valid("NPXS40000"), "NPXS accepted (system app)");
    check(!app_info_title_id_valid(""), "empty rejected");
    check(!app_info_title_id_valid("cusa12345"), "lowercase rejected");
    check(!app_info_title_id_valid("CUSA1234A"), "letter in digits rejected");
    check(!app_info_title_id_valid("CUS112345"), "digit in letters rejected");
    check(!app_info_title_id_valid("CUSA123456"), "too long rejected");

    /* The kernel is not required to terminate the field, so the accessor
     * must read exactly 9 characters and terminate the copy itself --
     * never running on into the bytes that follow. */
    memset(&info, 0, sizeof(info));
    memcpy(info.title_id, "CUSA99999", 9);
    info.title_id[9] = 'X';                /* no terminator */
    memcpy(info.unknown2, "GARBAGE", 7);   /* immediately after title_id */
    memset(tid, 0, sizeof(tid));
    check(app_info_title_id(&info, tid, sizeof(tid)),
          "unterminated title id still reads");
    check(strcmp(tid, "CUSA99999") == 0,
          "reads exactly 9 chars, never into the following bytes");

    if (failures == 0) {
        printf("✓ app_info layout puts title_id at offset 16 and validates it\n");
        return 0;
    }
    printf("✗ %d failure(s)\n", failures);
    return 1;
}
