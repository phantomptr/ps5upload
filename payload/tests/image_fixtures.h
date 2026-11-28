/* Small disk images for the host self-tests of payload/smb/smbd_image.c (#963): an exFAT, a UFS2
 * (.ffpkg) and a PFS (.ffpfs) image holding sce_sys/param.json or param.sfo and icon0.png, built
 * in memory with the on-disk layouts the reader expects. Every builder returns a heap buffer
 * (free it) and its length. Only what the reader looks at is filled in. */
#ifndef PS5UPLOAD_TEST_IMAGE_FIXTURES_H
#define PS5UPLOAD_TEST_IMAGE_FIXTURES_H

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Each test uses only some of them. */
#define FX_FN static __attribute__((unused))

FX_FN void fx_le16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}
FX_FN void fx_le32(uint8_t *p, uint32_t v) {
    fx_le16(p, (uint16_t)v);
    fx_le16(p + 2, (uint16_t)(v >> 16));
}
FX_FN void fx_le64(uint8_t *p, uint64_t v) {
    fx_le32(p, (uint32_t)v);
    fx_le32(p + 4, (uint32_t)(v >> 32));
}

/* A PNG-signed icon of `n` bytes (the reader checks only the signature). */
FX_FN uint8_t *fx_icon(size_t n, uint8_t fill) {
    static const uint8_t sig[8] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n' };
    uint8_t *b = malloc(n);
    memset(b, fill, n);
    memcpy(b, sig, 8);
    return b;
}

FX_FN const char *fx_param_json(const char *title_id, const char *title) {
    static char b[1024];
    snprintf(b, sizeof b,
             "{\"contentId\":\"UP9000-%s_00-GAME000000000000\",\"titleId\":\"%s\",\"contentVersion\":\"01.000.000\","
             "\"requiredSystemSoftwareVersion\":\"0x0750000000000000\",\"localizedParameters\":{\"defaultLanguage\":\"en-US\","
             "\"en-US\":{\"titleName\":\"%s\"}}}",
             title_id, title_id, title);
    return b;
}

/* ---- exFAT: 512-byte sectors, 4 KiB clusters ---- */

#define FX_EX_CLUSTER 4096u
#define FX_EX_FAT 12288u  /* sector 24 */
#define FX_EX_HEAP 32768u /* sector 64 */
#define FX_EX_CLUSTERS 64u

FX_FN uint8_t *fx_ex_cluster(uint8_t *img, uint32_t c) { return img + FX_EX_HEAP + (size_t)(c - 2) * FX_EX_CLUSTER; }
FX_FN void fx_ex_fat(uint8_t *img, uint32_t c, uint32_t next) { fx_le32(img + FX_EX_FAT + c * 4u, next); }

/* One entry set (file, stream, names) at `e`; returns the bytes it took. */
FX_FN size_t fx_ex_entry(uint8_t *e, const char *name, int dir, uint32_t first, uint64_t size, int contig) {
    size_t nl = strlen(name), names = (nl + 14) / 15, i;
    memset(e, 0, 32 * (2 + names));
    e[0] = 0x85;
    e[1] = (uint8_t)(1 + names);
    fx_le16(e + 4, dir ? 0x10 : 0x20);
    e[32] = 0xC0;
    e[33] = (uint8_t)(0x01 | (contig ? 0x02 : 0));
    e[35] = (uint8_t)nl;
    fx_le64(e + 32 + 8, size);
    fx_le32(e + 32 + 20, first);
    fx_le64(e + 32 + 24, size);
    for (i = 0; i < nl; i++) {
        uint8_t *n = e + 64 + (i / 15) * 32;
        n[0] = 0xC1;
        fx_le16(n + 2 + (i % 15) * 2, (uint8_t)name[i]);
    }
    return 32 * (2 + names);
}

/* The root's first cluster (2) is all bitmap / up-case / deleted entries, so the walk has to
 * follow the FAT to cluster 7 for sce_sys; param.json is contiguous, the icon a FAT chain over
 * clusters 5 and 8. `sfo`: the param file is a PARAM.SFO of `param_len` bytes. */
