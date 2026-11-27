/*
 * Host self-test for content_db.c — the console content-database reader.
 *
 * Builds the real content_db.c against the real vendored SQLite and points
 * it at fixture databases in a temp directory, so the schema discovery, the
 * fallback ordering and the write guards are exercised for real rather than
 * simulated.
 *
 * The cases that matter here are the ones that used to be impossible to
 * check, because the SQL half of this code had never run on any console:
 *   - the app table is found by inspection, not by a hardcoded name (the
 *     two dead implementations disagreed about that name);
 *   - a database that will not open falls back to the byte scanner;
 *   - an appinfo write that would touch zero rows changes nothing and says
 *     so, instead of reporting a silent no-op as success.
 */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "content_db.h"
#include "sqlite3.h"

/* content_db.c refuses to edit a running title. The host has no PS5
 * processes; this stub lets the test drive both sides of that guard. */
int g_fake_running = 0;
int proc_find_pid_by_title_id(const char *title_id) {
    (void)title_id;
    return g_fake_running ? 4242 : -1;
}

static void exec_or_die(sqlite3 *db, const char *sql) {
    char *err = NULL;
    if (sqlite3_exec(db, sql, NULL, NULL, &err) != SQLITE_OK) {
        fprintf(stderr, "fixture setup failed: %s\n", err ? err : "?");
        exit(1);
    }
}

/* app.db fixture, shaped like a real console rather than like the dead
 * code's guesses.
 *
 * Verified on FW 9.60 and 5.10: these tables are keyed by user id, so the
 * names look like tbl_info_<userid>. That matches neither hardcoded name
 * the dead paths used (tbl_appbrowse_2_appinfo / tbl_appbrowser_2_appinfo)
 * and would not be caught by a "tbl_app%" prefix filter either — so
 * discovery has to work from the table's shape.
 *
 * A second user's table is included and left empty, because a console with
 * more than one user has more than one of these and picking the first one
 * found would return nothing. */
static void make_app_db(const char *path) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db,
        "CREATE TABLE tbl_info_1000000 ("
        "  titleId TEXT, appId INTEGER, titleName TEXT);");
    exec_or_die(db,
        "CREATE TABLE tbl_info_1000001 ("
        "  titleId TEXT, appId INTEGER, titleName TEXT);");
    /* Decoys: right prefix, wrong shape. */
    exec_or_die(db, "CREATE TABLE tbl_appbrowse_2_appinfo (junk TEXT);");
    exec_or_die(db, "CREATE TABLE tbl_iconinfo_1000000 (titleId TEXT);");
    exec_or_die(db,
        "INSERT INTO tbl_info_1000000 VALUES"
        " ('CUSA00900', 101, 'Bloodborne'),"
        " ('PPSA01650', 102, 'YouTube'),"
        " ('NPXS40000', 103, 'Media Gallery');");
    sqlite3_close(db);
}

static void make_appinfo_db(const char *path) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db,
        "CREATE TABLE tbl_appinfo (titleId TEXT, key TEXT, val TEXT);");
    exec_or_die(db,
        "INSERT INTO tbl_appinfo VALUES"
        " ('PPSA01650','CONTENT_VERSION','01.00'),"
        " ('PPSA01650','VERSION_FILE_URI','http://sony.example/v.json'),"
        " ('PPSA03595','_org_path','/mnt/ext1/user/app/PPSA03595'),"
        " ('CUSA00900','CONTENT_VERSION','01.09');");
    sqlite3_close(db);
}

/* app.db with home-screen icon tables (#407): one per user, each with a
 * `visible` flag, plus a decoy that has titleId but no visible column. */
static void make_icon_db(const char *path) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db,
        "CREATE TABLE tbl_iconinfo_1000000 (titleId TEXT, visible INTEGER);"
        "CREATE TABLE tbl_iconinfo_1000001 (titleId TEXT, visible INTEGER);"
        "CREATE TABLE tbl_info_1000000 (titleId TEXT, titleName TEXT);"
        "INSERT INTO tbl_iconinfo_1000000 VALUES"
        " ('PPSA01650',1),('ITEM00001',1),('NPXS40000',1),('NPXS40037',0);"
        "INSERT INTO tbl_iconinfo_1000001 VALUES ('ITEM00001',1);"
        "INSERT INTO tbl_info_1000000 VALUES ('ITEM00001','Shortcut');");
    sqlite3_close(db);
}

