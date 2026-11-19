/* xfs_t over libsmb2's synchronous API. One context per connection, used by one thread at a time.
 *
 * libsmb2 keeps one process-wide list of live contexts (init.c active_contexts), changed only by
 * smb2_init_context and smb2_destroy_context: those two are serialized here. Everything else
 * touches only its own context, so connections on different threads run in parallel. */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/types.h>
#include <time.h>
#include <stdlib.h>
#include <string.h>

#include <smb2/smb2.h>
#include <smb2/libsmb2.h>
#include <smb2/libsmb2-raw.h>
#include <smb2/libsmb2-share-enum.h>
#include <smb2/smb2-errors.h>

#include "smbd_fs.h"

static pthread_mutex_t g_lib_mu = PTHREAD_MUTEX_INITIALIZER;

/* libsmb2 keeps the last NT status it saw until another error replaces it, and a failed compound
 * (unlink, rename) reports the CLOSE's errno, not the CREATE's: the status is the only reliable
 * account of what the server said. It is cleared before every call so it never describes an
 * earlier one. (A library function without a public declaration: libsmb2-private.h.) */
void smb2_set_nterror(struct smb2_context *smb2, int nterror, const char *error_string, ...);
#define FRESH(ctx) smb2_set_nterror((ctx), 0, "%s", "")

typedef struct {
    struct smb2_context *ctx;
    smbconn_t conn;
} smbfs_t;

typedef struct {
    struct smb2dir *dir;
} sdir_t;

void smbconn_clear(smbconn_t *c) {
    volatile char *p = (volatile char *)c->password;
    size_t i;
    for (i = 0; i < sizeof c->password; i++) p[i] = 0;
}

static struct smb2_context *ctx_new(void) {
    struct smb2_context *ctx;
    pthread_mutex_lock(&g_lib_mu);
    ctx = smb2_init_context();
    pthread_mutex_unlock(&g_lib_mu);
    return ctx;
}

static void ctx_free(struct smb2_context *ctx) {
    if (!ctx) return;
    pthread_mutex_lock(&g_lib_mu);
    smb2_destroy_context(ctx);
    pthread_mutex_unlock(&g_lib_mu);
}

/* What the server answered, when it answered with a refusal (0: no such answer). */
static int map_nt(uint32_t nt) {
    switch (nt) {
    case SMB2_STATUS_OBJECT_NAME_NOT_FOUND:
    case SMB2_STATUS_OBJECT_PATH_NOT_FOUND:
    case SMB2_STATUS_NO_SUCH_FILE: return XE_NOT_FOUND;
    case SMB2_STATUS_ACCESS_DENIED:
    case SMB2_STATUS_NETWORK_ACCESS_DENIED:
    case SMB2_STATUS_CANNOT_DELETE:
    case SMB2_STATUS_SHARING_VIOLATION:
    case SMB2_STATUS_MEDIA_WRITE_PROTECTED: return XE_ACCESS;
    case SMB2_STATUS_OBJECT_NAME_COLLISION:
    case SMB2_STATUS_DIRECTORY_NOT_EMPTY: return XE_EXISTS;
    case SMB2_STATUS_DISK_FULL: return XE_NO_SPACE;
    case SMB2_STATUS_OBJECT_NAME_INVALID: return XE_BAD_NAME;
    case SMB2_STATUS_NOT_A_DIRECTORY: return XE_NOT_DIR;
    case SMB2_STATUS_FILE_IS_A_DIRECTORY: return XE_IS_DIR;
    case SMB2_STATUS_NOT_SUPPORTED:
    case SMB2_STATUS_INVALID_DEVICE_REQUEST: return XE_NOT_SUPPORTED;
    case SMB2_STATUS_IO_TIMEOUT: return XE_TIMEOUT;
    case SMB2_STATUS_NETWORK_NAME_DELETED:
    case SMB2_STATUS_USER_SESSION_DELETED:
    case SMB2_STATUS_CONNECTION_DISCONNECTED:
    case SMB2_STATUS_CONNECTION_RESET: return XE_DISCONNECTED;
    default: return 0;
    }
}

/* A compound call (mkdir, unlink, rmdir) whose CREATE the server refused names the status only in
 * libsmb2's error text ("Create failed with status STATUS_OBJECT_NAME_COLLISION.") and returns the
 * CLOSE's ENETRESET: read the text (cleared before the call, so it is this call's). 0 = no match. */
