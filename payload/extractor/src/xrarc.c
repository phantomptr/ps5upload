/* Password-protected RAR as a plain RAR stream for libarchive (see xrarc.h).
 *
 * The stream is built as a queue of segments, one header at a time, as libarchive asks for more:
 *   MEM  bytes held here (a header that was decrypted or changed);
 *   RAW  a range of a volume, as it is (a header left alone, data that is not encrypted);
 *   DEC  a range of a volume holding encrypted file data, decrypted on the way.
 * Lengths do not change except in headers: AES-CBC keeps the data's size, so every size in the
 * headers stays true. Encrypted data is padded to whole 16-byte blocks: at the end of a stored
 * (uncompressed) file the padding is cut off and the header's size lowered to match; an empty
 * file's one block of padding is dropped and the file marked stored; after a compressed file it is
 * left, and libarchive's RAR5 reader ends the file at its last block (a PS5Upload change there; the
 * RAR 3/4 reader already does).
 *
 * A file's encrypted data is one AES-CBC stream even when it is split over volumes, and the split
 * falls anywhere, not on a 16-byte block. A block that straddles two volumes is decrypted when the
 * first volume's part ends, with the bytes it needs read ahead from the next part (whose headers are
 * read then); its plaintext tail is handed out at the start of that next part.
 *
 * The format, as the parts used here: RAR5 (rar 5.0+): a header is CRC32, size (vint), type, flags,
 * [extra size], [data size], fields, extra records. With encrypted headers, an encryption header
 * (type 4: version, flags, KDF count, salt, check value) follows the signature, and every later
 * header is a 16-byte IV then the header encrypted with AES-256-CBC, padded to 16. A file's data is
 * encrypted when its header has an encryption record (extra type 1: version, flags, KDF count, salt,
 * IV, check value). RAR 3/4: a block is CRC16, type, flags, size [, data size]; a file block with
 * flag 0x04 has its data encrypted with AES-128-CBC (key and IV from the password and the block's
 * salt); a main block with flag 0x80 means every later block is an 8-byte salt then the block
 * encrypted the same way, padded to 16. */
#include "xrarc.h"

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "../C/7zCrc.h"
#include "xrcrypt.h"

#define BUF_SIZE (256u << 10)
#define HDR_MAX (2u << 20) /* RAR5's largest header; RAR 3/4 headers are under 64 KiB */
#define SFX_SCAN (4u << 20)
#define OPEN_SCAN_MAX 4096
/* An extra record type no reader knows, written over the encryption record's type (libarchive skips
 * records it does not know). */
#define NEUTRAL_TYPE 0x7f

enum { SEG_MEM, SEG_RAW, SEG_DEC };

typedef struct fstream {
    xrc_aes_t aes; /* first: aligned (the struct is allocated aligned) */
    int refs;
    uint64_t packed; /* bytes of data in the parts before this one */
    xrarc_ent_t *ent;
    void *raw; /* the allocation */
} fstream_t;

typedef struct {
    int kind;
    unsigned vol;
    uint64_t off, len; /* RAW, DEC: in the volume; MEM: len */
    uint64_t out_len;  /* bytes handed out: len, less the cut padding (DEC) */
    uint8_t *mem;
    fstream_t *fs;
    int last;           /* DEC: the file's last part */
    uint64_t skip;      /* DEC: bytes at its start already decrypted (read ahead) ... */
    uint8_t pend[16];   /* ... and their plaintext, handed out first */
    unsigned pend_n;
} seg_t;

struct xrarc {
    char **vols;
    size_t nvols;
    const char *pw;
    int fmt; /* 4 or 5 */
    int err;
    int any_enc, confirmed, solid;
    int hdr_ok; /* an encrypted header decrypted with a good checksum */

    /* Reading the headers. */
    unsigned scan_vol;
    int vol_open;       /* the signature of scan_vol was handled */
    int hdr_enc;        /* this volume's headers are encrypted */
    uint8_t hkey[32];   /* RAR5: their key */
    uint64_t scan_pos, scan_size;
    int scan_done;
    fstream_t *split_fs; /* the encrypted file that goes on in the next volume */

    seg_t *segs;
    size_t nseg, cap, at; /* `at`: the one being handed out */
    uint64_t pos, outp;   /* in segs[at]: input (bytes of it read) and output so far */
    int pend_done;

    xrarc_ent_t *q_head, *q_tail;

    int fd[2];
    unsigned fd_vol[2];
    unsigned fd_next;

    uint8_t *buf, *hb; /* aligned: BUF_SIZE, HDR_MAX + 64 */
    void *buf_raw, *hb_raw;
};

/* ---- small helpers ---- */

static void *amalloc(size_t n, void **raw) {
    uint8_t *p = malloc(n + 16);
    if (!p) return NULL;
    *raw = p;
    return (void *)(((uintptr_t)p + 15) & ~(uintptr_t)15);
}

static int fail(xrarc_t *t, int e) {
    if (!t->err) t->err = e;
    return -1;
}

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }
static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static void put16(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}
static void put32(uint8_t *p, uint32_t v) {
    put16(p, v);
    put16(p + 2, v >> 16);
}

static int vget(const uint8_t *p, size_t n, size_t *at, uint64_t *v) {
    uint64_t r = 0;
    for (unsigned sh = 0; sh < 70 && *at < n; sh += 7) {
        uint8_t c = p[(*at)++];
        if (sh < 64) r |= (uint64_t)(c & 0x7f) << sh;
        if (!(c & 0x80)) {
            *v = r;
            return 0;
        }
    }
    return -1;
}

static size_t vput(uint8_t *p, uint64_t v) {
    size_t n = 0;
    do {
        uint8_t b = (uint8_t)(v & 0x7f);
        v >>= 7;
        p[n++] = (uint8_t)(b | (v ? 0x80 : 0));
    } while (v);
    return n;
}

