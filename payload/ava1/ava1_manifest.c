#include "ava1_manifest.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "ava1_wire.h"
#include "blake3.h"

int ava1_path_ok(const uint8_t *p, size_t n) {
    size_t i, start = 0;
    if (n == 0 || n > AVA1_MAX_PATH || p[0] == '/' || !ava1_utf8_valid(p, n)) return 0;
    for (i = 0; i <= n; i++) {
        if (i < n && p[i] == 0) return 0;
        if (i == n || p[i] == '/') {
            size_t len = i - start;
            if (len == 0) return 0;
            if (len == 1 && p[start] == '.') return 0;
            if (len == 2 && p[start] == '.' && p[start + 1] == '.') return 0;
            start = i + 1;
        }
    }
    return 1;
}

static int grow(void **p, size_t elem, uint32_t *cap, uint32_t need) {
    uint32_t c = *cap ? *cap : 64;
    void *q;
    while (c < need) c *= 2;
    if (c == *cap) return 0;
    q = realloc(*p, (size_t)c * elem);
    if (!q) return -1;
    *p = q;
    *cap = c;
    return 0;
}

int ava1_mstore_reserve(ava1_mstore_t *m, uint32_t n) {
    if (n > AVA1_MAX_ENTRIES) return AVA1_E_PROTO;
    if (n > m->cap) {
        ava1_ment_t *q = realloc(m->e, (size_t)n * sizeof *m->e);
        if (!q) return AVA1_E_IO;
        m->e = q;
        m->cap = n;
    }
    return 0;
}

int ava1_mstore_add(ava1_mstore_t *m, const ava1_manifest_entry_t *w) {
    ava1_ment_t *e;
    if (w->file_id != m->n) return AVA1_E_PROTO;
    if (m->n >= AVA1_MAX_ENTRIES) return AVA1_E_PROTO;
    if (!ava1_path_ok(w->path, w->path_len)) return AVA1_E_BADPATH;
    if (w->kind != AVA1_ENTRY_FILE && w->kind != AVA1_ENTRY_DIR) return AVA1_E_PROTO;
    if (w->kind == AVA1_ENTRY_FILE && m->bytes + w->size < m->bytes) return AVA1_E_PROTO;
    if (grow((void **)&m->e, sizeof *m->e, &m->cap, m->n + 1) != 0) return AVA1_E_IO;
    if (w->has_root && grow((void **)&m->roots, 32, &m->roots_cap, m->nroots + 1) != 0) return AVA1_E_IO;
    while (m->arena_len + w->path_len + 1 > m->arena_cap) {
        size_t c = m->arena_cap ? m->arena_cap * 2 : 65536;
        char *a = realloc(m->arena, c);
        if (!a) return AVA1_E_IO;
        m->arena = a;
        m->arena_cap = c;
    }
    e = &m->e[m->n];
    memset(e, 0, sizeof *e);
    e->kind = w->kind;
    e->mode = w->mode;
    e->size = w->kind == AVA1_ENTRY_FILE ? w->size : 0;
    e->mtime = w->mtime;
    e->path_off = (uint32_t)m->arena_len;
    e->path_len = w->path_len;
    memcpy(m->arena + m->arena_len, w->path, w->path_len);
    m->arena[m->arena_len + w->path_len] = 0;
    m->arena_len += (size_t)w->path_len + 1;
    if (w->has_root) {
        memcpy(m->roots[m->nroots], w->root, 32);
        e->root_idx = ++m->nroots;
    }
    if (w->kind == AVA1_ENTRY_FILE) {
        m->files++;
        m->bytes += w->size;
    }
    m->n++;
    return 0;
}

int ava1_mstore_add_page(ava1_mstore_t *m, const ava1_manifest_page_t *p) {
    ava1_r_t it;
    ava1_manifest_entry_t w;
    int rc;
    ava1_r_init(&it, p->entries, p->entries_len);
    while ((rc = ava1_manifest_entry_next(&it, &w)) == 1) {
        rc = ava1_mstore_add(m, &w);
        if (rc != 0) return rc;
    }
    return rc;
}

const char *ava1_mstore_path(const ava1_mstore_t *m, uint32_t id) {
    return id < m->n ? m->arena + m->e[id].path_off : NULL;
}

