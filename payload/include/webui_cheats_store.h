/* The console page's cheat download (webui_api.c is the glue): the page fetches a cheat file from a
 * community repo in the browser and POSTs it to /api/cheats/store?format=<f>&name=<file>; these pure
 * pieces check that request and place the file where the cheat engine looks (cheats.c scans
 * CHEATS_{JSON,SHN,MC4}_DIR on every list/get, so a new file needs no reload). The bytes themselves
 * go through wf_receive_file (webui_files.h): a same-folder temp file renamed after a same-device
 * check, never across a mount. Host-tested by payload/tests/webui_cheats_store_selftest.c. */
#ifndef PS5UPLOAD_WEBUI_CHEATS_STORE_H
#define PS5UPLOAD_WEBUI_CHEATS_STORE_H

#include <stddef.h>

#define WEBUI_CHEATS_ROOT "/data/ps5upload/cheats"   /* cheats.c CHEATS_ROOT */
#define WEBUI_CHEAT_FILE_MAX (4u * 1024u * 1024u)     /* cheats.c CHEAT_FILE_MAX: a larger file never loads */
#define WEBUI_CHEAT_NAME_MAX 128u                     /* bytes, NUL not counted */
#define WEBUI_CHEAT_FMT_CAP 8u

/* "json", "shn" or "mc4" (exactly, lower case): 1, else 0. */
int webui_cheat_format_ok(const char *fmt);

/* A file name the store accepts for `fmt`: a plain base name of 1..WEBUI_CHEAT_NAME_MAX bytes from
 * [A-Za-z0-9._-], not starting with '.', no "..", and ending in ".<fmt>" (any case) after a non-empty
 * stem. 1, else 0. */
int webui_cheat_name_ok(const char *fmt, const char *name);

/* The query "format=<f>&name=<file>" (either order; other keys ignored; no percent-escapes: every
 * byte a name may hold is sent as is) into `fmt` and `name`. NULL when usable, else the error token
 * ("bad_format" or "bad_name"; a key given twice is refused too). */
const char *webui_cheat_store_query(const char *query, char fmt[WEBUI_CHEAT_FMT_CAP], char name[WEBUI_CHEAT_NAME_MAX + 1]);

/* "<root>/<fmt>/<name>" into `out`, creating <root> and <root>/<fmt> (0777) when missing. 0, or -1
 * when it does not fit or the folder cannot be made (or is not a real folder: a link is refused). */
int webui_cheat_target(const char *root, const char *fmt, const char *name, char *out, size_t cap);

/* The game a cheat file is for, from its name as cheats.c matches it (filename_matches_title): a title
 * id (four letters, five digits, any case) then '.' or '_'. Into `out` in upper case: 1, else 0 and "". */
int webui_cheat_file_title(const char *name, char out[10]);

/* "<root>/state/<TITLE>.json", the game's saved switches (cheats.c state_path: always upper case),
 * into `out`. 0, or -1 when `title` is not one or it does not fit. */
int webui_cheat_state_path(const char *root, const char *title, char *out, size_t cap);

/* The game's version from an APP_INFO_QUERY reply ({"ok":true,"rows":[{"key":..,"val":..}..]}):
 * CONTENT_VERSION (PS5, "01.000.016") first, else the higher of VERSION and APP_VER (PS4, "01.32":
 * an update raises VERSION, appinfo_version.h), the same forms the cheat files name their target
 * version in. Only digits and dots, 1..cap-1 bytes. 1 found, else 0 and "". */
int webui_cheat_appinfo_version(const char *json, size_t n, char *out, size_t cap);

#endif
