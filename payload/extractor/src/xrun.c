/* Picks the back end for an archive, and finds a RAR set's volumes. */
#include "xjob.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "archive_name.h"

const char *xj_kind(const char *archive) {
    const char *slash = strrchr(archive, '/');
    return archive_kind(slash ? slash + 1 : archive);
}

/* Whether the request's top-level renames leave an entry out ("from//": an empty `to`). */
static int leaves_out(const char *map) {
    const char *p = map;
    while (*p) {
        const char *to = p + strcspn(p, "/");
        size_t tl;
        if (*to != '/') return 0;
        to++;
        tl = strcspn(to, "/");
        if (tl == 0) return 1;
        p = to + tl;
        if (*p == '/') p++;
    }
    return 0;
}

int xj_run(xjob_t *j) {
    const char *kind = xj_kind(j->archive);
    struct stat st;
    if (stat(j->archive, &st) != 0 || !S_ISREG(st.st_mode)) return xj_fail(j, "fs_extract_open_failed");
    if (!kind) return xj_fail(j, "fs_extract_not_archive");
    /* What is left out would go with the archive, unseen: freeing only takes all of it. */
    if (j->free_req && !j->list && leaves_out(j->top_map))
        return xj_fail(j, "fs_extract_free_refused: some items are left out");
    if (strcmp(kind, "rar") == 0) return xrar_extract(j);
    return x7z_extract(j);
}

void xj_free_list(char **list) {
    if (!list) return;
    for (char **p = list; *p; p++) free(*p);
    free(list);
}

static int push(char ***list, size_t *n, size_t *cap, const char *s) {
    if (*n + 2 > *cap) {
        size_t nc = *cap ? *cap * 2 : 8;
        char **g = realloc(*list, nc * sizeof *g);
        if (!g) return -1;
        *list = g;
        *cap = nc;
    }
    (*list)[*n] = strdup(s);
    if (!(*list)[*n]) return -1;
    (*n)++;
    (*list)[*n] = NULL;
    return 0;
}

static int exists(const char *p) {
    struct stat st;
    return stat(p, &st) == 0 && S_ISREG(st.st_mode);
}

int xj_rar_volume_name(const char *first, unsigned k, char *out, size_t cap) {
    return archive_volume_name(first, k, out, cap);
}

char **xj_rar_volumes(const char *first) {
    char **list = NULL, buf[XJ_PATH + 16];
    size_t n = 0, cap = 0;
    if (strlen(first) >= XJ_PATH) return NULL;
    if (push(&list, &n, &cap, first) != 0) goto oom;
    for (unsigned k = 1; k < 100000; k++) {
        if (xj_rar_volume_name(first, k, buf, sizeof buf) != 0 || !exists(buf)) break;
        if (push(&list, &n, &cap, buf) != 0) goto oom;
    }
    return list;
oom:
    xj_free_list(list);
    return NULL;
}
