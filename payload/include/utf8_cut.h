#ifndef PS5UPLOAD_UTF8_CUT_H
#define PS5UPLOAD_UTF8_CUT_H

/* Cutting UTF-8 text short on a character boundary. A byte cut puts half a character (a
 * Japanese or accented file name) at the edge of what is kept, and the engine reads such text
 * as invalid: a status line ended in garbage, or was not shown at all. Header-only, so the host
 * self-test (tests/utf8_cut_selftest.c) runs the same code. */
#include <stddef.h>

/* 1 for a UTF-8 continuation byte (10xxxxxx). */
static inline int utf8_is_cont(unsigned char c) { return (c & 0xC0) == 0x80; }

/* Where the last at most `keep` bytes of `s` (`len` bytes) start, moved forward past the
 * continuation bytes of a character the cut would split, so the tail starts on a whole
 * character. `len` when nothing is kept. */
static inline size_t utf8_tail_start(const char *s, size_t len, size_t keep) {
    size_t i = len > keep ? len - keep : 0;
    while (i < len && utf8_is_cont((unsigned char)s[i])) i++;
    return i;
}

/* The length of `s` (`len` bytes) without a last character that is cut short: a lead byte
 * whose continuation bytes did not all make it. Text that is whole, or not UTF-8 to begin
 * with, keeps its length. */
static inline size_t utf8_whole_prefix(const char *s, size_t len) {
    size_t i = len, have, need;
    unsigned char lead;
    while (i > 0 && len - i < 3 && utf8_is_cont((unsigned char)s[i - 1])) i--;
    if (i == 0) return len;
    lead = (unsigned char)s[i - 1];
    if (lead < 0xC0) return len; /* ASCII last, or a stray continuation byte: nothing cut here */
    need = lead >= 0xF0 ? 4 : lead >= 0xE0 ? 3 : 2;
    have = len - (i - 1);
    return have < need ? i - 1 : len;
}

#endif
