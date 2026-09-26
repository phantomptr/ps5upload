#include "sdk_changer.h"

#include "sdk_param.h"
#include "elf_param.h"
#include "sdk_pairs.h"
#include "libc_backport.h"
#include "fakelib_overlay.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef APP_BASE
#define APP_BASE "/user/app"
#endif
#ifndef APPMETA_BASE
#define APPMETA_BASE "/user/appmeta"
#endif
#define BACKUP_SUFFIX ".bak"
#define SDK_PATH_MAX 1024
#define SDK_COPY_BUF (64u * 1024u)


static void json_escape(const char *in, char *out, size_t cap) {
    size_t o = 0;
    for (const char *s = in; *s && o + 2 < cap; s++) {
        if (*s == '"' || *s == '\\') { out[o++] = '\\'; out[o++] = *s; }
        else if (*s == '\n') { out[o++] = '\\'; out[o++] = 'n'; }
        else if ((unsigned char)*s >= 0x20) out[o++] = *s;
    }
    out[o] = '\0';
}

static int read_file_all(const char *path, char **out_buf, size_t *out_len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    struct stat st;
    if (fstat(fd, &st) < 0) { close(fd); return -1; }
    size_t len = (size_t)st.st_size;
    char *buf = malloc(len + 1);
    if (!buf) { close(fd); return -1; }
    size_t off = 0;
    while (off < len) {
        ssize_t n = read(fd, buf + off, len - off);
        if (n <= 0) { free(buf); close(fd); return -1; }
        off += (size_t)n;
    }
    close(fd);
    buf[len] = '\0';
    *out_buf = buf;
    *out_len = len;
    return 0;
}

static void extract_json_str(const char *json, const char *field,
                              char *out, size_t cap) {
    out[0] = '\0';
    char pat[64];
    snprintf(pat, sizeof(pat), "\"%s\"", field);
    const char *p = strstr(json, pat);
    if (!p) return;
    p += strlen(pat);
    while (*p && *p != ':') p++;
    if (*p != ':') return;
    p++;
    while (*p == ' ' || *p == '\t') p++;
    if (*p == '"') {
        p++;
        size_t i = 0;
        while (*p && *p != '"' && i < cap - 1) {
            out[i++] = *p++;
        }
        out[i] = '\0';
    }
}

/* Patch every SDK-version field this ELF actually declares.
 *
 * Returns the number of fields written, PATCH_SIGNED for an encrypted
 * SELF (which must not be touched at all), or -1 if the file could not
 * be opened. Locating the fields through the program header table rather
 * than scanning for magic values means a coincidental match in game data
 * is no longer patched — see payload/include/elf_param.h. */
#define PATCH_SIGNED (-2)

