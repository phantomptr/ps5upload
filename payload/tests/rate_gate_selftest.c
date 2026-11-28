/* Host-side tests for the helper's request-rate gate (rate_gate.c): a person's pace is always
 * served (a burst, then the steady refill), an empty bucket asks for a second's wait, only a source
 * that keeps going while told to wait waits longer (up to the cap), strikes fade, sources are kept
 * apart, and a full table makes room without dropping a source being held back.
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <string.h>

#include "rate_gate.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static rate_gate_t g;

int main(void) {
    unsigned served = 0, refused = 0, w, max_w = 0;
    uint64_t t;
    const uint32_t A = 0x0A000001u, B = 0x0A000002u;

    rate_gate_init(&g, 100, 20);
    /* a page load and a burst of thumbnails: all served */
    for (int i = 0; i < 100; i++) served += rate_gate_take(&g, A, 1000) == 0;
    CHECK(served == 100);
    /* the 101st: a second's wait, no more */
    CHECK(rate_gate_take(&g, A, 1000) == 1);
    /* another source is not affected */
    CHECK(rate_gate_take(&g, B, 1000) == 0);
    /* 50 ms later: one token back (20 a second) */
    CHECK(rate_gate_take(&g, A, 1050) == 0);
    CHECK(rate_gate_take(&g, A, 1050) == 1);
    /* a person's steady pace (10 a second) for a minute: never refused */
    t = 10000;
    rate_gate_init(&g, 100, 20);
    refused = 0;
    for (int i = 0; i < 600; i++, t += 100) refused += rate_gate_take(&g, A, t) != 0;
    CHECK(refused == 0);
    /* a client that honours Retry-After: told 1 s, comes back after it, served */
    rate_gate_init(&g, 10, 5);
    for (int i = 0; i < 10; i++) (void)rate_gate_take(&g, A, 0);
    CHECK(rate_gate_take(&g, A, 0) == 1);
    CHECK(rate_gate_take(&g, A, 1000) == 0);
    /* a robot that ignores it: the wait grows, to the cap, never beyond */
    rate_gate_init(&g, 10, 5);
    t = 0;
    for (int i = 0; i < 5000; i++, t += 1) {
        w = rate_gate_take(&g, A, t);
        if (w > max_w) max_w = w;
    }
    CHECK(max_w == RATE_GATE_MAX_WAIT_S);
    /* and while held, even a slow request is refused until the hold ends */
    CHECK(rate_gate_take(&g, A, t + 1000) > 0);
    /* a person on another address meanwhile: served */
    CHECK(rate_gate_take(&g, B, t + 1000) == 0);
    /* once the robot stopped and the hold passed, it is served again (no ban) */
    t += 60000;
    CHECK(rate_gate_take(&g, A, t) == 0);
    /* one burst never earns a long wait: 200 refused at once is at most a few seconds */
    rate_gate_init(&g, 10, 5);
    max_w = 0;
    for (int i = 0; i < 210; i++) {
        w = rate_gate_take(&g, A, 500);
        if (w > max_w) max_w = w;
    }
    CHECK(max_w >= 1 && max_w <= 5);
    /* and the strikes fade: 30 s later a refusal is a second again */
    for (int i = 0; i < 10; i++) (void)rate_gate_take(&g, A, 30500);
    CHECK(rate_gate_take(&g, A, 30500) == 1);
    /* a full table: new sources take the places of idle ones, a held-back one keeps its place */
    rate_gate_init(&g, 2, 1);
    for (int i = 0; i < 500; i++) (void)rate_gate_take(&g, A, 100); /* A held back */
    for (uint32_t s = 100; s < 100 + 3 * RATE_GATE_SOURCES; s++) CHECK(rate_gate_take(&g, s, 200 + s) == 0);
    CHECK(rate_gate_take(&g, A, 1000) > 1); /* still held, not forgotten */
    /* counts */
    rate_gate_counts(&g, &refused, &served);
    CHECK(refused > 0 && served > 0);
    rate_gate_counts(&g, &refused, &served);
    CHECK(refused == 0 && served == 0);
    if (failures) {
        fprintf(stderr, "rate_gate selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("rate_gate selftest: ok\n");
    return 0;
}
