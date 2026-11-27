/* The game library scan (#498): walks a share, or the console's own drives, for games and writes
 * what it found to the library cache the console page reads (include/smbd_proto.h SMBD_LIB_DIR):
 *
 *   <dir>/<source>.json   {"source","scanned","complete","stopped","items":[item...]}
 *   <dir>/icons/<source>-<stamp>-<n>.png  the covers those items name
 *
 *   item = {"kind":"pkg"|"folder"|"image","path","name","title","title_id","content_id","version",
 *           "category","type":"game"|"patch"|"dlc"|"app"|"other","platform","fw","sdk","backport","fake",
 *           "size","icon"[,"meta","mtime"]} ("sdk" the SDK it was built with, "backport" a folder holding fakelib/; "icon" is a file name in icons/, "" when there is none; "path" is the path on
 *           the side scanned: share-relative for a share, absolute for the console; "meta" and
 *           "mtime" on an image only: "read" when its own param file named it, else why it could
 *           not be read (smbd_image.h), and the file's mtime)
 *
 * What counts as a game:
 *   - a .pkg whose header reads (pkg_info.c: title, ids, version, type, firmware, its icon0.png);
 *   - a folder holding sce_sys/param.json (PS5) or sce_sys/param.sfo (PS4): a folder dump (its
 *     icon is sce_sys/icon0.png; it is never walked into);
 *   - a disk image (.exfat, .ffpkg, .ffpfs, .ffpfsc): named by the sce_sys/param.json (or
 *     param.sfo) inside it and covered by its icon0.png, read in place with a few small reads
 *     (smbd_image.h, #963). One that cannot be read (a compressed .ffpfsc) keeps the title id in
 *     its name ("PPSA01234 Game.exfat") and its "meta" says why. An image whose size and mtime
 *     match the last result's is taken from it, not read again, so a rescan of a big share
 *     reads only what is new or changed.
 *
 * Bounded, because a share can be huge: LIB_DEPTH_MAX folders deep, LIB_ENTRIES_MAX entries looked
 * at, LIB_DIR_ENTRIES_MAX per folder, LIB_ITEMS_MAX items, LIB_TIME_MAX_S, covers up to
 * LIB_ICON_MAX each and LIB_ICON_BUDGET together. Hitting one ends the walk with "stopped" naming
 * it ("depth" only skips that folder); what was found so far is kept. Hidden entries (".x"),
 * macOS / NAS metadata folders and the helper's own data folder are skipped.
 *
 * The previous result stays in place until the new one is written whole (temp file + rename in the
 * same folder); the old scan's covers are then removed. */
#ifndef PS5UPLOAD_SMBD_LIBRARY_H
#define PS5UPLOAD_SMBD_LIBRARY_H

#include <stddef.h>
#include <stdint.h>

#include "smbd_copy.h"

#define LIB_DEPTH_MAX 6
#define LIB_ENTRIES_MAX 20000u
#define LIB_DIR_ENTRIES_MAX 5000u
#define LIB_ITEMS_MAX 500u
#define LIB_TIME_MAX_S 600
#define LIB_ICON_MAX (1024u * 1024u)
#define LIB_ICON_BUDGET (128ull * 1024ull * 1024ull)
#define LIB_SOURCE_MAX 32u
#define LIB_RESULT_MAX (4u * 1024u * 1024u)

typedef struct {
    unsigned depth_max;
    unsigned entries_max;
    unsigned items_max;
    int time_max_s;
    uint64_t icon_budget;
} lib_limits_t;

/* A source id: 1..LIB_SOURCE_MAX-1 of [A-Za-z0-9_-]. */
int lib_source_ok(const char *source);

/* Walks `roots` ('\n'-separated paths on `fs`; a root that is not there is skipped) and writes
 * `out_dir`/<source>.json and its covers. Progress: pr->files_done counts entries looked at,
 * pr->files_total items found, pr->current the folder being read; pr->cancel stops it (nothing is
 * written then). `skip_prefix` (may be NULL) is a path never walked into (the helper's own data).
 * `lim` NULL = the LIB_* defaults. 0, or a negative xerr_t. */
int lib_scan(xfs_t *fs, const char *source, const char *roots, const char *skip_prefix, const char *out_dir,
             const lib_limits_t *lim, copy_progress_t *pr);

/* A title id ("PPSA01234", "CUSA12345") inside a file name, into out (10 bytes); 1 found, 0 not. */
int lib_title_id_in(const char *name, char out[10]);

#endif