static int read_visible(const char *table, const char *title_id) {
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    char sql[128];
    int v = -1;
    snprintf(sql, sizeof sql, "SELECT visible FROM %s WHERE titleId = ?", table);
    assert(sqlite3_open(CONTENT_DB_APP, &db) == SQLITE_OK);
    assert(sqlite3_prepare_v2(db, sql, -1, &st, NULL) == SQLITE_OK);
    sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
    if (sqlite3_step(st) == SQLITE_ROW) v = sqlite3_column_int(st, 0);
    sqlite3_finalize(st);
    sqlite3_close(db);
    return v;
}

static int count_row(void *ctx, int ncol, const char *const *vals) {
    (void)ncol;
    (void)vals;
    (*(int *)ctx)++;
    return 0;
}

static int contains(const char *hay, const char *needle) {
    return strstr(hay, needle) != NULL;
}


/* ── app.db pagination fixtures (#473) ────────────────────────────────
 *
 * A big app table: `n` rows CUSA00000.. in title order, plus (when
 * `late` is set) one late-sorting row ZZZZ99999 -- the shape of a title
 * that used to fall off the end of the fixed 256-row window. */
static void make_big_app_db(const char *path, int n, int late,
                            int long_names) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db,
        "CREATE TABLE tbl_info_1000000 ("
        "  titleId TEXT, appId INTEGER, titleName TEXT);");
    exec_or_die(db, "BEGIN");
    sqlite3_stmt *st = NULL;
    assert(sqlite3_prepare_v2(db,
                              "INSERT INTO tbl_info_1000000 VALUES (?,?,?)",
                              -1, &st, NULL) == SQLITE_OK);
    for (int i = 0; i < n; i++) {
        char tid[16], name[300];
        snprintf(tid, sizeof tid, "CUSA%05d", i);
        if (long_names) {
            /* 200+ characters with quotes, backslashes and a control
             * byte: every one of them grows when escaped. */
            int o = snprintf(name, sizeof name, "Game \"%d\" \\ \t", i);
            while (o < 240) name[o++] = 'x';
            name[o] = '\0';
        } else {
            snprintf(name, sizeof name, "Game %d", i);
        }
        sqlite3_bind_text(st, 1, tid, -1, SQLITE_TRANSIENT);
        sqlite3_bind_int(st, 2, 1000 + i);
        sqlite3_bind_text(st, 3, name, -1, SQLITE_TRANSIENT);
        assert(sqlite3_step(st) == SQLITE_DONE);
        sqlite3_reset(st);
    }
    sqlite3_finalize(st);
    if (late)
        exec_or_die(db, "INSERT INTO tbl_info_1000000 VALUES"
                        " ('ZZZZ99999', 9, 'Late Title')");
    exec_or_die(db, "COMMIT");
    sqlite3_close(db);
}

/* The unsigned number after "key": in `json`, or -1. */
static long json_num(const char *json, const char *key) {
    char needle[40];
    snprintf(needle, sizeof needle, "\"%s\":", key);
    const char *p = strstr(json, needle);
    if (!p) return -1;
    return strtol(p + strlen(needle), NULL, 10);
}

static int json_true(const char *json, const char *key) {
    char needle[40];
    snprintf(needle, sizeof needle, "\"%s\":true", key);
    return strstr(json, needle) != NULL;
}

/* Count the title ids in one page and check each is the next expected
 * one, so a skipped or repeated row fails, not just a short count. */
static int page_ids(const char *json, int *seen_upto, int *saw_late) {
    int n = 0;
    const char *p = json;
    while ((p = strstr(p, "\"title_id\":\"")) != NULL) {
        p += strlen("\"title_id\":\"");
        if (strncmp(p, "ZZZZ99999", 9) == 0) {
            *saw_late = 1;
        } else {
            char want[16];
            snprintf(want, sizeof want, "CUSA%05d", *seen_upto);
            assert(strncmp(p, want, 9) == 0);
            (*seen_upto)++;
        }
        n++;
    }
    return n;
}