FX_FN uint8_t *fx_exfat(const uint8_t *param, size_t param_len, int sfo, size_t icon_len, int with_sys, size_t *len) {
    size_t n = FX_EX_HEAP + (size_t)FX_EX_CLUSTERS * FX_EX_CLUSTER, at, i;
    uint8_t *img = calloc(1, n), *d, *icon;
    memcpy(img + 3, "EXFAT   ", 8);
    fx_le32(img + 0x50, FX_EX_FAT / 512);
    fx_le32(img + 0x54, 8);
    fx_le32(img + 0x58, FX_EX_HEAP / 512);
    fx_le32(img + 0x5C, FX_EX_CLUSTERS);
    fx_le32(img + 0x60, 2);
    img[0x6C] = 9;
    img[0x6D] = 3;
    fx_ex_fat(img, 0, 0xFFFFFFF8u);
    fx_ex_fat(img, 1, 0xFFFFFFFFu);
    /* root: clusters 2 -> 7 */
    fx_ex_fat(img, 2, 7);
    fx_ex_fat(img, 7, 0xFFFFFFFFu);
    d = fx_ex_cluster(img, 2);
    d[0] = 0x81; /* allocation bitmap */
    d[32] = 0x82; /* up-case table */
    for (i = 64; i < FX_EX_CLUSTER; i += 32) d[i] = 0x05; /* deleted file entries */
    d = fx_ex_cluster(img, 7);
    at = fx_ex_entry(d, "eboot.bin", 0, 9, 16, 1);
    if (with_sys) at += fx_ex_entry(d + at, "SCE_SYS", 1, 3, FX_EX_CLUSTER, 1); /* any case */
    (void)at;
    fx_ex_fat(img, 3, 0xFFFFFFFFu);
    d = fx_ex_cluster(img, 3);
    at = fx_ex_entry(d, "a_long_file_name_over_fifteen.dat", 0, 9, 16, 1);
    at += fx_ex_entry(d + at, sfo ? "param.sfo" : "param.json", 0, 4, param_len, 1);
    if (icon_len) at += fx_ex_entry(d + at, "icon0.png", 0, 5, icon_len, 0);
    memcpy(fx_ex_cluster(img, 4), param, param_len);
    if (icon_len) {
        icon = fx_icon(icon_len, 0xA5);
        memcpy(fx_ex_cluster(img, 5), icon, FX_EX_CLUSTER);
        memcpy(fx_ex_cluster(img, 8), icon + FX_EX_CLUSTER, icon_len - FX_EX_CLUSTER);
        fx_ex_fat(img, 5, 8);
        fx_ex_fat(img, 8, 0xFFFFFFFFu);
        free(icon);
    }
    *len = n;
    return img;
}

/* ---- UFS2: 4 KiB fragments, 8 KiB blocks, one cylinder group ---- */

#define FX_UF_FSIZE 4096u
#define FX_UF_BSIZE 8192u
#define FX_UF_IBLKNO 24u
#define FX_UF_IPG 64u
#define FX_UF_FRAGS 256u

FX_FN uint8_t *fx_uf_inode(uint8_t *img, uint32_t n) { return img + FX_UF_IBLKNO * FX_UF_FSIZE + n * 256u; }

FX_FN void fx_uf_set(uint8_t *img, uint32_t n, int dir, uint64_t size, const uint64_t *db, int ndb, uint64_t ib0) {
    uint8_t *i = fx_uf_inode(img, n);
    int k;
    fx_le16(i, (uint16_t)(dir ? 0040755 : 0100644));
    fx_le64(i + 16, size);
    for (k = 0; k < ndb; k++) fx_le64(i + 112 + k * 8, db[k]);
    fx_le64(i + 208, ib0);
}

