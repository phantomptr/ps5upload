/* See include/webui_smb.h. Routes run on the web server's worker threads (256 KiB stacks): every
 * buffer larger than a path is on the heap, and a password is wiped from every buffer that held it. */
#include "webui_smb.h"

#include <dirent.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "ava1_data.h"
#include "monocypher.h"
#include "path_policy.h"
#include "smb_client.h"
#include "smb_shares.h"
#include "smbd_json.h"
#include "smbd_path.h"
#include "smbd_proto.h"
#include "webui_server.h"

/* A request that talks to a server waits this long for the daemon (its own SMB timeout is 8 s). */
#define SMB_WAIT_MS 12000
#define SMB_QUICK_MS 4000
/* Server requests in flight at once: the web server has four workers, and a server that does not
 * answer must not hold them all. */
#define SMB_INFLIGHT_MAX 2

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static int g_inflight;

/* A removed share's library cache (smbd_library.h: <id>.json and icons/<id>-*.png) goes with it;
 * it used to stay behind forever. Best effort: a cache that was never written is nothing to do. */
static void drop_library_cache(const char *id) {
    char path[SMBP_PATH_MAX];
    char prefix[SMB_ID_LEN + 2];
    DIR *d;
    struct dirent *e;
    if (snprintf(path, sizeof path, "%s/%s.json", SMBD_LIB_DIR, id) < (int)sizeof path) unlink(path);
    snprintf(prefix, sizeof prefix, "%s-", id);
    snprintf(path, sizeof path, "%s/icons", SMBD_LIB_DIR);
    d = opendir(path);
    if (!d) return;
    while ((e = readdir(d)) != NULL) {
        if (strncmp(e->d_name, prefix, strlen(prefix)) != 0) continue;
        if (snprintf(path, sizeof path, "%s/icons/%s", SMBD_LIB_DIR, e->d_name) < (int)sizeof path) unlink(path);
    }
    closedir(d);
}

int webui_smb_http_status(const char *t) {
    if (!t) return 500;
    if (!strcmp(t, "bad_request") || !strcmp(t, "bad_name") || !strncmp(t, "bad_", 4) || !strcmp(t, "too_deep") ||
        !strcmp(t, "into_itself"))
        return 400;
    if (!strcmp(t, "smb_auth") || !strcmp(t, "access_denied") || !strcmp(t, "path_not_allowed")) return 403;
    if (!strcmp(t, "smb_no_share") || !strcmp(t, "not_found") || !strcmp(t, "unknown_share") ||
        !strcmp(t, "unknown_job"))
        return 404;
    if (!strcmp(t, "exists") || !strcmp(t, "not_dir") || !strcmp(t, "is_dir") || !strcmp(t, "cancelled") ||
        !strcmp(t, "smb_not_supported"))
        return 409;
    if (!strcmp(t, "no_space") || !strcmp(t, "shares_full")) return 507;
    if (!strcmp(t, "busy") || !strncmp(t, "smb_daemon_", 11) || !strcmp(t, "smb_loader_unavailable")) return 503;
    if (!strcmp(t, "smb_unreachable") || !strcmp(t, "smb_timeout") || !strcmp(t, "smb_disconnected")) return 502;
    return 500;
}

/* ---- replies ---- */

static void reply(int fd, int status, sj_buf_t *b) {
    if (b->err || !b->buf) webui_send_json(fd, 500, "{\"error\":\"out_of_memory\"}");
    else webui_send(fd, status, "application/json", b->buf, b->len);
}

static void fail(int fd, const char *token, const char *detail) {
    sj_buf_t b;
    sj_init(&b, 2048);
    sj_raw(&b, "{");
    sj_kv_str(&b, 0, "error", token);
    if (detail && detail[0]) sj_kv_str(&b, 1, "detail", detail);
    sj_raw(&b, "}");
    reply(fd, webui_smb_http_status(token), &b);
    sj_free(&b);
}

/* A daemon reply as it is (200), or its {"error"} with the matching status. */
static void relay(int fd, const char *r) {
    char tok[64];
    size_t n = strlen(r);
    if (sj_str(r, n, "error", tok, sizeof tok) == 1) {
        if (!strcmp(tok, "key")) strcpy(tok, "smb_daemon_unavailable");
        webui_send(fd, webui_smb_http_status(tok), "application/json", r, n);
        return;
    }
    webui_send(fd, 200, "application/json", r, n);
}