static int map_text(const char *t) {
    if (!t || !t[0]) return 0;
    if (strstr(t, "OBJECT_NAME_COLLISION") || strstr(t, "DIRECTORY_NOT_EMPTY")) return XE_EXISTS;
    if (strstr(t, "OBJECT_NAME_NOT_FOUND") || strstr(t, "OBJECT_PATH_NOT_FOUND") || strstr(t, "NO_SUCH_FILE"))
        return XE_NOT_FOUND;
    if (strstr(t, "ACCESS_DENIED") || strstr(t, "SHARING_VIOLATION") || strstr(t, "CANNOT_DELETE")) return XE_ACCESS;
    if (strstr(t, "NOT_SUPPORTED")) return XE_NOT_SUPPORTED;
    if (strstr(t, "DISK_FULL")) return XE_NO_SPACE;
    if (strstr(t, "OBJECT_NAME_INVALID")) return XE_BAD_NAME;
    if (strstr(t, "NOT_A_DIRECTORY")) return XE_NOT_DIR;
    if (strstr(t, "FILE_IS_A_DIRECTORY")) return XE_IS_DIR;
    return 0;
}

/* A failed call's result as an xerr_t: the connection's state first (a dropped socket is
 * "disconnected" whatever errno the call picked), then the server's NT status and error text (both
 * cleared before the call: FRESH), then the errno libsmb2 mapped it to. */
static int map_rc(struct smb2_context *ctx, int rc) {
    int e = rc < 0 ? -rc : rc, byst;
    uint32_t nt = ctx ? (uint32_t)smb2_get_nterror(ctx) : 0;
    if (ctx && smb2_get_fd(ctx) < 0) return XE_DISCONNECTED;
    if (rc == -1) return XE_DISCONNECTED; /* wait_for_reply: poll failed / no connection */
    if (nt && (byst = map_nt(nt)) != 0) return byst;
    if (ctx && (byst = map_text(smb2_get_error(ctx))) != 0) return byst;
    switch (e) {
    case ENOENT: return XE_NOT_FOUND;
    case EACCES:
    case EPERM: return XE_ACCESS;
    case ENOSPC: return XE_NO_SPACE;
    case EEXIST: return XE_EXISTS;
    case ENOTDIR: return XE_NOT_DIR;
    case EISDIR: return XE_IS_DIR;
    case ENOMEM: return XE_NOMEM;
    case ETIMEDOUT: return XE_TIMEOUT;
    case ENETRESET:
    case EPIPE:
    case ECONNRESET:
    case ENOTCONN:
    case ECONNABORTED: return XE_DISCONNECTED;
    case EINVAL:
        if (nt == SMB2_STATUS_OBJECT_NAME_INVALID) return XE_BAD_NAME;
        return XE_IO;
    default:
        if (nt == SMB2_STATUS_OBJECT_NAME_INVALID) return XE_BAD_NAME;
        return XE_IO;
    }
}

/* A failed connect: the server's answer when it gave one, else "nothing answered". */
static int map_connect(struct smb2_context *ctx, int rc) {
    uint32_t nt = (uint32_t)smb2_get_nterror(ctx);
    switch (nt) {
    case SMB2_STATUS_LOGON_FAILURE:
    case SMB2_STATUS_WRONG_PASSWORD:
    case SMB2_STATUS_ACCOUNT_RESTRICTION:
    case SMB2_STATUS_ACCOUNT_DISABLED:
    case SMB2_STATUS_ACCOUNT_EXPIRED:
    case SMB2_STATUS_ACCOUNT_LOCKED_OUT:
    case SMB2_STATUS_PASSWORD_EXPIRED:
    case SMB2_STATUS_PASSWORD_MUST_CHANGE:
    case SMB2_STATUS_PASSWORD_RESTRICTION:
        return XE_AUTH;
    case SMB2_STATUS_BAD_NETWORK_NAME: return XE_NO_SHARE;
    case SMB2_STATUS_ACCESS_DENIED:
    case SMB2_STATUS_NETWORK_ACCESS_DENIED: return XE_ACCESS;
    case SMB2_STATUS_IO_TIMEOUT: return XE_TIMEOUT;
    default: break;
    }
    if (rc == -ETIMEDOUT) return XE_TIMEOUT;
    return XE_UNREACHABLE;
}

static void set_detail(char *detail, size_t cap, struct smb2_context *ctx) {
    const char *m;
    if (!detail || !cap) return;
    m = ctx ? smb2_get_error(ctx) : "";
    snprintf(detail, cap, "%s", m ? m : "");
}

