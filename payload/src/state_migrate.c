#include "state_migrate.h"

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#define MIGRATE_MAX_DEPTH 32
#define MIGRATE_PATH_MAX 1024

/* Removes `path` (a file, a link or a tree) and everything under it that lives on device `dev`.
 * Never follows a link, never crosses a mount, never renames. Returns 0 or -1. */
static int remove_tree(const char *path, unsigned long long dev, int depth) {
    struct stat st;
    if (lstat(path, &st) != 0) return errno == ENOENT ? 0 : -1;
    if (!S_ISDIR(st.st_mode)) return unlink(path) == 0 || errno == ENOENT ? 0 : -1;
    if ((unsigned long long)st.st_dev != dev || depth > MIGRATE_MAX_DEPTH) return -1;
    DIR *d = opendir(path);
    if (!d) return -1;
    int rc = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        char child[MIGRATE_PATH_MAX];
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        int n = snprintf(child, sizeof child, "%s/%s", path, e->d_name);
        if (n < 0 || (size_t)n >= sizeof child) {
            rc = -1;
            continue;
        }
        if (remove_tree(child, dev, depth + 1) != 0) rc = -1;
    }
    closedir(d);
    if (rmdir(path) != 0 && errno != ENOENT) rc = -1;
    return rc;
}

int payload_remove_retired_dirs(const char *root) {
    static const char *const retired[] = {"tx", "spool"};
    int rc = 0;
    if (!root || !*root) return -1;
    for (size_t i = 0; i < sizeof retired / sizeof retired[0]; i++) {
        char path[MIGRATE_PATH_MAX];
        struct stat st;
        int n = snprintf(path, sizeof path, "%s/%s", root, retired[i]);
        if (n < 0 || (size_t)n >= sizeof path) {
            rc = -1;
            continue;
        }
        if (lstat(path, &st) != 0) continue; /* already gone */
        if (remove_tree(path, (unsigned long long)st.st_dev, 0) != 0) {
            fprintf(stderr, "[payload2] could not remove the retired folder %s: %s\n", path, strerror(errno));
            rc = -1;
        } else {
            fprintf(stderr, "[payload2] removed the retired folder %s\n", path);
        }
    }
    return rc;
}
