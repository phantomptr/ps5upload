/* Host-side tests for UTF-16 → UTF-8 cheat-XML normalisation (#317).
 *
 * The bug this guards: SHN/MC4 trainer files are sometimes saved UTF-16
 * (a real `ff fe`/`fe ff` BOM). The cheat parser scans bytes for ASCII tags
 * like "<Cheat", which never match `<\0C\0h\0e\0a\0t\0`, so those files
 * parsed to zero cheats and "didn't appear for some games". These cases
 * cover the byte shapes actually seen in the wild (a real UTF-16LE SHN from
 * etaHEN's repo starts exactly `ff fe 3c 00 3f 00 78 00 6d 00 6c 00`). */
#include <stdio.h>
#include <string.h>

#include "../src/xml_encoding.c"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

/* Build the UTF-16 (LE or BE) encoding of an ASCII string into `out`,
 * optionally with a BOM. Returns the byte length. */
static size_t make_utf16(const char *ascii, int little, int bom,
                         unsigned char *out) {
    size_t o = 0;
    if (bom) {
        if (little) { out[o++] = 0xFF; out[o++] = 0xFE; }
        else        { out[o++] = 0xFE; out[o++] = 0xFF; }
    }
    for (const char *p = ascii; *p; p++) {
        if (little) { out[o++] = (unsigned char)*p; out[o++] = 0x00; }
        else        { out[o++] = 0x00; out[o++] = (unsigned char)*p; }
    }
    return o;
}

int main(void) {
    unsigned char buf[512];
    size_t out_len = 0;

    /* Plain 8-bit input is left alone (NULL → caller parses the original). */
    {
        const char *ascii = "<Trainer><Cheat Text=\"x\"></Cheat></Trainer>";
        char *r = xml_to_utf8(ascii, strlen(ascii), &out_len);
        CHECK(r == NULL);
        CHECK(out_len == 0);
    }

    /* UTF-16LE with BOM → the ASCII tags reappear so the parser can see them. */
    {
        size_t n = make_utf16("<Cheat Text=\"Infinite Money\">", 1, 1, buf);
        char *r = xml_to_utf8((char *)buf, n, &out_len);
        CHECK(r != NULL);
        CHECK(r && strcmp(r, "<Cheat Text=\"Infinite Money\">") == 0);
        CHECK(out_len == strlen("<Cheat Text=\"Infinite Money\">"));
        free(r);
    }

    /* UTF-16BE with BOM. */
    {
        size_t n = make_utf16("<Cheat>", 0, 1, buf);
        char *r = xml_to_utf8((char *)buf, n, &out_len);
        CHECK(r != NULL);
        CHECK(r && strcmp(r, "<Cheat>") == 0);
        free(r);
    }

    /* UTF-16LE with NO BOM — recognised by the odd-byte-NUL heuristic. */
    {
        size_t n = make_utf16("<Cheatline><Offset>27693CA</Offset>", 1, 0, buf);
        char *r = xml_to_utf8((char *)buf, n, &out_len);
        CHECK(r != NULL);
        CHECK(r && strcmp(r, "<Cheatline><Offset>27693CA</Offset>") == 0);
        free(r);
    }

    /* UTF-16BE with NO BOM — even-byte-NUL heuristic. */
    {
        size_t n = make_utf16("<Cheat Name=\"y\">", 0, 0, buf);
        char *r = xml_to_utf8((char *)buf, n, &out_len);
        CHECK(r != NULL);
        CHECK(r && strcmp(r, "<Cheat Name=\"y\">") == 0);
        free(r);
    }

    /* A non-ASCII BMP char (é = U+00E9) round-trips to correct 2-byte UTF-8. */
    {
        unsigned char le[8];
        size_t o = 0;
        le[o++] = 0xFF; le[o++] = 0xFE;      /* BOM */
        le[o++] = 0xE9; le[o++] = 0x00;      /* é */
        char *r = xml_to_utf8((char *)le, o, &out_len);
        CHECK(r != NULL);
        CHECK(out_len == 2);
        CHECK(r && (unsigned char)r[0] == 0xC3 && (unsigned char)r[1] == 0xA9);
        free(r);
    }

    /* Too short to classify → NULL, no crash. */
    {
        char *r = xml_to_utf8("<", 1, &out_len);
        CHECK(r == NULL);
    }

    if (failures == 0) {
        printf("xml_encoding_selftest: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "xml_encoding_selftest: %d failure(s)\n", failures);
    return 1;
}
