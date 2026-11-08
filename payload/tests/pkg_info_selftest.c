/* Host-side tests for the console's package reader (pkg_info.c): a PS4 CNT with PARAM.SFO and
 * icon0.png, a PS5 \x7FFIH envelope with param.json, and damaged packages (offsets past the end,
 * huge sizes, an encrypted icon, garbage) that must answer a cause or leave fields empty, never
 * read out of bounds. With a path argument it prints what it read from that file instead (a real
 * package: compare with the engine's /api/pkg/inspect).
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/pkg_info.c"
#include "pkg_install_logic.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

typedef struct {
    const uint8_t *b;
    size_t n;
} mem_t;

static int mem_read(void *ctx, uint64_t off, void *buf, size_t len) {
    mem_t *m = ctx;
    if (off > m->n || len > m->n - off) return -1; /* the reader must never ask past the end */
    memcpy(buf, m->b + off, len);
    return 0;
}

static void put_be32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}
static void put_le32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}
static void put_le16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

/* A PARAM.SFO with string keys and one int (SYSTEM_VER). Returns its length. */
static size_t build_sfo(uint8_t *b, const char *const *keys, const char *const *vals, size_t nstr, uint32_t sys_ver) {
    size_t n = nstr + 1, i, koff = 0, doff = 0;
    size_t key_tab = 20 + n * 16, data_tab;
    char kbuf[512];
    for (i = 0; i < nstr; i++) koff += strlen(keys[i]) + 1;
    koff += strlen("SYSTEM_VER") + 1;
    data_tab = key_tab + ((koff + 3) & ~(size_t)3);
    memset(b, 0, 4096);
    memcpy(b, "\0PSF", 4);
    put_le32(b + 4, 0x101);
    put_le32(b + 8, (uint32_t)key_tab);
    put_le32(b + 12, (uint32_t)data_tab);
    put_le32(b + 16, (uint32_t)n);
    koff = 0;
    for (i = 0; i < n; i++) {
        uint8_t *e = b + 20 + i * 16;
        const char *k = i < nstr ? keys[i] : "SYSTEM_VER";
        size_t kl = strlen(k) + 1;
        memcpy(kbuf, k, kl);
        memcpy(b + key_tab + koff, kbuf, kl);
        put_le16(e, (uint16_t)koff);
        koff += kl;
        if (i < nstr) {
            size_t vl = strlen(vals[i]) + 1, cap = (vl + 3) & ~(size_t)3;
            put_le16(e + 2, 0x0204);
            put_le32(e + 4, (uint32_t)vl);
            put_le32(e + 8, (uint32_t)cap);
            put_le32(e + 12, (uint32_t)doff);
            memcpy(b + data_tab + doff, vals[i], vl);
            doff += cap;
        } else {
            put_le16(e + 2, 0x0404);
            put_le32(e + 4, 4);
            put_le32(e + 8, 4);
            put_le32(e + 12, (uint32_t)doff);
            put_le32(b + data_tab + doff, sys_ver);
            doff += 4;
        }
    }
    return data_tab + doff;
}

static const uint8_t k_png[16] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n', 0, 0, 0, 13, 'I', 'H', 'D', 'R' };

/* A CNT at `base` in buf (entry table at base+0x100, data after it) with the given entries. */
typedef struct {
    uint32_t id, flags;
    const void *data;
    size_t len;
} ent_t;

static size_t build_cnt(uint8_t *buf, size_t base, const char *cid, const ent_t *ents, size_t n) {
    uint8_t *h = buf + base;
    size_t data = 0x100 + n * 0x20, i;
    put_be32(h, 0x7F434E54);
    put_be32(h + 0x10, (uint32_t)n);
    put_be32(h + 0x18, 0x100);
    memcpy(h + 0x40, cid, strlen(cid));
    for (i = 0; i < n; i++) {
        uint8_t *e = h + 0x100 + i * 0x20;
        put_be32(e, ents[i].id);
        put_be32(e + 4, ents[i].flags);
        put_be32(e + 0x10, (uint32_t)data);
        put_be32(e + 0x14, (uint32_t)ents[i].len);
        memcpy(h + data, ents[i].data, ents[i].len);
        data += (ents[i].len + 15) & ~(size_t)15;
    }
    return base + data;
}

