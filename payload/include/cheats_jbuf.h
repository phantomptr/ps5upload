/* The buffer cheats.c builds a title's cheat list in (cheats.get), apart from it so the host can
 * test it (payload/tests/cheats_reply_selftest.c).
 *
 * Nothing is ever written past `cap`, and `over` is set the moment something did not fit whole. A
 * caller that adds one list entry at a time notes `off` before the entry and, when `over` is set
 * after it, calls jb_rollback: the reply then ends at the last whole entry instead of in the
 * middle of one (which the engine cannot parse at all). */
#ifndef PS5UPLOAD_CHEATS_JBUF_H
#define PS5UPLOAD_CHEATS_JBUF_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

#include "cheats_list.h" /* cheats_utf8_char_len */

/* Room for `],"truncated":true}` and the NUL, kept back while the entries are written. */
#define CHEATS_MODS_TAIL_ROOM 32u

typedef struct {
    char  *buf;
    size_t cap;
    size_t off;
    int    over; /* something was cut */
} jbuf_t;

static inline void jb_init(jbuf_t *jb, char *buf, size_t cap) {
    jb->buf = buf;
    jb->cap = cap;
    jb->off = 0;
    jb->over = 0;
    if (cap > 0) buf[0] = '\0';
}

/* Back to `mark` (an earlier `off`), with nothing cut any more. */
static inline void jb_rollback(jbuf_t *jb, size_t mark) {
    jb->off = mark;
    jb->over = 0;
    jb->buf[jb->off] = '\0';
}

/* The inside of a JSON string: `"`, `\` and newline escaped, other control bytes dropped. */
static inline void jb_str(jbuf_t *jb, const char *s) {
    if (!s) return;
    while (*s && jb->off + 2 < jb->cap) {
        char c = *s;
        if (c == '"' || c == '\\') {
            if (jb->off + 3 >= jb->cap) break;
            jb->buf[jb->off++] = '\\';
            jb->buf[jb->off++] = c;
            s++;
        } else if (c == '\n') {
            if (jb->off + 3 >= jb->cap) break;
            jb->buf[jb->off++] = '\\';
            jb->buf[jb->off++] = 'n';
            s++;
        } else if ((unsigned char)c < 0x20) {
            s++;
        } else if ((unsigned char)c < 0x80) {
            jb->buf[jb->off++] = c;
            s++;
        } else {
            /* A non-ASCII byte must start a valid UTF-8 character, or the whole reply fails to
             * parse on the engine (Latin-1 / Shift-JIS names; a name cut mid-character). */
            int n = cheats_utf8_char_len(s);
            if (n == 0) {
                jb->buf[jb->off++] = '?';
                s++;
            } else {
                if (jb->off + (size_t)n + 2 >= jb->cap) break;
                memcpy(jb->buf + jb->off, s, (size_t)n);
                jb->off += (size_t)n;
                s += n;
            }
        }
    }
    if (*s) jb->over = 1;
    jb->buf[jb->off] = '\0';
}

static inline void jb_raw(jbuf_t *jb, const char *s) {
    size_t len = strlen(s);
    if (jb->off + len >= jb->cap) {
        len = jb->cap - jb->off - 1;
        jb->over = 1;
    }
    memcpy(jb->buf + jb->off, s, len);
    jb->off += len;
    jb->buf[jb->off] = '\0';
}

#define JB_PRINTF(jb, ...) do { \
    char _tmp[512]; \
    int _n = snprintf(_tmp, sizeof(_tmp), __VA_ARGS__); \
    if (_n >= (int)sizeof(_tmp)) (jb)->over = 1; \
    if (_n > 0) jb_raw((jb), _tmp); \
} while(0)

#endif /* PS5UPLOAD_CHEATS_JBUF_H */
