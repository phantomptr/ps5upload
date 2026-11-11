/* Decompress a `.ffpfsc` on the console (#453): the image inside written beside it.
 *
 * A `.ffpfsc` is one file (normally an `.exfat` game image) inside a minimal single-file PFS,
 * stored as a zlib PFSC container: 64 KiB blocks, each a zlib stream or, when that saved too
 * little, the raw block. The layout is MkPFS's, which ShadowMount+ mounts and ps5upload writes
 * (engine/crates/ps5upload-fpkg/src/ffpfsc.rs reads the same way):
 *
 *   block 1  inode table; the file is inode 3 (flags, stored size, raw size, first block)
 *   block 5  uroot entries: ., .., the file's name
 *   block N  the file's bytes from its first block: the container, or the raw file
 *
 * Doing it here saves the round trip a computer needs: download the container, unpack it,
 * upload the whole image back (191 GB over Wi-Fi for a 56 GB container in the report).
 *
 * JOB_OP_FFPFSC_UNPACK {"path":"<src>.ffpfsc"} writes `<dir>/<inner name>.part`, checks its
 * size (and, for an .exfat, the boot sector), then renames it to `<dir>/<inner name>` in the
 * same folder. It refuses a destination that exists and a drive without room for the image
 * plus 1 GiB. The source is kept. Every buffer is on the heap.
 *
 * The decoding is the kernel's: the container is mounted read-only the way ShadowMount+ mounts
 * it (fs_pfsc_mount_ro) and the file inside is read like any other. A first build inflated the
 * blocks here with vendored zlib, and the helper died with SIGBUS within minutes of loading it
 * (once the console froze), so nothing here decompresses. The container's block table is still
 * checked before the kernel sees it. */
#include "fs_jobs.h"

#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "path_policy.h"
#include "runtime.h"
#include "webui_codec.h"

#define UNP_BLOCK 0x10000ull
#define UNP_HEAD_BLOCKS 6ull /* blocks 0-5: header, inodes, directories */
#define UNP_INODE_LEN 0xA8u
#define UNP_PFS_VERSION 2ull
#define UNP_PFS_MAGIC 20130315ull
#define UNP_PFSC_MAGIC 0x43534650u
#define UNP_FLAG_COMPRESSED 0x1u
#define UNP_TABLE_WINDOW 8192u /* block offsets read at a time */
#define UNP_OUT_BUF (4u << 20) /* copied in 4 MiB runs */
#define UNP_MARGIN (1ull << 30)

static uint64_t le64(const uint8_t *b) {
    uint64_t v = 0;
    for (int i = 7; i >= 0; i--) v = (v << 8) | b[i];
    return v;
}
static uint32_t le32(const uint8_t *b) { return (uint32_t)b[0] | (uint32_t)b[1] << 8 | (uint32_t)b[2] << 16 | (uint32_t)b[3] << 24; }

static int read_at(int fd, uint64_t off, void *buf, size_t n) {
    uint8_t *p = buf;
    while (n) {
        ssize_t r = pread(fd, p, n, (off_t)off);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1;
        p += r;
        off += (uint64_t)r;
        n -= (size_t)r;
    }
    return 0;
}