static uint8_t g_buf[64 * 1024];

static const char *parse(size_t n, pkg_info_t *o) {
    mem_t m = { g_buf, n };
    return pkg_info_read(mem_read, &m, n, o);
}

static void ps4_game(void) {
    static const char *keys[] = { "APP_VER", "CATEGORY", "TITLE", "TITLE_ID", "CONTENT_ID" };
    static const char *vals[] = { "01.02", "gd", "Jak X: Combat Racing", "CUSA07842",
                                  "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0" };
    uint8_t sfo[4096];
    size_t sl = build_sfo(sfo, keys, vals, 5, 0x05050000);
    ent_t e[2] = { { 0x1000, 0, sfo, sl }, { 0x1200, 0, k_png, sizeof k_png } };
    pkg_info_t o;
    size_t n;
    memset(g_buf, 0, sizeof g_buf);
    n = build_cnt(g_buf, 0, "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0", e, 2);
    CHECK(parse(n, &o) == NULL);
    CHECK(strcmp(o.content_id, "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0") == 0);
    CHECK(strcmp(o.title, "Jak X: Combat Racing") == 0);
    CHECK(strcmp(o.title_id, "CUSA07842") == 0);
    CHECK(strcmp(o.category, "gd") == 0);
    CHECK(strcmp(o.app_ver, "01.02") == 0);
    CHECK(strcmp(o.fw, "5.05") == 0);
    CHECK(strcmp(o.platform, "ps4") == 0);
    CHECK(strcmp(pkg_info_kind(&o), "game") == 0);
    CHECK(!o.fih);
    CHECK(o.icon_len == sizeof k_png && o.icon_off > 0 && memcmp(g_buf + o.icon_off, k_png, 8) == 0);

    /* the same package with the icon entry encrypted: no icon offered */
    e[1].flags = 0x80000000u;
    memset(g_buf, 0, sizeof g_buf);
    n = build_cnt(g_buf, 0, "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0", e, 2);
    CHECK(parse(n, &o) == NULL);
    CHECK(o.icon_off == 0 && o.icon_len == 0);
}

static void ps5_image(void) {
    static const char js[] =
        "{\"applicationCategoryType\":0,\"contentId\":\"IV0000-PPSA30528_00-DEMO000000000000\","
        "\"contentVersion\":\"01.000.003\",\"localizedParameters\":{\"defaultLanguage\":\"ja-JP\","
        "\"en-US\":{\"titleName\":\"English \\\"Name\\\"\"},\"ja-JP\":{\"titleName\":\"\\u65e5\\u672c\"}},"
        "\"requiredSystemSoftwareVersion\":\"0x0750000000000000\",\"titleId\":\"PPSA30528\"}\0\0\0";
    ent_t e[2] = { { 0x2000, 0, js, sizeof js }, { 0x1200, 0, k_png, sizeof k_png } };
    pkg_info_t o;
    size_t n;
    memset(g_buf, 0, sizeof g_buf);
    put_be32(g_buf, 0x7F464948);
    g_buf[5] = 0x00; /* fake-signed */
    put_le16(g_buf + 6, 3);
    put_le32(g_buf + 0x58, 0x1000);
    n = build_cnt(g_buf, 0x1000, "IV0000-PPSA30528_00-DEMO000000000000", e, 2);
    CHECK(parse(n, &o) == NULL);
    CHECK(o.fih && o.fake);
    CHECK(strcmp(o.platform, "ps5") == 0);
    CHECK(strcmp(o.title_id, "PPSA30528") == 0);
    CHECK(strcmp(o.app_ver, "01.000.003") == 0);
    CHECK(strcmp(o.title, "\xe6\x97\xa5\xe6\x9c\xac") == 0); /* the default language's name, \u-decoded */
    CHECK(strcmp(o.fw, "7.50") == 0);
    CHECK(strcmp(o.category, "gd") == 0);
    CHECK(o.icon_off == 0x1000 + 0x100 + 2 * 0x20 + ((sizeof js + 15) & ~(size_t)15));

    /* patch flag in the CNT flags word */
    put_be32(g_buf + 0x1000 + 0x78, 0x00100000u);
    CHECK(parse(n, &o) == NULL);
    CHECK(strcmp(pkg_info_kind(&o), "patch") == 0);

    /* #724: an add-on's param.json has no category either; its content type (0x21) says it */
    put_be32(g_buf + 0x1000 + 0x78, 0x0a020000u);
    put_be32(g_buf + 0x1000 + 0x74, 0x21u);
    CHECK(parse(n, &o) == NULL);
    CHECK(strcmp(o.category, "ac") == 0);
    CHECK(strcmp(pkg_info_kind(&o), "dlc") == 0);
    put_be32(g_buf + 0x1000 + 0x74, 0x20u);
    CHECK(parse(n, &o) == NULL);
    CHECK(strcmp(pkg_info_kind(&o), "game") == 0);
}

