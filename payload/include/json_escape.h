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
 * a byte that is not part of a whole UTF-8 character reads '?', so a cut in the middle of one or
 * a stray byte never reaches the engine. */
static inline size_t json_escape_utf8_into(const char *src, char *dst, size_t dst_cap) {
    size_t n = json_escape_into(src, dst, dst_cap), i = 0;
    while (i < n) {
        unsigned char c = (unsigned char)dst[i];
        size_t need = c < 0x80 ? 1 : (c >= 0xC2 && c <= 0xDF) ? 2 : (c >= 0xE0 && c <= 0xEF) ? 3 : (c >= 0xF0 && c <= 0xF4) ? 4 : 0;
        size_t k = 1;
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
