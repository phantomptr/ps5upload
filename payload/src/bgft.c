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

static void *resolve_or_log(const char *sym, void *lib, const char **err_slot) {
    /* Clear any pending dlerror state from a prior dlsym so that if
     * we want to fetch the message later (currently we just record
     * "symbol missing"), it's fresh. Recommended by the dlsym(3)
     * man page even when we treat NULL return as the error signal. */
    (void)dlerror();
    void *p = dlsym(lib, sym);
    if (!p && *err_slot == NULL) {
        static char buf[160];
        snprintf(buf, sizeof(buf), "BGFT symbol missing: %s", sym);
        *err_slot = buf;
    }
    return p;
}

static void bgft_init_locked(void) {
    /* Step 1: load libSceBgft.sprx. RTLD_NOW so we fail fast if the
     * library can't be linked (vs hitting an undefined symbol later). */
    g_lib_bgft = dlopen("/system/common/lib/libSceBgft.sprx",
                        RTLD_NOW | RTLD_GLOBAL);
    if (!g_lib_bgft) {
        static char buf[160];
        snprintf(buf, sizeof(buf), "dlopen libSceBgft.sprx failed: %s",
                 dlerror());
        g_unavailable_reason = buf;
        return;
    }

    /* Step 2: resolve every BGFT symbol we need. Any missing one
     * means BGFT is not usable on this firmware. */
    g_bgft_init     = (sce_bgft_initialize_fn)
        resolve_or_log("sceBgftInitialize", g_lib_bgft, &g_unavailable_reason);
    g_bgft_register = (sce_bgft_register_task_fn)
        resolve_or_log("sceBgftServiceDownloadRegisterTask", g_lib_bgft, &g_unavailable_reason);
    g_bgft_start    = (sce_bgft_start_task_fn)
        resolve_or_log("sceBgftServiceIntDownloadStartTask", g_lib_bgft, &g_unavailable_reason);
    g_bgft_progress = (sce_bgft_get_progress_fn)
        resolve_or_log("sceBgftServiceDownloadGetProgress", g_lib_bgft, &g_unavailable_reason);

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
