/* The console web UI's settings file: {"lan":false,"tile":true,"enabled":true,"start_cheats":false}.
 *
 * lan:     0 serves the page on 127.0.0.1 only (the PS5's own browser); 1 also on the LAN.
 * tile:    1 keeps the "ps5upload" home-screen tile (webui_tile.c) registered; 0 removes it.
 * enabled: 0 turns PS5Upload Minion off as a whole (#480): the :9180 page is not served and the
 *          tile is removed and kept removed, whatever `tile` says; `tile` is kept so turning Minion
 *          back on restores the tile as it was. A file without the key (older helpers) is enabled.
 * start_cheats: 1 opens the page on Cheats (the running game's) instead of Home (#584). Only the
 *          page reads it. A boolean like the rest, so an older helper reading the file ignores it
 *          instead of reading the whole file as garbled.
 *
 * Pure file I/O (host-tested in payload/tests/webui_settings_selftest.c). Callers serialize
 * saves to one path themselves (webui_api.c holds its settings lock across a save). */
#ifndef PS5UPLOAD_WEBUI_SETTINGS_H
#define PS5UPLOAD_WEBUI_SETTINGS_H

#include <stddef.h>

#define WEBUI_SETTINGS_PATH "/data/ps5upload/webui.json"

typedef struct {
    int lan;
    int tile;
    int enabled;
    int start_cheats;
} webui_settings_t;

/* lan = 0, tile = 1, enabled = 1, start_cheats = 0. */
void webui_settings_defaults(webui_settings_t *out);

/* Whether the tile should be on the home screen: Minion on and its tile setting on. */
int webui_settings_tile_wanted(const webui_settings_t *s);

/* What the helper does with Minion at start, from the saved settings (pure; host-tested):
 *   serve      start the :9180 server (only when enabled)
 *   tile_add   register the tile (or keep it registered): enabled and tile on. The caller first
 *              checks whether the user deleted it on the PS5 itself (webui_tile_respect_outside_removal)
 *              and, if so, plans again with tile off.
 *   tile_sweep remove whatever is left of the tile and hide a greyed-out icon (#480): every start
 *              where the tile is not wanted, so a Minion that is off stays without a tile. */
typedef struct {
    int serve;
    int tile_add;
    int tile_sweep;
} webui_startup_t;
webui_startup_t webui_startup_plan(const webui_settings_t *s);

/* Fills *out from the file. Returns 0 when the file was read; 1 when it is missing or garbled
 * (not one JSON object, or a key present with a value other than true/false): *out is then the
 * defaults, whole. A valid object that lacks a key keeps that key's default. Never fails. */
int webui_settings_load(const char *path, webui_settings_t *out);

/* Writes the file atomically: `<path>.tmp` in the same directory, fsync, then rename() over
 * `path` within that directory (never across mounts). A failed save leaves the previous file
 * as it was and no temp file behind. 0, or -1. */
int webui_settings_save(const char *path, const webui_settings_t *s);

/* "Remove PS5Upload Minion" (webui_api_tile remove:true): deletes each path (a file, or a folder up
 * to four levels deep). *removed: how many were there
 * and are gone. 0 when nothing is left of any of them, -1 when one could not be removed. */
int webui_minion_forget(const char *const *paths, size_t n, int *removed);

#endif
