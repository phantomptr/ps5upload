#include "content_db.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#include "appdb_scan.h"
#include "proc_list.h"
#include "sqlite3.h"
#include "utf8_cut.h"

/* ── small helpers ───────────────────────────────────────────────────── */

/* Escape into a fixed buffer, always NUL-terminated. Control bytes become
 * \uXXXX because the engine parses these frames with serde and one raw
 * control character rejects the whole response, not just the field. */
static void cdb_json_escape(const char *in, char *out, size_t cap) {
    size_t o = 0;
    if (cap == 0) return;
    for (const unsigned char *p = (const unsigned char *)(in ? in : "");
         *p && o + 8 < cap; p++) {
        switch (*p) {
        case '"':  if (o + 2 < cap) { out[o++] = '\\'; out[o++] = '"';  } break;
        case '\\': if (o + 2 < cap) { out[o++] = '\\'; out[o++] = '\\'; } break;
        case '\n': if (o + 2 < cap) { out[o++] = '\\'; out[o++] = 'n';  } break;
        case '\r': if (o + 2 < cap) { out[o++] = '\\'; out[o++] = 'r';  } break;
        case '\t': if (o + 2 < cap) { out[o++] = '\\'; out[o++] = 't';  } break;
        default:
            if (*p < 0x20) {
                o += (size_t)snprintf(out + o, cap - o, "\\u%04x", *p);
            } else {
                out[o++] = (char)*p;
            }
        }
    }
    out[o] = '\0';
}

/* A title id is exactly 4 letters + 5 digits (CUSA12345 / PPSA01650).
 * Every caller-supplied id is checked before it reaches a bind, so a
 * malformed one is rejected at the edge rather than relied on being
 * harmless because it is parameterised. */
static int cdb_valid_title_id(const char *s) {
    if (!s) return 0;
    size_t n = strlen(s);
    if (n != 9) return 0;
    for (int i = 0; i < 4; i++)
        if (!((s[i] >= 'A' && s[i] <= 'Z') || (s[i] >= 'a' && s[i] <= 'z')))
            return 0;
    for (int i = 4; i < 9; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    return 1;
}

/*
 * Error envelope for the app-list response specifically.
 *
 * It must carry `err` and an empty `apps`, because that is the shape the
 * engine's AppDbList deserializes. The generic {"ok":false,"error":...}
 * form below would decode as "no error, zero titles" — serde fills both
 * missing fields with defaults — and pkg_install's appdb_has_title() would
 * turn that into a confident Some(false): "the title is not installed".
 * An unreadable database must read as "cannot tell" (None), never as a
 * failed install.
 */
static int cdb_emit_apps_err(char *buf, size_t cap, size_t *written,
                             const char *reason) {
    char esc[256];
    cdb_json_escape(reason, esc, sizeof(esc));
    int n = snprintf(buf, cap, "{\"err\":\"%s\",\"apps\":[]}", esc);
    if (n < 0 || (size_t)n >= cap) return -1;
    if (written) *written = (size_t)n;
    return 0;
}

static int cdb_emit_err(char *buf, size_t cap, size_t *written,
                        const char *reason) {
    char esc[256];
    cdb_json_escape(reason, esc, sizeof(esc));
    int n = snprintf(buf, cap, "{\"ok\":false,\"error\":\"%s\"}", esc);
    if (n < 0 || (size_t)n >= cap) return -1;
    if (written) *written = (size_t)n;
    return 0;
}

/*
 * Read-only open, in two tiers.
 *
 * Tier 1 is a normal read-only connection. It is the one that gives a
 * consistent view, and it is what we want whenever the shell is not
 * mid-write.
 *
 * Tier 2 adds immutable=1, which tells SQLite the file cannot change and
 * to skip locking entirely. That is the escape hatch for the case that
 * actually happens on a console: SceShellUI holds the database and a
 * normal open comes back SQLITE_BUSY. immutable=1 can read a torn page if
 * the shell writes underneath us -- but the byte-level fallback this
 * replaces had exactly the same exposure and no way to notice, so this is
 * strictly better rather than a new risk.
 *
 * `how` (optional) receives "sqlite" or "sqlite-immutable" for reporting.
 */
static int cdb_open_ro(const char *path, sqlite3 **out, const char **how) {
    if (out) *out = NULL;

    /* Fail fast on a missing file: sqlite3_open_v2 on a nonexistent path
     * in read-only mode reports SQLITE_CANTOPEN, which is indistinguishable
     * from a permissions problem in the message we surface. */
    if (access(path, R_OK) != 0) return SQLITE_CANTOPEN;

    sqlite3 *db = NULL;
    int rc = sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY, NULL);
    if (rc == SQLITE_OK && db) {
        sqlite3_busy_timeout(db, 2000);
        /* Force a real read: open succeeds lazily, so a locked or
         * not-a-database file only reveals itself on first page access. */
        sqlite3_stmt *probe = NULL;
        if (sqlite3_prepare_v2(db, "SELECT 1 FROM sqlite_master LIMIT 1", -1,
                               &probe, NULL) == SQLITE_OK) {
            int step = sqlite3_step(probe);
            sqlite3_finalize(probe);
            if (step == SQLITE_ROW || step == SQLITE_DONE) {
                if (how) *how = "sqlite";
                *out = db;
                return SQLITE_OK;
            }
        }
    }
    if (db) sqlite3_close(db);

    char uri[640];
    int n = snprintf(uri, sizeof(uri), "file:%s?immutable=1", path);
    if (n < 0 || (size_t)n >= sizeof(uri)) return SQLITE_CANTOPEN;

    db = NULL;
    rc = sqlite3_open_v2(uri, &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_URI,
                         NULL);
    if (rc != SQLITE_OK || !db) {
        if (db) sqlite3_close(db);
        return rc == SQLITE_OK ? SQLITE_CANTOPEN : rc;
    }
    if (how) *how = "sqlite-immutable";
    *out = db;
    return SQLITE_OK;
}