/* Page through the whole table the way the engine does; returns how many
 * rows arrived. Fails if a page claims more but makes no progress. */
static int page_all(char *buf, size_t cap, unsigned limit, int *saw_late,
                    int *pages, int *truncated) {
    int seen_upto = 0, total = 0;
    unsigned offset = 0;
    *saw_late = 0;
    *pages = 0;
    *truncated = 0;
    for (;;) {
        size_t written = 0;
        assert(content_db_apps_page_json(offset, limit, buf, cap, &written) ==
               0);
        assert(written > 0 && written < cap);
        assert(buf[written - 1] == '}');
        (*pages)++;
        int got = page_ids(buf, &seen_upto, saw_late);
        total += got;
        assert(json_num(buf, "offset") == (long)offset);
        assert(json_num(buf, "next") == (long)(offset + (unsigned)got));
        if (json_true(buf, "truncated")) *truncated = 1;
        if (!json_true(buf, "more")) break;
        assert(got > 0);
        offset += (unsigned)got;
        assert(*pages < 10000);
    }
    return total;
}

int main(void) {
    char buf[64 * 1024];
    size_t written = 0;

    /* ── app.db: schema discovery finds the table by shape ───────────── */
    make_app_db(CONTENT_DB_APP);
    assert(content_db_apps_json(buf, sizeof(buf), &written) == 0);
    assert(written > 0);
    assert(contains(buf, "\"source\":\"sqlite\""));
    assert(contains(buf, "\"title_id\":\"CUSA00900\""));
    assert(contains(buf, "\"name\":\"Bloodborne\""));
    assert(contains(buf, "\"app_id\":101"));
    printf("✓ user-id-suffixed app table found by shape, not by name\n");

    /* The empty sibling table and the titleId-only icon table must not win
     * the selection — both would produce a confident, wrong, empty list. */
    assert(contains(buf, "PPSA01650"));
    assert(contains(buf, "NPXS40000"));
    printf("✓ empty sibling tables do not shadow the populated one\n");

    /* The name must be the name — the bug that motivated the byte-level
     * reader was a scan returning a run of concatenated columns. */
    assert(!contains(buf, "Bloodborne\\u"));
    assert(!contains(buf, "CUSA00900Bloodborne"));
    printf("✓ column values are separated, not concatenated\n");

    /* ── row API agrees with the JSON wrapper ────────────────────────── */
    content_db_app_t rows[16];
    const char *source = NULL;
    int n = content_db_apps(rows, 16, &source);
    assert(n == 3);
    assert(source && strcmp(source, "sqlite") == 0);
    assert(strcmp(rows[0].title_id, "CUSA00900") == 0);
    assert(rows[0].app_id == 101);
    printf("✓ row API returns %d rows from sqlite\n", n);

    /* ── one title looked up by id (#480: the tile's removal check) ──── */
    assert(content_db_has_title("NPXS40000") == 1);
    assert(content_db_has_title("PSUP00001") == 0);
    assert(content_db_has_title("bad'id") == -1);
    printf("✓ a title is found by a WHERE lookup, and a missing one is 0\n");

    /* ── a name longer than a row holds is cut on a character ────────── */
    {
        char longname[512];
        size_t at = 0;
        longname[at++] = 'A';
        for (int i = 0; i < 100; i++) { /* U+65E5, 3 bytes each: 301 bytes in all */
            longname[at++] = (char)0xE6;
            longname[at++] = (char)0x97;
            longname[at++] = (char)0xA5;
        }
        longname[at] = '\0';
        unlink(CONTENT_DB_APP);
        sqlite3 *db = NULL;
        assert(sqlite3_open(CONTENT_DB_APP, &db) == SQLITE_OK);
        exec_or_die(db, "CREATE TABLE tbl_info_1000000 (titleId TEXT, appId INTEGER, titleName TEXT);");
        sqlite3_stmt *st = NULL;
        assert(sqlite3_prepare_v2(db, "INSERT INTO tbl_info_1000000 VALUES ('PPSA04321', 7, ?)", -1,
                                  &st, NULL) == SQLITE_OK);
        sqlite3_bind_text(st, 1, longname, -1, SQLITE_STATIC);
        assert(sqlite3_step(st) == SQLITE_DONE);
        sqlite3_finalize(st);
        sqlite3_close(db);
        content_db_app_t one[2];
        assert(content_db_apps(one, 2, NULL) == 1);
        /* 255 bytes would end in two bytes of a character: the whole ones are kept. */
        assert(strlen(one[0].name) == 1 + 84 * 3);
        assert(memcmp(one[0].name, longname, strlen(one[0].name)) == 0);
        printf("✓ a long title name is cut between characters, never in one\n");
        make_app_db(CONTENT_DB_APP);
    }

    /* ── a database that is not a database falls back to the scanner ─── */
    {
        FILE *f = fopen(CONTENT_DB_APP, "wb");
        assert(f);
        fputs("this is not a sqlite database at all", f);
        fclose(f);
    }
    source = NULL;
    n = content_db_apps(rows, 16, &source);
    /* The scanner cannot parse it either, so this must report failure
     * rather than an empty-but-successful list — an empty list reads as
     * "nothing installed" to the install verifier. */
    assert(n < 0);
    printf("✓ unreadable app.db reports failure, not an empty title list\n");

    /* An unreadable app.db must answer in the envelope the engine's
     * AppDbList deserializes: `err` set, `apps` empty. The generic
     * {"ok":false,"error":...} shape decodes as "no error, zero titles",
     * and pkg_install's appdb_has_title() turns that into a confident
     * "the title is not installed" — failing verification for an install
     * that actually succeeded. */
    assert(content_db_apps_json(buf, sizeof(buf), &written) == 0);
    assert(contains(buf, "\"err\":"));
    assert(contains(buf, "\"apps\":[]"));
    assert(!contains(buf, "\"ok\":false"));
    printf("✓ unreadable app.db answers with err+apps, not the ok:false shape\n");

    /* ── appinfo.db reads ────────────────────────────────────────────── */
    make_appinfo_db(CONTENT_DB_APPINFO);
    assert(content_db_appinfo_json("PPSA01650", NULL, buf, sizeof(buf),
                                   &written) == 0);
    assert(contains(buf, "\"ok\":true"));
    assert(contains(buf, "\"key\":\"CONTENT_VERSION\""));
    assert(contains(buf, "\"val\":\"01.00\""));
    assert(contains(buf, "\"key\":\"VERSION_FILE_URI\""));
    assert(!contains(buf, "CUSA00900"));
    printf("✓ appinfo.db rows read back, scoped to the requested title\n");

    /* key filter */
    assert(content_db_appinfo_json("PPSA01650", "CONTENT_VERSION", buf,
                                   sizeof(buf), &written) == 0);
    assert(contains(buf, "CONTENT_VERSION"));
    assert(!contains(buf, "VERSION_FILE_URI"));
    printf("✓ key filter narrows the projection\n");

    /* a malformed title id is refused at the edge */
    assert(content_db_appinfo_json("nope", NULL, buf, sizeof(buf),
                                   &written) == 0);
    assert(contains(buf, "\"ok\":false"));
    printf("✓ malformed title id refused before it reaches a bind\n");

    /* one value, the install guard's `_org_path` read (#884) */
    {
        char v[128];
        assert(content_db_appinfo_get("PPSA03595", "_org_path", v, sizeof v) == 1);
        assert(strcmp(v, "/mnt/ext1/user/app/PPSA03595") == 0);
        assert(content_db_appinfo_get("PPSA01650", "_org_path", v, sizeof v) == 0 && v[0] == '\0');
        assert(content_db_appinfo_get("nope", "_org_path", v, sizeof v) == -1);
        char tiny[8];
        assert(content_db_appinfo_get("PPSA03595", "_org_path", tiny, sizeof tiny) == 1);
        assert(strlen(tiny) == sizeof tiny - 1);
        printf("✓ one appinfo value read by title and key\n");
    }

    /* any appinfo row for a title, and a backup copy of a database (#961) */
    {
        assert(content_db_appinfo_has_title("PPSA03595") == 1);
        assert(content_db_appinfo_has_title("PPSA99999") == 0);
        assert(content_db_appinfo_has_title("nope") == -1);
        char bk[512], e[160];
        snprintf(bk, sizeof bk, "%s.restore-backup", CONTENT_DB_APPINFO);
        unlink(bk);
        assert(content_db_backup_file(CONTENT_DB_APPINFO, bk, e, sizeof e) == 0);
        sqlite3 *b = NULL;
        assert(sqlite3_open_v2(bk, &b, SQLITE_OPEN_READONLY, NULL) == SQLITE_OK);
        sqlite3_stmt *q = NULL;
        assert(sqlite3_prepare_v2(b, "SELECT val FROM tbl_appinfo WHERE titleId='PPSA03595' AND key='_org_path'", -1, &q, NULL) == SQLITE_OK);
        assert(sqlite3_step(q) == SQLITE_ROW);
        assert(strcmp((const char *)sqlite3_column_text(q, 0), "/mnt/ext1/user/app/PPSA03595") == 0);
        sqlite3_finalize(q);
        sqlite3_close(b);
        /* never over an existing file, and a source that is not there leaves nothing behind */
        assert(content_db_backup_file(CONTENT_DB_APPINFO, bk, e, sizeof e) == -1 && contains(e, "already exists"));
        unlink(bk);
        assert(content_db_backup_file("/nonexistent/x.db", bk, e, sizeof e) == -1);
        assert(access(bk, F_OK) != 0);
        printf("✓ appinfo presence by title, and a database copied aside (never over a file)\n");
    }

    /* ── appinfo.db writes ───────────────────────────────────────────── */
    char err[256];

    g_fake_running = 1;
    assert(content_db_appinfo_set("PPSA01650", "CONTENT_VERSION", "99.999.999",
                                  err, sizeof(err)) != 0);
    assert(contains(err, "running"));
    printf("✓ refuses to edit a running title: %s\n", err);

    g_fake_running = 0;
    assert(content_db_appinfo_set("PPSA01650", "CONTENT_VERSION", "99.999.999",
                                  err, sizeof(err)) == 0);
    assert(content_db_appinfo_json("PPSA01650", "CONTENT_VERSION", buf,
                                   sizeof(buf), &written) == 0);
    assert(contains(buf, "99.999.999"));
    printf("✓ write lands and reads back\n");

    /* a key that does not exist must not silently succeed */
    assert(content_db_appinfo_set("PPSA01650", "NO_SUCH_KEY", "x", err,
                                  sizeof(err)) != 0);
    assert(contains(err, "no NO_SUCH_KEY row"));
    printf("✓ zero-row update reported as failure: %s\n", err);

    /* a title that does not exist likewise */
    assert(content_db_appinfo_set("CUSA99999", "CONTENT_VERSION", "x", err,
                                  sizeof(err)) != 0);
    printf("✓ unknown title refused\n");

    /* lowercase / punctuation keys are rejected before the statement runs */
    assert(content_db_appinfo_set("PPSA01650", "content_version", "x", err,
                                  sizeof(err)) != 0);
    assert(contains(err, "A-Z"));
    assert(content_db_appinfo_set("PPSA01650", "KEY';DROP TABLE tbl_appinfo;--",
                                  "x", err, sizeof(err)) != 0);
    /* and the table is still there */
    assert(content_db_appinfo_json("CUSA00900", NULL, buf, sizeof(buf),
                                   &written) == 0);
    assert(contains(buf, "\"ok\":true"));
    printf("✓ key charset guard holds; table intact\n");

    /* ── the callback SELECT that play time uses ─────────────────────── */
    {
        int seen = 0;
        int rows = content_db_select_text(
            CONTENT_DB_APPINFO,
            "SELECT key FROM tbl_appinfo WHERE titleId = 'CUSA00900'", 10,
            &seen, count_row);
        assert(rows == 1);
        assert(seen == 1);
        printf("✓ callback SELECT delivers rows\n");

        /* Read-only means read-only, even though the connection already is. */
        rows = content_db_select_text(CONTENT_DB_APPINFO,
                                      "DELETE FROM tbl_appinfo", 10, &seen,
                                      count_row);
        assert(rows < 0);
        assert(content_db_appinfo_json("CUSA00900", NULL, buf, sizeof(buf),
                                       &written) == 0);
        assert(contains(buf, "\"ok\":true"));
        printf("✓ non-SELECT refused; table intact\n");
    }


    /* ── app.db pagination (#473) ───────────────────────────────────── */
    {
        static char pbuf[64 * 1024];
        int late, pages, trunc;
        const int sizes[] = {255, 256, 257, 1000, 1500};
        for (size_t i = 0; i < sizeof sizes / sizeof *sizes; i++) {
            make_big_app_db(CONTENT_DB_APP, sizes[i], 1, 0);
            int got = page_all(pbuf, sizeof pbuf, 0, &late, &pages, &trunc);
            assert(got == sizes[i] + 1);
            assert(late);
            assert(!trunc);
            printf("✓ %d rows + a late-sorting title: all %d arrive in %d page(s)\n",
                   sizes[i], got, pages);
        }

        /* The no-argument call (what an older engine sends) is page one:
         * never a silent cap that looks complete. */
        make_big_app_db(CONTENT_DB_APP, 1000, 1, 0);
        assert(content_db_apps_json(pbuf, sizeof pbuf, &written) == 0);
        assert(json_num(pbuf, "offset") == 0);
        assert(json_true(pbuf, "more"));
        assert(!json_true(pbuf, "truncated"));
        assert(json_num(pbuf, "next") > 256);
        assert(json_num(pbuf, "total") == 1001);
        printf("✓ the unpaged call says more rows follow (next=%ld)\n",
               json_num(pbuf, "next"));

        /* An earlier-sorting row added later shifts the window but does
         * not push the late title out. */
        {
            sqlite3 *db = NULL;
            assert(sqlite3_open(CONTENT_DB_APP, &db) == SQLITE_OK);
            exec_or_die(db, "INSERT INTO tbl_info_1000000 VALUES"
                            " ('AAAA00001', 1, 'Early Title')");
            sqlite3_close(db);
            int seen = 0, saw_late = 0, saw_early = 0;
            unsigned off = 0;
            for (;;) {
                assert(content_db_apps_page_json(off, 0, pbuf, sizeof pbuf,
                                                 &written) == 0);
                const char *p = pbuf;
                int got = 0;
                while ((p = strstr(p, "\"title_id\":\"")) != NULL) {
                    p += 12;
                    if (!strncmp(p, "ZZZZ99999", 9)) saw_late = 1;
                    if (!strncmp(p, "AAAA00001", 9)) saw_early = 1;
                    got++;
                }
                seen += got;
                if (!json_true(pbuf, "more")) break;
                off += (unsigned)got;
            }
            assert(seen == 1002 && saw_late && saw_early);
            printf("✓ an earlier-sorting row does not push the late title out\n");
        }

        /* An explicit limit is honoured. */
        make_big_app_db(CONTENT_DB_APP, 300, 0, 0);
        assert(content_db_apps_page_json(100, 50, pbuf, sizeof pbuf,
                                         &written) == 0);
        assert(contains(pbuf, "\"title_id\":\"CUSA00100\""));
        assert(contains(pbuf, "\"title_id\":\"CUSA00149\""));
        assert(!contains(pbuf, "\"title_id\":\"CUSA00150\""));
        assert(json_num(pbuf, "next") == 150);
        assert(json_true(pbuf, "more"));
        assert(content_db_apps_page_json(300, 50, pbuf, sizeof pbuf,
                                         &written) == 0);
        assert(contains(pbuf, "\"apps\":[]"));
        assert(!json_true(pbuf, "more"));
        assert(json_num(pbuf, "next") == 300);
        printf("✓ offset/limit select a window; past the end is empty and final\n");

        /* A small buffer stops early and says where -- a continuation,
         * not a complete-looking list. */
        make_big_app_db(CONTENT_DB_APP, 300, 1, 1);
        {
            static char small[2048];
            assert(content_db_apps_page_json(0, 0, small, sizeof small,
                                             &written) == 0);
            long next = json_num(small, "next");
            assert(next > 0 && next < 300);
            assert(json_true(small, "more"));
            assert(!json_true(small, "truncated"));
            assert(contains(small, "Game \\\"0\\\" \\\\ \\t"));
            int got = page_all(small, sizeof small, 0, &late, &pages, &trunc);
            assert(got == 301 && late && !trunc);
            printf("✓ a 2 KiB buffer pages long escaped names: 301 rows in %d pages\n",
                   pages);
        }

        /* A buffer that cannot hold even one row reports truncation and
         * stops, rather than looping or claiming an empty success. */
        {
            static char tiny[160];
            assert(content_db_apps_page_json(0, 0, tiny, sizeof tiny,
                                             &written) == 0);
            assert(contains(tiny, "\"apps\":[]"));
            assert(json_true(tiny, "truncated"));
            assert(!json_true(tiny, "more"));
            printf("✓ a buffer too small for one row is reported as truncated\n");
        }

        /* The row API is no longer clipped at 256 either. */
        make_big_app_db(CONTENT_DB_APP, 600, 0, 0);
        {
            content_db_app_t *big =
                (content_db_app_t *)malloc(sizeof(content_db_app_t) * 700);
            assert(big);
            assert(content_db_apps(big, 700, NULL) == 600);
            assert(strcmp(big[599].title_id, "CUSA00599") == 0);
            free(big);
        }
        printf("✓ row API returns all 600 rows\n");

        /* A query error is still the err envelope, and says it is final. */
        {
            FILE *f = fopen(CONTENT_DB_APP, "wb");
            assert(f);
            fputs("not a database", f);
            fclose(f);
            assert(content_db_apps_page_json(0, 0, pbuf, sizeof pbuf,
                                             &written) == 0);
            assert(contains(pbuf, "\"err\":") && contains(pbuf, "\"apps\":[]"));
            assert(!json_true(pbuf, "more"));
            printf("✓ unreadable app.db on a paged call still answers err+apps\n");
        }
    }

    /* ── home-screen icon hide / unhide (#407) ─────────────────────── */
    {
        make_icon_db(CONTENT_DB_APP);
        unlink(CONTENT_DB_APP_BACKUP);
        assert(content_db_hidden_json(buf, sizeof(buf), &written) == 0);
        assert(contains(buf, "\"ok\":true") && contains(buf, "\"hidden\":[]"));
        printf("✓ nothing hidden on a fresh console (the system's own hidden apps are not listed)\n");

        assert(content_db_icon_set_visible("ITEM00001", 0, err, sizeof(err)) == 0);
        assert(read_visible("tbl_iconinfo_1000000", "ITEM00001") == 0);
        assert(read_visible("tbl_iconinfo_1000001", "ITEM00001") == 0);
        assert(read_visible("tbl_iconinfo_1000000", "PPSA01650") == 1);
        printf("✓ hide flips visible for that title in every user's table, nothing else\n");

        assert(access(CONTENT_DB_APP_BACKUP, F_OK) == 0);
        printf("✓ app.db backed up before the first change\n");

        assert(content_db_hidden_json(buf, sizeof(buf), &written) == 0);
        assert(contains(buf, "\"hidden\":[\"ITEM00001\"]"));
        printf("✓ hidden list reads back from app.db\n");

        assert(content_db_icon_set_visible("ITEM00001", 1, err, sizeof(err)) == 0);
        assert(read_visible("tbl_iconinfo_1000000", "ITEM00001") == 1);
        assert(read_visible("tbl_iconinfo_1000001", "ITEM00001") == 1);
        assert(content_db_hidden_json(buf, sizeof(buf), &written) == 0);
        assert(contains(buf, "\"hidden\":[]"));
        printf("✓ unhide restores it\n");

        assert(content_db_icon_set_visible("NPXS40000", 0, err, sizeof(err)) != 0);
        assert(contains(err, "system"));
        assert(read_visible("tbl_iconinfo_1000000", "NPXS40000") == 1);
        printf("✓ system apps are refused: %s\n", err);

        assert(content_db_icon_set_visible("CUSA99999", 0, err, sizeof(err)) != 0);
        printf("✓ unknown title refused: %s\n", err);
        assert(content_db_icon_set_visible("x'; DROP", 0, err, sizeof(err)) != 0);
        printf("✓ malformed title id refused\n");
        unlink(CONTENT_DB_APP_BACKUP);
    }

    /* ── #480: presence looks in every titleId table, however many users ── */
    {
        sqlite3 *db = NULL;
        char sql[512];
        content_db_presence_t p;
        unlink(CONTENT_DB_APP);
        assert(sqlite3_open(CONTENT_DB_APP, &db) == SQLITE_OK);
        exec_or_die(db, "CREATE TABLE tbl_contentinfo (titleId TEXT NOT NULL, titleName TEXT, metaDataPath TEXT,"
                        " PRIMARY KEY(titleId));");
        /* Six users: tbl_contentinfo + 3 titleId tables each = 19, past the old cap of 12. The
         * tile's icon is in the last user's table only (created last, sorted last). */
        for (int u = 0; u < 6; u++) {
            snprintf(sql, sizeof sql,
                     "CREATE TABLE tbl_addon_10%d (titleId TEXT); CREATE TABLE tbl_playlist_10%d (titleId TEXT);"
                     " CREATE TABLE tbl_iconinfo_10%d (titleId TEXT NOT NULL, titleName TEXT, visible INT,"
                     " PRIMARY KEY(titleId));",
                     u, u, u);
            exec_or_die(db, sql);
        }
        exec_or_die(db, "INSERT INTO tbl_iconinfo_105 VALUES ('PSUP00001', 'PS5Upload Minion', 1);");
        exec_or_die(db, "INSERT INTO tbl_iconinfo_100 VALUES ('CUSA00900', 'Bloodborne', 1);");
        exec_or_die(db, "INSERT INTO tbl_contentinfo VALUES ('CUSA00900', 'Bloodborne', '/user/app/CUSA00900/sce_sys');");
        sqlite3_close(db);

        assert(content_db_title_presence("PSUP00001", &p) == 0);
        assert(p.tables == 19);
        assert(p.any == 1 && p.content == 0 && p.icons == 1 && p.icons_visible == 1);
        assert(strcmp(p.where, "tbl_iconinfo_105(visible)") == 0);
        assert(content_db_has_title("PSUP00001") == 1);
        printf("✓ a row in the 19th titleId table is found (the old scan stopped at 12): %s\n", p.where);

        /* Hidden, and an install record: both reported, and the record is told apart. */
        char err[160] = {0};
        assert(content_db_icon_set_visible("PSUP00001", 0, err, sizeof err) == 0);
        assert(sqlite3_open(CONTENT_DB_APP, &db) == SQLITE_OK);
        exec_or_die(db, "INSERT INTO tbl_contentinfo VALUES ('PSUP00001', 'PS5Upload Minion', '/user/app/PSUP00001/sce_sys');");
        sqlite3_close(db);
        assert(content_db_title_presence("PSUP00001", &p) == 0);
        assert(p.any == 1 && p.content == 1 && p.icons == 1 && p.icons_visible == 0);
        assert(strcmp(p.where, "tbl_contentinfo,tbl_iconinfo_105") == 0);
        printf("✓ the install record and a hidden icon are told apart: %s\n", p.where);

        assert(content_db_title_presence("PPSA99999", &p) == 0);
        assert(p.any == 0 && p.content == 0 && p.where[0] == '\0');
        assert(content_db_has_title("PPSA99999") == 0);
        assert(content_db_title_presence("bad'id", &p) == -1);
        unlink(CONTENT_DB_APP_BACKUP);
        printf("✓ an absent title is 0 in every table; a bad id is refused\n");
    }

    unlink(CONTENT_DB_APP);
    unlink(CONTENT_DB_APPINFO);
    printf("\nAll content_db self-tests passed.\n");
    return 0;
}
