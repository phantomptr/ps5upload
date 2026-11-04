/* The console web UI's JSON API (Home, Files): the webui_route_fn main.c hands to webui_start.
 *
 * Every route is POST with a JSON body and adapts it to a method the helper already serves
 * (management through mgmt_rpc_dispatch, the data layer through ava1_data_rpc, both reached the
 * way the AVA1 server reaches them), under one fixed identity: the console page owns the jobs it
 * starts as "ps5upload-console-webui". The route table in webui_api.c is the allowlist: any other
 * path answers 404, a known path with another method 405.
 *
 * POST /api/input-log {"events":["<line>",...]} is the one route that reaches no helper method: it
 * appends the page's controller-input measurements to WEBUI_INPUTLOG_PATH (webui_inputlog.h),
 * bodies over WEBUI_INPUTLOG_BODY_MAX answer 413, calls closer than 500 ms apart 429.
 *
 * Three GET routes sit outside the table: GET / serves the embedded page (webui_page.h),
 * GET /api/token answers {"token":"<hex>"} and GET /strings/<lang>.json one language's embedded
 * strings (webui_strings.h; an id not in that table answers 404), all after the server's
 * Host/Origin check only. */
#ifndef PS5UPLOAD_WEBUI_API_H
#define PS5UPLOAD_WEBUI_API_H

#include <stddef.h>
#include <stdint.h>

#include "webui_http.h"
#include "webui_settings.h"

/* The settings the helper started with (main.c loads WEBUI_SETTINGS_PATH before webui_start):
 * /api/settings answers them and /api/settings/set changes them. `saved`: the file was there and
 * read (webui_settings_load returned 0). Call once, before webui_start. */
void webui_api_settings_init(const webui_settings_t *s, int saved);

/* Starts the :9180 server with the saved LAN setting (call after webui_api_settings_init). 0 when
 * it runs, -1 when it could not start (logged). */
int webui_api_server_start(void);

/* A paired PC unregistered the tile (APP_UNREGISTER of WEBUI_TILE_TITLE_ID): the same as Settings ->
 * tile off. Saves tile:false to WEBUI_SETTINGS_PATH (so the next start does not register it again)
 * and waits for the tile worker to finish removing it. Returns 0 only after persistence and
 * confirmed Sony removal plus folder cleanup; -1 on any failure. */
int webui_api_tile_off(void);

/* The apps' view of what the console page started (mgmt console.tasks / console.task_cancel, #658).
 * Reply JSON into out, its length. console.tasks: {"ok":true,"tasks":[console_tasks.h],"smb_jobs":
 * [the SMB daemon's jobs, as /api/smb/jobs lists them],"now":<unix>}. console.task_cancel {"id":..},
 * a hex job id of the page's or "smb:"<id>: {"ok":true} or {"ok":false,"err":"bad_job"|"unknown_job"|
 * a daemon token}. A paired PC's job is not in the table, so it can be neither read nor stopped here. */
int webui_console_tasks(char *out, size_t cap);
int webui_console_task_cancel(const char *b, size_t n, char *out, size_t cap);

/* At helper start, the tile was removed outside the helper (webui_tile_respect_outside_removal):
 * saves tile:false so the page and the PC show it off. No tile request. */
void webui_api_tile_forget(void);

/* Paired-helper Minion get/set: {tile?, enabled?, lan?, remove?}. enabled:false turns PS5Upload
 * Minion off (tile removed and kept removed, :9180 stopped; the tile setting is kept for when it is
 * turned back on), enabled:true starts the server again at once and restores the tile when its
 * setting is on. lan: the page on the LAN too, or loopback only (rebinds the listener). remove:true
 * is enabled:false plus deleting Minion's own files on the console (webui_minion_forget);
 * webui.json stays so the next start keeps it off. Reply {tile, enabled, lan, saved, tile_busy,
 * tile_failed, tile_result, port, version, removed?}: `saved` false = no webui.json yet (defaults);
 * `removed` (remove only) = how many leftovers were deleted. JSON reply length, or -1. */
int webui_api_tile(const char *body, size_t len, char *out, size_t cap);

int webui_api_route(const webui_req_t *r, const uint8_t *body, size_t body_len, int fd);

/* The streamed routes (webui_server.h webui_stream_fn): POST /api/fs/save-text (a text file of up to
 * 1 MiB: one JSON line, '\n', the text) and POST /api/upload/data?id= (an upload's bytes, after
 * /api/upload/begin). GET /dl/<ticket>/<name> (after /api/download/begin) is served by
 * webui_api_route. None of them reaches a helper method. */
int webui_api_stream_for(const webui_req_t *r);
int webui_api_stream(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd);

#endif
