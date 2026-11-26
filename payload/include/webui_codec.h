/* Pure JSON <-> AVA1 adapters for the console web UI's API routes (webui_api.c).
 *
 * No I/O, no globals, no allocation: everything here is host-tested
 * (payload/tests/webui_codec_selftest.c). Request bodies are NOT NUL-terminated;
 * every reader takes (json, n). Only TOP-LEVEL keys of a JSON object match: a key
 * inside a nested object or inside a string never does. */
#ifndef PS5UPLOAD_WEBUI_CODEC_H
#define PS5UPLOAD_WEBUI_CODEC_H

#include <stddef.h>
#include <stdint.h>

/* The string value of top-level `key`, unescaped into `out` as UTF-8 (\" \\ \/ \b \f \n
 * \r \t and \uXXXX, surrogate pairs joined) and NUL-terminated.
 * Returns 1 found, 0 absent (or the body is not a JSON object), -1 present but unusable:
 * not a string, a bad escape, a lone surrogate, a \u0000 (it would cut a path short), a raw
 * control character, or longer than cap - 1 bytes. */
int webui_json_str(const char *json, size_t n, const char *key, char *out, size_t cap);
/* An unsigned integer value: 1 found, 0 absent, -1 present but not a non-negative integer
 * that fits 64 bits (a string, a fraction, an exponent, a sign). */
int webui_json_u64(const char *json, size_t n, const char *key, uint64_t *out);
/* true / false: 1 found (*out 1 or 0), 0 absent, -1 any other value. */
int webui_json_bool(const char *json, size_t n, const char *key, int *out);

/* 1 when top-level `key` is present with the value null, else 0. */
int webui_json_null(const char *json, size_t n, const char *key);

/* The top-level `key`'s array value as a span: *val at its '[', *len through its ']' (brackets
 * balanced outside strings). 1 found, 0 absent, -1 present but not a complete array. */
int webui_json_array(const char *json, size_t n, const char *key, const char **val, size_t *len);

/* The next string of the array span `arr` (from webui_json_array): *pos starts at 0 and is moved
 * past each element. 1 a string (unescaped as webui_json_str does), 0 the array ended, -1 an element
 * that is not a usable string or the array is malformed (stop there). */
int webui_json_array_str(const char *arr, size_t len, size_t *pos, char *out, size_t cap);

/* Escapes n bytes of `s` as the inside of a JSON string (no quotes added): `"` and `\` are
 * backslashed, control characters become \n \r \t \b \f or \u00XX, valid UTF-8 is copied as
 * is, and every byte that is not part of a valid UTF-8 sequence becomes �, so the result
 * is valid JSON whatever the input. Writes at most cap - 1 bytes plus a NUL (when cap > 0) and
 * returns the full escaped length, like snprintf: a return >= cap means it did not fit. */
size_t webui_json_escape(char *out, size_t cap, const char *s, size_t n);

/* An encoded ava1_fs_list_result_t as
 *   {"entries":[{"name":..,"kind":"file"|"dir"|"link"|"other","size":N,"mtime":N|null},...],
 *    "more":bool,"next":N}
 * Entries that do not fit `cap` are left out and "more" is set; "next" is the directory
 * position after the last entry written (the next page's offset; not a count), so a cut page
 * still pages correctly. Returns the number of entries written, or -1 when `res` does not decode or not even
 * the empty list fits. `out` is always NUL-terminated when cap > 0. */
int webui_list_to_json(const uint8_t *res, size_t n, char *out, size_t cap);

/* An encoded ava1_job_list_result_t as
 *   {"jobs":[{"job":"<32 hex>","kind":"upload"|"download"|"copy"|"op"|"other",
 *             "state":"running"|"done"|"failed","files_done":N,"files_total":N,
 *             "bytes_done":N,"bytes_total":N,"mine":bool},...]}
 * `mine` (may be NULL: never mine) tells whether a job is the caller's. Returns the number of
 * jobs written or -1 (does not decode, or does not fit). */
int webui_jobs_to_json(const uint8_t *res, size_t n, int (*mine)(const uint8_t id[16], void *ctx), void *ctx,
                       char *out, size_t cap);

/* The longest operation result /api/jobs/status carries (a HASH result is ~2.2 KiB with a long
 * path; CRC32 and FFPFSC_UNPACK far less). A longer one is answered as "result":null. */
