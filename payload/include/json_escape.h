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

#endif /* PS5UPLOAD_JSON_ESCAPE_H */
