/* ps5upload — tag-based backup & restore. See backup.h.
 *
 * Design: snapshots live at /data/ps5upload/backups/<tag>/<unix_ts>/.
 * Each snapshot has a .manifest file (basename \t original_path per line)
 * and flattened copies of the backed-up files. Restore reads .manifest
 * and copies each file back to its original path. */

#include "backup.h"
#include "runtime.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define BACKUPS_ROOT          "/data/ps5upload/backups"
#define BACKUPS_KEEP_PER_TAG  5

static pthread_mutex_t g_backup_lock = PTHREAD_MUTEX_INITIALIZER;

/* Same-second coalescing: if a snapshot for this tag was created in the
 * last second, reuse it. This lets a multi-file snapshot call append
 * files into one directory instead of scattering them. */
static char  g_last_tag[64] = "";
static time_t g_last_ts = 0;
static char  g_last_path[1024] = "";

static int mkpath_p(const char *path) {
    char tmp[1024];
    size_t len = strnlen(path, sizeof(tmp));
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            mkdir(tmp, 0755);
            *p = '/';
        }
    }
    return mkdir(tmp, 0755);
}

static int copy_file(const char *src, const char *dst) {
    /* Defense-in-depth: every write destination must be inside the
     * writable-roots allowlist. The restore path already validates the
     * manifest's "original" field before calling us, but this catches
     * any future caller that forgets. Backup snapshot dirs are always
     * under /data/ps5upload/backups/ which passes is_path_allowed. */
    if (!is_path_allowed(dst)) return -1;
    int sfd = open(src, O_RDONLY);
    if (sfd < 0) return -1;
    struct stat st;
    if (fstat(sfd, &st) != 0) {
        close(sfd);
        return -1;
    }
    char parent[1024];
    snprintf(parent, sizeof(parent), "%s", dst);
    char *slash = strrchr(parent, '/');
    if (slash) {
        *slash = 0;
        mkpath_p(parent);
    }
    int dfd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (dfd < 0) {
        close(sfd);
        return -1;
    }
    char buf[64 * 1024];
    ssize_t n;
    while ((n = read(sfd, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(dfd, buf + off, n - off);
            if (w <= 0) {
                close(sfd);
                close(dfd);
                unlink(dst);
                return -1;
            }
            off += w;
        }
    }
    fsync(dfd);
    close(sfd);
    close(dfd);
    struct timespec times[2];
    times[0].tv_sec = st.st_atime;
    times[0].tv_nsec = 0;
    times[1].tv_sec = st.st_mtime;
    times[1].tv_nsec = 0;
    utimensat(AT_FDCWD, dst, times, 0);
    return 0;
}

static const char *snapshot_dir_for(const char *tag) {
    time_t now = time(NULL);
    if (g_last_ts != 0 && now - g_last_ts < 1 && strcmp(g_last_tag, tag) == 0) {
        return g_last_path;
    }
    snprintf(g_last_path, sizeof(g_last_path), "%s/%s/%lld",
             BACKUPS_ROOT, tag, (long long)now);
    mkpath_p(g_last_path);
    strncpy(g_last_tag, tag, sizeof(g_last_tag) - 1);
    g_last_tag[sizeof(g_last_tag) - 1] = 0;
    g_last_ts = now;
    return g_last_path;
}

static void manifest_append(const char *snap_dir, const char *snap_basename,
                            const char *original) {
    char mpath[1024];
    snprintf(mpath, sizeof(mpath), "%s/.manifest", snap_dir);
    FILE *f = fopen(mpath, "a");
    if (!f) return;
    fprintf(f, "%s\t%s\n", snap_basename, original);
    fclose(f);
}

static void flatten_path(const char *src, char *out, size_t out_size) {
    const char *p = src;
    while (*p == '/') p++;
    size_t i = 0;
    while (*p && i < out_size - 1) {
        out[i++] = (*p == '/') ? '_' : *p;
        p++;
    }
    out[i] = 0;
}

static int snapshot_tree_inner(const char *snap_dir, const char *src,
                               int depth, int *file_count) {
    if (depth > 8) return 0;
    DIR *d = opendir(src);
    if (!d) return -1;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0)
            continue;
        char child[1024];
        if (snprintf(child, sizeof(child), "%s/%s", src, e->d_name)
            >= (int)sizeof(child))
            continue;
        struct stat st;
        if (lstat(child, &st) != 0) continue;
        if (S_ISLNK(st.st_mode)) continue;
        if (S_ISDIR(st.st_mode)) {
            snapshot_tree_inner(snap_dir, child, depth + 1, file_count);
        } else if (S_ISREG(st.st_mode)) {
            char flat[512];
            flatten_path(child, flat, sizeof(flat));
            char dst[1024];
            snprintf(dst, sizeof(dst), "%s/%s", snap_dir, flat);
            if (copy_file(child, dst) == 0) {
                manifest_append(snap_dir, flat, child);
                (*file_count)++;
            }
        }
    }
    closedir(d);
    return 0;
}