static int fd_write_all(int fd, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

static int fd_pwrite_all(int fd, const void *data, size_t len, off_t offset) {
    const unsigned char *p = (const unsigned char *)data;
    while (len > 0) {
        ssize_t n = pwrite(fd, p, len, offset);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        p += (size_t)n;
        len -= (size_t)n;
        offset += n;
    }
    return 0;
}

static int suffixed_path(const char *path, const char *suffix,
                         char *out, size_t out_cap) {
    int n = snprintf(out, out_cap, "%s%s", path, suffix);
    return n >= 0 && (size_t)n < out_cap ? 0 : -1;
}

/* Create a durable, same-directory backup without loading a potentially huge
 * ELF into memory. Existing valid backups are deliberately retained so a
 * second patch still restores the true original. */
static int make_backup(const char *path) {
    struct stat src_st;
    if (stat(path, &src_st) != 0 || !S_ISREG(src_st.st_mode)) return -1;

    char bak[SDK_PATH_MAX];
    char tmp[SDK_PATH_MAX];
    if (suffixed_path(path, BACKUP_SUFFIX, bak, sizeof(bak)) != 0 ||
        suffixed_path(path, BACKUP_SUFFIX ".tmp", tmp, sizeof(tmp)) != 0) {
        return -1;
    }

    struct stat bak_st;
    if (stat(bak, &bak_st) == 0) {
        return S_ISREG(bak_st.st_mode) && bak_st.st_size == src_st.st_size
                   ? 0
                   : -1;
    }
    if (errno != ENOENT) return -1;

    int in_fd = open(path, O_RDONLY);
    if (in_fd < 0) return -1;

    /* A prior interrupted attempt may leave only this private temporary file.
     * It is never a user backup and is safe to replace. */
    (void)unlink(tmp);
    int out_fd = open(tmp, O_WRONLY | O_CREAT | O_EXCL,
                      src_st.st_mode & 07777);
    if (out_fd < 0) {
        close(in_fd);
        return -1;
    }
    (void)fchmod(out_fd, src_st.st_mode & 07777);

    unsigned char *buf = malloc(SDK_COPY_BUF);
    int ok = buf != NULL;
    while (ok) {
        ssize_t n = read(in_fd, buf, SDK_COPY_BUF);
        if (n < 0) {
            if (errno == EINTR) continue;
            ok = 0;
        } else if (n == 0) {
            break;
        } else if (fd_write_all(out_fd, buf, (size_t)n) != 0) {
            ok = 0;
        }
    }
    free(buf);
    if (ok && fsync(out_fd) != 0) ok = 0;
    if (close(out_fd) != 0) ok = 0;
    close(in_fd);

    if (!ok || rename(tmp, bak) != 0) {
        (void)unlink(tmp);
        return -1;
    }
    return 0;
}

static int patch_binary_sdk(const char *path, uint32_t target_sdk) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    struct stat st;
    if (fstat(fd, &st) < 0) { close(fd); return -1; }
    size_t len = (size_t)st.st_size;
    if (len < 0x40) { close(fd); return 0; }

    void *map = mmap(NULL, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (map == MAP_FAILED) { close(fd); return -1; }

    uint8_t *data = (uint8_t *)map;
    elf_param_site_t sites[16];
    elf_param_status_t status = ELF_PARAM_OK;
    int found = elf_find_param_sites(data, len, sites,
                                     (int)(sizeof(sites) / sizeof(sites[0])),
                                     &status);

    /* A title declares BOTH an SDK pair member per param struct, so a site
     * needs rewriting when EITHER field disagrees. Checking only the
     * preferred field is how a half-patched executable — PPR downgraded,
     * PS4 left at 12.09 — got reported as fully patched. */
    sdk_pair_t pair;
    int have_pair = sdk_pair_lookup(target_sdk, &pair);
    int patched = 0;
    for (int i = 0; i < found; i++) {
        size_t base = sites[i].param_offset;
        int stale = have_pair
                        ? (elf_rd32(data + base + SCE_PARAM_PS5_SDK_OFFSET) != pair.ps5 ||
                           elf_rd32(data + base + SCE_PARAM_PS4_SDK_OFFSET) != pair.ps4)
                        : (elf_rd32(data + sites[i].offset) != target_sdk);
        if (stale) sites[patched++] = sites[i];
    }

    munmap(map, len);
    close(fd);

    if (found < 0) return status == ELF_PARAM_SIGNED_SELF ? PATCH_SIGNED : 0;
    if (patched == 0) return 0;
    if (make_backup(path) != 0) return -1;

    fd = open(path, O_WRONLY);
    if (fd < 0) return -1;
    struct stat now;
    if (fstat(fd, &now) != 0 || now.st_size != st.st_size) {
        close(fd);
        return -1;
    }

    for (int i = 0; i < patched; i++) {
        if (have_pair) {
            /* Both halves, always. The kernel prints them together
             * (`SDK vesion: PS4:09040001 PPR:04000031`) and a title whose
             * halves disagree launches and then dies. */
            unsigned char ps5v[4] = {
                (unsigned char)(pair.ps5 & 0xffu),
                (unsigned char)((pair.ps5 >> 8) & 0xffu),
                (unsigned char)((pair.ps5 >> 16) & 0xffu),
                (unsigned char)((pair.ps5 >> 24) & 0xffu),
            };
            unsigned char ps4v[4] = {
                (unsigned char)(pair.ps4 & 0xffu),
                (unsigned char)((pair.ps4 >> 8) & 0xffu),
                (unsigned char)((pair.ps4 >> 16) & 0xffu),
                (unsigned char)((pair.ps4 >> 24) & 0xffu),
            };
            size_t base = sites[i].param_offset;
            if (fd_pwrite_all(fd, ps5v, sizeof(ps5v),
                              (off_t)(base + SCE_PARAM_PS5_SDK_OFFSET)) != 0 ||
                fd_pwrite_all(fd, ps4v, sizeof(ps4v),
                              (off_t)(base + SCE_PARAM_PS4_SDK_OFFSET)) != 0) {
                close(fd);
                return -1;
            }
            continue;
        }
        /* No known pair for this request: write only the field this
         * segment declares, exactly as before. */
        unsigned char value[4] = {
            (unsigned char)(target_sdk & 0xffu),
            (unsigned char)((target_sdk >> 8) & 0xffu),
            (unsigned char)((target_sdk >> 16) & 0xffu),
            (unsigned char)((target_sdk >> 24) & 0xffu),
        };
        if (fd_pwrite_all(fd, value, sizeof(value),
                          (off_t)sites[i].offset) != 0) {
            close(fd);
            return -1;
        }
    }
    if (fsync(fd) != 0) {
        close(fd);
        return -1;
    }
    close(fd);
    return patched;
}

/* Returns the number of version fields rewritten, or -1 if the file
 * could not be read. Zero is a real answer — the document did not carry
 * either field — and the caller must report it rather than assume a
 * patch happened. */
static int patch_param_json_file(const char *path, uint32_t target_sdk) {
    char *buf = NULL;
    size_t len = 0;
    if (read_file_all(path, &buf, &len) != 0) return -1;

    size_t out_cap = len + 128;
    char *out = malloc(out_cap);
    if (!out) {
        free(buf);
        return -1;
    }

    size_t out_len = 0;
    int changed = sdk_param_rewrite_json(buf, len, target_sdk, out, out_cap,
                                         &out_len);
    free(buf);

    if (changed > 0) {
        if (make_backup(path) != 0) {
            free(out);
            return -1;
        }
        /* Same directory, so this rename never crosses a mount. */
        char tmp[SDK_PATH_MAX];
        struct stat st;
        if (stat(path, &st) != 0 ||
            suffixed_path(path, ".ps5upload.tmp", tmp, sizeof(tmp)) != 0) {
            free(out);
            return -1;
        }
        (void)unlink(tmp);
        int fd = open(tmp, O_WRONLY | O_CREAT | O_EXCL, st.st_mode & 07777);
        int ok = fd >= 0;
        if (ok) (void)fchmod(fd, st.st_mode & 07777);
        if (ok && fd_write_all(fd, out, out_len) != 0) ok = 0;
        if (ok && fsync(fd) != 0) ok = 0;
        if (fd >= 0 && close(fd) != 0) ok = 0;
        if (!ok || rename(tmp, path) != 0) {
            (void)unlink(tmp);
            changed = -1;
        }
    }
    free(out);
    return changed;
}

/* Apply BestPig's libc.prx edit, which some titles need before they will
 * start on a downgraded firmware. Separate from the SDK walk on purpose: it
 * targets one exact file, it is a same-length symbol swap rather than a
 * version field, and a libc that does not carry the symbol is the common
 * case (three of five titles on the test console) and must not read as an
 * error. Returns the number of occurrences rewritten, 0 for nothing to do,
 * -1 only when the file exists but could not be backed up or written. */
static int patch_libc_prx(const char *game_path) {
    char path[SDK_PATH_MAX];
    if (snprintf(path, sizeof(path), "%s/sce_module/libc.prx", game_path) < 0)
        return 0;

    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0) return 0;

    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    size_t len = (size_t)st.st_size;
    void *map = mmap(NULL, len, PROT_READ, MAP_PRIVATE, fd, 0);
    close(fd);
    if (map == MAP_FAILED) return 0;

    /* Find the sites first so an already-patched file is never rewritten
     * (and never gets a fresh backup that would shadow the true original). */
    unsigned char *copy = (unsigned char *)malloc(len);
    if (!copy) { munmap(map, len); return 0; }
    memcpy(copy, map, len);
    munmap(map, len);

    int n = libc_backport_apply(copy, len);
    if (n == 0) { free(copy); return 0; }

    if (make_backup(path) != 0) { free(copy); return -1; }
    fd = open(path, O_WRONLY);
    if (fd < 0) { free(copy); return -1; }
    struct stat now;
    if (fstat(fd, &now) != 0 || (size_t)now.st_size != len) {
        close(fd); free(copy); return -1;
    }
    int rc = fd_write_all(fd, copy, len);
    if (rc == 0 && fsync(fd) != 0) rc = -1;
    close(fd);
    free(copy);
    return rc == 0 ? n : -1;
}

