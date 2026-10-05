#include "xml_encoding.h"

#include <stdlib.h>

/* Encode one BMP code point as UTF-8. Writes 1-3 bytes, returns the count. */
static size_t enc_utf8(unsigned cp, unsigned char *out) {
    if (cp < 0x80) {
        out[0] = (unsigned char)cp;
        return 1;
    }
    if (cp < 0x800) {
        out[0] = (unsigned char)(0xC0 | (cp >> 6));
        out[1] = (unsigned char)(0x80 | (cp & 0x3F));
        return 2;
    }
    out[0] = (unsigned char)(0xE0 | (cp >> 12));
    out[1] = (unsigned char)(0x80 | ((cp >> 6) & 0x3F));
    out[2] = (unsigned char)(0x80 | (cp & 0x3F));
    return 3;
}

char *xml_to_utf8(const char *in, size_t in_len, size_t *out_len) {
    if (out_len) *out_len = 0;
    if (!in || in_len < 2) return NULL;

    const unsigned char *b = (const unsigned char *)in;
    int little = -1; /* 1 = UTF-16LE, 0 = UTF-16BE, -1 = not UTF-16 */
    size_t start = 0;

    if (b[0] == 0xFF && b[1] == 0xFE) {
        little = 1;
        start = 2; /* skip the BOM */
    } else if (b[0] == 0xFE && b[1] == 0xFF) {
        little = 0;
        start = 2;
    } else {
        /* No BOM. ASCII text stored as UTF-16LE has 0x00 at every odd byte
         * (the high half of each unit); UTF-16BE has it at every even byte.
         * Sample the head and require a strong majority so genuine 8-bit XML
         * with the odd embedded NUL is never misread. */
        size_t n = in_len < 128 ? in_len : 128;
        size_t zero_odd = 0, zero_even = 0, pairs = 0;
        for (size_t i = 0; i + 1 < n; i += 2) {
            if (b[i] == 0) zero_even++;
            if (b[i + 1] == 0) zero_odd++;
            pairs++;
        }
        if (pairs >= 4) {
            if (zero_odd * 4 >= pairs * 3) little = 1; /* high byte odd → LE */
            else if (zero_even * 4 >= pairs * 3) little = 0; /* high byte even → BE */
        }
    }

    if (little < 0) return NULL; /* already 8-bit — parse the original */

    /* Worst case is 3 UTF-8 bytes per UTF-16 code unit (BMP). */
    size_t cap = ((in_len - start) / 2) * 3 + 1;
    unsigned char *out = (unsigned char *)malloc(cap);
    if (!out) return NULL;

    size_t o = 0;
    for (size_t i = start; i + 1 < in_len; i += 2) {
        unsigned cu = little ? (unsigned)(b[i] | (b[i + 1] << 8))
                             : (unsigned)((b[i] << 8) | b[i + 1]);
        /* Astral planes (surrogate pairs) are vanishingly rare in cheat text;
         * fold either half to '?' rather than mis-decode. */
        if (cu >= 0xD800 && cu <= 0xDFFF) cu = '?';
        o += enc_utf8(cu, out + o);
    }
    out[o] = '\0';
    if (out_len) *out_len = o;
    return (char *)out;
}

size_t xml_unescape_entities(char *buf, size_t len) {
    if (!buf) return 0;
    size_t i = 0;
    while (i < len && (buf[i] == ' ' || buf[i] == '\t' || buf[i] == '\r' ||
                       buf[i] == '\n' || buf[i] == '\0'))
        i++;
    if (len - i < 4 || buf[i] != '&' || buf[i + 1] != 'l' || buf[i + 2] != 't' ||
        buf[i + 3] != ';')
        return len;

    size_t o = 0;
    for (size_t r = i; r < len;) {
        char c = buf[r];
        if (c == '&') {
            size_t semi = r + 1;
            while (semi < len && semi - r <= 8 && buf[semi] != ';') semi++;
            if (semi < len && buf[semi] == ';') {
                size_t n = semi - r - 1;
                const char *e = buf + r + 1;
                int ch = -1;
                if (n == 2 && e[0] == 'l' && e[1] == 't') ch = '<';
                else if (n == 2 && e[0] == 'g' && e[1] == 't') ch = '>';
                else if (n == 3 && e[0] == 'a' && e[1] == 'm' && e[2] == 'p') ch = '&';
                else if (n == 4 && e[0] == 'q' && e[1] == 'u' && e[2] == 'o' && e[3] == 't') ch = '"';
                else if (n == 4 && e[0] == 'a' && e[1] == 'p' && e[2] == 'o' && e[3] == 's') ch = '\'';
                else if (n >= 2 && e[0] == '#') {
                    unsigned v = 0;
                    int ok = 1;
                    if (e[1] == 'x' || e[1] == 'X') {
                        if (n < 3) ok = 0;
                        for (size_t k = 2; ok && k < n; k++) {
                            char d = e[k];
                            if (d >= '0' && d <= '9') v = v * 16 + (unsigned)(d - '0');
                            else if (d >= 'a' && d <= 'f') v = v * 16 + (unsigned)(d - 'a' + 10);
                            else if (d >= 'A' && d <= 'F') v = v * 16 + (unsigned)(d - 'A' + 10);
                            else ok = 0;
                        }
                    } else {
                        for (size_t k = 1; ok && k < n; k++) {
                            if (e[k] >= '0' && e[k] <= '9') v = v * 10 + (unsigned)(e[k] - '0');
                            else ok = 0;
                        }
                    }
                    if (ok && v > 0 && v < 0x80) ch = (int)v;
                }
                if (ch >= 0) {
                    /* `\&quot;` is an escaped quote written as an entity:
                     * drop the backslash that came before it. */
                    if (ch == '"' && o > 0 && buf[o - 1] == '\\') o--;
                    buf[o++] = (char)ch;
                    r = semi + 1;
                    continue;
                }
            }
        } else if (c == '\\' && r + 1 < len) {
            char d = buf[r + 1];
            int ch = -1;
            if (d == '"' || d == '\\' || d == '/') ch = d;
            else if (d == 'r') ch = '\r';
            else if (d == 'n') ch = '\n';
            else if (d == 't') ch = '\t';
            if (ch >= 0) {
                buf[o++] = (char)ch;
                r += 2;
                continue;
            }
        }
        buf[o++] = c;
        r++;
    }
    buf[o] = '\0';
    return o;
}
