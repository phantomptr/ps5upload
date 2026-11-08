/* The "ps5upload" home-screen tile: a small registered app whose param.json deeplinkUri makes
 * the PS5 open its browser on the console web UI.
 *
 * The folder is /data/ps5upload/tile/PSUP00001/ (WEBUI_TILE_DIR):
 *   sce_sys/param.json   webui_tile_param_json()
 *   sce_sys/icon0.png    the embedded 512x512 icon (build/webui_tile_icon.c, made by xxd from
 *                        payload/assets/webui_tile_icon0.png)
 *   eboot.bin            a few placeholder bytes: register_title_from_path refuses a folder
 *                        without one (register_src_missing_eboot). The tile never runs it; the
 *                        deeplinkUri opens the browser.
 *
 * Registration calls Sony and can take seconds: webui_tile_request() runs it on a short-lived
 * detached thread, never on a web worker or the accept thread. */
#ifndef PS5UPLOAD_WEBUI_TILE_H
#define PS5UPLOAD_WEBUI_TILE_H

#include <stddef.h>
#include <stdint.h>

#define WEBUI_TILE_PARENT "/data/ps5upload/tile"
#define WEBUI_TILE_DIR WEBUI_TILE_PARENT "/PSUP00001"
/* Written after the helper registered the tile, deleted when the helper removes it: while it is
 * there, a tile app.db no longer has was removed outside the helper (#480). Outside the tile
 * folder, which the nullfs exposes and Sony's own Delete may empty. */
#define WEBUI_TILE_MARKER WEBUI_TILE_PARENT "/registered"

/* The embedded icon (generated at build time). */
extern unsigned char webui_tile_icon0_png[];
extern unsigned int webui_tile_icon0_png_len;

/* {"applicationCategoryType":66048,"titleId":"PSUP00001","deeplinkUri":"http://127.0.0.1:<port>/",
 *  "localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"ps5upload"}}}
 * like snprintf: writes at most cap - 1 bytes + NUL, returns the full length. */
size_t webui_tile_param_json(char *out, size_t cap, uint16_t port);

/* Brings `dir` (created, parents too) to the three files above, writing a file only when its
 * bytes differ; each write is a temp file in the same directory renamed over the old one.
 * Returns how many files were (re)written, or -1. */
int webui_tile_write_files(const char *dir, uint16_t port);

/* Deletes `dir` and everything in it (same-directory unlink/rmdir only). Already gone is 0. */
int webui_tile_remove_files(const char *dir);

/* What ensure does, from what it observed (pure; host-tested):
 *   files_changed  webui_tile_write_files rewrote a file (register copies sce_sys: must re-copy)
 *   mount_live     the nullfs at /system_ex/app/PSUP00001 is mounted
 *   lnk_ours       the tracker (/user/app/PSUP00001/ps5upload.lnk, or mount.lnk) names WEBUI_TILE_DIR
 *   appmeta        /user/appmeta/PSUP00001 exists
 *   listed         app.db's install record for it (webui_tile_install_record): 1 / 0 / -1 unreadable
 * NONE: registered this boot, nothing to do. REBIND: registered before, but the bind did not
 * survive a reboot: re-mount only, no Sony call (a full register on FW 13.x runs AppInstallAll,
 * and install records could pile up one per boot). REGISTER: the full register_title_from_path.
 * A bind and a tracker are not a tile: the home screen draws app.db, so `listed` 0 always means
 * REGISTER (before this, a helper restarted in the same boot found the bind and its tracker,
 * logged "already registered" and reported "added" while the console listed no PSUP00001). */
typedef enum { WEBUI_TILE_NONE = 0, WEBUI_TILE_REBIND = 1, WEBUI_TILE_REGISTER = 2 } webui_tile_action_t;
webui_tile_action_t webui_tile_plan(int files_changed, int mount_live, int lnk_ours, int appmeta, int listed);

/* The steps of an ensure, so its order and its checks are host-tested with fakes (webui_tile_ensure
 * passes the console's). */
typedef struct {
    void *ctx;
    int (*write_files)(void *ctx);                    /* webui_tile_write_files: >0 changed, 0 same, -1 */
    int (*mount_live)(void *ctx);
    int (*lnk_ours)(void *ctx);
    int (*appmeta)(void *ctx);
    int (*listed)(void *ctx);                         /* app.db install record: 1 / 0 / -1 unreadable */
    int (*remount)(void *ctx, const char **err);       /* register_remount_nullfs: 0 / -1 */
    int (*register_full)(void *ctx, const char **err); /* register_title_from_path: 0 / -1 */
    void (*sleep_s)(void *ctx, unsigned seconds);
} webui_tile_ensure_ops_t;

