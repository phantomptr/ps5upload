/*
 * bgft.c — payload-side bindings for Sony's Background File Transfer
 *          (BGFT) service. See bgft.h for the public API contract.
 *
 * Implementation notes:
 *
 * 1. Symbol resolution. Sony's BGFT lives in libSceBgft.sprx. We dlopen
 *    it lazily at first install attempt and cache function pointers in
 *    a static `bgft_fns` table. If dlopen or any required symbol fails
 *    we record a reason string and fail every subsequent call cleanly
 *    (same pattern as register.c uses for libSceAppInstUtil).
 *
 * 2. BGFT init params. `sceBgftInitialize` wants a pre-allocated heap
 *    buffer. DPI uses 256 KiB; we match that. The buffer is allocated
 *    once (static), held forever — BGFT keeps a pointer to it, so it
 *    must outlive every Register/Start call.
 *
 * 3. User ID. BGFT's download_param needs the foreground user_id. We
 *    call sceUserServiceInitialize + sceUserServiceGetForegroundUser
 *    once and cache. Both are already linked via -lSceUserService in
 *    the Makefile so dlsym(RTLD_DEFAULT, ...) works without another
 *    dlopen pass.
 *
 * 4. Authid. Some community references set a specific authid
 *    (0x40001c0000000000) before calling BGFT. Our payload runs with
 *    the debugger authid (0x4800000000000006) which may already be
 *    sufficient on FW 9.60 — we'll log if BGFT rejects with EPERM and
 *    add a swap path if needed. Conservative for now: try with our
 *    existing authid first.
 *
 * 5. Concurrency. A single mutex guards the whole module. BGFT's
 *    own internal locking is opaque; serializing callers keeps the
 *    behavior predictable. Status polls also take the mutex briefly,
 *    which is fine — they're milliseconds at most.
 *
 * 6. Error pipeline. Every failure path sets `out_err_code` to a
 *    BGFT_ERR_* sentinel (high bit set) for our own machinery
 *    failures, or passes Sony's raw 0x80990xxx code through unchanged
 *    when the failure originated inside BGFT. The host-side
 *    err_code_message() maps both families to user-facing strings.
 */

#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "bgft.h"

/* ─── AppInstUtil install path (etaHEN/DPI style) ─────────────────────
 *
 * sceAppInstUtilInstallByPackage is Sony's high-level "fetch this URL,
 * decrypt, install" entry point — the same API etaHEN's DirectPKGInstaller
 * uses. It's compile-time linked via -lSceAppInstUtil so there's no
 * dlopen/dlsym dance and no per-firmware symbol-name drift. We try this
 * path first; if it fails (rare — would mean libSceAppInstUtil itself
 * isn't loaded), we fall back to the BGFT path below for legacy
 * firmwares where BGFT works but AppInstUtil doesn't.
 *
 * Layouts taken verbatim from etaHEN's util/include/common_utils.h. */

#define APPINST_PLAYGO_SCENARIOID_SIZE 3
#define APPINST_LANGUAGE_SIZE          8
#define APPINST_CONTENTID_SIZE         0x30
#define APPINST_NUM_LANGUAGES          30
#define APPINST_NUM_IDS                64

typedef struct {
    char    content_id[APPINST_CONTENTID_SIZE];
    int32_t content_type;
    int32_t content_platform;
} AppInstPkgInfo;

typedef struct {
    const char *uri;
    const char *ex_uri;
    const char *playgo_scenario_id;
    const char *content_id;
    const char *content_name;
    const char *icon_url;
} AppInstMetaInfo;

typedef struct {
    char  languages[APPINST_NUM_LANGUAGES][APPINST_LANGUAGE_SIZE];
    char  playgo_scenario_ids[APPINST_NUM_IDS][APPINST_PLAYGO_SCENARIOID_SIZE];
    char  content_ids[APPINST_NUM_IDS][APPINST_CONTENTID_SIZE];
    long  unknown[810];
} AppInstPlayGoInfo;

