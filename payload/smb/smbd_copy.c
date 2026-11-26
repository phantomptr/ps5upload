/* See smbd_copy.h. */
#include "smbd_copy.h"

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "smbd_path.h"

typedef struct {
    xfs_t *src;
    xfs_t *dst;
    const copy_opts_t *o;
    copy_progress_t *pr;
    char *failed;
    size_t failed_cap;
    uint8_t *buf;
    size_t chunk;
} ctx_t;

static void nap(const copy_opts_t *o, unsigned s) {
    if (o && o->sleep_s) o->sleep_s(s);
    else sleep(s);
}

static void note_failed(ctx_t *c, const char *path) {
    size_t n;
    if (!c->failed || !c->failed_cap || c->failed[0]) return;
    n = strlen(path);
    if (n >= c->failed_cap) n = c->failed_cap - 1;
    memcpy(c->failed, path, n);
    c->failed[n] = '\0';
}

static void set_current(copy_progress_t *pr, const char *path) {
    const char *b = smbp_base(path);
    size_t n = strlen(b);
    if (n >= sizeof pr->current) n = sizeof pr->current - 1;
    memcpy(pr->current, b, n);
    pr->current[n] = '\0';
}

/* A failure worth a reconnect: the connection, not the request. */
static int transient(int e) { return e == XE_DISCONNECTED || e == XE_TIMEOUT || e == XE_IO; }

/* After a transient failure on `fs`: reconnect it with backoff. 0 = connected again (try again),
 * else the error to give up with. `tries` counts the attempts already made. */
static int recover(ctx_t *c, xfs_t *fs, int err, int *tries) {
    if (!transient(err) || !fs->ops->reconnect) return err;
    while (*tries < COPY_RETRIES) {
        int rc;
        unsigned wait = 1u << *tries; /* 1, 2, 4 s */
        (*tries)++;
        nap(c->o, wait);
        if (c->pr->cancel) return XE_CANCELLED;
        rc = fs->ops->reconnect(fs);
        if (rc == 0) {
            c->pr->reconnects++;
            return 0;
        }
        if (!transient(rc) && rc != XE_UNREACHABLE) return rc;
    }
    return err;
}

/* ---- counting ---- */

static int count_tree(ctx_t *c, const char *path, int depth) {
    void *d = NULL;
    char name[SMBP_NAME_MAX], child[SMBP_PATH_MAX];
    xstat_t st;
    int rc, tries = 0;
    if (depth > COPY_MAX_DEPTH) return XE_TOO_DEEP;
    for (;;) {
        rc = c->src->ops->opendir(c->src, path, &d);
        if (rc == 0) break;
        if (recover(c, c->src, rc, &tries) != 0) {
            note_failed(c, path);
            return rc;
        }
    }
    for (;;) {
        if (c->pr->cancel) {
            rc = XE_CANCELLED;
            break;
        }
        rc = c->src->ops->readdir(c->src, d, name, sizeof name, &st);
        if (rc <= 0) break;
        if (st.kind == XK_OTHER || (c->o->skip_mac_metadata && smbp_skip_download(name))) {
            c->pr->skipped++; /* counted once, here: the copy pass skips the same entries */
            continue;
        }
        if (smbp_join(path, name, child, sizeof child) != 0) {
            rc = XE_BAD_NAME;
            note_failed(c, name);
            break;
        }
        if (st.kind == XK_DIR) {
            /* a sub-folder: the handle stays open while it is counted (depth-bounded) */
            rc = count_tree(c, child, depth + 1);
            if (rc != 0) break;
        } else {
            c->pr->files_total++;
            c->pr->bytes_total += st.size;
        }
    }
    c->src->ops->closedir(c->src, d);
    if (rc < 0) note_failed(c, path);
    return rc < 0 ? rc : 0;
}

/* ---- one file ---- */

