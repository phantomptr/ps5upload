/* Host-side tests for the console web UI's settings file (webui_settings.c) and the pure parts of
 * the home-screen tile (webui_tile.c built with WEBUI_TILE_HOST_TEST: the Sony register/unregister
 * half is left out): defaults for a missing or garbled file, save/load round trip through a
 * same-directory temp file, the tile's param.json byte for byte, and the tile folder being
 * written only when its bytes differ. File tests run in a fresh mkdtemp directory.
 * Build: see the root Makefile test-payload. */
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "webui_settings.h"
#include "webui_tile.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* The embedded icon is generated into payload/build/ at ELF build time; the test brings its own. */
unsigned char webui_tile_icon0_png[] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 1, 2, 3 };
unsigned int webui_tile_icon0_png_len = sizeof webui_tile_icon0_png;

static char g_dir[256];

static void put_file(const char *path, const char *text) {
    FILE *f = fopen(path, "wb");
    if (!f) {
        perror(path);
        exit(2);
    }
    fwrite(text, 1, strlen(text), f);
    fclose(f);
}

static size_t get_file(const char *path, char *buf, size_t cap) {
    FILE *f = fopen(path, "rb");
    size_t n;
    if (!f) return (size_t)-1;
    n = fread(buf, 1, cap - 1, f);
    fclose(f);
    buf[n] = '\0';
    return n;
}

static int count_entries(const char *dir) {
    DIR *d = opendir(dir);
    struct dirent *e;
    int n = 0;
    if (!d) return -1;
    while ((e = readdir(d)) != NULL)
        if (strcmp(e->d_name, ".") && strcmp(e->d_name, "..")) n++;
    closedir(d);
    return n;
}

static void missing_file_gives_defaults(void) {
    char p[512];
    webui_settings_t s = { 7, 7, 7, 7 };
    snprintf(p, sizeof p, "%s/none.json", g_dir);
    CHECK(webui_settings_load(p, &s) == 1);
    CHECK(s.lan == 0 && s.tile == 1 && s.enabled == 1);
}

static void garbled_file_gives_defaults(void) {
    static const char *bad[] = {
        "",
        "garbage",
        "{\"lan\":true",             /* cut short */
        "{\"lan\":1,\"tile\":true}", /* not a bool */
        "{\"lan\":true,\"tile\":\"no\"}",
        "{\"lan\":true,\"tile\":true,\"enabled\":0}", /* not a bool */
        "[true,false]",
        "\x00\x01\x02",
    };
    char p[512];
    size_t i;
    snprintf(p, sizeof p, "%s/bad.json", g_dir);
    for (i = 0; i < sizeof bad / sizeof bad[0]; i++) {
        webui_settings_t s = { 7, 7, 7, 7 };
        put_file(p, bad[i]);
        CHECK(webui_settings_load(p, &s) == 1);
        CHECK(s.lan == 0 && s.tile == 1 && s.enabled == 1);
    }
    /* A valid object missing a key keeps that key's default. */
    {
        webui_settings_t s = { 7, 7, 7, 7 };
        put_file(p, "{\"lan\":true}");
        CHECK(webui_settings_load(p, &s) == 0);
        CHECK(s.lan == 1 && s.tile == 1);
        put_file(p, " {\"tile\":false}\n");
        CHECK(webui_settings_load(p, &s) == 0);
        CHECK(s.lan == 0 && s.tile == 0);
        /* An older helper's file (no "enabled"): Minion stays on. */
        put_file(p, "{\"lan\":false,\"tile\":true}");
        CHECK(webui_settings_load(p, &s) == 0);
        CHECK(s.lan == 0 && s.tile == 1 && s.enabled == 1);
        put_file(p, "{\"lan\":true,\"tile\":true,\"enabled\":false}");
        CHECK(webui_settings_load(p, &s) == 0);
        CHECK(s.lan == 1 && s.tile == 1 && s.enabled == 0 && s.start_cheats == 0);
        /* No start page saved: Home. A start page that is not a bool garbles the file, like the rest. */
        put_file(p, "{\"lan\":false,\"tile\":true,\"enabled\":true,\"start_cheats\":true}");
        CHECK(webui_settings_load(p, &s) == 0);
        CHECK(s.start_cheats == 1 && s.enabled == 1);
        put_file(p, "{\"start_cheats\":\"cheats\"}");
        CHECK(webui_settings_load(p, &s) == 1);
        CHECK(s.start_cheats == 0 && s.enabled == 1);
    }
    unlink(p);
}