typedef struct {
    int32_t error_code;
    int32_t version;
    char    description[512];
    char    type[9];
} AppInstStatusErrorInfo;

typedef struct {
    char     status[16];
    char     src_type[8];
    uint32_t remain_time;
    uint64_t downloaded_size;
    uint64_t initial_chunk_size;
    uint64_t total_size;
    uint32_t promote_progress;
    AppInstStatusErrorInfo error_info;
    int32_t  local_copy_percent;
    int      is_copy_only;
} AppInstStatus;

extern int sceAppInstUtilInstallByPackage(AppInstMetaInfo *meta,
                                          AppInstPkgInfo *pkg_info,
                                          AppInstPlayGoInfo *playgo);
extern int sceAppInstUtilGetInstallStatus(const char *content_id,
                                          AppInstStatus *out);

/* Backend tag for in-flight install tracking. The bgft.h interface
 * returns a `task_id` that callers poll later via
 * `bgft_install_status`; we use a synthetic 32-bit id space (high
 * bit = appinst, lower bits = index into our task table) so a
 * single status call can route to the right backend without the
 * caller having to know which one was used. */
#define APPINST_TASK_ID_FLAG  0x40000000
#define APPINST_TASK_TABLE    16

typedef struct {
    int      in_use;
    char     content_id[APPINST_CONTENTID_SIZE];
} appinst_task_slot_t;

static appinst_task_slot_t g_appinst_tasks[APPINST_TASK_TABLE];
static pthread_mutex_t g_appinst_mtx = PTHREAD_MUTEX_INITIALIZER;

/** Allocate a slot, store the content_id we got back from
 *  sceAppInstUtilInstallByPackage. Returns the synthetic task_id
 *  (with APPINST_TASK_ID_FLAG set so bgft_install_status can route)
 *  or -1 on a full table. */
static int32_t appinst_task_register(const char *content_id) {
    pthread_mutex_lock(&g_appinst_mtx);
    for (int i = 0; i < APPINST_TASK_TABLE; i++) {
        if (!g_appinst_tasks[i].in_use) {
            g_appinst_tasks[i].in_use = 1;
            strncpy(g_appinst_tasks[i].content_id, content_id,
                    sizeof(g_appinst_tasks[i].content_id) - 1);
            g_appinst_tasks[i].content_id[sizeof(g_appinst_tasks[i].content_id) - 1] = '\0';
            pthread_mutex_unlock(&g_appinst_mtx);
            return (int32_t)(APPINST_TASK_ID_FLAG | (uint32_t)i);
        }
    }
    pthread_mutex_unlock(&g_appinst_mtx);
    return -1;
}

/** Look up the content_id for an appinst-tagged task_id. Returns
 *  0 + populates `out_content_id` on hit; -1 on miss / wrong tag. */
static int appinst_task_lookup(int32_t task_id, char *out, size_t out_cap) {
    if ((task_id & APPINST_TASK_ID_FLAG) == 0) return -1;
    int idx = task_id & ~APPINST_TASK_ID_FLAG;
    if (idx < 0 || idx >= APPINST_TASK_TABLE) return -1;
    pthread_mutex_lock(&g_appinst_mtx);
    if (!g_appinst_tasks[idx].in_use) {
        pthread_mutex_unlock(&g_appinst_mtx);
        return -1;
    }
    strncpy(out, g_appinst_tasks[idx].content_id, out_cap - 1);
    out[out_cap - 1] = '\0';
    pthread_mutex_unlock(&g_appinst_mtx);
    return 0;
}

/** Try the AppInstUtil install path. Returns 0 + sets *out_task_id
 *  on success; -1 + sets *out_err_code on failure (caller may then
 *  fall back to the BGFT path). The "success" return only means
 *  Sony accepted the install request — actual download completion
 *  is observed via appinst_install_status polling. */