static int enter(int fd) {
    int ok;
    pthread_mutex_lock(&g_mu);
    ok = g_inflight < SMB_INFLIGHT_MAX;
    if (ok) g_inflight++;
    pthread_mutex_unlock(&g_mu);
    if (!ok) fail(fd, "busy", NULL);
    return ok ? 0 : -1;
}

static void leave(void) {
    pthread_mutex_lock(&g_mu);
    g_inflight--;
    pthread_mutex_unlock(&g_mu);
}

/* Ask the daemon; answer the page with its reply (relay) or with why it could not be asked. The
 * request buffer is wiped (it carries the password). Returns the reply (heap) when keep is set. */
static char *ask(int fd, sj_buf_t *req, int timeout_ms, int keep) {
    char *r = NULL, err[48];
    int rc;
    if (req->err || !req->buf) {
        sj_free(req);
        fail(fd, "out_of_memory", NULL);
        return NULL;
    }
    rc = smbc_request(req->buf, req->len, timeout_ms, 1, &r, err, sizeof err);
    crypto_wipe(req->buf, req->cap);
    sj_free(req);
    if (rc != 0) {
        fail(fd, err[0] ? err : "smb_daemon_unavailable", NULL);
        return NULL;
    }
    if (keep) return r;
    relay(fd, r);
    free(r);
    return NULL;
}

/* The connection fields of a saved share (password included) for a daemon request. */
static void conn_fields(sj_buf_t *b, const smb_share_t *s) {
    sj_kv_str(b, 1, "host", s->host);
    sj_fmt(b, ",\"port\":%u", (unsigned)s->port);
    sj_kv_str(b, 1, "share", s->share);
    sj_kv_str(b, 1, "user", s->user);
    sj_kv_str(b, 1, "password", s->password);
    sj_kv_str(b, 1, "domain", s->domain);
}

static void share_json(sj_buf_t *b, const smb_share_t *s) {
    char path[1100];
    smb_path_make(s->id, s->start_path, path, sizeof path);
    sj_raw(b, "{");
    sj_kv_str(b, 0, "id", s->id);
    sj_kv_str(b, 1, "name", s->name);
    sj_kv_str(b, 1, "host", s->host);
    sj_fmt(b, ",\"port\":%u", (unsigned)s->port);
    sj_kv_str(b, 1, "share", s->share);
    sj_kv_str(b, 1, "user", s->user);
    sj_kv_str(b, 1, "domain", s->domain);
    sj_kv_str(b, 1, "start_path", s->start_path);
    sj_fmt(b, ",\"has_password\":%s", s->password[0] ? "true" : "false");
    sj_kv_str(b, 1, "path", path);
    sj_raw(b, "}");
}

/* "path" of the request as a saved share (password included: wipe it) + the path in it. On
 * failure the page has been answered and -1 returned. */
static int share_path(int fd, const char *b, size_t n, const char *key, smb_share_t *s, char *rel, size_t cap) {
    char p[SMBP_PATH_MAX + 32], id[SMB_ID_LEN + 1];
    char tok[16];
    int rc;
    snprintf(tok, sizeof tok, "bad_%s", key);
    if (sj_str(b, n, key, p, sizeof p) != 1 || smb_path_parse(p, id, rel, cap) != 0) {
        fail(fd, tok, NULL);
        return -1;
    }
    rc = smb_shares_get(id, s);
    if (rc != 0) {
        fail(fd, rc == SMB_SHARES_E_NOT_FOUND ? "unknown_share" : "io_error", NULL);
        return -1;
    }
    return 0;
}

static void wipe_share(smb_share_t *s) { crypto_wipe(s->password, sizeof s->password); }

/* ---- routes ---- */

void webui_smb_shares(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *v = calloc(SMB_SHARES_MAX, sizeof *v);
    sj_buf_t r;
    int cnt, i;
    (void)b;
    (void)n;
    (void)out;
    if (!v) {
        fail(fd, "out_of_memory", NULL);
        return;
    }
    cnt = smb_shares_load(v, SMB_SHARES_MAX);
    if (cnt < 0) {
        free(v);
        fail(fd, "io_error", NULL);
        return;
    }
    sj_init(&r, 0);
    sj_raw(&r, "{\"shares\":[");
    for (i = 0; i < cnt; i++) {
        if (i) sj_raw(&r, ",");
        share_json(&r, &v[i]);
    }
    sj_raw(&r, "]}");
    crypto_wipe(v, SMB_SHARES_MAX * sizeof *v);
    free(v);
    reply(fd, 200, &r);
    sj_free(&r);
}

