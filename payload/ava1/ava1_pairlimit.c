#include "ava1_pairlimit.h"

#include <string.h>

/* The entry for `ip`. With `create`, a missing one takes a free slot, else the least recently
 * touched slot that holds no spent budget. An entry with guesses in this window is never
 * evicted (its removal would hand the address a fresh budget): NULL when every slot is such. */
static ava1_pl_ent_t *find(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms, int create) {
    unsigned i, pick = AVA1_PL_IPS;
    for (i = 0; i < AVA1_PL_IPS; i++)
        if (p->e[i].used && p->e[i].ip == ip) {
            p->e[i].seen_ms = now_ms;
            return &p->e[i];
        }
    if (!create) return NULL;
    for (i = 0; i < AVA1_PL_IPS; i++) {
        if (!p->e[i].used) {
            pick = i;
            break;
        }
        if (p->e[i].fails == 0 && (pick == AVA1_PL_IPS || p->e[i].seen_ms < p->e[pick].seen_ms)) pick = i;
    }
    if (pick == AVA1_PL_IPS) return NULL;
    memset(&p->e[pick], 0, sizeof p->e[pick]);
    p->e[pick].used = 1;
    p->e[pick].ip = ip;
    p->e[pick].seen_ms = now_ms;
    return &p->e[pick];
}

/* Whether an unseen address could get an entry. */
static int room(const ava1_pairlimit_t *p) {
    unsigned i;
    for (i = 0; i < AVA1_PL_IPS; i++)
        if (!p->e[i].used || p->e[i].fails == 0) return 1;
    return 0;
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
    if (!e) return room(p); /* an unseen address needs a slot, and every one may be spent */
    return e->fails < p->per_ip_max;
}

int ava1_pl_reserve(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms, uint32_t *ip_fails) {
    ava1_pl_ent_t *e;
    if (!ava1_pl_guess_allowed(p, ip, now_ms)) return 0;
    e = find(p, ip, now_ms, 1);
    if (e) e->fails++;
    p->total++;
    if (ip_fails) *ip_fails = e ? e->fails : 0;
    return 1;
}

void ava1_pl_release(ava1_pairlimit_t *p, uint32_t ip) {
    ava1_pl_ent_t *e = find(p, ip, 0, 0);
    if (e && e->fails) e->fails--;
    if (p->total) p->total--;
}

int ava1_pl_spent(const ava1_pairlimit_t *p) { return p->total >= p->total_max; }

int ava1_pl_welcome_allowed(ava1_pairlimit_t *p, uint32_t ip, uint64_t now_ms) {
    ava1_pl_ent_t *e = find(p, ip, now_ms, 1);
    if (!e) return 0; /* every slot holds a spent budget: no new pairing sessions from unseen addresses */
    if (now_ms - e->win_ms >= p->welcome_win_ms || e->welcomes == 0) {
        e->win_ms = now_ms;
        e->welcomes = 0;
    }
    if (e->welcomes >= p->welcome_max) return 0;
    e->welcomes++;
    return 1;
}