static void drop_part(ctx_t *c, const char *part) {
    int rc = c->dst->ops->unlink(c->dst, part);
    /* The server may have dropped just now: one reconnect, then give up (best-effort clean-up). */
    if (transient(rc) && c->dst->ops->reconnect && c->dst->ops->reconnect(c->dst) == 0)
        (void)c->dst->ops->unlink(c->dst, part);
}

static int copy_file(ctx_t *c, const char *sp, const char *dp) {
    char part[SMBP_PATH_MAX];
    void *in = NULL, *out = NULL;
    xstat_t ds;
    uint64_t off = 0;
    int rc, existed = 0, stries = 0, dtries = 0;

    set_current(c->pr, sp);
    rc = c->dst->ops->stat(c->dst, dp, &ds);
    if (rc == 0) {
        if (ds.kind == XK_DIR) return XE_IS_DIR;
        if (!c->o->overwrite) return XE_EXISTS;
        existed = 1;
    } else if (rc != XE_NOT_FOUND) {
        return rc;
    }
    if (smbp_part_name(dp, part, sizeof part) != 0) return XE_BAD_NAME;

    for (;;) {
        rc = c->src->ops->open_read(c->src, sp, &in);
        if (rc == 0) break;
        if ((rc = recover(c, c->src, rc, &stries)) != 0) return rc;
    }
    for (;;) {
        rc = c->dst->ops->open_write(c->dst, part, 0, &out);
        if (rc == 0) break;
        if ((rc = recover(c, c->dst, rc, &dtries)) != 0) {
            c->src->ops->close(c->src, in);
            return rc;
        }
    }

    for (;;) {
        long long got, put = 0;
        if (c->pr->cancel) {
            rc = XE_CANCELLED;
            goto fail;
        }
        got = c->src->ops->pread(c->src, in, c->buf, c->chunk, off);
        if (got < 0) {
            rc = (int)got;
            c->src->ops->close(c->src, in);
            in = NULL;
            if ((rc = recover(c, c->src, rc, &stries)) != 0) goto fail;
            /* A reconnect of the source leaves the destination handle alone (another side). */
            if ((rc = c->src->ops->open_read(c->src, sp, &in)) != 0) {
                in = NULL;
                goto fail;
            }
            continue;
        }
        if (got == 0) break;
        while (put < got) {
            long long w;
            if (c->pr->cancel) {
                rc = XE_CANCELLED;
                goto fail;
            }
            w = c->dst->ops->pwrite(c->dst, out, c->buf + put, (size_t)(got - put), off + (uint64_t)put);
            if (w < 0) {
                rc = (int)w;
                c->dst->ops->close(c->dst, out);
                out = NULL;
                if ((rc = recover(c, c->dst, rc, &dtries)) != 0) goto fail;
                if ((rc = c->dst->ops->open_write(c->dst, part, 1, &out)) != 0) {
                    out = NULL;
                    goto fail;
                }
                continue;
            }
            if (w == 0) {
                rc = XE_IO;
                goto fail;
            }
            put += w;
        }
        off += (uint64_t)got;
        c->pr->bytes_done += (uint64_t)got;
    }

    c->src->ops->close(c->src, in);
    in = NULL;
    rc = c->dst->ops->close(c->dst, out);
    out = NULL;
    if (rc != 0) goto fail;
    rc = c->dst->ops->rename(c->dst, part, dp, existed);
    if (rc != 0) goto fail;
    c->pr->files_done++;
    return 0;

fail:
    if (in) c->src->ops->close(c->src, in);
    if (out) c->dst->ops->close(c->dst, out);
    drop_part(c, part);
    return rc;
}

/* ---- a folder ---- */

typedef struct {
    char *name;
    xkind_t kind;
} entry_t;

static void free_entries(entry_t *v, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) free(v[i].name);
    free(v);
}

/* The folder's entries (links and other specials left out), read in full before any is copied so
 * no directory handle stays open across the copy. */
