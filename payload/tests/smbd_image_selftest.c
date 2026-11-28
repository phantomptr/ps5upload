/* Host self-test: a disk image's own metadata (payload/smb/smbd_image.c, #963). exFAT, UFS2 and
 * PFS images built in memory (image_fixtures.h) give their param.json / param.sfo title, ids and
 * icon0.png; a PFS-wrapped .exfat is read through; a compressed one, a foreign file, an image
 * without sce_sys, a damaged one and a failing read each say why; reads stay small. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "image_fixtures.h"
#include "smbd_image.h"

static int g_fail;
#define CHECK(c)                                                         \
    do {                                                                 \
        if (!(c)) {                                                      \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
            g_fail++;                                                    \
        }                                                                \
    } while (0)

typedef struct {
    const uint8_t *b;
    size_t n;
    unsigned reads;
    uint64_t bytes;
    unsigned fail_after; /* 0 = never */
} mem_t;

static int mem_rd(void *ctx, uint64_t off, void *buf, size_t len) {
    mem_t *m = ctx;
    m->reads++;
    m->bytes += len;
    if (m->fail_after && m->reads > m->fail_after) return -1;
    if (off > m->n || len > m->n - off) return -1;
    memcpy(buf, m->b + off, len);
    return 0;
}

static const char *run(const uint8_t *img, size_t n, size_t icon_max, img_meta_t *out, mem_t *m) {
    memset(m, 0, sizeof *m);
    m->b = img;
    m->n = n;
    return img_meta_read(mem_rd, m, n, icon_max, out);
}

/* A PARAM.SFO of string keys. Returns its length. */
static size_t build_sfo(uint8_t *b, const char *const *keys, const char *const *vals, size_t n) {
    size_t i, koff = 0, doff = 0, key_tab = 20 + n * 16, data_tab;
    for (i = 0; i < n; i++) koff += strlen(keys[i]) + 1;
    data_tab = key_tab + ((koff + 3) & ~(size_t)3);
    memset(b, 0, 4096);
    memcpy(b, "\0PSF", 4);
    fx_le32(b + 4, 0x101);
    fx_le32(b + 8, (uint32_t)key_tab);
    fx_le32(b + 12, (uint32_t)data_tab);
    fx_le32(b + 16, (uint32_t)n);
    koff = 0;
    for (i = 0; i < n; i++) {
        uint8_t *e = b + 20 + i * 16;
        size_t kl = strlen(keys[i]) + 1, vl = strlen(vals[i]) + 1, cap = (vl + 3) & ~(size_t)3;
        memcpy(b + key_tab + koff, keys[i], kl);
        fx_le16(e, (uint16_t)koff);
        koff += kl;
        fx_le16(e + 2, 0x0204);
        fx_le32(e + 4, (uint32_t)vl);
        fx_le32(e + 8, (uint32_t)cap);
        fx_le32(e + 12, (uint32_t)doff);
        memcpy(b + data_tab + doff, vals[i], vl);
        doff += cap;
    }
    return data_tab + doff;
}

static void test_exfat(void) {
    const char *pj = fx_param_json("PPSA23012", "Call of Duty");
    size_t n;
    uint8_t *img = fx_exfat((const uint8_t *)pj, strlen(pj), 0, 6000, 1, &n);
    img_meta_t m;
    mem_t mm;
    const char *why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why == NULL);
    CHECK(strcmp(m.info.title, "Call of Duty") == 0);
    CHECK(strcmp(m.info.title_id, "PPSA23012") == 0);
    CHECK(strcmp(m.info.content_id, "UP9000-PPSA23012_00-GAME000000000000") == 0);
    CHECK(strcmp(m.info.platform, "ps5") == 0 && strcmp(m.info.fw, "7.50") == 0);
    CHECK(strcmp(m.format, "exfat") == 0);
    CHECK(m.icon && m.icon_len == 6000 && m.icon[0] == 0x89 && m.icon[4095] == 0xA5 && m.icon[5999] == 0xA5);
    /* boot sector + FAT + folders (4 KiB windows) + param + icon: nothing near the image's size */
    CHECK(mm.bytes < 32 * 1024 && mm.reads < 24);
    img_meta_free(&m);
    CHECK(m.icon == NULL);

    /* no icon wanted: none read */
    why = run(img, n, 0, &m, &mm);
    CHECK(why == NULL && m.icon == NULL && strcmp(m.info.title, "Call of Duty") == 0);
    img_meta_free(&m);
    /* an icon over the cap is left out, the title still read */
    why = run(img, n, 4000, &m, &mm);
    CHECK(why == NULL && m.icon == NULL && m.info.title_id[0]);
    img_meta_free(&m);
    /* a read that fails part way: worth another try */
    memset(&mm, 0, sizeof mm);
    mm.b = img;
    mm.n = n;
    mm.fail_after = 3;
    why = img_meta_read(mem_rd, &mm, n, 1024 * 1024, &m);
    CHECK(why && strcmp(why, "img_read") == 0 && m.info.title[0] == '\0' && m.icon == NULL);
    /* cut short: the folders point past its end */
    why = run(img, FX_EX_HEAP + 2 * FX_EX_CLUSTER, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_damaged") == 0);
    free(img);

    /* no sce_sys */
    img = fx_exfat((const uint8_t *)pj, strlen(pj), 0, 0, 0, &n);
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_no_param") == 0);
    free(img);

    /* a PS4 image: param.sfo */
    {
        const char *k[] = { "CATEGORY", "CONTENT_ID", "TITLE", "TITLE_ID" };
        const char *v[] = { "gd", "EP4293-CUSA32097_00-GAME000000000000", "A PS4 Game", "CUSA32097" };
        uint8_t sfo[4096];
        size_t sl = build_sfo(sfo, k, v, 4);
        img = fx_exfat(sfo, sl, 1, 0, 1, &n);
        why = run(img, n, 1024 * 1024, &m, &mm);
        CHECK(why == NULL && strcmp(m.info.title, "A PS4 Game") == 0 && strcmp(m.info.title_id, "CUSA32097") == 0 &&
              strcmp(m.info.platform, "ps4") == 0 && strcmp(m.info.category, "gd") == 0);
        img_meta_free(&m);
        free(img);
    }
}

