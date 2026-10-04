#ifndef AVA1_PAIRLIMIT_H
#define AVA1_PAIRLIMIT_H
/* Pairing limits (SPEC.md 4.6 / 5 item 5). A failed pairing attempt is a real guess at the
 * code: the PAKE was exchanged and the client's confirmation was wrong. Only those count.
 * Each source address has its own budget of guesses per window, under a global cap on all
 * guesses per window; sessions that never complete the PAKE reveal nothing and are only
 * bounded by a per-address rate of new pairing sessions. Pure data: the caller locks. */
#include <stdint.h>

#define AVA1_PL_IPS 32u

typedef struct {
    int used;
    uint32_t ip;        /* source address, as the caller keys it */
    uint32_t fails;     /* guesses from this address since the window opened */
    uint64_t seen_ms;   /* last touch, for eviction */
    uint64_t win_ms;    /* start of the current welcome-rate window */
    uint32_t welcomes;  /* pairing sessions welcomed in that window */
} ava1_pl_ent_t;

typedef struct {
    ava1_pl_ent_t e[AVA1_PL_IPS];
    uint32_t total; /* guesses from everyone since the window opened */
    uint32_t per_ip_max, total_max, welcome_max;
    uint64_t welcome_win_ms;
} ava1_pairlimit_t;

/* 0 for any value selects its default (5 per address, 20 in all, 6 welcomes per 10 s). */
void ava1_pl_init(ava1_pairlimit_t *p, uint32_t per_ip_max, uint32_t total_max, uint32_t welcome_max,
                  uint64_t welcome_win_ms);
/* The window was (re)opened: every guess budget starts over. */
void ava1_pl_reset(ava1_pairlimit_t *p);
/* Whether `ip` may still guess (its budget and the global cap are not spent). */
int ava1_pl_guess_allowed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms);
/* A wrong guess from `ip`. Returns 1 when the global cap is now spent: close the window. */
int ava1_pl_guess_failed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms, uint32_t *ip_fails);
/* A new pairing session from `ip`: 0 when it already had its share in this window. */
int ava1_pl_welcome_allowed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms);

#endif
