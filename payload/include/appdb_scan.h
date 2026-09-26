#ifndef PS5UPLOAD_APPDB_SCAN_H
#define PS5UPLOAD_APPDB_SCAN_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* Reader for Sony's app.db, for consoles where sqlite is unreachable —
 * which is all of them: the payload links no sqlite, and no
 * libSceSqlite.sprx exists under /system/common/lib, /system/priv/lib or
 * /system_ex/common/lib, so dlsym(RTLD_DEFAULT, "sqlite3_open_v2")
 * cannot resolve on any firmware.
 *
 * This walks SQLite's on-disk format directly: leaf table b-tree pages,
 * their cell pointer arrays, and each cell's record header. Reading the
 * record header is the whole point — SQLite concatenates a row's column
 * values with no separators, and only the header's serial types say
 * where one column ends and the next begins. A scan that just looked for
 * printable runs would return
 *
 *     PPSA19534UP0006-PPSA19534_00-...Battlefield(tm) 6/user/app/...
 *
 * as a single "name", which is exactly what it did before.
 *
 * Read-only, bounds-checked, and tolerant of anything it does not
 * recognise: a page or cell that does not parse is skipped, never
 * trusted. Header-only so the payload and the host-built selftest share
 * one implementation — same pattern as hw_guard.h and
 * ptrace_recovery.h. Tests: payload/tests/appdb_scan_selftest.c. */

#define APPDB_TITLE_ID_LEN 10 /* 9 chars + NUL */
#define APPDB_NAME_MAX 256

typedef struct {
    char title_id[APPDB_TITLE_ID_LEN];
    char name[APPDB_NAME_MAX];
} appdb_entry_t;

/* Byte length of the printable character starting at `p`, or 0 if `p`
 * does not begin one.
 *
 * Whole sequences only. Names travel inside a JSON frame that the engine
 * parses with serde, and one malformed byte rejects the entire response
 * rather than a single row — so a truncated sequence must never escape.
 * Covers 2-byte accents, 3-byte CJK, and 4-byte emoji, all of which
 * occur in real PS5 title names. */
static inline size_t appdb_text_seq_len(const unsigned char *p, size_t avail) {
    if (avail == 0) return 0;
    unsigned char c = p[0];
    if (c >= 0x20 && c < 0x7f) return 1;

    size_t need;
    if (c >= 0xC2 && c <= 0xDF) need = 2;
    else if (c >= 0xE0 && c <= 0xEF) need = 3;
    else if (c >= 0xF0 && c <= 0xF4) need = 4;
    else return 0; /* bare continuation, or an invalid lead byte */

    if (avail < need) return 0;
    for (size_t k = 1; k < need; k++)
        if ((p[k] & 0xC0) != 0x80) return 0;
    return need;
}

/* SQLite's variable-length integer. Returns bytes consumed (0 on
 * overrun) and writes the value through `out`. */
static inline size_t appdb_varint(const unsigned char *b, size_t len,
                                  size_t off, uint64_t *out) {
    uint64_t v = 0;
    for (size_t i = 0; i < 9; i++) {
        if (off + i >= len) return 0;
        unsigned char c = b[off + i];
        if (i == 8) {
            v = (v << 8) | c;
            *out = v;
            return 9;
        }
        v = (v << 7) | (uint64_t)(c & 0x7f);
        if (!(c & 0x80)) {
            *out = v;
            return i + 1;
        }
    }
    *out = v;
    return 9;
}

