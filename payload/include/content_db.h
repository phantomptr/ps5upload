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
/* A copy of app.db taken before the first home-screen icon change (#407),
 * kept so a hide can always be undone by hand. Only the first is kept. */
#ifndef CONTENT_DB_APP_BACKUP
#define CONTENT_DB_APP_BACKUP "/data/ps5upload/app.db.before-hide"
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
 * Where app.db mentions `title_id`, over EVERY table with a titleId column
 * (no cap: a console has tbl_contentinfo plus three such tables per user, so
 * a capped scan missed rows on a console with four or more users, #480).
 *   tables         titleId tables looked in
 *   any            1 when any of them has a row
 *   content        Sony's install record: rows in a table that also has a
 *                  metaDataPath column (tbl_contentinfo), 1 / 0; -1 when
 *                  app.db has no such table
 *   icons          rows in icon tables (titleId + visible: tbl_iconinfo_<user>)
 *   icons_visible  of those, how many have visible != 0 (drawn on the home screen)
 *   where          "tbl_contentinfo,tbl_iconinfo_<user>(visible)" for logs
 * Returns 0, or -1 when app.db could not be read or has no titleId table.
 */
typedef struct {
    int tables;
    int any;
    int content;
    int icons;
    int icons_visible;
    char where[320];
} content_db_presence_t;
int content_db_title_presence(const char *title_id, content_db_presence_t *out);

/*
 * Whether any app.db titleId table has a row for `title_id`
 * (content_db_title_presence's `any`). Returns 1 / 0, or -1 when app.db could
 * not be read (or has no such table).
 */
int content_db_has_title(const char *title_id);

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
 * Installed titles from app.db, one page at a time (#473): title id, app
 * id and display name for rows [offset, offset + limit) in title order.
 *
 * Emits {"apps":[{"title_id":"..","app_id":N,"name":".."}, ...],
 *        "source":"sqlite"|"sqlite-immutable"|"scan",
 *        "offset":O,"next":N,"more":bool,"truncated":bool,"total":T}
 *
 * `total` is the table's row count, sent only from SQL (the scanner's is a
 * lower bound); a caller that sees it change between pages knows rows
 * moved under it.
 * `next` is the row the following page starts at; `more` is true while
 * rows remain after it. A page that fills `buf` before `limit` rows stops
 * early with `more` set, so a small buffer pages rather than silently
 * dropping the rest. `truncated` is true only when rows exist that no
 * page can deliver: one row larger than `buf`, a SQL walk that stopped
 * before the end (SQLITE_BUSY and the like), or the byte scanner (the
 * fallback for a database that will not open) hitting its own bounds.
 *
 * `limit` 0 (or above CONTENT_DB_APPS_PAGE_MAX) means
 * CONTENT_DB_APPS_PAGE_MAX. An older engine sends no window at all and
 * reads page one; the extra fields are ignored by its decoder.
 *
 * `source` is reported rather than hidden: a caller that sees "scan"
 * knows the SQL open failed and the row set came from the byte-level
 * reader, which recovers ids reliably but names only sometimes.
 *
 * Never fails outright — falls back to appdb_scan.h, and an unreadable
 * database is {"err":"..","apps":[]}. Returns 0 on success, non-zero only
 * if `buf` is too small to hold even an error.
 */
#define CONTENT_DB_APPS_PAGE_MAX 512
int content_db_apps_page_json(unsigned offset, unsigned limit, char *buf,
                              size_t cap, size_t *written);

/* Page one: content_db_apps_page_json(0, 0, ...). */
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
 * One appinfo.db value for one title (e.g. `_org_path`, where the console
 * recorded the title's install). Read-only. 1 found (`out` holds it, cut to
 * fit), 0 no such row, -1 appinfo.db could not be read or the id is not a
 * title id.
 */
int content_db_appinfo_get(const char *title_id, const char *key, char *out, size_t cap);

/*
 * Whether appinfo.db has any row for `title_id` (Settings > Storage's record of the title).
 * Read-only. 1 / 0, or -1 when appinfo.db could not be read or the id is not a title id.
 */
int content_db_appinfo_has_title(const char *title_id);

/*
 * A consistent copy of the database at `src_path` into a NEW file `dst_path`, through SQLite's
 * backup API (safe while the shell has the database open; nothing is written to the source).
 * `dst_path` must not exist yet: an existing file is never replaced. 0, or -1 with a short reason
 * in `err` (a half-written copy is removed).
 */
int content_db_backup_file(const char *src_path, const char *dst_path, char *err, size_t errcap);

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

/*
 * Hide or show a title's home-screen icon (#407): sets `visible` in every
 * user's icon table in app.db (tables found by shape: a titleId and a
 * visible column; tbl_iconinfo_<userid> on the console). Nothing is
 * deleted; the title stays installed and runs as before.
 *
 * Guarded like content_db_appinfo_set: the id must be a title id, system
 * apps (NPXS...) are refused, app.db is backed up once before the first
 * change (CONTENT_DB_APP_BACKUP), the write runs inside BEGIN IMMEDIATE,
 * and it is rolled back unless at least one row and at most one row per
 * table changed.
 *
 * Returns 0 on success, else non-zero with a short reason in `err`.
 */
int content_db_icon_set_visible(const char *title_id, int visible, char *err,
                                size_t errcap);

/*
 * The titles whose icon is hidden in any user's icon table, leaving out the
 * system's own apps (NPXS...), which the system hides the same way:
 * {"ok":true,"hidden":["ITEM00001",...]} (sorted, no duplicates), or
 * {"ok":false,"error":".."}. Read-only.
 */
int content_db_hidden_json(char *buf, size_t cap, size_t *written);

/*
 * Hide again whichever of `ids` (`n` title ids) the system made visible: one transaction, and
 * only rows with visible != 0 are written, so a pass with nothing to undo leaves app.db as it
 * was. Returns how many icons it hid again, or -1 with a short reason in `err` (#450).
 */
int content_db_icons_rehide(const char *const *ids, int n, char *err, size_t errcap);

#endif /* PS5UPLOAD_CONTENT_DB_H */
