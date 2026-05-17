/*
 * ShadowMountPlus metadata self-healer — see smp_meta.h for the why.
 *
 * Port of sonicloader/src/smp_meta.c with three deliberate diffs:
 *   1. No chmod_recursive(0777) on game dirs. Sonicloader needs write
 *      access for garlic-savemgr; we only read app dirs and write to
 *      /user/appmeta. 777-ing /user/app is destructive to permission-
 *      sensitive firmware paths and we don't need it.
 *   2. Lazy-start via smp_meta_init() rather than auto-on-boot — most
 *      ps5upload sessions are transfer-only, so paying for a 30 s
 *      pthread is wasted work. Desktop UI fires init() when the user
 *      opts in.
 *   3. Bounded `last_missing` updates use snprintf instead of strncpy
 *      to guarantee NUL termination even on truncation.
 */

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#include "smp_meta.h"

#define APP_DIR     "/user/app"
#define APPMETA_DIR "/user/appmeta"

#define POLL_SECONDS_DEFAULT  30
#define POLL_SECONDS_MIN       5
#define POLL_SECONDS_MAX     600

/* META_FILES order matters only insofar as the "icon0.png healed"
 * counter is reported separately from the "everything else" counter.
 * icon0.png is the load-bearing one for the blank-tile symptom; the
 * rest are nice-to-have so background art and the boot jingle resolve. */
static const char *META_FILES[] = {
    "icon0.png",   /* home-screen tile */
    "pic0.png",    /* loading screen art */
    "pic1.png",    /* full-bleed background */
    "icon1.png",
    "snd0.at9",    /* boot jingle — harmless to omit but cheap to copy */
    NULL,
};

#define PARAM_JSON  "sce_sys/param.json"

/* ── shared state ─────────────────────────────────────────────── */
static pthread_mutex_t  g_lock           = PTHREAD_MUTEX_INITIALIZER;
static atomic_int       g_poll_seconds   = POLL_SECONDS_DEFAULT;
static atomic_int       g_run_now_flag   = 0;
static smp_meta_stats_t g_stats          = {0};
static int              g_thread_started = 0;

/* ── helpers ──────────────────────────────────────────────────── */

/* TITLE_ID shape is CUSAxxxxx / PPSAxxxxx / NPXSxxxxx / FAKExxxxx /
 * IV9999 etc. Keep this lenient: 8-12 chars, all uppercase letters +
 * digits. Sony has invented several prefixes over the years and we
 * don't want to false-negative on a future one. */
