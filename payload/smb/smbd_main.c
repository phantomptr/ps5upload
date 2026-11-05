/* PS5Upload SMB daemon (ps5upload-smb.elf): network-share access for the console's own Files page.
 * See include/smbd_proto.h for why it is a process of its own and how the helper talks to it.
 *
 * Ops (every request: {"key":..,"op":..}; a share connection is host, port, share, user, password,
 * domain; paths on a share are relative to it, "" = its top):
 *   ping                     -> {"ok":true,"version","elf","pid","running"}
 *   quit                     -> {"ok":true}; running jobs are cancelled and the process exits
 *   shares  (no share)       -> {"shares":[{"name","remark"}]}
 *   probe   path             -> {"ok":true,"dialect","free"}  (sign in, the folder must be there)
 *   list    path offset limit-> {"entries":[{"name","kind","size","mtime"}],"more","next","total"}
 *   stat    path             -> {"kind","size","mtime"}
 *   mkdir   path             -> {"ok":true}
 *   rename  path to          -> {"ok":true}  (inside one folder, never over something: a Replace's swap)
 *   copy    dir src dest overwrite -> {"job"}  dir "down": share src -> console dest; "up": back
 *   remove  path             -> {"job"}  (a file or folder tree on the share)
 *   scan    source (path | local:true) -> {"job"}  a game library scan (smbd_library.h) of the folder
 *                            on the share, or of the console's drives; for its job, files_done counts
 *                            entries looked at and files_total games found
 *   job     job              -> the job's status;  cancel job -> {"ok":true};  jobs -> {"jobs":[..]}
 * Errors: {"error":"<token>","detail":".."} (smbd_path.h smbp_err_token). */
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "config.h"
#include "smbd_fs.h"
#include "smbd_jobs.h"
#include "smbd_library.h"
#include "smbd_json.h"
#include "smbd_log.h"
#include "smbd_path.h"
#include "smbd_proto.h"

#define CONN_STACK (256u * 1024u)
#define CONNS_MAX 8
#define REQ_TIMEOUT_S 10
/* Interactive requests (list, stat, probe…) answer within the helper's wait. */
#define INTERACTIVE_TIMEOUT_S 8
#define LIST_ENTRIES_MAX 100000u
#define LIST_CACHE_S 120

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static char g_key[SMBD_KEY_HEX + 1];
static char g_elf_id[64];
static int g_conns;
static volatile time_t g_last_request;
static volatile int g_quit;

/* ---- log ---- */

static pthread_mutex_t g_log_mu = PTHREAD_MUTEX_INITIALIZER;

void smbd_log(const char *fmt, ...) {
    char line[768];
    va_list ap;
    struct stat st;
    FILE *f;
    int k;
    time_t now = time(NULL);
    k = snprintf(line, sizeof line, "%lld ", (long long)now);
    va_start(ap, fmt);
    vsnprintf(line + k, sizeof line - (size_t)k, fmt, ap);
    va_end(ap);
    pthread_mutex_lock(&g_log_mu);
    if (stat(SMBD_LOG_PATH, &st) == 0 && st.st_size > 256 * 1024) rename(SMBD_LOG_PATH, SMBD_LOG_PATH ".old");
    f = fopen(SMBD_LOG_PATH, "a");
    if (f) {
        fprintf(f, "%s\n", line);
        fclose(f);
    }
    pthread_mutex_unlock(&g_log_mu);
}

void smbd_name_thread(void) {
#ifdef SYS_thr_set_name
    (void)syscall(SYS_thr_set_name, -1, "ps5u-smbd");
#endif
}

/* ---- the key ---- */

static int read_key(char key[SMBD_KEY_HEX + 1], char *elf_id, size_t elf_cap) {
    char buf[256];
    FILE *f = fopen(SMBD_KEY_PATH, "r");
    size_t n;
    char *nl;
    if (!f) return -1;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = '\0';
    if (n < SMBD_KEY_HEX) return -1;
    memcpy(key, buf, SMBD_KEY_HEX);
    key[SMBD_KEY_HEX] = '\0';
    elf_id[0] = '\0';
    nl = strchr(buf, '\n');
    if (nl) {
        char *e = strchr(nl + 1, '\n');
        size_t l = e ? (size_t)(e - nl - 1) : strlen(nl + 1);
        if (l >= elf_cap) l = elf_cap - 1;
        memcpy(elf_id, nl + 1, l);
        elf_id[l] = '\0';
    }
    memset(buf, 0, sizeof buf);
    return 0;
}

