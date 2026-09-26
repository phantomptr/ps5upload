/* Host selftest for the Game Metadata title-id normalizer.
 *
 * The normalizer is the gate on every metadata lookup. It rejected the
 * bare 9-character title id that enumerating /user/appmeta produces --
 * which is exactly what the app's installed-games picker passes -- so
 * clicking any game failed with "Invalid format". These cases pin all
 * three accepted shapes and the rejections that matter.
 *
 * The functions are copied rather than included: tmdb.c pulls in PS5
 * headers that do not exist on the host. They are small and stable;
 * the point is to pin the accept/reject contract. */
#include <ctype.h>
#include <stdio.h>
#include <string.h>

static int is_valid_title_id(const char *s) {
    if (!s || strlen(s) != 12) return 0;
    for (int i = 0; i < 4; i++)
        if (!isupper((unsigned char)s[i])) return 0;
    for (int i = 4; i < 9; i++)
        if (!isdigit((unsigned char)s[i])) return 0;
    if (s[9] != '_') return 0;
    if (!isdigit((unsigned char)s[10]) || !isdigit((unsigned char)s[11])) return 0;
    return 1;
}

static int normalize_id(const char *input, char *title_id_out, size_t out_sz) {
    if (!input || !title_id_out || out_sz < 13) return 0;
    size_t len = strlen(input);
    if (len == 9) {
        char padded[13];
        memcpy(padded, input, 9);
        memcpy(padded + 9, "_00", 4);
        if (!is_valid_title_id(padded)) return 0;
        memcpy(title_id_out, padded, 13);
        return 1;
    }
    if (len == 12) {
        if (!is_valid_title_id(input)) return 0;
        memcpy(title_id_out, input, 12);
        title_id_out[12] = '\0';
        return 1;
    }
    if (len == 36) {
        if (input[6] != '-' || input[19] != '-') return 0;
        char tid[13];
        memcpy(tid, input + 7, 12);
        tid[12] = '\0';
        if (!is_valid_title_id(tid)) return 0;
        memcpy(title_id_out, tid, 13);
        return 1;
    }
    return 0;
}

static int failures = 0;

static void accepts(const char *in, const char *want) {
    char out[13] = {0};
    if (!normalize_id(in, out, sizeof(out))) {
        printf("  FAIL: %s was rejected, expected %s\n", in, want);
        failures++;
        return;
    }
    if (strcmp(out, want) != 0) {
        printf("  FAIL: %s -> %s, expected %s\n", in, out, want);
        failures++;
    }
}

static void rejects(const char *in) {
    char out[13] = {0};
    if (normalize_id(in, out, sizeof(out))) {
        printf("  FAIL: %s was accepted (-> %s), expected rejection\n", in, out);
        failures++;
    }
}

int main(void) {
    /* The regression: a bare title id, as /user/appmeta enumeration
     * produces and the installed-games picker passes. */
    accepts("CUSA12345", "CUSA12345_00");
    accepts("PPSA01234", "PPSA01234_00");

    /* The two shapes that already worked. */
    accepts("CUSA12345_00", "CUSA12345_00");
    accepts("CUSA12345_01", "CUSA12345_01");
    accepts("UP9000-CUSA12345_00-LABEL00123456789", "CUSA12345_00");

    /* Rejections. A wrong id must not silently become a cache key for
     * some other game's metadata. */
    rejects(NULL);
    rejects("");
    rejects("CUSA1234");           /* 8 — too short */
    rejects("CUSA123456");         /* 10 — neither shape */
    rejects("cusa12345");          /* lowercase prefix */
    rejects("CUS112345");          /* digit in the letter prefix */
    rejects("CUSA1234A");          /* letter in the numeric part */
    rejects("CUSA12345-00");       /* wrong separator */
    rejects("CUSA12345_0A");       /* non-digit suffix */
    rejects("UP9000+CUSA12345_00-LABEL00123456789"); /* right length, wrong delimiter */

    if (failures == 0) {
        printf("✓ metadata title-id normalizer accepts bare, full and content ids\n");
        return 0;
    }
    printf("✗ %d failure(s)\n", failures);
    return 1;
}