static void test_ufs2(void) {
    const char *pj = fx_param_json("PPSA01234", "Venus Vacation");
    size_t n;
    uint8_t *img = fx_ufs2((const uint8_t *)pj, strlen(pj), 0, 110000, &n);
    img_meta_t m;
    mem_t mm;
    const char *why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why == NULL);
    CHECK(strcmp(m.info.title, "Venus Vacation") == 0 && strcmp(m.info.title_id, "PPSA01234") == 0);
    CHECK(strcmp(m.format, "ufs2") == 0);
    /* 14 blocks: 12 direct and 2 through the indirect block */
    CHECK(m.icon && m.icon_len == 110000 && m.icon[0] == 0x89 && m.icon[100000] == 0x5A && m.icon[109999] == 0x5A);
    img_meta_free(&m);
    free(img);
    /* a superblock that does not add up */
    img = fx_ufs2((const uint8_t *)pj, strlen(pj), 0, 0, &n);
    fx_le32(img + 65536 + 52, 3000); /* not a power of two */
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_damaged") == 0);
    free(img);
}

static void test_pfs(void) {
    const char *pj = fx_param_json("PPSA05555", "A PFS Game");
    size_t n, en;
    uint8_t *img = fx_pfs((const uint8_t *)pj, strlen(pj), 5000, NULL, 0, NULL, 0, &n), *ex;
    img_meta_t m;
    mem_t mm;
    const char *why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why == NULL);
    CHECK(strcmp(m.info.title, "A PFS Game") == 0 && strcmp(m.info.title_id, "PPSA05555") == 0);
    CHECK(strcmp(m.format, "pfs") == 0 && m.icon && m.icon_len == 5000 && m.icon[4999] == 0x3C);
    img_meta_free(&m);
    free(img);

    /* a .exfat wrapped in a PFS: read through */
    pj = fx_param_json("PPSA07777", "Wrapped");
    ex = fx_exfat((const uint8_t *)pj, strlen(pj), 0, 6000, 1, &en);
    img = fx_pfs(NULL, 0, 0, ex, en, "PPSA07777.exfat", 0, &n);
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why == NULL && strcmp(m.info.title, "Wrapped") == 0 && strcmp(m.format, "exfat") == 0 && m.icon_len == 6000);
    img_meta_free(&m);
    free(img);

    /* compressed (a .ffpfsc): not decoded here */
    img = fx_pfs(NULL, 0, 0, ex, en, "PPSA07777.exfat", 1, &n);
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_compressed") == 0 && m.info.title[0] == '\0');
    free(img);
    free(ex);

    /* a signed PFS is a layout this reader does not take */
    pj = fx_param_json("PPSA05555", "A PFS Game");
    img = fx_pfs((const uint8_t *)pj, strlen(pj), 0, NULL, 0, NULL, 0, &n);
    fx_le16(img + 0x1C, 0x1);
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_unsupported") == 0);
    free(img);
}