/* The "server" text the page sends, as a host and port (and a share / folder when it carried one).
 * NULL, or the refusal's token. */
static const char *server_of(const char *b, size_t n, smb_share_t *s) {
    char text[SMBP_PATH_MAX + SMBP_HOST_MAX], share[SMBP_SHARE_MAX], path[SMBP_PATH_MAX];
    if (sj_str(b, n, "server", text, sizeof text) != 1 ||
        smbp_parse_server(text, s->host, sizeof s->host, &s->port, share, sizeof share, path, sizeof path) != 0)
        return "bad_host";
    if (!s->share[0] && share[0]) snprintf(s->share, sizeof s->share, "%s", share);
    if (!s->start_path[0] && path[0]) snprintf(s->start_path, sizeof s->start_path, "%s", path);
    return NULL;
}

static const char *creds_of(const char *b, size_t n, smb_share_t *s, int *password_given) {
    int rc;
    if (sj_str(b, n, "user", s->user, sizeof s->user) < 0) return "bad_user";
    if (sj_str(b, n, "domain", s->domain, sizeof s->domain) < 0) return "bad_domain";
    rc = sj_str(b, n, "password", s->password, sizeof s->password);
    if (rc < 0) return "bad_password";
    if (password_given) *password_given = rc == 1;
    return NULL;
}

void webui_smb_discover(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *s = calloc(1, sizeof *s), *old = NULL;
    sj_buf_t req, r;
    char *rep, *arr;
    const char *why;
    size_t rl;
    int given = 0, rc;
    (void)out;
    if (!s) {
        fail(fd, "out_of_memory", NULL);
        return;
    }
    if ((why = server_of(b, n, s)) != NULL || (why = creds_of(b, n, s, &given)) != NULL) {
        fail(fd, why, NULL);
        goto done;
    }
    /* editing a saved share ("id") without retyping its password looks with the saved one, for
     * the same server and user, as save_core does */
    if ((rc = sj_str(b, n, "id", s->id, sizeof s->id)) < 0) {
        fail(fd, "bad_id", NULL);
        goto done;
    }
    if (rc == 1 && !given) {
        if (!(old = calloc(1, sizeof *old))) {
            fail(fd, "out_of_memory", NULL);
            goto done;
        }
        if (smb_shares_get(s->id, old) != 0) {
            fail(fd, "unknown_share", NULL);
            goto done;
        }
        if (!strcmp(old->host, s->host) && !strcmp(old->user, s->user)) memcpy(s->password, old->password, sizeof s->password);
    }
    if (enter(fd) != 0) goto done;
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"shares\"");
    sj_kv_str(&req, 1, "host", s->host);
    sj_fmt(&req, ",\"port\":%u", (unsigned)s->port);
    sj_kv_str(&req, 1, "user", s->user);
    sj_kv_str(&req, 1, "password", s->password);
    sj_kv_str(&req, 1, "domain", s->domain);
    rep = ask(fd, &req, SMB_WAIT_MS, 1);
    leave();
    if (!rep) goto done;
    rl = strlen(rep);
    arr = strstr(rep, "\"shares\":");
    if (!arr || sj_str(rep, rl, "error", s->name, sizeof s->name) == 1) {
        relay(fd, rep);
        free(rep);
        goto done;
    }
    /* the daemon's {"shares":[...]} with the parsed address in front */
    sj_init(&r, 0);
    sj_raw(&r, "{");
    sj_kv_str(&r, 0, "host", s->host);
    sj_fmt(&r, ",\"port\":%u", (unsigned)s->port);
    sj_kv_str(&r, 1, "share", s->share);
    sj_kv_str(&r, 1, "path", s->start_path);
    sj_raw(&r, ",");
    sj_raw(&r, arr);
    free(rep);
    reply(fd, 200, &r);
    sj_free(&r);
done:
    if (old) {
        wipe_share(old);
        free(old);
    }
    wipe_share(s);
    free(s);
}

/* Save (add, or replace by id) a share after signing in to it and opening its start folder: 200
 * with *res = {"share":share,"dialect"}; else the HTTP status with `tok` set, and *res the daemon's
 * own {"error","detail"} when it answered one (empty otherwise). *res is always set up: free it.
 * The page's /api/smb/save and the PC's smb.shares_save both come here. */
