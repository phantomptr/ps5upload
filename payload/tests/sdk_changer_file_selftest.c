/* Host-side integration test for SDK Changer's filesystem safety.
 *
 * Covers the regressions that pure param/ELF parser tests cannot see:
 * package-installed titles must not be metadata-only "successes", signed and
 * non-ELF candidates must not get large backups, and real source + appmeta
 * backups must round-trip through Restore. */
#include <dirent.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static char g_test_app_base[1024];
static char g_test_appmeta_base[1024];

static const char *test_app_base(void) { return g_test_app_base; }
static const char *test_appmeta_base(void) { return g_test_appmeta_base; }

/* sdk_changer_scan now appends live overlay state. This filesystem test is
 * about patch/restore and intentionally has no lifecycle thread. */
int fakelib_overlay_status_json(char *buf, size_t cap) {
    return snprintf(buf, cap, "{\"state\":\"watching\",\"title_id\":\"\",\"error\":\"\"}");
}

#define APP_BASE test_app_base()
#define APPMETA_BASE test_appmeta_base()
#include "../src/sdk_changer.c"

static int failures = 0;

#define CHECK(expr)                                                         \
    do {                                                                    \
        if (!(expr)) {                                                      \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);       \
            failures++;                                                     \
        }                                                                   \
    } while (0)

static int mkdir_tree(const char *path) {
    char tmp[2048];
    int n = snprintf(tmp, sizeof(tmp), "%s", path);
    if (n < 0 || (size_t)n >= sizeof(tmp)) return -1;
    for (char *p = tmp + 1; *p; p++) {
        if (*p != '/') continue;
        *p = '\0';
        if (mkdir(tmp, 0755) != 0 && errno != EEXIST) return -1;
        *p = '/';
    }
    return mkdir(tmp, 0755) == 0 || errno == EEXIST ? 0 : -1;
}

static int write_bytes(const char *path, const void *data, size_t len) {
    FILE *f = fopen(path, "wb");
    if (!f) return -1;
    int ok = fwrite(data, 1, len, f) == len && fflush(f) == 0;
    if (fclose(f) != 0) ok = 0;
    return ok ? 0 : -1;
}

static int read_bytes(const char *path, unsigned char *out, size_t len,
                      size_t offset) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    int ok = fseek(f, (long)offset, SEEK_SET) == 0 &&
             fread(out, 1, len, f) == len;
    fclose(f);
    return ok ? 0 : -1;
}

static int exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static void rm_tree(const char *path) {
    struct stat st;
    if (lstat(path, &st) != 0) return;
    if (!S_ISDIR(st.st_mode)) {
        (void)unlink(path);
        return;
    }
    DIR *d = opendir(path);
    if (d) {
        struct dirent *de;
        while ((de = readdir(d)) != NULL) {
            if (strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0)
                continue;
            char child[2048];
            int n = snprintf(child, sizeof(child), "%s/%s", path, de->d_name);
            if (n >= 0 && (size_t)n < sizeof(child)) rm_tree(child);
        }
        closedir(d);
    }
    (void)rmdir(path);
}

static void wr16(unsigned char *buf, size_t at, uint16_t v) {
    buf[at] = (unsigned char)(v & 0xffu);
    buf[at + 1] = (unsigned char)((v >> 8) & 0xffu);
}

static void wr32(unsigned char *buf, size_t at, uint32_t v) {
    buf[at] = (unsigned char)(v & 0xffu);
    buf[at + 1] = (unsigned char)((v >> 8) & 0xffu);
    buf[at + 2] = (unsigned char)((v >> 16) & 0xffu);
    buf[at + 3] = (unsigned char)((v >> 24) & 0xffu);
}

static void wr64(unsigned char *buf, size_t at, uint64_t v) {
    wr32(buf, at, (uint32_t)(v & 0xffffffffu));
    wr32(buf, at + 4, (uint32_t)(v >> 32));
}

