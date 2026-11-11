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

    /* A real param.json is far larger than the 8 KiB this used to copy
     * through: with every language's title it reaches ~30 KiB. It read as "no
     * version fields" (0) and was silently left unpatched. The fields sit at
     * the END here, behind the bulk, as they do in real files. */
    {
        static char big[40000], big_out[40000 + 128];
        size_t n = 0;
        n += (size_t)snprintf(big + n, sizeof(big) - n, "{\"titleId\":\"PPSA01234\",\"pad\":\"");
        while (n < 30000) big[n++] = 'x';
        n += (size_t)snprintf(big + n, sizeof(big) - n,
                              "\",\"sdkVersion\":\"0x10000000\","
                              "\"requiredSystemSoftwareVersion\":\"0x1000000000000000\"}");
        size_t big_len = 0;
        CHECK(sdk_param_rewrite_json(big, n, 0x05050000, big_out,
                                     sizeof(big_out), &big_len) == 2);
        CHECK(big_len == n + 8); /* the short sdkVersion grew by 8 */
        CHECK(big_len == strlen(big_out));
        CHECK(strstr(big_out, "\"sdkVersion\":\"0x0505000000000000\",") != NULL);
        CHECK(strstr(big_out,
                     "\"requiredSystemSoftwareVersion\":\"0x0505000000000000\"}")
              != NULL);
        CHECK(memcmp(big_out, big, 30000) == 0);

        /* No room for the result: an error, never "nothing to change" and
         * never a half-rewritten document reported as done. */
        CHECK(sdk_param_rewrite_json(big, n, 0x05050000, big_out, n + 4,
                                     &big_len) == -1);
        CHECK(sdk_param_rewrite_json(big, n, 0x05050000, big_out, n,
                                     &big_len) == -1);
        /* Exactly enough (text + the 8 added bytes + NUL) is enough. */
        CHECK(sdk_param_rewrite_json(big, n, 0x05050000, big_out, n + 9,
                                     &big_len) == 2);
        CHECK(big_len == n + 8 && big_out[big_len] == '\0');
    }

    /* A value that SHRINKS moves the tail down without losing any of it. */
    {
        const char *in = "{\"sdkVersion\":\"0x1000000000000000000000\",\"z\":\"tail\"}";
        CHECK(sdk_param_rewrite_json(in, strlen(in), 0x05050000, out,
                                     sizeof(out), &out_len) == 1);
        CHECK(strcmp(out, "{\"sdkVersion\":\"0x0505000000000000\",\"z\":\"tail\"}") == 0);
        CHECK(out_len == strlen(out));
    }

    printf("sdk_param_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
