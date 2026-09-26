#ifndef PS5UPLOAD_CONTENT_DB_H
#define PS5UPLOAD_CONTENT_DB_H

#include <stddef.h>

/*
 * The console's content databases, read (and repaired) through a real
 * SQLite that the payload links statically.
 *
 * Background: no PS5 firmware ships libSceSqlite.sprx under any library
 * path, so every dlsym(RTLD_DEFAULT, "sqlite3_open_v2") in this codebase
 * resolved to NULL on every console. Two handlers carried a full SQL
 * implementation behind that probe and neither had ever executed; the
 * hand-rolled b-tree walker in appdb_scan.h was doing all the work.
 *
 * That was the right fix for reading title ids, and it stays as the
 * fallback here. What it cannot do is (a) reassemble a row whose columns
 * are serialized blobs — which is why play-time queries were declared
 * impossible — or (b) write. Repairing a console that lists a title it
 * refuses to delete means writing.
 *
 * "Sony does not ship SQLite" is a fact about the platform, not about
 * what we can link. See payload/Makefile for the build flags.
 *
 * Everything here is bounded and emits JSON into a caller-owned buffer.
 */

/* The two databases. app.db drives the home-screen tiles; appinfo.db
 * drives Settings -> Storage. They can disagree with each other and with
 * what is actually on disk, which is the usual shape of a broken title. */
/* Overridable so the host selftest can point them at fixtures; the
 * payload always builds with the real console paths. */
#ifndef CONTENT_DB_APP
#define CONTENT_DB_APP     "/system_data/priv/mms/app.db"
#endif
#ifndef CONTENT_DB_APPINFO
#define CONTENT_DB_APPINFO "/system_data/priv/mms/appinfo.db"
#endif

/* One row of app.db's title table. */
typedef struct {
    char title_id[16];
    char name[256];
    int  app_id;
} content_db_app_t;

/*
 * Installed titles as rows, so each caller can shape its own response
 * (the app list and the "recently played" list want different envelopes
 * and different filters).
 *
 * `source` (optional) receives a static string naming what answered:
 * "sqlite", "sqlite-immutable" or "scan".
 *
 * Returns the row count (>= 0), or -1 if nothing could be read at all.
 */
int content_db_apps(content_db_app_t *out, int max, const char **source);

/*
 * Run a read-only SELECT and hand each row to `row_cb` as an array of
 * NUL-terminated column strings (NULL for SQL NULL). Return non-zero from
 * the callback to stop early.
 *
 * This exists for queries whose rows need bespoke parsing rather than a
 * generic JSON projection -- play time, for instance, lives as a JSON
 * document inside a single column and has to be picked apart per row.
 *
 * Returns the number of rows delivered, or -1 if the query could not run.
 */
int content_db_select_text(const char *db_path, const char *sql, int max_rows,
                           void *ctx,
                           int (*row_cb)(void *ctx, int ncol,
                                         const char *const *vals));

/*
 * Installed titles from app.db: title id, app id and display name.
 *
 * Emits {"apps":[{"title_id":"..","app_id":N,"name":".."}, ...],
 *        "source":"sqlite"|"scan"}
 *
 * `source` is reported rather than hidden: a caller that sees "scan"
 * knows the SQL open failed and the row set came from the byte-level
 * reader, which recovers ids reliably but names only sometimes.
 *
 * Never fails outright — falls back to appdb_scan.h. Returns 0 on
 * success, non-zero only if `buf` is too small to hold even an error.
 */
int content_db_apps_json(char *buf, size_t cap, size_t *written);

/*
 * Key/value rows from appinfo.db for one title.
 *
 * appinfo.db stores per-title metadata as (titleId, key, val) triples --
 * CONTENT_VERSION, VERSION_FILE_URI and friends. Emits
 * {"ok":true,"title_id":"..","rows":[{"key":"..","val":".."}, ...]}
 *
 * Pass a NULL or empty `keys` for every key, or a comma-separated list
 * to filter. There is no b-tree fallback: a key/value projection cannot
 * be recovered from a byte scan, so an open failure is reported as one.
 */
int content_db_appinfo_json(const char *title_id, const char *keys,
                            char *buf, size_t cap, size_t *written);

/*
 * Set one appinfo.db value. This writes to a live system database.
 *
 * Guarded, because getting it wrong bricks the title's Settings entry:
 *   - the title must not be running (a running app holds the row);
 *   - the row must already exist -- this updates, it never inserts, so a
 *     typo in `key` changes nothing instead of adding a key the shell
 *     will never read;
 *   - the write runs inside BEGIN IMMEDIATE and is rolled back unless
 *     exactly one row changed.
 *
 * Callers are expected to have snapshotted both databases first
 * (fs_ops::backup_content_databases on the engine side).
 *
 * Returns 0 on success. On failure returns non-zero and writes a short
 * human-readable reason into `err`.
 */
int content_db_appinfo_set(const char *title_id, const char *key,
                           const char *val, char *err, size_t errcap);

#endif /* PS5UPLOAD_CONTENT_DB_H */