/* After a full register, how long app.db is given to list the tile (Sony's installer writes it a
 * moment after it returns: two records 2.2 s apart on FW 13.60 through AppInstallAll). */
#define WEBUI_TILE_LISTED_WAIT_S 10

/* write_files, webui_tile_plan, then nothing / remount (a full register when that fails) / a full
 * register; after a full register, app.db must list the tile within WEBUI_TILE_LISTED_WAIT_S
 * (unreadable counts as fine: never guess). 0, or -1 with *why set to a short reason code:
 * "write_failed", the register's own err (e.g. "register_install_api_error"), or "not_listed"
 * (Sony said yes, but the console never listed it). `*done` (optional) gets the action taken. */
int webui_tile_ensure_with(const webui_tile_ensure_ops_t *ops, webui_tile_action_t *done, const char **why);

/* At helper start, with the tile setting on: 1 when the tile was registered by this helper
 * (`marker`), app.db can be read and has no install record for it (`appdb_row` 1 / 0 / -1
 * unreadable: the tbl_contentinfo row, or any row on firmware without that table; an icon row Sony
 * left behind is hidden by the sweep, not a reason to register again), and it
 * is not this boot's live registration (`mount_live` && `lnk_ours`, when app.db may still lag
 * behind a register). That is a tile the user deleted on the PS5 itself (or a database rebuild
 * dropped): starting the helper must not bring it back (#480). Pure; host-tested. */
int webui_tile_removed_outside(int marker, int appdb_row, int mount_live, int lnk_ours);

/* Whether a removal took the tile off the home screen, from what app.db says afterwards
 * (`appdb_row` 1 / 0 / -1 unreadable), whether Sony's uninstall confirmed it
 * (unregister_tile_confirmed) and whether the leftover icon could be hidden (`hidden`):
 *   GONE    no row (or app.db unreadable and Sony confirmed): delete the tile's files.
 *   HIDDEN  a row Sony would not remove, now hidden (visible = 0): delete the files too; the
 *           hidden row is all that is left and draws nothing.
 *   KEEP    the icon is still there: keep the files, so it stays a working tile instead of a
 *           greyed-out one whose files are gone (#480). Pure; host-tested. */
typedef enum { WEBUI_TILE_GONE = 0, WEBUI_TILE_HIDDEN = 1, WEBUI_TILE_KEEP = 2 } webui_tile_removal_t;
webui_tile_removal_t webui_tile_removal_verdict(int appdb_row, int sony_confirmed, int hidden);

/* The steps of a removal, so the order is host-tested with fakes (webui_tile_remove passes the
 * console's). Each returns 0 on success unless noted. */
typedef struct {
    void *ctx;
    int (*staged)(void *ctx);        /* 1 when /user/app/PSUP00001 exists */
    int (*unregister)(void *ctx, int *confirmed, int *sony_ok); /* unregister_title; *confirmed =
                                      unregister_tile_confirmed, *sony_ok = Sony's call returned 0 */
    int (*appdb_row)(void *ctx);     /* register_appdb_has_title: 1 / 0 / -1 */
    int (*restage)(void *ctx);       /* write the folder and register it again */
    int (*hide)(void *ctx);          /* visible = 0, and the icon keeper keeps it so */
    int (*delete_staged)(void *ctx); /* /user/app/PSUP00001 and /user/appmeta/PSUP00001 */
    int (*delete_dir)(void *ctx);    /* WEBUI_TILE_DIR; refuses (-1) while a nullfs is bound on it */
    int (*unlink_marker)(void *ctx); /* WEBUI_TILE_MARKER */
} webui_tile_remove_ops_t;

/* unregister; while app.db keeps a row: once more if Sony said 0, restage + unregister when the
 * staged files were already gone (an unregister failure after a restage stops there: -1, nothing
 * deleted), then hide; then, unless webui_tile_removal_verdict is KEEP (-1, nothing deleted):
 * delete_staged, delete_dir, unlink_marker. 0 when every step worked. `verdict` (optional) gets
 * the verdict reached (KEEP when the removal stopped before one). */
int webui_tile_remove_with(const webui_tile_remove_ops_t *ops, webui_tile_removal_t *verdict);