const uint8_t *ava1_mstore_root(const ava1_mstore_t *m, uint32_t id) {
    return id < m->n && m->e[id].root_idx ? m->roots[m->e[id].root_idx - 1] : NULL;
}

static void to_wire(const ava1_mstore_t *m, uint32_t i, ava1_manifest_entry_t *w, int with_root) {
    const ava1_ment_t *e = &m->e[i];
    memset(w, 0, sizeof *w);
    w->file_id = i;
    w->kind = e->kind;
    w->mode = e->mode;
    w->size = e->size;
    w->mtime = e->mtime;
    w->path = (const uint8_t *)ava1_mstore_path(m, i);
    w->path_len = e->path_len;
    if (with_root && e->root_idx) {
        w->has_root = 1;
        memcpy(w->root, m->roots[e->root_idx - 1], 32);
    }
}

void ava1_mstore_hash(const ava1_mstore_t *m, uint8_t out[32]) {
    blake3_hasher h;
    uint8_t buf[AVA1_MAX_PATH + 64], len4[4];
    uint32_t i;
    blake3_hasher_init(&h);
    for (i = 0; i < m->n; i++) {
        ava1_manifest_entry_t w;
        ava1_w_t wr;
        to_wire(m, i, &w, 0);
        ava1_w_init(&wr, buf, sizeof buf);
        (void)ava1_manifest_entry_encode(&w, &wr);
        len4[0] = (uint8_t)wr.len;
        len4[1] = (uint8_t)(wr.len >> 8);
        len4[2] = (uint8_t)(wr.len >> 16);
        len4[3] = (uint8_t)(wr.len >> 24);
        blake3_hasher_update(&h, len4, 4);
        blake3_hasher_update(&h, buf, wr.len);
    }
    blake3_hasher_finalize(&h, out, 32);
}

int ava1_mstore_blob(const ava1_mstore_t *m, uint8_t **blob, size_t *len) {
    size_t cap = (size_t)m->n * 80 + m->arena_len + 64;
    uint32_t i;
    ava1_w_t w;
    *blob = malloc(cap);
    if (!*blob) return AVA1_E_IO;
    ava1_w_init(&w, *blob, cap);
    for (i = 0; i < m->n; i++) {
        ava1_manifest_entry_t e;
        to_wire(m, i, &e, 1);
        if (ava1_manifest_entry_append(&w, &e) != 0) break;
    }
    if (w.err) {
        free(*blob);
        *blob = NULL;
        return w.err;
    }
    *len = w.len;
    return 0;
}

int ava1_mstore_from_blob(ava1_mstore_t *m, const uint8_t *blob, size_t len) {
    ava1_r_t it;
    ava1_manifest_entry_t w;
    int rc;
    ava1_r_init(&it, blob, len);
    while ((rc = ava1_manifest_entry_next(&it, &w)) == 1) {
        rc = ava1_mstore_add(m, &w);
        if (rc != 0) return rc;
    }
    return rc;
}

int ava1_mstore_page(const ava1_mstore_t *m, const uint8_t job[16], uint32_t *next, uint8_t *out, size_t cap,
                     size_t *len) {
    ava1_manifest_page_t p;
    ava1_w_t blob, w;
    uint8_t *b = malloc(AVA1_PAGE_BYTES);
    uint32_t nx = *next;
    int rc;
    if (!b) return AVA1_E_IO;
    ava1_w_init(&blob, b, AVA1_PAGE_BYTES - 64);
    while (nx < m->n) {
        ava1_manifest_entry_t e;
        size_t before = blob.len;
        to_wire(m, nx, &e, 1);
        if (ava1_manifest_entry_append(&blob, &e) != 0) {
            if (before == 0) { /* one entry larger than a page cannot happen (paths <= 1 KiB) */
                free(b);
                return AVA1_E_SPACE;
            }
            blob.len = before;
            blob.err = 0;
            break;
        }
        nx++;
    }
    memset(&p, 0, sizeof p);
    memcpy(p.job_id, job, 16);
    p.entries = b;
    p.entries_len = (uint32_t)blob.len;
    ava1_w_init(&w, out, cap);
    rc = ava1_manifest_page_encode(&p, &w);
    *len = w.len;
    free(b);
    if (rc == 0) *next = nx; /* a page that did not fit `out` consumed nothing */
    return rc;
}