static void save_then_load_round_trips(void) {
    char p[512], text[256], sub[512];
    int lan, tile, enabled;
    snprintf(p, sizeof p, "%s/webui.json", g_dir);
    for (lan = 0; lan < 2; lan++)
        for (tile = 0; tile < 2; tile++)
            for (enabled = 0; enabled < 2; enabled++) {
                webui_settings_t in = { lan, tile, enabled, 0 }, out = { 7, 7, 7, 7 };
                CHECK(webui_settings_save(p, &in) == 0);
                CHECK(webui_settings_load(p, &out) == 0);
                CHECK(out.lan == lan && out.tile == tile && out.enabled == enabled);
            }
    /* The exact file text, and nothing left behind but the file itself. */
    {
        webui_settings_t d = { 0, 1, 1, 0 };
        CHECK(webui_settings_save(p, &d) == 0);
        CHECK(get_file(p, text, sizeof text) != (size_t)-1);
        CHECK(strcmp(text, "{\"lan\":false,\"tile\":true,\"enabled\":true,\"start_cheats\":false}") == 0);
        d.enabled = 0;
        CHECK(webui_settings_save(p, &d) == 0);
        CHECK(get_file(p, text, sizeof text) != (size_t)-1);
        CHECK(strcmp(text, "{\"lan\":false,\"tile\":true,\"enabled\":false,\"start_cheats\":false}") == 0);
        /* The start page (#584) round-trips and keeps the other keys. */
        d.enabled = 1;
        d.start_cheats = 1;
        CHECK(webui_settings_save(p, &d) == 0);
        {
            webui_settings_t back = { 7, 7, 7, 7 };
            CHECK(webui_settings_load(p, &back) == 0);
            CHECK(back.lan == 0 && back.tile == 1 && back.enabled == 1 && back.start_cheats == 1);
        }
        CHECK(count_entries(g_dir) == 1);
    }
    /* A save that cannot happen leaves no file and says so. */
    {
        webui_settings_t d = { 1, 0, 1, 0 };
        snprintf(sub, sizeof sub, "%s/no-such-dir/webui.json", g_dir);
        CHECK(webui_settings_save(sub, &d) == -1);
    }
    unlink(p);
}

/* What the helper does at start, for every combination of the saved settings. */
static void startup_plan_follows_enabled_and_tile(void) {
    static const struct {
        int tile, enabled, serve, add, sweep;
    } cases[] = {
        { 1, 1, 1, 1, 0 }, /* the default: page served, tile registered */
        { 0, 1, 1, 0, 1 }, /* tile off: page served, tile swept away */
        { 1, 0, 0, 0, 1 }, /* Minion off: no page, tile removed whatever its own setting */
        { 0, 0, 0, 0, 1 },
    };
    size_t i;
    int lan;
    for (lan = 0; lan < 2; lan++)
        for (i = 0; i < sizeof cases / sizeof cases[0]; i++) {
            webui_settings_t s = { lan, cases[i].tile, cases[i].enabled, 0 };
            webui_startup_t p = webui_startup_plan(&s);
            CHECK(p.serve == cases[i].serve);
            CHECK(p.tile_add == cases[i].add);
            CHECK(p.tile_sweep == cases[i].sweep);
            CHECK(webui_settings_tile_wanted(&s) == cases[i].add);
        }
    /* A file from an older helper plans exactly like before (enabled by default). */
    {
        char p[512];
        webui_settings_t s;
        webui_startup_t plan;
        snprintf(p, sizeof p, "%s/old.json", g_dir);
        put_file(p, "{\"lan\":false,\"tile\":true}");
        CHECK(webui_settings_load(p, &s) == 0);
        plan = webui_startup_plan(&s);
        CHECK(plan.serve == 1 && plan.tile_add == 1 && plan.tile_sweep == 0);
        /* Turned off, saved, read back at the next start: it stays off and the tile stays gone. */
        s.enabled = 0;
        CHECK(webui_settings_save(p, &s) == 0);
        CHECK(webui_settings_load(p, &s) == 0);
        plan = webui_startup_plan(&s);
        CHECK(plan.serve == 0 && plan.tile_add == 0 && plan.tile_sweep == 1);
        CHECK(s.tile == 1); /* kept for when Minion is turned back on */
        unlink(p);
    }
}

