/* The attribute reader of the SHN (XML) and game-patch cheat parsers, apart from cheats.c so the
 * host can test it (payload/tests/cheats_reply_selftest.c). */
#ifndef PS5UPLOAD_CHEATS_XML_ATTR_H
#define PS5UPLOAD_CHEATS_XML_ATTR_H

#include <stddef.h>
#include <string.h>

/* Finds attr="value" in the NUL-terminated `tag` and copies the value (cut at cap). 0 when found.
 *
 * A name that is there without ="value" (`ValueOff/>`, or a tag cut at its buffer right after the
 * name) is passed by ONCE. It used to be passed twice: the second step went over the tag's NUL
 * when the name was the last thing in it, and the search then ran on through whatever lay after
 * the buffer (the previous line's attribute still in a reused buffer, or memory past its end). */
static inline int cheats_xml_attr(const char *tag, const char *attr, char *out, size_t cap) {
    if (!tag || !attr || !out || cap == 0) return -1;
    out[0] = '\0';
    size_t alen = strlen(attr);
    if (alen == 0) return -1;
    const char *p = tag;
    while ((p = strstr(p, attr))) {
        /* A whole attribute name only: "Text" must not match inside "ContextText". */
        int whole = p == tag || p[-1] == ' ' || p[-1] == '\t' || p[-1] == '\n' || p[-1] == '\r';
        p += alen; /* still inside the tag: strstr matched alen bytes before its NUL */
        if (!whole) continue;
        while (*p == ' ' || *p == '\t' || *p == '=') p++;
        if (*p == '"') {
            p++;
            size_t i = 0;
            while (*p && *p != '"' && i + 1 < cap) {
                out[i++] = *p++;
            }
            out[i] = '\0';
            return 0;
        }
    }
    return -1;
}

#endif /* PS5UPLOAD_CHEATS_XML_ATTR_H */
