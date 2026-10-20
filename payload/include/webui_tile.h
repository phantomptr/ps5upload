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
 *   lnk_ours       /user/app/PSUP00001/mount.lnk names WEBUI_TILE_DIR
 *   appmeta        /user/appmeta/PSUP00001 exists
 * NONE: registered this boot, nothing to do. REBIND: registered before, but the bind did not
 * survive a reboot: re-mount only, no Sony call (a full register on FW 13.x runs AppInstallAll,
 * and install records could pile up one per boot). REGISTER: the full register_title_from_path. */
typedef enum { WEBUI_TILE_NONE = 0, WEBUI_TILE_REBIND = 1, WEBUI_TILE_REGISTER = 2 } webui_tile_action_t;
webui_tile_action_t webui_tile_plan(int files_changed, int mount_live, int lnk_ours, int appmeta);

#ifndef WEBUI_TILE_HOST_TEST
/* Writes the folder if missing/different, then registers PSUP00001 when it is not registered
 * this boot (or when a file changed: register copies sce_sys, so the copies must be refreshed).
 * After a reboot, when the title is still ours (webui_tile_plan REBIND), only the nullfs is
 * re-mounted; a failed re-mount falls back to the full register. Logs which path ran.
 * Synchronous (Sony calls): call it from webui_tile_request's thread. 0, or -1 (logged). */
int webui_tile_ensure(uint16_t port);

/* unregister_title(PSUP00001), then deletes the folder and the staged /user/app/PSUP00001 (left
 * there, FW 13.x AppInstallAll would re-install the removed tile on the next folder-game register).
 * Synchronous. 0, or -1 (logged). */
int webui_tile_remove(void);

/* Asks for the tile to be present (tile = 1) or gone (0). Returns at once: one detached worker
 * applies the LATEST request and repeats while new ones arrive, so ensure and remove never run
 * at the same time and a fast on/off/on ends in the last state asked for. Failures are logged. */
int webui_tile_remove_wait(uint16_t port);
void webui_tile_request(int tile, uint16_t port);

/* What the page shows after a tile switch: *busy 1 while the worker still applies a request,
 * *failed 1 when the last ensure/remove it applied failed (the tile is not in the state asked for). */
void webui_tile_state(int *busy, int *failed);
#endif

#endif
