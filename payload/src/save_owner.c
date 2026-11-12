/* Whose save it is, the console side (save_owner.h): the protected param.sfo block of a save
 * image, a user's account id, and the user's savedata.db row. */
#include "save_owner.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include <ps5/kernel.h>

#include "authid.h"
#include "json_escape.h"
#include "profile.h"
#include "sqlite3.h"

/* ── The protected block ──────────────────────────────────────────────────
 * The save filesystem's control device reads and writes it by the image's path: the kernel
 * decrypts it with the save's own sealed key and regenerates its check value on a write. Two
 * command pairs exist (the second carries one more pointer); which one a firmware answers is
 * not known ahead, so both are tried and every refusal is reported.
 * Block: "PS5PARAM" | u32 version 0 | u32 sfo size | u32 sfo offset (20) | the param.sfo. */
#define PARAM_READ_A 0xC0189111UL
#define PARAM_WRITE_A 0x80189112UL
#define PARAM_READ_B 0xC0209116UL
#define PARAM_WRITE_B 0x80209118UL
/* The identity the console's own save tools hold; tried only when ours is refused. */
#define SAVE_TOOL_AUTHID 0x4800000000000010ULL

typedef struct {
    const char *path;
    void *buf;
    uint32_t size;
    uint32_t pad;
    uint64_t *out;
} param_arg_t;

static const char *const k_param_devs[] = {"/dev/pfsctldev", "/dev/pfsctl"};
static const char k_param_magic[8] = {'P', 'S', '5', 'P', 'A', 'R', 'A', 'M'};

