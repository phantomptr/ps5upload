/* The attribute reader of the SHN (XML) and game-patch cheat parsers, apart from cheats.c so the
 * host can test it (payload/tests/cheats_reply_selftest.c). */
#ifndef PS5UPLOAD_CHEATS_XML_ATTR_H
#define PS5UPLOAD_CHEATS_XML_ATTR_H

#include <ctype.h>
#include <stddef.h>
#include <stdio.h>
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

/* The text of the first <tag ...>text</tag> at or after `start`, without the whitespace around it.
 * 0 when found; 1 when it was found but does not fit (out then holds its first cap - 1 bytes, a full
 * buffer, which the callers' "value too long" check refuses); -1 when there is none.
 *
 * The whitespace is left out BEFORE the text is measured. It used to be cut at cap - 1 first and
 * trimmed after: an indented value (a pretty-printed SHN, "<ValueOn>" then a newline and spaces) of
 * more than cap - 1 bytes came out shorter than a full buffer, passed the check, and a cheat cut
 * short (a code cave without its jump back) was written. */
static inline int cheats_xml_text(const char *start, const char *tag_name, char *out, size_t cap) {
    char open_tag[64], close_tag[64];
    const char *p, *end, *s;
    size_t len;
    int cut = 0;
    if (!start || !tag_name || !out || cap == 0) return -1;
    out[0] = '\0';
    snprintf(open_tag, sizeof(open_tag), "<%s", tag_name);
    snprintf(close_tag, sizeof(close_tag), "</%s>", tag_name);
    p = strstr(start, open_tag);
    if (!p) return -1;
    p += strlen(open_tag);
    while (*p && *p != '>') p++; /* skip attrs */
    if (*p == '>') p++;
    end = strstr(p, close_tag);
    if (!end) return -1;
    s = p;
    while (s < end && isspace((unsigned char)*s)) s++;
    while (end > s && isspace((unsigned char)end[-1])) end--;
    len = (size_t)(end - s);
    if (len >= cap) {
        len = cap - 1;
        cut = 1;
    }
    memcpy(out, s, len);
    out[len] = '\0';
    return cut;
}

/* Where the element whose opening tag runs from `open_start` to `open_end` (just past its '>')
 * ends: just past `close_tag` (e.g. "</Cheat>") for a paired element, `open_end` itself for a
 * self-closing one (<Cheatline .../>). `*body_end` is where its content ends: the '<' of the
 * closing tag, or `open_end` (no content). NULL when a paired element has no closing tag.
 * A self-closing element is told by its own tag, never by whether a closing tag follows
 * somewhere: <Cheatline .../> then <Cheatline>..</Cheatline> took the second line's close for
 * the first's (its attributes read, the second line skipped), and <Cheat .../> took the next
 * cheat's lines and close for its own. */
static inline const char *cheats_xml_element_end(const char *open_start, const char *open_end,
                                                 const char *close_tag, const char **body_end) {
    const char *close;
    if (open_end - open_start >= 2 && open_end[-1] == '>' && open_end[-2] == '/') {
        *body_end = open_end;
        return open_end;
    }
    close = strstr(open_end, close_tag);
    if (!close) return NULL;
    *body_end = close;
    return close + strlen(close_tag);
}

#endif /* PS5UPLOAD_CHEATS_XML_ATTR_H */
