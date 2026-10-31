/* Home-screen metadata repair for titles under /user/app (see include/smp_meta.h).
 *
 * One detached worker thread walks /user/app/<TITLE_ID>/ and, for each title,
 * fills in whatever tile art / param.json is missing from /user/appmeta/<TITLE_ID>/
 * by copying it from the game's own folder. The game folder is only ever read.
 *
 * Every write lands as a temporary file beside its destination and is renamed into
 * place in the same folder, so the rename can never cross a mount. */
#include "smp_meta.h"
#include "proc_identity.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#ifndef O_CLOEXEC
#define O_CLOEXEC 0
#endif

#define SMP_POLL_DEFAULT 30
#define SMP_POLL_MIN 5
#define SMP_POLL_MAX 600
#define SMP_STARTUP_WAIT_S 60
#define SMP_COPY_BUF (64u * 1024u)
#define SMP_COPY_MAX ((off_t)32 * 1024 * 1024)
#define SMP_PATH_CAP 512
#define SMP_THREAD_STACK (256u * 1024u)

/* The tile first: whether a title "has its tile" is decided by index 0. */
static const char *const k_art_files[] = {
    "icon0.png", "pic0.png", "pic1.png", "icon1.png", "snd0.at9",
};

static char g_app_root[SMP_PATH_CAP] = "/user/app";
static char g_meta_root[SMP_PATH_CAP] = "/user/appmeta";

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static smp_meta_stats_t g_stats; /* guarded by g_mu; poll_seconds lives in g_poll */
static int g_started;            /* guarded by g_mu */
static atomic_int g_poll = SMP_POLL_DEFAULT;
static atomic_int g_wake; /* 1 = a sweep was asked for and not yet picked up */

static void stats_add(int *field) {
    pthread_mutex_lock(&g_mu);
    (*field)++;
    pthread_mutex_unlock(&g_mu);
}