int backup_validate_tag(const char *tag) {
    if (!tag || !tag[0]) return -1;
    size_t len = strlen(tag);
    if (len > 32) return -1;
    for (size_t i = 0; i < len; i++) {
        char c = tag[i];
        if (!(isalnum((unsigned char)c) || c == '-' || c == '_'))
            return -1;
    }
    return 0;
}

int backup_snapshot(const char *tag, const char *src_path,
                    int64_t *out_timestamp, int *out_files,
                    uint64_t *out_bytes) {
    if (backup_validate_tag(tag) != 0) return -1;
    struct stat st;
    if (stat(src_path, &st) != 0) return -1;

    pthread_mutex_lock(&g_backup_lock);
    /* Reset coalescing so each explicit snapshot gets its own dir. */
    g_last_ts = 0;
    const char *snap = snapshot_dir_for(tag);
    int file_count = 0;

    if (S_ISDIR(st.st_mode)) {
        snapshot_tree_inner(snap, src_path, 0, &file_count);
    } else if (S_ISREG(st.st_mode)) {
        char flat[512];
        flatten_path(src_path, flat, sizeof(flat));
        char dst[1024];
        snprintf(dst, sizeof(dst), "%s/%s", snap, flat);
        if (copy_file(src_path, dst) == 0) {
            manifest_append(snap, flat, src_path);
            file_count = 1;
        }
    }

    /* Compute total bytes of the snapshot dir. */
    uint64_t total_bytes = 0;
    DIR *sd = opendir(snap);
    if (sd) {
        struct dirent *fe;
        while ((fe = readdir(sd))) {
            if (fe->d_name[0] == '.') continue;
            char fp[1024];
            snprintf(fp, sizeof(fp), "%s/%s", snap, fe->d_name);
            struct stat fs;
            if (stat(fp, &fs) == 0) total_bytes += (uint64_t)fs.st_size;
        }
        closedir(sd);
    }

    /* Extract timestamp from the path. */
    const char *base = strrchr(snap, '/');
    int64_t ts = base ? strtoll(base + 1, NULL, 10) : 0;

    if (out_timestamp) *out_timestamp = ts;
    if (out_files) *out_files = file_count;
    if (out_bytes) *out_bytes = total_bytes;

    pthread_mutex_unlock(&g_backup_lock);
    return file_count > 0 ? 0 : -1;
}

int backup_list(const char *tag_filter, char *buf, size_t cap,
                size_t *written) {
    pthread_mutex_lock(&g_backup_lock);
    size_t n = 0;
    n += snprintf(buf + n, cap - n, "{\"snapshots\":[");
    int wrote_one = 0;

    DIR *root = opendir(BACKUPS_ROOT);
    if (root) {
        struct dirent *te;
        while ((te = readdir(root))) {
            if (te->d_name[0] == '.') continue;
            if (tag_filter && tag_filter[0] &&
                strcmp(te->d_name, tag_filter) != 0)
                continue;
            char tagdir[512];
            snprintf(tagdir, sizeof(tagdir), "%s/%s", BACKUPS_ROOT,
                     te->d_name);
            struct stat ts;
            if (stat(tagdir, &ts) != 0 || !S_ISDIR(ts.st_mode)) continue;

            DIR *td = opendir(tagdir);
            if (!td) continue;
            struct dirent *se;
            while ((se = readdir(td))) {
                if (se->d_name[0] == '.') continue;
                char snapdir[768];
                snprintf(snapdir, sizeof(snapdir), "%s/%s", tagdir,
                         se->d_name);
                struct stat ss;
                if (stat(snapdir, &ss) != 0 || !S_ISDIR(ss.st_mode))
                    continue;
                /* Count manifest lines. */
                int files = 0;
                uint64_t bytes = 0;
                char mpath[900];
                snprintf(mpath, sizeof(mpath), "%s/.manifest", snapdir);
                FILE *mf = fopen(mpath, "r");
                if (mf) {
                    char ln[512];
                    while (fgets(ln, sizeof(ln), mf)) files++;
                    fclose(mf);
                }
                /* Sum file sizes. */
                DIR *sd = opendir(snapdir);
                if (sd) {
                    struct dirent *fe;
                    while ((fe = readdir(sd))) {
                        if (fe->d_name[0] == '.') continue;
                        char fp[1024];
                        snprintf(fp, sizeof(fp), "%s/%s", snapdir,
                                 fe->d_name);
                        struct stat fs;
                        if (stat(fp, &fs) == 0)
                            bytes += (uint64_t)fs.st_size;
                    }
                    closedir(sd);
                }
                if (n >= cap - 128) break;
                if (wrote_one) {
                    if (n < cap - 1) buf[n++] = ',';
                }
                wrote_one = 1;
                n += snprintf(buf + n, cap - n,
                    "{\"tag\":\"%s\",\"timestamp\":%lld,\"files\":%d,"
                    "\"bytes\":%llu}",
                    te->d_name, (long long)atoll(se->d_name),
                    files, (unsigned long long)bytes);
            }
            closedir(td);
        }
        closedir(root);
    }
    if (n < cap - 2) {
        buf[n++] = ']';
        buf[n++] = '}';
    }
    buf[n] = 0;
    if (written) *written = n;
    pthread_mutex_unlock(&g_backup_lock);
    return 0;
}