static void param_json_exact(void) {
    static const char want[] =
        "{\"applicationCategoryType\":66048,\"titleId\":\"PSUP00001\","
        "\"deeplinkUri\":\"http://127.0.0.1:9180/\","
        "\"localizedParameters\":{\"defaultLanguage\":\"en-US\",\"en-US\":{\"titleName\":\"PS5Upload Minion\"}}}";
    char buf[512];
    size_t n = webui_tile_param_json(buf, sizeof buf, 9180);
    CHECK(n == sizeof want - 1);
    CHECK(strcmp(buf, want) == 0);
    /* Too small: the full length is still reported, and nothing is overrun. */
    {
        char tiny[16];
        memset(tiny, 'x', sizeof tiny);
        CHECK(webui_tile_param_json(tiny, sizeof tiny, 9180) == sizeof want - 1);
        CHECK(tiny[sizeof tiny - 1] == '\0');
    }
}

static void tile_files_written_only_when_different(void) {
    char dir[512], p[600], buf[1024], want[512];
    struct stat st;
    snprintf(dir, sizeof dir, "%s/tile/PSUP00001", g_dir);
    CHECK(webui_tile_write_files(dir, 9180) == 3); /* param.json, icon0.png, eboot.bin */
    snprintf(p, sizeof p, "%s/sce_sys/param.json", dir);
    webui_tile_param_json(want, sizeof want, 9180);
    CHECK(get_file(p, buf, sizeof buf) == strlen(want));
    CHECK(strcmp(buf, want) == 0);
    snprintf(p, sizeof p, "%s/sce_sys/icon0.png", dir);
    CHECK(stat(p, &st) == 0 && (size_t)st.st_size == sizeof webui_tile_icon0_png);
    snprintf(p, sizeof p, "%s/eboot.bin", dir);
    CHECK(stat(p, &st) == 0 && st.st_size > 0);
    /* Unchanged: nothing is rewritten. */
    CHECK(webui_tile_write_files(dir, 9180) == 0);
    /* One file differs: only that one is rewritten; another port changes param.json. */
    snprintf(p, sizeof p, "%s/sce_sys/icon0.png", dir);
    put_file(p, "not the icon");
    CHECK(webui_tile_write_files(dir, 9180) == 1);
    CHECK(stat(p, &st) == 0 && (size_t)st.st_size == sizeof webui_tile_icon0_png);
    CHECK(webui_tile_write_files(dir, 9181) == 1);
    CHECK(webui_tile_write_files(dir, 9181) == 0);
    /* No temp files left beside them. */
    CHECK(count_entries(dir) == 2); /* sce_sys, eboot.bin */
    snprintf(p, sizeof p, "%s/sce_sys", dir);
    CHECK(count_entries(p) == 2);   /* param.json, icon0.png */
    /* Removing the folder takes everything with it. */
    CHECK(webui_tile_remove_files(dir) == 0);
    CHECK(stat(dir, &st) != 0);
    CHECK(webui_tile_remove_files(dir) == 0); /* already gone is fine */
    snprintf(p, sizeof p, "%s/tile", g_dir);
    rmdir(p);
}

/* ensure's decision: every combination of (files changed, mount live, mount.lnk ours, appmeta). */
static void tile_plan_decides_rebind_or_register(void) {
    int c, m, l, a;
    for (c = 0; c < 2; c++)
        for (m = 0; m < 2; m++)
            for (l = 0; l < 2; l++)
                for (a = 0; a < 2; a++) {
                    webui_tile_action_t want;
                    if (c) want = WEBUI_TILE_REGISTER;           /* the copies must be refreshed */
                    else if (m && l) want = WEBUI_TILE_NONE;     /* registered this boot */
                    else if (!m && l && a) want = WEBUI_TILE_REBIND; /* after a reboot: re-mount only */
                    else want = WEBUI_TILE_REGISTER;             /* never ours, half-removed, or someone else's bind */
                    if (webui_tile_plan(c, m, l, a) != want) {
                        fprintf(stderr, "FAIL plan(changed=%d mount=%d lnk=%d appmeta=%d) = %d, want %d\n", c, m, l, a,
                                (int)webui_tile_plan(c, m, l, a), (int)want);
                        failures++;
                    }
                }
    /* The cases the review named, spelled out. */
    CHECK(webui_tile_plan(0, 0, 1, 1) == WEBUI_TILE_REBIND);
    CHECK(webui_tile_plan(0, 0, 1, 0) == WEBUI_TILE_REGISTER);
    CHECK(webui_tile_plan(0, 0, 0, 1) == WEBUI_TILE_REGISTER);
    CHECK(webui_tile_plan(0, 1, 1, 1) == WEBUI_TILE_NONE);
    CHECK(webui_tile_plan(1, 1, 1, 1) == WEBUI_TILE_REGISTER);
}

/* #480: helper start leaves a tile the user deleted on the PS5 deleted, and never mistakes a
 * fresh registration (app.db lagging behind it) or an unreadable app.db for one. Every case is
 * written out: the expectation is the scenario, not the code's expression. */