static int vol_fd(xrarc_t *t, unsigned v) {
    for (unsigned i = 0; i < 2; i++)
        if (t->fd[i] >= 0 && t->fd_vol[i] == v) return t->fd[i];
    {
        unsigned i = t->fd_next++ % 2;
        if (t->fd[i] >= 0) close(t->fd[i]);
        t->fd[i] = open(t->vols[v], O_RDONLY);
        t->fd_vol[i] = v;
        if (t->fd[i] < 0) fail(t, XRC_E_OPEN);
        return t->fd[i];
    }
}

/* Exactly `n` bytes at `off` of volume `v`, else -1 (truncated, or a read error). */
static int rd(xrarc_t *t, unsigned v, uint64_t off, void *dst, size_t n) {
    int fd = vol_fd(t, v);
    uint8_t *d = dst;
    if (fd < 0) return -1;
    while (n) {
        ssize_t r = pread(fd, d, n, (off_t)off);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return fail(t, XRC_E_OPEN);
        if (r == 0) return fail(t, XRC_E_TRUNCATED);
        d += r;
        off += (uint64_t)r;
        n -= (size_t)r;
    }
    return 0;
}

/* ---- entries and file streams ---- */

void xrarc_release(xrarc_t *t, xrarc_ent_t *e) {
    (void)t;
    if (e && --e->refs <= 0) {
        xrc_wipe(e, sizeof *e);
        free(e);
    }
}

xrarc_ent_t *xrarc_take(xrarc_t *t) {
    xrarc_ent_t *e = t->q_head;
    if (!e) return NULL;
    t->q_head = e->next;
    if (!t->q_head) t->q_tail = NULL;
    e->next = NULL;
    return e;
}

static void fs_unref(xrarc_t *t, fstream_t *fs) {
    if (fs && --fs->refs <= 0) {
        void *raw = fs->raw;
        xrarc_release(t, fs->ent);
        xrc_wipe(fs, sizeof *fs);
        free(raw);
    }
}

static fstream_t *fs_new(xrarc_t *t) {
    void *raw;
    fstream_t *fs = amalloc(sizeof *fs, &raw);
    if (!fs) {
        fail(t, XRC_E_NOMEM);
        return NULL;
    }
    memset(fs, 0, sizeof *fs);
    fs->raw = raw;
    return fs;
}

/* ---- segments ---- */

static seg_t *seg_add(xrarc_t *t, int kind) {
    seg_t *s;
    if (t->nseg == t->cap) {
        size_t nc = t->cap ? t->cap * 2 : 64;
        seg_t *g = realloc(t->segs, nc * sizeof *g);
        if (!g) {
            fail(t, XRC_E_NOMEM);
            return NULL;
        }
        t->segs = g;
        t->cap = nc;
    }
    s = &t->segs[t->nseg++];
    memset(s, 0, sizeof *s);
    s->kind = kind;
    return s;
}

static int add_mem(xrarc_t *t, const uint8_t *p, size_t n) {
    seg_t *s;
    uint8_t *m = malloc(n ? n : 1);
    if (!m) return fail(t, XRC_E_NOMEM);
    memcpy(m, p, n);
    s = seg_add(t, SEG_MEM);
    if (!s) {
        free(m);
        return -1;
    }
    s->mem = m;
    s->len = s->out_len = n;
    return 0;
}

static int add_raw(xrarc_t *t, unsigned vol, uint64_t off, uint64_t len) {
    seg_t *s;
    if (!len) return 0;
    s = seg_add(t, SEG_RAW);
    if (!s) return -1;
    s->vol = vol;
    s->off = off;
    s->len = s->out_len = len;
    return 0;
}

static int add_dec(xrarc_t *t, unsigned vol, uint64_t off, uint64_t len, uint64_t cut, fstream_t *fs, int last) {
    seg_t *s;
    if (!len) return 0;
    s = seg_add(t, SEG_DEC);
    if (!s) return -1;
    s->vol = vol;
    s->off = off;
    s->len = len;
    s->out_len = len - cut;
    s->fs = fs;
    s->last = last;
    fs->refs++;
    return 0;
}

static void seg_free(xrarc_t *t, seg_t *s) {
    if (s->mem) {
        xrc_wipe(s->mem, (size_t)s->len);
        free(s->mem);
    }
    if (s->fs) fs_unref(t, s->fs);
    xrc_wipe(s, sizeof *s);
}

/* ---- headers: RAR5 ---- */

static uint32_t crc32(const void *p, size_t n) { return CrcCalc(p, n); }

/* Reads the header at scan_pos into t->hb (decrypted when the volume's headers are). Sets its size
 * and the bytes it takes in the volume. */
