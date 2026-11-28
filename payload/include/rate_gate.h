/* A request-rate gate per source address for the helper's own HTTP servers: Minion's (:9180,
 * webui_server.c, LAN sources only) and the loopback app API (:9181, app_api_server.c).
 *
 * The owner's rule: limits never stop a person, only robots. So:
 *   - each source has a bucket of `burst` requests that refills `per_sec` a second (smoothly, in
 *     thousandths): a page load, a folder of thumbnails or a quick run of button presses fits in
 *     the burst, and a person never sustains the refill rate;
 *   - an empty bucket answers "slow down" (429 with Retry-After) instead of serving: nothing is
 *     banned, and the next request after the wait is served again;
 *   - only a source that keeps hammering while told to wait gets longer waits: each refused request
 *     is a strike, strikes fade a few a second, and the wait grows with them (1 s, up to
 *     RATE_GATE_MAX_WAIT_S); a single burst never earns more than a second;
 *   - the table holds RATE_GATE_SOURCES sources; a new one takes the place of the one seen longest
 *     ago (never of one being held back, while a free or idle place exists).
 * Pure (the time comes in), host-tested in payload/tests/rate_gate_selftest.c. Thread-safe. */
#ifndef PS5UPLOAD_RATE_GATE_H
#define PS5UPLOAD_RATE_GATE_H

#include <pthread.h>
#include <stdint.h>

#define RATE_GATE_SOURCES 32
#define RATE_GATE_MAX_WAIT_S 30u
/* Strikes that fade a second, and how many strikes add a second to the wait. */
#define RATE_GATE_FADE_PER_S 25u
#define RATE_GATE_STRIKES_PER_S 50u

typedef struct {
    uint32_t src;
    int used;
    uint64_t milli;     /* tokens in thousandths */
    uint64_t last_ms;   /* the last refill */
    uint64_t seen_ms;   /* the last request */
    uint32_t strikes;
    uint64_t hold_until_ms; /* refused until then (a source told to wait that did not) */
} rate_gate_src_t;

typedef struct {
    pthread_mutex_t mu;
    unsigned burst, per_sec;
    rate_gate_src_t s[RATE_GATE_SOURCES];
    /* counted for the helper log */
    unsigned refused, served;
} rate_gate_t;

/* A gate with these limits (burst >= 1, per_sec >= 1). */
void rate_gate_init(rate_gate_t *g, unsigned burst, unsigned per_sec);

/* One request from src at now_ms: 0 serve it, else the seconds to wait (the Retry-After). */
unsigned rate_gate_take(rate_gate_t *g, uint32_t src, uint64_t now_ms);

/* Since the last call: how many were refused and served (the helper's log line). */
void rate_gate_counts(rate_gate_t *g, unsigned *refused, unsigned *served);

#endif