static void tile_outside_removal_is_respected(void) {
    static const struct {
        int marker, row, mount, lnk, want;
        const char *why;
    } cases[] = {
        { 1, 0, 0, 0, 1, "deleted on the PS5, then a reboot (no bind, no mount.lnk)" },
        { 1, 0, 0, 1, 1, "deleted on the PS5, Sony left mount.lnk, then a reboot" },
        { 1, 0, 1, 0, 1, "deleted on the PS5 this boot, a bind that is not ours" },
        { 1, 0, 1, 1, 0, "registered this boot, app.db not caught up yet" },
        { 1, 1, 0, 0, 0, "still registered: rebind or register" },
        { 1, 1, 0, 1, 0, "still registered after a reboot: rebind" },
        { 1, 1, 1, 1, 0, "registered and bound this boot" },
        { 1, -1, 0, 0, 0, "app.db unreadable: do not guess" },
        { 1, -1, 1, 1, 0, "app.db unreadable this boot" },
        { 0, 0, 0, 0, 0, "never registered by this helper (first start, or upgraded): register" },
        { 0, 1, 0, 1, 0, "registered by an older helper without the marker" },
        { 0, -1, 0, 0, 0, "no marker, app.db unreadable" },
    };
    size_t i;
    for (i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        if (webui_tile_removed_outside(cases[i].marker, cases[i].row, cases[i].mount, cases[i].lnk) != cases[i].want) {
            fprintf(stderr, "FAIL removed_outside: %s\n", cases[i].why);
            failures++;
        }
    }
}

/* A scripted console for webui_tile_remove_with: what each step answers, and the order they ran. */
typedef struct {
    int staged;
    int unreg_rc[4];     /* per unregister call */
    int sony_ok[4];
    int confirmed[4];
    int rows[4];         /* per appdb_row call */
    int restage_rc, hide_rc, delete_dir_rc;
    int n_unreg, n_rows;
    char log[256];
} fake_console_t;

static void note(fake_console_t *f, const char *step) {
    if (f->log[0]) strncat(f->log, ",", sizeof f->log - strlen(f->log) - 1);
    strncat(f->log, step, sizeof f->log - strlen(f->log) - 1);
}
static int f_staged(void *c) { return ((fake_console_t *)c)->staged; }
static int f_unregister(void *c, int *confirmed, int *sony_ok) {
    fake_console_t *f = c;
    int i = f->n_unreg++;
    note(f, "unregister");
    *confirmed = f->confirmed[i];
    *sony_ok = f->sony_ok[i];
    return f->unreg_rc[i];
}
static int f_row(void *c) {
    fake_console_t *f = c;
    return f->rows[f->n_rows++];
}
static int f_restage(void *c) { note(c, "restage"); return ((fake_console_t *)c)->restage_rc; }
static int f_hide(void *c) { note(c, "hide"); return ((fake_console_t *)c)->hide_rc; }
static int f_del_staged(void *c) { note(c, "delete_staged"); return 0; }
static int f_del_dir(void *c) { note(c, "delete_dir"); return ((fake_console_t *)c)->delete_dir_rc; }
static int f_marker(void *c) { note(c, "unlink_marker"); return 0; }

static webui_tile_removal_t g_verdict;

static int run_remove(fake_console_t *f) {
    webui_tile_remove_ops_t ops = { f, f_staged, f_unregister, f_row, f_restage, f_hide, f_del_staged, f_del_dir,
                                    f_marker };
    return webui_tile_remove_with(&ops, &g_verdict);
}

#define DELETES "delete_staged,delete_dir,unlink_marker"