static void damaged(void) {
    pkg_info_t o;
    size_t n, i;
    static const char *keys[] = { "TITLE" };
    static const char *vals[] = { "x" };
    uint8_t sfo[4096];
    size_t sl = build_sfo(sfo, keys, vals, 1, 0);
    ent_t e[1] = { { 0x1000, 0, sfo, sl } };

    memset(g_buf, 0, sizeof g_buf);
    memcpy(g_buf, "not a package at all", 20);
    CHECK(parse(sizeof g_buf, &o) != NULL);
    CHECK(parse(16, &o) != NULL);

    /* FIH pointing past the end */
    memset(g_buf, 0, sizeof g_buf);
    put_be32(g_buf, 0x7F464948);
    put_le16(g_buf + 6, 3);
    put_le32(g_buf + 0x58, 0xFFFFFFF0u);
    put_le32(g_buf + 0x5C, 0xFFFFFFFFu);
    CHECK(parse(sizeof g_buf, &o) != NULL);

    /* entry table / entries out of range: still answers, fields empty */
    memset(g_buf, 0, sizeof g_buf);
    n = build_cnt(g_buf, 0, "EP0000-CUSA00001_00-AAAAAAAAAAAAAAAA", e, 1);
    put_be32(g_buf + 0x100 + 0x10, 0xFFFFFFF0u); /* SFO offset past the end */
    CHECK(parse(n, &o) == NULL);
    CHECK(o.title[0] == '\0');
    CHECK(strcmp(o.title_id, "CUSA00001") == 0); /* from the content id */
    put_be32(g_buf + 0x100 + 0x10, 0x120);
    put_be32(g_buf + 0x100 + 0x14, 0x7FFFFFFFu); /* huge SFO */
    CHECK(parse(n, &o) == NULL);
    put_be32(g_buf + 0x10, 0x00FFFFFFu); /* absurd entry count */
    CHECK(parse(n, &o) == NULL);
    put_be32(g_buf + 0x10, 1);
    put_be32(g_buf + 0x18, 0xFFFFFFFFu); /* table past the end */
    CHECK(parse(n, &o) == NULL);

    /* SFO with bad internal offsets */
    memset(g_buf, 0, sizeof g_buf);
    n = build_cnt(g_buf, 0, "EP0000-CUSA00001_00-AAAAAAAAAAAAAAAA", e, 1);
    put_le32(g_buf + 0x120 + 20 + 12, 0xFFFFFFF0u); /* first entry's data offset */
    put_le32(g_buf + 0x120 + 8, 0xFFFFFFF0u);       /* key table */
    CHECK(parse(n, &o) == NULL);

    /* every prefix of a valid package, and single-byte corruptions: no crash, no read past the end */
    memset(g_buf, 0, sizeof g_buf);
    n = build_cnt(g_buf, 0, "EP0000-CUSA00001_00-AAAAAAAAAAAAAAAA", e, 1);
    for (i = 0; i <= n; i++) (void)parse(i, &o);
    for (i = 0; i < n; i++) {
        uint8_t keep = g_buf[i];
        g_buf[i] ^= 0xA5;
        (void)parse(n, &o);
        g_buf[i] = keep;
    }
}

