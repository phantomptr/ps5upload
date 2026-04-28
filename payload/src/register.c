/*
 * App registration pipeline for ps5upload.
 *
 * Stages sce_sys into /user/app/<title_id>/, nullfs-binds the source
 * at /system_ex/app/<title_id>/, calls Sony's installer API, and
 * offers a launch capability via sceLncUtilLaunchApp.
 *
 * See include/register.h for the public contract. The rest of this
 * file is implementation detail.
 */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>
#include <unistd.h>
#include <dlfcn.h>
#include <dirent.h>
#include <fcntl.h>
#include <pthread.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/param.h>
#include <sys/mount.h>
#include <sys/uio.h>
#include <sys/sysctl.h>

#include "register.h"

/* Serializes ALL calls into Sony's install/launch/uninstall APIs across
 * threads. On firmware 9.60, `sceAppInstUtilAppInstallTitleDir` and
 * `sceLncUtilLaunchApp` appear to hold internal locks briefly on
 * return, and concurrent calls from multiple HTTP handler threads
 * can hit Sony's kernel stubs while those locks are still held,
 * deadlocking the calling thread. We force one-at-a-time access
 * regardless of which HTTP handler received the request.
 *
 * Held in tandem with a 200ms post-call usleep so the kernel stub
 * has time to release its internal locks before the next caller
 * enters. */
static pthread_mutex_t g_sony_api_mtx = PTHREAD_MUTEX_INITIALIZER;

/* Post-call grace period — 200ms. Lets Sony's kernel stub release
 * internal locks before the next install/launch can proceed.
 * usleep is FreeBSD-native and doesn't require a dlsym on
 * sceKernelUsleep. */
#define SONY_API_POST_SLEEP_US 200000u

/* -- Mount / install layout ----------------------------------------------
 *
 * Paths match what Sony's installer expects -- deviating would mean
 * the installer would not find the metadata (which it looks up by well-known
 * path) and the XMB tile would be broken even on a successful install.
 */
#define REG_APP_BASE      "/user/app"
#define REG_APPMETA_BASE  "/user/appmeta"
#define REG_SYSTEM_EX_APP "/system_ex/app"
#define REG_APP_DB_PATH   "/system_data/priv/mms/app.db"

/* Sony library paths, resolved lazily via dlopen. Kept here (not in
 * register.h) because they are implementation detail. */
#define REG_LIB_APPINSTUTIL "/system/common/lib/libSceAppInstUtil.sprx"
#define REG_LIB_LNCSERVICE  "/system/common/lib/libSceLncService.sprx"
#define REG_LIB_SYSTEMSVC   "/system/common/lib/libSceSystemService.sprx"
#define REG_LIB_USERSVC     "/system/common/lib/libSceUserService.sprx"
#define REG_LIB_SQLITE3     "/system/common/lib/libSceSqlite.sprx"

#define REG_MAX_PATH 512

/* -- Cached function pointers resolved at module init ------------------
 *
 * Sony APIs are looked up lazily via dlopen + dlsym on the first
 * APP_* request (via pthread_once in register_module_init). We do
 * NOT link against -lSceAppInstUtil / -lSceSystemService / etc —
 * those cause the PS5 rtld to load those sprxes at payload-load
 * time, which on some firmware / loader combinations hangs, and
 * the payload never comes up.
 *
 * Loading lazily has three upsides:
 *   1. Payload boots reliably even if a sprx is slow to init
 *   2. Missing symbols degrade the affected command to an error
 *      frame, not a whole-payload crash
 *   3. Firmwares that have renamed / moved a symbol simply return
 *      NULL from dlsym — same graceful path as "not supported"
 */
typedef int (*app_install_title_dir_fn_t)(const char *, const char *, void *);
/* Sony's uninstall export is spelled "UnInstall" (capital I), not
 * "Uninstall". Earlier code called the non-existent
 * "sceAppInstUtilAppUnInstallTitle" — that typo silently returned
 * NULL from dlsym and made uninstall a no-op. Correct export
 * takes 3 args (we pass NULL for the trailing two). */
typedef int (*app_uninstall_fn_t)(const char *, int *, int *);
typedef int (*app_inst_util_initialize_fn_t)(void);
/* PS5 layout: second param is `char *argv`, third is a caller-
 * owned LncAppParam block (first 4 bytes = struct size). */
typedef int (*launch_app_fn_t)(const char *, char *, void *);
typedef int (*launch_app_sys_fn_t)(const char *, const char **, void *);
typedef int (*lnc_util_initialize_fn_t)(void);
typedef int (*user_service_initialize_fn_t)(int *);
typedef int (*user_service_get_foreground_user_fn_t)(int *);

/* SQLite subset -- just the minimum for a single-statement SELECT.
 * If libsqlite3 is unavailable (renamed on a future firmware, not
 * loaded), list_registered_titles degrades to "sqlite_unavailable"
 * and register/unregister/launch all continue to work -- the
 * snd0info patch and the "installed titles" Library filter are the
 * only features that need it. */
typedef struct sqlite3      sqlite3;
typedef struct sqlite3_stmt sqlite3_stmt;

typedef int  (*sqlite3_open_v2_fn)(const char *, sqlite3 **, int, const char *);
typedef int  (*sqlite3_close_fn)(sqlite3 *);
typedef int  (*sqlite3_busy_timeout_fn)(sqlite3 *, int);
typedef int  (*sqlite3_prepare_v2_fn)(sqlite3 *, const char *, int,
                                      sqlite3_stmt **, const char **);
typedef int  (*sqlite3_step_fn)(sqlite3_stmt *);
typedef int  (*sqlite3_finalize_fn)(sqlite3_stmt *);
typedef int  (*sqlite3_bind_text_fn)(sqlite3_stmt *, int, const char *, int,
                                     void (*)(void *));
typedef const unsigned char *(*sqlite3_column_text_fn)(sqlite3_stmt *, int);
typedef int  (*sqlite3_changes_fn)(sqlite3 *);

#define SQLITE_OK     0
#define SQLITE_ROW    100
#define SQLITE_DONE   101
#define SQLITE_OPEN_READWRITE 0x00000002
#define SQLITE_OPEN_READONLY  0x00000001

/* SQLITE_TRANSIENT is a sentinel sqlite3_bind_text accepts in place of
 * a real destructor: "make a private copy of the buffer". The actual
 * value is ((void(*)(void*))-1); declaring it as a cast-to-fn-pointer
 * lets us pass it through our typedef without a linker-visible stub. */
#define REG_SQLITE_TRANSIENT ((void (*)(void *))-1)

typedef struct {
    int   firmware_major;   /* e.g. 9 for 9.60; 0 if unknown */
    /* AppInstUtil -- registration */
    app_install_title_dir_fn_t    app_install_title_dir;
    app_uninstall_fn_t            app_uninstall;   /* may be NULL */
    app_inst_util_initialize_fn_t app_inst_util_initialize;  /* may be NULL */
    /* Lnc + SystemService -- launch (triple-strategy) */
    launch_app_fn_t          launch_app;
    launch_app_sys_fn_t      launch_app_sys;
    lnc_util_initialize_fn_t lnc_util_initialize;
    /* UserService -- foreground user for launch context */
    user_service_initialize_fn_t           user_service_initialize;
    user_service_get_foreground_user_fn_t  user_service_get_foreground_user;
    /* SQLite -- app.db */
    sqlite3_open_v2_fn      sq_open_v2;
    sqlite3_close_fn        sq_close;
    sqlite3_busy_timeout_fn sq_busy_timeout;
    sqlite3_prepare_v2_fn   sq_prepare_v2;
    sqlite3_step_fn         sq_step;
    sqlite3_finalize_fn     sq_finalize;
    sqlite3_bind_text_fn    sq_bind_text;
    sqlite3_column_text_fn  sq_column_text;
    sqlite3_changes_fn      sq_changes;
} reg_module_t;

static reg_module_t g_reg = {0};
/* pthread_once guarantees the module init body runs exactly once even
 * when multiple mgmt threads call register_module_init concurrently.
 * Without it, the "check resolved, set to 1, do dlopens" pattern had
 * a TOCTOU window where thread B could see resolved==1 before thread
 * A finished resolving fn pointers -- leading to spurious "service
 * unavailable" errors mid-init. */