#define WEBUI_JOB_RESULT_MAX 4096u

/* An encoded ava1_status_t (a JOB_STATUS reply) as
 *   {"job":"<32 hex>","state":"running"|"done"|"failed","files_done":N,"files_total":N,
 *    "bytes_done":N,"bytes_total":N,"message":".."|null,"result":".."|null,"code":N|null}
 * `message` is the operation's current line (on a failure its cause token), `result` the
 * operation's own result body (JSON text, carried as a string; null past WEBUI_JOB_RESULT_MAX),
 * `code` the AVA1 status of a failed job. 0, or -1 (does not decode, or does not fit). */
int webui_job_status_to_json(const uint8_t *res, size_t n, char *out, size_t cap);

/* 1 when `s` can be one file name: 1-255 bytes, no '/', no control character, not "." or "..". */
int webui_name_ok(const char *s);

/* A decompress's output name: as webui_name_ok, but at most WEBUI_UNPACK_NAME_MAX bytes so
 * "<name>.part" (written first, renamed at the end) still fits the 255-byte NAME_MAX. Used by
 * both the page route and the FFPFSC_UNPACK op itself. */
#define WEBUI_UNPACK_NAME_MAX 250
int webui_unpack_name_ok(const char *s);

/* A rename whose target already names the source itself (same device + inode: "a.txt" ->
 * "A.txt" on case-insensitive exFAT) goes through a temporary name in the same folder:
 * `from` -> `tmp` -> `to`. Fills `tmp` ("<dir>/.ps5upload-rename-<salt hex>") and returns 0
 * when `from` and `to` are two spellings in ONE folder (same directory part, byte for byte, so
 * the detour never leaves the mount); -1 otherwise (different folders, the same spelling, an
 * empty name, or `tmp` does not fit `cap`). */
int webui_rename_detour(const char *from, const char *to, unsigned salt, char *tmp, size_t cap);

/* A one-path job operation's args as the engine sends them: {"path":"<escaped>"}, then
 * ,"name":"<escaped>" when `name` is not NULL, then `tail` (raw JSON members such as
 * ,"mode":"0777"; may be NULL) and the closing brace. Returns the length, or -1 (no path, or
 * it does not fit; out is then ""). */
int webui_path_job_args(char *out, size_t cap, const char *path, const char *name, const char *tail);

/* 16 bytes as 32 lowercase hex characters + NUL. */
void webui_hex16(const uint8_t id[16], char out[33]);
/* Exactly 32 hex characters (either case) to 16 bytes: 0, or -1. */
int webui_parse_hex16(const char *s, uint8_t id[16]);

/* The HTTP status for an AVA1 error status and its cause: 400 a malformed request, 403 a path
 * the helper refuses, 404 something missing (unknown job, ENOENT), 409 busy / exists / crosses
 * a mount, 500 anything else. */
int webui_http_status_for(int ava1_status, const char *cause);

/* The Content-Type /api/fs/read answers for `path`, by its extension (case-insensitive):
 * png, jpg/jpeg, txt/log/ini (UTF-8 text), json; anything else (xml, svg, html included: they
 * can render as active documents) application/octet-stream. */
const char *webui_mime_for(const char *path);

/* `key=value` lines (HW_INFO, HW_TEMPS): the value of `key` as a string (1 found, 0 absent;
 * cut to cap - 1) or as a signed integer (1 found, 0 absent, -1 not an integer). */
int webui_kv_str(const char *text, size_t n, const char *key, char *out, size_t cap);
int webui_kv_int(const char *text, size_t n, const char *key, long *out);

/* A HW_TEMPS reply (key=value lines, hw_info.h hw_fan_status_text) as the Fan page's JSON:
 * {"cpu_c":N,"soc_c":N,"duty_pct":N,"target_c":N,"stock_c":N,"pinned_c":N,"min_c":N,"max_c":N,
 *  "restores":N,"last_restore_s":N,"guard":"off"|"ok"|"failing","filtered_c":N.N}
 * with null for a reading the helper reports as missing (temperatures 0, duty / target /
 * last_restore_s -1, stock 0, filtered -1). The number written, or -1 when it does not fit. */
int webui_fan_status_json(const char *text, size_t n, char *out, size_t cap);