/* A directory block at fragment `frag`: (inode, name) pairs, the last record padded to 512. */
FX_FN void fx_uf_dir(uint8_t *img, uint32_t frag, const uint32_t *inos, const char *const *names, int n) {
    uint8_t *d = img + (size_t)frag * FX_UF_FSIZE;
    size_t at = 0;
    int k;
    for (k = 0; k < n; k++) {
        size_t nl = strlen(names[k]), rec = (8 + nl + 4) & ~(size_t)3;
        if (k == n - 1) rec = 512 - at;
        fx_le32(d + at, inos[k]);
        fx_le16(d + at + 4, (uint16_t)rec);
        d[at + 6] = 4;
        d[at + 7] = (uint8_t)nl;
        memcpy(d + at + 8, names[k], nl);
        at += rec;
    }
}

/* Root (inode 2) -> sce_sys (3) -> the param file (4) and an icon (5) of `icon_len` bytes, which
 * past 12 blocks goes on through the single indirect block. */
FX_FN uint8_t *fx_ufs2(const uint8_t *param, size_t param_len, int sfo, size_t icon_len, size_t *len) {
    size_t n = (size_t)FX_UF_FRAGS * FX_UF_FSIZE, k;
    uint8_t *img = calloc(1, n), *sb = img + 65536, *icon;
    uint32_t root_inos[3] = { 2, 2, 3 }, sys_inos[4] = { 3, 2, 4, 5 };
    const char *root_names[3] = { ".", "..", "sce_sys" }, *sys_names[4] = { ".", "..", sfo ? "param.sfo" : "param.json", "icon0.png" };
    uint64_t db[12];
    fx_le32(sb + 16, FX_UF_IBLKNO);
    fx_le32(sb + 44, 1);
    fx_le32(sb + 48, FX_UF_BSIZE);
    fx_le32(sb + 52, FX_UF_FSIZE);
    fx_le32(sb + 184, FX_UF_IPG);
    fx_le32(sb + 188, FX_UF_FRAGS);
    fx_le32(sb + 1372, 0x19540119u);
    db[0] = 32;
    fx_uf_dir(img, 32, root_inos, root_names, 3);
    fx_uf_set(img, 2, 1, 512, db, 1, 0);
    db[0] = 34;
    fx_uf_dir(img, 34, sys_inos, sys_names, icon_len ? 4 : 3);
    fx_uf_set(img, 3, 1, 512, db, 1, 0);
    db[0] = 36;
    memcpy(img + 36 * FX_UF_FSIZE, param, param_len);
    fx_uf_set(img, 4, 0, param_len, db, 1, 0);
    if (icon_len) {
        size_t blocks = (icon_len + FX_UF_BSIZE - 1) / FX_UF_BSIZE;
        uint64_t frag = 40, ib = 38;
        icon = fx_icon(icon_len, 0x5A);
        for (k = 0; k < blocks; k++, frag += 2) {
            size_t take = icon_len - k * FX_UF_BSIZE < FX_UF_BSIZE ? icon_len - k * FX_UF_BSIZE : FX_UF_BSIZE;
            memcpy(img + frag * FX_UF_FSIZE, icon + k * FX_UF_BSIZE, take);
            if (k < 12) db[k] = frag;
            else fx_le64(img + ib * FX_UF_FSIZE + (k - 12) * 8, frag);
        }
        fx_uf_set(img, 5, 0, icon_len, db, blocks < 12 ? (int)blocks : 12, blocks > 12 ? ib : 0);
        free(icon);
    }
    *len = n;
    return img;
}

/* ---- PFS: 4 KiB blocks ---- */

#define FX_PF_BS 4096u
#define FX_PF_BLOCKS 64u

FX_FN void fx_pf_inode(uint8_t *img, uint32_t n, int dir, uint32_t flags, uint64_t size, uint64_t size_c, uint32_t db0) {
    uint8_t *d = img + FX_PF_BS + n * 0xA8u;
    uint32_t blocks = (uint32_t)((size + FX_PF_BS - 1) / FX_PF_BS);
    int k;
    fx_le16(d, (uint16_t)(dir ? 0x4000 | 0x16D : 0x8000 | 0x124));
    fx_le32(d + 4, flags);
    fx_le64(d + 8, size);
    fx_le64(d + 16, size_c);
    fx_le32(d + 0x60, blocks ? blocks : 1);
    fx_le32(d + 0x64, db0);
    for (k = 1; k < 12; k++) fx_le32(d + 0x64 + k * 4, n == 0 ? 0 : 0xFFFFFFFFu);
}

