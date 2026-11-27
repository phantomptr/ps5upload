/* Host self-test for install_guard.h (#884): a title the console's own installer put on M.2,
 * internal or extended storage is seen, and register.c checks it before anything is written.
 *
 * The reported case: Among Us (PPSA03595) installed on the M.2 SSD, at
 * /mnt/ext1/user/app/PPSA03595/app.pkg. Play on a dump of it staged /user/app/PPSA03595 on the
 * internal drive and asked Sony's installer to install that, which moved the console's record of
 * the game to Console Storage, where it has no package. */
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "install_guard.h"

#ifndef REGISTER_C_PATH
#error "REGISTER_C_PATH must name payload/src/register.c"
#endif

static char g_root[512];

static void mkdirs(const char *rel) {
    char p[1024];
    snprintf(p, sizeof p, "%s%s", g_root, rel);
    for (char *s = p + strlen(g_root) + 1; *s; s++) {
        if (*s != '/') continue;
        *s = '\0';
        (void)mkdir(p, 0755);
        *s = '/';
    }
    (void)mkdir(p, 0755);
}

static void touch(const char *rel) {
    char p[1024];
    snprintf(p, sizeof p, "%s%s", g_root, rel);
    char dir[1024];
    snprintf(dir, sizeof dir, "%s", rel);
    char *slash = strrchr(dir, '/');
    if (slash) {
        *slash = '\0';
        mkdirs(dir);
    }
    int fd = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    assert(fd >= 0);
    close(fd);
}

static void rm_rf(const char *abs) {
    char cmd[1200];
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", abs);
    assert(system(cmd) == 0);
}

static void fresh_root(void) {
    rm_rf(g_root);
    assert(mkdir(g_root, 0755) == 0);
}

/* The text of register.c, and the position of `needle` inside the body of `fn` (first match
 * after the function's definition line). -1 when absent. */
static char *g_src;

static void load_register_c(void) {
    FILE *f = fopen(REGISTER_C_PATH, "rb");
    assert(f);
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    g_src = malloc((size_t)n + 1);
    assert(g_src && fread(g_src, 1, (size_t)n, f) == (size_t)n);
    g_src[n] = '\0';
    fclose(f);
}

static const char *fn_body(const char *def) {
    const char *p = strstr(g_src, def);
    assert(p && "function definition not found in register.c");
    p = strchr(p, '{');
    assert(p);
    return p;
}

/* Offset of `needle` in the function starting at `body`, before the next top-level "\n}\n". */
static long pos_in(const char *body, const char *needle) {
    const char *end = strstr(body, "\n}\n");
    assert(end);
    const char *hit = strstr(body, needle);
    if (!hit || hit > end) return -1;
    return (long)(hit - body);
}