#include <pthread.h>
static pthread_once_t g_reg_once = PTHREAD_ONCE_INIT;

/* -- Firmware-major parse ---------------------------------------------- */

/* Extract the leading NN of NN.MM from `kern.version`. We keep this
 * minimal because the only consumer here is the firmware-12 gate on
 * sceAppInstUtilAppInstallTitleDir.
 *
 * Two parse tiers:
 *   1. "releases/NN" -- the canonical build-branch tag on production
 *      firmwares. Preferred because it's unambiguous.
 *   2. Bare NN.NN substring -- covers firmware strings that omit the
 *      "releases/" prefix (some sandboxed debug configurations). We
 *      skip any leading "11.0" since FreeBSD's OS major (11) would
 *      otherwise be mistaken for PS5 firmware 11.x.
 *
 * Returns 0 if neither matches, which errs toward "attempt the install"
 * -- a real 12+ console will fail at dlsym anyway and surface a clean
 * error. Matches the shape of the client-side parsePS5Firmware tiers. */
static int parse_firmware_major_from_kern_version(const char *kv) {
    if (!kv) return 0;
    const char *p = strstr(kv, "releases/");
    if (p) {
        p += strlen("releases/");
        int major = 0;
        int consumed = 0;
        while (*p >= '0' && *p <= '9') {
            major = major * 10 + (*p - '0');
            consumed++;
            p++;
        }
        if (consumed > 0) return major;
    }
    /* Fallback: first NN.NN after the FreeBSD "11.0" prefix. */
    const char *scan = kv;
    if (strncmp(scan, "FreeBSD 11.0", 12) == 0) scan += 12;
    while (*scan) {
        if (scan[0] >= '0' && scan[0] <= '9') {
            int major = 0;
            int consumed = 0;
            const char *q = scan;
            while (*q >= '0' && *q <= '9') {
                major = major * 10 + (*q - '0');
                consumed++;
                q++;
            }
            /* Require NN.NN shape to avoid matching an arbitrary number. */
            if (consumed > 0 && *q == '.' &&
                q[1] >= '0' && q[1] <= '9' && q[2] >= '0' && q[2] <= '9' &&
                (q[3] < '0' || q[3] > '9')) {
                return major;
            }
            scan = q;
        }
        scan++;
    }
    return 0;
}

static int detect_firmware_major(void) {
    char buf[256];
    size_t sz = sizeof(buf);
    if (sysctlbyname("kern.version", buf, &sz, NULL, 0) != 0) return 0;
    buf[sizeof(buf) - 1] = '\0';
    return parse_firmware_major_from_kern_version(buf);
}

/* -- dlopen / dlsym helpers --------------------------------------------
 *
 * Load a Sony .sprx into the process lazily so its symbols become
 * visible via RTLD_DEFAULT. We never dlclose — the lib stays loaded
 * for the payload's lifetime.
 */
static void reg_dlopen_sideload(const char *path) {
    dlerror();
    void *h = dlopen(path, RTLD_LAZY);
    if (!h) {
        const char *err = dlerror();
        fprintf(stderr, "[register] dlopen %s failed: %s\n",
                path, err ? err : "unknown");
    }
    /* Intentionally leak the handle — the sprx should stay loaded. */
}

static void *reg_dlsym_default(const char *sym) {
    dlerror();
    void *p = dlsym(RTLD_DEFAULT, sym);
    if (!p) (void)dlerror();
    return p;
}

/* -- Public module init / shutdown -------------------------------------
 *
 * First-touch lazy init: pthread_once guarantees this runs exactly
 * once even if multiple mgmt threads hit an APP_* handler
 * concurrently. Previous approach was link-time imports (-lSce*)
 * which made the rtld load all sprxes at payload boot — that
 * caused some firmware/loader combinations to wedge before main().
 * Going back to runtime dlopen/dlsym keeps the payload-boot path
 * free of Sony-library initializers; the cost is a one-time
 * ~100ms latency hit on the first install/launch/uninstall call.
 */

static void register_module_init_impl(void) {
    g_reg.firmware_major = detect_firmware_major();

    /* AppInstUtil: explicit dlopen because its exports aren't
     * automatically in RTLD_DEFAULT's scope.
     *
     * We try the dlopen on every firmware and rely on graceful
     * symbol-NULL fallback further down rather than hard-gating on
     * firmware major. Reasons:
     *
     *   - Sony renamed some install-API symbols in 12.x. An old gate
     *     of `firmware_major < 12` meant "don't even try on 12+", but
     *     dlopen itself is cheap and `dlsym(NULL)` already does the
     *     right thing — an install call just fails cleanly with
     *     "service_unavailable" when symbols are absent. Hard-gating
     *     blocks the path where Sony brings a renamed API back, or
     *     where a patched kernel/sprx restores the old names.
     *
     *   - The long-standing "dlopen hangs the mgmt thread on some
     *     firmwares" concern is handled by the eager-init pattern
     *     (see `register_services_init` below) which resolves these
     *     symbols on the main thread at startup, not inside a mgmt
     *     handler.
     *
     * Net: this runs on every firmware the SDK supports (1.00–12.70).
     * Install requests fail cleanly on any firmware where the API
     * isn't actually exported. */
    reg_dlopen_sideload(REG_LIB_APPINSTUTIL);
    g_reg.app_install_title_dir = (app_install_title_dir_fn_t)
        reg_dlsym_default("sceAppInstUtilAppInstallTitleDir");
    /* Correct name — capital I in UnInstall. The old code used
     * sceAppInstUtilAppUnInstallTitle (with "Title" suffix)
     * which doesn't exist → uninstall was silently a no-op. */
    g_reg.app_uninstall = (app_uninstall_fn_t)
        reg_dlsym_default("sceAppInstUtilAppUnInstall");
    g_reg.app_inst_util_initialize = (app_inst_util_initialize_fn_t)
        reg_dlsym_default("sceAppInstUtilInitialize");

    /* Lnc / SystemService / UserService: the SDK's kernel_web auto-
     * link brings these into RTLD_DEFAULT's scope without us having
     * to dlopen their specific sprxes. Dlopening them explicitly
     * was observed to hang the mgmt thread on some firmwares, so
     * we rely on the auto-linked versions only. */
    g_reg.launch_app = (launch_app_fn_t)
        reg_dlsym_default("sceLncUtilLaunchApp");
    g_reg.lnc_util_initialize = (lnc_util_initialize_fn_t)
        reg_dlsym_default("sceLncUtilInitialize");
    g_reg.launch_app_sys = (launch_app_sys_fn_t)
        reg_dlsym_default("sceSystemServiceLaunchApp");
    g_reg.user_service_initialize = (user_service_initialize_fn_t)
        reg_dlsym_default("sceUserServiceInitialize");
    g_reg.user_service_get_foreground_user = (user_service_get_foreground_user_fn_t)
        reg_dlsym_default("sceUserServiceGetForegroundUser");

}

void register_module_init(void) {
    pthread_once(&g_reg_once, register_module_init_impl);
}

/* Eager Sony-service init — intended to be called from main() on the
 * main thread, BEFORE the HTTP listener starts. See register.h for
 * the full rationale. Idempotent: pthread_once guarantees the impl
 * runs exactly once; the Sony API calls themselves are documented
 * idempotent by Sony, so re-entry (e.g., if a caller accidentally
 * invokes this from a handler) is safe.
 *
 * Not instrumented with a timeout because these Sony init functions
 * are observed to complete promptly (<100ms) when called from the
 * main thread at startup. If they ever hang at startup, the payload
 * won't come up at all — which is a louder and more recoverable
 * failure mode than the silent "install wedges 30s after user clicks
 * Install" we saw pre-fix. */