/* ── app.db ──────────────────────────────────────────────────────────── */

/*
 * Resolve app.db's title table and its columns at runtime.
 *
 * Two reasons not to hardcode a name. The dead SQL paths this module
 * replaces used *different* ones -- tbl_appbrowse_2_appinfo in one,
 * tbl_appbrowser_2_appinfo in the other -- and neither had ever run, so
 * the disagreement was never noticed. And both were wrong: a real console
 * (checked on FW 9.60 and 5.10) keys these tables by user id, so the
 * names look like tbl_info_<userid>, which no fixed string matches and no
 * "tbl_app%" prefix filter finds either.
 *
 * So the table is identified by shape rather than by name: any table with
 * a titleId column is a candidate, ranked by whether it also carries a
 * display name and an app id. Candidates are then tried in order and the
 * first that actually returns rows wins, because a console with several
 * users has several of these tables and the empty ones are not useful.
 */
typedef struct {
    char table[64];
    char col_title[64];
    char col_name[64];  /* empty if the table has no name column */
    char col_appid[64]; /* empty if the table has no app id column */
    int  score;
} cdb_app_schema_t;

#define CDB_MAX_SCHEMAS 12

static int cdb_has_column(sqlite3 *db, const char *table, const char *want,
                          char *out, size_t outcap) {
    char sql[192];
    if (snprintf(sql, sizeof(sql), "PRAGMA table_info(\"%s\")", table) < 0)
        return 0;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) return 0;
    int found = 0;
    while (sqlite3_step(st) == SQLITE_ROW) {
        const unsigned char *col = sqlite3_column_text(st, 1);
        if (col && strcasecmp((const char *)col, want) == 0) {
            snprintf(out, outcap, "%s", (const char *)col);
            found = 1;
            break;
        }
    }
    sqlite3_finalize(st);
    return found;
}

/* Collect candidate tables, best-scoring first. Returns how many. */
static int cdb_discover_app_schemas(sqlite3 *db, cdb_app_schema_t *out,
                                    int max) {
    static const char *name_cols[] = {"appName",    "titleName", "name",
                                      "title_name", "canonicalTitle"};
    static const char *appid_cols[] = {"appId", "app_id"};

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db,
                           "SELECT name FROM sqlite_master "
                           "WHERE type = 'table'",
                           -1, &st, NULL) != SQLITE_OK)
        return 0;

    int n = 0;
    while (n < max && sqlite3_step(st) == SQLITE_ROW) {
        const unsigned char *tbl = sqlite3_column_text(st, 0);
        if (!tbl) continue;

        cdb_app_schema_t c;
        memset(&c, 0, sizeof(c));
        snprintf(c.table, sizeof(c.table), "%s", (const char *)tbl);
        if (!cdb_has_column(db, c.table, "titleId", c.col_title,
                            sizeof(c.col_title)))
            continue;

        c.score = 1;
        for (size_t i = 0; i < sizeof(name_cols) / sizeof(*name_cols); i++)
            if (cdb_has_column(db, c.table, name_cols[i], c.col_name,
                               sizeof(c.col_name))) {
                c.score += 2; /* a display name is the point of the query */
                break;
            }
        for (size_t i = 0; i < sizeof(appid_cols) / sizeof(*appid_cols); i++)
            if (cdb_has_column(db, c.table, appid_cols[i], c.col_appid,
                               sizeof(c.col_appid))) {
                c.score += 1;
                break;
            }
        out[n++] = c;
    }
    sqlite3_finalize(st);

    /* Insertion sort, descending by score. n is at most CDB_MAX_SCHEMAS. */
    for (int i = 1; i < n; i++) {
        cdb_app_schema_t key = out[i];
        int j = i - 1;
        while (j >= 0 && out[j].score < key.score) {
            out[j + 1] = out[j];
            j--;
        }
        out[j + 1] = key;
    }
    return n;
}

/* Build the SELECT for one candidate. Identifiers come from sqlite_master
 * and PRAGMA table_info, never from a caller, so interpolating them here
 * cannot be steered. */
static void cdb_build_app_sql(const cdb_app_schema_t *sc, char *sql,
                              size_t cap) {
    snprintf(sql, cap,
             "SELECT \"%s\", %s%s%s, %s%s%s FROM \"%s\" "
             "WHERE \"%s\" IS NOT NULL ORDER BY \"%s\"",
             sc->col_title,
             sc->col_appid[0] ? "\"" : "", sc->col_appid[0] ? sc->col_appid : "0",
             sc->col_appid[0] ? "\"" : "",
             sc->col_name[0] ? "\"" : "'", sc->col_name[0] ? sc->col_name : "",
             sc->col_name[0] ? "\"" : "'",
             sc->table, sc->col_title, sc->col_title);
}

/* The byte scanner's bounds. It reads at most CDB_SCAN_RAW_MAX of the file
 * and recovers at most CDB_SCAN_ROWS_MAX titles; a database past either is
 * reported as truncated, never as a complete list. */
#define CDB_SCAN_RAW_MAX (1024u * 1024u)
#define CDB_SCAN_ROWS_MAX 4096

