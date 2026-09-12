/*
 * Host self-test for the Game Activity wire format — activity.c's three
 * JSON producers, compiled for real rather than re-implemented.
 *
 * Two bugs motivated this file, and neither was reachable by the older
 * activity_launch_selftest.c, which copies the arithmetic instead of
 * building the source:
 *
 *   1. activity_get_json() emitted "last_played" and "active", but the
 *      engine's ActivityEntry declares last_launch_ts and session_active,
 *      every field #[serde(default)]. The mismatch did not fail to parse;
 *      it silently produced zeros, so the UI's "Last played" read "—" and
 *      the Active badge could never light up.
 *
 *   2. Only the recently-played query carried a title name, because it is
 *      the only one sourced from app.db. The tracked list comes from the
 *      process watcher and the play-time list from the system logger, and
 *      neither source has a name column -- so two of the three tabs showed
 *      bare title ids.
 *
 * Including the .c gives the test the file's statics (record_launch,
 * play_time_json) without widening the header for testing's sake.
 */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "sqlite3.h"

#include "../src/activity.c"

/* content_db.c refuses to edit a running title and asks this. No PS5
 * processes on the host, so nothing is ever running. */
int proc_find_pid_by_title_id(const char *title_id) {
    (void)title_id;
    return -1;
}

/* activity.c's process watcher is never started here (activity_init is
 * not called), but find_running_title() still references this. */
int sceKernelGetAppInfo(pid_t pid, app_info_t *info) {
    (void)pid;
    (void)info;
    return -1;
}

static int failures = 0;
static void check(int cond, const char *what) {
    if (cond) {
        printf("  ok: %s\n", what);
    } else {
        printf("  FAIL: %s\n", what);
        failures++;
    }
}

static void exec_or_die(sqlite3 *db, const char *sql) {
    char *err = NULL;
    if (sqlite3_exec(db, sql, NULL, NULL, &err) != SQLITE_OK) {
        fprintf(stderr, "fixture setup failed: %s\n", err ? err : "?");
        exit(1);
    }
}

/* Same shape as content_db_selftest's fixture: user-id-suffixed table,
 * a second empty user table, and an NPXS system app that the recently
 * played filter must drop. */
static void make_app_db(const char *path) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db, "CREATE TABLE tbl_info_1000000 ("
                    "  titleId TEXT, appId INTEGER, titleName TEXT);");
    exec_or_die(db, "CREATE TABLE tbl_info_1000001 ("
                    "  titleId TEXT, appId INTEGER, titleName TEXT);");
    exec_or_die(db, "INSERT INTO tbl_info_1000000 VALUES"
                    " ('CUSA00900', 101, 'Bloodborne'),"
                    " ('PPSA01650', 102, 'Astro''s Playroom'),"
                    " ('NPXS40000', 103, 'Media Gallery');");
    sqlite3_close(db);
}

/* The system logger keeps one JSON document per session in a TEXT
 * column, newest rowid last. totalFgTime is cumulative, not per-session. */
static void make_sl2_db(const char *path) {
    unlink(path);
    sqlite3 *db = NULL;
    assert(sqlite3_open(path, &db) == SQLITE_OK);
    exec_or_die(db, "CREATE TABLE tbl_log (event_id TEXT, log TEXT);");
    exec_or_die(db,
        "INSERT INTO tbl_log VALUES"
        " ('ApplicationSessionEndBi','{\"appTitleId\":\"CUSA00900\","
        "\"totalFgTime\":1200}'),"
        " ('ApplicationSessionEndBi','{\"appTitleId\":\"CUSA00900\","
        "\"totalFgTime\":3600}'),"
        " ('ApplicationSessionEndBi','{\"appTitleId\":\"CUSA99999\","
        "\"totalFgTime\":60}');");
    sqlite3_close(db);
}

int main(void) {
    make_app_db(CONTENT_DB_APP);
    make_sl2_db(ACTIVITY_SL2_DB);

    char buf[64 * 1024];
    size_t written = 0;

    /* ── 1. tracked list: the engine's field names, not the payload's ── */
    printf("tracked list wire format\n");
    record_launch("CUSA00900");
    assert(activity_get_json(buf, sizeof(buf), &written) == 0);

    check(strstr(buf, "\"last_launch_ts\":") != NULL,
          "emits last_launch_ts (engine reads this; \"last_played\" "
          "silently defaulted to 0)");
    check(strstr(buf, "\"session_active\":true") != NULL,
          "emits session_active (engine reads this; \"active\" silently "
          "defaulted to false)");
    check(strstr(buf, "\"last_seen_ts\":") != NULL,
          "emits last_seen_ts, which the client type declares");
    check(strstr(buf, "\"last_played\"") == NULL,
          "no longer emits the payload-only \"last_played\" spelling");

    /* ── 2. tracked list carries a name ─────────────────────────────── */
    printf("tracked list name join\n");
    check(strstr(buf, "\"name\":\"Bloodborne\"") != NULL,
          "joins the app.db name onto a tracked title");

    record_launch("CUSA11111");
    assert(activity_get_json(buf, sizeof(buf), &written) == 0);
    check(strstr(buf, "\"title_id\":\"CUSA11111\",\"name\"") == NULL,
          "a title absent from app.db gets no name key, rather than \"\"");

    /* ── 3. play time carries a name ────────────────────────────────── */
    printf("play-time name join\n");
    assert(activity_db_query_json("play_time", buf, sizeof(buf), &written) == 0);
    check(strstr(buf, "\"name\":\"Bloodborne\"") != NULL,
          "joins the app.db name onto a play-time row");
    check(strstr(buf, "\"total_seconds\":3600") != NULL,
          "keeps the newest cumulative total, not the older one");
    check(strstr(buf, "CUSA99999") != NULL,
          "a logged title missing from app.db is still listed");

    /* ── 4. recently played did not regress ─────────────────────────── */
    printf("recently played\n");
    assert(activity_db_query_json("recently_played", buf, sizeof(buf),
                                  &written) == 0);
    check(strstr(buf, "\"name\":\"Bloodborne\"") != NULL,
          "still names installed titles");
    check(strstr(buf, "NPXS40000") == NULL,
          "still drops Sony's own system apps");

    if (failures) {
        printf("\n%d check(s) failed\n", failures);
        return 1;
    }
    printf("\nall activity JSON checks passed\n");
    return 0;
}