static void tile_remove_runs_in_order(void) {
    { /* the normal case: Sony removes it, then the files and the marker go */
        fake_console_t f = { .staged = 1, .confirmed = { 1 }, .sony_ok = { 1 }, .rows = { 0 } };
        CHECK(run_remove(&f) == 0);
        CHECK(strcmp(f.log, "unregister," DELETES) == 0);
        CHECK(g_verdict == WEBUI_TILE_GONE);
    }
    { /* the unregister itself failed: nothing is touched */
        fake_console_t f = { .staged = 1, .unreg_rc = { -1 } };
        CHECK(run_remove(&f) == -1);
        CHECK(strcmp(f.log, "unregister") == 0);
        CHECK(g_verdict == WEBUI_TILE_KEEP);
    }
    { /* one record left after Sony's 0: asked again, then gone */
        fake_console_t f = { .staged = 1, .confirmed = { 0, 1 }, .sony_ok = { 1, 1 }, .rows = { 1, 0 } };
        CHECK(run_remove(&f) == 0);
        CHECK(strcmp(f.log, "unregister,unregister," DELETES) == 0);
    }
    { /* the greyed-out tile: restaged, unregistered, gone */
        fake_console_t f = { .staged = 0, .sony_ok = { 0, 1 }, .confirmed = { 0, 1 }, .rows = { 1, 0 } };
        CHECK(run_remove(&f) == 0);
        CHECK(strcmp(f.log, "unregister,restage,unregister," DELETES) == 0);
    }
    { /* restaged, but that unregister failed: stop, nothing deleted, nothing hidden */
        fake_console_t f = { .staged = 0, .unreg_rc = { 0, -1 }, .rows = { 1 } };
        CHECK(run_remove(&f) == -1);
        CHECK(strcmp(f.log, "unregister,restage,unregister") == 0);
    }
    { /* Sony keeps the row: hidden, then the files go */
        fake_console_t f = { .staged = 1, .rows = { 1 } };
        CHECK(run_remove(&f) == 0);
        CHECK(strcmp(f.log, "unregister,hide," DELETES) == 0);
        CHECK(g_verdict == WEBUI_TILE_HIDDEN);
    }
    { /* Sony keeps it and it cannot be hidden: the tile keeps its files (no grey ghost) */
        fake_console_t f = { .staged = 1, .rows = { 1 }, .hide_rc = -1 };
        CHECK(run_remove(&f) == -1);
        CHECK(strcmp(f.log, "unregister,hide") == 0);
    }
    { /* app.db unreadable: only Sony's confirmation lets the files go */
        fake_console_t f = { .staged = 1, .rows = { -1 } };
        CHECK(run_remove(&f) == -1);
        CHECK(strcmp(f.log, "unregister") == 0);
        fake_console_t g = { .staged = 1, .confirmed = { 1 }, .sony_ok = { 1 }, .rows = { -1 } };
        CHECK(run_remove(&g) == 0);
        CHECK(strcmp(g.log, "unregister," DELETES) == 0);
    }
    { /* a nullfs still bound on the folder: it is kept and the removal reports it */
        fake_console_t f = { .staged = 1, .confirmed = { 1 }, .sony_ok = { 1 }, .rows = { 0 }, .delete_dir_rc = -1 };
        CHECK(run_remove(&f) == -1);
        CHECK(strcmp(f.log, "unregister," DELETES) == 0);
    }
}

/* #480: the files go only when the icon is gone from the home screen (or hidden); a row Sony keeps
 * and nobody could hide keeps a working tile instead of leaving a greyed-out one. */
static void tile_removal_verdict_follows_appdb(void) {
    int c, h;
    for (c = 0; c < 2; c++)
        for (h = 0; h < 2; h++) {
            CHECK(webui_tile_removal_verdict(0, c, h) == WEBUI_TILE_GONE);
            CHECK(webui_tile_removal_verdict(1, c, h) == (h ? WEBUI_TILE_HIDDEN : WEBUI_TILE_KEEP));
            CHECK(webui_tile_removal_verdict(-1, c, h) == (c ? WEBUI_TILE_GONE : WEBUI_TILE_KEEP));
        }
    /* The reported case: Sony answered 0 and the PC list was empty, but app.db kept the row. */
    CHECK(webui_tile_removal_verdict(1, 1, 0) == WEBUI_TILE_KEEP);
}

/* ---- #480: the sweep at helper start and the watch after a removal ---- */

static webui_tile_seen_t seen3(int rows, int content, int drawn) {
    webui_tile_seen_t v;
    v.rows = rows;
    v.content = content;
    v.icons_visible = drawn;
    return v;
}

static void tile_install_record_prefers_contentinfo(void) {
    webui_tile_seen_t v;
    v = seen3(1, 0, 1); /* only an icon row is left (Options > Delete left it): not installed */
    CHECK(webui_tile_install_record(&v) == 0);
    v = seen3(1, 1, 1);
    CHECK(webui_tile_install_record(&v) == 1);
    v = seen3(1, -1, 0); /* firmware without tbl_contentinfo: any row counts */
    CHECK(webui_tile_install_record(&v) == 1);
    v = seen3(0, -1, 0);
    CHECK(webui_tile_install_record(&v) == 0);
    v = seen3(-1, -1, 0);
    CHECK(webui_tile_install_record(&v) == -1);
    /* The outside-removal rule on top of it: an icon row alone no longer re-registers the tile. */
    v = seen3(1, 0, 1);
    CHECK(webui_tile_removed_outside(1, webui_tile_install_record(&v), 0, 0) == 1);
    v = seen3(1, 1, 1);
    CHECK(webui_tile_removed_outside(1, webui_tile_install_record(&v), 0, 1) == 0);
}