void register_services_init(void) {
    /* Resolve symbols first (lazy init path also does this; calling
     * it here just pulls the work to main-thread context so every
     * Sony symbol is already bound before any handler touches it). */
    register_module_init();

    /* Lock the serialization mutex so this first-ever init is
     * guaranteed to run before any HTTP handler enters the install
     * path. Not strictly required if main.c calls us before the
     * listener spawns, but defense-in-depth for anyone who refactors
     * main.c later. */
    pthread_mutex_lock(&g_sony_api_mtx);

    if (g_reg.user_service_initialize) {
        /* sceUserServiceInitialize takes an optional pointer to init
         * parameters; NULL = defaults. */
        (void)g_reg.user_service_initialize(NULL);
    }
    if (g_reg.app_inst_util_initialize) {
        (void)g_reg.app_inst_util_initialize();
    }
    if (g_reg.lnc_util_initialize) {
        /* Eagerly init LncUtil from main-thread context so
         * launch_title sees a ready-to-use service. */
        (void)g_reg.lnc_util_initialize();
    }

    pthread_mutex_unlock(&g_sony_api_mtx);
}

/* -- Filesystem helpers ------------------------------------------------ */

static int path_exists(const char *path) {
    struct stat st;
    return (stat(path, &st) == 0) ? 1 : 0;
}

static int is_safe_component(const char *name) {
    if (!name || name[0] == '\0') return 0;
    if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) return 0;
    for (const char *p = name; *p; p++) {
        if (*p == '/') return 0;
    }
    return 1;
}

/* mkdir -p, tolerant of EEXIST. Creates 0755. Returns 0 on success. */
static int mkdir_p(const char *path) {
    char buf[REG_MAX_PATH];
    size_t n = strlen(path);
    if (n == 0 || n + 1 > sizeof(buf)) return -1;
    memcpy(buf, path, n + 1);
    for (size_t i = 1; i < n; i++) {
        if (buf[i] == '/') {
            buf[i] = '\0';
            if (mkdir(buf, 0755) != 0 && errno != EEXIST) return -1;
            buf[i] = '/';
        }
    }
    if (mkdir(buf, 0755) != 0 && errno != EEXIST) return -1;
    return 0;
}

/* Simple byte-for-byte file copy. Modes are preserved via the fstat-
 * then-fchmod pattern. Returns 0 on success, -1 on any error. Used for
 * the sce_sys / appmeta staging step -- these files are small (~MiB
 * max per game metadata set).
 *
 * The I/O buffer is heap-allocated (64 KiB) rather than on the stack.
 * PS5 pthread default stacks are tight, and copy_file_bytes is called
 * from inside copy_dir_recursive (which ALREADY has two 512-byte path
 * buffers on its stack per level). A 64 KiB stack buffer on top would
 * blow the thread stack and crash the payload -- observed empirically
 * when register_title_from_path was first exercised. */
static int copy_file_bytes(const char *src, const char *dst) {
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0) return -1;
    struct stat st;
    if (fstat(in_fd, &st) != 0) { close(in_fd); return -1; }
    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, st.st_mode & 0777);
    if (out_fd < 0) { close(in_fd); return -1; }

    /* 64 KiB heap buffer -- large enough to amortize syscall overhead,
     * small enough to keep peak memory low when copying many files. */
    const size_t BUF_SZ = 64 * 1024;
    char *buf = (char *)malloc(BUF_SZ);
    if (!buf) { close(in_fd); close(out_fd); return -1; }
    ssize_t n;
    int ok = 0;
    while ((n = read(in_fd, buf, BUF_SZ)) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out_fd, buf + off, (size_t)(n - off));
            if (w < 0) { if (errno == EINTR) continue; ok = -1; goto done; }
            if (w == 0) { ok = -1; goto done; }  /* no progress -- bail */
            off += w;
        }
    }
    if (n < 0) ok = -1;
done:
    free(buf);
    close(in_fd);
    close(out_fd);
    return ok;
}

/* Recursive directory copy. Used for sce_sys staging. Bounded by the
 * target folder's actual size -- a game's sce_sys rarely exceeds a few
 * hundred KiB. */
static int copy_dir_recursive(const char *src, const char *dst) {
    DIR *d = opendir(src);
    if (!d) return -1;
    if (mkdir_p(dst) != 0) { closedir(d); return -1; }

    struct dirent *e;
    int rc = 0;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        char sp[REG_MAX_PATH], dp[REG_MAX_PATH];
        int n = snprintf(sp, sizeof(sp), "%s/%s", src, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sp)) { rc = -1; break; }
        n = snprintf(dp, sizeof(dp), "%s/%s", dst, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(dp)) { rc = -1; break; }
        struct stat st;
        if (stat(sp, &st) != 0) { rc = -1; break; }
        if (S_ISDIR(st.st_mode)) {
            if (copy_dir_recursive(sp, dp) != 0) { rc = -1; break; }
        } else if (S_ISREG(st.st_mode)) {
            if (copy_file_bytes(sp, dp) != 0) { rc = -1; break; }
        }
        /* Symlinks and devices are skipped -- sce_sys should not
         * contain them, and tolerating "unexpected node type" is
         * safer than trying to handle it. */
    }
    closedir(d);
    return rc;
}

/* Filter for which files from sce_sys get copied to
 * /user/appmeta/<title_id>. This keeps XMB metadata lean -- no
 * point duplicating the entire sce_sys tree. */
static int is_appmeta_file(const char *name) {
    if (!name) return 0;
    if (strcasecmp(name, "param.json") == 0) return 1;
    if (strcasecmp(name, "param.sfo") == 0)  return 1;
    const char *ext = strrchr(name, '.');
    if (!ext) return 0;
    if (strcasecmp(ext, ".png") == 0) return 1;
    if (strcasecmp(ext, ".dds") == 0) return 1;
    if (strcasecmp(ext, ".at9") == 0) return 1;
    return 0;
}

static int copy_sce_sys_to_appmeta(const char *src_sce_sys,
                                    const char *user_appmeta_dir) {
    DIR *d = opendir(src_sce_sys);
    if (!d) return -1;
    struct dirent *e;
    int rc = 0;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (!is_appmeta_file(e->d_name)) continue;
        char sp[REG_MAX_PATH], dp[REG_MAX_PATH];
        int n = snprintf(sp, sizeof(sp), "%s/%s", src_sce_sys, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(sp)) { rc = -1; break; }
        n = snprintf(dp, sizeof(dp), "%s/%s", user_appmeta_dir, e->d_name);
        if (n < 0 || (size_t)n >= sizeof(dp)) { rc = -1; break; }
        struct stat st;
        if (stat(sp, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        if (copy_file_bytes(sp, dp) != 0) { rc = -1; break; }
    }
    closedir(d);
    return rc;
}

static int write_link_file(const char *path, const char *value) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(value);
    ssize_t off = 0;
    while ((size_t)off < len) {
        ssize_t w = write(fd, value + off, len - (size_t)off);
        if (w < 0) { if (errno == EINTR) continue; close(fd); return -1; }
        if (w == 0) { close(fd); return -1; }  /* defensive: avoid infinite loop */
        off += w;
    }
    close(fd);
    return 0;
}

/* Normalise a source path into the shape we want in mount.lnk: no
 * trailing slash, no duplicate slashes. The Library scan emits paths
 * without trailing slashes, so normalising at write time keeps the
 * later "is this title registered from THIS path?" comparison honest.
 * Writes to `out` (which must be at least REG_MAX_PATH bytes). */
static void normalise_src_path(const char *in, char *out) {
    size_t n = strlen(in);
    if (n + 1 > REG_MAX_PATH) n = REG_MAX_PATH - 1;
    memcpy(out, in, n);
    out[n] = '\0';
    /* Strip trailing slashes, but keep the single "/" root as-is. */
    while (n > 1 && out[n - 1] == '/') {
        out[n - 1] = '\0';
        n--;
    }
}

/* -- param.json + param.sfo parsers ------------------------------------ */

/* Copy the value of a "key":"value" JSON string field into out.
 * Returns 0 on success, -1 on not found / malformed. Same semantics as
 * the extract_json_string_field already in runtime.c but kept local so
 * register.c can compile without a cross-file call. */
static int json_extract_string(const char *json, const char *key,
                                char *out, size_t out_cap) {
    if (!json || !key || !out || out_cap == 0) return -1;
    char needle[64];
    int n = snprintf(needle, sizeof(needle), "\"%s\":", key);
    if (n < 0 || (size_t)n >= sizeof(needle)) return -1;
    const char *p = strstr(json, needle);
    if (!p) return -1;
    p += strlen(needle);
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p != '"') return -1;
    p++;
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < out_cap) {
        /* We do not attempt full JSON escape handling -- param.json
         * titles don't contain backslashes in any observed dataset. */
        out[i++] = *p++;
    }
    out[i] = '\0';
    return (i > 0) ? 0 : -1;
}

