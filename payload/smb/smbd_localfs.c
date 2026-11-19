/* xfs_t over the console's own filesystem (POSIX). Also built on the host for the tests. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

#include "smbd_fs.h"

typedef struct {
    DIR *d;
    char path[SMBP_PATH_MAX];
} ldir_t;

typedef struct {
    int fd;
    int wrote;
} lfile_t;

static int map_errno(int e) {
    switch (e) {
    case ENOENT: return XE_NOT_FOUND;
    case EACCES:
    case EPERM:
    case EROFS: return XE_ACCESS;
    case ENOSPC:
#ifdef EDQUOT
    case EDQUOT:
#endif
        return XE_NO_SPACE;
    case EEXIST:
    case ENOTEMPTY: return XE_EXISTS;
    case ENOTDIR: return XE_NOT_DIR;
    case EISDIR: return XE_IS_DIR;
    case ENAMETOOLONG:
    case EINVAL:
#ifdef EILSEQ
    case EILSEQ:
#endif
        return XE_BAD_NAME;
    case ENOMEM: return XE_NOMEM;
    default: return XE_IO;
    }
}

static void fill(const struct stat *s, xstat_t *st) {
    st->kind = S_ISDIR(s->st_mode) ? XK_DIR : S_ISREG(s->st_mode) ? XK_FILE : XK_OTHER;
    st->size = S_ISREG(s->st_mode) ? (uint64_t)s->st_size : 0;
    st->mtime = (uint64_t)s->st_mtime;
}

static int l_stat(xfs_t *fs, const char *path, xstat_t *st) {
    struct stat s;
    (void)fs;
    if (lstat(path, &s) != 0) return map_errno(errno);
    fill(&s, st);
    return 0;
}

static int l_opendir(xfs_t *fs, const char *path, void **dir) {
    ldir_t *d;
    (void)fs;
    if (strlen(path) >= sizeof d->path) return XE_BAD_NAME;
    d = calloc(1, sizeof *d);
    if (!d) return XE_NOMEM;
    d->d = opendir(path);
    if (!d->d) {
        int e = map_errno(errno);
        free(d);
        return e;
    }
    strcpy(d->path, path);
    *dir = d;
    return 0;
}

static int l_readdir(xfs_t *fs, void *dir, char *name, size_t cap, xstat_t *st) {
    ldir_t *d = dir;
    struct dirent *e;
    (void)fs;
    for (;;) {
        char full[SMBP_PATH_MAX];
        struct stat s;
        errno = 0;
        e = readdir(d->d);
        if (!e) return errno ? map_errno(errno) : 0;
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        /* Too long to name or to reach: an error for this entry (smbd_copy.h), not left out. */
        if (strlen(e->d_name) >= cap) return XE_BAD_NAME;
        if (smbp_join(d->path, e->d_name, full, sizeof full) != 0) return XE_BAD_NAME;
        if (lstat(full, &s) != 0) continue; /* gone meanwhile */
        strcpy(name, e->d_name);
        fill(&s, st);
        return 1;
    }
}

static void l_closedir(xfs_t *fs, void *dir) {
    ldir_t *d = dir;
    (void)fs;
    if (!d) return;
    if (d->d) closedir(d->d);
    free(d);
}

static int l_mkdir(xfs_t *fs, const char *path) {
    (void)fs;
    if (mkdir(path, 0777) != 0) return map_errno(errno);
    (void)chmod(path, 0777);
    return 0;
}

static int l_open(const char *path, int flags, int wrote, void **fh) {
    lfile_t *f = calloc(1, sizeof *f);
    if (!f) return XE_NOMEM;
    f->fd = open(path, flags | O_NOFOLLOW, 0777);
    if (f->fd < 0) {
        int e = map_errno(errno);
        free(f);
        return e;
    }
    f->wrote = wrote;
    if (wrote) (void)fchmod(f->fd, 0777);
    *fh = f;
    return 0;
}

static int l_open_read(xfs_t *fs, const char *path, void **fh) {
    (void)fs;
    return l_open(path, O_RDONLY, 0, fh);
}

static int l_open_write(xfs_t *fs, const char *path, int resume, void **fh) {
    (void)fs;
    return l_open(path, O_WRONLY | O_CREAT | (resume ? 0 : O_TRUNC), 1, fh);
}

static long long l_pread(xfs_t *fs, void *fh, void *buf, size_t n, uint64_t off) {
    lfile_t *f = fh;
    ssize_t r;
    (void)fs;
    do r = pread(f->fd, buf, n, (off_t)off);
    while (r < 0 && errno == EINTR);
    return r < 0 ? map_errno(errno) : (long long)r;
}

static long long l_pwrite(xfs_t *fs, void *fh, const void *buf, size_t n, uint64_t off) {
    lfile_t *f = fh;
    ssize_t r;
    (void)fs;
    do r = pwrite(f->fd, buf, n, (off_t)off);
    while (r < 0 && errno == EINTR);
    return r < 0 ? map_errno(errno) : (long long)r;
}

static int l_close(xfs_t *fs, void *fh) {
    lfile_t *f = fh;
    int rc = 0;
    (void)fs;
    if (!f) return 0;
    /* A written file reaches the disk before it is renamed into place. */
    if (f->wrote && fsync(f->fd) != 0) rc = map_errno(errno);
    if (close(f->fd) != 0 && rc == 0) rc = map_errno(errno);
    free(f);
    return rc;
}

/* Same folder only (copy_run renames "<name>.ps5u-part" beside <name>): never across mounts,
 * which the PS5 kernel answers with a panic instead of EXDEV. */
static int l_rename(xfs_t *fs, const char *from, const char *to, int replace) {
    char a[SMBP_PATH_MAX], b[SMBP_PATH_MAX];
    struct stat st;
    (void)fs;
    smbp_parent(from, a, sizeof a);
    smbp_parent(to, b, sizeof b);
    if (strcmp(a, b) != 0) return XE_BAD_REQUEST;
    if (!replace && lstat(to, &st) == 0) return XE_EXISTS;
    if (rename(from, to) != 0) return map_errno(errno);
    return 0;
}

static int l_unlink(xfs_t *fs, const char *path) {
    (void)fs;
    return unlink(path) != 0 ? map_errno(errno) : 0;
}

static int l_rmdir(xfs_t *fs, const char *path) {
    (void)fs;
    return rmdir(path) != 0 ? map_errno(errno) : 0;
}

static int l_free_space(xfs_t *fs, const char *path, uint64_t *out) {
    struct statvfs v;
    (void)fs;
    if (statvfs(path, &v) != 0) return map_errno(errno);
    *out = (uint64_t)v.f_bavail * (uint64_t)(v.f_frsize ? v.f_frsize : v.f_bsize);
    return 0;
}

static const xfs_ops_t k_local_ops = {
    l_stat, l_opendir, l_readdir, l_closedir, l_mkdir, l_open_read, l_open_write, l_pread, l_pwrite, l_close,
    l_rename, l_unlink, l_rmdir, l_free_space, NULL, NULL,
};

void localfs_init(xfs_t *fs) {
    fs->ops = &k_local_ops;
    fs->impl = NULL;
}