/* 1 when `tid` is a game's title id: four letters of a game prefix (PPSA, CUSA, PCJS, PLJS, ...:
 * Sony's disc and digital game ranges for every region) and five digits. Homebrew (FAKE..., PLDM...,
 * WKAL...), system apps (NPXS...) and the page's own tile (WEBUI_TILE_TITLE_ID) are not games. */
int webui_is_game_title_id(const char *tid);

/* The console's time zone for the page, in minutes ahead of UTC that its clock shows (#928: the
 * PS5's own browser has no zone, so the page shows times with this one). From the registry's zone
 * offset (`tz_min`: minutes ahead of UTC, without summer time) and its summer-time flag (`summer`:
 * 1 while it is in effect, which moves the clock an hour ahead); tz_ok / summer_ok say whether each
 * could be read. 1 with *out set; 0 when the zone was not read or is no real zone (beyond -12:00 or
 * +14:00), so the page keeps the browser's own. A flag that was not read counts as no summer time. */
int webui_tz_minutes(int tz_ok, int tz_min, int summer_ok, int summer, long *out);

/* From a PROC_FOCUS reply ({"apps":[{"title_id":..,"app_id":..},...],"focus_app_id":N,
 * "big_app_id":N,...}): what Home shows as running (and may close). First the app that owns the
 * screen (focus_app_id, else big_app_id) when its title id is set, not a system app (NPXS...) and
 * not the page's own tile; else the first app whose title id is a game's (webui_is_game_title_id),
 * so the fallback never names the browser, the tile or a homebrew app. Returns 1 with title_id /
 * *app_id set, 0 when no game runs, -1 when the reply does not parse. */
int webui_focus_running(const char *json, size_t n, char *title_id, size_t cap, uint32_t *app_id);

/* ---- cheats (CHEATS_STATUS / CHEATS_GET / CHEATS_TOGGLE, as the engine calls them) ---- */

/* 1 when `tid` can name a game to the cheat methods: 4 to 15 ASCII letters and digits. */
int webui_cheats_title_ok(const char *tid);

/* A CHEATS_STATUS reply ({"enabled":bool,"patches_last":N,"patches_total":N,"game_running":bool,
 * "game_title_id":"..","game_pid":N,"held":bool}) as {"engine":bool,"game":{"title_id":".."}|null}, with
 * "held":true in `game` when the helper paused that game's cheats (cheats_guard.h). `game` is
 * null unless a game runs under a usable title id (webui_cheats_title_ok) that is not the page's own
 * tile. 0, or -1 (no "enabled", or does not fit; out is then ""). */
int webui_cheats_status_to_json(const char *json, size_t n, char *out, size_t cap);

/* A CHEATS_GET reply ({"title_id":..,"mods":[{"index":N,"name":..,"desc":..,"type":"checkbox"|"button",
 * "on":bool},...]}, or {"mods":[],"error":"no cheat files found"}) as
 *   {"mods":[{"index":N,"name":"..","desc":"..","button":bool,"on":bool},...],"truncated":bool}
 * Every string is re-escaped (valid JSON whatever the cheat file held). A mod without an index or a
 * name is left out. The helper's reply can end mid-entry (its 64 KiB buffer): the entries before the
 * cut are kept and "truncated" is set; so too when `out` is full. Returns the number of mods written,
 * or -1: no mods array, or the reply names an error other than "no cheat files found" (copied to
 * `err`, cut to err_cap - 1), or not even the empty list fits. */
int webui_cheats_mods_to_json(const char *json, size_t n, char *out, size_t cap, char *err, size_t err_cap);

/* A failed CHEATS_TOGGLE's cause ({"ok":false,"err":"<text>"}) as a token the page acts on:
 * "game_not_running" (no game, or a different game than the one asked for), "no_cheats",
 * "bad_index", "module_missing", "game_busy" (another debugger, e.g. the PC app's toggle at the same
 * moment, is attached), else "toggle_failed". The texts matched are cheats.c's (cheats_toggle). */
const char *webui_cheats_toggle_error(const char *cause, size_t n);

/* Why /api/volumes/unmount refuses `p` before asking the helper, or NULL: "system_volume" for the console's
 * own volumes (/, /data, /user, /system*, /preinst*, /dev*, /mnt/usbN, /mnt/extN), else "game_running" when
 * a game runs (`game_running` != 0 also when that could not be read). The helper's FS_UNMOUNT forces a busy
 * unmount, so a running game is never asked about: it would lose its image (ruling R20). */
