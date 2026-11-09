/* The "ps5upload" home-screen tile: see include/webui_tile.h.
 *
 * The file half (param.json, writing the folder, deleting it) is host-tested with
 * -DWEBUI_TILE_HOST_TEST (payload/tests/webui_settings_selftest.c); the Sony half (register,
 * unregister, the worker thread) is built for the console only. */
#include "webui_tile.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "config.h"

/* The tile's eboot.bin. register_title_from_path refuses a folder without one
 * (register_src_missing_eboot; measured on the Phat, ruling R6); a few placeholder bytes
 * register fine. Nothing ever runs it: the param.json deeplinkUri opens the browser. */
static const char k_eboot[] = "ps5upload home-screen tile: opens the browser on the console web UI\n";

size_t webui_tile_param_json(char *out, size_t cap, uint16_t port) {
    int k = snprintf(out, cap,
                     "{\"applicationCategoryType\":66048,\"titleId\":\"%s\","
                     "\"deeplinkUri\":\"http://127.0.0.1:%u/\","
                     "\"localizedParameters\":{\"defaultLanguage\":\"en-US\",\"en-US\":{\"titleName\":\"PS5Upload Minion\"}}}",
                     WEBUI_TILE_TITLE_ID, (unsigned)port);
    return k < 0 ? 0 : (size_t)k;
}

/* mkdir -p, 0777 like every folder the helper creates. */
static int mkdir_parents(const char *dir) {
    char p[1024];
    size_t i, n = strlen(dir);
    if (n == 0 || n >= sizeof p) return -1;
    memcpy(p, dir, n + 1);
    for (i = 1; i <= n; i++) {
        if (p[i] != '/' && p[i] != '\0') continue;
        p[i] = '\0';
        if (mkdir(p, 0777) != 0 && errno != EEXIST) return -1;
        p[i] = i < n ? '/' : '\0';
    }
    return 0;
}

/* 1 when `path` holds exactly these bytes, 0 otherwise (missing, other size or content). */
static int same_bytes(const char *path, const unsigned char *data, size_t len) {
    unsigned char buf[4096];
    struct stat st;
    size_t at = 0;
    int fd = open(path, O_RDONLY), same = 0;
    if (fd < 0) return 0;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || (size_t)st.st_size != len) goto out;
    while (at < len) {
        size_t want = len - at < sizeof buf ? len - at : sizeof buf;
        ssize_t r = read(fd, buf, want);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0 || memcmp(buf, data + at, (size_t)r) != 0) goto out;
        at += (size_t)r;
    }
    same = 1;
out:
    close(fd);
    return same;
}