/* 8 to 12 characters, each A-Z or 0-9. */
static int title_id_ok(const char *s) {
    size_t n = 0;
    for (; s[n]; n++) {
        char c = s[n];
        if (!((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9'))) return 0;
        if (n >= 12) return 0;
    }
    return n >= 8;
}

/* snprintf that reports truncation as failure. */
static int join(char *out, size_t cap, const char *a, const char *b, const char *c) {
    int n = snprintf(out, cap, "%s/%s%s%s", a, b, c ? "/" : "", c ? c : "");
    return n > 0 && (size_t)n < cap;
}

/* A plain, non-empty file (never a link, fifo or device). */
static int is_nonempty_regular(const char *path) {
    struct stat st;
    return lstat(path, &st) == 0 && S_ISREG(st.st_mode) && st.st_size > 0;
}

/* mkdir that accepts an existing directory, but not a link or a file in its place. */
static int ensure_dir(const char *path) {
    if (mkdir(path, 0755) != 0 && errno != EEXIST) return -1;
    struct stat st;
    return (lstat(path, &st) == 0 && S_ISDIR(st.st_mode)) ? 0 : -1;
}

static int write_all(int fd, const unsigned char *p, size_t n) {
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (w == 0) return -1;
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

/* Copy src to dst through "<dst>.tmp-<pid>" and a same-folder rename. On any failure the
 * temporary is removed and dst is left as it was. 0 on success. */
static int copy_file(const char *src, const char *dst) {
    char tmp[SMP_PATH_CAP];
    int n = snprintf(tmp, sizeof tmp, "%s.tmp-%ld", dst, (long)getpid());
    if (n <= 0 || (size_t)n >= sizeof tmp) return -1;

    int in = open(src, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (in < 0) return -1;
    struct stat st;
    if (fstat(in, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0 ||
        st.st_size > SMP_COPY_MAX) {
        close(in);
        return -1;
    }

    const int oflags = O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC;
    int out = open(tmp, oflags, 0644);
    if (out < 0 && errno == EEXIST) {
        /* A leftover from an interrupted run with the same pid: unlink removes the
         * name itself (a link is not followed), then try once more. */
        (void)unlink(tmp);
        out = open(tmp, oflags, 0644);
    }
    if (out < 0) {
        close(in);
        return -1;
    }

    int ok = 0;
    unsigned char *buf = malloc(SMP_COPY_BUF);
    if (buf) {
        off_t total = 0;
        ok = 1;
        for (;;) {
            ssize_t r = read(in, buf, SMP_COPY_BUF);
            if (r < 0) {
                if (errno == EINTR) continue;
                ok = 0;
                break;
            }
            if (r == 0) break;
            total += r;
            if (total > SMP_COPY_MAX || write_all(out, buf, (size_t)r) != 0) {
                ok = 0;
                break;
            }
        }
        if (total <= 0) ok = 0;
        free(buf);
    }
    close(in);
    if (ok && fsync(out) != 0) ok = 0;
    if (close(out) != 0) ok = 0;
    /* tmp and dst are siblings in one folder: never a cross-device rename. */
    if (ok && rename(tmp, dst) != 0) ok = 0;
    if (!ok) {
        (void)unlink(tmp);
        return -1;
    }
    return 0;
}

/* Is the destination already there? 1 = present (non-empty regular file), 0 = absent or
 * empty (may be written), -1 = something else occupies the name (a link, a folder): left
 * alone, never replaced. */
static int dst_state(const char *dst) {
    struct stat st;
    if (lstat(dst, &st) != 0) return 0;
    if (!S_ISREG(st.st_mode)) return -1;
    return st.st_size > 0 ? 1 : 0;
}

/* Bring one file in from the game folder. `sources` is a NULL-terminated list of folders
 * to look in, in order. 1 = present before, 2 = copied now, 0 = still missing. */
static int heal_file(const char *meta_dir, const char *const *sources, const char *name) {
    char dst[SMP_PATH_CAP];
    if (!join(dst, sizeof dst, meta_dir, name, NULL)) return 0;
    int have = dst_state(dst);
    if (have == 1) return 1;
    if (have < 0) return 0;
    for (size_t i = 0; sources[i]; i++) {
        char src[SMP_PATH_CAP];
        if (!join(src, sizeof src, sources[i], name, NULL)) continue;
        if (!is_nonempty_regular(src)) continue;
        return copy_file(src, dst) == 0 ? 2 : 0;
    }
    return 0;
}

/* One title. Returns 1 when it ends with a tile image, 0 when it does not. */
static int heal_title(const char *id) {
    char app_dir[SMP_PATH_CAP], sce_sys[SMP_PATH_CAP], meta_dir[SMP_PATH_CAP];
    if (!join(app_dir, sizeof app_dir, g_app_root, id, NULL) ||
        !join(sce_sys, sizeof sce_sys, g_app_root, id, "sce_sys") ||
        !join(meta_dir, sizeof meta_dir, g_meta_root, id, NULL))
        return 0;
    if (ensure_dir(g_meta_root) != 0 || ensure_dir(meta_dir) != 0) return 0;

    const char *const art_sources[] = {sce_sys, app_dir, NULL};
    int has_tile = 0;
    for (size_t i = 0; i < sizeof k_art_files / sizeof k_art_files[0]; i++) {
        int r = heal_file(meta_dir, art_sources, k_art_files[i]);
        if (r == 2) {
            pthread_mutex_lock(&g_mu);
            if (i == 0)
                g_stats.icons_healed++;
            else
                g_stats.pics_healed++;
            pthread_mutex_unlock(&g_mu);
            fprintf(stderr, "[smp-meta] %s: restored %s\n", id, k_art_files[i]);
        }
        if (i == 0) has_tile = r != 0;
    }

    const char *const json_sources[] = {sce_sys, NULL};
    if (heal_file(meta_dir, json_sources, "param.json") == 2) {
        stats_add(&g_stats.json_healed);
        fprintf(stderr, "[smp-meta] %s: restored param.json\n", id);
    }
    return has_tile;
}

static int entry_is_dir(const struct dirent *de) {
#ifdef DT_DIR
    if (de->d_type == DT_DIR) return 1;
    if (de->d_type != DT_UNKNOWN) return 0;
#endif
    char path[SMP_PATH_CAP];
    struct stat st;
    return join(path, sizeof path, g_app_root, de->d_name, NULL) && stat(path, &st) == 0 &&
           S_ISDIR(st.st_mode);
}

static void sweep(void) {
    int scanned = 0, missing = 0;
    char last_missing[sizeof g_stats.last_missing] = {0};
    DIR *d = opendir(g_app_root);
    if (!d) {
        fprintf(stderr, "[smp-meta] cannot read %s (errno %d)\n", g_app_root, errno);
    } else {
        struct dirent *de;
        while ((de = readdir(d)) != NULL) {
            if (!title_id_ok(de->d_name) || !entry_is_dir(de)) continue;
            scanned++;
            if (!heal_title(de->d_name)) {
                missing++;
                snprintf(last_missing, sizeof last_missing, "%.*s",
                         (int)(sizeof last_missing - 1), de->d_name);
            }
        }
        closedir(d);
    }
    pthread_mutex_lock(&g_mu);
    g_stats.games_scanned = scanned;
    g_stats.still_missing = missing;
    if (last_missing[0]) memcpy(g_stats.last_missing, last_missing, sizeof last_missing);
    g_stats.last_run_unix = (uint64_t)time(NULL);
    pthread_mutex_unlock(&g_mu);
}

/* Sleep up to `seconds`, a second at a time; a pending run_now ends it early (and is
 * consumed, so it causes exactly one sweep). */
static void wait_or_wake(int seconds) {
    for (int i = 0; i < seconds; i++) {
        if (atomic_exchange(&g_wake, 0)) return;
        sleep(1);
    }
    (void)atomic_exchange(&g_wake, 0);
}

static void *worker(void *arg) {
    (void)arg;
#ifdef SYS_thr_set_name
    proc_name_set_self("ps5upload-smp");
#endif
    pthread_mutex_lock(&g_mu);
    g_stats.running = 1;
    pthread_mutex_unlock(&g_mu);
    /* Let the jailbreak payloads and the mounter settle before the first pass. */
    wait_or_wake(SMP_STARTUP_WAIT_S);
    for (;;) {
        sweep();
        wait_or_wake(atomic_load(&g_poll));
    }
    return NULL;
}

int smp_meta_init(void) {
    pthread_mutex_lock(&g_mu);
    if (g_started) {
        pthread_mutex_unlock(&g_mu);
        return 0;
    }
    int rc = -1;
    pthread_attr_t a;
    if (pthread_attr_init(&a) == 0) {
        (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
        (void)pthread_attr_setstacksize(&a, SMP_THREAD_STACK);
        pthread_t t;
        if (pthread_create(&t, &a, worker, NULL) == 0) {
            g_started = 1;
            rc = 0;
        }
        pthread_attr_destroy(&a);
    }
    pthread_mutex_unlock(&g_mu);
    if (rc != 0) fprintf(stderr, "[smp-meta] could not start the worker thread\n");
    return rc;
}

void smp_meta_get_stats(smp_meta_stats_t *out) {
    if (!out) return;
    memset(out, 0, sizeof *out);
    pthread_mutex_lock(&g_mu);
    *out = g_stats;
    pthread_mutex_unlock(&g_mu);
    out->poll_seconds = atomic_load(&g_poll);
    out->last_missing[sizeof out->last_missing - 1] = '\0';
}

int smp_meta_run_now(void) {
    atomic_store(&g_wake, 1);
    return 0;
}

int smp_meta_set_poll_seconds(int seconds) {
    if (seconds < SMP_POLL_MIN) seconds = SMP_POLL_MIN;
    if (seconds > SMP_POLL_MAX) seconds = SMP_POLL_MAX;
    atomic_store(&g_poll, seconds);
    return seconds;
}

int smp_meta_get_poll_seconds(void) { return atomic_load(&g_poll); }

#ifdef SMP_META_HOST_TEST
void smp_meta_set_roots_for_test(const char *app_dir, const char *appmeta_dir) {
    snprintf(g_app_root, sizeof g_app_root, "%s", app_dir);
    snprintf(g_meta_root, sizeof g_meta_root, "%s", appmeta_dir);
}

void smp_meta_sweep_for_test(void) { sweep(); }

void smp_meta_reset_stats_for_test(void) {
    pthread_mutex_lock(&g_mu);
    memset(&g_stats, 0, sizeof g_stats);
    pthread_mutex_unlock(&g_mu);
}
#endif
