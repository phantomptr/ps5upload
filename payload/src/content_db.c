#include "content_db.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#include "appdb_scan.h"
#include "proc_list.h"
#include "sqlite3.h"

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

/* Row-level app list: SQL first, byte scan if the database will not open.
 * content_db_apps_json() and activity.c's "recently played" both build on
 * this so there is one schema-discovery implementation, not two. */
int content_db_apps(content_db_app_t *out, int max, const char **source) {
    if (!out || max <= 0) return -1;
    if (source) *source = "scan";

    sqlite3 *db = NULL;
    const char *how = "sqlite";
    int count = 0;

    if (cdb_open_ro(CONTENT_DB_APP, &db, &how) == SQLITE_OK && db) {
        cdb_app_schema_t cands[CDB_MAX_SCHEMAS];
        int ncand = cdb_discover_app_schemas(db, cands, CDB_MAX_SCHEMAS);

        /* First candidate that actually yields rows wins: a console with
         * several users has several of these tables, and the ones
         * belonging to other users are empty. */
        for (int ci = 0; ci < ncand && count == 0; ci++) {
            char sql[512];
            cdb_build_app_sql(&cands[ci], sql, sizeof(sql));

            sqlite3_stmt *st = NULL;
            if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK)
                continue;
            while (count < max && sqlite3_step(st) == SQLITE_ROW) {
                const unsigned char *tid = sqlite3_column_text(st, 0);
                if (!tid) continue;
                const unsigned char *nm = sqlite3_column_text(st, 2);
                memset(&out[count], 0, sizeof(out[count]));
                snprintf(out[count].title_id, sizeof(out[count].title_id),
                         "%s", (const char *)tid);
                snprintf(out[count].name, sizeof(out[count].name), "%s",
                         nm ? (const char *)nm : "");
                out[count].app_id = sqlite3_column_int(st, 1);
                count++;
            }
            sqlite3_finalize(st);
        }
        sqlite3_close(db);
    }

    /* Zero rows from SQL means the schema guess missed, not that the
     * console has no games -- and install verification reads an empty list
     * as a failed install. Fall through to the scan rather than assert a
     * confident zero. */
    if (count > 0) {
        if (source) *source = how;
        return count;
    }

    int fd = open(CONTENT_DB_APP, O_RDONLY);
    if (fd < 0) return -1;
    const size_t raw_cap = 1024 * 1024;
    unsigned char *raw = (unsigned char *)malloc(raw_cap);
    if (!raw) {
        close(fd);
        return -1;
    }
    size_t total = 0;
    while (total < raw_cap) {
        ssize_t r = read(fd, raw + total, raw_cap - total);
        if (r <= 0) break;
        total += (size_t)r;
    }
    close(fd);

    appdb_entry_t *entries =
        (appdb_entry_t *)malloc(sizeof(appdb_entry_t) * (size_t)max);
    if (!entries) {
        free(raw);
        return -1;
    }
    int scanned = appdb_scan_entries_ex(raw, total, entries, max, 1);
    free(raw);
    if (scanned < 0) {
        free(entries);
        return -1;
    }
    for (int i = 0; i < scanned; i++) {
        memset(&out[i], 0, sizeof(out[i]));
        snprintf(out[i].title_id, sizeof(out[i].title_id), "%s",
                 entries[i].title_id);
        snprintf(out[i].name, sizeof(out[i].name), "%s", entries[i].name);
        out[i].app_id = 0;
    }
    free(entries);
    if (source) *source = "scan";
    return scanned;
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

int content_db_apps_json(char *buf, size_t cap, size_t *written) {
    if (!buf || cap < 64) return -1;

    const int max_rows = 256;
    content_db_app_t *rows =
        (content_db_app_t *)malloc(sizeof(content_db_app_t) * (size_t)max_rows);
    if (!rows) return cdb_emit_apps_err(buf, cap, written, "out of memory");

    const char *source = "scan";
    int count = content_db_apps(rows, max_rows, &source);
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
        if (more < 0 || (size_t)(n + more) + 40 >= cap) break;
        n += more;
        emitted++;
    }
    free(rows);

    int end2 = snprintf(buf + n, cap - (size_t)n, "],\"source\":\"%s\"}",
                        source);
    if (end2 < 0 || (size_t)(n + end2) >= cap) return -1;
    n += end2;
    if (written) *written = (size_t)n;
    return 0;
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
