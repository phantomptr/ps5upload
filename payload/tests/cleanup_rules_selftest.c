/* Host test for cleanup_rules.c: the apply-time path allowlist of every clean-up category, the
 * leftover-package walk's skip list, version ordering, content ids, and package headers read from
 * synthetic PS4 (\x7FCNT) and PS5 (\x7FFIH) containers. */
#include <stdio.h>
#include <string.h>
#include "../src/cleanup_rules.c"

static int fails;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            fails++;                                                   \
        }                                                              \
    } while (0)

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

typedef struct {
    const uint8_t *b;
    size_t n;
} mem_t;
static int mem_read(void *ctx, uint64_t off, void *buf, size_t n) {
    mem_t *m = ctx;
    if (off > m->n || n > m->n - off) return -1;
    memcpy(buf, m->b + off, n);
    return 0;
}

static uint8_t img[8192];

/* A CNT container at `base` with one entry (`entry_id`) holding `piece`. */
static size_t make_cnt(size_t base, uint32_t ctype, uint32_t flags, const char *cid, uint32_t entry_id,
                       const void *piece, size_t piece_len) {
    uint8_t *c = img + base;
    put_be32(c, 0x7F434E54u);
    put_be32(c + 0x10, 1);         /* one entry */
    put_be32(c + 0x18, 0x200);     /* table at +0x200 */
    memcpy(c + 0x40, cid, strlen(cid));
    put_be32(c + 0x74, ctype);
    put_be32(c + 0x78, flags);
    put_be32(c + 0x200, entry_id);
    put_be32(c + 0x210, 0x400);    /* data at +0x400 */
    put_be32(c + 0x214, (uint32_t)piece_len);
    memcpy(c + 0x400, piece, piece_len);
    return base + 0x400 + piece_len;
}

/* A PARAM.SFO with one text key. */
static size_t make_sfo(uint8_t *s, const char *key, const char *val) {
    memset(s, 0, 256);
    memcpy(s, "\0PSF", 4);
    put_le32(s + 8, 0x24);         /* keys */
    put_le32(s + 12, 0x40);        /* data */
    put_le32(s + 16, 1);
    s[0x14] = 0;                   /* key offset 0 */
    s[0x16] = 0x04;
    s[0x17] = 0x02;                /* utf-8 text */
    put_le32(s + 0x18, (uint32_t)strlen(val) + 1);
    put_le32(s + 0x1C, 8);
    put_le32(s + 0x20, 0);
    memcpy(s + 0x24, key, strlen(key));
    memcpy(s + 0x40, val, strlen(val));
    return 0x50;
}

