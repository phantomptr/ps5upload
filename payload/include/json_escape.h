#ifndef PS5UPLOAD_JSON_ESCAPE_H
#define PS5UPLOAD_JSON_ESCAPE_H

/* A string as the inside of a JSON string: `"` and `\` are backslashed, a control byte reads '?'.
 * Cut at dst_cap, never mid-escape; always NUL-terminated (dst_cap > 0). Returns the length written.
 * The engine parses replies with serde: one bad byte rejects the whole of it. */
#include <stddef.h>

static inline size_t json_escape_into(const char *src, char *dst, size_t dst_cap) {
    size_t ei = 0, ni = 0;
    if (!dst || dst_cap == 0) return 0;
    while (src && src[ni] && ei + 2 < dst_cap) {
        unsigned char c = (unsigned char)src[ni];
        if (c == '"' || c == '\\') {
            dst[ei++] = '\\';
            dst[ei++] = (char)c;
        } else if (c < 0x20) {
            dst[ei++] = '?';
        } else {
            dst[ei++] = (char)c;
        }
        ni++;
    }
    dst[ei] = '\0';
    return ei;
}

/* As json_escape_into for text that may not be clean UTF-8 (a database column, a file's field):
 * a byte that is not part of a whole, valid UTF-8 character reads '?', so a cut in the middle of
 * one, a stray byte, an overlong form (C0/C1, E0 80-9F, F0 80-8F), an encoded surrogate (ED A0-BF)
 * or a code point past U+10FFFF (F4 90+, F5+) never reaches the engine (serde rejects them all). */
static inline size_t json_escape_utf8_into(const char *src, char *dst, size_t dst_cap) {
    size_t n = json_escape_into(src, dst, dst_cap), i = 0;
    while (i < n) {
        unsigned char c = (unsigned char)dst[i];
        size_t need = c < 0x80 ? 1 : (c >= 0xC2 && c <= 0xDF) ? 2 : (c >= 0xE0 && c <= 0xEF) ? 3 : (c >= 0xF0 && c <= 0xF4) ? 4 : 0;
        size_t k = 1;
        /* The second byte's range depends on the lead (RFC 3629 table): the rest is 80-BF. */
        unsigned char lo = c == 0xE0 ? 0xA0 : c == 0xF0 ? 0x90 : 0x80;
        unsigned char hi = c == 0xED ? 0x9F : c == 0xF4 ? 0x8F : 0xBF;
        if (need > 1 && i + 1 < n && ((unsigned char)dst[i + 1] < lo || (unsigned char)dst[i + 1] > hi)) need = 0;
        while (need && k < need && i + k < n && ((unsigned char)dst[i + k] & 0xC0) == 0x80) k++;
        if (need == 0 || k < need) {
            dst[i++] = '?';
            continue;
        }
        i += need;
    }
    return n;
}

#endif /* PS5UPLOAD_JSON_ESCAPE_H */
