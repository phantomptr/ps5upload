/* The console page's offline copy (client/src/console/offline/offlineCopy.ts): the service worker
 * (dist-console/sw.js) and the appcache manifest (dist-console/cache.appcache), embedded by xxd as
 * build/webui_offline.c (payload/Makefile). Served at GET /sw.js and GET /minion.appcache with their
 * own types and "Cache-Control: no-cache" (the browser checks them on every visit; a new build has
 * a new hash inside, which replaces the stored copy). Neither ever stores an /api/ reply.
 *
 * Getting rid of an older page the browser kept (2026-11-07: after a helper update the PS5 browser
 * kept showing its stored copy of the old page, whose "newer helper" banner needs a press):
 *   - pages up to 11.6.x named their appcache /cache.appcache. That path now answers 410 Gone
 *     (webui_offline_retired), which makes the browser delete that stored copy ("obsolete"): the
 *     next load of / comes from the helper.
 *   - those pages read GET /api/token without ?page=; the first such read in
 *     WEBUI_OLD_PAGE_NUDGE_MS answers 503 "stopping" (webui_offline_nudge). The old page then shows
 *     its own "not running" screen, asks again 5 s later, gets the token and reloads itself
 *     (OfflinePage), with no button press, into the helper's current page.
 *   - newer pages send ?page=<their version> and reload themselves when the helper is newer
 *     (offline/selfUpdate.ts); /api/token also carries the helper's version for that. */
#ifndef PS5UPLOAD_WEBUI_OFFLINE_H
#define PS5UPLOAD_WEBUI_OFFLINE_H

#include <stddef.h>
#include <stdint.h>

/* The generated arrays (xxd -i naming). */
extern unsigned char webui_sw_js[];
extern unsigned int webui_sw_js_len;
extern unsigned char webui_cache_appcache[];
extern unsigned int webui_cache_appcache_len;

/* Where the appcache manifest is served (client/src/console/index.html names the same path), and
 * the path pages up to 11.6.x used. */
#define WEBUI_APPCACHE_PATH "/minion.appcache"
#define WEBUI_APPCACHE_RETIRED_PATH "/cache.appcache"

typedef struct {
    const char *ctype;
    const unsigned char *data;
    size_t len;
} webui_offline_file_t;

/* The file a request path names exactly ("/sw.js", WEBUI_APPCACHE_PATH): 1 and *out filled, else 0. */
int webui_offline_find(const char *path, webui_offline_file_t *out);

/* 1 for a manifest path the page no longer uses (WEBUI_APPCACHE_RETIRED_PATH): answer 410 Gone, so
 * the browser drops the copy it stored under it. */
int webui_offline_retired(const char *path);

/* The extra headers for the 410: no-store, so nothing keeps the answer. */
#define WEBUI_OFFLINE_GONE_HEADERS "Cache-Control: no-store\r\n"

/* Whether a GET /api/token comes from a page that marks itself (?page=<version>, any position in
 * the query). 1 / 0. */
int webui_offline_page_marked(const char *query);

/* The old-page nudge: at most one refused token read per window. */
#define WEBUI_OLD_PAGE_NUDGE_MS 30000u
typedef struct {
    int armed;      /* a read was refused before */
    uint64_t at_ms; /* when */
} webui_nudge_t;

/* GET /api/token with `query`, at `now_ms` (a monotonic clock): 1 = answer 503 "stopping" this
 * once (an unmarked page, and none refused in the last WEBUI_OLD_PAGE_NUDGE_MS; *st is updated),
 * 0 = answer the token. A marked page is never refused. Pure; the caller serializes *st. */
int webui_offline_nudge(const char *query, uint64_t now_ms, webui_nudge_t *st);

/* {"token":"<token>","version":"<version>"} into out (like snprintf: the full length, or -1 when it
 * does not fit). */
int webui_offline_token_json(char *out, size_t cap, const char *token, const char *version);

#endif