static int read_hdr5(xrarc_t *t, size_t *hsize, uint64_t *raw_len) {
    uint8_t *hb = t->hb;
    uint64_t pos = t->scan_pos, size;
    size_t at = 4;
    if (t->hdr_enc) {
        uint8_t iv[16];
        xrc_aes_t aes;
        size_t enc;
        if (rd(t, t->scan_vol, pos, iv, 16) != 0 || rd(t, t->scan_vol, pos + 16, hb, 16) != 0) return -1;
        xrc_aes_init(&aes, t->hkey, 32, iv);
        xrc_aes_dec(&aes, hb, 1);
        if (vget(hb, 16, &at, &size) != 0 || size == 0 || size > HDR_MAX - at)
            return fail(t, t->hdr_ok ? XRC_E_CORRUPT : XRC_E_WRONG_PASSWORD);
        *hsize = at + (size_t)size;
        enc = (*hsize + 15) & ~(size_t)15;
        if (rd(t, t->scan_vol, pos + 16, hb, enc) != 0) return -1;
        xrc_aes_init(&aes, t->hkey, 32, iv);
        xrc_aes_dec(&aes, hb, enc / 16);
        xrc_wipe(&aes, sizeof aes);
        *raw_len = 16 + enc;
        if (crc32(hb + 4, *hsize - 4) != le32(hb)) return fail(t, t->hdr_ok ? XRC_E_CORRUPT : XRC_E_WRONG_PASSWORD);
        t->hdr_ok = 1;
        return 0;
    }
    {
        uint64_t left = t->scan_size - pos;
        size_t first = left < 7 ? (size_t)left : 7;
        if (first < 5 || rd(t, t->scan_vol, pos, hb, first) != 0) return fail(t, XRC_E_TRUNCATED);
        if (vget(hb, first, &at, &size) != 0 || size == 0 || size > HDR_MAX - at) return fail(t, XRC_E_CORRUPT);
        *hsize = at + (size_t)size;
        if (rd(t, t->scan_vol, pos, hb, *hsize) != 0) return -1;
        if (crc32(hb + 4, *hsize - 4) != le32(hb)) return fail(t, XRC_E_CORRUPT);
        *raw_len = *hsize;
        return 0;
    }
}

/* Emits the header in t->hb as it is: a copy when it was decrypted, else the volume's bytes. */
static int emit_hdr(xrarc_t *t, size_t hsize) {
    if (t->hdr_enc) return add_mem(t, t->hb, hsize);
    return add_raw(t, t->scan_vol, t->scan_pos, hsize);
}

typedef struct {
    size_t type_at; /* the record's type field, in hb */
    size_t data, end;
} xrec_t;