static int appinst_install_start(const char *url,
                                  const char *content_id,
                                  const char *title,
                                  int32_t *out_task_id,
                                  uint32_t *out_err_code) {
    AppInstMetaInfo meta;
    memset(&meta, 0, sizeof(meta));
    meta.uri          = url;
    meta.ex_uri       = "";
    meta.playgo_scenario_id = "";
    meta.content_id   = "";
    meta.content_name = title ? title : "ps5upload";
    meta.icon_url     = "";

    AppInstPkgInfo pkg_info;
    memset(&pkg_info, 0, sizeof(pkg_info));
    /* Seed pkg_info.content_id with what we already know from the
     * caller's PKG header — Sony's installer overwrites this on
     * return, but seeding helps when the URL doesn't carry the id
     * itself. Truncate-safe. */
    strncpy(pkg_info.content_id, content_id, sizeof(pkg_info.content_id) - 1);

    AppInstPlayGoInfo playgo;
    memset(&playgo, 0, sizeof(playgo));

    int rc = sceAppInstUtilInstallByPackage(&meta, &pkg_info, &playgo);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        fprintf(stderr,
                "[bgft] appinst path failed: rc=%d (content_id=%s, url=%s)\n",
                rc, content_id, url);
        return -1;
    }

    int32_t tid = appinst_task_register(pkg_info.content_id);
    if (tid < 0) {
        *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
        return -1;
    }
    *out_task_id  = tid;
    *out_err_code = 0;
    return 0;
}

/** Poll an in-flight AppInstUtil install. Maps Sony's status string
 *  ("downloading"/"installing"/"playable") to our phase enum. */
static int appinst_install_status(int32_t task_id,
                                   bgft_phase_t *out_phase,
                                   uint64_t *out_downloaded,
                                   uint64_t *out_total,
                                   uint32_t *out_err_code) {
    char content_id[APPINST_CONTENTID_SIZE];
    if (appinst_task_lookup(task_id, content_id, sizeof(content_id)) != 0) {
        *out_err_code = BGFT_ERR_REGISTER_FAILED;
        return -1;
    }
    AppInstStatus st;
    memset(&st, 0, sizeof(st));
    int rc = sceAppInstUtilGetInstallStatus(content_id, &st);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        *out_phase = BGFT_PHASE_ERROR;
        return 0;
    }
    *out_downloaded = st.downloaded_size;
    *out_total      = st.total_size;
    *out_err_code   = (uint32_t)st.error_info.error_code;
    if (st.error_info.error_code != 0) {
        *out_phase = BGFT_PHASE_ERROR;
    } else if (strncmp(st.status, "playable", 8) == 0
            || strncmp(st.status, "completed", 9) == 0) {
        *out_phase = BGFT_PHASE_DONE;
    } else if (strncmp(st.status, "installing", 10) == 0) {
        *out_phase = BGFT_PHASE_INSTALL;
    } else if (strncmp(st.status, "downloading", 11) == 0) {
        *out_phase = BGFT_PHASE_DOWNLOAD;
    } else {
        /* "queued", "checking", anything else — treat as queued so
         * the UI keeps polling vs erroring out on a state we just
         * don't have a name for. */
        *out_phase = BGFT_PHASE_QUEUED;
    }
    return 0;
}

/* ─── Sony API: BGFT struct + bindings ───────────────────────────── */

/* Sony BGFT struct layout. ABI verified against the public PSDevWiki
 * layout and community PS4/PS5 references. */
struct bgft_init_params {
    void *mem;
    unsigned long size;
};

struct bgft_download_param {
    int user_id;
    int entitlement_type;
    const char *id;
    const char *content_url;
    const char *content_ex_url;
    const char *content_name;
    const char *icon_path;
    const char *sku_id;
    int option;                    /* BGFT_TASK_OPTION_* */
    const char *playgo_scenario_id;
    const char *release_date;
    const char *package_type;
    const char *package_sub_type;
    unsigned long package_size;
};

