/* See smbd_image.h. Layouts: exFAT per Microsoft's exFAT specification (boot sector, FAT, 0x85 /
 * 0xC0 / 0xC1 entry sets), UFS2 per FreeBSD sys/ufs (the offsets the engine's ufs2.rs verified
 * on real .ffpkg images), PFS as engine/crates/ps5-dump-forge-pfs/src/read.rs reads it
 * (contiguous nodes only, super-root inode 0 listing "uroot"). Runs on a job thread: nothing
 * large on the stack. */
#include "smbd_image.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { IE_READ = -1, IE_DAMAGED = -2, IE_UNSUP = -3, IE_NOMEM = -4, IE_COMPRESSED = -5, IE_UNKNOWN = -6 };
enum { K_EXFAT = 1, K_UFS2, K_PFS };

#define CR_WINDOW 4096u
#define EXFAT_DIR_CLUSTERS 4096u
#define UFS2_SB_OFF 65536u
#define UFS2_MAGIC 0x19540119u
#define PFS_MAGIC 20130315ull
#define PFS_INODE_LEN 0xA8u
#define PFS_DIRENT_FILE 2u
#define PFS_DIRENT_DIR 3u

typedef struct {
    uint64_t size;
    int dir;
    /* exFAT */
    uint32_t first;
    int contig;
    /* UFS2 */
    uint64_t db[12];
    uint64_t ib0;
    /* PFS */
    uint64_t at;
    int compressed;
} ifile_t;

typedef struct {
    pkg_read_fn rd;
    void *ctx;
    uint64_t base; /* where this image starts in the file (a wrapped image's inner one) */
    uint64_t size;
    int kind;
    /* exFAT */
    uint64_t fat_off, heap_off;
    uint32_t csize, ccount;
    uint8_t fat[512];
    uint64_t fat_at; /* the FAT sector held in fat[], UINT64_MAX = none */
    /* UFS2 */
    uint32_t bsize, fsize, ipg, fpg, iblkno, ncg;
    /* PFS */
    uint32_t bs;
    uint64_t ninodes, ndblock;
} img_t;

static uint16_t le16(const uint8_t *b) { return (uint16_t)(b[0] | (b[1] << 8)); }
static uint32_t le32(const uint8_t *b) { return (uint32_t)b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24); }
static uint64_t le64(const uint8_t *b) { return (uint64_t)le32(b) | ((uint64_t)le32(b + 4) << 32); }

static int rd_at(img_t *m, uint64_t off, void *buf, size_t len) {
    if (off > m->size || len > m->size - off) return IE_DAMAGED;
    return m->rd(m->ctx, m->base + off, buf, len) == 0 ? 0 : IE_READ;
}

static int lower(int c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }

/* ---- exFAT ---- */

static int exfat_open(img_t *m, ifile_t *root) {
    uint8_t b[512];
    uint32_t bps_shift, spc_shift, c, n;
    int rc = rd_at(m, 0, b, sizeof b);
    if (rc) return rc == IE_DAMAGED ? IE_UNKNOWN : rc;
    if (memcmp(b + 3, "EXFAT   ", 8) != 0) return IE_UNKNOWN;
    bps_shift = b[0x6C];
    spc_shift = b[0x6D];
    if (bps_shift < 9 || bps_shift > 12 || bps_shift + spc_shift > 25) return IE_DAMAGED;
    m->kind = K_EXFAT;
    m->fat_off = (uint64_t)le32(b + 0x50) << bps_shift;
    m->heap_off = (uint64_t)le32(b + 0x58) << bps_shift;
    m->ccount = le32(b + 0x5C);
    m->csize = 1u << (bps_shift + spc_shift);
    m->fat_at = UINT64_MAX;
    c = le32(b + 0x60);
    if (c < 2 || c - 2 >= m->ccount) return IE_DAMAGED;
    memset(root, 0, sizeof *root);
    root->dir = 1;
    root->first = c;
    /* The root has no size of its own: its cluster chain, counted (bounded). */
    for (n = 1; n < EXFAT_DIR_CLUSTERS; n++) {
        uint64_t off = m->fat_off + (uint64_t)c * 4u, sec = off & ~(uint64_t)511;
        uint32_t next;
        if (sec != m->fat_at) {
            if ((rc = rd_at(m, sec, m->fat, sizeof m->fat)) != 0) return rc;
            m->fat_at = sec;
        }
        next = le32(m->fat + (off - sec));
        if (next < 2 || next - 2 >= m->ccount) break; /* the end of the chain (0xFFFFFFFF) */
        c = next;
    }
    root->size = (uint64_t)n * m->csize;
    if (root->size > IMG_DIR_MAX) root->size = IMG_DIR_MAX;
    return 0;
}