static int scan_file5(xrarc_t *t, size_t hsize, uint64_t raw_len, uint64_t type, uint64_t hflags, size_t at,
                      uint64_t extra_size, uint64_t data_size) {
    const uint8_t *hb = t->hb;
    uint64_t ff, unp, attr, comp, host, nlen;
    size_t at_ff = at, at_comp, ex0;
    uint32_t stored_crc = 0;
    int have_crc, is_file = type == 2, split_before = (hflags & 0x08) != 0, split_after = (hflags & 0x10) != 0;
    xrec_t crypt = {0, 0, 0}, hash = {0, 0, 0};
    int has_crypt = 0, has_hash = 0, mac = 0;
    uint64_t data_off = t->scan_pos + raw_len, cut = 0;
    fstream_t *fs = NULL;
    xrarc_ent_t *ent = NULL;
    uint8_t key[32], hashkey[32], check[8];

    if (vget(hb, hsize, &at, &ff) || vget(hb, hsize, &at, &unp) || vget(hb, hsize, &at, &attr))
        return fail(t, XRC_E_CORRUPT);
    if (ff & 0x02) at += 4;
    have_crc = (ff & 0x04) != 0;
    if (have_crc) {
        if (at + 4 > hsize) return fail(t, XRC_E_CORRUPT);
        stored_crc = le32(hb + at);
        at += 4;
    }
    at_comp = at;
    if (vget(hb, hsize, &at, &comp) || vget(hb, hsize, &at, &host) || vget(hb, hsize, &at, &nlen) ||
        nlen > hsize - at || extra_size > hsize - at - nlen)
        return fail(t, XRC_E_CORRUPT);
    ex0 = hsize - (size_t)extra_size;
    for (size_t p = ex0; p < hsize;) {
        uint64_t rsize, rtype;
        size_t q = p, tat;
        if (vget(hb, hsize, &q, &rsize) || rsize > hsize - q) return fail(t, XRC_E_CORRUPT);
        tat = q;
        if (vget(hb, q + (size_t)rsize, &q, &rtype)) return fail(t, XRC_E_CORRUPT);
        if (rtype == 1 && !has_crypt) {
            crypt.type_at = tat;
            crypt.data = q;
            crypt.end = tat + (size_t)rsize;
            has_crypt = 1;
        } else if (rtype == 2 && !has_hash) {
            hash.type_at = tat;
            hash.data = q;
            hash.end = tat + (size_t)rsize;
            has_hash = 1;
        }
        p = tat + (size_t)rsize;
    }
    if (!has_crypt) {
        /* Not encrypted: as it is. */
        if (emit_hdr(t, hsize) != 0) return -1;
        if (is_file && !split_before) {
            ent = calloc(1, sizeof *ent);
            if (!ent) return fail(t, XRC_E_NOMEM);
            ent->refs = 1;
            ent->complete = 1;
            if (t->q_tail) t->q_tail->next = ent;
            else t->q_head = ent;
            t->q_tail = ent;
        }
        return add_raw(t, t->scan_vol, data_off, data_size);
    }

    /* The encryption record: version, flags, KDF count, salt, IV, [check]. */
    t->any_enc = 1;
    {
        size_t q = crypt.data;
        uint64_t ver, cflags;
        const uint8_t *salt, *iv;
        unsigned lg2;
        if (vget(hb, crypt.end, &q, &ver) || vget(hb, crypt.end, &q, &cflags) || q + 33 > crypt.end)
            return fail(t, XRC_E_CORRUPT);
        if (ver != 0) return fail(t, XRC_E_UNSUPPORTED);
        lg2 = hb[q];
        salt = hb + q + 1;
        iv = hb + q + 17;
        q += 33;
        mac = (cflags & 0x02) != 0;
        /* Only files: a service header's data (a comment, the quick-open copy of the headers) is
         * skipped by libarchive, and RAR's check value in it does not follow the files' rule. */
        if (t->pw && is_file) {
            if (xrc_kdf5(t->pw, salt, lg2, key, hashkey, check) != 0) return fail(t, XRC_E_UNSUPPORTED);
            if ((cflags & 0x01) && q + 12 <= crypt.end && xrc_check5_valid(hb + q)) {
                if (memcmp(hb + q, check, 8) != 0) return fail(t, XRC_E_WRONG_PASSWORD);
                t->confirmed = 1;
            }
            if (split_before && t->split_fs) {
                fs = t->split_fs;
            } else {
                fs = fs_new(t);
                if (!fs) return -1;
                xrc_aes_init(&fs->aes, key, 32, iv);
            }
        }
    }

    if (is_file && !split_before) {
        ent = calloc(1, sizeof *ent);
        if (!ent) {
            if (fs && fs != t->split_fs && fs->refs == 0) fs_unref(t, (fs->refs = 1, fs));
            return fail(t, XRC_E_NOMEM);
        }
        ent->refs = 1;
        ent->encrypted = 1;
        ent->check = ent->keyed = mac;
        if (t->q_tail) t->q_tail->next = ent;
        else t->q_head = ent;
        t->q_tail = ent;
        if (fs) {
            fs->ent = ent;
            ent->refs++;
        }
    }
    if (!split_after) {
        /* The last part: the file's checksum. */
        xrarc_ent_t *e = ent ? ent : (fs ? fs->ent : NULL);
        if (e) {
            if (mac && t->pw) {
                memcpy(e->hashkey, hashkey, 32);
                if (have_crc) {
                    e->have_crc = 1;
                    e->crc = stored_crc;
                }
                if (has_hash) {
                    size_t q = hash.data;
                    uint64_t htype;
                    if (!vget(hb, hash.end, &q, &htype) && htype == 0 && q + 32 <= hash.end) {
                        e->have_blake = 1;
                        memcpy(e->blake, hb + q, 32);
                    }
                }
            }
            e->complete = 1;
        }
        /* A stored file: its padding to whole AES blocks is cut (libarchive copies every byte). */
        if (fs && ((comp >> 7) & 7) == 0 && !(ff & 0x08)) {
            uint64_t total = fs->packed + data_size;
            if (total > unp && total - unp < 16 && total - unp <= data_size) cut = total - unp;
        }
        /* An empty file still has one block of encrypted padding, which libarchive would take for
         * compressed data: none is handed on. */
        if (fs && unp == 0 && !(ff & 0x08) && !split_before) cut = data_size;
    }
    xrc_wipe(key, sizeof key);
    xrc_wipe(check, sizeof check);
    xrc_wipe(hashkey, sizeof hashkey);

    /* The header again, without the encryption: the record's type made unknown (skipped), and a
     * keyed checksum made 0, which libarchive takes as "none" (the caller checks it). The CRC field
     * itself stays: a split file's first parts hold a CRC of their packed bytes, and libarchive only
     * sums the file's data while the CRC it holds is not 0. */
    {
        uint8_t *nb = malloc(hsize + 32);
        size_t n = 0, body0, tail;
        uint8_t sz[10];
        size_t szn;
        if (!nb) {
            if (fs && fs->refs == 0 && fs != t->split_fs) {
                fs->refs = 1;
                fs_unref(t, fs);
            }
            return fail(t, XRC_E_NOMEM);
        }
        n = 4 + 3; /* room for the CRC and up to a 3-byte size, moved down below */
        body0 = n;
        n += vput(nb + n, type);
        n += vput(nb + n, hflags);
        if (hflags & 0x01) n += vput(nb + n, extra_size);
        if (hflags & 0x02) n += vput(nb + n, data_size - cut);
        n += vput(nb + n, ff);
        n += vput(nb + n, unp);
        n += vput(nb + n, attr);
        if (ff & 0x02) {
            size_t m = at_ff;
            uint64_t dummy;
            vget(hb, hsize, &m, &dummy);
            vget(hb, hsize, &m, &dummy);
            vget(hb, hsize, &m, &dummy);
            memcpy(nb + n, hb + m, 4);
            n += 4;
        }
        if (have_crc) {
            put32(nb + n, mac ? 0 : stored_crc);
            n += 4;
        }
        tail = n;
        memcpy(nb + n, hb + at_comp, hsize - at_comp);
        n += hsize - at_comp;
        nb[tail + (crypt.type_at - at_comp)] = (uint8_t)((nb[tail + (crypt.type_at - at_comp)] & 0x80) | NEUTRAL_TYPE);
        if (mac && has_hash) nb[tail + (hash.type_at - at_comp)] = (uint8_t)((nb[tail + (hash.type_at - at_comp)] & 0x80) | NEUTRAL_TYPE);
        /* No data left (an empty file): stored, or libarchive would look for a compressed block.
         * The method is bits 7-9 of the compression field, the low bits of its second byte. */
        if (cut && cut == data_size && (nb[tail] & 0x80)) nb[tail + 1] &= (uint8_t)~0x07;
        szn = vput(sz, n - body0);
        memcpy(nb + body0 - szn, sz, szn);
        put32(nb + body0 - szn - 4, crc32(nb + body0 - szn, n - body0 + szn));
        if (add_mem(t, nb + body0 - szn - 4, n - body0 + szn + 4) != 0) {
            free(nb);
            return -1;
        }
        free(nb);
    }

    /* The data. A service header's (a comment, a recovery record) is skipped by libarchive. */
    if (fs) {
        int r = 0;
        if (data_size) r = add_dec(t, t->scan_vol, data_off, data_size, cut, fs, !split_after);
        fs->packed += data_size;
        if (r == 0) {
            if (split_after) {
                if (t->split_fs != fs) {
                    fs_unref(t, t->split_fs);
                    t->split_fs = fs;
                    fs->refs++;
                }
            } else if (t->split_fs == fs) {
                t->split_fs = NULL;
                fs_unref(t, fs);
                fs = NULL;
            }
        }
        if (fs && fs->refs == 0) {
            fs->refs = 1;
            fs_unref(t, fs);
        }
        return r;
    }
    return add_raw(t, t->scan_vol, data_off, data_size);
}