int main(void) {
    snprintf(g_root, sizeof g_root, "%s/ps5upload-install-guard-%d",
             getenv("TMPDIR") ? getenv("TMPDIR") : "/tmp", (int)getpid());
    install_guard_hit_t hit;

    /* ── the id a folder gives itself ─────────────────────────────────────── */
    assert(install_guard_register_id_refusal("PPSA03595") == NULL);
    assert(install_guard_register_id_refusal("CUSA00900") == NULL);
    assert(install_guard_register_id_refusal("PSUP00001") == NULL); /* the Minion tile */
    assert(install_guard_register_id_refusal("LAPY20001") == NULL); /* homebrew */
    assert(strcmp(install_guard_register_id_refusal("NPXS20001"), "register_system_title") == 0);
    assert(strcmp(install_guard_register_id_refusal("npxs40087"), "register_system_title") == 0);
    {
        static const char *const bad[] = { NULL, "", "PPSA0359", "PPSA035955", "PPSA0359A", "PP5A03595",
                                           "PPSA 3595", "' OR 1=1", "PPSA03595_00", "..", "a/b" };
        for (size_t i = 0; i < sizeof bad / sizeof bad[0]; i++)
            assert(strcmp(install_guard_register_id_refusal(bad[i]), "register_title_id_unsafe") == 0);
    }
    printf("✓ a folder that names itself a system app, or gives no title id, is not registered\n");

    /* ── the reported layout: the game on M.2 ─────────────────────────────── */
    fresh_root();
    touch("/mnt/ext1/user/app/PPSA03595/app.pkg");
    touch("/mnt/ext1/user/app/PPSA03595/app.json");
    assert(install_guard_check(g_root, "PPSA03595", NULL, 0, NULL, &hit) == 1);
    assert(hit.where == INSTALL_GUARD_M2);
    assert(strcmp(hit.path, "/mnt/ext1/user/app/PPSA03595/app.pkg") == 0);
    assert(strcmp(install_guard_register_reason(hit.where), "register_title_installed:m2") == 0);
    assert(strcmp(install_guard_unregister_reason(hit.where), "unregister_title_installed:m2") == 0);
    printf("✓ a game installed on M.2 is seen before a folder of the same title is registered\n");

    /* After the damage: /user/app/<id> holds our sce_sys + icon0.png (and maybe mount.lnk), the
     * package is still on M.2: still refused, whatever the tracker says. */
    touch("/user/app/PPSA03595/sce_sys/param.json");
    touch("/user/app/PPSA03595/icon0.png");
    touch("/user/app/PPSA03595/mount.lnk");
    assert(install_guard_check(g_root, "PPSA03595", NULL, 0, NULL, &hit) == 1);
    assert(hit.where == INSTALL_GUARD_M2);
    printf("✓ still refused once PS5Upload's own files are next to it\n");

    /* ── internal, USB extended storage, another extended volume ───────── */
    fresh_root();
    touch("/user/app/CUSA00900/app.pkg");
    assert(install_guard_check(g_root, "CUSA00900", NULL, 0, NULL, &hit) == 1);
    assert(hit.where == INSTALL_GUARD_INTERNAL);
    assert(strcmp(hit.path, "/user/app/CUSA00900/app.pkg") == 0);
    fresh_root();
    touch("/mnt/ext0/user/app/CUSA07842/app.pkg");
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, NULL, &hit) == 1);
    assert(hit.where == INSTALL_GUARD_USB);
    fresh_root();
    touch("/mnt/ext2/user/app/PPSA01234/app.pkg");
    assert(install_guard_check(g_root, "PPSA01234", NULL, 0, NULL, &hit) == 0);
    {
        const char *mounts[] = {"/mnt/usb0", "/mnt/ext1", "/mnt/ext2", "/mnt/extX", "/data"};
        assert(install_guard_check(g_root, "PPSA01234", mounts, 5, NULL, &hit) == 1);
        assert(hit.where == INSTALL_GUARD_EXTENDED);
        assert(strcmp(hit.path, "/mnt/ext2/user/app/PPSA01234/app.pkg") == 0);
    }
    printf("✓ internal, USB extended storage and any mounted /mnt/ext<N> are all looked at\n");

    /* ── what is NOT the console's install ───────────────────────────── */
    fresh_root();
    /* Our own earlier registration of a folder: sce_sys + mount.lnk, no package. */
    touch("/user/app/PPSA17221/sce_sys/param.json");
    touch("/user/app/PPSA17221/mount.lnk");
    assert(install_guard_check(g_root, "PPSA17221", NULL, 0, NULL, &hit) == 0);
    assert(hit.where == INSTALL_GUARD_NONE);
    /* A dump folder elsewhere on M.2 is a source, not an install. */
    touch("/mnt/ext1/homebrew/PPSA17221/eboot.bin");
    assert(install_guard_check(g_root, "PPSA17221", NULL, 0, NULL, &hit) == 0);
    /* An app.pkg of ANOTHER title does not count. */
    touch("/mnt/ext1/user/app/PPSA99999/app.pkg");
    assert(install_guard_check(g_root, "PPSA17221", NULL, 0, NULL, &hit) == 0);
    /* Nothing at all. */
    assert(install_guard_check(g_root, "PPSA55555", NULL, 0, NULL, &hit) == 0);
    /* Not a title id: never a path. */
    assert(install_guard_check(g_root, "../../etc", NULL, 0, NULL, &hit) == 0);
    assert(install_guard_check(g_root, "PPSA0359", NULL, 0, NULL, &hit) == 0);
    printf("✓ our own folder registrations, dump folders and other titles are not refused\n");

    /* ── the console's record names a package on storage that is not plugged in ── */
    fresh_root();
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, "/mnt/ext0/user/app/CUSA07842", &hit) == 1);
    assert(hit.where == INSTALL_GUARD_RECORD);
    assert(strcmp(install_guard_register_reason(hit.where), "register_title_installed:record") == 0);
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, "/user/app/CUSA07842/app.pkg", &hit) == 1);
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, "/user/app/CUSA07842", &hit) == 0);
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, "", &hit) == 0);
    /* A title with a tracker of ours is ours, whatever the record says. */
    touch("/user/app/CUSA07842/mount.lnk");
    assert(install_guard_check(g_root, "CUSA07842", NULL, 0, "/mnt/ext0/user/app/CUSA07842", &hit) == 0);
    fresh_root();
    touch("/user/app/PSUP00001/ps5upload.lnk");
    assert(install_guard_has_tracker(g_root, "PSUP00001"));
    printf("✓ a record of a package on unplugged extended storage is refused too\n");

    /* ── register.c checks before any write ──────────────────────────── */
    load_register_c();
    {
        const char *body = fn_body("int register_title_from_path(const char *src_path,");
        long guard = pos_in(body, "register_title_installed_by_console(");
        assert(guard >= 0 && "register_title_from_path does not check for the console's install");
        /* and the id itself is judged first: the install check answers "no" for one it cannot read */
        long id_rule = pos_in(body, "install_guard_register_id_refusal(");
        assert(id_rule >= 0 && id_rule < guard && "register_title_from_path does not judge the title id first");
        const char *writes[] = {"patch_drm_type_to_standard(", "mkdir_p(user_app)",
                                "copy_dir_recursive(", "copy_file_bytes(", "mkdir_p(user_appmeta)",
                                "copy_sce_sys_to_appmeta(", "mount_title_nullfs(", "write_link_file(",
                                "app_install_title_dir(", "app_install_all("};
        for (size_t i = 0; i < sizeof writes / sizeof writes[0]; i++) {
            long w = pos_in(body, writes[i]);
            assert(w > 0 && "a write step was not found: update this test");
            if (w < guard) {
                fprintf(stderr, "register_title_from_path: %s runs before the install check\n", writes[i]);
                return 1;
            }
        }
    }
    {
        const char *body = fn_body("int register_remount_nullfs(const char *title_id, const char *src_path,");
        long guard = pos_in(body, "register_title_installed_by_console(");
        long bind = pos_in(body, "mount_title_nullfs(");
        long id_rule = pos_in(body, "install_guard_register_id_refusal(");
        assert(guard >= 0 && bind > guard);
        assert(id_rule >= 0 && id_rule < bind);
    }
    {
        const char *body = fn_body("int unregister_title_ex(const char *title_id, int registration_only,");
        long guard = pos_in(body, "register_title_installed_by_console(");
        assert(guard >= 0);
        assert(pos_in(body, "unmount_title_nullfs(") > guard);
        assert(pos_in(body, "g_reg.app_uninstall(") > guard);
        assert(pos_in(body, "(void)unlink(lnk)") > guard);
    }
    printf("✓ register, re-bind and a registration-only unregister check before touching anything\n");

    /* #961: a launch of a title app.db does not know ends before the ShellUI ptrace route and the
     * in-process retry ladder (the console's system UI restarted when it ran for such a game). */
    {
        const char *body = fn_body("int launch_title(const char *title_id, const char **err_reason_out,");
        long primary = pos_in(body, "int rc_ss = launch_via_system_service(");
        long db = pos_in(body, "register_appdb_has_title(title_id) == 0");
        long refuse = pos_in(body, "\"launch_not_in_database: rc=");
        long ptrace = pos_in(body, "shellui_rpc_launch_app(");
        long ladder = pos_in(body, "rc = g_reg.launch_app(title_id, NULL, (void *)&param);");
        assert(primary >= 0 && db > primary && refuse > db);
        assert(ptrace > refuse && ladder > refuse);
    }
    printf("✓ a launch of a title not in app.db stops before the ShellUI and fallback routes\n");

    free(g_src);
    rm_rf(g_root);
    return 0;
}
