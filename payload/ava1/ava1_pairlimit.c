#include "ava1_pairlimit.h"

#include <string.h>

static ava1_pl_ent_t *find(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms, int create) {
    unsigned i, oldest = 0;
    for (i = 0; i < AVA1_PL_IPS; i++)
        if (p->e[i].used && p->e[i].ip == ip) {
            p->e[i].seen_ms = now_ms;
            return &p->e[i];
        }
    if (!create) return NULL;
    for (i = 0; i < AVA1_PL_IPS; i++)
        if (!p->e[i].used) break;
    if (i == AVA1_PL_IPS) {
        /* Evict the address that was touched longest ago. */
        for (i = 1; i < AVA1_PL_IPS; i++)
            if (p->e[i].seen_ms < p->e[oldest].seen_ms) oldest = i;
        i = oldest;
    }
    memset(&p->e[i], 0, sizeof p->e[i]);
    p->e[i].used = 1;
    p->e[i].ip = ip;
    p->e[i].seen_ms = now_ms;
    return &p->e[i];
}

void ava1_pl_init(ava1_pairlimit_t *p, uint32_t per_ip_max, uint32_t total_max, uint32_t welcome_max,
                  uint64_t welcome_win_ms) {
    memset(p, 0, sizeof *p);
    p->per_ip_max = per_ip_max ? per_ip_max : 5u;
    p->total_max = total_max ? total_max : 20u;
    p->welcome_max = welcome_max ? welcome_max : 6u;
    p->welcome_win_ms = welcome_win_ms ? welcome_win_ms : 10000u;
}

void ava1_pl_reset(ava1_pairlimit_t *p) {
    unsigned i;
    p->total = 0;
    for (i = 0; i < AVA1_PL_IPS; i++) p->e[i].fails = 0;
}

int ava1_pl_guess_allowed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms) {
    ava1_pl_ent_t *e = find(p, ip, now_ms, 0);
    if (p->total >= p->total_max) return 0;
    return !e || e->fails < p->per_ip_max;
}

int ava1_pl_guess_failed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms, uint32_t *ip_fails) {
    ava1_pl_ent_t *e = find(p, ip, now_ms, 1);
    e->fails++;
    p->total++;
    if (ip_fails) *ip_fails = e->fails;
    return p->total >= p->total_max;
}

int ava1_pl_welcome_allowed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms) {
    ava1_pl_ent_t *e = find(p, ip, now_ms, 1);
    if (now_ms - e->win_ms >= p->welcome_win_ms || e->welcomes == 0) {
        e->win_ms = now_ms;
        e->welcomes = 0;
    }
    if (e->welcomes >= p->welcome_max) return 0;
    e->welcomes++;
    return 1;
}
