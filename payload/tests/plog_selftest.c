/* Host self-test for the helper's log levels (src/plog.c). */
#include "plog.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures;
#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            failures++;                                                  \
        }                                                                \
    } while (0)

static int apply(const char *s, uint64_t now) {
    return plog_apply(s, strlen(s), now);
}

int main(void) {
    char out[512];
    plog_reset();
    /* The start: info everywhere. */
    CHECK(plog_on(PLOG_INFO, PLOG_RP));
    CHECK(!plog_on(PLOG_DEBUG, PLOG_RP));
    CHECK(plog_on(PLOG_ERROR, PLOG_RP));

    /* A raise for one category, with an end. */
    uint64_t now = plog_now_s();
    CHECK(apply("{\"level\":3,\"cats\":{\"rp\":5,\"nope\":5},\"until_s\":60}", now) == 0);
    CHECK(plog_on(PLOG_TRACE, PLOG_RP));
    CHECK(!plog_on(PLOG_DEBUG, PLOG_INSTALL));
    CHECK(plog_ceiling == PLOG_TRACE);
    int n = plog_describe(out, sizeof out, now);
    CHECK(n > 0);
    CHECK(strstr(out, "\"rp\":5") != NULL);
    CHECK(strstr(out, "\"until_s\":60") != NULL);
    CHECK(strstr(out, "nope") == NULL);

    /* It runs out: back to info. */
    plog_expire(now + 61);
    CHECK(plog_ceiling == PLOG_INFO);
    plog_describe(out, sizeof out, now + 61);
    CHECK(strcmp(out, "{\"ok\":true,\"level\":3,\"cats\":{},\"until_s\":0}") == 0);

    /* A lowered level stays, and gates info. */
    CHECK(apply("{ \"level\" : 2 , \"cats\" : { \"install\" : 1 } , \"until_s\" : 0 }", now) == 0);
    CHECK(!plog_on(PLOG_INFO, PLOG_RP));
    CHECK(plog_on(PLOG_WARN, PLOG_RP));
    CHECK(!plog_on(PLOG_WARN, PLOG_INSTALL));
    CHECK(plog_floor == PLOG_ERROR);
    plog_expire(now + 100000);
    CHECK(!plog_on(PLOG_INFO, PLOG_RP));

    /* A raise with no time left is no raise. */
    CHECK(apply("{\"level\":5,\"cats\":{},\"until_s\":0}", now) == 0);
    CHECK(!plog_on(PLOG_DEBUG, PLOG_RP));

    /* A raise is never more than a day, whatever until_s says. */
    CHECK(apply("{\"level\":4,\"cats\":{},\"until_s\":9999999}", now) == 0);
    plog_describe(out, sizeof out, now);
    CHECK(strstr(out, "\"until_s\":86400") != NULL);
    plog_expire(now + 86401);
    CHECK(!plog_on(PLOG_DEBUG, PLOG_RP));

    /* Bad bodies change nothing; an empty one only reads. */
    plog_reset();
    CHECK(apply("[1]", now) == -1);
    CHECK(apply("{\"level\":\"debug\"}", now) == -1);
    CHECK(apply("{\"level\":4", now) == -1);
    CHECK(plog_apply("", 0, now) == 0);
    CHECK(plog_ceiling == PLOG_INFO);
    CHECK(plog_cat_by_name("files", 5) == PLOG_FILES);
    CHECK(plog_cat_by_name("fil", 3) == -1);

    /* The PIN stays on the TV. */
    plog_without_pin("[ps5upload] Remote Play PIN: 14928144", out, sizeof out);
    CHECK(strcmp(out, "[ps5upload] Remote Play PIN: (shown on the TV)") == 0);
    CHECK(strstr(out, "14928144") == NULL);
    plog_without_pin("[ps5upload] Remote Play PIN expired", out, sizeof out);
    CHECK(strcmp(out, "[ps5upload] Remote Play PIN expired") == 0);
    plog_without_pin("PIN 12", out, sizeof out);
    CHECK(strcmp(out, "PIN 12") == 0);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    puts("plog selftest: ok");
    return 0;
}
