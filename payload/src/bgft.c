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
#include <sys/types.h>
#include <unistd.h>

#include "bgft.h"
#include "shellui_rpc.h"

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

extern int sceAppInstUtilInitialize(void);
extern int sceAppInstUtilInstallByPackage(AppInstMetaInfo *meta,
                                          AppInstPkgInfo *pkg_info,
                                          AppInstPlayGoInfo *playgo);
extern int sceAppInstUtilGetInstallStatus(const char *content_id,
                                          AppInstStatus *out);
/* 2.2.54-fix-round-7: pre-install cancel for stuck tasks. Sony
 * retains queued install tasks across our payload restarts; once a
 * task for content_id X is queued, subsequent same-content_id
 * registers return 0x80B21106 until the prior task completes or is
 * cancelled. We call cancel just before install to clear any
 * orphan tasks (e.g. installs that registered earlier this session
 * but never observed-as-completed because we don't poll status). */
extern int sceAppInstUtilCancelInstall(const char *content_id);

/** One-shot initializer for the AppInstUtil subsystem. Without
 *  this, sceAppInstUtilInstallByPackage typically returns
 *  SCE_APP_INST_UTIL_ERROR_NOT_INITIALIZED (-2136797184). etaHEN
 *  initializes this implicitly via daemon startup; we don't have
 *  a daemon-level startup hook so we lazy-init at first install
 *  attempt. Idempotent return values from Sony's installer (e.g.
 *  "already initialized") are absorbed silently — what we care
 *  about is "the next call to InstallByPackage will work." */
static pthread_once_t g_appinst_init_once = PTHREAD_ONCE_INIT;
static int g_appinst_init_rc = 0;
static void appinst_init_locked(void) {
    g_appinst_init_rc = sceAppInstUtilInitialize();
    if (g_appinst_init_rc != 0) {
        fprintf(stderr,
                "[bgft] sceAppInstUtilInitialize returned 0x%08X "
                "(non-fatal — common 'already initialised' codes are absorbed)\n",
                (unsigned)g_appinst_init_rc);
    }
}

/* Backend tag for in-flight install tracking. The bgft.h interface
 * returns a `task_id` that callers poll later via
 * `bgft_install_status`; we use a synthetic 32-bit id space (high
 * bit = appinst, lower bits = index into our task table) so a
 * single status call can route to the right backend without the
 * caller having to know which one was used. */
#define APPINST_TASK_ID_FLAG       0x40000000
/* 2.2.53-fix: orthogonal flag set on top of APPINST_TASK_ID_FLAG when
 * the install was registered via shellui-rpc (Tier 1) instead of the
 * in-process AppInstUtil path (Tier 2). For shellui-rpc tasks we MUST
 * NOT call sceAppInstUtilGetInstallStatus from our process — observed
 * crash on FW 9.60: Sony's status API segfaults the caller when the
 * install was kicked off from a different process. The status handler
 * detects this flag and returns a synthetic phase=INSTALL forever; the
 * actual install runs to completion on the PS5 and the user verifies
 * via the PS5's notification panel. */
#define APPINST_VIA_SHELLUI_FLAG   0x20000000
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
 *  or -1 on a full table.
 *
 *  2.2.54-fix-round-11: reuse existing slot for the same content_id
 *  rather than always allocating fresh. Without this, repeated
 *  installs of the same pkg (common during testing or retries)
 *  exhaust the 16-slot table within a session — even though they
 *  refer to the same underlying Sony task. */
