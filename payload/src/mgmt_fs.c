/* AVA1 filesystem management methods: see include/mgmt_fs.h. */
#include "mgmt_fs.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/types.h>
#include <unistd.h>

#include "ava1_gen.h"
#include "ava1_wire.h"
#include "cross_device.h"
#include "path_policy.h"

#define FS_PATH_MAX 1024u
/* fs.list: entries per call (FTX2's ceiling) and the room an entry list may take in a reply. */
#define FS_LIST_MAX 256u
#define FS_LIST_BLOB_MAX (192u * 1024u)
#define FS_DEFAULT_FILE_MODE 0644u

static mgmt_fs_policy_t P;

void mgmt_fs_set_policy(const mgmt_fs_policy_t *p) {
    if (p) P = *p;
    else memset(&P, 0, sizeof P);
}

static int write_ok(const char *path) { return P.write_allowed && P.write_allowed(path); }
static int read_ok(const char *path, int unsafe_read) { return P.read_allowed && P.read_allowed(path, unsafe_read); }
static void counted(void) {
    if (P.count) P.count();
}

/* Copies a wire path into a NUL-terminated buffer. 0, or an error status (the cause is written). */
static int take_path(mgmt_ctx_t *cx, const uint8_t *p, uint16_t n, char *out, const char *too_long) {
    if (n >= FS_PATH_MAX) return mgmt_reply_error(cx, AVA1_ERR_PATH, too_long);
    if (n && memchr(p, 0, n)) return mgmt_reply_error(cx, AVA1_ERR_PATH, too_long);
    if (n) memcpy(out, p, n);
    out[n] = '\0';
    return AVA1_STATUS_OK;
}

/* A path component that is "." or ".." (a substring such as "My..Game" is fine). */
static int has_dotdot_component(const char *p) {
    const char *seg = p;
    while (*seg) {
        const char *end;
        size_t len;
        if (*seg == '/') {
            seg++;
            continue;
        }
        end = seg;
        while (*end && *end != '/') end++;
        len = (size_t)(end - seg);
        if (len == 1 && seg[0] == '.') return 1;
        if (len == 2 && seg[0] == '.' && seg[1] == '.') return 1;
        seg = end;
    }
    return 0;
}

static uint8_t kind_of(mode_t m) {
    if (S_ISDIR(m)) return (uint8_t)AVA1_ENTRY_DIR;
    if (S_ISREG(m)) return (uint8_t)AVA1_ENTRY_FILE;
    if (S_ISLNK(m)) return (uint8_t)AVA1_ENTRY_LINK;
    return (uint8_t)AVA1_ENTRY_OTHER;
}

static int reply_empty(mgmt_ctx_t *cx) {
    cx->out_len = 0;
    return AVA1_STATUS_OK;
}

static int errno_error(mgmt_ctx_t *cx, const char *prefix, int err) {
    char cause[96];
    snprintf(cause, sizeof cause, "%s_errno_%d", prefix, err);
    return mgmt_reply_error(cx, AVA1_ERR_IO, cause);
}

/* ---- fs.list ---- */

