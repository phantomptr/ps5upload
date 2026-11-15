/* "Remove PS5Upload from this PS5": see include/helper_uninstall.h.
 *
 * The folder half (hu_remove_tree, hu_mount_under) is host-tested with -DHU_HOST_TEST
 * (payload/tests/helper_uninstall_selftest.c); the mount table and the icons are console only. */
#include "helper_uninstall.h"

#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef HU_HOST_TEST
#include <sys/mount.h>

#include "config.h"
#include "icon_keeper.h"
#include "shell_builtin.h"
#endif

#define HU_MAX_DEPTH 64

static FILE *g_wipe_log;

static void wipe_note(const char *what, const char *path) {
    if (!g_wipe_log) return;
    fprintf(g_wipe_log, "%s %s\n", what, path);
    fflush(g_wipe_log);
    fsync(fileno(g_wipe_log));
}

static int is_kept(const char *path, const char *const *keep, size_t nkeep) {
    for (size_t i = 0; i < nkeep; i++)
        if (keep[i] && strcmp(path, keep[i]) == 0) return 1;
    return 0;
}

static void remove_at(const char *path, dev_t dev, int depth, const char *const *keep, size_t nkeep,
                      hu_counts_t *c) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    if (lstat(path, &st) != 0) {
        if (errno != ENOENT) c->failed++;
        return;
    }
    if (is_kept(path, keep, nkeep)) {
        c->kept++;
        return;
    }
    if (!S_ISDIR(st.st_mode)) {
        /* A file, a link (removed as a name, never followed), a socket. */
        if (unlink(path) == 0) c->removed++;
        else if (errno != ENOENT) c->failed++;
        return;
    }
    /* Another filesystem mounted here (an image, a share, a nullfs): never go in. */
    if (st.st_dev != dev) {
        c->kept++;
        return;
    }
    if (depth >= HU_MAX_DEPTH) {
        c->failed++;
        return;
    }
    d = opendir(path);
    if (!d) {
        c->failed++;
        return;
    }
    while ((e = readdir(d)) != NULL) {
        char child[1024];
        int k;
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        k = snprintf(child, sizeof child, "%s/%s", path, e->d_name);
        if (k < 0 || (size_t)k >= sizeof child) {
            c->failed++;
            continue;
        }
        if (depth == 0) wipe_note("entry", child);
        remove_at(child, dev, depth + 1, keep, nkeep, c);
    }
    closedir(d);
    if (rmdir(path) == 0) c->removed++;
    else if (errno != ENOTEMPTY && errno != EEXIST && errno != ENOENT) c->failed++;
    /* Not empty: it holds something kept (counted there), or a failure (counted there). */
}

void hu_remove_tree(const char *path, const char *const *keep, size_t nkeep, hu_counts_t *c) {
    struct stat st;
    if (!path || !c || lstat(path, &st) != 0) return;
    remove_at(path, st.st_dev, 0, keep, nkeep, c);
}

int hu_mount_under(const char *mp, const char *root) {
    size_t n = strlen(root);
    if (!mp || strncmp(mp, root, n) != 0) return 0;
    return mp[n] == 0 || mp[n] == '/';
}

size_t hu_source_keeps(const char *const *froms, size_t n, char (*out)[HU_KEEP_PATH], size_t cap) {
    static const char *const roots[] = {HU_ROOT, HU_USER_ROOT};
    size_t w = 0;
    for (size_t i = 0; i < n; i++) {
        const char *f = froms[i];
        for (size_t r = 0; f && r < 2; r++) {
            size_t len = strlen(roots[r]);
            if (!hu_mount_under(f, roots[r])) continue;
            /* The same folder under both names: the delete walks both roots. */
            for (size_t k = 0; k < 2 && w < cap; k++) {
                int m = snprintf(out[w], HU_KEEP_PATH, "%s%s", roots[k], f + len);
                if (m > 0 && m < HU_KEEP_PATH) w++;
            }
            break;
        }
    }
    return w;
}

static int g_requested;
static int g_keep_backups;

void hu_request(int keep_backups) {
    g_keep_backups = keep_backups ? 1 : 0;
    __atomic_store_n(&g_requested, 1, __ATOMIC_SEQ_CST);
}

int hu_requested(void) { return __atomic_load_n(&g_requested, __ATOMIC_SEQ_CST); }