static int write_all(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

static int may_write_here(const char *p) {
    const ava1_data_cfg_t *cfg = ava1_data_cfg();
    return cfg->may_write && cfg->may_write(p) && !path_tree_op_refused(p);
}

/* Where the single file lives in the image. 0, or a cause token with the reason. */
typedef struct {
    char name[256];
    uint64_t raw;    /* the file's own size */
    uint64_t stored; /* what it takes in the image */
    uint64_t base;   /* byte offset of the file or its container */
    int compressed;
} unp_layout_t;

static const char *read_layout(int fd, unp_layout_t *L) {
    const size_t len = (size_t)(UNP_HEAD_BLOCKS * UNP_BLOCK);
    uint8_t *head = malloc(len);
    const char *bad = NULL;
    if (!head) return "fs_unpack_no_memory";
    if (read_at(fd, 0, head, len) != 0) {
        bad = "fs_unpack_not_pfs";
        goto out;
    }
    if (le64(head) != UNP_PFS_VERSION || le64(head + 8) != UNP_PFS_MAGIC) {
        bad = "fs_unpack_not_pfs";
        goto out;
    }
    const uint8_t *node = head + UNP_BLOCK + 3 * UNP_INODE_LEN;
    uint32_t flags = le32(node + 4);
    uint64_t first = le32(node + 0x64);
    L->compressed = (flags & UNP_FLAG_COMPRESSED) != 0;
    L->stored = le64(node + 8);
    L->raw = L->compressed ? le64(node + 16) : L->stored;
    L->base = first * UNP_BLOCK;
    /* The file named in uroot: its third entry. */
    const uint8_t *uroot = head + 5 * UNP_BLOCK;
    size_t at = 0, room = (size_t)UNP_BLOCK;
    for (int i = 0; i < 3; i++) {
        if (at + 16 > room) {
            bad = "fs_unpack_bad_directory";
            goto out;
        }
        uint32_t nlen = le32(uroot + at + 8), step = le32(uroot + at + 12);
        if (step == 0 || at + 16 + nlen > room || nlen >= sizeof L->name) {
            bad = "fs_unpack_bad_directory";
            goto out;
        }
        memcpy(L->name, uroot + at + 16, nlen);
        L->name[nlen] = 0;
        at += step;
    }
    if (!L->name[0] || strchr(L->name, '/') || strcmp(L->name, ".") == 0 || strcmp(L->name, "..") == 0)
        bad = "fs_unpack_bad_name";
    else if (first < UNP_HEAD_BLOCKS || L->raw == 0 || L->raw > UINT64_MAX - UNP_MARGIN)
        /* The last: a size no file has. Every later `L.raw + UNP_MARGIN` (the free-space check)
         * and `L.raw + UNP_BLOCK` would wrap to a small number and pass. */
        bad = "fs_unpack_bad_inode";
out:
    free(head);
    return bad;
}

/* Whether the image's file, `stored` bytes at `base`, lies inside a source of `size` bytes. As
 * subtractions: both numbers come from the image, and `base + stored` wraps for a corrupt one,
 * which read as "fits". */
static int unp_inside(uint64_t base, uint64_t stored, uint64_t size) { return base <= size && stored <= size - base; }

/* The container's header and block table, read without decoding a block: every offset inside the
 * container and each block no larger than 64 KiB. A malformed one never reaches the kernel's
 * decoder. 0, or a cause token. */
static const char *check_container(int fd, const unp_layout_t *L, uint8_t *table) {
    uint8_t ph[0x30];
    if (read_at(fd, L->base, ph, sizeof ph) != 0 || le32(ph) != UNP_PFSC_MAGIC || le64(ph + 0x10) != UNP_BLOCK)
        return "fs_unpack_not_pfsc";
    uint64_t blocks = le64(ph + 0x28) / UNP_BLOCK, table_at = L->base + le64(ph + 0x18);
    if (blocks != (L->raw + UNP_BLOCK - 1) / UNP_BLOCK) return "fs_unpack_bad_container";
    for (uint64_t i0 = 0; i0 < blocks; i0 += UNP_TABLE_WINDOW) {
        uint64_t span = blocks - i0 < UNP_TABLE_WINDOW ? blocks - i0 : UNP_TABLE_WINDOW;
        if (read_at(fd, table_at + i0 * 8, table, (size_t)(span + 1) * 8) != 0) return "fs_unpack_bad_container";
        for (uint64_t k = 0; k < span; k++) {
            uint64_t from = le64(table + k * 8), to = le64(table + (k + 1) * 8);
            if (to <= from || to - from > UNP_BLOCK || to > L->stored) return "fs_unpack_bad_container";
        }
    }
    return NULL;
}

/* One unpack at a time: each holds a mount and an LVD unit, and they share one mount point. */
static pthread_mutex_t unp_busy = PTHREAD_MUTEX_INITIALIZER;

/* An .exfat must open as one: "EXFAT   " at offset 3 of its first sector. */
static int exfat_boot_ok(const char *path) {
    uint8_t b[11];
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    int ok = read_at(fd, 0, b, sizeof b) == 0 && memcmp(b + 3, "EXFAT   ", 8) == 0;
    close(fd);
    return ok;
}

static int ends_with_ci(const char *s, const char *suffix) {
    size_t a = strlen(s), b = strlen(suffix);
    return a >= b && strcasecmp(s + a - b, suffix) == 0;
}

/* The output's file name: one plain name with room for ".part" (the check the page route
 * makes too, webui_codec.c). */
static int unp_name_ok(const char *s) { return webui_unpack_name_ok(s); }

/* The checks the unpack makes before it writes anything, for a caller that asks first (the
 * console page): the source policy, the layout, the destination and the free space. */
const char *fsj_ffpfsc_probe(const char *path, fsj_unpack_probe_t *p) {
    char src[1024], dir[1024], dst[1400];
    unp_layout_t L;
    struct stat st;
    struct statvfs sv;
    const char *bad;
    int in;
    memset(p, 0, sizeof *p);
    memset(&L, 0, sizeof L);
    if (snprintf(src, sizeof src, "%s", path) >= (int)sizeof src) return "fs_unpack_path_not_allowed";
    if (fsj_normalize_path(src) != 0 || !may_write_here(src) || !ends_with_ci(src, ".ffpfsc") ||
        strncmp(src, "/mnt/shadowmnt/", 15) == 0)
        return "fs_unpack_path_not_allowed";
    in = open(src, O_RDONLY);
    if (in < 0) return "fs_unpack_open_failed";
    if (fstat(in, &st) != 0 || !S_ISREG(st.st_mode)) {
        close(in);
        return "fs_unpack_open_failed";
    }
    bad = read_layout(in, &L);
    close(in);
    if (bad) return bad;
    if (!unp_inside(L.base, L.stored, (uint64_t)st.st_size)) return "fs_unpack_truncated";
    snprintf(dir, sizeof dir, "%s", src);
    char *slash = strrchr(dir, '/');
    if (!slash) return "fs_unpack_path_not_allowed";
    *slash = 0;
    snprintf(p->name, sizeof p->name, "%s", L.name);
    p->size = L.raw;
    p->need = L.raw + UNP_MARGIN;
    p->compressed = L.compressed;
    snprintf(dst, sizeof dst, "%s/%s", dir, L.name);
    p->exists = lstat(dst, &st) == 0;
    if (statvfs(dir, &sv) == 0) {
        p->free = (uint64_t)sv.f_bavail * sv.f_frsize;
        p->have_free = 1;
    }
    return NULL;
}

int fsj_op_ffpfsc_unpack(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char src[1024], dir[1024], dst[1400], part[1408], resp[1600];
    unp_layout_t L;
    struct stat st;
    struct statvfs sv;
    int in = -1, out = -1, status = AVA1_ERR_IO;
    uint8_t *table = NULL, *buf = NULL;
    char mp[256] = {0}, inner[600], why[320];
    int unit = -1, locked = 0;
    (void)arg;
    (void)n;
    memset(&L, 0, sizeof L);
    part[0] = 0; /* set once the destination is known; only then is there a .part to remove */

    if (!fsj_json_str((const char *)args, "path", src, sizeof src) || !src[0]) {
        ava1_op_message(c, "fs_unpack_missing_path");
        return AVA1_ERR_PROTOCOL;
    }
    if (fsj_normalize_path(src) != 0 || !may_write_here(src) || !ends_with_ci(src, ".ffpfsc") ||
        strncmp(src, "/mnt/shadowmnt/", 15) == 0) {
        ava1_op_message(c, "fs_unpack_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    if (pthread_mutex_trylock(&unp_busy) != 0) {
        ava1_op_message(c, "fs_unpack_busy");
        return AVA1_ERR_BUSY;
    }
    locked = 1;
    in = open(src, O_RDONLY);
    if (in < 0 || fstat(in, &st) != 0 || !S_ISREG(st.st_mode)) {
        ava1_op_message(c, "fs_unpack_open_failed");
        status = AVA1_ERR_PATH;
        goto done;
    }
    const char *bad = read_layout(in, &L);
    if (bad) {
        ava1_op_message(c, "%s", bad);
        status = AVA1_ERR_IO;
        goto done;
    }
    if (!unp_inside(L.base, L.stored, (uint64_t)st.st_size)) {
        ava1_op_message(c, "fs_unpack_truncated");
        goto done;
    }
    /* Beside the source, under the name the image carries inside. */
    snprintf(dir, sizeof dir, "%s", src);
    char *slash = strrchr(dir, '/');
    if (!slash) {
        ava1_op_message(c, "fs_unpack_path_not_allowed");
        status = AVA1_ERR_PATH;
        goto done;
    }
    *slash = 0;
    /* "name" (optional; the console page's Keep both): the image's file name beside the source
     * instead of the one it carries inside. One plain name, never a path. */
    {
        char want[256];
        int rc = fsj_json_str((const char *)args, "name", want, sizeof want);
        /* Present but unusable (too long, not a string, a bad escape): refused, never the
         * image's own name instead. */
        int present = rc || fsj_json_present((const char *)args, "name");
        if (present && (!rc || !unp_name_ok(want))) {
            ava1_op_message(c, "fs_unpack_bad_name");
            status = AVA1_ERR_PROTOCOL;
            goto done;
        }
        /* The image's own name has to leave room for ".part" too. */
        if (!present && !unp_name_ok(L.name)) {
            ava1_op_message(c, "fs_unpack_bad_name");
            status = AVA1_ERR_PROTOCOL;
            goto done;
        }
        snprintf(dst, sizeof dst, "%s/%s", dir, rc ? want : L.name);
    }
    snprintf(part, sizeof part, "%s.part", dst);
    if (!may_write_here(dst) || !may_write_here(part)) {
        ava1_op_message(c, "fs_unpack_path_not_allowed");
        status = AVA1_ERR_PATH;
        goto done;
    }
    if (lstat(dst, &st) == 0) {
        ava1_op_message(c, "fs_unpack_destination_exists");
        status = AVA1_ERR_PATH;
        part[0] = 0; /* nothing of ours to remove */
        goto done;
    }
    if (statvfs(dir, &sv) == 0 && (uint64_t)sv.f_bavail * sv.f_frsize < L.raw + UNP_MARGIN) {
        ava1_op_message(c, "fs_unpack_no_space: needs %llu bytes, %llu free", (unsigned long long)(L.raw + UNP_MARGIN),
                        (unsigned long long)sv.f_bavail * sv.f_frsize);
        goto done;
    }
    out = open(part, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    buf = malloc(UNP_OUT_BUF);
    if (out < 0 || !buf) {
        ava1_op_message(c, out < 0 ? "fs_unpack_create_failed" : "fs_unpack_no_memory");
        goto done;
    }
    ava1_op_set_total(c, 1, L.raw);
    ava1_op_message(c, "unpacking %s", L.name);

    uint64_t written = 0;
    if (!L.compressed) {
        /* Stored as it is: a straight copy. */
        while (written < L.raw) {
            if (ava1_op_cancelled(c)) {
                ava1_op_message(c, "fs_unpack_cancelled");
                status = AVA1_ERR_CANCELLED;
                goto done;
            }
            size_t take = (size_t)((L.raw - written) < UNP_OUT_BUF ? (L.raw - written) : UNP_OUT_BUF);
            if (read_at(in, L.base + written, buf, take) != 0 || write_all(out, buf, take) != 0) {
                ava1_op_message(c, "fs_unpack_io_failed");
                goto done;
            }
            written += take;
            ava1_op_add(c, 0, take);
        }
    } else {
        table = malloc((size_t)(UNP_TABLE_WINDOW + 1) * 8);
        if (!table) {
            ava1_op_message(c, "fs_unpack_no_memory");
            goto done;
        }
        const char *broken = check_container(in, &L, table);
        if (broken) {
            ava1_op_message(c, "%s", broken);
            goto done;
        }
        why[0] = 0;
        if (fs_pfsc_mount_ro(src, "job", part, mp, sizeof mp, &unit, why, sizeof why) != 0) {
            ava1_op_message(c, "%s", why[0] ? why : "fs_unpack_mount_failed");
            goto done;
        }
        snprintf(inner, sizeof inner, "%s/%s", mp, L.name);
        int fin = open(inner, O_RDONLY);
        /* The kernel lists the file rounded up to whole 64 KiB blocks (a 314,585,145-byte image
         * shows as 314,638,336); the inode's raw size is the real length, and only that is copied. */
        if (fin < 0 || fstat(fin, &st) != 0 || (uint64_t)st.st_size < L.raw ||
            (uint64_t)st.st_size >= L.raw + UNP_BLOCK) {
            /* Say what the mount holds instead: the next step depends on it. */
            char seen[200] = {0};
            size_t at = 0;
            DIR *d = opendir(mp);
            struct dirent *e;
            while (d && (e = readdir(d)) && at + 2 < sizeof seen)
                at += (size_t)snprintf(seen + at, sizeof seen - at, "%s ", e->d_name);
            if (d) closedir(d);
            ava1_op_message(c, "fs_unpack_inner_missing: %s %s (size %lld, want %llu) in [%s]", L.name,
                            fin < 0 ? strerror(errno) : "", fin < 0 ? -1LL : (long long)st.st_size,
                            (unsigned long long)L.raw, seen);
            if (fin >= 0) close(fin);
            goto done;
        }
        while (written < L.raw) {
            if (ava1_op_cancelled(c)) {
                ava1_op_message(c, "fs_unpack_cancelled");
                status = AVA1_ERR_CANCELLED;
                close(fin);
                goto done;
            }
            size_t take = (size_t)((L.raw - written) < UNP_OUT_BUF ? (L.raw - written) : UNP_OUT_BUF);
            if (read_at(fin, written, buf, take) != 0) {
                /* The kernel's decoder refused a block: the container is damaged. */
                ava1_op_message(c, "fs_unpack_corrupt_block %llu", (unsigned long long)(written / UNP_BLOCK));
                close(fin);
                goto done;
            }
            if (write_all(out, buf, take) != 0) {
                ava1_op_message(c, "fs_unpack_write_failed");
                close(fin);
                goto done;
            }
            written += take;
            ava1_op_add(c, 0, take);
        }
        close(fin);
        fs_pfsc_release(mp, unit);
        mp[0] = 0;
        unit = -1;
    }
    if (fsync(out) != 0 || close(out) != 0) {
        out = -1;
        ava1_op_message(c, "fs_unpack_write_failed");
        goto done;
    }
    out = -1;
    if (stat(part, &st) != 0 || (uint64_t)st.st_size != L.raw) {
        ava1_op_message(c, "fs_unpack_size_mismatch");
        goto done;
    }
    if (ends_with_ci(L.name, ".exfat") && !exfat_boot_ok(part)) {
        /* Decoded in full, but the image inside is another file system (not a damaged block). */
        fprintf(stderr, "ffpfsc unpack: %s holds %s, which is not an exFAT image; nothing kept\n", src, L.name);
        ava1_op_message(c, "fs_unpack_not_exfat");
        goto done;
    }
    /* Same folder, so the rename never crosses a mount. */
    if (rename(part, dst) != 0) {
        ava1_op_message(c, "fs_unpack_rename_failed");
        goto done;
    }
    {
        int len = snprintf(resp, sizeof resp, "{\"path\":\"%s\",\"size\":%llu}", dst, (unsigned long long)L.raw);
        if (len > 0 && (size_t)len < sizeof resp && !strchr(dst, '"') && !strchr(dst, '\\'))
            (void)ava1_op_set_result(c, resp, (size_t)len);
    }
    ava1_op_message(c, "%s", "");
    status = AVA1_STATUS_OK;
done:
    if (out >= 0) close(out);
    if (in >= 0) close(in);
    if (mp[0] || unit >= 0) fs_pfsc_release(mp, unit);
    if (status != AVA1_STATUS_OK && part[0]) unlink(part);
    if (locked) pthread_mutex_unlock(&unp_busy);
    free(table);
    free(buf);
    return status;
}