static int read_entries(ctx_t *c, const char *path, entry_t **out, size_t *count) {
    entry_t *v = NULL;
    size_t n = 0, cap = 0;
    void *d = NULL;
    char name[SMBP_NAME_MAX];
    xstat_t st;
    int rc, tries = 0;
    *out = NULL;
    *count = 0;
    for (;;) {
        rc = c->src->ops->opendir(c->src, path, &d);
        if (rc == 0) break;
        if ((rc = recover(c, c->src, rc, &tries)) != 0) return rc;
    }
    for (;;) {
        rc = c->src->ops->readdir(c->src, d, name, sizeof name, &st);
        if (rc <= 0) break;
        if (st.kind == XK_OTHER) continue;
        if (c->o->skip_mac_metadata && smbp_skip_download(name)) continue;
        if (n == cap) {
            size_t nc = cap ? cap * 2 : 64;
            entry_t *nv;
            if (nc > COPY_DIR_ENTRIES_MAX) {
                rc = XE_NOMEM;
                break;
            }
            nv = realloc(v, nc * sizeof *nv);
            if (!nv) {
                rc = XE_NOMEM;
                break;
            }
            v = nv;
            cap = nc;
        }
        v[n].name = strdup(name);
        if (!v[n].name) {
            rc = XE_NOMEM;
            break;
        }
        v[n].kind = st.kind;
        n++;
    }
    c->src->ops->closedir(c->src, d);
    if (rc < 0) {
        free_entries(v, n);
        return rc;
    }
    *out = v;
    *count = n;
    return 0;
}

static int copy_dir(ctx_t *c, const char *sp, const char *dp, int depth) {
    entry_t *v = NULL;
    size_t n = 0, i;
    xstat_t ds;
    int rc, tries = 0;
    if (depth > COPY_MAX_DEPTH) return XE_TOO_DEEP;
    rc = c->dst->ops->stat(c->dst, dp, &ds);
    if (rc == 0) {
        if (ds.kind != XK_DIR) {
            note_failed(c, dp);
            return XE_NOT_DIR;
        }
        if (depth == 0 && !c->o->overwrite) return XE_EXISTS;
    } else if (rc == XE_NOT_FOUND) {
        for (;;) {
            rc = c->dst->ops->mkdir(c->dst, dp);
            if (rc == 0) break;
            if ((rc = recover(c, c->dst, rc, &tries)) != 0) {
                note_failed(c, dp);
                return rc;
            }
        }
    } else {
        note_failed(c, dp);
        return rc;
    }
    rc = read_entries(c, sp, &v, &n);
    if (rc != 0) {
        note_failed(c, sp);
        return rc;
    }
    for (i = 0; i < n && rc == 0; i++) {
        char s2[SMBP_PATH_MAX], d2[SMBP_PATH_MAX];
        if (c->pr->cancel) {
            rc = XE_CANCELLED;
            break;
        }
        if (smbp_join(sp, v[i].name, s2, sizeof s2) != 0 || smbp_join(dp, v[i].name, d2, sizeof d2) != 0) {
            note_failed(c, v[i].name);
            rc = XE_BAD_NAME;
            break;
        }
        if (v[i].kind == XK_DIR) {
            rc = copy_dir(c, s2, d2, depth + 1);
        } else {
            rc = copy_file(c, s2, d2);
            if (rc != 0) note_failed(c, s2);
        }
    }
    free_entries(v, n);
    return rc;
}