static int same(const char *a, const char *b, size_t n) {
    unsigned char d = 0;
    size_t i;
    for (i = 0; i < n; i++) d |= (unsigned char)(a[i] ^ b[i]);
    return d == 0;
}

/* The request's key matches; a mismatch re-reads the file once (the helper may have made a new key). */
static int key_ok(const char *got) {
    int ok;
    if (strlen(got) != SMBD_KEY_HEX) return 0;
    pthread_mutex_lock(&g_mu);
    ok = g_key[0] && same(got, g_key, SMBD_KEY_HEX);
    if (!ok) {
        char k[SMBD_KEY_HEX + 1], id[64];
        if (read_key(k, id, sizeof id) == 0 && !same(k, g_key, SMBD_KEY_HEX)) {
            memcpy(g_key, k, sizeof g_key);
            ok = same(got, g_key, SMBD_KEY_HEX);
        }
        memset(k, 0, sizeof k);
    }
    pthread_mutex_unlock(&g_mu);
    return ok;
}

/* ---- replies ---- */

static void reply_err(sj_buf_t *b, int e, const char *detail) {
    sj_raw(b, "{");
    sj_kv_str(b, 0, "error", smbp_err_token(e));
    if (detail && detail[0]) sj_kv_str(b, 1, "detail", detail);
    sj_raw(b, "}");
}

static const char *kind_str(xkind_t k) { return k == XK_DIR ? "dir" : k == XK_FILE ? "file" : "other"; }

/* ---- request fields ---- */

static int conn_from(const char *r, size_t n, smbconn_t *c, int need_share) {
    uint64_t port = 445;
    memset(c, 0, sizeof *c);
    if (sj_str(r, n, "host", c->host, sizeof c->host) != 1 || !smbp_host_ok(c->host)) return XE_BAD_REQUEST;
    if (sj_u64(r, n, "port", &port) < 0 || port == 0 || port > 65535) return XE_BAD_REQUEST;
    c->port = (uint16_t)port;
    if (need_share && (sj_str(r, n, "share", c->share, sizeof c->share) != 1 || !smbp_share_ok(c->share)))
        return XE_BAD_REQUEST;
    if (sj_str(r, n, "user", c->user, sizeof c->user) < 0) return XE_BAD_REQUEST;
    if (sj_str(r, n, "password", c->password, sizeof c->password) < 0) return XE_BAD_REQUEST;
    if (sj_str(r, n, "domain", c->domain, sizeof c->domain) < 0) return XE_BAD_REQUEST;
    c->timeout_s = INTERACTIVE_TIMEOUT_S;
    return 0;
}

static int rel_from(const char *r, size_t n, const char *key, char *out, size_t cap) {
    char raw[SMBP_PATH_MAX];
    int rc = sj_str(r, n, key, raw, sizeof raw);
    if (rc < 0) return XE_BAD_REQUEST;
    if (rc == 0) raw[0] = '\0';
    return smbp_rel_normalize(raw, out, cap) == 0 ? 0 : XE_BAD_NAME;
}

static int local_from(const char *r, size_t n, const char *key, char *out, size_t cap) {
    if (sj_str(r, n, key, out, cap) != 1 || !smbp_local_ok(out)) return XE_BAD_REQUEST;
    return 0;
}

/* ---- list (with a one-folder cache, so paging a big folder does not re-read it per page) ---- */

typedef struct {
    char *name;
    xkind_t kind;
    uint64_t size;
    uint64_t mtime;
} lent_t;

static pthread_mutex_t g_cache_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
    char key[SMBP_HOST_MAX + SMBP_SHARE_MAX + 2 * SMBP_PATH_MAX];
    lent_t *v;
    size_t n;
    int truncated; /* the folder has more entries than LIST_ENTRIES_MAX; the listing shows the first */
    time_t at;
} g_cache;

static void free_lents(lent_t *v, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) free(v[i].name);
    free(v);
}

static int cmp_lent(const void *a, const void *b) {
    return strcmp(((const lent_t *)a)->name, ((const lent_t *)b)->name);
}