static void cdb_fill_row(content_db_app_t *r, const char *tid,
                         const char *name, int app_id) {
    memset(r, 0, sizeof(*r));
    snprintf(r->title_id, sizeof(r->title_id), "%s", tid);
    snprintf(r->name, sizeof(r->name), "%s", name ? name : "");
    /* A name longer than the row holds (about 86 CJK characters) is cut on a character, never
     * in one: half a character is invalid UTF-8, and the engine then refused the whole page of
     * the list, so every title only app.db knew of went missing from the installed list. */
    r->name[utf8_whole_prefix(r->name, strlen(r->name))] = '\0';
    r->app_id = app_id;
}

/*
 * Row-level app list, windowed: rows [offset, offset + max) of the title
 * table, in title order, go to `out`. SQL first, byte scan if the database
 * will not open. content_db_apps(), the paged JSON and activity.c's
 * "recently played" all build on this, so there is one schema-discovery
 * implementation, not two.
 *
 * `total` (optional) receives how many rows the table holds -- with the
 * scanner, a lower bound that is at least offset + max + 1 when more rows
 * follow the window. `capped` (optional) is set when rows exist that the
 * reader could not reach (the scanner's bounds).
 *
 * Returns the number of rows written to `out`, or -1 if nothing could be
 * read at all.
 */
static int cdb_apps_window(content_db_app_t *out, unsigned offset, int max,
                           unsigned *total, int *capped, const char **source) {
    if (total) *total = 0;
    if (capped) *capped = 0;
    if (!out || max <= 0) return -1;
    if (source) *source = "scan";

    sqlite3 *db = NULL;
    const char *how = "sqlite";
    unsigned rows_seen = 0;
    int count = 0;

    if (cdb_open_ro(CONTENT_DB_APP, &db, &how) == SQLITE_OK && db) {
        cdb_app_schema_t cands[CDB_MAX_SCHEMAS];
        int ncand = cdb_discover_app_schemas(db, cands, CDB_MAX_SCHEMAS);

        /* First candidate that actually yields rows wins: a console with
         * several users has several of these tables, and the ones
         * belonging to other users are empty. */
        for (int ci = 0; ci < ncand && rows_seen == 0; ci++) {
            char sql[512];
            cdb_build_app_sql(&cands[ci], sql, sizeof(sql));

            sqlite3_stmt *st = NULL;
            if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK)
                continue;
            /* Every row is counted (so the caller knows whether more
             * follow); only the window is copied. */
            int step;
            while ((step = sqlite3_step(st)) == SQLITE_ROW) {
                const unsigned char *tid = sqlite3_column_text(st, 0);
                if (!tid) continue;
                unsigned idx = rows_seen++;
                if (idx < offset || count >= max) continue;
                const unsigned char *nm = sqlite3_column_text(st, 2);
                cdb_fill_row(&out[count], (const char *)tid,
                             (const char *)nm, sqlite3_column_int(st, 1));
                count++;
            }
            /* A walk that ended on anything but DONE (BUSY under the
             * shell's write, a damaged page) did not see every row. */
            if (rows_seen > 0 && step != SQLITE_DONE && capped) *capped = 1;
            sqlite3_finalize(st);
        }
        sqlite3_close(db);
    }

    /* Zero rows from SQL means the schema guess missed, not that the
     * console has no games -- and install verification reads an empty list
     * as a failed install. Fall through to the scan rather than assert a
     * confident zero. */
    if (rows_seen > 0) {
        if (source) *source = how;
        if (total) *total = rows_seen;
        return count;
    }

    int fd = open(CONTENT_DB_APP, O_RDONLY);
    if (fd < 0) return -1;
    unsigned char *raw = (unsigned char *)malloc(CDB_SCAN_RAW_MAX);
    if (!raw) {
        close(fd);
        return -1;
    }
    size_t got = 0;
    while (got < CDB_SCAN_RAW_MAX) {
        ssize_t r = read(fd, raw + got, CDB_SCAN_RAW_MAX - got);
        if (r <= 0) break;
        got += (size_t)r;
    }
    /* A file larger than the read window has rows the scan never sees. */
    int raw_capped = 0;
    if (got == CDB_SCAN_RAW_MAX) {
        unsigned char probe;
        raw_capped = read(fd, &probe, 1) > 0;
    }
    close(fd);

    /* One past the window, so "more rows follow" is known without
     * holding every row. */
    unsigned long want = (unsigned long)offset + (unsigned long)max + 1ul;
    int scan_max = want > CDB_SCAN_ROWS_MAX ? CDB_SCAN_ROWS_MAX : (int)want;
    appdb_entry_t *entries =
        (appdb_entry_t *)malloc(sizeof(appdb_entry_t) * (size_t)scan_max);
    if (!entries) {
        free(raw);
        return -1;
    }
    int scanned = appdb_scan_entries_ex(raw, got, entries, scan_max, 1);
    free(raw);
    if (scanned < 0) {
        free(entries);
        return -1;
    }
    for (int i = (int)(offset < (unsigned)scanned ? offset : (unsigned)scanned);
         i < scanned && count < max; i++) {
        cdb_fill_row(&out[count], entries[i].title_id, entries[i].name, 0);
        count++;
    }
    free(entries);
    if (capped)
        *capped = raw_capped || (scanned == CDB_SCAN_ROWS_MAX &&
                                 scan_max == CDB_SCAN_ROWS_MAX);
    if (total) *total = (unsigned)scanned;
    if (source) *source = "scan";
    return count;
}

int content_db_apps(content_db_app_t *out, int max, const char **source) {
    return cdb_apps_window(out, 0, max, NULL, NULL, source);
}