/* DRM-type patcher. Reads `<src>/sce_sys/param.json`, finds
 * `"applicationDrmType":"<value>"`, and rewrites it to `"standard"`
 * if it isn't already. Some PSN-extracted dumps land with
 * `applicationDrmType` set to `"PSN"` or `"disc"`, and Sony's
 * launcher refuses to run them with a DRM error. Setting the
 * field to `"standard"` is the minimum-invasive adjustment that
 * makes the dump bootable.
 *
 * Returns:
 *   0   — file already had `"standard"` (no I/O), or patch applied.
 *   -1  — read/parse/write failure. *err_out is populated with a
 *         short reason string callers can forward.
 *
 * Bounds: rejects param.json larger than 1 MiB (no real game
 * exceeds this — observed largest is ~30 KiB). Operates on a
 * single in-memory buffer to keep the patch atomic-ish; the write
 * is a single fopen + fwrite + fclose cycle, so a crash mid-write
 * could truncate the file. We accept that risk because (a) this
 * is opt-in, (b) param.json is small and re-extracted from the
 * source dump, and (c) the alternative — temp file + rename —
 * doesn't work on most PS5 mounted-image filesystems where rename
 * across the mount boundary fails with EXDEV.
 *
 * The JSON edit is naive: find the key string, then the colon,
 * then the two surrounding quotes. No real JSON parser —
 * param.json's shape is stable enough that this works on every
 * dump we've seen. */
static int patch_drm_type_to_standard(const char *src_path,
                                       const char **err_out) {
    if (!src_path) {
        if (err_out) *err_out = "register_drm_patch_invalid_args";
        return -1;
    }
    char param_json_path[REG_MAX_PATH];
    int n = snprintf(param_json_path, sizeof(param_json_path),
                     "%s/sce_sys/param.json", src_path);
    if (n < 0 || (size_t)n >= sizeof(param_json_path)) {
        if (err_out) *err_out = "register_drm_patch_path_too_long";
        return -1;
    }
    FILE *f = fopen(param_json_path, "rb");
    if (!f) {
        /* Some dumps use param.sfo only (binary, not JSON). Treat
         * as a no-op rather than an error — if the launcher can
         * read sfo at all it doesn't need the json patch. */
        if (err_out) *err_out = NULL;
        return 0;
    }
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        if (err_out) *err_out = "register_drm_patch_seek_failed";
        return -1;
    }
    long len = ftell(f);
    if (len <= 0 || len > 1024 * 1024) {
        fclose(f);
        if (err_out) *err_out = "register_drm_patch_size_unreasonable";
        return -1;
    }
    rewind(f);
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) {
        fclose(f);
        if (err_out) *err_out = "register_drm_patch_oom";
        return -1;
    }
    size_t got = fread(buf, 1, (size_t)len, f);
    fclose(f);
    if (got != (size_t)len) {
        free(buf);
        if (err_out) *err_out = "register_drm_patch_read_short";
        return -1;
    }
    buf[len] = '\0';

    static const char KEY[] = "\"applicationDrmType\"";
    char *key_pos = strstr(buf, KEY);
    if (!key_pos) {
        /* No applicationDrmType field at all — nothing to patch.
         * Some homebrew dumps omit it entirely; the launcher
         * defaults to "standard" in that case. */
        free(buf);
        if (err_out) *err_out = NULL;
        return 0;
    }
    char *colon = strchr(key_pos + sizeof(KEY) - 1, ':');
    char *q1 = colon ? strchr(colon, '"') : NULL;
    char *q2 = q1 ? strchr(q1 + 1, '"') : NULL;
    if (!q1 || !q2) {
        free(buf);
        if (err_out) *err_out = "register_drm_patch_value_malformed";
        return -1;
    }
    /* Already "standard" — exit clean without writing. */
    static const char STANDARD[] = "standard";
    if ((size_t)(q2 - q1 - 1) == strlen(STANDARD) &&
        strncmp(q1 + 1, STANDARD, strlen(STANDARD)) == 0) {
        free(buf);
        if (err_out) *err_out = NULL;
        return 0;
    }
    /* Rewrite. Total new size = bytes-up-to-and-including-q1 +
     * "standard" + bytes-from-q2-onward (including the closing
     * quote and the trailing NUL). */
    size_t prefix_len = (size_t)(q1 - buf) + 1; /* through opening quote */
    size_t suffix_len = strlen(q2);             /* from closing quote inclusive */
    size_t new_len = prefix_len + strlen(STANDARD) + suffix_len;
    char *out = (char *)malloc(new_len + 1);
    if (!out) {
        free(buf);
        if (err_out) *err_out = "register_drm_patch_oom";
        return -1;
    }
    memcpy(out, buf, prefix_len);
    memcpy(out + prefix_len, STANDARD, strlen(STANDARD));
    memcpy(out + prefix_len + strlen(STANDARD), q2, suffix_len);
    out[new_len] = '\0';
    free(buf);

    f = fopen(param_json_path, "wb");
    if (!f) {
        free(out);
        /* Likely a read-only mount (RO image, system partition).
         * Surface this distinctly so the UI can suggest remounting
         * RW or unsetting the patch flag. */
        if (err_out) *err_out = "register_drm_patch_write_open_failed";
        return -1;
    }
    size_t written = fwrite(out, 1, new_len, f);
    int close_rc = fclose(f);
    free(out);
    if (written != new_len || close_rc != 0) {
        if (err_out) *err_out = "register_drm_patch_write_failed";
        return -1;
    }
    if (err_out) *err_out = NULL;
    return 0;
}

static int parse_title_from_param_json(const char *path,
                                        char *out_id, size_t id_cap,
                                        char *out_name, size_t name_cap) {
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) return -1;
    if (st.st_size <= 0 || st.st_size > 1024 * 1024) return -1;

    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    size_t len = (size_t)st.st_size;
    char *buf = (char *)malloc(len + 1);
    if (!buf) { fclose(f); return -1; }
    int ok = (fread(buf, 1, len, f) == len) ? 0 : -1;
    fclose(f);
    if (ok != 0) { free(buf); return -1; }
    buf[len] = '\0';

    /* title_id / titleId -- accept both field names. */
    if (json_extract_string(buf, "titleId", out_id, id_cap) != 0 &&
        json_extract_string(buf, "title_id", out_id, id_cap) != 0) {
        free(buf);
        return -1;
    }

    /* Title -- prefer the en-US block if present, else top-level. */
    out_name[0] = '\0';
    const char *en = strstr(buf, "\"en-US\"");
    if (en) {
        (void)json_extract_string(en, "titleName", out_name, name_cap);
    }
    if (out_name[0] == '\0') {
        (void)json_extract_string(buf, "titleName", out_name, name_cap);
    }
    if (out_name[0] == '\0') {
        /* Fall back to title_id so the XMB tile has a label. */
        size_t idlen = strlen(out_id);
        if (idlen + 1 <= name_cap) memcpy(out_name, out_id, idlen + 1);
    }
    free(buf);
    return 0;
}

/* PSF (param.sfo) binary layout -- fixed at PS4 launch and unchanged
 * since. 20-byte header, then a key-index table, then a string table
 * for keys, and a data table for values. See psdevwiki "param.sfo".
 *
 * We only look up TITLE_ID and TITLE, both of which have param_fmt =
 * 0x0204 (UTF-8) in every real-world game. If we ever need to handle
 * the integer-valued fields (ATTRIBUTE, APP_VER, etc.) we would also
 * need to honour the param_fmt switch. */