static int read_listing(xfs_t *fs, const char *path, lent_t **out, size_t *count, int *truncated) {
    void *d = NULL;
    lent_t *v = NULL;
    size_t n = 0, cap = 0;
    char name[SMBP_NAME_MAX];
    xstat_t st;
    int rc = fs->ops->opendir(fs, path, &d);
    *truncated = 0;
    if (rc != 0) return rc;
    while ((rc = fs->ops->readdir(fs, d, name, sizeof name, &st)) > 0) {
        if (n == cap) {
            size_t nc = cap ? cap * 2 : 128;
            lent_t *nv;
            /* The cap is LIST_ENTRIES_MAX exactly (doubling past it used to stop at 65,536 with
             * no word about it); the rest is not shown and the listing says so. */
            if (cap >= LIST_ENTRIES_MAX) {
                *truncated = 1;
                break;
            }
            if (nc > LIST_ENTRIES_MAX) nc = LIST_ENTRIES_MAX;
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
        v[n].size = st.size;
        v[n].mtime = st.mtime;
        n++;
    }
    fs->ops->closedir(fs, d);
    if (rc < 0) {
        free_lents(v, n);
        return rc;
    }
    if (n > 1) qsort(v, n, sizeof *v, cmp_lent);
    *out = v;
    *count = n;
    return 0;
}

static void op_list(const char *r, size_t n, sj_buf_t *b) {
    smbconn_t c;
    char path[SMBP_PATH_MAX], key[sizeof g_cache.key], detail[160];
    uint64_t offset = 0, limit = SMBD_LIST_MAX;
    lent_t *v = NULL;
    size_t cnt = 0, i, end;
    int rc, truncated = 0;
    if ((rc = conn_from(r, n, &c, 1)) != 0 || (rc = rel_from(r, n, "path", path, sizeof path)) != 0) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    if (sj_u64(r, n, "offset", &offset) < 0 || sj_u64(r, n, "limit", &limit) < 0) {
        smbconn_clear(&c);
        reply_err(b, XE_BAD_REQUEST, NULL);
        return;
    }
    if (limit == 0 || limit > SMBD_LIST_MAX) limit = SMBD_LIST_MAX;
    snprintf(key, sizeof key, "%s|%u|%s|%s|%s|%s", c.host, (unsigned)c.port, c.share, c.user, c.domain, path);
    pthread_mutex_lock(&g_cache_mu);
    if (offset > 0 && g_cache.v && strcmp(g_cache.key, key) == 0 && time(NULL) - g_cache.at < LIST_CACHE_S) {
        /* a later page: answered from the folder read for the first one */
        cnt = g_cache.n;
        v = g_cache.v;
        truncated = g_cache.truncated;
        g_cache.v = NULL;
        g_cache.n = 0;
    }
    pthread_mutex_unlock(&g_cache_mu);
    if (!v) {
        xfs_t fs;
        rc = smbfs_open(&fs, &c, detail, sizeof detail);
        if (rc != 0) {
            smbconn_clear(&c);
            reply_err(b, rc, detail);
            return;
        }
        rc = read_listing(&fs, path, &v, &cnt, &truncated);
        smbfs_close(&fs);
        if (rc != 0) {
            smbconn_clear(&c);
            reply_err(b, rc, NULL);
            return;
        }
    }
    smbconn_clear(&c);
    end = offset + limit < cnt ? (size_t)(offset + limit) : cnt;
    sj_raw(b, "{\"entries\":[");
    for (i = (size_t)offset; i < end; i++) {
        if (i > offset) sj_raw(b, ",");
        sj_raw(b, "{");
        sj_kv_str(b, 0, "name", v[i].name);
        sj_kv_str(b, 1, "kind", kind_str(v[i].kind));
        sj_fmt(b, ",\"size\":%llu,\"mtime\":%llu}", (unsigned long long)v[i].size, (unsigned long long)v[i].mtime);
    }
    sj_fmt(b, "],\"more\":%s,\"next\":%llu,\"total\":%llu%s}", end < cnt ? "true" : "false", (unsigned long long)end,
           (unsigned long long)cnt, truncated ? ",\"truncated\":true" : "");
    /* keep it for the next page */
    pthread_mutex_lock(&g_cache_mu);
    free_lents(g_cache.v, g_cache.n);
    g_cache.v = v;
    g_cache.n = cnt;
    g_cache.truncated = truncated;
    g_cache.at = time(NULL);
    snprintf(g_cache.key, sizeof g_cache.key, "%s", key);
    pthread_mutex_unlock(&g_cache_mu);
}

static void cache_drop(void) {
    pthread_mutex_lock(&g_cache_mu);
    free_lents(g_cache.v, g_cache.n);
    g_cache.v = NULL;
    g_cache.n = 0;
    g_cache.key[0] = '\0';
    pthread_mutex_unlock(&g_cache_mu);
}

/* ---- the other ops ---- */

static void op_shares(const char *r, size_t n, sj_buf_t *b) {
    smbconn_t c;
    smb_share_info_t *s;
    char detail[160];
    int rc, i;
    if ((rc = conn_from(r, n, &c, 0)) != 0) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    s = calloc(128, sizeof *s);
    if (!s) {
        smbconn_clear(&c);
        reply_err(b, XE_NOMEM, NULL);
        return;
    }
    rc = smbfs_list_shares(&c, s, 128, detail, sizeof detail);
    smbconn_clear(&c);
    if (rc < 0) {
        reply_err(b, rc, detail);
    } else {
        sj_raw(b, "{\"shares\":[");
        for (i = 0; i < rc; i++) {
            if (i) sj_raw(b, ",");
            sj_raw(b, "{");
            sj_kv_str(b, 0, "name", s[i].name);
            sj_kv_str(b, 1, "remark", s[i].remark);
            sj_raw(b, "}");
        }
        sj_raw(b, "]}");
    }
    free(s);
}

static void dialect_str(unsigned d, char out[8]) {
    snprintf(out, 8, "%x.%x.%x", (d >> 8) & 0xf, (d >> 4) & 0xf, d & 0xf);
}

/* probe / stat / mkdir: one connection, one call. */
static void op_simple(const char *op, const char *r, size_t n, sj_buf_t *b) {
    smbconn_t c;
    char path[SMBP_PATH_MAX], detail[160];
    xfs_t fs;
    xstat_t st;
    int rc;
    if ((rc = conn_from(r, n, &c, 1)) != 0 || (rc = rel_from(r, n, "path", path, sizeof path)) != 0) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    rc = smbfs_open(&fs, &c, detail, sizeof detail);
    smbconn_clear(&c);
    if (rc != 0) {
        reply_err(b, rc, detail);
        return;
    }
    if (strcmp(op, "probe") == 0) {
        uint64_t free_b = 0;
        rc = fs.ops->stat(&fs, path, &st);
        if (rc == 0 && st.kind != XK_DIR) rc = XE_NOT_DIR;
        if (rc == 0) {
            char d[8];
            int have_free = fs.ops->free_space(&fs, path, &free_b) == 0;
            dialect_str(smbfs_dialect(&fs), d);
            sj_raw(b, "{\"ok\":true");
            sj_kv_str(b, 1, "dialect", d);
            if (have_free) sj_fmt(b, ",\"free\":%llu}", (unsigned long long)free_b);
            else sj_raw(b, ",\"free\":null}");
        }
    } else if (strcmp(op, "stat") == 0) {
        rc = fs.ops->stat(&fs, path, &st);
        if (rc == 0)
            sj_fmt(b, "{\"kind\":\"%s\",\"size\":%llu,\"mtime\":%llu}", kind_str(st.kind), (unsigned long long)st.size,
                   (unsigned long long)st.mtime);
    } else { /* mkdir: a name we create, so the strict (Windows-safe) rule for its last component */
        const char *leaf = strrchr(path, '/');
        leaf = leaf ? leaf + 1 : path;
        rc = !path[0] ? XE_BAD_REQUEST : !smbp_name_ok(leaf) ? XE_BAD_NAME : fs.ops->mkdir(&fs, path);
        if (rc == 0) {
            cache_drop();
            sj_raw(b, "{\"ok\":true}");
        }
    }
    smbfs_close(&fs);
    if (rc != 0) reply_err(b, rc, NULL);
}

/* The page's Replace on a share: the old item moves aside and the finished copy takes its name, so a
 * failure on the way never leaves the share with neither (client/src/console/files/replace.ts). One
 * folder only, and a target that is taken is refused: the server's rename never overwrites here
 * (s_rename). The new name is one we create: the strict (Windows-safe) rule, as for an upload. */
static void op_rename(const char *r, size_t n, sj_buf_t *b) {
    smbconn_t c;
    char from[SMBP_PATH_MAX], to[SMBP_PATH_MAX], pf[SMBP_PATH_MAX], pt[SMBP_PATH_MAX], detail[160];
    const char *leaf;
    xfs_t fs;
    int rc;
    if ((rc = conn_from(r, n, &c, 1)) != 0 || (rc = rel_from(r, n, "path", from, sizeof from)) != 0 ||
        (rc = rel_from(r, n, "to", to, sizeof to)) != 0) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    smbp_parent(from, pf, sizeof pf);
    smbp_parent(to, pt, sizeof pt);
    leaf = strrchr(to, '/');
    leaf = leaf ? leaf + 1 : to;
    rc = !from[0] || !to[0] || strcmp(pf, pt) != 0 ? XE_BAD_REQUEST : !smbp_name_ok(leaf) ? XE_BAD_NAME : 0;
    if (rc != 0) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    rc = smbfs_open(&fs, &c, detail, sizeof detail);
    smbconn_clear(&c);
    if (rc != 0) {
        reply_err(b, rc, detail);
        return;
    }
    rc = fs.ops->rename(&fs, from, to, 0);
    smbfs_close(&fs);
    if (rc != 0) {
        reply_err(b, rc, NULL);
        return;
    }
    cache_drop();
    sj_raw(b, "{\"ok\":true}");
}

static void op_copy(const char *r, size_t n, sj_buf_t *b, int remove) {
    smbconn_t c;
    char dir[8], src[SMBP_PATH_MAX], dst[SMBP_PATH_MAX], id[33];
    int rc, overwrite = 0;
    job_kind_t kind;
    if ((rc = conn_from(r, n, &c, 1)) != 0) goto bad;
    if (remove) {
        kind = JOB_REMOVE;
        if ((rc = rel_from(r, n, "path", src, sizeof src)) != 0) goto bad;
        if (!src[0]) {
            rc = XE_BAD_REQUEST; /* never the whole share */
            goto bad;
        }
        dst[0] = '\0';
    } else {
        if (sj_str(r, n, "dir", dir, sizeof dir) != 1) {
            rc = XE_BAD_REQUEST;
            goto bad;
        }
        if (sj_bool(r, n, "overwrite", &overwrite) < 0) {
            rc = XE_BAD_REQUEST;
            goto bad;
        }
        if (strcmp(dir, "down") == 0) {
            kind = JOB_DOWN;
            if ((rc = rel_from(r, n, "src", src, sizeof src)) != 0 || (rc = local_from(r, n, "dest", dst, sizeof dst)) != 0)
                goto bad;
        } else if (strcmp(dir, "up") == 0) {
            kind = JOB_UP;
            if ((rc = local_from(r, n, "src", src, sizeof src)) != 0 || (rc = rel_from(r, n, "dest", dst, sizeof dst)) != 0)
                goto bad;
            if (!dst[0]) {
                rc = XE_BAD_REQUEST;
                goto bad;
            }
            { /* the upload's target is a name we create: the strict (Windows-safe) rule */
                const char *leaf = strrchr(dst, '/');
                if (!smbp_name_ok(leaf ? leaf + 1 : dst)) {
                    rc = XE_BAD_NAME;
                    goto bad;
                }
            }
        } else {
            rc = XE_BAD_REQUEST;
            goto bad;
        }
    }
    rc = jobs_start(kind, &c, src, dst, overwrite, id);
    smbconn_clear(&c);
    if (rc != 0) {
        reply_err(b, rc, NULL);
        return;
    }
    cache_drop();
    sj_raw(b, "{");
    sj_kv_str(b, 0, "job", id);
    sj_raw(b, "}");
    return;
bad:
    smbconn_clear(&c);
    reply_err(b, rc, NULL);
}

static void op_scan(const char *r, size_t n, sj_buf_t *b) {
    smbconn_t c;
    char source[LIB_SOURCE_MAX], path[SMBP_PATH_MAX], id[33];
    int rc, local = 0;
    memset(&c, 0, sizeof c);
    path[0] = '\0';
    if (sj_str(r, n, "source", source, sizeof source) != 1 || !lib_source_ok(source) || sj_bool(r, n, "local", &local) < 0) {
        reply_err(b, XE_BAD_REQUEST, NULL);
        return;
    }
    if (!local && ((rc = conn_from(r, n, &c, 1)) != 0 || (rc = rel_from(r, n, "path", path, sizeof path)) != 0)) {
        smbconn_clear(&c);
        reply_err(b, rc, NULL);
        return;
    }
    rc = jobs_start(local ? JOB_SCAN_LOCAL : JOB_SCAN_SMB, &c, path, source, 0, id);
    smbconn_clear(&c);
    if (rc != 0) {
        reply_err(b, rc, NULL);
        return;
    }
    sj_raw(b, "{");
    sj_kv_str(b, 0, "job", id);
    sj_raw(b, "}");
}

static void handle(const char *r, size_t n, sj_buf_t *b) {
    char key[SMBD_KEY_HEX + 8], op[16], job[40];
    if (sj_str(r, n, "key", key, sizeof key) != 1 || !key_ok(key)) {
        sj_raw(b, "{\"error\":\"key\"}");
        return;
    }
    memset(key, 0, sizeof key);
    if (sj_str(r, n, "op", op, sizeof op) != 1) {
        reply_err(b, XE_BAD_REQUEST, NULL);
        return;
    }
    g_last_request = time(NULL);
    if (strcmp(op, "ping") == 0) {
        sj_raw(b, "{\"ok\":true");
        sj_kv_str(b, 1, "version", PS5UPLOAD2_VERSION);
        sj_kv_str(b, 1, "elf", g_elf_id);
        sj_fmt(b, ",\"pid\":%ld,\"running\":%d}", (long)getpid(), jobs_running());
    } else if (strcmp(op, "quit") == 0) {
        g_quit = 1;
        jobs_cancel_all();
        sj_raw(b, "{\"ok\":true}");
    } else if (strcmp(op, "shares") == 0) {
        op_shares(r, n, b);
    } else if (strcmp(op, "list") == 0) {
        op_list(r, n, b);
    } else if (strcmp(op, "probe") == 0 || strcmp(op, "stat") == 0 || strcmp(op, "mkdir") == 0) {
        op_simple(op, r, n, b);
    } else if (strcmp(op, "rename") == 0) {
        op_rename(r, n, b);
    } else if (strcmp(op, "copy") == 0) {
        op_copy(r, n, b, 0);
    } else if (strcmp(op, "remove") == 0) {
        op_copy(r, n, b, 1);
    } else if (strcmp(op, "scan") == 0) {
        op_scan(r, n, b);
    } else if (strcmp(op, "job") == 0 || strcmp(op, "cancel") == 0) {
        int rc;
        if (sj_str(r, n, "job", job, sizeof job) != 1) {
            reply_err(b, XE_BAD_REQUEST, NULL);
            return;
        }
        rc = op[0] == 'j' ? jobs_status(job, b) : jobs_cancel(job);
        if (rc != 0) reply_err(b, rc, NULL);
        else if (op[0] == 'c') sj_raw(b, "{\"ok\":true}");
    } else if (strcmp(op, "jobs") == 0) {
        jobs_list(b);
    } else {
        reply_err(b, XE_BAD_REQUEST, NULL);
    }
}

/* ---- the server ---- */

static void *conn_main(void *arg) {
    int fd = (int)(intptr_t)arg;
    char *req = malloc(SMBD_REQ_MAX + 1);
    size_t got = 0;
    sj_buf_t b;
    smbd_name_thread();
    sj_init(&b, SMBD_REPLY_MAX);
    if (req) {
        for (;;) {
            ssize_t k;
            char *nl;
            if (got >= SMBD_REQ_MAX) {
                got = 0;
                break;
            }
            k = recv(fd, req + got, SMBD_REQ_MAX - got, 0);
            if (k <= 0) {
                if (k < 0 && errno == EINTR) continue;
                got = 0;
                break;
            }
            got += (size_t)k;
            req[got] = '\0';
            nl = memchr(req, '\n', got);
            if (nl) {
                got = (size_t)(nl - req);
                break;
            }
        }
        if (got) handle(req, got, &b);
        memset(req, 0, SMBD_REQ_MAX + 1); /* it may have carried a password */
        free(req);
    }
    if (b.err || !b.buf) {
        sj_free(&b);
        sj_init(&b, 0);
        sj_raw(&b, "{\"error\":\"out_of_memory\"}");
    }
    sj_raw(&b, "\n");
    if (b.buf) {
        size_t off = 0;
        while (off < b.len) {
            ssize_t w = send(fd, b.buf + off, b.len - off, 0);
            if (w < 0 && errno == EINTR) continue;
            if (w <= 0) break;
            off += (size_t)w;
        }
    }
    sj_free(&b);
    close(fd);
    pthread_mutex_lock(&g_mu);
    g_conns--;
    pthread_mutex_unlock(&g_mu);
    return NULL;
}

static int listen_loopback(void) {
    struct sockaddr_in a;
    int one = 1, fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(SMBD_PORT);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(fd, (struct sockaddr *)&a, sizeof a) != 0 || listen(fd, 16) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

/* elfldr hands the payload the sender's socket as stdin/stdout/stderr, and the helper closes its end
 * once the ELF is sent: point all three somewhere safe. */
static void detach_stdio(void) {
    int fd = open("/dev/null", O_RDWR);
    if (fd < 0) return;
    dup2(fd, STDIN_FILENO);
    dup2(fd, STDOUT_FILENO);
    dup2(fd, STDERR_FILENO);
    if (fd > STDERR_FILENO) close(fd);
}

int main(void) {
    int lfd;
    smbd_name_thread();
    signal(SIGPIPE, SIG_IGN);
    umask(0);
    detach_stdio();
    mkdir("/data/ps5upload", 0777);
    mkdir(SMBD_DIR, 0700);
    if (read_key(g_key, g_elf_id, sizeof g_elf_id) != 0) {
        smbd_log("no key at %s: not starting", SMBD_KEY_PATH);
        return 1;
    }
    lfd = listen_loopback();
    if (lfd < 0) {
        smbd_log("port %d busy (errno %d): another daemon runs; exiting", SMBD_PORT, errno);
        return 1;
    }
    g_last_request = time(NULL);
    smbd_log("started v%s elf %s pid %ld", PS5UPLOAD2_VERSION, g_elf_id, (long)getpid());
    while (!g_quit) {
        struct timeval tv = {5, 0};
        fd_set rf;
        int fd, sel;
        FD_ZERO(&rf);
        FD_SET(lfd, &rf);
        sel = select(lfd + 1, &rf, NULL, NULL, &tv);
        if (sel < 0 && errno != EINTR) {
            smbd_log("select failed (errno %d)", errno);
            sleep(1);
            continue;
        }
        if (sel <= 0) {
            if (jobs_running() == 0 && time(NULL) - g_last_request > SMBD_IDLE_EXIT_S) {
                smbd_log("idle: exiting");
                break;
            }
            continue;
        }
        fd = accept(lfd, NULL, NULL);
        if (fd < 0) continue;
        {
            struct timeval to = {REQ_TIMEOUT_S, 0};
            pthread_attr_t at;
            pthread_t th;
            int busy;
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);
            setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &to, sizeof to);
            pthread_mutex_lock(&g_mu);
            busy = g_conns >= CONNS_MAX;
            if (!busy) g_conns++;
            pthread_mutex_unlock(&g_mu);
            if (busy) {
                static const char k_busy[] = "{\"error\":\"busy\"}\n";
                (void)send(fd, k_busy, sizeof k_busy - 1, 0);
                close(fd);
                continue;
            }
            pthread_attr_init(&at);
            pthread_attr_setstacksize(&at, CONN_STACK);
            pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
            if (pthread_create(&th, &at, conn_main, (void *)(intptr_t)fd) != 0) {
                close(fd);
                pthread_mutex_lock(&g_mu);
                g_conns--;
                pthread_mutex_unlock(&g_mu);
            }
            pthread_attr_destroy(&at);
        }
    }
    close(lfd);
    /* quit: give cancelled jobs a moment to remove their part files */
    {
        int i;
        for (i = 0; i < 50 && jobs_running() > 0; i++) usleep(100 * 1000);
    }
    cache_drop();
    smbd_log("stopped");
    return 0;
}
