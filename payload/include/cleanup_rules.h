/* Console clean-up (storage_cleanup.c): the rules that decide what may be removed, kept free of
 * console calls so the host self-test (tests/cleanup_rules_selftest.c) runs the real code.
 *
 * Categories (spec 2026-10-22-console-cleanup-design.md §2.1):
 *   A  update of an installed game     <root>/user/patch/<TID>            Sony AppUnInstallPat
 *   B  update of a game that is gone   <root>/user/patch/<TID>            Sony, then a file delete
 *   C  leftover package file           *.pkg / *.pkg.part on any drive    file delete
 *   D  ps5upload temporary files       <drive>/ps5upload/{pkg_temp,tests}/<name>, /mnt/ps5upload/.unpack/<name>
 *   E  console download file           /user/download/<TID>/download0.dat (title not installed)
 *   F  leftovers of a removed game     <root>/user/appmeta/<TID>, /user/playgo/{app,patch,addcont}/<content id>
 *   G  worth a look                    listed only, never removed here
 * <root> is /user, /mnt/ext0/user or /mnt/ext1/user. */
#ifndef PS5UPLOAD_CLEANUP_RULES_H
#define PS5UPLOAD_CLEANUP_RULES_H

#include <stddef.h>
#include <stdint.h>

/* 1 when `s` is exactly four capital letters and five digits (CUSA07842, PPSA01650, NPXS40140). */
int scr_is_title_id(const char *s);
/* 1 for a system title (NPXS…): never listed, whatever app.db says. */
int scr_is_system_title(const char *tid);
/* The title id inside a content id ("UP9000-CUSA07842_00-SCUS974290000001" -> "CUSA07842").
 * 1 found (out holds 10 bytes), 0 when `cid` is not shaped like one. */
int scr_tid_of_content_id(const char *cid, char out[10]);
/* The add-on label of a content id: the 16 characters after "_00-". 1 found, 0 not. */
int scr_label_of_content_id(const char *cid, char out[17]);

/* Compares dotted versions numerically ("01.04" < "01.10", "1.000.000" == "01.000.000").
 * -1, 0, 1; -2 when either is empty or not a version. */
int scr_version_cmp(const char *a, const char *b);

/* What a package header says. */
typedef struct {
    char content_id[37];
    char title_id[10];
    char version[16];       /* APP_VER (PS4 param.sfo) or contentVersion (PS5 param.json); "" unknown */
    uint32_t content_type;
    uint32_t content_flags;
    char kind;              /* 'b' base, 'p' update, 'd' add-on, 0 unknown */
    int ps5;                /* the PS5 container (\x7FFIH) */
} scr_pkg_info_t;

/* Reads `n` bytes at `off` into `buf`: 0, or -1. */
typedef int (*scr_read_at_fn)(void *ctx, uint64_t off, void *buf, size_t n);
/* Reads the header of a package of `size` bytes. 0 (the content id at least was read), -1 when the
 * bytes are not a package. Never reads more than 64 KiB per piece. */
int scr_pkg_read(scr_read_at_fn read_at, void *ctx, uint64_t size, scr_pkg_info_t *out);

/* A string value of a PARAM.SFO (`key`, NUL-terminated in the file): 1 found, 0 not. */
int scr_sfo_str(const uint8_t *sfo, size_t n, const char *key, char *out, size_t cap);
/* The first string value of "key" in a JSON text: 1 found, 0 not (no unescaping; versions and ids). */
int scr_json_str(const char *json, size_t n, const char *key, char *out, size_t cap);

/* The unsigned integers of the array at top-level-ish key `key` ("item_ids":[1,2,3]): the count put in
 * `out` (at most `max`), or -1 when the key is missing or the array is malformed. */
int scr_parse_id_list(const char *json, size_t n, const char *key, uint32_t *out, int max);

/* The drive a path lives on: "internal", "ext0", "ext1", "usb0"…"usb7", or "" for anything else. */
const char *scr_drive_of(const char *path);

/* Whether `path` is one category `cat` may remove (the apply-time allowlist: the engine and the pages
 * name item ids, never paths, and an item whose path fails this is refused). 1 allowed, 0 not.
 * `tid` is the item's title id (A/B/E/F: the path must carry it). */
int scr_path_allowed(char cat, const char *path, const char *tid);

/* 1 when a path component of `path` is "..", "." or empty (a doubled slash). */
int scr_path_has_dot_parts(const char *path);

/* Names below a drive's root the leftover-package walk never enters (other payloads' folders, the
 * package library, games in homebrew/, Sony's trees on an extended drive). `rel` is the path below
 * the drive root without a leading slash ("ps5upload/pkg_library"); `drive_root` is "/data",
 * "/mnt/ext0", "/mnt/usb0"… 1 = skip. */
int scr_walk_skip(const char *drive_root, const char *rel);

/* Kraken Mount (a payload that mounts games from package files and game folders where they sit):
 * clean-up never removes anything directly in a folder it scans, whether it runs now or not,
 * because those files ARE the user's games (#894). It scans one level deep. */
#define SCR_KRAKEN_ROOTS_MAX 32
#define SCR_KRAKEN_ROOT_LEN 256
/* Its folder: present once it has run on this console. */
#define SCR_KRAKEN_DIR "/data/krakenmount"
#define SCR_KRAKEN_CONFIG "/data/krakenmount/config.toml"

/* The `scanpath` lines of its config.toml (a string or an array of strings on one line, `#`
 * comments, trailing slashes dropped, absolute paths only). They replace its default roots when
 * there are any. The count put in `out` (at most `max`). */
int scr_kraken_scanpaths(const char *toml, size_t n, char out[][SCR_KRAKEN_ROOT_LEN], int max);

/* 1 when `dir` is one of its default roots: /data/homebrew, /data/etaHEN/games, each extended and
 * USB drive's homebrew and etaHEN/games, and the extended and USB drives' own roots. */
int scr_kraken_default_root(const char *dir);

/* 1 when `path` sits directly in a folder Kraken Mount scans: one of `roots` (its scanpath lines),
 * or one of its default roots when `nroots` is 0. /user/data and /data are the same folder. */
int scr_kraken_keeps(const char *path, const char (*roots)[SCR_KRAKEN_ROOT_LEN], int nroots);

/* 1 for what it mounts: a folder or a .pkg file (any case). A .pkg.part, a disk image or any other
 * file in its roots is not its game. */
int scr_kraken_mountable(const char *path, int is_dir);

#endif