/* `<path>.tmp` in the same directory, fsync, rename over `path` (never across a mount). */
static int write_atomic(const char *path, const unsigned char *data, size_t len) {
    char tmp[1100];
    size_t done = 0;
    int fd, k = snprintf(tmp, sizeof tmp, "%s.tmp", path);
    if (k < 0 || (size_t)k >= sizeof tmp) return -1;
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    while (done < len) {
        ssize_t w = write(fd, data + done, len - done);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) break;
        done += (size_t)w;
    }
    if (done != len || fsync(fd) != 0) {
        close(fd);
        unlink(tmp);
        return -1;
    }
    if (close(fd) != 0 || rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* 1 written, 0 already equal, -1 failed. */
static int sync_file(const char *dir, const char *name, const unsigned char *data, size_t len) {
    char p[1024];
    int k = snprintf(p, sizeof p, "%s/%s", dir, name);
    if (k < 0 || (size_t)k >= sizeof p) return -1;
    if (same_bytes(p, data, len)) return 0;
    return write_atomic(p, data, len) == 0 ? 1 : -1;
}

int webui_tile_write_files(const char *dir, uint16_t port) {
    char sce_sys[1024], param[512];
    size_t plen = webui_tile_param_json(param, sizeof param, port);
    int k, r, changed = 0;
    if (plen == 0 || plen >= sizeof param) return -1;
    k = snprintf(sce_sys, sizeof sce_sys, "%s/sce_sys", dir);
    if (k < 0 || (size_t)k >= sizeof sce_sys) return -1;
    if (mkdir_parents(sce_sys) != 0) return -1;
    r = sync_file(sce_sys, "param.json", (const unsigned char *)param, plen);
    if (r < 0) return -1;
    changed += r;
    r = sync_file(sce_sys, "icon0.png", webui_tile_icon0_png, webui_tile_icon0_png_len);
    if (r < 0) return -1;
    changed += r;
    r = sync_file(dir, "eboot.bin", (const unsigned char *)k_eboot, sizeof k_eboot - 1);
    if (r < 0) return -1;
    changed += r;
    return changed;
}

/* rm -r, bounded depth (the tile folder is two levels deep). */
static int remove_tree(const char *path, int depth) {
    struct stat st;
    DIR *d;
    struct dirent *e;
    int rc = 0;
    if (lstat(path, &st) != 0) return errno == ENOENT ? 0 : -1;
    if (!S_ISDIR(st.st_mode)) return unlink(path) == 0 || errno == ENOENT ? 0 : -1;
    if (depth > 4) return -1;
    d = opendir(path);
    if (!d) return -1;
    while ((e = readdir(d)) != NULL) {
        char child[1024];
        int k;
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        k = snprintf(child, sizeof child, "%s/%s", path, e->d_name);
        if (k < 0 || (size_t)k >= sizeof child || remove_tree(child, depth + 1) != 0) rc = -1;
    }
    closedir(d);
    if (rmdir(path) != 0 && errno != ENOENT) rc = -1;
    return rc;
}

int webui_tile_remove_files(const char *dir) { return remove_tree(dir, 0); }

webui_tile_action_t webui_tile_plan(int files_changed, int mount_live, int lnk_ours, int appmeta, int listed) {
    if (files_changed) return WEBUI_TILE_REGISTER;
    /* No install record: the home screen shows nothing, whatever is bound. Unreadable: never guess. */
    if (listed == 0) return WEBUI_TILE_REGISTER;
    if (mount_live && lnk_ours) return WEBUI_TILE_NONE;
    /* A live mount that is not ours, or a half-removed registration: only the full register
     * (which pre-cleans the mount point and restages everything) puts it right. */
    if (!mount_live && lnk_ours && appmeta) return WEBUI_TILE_REBIND;
    return WEBUI_TILE_REGISTER;
}

int webui_tile_ensure_with(const webui_tile_ensure_ops_t *o, webui_tile_action_t *done, const char **why) {
    const char *err = NULL;
    webui_tile_action_t a;
    int changed, listed, rc = 0, full = 0;
    unsigned waited;
    if (why) *why = "";
    if (done) *done = WEBUI_TILE_NONE;
    changed = o->write_files(o->ctx);
    if (changed < 0) {
        if (why) *why = "write_failed";
        return -1;
    }
    /* A changed file re-registers anyway: app.db is read only when it can change the plan. */
    listed = changed > 0 ? -1 : o->listed(o->ctx);
    a = webui_tile_plan(changed > 0, o->mount_live(o->ctx), o->lnk_ours(o->ctx), o->appmeta(o->ctx), listed);
    if (done) *done = a;
    if (a == WEBUI_TILE_REBIND && o->remount(o->ctx, &err) != 0) a = WEBUI_TILE_REGISTER; /* fall back */
    if (a == WEBUI_TILE_REGISTER) {
        err = NULL;
        full = 1;
        rc = o->register_full(o->ctx, &err);
        if (rc != 0) {
            if (why) *why = err && *err ? err : "register_failed";
            return -1;
        }
    }
    if (!full) return 0;
    /* Sony said yes: the tile exists only once app.db lists it. */
    for (waited = 0;; waited++) {
        listed = o->listed(o->ctx);
        if (listed != 0 || waited >= WEBUI_TILE_LISTED_WAIT_S) break;
        o->sleep_s(o->ctx, 1);
    }
    if (listed == 0) {
        if (why) *why = "not_listed";
        return -1;
    }
    return 0;
}

int webui_tile_removed_outside(int marker, int appdb_row, int mount_live, int lnk_ours) {
    return marker && appdb_row == 0 && !(mount_live && lnk_ours);
}

webui_tile_removal_t webui_tile_removal_verdict(int appdb_row, int sony_confirmed, int hidden) {
    if (appdb_row == 0) return WEBUI_TILE_GONE;
    if (appdb_row < 0) return sony_confirmed ? WEBUI_TILE_GONE : WEBUI_TILE_KEEP;
    return hidden ? WEBUI_TILE_HIDDEN : WEBUI_TILE_KEEP;
}

int webui_tile_remove_with(const webui_tile_remove_ops_t *o, webui_tile_removal_t *verdict) {
    int confirmed = 0, sony_ok = 0, c2 = 0, s2 = 0, row, hidden = 0, rc = 0;
    int staged = o->staged(o->ctx);
    webui_tile_removal_t v;
    if (verdict) *verdict = WEBUI_TILE_KEEP;
    /* The nullfs must be gone before the folder under it is deleted: on an unregister failure the
     * folders stay (a tile pointing at nothing is worse than a tile that still works). */
    if (o->unregister(o->ctx, &confirmed, &sony_ok) != 0) return -1;
    /* Judge by app.db, which the home screen draws. Its row outliving the uninstall while the tile's
     * files were deleted is the greyed-out tile of #480. */
    row = o->appdb_row(o->ctx);
    if (row == 1 && sony_ok) {
        /* One uninstall can remove one of two install records (measured on FW 13.x after
         * AppInstallAll, where unregister_title already repeats it; not there after
         * AppInstallTitleDir): ask once more. */
        if (o->unregister(o->ctx, &c2, &s2) == 0) row = o->appdb_row(o->ctx);
    }
    if (row == 1 && !staged && o->restage(o->ctx) == 0) {
        /* Sony's record outlived the tile's files (an older helper deleted them after a refused
         * uninstall); it is staged and registered again so the uninstall has an app to remove.
         * The folder is bound again now: only an unregister that worked lets anything go. */
        if (o->unregister(o->ctx, &confirmed, &sony_ok) != 0) return -1;
        row = o->appdb_row(o->ctx);
    }
    /* Sony keeps it: hide the icon the way the PC hides one (#407), and keep it hidden. */
    if (row == 1) hidden = o->hide(o->ctx) == 0;
    v = webui_tile_removal_verdict(row, confirmed, hidden);
    if (verdict) *verdict = v;
    if (v == WEBUI_TILE_KEEP) return -1;
    /* The staged /user/app copy goes too: on FW 13.x the next folder-game register runs
     * AppInstallAll, which would install the removed tile again. */
    if (o->delete_staged(o->ctx) != 0) rc = -1;
    if (o->delete_dir(o->ctx) != 0) rc = -1; /* refuses while a nullfs is bound on the folder */
    if (o->unlink_marker(o->ctx) != 0) rc = -1;
    return rc;
}

const char *webui_tile_result_name(webui_tile_result_t r) {
    switch (r) {
    case WEBUI_TILE_RESULT_ADDED: return "added";
    case WEBUI_TILE_RESULT_REMOVED: return "removed";
    case WEBUI_TILE_RESULT_HIDDEN: return "hidden";
    case WEBUI_TILE_RESULT_FAILED: return "failed";
    default: return "";
    }
}

int webui_tile_install_record(const webui_tile_seen_t *seen) {
    if (seen->rows < 0) return -1;
    return seen->content >= 0 ? seen->content : seen->rows;
}

webui_tile_sweep_t webui_tile_sweep_plan(int tile_on, const webui_tile_seen_t *seen, int registered,
                                         int leftovers, int may_remove) {
    if (tile_on) return WEBUI_TILE_SWEEP_NONE;        /* the user wants the tile: never fight it */
    if (seen->rows < 0) return WEBUI_TILE_SWEEP_NONE; /* unreadable: never guess */
    if (seen->rows == 0) return leftovers && !registered ? WEBUI_TILE_SWEEP_CLEAN : WEBUI_TILE_SWEEP_NONE;
    /* app.db lists it. Our own tile (bound, or its files still there) is a removal to finish. */
    if (registered || leftovers) return may_remove ? WEBUI_TILE_SWEEP_REMOVE : WEBUI_TILE_SWEEP_NONE;
    /* Listed, nothing of ours behind it: a zombie when Sony still has an install record or an icon
     * is still drawn. Only hidden icon rows left: nothing shows. */
    if (seen->content == 1 || seen->icons_visible > 0) return WEBUI_TILE_SWEEP_ZOMBIE;
    return WEBUI_TILE_SWEEP_NONE;
}

webui_tile_result_t webui_tile_fix_zombie(const webui_tile_sweep_ops_t *o, int *uninstalled) {
    webui_tile_seen_t seen;
    if (o->look(o->ctx, &seen) != 0 || seen.rows < 0) return WEBUI_TILE_RESULT_FAILED;
    if (seen.content == 1 && !*uninstalled) {
        /* Sony still counts it installed: ask it to uninstall once more, then look again. */
        *uninstalled = 1;
        (void)o->uninstall(o->ctx);
        if (o->look(o->ctx, &seen) != 0 || seen.rows < 0) return WEBUI_TILE_RESULT_FAILED;
    }
    /* Whatever happens next, the icon keeper hides it again if the system shows it later. */
    o->keep_hidden(o->ctx);
    if (seen.rows == 0) return WEBUI_TILE_RESULT_REMOVED;
    if (seen.icons_visible > 0 && o->hide(o->ctx) != 0) return WEBUI_TILE_RESULT_FAILED;
    return WEBUI_TILE_RESULT_HIDDEN;
}

webui_tile_result_t webui_tile_sweep_once(const webui_tile_sweep_ops_t *o, int may_remove, int *uninstalled) {
    webui_tile_seen_t seen;
    if (o->tile_on(o->ctx)) return WEBUI_TILE_RESULT_NONE; /* checked first: no app.db read at all */
    if (o->look(o->ctx, &seen) != 0) return WEBUI_TILE_RESULT_NONE;
    switch (webui_tile_sweep_plan(o->tile_on(o->ctx), &seen, o->registered(o->ctx), o->leftovers(o->ctx),
                                  may_remove)) {
    case WEBUI_TILE_SWEEP_REMOVE:
        return o->remove(o->ctx) == 0 ? WEBUI_TILE_RESULT_REMOVED : WEBUI_TILE_RESULT_FAILED;
    case WEBUI_TILE_SWEEP_ZOMBIE:
        /* A record Sony kept after one more uninstall, with every icon already hidden: done. */
        if (*uninstalled && seen.icons_visible == 0) return WEBUI_TILE_RESULT_NONE;
        return webui_tile_fix_zombie(o, uninstalled);
    case WEBUI_TILE_SWEEP_CLEAN:
        return o->clean(o->ctx) == 0 ? WEBUI_TILE_RESULT_REMOVED : WEBUI_TILE_RESULT_FAILED;
    default:
        return WEBUI_TILE_RESULT_NONE;
    }
}

webui_tile_result_t webui_tile_settle_with(const webui_tile_sweep_ops_t *o, int *uninstalled) {
    static const unsigned delays[] = WEBUI_TILE_SETTLE_DELAYS;
    webui_tile_result_t last = WEBUI_TILE_RESULT_NONE;
    size_t i;
    for (i = 0; i < sizeof delays / sizeof delays[0]; i++) {
        webui_tile_result_t r;
        if (o->wait(o->ctx, delays[i])) break;
        r = webui_tile_sweep_once(o, 0, uninstalled);
        if (r != WEBUI_TILE_RESULT_NONE) last = r;
    }
    return last;
}

#ifndef WEBUI_TILE_HOST_TEST

#include <pthread.h>
#include <sys/mount.h>
#include <sys/param.h>
#include <time.h>

#include "content_db.h"
#include "icon_keeper.h"
#include "power_phase.h"
#include "proc_identity.h"
#include "register.h"
#include "unregister_outcome.h"
#include "webui_settings.h"

#define TILE_STACK (256u * 1024u)
#define TILE_THREAD_NAME "ps5upload-tile"
#define TILE_SYSTEM_EX "/system_ex/app/" WEBUI_TILE_TITLE_ID
#define TILE_MOUNT_LNK "/user/app/" WEBUI_TILE_TITLE_ID "/mount.lnk"
/* The tile's tracker while it is registered: mount.lnk under another name. ShadowMount+ 1.7 treats
 * every /user/app/<id>/mount.lnk as one of its on-demand titles and unmounts its /system_ex/app
 * bind a few seconds after it appears ("[LINK] runtime layers released: PSUP00001", measured on the
 * Phat, FW 13.60, SMP v1.7beta7: the tile's bind lasted under 6 s with mount.lnk and 60 s+ without
 * it). An unbound tile is the greyed-out icon that still opens the page (#480). unregister_title
 * recognizes our registrations by mount.lnk, so it is put back just before an unregister. */
#define TILE_OWN_LNK "/user/app/" WEBUI_TILE_TITLE_ID "/ps5upload.lnk"

/* Serializes ensure and remove (Sony calls, the folder) whoever calls them. */
static pthread_mutex_t g_op_mu = PTHREAD_MUTEX_INITIALIZER;

#define TILE_USER_APP "/user/app/" WEBUI_TILE_TITLE_ID
#define TILE_APPMETA "/user/appmeta/" WEBUI_TILE_TITLE_ID

/* The nullfs register_title_from_path binds at /system_ex/app/PSUP00001 is live. It is rolled back
 * when Sony's install call fails and does not survive a reboot. One statfs. */
static int tile_mount_live(void) {
    struct statfs sfs;
    return statfs(TILE_SYSTEM_EX, &sfs) == 0 && strcmp(sfs.f_mntonname, TILE_SYSTEM_EX) == 0;
}

/* The tracker (TILE_OWN_LNK, or mount.lnk as register_title_from_path and older helpers left it)
 * names WEBUI_TILE_DIR. */
static int tile_lnk_ours(void) {
    char lnk[256];
    ssize_t n;
    int fd = open(TILE_OWN_LNK, O_RDONLY);
    if (fd < 0) fd = open(TILE_MOUNT_LNK, O_RDONLY);
    if (fd < 0) return 0;
    n = read(fd, lnk, sizeof lnk - 1);
    close(fd);
    if (n <= 0) return 0;
    lnk[n] = '\0';
    while (n > 0 && (lnk[n - 1] == '\n' || lnk[n - 1] == '\r' || lnk[n - 1] == '/')) lnk[--n] = '\0';
    return strcmp(lnk, WEBUI_TILE_DIR) == 0;
}

static int is_dir(const char *p) {
    struct stat st;
    return stat(p, &st) == 0 && S_ISDIR(st.st_mode);
}

/* What app.db says about PSUP00001, every titleId table looked at, logged so a bug report shows
 * exactly which tables still held it at each step (#480). `step` names the step. */
static void tile_look(webui_tile_seen_t *seen, const char *step) {
    content_db_presence_t p;
    if (content_db_title_presence(WEBUI_TILE_TITLE_ID, &p) != 0) {
        seen->rows = -1;
        seen->content = -1;
        seen->icons_visible = 0;
        fprintf(stderr, "webui tile: app.db %s: cannot be read\n", step);
        return;
    }
    seen->rows = p.any;
    seen->content = p.content;
    seen->icons_visible = p.icons_visible;
    if (p.any)
        fprintf(stderr, "webui tile: app.db %s: %s in %s (%d tables; install record %d, icons %d, drawn %d)\n",
                step, WEBUI_TILE_TITLE_ID, p.where, p.tables, p.content, p.icons, p.icons_visible);
    else
        fprintf(stderr, "webui tile: app.db %s: %s not listed (%d tables)\n", step, WEBUI_TILE_TITLE_ID, p.tables);
}

/* Sony's own rc is logged by register.c ("[register] AppInstallAll PSUP00001 rc=0x..."). */
static int tile_register_why(const char **why) {
    char tid[REGISTER_MAX_TITLE_ID], tname[REGISTER_MAX_TITLE_NAME];
    const char *err = NULL;
    int used = 0;
    if (register_title_from_path(WEBUI_TILE_DIR, 0, tid, tname, &used, &err) != 0) {
        fprintf(stderr, "webui tile: full register FAILED: %s\n", err ? err : "unknown");
        if (why) *why = err;
        return -1;
    }
    fprintf(stderr, "webui tile: full register of %s returned ok\n", tid);
    return 0;
}

static int tile_register(void) { return tile_register_why(NULL); }

/* The tile is on the home screen (again): remember that the helper put it there, stop the icon
 * keeper hiding it, and make its icon visible: an earlier removal may have only been able to hide
 * it, and a registered tile must never stay invisible. "No icon" (app.db not updated yet) is fine. */
/* mount.lnk -> TILE_OWN_LNK (same directory), so ShadowMount+ leaves the bind alone; when it already
 * released the bind in the seconds mount.lnk was there, bind it again. */
static void tile_lnk_hide_from_smp(void) {
    const char *err = NULL;
    if (access(TILE_MOUNT_LNK, F_OK) == 0) {
        if (rename(TILE_MOUNT_LNK, TILE_OWN_LNK) != 0) {
            fprintf(stderr, "webui tile: cannot rename %s: %s\n", TILE_MOUNT_LNK, strerror(errno));
            return;
        }
        fprintf(stderr, "webui tile: tracker kept as %s (ShadowMount+ releases titles with a mount.lnk)\n",
                TILE_OWN_LNK);
    }
    if (!tile_mount_live()) {
        if (register_remount_nullfs(WEBUI_TILE_TITLE_ID, WEBUI_TILE_DIR, &err) == 0)
            fprintf(stderr, "webui tile: the bind was released meanwhile; bound again\n");
        else
            fprintf(stderr, "webui tile: the bind was released and re-binding failed: %s\n", err ? err : "unknown");
    }
}

/* TILE_OWN_LNK -> mount.lnk, so unregister_title sees our registration (unmounts the bind, removes
 * the appmeta copy). */
static void tile_lnk_restore(void) {
    if (access(TILE_OWN_LNK, F_OK) != 0) return;
    if (access(TILE_MOUNT_LNK, F_OK) == 0) (void)unlink(TILE_OWN_LNK);
    else if (rename(TILE_OWN_LNK, TILE_MOUNT_LNK) != 0)
        fprintf(stderr, "webui tile: cannot restore %s: %s\n", TILE_MOUNT_LNK, strerror(errno));
}

static void tile_now_registered(void) {
    char why[160] = {0};
    int fd = open(WEBUI_TILE_MARKER, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0) close(fd);
    webui_tile_seen_t seen;
    tile_lnk_hide_from_smp();
    icon_keeper_note(WEBUI_TILE_TITLE_ID, 0);
    (void)content_db_icon_set_visible(WEBUI_TILE_TITLE_ID, 1, why, sizeof why);
    tile_look(&seen, "registered");
}

int webui_tile_respect_outside_removal(void) {
    webui_tile_seen_t seen;
    int out;
    if (access(WEBUI_TILE_MARKER, F_OK) != 0) return 0;
    pthread_mutex_lock(&g_op_mu);
    tile_look(&seen, "at start (tile on)");
    /* Sony's install record decides: Options > Delete removes it, and an icon row it leaves behind
     * is a leftover the sweep hides, not a reason to register the tile again. Without such a table
     * (other firmware), any row counts. */
    out = webui_tile_removed_outside(1, webui_tile_install_record(&seen), tile_mount_live(), tile_lnk_ours());
    if (out) {
        fprintf(stderr, "webui tile: removed outside the helper (no install record for %s); not registering it again\n",
                WEBUI_TILE_TITLE_ID);
        /* Left staged, FW 13.x AppInstallAll would install it again with the next folder game. The
         * folder stays while a bind still sits on it. */
        (void)webui_tile_remove_files(TILE_USER_APP);
        (void)webui_tile_remove_files(TILE_APPMETA);
        if (!tile_mount_live()) (void)webui_tile_remove_files(WEBUI_TILE_DIR);
        (void)unlink(WEBUI_TILE_MARKER);
    }
    pthread_mutex_unlock(&g_op_mu);
    return out;
}

/* ---- ensure's console steps (webui_tile_ensure_ops_t) ---- */

static int en_write_files(void *ctx) {
    int changed = webui_tile_write_files(WEBUI_TILE_DIR, *(const uint16_t *)ctx);
    if (changed < 0) fprintf(stderr, "webui tile: cannot write %s: %s\n", WEBUI_TILE_DIR, strerror(errno));
    return changed;
}

static int en_mount_live(void *ctx) {
    (void)ctx;
    return tile_mount_live();
}

static int en_lnk_ours(void *ctx) {
    (void)ctx;
    return tile_lnk_ours();
}

static int en_appmeta(void *ctx) {
    (void)ctx;
    return is_dir(TILE_APPMETA);
}

static int en_listed(void *ctx) {
    webui_tile_seen_t seen;
    (void)ctx;
    tile_look(&seen, "ensure");
    return webui_tile_install_record(&seen);
}

static int en_remount(void *ctx, const char **err) {
    (void)ctx;
    /* Still registered with Sony from an earlier boot: re-mount only, no Sony call. */
    if (register_remount_nullfs(WEBUI_TILE_TITLE_ID, WEBUI_TILE_DIR, err) == 0) {
        fprintf(stderr, "webui tile: re-bound the nullfs (no Sony call)\n");
        return 0;
    }
    fprintf(stderr, "webui tile: re-bind failed (%s); falling back to a full register\n",
            *err ? *err : "unknown");
    return -1;
}

static int en_register_full(void *ctx, const char **err) {
    (void)ctx;
    /* A changed file (register copies sce_sys into /user/app and /user/appmeta, so the copies must be
     * refreshed; Sony answers 0x80990002 "restored": success), not listed, or never registered. */
    return tile_register_why(err);
}

static void en_sleep_s(void *ctx, unsigned seconds) {
    (void)ctx;
    sleep(seconds);
}

static const char *tile_action_name(webui_tile_action_t a) {
    return a == WEBUI_TILE_NONE ? "none (registered this boot and listed)"
           : a == WEBUI_TILE_REBIND ? "re-bind"
                                    : "full register";
}

int webui_tile_ensure(uint16_t port, const char **why) {
    webui_tile_ensure_ops_t ops = {
        &port, en_write_files, en_mount_live, en_lnk_ours, en_appmeta, en_listed, en_remount, en_register_full,
        en_sleep_s,
    };
    webui_tile_action_t a = WEBUI_TILE_NONE;
    const char *reason = "";
    int rc;
    pthread_mutex_lock(&g_op_mu);
    /* First: a removal had the icon keeper keep PSUP00001 hidden, and the register below changes
     * app.db, which wakes the keeper (seen on the Phat: it hid the new icon mid-register). */
    icon_keeper_note(WEBUI_TILE_TITLE_ID, 0);
    rc = webui_tile_ensure_with(&ops, &a, &reason);
    if (rc == 0) {
        tile_now_registered();
        fprintf(stderr, "webui tile: ensure ok (planned: %s)\n", tile_action_name(a));
    } else {
        fprintf(stderr, "webui tile: ensure FAILED (planned: %s): %s; the tile is NOT on the home screen\n",
                tile_action_name(a), reason);
    }
    pthread_mutex_unlock(&g_op_mu);
    if (why) *why = reason;
    return rc;
}

static int op_staged(void *ctx) {
    (void)ctx;
    return is_dir(TILE_USER_APP);
}

static int op_unregister(void *ctx, int *confirmed, int *sony_ok) {
    const char *err = NULL;
    unregister_outcome_t o;
    (void)ctx;
    tile_lnk_restore();
    if (unregister_title(WEBUI_TILE_TITLE_ID, &err, &o) != 0) {
        fprintf(stderr, "webui tile: unregister failed: %s; keeping %s\n", err ? err : "unknown", WEBUI_TILE_DIR);
        tile_lnk_hide_from_smp();
        return -1;
    }
    *confirmed = unregister_tile_confirmed(&o);
    *sony_ok = o.api_called && o.sony_rc == 0;
    fprintf(stderr, "webui tile: uninstall rc=0x%08x (ours %d, still listed %d)\n", (unsigned)o.sony_rc, o.ours,
            o.still_present);
    return 0;
}

static int op_appdb_row(void *ctx) {
    webui_tile_seen_t seen;
    (void)ctx;
    tile_look(&seen, "after uninstall");
    return seen.rows;
}

static int op_restage(void *ctx) {
    (void)ctx;
    fprintf(stderr, "webui tile: %s is listed without its files; restaging it to uninstall it\n",
            WEBUI_TILE_TITLE_ID);
    if (webui_tile_write_files(WEBUI_TILE_DIR, WEBUI_PORT) < 0) return -1;
    return tile_register();
}

static int op_hide(void *ctx) {
    char why[160] = {0};
    (void)ctx;
    if (content_db_icon_set_visible(WEBUI_TILE_TITLE_ID, 0, why, sizeof why) != 0) {
        fprintf(stderr, "webui tile: cannot hide the icon: %s\n", why);
        return -1;
    }
    icon_keeper_note(WEBUI_TILE_TITLE_ID, 1);
    fprintf(stderr, "webui tile: the console keeps %s; its icon is hidden\n", WEBUI_TILE_TITLE_ID);
    return 0;
}

static int op_delete_staged(void *ctx) {
    (void)ctx;
    (void)webui_tile_remove_files(TILE_APPMETA);
    if (webui_tile_remove_files(TILE_USER_APP) == 0) return 0;
    fprintf(stderr, "webui tile: cannot delete %s: %s\n", TILE_USER_APP, strerror(errno));
    return -1;
}

static int op_delete_dir(void *ctx) {
    (void)ctx;
    if (tile_mount_live()) {
        fprintf(stderr, "webui tile: a nullfs is still bound on %s; keeping it\n", WEBUI_TILE_DIR);
        return -1;
    }
    if (webui_tile_remove_files(WEBUI_TILE_DIR) == 0) return 0;
    fprintf(stderr, "webui tile: cannot delete %s: %s\n", WEBUI_TILE_DIR, strerror(errno));
    return -1;
}

static int op_unlink_marker(void *ctx) {
    (void)ctx;
    return unlink(WEBUI_TILE_MARKER) == 0 || errno == ENOENT ? 0 : -1;
}

/* The removal, and what it achieved for the page and the PC. */
static webui_tile_result_t tile_remove(void) {
    static const webui_tile_remove_ops_t ops = {
        NULL, op_staged, op_unregister, op_appdb_row, op_restage, op_hide, op_delete_staged, op_delete_dir,
        op_unlink_marker,
    };
    webui_tile_removal_t v = WEBUI_TILE_KEEP;
    int rc;
    pthread_mutex_lock(&g_op_mu);
    fprintf(stderr, "webui tile: removing %s\n", WEBUI_TILE_TITLE_ID);
    rc = webui_tile_remove_with(&ops, &v);
    /* Gone now; if the system lists it again later (app.db changes asynchronously), the icon keeper
     * hides it again at once, and the watch that follows fixes it. Turning the tile on undoes this. */
    if (v != WEBUI_TILE_KEEP) icon_keeper_note(WEBUI_TILE_TITLE_ID, 1);
    pthread_mutex_unlock(&g_op_mu);
    if (rc != 0) {
        fprintf(stderr, "webui tile: removal not complete; the tile's files are kept where needed\n");
        return WEBUI_TILE_RESULT_FAILED;
    }
    fprintf(stderr, "webui tile: removal done (%s)\n", v == WEBUI_TILE_HIDDEN ? "hidden" : "removed");
    return v == WEBUI_TILE_HIDDEN ? WEBUI_TILE_RESULT_HIDDEN : WEBUI_TILE_RESULT_REMOVED;
}

int webui_tile_remove(void) { return tile_remove() == WEBUI_TILE_RESULT_FAILED ? -1 : 0; }

/* ---- the sweep and the post-removal watch (#480) ---- */

/* The tile thread's request state; see webui_tile_request. */
static pthread_mutex_t g_req_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_req_done = PTHREAD_COND_INITIALIZER; /* g_busy went to 0, or the worker ended */
static pthread_cond_t g_req_new = PTHREAD_COND_INITIALIZER;  /* a request or sweep arrived */
static unsigned g_req_gen;
static int g_req_tile;
static uint16_t g_req_port;
static int g_worker_running;
static int g_busy;          /* a request or the start sweep is being applied */
static int g_sweep_pending; /* webui_tile_sweep_request not yet run */
static int g_last_failed;   /* the last ensure/remove the worker applied returned -1 */
static webui_tile_result_t g_last_result;
static const char *g_last_error = ""; /* webui_tile_last_error */

/* The sweep's context: the request generation it runs under (a newer one stops it). */
typedef struct {
    unsigned gen;
} sweep_ctx_t;

/* Read from the file each time: the settings code saves it before it asks for anything. With
 * Minion off the tile is not wanted, whatever its own setting says. */
static int sw_tile_on(void *ctx) {
    webui_settings_t ws;
    (void)ctx;
    (void)webui_settings_load(WEBUI_SETTINGS_PATH, &ws);
    return webui_settings_tile_wanted(&ws);
}

static int sw_look(void *ctx, webui_tile_seen_t *seen) {
    (void)ctx;
    tile_look(seen, "check");
    return 0;
}

static int sw_registered(void *ctx) {
    (void)ctx;
    return tile_mount_live() && tile_lnk_ours();
}

static int sw_leftovers(void *ctx) {
    (void)ctx;
    return is_dir(TILE_USER_APP) || is_dir(TILE_APPMETA) || is_dir(WEBUI_TILE_DIR) ||
           access(WEBUI_TILE_MARKER, F_OK) == 0;
}

static int sw_uninstall(void *ctx) {
    const char *err = NULL;
    unregister_outcome_t o;
    int rc;
    (void)ctx;
    pthread_mutex_lock(&g_op_mu);
    fprintf(stderr, "webui tile: %s is listed but not registered and its files are gone; asking Sony to uninstall it\n",
            WEBUI_TILE_TITLE_ID);
    rc = unregister_title(WEBUI_TILE_TITLE_ID, &err, &o);
    if (rc != 0) fprintf(stderr, "webui tile: uninstall failed: %s\n", err ? err : "unknown");
    else fprintf(stderr, "webui tile: uninstall rc=0x%08x\n", (unsigned)o.sony_rc);
    pthread_mutex_unlock(&g_op_mu);
    return rc;
}

static int sw_hide(void *ctx) {
    int rc;
    pthread_mutex_lock(&g_op_mu);
    rc = op_hide(ctx);
    pthread_mutex_unlock(&g_op_mu);
    return rc;
}

static void sw_keep_hidden(void *ctx) {
    (void)ctx;
    icon_keeper_note(WEBUI_TILE_TITLE_ID, 1);
}

static int sw_remove(void *ctx) {
    (void)ctx;
    fprintf(stderr, "webui tile: the tile is off but %s is still there; finishing its removal\n",
            WEBUI_TILE_TITLE_ID);
    return webui_tile_remove();
}

static int sw_clean(void *ctx) {
    int rc = 0;
    (void)ctx;
    pthread_mutex_lock(&g_op_mu);
    fprintf(stderr, "webui tile: app.db no longer lists %s; deleting what was left of it\n", WEBUI_TILE_TITLE_ID);
    if (op_delete_staged(NULL) != 0) rc = -1;
    if (op_delete_dir(NULL) != 0) rc = -1;
    if (op_unlink_marker(NULL) != 0) rc = -1;
    pthread_mutex_unlock(&g_op_mu);
    return rc;
}

/* Sleeps `seconds`; returns 1 at once when a tile request newer than the sweep's arrives. */
static int sw_wait(void *ctx, unsigned seconds) {
    const sweep_ctx_t *c = ctx;
    struct timespec until;
    int newer;
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_sec += (time_t)seconds;
    pthread_mutex_lock(&g_req_mu);
    while (g_req_gen == c->gen) {
        if (pthread_cond_timedwait(&g_req_new, &g_req_mu, &until) == ETIMEDOUT) break;
    }
    newer = g_req_gen != c->gen;
    pthread_mutex_unlock(&g_req_mu);
    /* The wait above is on the wall clock, which jumps ahead on a wake and ends it at once. Never
     * the next sweep (app.db, Sony register/uninstall) while the console goes to rest or settles
     * after a wake (power_phase.h). */
    while (!newer && power_phase_quiet()) {
        sleep(1);
        pthread_mutex_lock(&g_req_mu);
        newer = g_req_gen != c->gen;
        pthread_mutex_unlock(&g_req_mu);
    }
    return newer;
}

static void sweep_ops(webui_tile_sweep_ops_t *o, sweep_ctx_t *c) {
    o->ctx = c;
    o->tile_on = sw_tile_on;
    o->look = sw_look;
    o->registered = sw_registered;
    o->leftovers = sw_leftovers;
    o->uninstall = sw_uninstall;
    o->hide = sw_hide;
    o->keep_hidden = sw_keep_hidden;
    o->remove = sw_remove;
    o->clean = sw_clean;
    o->wait = sw_wait;
}

/* Caller holds g_req_mu. */
static void note_result(webui_tile_result_t r) {
    if (r == WEBUI_TILE_RESULT_NONE) return;
    g_last_result = r;
    g_last_failed = r == WEBUI_TILE_RESULT_FAILED;
    /* The request path sets its own reason right after; the sweep and the watch have this one. */
    g_last_error = g_last_failed ? "icon_still_shown" : "";
}

/* One worker at a time: it applies the newest request (repeating while new ones arrive), then a
 * pending start sweep, then, after a removal or a sweep, watches app.db for a while
 * (webui_tile_settle_with) until a new request stops it. g_busy covers the first two only: the
 * page's spinner ends when the removal is done, not when the watch is. */
static void *tile_worker(void *arg) {
    unsigned done = 0;
    int watch = 0, uninstalled = 0;
    (void)arg;
    proc_name_set_self(TILE_THREAD_NAME);
    pthread_mutex_lock(&g_req_mu);
    for (;;) {
        unsigned gen = g_req_gen;
        int tile = g_req_tile;
        uint16_t port = g_req_port;
        webui_tile_result_t r;
        if (done != gen) {
            const char *why = "";
            g_busy = 1;
            g_sweep_pending = 0; /* a request decides the tile now */
            pthread_mutex_unlock(&g_req_mu);
            if (tile) r = webui_tile_ensure(port, &why) == 0 ? WEBUI_TILE_RESULT_ADDED : WEBUI_TILE_RESULT_FAILED;
            else r = tile_remove();
            if (r == WEBUI_TILE_RESULT_FAILED && !tile) why = "removal_incomplete";
            pthread_mutex_lock(&g_req_mu);
            done = gen;
            note_result(r);
            g_last_error = r == WEBUI_TILE_RESULT_FAILED ? (why && *why ? why : "failed") : "";
            watch = !tile;
            uninstalled = 0;
            continue;
        }
        if (g_sweep_pending) {
            sweep_ctx_t c = { done };
            webui_tile_sweep_ops_t o;
            g_sweep_pending = 0;
            g_busy = 1;
            pthread_mutex_unlock(&g_req_mu);
            sweep_ops(&o, &c);
            uninstalled = 0;
            r = webui_tile_sweep_once(&o, 1, &uninstalled);
            pthread_mutex_lock(&g_req_mu);
            if (g_req_gen == done) note_result(r);
            watch = 1;
            continue;
        }
        g_busy = 0;
        pthread_cond_broadcast(&g_req_done);
        if (!watch) break;
        watch = 0;
        {
            sweep_ctx_t c = { done };
            webui_tile_sweep_ops_t o;
            pthread_mutex_unlock(&g_req_mu);
            sweep_ops(&o, &c);
            fprintf(stderr, "webui tile: watching app.db for %s for a while\n", WEBUI_TILE_TITLE_ID);
            r = webui_tile_settle_with(&o, &uninstalled);
            pthread_mutex_lock(&g_req_mu);
            if (g_req_gen == done) {
                note_result(r);
                fprintf(stderr, "webui tile: watch ended (%s)\n",
                        r == WEBUI_TILE_RESULT_NONE ? "nothing to fix" : webui_tile_result_name(r));
            }
        }
    }
    g_worker_running = 0;
    g_busy = 0;
    pthread_cond_broadcast(&g_req_done);
    pthread_mutex_unlock(&g_req_mu);
    return NULL;
}

const char *webui_tile_last_error(void) {
    const char *e;
    pthread_mutex_lock(&g_req_mu);
    e = g_last_failed ? g_last_error : "";
    pthread_mutex_unlock(&g_req_mu);
    return e;
}

void webui_tile_state(int *busy, int *failed, webui_tile_result_t *result) {
    pthread_mutex_lock(&g_req_mu);
    *busy = g_busy;
    *failed = g_last_failed;
    if (result) *result = g_last_result;
    pthread_mutex_unlock(&g_req_mu);
}

/* Caller holds g_req_mu: wake the running worker, or start one. */
static void kick_worker(void) {
    pthread_attr_t attr;
    pthread_t t;
    int rc = -1;
    g_busy = 1;
    if (g_worker_running) {
        pthread_cond_broadcast(&g_req_new); /* the running worker (maybe watching) picks it up */
        return;
    }
    if (pthread_attr_init(&attr) == 0) {
        if (pthread_attr_setstacksize(&attr, TILE_STACK) == 0 &&
            pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0)
            rc = pthread_create(&t, &attr, tile_worker, NULL);
        pthread_attr_destroy(&attr);
    }
    if (rc == 0) g_worker_running = 1;
    else {
        g_busy = 0;
        g_last_failed = 1;
        g_last_result = WEBUI_TILE_RESULT_FAILED;
        g_last_error = "no_tile_thread";
        pthread_cond_broadcast(&g_req_done);
        fprintf(stderr, "webui tile: cannot start the tile thread (%d)\n", rc);
    }
}

unsigned webui_tile_request(int tile, uint16_t port) {
    unsigned gen;
    pthread_mutex_lock(&g_req_mu);
    g_req_gen++;
    if (g_req_gen == 0) g_req_gen = 1; /* 0 is the worker's "nothing applied yet" */
    g_req_tile = tile ? 1 : 0;
    g_req_port = port;
    gen = g_req_gen;
    kick_worker();
    pthread_mutex_unlock(&g_req_mu);
    return gen;
}

void webui_tile_sweep_request(void) {
    pthread_mutex_lock(&g_req_mu);
    g_sweep_pending = 1;
    kick_worker();
    pthread_mutex_unlock(&g_req_mu);
}

/* The PC uninstall ACK means completed, unlike the Settings endpoint's explicit busy state. */
int webui_tile_remove_wait(unsigned mine) {
    struct timespec until;
    int rc;
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_sec += WEBUI_TILE_WAIT_S;
    pthread_mutex_lock(&g_req_mu);
    while (g_busy && g_req_gen == mine) {
        if (pthread_cond_timedwait(&g_req_done, &g_req_mu, &until) == ETIMEDOUT) break;
    }
    rc = g_busy || g_last_failed || g_req_tile != 0 || g_req_gen != mine ? -1 : 0;
    if (g_busy) fprintf(stderr, "webui tile: removal still running after %d s\n", WEBUI_TILE_WAIT_S);
    pthread_mutex_unlock(&g_req_mu);
    return rc;
}

#endif /* WEBUI_TILE_HOST_TEST */