static int parse_title_from_param_sfo(const char *path,
                                       char *out_id, size_t id_cap,
                                       char *out_name, size_t name_cap) {
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) return -1;
    if (st.st_size < 20 || st.st_size > 1024 * 1024) return -1;

    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    unsigned char *buf = (unsigned char *)malloc((size_t)st.st_size);
    if (!buf) { fclose(f); return -1; }
    size_t got = fread(buf, 1, (size_t)st.st_size, f);
    fclose(f);
    if (got != (size_t)st.st_size) { free(buf); return -1; }

    if (buf[0] != 0x00 || buf[1] != 'P' || buf[2] != 'S' || buf[3] != 'F') {
        free(buf);
        return -1;
    }
    uint32_t key_tbl = (uint32_t)buf[ 8] | ((uint32_t)buf[ 9] << 8)
                     | ((uint32_t)buf[10] << 16) | ((uint32_t)buf[11] << 24);
    uint32_t data_tbl = (uint32_t)buf[12] | ((uint32_t)buf[13] << 8)
                      | ((uint32_t)buf[14] << 16) | ((uint32_t)buf[15] << 24);
    uint32_t entries  = (uint32_t)buf[16] | ((uint32_t)buf[17] << 8)
                      | ((uint32_t)buf[18] << 16) | ((uint32_t)buf[19] << 24);
    if (key_tbl  >= (uint32_t)st.st_size ||
        data_tbl >= (uint32_t)st.st_size ||
        entries == 0 || entries > 1000) {
        free(buf);
        return -1;
    }

    out_id[0] = '\0';
    out_name[0] = '\0';

    for (uint32_t i = 0; i < entries; i++) {
        uint32_t eoff = 20 + i * 16;
        if (eoff + 16 > (uint32_t)st.st_size) break;
        uint16_t key_off  = (uint16_t)(buf[eoff] | (buf[eoff + 1] << 8));
        uint32_t param_len = (uint32_t)buf[eoff + 4]
                           | ((uint32_t)buf[eoff + 5] << 8)
                           | ((uint32_t)buf[eoff + 6] << 16)
                           | ((uint32_t)buf[eoff + 7] << 24);
        uint32_t data_off = (uint32_t)buf[eoff + 12]
                          | ((uint32_t)buf[eoff + 13] << 8)
                          | ((uint32_t)buf[eoff + 14] << 16)
                          | ((uint32_t)buf[eoff + 15] << 24);

        if (key_tbl + key_off >= (uint32_t)st.st_size) continue;
        const char *key = (const char *)(buf + key_tbl + key_off);
        /* Bound the key read by the size of the key table. */
        size_t keymax = (size_t)st.st_size - (size_t)(key_tbl + key_off);
        size_t klen = strnlen(key, keymax);
        if (klen == keymax) continue; /* unterminated */

        if (data_tbl + data_off + param_len > (uint32_t)st.st_size) continue;
        const char *val = (const char *)(buf + data_tbl + data_off);

        if (strcmp(key, "TITLE_ID") == 0 && out_id[0] == '\0') {
            size_t copy = param_len;
            if (copy >= id_cap) copy = id_cap - 1;
            memcpy(out_id, val, copy);
            out_id[copy] = '\0';
            /* Strip trailing NULs that PSF pads values with. */
            while (copy > 0 && out_id[copy - 1] == '\0') copy--;
            out_id[copy] = '\0';
        } else if (strcmp(key, "TITLE") == 0 && out_name[0] == '\0') {
            size_t copy = param_len;
            if (copy >= name_cap) copy = name_cap - 1;
            memcpy(out_name, val, copy);
            out_name[copy] = '\0';
            while (copy > 0 && out_name[copy - 1] == '\0') copy--;
            out_name[copy] = '\0';
        }
    }
    free(buf);
    if (out_id[0] == '\0') return -1;
    if (out_name[0] == '\0') {
        size_t idlen = strlen(out_id);
        if (idlen + 1 <= name_cap) memcpy(out_name, out_id, idlen + 1);
    }
    return 0;
}

/* Look for param.json first, then param.sfo. Native-game folders
 * dropped into /data/homebrew may use either. */
static int read_title_metadata(const char *src_path,
                                char *out_id, size_t id_cap,
                                char *out_name, size_t name_cap) {
    char p[REG_MAX_PATH];
    int n = snprintf(p, sizeof(p), "%s/sce_sys/param.json", src_path);
    if (n > 0 && (size_t)n < sizeof(p) &&
        parse_title_from_param_json(p, out_id, id_cap, out_name, name_cap) == 0) {
        return 0;
    }
    n = snprintf(p, sizeof(p), "%s/sce_sys/param.sfo", src_path);
    if (n > 0 && (size_t)n < sizeof(p) &&
        parse_title_from_param_sfo(p, out_id, id_cap, out_name, name_cap) == 0) {
        return 0;
    }
    return -1;
}

/* -- nullfs bind-mount pipeline ----------------------------------------- */

/* nmount iovec helper — each entry must be a NUL-terminated cstring;
 * the kernel copies `iov_len` bytes including the NUL. */
#define IOV_STR(s) { .iov_base = (void *)(s), .iov_len = strlen(s) + 1 }

/* Bind-mount `src_path` at `/system_ex/app/<title_id>/` via nullfs.
 * If something is already mounted at the target, we force-unmount it
 * first ("reset the stack" behaviour) so the new bind sees a
 * clean mountpoint. */
static int mount_title_nullfs(const char *title_id, const char *src_path,
                              const char **err_out) {
    char dst[REG_MAX_PATH];
    int n = snprintf(dst, sizeof(dst), "%s/%s", REG_SYSTEM_EX_APP, title_id);
    if (n < 0 || (size_t)n >= sizeof(dst)) {
        if (err_out) *err_out = "register_nullfs_dst_overflow";
        return -1;
    }
    struct stat st;
    if (stat(src_path, &st) != 0 || !S_ISDIR(st.st_mode)) {
        if (err_out) *err_out = "register_src_not_directory";
        return -1;
    }
    /* Sanity check: the source should have an eboot.bin. Registering a
     * folder without one results in a tile that crashes on launch. */
    char src_eboot[REG_MAX_PATH];
    if (snprintf(src_eboot, sizeof(src_eboot), "%s/eboot.bin", src_path) > 0
        && !path_exists(src_eboot)) {
        if (err_out) *err_out = "register_source_missing_eboot";
        return -1;
    }

    /* Create the mountpoint. mkdir_p so we can create /system_ex/app
     * itself on the (unusual) case where it does not exist yet. */
    if (mkdir_p(REG_SYSTEM_EX_APP) != 0 && errno != EEXIST) {
        if (err_out) *err_out = "register_mkdir_system_ex_failed";
        return -1;
    }
    if (mkdir(dst, 0755) != 0 && errno != EEXIST) {
        if (err_out) *err_out = "register_mkdir_target_failed";
        return -1;
    }

    /* Force-clear any pre-existing mount at dst. Tolerant of ENOENT/EINVAL
     * (nothing mounted), but we do not force MNT_FORCE a second time --
     * if the system truly cannot unmount, fail fast so we do not layer
     * ourselves on top of stale state. */
    struct statfs dst_stat;
    if (statfs(dst, &dst_stat) == 0 &&
        strcmp(dst_stat.f_mntonname, dst) == 0) {
        if (unmount(dst, 0) != 0 && errno != EINVAL && errno != ENOENT) {
            if (unmount(dst, MNT_FORCE) != 0 && errno != EINVAL && errno != ENOENT) {
                if (err_out) *err_out = "register_preclean_unmount_failed";
                return -1;
            }
        }
    }

    char fstype[]  = "nullfs";
    char kfrom[]   = "from";
    char kfspath[] = "fspath";
    char kfstype[] = "fstype";
    struct iovec iov[6];
    iov[0].iov_base = kfstype;  iov[0].iov_len = sizeof(kfstype);
    iov[1].iov_base = fstype;   iov[1].iov_len = sizeof(fstype);
    iov[2].iov_base = kfrom;    iov[2].iov_len = sizeof(kfrom);
    iov[3].iov_base = (void *)src_path; iov[3].iov_len = strlen(src_path) + 1;
    iov[4].iov_base = kfspath;  iov[4].iov_len = sizeof(kfspath);
    iov[5].iov_base = dst;      iov[5].iov_len = strlen(dst) + 1;

    if (nmount(iov, 6, 0) != 0) {
        if (err_out) *err_out = "register_nullfs_nmount_failed";
        return -1;
    }

    char dst_eboot[REG_MAX_PATH];
    if (snprintf(dst_eboot, sizeof(dst_eboot), "%s/eboot.bin", dst) > 0
        && !path_exists(dst_eboot)) {
        /* Roll back -- the mount succeeded structurally but the bind
         * did not surface our eboot.bin. Leaving the mount in place
         * would cause later launch failures with no clear cause. */
        (void)unmount(dst, MNT_FORCE);
        if (err_out) *err_out = "register_nullfs_post_no_eboot";
        return -1;
    }
    return 0;
}