static int32_t appinst_task_register(const char *content_id) {
    pthread_mutex_lock(&g_appinst_mtx);
    /* First pass: existing slot for the same content_id? */
    for (int i = 0; i < APPINST_TASK_TABLE; i++) {
        if (g_appinst_tasks[i].in_use &&
            strncmp(g_appinst_tasks[i].content_id, content_id,
                    sizeof(g_appinst_tasks[i].content_id)) == 0) {
            pthread_mutex_unlock(&g_appinst_mtx);
            return (int32_t)(APPINST_TASK_ID_FLAG | (uint32_t)i);
        }
    }
    /* Second pass: first free slot. */
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

/** Free a slot when the install task reaches a terminal state
 *  (DONE/ERROR per Sony). Releases the slot for future registers
 *  with different content_ids. Called from appinst_install_status
 *  on phase transitions. Safe to call on already-free slots
 *  (idempotent). */
static void appinst_task_release(int32_t task_id) {
    if ((task_id & APPINST_TASK_ID_FLAG) == 0) return;
    int idx = task_id & ~(APPINST_TASK_ID_FLAG | APPINST_VIA_SHELLUI_FLAG);
    if (idx < 0 || idx >= APPINST_TASK_TABLE) return;
    pthread_mutex_lock(&g_appinst_mtx);
    g_appinst_tasks[idx].in_use = 0;
    g_appinst_tasks[idx].content_id[0] = '\0';
    pthread_mutex_unlock(&g_appinst_mtx);
}

/** Look up the content_id for an appinst-tagged task_id. Returns
 *  0 + populates `out_content_id` on hit; -1 on miss / wrong tag.
 *
 *  2.2.54-fix-round-15: also strip APPINST_VIA_SHELLUI_FLAG when
 *  computing idx. The flag is OR'd into the synthetic task_id at
 *  shellui-rpc register time; without stripping it here, idx
 *  becomes 0x20000000 (huge) and the slot lookup fails — surfaces
 *  as `phase=queued, err=0xE0000004 BGFT_ERR_REGISTER_FAILED` on
 *  every status poll for shellui-rpc-backed tasks. */
static int appinst_task_lookup(int32_t task_id, char *out, size_t out_cap) {
    if ((task_id & APPINST_TASK_ID_FLAG) == 0) return -1;
    int idx = task_id & ~(APPINST_TASK_ID_FLAG | APPINST_VIA_SHELLUI_FLAG);
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

/* ShellCore's authid — required by sceAppInstUtilInstallByPackage and
 * sceAppInstUtilGetInstallStatus on PS5. Confirmed via etaHEN's
 * pkg-writeup.md: "Modify the auth ID within the struct ucred
 * authentication information to match ShellCore's identifier
 * (0x3800000000000010)". Without this the call returns
 * 0x80B22404 (PlayGo HTTP_404 — Sony rejects the URL pre-flight from
 * non-ShellCore authids on FW 9.60+) for HTTP URLs, or 0x80B21106
 * (parser/parameter rejection) for file:// URLs. The default
 * jailbreak elevation sets us to 0x4800000000000006 (debugger
 * authid) which is right for ptrace + kernel R/W but wrong for
 * Sony's install API gates. */
#define PS5_SHELLCORE_AUTHID 0x3800000000000010ull

/* External — defined in main.c, exposed via runtime.h. We re-declare
 * here rather than pulling kernel.h into bgft.c. */
extern uint64_t kernel_get_ucred_authid(pid_t pid);
extern int kernel_set_ucred_authid(pid_t pid, uint64_t authid);

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
    /* pkg_info.content_id is OUTPUT per etaHEN's reference. Sony's
     * installer extracts the content_id from the pkg header on
     * return; seeding it caused 0x80B21106 parser-error in testing. */

    AppInstPlayGoInfo playgo;
    memset(&playgo, 0, sizeof(playgo));

    /* Swap to ShellCore authid for the entire init+install window.
     * etaHEN's payload runs with ShellCore authid persistently —
     * sceAppInstUtilInitialize sees ShellCore authid when it sets up
     * its IPC handles to Sony's installer daemon, and that state
     * persists across the InstallByPackage call. Calling Initialize
     * with debugger authid (0x4800) primes Sony's daemon with wrong
     * IPC peer info, which surfaces later as 0x80020003 ESRCH ("no
     * such process") because Sony tries to IPC back to a debugger-
     * authid peer that the install path doesn't know how to talk to.
     *
     * Pre-fix-round-5 (this round): the swap only wrapped the install
     * call. Init ran under debugger authid first, then we swapped
     * for the install — too late, init had already cached the wrong
     * peer. Now both calls run under ShellCore authid. */
    pid_t mypid = getpid();
    uint64_t saved_authid = kernel_get_ucred_authid(mypid);
    int authid_swapped = 0;
    if (saved_authid != 0) {
        if (kernel_set_ucred_authid(mypid, PS5_SHELLCORE_AUTHID) == 0) {
            authid_swapped = 1;
        } else {
            fprintf(stderr,
                    "[bgft] WARN: failed to swap to ShellCore authid; "
                    "install call will run with current authid (0x%llx)\n",
                    (unsigned long long)saved_authid);
        }
    }

    /* Init Sony's installer subsystem under ShellCore authid. The
     * pthread_once guard makes this idempotent across installs; the
     * FIRST install's pthread_once-controlled invocation sets up
     * Sony's IPC handles correctly. */
    pthread_once(&g_appinst_init_once, appinst_init_locked);

    /* Direct testing showed adding sceAppInstUtilCancelInstall HERE
     * (in addition to the shellui-rpc tier's cancel) regressed
     * register success — calls that previously returned err_code=0
     * started returning 0x80B21106 after the cancel was added. The
     * suspected mechanism: cancel() flushes IPC state Sony's daemon
     * needs for the immediately-following InstallByPackage on the
     * same authid. Keep cancel ONLY in the shellui-rpc tier where
     * it's been proven non-destructive. */

    int rc = sceAppInstUtilInstallByPackage(&meta, &pkg_info, &playgo);

    /* Restore prior authid. Best-effort: if restore fails, our
     * process keeps ShellCore authid which would silently break
     * subsequent ptrace ops. Log loudly so the diag surfaces it. */
    if (authid_swapped) {
        if (kernel_set_ucred_authid(mypid, saved_authid) != 0) {
            fprintf(stderr,
                    "[bgft] WARN: failed to restore authid 0x%llx after "
                    "InstallByPackage; subsequent ptrace may fail.\n",
                    (unsigned long long)saved_authid);
        }
    }
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

    /* GetInstallStatus also requires ShellCore authid (same gate as
     * InstallByPackage). Calling it from our default debugger authid
     * was the crash source observed pre-2.2.53-fix-round-3 — the call
     * dereferences cross-process state the kernel won't let a non-
     * ShellCore caller see, and the segfault propagates back to us.
     * Same swap+restore pattern as appinst_install_start. */
    pid_t mypid = getpid();
    uint64_t saved_authid = kernel_get_ucred_authid(mypid);
    if (saved_authid != 0) {
        (void)kernel_set_ucred_authid(mypid, PS5_SHELLCORE_AUTHID);
    }

    int rc = sceAppInstUtilGetInstallStatus(content_id, &st);

    if (saved_authid != 0) {
        (void)kernel_set_ucred_authid(mypid, saved_authid);
    }

    if (rc != 0) {
        /* Hard error from GetInstallStatus — the task is dead.
         * Release the slot here too; the terminal-state release
         * block below is unreachable on this path because we
         * early-return. Without this the 16-slot table leaks one
         * entry per status-call failure (e.g. content_id Sony
         * already forgot about), and the next install with a
         * different content_id eventually hits TASK_TABLE_FULL. */
        *out_err_code = (uint32_t)rc;
        *out_phase = BGFT_PHASE_ERROR;
        appinst_task_release(task_id);
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
    } else if (strncmp(st.status, "installing", 10) == 0
            || strncmp(st.status, "promoting", 9) == 0) {
        /* etaHEN's writeup confirms Sony's actual state strings:
         * transferring → promoting → playable (or error/none).
         * "installing" was our pre-2.2.53 best-guess; keep it as
         * an alias since some firmwares may still emit it. */
        *out_phase = BGFT_PHASE_INSTALL;
    } else if (strncmp(st.status, "downloading", 11) == 0
            || strncmp(st.status, "transferring", 12) == 0) {
        *out_phase = BGFT_PHASE_DOWNLOAD;
    } else {
        /* "queued", "checking", "none", anything else — treat as
         * queued so the UI keeps polling vs erroring out on a state
         * we just don't have a name for. */
        *out_phase = BGFT_PHASE_QUEUED;
    }
    /* 2.2.54-fix-round-11: free the slot on terminal transitions so
     * a future install with a different content_id can reuse it.
     * Without this, the 16-slot table fills up after a single
     * session of repeated installs (or errored installs that leave
     * the slot allocated). The host's polling loop calls status
     * one more time after seeing DONE/ERROR; that final poll
     * lookup will fail (slot freed), surface as
     * BGFT_ERR_REGISTER_FAILED, but the host already transitioned
     * the row to terminal state — no UX impact. */
    if (*out_phase == BGFT_PHASE_DONE || *out_phase == BGFT_PHASE_ERROR) {
        appinst_task_release(task_id);
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
/* IntDebug Register variant — sceBgftServiceIntDebugDownloadRegisterPkg.
 * Best-effort symbol resolution; null on firmwares that don't expose it.
 * For PS4-fake-pkg-on-PS5 (the canonical jailbroken-PS5 install workflow)
 * this is the path Sony's installer uses when entitlement_type=5 ("debug
 * install"); without it, regular Register tends to reject fakepkgs with
 * 0x8099003x-class entitlement errors. DPI's main.c:72 + main.c:148-159
 * is the reference for the regular→debug fallback sequence we mirror in
 * `bgft_install_start`. Probed alongside g_bgft_register and treated as
 * an optional capability.
 *
 * Why a separate global rather than overwriting g_bgft_register: we want
 * the regular path available too — Sony-signed pkgs (the rare case our
 * users hit) need regular Register; the host-side decision of which to
 * try first depends on the pkg header's signed-vs-fake state which we
 * don't always know in advance. */
static sce_bgft_register_task_fn   g_bgft_register_debug = NULL;
/* Most recent Register-path tag, surfaced via
 * bgft_install_last_register_path(). Updated under g_mtx within
 * bgft_install_start. The "none" sentinel covers two distinct cases —
 * "init never ran" and "BGFT is unavailable on this FW" — which the
 * host UI distinguishes by also reading bgft_install_unavailable_reason(). */
static const char *g_last_register_path = "none";
/* Per-tier error codes from the most recent install attempt, surfaced
 * via bgft_install_last_*_err(). Set unconditionally on every attempt
 * (success: 0, failure: error code). Pre-2.2.54-fix-round-8 used
 * UINT32_MAX as the "not attempted" sentinel — but that VALUE
 * collides with `pt_call` returning -1 (cast to uint32 = 0xFFFFFFFF =
 * UINT32_MAX), making real tier failures appear as "tier never
 * attempted" in the diagnostic. Now using a separate `_set` boolean
 * flag so the value can be any 32-bit number including 0xFFFFFFFF. */
static uint32_t g_last_shellui_err = 0;
static int      g_last_shellui_err_set = 0;
static uint32_t g_last_appinst_err = 0;
static int      g_last_appinst_err_set = 0;
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
 *  multi-name probe lets the payload boot on any of them. On a
 *  resolution failure, writes a comma-separated list of the
 *  attempted symbol names into the caller-provided `err_buf` so
 *  the surfaced diagnostic names every variant we tried.
 *
 *  Pre-fix-round-5 used a SINGLE process-wide `static char buf` here.
 *  Two callers — the four required-symbol probes (writing to
 *  `g_unavailable_reason`) and the IntDebug-Register probe (writing
 *  to a local `unused_err`) — both ended up pointing at the same
 *  static memory. When the IntDebug probe later wrote to the buf,
 *  it CLOBBERED whatever earlier diagnostic message
 *  `g_unavailable_reason` was already pointing at. The user would
 *  see "BGFT symbol missing (tried: sceBgftServiceIntDebugDownloadRegisterPkg…)"
 *  even when the real failure was "sceBgftInitialize couldn't be
 *  resolved" or "dlopen failed". Caller-provided buffers keep each
 *  failure message in its own memory, so attribution stays correct. */
static void *resolve_any(
    const char *const *syms,
    size_t syms_count,
    void *lib,
    char *err_buf,
    size_t err_cap
) {
    for (size_t i = 0; i < syms_count; i++) {
        (void)dlerror();
        void *p = dlsym(lib, syms[i]);
        if (p) return p;
    }
    if (err_buf && err_cap > 0) {
        /* Build a comma-separated list so the user can see all the
         * variants we tried — searching any one of them locates
         * the right symbol on psdevwiki / community references. */
        size_t off = 0;
        off += (size_t)snprintf(err_buf + off, err_cap - off,
                                "BGFT symbol missing (tried: ");
        for (size_t i = 0; i < syms_count && off + 2 < err_cap; i++) {
            off += (size_t)snprintf(err_buf + off, err_cap - off,
                                    "%s%s", i == 0 ? "" : ", ", syms[i]);
        }
        if (off + 1 < err_cap) err_buf[off++] = ')';
        if (off < err_cap) err_buf[off] = '\0';
        else err_buf[err_cap - 1] = '\0';
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
                                                 "sceBgftServiceInitialize",
                                                 "sceBgftServiceIntInit" };
    static const char *const SYM_REGISTER[] = { "sceBgftServiceDownloadRegisterTask",
                                                 "sceBgftServiceIntDownloadRegisterTask",
                                                 "sceBgftDownloadRegisterTask" };
    /* Debug-Register variant — accepts fakepkgs (PS4 fake-signed pkgs
     * being installed on a jailbroken PS5 in PS4 emulation mode is the
     * canonical workflow). DPI's main.c falls back to this when the
     * regular Register returns 0x80990088 ("DUPLICATE_TASK") or any
     * other rejection. We probe it separately so a missing symbol on
     * one firmware doesn't poison the regular-Register path. */
    static const char *const SYM_REGISTER_DEBUG[] = {
        "sceBgftServiceIntDebugDownloadRegisterPkg",
        /* Older firmwares occasionally exposed a non-Service-prefixed
         * variant; probe both shapes. */
        "sceBgftDebugDownloadRegisterPkg" };
    static const char *const SYM_START[]    = { "sceBgftServiceIntDownloadStartTask",
                                                 "sceBgftServiceDownloadStartTask",
                                                 "sceBgftDownloadStartTask" };
    static const char *const SYM_PROGRESS[] = { "sceBgftServiceDownloadGetProgress",
                                                 "sceBgftServiceIntDownloadGetProgress",
                                                 "sceBgftDownloadGetProgress" };
    /* Each probe writes its diagnostic into its own static buffer so
     * a later failure can't clobber an earlier one (see resolve_any
     * doc). After all four required probes run, we surface the FIRST
     * non-empty buffer as g_unavailable_reason — that's the
     * earliest failure in the resolution chain, which is the most
     * actionable diagnosis. */
    static char init_err[200];
    static char register_err[200];
    static char start_err[200];
    static char progress_err[200];
    static char debug_err[200];
    init_err[0] = '\0';
    register_err[0] = '\0';
    start_err[0] = '\0';
    progress_err[0] = '\0';
    debug_err[0] = '\0';

    g_bgft_init     = (sce_bgft_initialize_fn)
        resolve_any(SYM_INIT,     sizeof(SYM_INIT)/sizeof(SYM_INIT[0]),
                    g_lib_bgft, init_err, sizeof(init_err));
    g_bgft_register = (sce_bgft_register_task_fn)
        resolve_any(SYM_REGISTER, sizeof(SYM_REGISTER)/sizeof(SYM_REGISTER[0]),
                    g_lib_bgft, register_err, sizeof(register_err));
    /* IntDebug-Register is best-effort — its absence doesn't block
     * BGFT init. Recorded in `debug_err` for diagnostic surfacing
     * via `bgft_install_intdebug_available()` rather than for
     * the unavailable-reason path; fakepkg installs degrade to
     * regular Register on this FW. */
    g_bgft_register_debug = (sce_bgft_register_task_fn)
        resolve_any(SYM_REGISTER_DEBUG,
                    sizeof(SYM_REGISTER_DEBUG)/sizeof(SYM_REGISTER_DEBUG[0]),
                    g_lib_bgft, debug_err, sizeof(debug_err));
    g_bgft_start    = (sce_bgft_start_task_fn)
        resolve_any(SYM_START,    sizeof(SYM_START)/sizeof(SYM_START[0]),
                    g_lib_bgft, start_err, sizeof(start_err));
    g_bgft_progress = (sce_bgft_get_progress_fn)
        resolve_any(SYM_PROGRESS, sizeof(SYM_PROGRESS)/sizeof(SYM_PROGRESS[0]),
                    g_lib_bgft, progress_err, sizeof(progress_err));

    /* Aggregate first-failure into g_unavailable_reason. Order
     * mirrors the natural dependency chain: Init must work before
     * Register; Register before Start; Start before Progress.
     * IntDebug is intentionally excluded — its absence is a
     * BGFT capability the host's diagnostic disclosure surfaces
     * via `intdebug_avail`, not a fatal init failure. */
    if (!g_unavailable_reason) {
        if (init_err[0])          g_unavailable_reason = init_err;
        else if (register_err[0]) g_unavailable_reason = register_err;
        else if (start_err[0])    g_unavailable_reason = start_err;
        else if (progress_err[0]) g_unavailable_reason = progress_err;
    }

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

const char *bgft_install_last_register_path(void) {
    /* Plain pointer read — `g_last_register_path` only ever points at
     * one of three string literals ("intdebug" / "regular" / "none"),
     * so a torn read still produces a valid pointer. Mutex avoidance
     * keeps PKG_INSTALL_STATUS responses cheap. */
    return g_last_register_path;
}

uint32_t bgft_install_last_shellui_err(void) {
    return g_last_shellui_err;
}

int bgft_install_last_shellui_err_set(void) {
    return g_last_shellui_err_set;
}

uint32_t bgft_install_last_appinst_err(void) {
    return g_last_appinst_err;
}

int bgft_install_last_appinst_err_set(void) {
    return g_last_appinst_err_set;
}

int bgft_install_intdebug_available(void) {
    return g_bgft_register_debug != NULL ? 1 : 0;
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
    /* Path 0: ShellUI RPC install. Sony's PlayGo whitelists ShellUI's
     * process attributes for HTTP fetch, so calling
     * sceAppInstUtilInstallByPackage from inside ShellUI's process
     * succeeds where the same call from our own context returns
     * 0x80B22404 (PlayGo HTTP 404 pre-flight) on FW 9.60+ regardless
     * of cred-forge.
     *
     * On success, we synthesize a task_id with APPINST_TASK_ID_FLAG
     * set and register a slot so bgft_install_status routes the
     * status poll back through sceAppInstUtilGetInstallStatus
     * (still works from our process for status — it's only the
     * install-START that has the caller-pid check). The content_id
     * the slot needs is whatever we passed in; ShellUI's installer
     * fills in pkg_info.content_id but we don't read that back yet
     * (Sony's GetInstallStatus accepts a caller-supplied content_id
     * just fine). */
    /* Reset per-tier diagnostics so a previous attempt's values
     * don't leak into this one (the host always wants the *current*
     * attempt's tier outcomes, not whatever the last call left). */
    g_last_shellui_err = 0;
    g_last_shellui_err_set = 0;
    g_last_appinst_err = 0;
    g_last_appinst_err_set = 0;
    /* Also reset g_last_register_path. A previous SUCCESSFUL attempt
     * leaves the tag as "shellui-rpc"/"appinst"/"intdebug"/"regular";
     * if THIS attempt fails before any tier sets a fresh value, the
     * host would surface the stale tag from the prior success and
     * confuse the user ("register_path=shellui-rpc but task_id=-1?").
     * "none" is the correct sentinel for "no tier succeeded yet". */
    g_last_register_path = "none";

    {
        /* Lazily init the ShellUI RPC layer if it hasn't been touched
         * yet (find ShellUI's pid + resolve symbols inside its address
         * space). Without this call, shellui_rpc_ready() returns
         * false and shellui_rpc_install_pkg short-circuits with
         * rc=-1 / err=0 — Tier 1 silently never runs. The launcher
         * code path (register.c) does the same lazy init. Idempotent;
         * subsequent calls are no-ops. */
        (void)shellui_rpc_init();
        uint32_t shellui_err = 0;
        int rc = shellui_rpc_install_pkg(url, content_id, title,
                                          &shellui_err);
        /* Record the outcome unconditionally — both rc and shellui_err
         * matter for diagnostics. shellui_err = 0 on full success;
         * non-zero either captures Sony's error code (when the call
         * dispatched) or our own machinery error (0xE000_xxxx range)
         * when it didn't. The host renders this in the diag panel. */
        g_last_shellui_err = shellui_err;
        g_last_shellui_err_set = 1;
        if (rc == 0) {
            int32_t tid = appinst_task_register(content_id);
            if (tid >= 0) {
                /* OR in the shellui-rpc backing flag so
                 * bgft_install_status can route AROUND Sony's
                 * GetInstallStatus (which segfaults our process
                 * for shellui-rpc-initiated installs on FW 9.60). */
                *out_task_id = tid | APPINST_VIA_SHELLUI_FLAG;
                *out_err_code = 0;
                g_last_register_path = "shellui-rpc";
                return 0;
            }
            /* Task table full — install was queued PS5-side, but we
             * can't track its status. Surface as a soft success: the
             * install will run, the user just won't see live progress
             * on this row. Fall through to register/start tracking
             * via the legacy AppInstUtil path so we get a tracked
             * task_id; if THAT also succeeds, the in-flight ShellUI
             * install will collide with the new register. Better to
             * fail loud here than silently double-install. */
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
        /* ShellUI RPC failed. Common reasons:
         *   - libSceAppInstUtil not loaded into ShellUI's address
         *     space (shellui_err = 0xE0000002 SYMBOL_MISSING).
         *   - ptrace attach failed (kstuff not loaded; shellui_err = 0).
         *   - Sony rejected with a real error code (passed through).
         * In all cases, try the in-process AppInstUtil path below as
         * a fallback. The shellui_err is preserved into saved_app_err
         * by the legacy code so it can be surfaced if the fallback
         * also fails. */
        if (shellui_err != 0) {
            fprintf(stderr,
                    "[bgft] shellui-rpc install failed (rc=0x%08X), trying in-process AppInstUtil\n",
                    (unsigned)shellui_err);
        } else {
            fprintf(stderr,
                    "[bgft] shellui-rpc install attach/mmap failed, trying in-process AppInstUtil\n");
        }
    }

    int32_t app_tid = -1;
    uint32_t app_err = 0;
    int appinst_rc = appinst_install_start(url, content_id, title, &app_tid, &app_err);
    g_last_appinst_err = app_err;
    g_last_appinst_err_set = 1;
    if (appinst_rc == 0) {
        *out_task_id = app_tid;
        *out_err_code = 0;
        /* Tag the path so PKG_INSTALL_ACK / STATUS_ACK don't lie about
         * which install path succeeded. Pre-fix-round-3 the AppInstUtil
         * success branch never wrote g_last_register_path, so a status
         * poll would surface a stale "intdebug" / "regular" left over
         * from a prior install — misleading the host's diagnostic
         * disclosure. */
        g_last_register_path = "appinst";
        return 0;
    }
    fprintf(stderr,
            "[bgft] appinst path failed (rc=0x%08X), falling back to BGFT\n",
            (unsigned)app_err);

    /* Save the AppInstUtil error so we can surface it as the
     * primary cause if BGFT also fails. Without this, the
     * fall-through used to clobber `*out_err_code` with
     * BGFT_ERR_LIB_NOT_LOADABLE — which is technically true (BGFT
     * isn't loadable on this firmware) but tells the user the
     * wrong story: the install fundamentally failed because Sony
     * rejected our InstallByPackage call, NOT because BGFT
     * couldn't load. The user's debug path goes the wrong way.
     * Now if both backends fail, we surface the AppInstUtil error
     * (it's more actionable: "Sony rejected this PKG with code X"
     * vs "lib not loadable"). The fprintf above keeps the BGFT
     * fallback attempt visible in the payload log. */
    const uint32_t saved_app_err = app_err;

    pthread_once(&g_init_once, bgft_init_once);
    if (!g_init_ok) {
        /* Prefer the AppInstUtil error when we have one — it's
         * the real cause. Fall through to BGFT_ERR_LIB_NOT_LOADABLE
         * only if AppInstUtil somehow returned 0 in app_err
         * (shouldn't happen — we got here because appinst_install_start
         * returned -1, so app_err is set, but defense-in-depth). */
        *out_err_code = saved_app_err != 0
                          ? saved_app_err
                          : BGFT_ERR_LIB_NOT_LOADABLE;
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
    int rc = -1;
    /* Register fallback ladder, mirroring DPI's main.c:148-159:
     *   1. IntDebug variant first when available — the canonical path
     *      for fakepkg installs on a jailbroken PS5. Sony's regular
     *      Register applies entitlement checks that reject any pkg
     *      without a Sony-signed license; the Debug variant skips
     *      that check, which is exactly what PS4-fake-on-PS5 needs.
     *   2. Regular Register as fallback — for Sony-signed pkgs (rare
     *      in our user base) and for firmwares where the Debug
     *      symbol couldn't be resolved.
     *   3. If IntDebug returns 0x80990088 (DUPLICATE_TASK), the task
     *      was already queued — fall through to regular Register
     *      which BGFT will treat as a re-attach. DPI handles this
     *      branch the same way.
     *
     * The cred-forge that lets the Int-family symbols actually run is
     * already applied process-wide via runtime_apply_ucred_jailbreak()
     * (see main.c:107). Each incoming frame re-asserts elevation, so
     * we don't need a per-call forge/restore here. */
    int debug_rc = 0;
    if (g_bgft_register_debug) {
        debug_rc = g_bgft_register_debug(&p, &task_id);
        if (debug_rc == 0) {
            rc = 0;
            g_last_register_path = "intdebug";
        } else if ((uint32_t)debug_rc == 0x80990088u) {
            /* Duplicate task — try regular Register so BGFT can
             * re-attach to the existing task instead of erroring out.
             * Set the path tag eagerly so a failure on the regular
             * retry still reports the variant that actually ran;
             * pre-fix this branch only updated the tag on success,
             * leaving `g_last_register_path` at its prior-call value
             * (e.g. "intdebug" from the previous install) on a
             * failed retry — misleading the host's diagnostics. */
            task_id = -1;
            g_last_register_path = "regular";
            rc = g_bgft_register(&p, &task_id);
        } else {
            rc = debug_rc;
            g_last_register_path = "intdebug";
        }
    } else {
        /* No IntDebug symbol on this firmware — fakepkg install will
         * likely fail with an entitlement error, but try regular
         * Register anyway in case the user has a Sony-signed pkg.
         * Set the tag before the call so a failure here also reports
         * "regular" rather than the previous-call leftover. */
        g_last_register_path = "regular";
        rc = g_bgft_register(&p, &task_id);
    }
    if (rc != 0) {
        /* Prefer the saved AppInstUtil error if we have one — it's
         * the real cause of the install failure. The BGFT register
         * failure here is a *secondary* symptom of running on
         * firmware where neither install path works. The
         * AppInstUtil-side error is what the humanizer maps to
         * actionable copy. */
        *out_err_code = saved_app_err != 0 ? saved_app_err : (uint32_t)rc;
        pthread_mutex_unlock(&g_mtx);
        fprintf(stderr,
                "[bgft] sceBgftServiceDownloadRegisterTask failed: 0x%08X "
                "(content_id=%s, url=%s)\n",
                (unsigned)rc, content_id, url);
        return -1;
    }

    rc = g_bgft_start(task_id);
    if (rc != 0) {
        /* Same logic: prefer the AppInstUtil-side error code if it
         * was set during the AppInstUtil-first attempt. */
        *out_err_code = saved_app_err != 0 ? saved_app_err : (uint32_t)rc;
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
     * task ids carry the high-bit flag; BGFT ids do not.
     *
     * 2.2.54-fix-round-9: route shellui-rpc-backed tasks through
     * Sony's GetInstallStatus too. The previous synthetic-phase
     * bypass was defensive — needed when GetInstallStatus crashed
     * the payload because we called it under debugger authid. With
     * appinst_install_status now wrapping the call in the ShellCore
     * authid swap (verified safe via 5 consecutive polls without
     * crash), real status polling works for shellui-rpc tasks too.
     * The user gets actual download/install progress in the UI
     * AND the row transitions from install→done when Sony completes,
     * which clears Sony's queue and lets the next install register. */
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