static int save_core(const char *b, size_t n, sj_buf_t *res, char *tok, size_t tok_cap) {
    smb_share_t *s = calloc(1, sizeof *s), *old = calloc(1, sizeof *old);
    sj_buf_t req;
    char *rep = NULL, dialect[16] = "", err[48] = "";
    const char *why = NULL;
    int given = 0, rc, st = 500;
    sj_init(res, 0);
    tok[0] = '\0';
    if (!s || !old) {
        why = "out_of_memory";
        goto done;
    }
    if (sj_str(b, n, "id", s->id, sizeof s->id) < 0) {
        why = "bad_id";
        goto done;
    }
    if (s->id[0] && smb_shares_get(s->id, old) != 0) {
        why = "unknown_share";
        goto done;
    }
    if (sj_str(b, n, "name", s->name, sizeof s->name) < 0 || sj_str(b, n, "share", s->share, sizeof s->share) < 0 ||
        sj_str(b, n, "start_path", s->start_path, sizeof s->start_path) < 0) {
        why = "bad_request";
        goto done;
    }
    if ((why = server_of(b, n, s)) != NULL || (why = creds_of(b, n, s, &given)) != NULL) goto done;
    /* editing without retyping the password keeps the saved one (for the same server and user) */
    if (s->id[0] && !given && !strcmp(old->host, s->host) && !strcmp(old->user, s->user))
        memcpy(s->password, old->password, sizeof s->password);
    if (smb_share_check(s, &why) != 0) goto done;
    pthread_mutex_lock(&g_mu);
    rc = g_inflight < SMB_INFLIGHT_MAX;
    if (rc) g_inflight++;
    pthread_mutex_unlock(&g_mu);
    if (!rc) {
        why = "busy";
        goto done;
    }
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"probe\"");
    conn_fields(&req, s);
    sj_kv_str(&req, 1, "path", s->start_path);
    rc = req.err || !req.buf ? -1 : smbc_request(req.buf, req.len, SMB_WAIT_MS, 1, &rep, err, sizeof err);
    if (req.buf) crypto_wipe(req.buf, req.cap);
    sj_free(&req);
    leave();
    if (rc != 0) {
        snprintf(tok, tok_cap, "%s", err[0] ? err : "smb_daemon_unavailable");
        st = webui_smb_http_status(tok);
        goto done;
    }
    if (sj_str(rep, strlen(rep), "error", tok, tok_cap) == 1) {
        if (!strcmp(tok, "key")) snprintf(tok, tok_cap, "smb_daemon_unavailable");
        else sj_raw(res, rep);
        st = webui_smb_http_status(tok);
        goto done;
    }
    (void)sj_str(rep, strlen(rep), "dialect", dialect, sizeof dialect);
    rc = smb_shares_put(s);
    if (rc != 0) {
        why = rc == SMB_SHARES_E_FULL ? "shares_full" : "io_error";
        goto done;
    }
    sj_raw(res, "{\"share\":");
    share_json(res, s);
    sj_kv_str(res, 1, "dialect", dialect);
    sj_raw(res, "}");
    st = 200;
done:
    if (why) {
        snprintf(tok, tok_cap, "%s", why);
        st = webui_smb_http_status(tok);
    }
    free(rep);
    if (s) wipe_share(s);
    if (old) wipe_share(old);
    free(s);
    free(old);
    return st;
}

void webui_smb_save(const char *b, size_t n, uint8_t *out, int fd) {
    sj_buf_t r;
    char tok[64];
    int st = save_core(b, n, &r, tok, sizeof tok);
    (void)out;
    if (st == 200 || (r.buf && r.len && !r.err)) reply(fd, st, &r);
    else fail(fd, tok[0] ? tok : "internal", NULL);
    sj_free(&r);
}

void webui_smb_remove(const char *b, size_t n, uint8_t *out, int fd) {
    char id[SMB_ID_LEN + 2];
    int rc;
    (void)out;
    if (sj_str(b, n, "id", id, sizeof id) != 1 || strlen(id) != SMB_ID_LEN) {
        fail(fd, "bad_id", NULL);
        return;
    }
    rc = smb_shares_remove(id);
    if (rc == SMB_SHARES_E_NOT_FOUND) fail(fd, "unknown_share", NULL);
    else if (rc != 0) fail(fd, "io_error", NULL);
    else {
        drop_library_cache(id);
        webui_send_json(fd, 200, "{}");
    }
}