static void walk_and_patch(const char *dir_path, uint32_t target_sdk,
                           int *patched_count, int *signed_count,
                           int *error_count) {
    DIR *dir = opendir(dir_path);
    if (!dir) return;
    struct dirent *de;
    while ((de = readdir(dir))) {
        if (de->d_name[0] == '.') continue;

        char full[SDK_PATH_MAX];
        int pn = snprintf(full, sizeof(full), "%s/%s", dir_path, de->d_name);
        if (pn < 0 || (size_t)pn >= sizeof(full)) {
            if (error_count) (*error_count)++;
            continue;
        }

        struct stat st;
        if (lstat(full, &st) != 0) continue;

        if (S_ISDIR(st.st_mode)) {
            /* Never rewrite `fakelib/`. Those are replacement SYSTEM
             * libraries a backport supplies, and they are meant to keep
             * the SDK version of the firmware they came from — every
             * working backported title on the test console carries them
             * at 0x09040001 while its own eboot sits at 0x04000031.
             * Rewriting them to the title's target made a game that
             * already failed fail differently, and would silently damage
             * a working one. */
            if (strcmp(de->d_name, "fakelib") == 0) continue;
            walk_and_patch(full, target_sdk, patched_count, signed_count,
                           error_count);
            continue;
        }
        if (!S_ISREG(st.st_mode)) continue;

        const char *dot = strrchr(de->d_name, '.');
        if (!dot) continue;
        if (strcasecmp(dot, ".bin") == 0 ||
            strcasecmp(dot, ".self") == 0 ||
            strcasecmp(dot, ".sprx") == 0 ||
            strcasecmp(dot, ".prx") == 0 ||
            strcasecmp(dot, ".elf") == 0) {
            int n = patch_binary_sdk(full, target_sdk);
            if (n > 0) *patched_count += n;
            else if (n == PATCH_SIGNED && signed_count) (*signed_count)++;
            else if (n < 0 && error_count) (*error_count)++;
        }
    }
    closedir(dir);
}