static int exfat_next(img_t *m, uint32_t c, uint32_t *next) {
    uint64_t off = m->fat_off + (uint64_t)c * 4u, sec = off & ~(uint64_t)511;
    int rc;
    if (sec != m->fat_at) {
        if ((rc = rd_at(m, sec, m->fat, sizeof m->fat)) != 0) return rc;
        m->fat_at = sec;
    }
    *next = le32(m->fat + (off - sec));
    return *next >= 2 && *next - 2 < m->ccount ? 0 : IE_DAMAGED;
}

static int exfat_read(img_t *m, const ifile_t *f, uint64_t off, uint8_t *buf, size_t len) {
    uint64_t idx = 0, want;
    uint32_t c = f->first;
    int rc;
    while (len) {
        size_t n;
        uint64_t within;
        want = off / m->csize;
        within = off % m->csize;
        if (f->contig) {
            if (want >= m->ccount || (uint64_t)f->first - 2 + want >= m->ccount) return IE_DAMAGED;
            c = (uint32_t)(f->first + want);
        } else {
            if (want < idx) {
                idx = 0;
                c = f->first;
            }
            for (; idx < want; idx++)
                if ((rc = exfat_next(m, c, &c)) != 0) return rc;
        }
        if (c < 2 || c - 2 >= m->ccount) return IE_DAMAGED;
        n = len;
        if (n > m->csize - within) n = (size_t)(m->csize - within);
        if ((rc = rd_at(m, m->heap_off + (uint64_t)(c - 2) * m->csize + within, buf, n)) != 0) return rc;
        buf += n;
        off += n;
        len -= n;
    }
    return 0;
}

/* ---- UFS2 ---- */

static int ufs2_inode(img_t *m, uint64_t n, ifile_t *f) {
    uint8_t b[256];
    uint64_t off;
    uint16_t mode;
    int rc, i;
    if (n == 0 || n >= (uint64_t)m->ncg * m->ipg) return IE_DAMAGED;
    off = (n / m->ipg) * m->fpg * (uint64_t)m->fsize + (uint64_t)m->iblkno * m->fsize + (n % m->ipg) * 256u;
    if ((rc = rd_at(m, off, b, sizeof b)) != 0) return rc;
    memset(f, 0, sizeof *f);
    mode = le16(b);
    f->dir = (mode & 0170000) == 0040000;
    if (!f->dir && (mode & 0170000) != 0100000) return IE_DAMAGED; /* neither a file nor a folder */
    f->size = le64(b + 16);
    for (i = 0; i < 12; i++) f->db[i] = le64(b + 112 + i * 8);
    f->ib0 = le64(b + 208);
    return 0;
}

static int ufs2_open(img_t *m, ifile_t *root) {
    uint8_t *b;
    int rc;
    if (m->size < UFS2_SB_OFF + 1376u) return IE_UNKNOWN;
    b = malloc(1376);
    if (!b) return IE_NOMEM;
    rc = rd_at(m, UFS2_SB_OFF, b, 1376);
    if (rc == 0 && le32(b + 1372) != UFS2_MAGIC) rc = IE_UNKNOWN;
    if (rc == 0) {
        m->iblkno = le32(b + 16);
        m->ncg = le32(b + 44);
        m->bsize = le32(b + 48);
        m->fsize = le32(b + 52);
        m->ipg = le32(b + 184);
        m->fpg = le32(b + 188);
        if (m->fsize < 512 || m->fsize > 262144 || (m->fsize & (m->fsize - 1)) || m->bsize < m->fsize ||
            m->bsize > 262144 || (m->bsize & (m->bsize - 1)) || m->bsize / m->fsize > 8 || !m->ipg || !m->fpg || !m->ncg ||
            (uint64_t)m->iblkno * m->fsize + (uint64_t)m->ipg * 256u > (uint64_t)m->fpg * m->fsize)
            rc = IE_DAMAGED;
    }
    free(b);
    if (rc) return rc;
    m->kind = K_UFS2;
    rc = ufs2_inode(m, 2, root);
    if (rc == 0 && !root->dir) rc = IE_DAMAGED;
    return rc;
}

