#ifndef PS5UPLOAD2_PLAY_PICK_H
#define PS5UPLOAD2_PLAY_PICK_H

/* Which running title is being played right now, for play-time tracking.
 *
 * The tracker used to credit the FIRST title process in the kernel's list.
 * A PS5 keeps a game suspended in the background while you play another, so
 * whichever of the two came first got the time: games played for minutes
 * showed hours, and the game actually played showed minutes.
 *
 * Played = using the CPU. The title on screen renders and climbs steadily in
 * accumulated CPU time; a suspended one, or any title while the console is
 * in rest mode, barely moves. So between two polls the title whose CPU time
 * grew the most, by at least PLAY_ACTIVE_PERMILLE of one core, is the one
 * being played. Only sysctl fields: no Sony call, no firmware dependency.
 *
 * Pure and header-only so the host selftest runs the same code. */

#include <stdint.h>
#include <string.h>

#define PLAY_PICK_MAX 32
/* 2% of one core: far below any game drawing frames, far above a
 * suspended process. */
#define PLAY_ACTIVE_PERMILLE 20

typedef struct {
    int      pid;
    char     title_id[16];
    uint64_t runtime_us; /* ki_runtime: accumulated CPU microseconds */
    int      stopped;    /* ki_stat == SSTOP */
} play_proc_t;

/* Index into `now` of the title being played, or -1 when none is.
 *
 * `prev` is the previous poll's list, `interval_us` the time between the
 * two. A process seen for the first time has no CPU delta yet: it is picked
 * only when nothing measured qualifies and it is the only new one (a game
 * that was just started), so two unmeasured titles never guess. */
static inline int play_pick(const play_proc_t *now, int n,
                            const play_proc_t *prev, int nprev,
                            uint64_t interval_us) {
    uint64_t min_delta = interval_us / 1000u * PLAY_ACTIVE_PERMILLE;
    int best = -1, fresh = -1, nfresh = 0;
    uint64_t best_delta = 0;
    for (int i = 0; i < n; i++) {
        if (now[i].stopped) continue;
        int j;
        for (j = 0; j < nprev; j++)
            if (prev[j].pid == now[i].pid &&
                strcmp(prev[j].title_id, now[i].title_id) == 0) break;
        if (j == nprev) {
            fresh = i;
            nfresh++;
            continue;
        }
        if (now[i].runtime_us < prev[j].runtime_us) continue;
        uint64_t d = now[i].runtime_us - prev[j].runtime_us;
        if (d >= min_delta && (best < 0 || d > best_delta)) {
            best = i;
            best_delta = d;
        }
    }
    if (best >= 0) return best;
    return nfresh == 1 ? fresh : -1;
}

/* Whether `pid` (with `title_id`) was in the previous poll: a title coming
 * back to the screen is not a new launch. */
static inline int play_seen_before(const play_proc_t *prev, int nprev,
                                   const play_proc_t *p) {
    for (int j = 0; j < nprev; j++)
        if (prev[j].pid == p->pid && strcmp(prev[j].title_id, p->title_id) == 0)
            return 1;
    return 0;
}

#endif /* PS5UPLOAD2_PLAY_PICK_H */