static void make_patchable_elf(unsigned char *elf, size_t len,
                               uint32_t sdk) {
    memset(elf, 0, len);
    memcpy(elf, "\x7f" "ELF", 4);
    elf[4] = 2;
    elf[5] = 1;
    wr64(elf, 0x20, 0x40);
    wr16(elf, 0x36, 0x38);
    wr16(elf, 0x38, 1);
    wr32(elf, 0x40, PT_SCE_PROCPARAM);
    wr64(elf, 0x48, 0x200);
    wr64(elf, 0x60, 0x40);
    wr32(elf, 0x200 + SCE_PARAM_MAGIC_OFFSET, SCE_PROCESS_PARAM_MAGIC);
    wr32(elf, 0x200 + SCE_PARAM_PS5_SDK_OFFSET, sdk);
}

static uint32_t read_u32_at(const char *path, size_t offset) {
    unsigned char b[4] = {0};
    CHECK(read_bytes(path, b, sizeof(b), offset) == 0);
    return (uint32_t)b[0] | ((uint32_t)b[1] << 8) |
           ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
}

int main(void) {
    char root_template[] = "/tmp/ps5upload-sdk-changer-XXXXXX";
    char *root = mkdtemp(root_template);
    CHECK(root != NULL);
    if (!root) return 1;

    snprintf(g_test_app_base, sizeof(g_test_app_base), "%s/user-app", root);
    snprintf(g_test_appmeta_base, sizeof(g_test_appmeta_base), "%s/appmeta", root);

    const char *title_id = "CUSA12345";
    const char *pkg_title_id = "CUSA54321";
    char game[2048], game_sce[2048], app_dir[2048], meta_sce[2048];
    char pkg_app[2048], pkg_meta_sce[2048];
    snprintf(game, sizeof(game), "%s/game", root);
    snprintf(game_sce, sizeof(game_sce), "%s/sce_sys", game);
    snprintf(app_dir, sizeof(app_dir), "%s/%s", g_test_app_base, title_id);
    snprintf(meta_sce, sizeof(meta_sce), "%s/%s/sce_sys",
             g_test_appmeta_base, title_id);
    snprintf(pkg_app, sizeof(pkg_app), "%s/%s", g_test_app_base, pkg_title_id);
    snprintf(pkg_meta_sce, sizeof(pkg_meta_sce), "%s/%s/sce_sys",
             g_test_appmeta_base, pkg_title_id);
    CHECK(mkdir_tree(game_sce) == 0);
    CHECK(mkdir_tree(app_dir) == 0);
    CHECK(mkdir_tree(meta_sce) == 0);
    CHECK(mkdir_tree(pkg_app) == 0);
    CHECK(mkdir_tree(pkg_meta_sce) == 0);

    char tracker[2048], source_param[2048], meta_param[2048];
    char pkg_meta_param[2048], elf_path[2048], signed_path[2048], asset_path[2048];
    snprintf(tracker, sizeof(tracker), "%s/mount.lnk", app_dir);
    snprintf(source_param, sizeof(source_param), "%s/param.json", game_sce);
    snprintf(meta_param, sizeof(meta_param), "%s/param.json", meta_sce);
    snprintf(pkg_meta_param, sizeof(pkg_meta_param), "%s/param.json", pkg_meta_sce);
    snprintf(elf_path, sizeof(elf_path), "%s/eboot.elf", game);
    snprintf(signed_path, sizeof(signed_path), "%s/signed.self", game);
    snprintf(asset_path, sizeof(asset_path), "%s/asset.bin", game);

    const char *param =
        "{\"titleId\":\"CUSA12345\",\"titleName\":\"Fixture\","
        "\"sdkVersion\":\"0x1000000000000000\","
        "\"requiredSystemSoftwareVersion\":\"0x1000000000000000\"}";
    const char *pkg_param =
        "{\"titleId\":\"CUSA54321\",\"titleName\":\"Package\","
        "\"sdkVersion\":\"0x1000000000000000\"}";
    CHECK(write_bytes(tracker, game, strlen(game)) == 0);
    CHECK(write_bytes(source_param, param, strlen(param)) == 0);
    CHECK(write_bytes(meta_param, param, strlen(param)) == 0);
    CHECK(write_bytes(pkg_meta_param, pkg_param, strlen(pkg_param)) == 0);

    unsigned char elf[0x240];
    make_patchable_elf(elf, sizeof(elf), 0x10000000u);
    CHECK(write_bytes(elf_path, elf, sizeof(elf)) == 0);
    unsigned char signed_self[64] = {0};
    wr32(signed_self, 0, 0xEEF51454u);
    CHECK(write_bytes(signed_path, signed_self, sizeof(signed_self)) == 0);
    unsigned char asset[128] = {0};
    memcpy(asset, "ordinary game data", 18);
    CHECK(write_bytes(asset_path, asset, sizeof(asset)) == 0);

    /* A backport's replacement system libraries. These must be left ALONE:
     * they carry the SDK version of the firmware they were taken from, and
     * every working backported title on the test console keeps them at
     * 0x09040001 while its own eboot is patched down to 0x04000031.
     * Rewriting them corrupts a working backport. */
    /* A libc.prx carrying the symbol BestPig's patch swaps. It must be
     * rewritten by the same action that downgrades the SDK — a title can
     * need both, and asking the user to run two tools is how the step gets
     * missed. */
    char mod_dir[2048], libc_path[2048];
    snprintf(mod_dir, sizeof(mod_dir), "%s/sce_module", game);
    CHECK(mkdir_tree(mod_dir) == 0);
    snprintf(libc_path, sizeof(libc_path), "%s/libc.prx", mod_dir);
    unsigned char libc[256];
    memset(libc, 0x33, sizeof(libc));
    memcpy(libc + 64, "4h6F1LLbTiw#A#B", 15);
    CHECK(write_bytes(libc_path, libc, sizeof(libc)) == 0);

    char fakelib_dir[2048], fakelib_path[2048];
    snprintf(fakelib_dir, sizeof(fakelib_dir), "%s/fakelib", game);
    CHECK(mkdir_tree(fakelib_dir) == 0);
    snprintf(fakelib_path, sizeof(fakelib_path), "%s/libSceAgc.sprx", fakelib_dir);
    unsigned char fakelib[0x240];
    make_patchable_elf(fakelib, sizeof(fakelib), 0x09040001u);
    CHECK(write_bytes(fakelib_path, fakelib, sizeof(fakelib)) == 0);

    /* `.bak` files this tool did not make. Restore used to rename EVERY `.bak`
     * in the tree over its "original" — the AMPR index tool keeps the game's
     * own index as ampr_emu.index.bak, so Restore silently undid that too —
     * and a `.bak` of another size beside an executable made the patch fail
     * ("could not back up"). None of them may be touched, and none may get in
     * the way. */
    char idx_path[2048], idx_bak[2048], foreign_elf_bak[2048], note_bak[2048];
    snprintf(idx_path, sizeof(idx_path), "%s/ampr_emu.index", game);
    snprintf(idx_bak, sizeof(idx_bak), "%s/ampr_emu.index.bak", game);
    snprintf(foreign_elf_bak, sizeof(foreign_elf_bak), "%s.bak", elf_path);
    snprintf(note_bak, sizeof(note_bak), "%s/sce_sys/param.json.old.bak", game);
    CHECK(write_bytes(idx_path, "new index", 9) == 0);
    CHECK(write_bytes(idx_bak, "the game's own index", 20) == 0);
    CHECK(write_bytes(foreign_elf_bak, "someone's copy", 14) == 0);
    CHECK(write_bytes(note_bak, "{}", 2) == 0);

    char scan[8192];
    size_t scan_len = 0;
    CHECK(sdk_changer_scan(scan, sizeof(scan), &scan_len) == 0);
    CHECK(scan_len > 0);
    char *folder_obj = strstr(scan, "\"title_id\":\"CUSA12345\"");
    char *pkg_obj = strstr(scan, "\"title_id\":\"CUSA54321\"");
    CHECK(folder_obj && strstr(folder_obj, "\"patchable\":true"));
    CHECK(pkg_obj && strstr(pkg_obj, "\"patchable\":false"));

    char detail[512] = {0};
    CHECK(sdk_changer_patch(pkg_title_id, "0x04000031", 1, detail,
                            sizeof(detail)) != 0);
    char pkg_bak[2048];
    snprintf(pkg_bak, sizeof(pkg_bak), "%s" BACKUP_SUFFIX, pkg_meta_param);
    CHECK(!exists(pkg_bak));

    memset(detail, 0, sizeof(detail));
    CHECK(sdk_changer_patch(title_id, "0x04000031", 1, detail,
                            sizeof(detail)) == 0);
    CHECK(strstr(detail, "ELF sites: 1") != NULL);
    CHECK(strstr(detail, "libc sites: 1") != NULL);
    /* And with the flag OFF the libc is left completely alone. Applying it
     * to a title that does not need it crashed the game after five modules
     * where it otherwise loaded seventy, so this must never be implicit. */
    {
        char detail_off[512] = {0};
        char libc2[2048], mod2[2048], game2[2048], sce2[2048], app2[2048];
        snprintf(game2, sizeof(game2), "%s/game2", root);
        snprintf(sce2, sizeof(sce2), "%s/sce_sys", game2);
        snprintf(mod2, sizeof(mod2), "%s/sce_module", game2);
        snprintf(app2, sizeof(app2), "%s/%s", g_test_app_base, "CUSA22222");
        CHECK(mkdir_tree(sce2) == 0);
        CHECK(mkdir_tree(mod2) == 0);
        CHECK(mkdir_tree(app2) == 0);
        char trk2[2048], par2[2048];
        snprintf(trk2, sizeof(trk2), "%s/mount.lnk", app2);
        snprintf(par2, sizeof(par2), "%s/param.json", sce2);
        CHECK(write_bytes(trk2, game2, strlen(game2)) == 0);
        const char *p2 = "{\"titleId\":\"CUSA22222\",\"sdkVersion\":\"0x1000000000000000\"}";
        CHECK(write_bytes(par2, p2, strlen(p2)) == 0);
        snprintf(libc2, sizeof(libc2), "%s/libc.prx", mod2);
        unsigned char l2[256];
        memset(l2, 0x33, sizeof(l2));
        memcpy(l2 + 64, "4h6F1LLbTiw#A#B", 15);
        CHECK(write_bytes(libc2, l2, sizeof(l2)) == 0);

        CHECK(sdk_changer_patch("CUSA22222", "0x04000031", 0, detail_off,
                                sizeof(detail_off)) == 0);
        CHECK(strstr(detail_off, "libc sites: 0") != NULL);
        unsigned char back2[256];
        CHECK(read_bytes(libc2, back2, sizeof(back2), 0) == 0);
        CHECK(memcmp(back2 + 64, "4h6F1LLbTiw#A#B", 15) == 0);
        char libc2_bak[2048];
        snprintf(libc2_bak, sizeof(libc2_bak), "%s" BACKUP_SUFFIX, libc2);
        CHECK(!exists(libc2_bak));
    }
    {
        unsigned char got[256];
        CHECK(read_bytes(libc_path, got, sizeof(got), 0) == 0);
        CHECK(memcmp(got + 64, "IWIBBdTHit4#A#B", 15) == 0);
        /* Same length, so nothing around it moved. */
        for (size_t i = 0; i < 64; i++) CHECK(got[i] == 0x33);
        for (size_t i = 64 + 15; i < sizeof(got); i++) CHECK(got[i] == 0x33);
    }
    CHECK(read_u32_at(elf_path, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) ==
          0x04000031u);
    /* Untouched, and no backup taken for it either. */
    CHECK(read_u32_at(fakelib_path, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) ==
          0x09040001u);
    /* Both halves of the pair are written, not just the one this segment
     * type prefers — a half-patched executable launches and then dies. */
    CHECK(read_u32_at(elf_path, 0x200 + SCE_PARAM_PS4_SDK_OFFSET) ==
          0x09040001u);
    char fakelib_bak[2048];
    snprintf(fakelib_bak, sizeof(fakelib_bak), "%s" BACKUP_SUFFIX, fakelib_path);
    CHECK(!exists(fakelib_bak));

    char source_bak[2048], meta_bak[2048], elf_bak[2048];
    char signed_bak[2048], asset_bak[2048];
    snprintf(source_bak, sizeof(source_bak), "%s" BACKUP_SUFFIX, source_param);
    snprintf(meta_bak, sizeof(meta_bak), "%s" BACKUP_SUFFIX, meta_param);
    snprintf(elf_bak, sizeof(elf_bak), "%s" BACKUP_SUFFIX, elf_path);
    snprintf(signed_bak, sizeof(signed_bak), "%s" BACKUP_SUFFIX, signed_path);
    snprintf(asset_bak, sizeof(asset_bak), "%s" BACKUP_SUFFIX, asset_path);
    CHECK(exists(source_bak));
    CHECK(exists(meta_bak));
    CHECK(exists(elf_bak));
    CHECK(!exists(signed_bak));
    CHECK(!exists(asset_bak));

    /* libc.prx is backed up too, so restore must put it back with the rest —
     * a backport that leaves the symbol swapped is not "restored". */
    char libc_bak[2048];
    snprintf(libc_bak, sizeof(libc_bak), "%s" BACKUP_SUFFIX, libc_path);
    CHECK(exists(libc_bak));

    int restored = 0;
    memset(detail, 0, sizeof(detail));
    CHECK(sdk_changer_restore(title_id, &restored, detail, sizeof(detail)) == 0);
    CHECK(restored == 4);
    CHECK(read_u32_at(elf_path, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) ==
          0x10000000u);
    {
        unsigned char back[256];
        CHECK(read_bytes(libc_path, back, sizeof(back), 0) == 0);
        CHECK(memcmp(back + 64, "4h6F1LLbTiw#A#B", 15) == 0);
    }
    CHECK(!exists(source_bak));
    CHECK(!exists(meta_bak));
    CHECK(!exists(elf_bak));
    CHECK(!exists(libc_bak));

    /* The foreign `.bak` files are exactly as they were, and so is what they
     * sit beside. */
    {
        unsigned char got[32] = {0};
        CHECK(read_bytes(idx_path, got, 9, 0) == 0);
        CHECK(memcmp(got, "new index", 9) == 0);
        CHECK(read_bytes(idx_bak, got, 20, 0) == 0);
        CHECK(memcmp(got, "the game's own index", 20) == 0);
        CHECK(read_bytes(foreign_elf_bak, got, 14, 0) == 0);
        CHECK(memcmp(got, "someone's copy", 14) == 0);
        CHECK(exists(note_bak));
    }

    /* A title patched by a helper from before the suffix changed: its backups
     * are plain `.bak`. Restore must still put those back — and only those. */
    {
        const char *old_id = "CUSA33333";
        char game3[2048], sce3[2048], app3[2048], trk3[2048];
        snprintf(game3, sizeof(game3), "%s/game3", root);
        snprintf(sce3, sizeof(sce3), "%s/sce_sys", game3);
        snprintf(app3, sizeof(app3), "%s/%s", g_test_app_base, old_id);
        CHECK(mkdir_tree(sce3) == 0);
        CHECK(mkdir_tree(app3) == 0);
        snprintf(trk3, sizeof(trk3), "%s/mount.lnk", app3);
        CHECK(write_bytes(trk3, game3, strlen(game3)) == 0);

        char elf3[2048], elf3_bak[2048], par3[2048], par3_bak[2048];
        char pak3[2048], pak3_bak[2048], prx3[2048], prx3_bak[2048];
        char lone_bak[2048], lone[2048];
        snprintf(elf3, sizeof(elf3), "%s/eboot.bin", game3);
        snprintf(elf3_bak, sizeof(elf3_bak), "%s.bak", elf3);
        snprintf(par3, sizeof(par3), "%s/param.json", sce3);
        snprintf(par3_bak, sizeof(par3_bak), "%s.bak", par3);
        snprintf(pak3, sizeof(pak3), "%s/data.pak", game3);
        snprintf(pak3_bak, sizeof(pak3_bak), "%s.bak", pak3);
        snprintf(prx3, sizeof(prx3), "%s/other.prx", game3);
        snprintf(prx3_bak, sizeof(prx3_bak), "%s.bak", prx3);
        snprintf(lone, sizeof(lone), "%s/gone.sprx", game3);
        snprintf(lone_bak, sizeof(lone_bak), "%s.bak", lone);

        unsigned char now_elf[0x240], orig_elf[0x240];
        make_patchable_elf(now_elf, sizeof(now_elf), 0x04000031u);
        make_patchable_elf(orig_elf, sizeof(orig_elf), 0x10000000u);
        CHECK(write_bytes(elf3, now_elf, sizeof(now_elf)) == 0);
        CHECK(write_bytes(elf3_bak, orig_elf, sizeof(orig_elf)) == 0);
        const char *now_par =
            "{\"titleId\":\"CUSA33333\",\"sdkVersion\":\"0x0400003100000000\"}";
        const char *orig_par =
            "{\"titleId\":\"CUSA33333\",\"sdkVersion\":\"0x1000000000000000\"}";
        CHECK(write_bytes(par3, now_par, strlen(now_par)) == 0);
        CHECK(write_bytes(par3_bak, orig_par, strlen(orig_par)) == 0);
        /* Same size as its neighbour, but not a type this tool ever backed up. */
        CHECK(write_bytes(pak3, "AAAA", 4) == 0);
        CHECK(write_bytes(pak3_bak, "BBBB", 4) == 0);
        /* A patchable type, but not the size of the file beside it: an
         * in-place patch cannot have produced that pair. */
        CHECK(write_bytes(prx3, "CCCCCCCC", 8) == 0);
        CHECK(write_bytes(prx3_bak, "DDDD", 4) == 0);
        /* And one with no original beside it at all. */
        CHECK(write_bytes(lone_bak, "EEEE", 4) == 0);

        /* Patching again keeps the old backup as THE backup: a second one,
         * taken now, would hold the already-patched file. */
        char d3[512] = {0};
        CHECK(sdk_changer_patch(old_id, "0x05000000", 0, d3, sizeof(d3)) == 0);
        CHECK(read_u32_at(elf3, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) != 0x04000031u);
        char elf3_own[2048], par3_own[2048];
        snprintf(elf3_own, sizeof(elf3_own), "%s" BACKUP_SUFFIX, elf3);
        snprintf(par3_own, sizeof(par3_own), "%s" BACKUP_SUFFIX, par3);
        CHECK(!exists(elf3_own));
        CHECK(!exists(par3_own));
        CHECK(exists(elf3_bak));
        CHECK(exists(par3_bak));

        int restored3 = 0;
        char r3[256] = {0};
        CHECK(sdk_changer_restore(old_id, &restored3, r3, sizeof(r3)) == 0);
        CHECK(restored3 == 2);
        CHECK(read_u32_at(elf3, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) == 0x10000000u);
        CHECK(!exists(elf3_bak));
        CHECK(!exists(par3_bak));
        {
            char got[128] = {0};
            CHECK(read_bytes(par3, (unsigned char *)got, strlen(orig_par), 0) == 0);
            CHECK(strcmp(got, orig_par) == 0);
            unsigned char four[4];
            CHECK(read_bytes(pak3, four, 4, 0) == 0 && memcmp(four, "AAAA", 4) == 0);
            CHECK(read_bytes(pak3_bak, four, 4, 0) == 0 && memcmp(four, "BBBB", 4) == 0);
            CHECK(read_bytes(prx3_bak, four, 4, 0) == 0 && memcmp(four, "DDDD", 4) == 0);
            unsigned char eight[8];
            CHECK(read_bytes(prx3, eight, 8, 0) == 0 && memcmp(eight, "CCCCCCCC", 8) == 0);
            CHECK(exists(lone_bak));
            CHECK(!exists(lone));
        }

        /* A `.bak` that only LOOKS like ours next to one of our own backups:
         * ours is the original, whichever the folder lists first, and the
         * other stays. Same size on purpose — after the restore it would pass
         * for an old backup if it were judged then. */
        unsigned char third[0x240];
        make_patchable_elf(third, sizeof(third), 0x10000000u);
        CHECK(write_bytes(elf3, third, sizeof(third)) == 0);
        memset(d3, 0, sizeof(d3));
        /* eboot.bin has no `.bak` now, so this patch gives it one of ours. */
        CHECK(sdk_changer_patch(old_id, "0x04000031", 0, d3, sizeof(d3)) == 0);
        CHECK(exists(elf3_own));
        unsigned char decoy[0x240];
        make_patchable_elf(decoy, sizeof(decoy), 0x77000000u);
        CHECK(write_bytes(elf3_bak, decoy, sizeof(decoy)) == 0);
        restored3 = 0;
        CHECK(sdk_changer_restore(old_id, &restored3, r3, sizeof(r3)) == 0);
        CHECK(restored3 == 2); /* eboot.bin and param.json, from our backups */
        CHECK(read_u32_at(elf3, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) == 0x10000000u);
        CHECK(exists(elf3_bak));
        CHECK(read_u32_at(elf3_bak, 0x200 + SCE_PARAM_PS5_SDK_OFFSET) == 0x77000000u);
        CHECK(!exists(elf3_own));
    }

    /* A list that does not fit its buffer is cut at a whole entry and SAYS so.
     * It used to look complete, or (when the tail did not fit either) come
     * back as an error the handler ignored, i.e. an empty reply. Every buffer
     * size either yields a whole document or a plain failure. */
    {
        char full_scan[8192];
        size_t full_len = 0;
        CHECK(sdk_changer_scan(full_scan, sizeof(full_scan), &full_len) == 0);
        CHECK(strstr(full_scan, "\"truncated\":false") != NULL);
        CHECK(strstr(full_scan, "\"title_id\":\"CUSA54321\"") != NULL);
        int saw_cut = 0, saw_cut_with_titles = 0;
        for (size_t cap = 1; cap <= full_len + 2; cap++) {
            char *small = malloc(cap);
            CHECK(small != NULL);
            if (!small) break;
            size_t got = 0;
            int rc = sdk_changer_scan(small, cap, &got);
            if (rc == 0) {
                CHECK(got < cap && got == strlen(small));
                CHECK(strncmp(small, "{\"titles\":[", 11) == 0);
                CHECK(got > 0 && small[got - 1] == '}');
                CHECK(strstr(small, "],\"truncated\":") != NULL);
                CHECK(strstr(small, "\"overlay\":{\"state\":\"watching\"") != NULL);
                int depth = 0, bad = 0;
                for (size_t i = 0; i < got; i++) {
                    if (small[i] == '{') depth++;
                    if (small[i] == '}' && --depth < 0) bad = 1;
                }
                CHECK(depth == 0 && !bad);
                if (strstr(small, "\"truncated\":true")) {
                    saw_cut = 1;
                    if (strstr(small, "\"title_id\"")) saw_cut_with_titles = 1;
                    CHECK(got < full_len);
                } else {
                    CHECK(got == full_len);
                }
            }
            free(small);
        }
        CHECK(saw_cut && saw_cut_with_titles);
    }

    rm_tree(root);
    printf("sdk_changer_file_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
