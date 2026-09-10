#ifndef PS5UPLOAD_LIBC_BACKPORT_H
#define PS5UPLOAD_LIBC_BACKPORT_H

#include <stddef.h>
#include <string.h>

/* BestPig's `sce_module/libc.prx` compatibility edit.
 *
 * Some titles will not start on a downgraded firmware until one symbol NID in
 * libc.prx is swapped. BackPork documents it as a perl substitution over the
 * base64 NID text; MacPork applies the same edit to the decoded bytes:
 *
 *     base64("4h6F1LLbTiw") == e2 1e 85 d4 b2 db 4e 2c   (BackPork's FIND)
 *     base64("IWIBBdTHit4") == 21 62 01 05 d4 c7 8a de   (BackPork's REPLACE)
 *
 * Two projects, two encodings, one edit — which is a useful cross-check that
 * the pattern below is the right one. We patch the ASCII form because that is
 * what the symbol table actually stores: on a real title the text form is
 * present exactly once and the raw-byte form not at all.
 *
 * Both strings are the same length, so this never moves anything: no offsets
 * shift, no segment sizes change, and the surrounding SELF container stays
 * consistent.
 *
 * Tests: payload/tests/libc_backport_selftest.c */

#define LIBC_BACKPORT_FIND    "4h6F1LLbTiw#A#B"
#define LIBC_BACKPORT_REPLACE "IWIBBdTHit4#A#B"
#define LIBC_BACKPORT_LEN     15u

/* Replace every occurrence in `buf`, in place. Returns how many were
 * rewritten — 0 means the file is already patched or is a libc build that
 * does not carry this symbol, which is not an error. */
static inline int libc_backport_apply(unsigned char *buf, size_t len) {
    if (!buf || len < LIBC_BACKPORT_LEN) return 0;
    int n = 0;
    for (size_t i = 0; i + LIBC_BACKPORT_LEN <= len; i++) {
        if (memcmp(buf + i, LIBC_BACKPORT_FIND, LIBC_BACKPORT_LEN) != 0) continue;
        memcpy(buf + i, LIBC_BACKPORT_REPLACE, LIBC_BACKPORT_LEN);
        i += LIBC_BACKPORT_LEN - 1;
        n++;
    }
    return n;
}

/* Already carrying the replacement? Used to report "nothing to do" distinctly
 * from "this libc does not need it". */
static inline int libc_backport_is_applied(const unsigned char *buf, size_t len) {
    if (!buf || len < LIBC_BACKPORT_LEN) return 0;
    for (size_t i = 0; i + LIBC_BACKPORT_LEN <= len; i++) {
        if (memcmp(buf + i, LIBC_BACKPORT_REPLACE, LIBC_BACKPORT_LEN) == 0) return 1;
    }
    return 0;
}

#endif