int copy_run(xfs_t *src, const char *src_path, xfs_t *dst, const char *dst_path, const copy_opts_t *opts,
             copy_progress_t *pr, char *failed, size_t failed_cap) {
    ctx_t c;
    xstat_t st;
    char parent[SMBP_PATH_MAX];
    uint64_t free_b = 0;
    size_t sc, dc;
    int rc;
    memset(&c, 0, sizeof c);
    c.src = src;
    c.dst = dst;
    c.o = opts;
    c.pr = pr;
    c.failed = failed;
    c.failed_cap = failed_cap;
    if (failed && failed_cap) failed[0] = '\0';

    rc = src->ops->stat(src, src_path, &st);
    if (rc != 0) {
        note_failed(&c, src_path);
        return rc;
    }
    if (st.kind == XK_OTHER) return XE_BAD_REQUEST;

    pr->scanning = 1;
    if (st.kind == XK_DIR) {
        rc = count_tree(&c, src_path, 0);
    } else {
        pr->files_total = 1;
        pr->bytes_total = st.size;
    }
    pr->scanning = 0;
    if (rc != 0) return rc;
    if (pr->cancel) return XE_CANCELLED;

    smbp_parent(dst_path, parent, sizeof parent);
    if (dst->ops->free_space && dst->ops->free_space(dst, parent, &free_b) == 0 &&
        free_b < pr->bytes_total + opts->space_margin)
        return XE_NO_SPACE;

    sc = src->ops->chunk ? src->ops->chunk(src) : 0;
    dc = dst->ops->chunk ? dst->ops->chunk(dst) : 0;
    c.chunk = COPY_CHUNK_MAX;
    if (sc && sc < c.chunk) c.chunk = sc;
    if (dc && dc < c.chunk) c.chunk = dc;
    c.buf = malloc(c.chunk);
    if (!c.buf) return XE_NOMEM;

    if (st.kind == XK_DIR) {
        rc = copy_dir(&c, src_path, dst_path, 0);
    } else {
        rc = copy_file(&c, src_path, dst_path);
        if (rc != 0) note_failed(&c, src_path);
    }
    free(c.buf);
    return rc;
}

/* ---- removal ---- */

static int remove_rec(xfs_t *fs, const char *path, copy_progress_t *pr, int depth) {
    xstat_t st;
    void *d = NULL;
    char name[SMBP_NAME_MAX];
    int rc;
    if (depth > COPY_MAX_DEPTH) return XE_TOO_DEEP;
    if (pr && pr->cancel) return XE_CANCELLED;
    rc = fs->ops->stat(fs, path, &st);
    if (rc != 0) return rc;
    if (st.kind != XK_DIR) return fs->ops->unlink(fs, path);
    /* Delete children, re-opening the folder after each batch: a listing is never iterated while
     * entries are removed from it. */
    for (;;) {
        char (*names)[SMBP_NAME_MAX];
        size_t n = 0, i;
        names = malloc(64 * sizeof *names);
        if (!names) return XE_NOMEM;
        rc = fs->ops->opendir(fs, path, &d);
        if (rc != 0) {
            free(names);
            return rc;
        }
        while (n < 64 && (rc = fs->ops->readdir(fs, d, name, sizeof name, &st)) > 0) memcpy(names[n++], name, sizeof name);
        fs->ops->closedir(fs, d);
        if (rc < 0) {
            free(names);
            return rc;
        }
        if (n == 0) {
            free(names);
            break;
        }
        {
            size_t removed = 0;
            for (i = 0; i < n; i++) {
                char child[SMBP_PATH_MAX];
                if (smbp_join(path, names[i], child, sizeof child) != 0) {
                    free(names);
                    return XE_BAD_NAME;
                }
                rc = remove_rec(fs, child, pr, depth + 1);
                if (rc == 0) removed++;
                else if (rc != XE_NOT_FOUND) {
                    free(names);
                    return rc;
                }
            }
            free(names);
            /* The listing keeps naming entries that stat / unlink say are not there (a server
             * whose listing and namespace disagree): a pass that removed nothing would repeat
             * forever, so it is a failure. */
            if (removed == 0) return XE_IO;
        }
    }
    return fs->ops->rmdir(fs, path);
}

int copy_remove_tree(xfs_t *fs, const char *path, copy_progress_t *pr) { return remove_rec(fs, path, pr, 0); }