static int ufs2_read(img_t *m, const ifile_t *f, uint64_t off, uint8_t *buf, size_t len) {
    int rc;
    while (len) {
        uint64_t lbn = off / m->bsize, within = off % m->bsize, frag;
        size_t n = len;
        if (n > m->bsize - within) n = (size_t)(m->bsize - within);
        if (lbn < 12) {
            frag = f->db[lbn];
        } else if (lbn - 12 < m->bsize / 8u) {
            uint8_t p[8];
            if (!f->ib0) {
                frag = 0;
            } else {
                if (f->ib0 > UINT64_MAX / m->fsize) return IE_DAMAGED;
                if ((rc = rd_at(m, f->ib0 * m->fsize + (lbn - 12) * 8u, p, 8)) != 0) return rc;
                frag = le64(p);
            }
        } else {
            return IE_UNSUP; /* past the single indirect block: never a param file or an icon */
        }
        if (!frag) {
            memset(buf, 0, n); /* a hole */
        } else {
            if (frag > UINT64_MAX / m->fsize) return IE_DAMAGED;
            if ((rc = rd_at(m, frag * m->fsize + within, buf, n)) != 0) return rc;
        }
        buf += n;
        off += n;
        len -= n;
    }
    return 0;
}

/* ---- PFS ---- */

static int pfs_inode(img_t *m, uint64_t n, ifile_t *f) {
    uint8_t d[PFS_INODE_LEN];
    uint64_t per = m->bs / PFS_INODE_LEN, size, size_c, blocks;
    uint32_t flags, fill;
    int32_t db0;
    uint16_t mode;
    int rc, k;
    if (n >= m->ninodes) return IE_DAMAGED;
    if ((rc = rd_at(m, (1 + n / per) * m->bs + (n % per) * PFS_INODE_LEN, d, sizeof d)) != 0) return rc;
    memset(f, 0, sizeof *f);
    mode = le16(d);
    flags = le32(d + 4);
    size = le64(d + 8);
    size_c = le64(d + 16);
    blocks = le32(d + 0x60);
    db0 = (int32_t)le32(d + 0x64);
    f->dir = (mode & 0x4000) != 0;
    if (!f->dir && !(mode & 0x8000)) return IE_DAMAGED;
    f->compressed = (flags & 0x1u) != 0;
    /* one contiguous run from db[0]: every other pointer -1 (0 on the super-root), no indirect */
    fill = le32(d + 0x68);
    for (k = 1; k < 12; k++)
        if (le32(d + 0x64 + k * 4) != fill) return IE_UNSUP;
    for (k = 0; k < 5; k++)
        if (le32(d + 0x94 + k * 4)) return IE_UNSUP;
    if (!(fill == 0xFFFFFFFFu || (n == 0 && fill == 0))) return IE_UNSUP;
    if (!f->compressed && size != size_c) return IE_UNSUP;
    if (db0 <= 0 || (uint64_t)db0 + blocks > m->ndblock) return IE_DAMAGED;
    f->at = (uint64_t)db0 * m->bs;
    f->size = f->compressed ? size_c : size; /* the logical size; only an uncompressed file is read */
    if (f->at > m->size || size > m->size - f->at) return IE_DAMAGED;
    return 0;
}

static int pfs_open(img_t *m, ifile_t *sroot) {
    uint8_t h[0x50];
    uint64_t version;
    uint16_t mode;
    int rc = rd_at(m, 0, h, sizeof h);
    if (rc) return rc == IE_DAMAGED ? IE_UNKNOWN : rc;
    version = le64(h);
    if (version < 1 || version > 2 || le64(h + 8) != PFS_MAGIC) return IE_UNKNOWN;
    mode = le16(h + 0x1C);
    if (mode & 0x7) return IE_UNSUP; /* signed, 64-bit inodes, encrypted */
    m->bs = le32(h + 0x20);
    if (m->bs < 0x1000 || m->bs > 0x10000 || (m->bs & (m->bs - 1))) return IE_DAMAGED;
    m->ninodes = le64(h + 0x30);
    m->ndblock = le64(h + 0x38);
    if (!m->ninodes) return IE_DAMAGED;
    m->kind = K_PFS;
    rc = pfs_inode(m, 0, sroot);
    if (rc == 0 && (!sroot->dir || sroot->compressed)) rc = IE_DAMAGED;
    return rc;
}