/* One sign-in attempt. anonymous = 1: a NULL password (an anonymous NTLM session). */
static struct smb2_context *try_connect(const smbconn_t *c, const char *share, int anonymous, int *err, char *detail,
                                        size_t detail_cap) {
    char server[SMBP_HOST_MAX + 16];
    const char *user = c->user[0] ? c->user : "Guest";
    struct smb2_context *ctx;
    int rc;
    if (smbp_server_arg(c->host, c->port ? c->port : 445, server, sizeof server) != 0) {
        *err = XE_BAD_REQUEST;
        return NULL;
    }
    ctx = ctx_new();
    if (!ctx) {
        *err = XE_NOMEM;
        return NULL;
    }
    smb2_set_timeout(ctx, c->timeout_s > 0 ? c->timeout_s : 10);
    smb2_set_version(ctx, SMB2_VERSION_ANY);
    smb2_set_security_mode(ctx, SMB2_NEGOTIATE_SIGNING_ENABLED);
    smb2_set_user(ctx, user);
    smb2_set_password(ctx, anonymous ? NULL : c->password);
    if (c->domain[0]) smb2_set_domain(ctx, c->domain);
    rc = smb2_connect_share(ctx, server, share, user);
    if (rc < 0) {
        *err = map_connect(ctx, rc);
        set_detail(detail, detail_cap, ctx);
        ctx_free(ctx);
        return NULL;
    }
    *err = 0;
    return ctx;
}

/* Sign in: the password given (empty for a guest); with an empty one, an anonymous session next. */
static struct smb2_context *connect_share(const smbconn_t *c, const char *share, int *err, char *detail,
                                          size_t detail_cap) {
    struct smb2_context *ctx = try_connect(c, share, 0, err, detail, detail_cap);
    if (!ctx && !c->password[0] && (*err == XE_AUTH || *err == XE_ACCESS))
        ctx = try_connect(c, share, 1, err, detail, detail_cap);
    return ctx;
}

/* ---- ops ---- */

#define SFS(fs) ((smbfs_t *)(fs)->impl)

static int s_stat(xfs_t *fs, const char *path, xstat_t *st) {
    struct smb2_stat_64 s;
    int rc;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    memset(&s, 0, sizeof s);
    FRESH(SFS(fs)->ctx);
    rc = smb2_stat(SFS(fs)->ctx, path, &s);
    if (rc < 0) return map_rc(SFS(fs)->ctx, rc);
    st->kind = s.smb2_type == SMB2_TYPE_DIRECTORY ? XK_DIR : s.smb2_type == SMB2_TYPE_FILE ? XK_FILE : XK_OTHER;
    st->size = s.smb2_size;
    st->mtime = s.smb2_mtime;
    return 0;
}

static int s_opendir(xfs_t *fs, const char *path, void **dir) {
    sdir_t *d;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    d = calloc(1, sizeof *d);
    if (!d) return XE_NOMEM;
    FRESH(SFS(fs)->ctx);
    d->dir = smb2_opendir(SFS(fs)->ctx, path);
    if (!d->dir) {
        int e = map_rc(SFS(fs)->ctx, -EIO);
        free(d);
        return e;
    }
    *dir = d;
    return 0;
}

static int s_readdir(xfs_t *fs, void *dir, char *name, size_t cap, xstat_t *st) {
    sdir_t *d = dir;
    struct smb2dirent *ent;
    for (;;) {
        ent = smb2_readdir(SFS(fs)->ctx, d->dir);
        if (!ent) return 0;
        if (!ent->name || strcmp(ent->name, ".") == 0 || strcmp(ent->name, "..") == 0) continue;
        /* Longer than a name here holds (255 UTF-8 bytes; a long name in CJK or emoji passes that
         * well under the share's 255 characters): an error for this entry, so a folder copy fails
         * and names the folder instead of reporting done without it. The next call goes on. */
        if (strlen(ent->name) >= cap) return XE_BAD_NAME;
        /* A name the server should never send: a separator or a control byte in it would be joined
         * into a console path as-is (copy_dir), so "../x" from a hostile or broken server could
         * leave the approved destination. Left out. */
        if (strpbrk(ent->name, "/\\") != NULL) continue;
        {
            const unsigned char *q = (const unsigned char *)ent->name;
            int bad = 0;
            for (; *q; q++)
                if (*q < 0x20 || *q == 0x7f) bad = 1;
            if (bad) continue;
        }
        strcpy(name, ent->name);
        st->kind = ent->st.smb2_type == SMB2_TYPE_DIRECTORY ? XK_DIR
                   : ent->st.smb2_type == SMB2_TYPE_FILE    ? XK_FILE
                                                            : XK_OTHER;
        st->size = ent->st.smb2_size;
        st->mtime = ent->st.smb2_mtime;
        return 1;
    }
}