/* What the helper last did to the tile, for the page and the PC (#480): "added", "removed" (gone
 * from app.db), "hidden" (the console kept its record; the icon is hidden instead), "failed", or
 * "" before anything ran. */
typedef enum {
    WEBUI_TILE_RESULT_NONE = 0,
    WEBUI_TILE_RESULT_ADDED,
    WEBUI_TILE_RESULT_REMOVED,
    WEBUI_TILE_RESULT_HIDDEN,
    WEBUI_TILE_RESULT_FAILED,
} webui_tile_result_t;
const char *webui_tile_result_name(webui_tile_result_t r);

/* What app.db says about PSUP00001 (content_db_title_presence, reduced to what the tile needs):
 * `rows` 1 / 0 / -1 unreadable; `content` Sony's install record 1 / 0 / -1 no such table;
 * `icons_visible` icon rows the home screen draws. */
typedef struct {
    int rows;
    int content;
    int icons_visible;
} webui_tile_seen_t;

/* What app.db's answer means for "is the tile still installed": Sony's install record
 * (tbl_contentinfo) when app.db has that table, else any row. 1 / 0 / -1 unreadable. */
int webui_tile_install_record(const webui_tile_seen_t *seen);

/* What to do about what is left of the tile (pure; host-tested):
 *   NONE    the saved setting has the tile on (`tile_on`: the normal ensure path owns it, never a
 *           sweep), nothing left, app.db unreadable (never guess), or a removal that failed and
 *           kept a working tile (only a new removal request or the next helper start retries it).
 *   REMOVE  app.db lists it and the tile's own files are still there (a removal that never
 *           finished): run the full removal. Only when `may_remove` (the sweep at helper start).
 *   ZOMBIE  app.db lists it but the tile is not registered and its files are gone: the greyed-out
 *           tile of #480. Sony's uninstall once more when it still has an install record, then hide
 *           the icon in every icon table.
 *   CLEAN   app.db has no row, but files were left behind (staged copy, folder or marker): delete
 *           them, no Sony call (a staged copy would be re-installed by FW 13.x AppInstallAll).
 * `registered`: the nullfs is live and ours. `leftovers`: staged copy, tile folder or marker. */
typedef enum {
    WEBUI_TILE_SWEEP_NONE = 0,
    WEBUI_TILE_SWEEP_REMOVE,
    WEBUI_TILE_SWEEP_ZOMBIE,
    WEBUI_TILE_SWEEP_CLEAN,
} webui_tile_sweep_t;
webui_tile_sweep_t webui_tile_sweep_plan(int tile_on, const webui_tile_seen_t *seen, int registered,
                                         int leftovers, int may_remove);

/* The steps of a zombie fix and of the post-removal watch, host-tested with fakes. */
typedef struct {
    void *ctx;
    int (*tile_on)(void *ctx);                       /* the saved setting (webui.json) has the tile on */
    int (*look)(void *ctx, webui_tile_seen_t *seen); /* read app.db (logs which tables hold it) */
    int (*registered)(void *ctx);                    /* nullfs live and ours */
    int (*leftovers)(void *ctx);                     /* staged copy, tile folder or marker */
    int (*uninstall)(void *ctx);                     /* Sony's uninstall of PSUP00001, 0 = ran */
    int (*hide)(void *ctx);                          /* visible = 0 everywhere + icon keeper, 0 = ok */
    void (*keep_hidden)(void *ctx);                  /* icon keeper: keep PSUP00001 hidden */
    int (*remove)(void *ctx);                        /* the full removal (webui_tile_remove), 0 = ok */
    int (*clean)(void *ctx);                         /* delete the leftovers, no Sony call, 0 = ok */
    int (*wait)(void *ctx, unsigned seconds);        /* sleep; 1 = a new tile request came: stop */
} webui_tile_sweep_ops_t;

/* A greyed-out tile (ZOMBIE): Sony's uninstall once when it still has an install record (and
 * `*uninstalled` says it was not tried yet; set here), look again, then hide every icon still
 * drawn and have the icon keeper keep it hidden. REMOVED when app.db no longer lists it, HIDDEN
 * when only hidden rows are left, FAILED when an icon is still drawn. */
webui_tile_result_t webui_tile_fix_zombie(const webui_tile_sweep_ops_t *ops, int *uninstalled);