static int unmount_title_nullfs(const char *title_id) {
    char dst[REG_MAX_PATH];
    int n = snprintf(dst, sizeof(dst), "%s/%s", REG_SYSTEM_EX_APP, title_id);
    if (n < 0 || (size_t)n >= sizeof(dst)) return -1;

    /* unmount then MNT_FORCE. Either rc=0 OR errno in (EINVAL, ENOENT)
     * is acceptable -- both mean "nothing mounted here" which is our
     * desired end state. */
    int rc = unmount(dst, 0);
    if (rc == 0) return 0;
    if (errno == EINVAL || errno == ENOENT) return 0;
    rc = unmount(dst, MNT_FORCE);
    if (rc == 0) return 0;
    if (errno == EINVAL || errno == ENOENT) return 0;
    return -1;
}

/* -- sqlite-backed snd0info patch + list_registered -------------------- */

/* An earlier build updated snd0info on the app.db row after Sony's
 * installer ran, but we removed that path with the sqlite dependency
 * because dlopen'ing libSceSqlite.sprx hangs indefinitely on some
 * firmware versions. The
 * snd0info patch was best-effort (audio fallback on the XMB tile) so
 * the degradation on skip is purely cosmetic: a title whose snd0.at9
 * isn't pointed-to plays no tile audio, but the game itself launches
 * fine. This stub preserves the callsite so register_title_from_path
 * doesn't need editing; it always returns -1 (no update). */
static int update_snd0info(const char *title_id) {
    (void)title_id;
    return -1;
}

/* Check whether a name looks like a PS5 title-id (9 chars, PPSA / CUSA /
 * NPXS / similar prefix). Strict enough to filter out non-title
 * directories under /user/app/ without over-filtering. */
static int looks_like_title_id(const char *name) {
    if (!name) return 0;
    size_t n = strlen(name);
    if (n != 9) return 0;
    /* 4 uppercase letters */
    for (int i = 0; i < 4; i++) {
        char c = name[i];
        if (c < 'A' || c > 'Z') return 0;
    }
    /* 5 digits */
    for (int i = 4; i < 9; i++) {
        char c = name[i];
        if (c < '0' || c > '9') return 0;
    }
    return 1;
}

/* List registered titles by scanning /user/app/ directly. This used to
 * open Sony's app.db via sqlite3, which required dlopen'ing
 * libSceSqlite.sprx -- and that dlopen hung indefinitely on firmware
 * where the library has been moved or renamed (empirically observed on
 * a 9.60 / CFI-1215A console). The filesystem scan is functionally
 * equivalent for our needs: any title we installed leaves a
 * /user/app/<id>/mount.lnk tracker, and any title Sony's XMB knows
 * about has a /user/app/<id>/ directory. So readdir + stat is enough,
 * no library init needed, no hang path. */
int list_registered_titles_json(char *out_json,
                                size_t out_cap,
                                size_t *out_written,
                                const char **err_reason_out) {
    if (!out_json || out_cap < 32) {
        if (err_reason_out) *err_reason_out = "list_buffer_too_small";
        return -1;
    }

    DIR *dir = opendir(REG_APP_BASE);
    if (!dir) {
        /* No /user/app/ means no registered titles -- return empty list
         * rather than error. Matches the sqlite behaviour where an
         * empty app.db returns 0 rows. */
        int n = snprintf(out_json, out_cap, "{\"apps\":[]}");
        if (n < 0) return -1;
        if (out_written) *out_written = (size_t)n;
        return 0;
    }

    size_t off = 0;
    int first = 1;
    int n = snprintf(out_json + off, out_cap - off, "{\"apps\":[");
    if (n < 0 || (size_t)n >= out_cap - off) {
        closedir(dir);
        if (err_reason_out) *err_reason_out = "list_json_overflow";
        return -1;
    }
    off += (size_t)n;

    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        const char *tid = entry->d_name;
        if (!looks_like_title_id(tid)) continue;

        char lnk[REG_MAX_PATH];
        char lnk_img[REG_MAX_PATH];
        char src[REG_MAX_PATH] = {0};
        int image_backed = 0;
        snprintf(lnk,     sizeof(lnk),     "%s/%s/mount.lnk",     REG_APP_BASE, tid);
        snprintf(lnk_img, sizeof(lnk_img), "%s/%s/mount_img.lnk", REG_APP_BASE, tid);
        FILE *lf = fopen(lnk, "r");
        if (lf) {
            if (fgets(src, sizeof(src), lf)) {
                size_t slen = strlen(src);
                if (slen > 0 && src[slen - 1] == '\n') src[slen - 1] = '\0';
            }
            fclose(lf);
            if (path_exists(lnk_img)) image_backed = 1;
        }

        /* Minimal JSON escape for src (paths can contain spaces, not
         * much else -- but backslash/quote protection is cheap). */
        char src_esc[REG_MAX_PATH * 2 + 2];
        size_t sei = 0;
        for (const char *sp = src; *sp && sei + 2 < sizeof(src_esc); sp++) {
            unsigned char c = (unsigned char)*sp;
            if (c == '\\' || c == '"') {
                src_esc[sei++] = '\\';
                src_esc[sei++] = (char)c;
            } else if (c >= 0x20) {
                src_esc[sei++] = (char)c;
            }
        }
        src_esc[sei] = '\0';

        n = snprintf(out_json + off, out_cap - off,
                     "%s{\"title_id\":\"%s\",\"title_name\":\"%s\","
                     "\"src\":\"%s\",\"image_backed\":%s}",
                     first ? "" : ",",
                     tid, tid, src_esc,
                     image_backed ? "true" : "false");
        if (n < 0 || (size_t)n >= out_cap - off) {
            closedir(dir);
            if (err_reason_out) *err_reason_out = "list_json_overflow";
            return -1;
        }
        off += (size_t)n;
        first = 0;
    }
    closedir(dir);

    n = snprintf(out_json + off, out_cap - off, "]}");
    if (n < 0 || (size_t)n >= out_cap - off) {
        if (err_reason_out) *err_reason_out = "list_json_overflow";
        return -1;
    }
    off += (size_t)n;
    if (out_written) *out_written = off;
    return 0;
}

/* -- Public register / unregister / launch ----------------------------- */

