/* Host-side test for the app.db reader.
 *
 * The reader is the only way to read app.db on real hardware: the
 * console ships no libSceSqlite.sprx, so the sqlite path never resolves.
 * Both the FTX2 APPDB_QUERY handler and Game Activity's "Recently
 * Played" depend on it.
 *
 * The fixture builds genuine SQLite leaf pages rather than a convenient
 * stand-in. That matters: SQLite writes a row's column values back to
 * back with no separators, and only the record header says where each
 * one ends. A fixture that separated columns would let a reader that
 * ignores the header look correct — which is exactly the bug this
 * replaced. Column layouts below are taken from a real PS5 app.db. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "../include/appdb_scan.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

/* Names reach the engine inside a JSON frame that serde parses; one bad
 * byte rejects the whole response, so validity is a hard invariant. */
static int is_valid_utf8(const char *s) {
    const unsigned char *p = (const unsigned char *)s;
    while (*p) {
        size_t need;
        if (*p < 0x80) {
            p++;
            continue;
        } else if (*p >= 0xC2 && *p <= 0xDF) need = 1;
        else if (*p >= 0xE0 && *p <= 0xEF) need = 2;
        else if (*p >= 0xF0 && *p <= 0xF4) need = 3;
        else return 0;
        p++;
        for (size_t k = 0; k < need; k++, p++)
            if ((*p & 0xC0) != 0x80) return 0;
    }
    return 1;
}

/* ── Minimal SQLite writer ─────────────────────────────────────────── */

#define PAGE_SIZE 4096
static unsigned char g_db[PAGE_SIZE * 4];
static size_t g_db_len;
static size_t g_content_top; /* cells fill the page from the back */
static int g_ncells;

static size_t put_varint(unsigned char *p, uint64_t v) {
    if (v < 0x80) {
        p[0] = (unsigned char)v;
        return 1;
    }
    if (v < 0x4000) {
        p[0] = (unsigned char)(0x80 | (v >> 7));
        p[1] = (unsigned char)(v & 0x7f);
        return 2;
    }
    p[0] = (unsigned char)(0x80 | (v >> 14));
    p[1] = (unsigned char)(0x80 | ((v >> 7) & 0x7f));
    p[2] = (unsigned char)(v & 0x7f);
    return 3;
}

static void db_reset(void) {
    memset(g_db, 0, sizeof(g_db));
    memcpy(g_db, "SQLite format 3\0", 16);
    g_db[16] = (unsigned char)(PAGE_SIZE >> 8);
    g_db[17] = (unsigned char)(PAGE_SIZE & 0xff);
    g_db[100] = 0x0D; /* leaf table b-tree */
    g_content_top = PAGE_SIZE;
    g_ncells = 0;
    g_db_len = PAGE_SIZE;
}

/* Append one row of TEXT columns as a real record: header of serial
 * types, then the values concatenated with nothing between them. */
static void db_add_row(const char *const *cols, const size_t *lens, int n) {
    unsigned char rec[1024];
    unsigned char types[64];
    size_t tn = 0;
    for (int i = 0; i < n; i++)
        tn += put_varint(types + tn, (uint64_t)(lens[i] * 2 + 13));

    /* The header's size varint counts itself. */
    size_t hsize_len = put_varint(rec, tn + 1) == 1 ? 1 : 2;
    size_t rn = put_varint(rec, tn + hsize_len);
    memcpy(rec + rn, types, tn);
    rn += tn;
    for (int i = 0; i < n; i++) {
        memcpy(rec + rn, cols[i], lens[i]);
        rn += lens[i];
    }

    unsigned char cell[1200];
    size_t cn = put_varint(cell, rn);
    cn += put_varint(cell + cn, (uint64_t)(g_ncells + 1)); /* rowid */
    memcpy(cell + cn, rec, rn);
    cn += rn;

    g_content_top -= cn;
    memcpy(g_db + g_content_top, cell, cn);

    size_t ptr = 100 + 8 + (size_t)g_ncells * 2;
    g_db[ptr] = (unsigned char)(g_content_top >> 8);
    g_db[ptr + 1] = (unsigned char)(g_content_top & 0xff);
    g_ncells++;

    g_db[103] = (unsigned char)(g_ncells >> 8);
    g_db[104] = (unsigned char)(g_ncells & 0xff);
    g_db[105] = (unsigned char)(g_content_top >> 8);
    g_db[106] = (unsigned char)(g_content_top & 0xff);
}

/* Convenience for all-NUL-terminated columns. */
static void db_add(const char *const *cols, int n) {
    size_t lens[16];
    for (int i = 0; i < n; i++) lens[i] = strlen(cols[i]);
    db_add_row(cols, lens, n);
}