static void how_add(char *how, size_t how_sz, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void how_add(char *how, size_t how_sz, const char *fmt, ...) {
    if (!how || how_sz == 0) return;
    size_t at = strlen(how);
    if (at + 3 >= how_sz) return;
    if (at) {
        memcpy(how + at, "; ", 3);
        at += 2;
    }
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(how + at, how_sz - at, fmt, ap);
    va_end(ap);
}

/* One pass over the devices and both commands. 0 when one answered (for a read: with a block, or
 * `*empty` set when it answered with none), else -errno of the last refusal. */
static int param_pass(const char *image, uint8_t *blob, int wr, int *empty, char *how, size_t how_sz) {
    int last = ENOENT;
    int unsupported = 0; /* a device answered "not supported" */
    for (size_t d = 0; d < sizeof k_param_devs / sizeof k_param_devs[0]; d++) {
        /* The device is created owner-read-only: O_RDWR is refused even to root. */
        int fd = open(k_param_devs[d], O_RDONLY);
        if (fd < 0) {
            last = errno;
            how_add(how, how_sz, "open %s: errno %d", k_param_devs[d], last);
            continue;
        }
        const unsigned long cmds[2] = {wr ? PARAM_WRITE_B : PARAM_READ_B, wr ? PARAM_WRITE_A : PARAM_READ_A};
        for (int c = 0; c < 2; c++) {
            uint64_t out = 0;
            param_arg_t a;
            memset(&a, 0, sizeof a);
            a.path = image;
            a.buf = blob;
            a.size = SAVE_PARAM_BLOB_BYTES;
            a.out = &out;
            if (!wr) memset(blob, 0, SAVE_PARAM_BLOB_BYTES);
            errno = 0;
            if (ioctl(fd, cmds[c], &a) < 0) {
                last = errno ? errno : EIO;
                if (last == EOPNOTSUPP || last == ENOTSUP) unsupported = 1;
                how_add(how, how_sz, "%s %lx: errno %d", k_param_devs[d], cmds[c], last);
                continue;
            }
            if (!wr && memcmp(blob, k_param_magic, 8) != 0) {
                *empty = 1;
                how_add(how, how_sz, "%s %lx: no block", k_param_devs[d], cmds[c]);
                continue;
            }
            how_add(how, how_sz, "%s %lx: ok", k_param_devs[d], cmds[c]);
            close(fd);
            return 0;
        }
        close(fd);
    }
    /* "Not supported" is the answer for an image that keeps its param.sfo as a file inside the
     * save and has no protected block (seen on FW 13.60). */
    if (!wr && unsupported) *empty = 1;
    return *empty ? 0 : -last;
}

/* The pass under our own identity, then once more as the console's save tool when refused. */
static int param_call(const char *image, uint8_t *blob, int wr, int *empty, char *how, size_t how_sz) {
    *empty = 0;
    if (how && how_sz) how[0] = 0;
    int r = param_pass(image, blob, wr, empty, how, how_sz);
    if (r == 0 || (r != -EPERM && r != -EACCES)) return r;
    uint64_t saved = ps5_authid_acquire("save-owner", SAVE_TOOL_AUTHID);
    if (saved == 0) return r;
    how_add(how, how_sz, "again as the save tool");
    r = param_pass(image, blob, wr, empty, how, how_sz);
    ps5_authid_release(saved, "save-owner");
    return r;
}

static uint32_t rd32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static void wr32(uint8_t *p, uint32_t v) {
    for (int i = 0; i < 4; i++) p[i] = (uint8_t)(v >> (8 * i));
}

int save_param_read(const char *image, uint8_t *sfo, uint32_t cap, char *how, size_t how_sz) {
    if (!image || !image[0] || !sfo) return -EINVAL;
    uint8_t *blob = calloc(1, SAVE_PARAM_BLOB_BYTES + 256); /* room past the block, as for the key call */
    if (!blob) return -ENOMEM;
    int empty = 0;
    int r = param_call(image, blob, 0, &empty, how, how_sz);
    if (r == 0 && memcmp(blob, k_param_magic, 8) != 0) {
        free(blob);
        return 0; /* answered, and the image has no block */
    }
    if (r == 0) {
        uint32_t ver = rd32(blob + 8), sz = rd32(blob + 12), off = rd32(blob + 16);
        if (ver != 0 || sz == 0 || sz > cap || off < SAVE_PARAM_HEAD_BYTES ||
            (uint64_t)off + sz > SAVE_PARAM_BLOB_BYTES || !save_sfo_valid(blob + off, sz)) {
            r = -EBADMSG;
        } else {
            memcpy(sfo, blob + off, sz);
            r = (int)sz;
        }
    }
    free(blob);
    return r;
}

int save_param_write(const char *image, const uint8_t *sfo, uint32_t size, char *how, size_t how_sz) {
    if (!image || !image[0] || !save_sfo_valid(sfo, size) || size > SAVE_SFO_MAX) return -EINVAL;
    uint8_t *blob = calloc(1, SAVE_PARAM_BLOB_BYTES + 256);
    uint8_t *check = calloc(1, SAVE_SFO_MAX);
    if (!blob || !check) {
        free(blob);
        free(check);
        return -ENOMEM;
    }
    memcpy(blob, k_param_magic, 8);
    wr32(blob + 8, 0);
    wr32(blob + 12, size);
    wr32(blob + 16, SAVE_PARAM_HEAD_BYTES);
    memcpy(blob + SAVE_PARAM_HEAD_BYTES, sfo, size);
    int empty = 0;
    int r = param_call(image, blob, 1, &empty, how, how_sz);
    if (r == 0) {
        int n = save_param_read(image, check, SAVE_SFO_MAX, NULL, 0);
        if (n != (int)size || memcmp(check, sfo, size) != 0) r = -EIO;
    }
    free(blob);
    free(check);
    return r;
}

/* ── The user's account ───────────────────────────────────────────────── */

int save_owner_account(uint32_t uid, uint64_t *account_id) {
    char user[PROFILE_NAME_MAX] = {0};
    if (!account_id) return -1;
    *account_id = 0;
    if (profile_user_name(uid, user, sizeof user) != 0 || !user[0]) return -1;
    for (int s = 1; s <= PROFILE_SLOT_COUNT; s++) {
        char name[PROFILE_NAME_MAX] = {0};
        if (profile_slot_get_name(s, name, NULL) != 0 || strcmp(name, user) != 0) continue;
        uint64_t id = 0;
        if (profile_slot_get_id(s, &id, NULL) != 0) return -3;
        *account_id = id;
        return 0;
    }
    return -2;
}

/* ── The user's save database ─────────────────────────────────────────── */

static void db_path(uint32_t uid, char *out, size_t cap) {
    snprintf(out, cap, "/system_data/savedata_prospero/%08x/db/user/savedata.db", (unsigned)uid);
}

static int db_has_column(sqlite3 *db, const char *name) {
    sqlite3_stmt *st = NULL;
    int found = 0;
    if (sqlite3_prepare_v2(db, "PRAGMA table_info(savedata)", -1, &st, NULL) != SQLITE_OK) return 0;
    while (sqlite3_step(st) == SQLITE_ROW) {
        const char *col = (const char *)sqlite3_column_text(st, 1);
        if (col && strcmp(col, name) == 0) found = 1;
    }
    sqlite3_finalize(st);
    return found;
}

int save_db_upsert(uint32_t uid, const save_sfo_info_t *info, const save_usage_t *usage, char *err,
                   size_t err_sz) {
    char path[160];
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int result = -1;
    if (err && err_sz) err[0] = 0;
    if (!info || !usage || !info->title_id[0] || !info->dir_name[0]) {
        if (err) snprintf(err, err_sz, "the save has no title id or folder name");
        return -1;
    }
    db_path(uid, path, sizeof path);
    /* The console makes this database when the user first has a save; without it there is no
     * table layout to follow, so it is not made here. */
    if (sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE, NULL) != SQLITE_OK || !db) {
        if (err) snprintf(err, err_sz, "cannot open %s: %s", path, db ? sqlite3_errmsg(db) : "no memory");
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 3000);
    char mtime[40];
    time_t now = time(NULL);
    struct tm tmv;
    gmtime_r(&now, &tmv);
    strftime(mtime, sizeof mtime, "%Y-%m-%dT%H:%M:%S.00Z", &tmv);

    int64_t id = -1;
    if (sqlite3_prepare_v2(db, "SELECT id FROM savedata WHERE title_id=?1 AND dir_name=?2", -1, &st, NULL) != SQLITE_OK)
        goto fail;
    sqlite3_bind_text(st, 1, info->title_id, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 2, info->dir_name, -1, SQLITE_STATIC);
    if (sqlite3_step(st) == SQLITE_ROW) id = sqlite3_column_int64(st, 0);
    sqlite3_finalize(st);
    st = NULL;

    /* Columns a newer firmware added (13.60 has both); an older table lacks them. The console
     * writes '' and 0 where it has nothing, never NULL, so a new row does the same. */
    int has_perf = db_has_column(db, "convert_performance");
    int has_usb = db_has_column(db, "account_id_in_usb_path");
    char insert[900];
    snprintf(insert, sizeof insert,
             "INSERT INTO savedata (title_id, dir_name, main_title, sub_title, detail, tmp_dir_name, "
             "is_broken, user_param, blocks, free_blocks, size_kib, mtime, fake_broken, account_id, "
             "user_id, faked_owner, cloud_icon_url, cloud_revision, game_title_id, system_blocks%s%s) "
             "VALUES (?1, ?2, ?3, ?4, ?5, '', 0, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12, 0, '', 0, ?1, ?13%s%s)",
             has_perf ? ", convert_performance" : "", has_usb ? ", account_id_in_usb_path" : "",
             has_perf ? ", 0" : "", has_usb ? ", '0000000000000000'" : "");
    const char *sql = id >= 0
        ? "UPDATE savedata SET main_title=?3, sub_title=?4, detail=?5, is_broken=0, user_param=?6, "
          "blocks=?7, free_blocks=?8, size_kib=?9, mtime=?10, account_id=?11, user_id=?12, "
          "system_blocks=?13 WHERE title_id=?1 AND dir_name=?2"
        : insert;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK) goto fail;
    sqlite3_bind_text(st, 1, info->title_id, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 2, info->dir_name, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 3, info->main_title, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 4, info->sub_title, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 5, info->detail, -1, SQLITE_STATIC);
    sqlite3_bind_int64(st, 6, (int64_t)info->list_param);
    sqlite3_bind_int64(st, 7, (int64_t)usage->blocks);
    sqlite3_bind_int64(st, 8, (int64_t)usage->free_blocks);
    sqlite3_bind_int64(st, 9, (int64_t)usage->size_kib);
    sqlite3_bind_text(st, 10, mtime, -1, SQLITE_STATIC);
    sqlite3_bind_int64(st, 11, (int64_t)info->account_id);
    sqlite3_bind_int64(st, 12, (int64_t)uid);
    sqlite3_bind_int64(st, 13, (int64_t)info->system_blocks);
    if (sqlite3_step(st) != SQLITE_DONE) goto fail;
    result = id >= 0 ? 2 : 1;