int register_title_from_path(const char *src_path,
                             int patch_drm_type,
                             char *out_title_id,
                             char *out_title_name,
                             int *out_used_nullfs,
                             const char **err_reason_out) {
    register_module_init();
    if (!src_path || !out_title_id || !out_title_name) {
        if (err_reason_out) *err_reason_out = "register_args_invalid";
        return -1;
    }
    out_title_id[0] = '\0';
    out_title_name[0] = '\0';
    if (out_used_nullfs) *out_used_nullfs = 0;

    /* 0 -- resolve metadata so we have a title_id. */
    char title_id[REGISTER_MAX_TITLE_ID];
    char title_name[REGISTER_MAX_TITLE_NAME];
    if (read_title_metadata(src_path, title_id, sizeof(title_id),
                                      title_name, sizeof(title_name)) != 0) {
        if (err_reason_out) *err_reason_out = "register_metadata_missing";
        return -1;
    }
    if (!is_safe_component(title_id)) {
        if (err_reason_out) *err_reason_out = "register_title_id_unsafe";
        return -1;
    }

    /* 0b -- pre-validate the source has what Sony's launcher needs.
     * Doing this BEFORE the copies avoids leaving orphan state under
     * /user/app/<id>/ when the source folder is incomplete (e.g., a
     * half-extracted archive with sce_sys/ but no eboot.bin). Any of
     * these would cause mount_title_nullfs or the launcher to fail
     * later anyway -- failing early keeps the filesystem clean. */
    {
        struct stat st_probe;
        char probe[REG_MAX_PATH];
        if (stat(src_path, &st_probe) != 0 || !S_ISDIR(st_probe.st_mode)) {
            if (err_reason_out) *err_reason_out = "register_src_not_directory";
            return -1;
        }
        snprintf(probe, sizeof(probe), "%s/sce_sys", src_path);
        if (stat(probe, &st_probe) != 0 || !S_ISDIR(st_probe.st_mode)) {
            if (err_reason_out) *err_reason_out = "register_src_missing_sce_sys";
            return -1;
        }
        snprintf(probe, sizeof(probe), "%s/eboot.bin", src_path);
        if (!path_exists(probe)) {
            if (err_reason_out) *err_reason_out = "register_src_missing_eboot";
            return -1;
        }
    }

    /* 0c -- optional DRM-type patch. Runs AFTER pre-validation (so
     * we don't write to a half-extracted source) but BEFORE the
     * staging copies (so the COPY of param.json that lands in
     * /user/app/<id>/sce_sys/ is the patched one). The launcher
     * reads from the nullfs-bound source via /system_ex, so the
     * source's param.json is what actually matters at launch time
     * — but keeping the staged copy in sync prevents confusion if
     * a future codepath reads from /user/app instead. */
    if (patch_drm_type) {
        const char *drm_err = NULL;
        if (patch_drm_type_to_standard(src_path, &drm_err) != 0) {
            if (err_reason_out) {
                *err_reason_out = drm_err ? drm_err : "register_drm_patch_failed";
            }
            return -1;
        }
    }

    /* 1 -- copy sce_sys to /user/app/<title_id>/sce_sys */
    char user_app[REG_MAX_PATH];
    char user_sce_sys[REG_MAX_PATH];
    char user_appmeta[REG_MAX_PATH];
    char src_sce_sys[REG_MAX_PATH];
    snprintf(user_app,     sizeof(user_app),     "%s/%s", REG_APP_BASE, title_id);
    snprintf(user_sce_sys, sizeof(user_sce_sys), "%s/sce_sys", user_app);
    snprintf(user_appmeta, sizeof(user_appmeta), "%s/%s", REG_APPMETA_BASE, title_id);
    snprintf(src_sce_sys,  sizeof(src_sce_sys),  "%s/sce_sys", src_path);

    if (mkdir_p(user_app) != 0) {
        if (err_reason_out) *err_reason_out = "register_mkdir_user_app_failed";
        return -1;
    }
    if (copy_dir_recursive(src_sce_sys, user_sce_sys) != 0) {
        if (err_reason_out) *err_reason_out = "register_copy_sce_sys_failed";
        return -1;
    }

    /* 2 -- top-level icon0.png as a sibling of sce_sys */
    char icon_src[REG_MAX_PATH];
    char icon_dst[REG_MAX_PATH];
    snprintf(icon_src, sizeof(icon_src), "%s/icon0.png", src_sce_sys);
    snprintf(icon_dst, sizeof(icon_dst), "%s/icon0.png", user_app);
    if (path_exists(icon_src)) {
        (void)copy_file_bytes(icon_src, icon_dst);
    }

    /* 3 -- appmeta */
    if (mkdir_p(user_appmeta) != 0) {
        if (err_reason_out) *err_reason_out = "register_mkdir_appmeta_failed";
        return -1;
    }
    if (copy_sce_sys_to_appmeta(src_sce_sys, user_appmeta) != 0) {
        if (err_reason_out) *err_reason_out = "register_copy_appmeta_failed";
        return -1;
    }

    /* 4 -- nullfs bind */
    const char *mnt_err = NULL;
    if (mount_title_nullfs(title_id, src_path, &mnt_err) != 0) {
        if (err_reason_out) *err_reason_out = mnt_err ? mnt_err : "register_nullfs_failed";
        return -1;
    }

    /* 5 -- tracking files. Normalise src first so the path we write
     * matches what the Library scan produces; otherwise an input like
     * "/data/homebrew/foo/" would fail the registered-by-src lookup
     * for the scan-emitted "/data/homebrew/foo".
     *
     * Failure to write mount.lnk used to be silently ignored (via
     * (void) cast). That made the Library show the title as "not
     * registered by us" even when register succeeded -- because the
     * registered-vs-external check is "does /user/app/<id>/mount.lnk
     * exist?". Roll back the nullfs bind + report the error so the
     * user knows to check filesystem permissions. */
    char src_norm[REG_MAX_PATH];
    normalise_src_path(src_path, src_norm);
    char lnk[REG_MAX_PATH];
    snprintf(lnk, sizeof(lnk), "%s/mount.lnk", user_app);
    if (write_link_file(lnk, src_norm) != 0) {
        (void)unmount_title_nullfs(title_id);
        if (err_reason_out) *err_reason_out = "register_tracking_file_write_failed";
        return -1;
    }
    /* If the source is under /mnt/ps5upload/, write mount_img.lnk too.
     * This lets the Library expose image-backed titles distinctly so
     * we can warn before unmounting the image. Best-effort: if this
     * specific write fails we continue (main mount.lnk is sufficient
     * for the Library to track us); the image_backed flag just won't
     * get set. */
    if (strncmp(src_norm, "/mnt/ps5upload/", 15) == 0) {
        char lnk_img[REG_MAX_PATH];
        snprintf(lnk_img, sizeof(lnk_img), "%s/mount_img.lnk", user_app);
        (void)write_link_file(lnk_img, src_norm);
    }

    /* 6 -- Sony installer API */
    if (!g_reg.app_install_title_dir) {
        if (err_reason_out) *err_reason_out = "register_install_api_unavailable";
        /* We leave the nullfs in place so the nominal tile works if
         * the user later loads it into the XMB by other means. Do
         * not roll back the staged metadata either -- it is safe to
         * re-use. */
        return -1;
    }
    /* Serialize across threads + call installer + post-call grace.
     * The mutex prevents two HTTP handlers from calling Sony's
     * installer concurrently (kernel stub deadlock on 9.60); the
     * post-sleep gives Sony's stub time to release internal locks
     * before the next caller. */
    pthread_mutex_lock(&g_sony_api_mtx);
    /* Belt-and-suspenders: even though register_services_init() has
     * already called this from main-thread context at startup, calling
     * it again before the install is cheap (Sony's init is idempotent)
     * and ensures the call-site is robust to future refactors where
     * services_init may be skipped. */
    if (g_reg.app_inst_util_initialize) {
        (void)g_reg.app_inst_util_initialize();
    }
    int res = g_reg.app_install_title_dir(title_id, REG_APP_BASE "/", 0);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&g_sony_api_mtx);
    /* 0 = new install, 0x80990002 = restored (idempotent), both are success. */
    if (res != 0 && (uint32_t)res != 0x80990002u) {
        /* Roll back the nullfs bind so we don't leak a kernel resource
         * for a title the XMB can't launch (no app.db row). Staged
         * metadata under /user/app/<id>/ is intentionally kept -- a
         * retry from the same src_path will succeed idempotently and
         * those files are ready to go. */
        (void)unmount_title_nullfs(title_id);
        if (err_reason_out) *err_reason_out = "register_install_api_error";
        return -1;
    }

    /* 7 -- snd0info patch (best effort) */
    char snd0[REG_MAX_PATH];
    snprintf(snd0, sizeof(snd0), "%s/sce_sys/snd0.at9", src_path);
    if (path_exists(snd0)) {
        (void)update_snd0info(title_id);
    }

    /* Populate outputs only on full success. */
    size_t tlen  = strlen(title_id);
    size_t nlen  = strlen(title_name);
    if (tlen + 1 > REGISTER_MAX_TITLE_ID || nlen + 1 > REGISTER_MAX_TITLE_NAME) {
        if (err_reason_out) *err_reason_out = "register_name_overflow";
        return -1;
    }
    memcpy(out_title_id, title_id, tlen + 1);
    memcpy(out_title_name, title_name, nlen + 1);
    if (out_used_nullfs) *out_used_nullfs = 1;
    return 0;
}