/* Path order: component-wise, i.e. '/' sorts before every other byte. */
static int path_cmp(const void *a, const void *b) {
    const unsigned char *x = *(const unsigned char *const *)a, *y = *(const unsigned char *const *)b;
    for (;; x++, y++) {
        unsigned cx = *x == '/' ? 1u : (*x ? *x + 1u : 0u), cy = *y == '/' ? 1u : (*y ? *y + 1u : 0u);
        if (cx != cy) return cx < cy ? -1 : 1;
        if (!*x) return 0;
    }
}

typedef struct {
    char **v;
    uint32_t n, cap;
} strv_t;

static int strv_push(strv_t *s, const char *p) {
    char *c = strdup(p);
    if (!c || grow((void **)&s->v, sizeof *s->v, &s->cap, s->n + 1) != 0) {
        free(c);
        return -1;
    }
    s->v[s->n++] = c;
    return 0;
}

static void strv_free(strv_t *s) {
    uint32_t i;
    for (i = 0; i < s->n; i++) free(s->v[i]);
    free(s->v);
    memset(s, 0, sizeof *s);
}

static void fill_entry(ava1_manifest_entry_t *w, uint32_t id, const char *rel, const struct stat *st) {
    memset(w, 0, sizeof *w);
    w->file_id = id;
    w->kind = S_ISDIR(st->st_mode) ? AVA1_ENTRY_DIR : AVA1_ENTRY_FILE;
    w->mode = (uint32_t)(st->st_mode & 07777);
    w->size = S_ISDIR(st->st_mode) ? 0 : (uint64_t)st->st_size;
    w->mtime = (uint64_t)st->st_mtime;
    w->path = (const uint8_t *)rel;
    w->path_len = (uint16_t)strlen(rel);
}

/* A discovered entry with the stat fields the manifest needs, so the walk stats each path
 * once. `path` stays the first member: path_cmp sorts the records directly. */
typedef struct {
    char *path;
    uint32_t mode;
    uint64_t size, mtime;
    int dir;
} frec_t;

static int frec_push(frec_t **v, uint32_t *n, uint32_t *cap, const char *p, const struct stat *st) {
    char *c = strdup(p);
    if (!c || grow((void **)v, sizeof **v, cap, *n + 1) != 0) {
        free(c);
        return -1;
    }
    (*v)[*n].path = c;
    (*v)[*n].mode = (uint32_t)(st->st_mode & 07777);
    (*v)[*n].dir = S_ISDIR(st->st_mode) ? 1 : 0;
    (*v)[*n].size = S_ISDIR(st->st_mode) ? 0 : (uint64_t)st->st_size;
    (*v)[*n].mtime = (uint64_t)st->st_mtime;
    (*n)++;
    return 0;
}

/* One implementation for both modes (ruling C1). The walk follows the path order Rust's
 * walk produces; the entries are sorted by `path_cmp` afterwards either way. */
