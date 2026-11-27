/* Putting an installed package game back on the home screen (#961).
 *
 * What happens to the user: after Settings › Rebuild Database (Safe Mode, where no fake-package
 * support is loaded), every game installed from a fake package disappears from the home screen
 * and from Settings › Storage, but its files stay: <volume>/user/app/<id>/app.pkg (internal, USB
 * extended storage /mnt/ext0, M.2 /mnt/ext1). Only a full reinstall brought them back.
 *
 * What this does instead: hand the package that is already on the console to the console's own
 * installer again, the same as installing it the first time, so Sony's code writes every record.
 * Nothing here writes app.db or appinfo.db.
 *
 * The package cannot be installed from where it sits: the installer writes <volume>/user/app/<id>
 * while it reads its source, and it clears an old install before writing the new one (the engine
 * refuses an install whose source is an installed game's own file for that reason). So:
 *
 *   check    read-only: is this title restorable, and why not if not.
 *   prepare  back up app.db and appinfo.db, then MOVE app.pkg (and the title's update, patch.pkg,
 *            if it is on the same drive) aside on the SAME drive: <volume>/ps5upload/restore/<id>/
 *            (/user/ps5upload/restore/<id> for the internal drive). A same-device rename only
 *            (cross_device.h: a rename across devices panics this kernel), so no copy and no extra
 *            space. A record (restore.txt) is written there first, so an interrupted restore is
 *            found again. The engine then installs <aside>/app.pkg.
 *   finish   after the install ended (TITLE_RESTORE_INSTALL_ENDED). When app.db has the title and
 *            an app.pkg exists in a <volume>/user/app/<id> again: the copy aside is deleted and the
 *            update put back where it was. With TITLE_RESTORE_ROLLBACK (the install failed): the
 *            package and the update go back where they were, only if the title is still not in
 *            app.db and the place is still free.
 *            Anything else (the install still running, the database unreadable, the place taken)
 *            keeps every file and says so.
 *
 * Refused, before anything is touched: a title id that is not one, a system app, a title app.db
 * knows (it IS on the home screen: never touched), a title appinfo.db has a record for (the
 * console knows it somewhere: never touched), a folder registration (mount.lnk: not a package),
 * the same title's app.pkg on two drives, a package whose header says retail-signed, no drive with
 * room for the copy the installer writes, an aside folder on another device, or a database that
 * could not be read ("can't tell" is never "not there").
 *
 * The file operations and the decisions are here; the database answers come through callbacks so
 * the host self-test runs the whole flow on a scratch folder (payload/tests/title_restore_selftest.c). */
#ifndef PS5UPLOAD_TITLE_RESTORE_H
#define PS5UPLOAD_TITLE_RESTORE_H

#include <stddef.h>
#include <stdint.h>

#define TITLE_RESTORE_BACKUP_ROOT "/data/ps5upload/backup/restore"
#define TITLE_RESTORE_RECORD "restore.txt"
/* At most this many volumes are looked at: internal, ext0, ext1 and the mount table's others. */
#define TITLE_RESTORE_MAX_VOLUMES 12

typedef struct {
    /* Prepended to every console path for file operations ("" or NULL on the console; a scratch
     * folder in the host test). Replies always name console paths. */
    const char *prefix;
    /* NULL: TITLE_RESTORE_BACKUP_ROOT (the prefix applies). */
    const char *backup_root;
    /* Mount points from the mount table; only /mnt/ext<N> ones count, ext0/ext1 are always looked at. */
    const char *const *extra_volumes;
    int nextra;
    /* app.db / appinfo.db: 1 the title has a row, 0 none, -1 could not be read. */
    int (*appdb_has)(const char *title_id);
    int (*appinfo_has)(const char *title_id);
    /* Copy the database `which` ("app.db" or "appinfo.db") to the new file `dst` (prefix applied
     * already). 0, or -1 with a reason in err. */
    int (*backup_db)(const char *which, const char *dst, char *err, size_t errcap);
    /* Free bytes for a writer under `dir` (prefix applied). NULL: statvfs. 0, or -1. */
    int (*free_bytes)(const char *dir, uint64_t *out);
} title_restore_env_t;

/* finish's flags. INSTALL_ENDED: the caller knows the install of the copy aside has ended (the
 * engine's install job reached its verdict, or the user saw the game start); only then is the copy
 * aside deleted. ROLLBACK: it ended without putting the game back, so the files go back where they
 * were (implies the install ended: a running install could write into the place). */
#define TITLE_RESTORE_ROLLBACK 1
#define TITLE_RESTORE_INSTALL_ENDED 2

/* op "check" | "prepare" | "finish" (with `flags`). `stamp` names the backup folder (prepare;
 * "20261126-101500"). Writes one JSON object into out and returns its length, or -1 when it did
 * not fit.
 *
 * Every reply has "ok" and "state":
 *   check:   restorable | registered | record_elsewhere | not_installed | pending |
 *            folder_registration | ambiguous | retail | no_room | aside_cross_device |
 *            db_unreadable | invalid_title | system_title  (ok:true: an answer, not a failure)
 *   prepare: prepared (ok:true), else ok:false with err "restore_<check state>" or
 *            restore_backup_failed / restore_move_failed / restore_aside_busy
 *   finish:  restored | rolled_back | nothing_pending (ok:true);
 *            waiting | unsettled (ok:true, files kept; "reason" says why) */
int title_restore_run(const title_restore_env_t *env, const char *op, const char *title_id,
                      int flags, const char *stamp, char *out, size_t cap);

#endif /* PS5UPLOAD_TITLE_RESTORE_H */