int main(void) {
    char t[17], v[32];
    uint32_t ids[8];

    /* title and content ids */
    CHECK(scr_is_title_id("CUSA07842"));
    CHECK(scr_is_title_id("PPSA01650"));
    CHECK(!scr_is_title_id("cusa07842"));
    CHECK(!scr_is_title_id("CUSA0784"));
    CHECK(!scr_is_title_id("CUSA078421"));
    CHECK(!scr_is_title_id("external"));
    CHECK(scr_is_system_title("NPXS40140"));
    CHECK(!scr_is_system_title("PPSA01650"));
    CHECK(scr_tid_of_content_id("UP9000-CUSA07842_00-SCUS974290000001", t) && strcmp(t, "CUSA07842") == 0);
    CHECK(scr_label_of_content_id("UP9000-CUSA07842_00-SCUS974290000001", t) && strcmp(t, "SCUS974290000001") == 0);
    CHECK(!scr_tid_of_content_id("UP9000-CUSA07842-00-X", t));
    CHECK(!scr_tid_of_content_id("../../etc", t));
    CHECK(!scr_tid_of_content_id("UP9000-CUSA07842_00-A/B", t));

    /* versions */
    CHECK(scr_version_cmp("01.04", "01.10") == -1);
    CHECK(scr_version_cmp("01.10", "01.04") == 1);
    CHECK(scr_version_cmp("1.000.000", "01.000.000") == 0);
    CHECK(scr_version_cmp("01.04", "01.04.000") == 0);
    CHECK(scr_version_cmp("", "01.00") == -2);
    CHECK(scr_version_cmp("1.0a", "1.0") == -2);

    /* id lists */
    CHECK(scr_parse_id_list("{\"scan_id\":7,\"item_ids\":[3, 12,1]}", 34, "item_ids", ids, 8) == 3 && ids[1] == 12);
    CHECK(scr_parse_id_list("{\"item_ids\":[]}", 15, "item_ids", ids, 8) == 0);
    CHECK(scr_parse_id_list("{\"item_ids\":[1,]}", 17, "item_ids", ids, 8) == -1);
    CHECK(scr_parse_id_list("{\"item_ids\":[-1]}", 17, "item_ids", ids, 8) == -1);
    CHECK(scr_parse_id_list("{\"x\":1}", 7, "item_ids", ids, 8) == -1);
    CHECK(scr_parse_id_list("{\"item_ids\":[1,2,3]}", 20, "item_ids", ids, 2) == -1);
    /* Ids are 32-bit UNSIGNED and the list hands the top half through as it is, so whoever range-checks
     * them (sc_apply_start) must compare unsigned: as an int, 2147483649 is negative and "<= n_items". */
    CHECK(scr_parse_id_list("{\"item_ids\":[2147483649,4294967295]}", 35, "item_ids", ids, 8) == 2 &&
          ids[0] == 2147483649u && ids[1] == 4294967295u && (int)ids[0] < 0);
    CHECK(scr_parse_id_list("{\"item_ids\":[4294967296]}", 25, "item_ids", ids, 8) == -1);

    /* drives */
    CHECK(strcmp(scr_drive_of("/user/patch/CUSA07842"), "internal") == 0);
    CHECK(strcmp(scr_drive_of("/data/pkg/x.pkg"), "internal") == 0);
    CHECK(strcmp(scr_drive_of("/mnt/ext0/user/patch/X"), "ext0") == 0);
    CHECK(strcmp(scr_drive_of("/mnt/usb3/a.pkg"), "usb3") == 0);
    CHECK(strcmp(scr_drive_of("/mnt/usb9/a.pkg"), "") == 0);
    CHECK(strcmp(scr_drive_of("/system_ex/app"), "") == 0);

    /* A/B: a title's update folder on a drive that has user/, the item's own title only */
    CHECK(scr_path_allowed('A', "/user/patch/CUSA07842", "CUSA07842"));
    CHECK(scr_path_allowed('B', "/mnt/ext0/user/patch/CUSA02092", "CUSA02092"));
    CHECK(scr_path_allowed('B', "/mnt/ext1/user/patch/CUSA02092", "CUSA02092"));
    CHECK(!scr_path_allowed('A', "/user/patch/CUSA07842", "CUSA00001"));
    CHECK(!scr_path_allowed('A', "/user/patch/CUSA07842/patch.pkg", "CUSA07842"));
    CHECK(!scr_path_allowed('A', "/user/patch", "CUSA07842"));
    CHECK(!scr_path_allowed('A', "/user/app/CUSA07842", "CUSA07842"));
    CHECK(!scr_path_allowed('A', "/user/patch/../app/CUSA07842", "CUSA07842"));
    CHECK(!scr_path_allowed('A', "/user//patch/CUSA07842", "CUSA07842"));
    CHECK(!scr_path_allowed('B', "/user/patch/NPXS40140", "NPXS40140"));
    CHECK(!scr_path_allowed('B', "/mnt/usb0/user/patch/CUSA02092", "CUSA02092"));

    /* C: packages on a drive, never in the package library, homebrew folders, Sony's trees or other payloads */
    CHECK(scr_path_allowed('C', "/data/pkg/UP9000-CUSA07842_00-X.pkg", ""));
    CHECK(scr_path_allowed('C', "/user/data/patchdl/a.pkg", ""));
    CHECK(scr_path_allowed('C', "/data/x.pkg.part", ""));
    CHECK(scr_path_allowed('C', "/data/homebrew/game.pkg", ""));
    CHECK(scr_path_allowed('C', "/mnt/usb0/pkgs/game.pkg", ""));
    CHECK(scr_path_allowed('C', "/mnt/ext0/downloads/a.pkg", ""));
    CHECK(scr_path_allowed('C', "/data/ps5upload/downloads/a.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/ps5upload/pkg_library/a.pkg", ""));
    CHECK(!scr_path_allowed('C', "/mnt/usb1/ps5upload/pkg_library/a.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/ps5upload/pkg_temp/a.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/homebrew/PPSA03671-app/a.pkg", ""));
    CHECK(!scr_path_allowed('C', "/mnt/ext0/user/app/PPSA1/app.pkg", ""));
    CHECK(!scr_path_allowed('C', "/user/app/PPSA17221/app.pkg", ""));
    CHECK(!scr_path_allowed('C', "/user/patch/CUSA07842/patch.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/etaHEN/x.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/shadowmount/x.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data/pkg/notes.txt", ""));
    CHECK(!scr_path_allowed('C', "/data/../system/x.pkg", ""));
    CHECK(!scr_path_allowed('C', "/system_ex/x.pkg", ""));
    CHECK(!scr_path_allowed('C', "/data", ""));

    /* D: one entry inside pkg_temp/tests on a drive, or an unpack folder */
    CHECK(scr_path_allowed('D', "/data/ps5upload/pkg_temp/abc.pkg", ""));
    CHECK(scr_path_allowed('D', "/user/data/ps5upload/tests/run1", ""));
    CHECK(scr_path_allowed('D', "/mnt/ext0/ps5upload/pkg_temp/x", ""));
    CHECK(scr_path_allowed('D', "/mnt/usb2/ps5upload/tests/x", ""));
    CHECK(scr_path_allowed('D', "/mnt/ps5upload/.unpack/j1", ""));
    CHECK(!scr_path_allowed('D', "/data/ps5upload/pkg_temp", ""));
    CHECK(!scr_path_allowed('D', "/data/ps5upload/pkg_temp/a/b", ""));
    CHECK(!scr_path_allowed('D', "/data/ps5upload/pkg_library/a.pkg", ""));
    CHECK(!scr_path_allowed('D', "/data/ps5upload/backups/x", ""));

    /* E: download0.dat of the item's title only */
    CHECK(scr_path_allowed('E', "/user/download/PPSA22999/download0.dat", "PPSA22999"));
    CHECK(!scr_path_allowed('E', "/user/download/PPSA22999/download0.dat", "PPSA26344"));
    CHECK(!scr_path_allowed('E', "/user/download/PPSA22999", "PPSA22999"));
    CHECK(!scr_path_allowed('E', "/user/download/NPXS40140/download0.dat", "NPXS40140"));

    /* F: appmeta and PlayGo state of the item's title */
    CHECK(scr_path_allowed('F', "/user/appmeta/CUSA00001", "CUSA00001"));
    CHECK(scr_path_allowed('F', "/mnt/ext0/user/appmeta/CUSA00001", "CUSA00001"));
    CHECK(scr_path_allowed('F', "/user/playgo/patch/UP9000-CUSA07842_00-SCUS974290000001", "CUSA07842"));
    CHECK(!scr_path_allowed('F', "/user/playgo/patch/UP9000-CUSA07842_00-SCUS974290000001", "CUSA00001"));
    CHECK(!scr_path_allowed('F', "/user/appmeta/external", "external"));
    CHECK(!scr_path_allowed('F', "/user/appmeta", "CUSA00001"));
    CHECK(!scr_path_allowed('F', "/user/home/CUSA00001", "CUSA00001"));

    /* G and anything unknown: never */
    CHECK(!scr_path_allowed('G', "/data/homebrew/PPSA03671-app", "PPSA03671"));
    CHECK(!scr_path_allowed('Z', "/data/x.pkg", ""));

    /* walk skips */
    CHECK(scr_walk_skip("/data", "pldmgr"));
    CHECK(scr_walk_skip("/data", "etaHEN/payloads"));
    CHECK(!scr_walk_skip("/data", "etaHENx"));
    CHECK(scr_walk_skip("/data", "homebrew/PPSA03671-app"));
    CHECK(!scr_walk_skip("/data", "homebrew"));
    CHECK(scr_walk_skip("/mnt/ext0", "user"));
    CHECK(scr_walk_skip("/mnt/ext1", "system_data/priv"));
    CHECK(!scr_walk_skip("/mnt/usb0", "user"));
    CHECK(scr_walk_skip("/mnt/usb0", "ps5upload/pkg_library"));
    CHECK(!scr_walk_skip("/mnt/usb0", "ps5upload/downloads"));

    /* package headers */
    {
        static uint8_t sfo[256];
        scr_pkg_info_t pi;
        mem_t m;
        size_t sl = make_sfo(sfo, "APP_VER", "01.04");
        memset(img, 0, sizeof img);
        size_t n = make_cnt(0, 0x1A, 0x40000000u, "UP9000-CUSA07842_00-SCUS974290000001", 0x1000, sfo, sl);
        m.b = img;
        m.n = n;
        CHECK(scr_pkg_read(mem_read, &m, n, &pi) == 0);
        CHECK(strcmp(pi.title_id, "CUSA07842") == 0);
        CHECK(pi.kind == 'p');
        CHECK(strcmp(pi.version, "01.04") == 0);
        CHECK(!pi.ps5);
        CHECK(scr_sfo_str(sfo, sl, "APP_VER", v, sizeof v) && strcmp(v, "01.04") == 0);
        CHECK(!scr_sfo_str(sfo, sl, "APP", v, sizeof v));

        /* a PS5 container: FIH head, the CNT at 0x1000 with a param.json */
        const char *js = "{\"contentId\":\"UP4381-PPSA01650_00-YOUTUBESIEA00000\",\"contentVersion\":\"12.600.000\"}";
        memset(img, 0, sizeof img);
        put_be32(img, 0x7F464948u);
        img[0x58] = 0x00;
        img[0x59] = 0x10; /* 0x1000, little-endian */
        n = make_cnt(0x1000, 0x20, 0, "UP4381-PPSA01650_00-YOUTUBESIEA00000", 0x2000, js, strlen(js));
        m.n = n;
        CHECK(scr_pkg_read(mem_read, &m, n, &pi) == 0);
        CHECK(pi.ps5 && pi.kind == 'b' && strcmp(pi.title_id, "PPSA01650") == 0);
        CHECK(strcmp(pi.version, "12.600.000") == 0);

        /* add-on content, and a file that is not a package */
        memset(img, 0, sizeof img);
        n = make_cnt(0, 0x1B, 0, "UP9000-CUSA07842_00-DLC0000000000001", 0x9999, "x", 1);
        m.n = n;
        CHECK(scr_pkg_read(mem_read, &m, n, &pi) == 0 && pi.kind == 'd' && pi.version[0] == '\0');
        memset(img, 'A', sizeof img);
        m.n = sizeof img;
        CHECK(scr_pkg_read(mem_read, &m, sizeof img, &pi) == -1);
        /* a FIH whose CNT offset points past the end */
        memset(img, 0, sizeof img);
        put_be32(img, 0x7F464948u);
        img[0x5B] = 0x7F;
        m.n = 0x400;
        CHECK(scr_pkg_read(mem_read, &m, 0x400, &pi) == -1);
    }

    if (fails) return 1;
    printf("cleanup rules selftest: ok\n");
    return 0;
}
