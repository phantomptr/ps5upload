/* Trophy files that still name an account id the user no longer has (#853).
 *
 * A PS5 user's trophy progress for a PS5 game is /user/home/<uid>/trophy2/nobackup/data/<NPWR..>/
 * TRPTITLE.DAT. The file says whose it is: the account id (8 bytes, big endian) at file offset 0x200
 * and the user id (4 bytes, big endian) at 0x208. Offline activation changes the account id in the
 * registry but not in these files, and the console then stops adding trophies to them. This puts the
 * new id into them.
 *
 * TRPTITLE.DAT ("T2PD" v1): a 0x40-byte header (magic, version 0x10000 at 4, block stride 0x420 as a
 * big-endian u64 at 8, the block count as a big-endian u64 at 0x20), then blocks of 0x400 bytes of data
 * each followed by the SHA-256 of those 0x400 bytes. A changed block gets its hash written again; the
 * console checks them. Layout from KIK-Trophy (core.cjs rewriteDat, which writes the same two fields)
 * and RecompTrophy/psbox's builder, both read from real console files; see trophy_fix.c.
 *
 * Only that one place is ever changed when the old id is 0 or not given. With a known non-zero old id,
 * the same 8 bytes elsewhere in the file's data (either byte order) are changed too. Any other file in
 * the user's trophy folders that holds the old id is only reported: its integrity rules are not known.
 *
 * No Sony call: plain file reads and writes. Every changed file is first copied to
 * <backup_root>/<uid as 8 hex>/<YYYYMMDD-HHMMSS>/<its path under the user's home>, never inside the
 * trophy folders, and replaced through a temp file beside it (same folder, so the rename cannot cross
 * a filesystem), with its mode and owner kept. */
#ifndef PS5UPLOAD_TROPHY_FIX_H
#define PS5UPLOAD_TROPHY_FIX_H

#include <stddef.h>
#include <stdint.h>

#define TROPHY_FIX_HOME_ROOT "/user/home"
#define TROPHY_FIX_BACKUP_ROOT "/data/ps5upload/backup/trophy"

#define T2PD_HEAD 0x40u
#define T2PD_CHUNK 0x400u
#define T2PD_STRIDE 0x420u
#define T2PD_ACCOUNT_AT 0x200u /* file offset: the account id, big endian */
#define T2PD_USER_AT 0x208u    /* file offset: the user id, big endian */
#define TROPHY_FIX_FILE_MAX (4u << 20)

/* What one file came to. */
enum {
    TF_NONE = 0,        /* not a T2PD file and does not hold the old id: not listed */
    TF_CHANGE = 1,      /* holds the old id (or, with no old id given, another one): changed / would be */
    TF_ALREADY = 2,     /* already names the new id */
    TF_OTHER_ID = 3,    /* names an id that is neither the old nor the new one: left alone */
    TF_OTHER_USER = 4,  /* its user id is not this user's: left alone */
    TF_BAD_HASH = 5,    /* a T2PD whose block hashes do not check out: left alone */
    TF_UNKNOWN = 6,     /* not a T2PD, but holds the old id: reported, left alone */
};

typedef struct {
    int state;
    uint64_t from;      /* the id the account field held */
    int anchor;         /* 1 when the account field is (or would be) changed */
    int extra;          /* other places the old id was found and (would be) changed */
    int blocks;         /* block hashes written again */
} trophy_t2pd_result_t;

/* In memory: whether `buf` (a whole file of `n` bytes) is a T2PD file and what the fix does to it.
 * `have_old` 0: the account field is set to `new_id` whatever it holds. With `apply`, `buf` is changed
 * and its hashes written again. Returns r->state. */
int trophy_t2pd_fix(uint8_t *buf, size_t n, uint32_t uid, uint64_t old_id, int have_old,
                    uint64_t new_id, int apply, trophy_t2pd_result_t *r);

/* 1 when `buf` is a T2PD file whose every block hash checks out (or is zero for an all-zero block). */
int trophy_t2pd_valid(const uint8_t *buf, size_t n);

typedef struct {
    const char *home_root;   /* NULL: TROPHY_FIX_HOME_ROOT */
    const char *backup_root; /* NULL: TROPHY_FIX_BACKUP_ROOT */
} trophy_fix_roots_t;

/* The whole fix for one user, answered as JSON into `out`:
 * {"ok":true,"uid":u,"dry_run":b,"old":"0x..."|null,"new":"0x...","backup":"<name>"|"",
 *  "scanned":n,"changed":n,"already":n,"left":n,"failed":n,"files":[{"path","state","from","anchor",
 *  "extra"},...],"more":b}
 * or {"ok":false,"err":"..."}. `stamp` names the backup folder (YYYYMMDD-HHMMSS). Returns the length,
 * or -1 when it did not fit. */
int trophy_fix_run(const trophy_fix_roots_t *roots, uint32_t uid, uint64_t old_id, int have_old,
                   uint64_t new_id, int dry_run, const char *stamp, char *out, size_t cap);

/* The user's backups: {"ok":true,"uid":u,"backups":[{"name","files"},...]} (newest first). */
int trophy_fix_backups_json(const trophy_fix_roots_t *roots, uint32_t uid, char *out, size_t cap);

/* Puts every file of backup `name` back: {"ok":b,"uid":u,"backup":name,"restored":n,"failed":n}. */
int trophy_fix_restore(const trophy_fix_roots_t *roots, uint32_t uid, const char *name, char *out,
                       size_t cap);

#endif /* PS5UPLOAD_TROPHY_FIX_H */