/* Appends `name` (and "(visible)" when `visible`) to `where`, comma separated, while it fits. */
static void cdb_where_add(char *where, size_t cap, const char *name, int visible) {
    size_t at = strlen(where);
    int k = snprintf(where + at, cap - at, "%s%s%s", at ? "," : "", name, visible ? "(visible)" : "");
    if (k < 0 || (size_t)k >= cap - at) where[at] = '\0'; /* keep whole entries only */
}

int content_db_title_presence(const char *title_id, content_db_presence_t *out) {
    sqlite3 *db = NULL;
    sqlite3_stmt *tables = NULL;
    if (!out) return -1;
    memset(out, 0, sizeof *out);
    out->content = -1;
    if (!cdb_valid_title_id(title_id)) return -1;
    if (cdb_open_ro(CONTENT_DB_APP, &db, NULL) != SQLITE_OK || !db) return -1;
    if (sqlite3_prepare_v2(db, "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name", -1, &tables,
                           NULL) != SQLITE_OK) {
        sqlite3_close(db);
        return -1;
    }
    while (sqlite3_step(tables) == SQLITE_ROW) {
        const unsigned char *t = sqlite3_column_text(tables, 0);
        char name[96], col[64], meta[64], vis[64], sql[256];
        sqlite3_stmt *st = NULL;
        if (!t || strlen((const char *)t) >= sizeof name || strchr((const char *)t, '"')) continue;
        snprintf(name, sizeof name, "%s", (const char *)t);
        if (!cdb_has_column(db, name, "titleId", col, sizeof col)) continue;
        int is_content = cdb_has_column(db, name, "metaDataPath", meta, sizeof meta);
        int is_icon = cdb_has_column(db, name, "visible", vis, sizeof vis);
        out->tables++;
        if (is_content && out->content < 0) out->content = 0;
        /* Identifiers come from sqlite_master and PRAGMA table_info; the id is bound. */
        if (is_icon)
            snprintf(sql, sizeof sql, "SELECT COUNT(*), COALESCE(SUM(\"%s\" <> 0), 0) FROM \"%s\" WHERE \"%s\" = ?",
                     vis, name, col);
        else
            snprintf(sql, sizeof sql, "SELECT COUNT(*), 0 FROM \"%s\" WHERE \"%s\" = ?", name, col);
        if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) continue;
        sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
        if (sqlite3_step(st) == SQLITE_ROW) {
            int rows = sqlite3_column_int(st, 0), shown = sqlite3_column_int(st, 1);
            if (rows > 0) {
                out->any = 1;
                if (is_content) out->content = 1;
                if (is_icon) {
                    out->icons += rows;
                    out->icons_visible += shown;
                }
                cdb_where_add(out->where, sizeof out->where, name, is_icon && shown > 0);
            }
        }
        sqlite3_finalize(st);
    }
    sqlite3_finalize(tables);
    sqlite3_close(db);
    return out->tables > 0 ? 0 : -1;
}

int content_db_has_title(const char *title_id) {
    content_db_presence_t p;
    if (content_db_title_presence(title_id, &p) != 0) return -1;
    return p.any;
}

int content_db_select_text(const char *db_path, const char *sql, int max_rows,
                           void *ctx,
                           int (*row_cb)(void *ctx, int ncol,
                                         const char *const *vals)) {
    if (!db_path || !sql || !row_cb) return -1;
    if (max_rows <= 0) max_rows = 100;

    const char *p = sql;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (strncasecmp(p, "SELECT", 6) != 0 && strncasecmp(p, "PRAGMA", 6) != 0)
        return -1;

    sqlite3 *db = NULL;
    if (cdb_open_ro(db_path, &db, NULL) != SQLITE_OK || !db) return -1;

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) {
        sqlite3_close(db);
        return -1;
    }

    int rows = 0;
    while (rows < max_rows && sqlite3_step(st) == SQLITE_ROW) {
        int ncol = sqlite3_column_count(st);
        if (ncol > 16) ncol = 16;
        const char *vals[16];
        for (int c = 0; c < ncol; c++)
            vals[c] = (const char *)sqlite3_column_text(st, c);
        rows++;
        if (row_cb(ctx, ncol, vals) != 0) break;
    }
    sqlite3_finalize(st);
    sqlite3_close(db);
    return rows;
}

/* The longest tail a page can end with:
 * ],"source":"sqlite-immutable","offset":4294967295,"next":4294967295,
 * "more":false,"truncated":false} -- about 110 bytes. */
#define CDB_PAGE_TAIL_RESERVE 160