/* 1 a header read, -1 failure. Sets *end at the end of the volume's archive. */
static int scan5(xrarc_t *t, int *end) {
    size_t hsize, at = 4;
    uint64_t raw_len, size, type, hflags, extra_size = 0, data_size = 0;
    if (read_hdr5(t, &hsize, &raw_len) != 0) return -1;
    if (vget(t->hb, hsize, &at, &size) || vget(t->hb, hsize, &at, &type) || vget(t->hb, hsize, &at, &hflags))
        return fail(t, XRC_E_CORRUPT);
    if ((hflags & 0x01) && vget(t->hb, hsize, &at, &extra_size)) return fail(t, XRC_E_CORRUPT);
    if ((hflags & 0x02) && vget(t->hb, hsize, &at, &data_size)) return fail(t, XRC_E_CORRUPT);
    if (type == 4) {
        /* Encryption header: version, flags, KDF count, salt, [check]. Everything after it is
         * encrypted. */
        uint64_t ver, cflags;
        uint8_t hashkey[32], check[8];
        t->any_enc = 1;
        if (vget(t->hb, hsize, &at, &ver) || vget(t->hb, hsize, &at, &cflags) || at + 17 > hsize)
            return fail(t, XRC_E_CORRUPT);
        if (ver != 0) return fail(t, XRC_E_UNSUPPORTED);
        if (!t->pw) return fail(t, XRC_E_PASSWORD_REQUIRED);
        if (xrc_kdf5(t->pw, t->hb + at + 1, t->hb[at], t->hkey, hashkey, check) != 0) return fail(t, XRC_E_UNSUPPORTED);
        if ((cflags & 0x01) && at + 29 <= hsize && xrc_check5_valid(t->hb + at + 17)) {
            if (memcmp(t->hb + at + 17, check, 8) != 0) return fail(t, XRC_E_WRONG_PASSWORD);
            t->confirmed = 1;
        }
        xrc_wipe(hashkey, sizeof hashkey);
        xrc_wipe(check, sizeof check);
        t->scan_pos += raw_len + data_size;
        t->hdr_enc = 1;
        return 1;
    }
    if (type == 1) {
        uint64_t aflags;
        if (!vget(t->hb, hsize, &at, &aflags)) t->solid = (aflags & 0x04) ? 1 : 0;
    }
    if (type == 2 || type == 3) {
        if (scan_file5(t, hsize, raw_len, type, hflags, at, extra_size, data_size) != 0) return -1;
    } else {
        if (emit_hdr(t, hsize) != 0 || add_raw(t, t->scan_vol, t->scan_pos + raw_len, data_size) != 0) return -1;
    }
    if (type == 5) *end = 1;
    t->scan_pos += raw_len + data_size;
    return 1;
}

/* ---- headers: RAR 3/4 ---- */

static uint16_t crc16(const uint8_t *h, size_t size) { return (uint16_t)(crc32(h + 2, size - 2) & 0xffff); }

static int read_hdr4(xrarc_t *t, size_t *size, uint64_t *raw_len) {
    uint8_t *hb = t->hb;
    uint64_t pos = t->scan_pos;
    if (t->hdr_enc) {
        uint8_t salt[8], key[16], iv[16];
        xrc_aes_t aes;
        size_t enc;
        if (rd(t, t->scan_vol, pos, salt, 8) != 0 || rd(t, t->scan_vol, pos + 8, hb, 16) != 0) return -1;
        xrc_kdf3(t->pw, salt, key, iv);
        xrc_aes_init(&aes, key, 16, iv);
        xrc_aes_dec(&aes, hb, 1);
        *size = le16(hb + 5);
        if (*size < 7) return fail(t, t->hdr_ok ? XRC_E_CORRUPT : XRC_E_WRONG_PASSWORD);
        enc = (*size + 15) & ~(size_t)15;
        if (rd(t, t->scan_vol, pos + 8, hb, enc) != 0) return -1;
        xrc_aes_init(&aes, key, 16, iv);
        xrc_aes_dec(&aes, hb, enc / 16);
        xrc_wipe(&aes, sizeof aes);
        xrc_wipe(key, sizeof key);
        if (crc16(hb, *size) != le16(hb)) return fail(t, t->hdr_ok ? XRC_E_CORRUPT : XRC_E_WRONG_PASSWORD);
        t->hdr_ok = 1;
        t->confirmed = 1;
        *raw_len = 8 + enc;
        return 0;
    }
    if (t->scan_size - pos < 7 || rd(t, t->scan_vol, pos, hb, 7) != 0) return fail(t, XRC_E_TRUNCATED);
    *size = le16(hb + 5);
    if (*size < 7) return fail(t, XRC_E_CORRUPT);
    if (rd(t, t->scan_vol, pos, hb, *size) != 0) return -1;
    *raw_len = *size;
    return 0;
}