#ifndef HU_HOST_TEST
/* Sources of the live mounts (f_mntfromname), into `froms` (pointers into `*m`, freed by the
 * caller). */
static size_t live_mount_sources(struct statfs **m, const char **froms, size_t cap) {
    int n = mntinfo_snapshot(m);
    size_t w = 0;
    for (int i = 0; i < n && *m && w < cap; i++) froms[w++] = (*m)[i].f_mntfromname;
    return w;
}
#endif

#define HU_MAX_KEEP 32

/* Main calls this last; the exit watchdog calls it too, so a slow stop cannot end the process
 * with the folders half removed or untouched. One of them does the work, the other waits. */
static pthread_mutex_t g_wipe_mu = PTHREAD_MUTEX_INITIALIZER;
static int g_wiped;

static void wipe_now(void);

void hu_wipe(void) {
    pthread_mutex_lock(&g_wipe_mu);
    if (!g_wiped) {
        wipe_now();
        g_wiped = 1;
    }
    pthread_mutex_unlock(&g_wipe_mu);
}

static void wipe_now(void) {
    hu_counts_t c = {0, 0, 0};
    static char sources[HU_MAX_KEEP][HU_KEEP_PATH];
    const char *keep[HU_MAX_KEEP + 2];
    size_t nkeep = 0, nsrc = 0;
#ifndef HU_HOST_TEST
    struct statfs *m = NULL;
    static const char *froms[512];
    nsrc = hu_source_keeps(froms, live_mount_sources(&m, froms, 512), sources, HU_MAX_KEEP);
    free(m);
#endif
    g_wipe_log = fopen(HU_WIPE_LOG, "w");
    for (size_t i = 0; i < nsrc; i++) wipe_note("keep (mounted from)", sources[i]);
    wipe_note("walk", HU_ROOT);
    keep[nkeep++] = HU_NP_BACKUP;
    if (g_keep_backups) keep[nkeep++] = HU_BACKUPS;
    for (size_t i = 0; i < nsrc; i++) keep[nkeep++] = sources[i];
    /* The user-path keeps match the second walk (/user/data/ps5upload is the same folder). */
    hu_remove_tree(HU_ROOT, keep, nkeep, &c);
    {
        char unp[2][HU_KEEP_PATH];
        const char *ukeep[HU_MAX_KEEP + 2];
        size_t un = 0;
        snprintf(unp[0], sizeof unp[0], "%s/np-backup", HU_USER_ROOT);
        snprintf(unp[1], sizeof unp[1], "%s/backups", HU_USER_ROOT);
        ukeep[un++] = unp[0];
        if (g_keep_backups) ukeep[un++] = unp[1];
        for (size_t i = 0; i < nsrc; i++) ukeep[un++] = sources[i];
        wipe_note("walk", HU_USER_ROOT);
        hu_remove_tree(HU_USER_ROOT, ukeep, un, &c);
    }
    if (unlink(HU_STARTUP_LOG) != 0 && errno != ENOENT) c.failed++;
    if (g_wipe_log) {
        fprintf(g_wipe_log, "done removed=%u kept=%u failed=%u\n", c.removed, c.kept, c.failed);
        fclose(g_wipe_log);
        g_wipe_log = NULL;
    }
}

#ifndef HU_HOST_TEST
int hu_mounts_in_use(char *out, size_t cap) {
    struct statfs *m = NULL;
    if (cap < 3) return -1;
    int n = mntinfo_snapshot(&m), found = 0, listed = 0;
    size_t at = 0;
    if (n <= 0 || !m) {
        free(m);
        snprintf(out, cap, "[]");
        return -1;
    }
    out[at++] = '[';
    for (int i = 0; i < n; i++) {
        const char *mp = m[i].f_mntonname;
        if (!hu_mount_under(mp, HU_ROOT) && !hu_mount_under(mp, HU_USER_ROOT)) continue;
        found++;
        /* Mount points here are paths the helper made: plain characters, no quote to escape. */
        if (strchr(mp, '"') || strchr(mp, '\\')) continue;
        int k = snprintf(out + at, cap - at, "%s\"%s\"", listed ? "," : "", mp);
        if (k < 0 || (size_t)k >= cap - at - 1) break;
        at += (size_t)k;
        listed++;
    }
    free(m);
    out[at++] = ']';
    out[at] = 0;
    return found;
}

int hu_show_hidden_icons(void) { return icon_keeper_show_all(WEBUI_TILE_TITLE_ID); }
#endif