static void tile_sweep_plan_cases(void) {
    static const struct {
        int tile_on, rows, content, drawn, registered, leftovers, may_remove;
        webui_tile_sweep_t want;
        const char *why;
    } cases[] = {
        { 1, 1, 1, 1, 0, 0, 1, WEBUI_TILE_SWEEP_NONE, "tile on: the ensure path owns it, even a zombie" },
        { 1, 1, 1, 1, 1, 1, 1, WEBUI_TILE_SWEEP_NONE, "tile on and registered" },
        { 1, 0, 0, 0, 0, 1, 1, WEBUI_TILE_SWEEP_NONE, "tile on, leftovers: ensure registers them" },
        { 0, 1, 1, 1, 0, 0, 1, WEBUI_TILE_SWEEP_ZOMBIE, "the reported zombie: listed, drawn, no files" },
        { 0, 1, 0, 1, 0, 0, 0, WEBUI_TILE_SWEEP_ZOMBIE, "an icon row Sony left behind, drawn (watch)" },
        { 0, 1, 1, 0, 0, 0, 0, WEBUI_TILE_SWEEP_ZOMBIE, "an install record without files, icon hidden" },
        { 0, 1, -1, 1, 0, 0, 1, WEBUI_TILE_SWEEP_ZOMBIE, "no contentinfo table, icon drawn" },
        { 0, 1, 0, 0, 0, 0, 1, WEBUI_TILE_SWEEP_NONE, "only hidden icon rows: nothing shows" },
        { 0, 1, 1, 1, 0, 1, 1, WEBUI_TILE_SWEEP_REMOVE, "off but its files are there: finish the removal" },
        { 0, 1, 1, 1, 1, 1, 1, WEBUI_TILE_SWEEP_REMOVE, "off but still registered this boot" },
        { 0, 1, 1, 1, 0, 1, 0, WEBUI_TILE_SWEEP_NONE, "the watch never re-runs a removal that kept the tile" },
        { 0, 1, 1, 1, 1, 0, 0, WEBUI_TILE_SWEEP_NONE, "the watch leaves a bound tile alone" },
        { 0, 0, 0, 0, 0, 1, 1, WEBUI_TILE_SWEEP_CLEAN, "gone from app.db, files left: delete them" },
        { 0, 0, 0, 0, 0, 1, 0, WEBUI_TILE_SWEEP_CLEAN, "gone from app.db, files left (watch)" },
        { 0, 0, 0, 0, 1, 1, 1, WEBUI_TILE_SWEEP_NONE, "gone from app.db but bound: app.db lagging, wait" },
        { 0, 0, 0, 0, 0, 0, 1, WEBUI_TILE_SWEEP_NONE, "nothing at all" },
        { 0, -1, -1, 0, 0, 0, 1, WEBUI_TILE_SWEEP_NONE, "app.db unreadable: never guess" },
        { 0, -1, -1, 0, 0, 1, 1, WEBUI_TILE_SWEEP_NONE, "app.db unreadable, files left: never guess" },
    };
    size_t i;
    for (i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        webui_tile_seen_t v = seen3(cases[i].rows, cases[i].content, cases[i].drawn);
        webui_tile_sweep_t got = webui_tile_sweep_plan(cases[i].tile_on, &v, cases[i].registered, cases[i].leftovers,
                                                       cases[i].may_remove);
        if (got != cases[i].want) {
            fprintf(stderr, "FAIL sweep_plan: %s (got %d)\n", cases[i].why, (int)got);
            failures++;
        }
    }
}

/* A scripted app.db over time: each look returns the next state (the last one repeats). */
typedef struct {
    int tile_on;
    webui_tile_seen_t states[12];
    int n_states, at;
    int registered, leftovers;
    int hide_rc, remove_rc;
    int stop_after_waits; /* wait reports a new request on this wait (1-based); 0 never */
    int n_waits;
    unsigned waited;
    char log[512];
} fake_appdb_t;