static void s_closedir(xfs_t *fs, void *dir) {
    sdir_t *d = dir;
    if (!d) return;
    if (d->dir && SFS(fs)->ctx) smb2_closedir(SFS(fs)->ctx, d->dir);
    free(d);
}

static int s_mkdir(xfs_t *fs, const char *path) {
    int rc;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    FRESH(SFS(fs)->ctx);
    rc = smb2_mkdir(SFS(fs)->ctx, path);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : 0;
}

static int s_open(xfs_t *fs, const char *path, int flags, void **fh) {
    struct smb2fh *h;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    FRESH(SFS(fs)->ctx);
    h = smb2_open(SFS(fs)->ctx, path, flags);
    if (!h) return map_rc(SFS(fs)->ctx, -EIO);
    *fh = h;
    return 0;
}

static int s_open_read(xfs_t *fs, const char *path, void **fh) { return s_open(fs, path, O_RDONLY, fh); }

static int s_open_write(xfs_t *fs, const char *path, int resume, void **fh) {
    return s_open(fs, path, O_WRONLY | O_CREAT | (resume ? 0 : O_TRUNC), fh);
}

static long long s_pread(xfs_t *fs, void *fh, void *buf, size_t n, uint64_t off) {
    int rc;
    uint32_t max;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    max = smb2_get_max_read_size(SFS(fs)->ctx);
    if (max && n > max) n = max;
    FRESH(SFS(fs)->ctx);
    rc = smb2_pread(SFS(fs)->ctx, fh, buf, (uint32_t)n, off);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : rc;
}

static long long s_pwrite(xfs_t *fs, void *fh, const void *buf, size_t n, uint64_t off) {
    int rc;
    uint32_t max;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    max = smb2_get_max_write_size(SFS(fs)->ctx);
    if (max && n > max) n = max;
    FRESH(SFS(fs)->ctx);
    rc = smb2_pwrite(SFS(fs)->ctx, fh, (const uint8_t *)buf, (uint32_t)n, off);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : rc;
}

static int s_close(xfs_t *fs, void *fh) {
    int rc;
    if (!fh) return 0;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    FRESH(SFS(fs)->ctx);
    rc = smb2_close(SFS(fs)->ctx, fh);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : 0;
}

static int s_unlink(xfs_t *fs, const char *path) {
    int rc;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    FRESH(SFS(fs)->ctx);
    rc = smb2_unlink(SFS(fs)->ctx, path);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : 0;
}

static int s_rmdir(xfs_t *fs, const char *path) {
    int rc;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    FRESH(SFS(fs)->ctx);
    rc = smb2_rmdir(SFS(fs)->ctx, path);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : 0;
}

static int s_rename_raw(xfs_t *fs, const char *from, const char *to) {
    int rc;
    FRESH(SFS(fs)->ctx);
    rc = smb2_rename(SFS(fs)->ctx, from, to);
    return rc < 0 ? map_rc(SFS(fs)->ctx, rc) : 0;
}

/* SMB2 SET_INFO rename refuses an existing target unless asked otherwise, and libsmb2 never asks:
 * replace = move the old file aside first ("<to>.ps5u-old"), rename the new one into place, then
 * drop the old one. Unlinking the old file first and then failing the rename (a server that refuses
 * rename, a drop) left the share with neither version; now the old one is put back. */
static int s_rename(xfs_t *fs, const char *from, const char *to, int replace) {
    char aside[SMBP_PATH_MAX];
    int rc, kept = 0;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    if (replace) {
        /* A free aside name: a file already called "<to>.ps5u-old" may be the person's own, or the
         * only copy left by an interrupted replace, so it is never deleted; the next number is used. */
        xstat_t ast;
        int n;
        for (n = 0; n < 16; n++) {
            int w = n == 0 ? snprintf(aside, sizeof aside, "%s.ps5u-old", to)
                           : snprintf(aside, sizeof aside, "%s.ps5u-old%d", to, n);
            if (w < 0 || w >= (int)sizeof aside) return XE_BAD_NAME;
            rc = s_stat(fs, aside, &ast);
            if (rc == XE_NOT_FOUND) break;
            if (rc != 0) return rc;
        }
        if (n == 16) return XE_EXISTS;
        rc = s_rename_raw(fs, to, aside);
        if (rc == 0) kept = 1;
        else if (rc != XE_NOT_FOUND) return rc; /* `to` is still there, untouched */
    }
    rc = s_rename_raw(fs, from, to);
    if (rc != 0) {
        if (kept) (void)s_rename_raw(fs, aside, to); /* the old version back in place, best effort */
        return rc;
    }
    if (kept) (void)s_unlink(fs, aside);
    return 0;
}