/* list / stat / mkdir: one saved share, one path, one daemon call. */
static void path_op(const char *op, const char *b, size_t n, int fd) {
    smb_share_t *s = calloc(1, sizeof *s);
    char *rel = malloc(SMBP_PATH_MAX), *rep;
    sj_buf_t req;
    uint64_t offset = 0, limit = SMBD_LIST_MAX;
    int missing_ok = 0;
    if (!s || !rel) {
        fail(fd, "out_of_memory", NULL);
        goto done;
    }
    if (share_path(fd, b, n, "path", s, rel, SMBP_PATH_MAX) != 0) goto done;
    if (!strcmp(op, "list") && (sj_u64(b, n, "offset", &offset) < 0 || sj_u64(b, n, "limit", &limit) < 0)) {
        fail(fd, "bad_request", NULL);
        goto done;
    }
    if (!strcmp(op, "mkdir") && !rel[0]) {
        fail(fd, "bad_path", NULL);
        goto done;
    }
    if (sj_bool(b, n, "missing_ok", &missing_ok) < 0) missing_ok = 0;
    if (enter(fd) != 0) goto done;
    sj_init(&req, 0);
    sj_fmt(&req, "\"op\":\"%s\"", op);
    conn_fields(&req, s);
    sj_kv_str(&req, 1, "path", rel);
    if (!strcmp(op, "list")) sj_fmt(&req, ",\"offset\":%llu,\"limit\":%llu", (unsigned long long)offset, (unsigned long long)limit);
    rep = ask(fd, &req, SMB_WAIT_MS, 1);
    leave();
    if (rep) {
        char tok[32];
        if (missing_ok && sj_str(rep, strlen(rep), "error", tok, sizeof tok) == 1 && !strcmp(tok, "not_found"))
            webui_send_json(fd, 200, "{\"missing\":true}");
        else relay(fd, rep);
        free(rep);
    }
done:
    if (s) wipe_share(s);
    free(s);
    free(rel);
}

void webui_smb_list(const char *b, size_t n, uint8_t *out, int fd) {
    (void)out;
    path_op("list", b, n, fd);
}

void webui_smb_stat(const char *b, size_t n, uint8_t *out, int fd) {
    (void)out;
    path_op("stat", b, n, fd);
}

void webui_smb_mkdir(const char *b, size_t n, uint8_t *out, int fd) {
    (void)out;
    path_op("mkdir", b, n, fd);
}

/* The helper's own policy for a console path: what its file routes may read / write. */
static int local_allowed(const char *p, int write) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    if (!smbp_local_ok(p) || path_in_protected(p) || path_tree_op_refused(p) || !cfg) return 0;
    return write ? cfg->may_write && cfg->may_write(p) : cfg->may_read && cfg->may_read(p, 0);
}

void webui_smb_copy(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *s = calloc(1, sizeof *s);
    char *src = malloc(SMBP_PATH_MAX + 32), *dst = malloc(SMBP_PATH_MAX + 32), *rel = malloc(SMBP_PATH_MAX);
    sj_buf_t req;
    int overwrite = 0, down;
    (void)out;
    if (!s || !src || !dst || !rel) {
        fail(fd, "out_of_memory", NULL);
        goto done;
    }
    if (sj_str(b, n, "src", src, SMBP_PATH_MAX + 32) != 1 || sj_str(b, n, "dest", dst, SMBP_PATH_MAX + 32) != 1) {
        fail(fd, "bad_request", NULL);
        goto done;
    }
    if (sj_bool(b, n, "overwrite", &overwrite) < 0) {
        fail(fd, "bad_overwrite", NULL);
        goto done;
    }
    down = !strncmp(src, SMB_PATH_PREFIX, sizeof SMB_PATH_PREFIX - 1);
    if (down == !strncmp(dst, SMB_PATH_PREFIX, sizeof SMB_PATH_PREFIX - 1)) {
        fail(fd, "bad_request", NULL); /* exactly one side is a share */
        goto done;
    }
    if (share_path(fd, b, n, down ? "src" : "dest", s, rel, SMBP_PATH_MAX) != 0) goto done;
    if (!rel[0]) {
        fail(fd, down ? "bad_src" : "bad_dest", NULL); /* never the whole share as one item */
        goto done;
    }
    if (!local_allowed(down ? dst : src, down)) {
        fail(fd, "path_not_allowed", NULL);
        goto done;
    }
    /* Counted like every other daemon call (enter/leave): the first request after a start waits
     * for the daemon to come up, and uncounted ones parked every web worker behind it, the page's
     * own files with them. Over the limit the page is told "busy" at once. */
    if (enter(fd) != 0) goto done;
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"copy\"");
    conn_fields(&req, s);
    sj_kv_str(&req, 1, "dir", down ? "down" : "up");
    sj_kv_str(&req, 1, "src", down ? rel : src);
    sj_kv_str(&req, 1, "dest", down ? dst : rel);
    sj_fmt(&req, ",\"overwrite\":%s", overwrite ? "true" : "false");
    fprintf(stderr, "[smb] copy %s %s share %s\n", down ? "from" : "to", s->host, s->share);
    (void)ask(fd, &req, SMB_QUICK_MS, 0);
    leave();