static int restore_from_manifest(const char *snap_dir, int *restored) {
    char mpath[1024];
    snprintf(mpath, sizeof(mpath), "%s/.manifest", snap_dir);
    FILE *f = fopen(mpath, "r");
    if (!f) return -1;
    int n = 0;
    char line[1100];
    while (fgets(line, sizeof(line), f)) {
        size_t L = strlen(line);
        while (L > 0 && (line[L - 1] == '\n' || line[L - 1] == '\r'))
            line[--L] = 0;
        char *tab = strchr(line, '\t');
        if (!tab) continue;
        *tab = 0;
        const char *snap_basename = line;
        const char *original = tab + 1;
        /* CWE-59 path traversal guard: reject manifests whose "original"
         * field escapes the writable-roots allowlist. A hostile or hand-
         * edited manifest could name e.g. /system_ex/something or use
         * ../ components to write outside the intended game-data area.
         * is_path_allowed checks both the lexical form and the realpath()
         * resolved form (symlink-escape). */
        if (!is_path_allowed(original)) continue;
        char src[1024];
        snprintf(src, sizeof(src), "%s/%s", snap_dir, snap_basename);
        if (copy_file(src, original) == 0) n++;
    }
    fclose(f);
    if (restored) *restored = n;
    return 0;
}

int backup_restore(const char *tag, int64_t timestamp, int *out_restored) {
    if (backup_validate_tag(tag) != 0) return -1;
    pthread_mutex_lock(&g_backup_lock);
    char snapdir[1024];
    snprintf(snapdir, sizeof(snapdir), "%s/%s/%lld", BACKUPS_ROOT, tag,
             (long long)timestamp);
    struct stat st;
    if (stat(snapdir, &st) != 0 || !S_ISDIR(st.st_mode)) {
        pthread_mutex_unlock(&g_backup_lock);
        return -1;
    }
    int restored = 0;
    int rc = restore_from_manifest(snapdir, &restored);
    if (out_restored) *out_restored = restored;
    pthread_mutex_unlock(&g_backup_lock);
    return rc;
}

static int rm_rf(const char *path) {
    struct stat st;
    if (lstat(path, &st) != 0) return -1;
    if (S_ISDIR(st.st_mode)) {
        DIR *d = opendir(path);
        if (d) {
            struct dirent *e;
            while ((e = readdir(d))) {
                if (strcmp(e->d_name, ".") == 0 ||
                    strcmp(e->d_name, "..") == 0)
                    continue;
                char child[1024];
                snprintf(child, sizeof(child), "%s/%s", path, e->d_name);
                rm_rf(child);
            }
            closedir(d);
        }
        return rmdir(path);
    }
    return unlink(path);
}

int backup_delete(const char *tag, int64_t timestamp) {
    if (backup_validate_tag(tag) != 0) return -1;
    pthread_mutex_lock(&g_backup_lock);
    char snapdir[1024];
    snprintf(snapdir, sizeof(snapdir), "%s/%s/%lld", BACKUPS_ROOT, tag,
             (long long)timestamp);
    int rc = rm_rf(snapdir);
    /* Also try removing the tag dir if now empty. */
    char tagdir[640];
    snprintf(tagdir, sizeof(tagdir), "%s/%s", BACKUPS_ROOT, tag);
    rmdir(tagdir);
    pthread_mutex_unlock(&g_backup_lock);
    return rc;
}

void backup_init(void) {
    mkpath_p(BACKUPS_ROOT);
}