int unregister_title(const char *title_id, const char **err_reason_out) {
    register_module_init();
    if (!title_id || !is_safe_component(title_id)) {
        if (err_reason_out) *err_reason_out = "unregister_title_id_invalid";
        return -1;
    }
    /* Nullfs unmount first. This is the teardown that matters most --
     * leaving a stale nullfs bound to /system_ex/app/<id>/ points
     * the XMB launcher at a frozen snapshot whose underlying file
     * may have been deleted. */
    if (unmount_title_nullfs(title_id) != 0) {
        if (err_reason_out) *err_reason_out = "unregister_unmount_failed";
        return -1;
    }

    /* Best-effort: remove the tracking link files. The staged
     * metadata under /user/app/<title_id>/ stays -- Sony's installer
     * may still need it, and removing it is what Uninstall from
     * Settings does. */
    char lnk[REG_MAX_PATH];
    char lnk_img[REG_MAX_PATH];
    snprintf(lnk,     sizeof(lnk),     "%s/%s/mount.lnk",     REG_APP_BASE, title_id);
    snprintf(lnk_img, sizeof(lnk_img), "%s/%s/mount_img.lnk", REG_APP_BASE, title_id);
    (void)unlink(lnk);
    (void)unlink(lnk_img);

    /* If Sony's AppUninstall is available, clear the XMB tile too.
     * Failure here is NOT fatal -- the nullfs is already gone, so the
     * user gets a dead tile rather than a launchable broken one.
     *
     * Same serialization + grace period as install. Sony's uninstall
     * stub shares implementation internals with install (they touch
     * the same app.db row), so the same kernel-lock concerns apply. */
    if (g_reg.app_uninstall) {
        /* Signature is (title_id, p1, p2). The trailing out-params
         * accept NULL — the call only needs the title_id and
         * returns successfully when the title exists. */
        pthread_mutex_lock(&g_sony_api_mtx);
        (void)g_reg.app_uninstall(title_id, NULL, NULL);
        usleep(SONY_API_POST_SLEEP_US);
        pthread_mutex_unlock(&g_sony_api_mtx);
    }
    return 0;
}

/* Opens the PS5's built-in web browser (NPXS20001) via
 * sceSystemServiceLaunchApp. Exposed for the APP_LAUNCH_BROWSER frame. */
int register_browser_launch(void) {
    register_module_init();
    if (!g_reg.launch_app_sys) return -1;
    int rc = g_reg.launch_app_sys("NPXS20001", NULL, NULL);
    return (rc == 0) ? 0 : -1;
}

/* Forward declaration of the SceShellUI RPC layer. Defined in
 * shellui_rpc.c; uses the ptrace remote-call mechanism in
 * ptrace_remote.c. We call into ShellUI for launch because
 * Sony's launcher does a caller-pid check that only ShellUI
 * satisfies. */
extern int shellui_rpc_init(void);
extern int shellui_rpc_ready(void);
extern int shellui_rpc_launch_app(const char *title_id);

int launch_title(const char *title_id, const char **err_reason_out) {
    register_module_init();
    if (!title_id || !is_safe_component(title_id)) {
        if (err_reason_out) *err_reason_out = "launch_title_id_invalid";
        return -1;
    }

    /* Primary path: route the call through SceShellUI via ptrace.
     * Sony's launcher accepts the call when getpid() inside the
     * stub matches ShellUI, which our ptrace remote-call provides
     * by running the call on ShellUI's stack. Init is idempotent:
     * the first call latches the resolved addresses, subsequent
     * calls become no-ops.
     *
     * shellui_rpc_launch_app returns:
     *   0   — Sony's launcher accepted the call (success).
     *   >0  — Sony's launcher returned a real error code (title
     *         not registered, no foreground user, etc.). We
     *         report it directly rather than falling through to
     *         the in-process call which would just produce a
     *         less informative error.
     *   -1  — the RPC machinery itself failed (couldn't attach
     *         to ShellUI, symbol missing). Fall through to the
     *         in-process direct calls below — those won't work
     *         on firmwares enforcing the caller-pid check, but
     *         they're the only fallback we have. */
    (void)shellui_rpc_init();
    if (shellui_rpc_ready()) {
        int rc = shellui_rpc_launch_app(title_id);
        if (rc == 0) return 0;
        if (rc > 0) {
            static __thread char reason_buf[64];
            snprintf(reason_buf, sizeof(reason_buf),
                     "launch_sony_error_0x%08x", (unsigned int)rc);
            if (err_reason_out) *err_reason_out = reason_buf;
            return -1;
        }
        /* rc == -1: machinery failed, fall through. */
    }

    if (!g_reg.launch_app && !g_reg.launch_app_sys) {
        if (err_reason_out) *err_reason_out = "launch_service_unavailable";
        return -1;
    }

    /* Initialize services (idempotent -- safe if already init). Without
     * these, sceLncUtilLaunchApp can return 0x8094000F ("no foreground
     * user set") on firmware where the launcher expects explicit
     * UserService init before a launch can succeed. */
    if (g_reg.user_service_initialize) {
        (void)g_reg.user_service_initialize(NULL);
    }
    if (g_reg.lnc_util_initialize) {
        (void)g_reg.lnc_util_initialize();
    }
    int fg_user = 0;
    if (g_reg.user_service_get_foreground_user) {
        (void)g_reg.user_service_get_foreground_user(&fg_user);
    }
    /* fg_user is used by the launchApp param block below.
     * LncAppParam.user_id needs to be the active console user, not
     * 0. */

    int rc = -1;
    int tried_param = 0, tried_null = 0, tried_sys = 0;
    int launched = 0;

    /* Serialize. Only one Sony launch API call at a time — the
     * kernel stubs share locks and concurrent callers deadlock. */
    pthread_mutex_lock(&g_sony_api_mtx);

    /* Triple-strategy launch chain. The "param" attempt uses the
     * canonical 24-byte LncAppParam (size, user_id, app_opt,
     * crash_report, check_flag), with user_id populated from the
     * foreground user. Sony's launcher rejects user_id=0 with
     * "no foreground user".
     *
     *   1. sceLncUtilLaunchApp(title_id, NULL, &param)
     *      — populated 24-byte struct.
     *   2. sceLncUtilLaunchApp(title_id, NULL, NULL)
     *      — NULL-param fallback for firmwares where the
     *        launcher rejects any param block.
     *   3. sceSystemServiceLaunchApp(title_id, NULL, NULL)
     *      — different internal path; works on some firmwares
     *        where Lnc returns SCE_LNC_UTIL_ERROR_NOT_FOUND. */
    if (!launched && g_reg.launch_app) {
        tried_param = 1;
        struct lnc_app_param {
            uint32_t sz;
            int32_t  user_id;
            uint32_t app_opt;
            uint64_t crash_report;
            uint64_t check_flag;
        } __attribute__((packed)) param;
        memset(&param, 0, sizeof(param));
        param.sz = (uint32_t)sizeof(param);
        param.user_id = fg_user;
        rc = g_reg.launch_app(title_id, NULL, (void *)&param);
        usleep(SONY_API_POST_SLEEP_US);
        if (rc == 0) launched = 1;
    }
    if (!launched && g_reg.launch_app) {
        tried_null = 1;
        rc = g_reg.launch_app(title_id, NULL, NULL);
        usleep(SONY_API_POST_SLEEP_US);
        if (rc == 0) launched = 1;
    }
    if (!launched && g_reg.launch_app_sys) {
        tried_sys = 1;
        rc = g_reg.launch_app_sys(title_id, NULL, NULL);
        usleep(SONY_API_POST_SLEEP_US);
        if (rc == 0) launched = 1;
    }

    pthread_mutex_unlock(&g_sony_api_mtx);

    if (launched) return 0;

    /* All strategies failed. Surface a diagnostic so the engine error
     * includes which paths we tried.
     *
     * Thread-local buffer — two mgmt threads can be executing this
     * concurrently (launch from client A + launch from client B), and
     * a plain `static char` would race both `snprintf` and the reader
     * that follows the returned pointer. `__thread` gives each mgmt
     * thread its own buffer so the returned const char * stays valid
     * for the caller's use-and-copy window without a lock. */
    {
        static __thread char reason_buf[128];
        snprintf(reason_buf, sizeof(reason_buf),
                 "launch_all_strategies_failed: param=%d null=%d sys=%d",
                 tried_param, tried_null, tried_sys);
        if (err_reason_out) *err_reason_out = reason_buf;
    }
    return -1;
}