fail:
    if (result < 0 && err) snprintf(err, err_sz, "savedata.db: %s", sqlite3_errmsg(db));
    if (st) sqlite3_finalize(st);
    sqlite3_close(db);
    return result;
}

int save_db_rows_json(uint32_t uid, const char *title_id, char *out, size_t cap) {
    char path[160];
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    size_t at = 0;
    int ok = 0;
    if (!out || cap < 3 || !title_id) return -1;
    db_path(uid, path, sizeof path);
    if (sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY, NULL) != SQLITE_OK || !db) {
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 3000);
    if (sqlite3_prepare_v2(db, "SELECT * FROM savedata WHERE title_id=?1", -1, &st, NULL) != SQLITE_OK) goto done;
    sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
    out[at++] = '[';
    int rows = 0;
    while (sqlite3_step(st) == SQLITE_ROW) {
        int cols = sqlite3_column_count(st);
        if (at + 2 >= cap) goto done;
        if (rows++) out[at++] = ',';
        out[at++] = '{';
        for (int c = 0; c < cols; c++) {
            char val[600] = "null";
            int type = sqlite3_column_type(st, c);
            if (type == SQLITE_INTEGER) {
                snprintf(val, sizeof val, "%lld", (long long)sqlite3_column_int64(st, c));
            } else if (type != SQLITE_NULL && type != SQLITE_BLOB) {
                const char *text = (const char *)sqlite3_column_text(st, c);
                char esc[560];
                json_escape_into(text ? text : "", esc, sizeof esc);
                snprintf(val, sizeof val, "\"%s\"", esc);
            }
            int n = snprintf(out + at, cap - at, "%s\"%s\":%s", c ? "," : "", sqlite3_column_name(st, c), val);
            if (n < 0 || (size_t)n >= cap - at) goto done;
            at += (size_t)n;
        }
        if (at + 2 >= cap) goto done;
        out[at++] = '}';
    }
    if (at + 2 >= cap) goto done;
    out[at++] = ']';
    out[at] = 0;
    ok = 1;
