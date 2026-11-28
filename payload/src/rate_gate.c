/* The helper's request-rate gate per source: see include/rate_gate.h. */
#include "rate_gate.h"

#include <string.h>

void rate_gate_init(rate_gate_t *g, unsigned burst, unsigned per_sec) {
    memset(g->s, 0, sizeof g->s);
    pthread_mutex_init(&g->mu, NULL);
    g->burst = burst ? burst : 1;
    g->per_sec = per_sec ? per_sec : 1;
    g->refused = g->served = 0;
}

static rate_gate_src_t *slot_for(rate_gate_t *g, uint32_t src, uint64_t now_ms) {
    rate_gate_src_t *free_slot = NULL, *oldest = NULL;
    for (int i = 0; i < RATE_GATE_SOURCES; i++) {
        rate_gate_src_t *e = &g->s[i];
        if (e->used && e->src == src) return e;
        if (!e->used) {
            if (!free_slot) free_slot = e;
            continue;
        }
        /* a place being held back is kept while another can be had */
        if (e->hold_until_ms > now_ms || e->strikes) continue;
        if (!oldest || e->seen_ms < oldest->seen_ms) oldest = e;
    }
    if (!free_slot) free_slot = oldest;
    if (!free_slot) {
        /* every place holds a source being held back: the one seen longest ago goes */
        free_slot = &g->s[0];
        for (int i = 1; i < RATE_GATE_SOURCES; i++)
            if (g->s[i].seen_ms < free_slot->seen_ms) free_slot = &g->s[i];
    }
    memset(free_slot, 0, sizeof *free_slot);
    free_slot->used = 1;
    free_slot->src = src;
    free_slot->milli = (uint64_t)g->burst * 1000u;
    free_slot->last_ms = now_ms;
    return free_slot;
}

unsigned rate_gate_take(rate_gate_t *g, uint32_t src, uint64_t now_ms) {
    rate_gate_src_t *e;
    uint64_t cap, el;
    unsigned wait = 0;
    pthread_mutex_lock(&g->mu);
    e = slot_for(g, src, now_ms);
    cap = (uint64_t)g->burst * 1000u;
    el = now_ms > e->last_ms ? now_ms - e->last_ms : 0;
    /* refill: per_sec a second, in thousandths (1 ms = per_sec thousandths) */
    e->milli += el * g->per_sec;
    if (e->milli > cap) e->milli = cap;
    /* strikes fade */
    if (e->strikes) {
        uint64_t fade = el * RATE_GATE_FADE_PER_S / 1000u;
        e->strikes = fade >= e->strikes ? 0 : e->strikes - (uint32_t)fade;
    }
    if (el) e->last_ms = now_ms;
    e->seen_ms = now_ms;
    if (now_ms < e->hold_until_ms || e->milli < 1000u) {
        uint64_t w;
        e->strikes++;
        w = 1u + e->strikes / RATE_GATE_STRIKES_PER_S;
        if (w > RATE_GATE_MAX_WAIT_S) w = RATE_GATE_MAX_WAIT_S;
        /* longer waits only for a source that keeps going while told to wait */
        if (w > 1 && now_ms + w * 1000u > e->hold_until_ms) e->hold_until_ms = now_ms + w * 1000u;
        if (now_ms < e->hold_until_ms) {
            uint64_t left = (e->hold_until_ms - now_ms + 999u) / 1000u;
            if (left > w) w = left;
        }
        wait = (unsigned)w;
        g->refused++;
    } else {
        e->milli -= 1000u;
        g->served++;
    }
    pthread_mutex_unlock(&g->mu);
    return wait;
}

void rate_gate_counts(rate_gate_t *g, unsigned *refused, unsigned *served) {
    pthread_mutex_lock(&g->mu);
    if (refused) *refused = g->refused;
    if (served) *served = g->served;
    g->refused = g->served = 0;
    pthread_mutex_unlock(&g->mu);
}