static void walk_and_restore(const char *dir_path, int *restored_count) {
    DIR *dir = opendir(dir_path);
    if (!dir) return;
    struct dirent *de;
    while ((de = readdir(dir))) {
        if (de->d_name[0] == '.') continue;

        char full[SDK_PATH_MAX];
        int pn = snprintf(full, sizeof(full), "%s/%s", dir_path, de->d_name);
        if (pn < 0 || (size_t)pn >= sizeof(full)) continue;

        struct stat st;
        if (lstat(full, &st) != 0) continue;

        if (S_ISDIR(st.st_mode)) {
            walk_and_restore(full, restored_count);
            continue;
        }
        if (!S_ISREG(st.st_mode)) continue;

        size_t namelen = strlen(de->d_name);
        size_t suffixlen = strlen(BACKUP_SUFFIX);
        if (namelen > suffixlen &&
            strcmp(de->d_name + namelen - suffixlen, BACKUP_SUFFIX) == 0) {
            char orig[SDK_PATH_MAX];
            size_t fulllen = strlen(full);
            if (fulllen - suffixlen >= sizeof(orig)) continue;
            memcpy(orig, full, fulllen - suffixlen);
            orig[fulllen - suffixlen] = '\0';

            if (rename(full, orig) == 0) {
                *restored_count += 1;
            }
        }
    }
    closedir(dir);
}