static void anote(fake_appdb_t *f, const char *step) {
    if (f->log[0]) strncat(f->log, ",", sizeof f->log - strlen(f->log) - 1);
    strncat(f->log, step, sizeof f->log - strlen(f->log) - 1);
}
static int a_tile_on(void *c) { return ((fake_appdb_t *)c)->tile_on; }
static int a_look(void *c, webui_tile_seen_t *seen) {
    fake_appdb_t *f = c;
    int i = f->at < f->n_states ? f->at : f->n_states - 1;
    f->at++;
    *seen = f->states[i];
    anote(f, "look");
    return 0;
}
static int a_registered(void *c) { return ((fake_appdb_t *)c)->registered; }
static int a_leftovers(void *c) { return ((fake_appdb_t *)c)->leftovers; }
static int a_uninstall(void *c) { anote(c, "uninstall"); return 0; }
static int a_hide(void *c) {
    fake_appdb_t *f = c;
    anote(f, "hide");
    return f->hide_rc;
}
static void a_keep(void *c) { anote(c, "keep"); }
static int a_remove(void *c) { anote(c, "remove"); return ((fake_appdb_t *)c)->remove_rc; }
static int a_clean(void *c) { anote(c, "clean"); return 0; }
static int a_wait(void *c, unsigned s) {
    fake_appdb_t *f = c;
    f->n_waits++;
    if (f->stop_after_waits && f->n_waits == f->stop_after_waits) return 1;
    f->waited += s;
    return 0;
}

static webui_tile_sweep_ops_t aops(fake_appdb_t *f) {
    webui_tile_sweep_ops_t o = { f, a_tile_on, a_look, a_registered, a_leftovers, a_uninstall, a_hide, a_keep,
                                 a_remove, a_clean, a_wait };
    return o;
}

static void tile_zombie_is_uninstalled_then_hidden(void) {
    { /* the reported state: Sony's record and a drawn icon, no files. Sony gives up the record. */
        fake_appdb_t f = { .states = { seen3(1, 1, 1), seen3(1, 1, 1), seen3(0, 0, 0) }, .n_states = 3 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_REMOVED);
        CHECK(strcmp(f.log, "look,look,uninstall,look,keep") == 0);
        CHECK(un == 1);
    }
    { /* Sony keeps it: hidden everywhere, and the icon keeper keeps it hidden */
        fake_appdb_t f = { .states = { seen3(1, 1, 1) }, .n_states = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_HIDDEN);
        CHECK(strcmp(f.log, "look,look,uninstall,look,keep,hide") == 0);
    }
    { /* an icon row alone (no install record): no Sony call, just hidden */
        fake_appdb_t f = { .states = { seen3(1, 0, 1) }, .n_states = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_HIDDEN);
        CHECK(strcmp(f.log, "look,look,keep,hide") == 0);
        CHECK(un == 0);
    }
    { /* hiding failed: reported, never a silent success */
        fake_appdb_t f = { .states = { seen3(1, 0, 1) }, .n_states = 1, .hide_rc = -1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_FAILED);
    }
    { /* the tile is on: nothing is read, nothing touched */
        fake_appdb_t f = { .tile_on = 1, .states = { seen3(1, 1, 1) }, .n_states = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_NONE);
        CHECK(f.log[0] == '\0');
    }
    { /* at start, off, its files still there: the full removal */
        fake_appdb_t f = { .states = { seen3(1, 1, 1) }, .n_states = 1, .leftovers = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_REMOVED);
        CHECK(strcmp(f.log, "look,remove") == 0);
    }
    { /* gone from app.db, files left: deleted without a Sony call */
        fake_appdb_t f = { .states = { seen3(0, 0, 0) }, .n_states = 1, .leftovers = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_sweep_once(&o, 1, &un) == WEBUI_TILE_RESULT_REMOVED);
        CHECK(strcmp(f.log, "look,clean") == 0);
    }
}