done:
    if (st) sqlite3_finalize(st);
    sqlite3_close(db);
    return ok ? (int)at : -1;
}

int save_db_schema(uint32_t uid, char *out, size_t cap) {
    char path[160];
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int n = -1;
    if (!out || cap == 0) return -1;
    out[0] = 0;
    db_path(uid, path, sizeof path);
    if (sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY, NULL) != SQLITE_OK || !db) {
        if (db) sqlite3_close(db);
        return -1;
    }
    if (sqlite3_prepare_v2(db, "SELECT sql FROM sqlite_master WHERE name='savedata'", -1, &st, NULL) == SQLITE_OK &&
        sqlite3_step(st) == SQLITE_ROW) {
        const char *text = (const char *)sqlite3_column_text(st, 0);
        n = snprintf(out, cap, "%s", text ? text : "");
        if (n < 0 || (size_t)n >= cap) n = -1;
    }
    if (st) sqlite3_finalize(st);
    sqlite3_close(db);
    return n;
}

int save_db_forget(uint32_t uid, const char *title_id, const char *dir_name) {
    char path[160];
    sqlite3 *db = NULL;
    sqlite3_stmt *st = NULL;
    int n = -1;
    if (!title_id || !dir_name) return -1;
    db_path(uid, path, sizeof path);
    if (sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE, NULL) != SQLITE_OK || !db) {
        if (db) sqlite3_close(db);
        return -1;
    }
    sqlite3_busy_timeout(db, 3000);
    if (sqlite3_prepare_v2(db, "DELETE FROM savedata WHERE title_id=?1 AND dir_name=?2", -1, &st, NULL) == SQLITE_OK) {
        sqlite3_bind_text(st, 1, title_id, -1, SQLITE_STATIC);
        sqlite3_bind_text(st, 2, dir_name, -1, SQLITE_STATIC);
        if (sqlite3_step(st) == SQLITE_DONE) n = sqlite3_changes(db);
    }
    if (st) sqlite3_finalize(st);
    sqlite3_close(db);
    return n;
}