const char *webui_unmount_refusal(const char *p, int game_running);

/* ---- games and processes (APP_LAUNCH / APP_UNREGISTER / APP_ICON_SET / PROCESS_LIST / PROCESS_KILL) ---- */

/* Why the Games section may not act on `tid`, or NULL: "bad_title_id" (not 4-15 ASCII letters and
 * digits), "own_tile" (the page's own tile, WEBUI_TILE_TITLE_ID, any case: Settings removes it), and
 * for a `destructive` action (uninstall, hide) also "system_title" (NPXS...: the console's own apps,
 * which the PC app never uninstalls on one confirm and never hides). */
const char *webui_app_refusal(const char *tid, int destructive);

/* From a PROCESS_LIST reply ({"ok":true,"procs":[{"pid":N,..,"title_id":"..","app_id":N,"kind":"app"|
 * "payload"|"system","is_self":bool},...]}): why the page may not stop `pid`, or NULL. "not_running"
 * when the pid is not listed (or the reply does not parse), "helper" for the helper's own process,
 * "system" for anything not a game ("app") or a payload ("payload") and for pid 0/1. The PC app asks
 * twice before a system kill; the console page, whose browser is itself a system process, never
 * offers one. */
const char *webui_proc_kill_refusal(const char *json, size_t n, uint64_t pid);

/* From a PROCESS_LIST reply: 1 when a game ("app") process with title id `tid` runs (when
 * `other` is set: a game with any OTHER title id), 0 when none, -1 when the reply does not parse. */
int webui_proc_game_running(const char *json, size_t n, const char *tid, int other);

/* ---- search index (INDEX_START / INDEX_STATUS / INDEX_SEARCH / INDEX_CANCEL, runtime.c) ----
 * The handlers read their bodies with a quote-to-quote scan (no JSON unescaping), so every string the
 * page sends is checked to hold no '"', no '\\' and no control character before it is put in one. */

/* The roots INDEX_START takes: at most 8, each under 256 bytes (the handler's own limits). */
#define WEBUI_INDEX_ROOTS_MAX 8
#define WEBUI_INDEX_ROOT_CAP 256

/* 1 when `p` can be an index root: starts with '/', 1-255 bytes, no '"', '\\' or control character. */
int webui_index_root_ok(const char *p);
/* 1 when `q` can be a name pattern: 1-255 bytes, no '"', '\\' or control character. */
int webui_index_query_ok(const char *q);

/* Runs of '*' in a pattern collapsed to one, in place (they mean the same; fewer backtrack points). */
void webui_index_collapse_stars(char *q);

/* INDEX_START's body {"roots":["/a","/b"]} for `n` roots (each webui_index_root_ok, 1 <= n <= 8).
 * Returns the length, or -1 (a bad root, a bad count, or it does not fit; out is then ""). */
int webui_index_start_body(char *out, size_t cap, const char *const *roots, int n);

/* An INDEX_STATUS reply ({"phase":"idle"|"building"|"ready","files":N,"truncated":bool,"started_at":N,
 * "completed_at":N}) as
 *   {"phase":"..","files":N,"truncated":bool,"cancelled":bool,"roots":["..",..]|null}
 * `roots` (NULL: null) are the roots the page started this index with, `cancelled` that the page stopped
 * it (the helper itself does not say). 0, or -1 (no usable phase, or it does not fit). */
int webui_index_status_to_json(const char *text, size_t n, const char *const *roots, int nroots, int cancelled,
                               char *out, size_t cap);

/* An INDEX_SEARCH reply ({"results":[{"path":"..","size":N},..](,"truncated":true)}) as
 *   {"results":[{"path":"..","size":N},..],"truncated":bool}
 * keeping only paths under `under` (NULL: all; "/" all) that `hide` (may be NULL) does not hide, at
 * most `max` of them. "truncated" is set when the helper said so, when it answered `asked` results
 * (its limit: there may be more), when a result was dropped for `max`, or when `out` is full. Returns
 * the number written, or -1 (no results array, or not even the empty list fits). */
int webui_index_results_to_json(const char *text, size_t n, const char *under, size_t max, size_t asked,
                                int (*hide)(const char *path, void *ctx), void *ctx, char *out, size_t cap);

#endif