/* Progress query result. PSDevWiki layout — verified against BGFT
 * symbol probes; some fields are implementation-detail and ignored. */
struct bgft_progress {
    uint32_t error_result;
    uint32_t state;             /* 0=invalid, 1=queued, 2=running, 3=...; mapped to bgft_phase_t below */
    uint32_t local_state;
    uint64_t expected_total;
    uint64_t transferred_total;
    uint64_t inserted_total;
    uint32_t bits;
    uint32_t sub_status;
    uint32_t error_num;
    uint32_t reserved[3];
};

#define BGFT_TASK_OPTION_DISABLE_CDN_QUERY_PARAM 0x40

typedef int (*sce_bgft_initialize_fn)(struct bgft_init_params *);
typedef int (*sce_bgft_finalize_fn)(void);
typedef int (*sce_bgft_register_task_fn)(struct bgft_download_param *, int *);
typedef int (*sce_bgft_start_task_fn)(int);
typedef int (*sce_bgft_get_progress_fn)(int, struct bgft_progress *);

typedef int (*sce_user_service_initialize_fn)(void *);
typedef int (*sce_user_service_get_foreground_user_fn)(int *);
typedef int (*sce_app_inst_util_initialize_fn)(void);

/* ─── Module state ────────────────────────────────────────────────── */

static pthread_mutex_t g_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t g_init_once = PTHREAD_ONCE_INIT;
static int g_init_ok = 0;
static const char *g_unavailable_reason = NULL;

static void *g_lib_bgft = NULL;
static sce_bgft_initialize_fn      g_bgft_init = NULL;
static sce_bgft_register_task_fn   g_bgft_register = NULL;
static sce_bgft_start_task_fn      g_bgft_start = NULL;
static sce_bgft_get_progress_fn    g_bgft_progress = NULL;

static int g_foreground_user_id = 0;

/* 256 KiB BGFT working buffer — Sony retains this pointer for the
 * lifetime of the BGFT subsystem. Static lifetime keeps the buffer
 * alive across all Register/Start calls without manual refcounting. */
#define BGFT_HEAP_BYTES (256 * 1024)
static unsigned char g_bgft_heap[BGFT_HEAP_BYTES] __attribute__((aligned(16)));

/* ─── Init (one-shot) ─────────────────────────────────────────────── */

/** Try a list of candidate symbol names against `lib`, returning the
 *  first one that resolves. Different firmwares occasionally expose
 *  the same Sony function under slightly-different decorations
 *  (e.g. `sceBgftInitialize` vs `sceBgftServiceInitialize`); a
 *  multi-name probe lets the payload boot on any of them. Records
 *  the *last* attempted name in `*err_slot` when nothing resolved,
 *  so the surfaced error names a real symbol the user can search
 *  rather than the contents of an array. */
static void *resolve_any(
    const char *const *syms,
    size_t syms_count,
    void *lib,
    const char **err_slot
) {
    for (size_t i = 0; i < syms_count; i++) {
        (void)dlerror();
        void *p = dlsym(lib, syms[i]);
        if (p) return p;
    }
    if (*err_slot == NULL) {
        static char buf[200];
        /* Build a comma-separated list so the user can see all the
         * variants we tried — searching any one of them locates
         * the right symbol on psdevwiki / community references. */
        size_t off = 0;
        off += (size_t)snprintf(buf + off, sizeof(buf) - off,
                                "BGFT symbol missing (tried: ");
        for (size_t i = 0; i < syms_count && off + 2 < sizeof(buf); i++) {
            off += (size_t)snprintf(buf + off, sizeof(buf) - off,
                                    "%s%s", i == 0 ? "" : ", ", syms[i]);
        }
        if (off + 1 < sizeof(buf)) buf[off++] = ')';
        if (off < sizeof(buf)) buf[off] = '\0';
        else buf[sizeof(buf) - 1] = '\0';
        *err_slot = buf;
    }
    return NULL;
}