done:
    if (s) wipe_share(s);
    free(s);
    free(src);
    free(dst);
    free(rel);
}

void webui_smb_delete(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *s = calloc(1, sizeof *s);
    char *rel = malloc(SMBP_PATH_MAX);
    sj_buf_t req;
    (void)out;
    if (!s || !rel) {
        fail(fd, "out_of_memory", NULL);
        goto done;
    }
    if (share_path(fd, b, n, "path", s, rel, SMBP_PATH_MAX) != 0) goto done;
    if (!rel[0]) {
        fail(fd, "bad_path", NULL);
        goto done;
    }
    if (enter(fd) != 0) goto done; /* counted, as in webui_smb_copy */
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"remove\"");
    conn_fields(&req, s);
    sj_kv_str(&req, 1, "path", rel);
    (void)ask(fd, &req, SMB_QUICK_MS, 0);
    leave();
done:
    if (s) wipe_share(s);
    free(s);
    free(rel);
}

/* A rename inside one folder of a share: the swap of a Replace (the page's files/replace.ts). The daemon
 * refuses anything else as well; a target that is taken is the server's own "exists". */
void webui_smb_rename(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *s = calloc(1, sizeof *s);
    char *from = malloc(SMBP_PATH_MAX), *to = malloc(SMBP_PATH_MAX), *pf = malloc(SMBP_PATH_MAX), *pt = malloc(SMBP_PATH_MAX);
    char *raw = malloc(SMBP_PATH_MAX + 32), *rep;
    char id[SMB_ID_LEN + 1];
    sj_buf_t req;
    (void)out;
    if (!s || !from || !to || !pf || !pt || !raw) {
        fail(fd, "out_of_memory", NULL);
        goto done;
    }
    if (share_path(fd, b, n, "from", s, from, SMBP_PATH_MAX) != 0) goto done;
    if (sj_str(b, n, "to", raw, SMBP_PATH_MAX + 32) != 1 || smb_path_parse(raw, id, to, SMBP_PATH_MAX) != 0 || strcmp(id, s->id) != 0) {
        fail(fd, "bad_to", NULL);
        goto done;
    }
    smbp_parent(from, pf, SMBP_PATH_MAX);
    smbp_parent(to, pt, SMBP_PATH_MAX);
    if (!from[0] || !to[0] || strcmp(pf, pt) != 0) {
        fail(fd, "bad_path", NULL);
        goto done;
    }
    if (enter(fd) != 0) goto done;
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"rename\"");
    conn_fields(&req, s);
    sj_kv_str(&req, 1, "path", from);
    sj_kv_str(&req, 1, "to", to);
    rep = ask(fd, &req, SMB_WAIT_MS, 1);
    leave();
    if (rep) {
        relay(fd, rep);
        free(rep);
    }
done:
    if (s) wipe_share(s);
    free(s);
    free(from);
    free(to);
    free(pf);
    free(pt);
    free(raw);
}

static void job_op(const char *op, const char *b, size_t n, int fd) {
    char job[40];
    size_t i;
    sj_buf_t req;
    if (sj_str(b, n, "job", job, sizeof job) != 1 || strlen(job) != 32) {
        fail(fd, "bad_job", NULL);
        return;
    }
    for (i = 0; i < 32; i++)
        if (!((job[i] >= '0' && job[i] <= '9') || (job[i] >= 'a' && job[i] <= 'f'))) {
            fail(fd, "bad_job", NULL);
            return;
        }
    sj_init(&req, 0);
    sj_fmt(&req, "\"op\":\"%s\"", op);
    sj_kv_str(&req, 1, "job", job);
    {
        char *r = NULL, err[48], tok[32];
        int rc = smbc_request(req.buf ? req.buf : "", req.len, SMB_QUICK_MS, 0, &r, err, sizeof err);
        sj_free(&req);
        if (rc != 0) {
            /* no daemon: it ended (rest mode, idle) and its jobs with it */
            fail(fd, !strcmp(err, "smb_daemon_not_running") ? "unknown_job" : err, NULL);
            return;
        }
        {
            size_t rn = strlen(r);
            char st[16];
            /* A job's status carries its state and, once failed or cancelled, "error":"<why>":
             * that is the job's outcome, answered 200 as it is, never the daemon's refusal of the
             * request (which relay() would turn into 507/409/502 and the page into "disconnected",
             * leaving its failed / cancelled branches unreachable). */
            if (sj_str(r, rn, "state", st, sizeof st) == 1) webui_send(fd, 200, "application/json", r, rn);
            else if (sj_str(r, rn, "error", tok, sizeof tok) == 1 && !strcmp(tok, "not_found")) fail(fd, "unknown_job", NULL);
            else relay(fd, r);
        }
        free(r);
    }
}

