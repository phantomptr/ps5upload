/* GET /sw.js and GET /minion.appcache, and retiring an older page's offline copy: see
 * include/webui_offline.h. */
#include "webui_offline.h"

#include <stdio.h>
#include <string.h>

int webui_offline_find(const char *path, webui_offline_file_t *out) {
    if (!path || !out) return 0;
    if (strcmp(path, "/sw.js") == 0) {
        /* A service worker must come as JavaScript or the browser refuses to register it. */
        out->ctype = "text/javascript; charset=utf-8";
        out->data = webui_sw_js;
        out->len = webui_sw_js_len;
        return 1;
    }
    if (strcmp(path, WEBUI_APPCACHE_PATH) == 0) {
        /* An appcache manifest is only honoured with this type. */
        out->ctype = "text/cache-manifest; charset=utf-8";
        out->data = webui_cache_appcache;
        out->len = webui_cache_appcache_len;
        return 1;
    }
    return 0;
}

int webui_offline_retired(const char *path) { return path && strcmp(path, WEBUI_APPCACHE_RETIRED_PATH) == 0; }

int webui_offline_page_marked(const char *query) {
    const char *q = query;
    if (!q) return 0;
    while (*q) {
        if (strncmp(q, "page=", 5) == 0) return 1;
        q = strchr(q, '&');
        if (!q) break;
        q++;
    }
    return 0;
}

int webui_offline_nudge(const char *query, uint64_t now_ms, webui_nudge_t *st) {
    if (!st || webui_offline_page_marked(query)) return 0;
    if (st->armed && now_ms - st->at_ms < WEBUI_OLD_PAGE_NUDGE_MS) return 0;
    st->armed = 1;
    st->at_ms = now_ms;
    return 1;
}

int webui_offline_token_json(char *out, size_t cap, const char *token, const char *version) {
    int n = snprintf(out, cap, "{\"token\":\"%s\",\"version\":\"%s\"}", token ? token : "", version ? version : "");
    return n < 0 || (size_t)n >= cap ? -1 : n;
}
