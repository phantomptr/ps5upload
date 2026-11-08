/* Host-side tests for the page's offline copy (webui_offline.c): GET /sw.js and GET /minion.appcache
 * resolve, each with the type its browser feature requires, and nothing else (never an /api/ path)
 * does; the retired /cache.appcache answers 410 Gone with no-store; an older stored page's unmarked
 * token read is refused at most once per window, a marked one never; the token reply carries the
 * helper's version. Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <string.h>

#include "webui_http.h"
#include "webui_offline.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* Stand-ins for build/webui_offline.c. */
unsigned char webui_sw_js[] = "self.addEventListener";
unsigned int webui_sw_js_len = sizeof webui_sw_js - 1;
unsigned char webui_cache_appcache[] = "CACHE MANIFEST\n";
unsigned int webui_cache_appcache_len = sizeof webui_cache_appcache - 1;

static void lookup(void) {
    webui_offline_file_t f;
    static const char *other[] = { "/", "/sw.js/", "/SW.JS", "/sw.json", "/api/sw.js", "/minion.appcache.bak",
                                   "/strings/sw.js", "/api/token", "", "sw.js", "/cache.appcache" };
    size_t i;

    memset(&f, 0, sizeof f);
    CHECK(webui_offline_find("/sw.js", &f) == 1);
    CHECK(strcmp(f.ctype, "text/javascript; charset=utf-8") == 0);
    CHECK(f.data == webui_sw_js && f.len == webui_sw_js_len);

    memset(&f, 0, sizeof f);
    CHECK(webui_offline_find("/minion.appcache", &f) == 1);
    CHECK(strcmp(f.ctype, "text/cache-manifest; charset=utf-8") == 0);
    CHECK(f.data == webui_cache_appcache && f.len == webui_cache_appcache_len);

    for (i = 0; i < sizeof other / sizeof other[0]; i++) CHECK(webui_offline_find(other[i], &f) == 0);
    CHECK(webui_offline_find(NULL, &f) == 0);
    CHECK(webui_offline_find("/sw.js", NULL) == 0);
}

/* The manifest pages up to 11.6.x stored their copy under: 410 Gone, never stored. */
static void retired_manifest_is_gone(void) {
    char head[512];
    int n;
    CHECK(webui_offline_retired("/cache.appcache") == 1);
    CHECK(webui_offline_retired("/minion.appcache") == 0);
    CHECK(webui_offline_retired("/") == 0);
    CHECK(webui_offline_retired("/cache.appcache/") == 0);
    CHECK(webui_offline_retired(NULL) == 0);
    n = webui_http_head(head, sizeof head, 410, "text/plain; charset=utf-8", 0, WEBUI_OFFLINE_GONE_HEADERS);
    CHECK(n > 0);
    CHECK(strncmp(head, "HTTP/1.1 410 Gone\r\n", 19) == 0);
    CHECK(strstr(head, "Cache-Control: no-store\r\n") != NULL);
    /* exactly one Cache-Control line */
    CHECK(strstr(strstr(head, "Cache-Control:") + 1, "Cache-Control:") == NULL);
    CHECK(strstr(head, "Content-Length: 0\r\n") != NULL);
}

static void marked_pages(void) {
    CHECK(webui_offline_page_marked("page=11.6.1") == 1);
    CHECK(webui_offline_page_marked("x=1&page=dev") == 1);
    CHECK(webui_offline_page_marked("page=") == 1);
    CHECK(webui_offline_page_marked("") == 0);
    CHECK(webui_offline_page_marked(NULL) == 0);
    CHECK(webui_offline_page_marked("apage=1") == 0);
    CHECK(webui_offline_page_marked("x=page=1") == 0);
    CHECK(webui_offline_page_marked("pages=1") == 0);
}

/* The old page: refused once (it shows "not running", asks again 5 s later, gets the token and
 * reloads), then answered; refused again only after the window, so it never loops. */
static void old_page_nudged_once_per_window(void) {
    webui_nudge_t st;
    memset(&st, 0, sizeof st);
    CHECK(webui_offline_nudge("", 1000, &st) == 1);
    CHECK(webui_offline_nudge("", 6000, &st) == 0); /* its retry 5 s later */
    CHECK(webui_offline_nudge(NULL, 1000 + WEBUI_OLD_PAGE_NUDGE_MS - 1, &st) == 0);
    CHECK(webui_offline_nudge("", 1000 + WEBUI_OLD_PAGE_NUDGE_MS, &st) == 1);
    /* a page that marks its read is never refused, and does not use up the window */
    memset(&st, 0, sizeof st);
    CHECK(webui_offline_nudge("page=11.6.1", 0, &st) == 0);
    CHECK(webui_offline_nudge("page=11.6.1", 0, &st) == 0);
    CHECK(st.armed == 0);
    CHECK(webui_offline_nudge("", 0, &st) == 1); /* first unmarked read at t=0 still counts */
    CHECK(webui_offline_nudge("", 0, &st) == 0);
    CHECK(webui_offline_nudge("", 0, NULL) == 0);
}

static void token_reply_has_the_version(void) {
    char out[160], tiny[20];
    int n = webui_offline_token_json(out, sizeof out, "abc123", "11.6.1");
    CHECK(n > 0 && strcmp(out, "{\"token\":\"abc123\",\"version\":\"11.6.1\"}") == 0);
    CHECK((size_t)n == strlen(out));
    CHECK(webui_offline_token_json(tiny, sizeof tiny, "abc123", "11.6.1") == -1);
}

int main(void) {
    lookup();
    retired_manifest_is_gone();
    marked_pages();
    old_page_nudged_once_per_window();
    token_reply_has_the_version();
    if (failures) {
        fprintf(stderr, "webui_offline_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("webui_offline_selftest: ok\n");
    return 0;
}