void webui_smb_job(const char *b, size_t n, uint8_t *out, int fd) {
    (void)out;
    job_op("job", b, n, fd);
}

void webui_smb_cancel(const char *b, size_t n, uint8_t *out, int fd) {
    (void)out;
    job_op("cancel", b, n, fd);
}

void webui_smb_jobs(const char *b, size_t n, uint8_t *out, int fd) {
    char *r = NULL, err[48];
    (void)b;
    (void)n;
    (void)out;
    /* never starts the daemon just to list: no daemon, no jobs */
    if (smbc_request("\"op\":\"jobs\"", 11, SMB_QUICK_MS, 0, &r, err, sizeof err) != 0) {
        if (!strcmp(err, "smb_daemon_not_running")) webui_send_json(fd, 200, "{\"jobs\":[]}");
        else fail(fd, err, NULL);
        return;
    }
    relay(fd, r);
    free(r);
}

/* A listing for a poll: a daemon that is slow to answer costs one empty list, not a worker's seconds. */
#define SMB_PC_JOBS_MS 1500

char *webui_smb_pc_jobs(void) {
    char *r = NULL, err[48];
    if (smbc_request("\"op\":\"jobs\"", 11, SMB_PC_JOBS_MS, 0, &r, err, sizeof err) != 0) return NULL;
    return r;
}

int webui_smb_pc_cancel(const char *id, char *err, size_t err_cap) {
    sj_buf_t req;
    char *r = NULL, tok[32], daemon_err[48];
    int rc, i;
    if (strlen(id) != 32) {
        snprintf(err, err_cap, "bad_job");
        return -1;
    }
    for (i = 0; i < 32; i++)
        if (!((id[i] >= '0' && id[i] <= '9') || (id[i] >= 'a' && id[i] <= 'f'))) {
            snprintf(err, err_cap, "bad_job");
            return -1;
        }
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"cancel\"");
    sj_kv_str(&req, 1, "job", id);
    rc = smbc_request(req.buf ? req.buf : "", req.len, SMB_QUICK_MS, 0, &r, daemon_err, sizeof daemon_err);
    sj_free(&req);
    if (rc != 0) {
        snprintf(err, err_cap, "%s", !strcmp(daemon_err, "smb_daemon_not_running") ? "unknown_job" : daemon_err);
        return -1;
    }
    rc = 0;
    if (sj_str(r, strlen(r), "error", tok, sizeof tok) == 1 && strcmp(tok, "cancelled") != 0) {
        snprintf(err, err_cap, "%s", !strcmp(tok, "not_found") ? "unknown_job" : tok);
        rc = -1;
    }
    free(r);
    return rc;
}

/* ---- the game library (#498) ---- */

void webui_library_scan(const char *b, size_t n, uint8_t *out, int fd) {
    smb_share_t *s = NULL;
    char source[SMB_ID_LEN + 8];
    sj_buf_t req;
    (void)out;
    if (sj_str(b, n, "source", source, sizeof source) != 1) {
        fail(fd, "bad_source", NULL);
        return;
    }
    sj_init(&req, 0);
    sj_raw(&req, "\"op\":\"scan\"");
    sj_kv_str(&req, 1, "source", source);
    if (!strcmp(source, "console")) {
        sj_raw(&req, ",\"local\":true");
    } else {
        int rc;
        s = calloc(1, sizeof *s);
        if (!s) {
            sj_free(&req);
            fail(fd, "out_of_memory", NULL);
            return;
        }
        rc = strlen(source) == SMB_ID_LEN ? smb_shares_get(source, s) : SMB_SHARES_E_NOT_FOUND;
        if (rc != 0) {
            sj_free(&req);
            free(s);
            fail(fd, rc == SMB_SHARES_E_NOT_FOUND ? "unknown_share" : "io_error", NULL);
            return;
        }
        conn_fields(&req, s);
        sj_kv_str(&req, 1, "path", s->start_path);
        fprintf(stderr, "[smb] library scan of %s share %s\n", s->host, s->share);
    }
    /* Counted, as in webui_smb_copy. Refused: the request (it carries the password) is wiped here,
     * as ask() would have. */
    if (enter(fd) != 0) {
        if (req.buf) crypto_wipe(req.buf, req.cap);
        sj_free(&req);
    } else {
        (void)ask(fd, &req, SMB_QUICK_MS, 0);
        leave();
    }
    if (s) wipe_share(s);
    free(s);
}