int content_db_apps_page_json(unsigned offset, unsigned limit, char *buf,
                              size_t cap, size_t *written) {
    if (!buf || cap < 64) return -1;
    if (limit == 0 || limit > CONTENT_DB_APPS_PAGE_MAX)
        limit = CONTENT_DB_APPS_PAGE_MAX;

    content_db_app_t *rows =
        (content_db_app_t *)malloc(sizeof(content_db_app_t) * (size_t)limit);
    if (!rows) return cdb_emit_apps_err(buf, cap, written, "out of memory");

    const char *source = "scan";
    unsigned total = 0;
    int capped = 0;
    int count = cdb_apps_window(rows, offset, (int)limit, &total, &capped,
                                &source);
    if (count < 0) {
        free(rows);
        return cdb_emit_apps_err(buf, cap, written, "cannot read app.db");
    }

    int n = snprintf(buf, cap, "{\"apps\":[");
    if (n < 0 || (size_t)n >= cap) {
        free(rows);
        return -1;
    }
    int emitted = 0;
    for (int i = 0; i < count; i++) {
        char tid[32], name[512];
        cdb_json_escape(rows[i].title_id, tid, sizeof(tid));
        cdb_json_escape(rows[i].name, name, sizeof(name));
        int more = snprintf(buf + n, cap - (size_t)n,
                            "%s{\"title_id\":\"%s\",\"app_id\":%d,"
                            "\"name\":\"%s\"}",
                            emitted ? "," : "", tid, rows[i].app_id, name);
        /* Full: stop here and let `next` say where the following page
         * starts. This used to close the JSON as if it were the whole
         * list (#473). */
        if (more < 0 || (size_t)(n + more) + CDB_PAGE_TAIL_RESERVE >= cap)
            break;
        n += more;
        emitted++;
    }
    free(rows);

    unsigned next = offset + (unsigned)emitted;
    /* Rows remain after this page: the table holds more than `next`. */
    int more_rows = total > next;
    /* A row that cannot fit even on an empty page can never be delivered:
     * say so and stop, rather than send the caller round in a loop. */
    int stuck = more_rows && emitted == 0 && count > 0;
    int truncated = capped || stuck;
    if (stuck) more_rows = 0;

    /* `total` is exact only from SQL: the scanner's is a lower bound that
     * grows with the window, so it is left out rather than mislead a
     * caller comparing pages. */
    char total_field[32] = "";
    if (strcmp(source, "scan") != 0)
        snprintf(total_field, sizeof(total_field), ",\"total\":%u", total);
    int end2 = snprintf(buf + n, cap - (size_t)n,
                        "],\"source\":\"%s\",\"offset\":%u,\"next\":%u,"
                        "\"more\":%s,\"truncated\":%s%s}",
                        source, offset, next, more_rows ? "true" : "false",
                        truncated ? "true" : "false", total_field);
    if (end2 < 0 || (size_t)(n + end2) >= cap) return -1;
    n += end2;
    if (written) *written = (size_t)n;
    return 0;
}

int content_db_apps_json(char *buf, size_t cap, size_t *written) {
    return content_db_apps_page_json(0, 0, buf, cap, written);
}

/* ── appinfo.db ──────────────────────────────────────────────────────── */

int content_db_appinfo_json(const char *title_id, const char *keys,
                            char *buf, size_t cap, size_t *written) {
    if (!buf || cap < 64) return -1;
    if (!cdb_valid_title_id(title_id))
        return cdb_emit_err(buf, cap, written, "invalid title id");

    sqlite3 *db = NULL;
    const char *how = "sqlite";
    int rc = cdb_open_ro(CONTENT_DB_APPINFO, &db, &how);
    if (rc != SQLITE_OK || !db)
        return cdb_emit_err(buf, cap, written, "cannot open appinfo.db");

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db,
                           "SELECT key, val FROM tbl_appinfo "
                           "WHERE titleId = ? ORDER BY key",
                           -1, &st, NULL) != SQLITE_OK) {
        char msg[256];
        snprintf(msg, sizeof(msg), "appinfo.db query failed: %s",
                 sqlite3_errmsg(db));
        sqlite3_close(db);
        return cdb_emit_err(buf, cap, written, msg);
    }
    sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);

    char etid[32];
    cdb_json_escape(title_id, etid, sizeof(etid));
    int n = snprintf(buf, cap, "{\"ok\":true,\"title_id\":\"%s\",\"rows\":[",
                     etid);
    int emitted = 0;
    while (sqlite3_step(st) == SQLITE_ROW) {
        const unsigned char *k = sqlite3_column_text(st, 0);
        const unsigned char *v = sqlite3_column_text(st, 1);
        if (!k) continue;
        /* Filter client-side: the key set is small and a LIKE/IN built
         * from caller text would need escaping we do not otherwise need. */
        if (keys && *keys) {
            char needle[128];
            snprintf(needle, sizeof(needle), ",%s,", (const char *)k);
            char haystack[512];
            snprintf(haystack, sizeof(haystack), ",%s,", keys);
            if (!strstr(haystack, needle)) continue;
        }
        char ek[128], ev[512];
        cdb_json_escape((const char *)k, ek, sizeof(ek));
        cdb_json_escape(v ? (const char *)v : "", ev, sizeof(ev));
        int more = snprintf(buf + n, cap - (size_t)n,
                            "%s{\"key\":\"%s\",\"val\":\"%s\"}",
                            emitted ? "," : "", ek, ev);
        if (more < 0 || (size_t)(n + more) + 32 >= cap) break;
        n += more;
        emitted++;
    }
    sqlite3_finalize(st);
    sqlite3_close(db);

    int end = snprintf(buf + n, cap - (size_t)n, "],\"source\":\"%s\"}", how);
    if (end < 0 || (size_t)(n + end) >= cap) return -1;
    n += end;
    if (written) *written = (size_t)n;
    return 0;
}

int content_db_appinfo_get(const char *title_id, const char *key, char *out, size_t cap) {
    if (!out || cap == 0) return -1;
    out[0] = '\0';
    if (!cdb_valid_title_id(title_id) || !key || !key[0]) return -1;
    sqlite3 *db = NULL;
    if (cdb_open_ro(CONTENT_DB_APPINFO, &db, NULL) != SQLITE_OK || !db) return -1;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, "SELECT val FROM tbl_appinfo WHERE titleId = ? AND key = ? LIMIT 1", -1,
                           &st, NULL) != SQLITE_OK) {
        sqlite3_close(db);
        return -1;
    }
    sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 2, key, -1, SQLITE_STATIC);
    int found = 0;
    int rc = sqlite3_step(st);
    if (rc == SQLITE_ROW) {
        const unsigned char *v = sqlite3_column_text(st, 0);
        snprintf(out, cap, "%s", v ? (const char *)v : "");
        found = 1;
    } else if (rc != SQLITE_DONE) {
        found = -1;
    }
    sqlite3_finalize(st);
    sqlite3_close(db);
    return found;
}