static int scan4(xrarc_t *t, int *end) {
    uint8_t *hb = t->hb;
    size_t size;
    uint64_t raw_len, data_size = 0, data_off;
    unsigned type, flags;
    int decrypted = t->hdr_enc;
    if (read_hdr4(t, &size, &raw_len) != 0) return -1;
    type = hb[2];
    flags = le16(hb + 3);
    if (flags & 0x8000) {
        if (size < 11) return fail(t, XRC_E_CORRUPT);
        data_size = le32(hb + 7);
        if ((type == 0x74 || type == 0x7a) && (flags & 0x100)) {
            if (size < 40) return fail(t, XRC_E_CORRUPT);
            data_size |= (uint64_t)le32(hb + 32) << 32;
        }
    }
    data_off = t->scan_pos + raw_len;
    if (type == 0x73) {
        t->solid = (flags & 0x0008) ? 1 : 0;
        if (flags & 0x0080) {
            t->any_enc = 1;
            if (!t->pw) return fail(t, XRC_E_PASSWORD_REQUIRED);
            put16(hb + 3, flags & ~0x0080u);
            put16(hb, crc16(hb, size));
            if (add_mem(t, hb, size) != 0) return -1;
            t->hdr_enc = 1;
            t->scan_pos += raw_len;
            return 1;
        }
    } else if (type == 0x74 && (flags & 0x04)) {
        unsigned name_size, name_off = (flags & 0x100) ? 40 : 32;
        int split_before = (flags & 0x01) != 0, split_after = (flags & 0x02) != 0;
        const uint8_t *salt = NULL;
        fstream_t *fs = NULL;
        xrarc_ent_t *ent = NULL;
        uint64_t cut = 0;
        if (size < name_off) return fail(t, XRC_E_CORRUPT);
        name_size = le16(hb + 26);
        if (flags & 0x400) {
            if (name_off + name_size + 8 > size) return fail(t, XRC_E_CORRUPT);
            salt = hb + name_off + name_size;
        }
        t->any_enc = 1;
        if (t->pw) {
            uint8_t key[16], iv[16];
            if (hb[24] < 29) return fail(t, XRC_E_UNSUPPORTED); /* RAR 1.5/2.0 encryption */
            if (split_before && t->split_fs) {
                fs = t->split_fs;
            } else {
                fs = fs_new(t);
                if (!fs) return -1;
                xrc_kdf3(t->pw, salt, key, iv);
                xrc_aes_init(&fs->aes, key, 16, iv);
                xrc_wipe(key, sizeof key);
            }
        }
        if (!split_before) {
            ent = calloc(1, sizeof *ent);
            if (!ent) {
                if (fs && fs != t->split_fs) {
                    fs->refs = 1;
                    fs_unref(t, fs);
                }
                return fail(t, XRC_E_NOMEM);
            }
            ent->refs = 1;
            ent->encrypted = 1;
            ent->check = 1;
            if (t->q_tail) t->q_tail->next = ent;
            else t->q_head = ent;
            t->q_tail = ent;
            if (fs) {
                fs->ent = ent;
                ent->refs++;
            }
        }
        if (!split_after) {
            /* The last part holds the file's CRC. */
            xrarc_ent_t *e = ent ? ent : (fs ? fs->ent : NULL);
            if (e) {
                e->have_crc = 1;
                e->crc = le32(hb + 16);
                e->complete = 1;
            }
        }
        /* A stored file's padding is cut (see scan_file5). */
        if (fs && !split_after && hb[25] == 0x30) {
            uint64_t unp = le32(hb + 11), total = fs->packed + data_size;
            if (flags & 0x100) unp |= (uint64_t)le32(hb + 36) << 32;
            if (total > unp && total - unp < 16 && total - unp <= data_size) cut = total - unp;
        }
        put16(hb + 3, flags & ~0x04u);
        if (cut) {
            put32(hb + 7, (uint32_t)(data_size - cut));
            if (flags & 0x100) put32(hb + 32, (uint32_t)((data_size - cut) >> 32));
        }
        put16(hb, crc16(hb, size));
        if (add_mem(t, hb, size) != 0) return -1;
        if (fs) {
            int r = add_dec(t, t->scan_vol, data_off, data_size, cut, fs, !split_after);
            fs->packed += data_size;
            if (r == 0) {
                if (split_after) {
                    if (t->split_fs != fs) {
                        fs_unref(t, t->split_fs);
                        t->split_fs = fs;
                        fs->refs++;
                    }
                } else if (t->split_fs == fs) {
                    t->split_fs = NULL;
                    fs_unref(t, fs);
                    fs = NULL;
                }
            }
            if (fs && fs->refs == 0) {
                fs->refs = 1;
                fs_unref(t, fs);
            }
            if (r != 0) return -1;
        } else if (add_raw(t, t->scan_vol, data_off, data_size) != 0) {
            return -1;
        }
        t->scan_pos = data_off + data_size;
        return 1;
    } else if (type == 0x74) {
        if (!(flags & 0x01)) {
            xrarc_ent_t *ent = calloc(1, sizeof *ent);
            if (!ent) return fail(t, XRC_E_NOMEM);
            ent->refs = 1;
            ent->complete = 1;
            if (t->q_tail) t->q_tail->next = ent;
            else t->q_head = ent;
            t->q_tail = ent;
        }
    }
    if (decrypted ? add_mem(t, hb, size) : add_raw(t, t->scan_vol, t->scan_pos, size)) return -1;
    if (add_raw(t, t->scan_vol, data_off, data_size) != 0) return -1;
    if (type == 0x7b) *end = 1;
    t->scan_pos = data_off + data_size;
    return 1;
}

/* ---- volumes ---- */