/* ---- the PC app (mgmt smb.shares_*) ---- */

/* `json` into out as the method's reply. */
static int pc_out(char *out, size_t cap, const char *json, size_t len) {
    if (len + 1 > cap) return snprintf(out, cap, "{\"ok\":false,\"err\":\"too_large\"}");
    memcpy(out, json, len);
    out[len] = '\0';
    return (int)len;
}

/* A refusal: {"ok":false,"err":token[,"detail"]}. */
static int pc_err(char *out, size_t cap, const char *tok, const char *detail) {
    sj_buf_t b;
    int k;
    sj_init(&b, 4096);
    sj_raw(&b, "{\"ok\":false");
    sj_kv_str(&b, 1, "err", tok);
    if (detail && detail[0]) sj_kv_str(&b, 1, "detail", detail);
    sj_raw(&b, "}");
    k = b.buf && !b.err ? pc_out(out, cap, b.buf, b.len) : snprintf(out, cap, "{\"ok\":false,\"err\":\"out_of_memory\"}");
    sj_free(&b);
    return k;
}

int webui_smb_pc_list(char *out, size_t cap) {
    smb_share_t *v = calloc(SMB_SHARES_MAX, sizeof *v);
    sj_buf_t r;
    int cnt, i, k;
    if (!v) return pc_err(out, cap, "out_of_memory", NULL);
    cnt = smb_shares_load(v, SMB_SHARES_MAX);
    if (cnt < 0) {
        free(v);
        return pc_err(out, cap, "io_error", NULL);
    }
    sj_init(&r, 0);
    sj_raw(&r, "{\"ok\":true,\"shares\":[");
    for (i = 0; i < cnt; i++) {
        if (i) sj_raw(&r, ",");
        share_json(&r, &v[i]);
    }
    sj_fmt(&r, "],\"max\":%d}", SMB_SHARES_MAX);
    crypto_wipe(v, SMB_SHARES_MAX * sizeof *v);
    free(v);
    k = r.buf && !r.err ? pc_out(out, cap, r.buf, r.len) : pc_err(out, cap, "out_of_memory", NULL);
    sj_free(&r);
    return k;
}

int webui_smb_pc_save(const char *b, size_t n, char *out, size_t cap) {
    sj_buf_t r;
    char tok[64], detail[160] = "";
    int k, st = save_core(b, n, &r, tok, sizeof tok);
    if (st == 200 && r.buf && r.len > 1) {
        /* {"share":..} -> {"ok":true,"share":..} */
        sj_buf_t o;
        sj_init(&o, 0);
        sj_raw(&o, "{\"ok\":true,");
        sj_raw(&o, r.buf + 1);
        k = o.buf && !o.err ? pc_out(out, cap, o.buf, o.len) : pc_err(out, cap, "out_of_memory", NULL);
        sj_free(&o);
    } else {
        if (r.buf && r.len) (void)sj_str(r.buf, r.len, "detail", detail, sizeof detail);
        k = pc_err(out, cap, tok[0] ? tok : "internal", detail);
    }
    sj_free(&r);
    return k;
}

int webui_smb_pc_remove(const char *b, size_t n, char *out, size_t cap) {
    char id[SMB_ID_LEN + 2];
    int rc;
    if (sj_str(b, n, "id", id, sizeof id) != 1 || strlen(id) != SMB_ID_LEN) return pc_err(out, cap, "bad_id", NULL);
    rc = smb_shares_remove(id);
    if (rc == SMB_SHARES_E_NOT_FOUND) return pc_err(out, cap, "unknown_share", NULL);
    if (rc != 0) return pc_err(out, cap, "io_error", NULL);
    drop_library_cache(id);
    return snprintf(out, cap, "{\"ok\":true}");
}