int content_db_appinfo_has_title(const char *title_id) {
    if (!cdb_valid_title_id(title_id)) return -1;
    sqlite3 *db = NULL;
    if (cdb_open_ro(CONTENT_DB_APPINFO, &db, NULL) != SQLITE_OK || !db) return -1;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, "SELECT 1 FROM tbl_appinfo WHERE titleId = ? LIMIT 1", -1, &st,
                           NULL) != SQLITE_OK) {
        sqlite3_close(db);
        return -1;
    }
    sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
    int rc = sqlite3_step(st);
    int found = rc == SQLITE_ROW ? 1 : rc == SQLITE_DONE ? 0 : -1;
    sqlite3_finalize(st);
    sqlite3_close(db);
    return found;
}

int content_db_backup_file(const char *src_path, const char *dst_path, char *err, size_t errcap) {
    if (!err || errcap == 0) return -1;
    err[0] = '\0';
    if (!src_path || !dst_path || !src_path[0] || !dst_path[0]) {
        snprintf(err, errcap, "backup: no path");
        return -1;
    }
    if (access(dst_path, F_OK) == 0) {
        snprintf(err, errcap, "backup: the copy already exists");
        return -1;
    }
    sqlite3 *src = NULL;
    if (cdb_open_ro(src_path, &src, NULL) != SQLITE_OK || !src) {
        snprintf(err, errcap, "backup: cannot read %s", src_path);
        return -1;
    }
    sqlite3 *dst = NULL;
    int ok = 0;
    if (sqlite3_open_v2(dst_path, &dst, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL) ==
            SQLITE_OK &&
        dst) {
        sqlite3_backup *b = sqlite3_backup_init(dst, "main", src, "main");
        if (b) {
            ok = sqlite3_backup_step(b, -1) == SQLITE_DONE;
            sqlite3_backup_finish(b);
        }
    }
    if (dst) sqlite3_close(dst);
    sqlite3_close(src);
    if (!ok) {
        unlink(dst_path);
        snprintf(err, errcap, "backup: could not copy %s", src_path);
        return -1;
    }
    return 0;
}

