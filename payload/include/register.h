#ifndef PS5UPLOAD2_REGISTER_H
#define PS5UPLOAD2_REGISTER_H

#include <stddef.h>

/*
 * App registration pipeline -- takes a game folder on PS5 (either a
 * real directory under /data/homebrew/ or /mnt/extN/, OR content
 * inside a mounted .exfat at /mnt/ps5upload/<name>/) and registers
 * it in the XMB so it can be launched.
 *
 * Pipeline per register_title_from_path():
 *   1. Read sce_sys/param.json or sce_sys/param.sfo to derive
 *      title_id and title_name.
 *   2. Copy sce_sys/ into /user/app/<title_id>/sce_sys/
 *   3. Copy icon0.png into /user/app/<title_id>/icon0.png (top-level)
 *   4. Copy param.* + .png + .dds + .at9 into /user/appmeta/<title_id>/
 *   5. nmount nullfs { from = src_path, fspath = /system_ex/app/<title_id> }
 *      so Sony's launcher sees the game content at its expected path
 *      without physically copying gigabytes of data.
 *   6. Write tracking links /user/app/<title_id>/mount.lnk (= src_path)
 *      so we can unmount later even after a payload reload.
 *   7. dlopen libSceAppInstUtil.sprx and call
 *      sceAppInstUtilAppInstallTitleDir(title_id, "/user/app/", 0).
 *      Sony handles the app.db row itself; we never touch the schema.
 *   8. If param.json or param.sfo supplied an snd0.at9, patch snd0info
 *      on the newly-registered row via a single SQL UPDATE (the ONLY
 *      direct app.db write we do).
 *
 * unregister_title() reverses steps 5 and 6. It does NOT try to remove
 * the app.db row — the tile stays in XMB but points at nothing, which
 * is safer than risking a malformed DELETE that could lock the XMB.
 *
 * launch_title() dlopens libSceLncService.sprx and calls
 * sceLncUtilLaunchApp(title_id, NULL, NULL) so the user can Run from
 * the app instead of switching to XMB.
 *
 * Firmware compatibility: sceAppInstUtilAppInstallTitleDir is not
 * exported on firmware 12.x (Sony moved to a queue-based installer).
 * register_title_from_path() returns -1 with reason
 * "register_install_api_unavailable" on 12+ ; document as a known
 * limitation. A queue-based batch installer path could address this
 * in the future.
 */

/* Maximum title-id and title-name lengths. */
#define REGISTER_MAX_TITLE_ID    16   /* "CUSA00xxx" or "PPSA00xxx" fits in 10 */
#define REGISTER_MAX_TITLE_NAME  128

/* All four public APIs return 0 on success, -1 on failure.
 * On -1 they set *err_reason_out (if non-NULL) to a pointer to a
 * static, NUL-terminated short error string suitable for sending back
 * over the ERROR frame. Callers must NOT free the string. */

/* Register a title from a source folder (any path the payload's
 * is_path_allowed accepts, including /mnt/ps5upload/<name>/<game>/).
 * On success, *out_title_id and *out_title_name are populated and
 * *out_used_nullfs is set to 1 (always 1 today; reserved for future
 * alternate pipelines).
 *
 * `patch_drm_type` (added in 2.2.26) optionally rewrites the
 * source's `sce_sys/param.json` so `applicationDrmType` is set to
 * `"standard"`. Some PSN-extracted dumps ship with the field set to
 * `"PSN"` or `"disc"`, which Sony's launcher rejects on a userland
 * dump. INVASIVE — modifies the user's source file in place — so
 * it's opt-in. Pass 0 to leave `param.json` alone, 1 to patch when
 * needed. The patch only writes when the existing value differs
 * from `"standard"`; if the param.json is already correct, no I/O
 * is done. Mounted images must be RW; on a read-only mount the
 * write fails and the function returns the
 * `register_drm_patch_failed` error, leaving the rest of the
 * registration aborted before any state is created.
 *
 * Idempotent: calling twice with the same src_path is safe. Sony's
 * installer returns 0x80990002 ("restored") which we normalise to
 * success. */
int register_title_from_path(const char *src_path,
                             int patch_drm_type,
                             char *out_title_id,
                             char *out_title_name,
                             int *out_used_nullfs,
                             const char **err_reason_out);

/* Reverse of register_title_from_path. Best-effort: always unmounts
 * the nullfs at /system_ex/app/<title_id>, removes tracking files,
 * and (if available) calls sceAppInstUtilAppUninstall to clear the
 * XMB tile. Returns 0 if the unmount succeeded (tile removal is
 * optional and reported via the shared log, not the return). */
int unregister_title(const char *title_id,
                     const char **err_reason_out);

/* Launch an already-registered title via sceLncUtilLaunchApp. The title
 * must already be in app.db; this does not register. On firmware where
 * the service is unavailable, returns -1 with reason
 * "launch_service_unavailable". */
int launch_title(const char *title_id,
                 const char **err_reason_out);

/* Fill out_json (pre-allocated, at least out_cap bytes) with a JSON
 * object shaped like:
 *   {"apps":[{"title_id":"PPSA00123","title_name":"Foo",
 *             "src":"/mnt/ps5upload/foo","image_backed":true}, ...]}
 * where src is the path written into /user/app/<title_id>/mount.lnk
 * (empty if we did not manage the registration) and image_backed is
 * true if a mount_img.lnk is present alongside (meaning the source
 * lives inside a mounted disk image that must stay mounted for the
 * title to stay launchable).
 *
 * Sets *out_written to the JSON byte count on success. Returns -1 on
 * any failure (sqlite unavailable, app.db read error, buffer too
 * small). */
int list_registered_titles_json(char *out_json,
                                size_t out_cap,
                                size_t *out_written,
                                const char **err_reason_out);

/* Open the PS5's built-in web browser via sceSystemServiceLaunchApp
 * on the NPXS20001 title_id. Returns 0 on success, -1 if the launch
 * service isn't available on this firmware. */
int register_browser_launch(void);

/* Called once at payload init. Resolves dlopen handles and function
 * pointers for libSceAppInstUtil, libSceLncService, and libsqlite3.
 * Missing libraries are tolerated; individual APIs will fail with
 * a specific err_reason when invoked, not at init. */
void register_module_init(void);

/* Call once at payload startup FROM THE MAIN THREAD, before the mgmt
 * HTTP listener starts spawning handler threads. Invokes
 * sceUserServiceInitialize + sceAppInstUtilInitialize + sceLncUtilInitialize
 * so Sony's services are established in main-thread context.
 *
 * Earlier versions lazy-initialized these from spawned HTTP handler
 * threads, which on firmware 9.60 causes
 * sceAppInstUtilAppInstallTitleDir to deadlock inside Sony's kernel
 * stub (suspected: per-process lock expects main-thread ownership).
 *
 * Idempotent and failure-tolerant — if a Sony init returns non-zero
 * or the symbol isn't present, we log and continue so the rest of
 * the payload (transfer, mount, fs ops) still comes up. The register
 * / launch / uninstall paths themselves still guard on symbol
 * availability, so a missing init is surfaced as a clean error
 * rather than a hang. */
void register_services_init(void);

#endif /* PS5UPLOAD2_REGISTER_H */