/** Candidate library paths. The canonical one is in /system/common/
 *  but some firmwares ship it via /system_ex/. We try them in order
 *  and stop at the first dlopen success. */
static const char *const BGFT_LIB_PATHS[] = {
    "/system/common/lib/libSceBgft.sprx",
    "/system_ex/common/lib/libSceBgft.sprx",
    "/system/priv/lib/libSceBgft.sprx",
};

static void bgft_init_locked(void) {
    /* Step 1: load libSceBgft.sprx. Try each candidate path; the
     * canonical /system/common/ first, but on firmwares that moved
     * it to /system_ex/ or /system/priv/ we fall through. RTLD_NOW
     * so we fail fast if the library can't be linked (vs hitting
     * an undefined symbol later). */
    static char dlerr_buf[256];
    dlerr_buf[0] = '\0';
    for (size_t i = 0; i < sizeof(BGFT_LIB_PATHS) / sizeof(BGFT_LIB_PATHS[0]); i++) {
        g_lib_bgft = dlopen(BGFT_LIB_PATHS[i], RTLD_NOW | RTLD_GLOBAL);
        if (g_lib_bgft) break;
        const char *e = dlerror();
        size_t off = strlen(dlerr_buf);
        snprintf(dlerr_buf + off, sizeof(dlerr_buf) - off,
                 "%s%s: %s", off == 0 ? "" : " | ",
                 BGFT_LIB_PATHS[i], e ? e : "(no error)");
    }
    if (!g_lib_bgft) {
        static char buf[320];
        snprintf(buf, sizeof(buf), "dlopen libSceBgft.sprx failed: %s",
                 dlerr_buf);
        g_unavailable_reason = buf;
        return;
    }

    /* Step 2: resolve every BGFT symbol we need, trying known
     * name variants per-symbol. Different firmwares expose the
     * same call under slightly different decorations; a missing
     * symbol after all variants means BGFT-via-this-API is not
     * usable on this firmware. */
    static const char *const SYM_INIT[]     = { "sceBgftInitialize",
                                                 "sceBgftServiceInitialize" };
    static const char *const SYM_REGISTER[] = { "sceBgftServiceDownloadRegisterTask",
                                                 "sceBgftServiceIntDownloadRegisterTask",
                                                 "sceBgftDownloadRegisterTask" };
    static const char *const SYM_START[]    = { "sceBgftServiceIntDownloadStartTask",
                                                 "sceBgftServiceDownloadStartTask",
                                                 "sceBgftDownloadStartTask" };
    static const char *const SYM_PROGRESS[] = { "sceBgftServiceDownloadGetProgress",
                                                 "sceBgftServiceIntDownloadGetProgress",
                                                 "sceBgftDownloadGetProgress" };
    g_bgft_init     = (sce_bgft_initialize_fn)
        resolve_any(SYM_INIT,     sizeof(SYM_INIT)/sizeof(SYM_INIT[0]),
                    g_lib_bgft, &g_unavailable_reason);
    g_bgft_register = (sce_bgft_register_task_fn)
        resolve_any(SYM_REGISTER, sizeof(SYM_REGISTER)/sizeof(SYM_REGISTER[0]),
                    g_lib_bgft, &g_unavailable_reason);
    g_bgft_start    = (sce_bgft_start_task_fn)
        resolve_any(SYM_START,    sizeof(SYM_START)/sizeof(SYM_START[0]),
                    g_lib_bgft, &g_unavailable_reason);
    g_bgft_progress = (sce_bgft_get_progress_fn)
        resolve_any(SYM_PROGRESS, sizeof(SYM_PROGRESS)/sizeof(SYM_PROGRESS[0]),
                    g_lib_bgft, &g_unavailable_reason);

    if (!g_bgft_init || !g_bgft_register || !g_bgft_start || !g_bgft_progress) {
        return;
    }

    /* Step 3: best-effort init of user service + foreground user.
     * sceUserServiceInitialize is in libSceUserService which is
     * linked at compile time, so dlsym(RTLD_DEFAULT) works. */
    sce_user_service_initialize_fn user_init =
        (sce_user_service_initialize_fn)dlsym(RTLD_DEFAULT, "sceUserServiceInitialize");
    sce_user_service_get_foreground_user_fn user_get_fg =
        (sce_user_service_get_foreground_user_fn)dlsym(RTLD_DEFAULT, "sceUserServiceGetForegroundUser");
    if (user_init) {
        /* Some FW need an init params struct; passing NULL falls back
         * to defaults which has worked in DPI's payload. */
        (void)user_init(NULL);
    }
    if (user_get_fg) {
        int u = 0;
        if (user_get_fg(&u) == 0 && u != 0) {
            g_foreground_user_id = u;
        }
    }

    /* Step 4: best-effort init of AppInstUtil. BGFT calls into it
     * during the install. Already linked at compile time. */
    sce_app_inst_util_initialize_fn app_init =
        (sce_app_inst_util_initialize_fn)dlsym(RTLD_DEFAULT, "sceAppInstUtilInitialize");
    if (app_init) {
        (void)app_init();
    }

    /* Step 5: BGFT init. Returns 0 on success. The 0x80990001
     * "already initialised" code is benign — DPI ignores it; we
     * treat it as success. */
    struct bgft_init_params ip = {
        .mem  = g_bgft_heap,
        .size = BGFT_HEAP_BYTES,
    };
    int rc = g_bgft_init(&ip);
    if (rc != 0 && (uint32_t)rc != 0x80990001u) {
        static char buf[160];
        snprintf(buf, sizeof(buf), "sceBgftInitialize failed: 0x%08X",
                 (unsigned)rc);
        g_unavailable_reason = buf;
        return;
    }

    g_init_ok = 1;
}