static int pfs_read(img_t *m, const ifile_t *f, uint64_t off, uint8_t *buf, size_t len) {
    if (f->compressed) return IE_COMPRESSED;
    return rd_at(m, f->at + off, buf, len);
}

/* ---- files and folders, any of the three ---- */

static int file_read(img_t *m, const ifile_t *f, uint64_t off, uint8_t *buf, size_t len) {
    if (off > f->size || len > f->size - off) return IE_DAMAGED;
    if (m->kind == K_EXFAT) return exfat_read(m, f, off, buf, len);
    if (m->kind == K_UFS2) return ufs2_read(m, f, off, buf, len);
    return pfs_read(m, f, off, buf, len);
}

/* A folder's bytes through a 4 KiB window. */
typedef struct {
    img_t *m;
    const ifile_t *f;
    uint64_t at;
    size_t have;
    uint8_t buf[CR_WINDOW];
} cr_t;

static int cr_get(cr_t *c, uint64_t off, uint8_t *out, size_t len) {
    uint64_t end = c->f->size < IMG_DIR_MAX ? c->f->size : IMG_DIR_MAX;
    if (len > CR_WINDOW || off > end || len > end - off) return IE_DAMAGED;
    if (off < c->at || off + len > c->at + c->have) {
        size_t n = end - off < CR_WINDOW ? (size_t)(end - off) : CR_WINDOW;
        int rc = file_read(c->m, c->f, off, c->buf, n);
        if (rc) return rc;
        c->at = off;
        c->have = n;
    }
    memcpy(out, c->buf + (off - c->at), len);
    return 0;
}

static int name_eq(const uint8_t *a, size_t n, const char *b) {
    size_t i;
    if (strlen(b) != n) return 0;
    for (i = 0; i < n; i++)
        if (lower(a[i]) != lower((unsigned char)b[i])) return 0;
    return 1;
}

/* exFAT: the entry set named `name` (ASCII, any case). 1 found, 0 not, < 0 an error. */
static int exfat_lookup(cr_t *c, const char *name, ifile_t *out) {
    uint8_t e[32], s[32];
    uint64_t pos = 0, end = c->f->size < IMG_DIR_MAX ? c->f->size : IMG_DIR_MAX;
    size_t want = strlen(name);
    int rc;
    while (pos + 32 <= end) {
        unsigned sc, i, nlen;
        uint8_t nm[255];
        int ok = 1;
        if ((rc = cr_get(c, pos, e, 32)) != 0) return rc;
        if (e[0] == 0x00) return 0; /* the end of the folder */
        if (e[0] != 0x85) {
            pos += 32;
            continue;
        }
        sc = e[1];
        if (sc < 2 || sc > 18 || pos + 32u * (1 + sc) > end) return IE_DAMAGED;
        if ((rc = cr_get(c, pos + 32, s, 32)) != 0) return rc;
        if (s[0] != 0xC0) return IE_DAMAGED;
        nlen = s[3];
        if (nlen == 0 || (nlen + 14) / 15 > sc - 1) return IE_DAMAGED;
        if (nlen == want) {
            for (i = 0; i < nlen && ok; i++) {
                uint8_t n2[32];
                unsigned ent = 2 + i / 15, at = 2 + (i % 15) * 2;
                uint16_t u;
                if ((rc = cr_get(c, pos + 32u * ent, n2, 32)) != 0) return rc;
                if (n2[0] != 0xC1) return IE_DAMAGED;
                u = le16(n2 + at);
                if (u >= 0x80) ok = 0;
                else nm[i] = (uint8_t)u;
            }
            if (ok && name_eq(nm, nlen, name)) {
                memset(out, 0, sizeof *out);
                out->dir = (le16(e + 4) & 0x10) != 0;
                out->contig = (s[1] & 0x02) != 0;
                out->first = le32(s + 20);
                out->size = le64(s + 24);
                if (out->size && (out->first < 2 || out->first - 2 >= c->m->ccount)) return IE_DAMAGED;
                return 1;
            }
        }
        pos += 32u * (1 + sc);
    }
    return 0;
}