static void fw_text(void) {
    char t[16];
    pkg_info_fw_text(0x05050000, t, sizeof t);
    CHECK(strcmp(t, "5.05") == 0);
    pkg_info_fw_text(0x10000000, t, sizeof t);
    CHECK(strcmp(t, "10.00") == 0);
    pkg_info_fw_text(0, t, sizeof t);
    CHECK(t[0] == '\0');
    CHECK(hex_fw("0x0450000000000000") == 0x04500000u);
    CHECK(hex_fw("junk") == 0);
}

static void install_logic(void) {
    char name[64], carry[16];
    size_t have = 0;
    static const uint8_t a[] = "xxxxAVA1TR", b2[] = "USTyyyy", plain[] = "nothing to see here";
    CHECK(pkg_needs_staging("/mnt/usb0/x.pkg"));
    CHECK(pkg_needs_staging("/mnt/ext1/games/x.pkg"));
    CHECK(!pkg_needs_staging("/data/pkgs/x.pkg"));
    CHECK(!pkg_needs_staging("/user/data/x.pkg"));
    CHECK(pkg_version_older("1.3.9", "1.4.0"));
    CHECK(pkg_version_older("1.4", "1.4.1"));
    CHECK(!pkg_version_older("1.4.0", "1.4.0"));
    CHECK(!pkg_version_older("1.10.0", "1.4.0"));
    pkg_stage_name("UP9000-CUSA07842_00-SCUSJAKXPS4PORT0", 7, name, sizeof name);
    CHECK(strcmp(name, "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0.pkg") == 0);
    pkg_stage_name("../../etc", 0xabc, name, sizeof name);
    CHECK(strcmp(name, "minion-00000abc.pkg") == 0);
    pkg_stage_name("", 1, name, sizeof name);
    CHECK(strcmp(name, "minion-00000001.pkg") == 0);
    CHECK(pkg_is_payload_name("kstuff.ELF") && pkg_is_payload_name("a.bin") && !pkg_is_payload_name("a.elf.txt"));
    /* the marker split across two blocks, inside one, and absent */
    CHECK(pkg_scan_marker(a, sizeof a - 1, carry, &have) == 0);
    CHECK(pkg_scan_marker(b2, sizeof b2 - 1, carry, &have) == 1);
    have = 0;
    CHECK(pkg_scan_marker((const uint8_t *)"..AVA1TRUST..", 13, carry, &have) == 1);
    have = 0;
    CHECK(pkg_scan_marker(plain, sizeof plain - 1, carry, &have) == 0);
    /* tiny blocks, one byte at a time */
    {
        const char *s = "zzAVA1TRUSTzz";
        size_t i;
        int hit = 0;
        have = 0;
        for (i = 0; s[i]; i++) hit |= pkg_scan_marker((const uint8_t *)s + i, 1, carry, &have);
        CHECK(hit);
    }
}

static int file_read(void *ctx, uint64_t off, void *buf, size_t len) {
    FILE *f = ctx;
    if (fseeko(f, (off_t)off, SEEK_SET) != 0) return -1;
    return fread(buf, 1, len, f) == len ? 0 : -1;
}

int main(int argc, char **argv) {
    if (argc > 1) {
        FILE *f = fopen(argv[1], "rb");
        pkg_info_t o;
        const char *bad;
        uint64_t size;
        if (!f) return 2;
        fseeko(f, 0, SEEK_END);
        size = (uint64_t)ftello(f);
        bad = pkg_info_read(file_read, f, size, &o);
        fclose(f);
        if (bad) {
            printf("error: %s\n", bad);
            return 1;
        }
        printf("title=%s\ntitle_id=%s\ncontent_id=%s\ncategory=%s kind=%s\nversion=%s\nfw=%s\nplatform=%s fih=%d fake=%d\n"
               "icon=%llu+%u\n",
               o.title, o.title_id, o.content_id, o.category, pkg_info_kind(&o), o.app_ver, o.fw, o.platform, o.fih,
               o.fake, (unsigned long long)o.icon_off, o.icon_len);
        return 0;
    }
    ps4_game();
    ps5_image();
    damaged();
    fw_text();
    install_logic();
    if (failures) {
        fprintf(stderr, "pkg_info_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("pkg_info_selftest: ok\n");
    return 0;
}