int main(void) {
    appdb_entry_t out[8];
    int n;

    /* Not a database at all. */
    db_reset();
    memcpy(g_db, "NOT-A-SQLITE-DB", 15);
    CHECK(appdb_scan_entries(g_db, g_db_len, out, 8) == -1);

    /* Too short to hold the file header. */
    db_reset();
    CHECK(appdb_scan_entries(g_db, 50, out, 8) == -1);

    /* The simple shape: id then name. */
    db_reset();
    {
        const char *row[] = {"PPSA01234", "Astro's Playroom"};
        db_add(row, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 1);
    if (n == 1) {
        CHECK(strcmp(out[0].title_id, "PPSA01234") == 0);
        CHECK(strcmp(out[0].name, "Astro's Playroom") == 0);
    }

    /* The real tbl_info shape. Every column except the name is a decoy,
     * and all five are adjacent bytes in the file. */
    db_reset();
    {
        const char *row[] = {"PPSA19534",
                             "UP0006-PPSA19534_00-GLACIERGAME00000",
                             "Battlefield 6",
                             "/user/app/PPSA19534/sce_sys",
                             "2026-08-09 22:02:41.212"};
        db_add(row, 5);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 1);
    if (n == 1) {
        CHECK(strcmp(out[0].title_id, "PPSA19534") == 0);
        CHECK(strcmp(out[0].name, "Battlefield 6") == 0);
    }

    /* Two distinct games. */
    db_reset();
    {
        const char *a[] = {"PPSA01234", "Astro's Playroom"};
        const char *b[] = {"CUSA98765", "Bloodborne"};
        db_add(a, 2);
        db_add(b, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 2);
    if (n == 2) {
        CHECK(strcmp(out[0].title_id, "PPSA01234") == 0);
        CHECK(strcmp(out[1].name, "Bloodborne") == 0);
    }

    /* app.db repeats a title across tables; one entry per id. */
    db_reset();
    {
        const char *a[] = {"PPSA01234", "Astro's Playroom"};
        const char *b[] = {"PPSA01234", "Astro's Playroom"};
        db_add(a, 2);
        db_add(b, 2);
    }
    CHECK(appdb_scan_entries(g_db, g_db_len, out, 8) == 1);

    /* Never write past the caller's array. */
    db_reset();
    {
        const char *a[] = {"PPSA00001", "Game One"};
        const char *b[] = {"PPSA00002", "Game Two"};
        const char *c[] = {"PPSA00003", "Game Three"};
        db_add(a, 2);
        db_add(b, 2);
        db_add(c, 2);
    }
    CHECK(appdb_scan_entries(g_db, g_db_len, out, 2) == 2);

    /* A row with no title id is not a title row. */
    db_reset();
    {
        const char *row[] = {"cid:scp:000000000098b7b7", "Some Metadata"};
        db_add(row, 2);
    }
    CHECK(appdb_scan_entries(g_db, g_db_len, out, 8) == 0);

    /* Three-byte CJK survives intact. */
    db_reset();
    {
        const char *row[] = {"PPSA02000", "アストロボット"};
        db_add(row, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 1);
    if (n == 1) {
        CHECK(strcmp(out[0].name, "アストロボット") == 0);
        CHECK(is_valid_utf8(out[0].name));
    }

    /* Four-byte sequences too. */
    db_reset();
    {
        const char *row[] = {"PPSA02001", "Rocket League 🚀 Edition"};
        db_add(row, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 1);
    if (n == 1) CHECK(is_valid_utf8(out[0].name));

    /* A name holding a truncated sequence must be rejected outright, not
     * emitted with a dangling lead byte — that is what made the engine
     * reject the entire response on hardware. */
    db_reset();
    {
        const char bad[] = {'B', 'r', 'o', 'k', 'e', 'n', (char)0xC3};
        const char *row[] = {"PPSA02002", bad};
        size_t lens[] = {9, sizeof(bad)};
        db_add_row(row, lens, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    for (int i = 0; i < n; i++) CHECK(is_valid_utf8(out[i].name));

    /* Trailing padding is trimmed. */
    db_reset();
    {
        const char *row[] = {"PPSA02004", "Padded Title   "};
        db_add(row, 2);
    }
    n = appdb_scan_entries(g_db, g_db_len, out, 8);
    CHECK(n == 1);
    if (n == 1) CHECK(strcmp(out[0].name, "Padded Title") == 0);


    /* ids-only mode: install verification (appdb_has_title) only asks
     * whether a title id is present. A row carrying an id but no usable
     * name must still be reported there, or a real install reads as a
     * failure. The default mode keeps skipping it, because "Recently
     * Played" must not show nameless rows. */
    {
        const char *row[] = {"PPSA03000", "cid:scp:000000000098b7b7"};
        db_reset();
        db_add(row, 2);
        CHECK(appdb_scan_entries(g_db, g_db_len, out, 8) == 0);
        n = appdb_scan_entries_ex(g_db, g_db_len, out, 8, 1);
        CHECK(n == 1);
        if (n == 1) {
            CHECK(strcmp(out[0].title_id, "PPSA03000") == 0);
            CHECK(out[0].name[0] == '\0');
        }
    }

    /* ids-only must not change rows that do have a name. */
    {
        const char *row[] = {"PPSA03001", "Astro's Playroom"};
        db_reset();
        db_add(row, 2);
        n = appdb_scan_entries_ex(g_db, g_db_len, out, 8, 1);
        CHECK(n == 1);
        if (n == 1) CHECK(strcmp(out[0].name, "Astro's Playroom") == 0);
    }

    /* A row with no title id at all is still not a title row. */
    {
        const char *row[] = {"cid:scp:0000000000000001", "Some Metadata"};
        db_reset();
        db_add(row, 2);
        CHECK(appdb_scan_entries_ex(g_db, g_db_len, out, 8, 1) == 0);
    }

    printf("appdb_scan_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
