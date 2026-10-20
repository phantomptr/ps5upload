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

webui_tile_action_t webui_tile_plan(int files_changed, int mount_live, int lnk_ours, int appmeta) {
    if (files_changed) return WEBUI_TILE_REGISTER;
    if (mount_live && lnk_ours) return WEBUI_TILE_NONE;
    /* A live mount that is not ours, or a half-removed registration: only the full register
     * (which pre-cleans the mount point and restages everything) puts it right. */
    if (!mount_live && lnk_ours && appmeta) return WEBUI_TILE_REBIND;
    return WEBUI_TILE_REGISTER;
}

#ifndef WEBUI_TILE_HOST_TEST

#include <pthread.h>
#include <sys/mount.h>
#include <sys/param.h>

#include "proc_identity.h"
#include "register.h"

#define TILE_STACK (256u * 1024u)
#define TILE_THREAD_NAME "ps5upload-tile"
#define TILE_SYSTEM_EX "/system_ex/app/" WEBUI_TILE_TITLE_ID
#define TILE_MOUNT_LNK "/user/app/" WEBUI_TILE_TITLE_ID "/mount.lnk"

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

/* /user/app/PSUP00001/mount.lnk (written by register_title_from_path) names WEBUI_TILE_DIR. */
static int tile_lnk_ours(void) {
    char lnk[256];
    ssize_t n;
    int fd = open(TILE_MOUNT_LNK, O_RDONLY);
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

static int tile_register(void) {
    char tid[REGISTER_MAX_TITLE_ID], tname[REGISTER_MAX_TITLE_NAME];
    const char *err = NULL;
    int used = 0;
    if (register_title_from_path(WEBUI_TILE_DIR, 0, tid, tname, &used, &err) != 0) {
        fprintf(stderr, "webui tile: full register failed: %s\n", err ? err : "unknown");
        return -1;
    }
    fprintf(stderr, "webui tile: full register of %s done\n", tid);
    return 0;
}

int webui_tile_ensure(uint16_t port) {
    const char *err = NULL;
    int changed, rc = 0;
    pthread_mutex_lock(&g_op_mu);
    changed = webui_tile_write_files(WEBUI_TILE_DIR, port);
    if (changed < 0) {
        fprintf(stderr, "webui tile: cannot write %s: %s\n", WEBUI_TILE_DIR, strerror(errno));
        rc = -1;
    } else {
        switch (webui_tile_plan(changed > 0, tile_mount_live(), tile_lnk_ours(), is_dir(TILE_APPMETA))) {
        case WEBUI_TILE_NONE:
            fprintf(stderr, "webui tile: already registered this boot\n");
            break;
        case WEBUI_TILE_REBIND:
            /* Still registered with Sony from an earlier boot: re-mount only, no Sony call. */
            if (register_remount_nullfs(WEBUI_TILE_TITLE_ID, WEBUI_TILE_DIR, &err) == 0) {
                fprintf(stderr, "webui tile: re-bound the nullfs (no Sony call)\n");
            } else {
                fprintf(stderr, "webui tile: re-bind failed (%s); falling back to a full register\n",
                        err ? err : "unknown");
                rc = tile_register();
            }
            break;
        default:
            /* A changed file (register copies sce_sys into /user/app and /user/appmeta, so the copies
             * must be refreshed; Sony answers 0x80990002 "restored": success), or never registered. */
            rc = tile_register();
            break;
        }
    }
    pthread_mutex_unlock(&g_op_mu);
    return rc;
}

int webui_tile_remove(void) {
    const char *err = NULL;
    unregister_outcome_t outcome;
    int rc = 0;
    pthread_mutex_lock(&g_op_mu);
    /* The nullfs must be gone before the folder under it is deleted: on an unregister failure the
     * folders stay (a tile pointing at nothing is worse than a tile that still works). */
    if (unregister_title(WEBUI_TILE_TITLE_ID, &err, &outcome) != 0) {
        fprintf(stderr, "webui tile: unregister failed: %s; keeping %s\n", err ? err : "unknown", WEBUI_TILE_DIR);
        rc = -1;
    } else {
        if (!unregister_tile_confirmed(&outcome)) {
            fprintf(stderr, "webui tile: removal unconfirmed rc=0x%08x; keeping recovery files\n",
                    (unsigned)outcome.sony_rc);
            pthread_mutex_unlock(&g_op_mu);
            return -1;
        }
        /* unregister_title keeps the staged /user/app/PSUP00001 (sce_sys, icon, mount.lnk). On FW 13.x
         * register goes through AppInstallAll, which installs everything staged there: left behind,
         * the next folder-game register would bring back the tile the user removed. */
        if (webui_tile_remove_files(TILE_USER_APP) != 0) {
            fprintf(stderr, "webui tile: cannot delete %s: %s\n", TILE_USER_APP, strerror(errno));
            rc = -1;
        }
        if (webui_tile_remove_files(WEBUI_TILE_DIR) != 0) {
            fprintf(stderr, "webui tile: cannot delete %s: %s\n", WEBUI_TILE_DIR, strerror(errno));
            rc = -1;
        }
    }
    pthread_mutex_unlock(&g_op_mu);
    return rc;
}

/* One worker at a time applies the latest request: g_req_gen counts requests, the worker loops
 * until it has applied the newest one. */
static pthread_mutex_t g_req_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_req_done = PTHREAD_COND_INITIALIZER;
static unsigned g_req_gen;
static int g_req_tile;
static uint16_t g_req_port;
static int g_worker_running;
static int g_last_failed; /* the last ensure/remove the worker applied returned -1 */

static void *tile_worker(void *arg) {
    unsigned done = 0;
    (void)arg;
    proc_name_set_self(TILE_THREAD_NAME);
    pthread_mutex_lock(&g_req_mu);
    for (;;) {
        unsigned gen = g_req_gen;
        int tile = g_req_tile;
        uint16_t port = g_req_port;
        if (done == gen) break;
        pthread_mutex_unlock(&g_req_mu);
        int rc = tile ? webui_tile_ensure(port) : webui_tile_remove();
        done = gen;
        pthread_mutex_lock(&g_req_mu);
        g_last_failed = rc != 0;
    }
    g_worker_running = 0;
    pthread_cond_broadcast(&g_req_done);
    pthread_mutex_unlock(&g_req_mu);
    return NULL;
}

void webui_tile_state(int *busy, int *failed) {
    pthread_mutex_lock(&g_req_mu);
    *busy = g_worker_running;
    *failed = g_last_failed;
    pthread_mutex_unlock(&g_req_mu);
}

void webui_tile_request(int tile, uint16_t port) {
    pthread_attr_t attr;
    pthread_t t;
    int rc = -1;
    pthread_mutex_lock(&g_req_mu);
    g_req_gen++;
    if (g_req_gen == 0) g_req_gen = 1; /* 0 is the worker's "nothing applied yet" */
    g_req_tile = tile ? 1 : 0;
    g_req_port = port;
    if (g_worker_running) {
        pthread_mutex_unlock(&g_req_mu); /* the running worker picks this request up */
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
        g_last_failed = 1;
        fprintf(stderr, "webui tile: cannot start the tile thread (%d)\n", rc);
    }
    pthread_mutex_unlock(&g_req_mu);
}

/* The PC uninstall ACK means completed, unlike the Settings endpoint's explicit busy state.
 * Called with the settings mutex held, so another toggle cannot supersede this request. */
int webui_tile_remove_wait(uint16_t port) {
    webui_tile_request(0, port);
    pthread_mutex_lock(&g_req_mu);
    while (g_worker_running) pthread_cond_wait(&g_req_done, &g_req_mu);
    int rc = g_last_failed || g_req_tile != 0 ? -1 : 0;
    pthread_mutex_unlock(&g_req_mu);
    return rc;
}

#endif /* WEBUI_TILE_HOST_TEST */
