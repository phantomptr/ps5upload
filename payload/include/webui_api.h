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
 * /api/settings answers them and /api/settings/set changes them. Call once, before webui_start. */
void webui_api_settings_init(const webui_settings_t *s);

/* A paired PC unregistered the tile (APP_UNREGISTER of WEBUI_TILE_TITLE_ID): the same as Settings ->
 * tile off. Saves tile:false to WEBUI_SETTINGS_PATH (so the next start does not register it again)
 * and waits for the tile worker to finish removing it. Returns 0 only after persistence and
 * confirmed Sony removal plus folder cleanup; -1 on any failure. */
int webui_api_tile_off(void);

/* Paired-helper tile get/set; never changes LAN access. JSON reply length, or -1. */
int webui_api_tile(const char *body, size_t len, char *out, size_t cap);

int webui_api_route(const webui_req_t *r, const uint8_t *body, size_t body_len, int fd);

/* The streamed routes (webui_server.h webui_stream_fn): POST /api/fs/save-text (a text file of up to
 * 1 MiB: one JSON line, '\n', the text) and POST /api/upload/data?id= (an upload's bytes, after
 * /api/upload/begin). GET /dl/<ticket>/<name> (after /api/download/begin) is served by
 * webui_api_route. None of them reaches a helper method. */
int webui_api_stream_for(const webui_req_t *r);
int webui_api_stream(const webui_req_t *r, const uint8_t *pre, size_t pre_len, int fd);

#endif