int mgmt_run_fs_list(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_list_t q;
    ava1_fs_list_result_t res;
    ava1_w_t blob, out;
    char path[FS_PATH_MAX], full[FS_PATH_MAX + 260];
    uint8_t *buf;
    DIR *dir;
    struct dirent *ent;
    uint64_t idx = 0, emitted = 0, limit;
    int rc, more = 0;

    if (ava1_fs_list_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsList request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_list_dir_bad_path")) != AVA1_STATUS_OK) return rc;
    limit = q.limit == 0 || q.limit > FS_LIST_MAX ? FS_LIST_MAX : q.limit;
    if (path[0] != '/') return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_list_dir_bad_path");
    /* Component-scoped: a bare strstr("..") locked users out of directories like `..cache`. */
    if (has_dotdot_component(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_list_dir_path_denied");
    dir = opendir(path);
    if (!dir) return errno_error(cx, "fs_list_dir_opendir", errno);
    buf = malloc(FS_LIST_BLOB_MAX);
    if (!buf) {
        closedir(dir);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "fs_list_dir_oom");
    }
    ava1_w_init(&blob, buf, FS_LIST_BLOB_MAX);
    while ((ent = readdir(dir)) != NULL) {
        const char *name = ent->d_name;
        struct stat st;
        ava1_fs_entry_t e;
        char clean[260];
        size_t nl = strlen(name), at;
        int stat_ok;
        if (name[0] == '.' && (name[1] == '\0' || (name[1] == '.' && name[2] == '\0'))) continue;
        if (idx < q.offset) {
            idx++;
            continue;
        }
        if (emitted >= limit) {
            more = 1;
            break;
        }
        if (snprintf(full, sizeof full, "%s/%s", path, name) >= (int)sizeof path) {
            /* Skipped without counting, so the caller's offset arithmetic still lines up
             * (the FTX2 handler's rule: a page of such names must not look like the end). */
            continue;
        }
        stat_ok = lstat(full, &st) == 0;
        memset(&e, 0, sizeof e);
        /* The wire name is UTF-8 text; a name that is not valid is shown with '?' for its high bytes. */
        if (nl >= sizeof clean) nl = sizeof clean - 1;
        memcpy(clean, name, nl);
        clean[nl] = '\0';
        if (!ava1_utf8_valid((const uint8_t *)clean, nl)) {
            size_t i;
            for (i = 0; i < nl; i++)
                if ((unsigned char)clean[i] >= 0x80) clean[i] = '?';
        }
        {
            size_t i;
            for (i = 0; i < nl; i++)
                if ((unsigned char)clean[i] < 0x20) clean[i] = '?';
        }
        e.name = (const uint8_t *)clean;
        e.name_len = (uint16_t)nl;
        if (stat_ok) {
            e.kind = kind_of(st.st_mode);
            e.size = S_ISREG(st.st_mode) ? (uint64_t)st.st_size : 0;
            e.has_mtime = 1;
            e.mtime = (uint64_t)(st.st_mtime > 0 ? st.st_mtime : 0);
            e.has_mode = 1;
            e.mode = (uint32_t)(st.st_mode & 07777);
        } else {
            e.kind = (uint8_t)AVA1_ENTRY_UNKNOWN; /* a cross-mount symlink or a vanished entry */
        }
        at = blob.len;
        if (ava1_fs_entry_append(&blob, &e) != 0) {
            /* The blob is full: the rest of the page is the next call's. Undo the partial entry. */
            blob.len = at;
            blob.err = 0;
            more = 1;
            break;
        }
        emitted++;
        idx++;
    }
    closedir(dir);
    memset(&res, 0, sizeof res);
    res.entries = buf;
    res.entries_len = (uint32_t)blob.len;
    res.total_scanned = idx > 0xffffffffu ? 0xffffffffu : (uint32_t)idx;
    res.more = (uint8_t)more;
    ava1_w_init(&out, cx->out, cx->cap);
    rc = ava1_fs_list_result_encode(&res, &out);
    free(buf);
    if (rc != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    cx->out_len = out.len;
    counted();
    return AVA1_STATUS_OK;
}

/* ---- fs.stat ---- */

int mgmt_run_fs_stat(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_path_t q;
    ava1_fs_stat_t s;
    ava1_w_t out;
    struct stat st;
    char path[FS_PATH_MAX];
    int rc;
    if (ava1_fs_path_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsPath request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_stat_bad_path")) != AVA1_STATUS_OK) return rc;
    /* The same policy as fs.list: metadata of any absolute path without a `..` component. */
    if (path[0] != '/') return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_stat_bad_path");
    if (has_dotdot_component(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_stat_path_denied");
    /* Follows a link (an existence probe asks about the target); a dangling link is reported as a link. */
    if (stat(path, &st) != 0 && lstat(path, &st) != 0) return errno_error(cx, "fs_stat_failed", errno);
    memset(&s, 0, sizeof s);
    s.kind = kind_of(st.st_mode);
    s.size = S_ISREG(st.st_mode) ? (uint64_t)st.st_size : 0;
    s.mtime = (uint64_t)(st.st_mtime > 0 ? st.st_mtime : 0);
    s.mode = (uint32_t)(st.st_mode & 07777);
    s.dev = (uint64_t)st.st_dev;
    ava1_w_init(&out, cx->out, cx->cap);
    if (ava1_fs_stat_encode(&s, &out) != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    cx->out_len = out.len;
    return AVA1_STATUS_OK;
}

/* ---- fs.freespace ---- */

/* The working margin kept back from a write on a drive: 1/64th of it, at most 1 GiB. The same rule as
 * runtime.c's capacity_reserve_for_mount (the console's own capacity gate) and the engine's
 * Volume::safety_reserve_bytes: small enough never to block an upload that fits, and nothing speculative
 * (the console's hidden allocator pool is not modelled; the memory admit budget is not disk). */
#define FS_RESERVE_CAP (1024ull * 1024ull * 1024ull)

uint64_t mgmt_fs_reserve_for(uint64_t total_bytes) {
    uint64_t scaled = total_bytes / 64u;
    return scaled < FS_RESERVE_CAP ? scaled : FS_RESERVE_CAP;
}

int mgmt_run_fs_freespace(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_path_t q;
    ava1_fs_free_space_t r;
    ava1_w_t out;
    struct statvfs vfs;
    struct stat st;
    char path[FS_PATH_MAX];
    uint64_t bs, free_bytes;
    int rc;
    if (ava1_fs_path_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsPath request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_freespace_bad_path")) != AVA1_STATUS_OK) return rc;
    if (path[0] != '/') return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_freespace_bad_path");
    if (has_dotdot_component(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_freespace_path_denied");
    /* A destination usually does not exist yet: ask the nearest ancestor that does, never leaving "/". */
    for (;;) {
        char *slash;
        if (statvfs(path, &vfs) == 0 && stat(path, &st) == 0) break;
        slash = strrchr(path, '/');
        if (!slash || slash == path) {
            if (path[1] == '\0') return errno_error(cx, "fs_freespace_failed", errno);
            path[1] = '\0';
            continue;
        }
        *slash = '\0';
    }
    bs = (uint64_t)(vfs.f_frsize ? vfs.f_frsize : vfs.f_bsize);
    free_bytes = (uint64_t)vfs.f_bavail * bs; /* what an unprivileged writer can take */
    memset(&r, 0, sizeof r);
    r.total = (uint64_t)vfs.f_blocks * bs;
    r.free = free_bytes;
    r.reserve = mgmt_fs_reserve_for(r.total);
    r.usable = free_bytes > r.reserve ? free_bytes - r.reserve : 0;
    r.dev = (uint64_t)st.st_dev;
    ava1_w_init(&out, cx->out, cx->cap);
    if (ava1_fs_free_space_encode(&r, &out) != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    cx->out_len = out.len;
    return AVA1_STATUS_OK;
}

/* ---- fs.mkdir ---- */

/* mkdir -p of every ancestor of `path` (not `path`). Intermediate directories get 0777 as FTX2's did. */
static int make_parents(const char *path) {
    char tmp[FS_PATH_MAX];
    size_t len = strlen(path), i;
    if (len >= sizeof tmp) return -1;
    memcpy(tmp, path, len + 1);
    for (i = 1; i < len; i++) {
        if (tmp[i] != '/') continue;
        tmp[i] = '\0';
        if (mkdir(tmp, 0777) != 0 && errno != EEXIST) return -1;
        tmp[i] = '/';
    }
    return 0;
}

int mgmt_run_fs_mkdir(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_mkdir_t q;
    struct stat st;
    char path[FS_PATH_MAX];
    size_t len;
    int rc, created;
    if (ava1_fs_mkdir_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsMkdir request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_mkdir_path_not_allowed")) != AVA1_STATUS_OK) return rc;
    if (!write_ok(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_mkdir_path_not_allowed");
    /* A trailing slash names the same directory. */
    len = strlen(path);
    while (len > 1 && path[len - 1] == '/') path[--len] = '\0';
    if (q.parents && make_parents(path) != 0) return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_mkdir_parents_failed");
    created = mkdir(path, (mode_t)(q.mode & 07777)) == 0;
    if (!created) {
        if (errno != EEXIST) return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_mkdir_failed");
        /* It exists: fine for a directory (mkdir -p), an error for anything else. */
        if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return mgmt_reply_error(cx, AVA1_ERR_EXISTS, "fs_mkdir_exists_not_dir");
    } else if (chmod(path, (mode_t)(q.mode & 07777)) != 0) {
        /* mkdir(2) applies the umask; the caller asked for these bits. */
        return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_mkdir_chmod_failed");
    }
    counted();
    return reply_empty(cx);
}

/* ---- fs.rename ---- */

int mgmt_run_fs_rename(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_rename_t q;
    char from[FS_PATH_MAX], to[FS_PATH_MAX];
    struct stat st;
    int rc;
    xdev_dev_fn dev = P.dev_of ? P.dev_of : xdev_stat_dev;
    xdev_dev_fn src_dev = P.src_dev_of ? P.src_dev_of : xdev_lstat_dev;
    if (ava1_fs_rename_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsRename request");
    if ((rc = take_path(cx, q.from, q.from_len, from, "fs_move_path_not_allowed")) != AVA1_STATUS_OK) return rc;
    if ((rc = take_path(cx, q.to, q.to_len, to, "fs_move_path_not_allowed")) != AVA1_STATUS_OK) return rc;
    if (!write_ok(from) || !write_ok(to)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_move_path_not_allowed");
    /* Moving a directory that CONTAINS the trust store moves the store with it. */
    if (path_tree_op_refused(from) || path_tree_op_refused(to)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_move_path_not_allowed");
    /* NEVER rename(2) across devices: on this kernel it does not fail with EXDEV, it panics the
     * console. Compare the source's OWN device (lstat: a link is judged by where it lives, not by
     * its target) with the destination's parent before any rename. A device that cannot be read
     * (missing source or directory) is refused too: only a definite SAME reaches rename(). */
    xdev_result_t xr = xdev_rename_crosses_l(from, to, src_dev, dev);
    if (xr == XDEV_CROSSES) return mgmt_reply_error(cx, AVA1_ERR_CROSS_DEVICE, "fs_move_cross_mount");
    if (!xdev_rename_is_safe(xr)) return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_move_device_unknown"); /* fail closed (review 007 #4) */
    if (!q.overwrite && lstat(to, &st) == 0) return mgmt_reply_error(cx, AVA1_ERR_EXISTS, "fs_move_exists");
    if (rename(from, to) != 0) {
        if (errno == EXDEV) return mgmt_reply_error(cx, AVA1_ERR_CROSS_DEVICE, "fs_move_cross_mount");
        return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_move_failed");
    }
    counted();
    return reply_empty(cx);
}

/* ---- fs.chmod ---- */

int mgmt_run_fs_chmod(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_chmod_t q;
    char path[FS_PATH_MAX];
    int rc;
    if (ava1_fs_chmod_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsChmod request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_chmod_path_not_allowed")) != AVA1_STATUS_OK) return rc;
    if (!write_ok(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_chmod_path_not_allowed");
    /* Bits above 07777 are not permissions: clamp as FTX2 did. */
    if (chmod(path, (mode_t)(q.mode > 07777 ? 07777 : q.mode)) != 0) return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_chmod_failed");
    counted();
    return reply_empty(cx);
}

/* ---- fs.read ---- */

int mgmt_run_fs_read(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_read_t q;
    ava1_fs_read_result_t res;
    ava1_w_t out;
    char path[FS_PATH_MAX];
    struct stat st;
    uint8_t *buf;
    uint64_t want;
    size_t got = 0;
    int fd, rc, unsafe_read;
    if (ava1_fs_read_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsRead request");
    if ((rc = take_path(cx, q.path, q.path_len, path, "fs_read_path_not_allowed")) != AVA1_STATUS_OK) return rc;
    unsafe_read = (q.flags & AVA1_FSR_UNSAFE) != 0;
    if (!read_ok(path, unsafe_read)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "fs_read_path_not_allowed");
    /* The reply is `data + 7` bytes; a longer ask is a short read (eof = 0), never an error. */
    want = q.len > AVA1_FS_READ_MAX ? AVA1_FS_READ_MAX : q.len;
    if (want + 7 > cx->cap) want = cx->cap > 7 ? cx->cap - 7 : 0;
    fd = open(path, O_RDONLY | O_NONBLOCK); /* a FIFO must not block a worker; fstat below refuses it */
    if (fd < 0) {
        int e = errno;
        /* The FTX2 handler said stat_failed for a missing file and open_failed for the rest. */
        if (e == ENOENT || e == ENOTDIR) return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_read_stat_failed");
        return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_read_open_failed");
    }
    if (fstat(fd, &st) != 0) {
        close(fd);
        return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_read_stat_failed");
    }
    if (!S_ISREG(st.st_mode)) {
        close(fd);
        return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_read_not_regular_file");
    }
    if ((uint64_t)st.st_size <= q.offset) want = 0; /* at or past the end: an empty eof reply */
    else if ((uint64_t)st.st_size - q.offset < want) want = (uint64_t)st.st_size - q.offset;
    buf = malloc(want ? (size_t)want : 1);
    if (!buf) {
        close(fd);
        return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, "fs_read_oom");
    }
    /* read(2) can return less than asked for a regular file (a page-cache boundary): loop to the
     * ask or the end, so a short reply always means eof. */
    while (got < (size_t)want) {
        ssize_t r = pread(fd, buf + got, (size_t)want - got, (off_t)(q.offset + got));
        if (r < 0) {
            if (errno == EINTR) continue;
            close(fd);
            free(buf);
            return mgmt_reply_error(cx, AVA1_ERR_IO, "fs_read_read_failed");
        }
        if (r == 0) break;
        got += (size_t)r;
    }
    close(fd);
    memset(&res, 0, sizeof res);
    res.data = buf;
    res.data_len = (uint32_t)got;
    /* eof: the reply reaches the end of the file as it was at fstat. */
    res.eof = (uint8_t)(q.offset + got >= (uint64_t)st.st_size);
    ava1_w_init(&out, cx->out, cx->cap);
    rc = ava1_fs_read_result_encode(&res, &out);
    free(buf);
    if (rc != 0) return mgmt_reply_error(cx, AVA1_ERR_INTERNAL, MGMT_ERR_TRUNCATED);
    cx->out_len = out.len;
    counted();
    return AVA1_STATUS_OK;
}

/* ---- fs.write ---- */

static int write_all(int fd, const uint8_t *p, size_t n, uint64_t at, int use_offset) {
    size_t done = 0;
    while (done < n) {
        ssize_t w = use_offset ? pwrite(fd, p + done, n - done, (off_t)(at + done)) : write(fd, p + done, n - done);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        done += (size_t)w;
    }
    return 0;
}

int mgmt_run_fs_write(const uint8_t *req, uint32_t n, mgmt_ctx_t *cx) {
    ava1_fs_write_t q;
    char path[FS_PATH_MAX], tmp[FS_PATH_MAX + sizeof MGMT_FS_TMP_SUFFIX];
    struct stat st;
    uint32_t f;
    int rc, fd, whole, at_off, append, commit, oflags;
    mode_t mode;
    if (ava1_fs_write_decode(req, n, &q) != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "bad FsWrite request");
    if (q.path_len == 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "path_required");
    if ((rc = take_path(cx, q.path, q.path_len, path, "path_unsafe")) != AVA1_STATUS_OK) return rc;
    f = q.flags;
    append = (f & AVA1_FSW_APPEND) != 0;
    at_off = (f & AVA1_FSW_AT_OFFSET) != 0;
    if ((f & AVA1_FSW_CREATE) && (f & AVA1_FSW_OVERWRITE)) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "fs_write_flags_conflict");
    if (append && at_off) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "fs_write_flags_conflict");
    whole = !append && !at_off;
    if (whole && q.offset != 0) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "fs_write_offset_without_chunk");
    /* The request is capped at 56 KiB, so more than a chunk cannot arrive; refuse rather than guess. */
    if (q.data_len > AVA1_FSW_CHUNK_MAX) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "too_large");
    commit = whole || (f & AVA1_FSW_COMMIT) != 0;
    if (!write_ok(path)) return mgmt_reply_error(cx, AVA1_ERR_PATH, "path_unsafe");
    if (snprintf(tmp, sizeof tmp, "%s%s", path, MGMT_FS_TMP_SUFFIX) >= (int)sizeof tmp) return mgmt_reply_error(cx, AVA1_ERR_PATH, "path_unsafe");
    /* FSW_CREATE: the target must not exist. Checked up front for the single-call write (nothing is
     * written for a refusal) and again at commit (it may have appeared while chunks were sent). */
    if (whole && (f & AVA1_FSW_CREATE) && lstat(path, &st) == 0) return mgmt_reply_error(cx, AVA1_ERR_EXISTS, "exists");
    mode = (mode_t)(q.has_mode ? (q.mode & 07777) : FS_DEFAULT_FILE_MODE);
    /* The tmp file is ours alone, opened so that nothing planted there can redirect the write:
     * O_NOFOLLOW (a symlink at the tmp name is an error, never followed) and O_NONBLOCK (a FIFO
     * cannot block a worker; fstat below refuses anything but a regular file). The single-call write
     * and the first chunk (offset 0) remove whatever sits at the tmp name and create it fresh with
     * O_EXCL, so a retry starts clean and a pre-planted file or link is never opened. A later chunk
     * must find the tmp file its first chunk made: with none it is refused, not made as a sparse file. */
    oflags = O_WRONLY | O_NOFOLLOW | O_NONBLOCK;
    if (whole || (at_off && q.offset == 0)) {
        if (unlink(tmp) != 0 && errno != ENOENT) return mgmt_reply_error(cx, AVA1_ERR_IO, "open_failed");
        oflags |= O_CREAT | O_EXCL;
    } else if (append) {
        oflags |= O_CREAT; /* an append writer's first chunk has no offset to say so */
    }
    fd = open(tmp, oflags, FS_DEFAULT_FILE_MODE);
    if (fd < 0) {
        if (errno == ENOENT) return mgmt_reply_error(cx, AVA1_ERR_PROTOCOL, "fs_write_no_tmp_file");
        return mgmt_reply_error(cx, AVA1_ERR_IO, "open_failed");
    }
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) {
        close(fd);
        return mgmt_reply_error(cx, AVA1_ERR_IO, "open_failed");
    }
    if (append) {
        if (lseek(fd, 0, SEEK_END) < 0) {
            close(fd);
            return mgmt_reply_error(cx, AVA1_ERR_IO, "write_failed");
        }
        if (write_all(fd, q.data, q.data_len, 0, 0) != 0) {
            close(fd);
            return mgmt_reply_error(cx, AVA1_ERR_IO, "write_failed");
        }
    } else if (write_all(fd, q.data, q.data_len, q.offset, 1) != 0) {
        int e = errno;
        close(fd);
        if (whole) unlink(tmp);
        return mgmt_reply_error(cx, e == ENOSPC ? AVA1_ERR_NO_SPACE : AVA1_ERR_IO, "write_failed");
    }
    if (!commit) {
        close(fd);
        return reply_empty(cx);
    }
    if (fsync(fd) != 0 && errno != EINVAL && errno != ENOTSUP) { /* a filesystem with no fsync is not an error */
        close(fd);
        unlink(tmp);
        return mgmt_reply_error(cx, AVA1_ERR_IO, "write_failed");
    }
    (void)fchmod(fd, mode);
    close(fd);
    if ((f & AVA1_FSW_CREATE) && lstat(path, &st) == 0) {
        unlink(tmp);
        return mgmt_reply_error(cx, AVA1_ERR_EXISTS, "exists");
    }
    /* Same directory as the target by construction, so this rename never crosses a device. */
    if (rename(tmp, path) != 0) {
        unlink(tmp);
        return mgmt_reply_error(cx, AVA1_ERR_IO, "rename_failed");
    }
    counted();
    return reply_empty(cx);
}