static int is_title_id(const char *title_id);
static int resolve_game_path(const char *title_id, char *out, size_t out_cap);

void sdk_changer_init(void) {
}

int sdk_changer_scan(char *buf, size_t cap, size_t *written) {
    DIR *dir = opendir(APPMETA_BASE);
    if (!dir) {
        int n = snprintf(buf, cap, "{\"titles\":[],\"error\":\"cannot open appmeta\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        return 0;
    }

    int n = snprintf(buf, cap, "{\"titles\":[");
    int first = 1;

    struct dirent *de;
    while ((de = readdir(dir))) {
        if (de->d_name[0] == '.') continue;

        char param_path[SDK_PATH_MAX];
        int path_n = snprintf(param_path, sizeof(param_path),
                              "%s/%s/sce_sys/param.json",
                              APPMETA_BASE, de->d_name);
        if (path_n < 0 || (size_t)path_n >= sizeof(param_path)) continue;
        struct stat st;
        if (stat(param_path, &st) != 0) {
            path_n = snprintf(param_path, sizeof(param_path),
                              "%s/%s/param.json", APPMETA_BASE, de->d_name);
            if (path_n < 0 || (size_t)path_n >= sizeof(param_path)) continue;
            if (stat(param_path, &st) != 0) continue;
        }

        char *content = NULL;
        size_t clen = 0;
        if (read_file_all(param_path, &content, &clen) != 0) continue;

        char title_id[32], name[128], sdk_ver[32], fw_req[32];
        extract_json_str(content, "titleId", title_id, sizeof(title_id));
        extract_json_str(content, "titleName", name, sizeof(name));
        extract_json_str(content, "sdkVersion", sdk_ver, sizeof(sdk_ver));
        extract_json_str(content, "requiredSystemSoftwareVersion", fw_req, sizeof(fw_req));
        free(content);

        if (!is_title_id(title_id)) continue;

        char source[SDK_PATH_MAX] = {0};
        int patchable = resolve_game_path(title_id, source, sizeof(source));

        char esc_tid[40], esc_name[160], esc_sdk[40], esc_fw[40];
        char esc_source[SDK_PATH_MAX * 2 + 1];
        json_escape(title_id, esc_tid, sizeof(esc_tid));
        json_escape(name, esc_name, sizeof(esc_name));
        json_escape(sdk_ver, esc_sdk, sizeof(esc_sdk));
        json_escape(fw_req, esc_fw, sizeof(esc_fw));
        json_escape(source, esc_source, sizeof(esc_source));

        const char *sep = first ? "" : ",";
        first = 0;
        int more = snprintf(buf + n, cap - (size_t)n,
            "%s{\"title_id\":\"%s\",\"name\":\"%s\","
            "\"sdk_version\":\"%s\",\"fw_required\":\"%s\","
            "\"patchable\":%s,\"source\":\"%s\"}",
            sep, esc_tid, esc_name, esc_sdk, esc_fw,
            patchable ? "true" : "false", esc_source);
        if (more < 0 || (size_t)(n + more) >= cap) break;
        n += more;
    }
    closedir(dir);

    char overlay[256];
    int overlay_n = fakelib_overlay_status_json(overlay, sizeof(overlay));
    if (overlay_n < 0 || (size_t)overlay_n >= sizeof(overlay)) return -1;
    int end = snprintf(buf + n, cap - (size_t)n, "],\"overlay\":%s}", overlay);
    if (end < 0 || (size_t)(n + end) >= cap) return -1;
    n += end;
    if (written) *written = (size_t)n;
    return 0;
}

static int is_title_id(const char *title_id) {
    if (!title_id || strlen(title_id) != 9) return 0;
    for (int i = 0; i < 4; i++) {
        if (title_id[i] < 'A' || title_id[i] > 'Z') return 0;
    }
    for (int i = 4; i < 9; i++) {
        if (title_id[i] < '0' || title_id[i] > '9') return 0;
    }
    return 1;
}

static int path_has_parent_component(const char *path) {
    const char *p = path;
    while (*p) {
        while (*p == '/') p++;
        const char *start = p;
        while (*p && *p != '/') p++;
        if ((size_t)(p - start) == 2 && start[0] == '.' && start[1] == '.') {
            return 1;
        }
    }
    return 0;
}

/* Folder/image registrations have an authoritative source tracker written by
 * register_title_from_path(). Package-installed and system titles do not.
 * Reading that exact path avoids fuzzy title-id matches and, critically,
 * avoids ever walking Sony-owned /system_ex or an unrelated USB directory. */
static int resolve_game_path(const char *title_id, char *out, size_t out_cap) {
    if (!is_title_id(title_id) || !out || out_cap == 0) return 0;

    char tracker[SDK_PATH_MAX];
    int n = snprintf(tracker, sizeof(tracker), "%s/%s/mount.lnk",
                     APP_BASE, title_id);
    if (n < 0 || (size_t)n >= sizeof(tracker)) return 0;

    FILE *f = fopen(tracker, "r");
    if (!f) return 0;
    char source[SDK_PATH_MAX];
    int have_line = fgets(source, sizeof(source), f) != NULL;
    int extra = have_line ? fgetc(f) : EOF;
    fclose(f);
    if (!have_line || extra != EOF) return 0;

    size_t len = strlen(source);
    while (len > 0 && isspace((unsigned char)source[len - 1])) {
        source[--len] = '\0';
    }
    if (source[0] != '/' || len <= 1 || path_has_parent_component(source)) {
        return 0;
    }
    while (len > 1 && source[len - 1] == '/') source[--len] = '\0';

    struct stat st;
    if (stat(source, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;
    n = snprintf(out, out_cap, "%s", source);
    if (n < 0 || (size_t)n >= out_cap) {
        out[0] = '\0';
        return 0;
    }
    return 1;
}

static int patch_param_if_present(const char *path, uint32_t target,
                                  int *error_count) {
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) return 0;
    int n = patch_param_json_file(path, target);
    if (n < 0 && error_count) (*error_count)++;
    return n > 0 ? n : 0;
}

static int patch_game_params(const char *game_path, uint32_t target,
                             int *error_count) {
    int total = 0;
    char path[SDK_PATH_MAX];
    int n = snprintf(path, sizeof(path), "%s/sce_sys/param.json", game_path);
    if (n < 0 || (size_t)n >= sizeof(path)) {
        if (error_count) (*error_count)++;
        return 0;
    }
    total += patch_param_if_present(path, target, error_count);

    n = snprintf(path, sizeof(path), "%s/param.json", game_path);
    if (n < 0 || (size_t)n >= sizeof(path)) {
        if (error_count) (*error_count)++;
        return total;
    }
    total += patch_param_if_present(path, target, error_count);
    return total;
}

static int patch_appmeta_param(const char *title_id, uint32_t target,
                               int *error_count) {
    char path[SDK_PATH_MAX];
    struct stat st;
    int n = snprintf(path, sizeof(path), "%s/%s/sce_sys/param.json",
                     APPMETA_BASE, title_id);
    if (n < 0 || (size_t)n >= sizeof(path)) {
        if (error_count) (*error_count)++;
        return 0;
    }
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) {
        n = snprintf(path, sizeof(path), "%s/%s/param.json",
                     APPMETA_BASE, title_id);
        if (n < 0 || (size_t)n >= sizeof(path)) {
            if (error_count) (*error_count)++;
            return 0;
        }
    }
    return patch_param_if_present(path, target, error_count);
}

int sdk_changer_patch(const char *title_id, const char *target_sdk,
                      int patch_libc, char *err, size_t err_cap) {
    if (!title_id || !target_sdk) {
        if (err) snprintf(err, err_cap, "missing title_id or target_sdk");
        return -1;
    }
    if (!is_title_id(title_id)) {
        if (err) snprintf(err, err_cap, "invalid title_id: %s", title_id);
        return -1;
    }

    char *end = NULL;
    errno = 0;
    unsigned long parsed = strtoul(target_sdk, &end, 16);
    if (errno != 0 || end == target_sdk || !end || *end != '\0' ||
        parsed == 0 || parsed > UINT32_MAX) {
        if (err) snprintf(err, err_cap, "invalid target_sdk: %s", target_sdk);
        return -1;
    }
    uint32_t target = (uint32_t)parsed;

    char game_path[SDK_PATH_MAX] = "";
    if (!resolve_game_path(title_id, game_path, sizeof(game_path))) {
        if (err) {
            snprintf(err, err_cap,
                     "%s is not a mounted folder/image registration or its "
                     "source is unavailable; package-installed titles cannot "
                     "be safely rewritten",
                     title_id);
        }
        return -1;
    }

    int errors = 0;
    int game_json_changed = patch_game_params(game_path, target, &errors);
    int patched = 0;
    int signed_skipped = 0;
    walk_and_patch(game_path, target, &patched, &signed_skipped, &errors);

    /* Opt-in only — see sdk_changer.h. */
    int libc_sites = patch_libc ? patch_libc_prx(game_path) : 0;
    if (libc_sites < 0) { errors++; libc_sites = 0; }

    int actual_changed = game_json_changed + patched + libc_sites;
    int appmeta_changed = 0;
    if (actual_changed > 0) {
        /* appmeta is only a display cache. Update it after the real source was
         * changed, never as a substitute for patching the game itself. */
        appmeta_changed = patch_appmeta_param(title_id, target, &errors);
    }

    /* Report what actually happened. This used to return success
     * unconditionally and claim param.json had been patched even when
     * nothing was written, so a no-op looked identical to a real patch. */
    if (actual_changed == 0) {
        if (err) {
            if (errors > 0)
                snprintf(err, err_cap,
                         "could not back up or rewrite %d candidate file(s) "
                         "under %s",
                         errors, game_path);
            else if (signed_skipped > 0)
                snprintf(err, err_cap,
                         "%d file(s) are signed SELFs and cannot be patched — "
                         "this title needs decrypted ELFs first. param.json "
                         "also had no rewritable version fields.",
                         signed_skipped);
            else
                snprintf(err, err_cap,
                         "nothing to patch for %s (no version fields in "
                         "param.json and no patchable ELF SDK sites under %s)",
                         title_id, game_path);
        }
        return -1;
    }

    if (err) {
        snprintf(err, err_cap,
                 "source param fields: %d, ELF sites: %d, libc sites: %d, "
                 "metadata fields: %d, signed skipped: %d, write errors: %d, "
                 "path=%s",
                 game_json_changed, patched, libc_sites, appmeta_changed,
                 signed_skipped, errors, game_path);
    }
    return 0;
}

int sdk_changer_restore(const char *title_id, int *restored_count,
                        char *err, size_t err_cap) {
    if (!title_id) {
        if (err) snprintf(err, err_cap, "missing title_id");
        return -1;
    }
    if (!is_title_id(title_id)) {
        if (err) snprintf(err, err_cap, "invalid title_id: %s", title_id);
        return -1;
    }
    if (restored_count) *restored_count = 0;

    int count = 0;
    char game_path[SDK_PATH_MAX] = "";
    if (resolve_game_path(title_id, game_path, sizeof(game_path))) {
        walk_and_restore(game_path, &count);
    }
    /* Also restore appmeta backups so the list reverts too. */
    char meta[SDK_PATH_MAX];
    int mn = snprintf(meta, sizeof(meta), "%s/%s", APPMETA_BASE, title_id);
    struct stat st;
    if (mn >= 0 && (size_t)mn < sizeof(meta) &&
        stat(meta, &st) == 0 && S_ISDIR(st.st_mode)) {
        walk_and_restore(meta, &count);
    }

    if (restored_count) *restored_count = count;

    if (count == 0) {
        if (err) snprintf(err, err_cap, "no .bak files found for %s", title_id);
    }

    return 0;
}
