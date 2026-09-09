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
    snprintf(pkg_bak, sizeof(pkg_bak), "%s.bak", pkg_meta_param);
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
        snprintf(libc2_bak, sizeof(libc2_bak), "%s.bak", libc2);
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
    snprintf(fakelib_bak, sizeof(fakelib_bak), "%s.bak", fakelib_path);
    CHECK(!exists(fakelib_bak));

    char source_bak[2048], meta_bak[2048], elf_bak[2048];
    char signed_bak[2048], asset_bak[2048];
    snprintf(source_bak, sizeof(source_bak), "%s.bak", source_param);
    snprintf(meta_bak, sizeof(meta_bak), "%s.bak", meta_param);
    snprintf(elf_bak, sizeof(elf_bak), "%s.bak", elf_path);
    snprintf(signed_bak, sizeof(signed_bak), "%s.bak", signed_path);
    snprintf(asset_bak, sizeof(asset_bak), "%s.bak", asset_path);
    CHECK(exists(source_bak));
    CHECK(exists(meta_bak));
    CHECK(exists(elf_bak));
    CHECK(!exists(signed_bak));
    CHECK(!exists(asset_bak));

    /* libc.prx is backed up too, so restore must put it back with the rest —
     * a backport that leaves the symbol swapped is not "restored". */
    char libc_bak[2048];
    snprintf(libc_bak, sizeof(libc_bak), "%s.bak", libc_path);
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

    rm_tree(root);
    printf("sdk_changer_file_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