int content_db_appinfo_set(const char *title_id, const char *key,
                           const char *val, char *err, size_t errcap) {
#define CDB_FAIL(msg) do { snprintf(err, errcap, "%s", (msg)); goto fail; } while (0)

    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int in_txn = 0;

    if (!err || errcap == 0) return -1;
    if (!cdb_valid_title_id(title_id)) {
        snprintf(err, errcap, "invalid title id");
        return -1;
    }
    if (!key || !*key || !val) {
        snprintf(err, errcap, "key and value are required");
        return -1;
    }
    /* Keys are SCREAMING_SNAKE identifiers. Rejecting anything else keeps
     * a caller from aiming the update at a row it should not reach even
     * though the statement is parameterised. */
    for (const char *p = key; *p; p++) {
        if (!((*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9') ||
              *p == '_')) {
            snprintf(err, errcap, "key must be A-Z, 0-9 and underscore");
            return -1;
        }
    }

    /* A running title holds its appinfo row; editing it underneath the
     * shell is how you get a Settings entry that renders as a blank tile.
     * This is the same precondition the reference implementation states
     * in prose ("send this after the app has been killed") -- enforced
     * here rather than left to the operator. */
    if (proc_find_pid_by_title_id(title_id) > 0) {
        snprintf(err, errcap,
                 "%s is running -- close it before editing appinfo.db",
                 title_id);
        return -1;
    }

    if (sqlite3_open_v2(CONTENT_DB_APPINFO, &db, SQLITE_OPEN_READWRITE,
                        NULL) != SQLITE_OK || !db) {
        snprintf(err, errcap, "cannot open appinfo.db for writing%s%s",
                 db ? ": " : "", db ? sqlite3_errmsg(db) : "");
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 5000);

    /* BEGIN IMMEDIATE takes the write lock now rather than on first
     * write, so a shell that holds the database makes us fail here --
     * before any change -- instead of midway through. */
    if (sqlite3_exec(db, "BEGIN IMMEDIATE", NULL, NULL, NULL) != SQLITE_OK)
        CDB_FAIL("appinfo.db is locked by the system");
    in_txn = 1;

    if (sqlite3_prepare_v2(db,
                           "UPDATE tbl_appinfo SET val = ? "
                           "WHERE titleId = ? AND key = ?",
                           -1, &st, NULL) != SQLITE_OK)
        CDB_FAIL("appinfo.db has an unexpected schema");

    sqlite3_bind_text(st, 1, val, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 2, title_id, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 3, key, -1, SQLITE_STATIC);

    if (sqlite3_step(st) != SQLITE_DONE) CDB_FAIL(sqlite3_errmsg(db));

    int changed = sqlite3_changes(db);
    sqlite3_finalize(st);
    st = NULL;

    /* Exactly one row, or nothing. Zero means the (titleId, key) pair does
     * not exist and we would otherwise report a silent no-op as success;
     * more than one means the schema is not what we think it is and the
     * blast radius is larger than the caller asked for. */
    if (changed == 0) {
        snprintf(err, errcap, "appinfo.db has no %s row for %s", key,
                 title_id);
        goto fail;
    }
    if (changed != 1) {
        snprintf(err, errcap, "refusing to change %d %s rows for %s", changed,
                 key, title_id);
        goto fail;
    }

    if (sqlite3_exec(db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK)
        CDB_FAIL("could not commit the appinfo.db change");
    in_txn = 0;
    sqlite3_close(db);
    return 0;

fail:
    if (st) sqlite3_finalize(st);
    if (in_txn) sqlite3_exec(db, "ROLLBACK", NULL, NULL, NULL);
    if (db) sqlite3_close(db);
    return -1;
#undef CDB_FAIL
}

/* ── home-screen icons (#407) ────────────────────────────────────────── */

#define CDB_MAX_ICON_TABLES 16

/* The per-user icon tables: any table with both a titleId and a visible
 * column, found by shape like the title table (never by a guessed name). */
static int cdb_icon_tables(sqlite3 *db, char names[][96], int max) {
    sqlite3_stmt *st = NULL;
    int n = 0;
    if (sqlite3_prepare_v2(db,
                           "SELECT name FROM sqlite_master WHERE type='table' "
                           "ORDER BY name",
                           -1, &st, NULL) != SQLITE_OK)
        return 0;
    while (n < max && sqlite3_step(st) == SQLITE_ROW) {
        const char *name = (const char *)sqlite3_column_text(st, 0);
        char col[64];
        if (!name || strlen(name) >= 96 || strchr(name, '"')) continue;
        if (cdb_has_column(db, name, "titleId", col, sizeof col) &&
            cdb_has_column(db, name, "visible", col, sizeof col))
            snprintf(names[n++], 96, "%s", name);
    }
    sqlite3_finalize(st);
    return n;
}

/* Copy app.db aside once, through SQLite's backup API (a consistent
 * snapshot even while the shell has it open). The first copy is the one
 * worth keeping: it predates every change this app made. */
static int cdb_backup_app_db_once(sqlite3 *src, char *err, size_t errcap) {
    if (access(CONTENT_DB_APP_BACKUP, F_OK) == 0) return 0;
    char dir[256];
    snprintf(dir, sizeof dir, "%s", CONTENT_DB_APP_BACKUP);
    char *slash = strrchr(dir, '/');
    if (slash && slash != dir) {
        *slash = 0;
        mkdir(dir, 0777);
    }
    sqlite3 *dst = NULL;
    int ok = 0;
    if (sqlite3_open_v2(CONTENT_DB_APP_BACKUP, &dst,
                        SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                        NULL) == SQLITE_OK && dst) {
        sqlite3_backup *b = sqlite3_backup_init(dst, "main", src, "main");
        if (b) {
            ok = sqlite3_backup_step(b, -1) == SQLITE_DONE;
            sqlite3_backup_finish(b);
        }
    }
    if (dst) sqlite3_close(dst);
    if (!ok) {
        unlink(CONTENT_DB_APP_BACKUP);
        snprintf(err, errcap, "could not back up app.db first, nothing changed");
        return -1;
    }
    return 0;
}

int content_db_icon_set_visible(const char *title_id, int visible, char *err,
                                size_t errcap) {
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int in_txn = 0;
    char tables[CDB_MAX_ICON_TABLES][96];

    if (!err || errcap == 0) return -1;
    if (!cdb_valid_title_id(title_id)) {
        snprintf(err, errcap, "invalid title id");
        return -1;
    }
    if (strncasecmp(title_id, "NPXS", 4) == 0) {
        snprintf(err, errcap, "%s is a system app and cannot be hidden",
                 title_id);
        return -1;
    }
    if (sqlite3_open_v2(CONTENT_DB_APP, &db, SQLITE_OPEN_READWRITE, NULL) !=
            SQLITE_OK || !db) {
        snprintf(err, errcap, "cannot open app.db for writing");
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 5000);

    int nt = cdb_icon_tables(db, tables, CDB_MAX_ICON_TABLES);
    if (nt == 0) {
        snprintf(err, errcap,
                 "app.db has no icon table with a visible column; nothing changed");
        goto fail;
    }
    if (cdb_backup_app_db_once(db, err, errcap) != 0) goto fail;

    if (sqlite3_exec(db, "BEGIN IMMEDIATE", NULL, NULL, NULL) != SQLITE_OK) {
        snprintf(err, errcap, "app.db is locked by the system, try again");
        goto fail;
    }
    in_txn = 1;

    int changed = 0;
    for (int i = 0; i < nt; i++) {
        char sql[192];
        snprintf(sql, sizeof sql,
                 "UPDATE \"%s\" SET visible = ? WHERE titleId = ?", tables[i]);
        if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) {
            snprintf(err, errcap, "app.db has an unexpected schema");
            goto fail;
        }
        sqlite3_bind_int(st, 1, visible ? 1 : 0);
        sqlite3_bind_text(st, 2, title_id, -1, SQLITE_STATIC);
        if (sqlite3_step(st) != SQLITE_DONE) {
            snprintf(err, errcap, "%s", sqlite3_errmsg(db));
            goto fail;
        }
        int c = sqlite3_changes(db);
        sqlite3_finalize(st);
        st = NULL;
        /* One icon per title per user: more means the table is not what we
         * think it is, and the change would reach further than asked. */
        if (c > 1) {
            snprintf(err, errcap, "refusing to change %d rows for %s in %s", c,
                     title_id, tables[i]);
            goto fail;
        }
        changed += c;
    }
    if (changed == 0) {
        snprintf(err, errcap, "%s has no home-screen icon", title_id);
        goto fail;
    }
    if (sqlite3_exec(db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK) {
        snprintf(err, errcap, "could not commit the app.db change");
        goto fail;
    }
    sqlite3_close(db);
    return 0;

fail:
    if (st) sqlite3_finalize(st);
    if (in_txn) sqlite3_exec(db, "ROLLBACK", NULL, NULL, NULL);
    sqlite3_close(db);
    return -1;
}

int content_db_icons_rehide(const char *const *ids, int n, char *err, size_t errcap) {
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int in_txn = 0, hid = 0;
    char tables[CDB_MAX_ICON_TABLES][96];

    if (!err || errcap == 0 || !ids || n <= 0) return 0;
    if (sqlite3_open_v2(CONTENT_DB_APP, &db, SQLITE_OPEN_READWRITE, NULL) != SQLITE_OK || !db) {
        snprintf(err, errcap, "cannot open app.db for writing");
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 5000);
    int nt = cdb_icon_tables(db, tables, CDB_MAX_ICON_TABLES);
    if (nt == 0) {
        snprintf(err, errcap, "app.db has no icon table with a visible column");
        goto fail;
    }
    if (sqlite3_exec(db, "BEGIN IMMEDIATE", NULL, NULL, NULL) != SQLITE_OK) {
        snprintf(err, errcap, "app.db is locked by the system");
        goto fail;
    }
    in_txn = 1;
    for (int i = 0; i < nt; i++) {
        char sql[192];
        snprintf(sql, sizeof sql,
                 "UPDATE \"%s\" SET visible = 0 WHERE titleId = ? AND visible <> 0", tables[i]);
        if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) {
            snprintf(err, errcap, "app.db has an unexpected schema");
            goto fail;
        }
        for (int k = 0; k < n; k++) {
            if (!cdb_valid_title_id(ids[k]) || strncasecmp(ids[k], "NPXS", 4) == 0) continue;
            sqlite3_reset(st);
            sqlite3_bind_text(st, 1, ids[k], -1, SQLITE_STATIC);
            if (sqlite3_step(st) != SQLITE_DONE) {
                snprintf(err, errcap, "%s", sqlite3_errmsg(db));
                goto fail;
            }
            int c = sqlite3_changes(db);
            if (c > 1) {
                snprintf(err, errcap, "refusing to change %d rows for %s in %s", c, ids[k],
                         tables[i]);
                goto fail;
            }
            hid += c;
        }
        sqlite3_finalize(st);
        st = NULL;
    }
    if (sqlite3_exec(db, hid ? "COMMIT" : "ROLLBACK", NULL, NULL, NULL) != SQLITE_OK) {
        snprintf(err, errcap, "could not commit the app.db change");
        goto fail;
    }
    sqlite3_close(db);
    return hid;

fail:
    if (st) sqlite3_finalize(st);
    if (in_txn) sqlite3_exec(db, "ROLLBACK", NULL, NULL, NULL);
    sqlite3_close(db);
    return -1;
}

int content_db_hidden_json(char *buf, size_t cap, size_t *written) {
    if (!buf || cap < 64) return -1;
    sqlite3 *db = NULL;
    char tables[CDB_MAX_ICON_TABLES][96];
    if (cdb_open_ro(CONTENT_DB_APP, &db, NULL) != SQLITE_OK || !db)
        return cdb_emit_err(buf, cap, written, "cannot read app.db");
    int nt = cdb_icon_tables(db, tables, CDB_MAX_ICON_TABLES);
    if (nt == 0) {
        sqlite3_close(db);
        return cdb_emit_err(buf, cap, written,
                            "app.db has no icon table with a visible column");
    }
    /* One query over every user's table; UNION removes the duplicates. */
    char sql[CDB_MAX_ICON_TABLES * 140];
    size_t o = 0;
    for (int i = 0; i < nt; i++) {
        /* Checked: a table name comes from app.db, and an unchecked `o` past the buffer made the
         * next size wrap (16 names of 95 bytes wrote past `sql`). A table that does not fit is
         * left out. */
        int k = snprintf(sql + o, sizeof sql - o, "%sSELECT titleId FROM \"%s\" WHERE visible = 0",
                         o ? " UNION " : "", tables[i]);
        if (k < 0 || (size_t)k + 16 >= sizeof sql - o) {
            sql[o] = '\0';
            break;
        }
        o += (size_t)k;
    }
    if (o == 0) {
        sqlite3_close(db);
        return cdb_emit_err(buf, cap, written, "app.db has an unexpected schema");
    }
    snprintf(sql + o, sizeof sql - o, " ORDER BY 1");
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) {
        sqlite3_close(db);
        return cdb_emit_err(buf, cap, written, "app.db has an unexpected schema");
    }
    int n = snprintf(buf, cap, "{\"ok\":true,\"hidden\":[");
    int first = 1;
    while (sqlite3_step(st) == SQLITE_ROW && (size_t)n + 32 < cap) {
        const char *t = (const char *)sqlite3_column_text(st, 0);
        /* The system hides some of its own apps the same way; those are not ours to show. */
        if (!t || !cdb_valid_title_id(t) || strncasecmp(t, "NPXS", 4) == 0) continue;
        n += snprintf(buf + n, cap - (size_t)n, "%s\"%s\"", first ? "" : ",", t);
        first = 0;
    }
    sqlite3_finalize(st);
    sqlite3_close(db);
    n += snprintf(buf + n, cap - (size_t)n, "]}");
    if (written) *written = (size_t)n;
    return 0;
}