/* Title ids are exactly four uppercase letters then five digits. */
static inline int appdb_is_title_id(const char *s, size_t n) {
    if (n != 9) return 0;
    for (size_t i = 0; i < 4; i++)
        if (s[i] < 'A' || s[i] > 'Z') return 0;
    for (size_t i = 4; i < 9; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    return 1;
}

/* Rows carry several strings besides the name: the content id
 * ("UP0006-PPSA19534_00-..."), a sandbox path, a "cid:scp:..." handle,
 * and ISO timestamps. None of them are the title. */
static inline int appdb_is_not_a_name(const char *s, size_t n) {
    if (n < 2) return 1;
    if (s[0] == '/') return 1;
    if (n >= 4 && memcmp(s, "cid:", 4) == 0) return 1;
    if (appdb_is_title_id(s, n)) return 1;
    /* ISO date: 2026-08-09 ... */
    if (n >= 10 && s[4] == '-' && s[7] == '-') {
        int digits = 1;
        for (size_t i = 0; i < 4; i++)
            if (s[i] < '0' || s[i] > '9') digits = 0;
        if (digits) return 1;
    }
    /* Content id: two uppercase letters, four digits, then '-'. */
    if (n >= 7 && s[0] >= 'A' && s[0] <= 'Z' && s[1] >= 'A' && s[1] <= 'Z' &&
        s[2] >= '0' && s[2] <= '9' && s[3] >= '0' && s[3] <= '9' &&
        s[4] >= '0' && s[4] <= '9' && s[5] >= '0' && s[5] <= '9' && s[6] == '-')
        return 1;
    return 0;
}

/* Whole string must be printable, valid UTF-8. */
static inline int appdb_is_clean_text(const char *s, size_t n) {
    size_t i = 0;
    while (i < n) {
        size_t seq = appdb_text_seq_len((const unsigned char *)s + i, n - i);
        if (seq == 0) return 0;
        i += seq;
    }
    return 1;
}

/* Pull the title id and name out of one decoded row.
 * Returns 1 when both were found. */
static inline int appdb_row_to_entry(const unsigned char *buf, size_t len,
                                     const size_t *val_off,
                                     const size_t *val_len, int nvals,
                                     appdb_entry_t *out, int ids_only) {
    int tid_at = -1;
    for (int i = 0; i < nvals; i++) {
        if (val_off[i] + val_len[i] > len) return 0;
        if (appdb_is_title_id((const char *)buf + val_off[i], val_len[i])) {
            tid_at = i;
            break;
        }
    }
    if (tid_at < 0) return 0;

    for (int i = 0; i < nvals; i++) {
        const char *s = (const char *)buf + val_off[i];
        size_t n = val_len[i];
        if (n == 0 || n >= APPDB_NAME_MAX) continue;
        if (appdb_is_not_a_name(s, n)) continue;
        if (!appdb_is_clean_text(s, n)) continue;

        memcpy(out->title_id, buf + val_off[tid_at], 9);
        out->title_id[9] = '\0';
        memcpy(out->name, s, n);
        out->name[n] = '\0';
        /* Trailing padding shows up in some rows. */
        while (n > 0 && out->name[n - 1] == ' ') out->name[--n] = '\0';
        if (n >= 2) return 1;
        break;
    }

    /* No usable name. Install verification only asks whether the id is
     * present, so report it there; the default caller feeds a
     * user-visible list and must not show a nameless row. */
    if (ids_only) {
        memcpy(out->title_id, buf + val_off[tid_at], 9);
        out->title_id[9] = '\0';
        out->name[0] = '\0';
        return 1;
    }
    return 0;
}

/* Decode one leaf cell into `out`. Returns 1 on success. */
static inline int appdb_read_cell(const unsigned char *buf, size_t len,
                                  size_t coff, appdb_entry_t *out,
                                  int ids_only) {
    uint64_t payload = 0, rowid = 0, hsize = 0;
    size_t o = coff;
    size_t got = appdb_varint(buf, len, o, &payload);
    if (got == 0) return 0;
    o += got;
    got = appdb_varint(buf, len, o, &rowid);
    if (got == 0) return 0;
    o += got;

    size_t rec = o;
    /* A payload that runs past the buffer spilled onto an overflow page;
     * we do not follow those, so skip the row rather than read garbage. */
    if (payload == 0 || rec + payload > len) return 0;

    got = appdb_varint(buf, len, rec, &hsize);
    if (got == 0 || hsize < got || rec + hsize > len) return 0;

    size_t toff = rec + got;
    size_t voff = rec + hsize;
    size_t val_off[32], val_len[32];
    int nvals = 0;

    while (toff < rec + hsize && nvals < 32) {
        uint64_t stype = 0;
        size_t adv = appdb_varint(buf, len, toff, &stype);
        if (adv == 0) return 0;
        toff += adv;

        size_t width;
        if (stype == 0 || stype == 8 || stype == 9) width = 0;
        else if (stype <= 4) width = (size_t)stype;
        else if (stype == 5) width = 6;
        else if (stype == 6 || stype == 7) width = 8;
        else if (stype >= 12) width = (size_t)((stype - 12 - (stype & 1)) / 2);
        else width = 0; /* 10, 11 — reserved */

        if (voff + width > len) return 0;
        /* Only TEXT (odd serial types >= 13) can hold a title or name. */
        if (stype >= 13 && (stype & 1)) {
            val_off[nvals] = voff;
            val_len[nvals] = width;
            nvals++;
        }
        voff += width;
    }
    if (nvals == 0) return 0;
    return appdb_row_to_entry(buf, len, val_off, val_len, nvals, out,
                              ids_only);
}

/* Scan an app.db image for title-id/name pairs.
 *
 * Returns the number of entries written to `out` (at most `max`), or -1
 * if the buffer is too short or lacks the SQLite file header. */
static inline int appdb_scan_entries_ex(const unsigned char *buf, size_t len,
                                        appdb_entry_t *out, int max,
                                        int ids_only) {
    if (!buf || !out || max <= 0) return -1;
    if (len < 100 || memcmp(buf, "SQLite format 3\0", 16) != 0) return -1;

    size_t page_size = ((size_t)buf[16] << 8) | (size_t)buf[17];
    if (page_size == 1) page_size = 65536; /* the format's escape for 64K */
    if (page_size < 512 || (page_size & (page_size - 1)) != 0) return -1;

    int count = 0;
    for (size_t base = 0; base + page_size <= len && count < max;
         base += page_size) {
        /* Page 1 carries the 100-byte file header before its own. */
        size_t hoff = base + (base == 0 ? 100 : 0);
        if (hoff + 8 > len) break;
        if (buf[hoff] != 0x0D) continue; /* leaf table b-tree only */

        size_t ncells = ((size_t)buf[hoff + 3] << 8) | (size_t)buf[hoff + 4];
        size_t ptr = hoff + 8;
        for (size_t c = 0; c < ncells && count < max; c++, ptr += 2) {
            if (ptr + 2 > len) break;
            size_t rel = ((size_t)buf[ptr] << 8) | (size_t)buf[ptr + 1];
            if (rel == 0 || rel >= page_size) continue;
            size_t coff = base + rel;
            if (coff >= len) continue;

            appdb_entry_t entry;
            if (!appdb_read_cell(buf, len, coff, &entry, ids_only)) continue;

            /* app.db repeats a title across several tables; keep the
             * first name we see for each id. */
            int dup = 0;
            for (int k = 0; k < count; k++) {
                if (strcmp(out[k].title_id, entry.title_id) == 0) {
                    dup = 1;
                    break;
                }
            }
            if (dup) continue;

            out[count] = entry;
            count++;
        }
    }

    return count;
}

/* Title/name pairs for display: rows without a usable name are skipped. */
static inline int appdb_scan_entries(const unsigned char *buf, size_t len,
                                     appdb_entry_t *out, int max) {
    return appdb_scan_entries_ex(buf, len, out, max, 0);
}

#endif