static void test_foreign(void) {
    uint8_t *junk = calloc(1, 200000);
    img_meta_t m;
    mem_t mm;
    const char *why;
    memset(junk, 'x', 200000);
    why = run(junk, 200000, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_unknown") == 0);
    why = run(junk, 10, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_unknown") == 0);
    free(junk);
}

/* A read that fails at any point of any image is "img_read" (the library's cache does not keep it,
 * so the next scan tries again), or the image read without its cover: never "img_damaged" or
 * "img_no_param", which the cache keeps for as long as the file is unchanged. The UFS2 reader
 * turned a failed inode read under sce_sys into damage. */
static void sweep_failures(const char *label, const uint8_t *img, size_t n) {
    img_meta_t m;
    mem_t mm;
    unsigned total, k;
    const char *why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why == NULL);
    img_meta_free(&m);
    total = mm.reads;
    for (k = 1; k < total; k++) {
        memset(&mm, 0, sizeof mm);
        mm.b = img;
        mm.n = n;
        mm.fail_after = k; /* read k+1 and the later ones fail */
        why = img_meta_read(mem_rd, &mm, n, 1024 * 1024, &m);
        if (why && strcmp(why, "img_read") != 0) {
            fprintf(stderr, "%s: read #%u failing gave %s\n", label, k + 1, why);
            g_fail++;
        }
        img_meta_free(&m);
    }
}

static void test_failed_reads(void) {
    const char *pj = fx_param_json("PPSA01234", "Venus Vacation");
    size_t n;
    uint8_t *img = fx_ufs2((const uint8_t *)pj, strlen(pj), 0, 110000, &n);
    sweep_failures("ufs2", img, n);
    free(img);
    img = fx_exfat((const uint8_t *)pj, strlen(pj), 0, 6000, 1, &n);
    sweep_failures("exfat", img, n);
    free(img);
    img = fx_pfs((const uint8_t *)pj, strlen(pj), 5000, NULL, 0, NULL, 0, &n);
    sweep_failures("pfs", img, n);
    free(img);
}

/* exFAT folders that are long FAT chains (512-byte clusters, every link in another FAT sector, all
 * deleted entries so each lookup reads the whole 1 MiB): a folder is read window by window, and each
 * window used to walk its chain again from the first cluster, ~800,000 reads for one image. */
#define LC_CS 512u
#define LC_DIRCL 2048u
#define LC_NCL (2 * LC_DIRCL + 16)
#define LC_FAT 512u
#define LC_HEAP (LC_FAT + ((LC_NCL * 4u + 511u) / 512u) * 512u)
static uint8_t *lc_cl(uint8_t *img, uint32_t c) { return img + LC_HEAP + (size_t)(c - 2) * LC_CS; }
static uint32_t lc_chain(uint32_t base, uint32_t k) { return base + (k % 2 ? LC_DIRCL / 2 + k / 2 : k / 2); }

static void test_long_chains(void) {
    size_t n = LC_HEAP + (size_t)LC_NCL * LC_CS, i;
    uint8_t *img = calloc(1, n);
    uint32_t k, rbase = 2, sbase = 2 + LC_DIRCL;
    img_meta_t m;
    mem_t mm;
    const char *why;
    memcpy(img + 3, "EXFAT   ", 8);
    fx_le32(img + 0x50, LC_FAT / 512);
    fx_le32(img + 0x58, LC_HEAP / 512);
    fx_le32(img + 0x5C, LC_NCL);
    fx_le32(img + 0x60, lc_chain(rbase, 0));
    img[0x6C] = 9;
    img[0x6D] = 0;
    for (k = 0; k < LC_DIRCL; k++) {
        fx_le32(img + LC_FAT + lc_chain(rbase, k) * 4u, k + 1 < LC_DIRCL ? lc_chain(rbase, k + 1) : 0xFFFFFFFFu);
        fx_le32(img + LC_FAT + lc_chain(sbase, k) * 4u, k + 1 < LC_DIRCL ? lc_chain(sbase, k + 1) : 0xFFFFFFFFu);
        for (i = 0; i < LC_CS; i += 32) {
            lc_cl(img, lc_chain(rbase, k))[i] = 0x05;
            lc_cl(img, lc_chain(sbase, k))[i] = 0x05;
        }
    }
    fx_ex_entry(lc_cl(img, lc_chain(rbase, LC_DIRCL - 1)) + LC_CS - 96, "sce_sys", 1, lc_chain(sbase, 0),
                (uint64_t)LC_DIRCL * LC_CS, 0);
    why = run(img, n, 1024 * 1024, &m, &mm);
    CHECK(why && strcmp(why, "img_no_param") == 0);
    /* each cluster's FAT sector and data about once per folder pass: a few thousand reads */
    CHECK(mm.reads < 20000);
    img_meta_free(&m);
    free(img);
}

int main(void) {
    test_exfat();
    test_ufs2();
    test_pfs();
    test_foreign();
    test_failed_reads();
    test_long_chains();
    if (g_fail) {
        fprintf(stderr, "smbd_image_selftest: %d failure(s)\n", g_fail);
        return 1;
    }
    printf("smbd_image_selftest: ok\n");
    return 0;
}