static int s_free_space(xfs_t *fs, const char *path, uint64_t *out) {
    struct smb2_statvfs v;
    uint64_t bs;
    int rc;
    if (!SFS(fs)->ctx) return XE_DISCONNECTED;
    memset(&v, 0, sizeof v);
    FRESH(SFS(fs)->ctx);
    rc = smb2_statvfs(SFS(fs)->ctx, path, &v);
    if (rc < 0) return map_rc(SFS(fs)->ctx, rc);
    bs = v.f_frsize ? v.f_frsize : v.f_bsize;
    if (!bs) return XE_IO;
    *out = v.f_bavail * bs;
    return 0;
}

static int s_reconnect(xfs_t *fs) {
    smbfs_t *s = SFS(fs);
    int err = 0;
    ctx_free(s->ctx);
    s->ctx = connect_share(&s->conn, s->conn.share, &err, NULL, 0);
    return s->ctx ? 0 : err;
}

static size_t s_chunk(xfs_t *fs) {
    smbfs_t *s = SFS(fs);
    uint32_t r, w;
    if (!s->ctx) return 0;
    r = smb2_get_max_read_size(s->ctx);
    w = smb2_get_max_write_size(s->ctx);
    if (r && w) return r < w ? r : w;
    return r ? r : w;
}

static const xfs_ops_t k_smb_ops = {
    s_stat, s_opendir, s_readdir, s_closedir, s_mkdir, s_open_read, s_open_write, s_pread, s_pwrite, s_close,
    s_rename, s_unlink, s_rmdir, s_free_space, s_reconnect, s_chunk,
};

int smbfs_open(xfs_t *fs, const smbconn_t *c, char *detail, size_t detail_cap) {
    smbfs_t *s = calloc(1, sizeof *s);
    int err = 0;
    if (detail && detail_cap) detail[0] = '\0';
    if (!s) return XE_NOMEM;
    s->conn = *c;
    s->ctx = connect_share(&s->conn, s->conn.share, &err, detail, detail_cap);
    if (!s->ctx) {
        smbconn_clear(&s->conn);
        free(s);
        return err ? err : XE_UNREACHABLE;
    }
    fs->ops = &k_smb_ops;
    fs->impl = s;
    return 0;
}

void smbfs_close(xfs_t *fs) {
    smbfs_t *s = fs->impl ? SFS(fs) : NULL;
    if (!s) return;
    if (s->ctx) {
        (void)smb2_disconnect_share(s->ctx);
        ctx_free(s->ctx);
    }
    smbconn_clear(&s->conn);
    free(s);
    fs->impl = NULL;
}

unsigned smbfs_dialect(xfs_t *fs) {
    smbfs_t *s = fs->impl ? SFS(fs) : NULL;
    return s && s->ctx ? smb2_get_dialect(s->ctx) : 0;
}

int smbfs_list_shares(const smbconn_t *c, smb_share_info_t *out, int max, char *detail, size_t detail_cap) {
    struct smb2_context *ctx;
    struct smb2_share_enum_reply *rep;
    int err = 0, n = 0;
    uint32_t i;
    if (detail && detail_cap) detail[0] = '\0';
    ctx = connect_share(c, "IPC$", &err, detail, detail_cap);
    if (!ctx) return err ? err : XE_UNREACHABLE;
    rep = smb2_share_enum_sync(ctx, SMB2_SHARE_INFO_1);
    if (!rep) {
        set_detail(detail, detail_cap, ctx);
        err = map_rc(ctx, -EIO);
        if ((uint32_t)smb2_get_nterror(ctx) == SMB2_STATUS_ACCESS_DENIED) err = XE_ACCESS;
        ctx_free(ctx);
        return err;
    }
    for (i = 0; i < rep->entries_read && n < max; i++) {
        struct smb2_share_info_1 *s = &rep->share_info.info_1[i];
        size_t len;
        if (!s->netname || (s->type & 3) != SMB2_SHARE_TYPE_DISKTREE) continue;
        if (s->type & SMB2_SHARE_TYPE_HIDDEN) continue;
        len = strlen(s->netname);
        if (len == 0 || len >= sizeof out[n].name || s->netname[len - 1] == '$') continue;
        memcpy(out[n].name, s->netname, len + 1);
        snprintf(out[n].remark, sizeof out[n].remark, "%s", s->remark ? s->remark : "");
        n++;
    }
    smb2_free_data(ctx, rep);
    (void)smb2_disconnect_share(ctx);
    ctx_free(ctx);
    return n;
}
