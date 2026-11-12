/* Whose save it is (#809): a PS5 save's owner is data, not encryption. The image opens on any
 * console; the account and user it belongs to are in its param.sfo (ACCOUNT_ID, USER_ID), which
 * for most PS5 saves lives in a protected block in the image header and not in the file inside
 * the save, and the console lists a save from a row in the user's savedata.db.
 *
 * save_sfo_* (save_sfo.c) read and patch a param.sfo in memory: no console calls, host-tested.
 * save_param_*, save_owner_account and save_db_* (save_owner.c) are the console side. */
#ifndef PS5UPLOAD_SAVE_OWNER_H
#define PS5UPLOAD_SAVE_OWNER_H

#include <stddef.h>
#include <stdint.h>

/* The protected block is 0x8000 bytes: a 20-byte wrapper, then the param.sfo. */
#define SAVE_PARAM_BLOB_BYTES 0x8000u
#define SAVE_PARAM_HEAD_BYTES 20u
#define SAVE_SFO_MAX (SAVE_PARAM_BLOB_BYTES - SAVE_PARAM_HEAD_BYTES)

typedef struct {
    char title_id[16];
    char dir_name[40];
    char main_title[132];
    char sub_title[132];
    char detail[1028];
    uint64_t blocks;        /* SAVEDATA_BLOCKS */
    uint64_t system_blocks; /* SYSTEM_BLOCKS */
    uint64_t account_id;
    uint32_t user_id;
    uint32_t list_param; /* SAVEDATA_LIST_PARAM, else the first 4 bytes of PARAMS */
    int has_account;     /* an ACCOUNT_ID field of 8 bytes or more */
    int has_user;        /* a USER_ID field, or a PARAMS record holding one at +4 */
} save_sfo_info_t;

/* 1 when `buf` is a param.sfo whose index and every value lie inside `size`. */
int save_sfo_valid(const uint8_t *buf, size_t size);
/* Fills `out` from a valid param.sfo. 0, or -1 when it is not one. */
int save_sfo_info(const uint8_t *buf, size_t size, save_sfo_info_t *out);
/* Writes the owner into a valid param.sfo in place: ACCOUNT_ID, and the user id in USER_ID or
 * PARAMS+4 (whichever it has). 0; -1 not a param.sfo; -2 no ACCOUNT_ID field; -3 no user field. */
int save_sfo_set_owner(uint8_t *buf, size_t size, uint64_t account_id, uint32_t user_id);
/* The field names, space-separated, for diagnostics. Returns the length written. */
size_t save_sfo_keys(const uint8_t *buf, size_t size, char *out, size_t cap);

/* The param.sfo in the protected block of the save image at `image`. Returns its size; 0 when
 * the image has none (its param.sfo is the file inside the save); -errno when no call answered.
 * `how` says which device and command answered, or what each refused with. */
int save_param_read(const char *image, uint8_t *sfo, uint32_t cap, char *how, size_t how_sz);
/* Writes `sfo` as the protected block and reads it back. 0, or -errno (-EIO: read back differs). */
int save_param_write(const char *image, const uint8_t *sfo, uint32_t size, char *how, size_t how_sz);

/* The account id of console user `uid` (its registry slot, found by the user's name).
 * 0; -1 the user has no name; -2 no account slot carries that name; -3 the slot has no id. */
int save_owner_account(uint32_t uid, uint64_t *account_id);

/* Usage of an open save, for its database row. */
typedef struct {
    uint64_t blocks;      /* capacity, in the save filesystem's blocks */
    uint64_t free_blocks; /* of those, unused */
    uint64_t size_kib;    /* the image file, in KiB */
} save_usage_t;

/* Adds or updates the row of the save in user `uid`'s savedata.db. Returns 1 added, 2 updated,
 * or -1 with `err` filled. */
int save_db_upsert(uint32_t uid, const save_sfo_info_t *info, const save_usage_t *usage, char *err,
                   size_t err_sz);
/* The rows of `title_id` in user `uid`'s savedata.db as a JSON array (for diagnostics). Returns
 * the length written, or -1 (no database, or it does not fit). */
int save_db_rows_json(uint32_t uid, const char *title_id, char *out, size_t cap);
/* Removes the row of save `dir_name` of `title_id` from the user's savedata.db: the undo of a row
 * that was added. Rows removed (0 or 1), or -1. */
int save_db_forget(uint32_t uid, const char *title_id, const char *dir_name);
/* The CREATE TABLE text of the user's savedata table (for diagnostics). Length, or -1. */
int save_db_schema(uint32_t uid, char *out, size_t cap);

#endif