/* One sweep step: look, plan (webui_tile_sweep_plan), act. NONE when nothing was done (also for
 * a record Sony kept after the one extra uninstall, with no icon drawn). `*uninstalled` carries
 * "Sony's uninstall was tried once more" from step to step: start it at 0. */
webui_tile_result_t webui_tile_sweep_once(const webui_tile_sweep_ops_t *ops, int may_remove, int *uninstalled);

/* After a removal (or the sweep at helper start), app.db can change for a while yet: the home
 * screen and Sony's installer update it asynchronously, and a row that was gone can come back
 * (#480). Looks again after each of WEBUI_TILE_SETTLE_DELAYS seconds (about two minutes in all)
 * and fixes a zombie whenever one shows up; stops early when `wait` reports a new request.
 * The last non-NONE result, or NONE. */
#define WEBUI_TILE_SETTLE_DELAYS { 3, 5, 10, 15, 25, 30, 30 }
webui_tile_result_t webui_tile_settle_with(const webui_tile_sweep_ops_t *ops, int *uninstalled);

#ifndef WEBUI_TILE_HOST_TEST
/* At helper start with the tile setting on: when webui_tile_removed_outside, deletes the tile's
 * files and marker (no Sony call) and returns 1: the caller saves tile:false instead of
 * registering it again (and then calls webui_tile_sweep_request, which hides an icon the
 * deletion left behind). Else 0. */
int webui_tile_respect_outside_removal(void);

/* At helper start with the tile setting off: on the tile thread, one sweep (webui_tile_sweep_once
 * with may_remove) and then the settle watch. Fixes a console already showing the greyed-out tile
 * as soon as it runs this helper (#480). Returns at once; a tile request supersedes it. */
void webui_tile_sweep_request(void);

/* Writes the folder if missing/different, then registers PSUP00001 when it is not registered
 * this boot (or when a file changed: register copies sce_sys, so the copies must be refreshed).
 * After a reboot, when the title is still ours (webui_tile_plan REBIND), only the nullfs is
 * re-mounted; a failed re-mount falls back to the full register. Logs which path ran.
 * Synchronous (Sony calls): call it from webui_tile_request's thread. 0, or -1 (logged) with *why
 * (optional) set as webui_tile_ensure_with does. */
int webui_tile_ensure(uint16_t port, const char **why);

/* unregister_title(PSUP00001), then checks app.db, which is what the home screen draws: while a row
 * is left it asks Sony once more; a leftover whose staged files were already gone (the greyed-out
 * tile of #480) is restaged and registered so Sony's uninstall has its app to remove; a row Sony
 * still keeps is hidden (tbl_iconinfo visible = 0, like hiding an icon from the PC). Then, unless
 * the icon is still showing (webui_tile_removal_verdict KEEP), deletes the folder, the marker and
 * the staged /user/app/PSUP00001 (left there, FW 13.x AppInstallAll would re-install the removed
 * tile on the next folder-game register). Synchronous. 0, or -1 (logged). */
int webui_tile_remove(void);

/* Waits until the tile thread applied removal request `gen` (webui_tile_request(0, ..)'s return),
 * at most WEBUI_TILE_WAIT_S; not across the settle watch. 0 when removed or hidden; -1 on a
 * failure, a newer request that superseded it, or the time limit. Never call it holding a lock
 * the page's own requests take: they would hang behind the Sony calls (#480). */
#define WEBUI_TILE_WAIT_S 180
int webui_tile_remove_wait(unsigned gen);

/* Asks for the tile to be present (tile = 1) or gone (0). Returns at once: one detached worker
 * applies the LATEST request and repeats while new ones arrive, so ensure and remove never run
 * at the same time and a fast on/off/on ends in the last state asked for. Failures are logged.
 * After a removal the same worker keeps watching app.db (webui_tile_settle_with) until a new
 * request arrives or the watch ends. */
unsigned webui_tile_request(int tile, uint16_t port); /* the request's generation */

/* What the page shows after a tile switch: *busy 1 while the worker still applies a request,
 * *failed 1 when the last ensure/remove it applied failed (the tile is not in the state asked for),
 * *result what the helper last did (webui_tile_result_name; the settle watch can update it later,
 * after *busy went back to 0). `result` may be NULL. */
void webui_tile_state(int *busy, int *failed, webui_tile_result_t *result);

/* Why the last ensure/remove the worker applied failed: a short reason code (static storage; see
 * webui_tile_ensure_with), "" when it did not fail. For the page and the PC ("tile_error"). */
const char *webui_tile_last_error(void);
#endif

#endif
