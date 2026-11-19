/* RAR (1.5 to 5, single or multi-volume) through libarchive's readers.
 *
 * Why not 7-Zip's RAR decoders, which the rest of the extractor uses: those are derived from the
 * UnRAR source, whose license adds a restriction the GPL does not allow, and this ELF also links
 * GPL-3.0 code that is not ours to relicense (the payload SDK's startup code). libarchive's RAR and
 * RAR5 readers are independent implementations under the BSD license. They do not decrypt: an
 * encrypted RAR is refused with fs_extract_rar_encrypted, which the apps explain.
 *
 * Progress is in archive bytes read. Free space is checked per file from its header, and, for an
 * archive that is not solid, for all of it before anything is written (#869: a 150 GB set ran for
 * 2 h 21 min and filled the drive at 99%). The total comes from one pass over the headers, which in
 * an archive that is not solid skips each file's data without decoding it. A solid archive would be
 * decoded twice for that, so it keeps the per-file check only. */
#include "xjob.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "archive.h"
#include "archive_entry.h"
#include "archive_name.h"

#define XRAR_BLOCK (256u << 10)

/* Where the volumes on disk end, for telling a missing volume from damage. */
typedef struct {
    size_t count;
    uint64_t last_starts; /* the last volume's first byte, counted across the set */
    int is_set;           /* x.partN.rar, or more than one volume found */
    char next[XJ_PATH];   /* the name the next volume would have */
} xrar_vols_t;

/* A libarchive error as one of the extractor's cause tokens. An error while reading the last volume
 * found, of a set, is the next volume missing: the set only says it goes on in that volume's header,
 * which the reader then fails to follow. */
static int rar_fail(xjob_t *j, struct archive *a, const xrar_vols_t *v) {
    const char *msg = archive_error_string(a);
    const char *next = strrchr(v->next, '/');
    if (xj_failed(j)) return -1;
    if (!msg) msg = "";
    if (strstr(msg, "ncrypt")) return xj_fail(j, "fs_extract_rar_encrypted");
    if (strstr(msg, "subsequent volume") ||
        (v->is_set && (uint64_t)archive_filter_bytes(a, -1) >= v->last_starts && !strstr(msg, "CRC") &&
         !strstr(msg, "Checksum")))
        return xj_fail(j, "fs_extract_missing_volume: %s", next ? next + 1 : v->next);
    if (strstr(msg, "Truncated") || strstr(msg, "Premature")) return xj_fail(j, "fs_extract_truncated: %s", msg);
    if (strstr(msg, "allocat") || strstr(msg, "memory")) return xj_fail(j, "fs_extract_no_memory: %s", msg);
    if (strstr(msg, "nsupported") || strstr(msg, "No support")) return xj_fail(j, "fs_extract_unsupported_method: %s", msg);
    if (archive_errno(a) == ENOENT) return xj_fail(j, "fs_extract_open_failed: %s", msg);
    return xj_fail(j, "fs_extract_corrupt: %s", msg);
}

static uint64_t vint(const unsigned char *p, size_t n, size_t *at, int *ok) {
    uint64_t v = 0;
    for (int shift = 0; shift < 64 && *at < n; shift += 7) {
        unsigned char c = p[(*at)++];
        v |= (uint64_t)(c & 0x7f) << shift;
        if (!(c & 0x80)) return v;
    }
    *ok = 0;
    return 0;
}

/* Whether the archive is solid, from its main header: 1, 0, or -1 when that cannot be told (a
 * self-extractor, encrypted headers, not a RAR). `p` is the first volume's first `n` bytes. */