static int ufs2_lookup(cr_t *c, const char *name, ifile_t *out) {
    uint8_t h[8], nm[255];
    uint64_t pos = 0, end = c->f->size < IMG_DIR_MAX ? c->f->size : IMG_DIR_MAX;
    int rc;
    while (pos + 8 <= end) {
        uint32_t ino;
        uint16_t reclen;
        uint8_t nlen;
        if ((rc = cr_get(c, pos, h, 8)) != 0) return rc;
        ino = le32(h);
        reclen = le16(h + 4);
        nlen = h[7];
        if (reclen < 8 || pos + reclen > end || (ino && (unsigned)nlen + 8 > reclen)) return IE_DAMAGED;
        if (ino && nlen == strlen(name)) {
            if ((rc = cr_get(c, pos + 8, nm, nlen)) != 0) return rc;
            if (name_eq(nm, nlen, name)) return ufs2_inode(c->m, ino, out) == 0 ? 1 : IE_DAMAGED;
        }
        pos += reclen;
    }
    return 0;
}

/* PFS: `name`, or (name NULL) the folder's only file into out and its name into `only` (cap 256). */
static int pfs_lookup(cr_t *c, const char *name, ifile_t *out, char *only) {
    uint8_t h[16], nm[256];
    uint64_t pos = 0, end = c->f->size < IMG_DIR_MAX ? c->f->size : IMG_DIR_MAX;
    unsigned files = 0;
    int rc, found = 0;
    while (pos + 16 <= end) {
        uint32_t ino, type, nlen, len;
        static const uint8_t zero[16];
        if ((rc = cr_get(c, pos, h, 16)) != 0) return rc;
        if (!memcmp(h, zero, 16)) break;
        ino = le32(h);
        type = le32(h + 4);
        nlen = le32(h + 8);
        len = le32(h + 12);
        if (nlen == 0 || nlen > 255 || len % 8 || len < 16 + nlen || pos + len > end) return IE_DAMAGED;
        if (type == PFS_DIRENT_FILE || type == PFS_DIRENT_DIR) {
            if ((rc = cr_get(c, pos + 16, nm, nlen)) != 0) return rc;
            if (name && name_eq(nm, nlen, name)) {
                if ((rc = pfs_inode(c->m, ino, out)) != 0) return rc;
                return (type == PFS_DIRENT_DIR) == (out->dir != 0) ? 1 : IE_DAMAGED;
            }
            if (!name && type == PFS_DIRENT_FILE && ++files == 1) {
                if ((rc = pfs_inode(c->m, ino, out)) != 0) return rc;
                memcpy(only, nm, nlen);
                only[nlen] = '\0';
                found = 1;
            }
            if (!name && type == PFS_DIRENT_DIR) return 0; /* a game tree, not a wrapped image */
        }
        pos += len;
    }
    return !name && found && files == 1 ? 1 : 0;
}

static int lookup(img_t *m, cr_t *c, const ifile_t *dir, const char *name, ifile_t *out) {
    c->m = m;
    c->f = dir;
    c->at = 0;
    c->have = 0;
    if (m->kind == K_EXFAT) return exfat_lookup(c, name, out);
    if (m->kind == K_UFS2) return ufs2_lookup(c, name, out);
    return pfs_lookup(c, name, out, NULL);
}

static const char *cause(int rc) {
    switch (rc) {
    case IE_READ: return "img_read";
    case IE_UNSUP: return "img_unsupported";
    case IE_NOMEM: return "img_no_memory";
    case IE_COMPRESSED: return "img_compressed";
    case IE_UNKNOWN: return "img_unknown";
    default: return "img_damaged";
    }
}

static int ends_ci(const char *s, const char *suffix) {
    size_t n = strlen(s), k = strlen(suffix), i;
    if (n <= k) return 0;
    for (i = 0; i < k; i++)
        if (lower((unsigned char)s[n - k + i]) != suffix[i]) return 0;
    return 1;
}

/* A whole small file into a fresh heap buffer. */
static int read_whole(img_t *m, const ifile_t *f, uint8_t **out) {
    uint8_t *b = malloc((size_t)f->size + 1);
    int rc;
    *out = NULL;
    if (!b) return IE_NOMEM;
    if ((rc = file_read(m, f, 0, b, (size_t)f->size)) != 0) {
        free(b);
        return rc;
    }
    b[f->size] = 0;
    *out = b;
    return 0;
}

