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

int main(void) {
    test_exfat();
    test_ufs2();
    test_pfs();
    test_foreign();
    if (g_fail) {
        fprintf(stderr, "smbd_image_selftest: %d failure(s)\n", g_fail);
        return 1;
    }
    printf("smbd_image_selftest: ok\n");
    return 0;
}
