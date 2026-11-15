/* "Remove PS5Upload from this PS5": see include/helper_uninstall.h.
 *
 * The folder half (hu_remove_tree, hu_mount_under) is host-tested with -DHU_HOST_TEST
 * (payload/tests/helper_uninstall_selftest.c); the mount table and the icons are console only. */
#include "helper_uninstall.h"

#include <dirent.h>
#include <errno.h>
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

static int g_requested;
static int g_keep_backups;

void hu_request(int keep_backups) {
    g_keep_backups = keep_backups ? 1 : 0;
    __atomic_store_n(&g_requested, 1, __ATOMIC_SEQ_CST);
}

int hu_requested(void) { return __atomic_load_n(&g_requested, __ATOMIC_SEQ_CST); }

void hu_wipe(void) {
    hu_counts_t c = {0, 0, 0};
    const char *keep[1] = {HU_BACKUPS};
    hu_remove_tree(HU_ROOT, keep, g_keep_backups ? 1 : 0, &c);
    hu_remove_tree(HU_USER_ROOT, NULL, 0, &c);
    if (unlink(HU_STARTUP_LOG) != 0 && errno != ENOENT) c.failed++;
}

#ifndef HU_HOST_TEST
int hu_mounts_in_use(char *out, size_t cap) {
    struct statfs *m = NULL;
    int n = mntinfo_snapshot(&m), found = 0;
    size_t at = 0;
    if (cap < 3) return -1;
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
        int k = snprintf(out + at, cap - at, "%s\"%s\"", found > 1 ? "," : "", mp);
        if (k < 0 || (size_t)k >= cap - at - 1) break;
        at += (size_t)k;
    }
    free(m);
    out[at++] = ']';
    out[at] = 0;
    return found;
}

int hu_show_hidden_icons(void) { return icon_keeper_show_all(WEBUI_TILE_TITLE_ID); }
#endif