static void tile_settle_catches_a_row_that_comes_back(void) {
    static const unsigned delays[] = WEBUI_TILE_SETTLE_DELAYS;
    unsigned total = 0;
    size_t i;
    for (i = 0; i < sizeof delays / sizeof delays[0]; i++) total += delays[i];
    CHECK(total >= 100 && total <= 150); /* "about two minutes" */
    { /* gone, gone, then the home screen writes the icon back: hidden (no record: no Sony call) */
        fake_appdb_t f = { .states = { seen3(0, 0, 0), seen3(0, 0, 0), seen3(1, 0, 1), seen3(1, 0, 1), seen3(1, 0, 0) },
                           .n_states = 5 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_settle_with(&o, &un) == WEBUI_TILE_RESULT_HIDDEN);
        CHECK(strncmp(f.log, "look,look,look,look,keep,hide,look", 34) == 0);
        CHECK(f.waited == total);
    }
    { /* an install record reappears and stays, icon hidden: one extra uninstall in the whole watch */
        fake_appdb_t f = { .states = { seen3(0, 0, 0), seen3(1, 1, 0) }, .n_states = 2 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        (void)webui_tile_settle_with(&o, &un);
        CHECK(strstr(f.log, "uninstall") != NULL);
        CHECK(strstr(strstr(f.log, "uninstall") + 1, "uninstall") == NULL);
    }
    { /* stays gone: nothing done */
        fake_appdb_t f = { .states = { seen3(0, 0, 0) }, .n_states = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_settle_with(&o, &un) == WEBUI_TILE_RESULT_NONE);
        CHECK(strstr(f.log, "hide") == NULL && strstr(f.log, "uninstall") == NULL);
    }
    { /* a new request (the tile turned on again) stops the watch at once */
        fake_appdb_t f = { .states = { seen3(0, 0, 0), seen3(1, 0, 1) }, .n_states = 2, .stop_after_waits = 2 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_settle_with(&o, &un) == WEBUI_TILE_RESULT_NONE);
        CHECK(strcmp(f.log, "look") == 0);
    }
    { /* a removal that kept a working tile (files there): the watch does not retry it */
        fake_appdb_t f = { .states = { seen3(1, 1, 1) }, .n_states = 1, .leftovers = 1 };
        webui_tile_sweep_ops_t o = aops(&f);
        int un = 0;
        CHECK(webui_tile_settle_with(&o, &un) == WEBUI_TILE_RESULT_NONE);
        CHECK(strstr(f.log, "remove") == NULL && strstr(f.log, "hide") == NULL);
    }
}

static void tile_result_names(void) {
    CHECK(strcmp(webui_tile_result_name(WEBUI_TILE_RESULT_NONE), "") == 0);
    CHECK(strcmp(webui_tile_result_name(WEBUI_TILE_RESULT_ADDED), "added") == 0);
    CHECK(strcmp(webui_tile_result_name(WEBUI_TILE_RESULT_REMOVED), "removed") == 0);
    CHECK(strcmp(webui_tile_result_name(WEBUI_TILE_RESULT_HIDDEN), "hidden") == 0);
    CHECK(strcmp(webui_tile_result_name(WEBUI_TILE_RESULT_FAILED), "failed") == 0);
}

/* Remove PS5Upload Minion: files and folders go, a missing path is not counted, webui.json is not
 * among them (the caller keeps it, off). */
static void minion_forget_removes_leftovers(void) {
    char prefs[320], log[320], lib[320], icons[320], icon[352], keep[320];
    const char *paths[3];
    int removed = -1;
    struct stat st;
    snprintf(prefs, sizeof prefs, "%s/files-prefs.json", g_dir);
    snprintf(log, sizeof log, "%s/webui-input.log", g_dir);
    snprintf(lib, sizeof lib, "%s/library", g_dir);
    snprintf(icons, sizeof icons, "%s/icons", lib);
    snprintf(icon, sizeof icon, "%s/a.png", icons);
    snprintf(keep, sizeof keep, "%s/webui.json", g_dir);
    put_file(prefs, "{}");
    put_file(keep, "{\"enabled\":false}");
    CHECK(mkdir(lib, 0777) == 0 && mkdir(icons, 0777) == 0);
    put_file(icon, "x");
    paths[0] = prefs;
    paths[1] = log; /* never written */
    paths[2] = lib;
    CHECK(webui_minion_forget(paths, 3, &removed) == 0);
    CHECK(removed == 2);
    CHECK(lstat(prefs, &st) != 0 && lstat(lib, &st) != 0);
    CHECK(lstat(keep, &st) == 0);
    CHECK(webui_minion_forget(paths, 3, &removed) == 0 && removed == 0);
    unlink(keep);
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "/tmp/ps5upload-webui-settings-XXXXXX");
    if (!mkdtemp(g_dir)) {
        perror("mkdtemp");
        return 2;
    }
    missing_file_gives_defaults();
    garbled_file_gives_defaults();
    save_then_load_round_trips();
    startup_plan_follows_enabled_and_tile();
    param_json_exact();
    tile_files_written_only_when_different();
    tile_plan_decides_rebind_or_register();
    tile_outside_removal_is_respected();
    tile_removal_verdict_follows_appdb();
    tile_remove_runs_in_order();
    tile_install_record_prefers_contentinfo();
    tile_sweep_plan_cases();
    tile_zombie_is_uninstalled_then_hidden();
    tile_settle_catches_a_row_that_comes_back();
    tile_result_names();
    minion_forget_removes_leftovers();
    rmdir(g_dir);
    if (failures) {
        fprintf(stderr, "webui settings self-test: %d failure(s)\n", failures);
        return 1;
    }
    printf("webui settings self-test: ok\n");
    return 0;
}