static int xrar_solid(const unsigned char *p, size_t n) {
    static const unsigned char sig5[8] = {'R', 'a', 'r', '!', 0x1a, 0x07, 0x01, 0x00};
    static const unsigned char sig4[7] = {'R', 'a', 'r', '!', 0x1a, 0x07, 0x00};
    if (n >= sizeof sig5 && memcmp(p, sig5, sizeof sig5) == 0) {
        /* CRC32, header size, type, header flags, [extra size], [data size], archive flags */
        size_t at = sizeof sig5 + 4;
        int ok = 1;
        uint64_t type, hflags, aflags;
        (void)vint(p, n, &at, &ok);
        type = vint(p, n, &at, &ok);
        hflags = vint(p, n, &at, &ok);
        if (hflags & 1) (void)vint(p, n, &at, &ok);
        if (hflags & 2) (void)vint(p, n, &at, &ok);
        aflags = vint(p, n, &at, &ok);
        if (!ok || type != 1) return -1;
        return (aflags & 0x4) ? 1 : 0;
    }
    if (n >= sizeof sig4 + 5 && memcmp(p, sig4, sizeof sig4) == 0) {
        /* CRC16, type (0x73 = main), flags */
        unsigned flags = (unsigned)p[sizeof sig4 + 3] | ((unsigned)p[sizeof sig4 + 4] << 8);
        if (p[sizeof sig4 + 2] != 0x73 || (flags & 0x0080)) return -1;
        return (flags & 0x0008) ? 1 : 0;
    }
    return -1;
}

/* The unpacked size of every file in a set that is not solid, or 0 when it is not known: a solid
 * or unreadable archive, a file without a size, a cancel. Never sets the job's cause: whatever is
 * wrong with the archive, the extraction itself says so. */
static uint64_t rar_total(xjob_t *j, char **vols) {
    unsigned char head[64];
    struct archive *a;
    struct archive_entry *ae;
    uint64_t total = 0;
    size_t n;
    FILE *f = fopen(vols[0], "rb");
    if (!f) return 0;
    n = fread(head, 1, sizeof head, f);
    fclose(f);
    if (xrar_solid(head, n) != 0) return 0;
    a = archive_read_new();
    if (!a) return 0;
    archive_read_support_format_rar(a);
    archive_read_support_format_rar5(a);
    if (archive_read_open_filenames(a, (const char **)vols, XRAR_BLOCK) != ARCHIVE_OK) goto unknown;
    for (;;) {
        int r = archive_read_next_header(a, &ae);
        if (r == ARCHIVE_EOF) break;
        if (r != ARCHIVE_OK && r != ARCHIVE_WARN) goto unknown;
        if (j->cancelled && j->cancelled(j)) goto unknown;
        if (archive_entry_filetype(ae) == AE_IFREG && !archive_entry_hardlink(ae)) {
            if (!archive_entry_size_is_set(ae) || archive_entry_size(ae) < 0) goto unknown;
            total += (uint64_t)archive_entry_size(ae);
        }
        if (archive_read_data_skip(a) != ARCHIVE_OK) goto unknown;
    }
    archive_read_close(a);
    archive_read_free(a);
    return total;
unknown:
    archive_read_close(a);
    archive_read_free(a);
    return 0;
}

static int kind_of(struct archive_entry *e) {
    switch (archive_entry_filetype(e)) {
    case AE_IFREG: return archive_entry_hardlink(e) ? XE_LINK : XE_FILE;
    case AE_IFDIR: return XE_DIR;
    case AE_IFLNK: return XE_LINK;
    default: return XE_OTHER;
    }
}