/* A PFS directory at block `blk`: (inode, type, name) entries. Returns its byte length. */
FX_FN size_t fx_pf_dir(uint8_t *img, uint32_t blk, const uint32_t *inos, const uint32_t *types, const char *const *names, int n) {
    uint8_t *d = img + (size_t)blk * FX_PF_BS;
    size_t at = 0;
    int k;
    for (k = 0; k < n; k++) {
        size_t nl = strlen(names[k]), len = (16 + nl + 1 + 7) & ~(size_t)7;
        fx_le32(d + at, inos[k]);
        fx_le32(d + at + 4, types[k]);
        fx_le32(d + at + 8, (uint32_t)nl);
        fx_le32(d + at + 12, (uint32_t)len);
        memcpy(d + at + 16, names[k], nl);
        at += len;
    }
    return at;
}

/* super-root (0) -> uroot (2) -> sce_sys (3) -> param (4), icon (5). With `inner` set, uroot holds
 * only `inner_name` (inode 3, flags `inner_flags`: 1 = compressed) with those bytes. */
FX_FN uint8_t *fx_pfs(const uint8_t *param, size_t param_len, size_t icon_len, const uint8_t *inner, size_t inner_len,
                       const char *inner_name, uint32_t inner_flags, size_t *len) {
    size_t n = (size_t)FX_PF_BLOCKS * FX_PF_BS + inner_len, sl;
    uint8_t *img = calloc(1, n), *icon;
    uint32_t s_inos[2] = { 1, 2 }, s_types[2] = { 2, 3 };
    const char *s_names[2] = { "flat_path_table", "uroot" };
    uint32_t u_inos[3] = { 2, 2, 3 }, u_types[3] = { 4, 5, 3 };
    const char *u_names[3] = { ".", "..", "sce_sys" };
    uint32_t y_inos[4] = { 3, 2, 4, 5 }, y_types[4] = { 4, 5, 2, 2 };
    const char *y_names[4] = { ".", "..", "param.json", "icon0.png" };
    fx_le64(img, 2);
    fx_le64(img + 8, 20130315ull);
    fx_le32(img + 0x20, FX_PF_BS);
    fx_le64(img + 0x30, 8);
    fx_le64(img + 0x38, n / FX_PF_BS + 1);
    sl = fx_pf_dir(img, 2, s_inos, s_types, s_names, 2);
    fx_pf_inode(img, 0, 1, 0, sl, sl, 2);
    fx_pf_inode(img, 1, 0, 0, 16, 16, 10);
    if (inner) {
        u_inos[2] = 3;
        u_types[2] = 2;
        u_names[2] = inner_name;
        sl = fx_pf_dir(img, 3, u_inos, u_types, u_names, 3);
        fx_pf_inode(img, 2, 1, 0, sl, sl, 3);
        memcpy(img + (size_t)FX_PF_BLOCKS * FX_PF_BS, inner, inner_len);
        fx_pf_inode(img, 3, 0, inner_flags, inner_len, inner_flags & 1 ? inner_len * 3 : inner_len, FX_PF_BLOCKS);
        *len = n;
        return img;
    }
    sl = fx_pf_dir(img, 3, u_inos, u_types, u_names, 3);
    fx_pf_inode(img, 2, 1, 0, sl, sl, 3);
    sl = fx_pf_dir(img, 4, y_inos, y_types, y_names, icon_len ? 4 : 3);
    fx_pf_inode(img, 3, 1, 0, sl, sl, 4);
    memcpy(img + 5 * FX_PF_BS, param, param_len);
    fx_pf_inode(img, 4, 0, 0, param_len, param_len, 5);
    if (icon_len) {
        icon = fx_icon(icon_len, 0x3C);
        memcpy(img + 6 * FX_PF_BS, icon, icon_len);
        fx_pf_inode(img, 5, 0, 0, icon_len, icon_len, 6);
        free(icon);
    }
    *len = n;
    return img;
}

#endif
