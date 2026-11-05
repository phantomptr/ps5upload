#include <stdio.h>
#include <string.h>
#include "webui_auth.h"

static int fails;
#define CHECK(c) do { if (!(c)) { printf("FAIL %s:%d %s\n", __FILE__, __LINE__, #c); fails++; } } while (0)

static const char *OWN[] = { "192.0.2.99" };
static const char TOK[] =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

static webui_auth_cfg_t cfg(int lan) {
    webui_auth_cfg_t c = { TOK, 9180, lan, OWN, 1 };
    return c;
}
static webui_req_t req(const char *host, const char *origin, const char *tok) {
    webui_req_t r;
    memset(&r, 0, sizeof r);
    strcpy(r.method, "GET");
    strcpy(r.path, "/api/x");
    if (host) strcpy(r.host, host);
    if (origin) strcpy(r.origin, origin);
    if (tok) strcpy(r.token, tok);
    return r;
}

int main(void) {
    webui_auth_cfg_t c0 = cfg(0), c1 = cfg(1);
    webui_req_t r;

    r = req("127.0.0.1:9180", NULL, NULL);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_TOKEN);           /* api_without_token_denied */
    r = req("127.0.0.1:9180", NULL,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdee");
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_TOKEN);           /* api_wrong_token_denied */
    r = req("127.0.0.1:9180", NULL, TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_ALLOW);                /* api_with_token_allowed */
    r = req("localhost:9180", "http://localhost:9180", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_ALLOW);
    r = req("LOCALHOST:9180", "http://LocalHost:9180", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_ALLOW);

    r = req("evil.example:9180", NULL, TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_HOST);            /* rebinding_host_denied */
    CHECK(webui_check(&r, &c1, 0) == WEBUI_DENY_HOST);
    r = req("127.0.0.1", NULL, TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_HOST);
    r = req("127.0.0.1:9181", NULL, TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_HOST);
    r = req("", NULL, TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_HOST);

    r = req("192.0.2.99:9180", NULL, TOK);                     /* lan_host_only_when_lan_on */
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_HOST);
    CHECK(webui_check(&r, &c1, 1) == WEBUI_ALLOW);
    r = req("192.0.2.98:9180", NULL, TOK);
    CHECK(webui_check(&r, &c1, 1) == WEBUI_DENY_HOST);

    r = req("127.0.0.1:9180", "http://evil.example", TOK);        /* foreign_origin_denied */
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_ORIGIN);
    r = req("127.0.0.1:9180", "null", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_ORIGIN);
    r = req("127.0.0.1:9180", "https://127.0.0.1:9180", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_ORIGIN);
    r = req("127.0.0.1:9180", "http://localhost:9180", TOK);      /* allowed host value */
    CHECK(webui_check(&r, &c0, 1) == WEBUI_ALLOW);
    r = req("127.0.0.1:9180", "http://192.0.2.99:9180", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_ORIGIN);
    CHECK(webui_check(&r, &c1, 1) == WEBUI_ALLOW);
    r = req("127.0.0.1:9180", "http://127.0.0.1:9180/", TOK);
    CHECK(webui_check(&r, &c0, 1) == WEBUI_DENY_ORIGIN);

    r = req("127.0.0.1:9180", NULL, NULL);                        /* page_needs_host_not_token */
    CHECK(webui_check(&r, &c0, 0) == WEBUI_ALLOW);
    r = req("localhost:9180", "http://localhost:9180", NULL);     /* token_route */
    CHECK(webui_check(&r, &c0, 0) == WEBUI_ALLOW);
    r = req("127.0.0.1:9180", "http://evil.example", NULL);
    CHECK(webui_check(&r, &c0, 0) == WEBUI_DENY_ORIGIN);

    {   /* token_is_64_hex_and_changes */
        char a[65], b[65];
        int i;
        webui_token_new(a);
        webui_token_new(b);
        CHECK(strlen(a) == 64);
        for (i = 0; i < 64; i++)
            CHECK((a[i] >= '0' && a[i] <= '9') || (a[i] >= 'a' && a[i] <= 'f'));
        CHECK(strcmp(a, b) != 0);
    }

    if (fails) { printf("webui_auth_selftest: %d failure(s)\n", fails); return 1; }
    printf("webui_auth_selftest: ok\n");
    return 0;
}