/* The signature of volume scan_vol (the first may be a self-extractor: searched for). */
static int open_vol(xrarc_t *t) {
    static const uint8_t sig[6] = {'R', 'a', 'r', '!', 0x1a, 0x07};
    struct stat st;
    int fd = vol_fd(t, t->scan_vol);
    uint64_t limit, off = 0;
    int fmt = 0;
    if (fd < 0) return -1;
    if (fstat(fd, &st) != 0) return fail(t, XRC_E_OPEN);
    t->scan_size = (uint64_t)st.st_size;
    limit = t->scan_vol == 0 ? SFX_SCAN : 1;
    while (off < limit && off + 8 <= t->scan_size && !fmt) {
        size_t n = (size_t)((t->scan_size - off) < BUF_SIZE ? (t->scan_size - off) : BUF_SIZE);
        if (rd(t, t->scan_vol, off, t->buf, n) != 0) return -1;
        for (size_t i = 0; i + 8 <= n && off + i < limit; i++) {
            if (memcmp(t->buf + i, sig, 6) != 0) continue;
            if (t->buf[i + 6] == 0x00) fmt = 4;
            else if (t->buf[i + 6] == 0x01 && t->buf[i + 7] == 0x00) fmt = 5;
            else continue;
            off += i;
            break;
        }
        if (!fmt) off += n > 7 ? n - 7 : n;
    }
    if (!fmt) return fail(t, t->scan_vol == 0 ? XRC_E_NOT_RAR : XRC_E_CORRUPT);
    if (t->fmt && fmt != t->fmt) return fail(t, XRC_E_CORRUPT);
    t->fmt = fmt;
    if (add_raw(t, t->scan_vol, off, fmt == 5 ? 8 : 7) != 0) return -1;
    t->scan_pos = off + (fmt == 5 ? 8 : 7);
    t->hdr_enc = 0;
    t->vol_open = 1;
    return 0;
}

/* Reads one more header (segments added). 1, 0 at the end of the last volume, -1 failure. */
static int scan_one(xrarc_t *t) {
    int end = 0, r;
    if (t->err) return -1;
    if (t->scan_done) return 0;
    if (!t->vol_open && open_vol(t) != 0) return -1;
    if (t->scan_pos >= t->scan_size) {
        end = 1;
        r = 1;
    } else {
        r = t->fmt == 5 ? scan5(t, &end) : scan4(t, &end);
        if (r < 0) return -1;
    }
    if (end) {
        t->vol_open = 0;
        if (++t->scan_vol >= t->nvols) {
            t->scan_done = 1;
            if (t->split_fs) return fail(t, XRC_E_MISSING_VOLUME);
        }
    }
    return 1;
}

/* ---- handing the stream out ---- */

/* Decrypts the last `r` (< 16) bytes of DEC segment `i` with the bytes the block needs from the
 * file's next parts; their plaintext is kept for those parts. Writes the first `r` bytes to `out`. */
static int straddle(xrarc_t *t, size_t i, unsigned r, uint8_t *out) {
    _Alignas(16) uint8_t blk[16];
    size_t j = i + 1, took[16], from[16];
    unsigned nt = 0, have = r, need = 16 - r;
    fstream_t *fs = t->segs[i].fs;
    if (t->segs[i].last) return fail(t, XRC_E_CORRUPT); /* not a whole number of blocks */
    if (rd(t, t->segs[i].vol, t->segs[i].off + t->segs[i].len - r, blk, r) != 0) return -1;
    while (need) {
        seg_t *s;
        unsigned take;
        while (j >= t->nseg) {
            int k = scan_one(t);
            if (k < 0) return -1;
            if (k == 0) return fail(t, XRC_E_MISSING_VOLUME);
        }
        s = &t->segs[j];
        if (s->kind != SEG_DEC || s->fs != fs) {
            j++;
            continue;
        }
        take = s->len < need ? (unsigned)s->len : need;
        if (rd(t, s->vol, s->off, blk + have, take) != 0) return -1;
        s->skip = take;
        took[nt] = take;
        from[nt] = j;
        nt++;
        have += take;
        need -= take;
        if (need && s->last) return fail(t, XRC_E_CORRUPT);
        j++;
    }
    xrc_aes_dec(&fs->aes, blk, 1);
    memcpy(out, blk, r);
    have = r;
    for (unsigned k = 0; k < nt; k++) {
        seg_t *s = &t->segs[from[k]];
        memcpy(s->pend, blk + have, took[k]);
        s->pend_n = (unsigned)took[k];
        have += (unsigned)took[k];
    }
    xrc_wipe(blk, 16);
    return 0;
}

/* Up to BUF_SIZE bytes of segment `at` into *out. >0 bytes, 0 the segment is done, -1 failure. */
static la_ssize_t serve(xrarc_t *t, const void **out) {
    seg_t *s = &t->segs[t->at];
    if (s->kind == SEG_MEM) {
        size_t n = (size_t)(s->len - t->pos);
        *out = s->mem + t->pos;
        t->pos += n;
        return (la_ssize_t)n;
    }
    if (s->kind == SEG_RAW) {
        uint64_t left = s->len - t->pos;
        size_t n = left < BUF_SIZE ? (size_t)left : BUF_SIZE;
        if (!n) return 0;
        if (rd(t, s->vol, s->off + t->pos, t->buf, n) != 0) return -1;
        t->pos += n;
        *out = t->buf;
        return (la_ssize_t)n;
    }
    if (!t->pend_done) {
        t->pend_done = 1;
        t->pos = s->skip;
        if (s->pend_n) {
            uint64_t n = s->pend_n < s->out_len - t->outp ? s->pend_n : s->out_len - t->outp;
            memcpy(t->buf, s->pend, (size_t)n);
            xrc_wipe(s->pend, sizeof s->pend);
            t->outp += n;
            if (n) {
                *out = t->buf;
                return (la_ssize_t)n;
            }
        }
    }
    for (;;) {
        uint64_t left = s->len - t->pos, give;
        size_t n;
        if (!left) return 0;
        if (left >= 16) {
            n = left < BUF_SIZE ? (size_t)(left & ~(uint64_t)15) : BUF_SIZE;
            if (rd(t, s->vol, s->off + t->pos, t->buf, n) != 0) return -1;
            xrc_aes_dec(&s->fs->aes, t->buf, n / 16);
        } else {
            uint8_t tail[16];
            size_t at = t->at;
            n = (size_t)left;
            if (straddle(t, at, (unsigned)n, tail) != 0) return -1;
            s = &t->segs[at]; /* the scan may have moved the array */
            memcpy(t->buf, tail, n);
            xrc_wipe(tail, sizeof tail);
        }
        t->pos += n;
        give = s->out_len - t->outp < n ? s->out_len - t->outp : n;
        t->outp += give;
        if (give) {
            *out = t->buf;
            return (la_ssize_t)give;
        }
    }
}