static const char *read_in(img_t *m, int depth, size_t icon_max, img_meta_t *out, cr_t *c) {
    ifile_t *root = calloc(4, sizeof *root), *sys, *f, *sroot;
    uint8_t *meta = NULL;
    const char *why = NULL;
    int rc, is_json = 1;
    if (!root) return "img_no_memory";
    sys = root + 1;
    f = root + 2;
    sroot = root + 3;
    rc = exfat_open(m, root);
    if (rc == IE_UNKNOWN) rc = pfs_open(m, sroot);
    if (rc == IE_UNKNOWN) rc = ufs2_open(m, root);
    if (rc) {
        why = cause(rc);
        goto out;
    }
    if (m->kind == K_PFS) {
        rc = lookup(m, c, sroot, "uroot", root);
        if (rc != 1 || !root->dir) {
            why = rc < 0 ? cause(rc) : "img_damaged";
            goto out;
        }
    }
    rc = lookup(m, c, root, "sce_sys", sys);
    if (rc == 0 && m->kind == K_PFS) {
        /* No game tree: a wrapped image, one file in the root (a .ffpfsc's, compressed). */
        char *only = malloc(256);
        if (!only) {
            why = "img_no_memory";
            goto out;
        }
        c->m = m;
        c->f = root;
        c->at = 0;
        c->have = 0;
        rc = pfs_lookup(c, NULL, f, only);
        if (rc == 1 && f->compressed) {
            why = "img_compressed";
        } else if (rc == 1 && depth == 0 && (ends_ci(only, ".exfat") || ends_ci(only, ".ffpkg"))) {
            img_t *in = calloc(1, sizeof *in);
            if (!in) {
                why = "img_no_memory";
            } else {
                in->rd = m->rd;
                in->ctx = m->ctx;
                in->base = m->base + f->at;
                in->size = f->size;
                why = read_in(in, depth + 1, icon_max, out, c);
                free(in);
            }
        } else {
            why = rc < 0 ? cause(rc) : "img_no_param";
        }
        free(only);
        goto out;
    }
    if (rc != 1 || !sys->dir) {
        why = rc < 0 ? cause(rc) : "img_no_param";
        goto out;
    }
    rc = lookup(m, c, sys, "param.json", f);
    if (rc != 1 || f->dir) {
        if (rc < 0 && rc != IE_DAMAGED) {
            why = cause(rc);
            goto out;
        }
        is_json = 0;
        rc = lookup(m, c, sys, "param.sfo", f);
    }
    if (rc != 1 || f->dir || f->size == 0 || f->size > PKG_INFO_MAX_META) {
        why = rc < 0 ? cause(rc) : rc == 1 && f->size > PKG_INFO_MAX_META ? "img_damaged" : "img_no_param";
        goto out;
    }
    if ((rc = read_whole(m, f, &meta)) != 0) {
        why = cause(rc);
        goto out;
    }
    pkg_info_from_meta(meta, (size_t)f->size, is_json, &out->info);
    snprintf(out->format, sizeof out->format, "%s", m->kind == K_EXFAT ? "exfat" : m->kind == K_UFS2 ? "ufs2" : "pfs");
    /* the cover: optional, so a failure here costs only the icon */
    if (icon_max && lookup(m, c, sys, "icon0.png", f) == 1 && !f->dir && f->size >= 8 && f->size <= icon_max) {
        static const uint8_t sig[8] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n' };
        uint8_t *png = NULL;
        if (read_whole(m, f, &png) == 0 && memcmp(png, sig, 8) == 0) {
            out->icon = png;
            out->icon_len = (size_t)f->size;
            png = NULL;
        }
        free(png);
    }
out:
    free(meta);
    free(root);
    return why;
}

const char *img_meta_read(pkg_read_fn rd, void *ctx, uint64_t size, size_t icon_max, img_meta_t *out) {
    img_t *m;
    cr_t *c;
    const char *why;
    memset(out, 0, sizeof *out);
    m = calloc(1, sizeof *m);
    c = malloc(sizeof *c);
    if (!m || !c) {
        free(m);
        free(c);
        return "img_no_memory";
    }
    m->rd = rd;
    m->ctx = ctx;
    m->size = size;
    why = read_in(m, 0, icon_max, out, c);
    if (why) img_meta_free(out);
    free(m);
    free(c);
    return why;
}

void img_meta_free(img_meta_t *m) {
    free(m->icon);
    memset(m, 0, sizeof *m);
}
