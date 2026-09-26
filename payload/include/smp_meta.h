/*
 * ShadowMountPlus metadata self-healer.
 *
 * SMP mounts USB / external-storage games into /user/app/<TITLE_ID>/ but
 * sometimes — racing the firmware's own metadata builder, or hitting
 * ENOSPC on /user, or when the package nests sce_sys differently — it
 * leaves /user/appmeta/<TITLE_ID>/ without an icon0.png. The on-disk
 * symptom is a blank home-screen tile for a game that otherwise launches
 * fine. Reinstalling SMP doesn't help: the app dir is healthy, only the
 * appmeta side is incomplete.
 *
 * We can't patch SMP (it's a third-party ELF the user pulls fresh from
 * GitHub) so we run a background sweep that diffs /user/app vs
 * /user/appmeta and copies the missing files from the game's sce_sys/.
 * Mirrors sonicloader's src/smp_meta.c approach; ours intentionally
 * drops sonicloader's chmod_recursive(0777) step because we don't need
 * write access into the app dirs — only into appmeta.
 *
 * Off by default. The desktop UI explicitly starts the watcher when the
 * user opts in from the Hardware → SMP Meta Heal panel.
 */
#ifndef PS5UPLOAD2_SMP_META_H
#define PS5UPLOAD2_SMP_META_H

#include <stdint.h>

/* Stat snapshot returned by `smp_meta_get_stats`. Matches the JSON
 * shape the engine wraps into the FTX2 STATS_ACK frame. */
typedef struct {
    int      running;            /* 1 once the watcher thread is alive */
    int      poll_seconds;       /* tick interval (min 5, max 600) */
    uint64_t last_run_unix;      /* seconds since epoch of last sweep */
    int      games_scanned;      /* TITLE_ID dirs walked on last sweep */
    int      icons_healed;       /* lifetime count of icon0.png writes */
    int      pics_healed;        /* lifetime count of picN/iconN/snd0 writes */
    int      json_healed;        /* lifetime count of param.json writes */
    int      still_missing;      /* games where icon0.png couldn't be sourced */
    char     last_missing[64];   /* TITLE_ID of the most recent unfixable game */
} smp_meta_stats_t;

/* Idempotent. First call launches the watcher (detached pthread, syscall
 * thread name "ps5upload-smp"). Returns 0 on success, -1 on
 * pthread_create failure (in which case a future call can retry). */
int smp_meta_init(void);

/* Snapshot of current stats. Locking is internal — safe from any
 * thread. Always zero-fills `out` first so partial reads are safe. */
void smp_meta_get_stats(smp_meta_stats_t *out);

/* Trigger an immediate sweep (instead of waiting for the next tick).
 * The trigger is recorded as a flag the watcher polls between sleeps,
 * so this returns immediately; the sweep runs on the worker thread. */
int smp_meta_run_now(void);

/* Tune the poll interval. Clamped to [5, 600] seconds. Returns the
 * post-clamp value actually stored. */
int smp_meta_set_poll_seconds(int seconds);

int smp_meta_get_poll_seconds(void);

#endif /* PS5UPLOAD2_SMP_META_H */
