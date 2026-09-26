/* Host-side test for param.json version rewriting.
 *
 * The SDK Version Changer lowers a title's required firmware so a game
 * built for newer firmware will start on an older jailbroken console.
 * The previous implementation rewrote the value in place and could only
 * do so when the replacement was byte-for-byte the same length —
 * otherwise it changed nothing and the caller still reported "Patched".
 * The length cases below are the ones that silently did nothing. */
#include <stdio.h>
#include <string.h>

#include "../include/sdk_param.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

int main(void) {
    char out[8192];
    size_t out_len = 0;

    /* Firmware 5.05 is written as 0x0505000000000000. */
    {
        char v[32];
        CHECK(sdk_param_format_version(0x05050000, v, sizeof(v)) > 0);
        CHECK(strcmp(v, "0x0505000000000000") == 0);
        CHECK(sdk_param_format_version(0x09600000, v, sizeof(v)) > 0);
        CHECK(strcmp(v, "0x0960000000000000") == 0);
    }

    /* The ordinary case: both fields present, same length as the
     * replacement. */
    {
        const char *in =
            "{\"titleId\":\"PPSA01234\",\"sdkVersion\":\"0x1000000000000000\","
            "\"requiredSystemSoftwareVersion\":\"0x1000000000000000\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 2);
        CHECK(strstr(out, "\"sdkVersion\":\"0x0505000000000000\"") != NULL);
        CHECK(strstr(out,
                     "\"requiredSystemSoftwareVersion\":\"0x0505000000000000\"")
              != NULL);
        CHECK(strstr(out, "\"titleId\":\"PPSA01234\"") != NULL);
    }

    /* The regression this exists for: a shorter existing value. The old
     * in-place rewrite required an exact length match, so this changed
     * nothing while the UI said it had worked. */
    {
        const char *in =
            "{\"sdkVersion\":\"0x10000000\","
            "\"requiredSystemSoftwareVersion\":\"0x10000000\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 2);
        CHECK(strstr(out, "\"sdkVersion\":\"0x0505000000000000\"") != NULL);
        CHECK(out_len == strlen(out));
    }

    /* A longer existing value must also be replaced, not truncated. */
    {
        const char *in = "{\"requiredSystemSoftwareVersion\":"
                         "\"0x1000000000000000000000\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 1);
        CHECK(strstr(out, "\"0x0505000000000000\"") != NULL);
        CHECK(strstr(out, "000000000000") == NULL || strlen(out) < strlen(in));
    }

    /* Whitespace after the colon is legal JSON. */
    {
        const char *in = "{\"sdkVersion\" :   \"0x1000000000000000\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 1);
        CHECK(strstr(out, "\"0x0505000000000000\"") != NULL);
    }

    /* Only one of the two fields present. */
    {
        const char *in = "{\"requiredSystemSoftwareVersion\":\"0x1000000000000000\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 1);
    }

    /* Neither field present — must report 0 so the caller can say so
     * instead of claiming a patch happened. */
    {
        const char *in = "{\"titleId\":\"PPSA01234\",\"titleName\":\"Game\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 0);
    }

    /* Everything outside the replaced value survives byte-for-byte. */
    {
        const char *in =
            "{\"a\":\"keep\",\"sdkVersion\":\"0x1000000000000000\",\"z\":\"tail\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x09600000, out,
                                     sizeof(out), &out_len) == 1);
        CHECK(strstr(out, "\"a\":\"keep\"") != NULL);
        CHECK(strstr(out, "\"z\":\"tail\"") != NULL);
    }

    /* A field whose value is not a string must be left alone rather than
     * corrupted. */
    {
        const char *in = "{\"sdkVersion\":16777216}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 0);
    }

    printf("sdk_param_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