/* Done with segment `at`: on to the next (older ones are dropped now and then). */
static void next_seg(xrarc_t *t) {
    seg_free(t, &t->segs[t->at]);
    t->at++;
    t->pos = t->outp = 0;
    t->pend_done = 0;
    if (t->at == t->nseg) {
        t->at = t->nseg = 0;
    } else if (t->at >= 256 && t->at * 2 >= t->nseg) {
        memmove(t->segs, t->segs + t->at, (t->nseg - t->at) * sizeof *t->segs);
        t->nseg -= t->at;
        t->at = 0;
    }
}

static const char *err_text(int e) {
    switch (e) {
    case XRC_E_PASSWORD_REQUIRED: return "password required";
    case XRC_E_WRONG_PASSWORD: return "wrong password";
    case XRC_E_MISSING_VOLUME: return "a volume is missing";
    case XRC_E_TRUNCATED: return "truncated";
    case XRC_E_UNSUPPORTED: return "unsupported encryption";
    case XRC_E_NOMEM: return "out of memory";
    case XRC_E_OPEN: return "read error";
    case XRC_E_NOT_RAR: return "not a RAR archive";
    default: return "damaged";
    }
}

static la_ssize_t read_cb(struct archive *a, void *ud, const void **out) {
    xrarc_t *t = ud;
    for (;;) {
        la_ssize_t n;
        if (t->err) break;
        if (t->at == t->nseg) {
            int k = scan_one(t);
            if (k < 0) break;
            if (k == 0) return 0;
            continue;
        }
        n = serve(t, out);
        if (n < 0) break;
        if (n > 0) return n;
        next_seg(t);
    }
    if (a) archive_set_error(a, EIO, "RAR decryption: %s", err_text(t->err));
    return -1;
}

/* Skips within the current segment; never into the middle of encrypted data that goes on (its
 * chaining would be lost): libarchive then reads instead. */
static la_int64_t skip_cb(struct archive *a, void *ud, la_int64_t req) {
    xrarc_t *t = ud;
    (void)a;
    for (;;) {
        seg_t *s;
        if (t->err || req <= 0) return 0;
        if (t->at == t->nseg) {
            if (scan_one(t) <= 0) return 0;
            continue;
        }
        s = &t->segs[t->at];
        if (s->kind != SEG_DEC) {
            uint64_t left = s->len - t->pos, n = (uint64_t)req < left ? (uint64_t)req : left;
            if (!n) {
                next_seg(t);
                continue;
            }
            t->pos += n;
            return (la_int64_t)n;
        }
        if (t->pend_done && t->pos >= s->len) {
            next_seg(t);
            continue;
        }
        if (s->last && (uint64_t)req >= s->out_len - t->outp) {
            uint64_t n = s->out_len - t->outp;
            t->pend_done = 1;
            t->pos = s->len;
            t->outp = s->out_len;
            if (!n) {
                next_seg(t);
                continue;
            }
            return (la_int64_t)n;
        }
        return 0;
    }
}

/* ---- the handle ---- */

xrarc_t *xrarc_open(char **vols, const char *password, int *err) {
    xrarc_t *t = calloc(1, sizeof *t);
    *err = XRC_E_NOMEM;
    if (!t) return NULL;
    xrc_init();
    CrcGenerateTable();
    t->vols = vols;
    while (vols[t->nvols]) t->nvols++;
    t->pw = password;
    t->solid = -1;
    t->fd[0] = t->fd[1] = -1;
    t->buf = amalloc(BUF_SIZE, &t->buf_raw);
    t->hb = amalloc(HDR_MAX + 64, &t->hb_raw);
    if (!t->buf || !t->hb || !t->nvols) {
        if (t->nvols) fail(t, XRC_E_NOMEM);
        else fail(t, XRC_E_OPEN);
        goto bad;
    }
    /* The first volume's headers, up to the first encrypted one: a missing or wrong password
     * shows now, before anything is written (an archive may start with files that have none). */
    for (int n = 0; n < OPEN_SCAN_MAX && !t->any_enc && t->scan_vol == 0 && !t->scan_done; n++) {
        int k = scan_one(t);
        if (k < 0) goto bad;
        if (k == 0) break;
    }
    if (t->err) goto bad;
    *err = XRC_OK;
    return t;
bad:
    *err = t->err ? t->err : XRC_E_CORRUPT;
    xrarc_close(t);
    return NULL;
}

int xrarc_encrypted(const xrarc_t *t) { return t->any_enc; }
int xrarc_confirmed(const xrarc_t *t) { return t->confirmed; }
int xrarc_solid(const xrarc_t *t) { return t->solid; }
int xrarc_error(const xrarc_t *t) { return t->err; }

int xrarc_attach(xrarc_t *t, struct archive *a) { return archive_read_open2(a, t, NULL, read_cb, skip_cb, NULL); }

void xrarc_close(xrarc_t *t) {
    if (!t) return;
    for (size_t i = t->at; i < t->nseg; i++) seg_free(t, &t->segs[i]);
    free(t->segs);
    fs_unref(t, t->split_fs);
    while (t->q_head) xrarc_release(t, xrarc_take(t));
    for (unsigned i = 0; i < 2; i++)
        if (t->fd[i] >= 0) close(t->fd[i]);
    if (t->buf) xrc_wipe(t->buf, BUF_SIZE);
    if (t->hb) xrc_wipe(t->hb, HDR_MAX + 64);
    xrc_wipe(t->hkey, sizeof t->hkey);
    free(t->buf_raw);
    free(t->hb_raw);
    free(t);
}