static int title_id_looks_valid(const char *name) {
    size_t n = strlen(name);
    if (n < 8 || n > 12) return 0;
    for (size_t i = 0; i < n; i++) {
        char c = name[i];
        if (!((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9'))) return 0;
    }
    return 1;
}

static int file_exists_nonempty(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) return 0;
    return st.st_size > 0;
}

/* mkdir -p of a single path. EEXIST is fine. */
static int mkdir_one(const char *path) {
    if (mkdir(path, 0755) == 0) return 0;
    if (errno == EEXIST) return 0;
    return -1;
}

/* Buffered copy with partial-write handling. Returns 0 on success.
 * Unlinks the destination on any failure so we don't leave a
 * half-written icon0.png that file_exists_nonempty would later mistake
 * for "already healed". */
static int copy_file(const char *src, const char *dst) {
    int sfd = -1, dfd = -1;
    uint8_t buf[16384];
    ssize_t n;
    int rc = -1;

    if ((sfd = open(src, O_RDONLY)) < 0) goto done;
    if ((dfd = open(dst, O_CREAT | O_WRONLY | O_TRUNC, 0644)) < 0) goto done;

    while ((n = read(sfd, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(dfd, buf + off, n - off);
            if (w <= 0) goto done;
            off += w;
        }
    }
    if (n < 0) goto done;
    rc = 0;

done:
    if (sfd >= 0) close(sfd);
    if (dfd >= 0) close(dfd);
    if (rc != 0) unlink(dst);   /* don't leave half-written files behind */
    return rc;
}

/* Heal one TITLE_ID slot. Walks META_FILES and the param.json special
 * case. Returns 1 when icon0.png is in place at the end (whether we
 * copied it or it was already there); 0 otherwise. The caller uses the
 * return value to drive the "still missing" counter for the UI. */
static int heal_one_title(const char *title_id) {
    char appmeta_dir[256];
    char src_meta_dir[256];
    char src_path[384];
    char dst_path[384];
    int  icon_ok = 0;

    snprintf(appmeta_dir, sizeof(appmeta_dir),
             "%s/%s", APPMETA_DIR, title_id);
    /* SMP-mounted apps keep their sce_sys/ at the top of the app dir.
     * Some packages flatten metadata at the root (icon0.png next to
     * eboot.bin), so we try sce_sys/ first then bare-root as a fallback. */
    snprintf(src_meta_dir, sizeof(src_meta_dir),
             "%s/%s/sce_sys", APP_DIR, title_id);

    if (mkdir_one(APPMETA_DIR) < 0) return 0;
    if (mkdir_one(appmeta_dir) < 0) return 0;

    for (int i = 0; META_FILES[i]; i++) {
        snprintf(dst_path, sizeof(dst_path),
                 "%s/%s", appmeta_dir, META_FILES[i]);

        if (file_exists_nonempty(dst_path)) {
            if (!strcmp(META_FILES[i], "icon0.png")) icon_ok = 1;
            continue;
        }

        /* Try sce_sys/<file> first, then bare <file> at the app root. */
        snprintf(src_path, sizeof(src_path),
                 "%s/%s", src_meta_dir, META_FILES[i]);
        int copied = 0;
        if (file_exists_nonempty(src_path) &&
            copy_file(src_path, dst_path) == 0) {
            copied = 1;
        } else {
            snprintf(src_path, sizeof(src_path),
                     "%s/%s/%s", APP_DIR, title_id, META_FILES[i]);
            if (file_exists_nonempty(src_path) &&
                copy_file(src_path, dst_path) == 0) {
                copied = 1;
            }
        }

        if (copied) {
            pthread_mutex_lock(&g_lock);
            if (!strcmp(META_FILES[i], "icon0.png")) {
                g_stats.icons_healed++;
                icon_ok = 1;
            } else {
                g_stats.pics_healed++;
            }
            pthread_mutex_unlock(&g_lock);
        }
    }

    /* param.json carries the visible game name; without it the tile
     * shows "Unknown Title". Distinct counter so the UI can show
     * "icons / pics / json" healed separately. */
    snprintf(src_path, sizeof(src_path),
             "%s/%s/%s", APP_DIR, title_id, PARAM_JSON);
    snprintf(dst_path, sizeof(dst_path),
             "%s/param.json", appmeta_dir);
    if (!file_exists_nonempty(dst_path) &&
        file_exists_nonempty(src_path) &&
        copy_file(src_path, dst_path) == 0) {
        pthread_mutex_lock(&g_lock);
        g_stats.json_healed++;
        pthread_mutex_unlock(&g_lock);
    }

    return icon_ok;
}

/* One full sweep over /user/app. */
static void sweep_once(void) {
    DIR *d = opendir(APP_DIR);
    if (!d) {
        /* /user/app doesn't exist — pre-jailbreak or no games installed
         * yet. Not an error; just a no-op sweep that bumps last_run. */
        pthread_mutex_lock(&g_lock);
        g_stats.last_run_unix = (uint64_t)time(NULL);
        pthread_mutex_unlock(&g_lock);
        return;
    }

    int local_scanned = 0;
    int local_missing = 0;
    char last_missing[64] = "";

    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.') continue;
        if (!title_id_looks_valid(e->d_name)) continue;

        /* Skip non-directories. d_type may be DT_UNKNOWN on some FSes;
         * fall through to a stat() probe in that case. */
        if (e->d_type != DT_DIR && e->d_type != DT_UNKNOWN) continue;
        if (e->d_type == DT_UNKNOWN) {
            char probe[256];
            snprintf(probe, sizeof(probe), "%s/%s", APP_DIR, e->d_name);
            struct stat st;
            if (stat(probe, &st) != 0 || !S_ISDIR(st.st_mode)) continue;
        }

        local_scanned++;

        int healthy = heal_one_title(e->d_name);
        if (!healthy) {
            local_missing++;
            /* snprintf guarantees NUL termination even on truncation;
             * sonicloader's strncpy+manual-NUL is harder to read and
             * leaves stale chars on a shorter-than-buffer overwrite. */
            snprintf(last_missing, sizeof(last_missing), "%s", e->d_name);
        }
    }
    closedir(d);

    pthread_mutex_lock(&g_lock);
    g_stats.games_scanned = local_scanned;
    g_stats.still_missing = local_missing;
    snprintf(g_stats.last_missing, sizeof(g_stats.last_missing),
             "%s", local_missing > 0 ? last_missing : "");
    g_stats.last_run_unix = (uint64_t)time(NULL);
    pthread_mutex_unlock(&g_lock);
}

/* ── worker thread ────────────────────────────────────────────── */

static void *worker_thread_fn(void *arg) {
    (void)arg;
    /* Best-effort thread name for ps/top output; ignored if the syscall
     * isn't available. SYS_thr_set_name is FreeBSD-specific. */
    (void)syscall(SYS_thr_set_name, -1, "ps5upload-smp");

    pthread_mutex_lock(&g_lock);
    g_stats.running = 1;
    pthread_mutex_unlock(&g_lock);

    /* Wait up to 60 s for kstuff + SMP to fully settle before the first
     * sweep. Sonicloader observed SIGILL in kstuff's ZeroConf thread
     * when chmod_recursive ran during the kstuff init window; we don't
     * chmod, but the safe-startup pause is cheap and matches their
     * proven timing. A run_now trigger short-circuits the wait. */
    for (int i = 0; i < 60; i++) {
        if (atomic_load(&g_run_now_flag)) break;
        sleep(1);
    }
    atomic_store(&g_run_now_flag, 0);
    sweep_once();

    while (1) {
        int interval = atomic_load(&g_poll_seconds);
        if (interval < POLL_SECONDS_MIN) interval = POLL_SECONDS_MIN;
        if (interval > POLL_SECONDS_MAX) interval = POLL_SECONDS_MAX;

        /* Sleep in 1 s slices so a run_now flag flips inside one tick.
         * Same shape as the fan watcher in hw_info.c. */
        for (int i = 0; i < interval; i++) {
            if (atomic_load(&g_run_now_flag)) break;
            sleep(1);
        }
        atomic_store(&g_run_now_flag, 0);

        sweep_once();
    }
    return NULL;
}

/* ── public API ───────────────────────────────────────────────── */

int smp_meta_init(void) {
    pthread_mutex_lock(&g_lock);
    if (g_thread_started) {
        pthread_mutex_unlock(&g_lock);
        return 0;
    }
    g_thread_started = 1;
    pthread_mutex_unlock(&g_lock);

    pthread_t t;
    if (pthread_create(&t, NULL, worker_thread_fn, NULL) != 0) {
        /* Roll the flag back so the caller can retry. */
        pthread_mutex_lock(&g_lock);
        g_thread_started = 0;
        pthread_mutex_unlock(&g_lock);
        return -1;
    }
    pthread_detach(t);
    return 0;
}

void smp_meta_get_stats(smp_meta_stats_t *out) {
    if (!out) return;
    memset(out, 0, sizeof(*out));
    pthread_mutex_lock(&g_lock);
    *out = g_stats;
    out->poll_seconds = atomic_load(&g_poll_seconds);
    pthread_mutex_unlock(&g_lock);
}

int smp_meta_run_now(void) {
    atomic_store(&g_run_now_flag, 1);
    return 0;
}

int smp_meta_set_poll_seconds(int seconds) {
    if (seconds < POLL_SECONDS_MIN) seconds = POLL_SECONDS_MIN;
    if (seconds > POLL_SECONDS_MAX) seconds = POLL_SECONDS_MAX;
    atomic_store(&g_poll_seconds, seconds);
    return seconds;
}

int smp_meta_get_poll_seconds(void) {
    return atomic_load(&g_poll_seconds);
}
