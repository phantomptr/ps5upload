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
        " ('CUSA00900','CONTENT_VERSION','01.09');");
    sqlite3_close(db);
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

    unlink(CONTENT_DB_APP);
    unlink(CONTENT_DB_APPINFO);
    printf("\nAll content_db self-tests passed.\n");
    return 0;
}