static void bgft_init_once(void) {
    pthread_mutex_lock(&g_mtx);
    bgft_init_locked();
    pthread_mutex_unlock(&g_mtx);
}

const char *bgft_install_unavailable_reason(void) {
    return g_unavailable_reason;
}

/* ─── Public API: install start ───────────────────────────────────── */

int bgft_install_start(const char *url,
                        const char *content_id,
                        uint64_t size,
                        const char *title,
                        const char *package_type,
                        int32_t *out_task_id,
                        uint32_t *out_err_code) {
    if (!url || !content_id || !out_task_id || !out_err_code) {
        if (out_err_code) *out_err_code = BGFT_ERR_REGISTER_FAILED;
        return -1;
    }
    *out_task_id = -1;
    *out_err_code = 0;

    /* AppInstUtil-first dispatch (added 2.2.44, etaHEN-style). The
     * AppInstUtil path is compile-time linked via -lSceAppInstUtil
     * so it doesn't suffer the dlopen/dlsym fragility that bites
     * BGFT on firmwares where libSceBgft.sprx is at a different
     * path or exports symbols under a different decoration. We try
     * AppInstUtil first and only fall through to the legacy BGFT
     * path if the AppInstUtil call itself returns nonzero — which
     * would be a Sony-side error, not a missing-binding error.
     *
     * The synthetic task_id we return has APPINST_TASK_ID_FLAG set
     * so bgft_install_status routes the poll back to the
     * AppInstUtil path; BGFT-issued task_ids stay in the natural
     * 0-N range and route through the BGFT poll. Single mutex
     * isn't shared between the two backends — they don't touch
     * each other's state. */
    int32_t app_tid = -1;
    uint32_t app_err = 0;
    if (appinst_install_start(url, content_id, title, &app_tid, &app_err) == 0) {
        *out_task_id = app_tid;
        *out_err_code = 0;
        return 0;
    }
    fprintf(stderr,
            "[bgft] appinst path failed (rc=0x%08X), falling back to BGFT\n",
            (unsigned)app_err);

    pthread_once(&g_init_once, bgft_init_once);
    if (!g_init_ok) {
        *out_err_code = BGFT_ERR_LIB_NOT_LOADABLE;
        return -1;
    }

    pthread_mutex_lock(&g_mtx);

    struct bgft_download_param p;
    memset(&p, 0, sizeof(p));
    p.user_id            = g_foreground_user_id;
    p.entitlement_type   = 5;
    p.id                 = content_id;
    p.content_url        = url;
    p.content_ex_url     = "";
    p.content_name       = title ? title : "";
    p.icon_path          = "";
    p.sku_id             = "";
    p.option             = BGFT_TASK_OPTION_DISABLE_CDN_QUERY_PARAM;
    p.playgo_scenario_id = "0";
    p.release_date       = "";
    p.package_type       = package_type ? package_type : "PS4GD";
    p.package_sub_type   = "";
    p.package_size       = (unsigned long)size;

    int task_id = -1;
    int rc = g_bgft_register(&p, &task_id);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        pthread_mutex_unlock(&g_mtx);
        fprintf(stderr,
                "[bgft] sceBgftServiceDownloadRegisterTask failed: 0x%08X "
                "(content_id=%s, url=%s)\n",
                (unsigned)rc, content_id, url);
        return -1;
    }

    rc = g_bgft_start(task_id);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        pthread_mutex_unlock(&g_mtx);
        fprintf(stderr,
                "[bgft] sceBgftServiceIntDownloadStartTask(%d) failed: 0x%08X\n",
                task_id, (unsigned)rc);
        return -1;
    }

    *out_task_id = task_id;
    pthread_mutex_unlock(&g_mtx);
    return 0;
}