int xrar_extract(xjob_t *j) {
    char **vols = xj_rar_volumes(j->archive);
    struct archive *a = NULL;
    struct archive_entry *ae;
    xrar_vols_t vi;
    size_t at, len;
    int rc = -1, opened_dest = 0;
    if (!vols) return xj_fail(j, "fs_extract_no_memory");
    memset(&vi, 0, sizeof vi);
    for (char **v = vols; *v; v++) {
        struct stat st;
        vi.last_starts = j->bytes_total;
        if (stat(*v, &st) == 0) j->bytes_total += (uint64_t)st.st_size;
        vi.count++;
    }
    vi.is_set = vi.count > 1 || archive_rar_part(j->archive, &at, &len);
    if (xj_rar_volume_name(j->archive, (unsigned)vi.count, vi.next, sizeof vi.next) != 0) vi.next[0] = 0;
    /* All of it has to fit, before the folder is made (a merge may skip files it already has, so
     * it keeps the per-file check only). */
    if (vi.count && !j->merge) {
        uint64_t total = rar_total(j, vols);
        if (total && xs_check_space(j, total) != 0) {
            xj_free_list(vols);
            return -1;
        }
    }
    a = archive_read_new();
    if (!a) {
        xj_free_list(vols);
        return xj_fail(j, "fs_extract_no_memory");
    }
    archive_read_support_format_rar(a);
    archive_read_support_format_rar5(a);
    if (archive_read_open_filenames(a, (const char **)vols, XRAR_BLOCK) != ARCHIVE_OK) {
        rar_fail(j, a, &vi);
        goto out;
    }
    for (;;) {
        int r = archive_read_next_header(a, &ae);
        if (r == ARCHIVE_EOF) break;
        if (r != ARCHIVE_OK && r != ARCHIVE_WARN) {
            /* No reader took it: not a RAR at all. */
            const char *msg = archive_error_string(a);
            if (!opened_dest && msg && strstr(msg, "Unrecognized")) xj_fail(j, "fs_extract_not_archive");
            rar_fail(j, a, &vi);
            goto out;
        }
        if (archive_entry_is_encrypted(ae)) {
            xj_fail(j, "fs_extract_rar_encrypted");
            goto out;
        }
        if (!opened_dest) {
            if (xs_open_dest(j) != 0) goto out;
            opened_dest = 1;
        }
        {
            const char *name = archive_entry_pathname_utf8(ae);
            int kind = kind_of(ae);
            uint64_t size = archive_entry_size_is_set(ae) && archive_entry_size(ae) > 0 ? (uint64_t)archive_entry_size(ae) : 0;
            int64_t mtime = archive_entry_mtime_is_set(ae) ? (int64_t)archive_entry_mtime(ae) : -1;
            xentry_t e;
            int b;
            if (!name) name = archive_entry_pathname(ae);
            if (!name) {
                j->unsafe++;
                continue;
            }
            b = xs_begin(j, &e, name, kind, size, mtime);
            if (b == XS_FAIL) goto out;
            if (b == XS_SKIP) {
                if (archive_read_data_skip(a) != ARCHIVE_OK) {
                    rar_fail(j, a, &vi);
                    goto out;
                }
                j->bytes_done = (uint64_t)archive_filter_bytes(a, -1);
                continue;
            }
            for (;;) {
                const void *buf;
                size_t len;
                la_int64_t off;
                r = archive_read_data_block(a, &buf, &len, &off);
                if (r == ARCHIVE_EOF) break;
                if (r != ARCHIVE_OK && r != ARCHIVE_WARN) {
                    rar_fail(j, a, &vi);
                    xs_end(j, &e, 0);
                    goto out;
                }
                /* A gap (a sparse run) is written as zeros, in small steps. */
                while ((uint64_t)off > e.written) {
                    static const char zeros[4096];
                    uint64_t gap = (uint64_t)off - e.written;
                    if (xs_write(j, &e, zeros, gap < sizeof zeros ? (size_t)gap : sizeof zeros) != 0) {
                        xs_end(j, &e, 0);
                        goto out;
                    }
                }
                if (len && xs_write(j, &e, buf, len) != 0) {
                    xs_end(j, &e, 0);
                    goto out;
                }
                j->bytes_done = (uint64_t)archive_filter_bytes(a, -1);
            }
            if (xs_end(j, &e, 1) != 0) goto out;
        }
    }
    if (!opened_dest) {
        /* An archive with nothing in it still gets its (empty) folder. */
        if (xs_open_dest(j) != 0) goto out;
    }
    j->bytes_done = j->bytes_total;
    rc = 0;
out:
    if (a) {
        archive_read_close(a);
        archive_read_free(a);
    }
    xj_free_list(vols);
    if (rc != 0 && !xj_failed(j)) xj_fail(j, "fs_extract_corrupt");
    return rc;
}