int ava1_mstore_walk_deny(ava1_mstore_t *m, const char *root, unsigned flags, int (*deny)(const char *abs)) {
    strv_t dirs = { 0 };
    frec_t *found = NULL;
    uint32_t found_n = 0, found_cap = 0;
    char *abs = malloc(2 * (AVA1_MAX_PATH + 2) + 512);
    int rc = 0;
    uint32_t i;
    if (!abs || strv_push(&dirs, "") != 0) {
        free(abs);
        return AVA1_E_IO;
    }
    while (dirs.n && rc == 0) {
        char *rel = dirs.v[--dirs.n];
        DIR *d;
        struct dirent *de;
        snprintf(abs, 2 * (AVA1_MAX_PATH + 2) + 512, "%s%s%s", root, *rel ? "/" : "", rel);
        d = opendir(abs);
        if (!d) rc = AVA1_E_IO;
        while (d && rc == 0 && (de = readdir(d)) != NULL) {
            char child[AVA1_MAX_PATH + 2];
            struct stat st, lst;
            if (strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0) continue;
            if (snprintf(child, sizeof child, "%s%s%s", rel, *rel ? "/" : "", de->d_name) >= (int)sizeof child) {
                rc = AVA1_E_BADPATH;
                break;
            }
            snprintf(abs, 2 * (AVA1_MAX_PATH + 2) + 512, "%s/%s", root, child);
            /* A stat failure on a discovered entry is fatal in both modes: Rust's walk
             * fails on a dangling link, and a silent skip would produce a manifest that
             * does not match the source tree (ruling C1). */
            if (lstat(abs, &lst) != 0) {
                rc = AVA1_E_IO;
                break;
            }
            /* A link into the trust store is skipped whole (a directory link is not descended, a file
             * link is not listed). Judged on the canonical target by the hook. */
            if (S_ISLNK(lst.st_mode) && deny && deny(abs)) continue;
            /* One stat per file: only a symlink needs its target's stat too. */
            if (S_ISLNK(lst.st_mode)) {
                if (stat(abs, &st) != 0) {
                    rc = AVA1_E_IO;
                    break;
                }
            } else {
                st = lst;
            }
            /* Default mode skips directory symlinks (no loops); AVA1_WALK_FOLLOW pushes
             * them like directories and walks them. A cycle then ends in ELOOP (a fatal
             * stat) or a path-length error — an error, never a spin. */
            if (!(flags & AVA1_WALK_FOLLOW) && S_ISLNK(lst.st_mode) && S_ISDIR(st.st_mode)) continue;
            if (!S_ISDIR(st.st_mode) && !S_ISREG(st.st_mode)) continue;
            if (frec_push(&found, &found_n, &found_cap, child, &st) != 0 ||
                (S_ISDIR(st.st_mode) && strv_push(&dirs, child) != 0))
                rc = AVA1_E_IO;
        }
        if (d) closedir(d);
        free(rel);
    }
    /* qsort's base must not be NULL even for zero elements (an empty folder leaves it NULL: UB). */
    if (rc == 0 && found_n > 1) qsort(found, found_n, sizeof *found, path_cmp);
    for (i = 0; rc == 0 && i < found_n; i++) {
        ava1_manifest_entry_t w;
        memset(&w, 0, sizeof w);
        w.file_id = m->n;
        w.kind = found[i].dir ? AVA1_ENTRY_DIR : AVA1_ENTRY_FILE;
        w.mode = found[i].mode;
        w.size = found[i].size;
        w.mtime = found[i].mtime;
        w.path = (const uint8_t *)found[i].path;
        w.path_len = (uint16_t)strlen(found[i].path);
        rc = ava1_mstore_add(m, &w);
    }
    strv_free(&dirs);
    for (i = 0; i < found_n; i++) free(found[i].path);
    free(found);
    free(abs);
    return rc;
}

int ava1_mstore_walk_ex(ava1_mstore_t *m, const char *root, unsigned flags) {
    return ava1_mstore_walk_deny(m, root, flags, NULL);
}

int ava1_mstore_walk(ava1_mstore_t *m, const char *root) { return ava1_mstore_walk_ex(m, root, 0); }

int ava1_open_read_safe(const char *path, int (*deny)(const char *abs)) {
    char res[AVA1_MAX_PATH + 600];
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd >= 0) return fd;
    if (errno != ELOOP && errno != EMLINK) return -errno; /* FreeBSD says EMLINK for O_NOFOLLOW on a link */
    if (!realpath(path, res)) return -errno;
    if (deny && deny(res)) return -EACCES;
    fd = open(res, O_RDONLY | O_NOFOLLOW);
    return fd >= 0 ? fd : -errno;
}

int ava1_mstore_single(ava1_mstore_t *m, const char *file) {
    struct stat st;
    ava1_manifest_entry_t w;
    const char *name = strrchr(file, '/');
    if (stat(file, &st) != 0) return AVA1_E_IO;
    if (!S_ISREG(st.st_mode)) return AVA1_E_BADPATH;
    fill_entry(&w, m->n, name ? name + 1 : file, &st);
    return ava1_mstore_add(m, &w);
}

void ava1_mstore_free(ava1_mstore_t *m) {
    free(m->e);
    free(m->roots);
    free(m->arena);
    memset(m, 0, sizeof *m);
}