/* ─── Public API: install status ──────────────────────────────────── */

/* Map BGFT's internal state code to our public phase enum. Codes
 * cross-referenced from PSDevWiki + DPI debugging notes:
 *   0/1 → queued
 *   2   → downloading
 *   3   → installing/preparing
 *   4   → done
 *   5+  → error
 * On any unknown state, treat as queued so the UI keeps polling
 * (vs erroring out on a state we just don't have a name for). */
static bgft_phase_t map_state(uint32_t state, uint32_t err) {
    if (err != 0) return BGFT_PHASE_ERROR;
    switch (state) {
        case 0:
        case 1: return BGFT_PHASE_QUEUED;
        case 2: return BGFT_PHASE_DOWNLOAD;
        case 3: return BGFT_PHASE_INSTALL;
        case 4: return BGFT_PHASE_DONE;
        default: return BGFT_PHASE_QUEUED;
    }
}

int bgft_install_status(int32_t task_id,
                         bgft_phase_t *out_phase,
                         uint64_t *out_downloaded,
                         uint64_t *out_total,
                         uint32_t *out_err_code) {
    if (!out_phase || !out_downloaded || !out_total || !out_err_code) {
        return -1;
    }
    *out_phase = BGFT_PHASE_QUEUED;
    *out_downloaded = 0;
    *out_total = 0;
    *out_err_code = 0;

    /* Route to whichever backend issued this task_id. AppInstUtil
     * task ids carry the high-bit flag; BGFT ids do not. */
    if ((task_id & APPINST_TASK_ID_FLAG) != 0) {
        return appinst_install_status(task_id, out_phase, out_downloaded,
                                       out_total, out_err_code);
    }

    pthread_once(&g_init_once, bgft_init_once);
    if (!g_init_ok) {
        *out_err_code = BGFT_ERR_LIB_NOT_LOADABLE;
        return -1;
    }

    pthread_mutex_lock(&g_mtx);
    struct bgft_progress prog;
    memset(&prog, 0, sizeof(prog));
    int rc = g_bgft_progress(task_id, &prog);
    pthread_mutex_unlock(&g_mtx);

    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        return -1;
    }

    *out_phase      = map_state(prog.state, prog.error_result);
    *out_downloaded = prog.transferred_total;
    *out_total      = prog.expected_total;
    *out_err_code   = prog.error_result;
    return 0;
}
