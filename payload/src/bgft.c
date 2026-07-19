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
 *    buffer. We allocate 256 KiB — Sony retains the pointer for the
 *    lifetime of the subsystem, so the buffer is static (held forever).
 *    Must outlive every Register/Start call.
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
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

#include "authid.h"
#include "bgft.h"
#include "shellui_rpc.h"
#include "sony_api_lock.h"
#include "kernel_rw_lock.h"

/* ─── AppInstUtil install path ─────────────────────────────────────
 *
 * sceAppInstUtilInstallByPackage is Sony's high-level "fetch this URL,
 * decrypt, install" entry point. It's compile-time linked via
 * -lSceAppInstUtil so there's no dlopen/dlsym dance and no per-firmware
 * symbol-name drift. We try this path first; if it fails (rare — would
 * mean libSceAppInstUtil itself isn't loaded), we fall back to the BGFT
 * path below for legacy firmwares where BGFT works but AppInstUtil
 * doesn't.
 *
 * Struct layouts match Sony's AppInstUtil ABI (psdevwiki reference). */

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
/* Local-path installer (2.20.2 — elf-arsenal/ShadowMount approach).
 * Sony's dedicated "install this .pkg that already lives on the
 * console's local disk" entry point. Unlike InstallByPackage — which
 * parses `meta.uri` as a *URI* and on FW 9.60+ rejects both HTTP fetches
 * (0x80B22404 PlayGo HTTP pre-flight) and bare absolute paths (no scheme
 * to parse) — this takes a raw absolute path and reads the bytes
 * straight off local disk. No PlayGo HTTP gate, no process-context
 * whitelist. This is the path elf-arsenal uses as its primary local
 * installer; we adopt it for staged (Tier-1) installs where the .pkg is
 * already under /user/data. `pkg_info.content_id` is OUTPUT (filled from
 * the pkg header on success).
 *
 * Resolved via dlsym(RTLD_DEFAULT) at call time rather than direct-linked
 * — see [[reference_ps5_sdk_stub_vs_sprx]]: the SDK's libSceAppInstUtil
 * stub exports a SUPERSET of what the on-console runtime SPRX actually
 * provides, so eager-binding a symbol the runtime lacks would fail the
 * dynamic loader and silently kill main() (no-boot regression). This tier
 * is non-essential (InstallByPackage is the fallback), so a lazy resolve
 * that gracefully degrades to NULL is the correct discipline. */
typedef int (*sce_app_inst_util_app_install_pkg_fn)(const char *path,
                                                    AppInstPkgInfo *pkg_info);
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
 *  SCE_APP_INST_UTIL_ERROR_NOT_INITIALIZED (-2136797184). System
 *  daemons initialize this implicitly at boot; we don't have a
 *  daemon-level startup hook so we lazy-init at first install
 *  attempt. Idempotent return values from Sony's installer (e.g.
 *  "already initialized") are absorbed silently — what we care
 *  about is "the next call to InstallByPackage will work." */
static pthread_once_t g_appinst_init_once = PTHREAD_ONCE_INIT;
static void appinst_init_locked(void) {
    int rc = sceAppInstUtilInitialize();
    if (rc != 0) {
        fprintf(stderr,
                "[bgft] sceAppInstUtilInitialize returned 0x%08X "
                "(non-fatal — common 'already initialised' codes are absorbed)\n",
                (unsigned)rc);
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
/* v2.16.1 scaffolding for the eventual separate-process Tier-0 helper.
 * Set on task_ids issued by the in-process worker-thread variant gated
 * behind the `PS5UPLOAD_TIER0_WORKER=1` env var. The worker serializes
 * Sony installer calls into a single thread, which eliminates multi-
 * thread races on Sony's kernel-stub state and centralises authid
 * handling. It does NOT yet provide cred isolation — ucred is per-
 * process, so sibling threads in our payload still observe whatever
 * authid the worker is currently holding. True isolation requires
 * splitting the helper into its own process (Sony's `dpi`-style
 * pattern) and is deferred to v2.17.0: needs a new build artefact,
 * a deploy/launch mechanism, lifecycle management, and hardware
 * iteration rounds we can't fit mid-release.
 *
 * Default-off in v2.16.1 so the well-tested 3-tier ladder remains
 * the production path. Users opting into hardware testing of the
 * worker can set the env var and inspect via= in install status.
 * The flag is shared with the engine (`pkg_install::APPINST_VIA_
 * TIER0_FLAG`); via_tier() maps it to "tier0-worker". */
#define APPINST_VIA_TIER0_FLAG     0x10000000
/* 2.20.2-fix: set on task_ids issued by the dedicated local-disk
 * installer (sceAppInstUtilAppInstallPkg, register_path "appinst-local").
 * Like APPINST_VIA_SHELLUI_FLAG, it makes bgft_install_status take the
 * synthetic-DONE bypass instead of calling sceAppInstUtilGetInstallStatus.
 *
 * WHY: hardware testing on FW 9.60 proved that GetInstallStatus segfaults
 * the payload when polled for an AppInstallPkg-registered task — the
 * register itself (err_code 0) and the install proceed fine, but the very
 * first status poll takes down both listeners (the fatal-signal handler
 * closes them), forcing a payload reload. The crash reproduced with a
 * KNOWN-GOOD content_id, so it is not an empty/garbage-id issue — Sony's
 * status API simply can't be polled from our process for this task class
 * on 9.60. This is the same failure mode the shellui-rpc tier documents.
 *
 * Distinct from the regular in-process InstallByPackage path (plain
 * APPINST_TASK_ID_FLAG): on firmwares where InstallByPackage succeeds,
 * GetInstallStatus is known-safe and gives real download/install
 * progress, so we must NOT blanket-bypass it — only the local-disk
 * variant gets the bypass. */
#define APPINST_VIA_LOCAL_FLAG     0x08000000
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
 *  refer to the same underlying Sony task.
 *
 *  `out_fresh` (optional): set to 1 when this call ALLOCATED the slot,
 *  0 when it reused an existing same-content_id slot. Callers that
 *  pre-reserve a slot before handing the install to Sony must only
 *  release it on failure when it was freshly allocated — releasing a
 *  REUSED slot would strip tracking from the still-in-flight install
 *  that owns it (retry-while-installing scenario). */
static int32_t appinst_task_register(const char *content_id,
                                     int *out_fresh) {
    if (out_fresh) *out_fresh = 0;
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
            if (out_fresh) *out_fresh = 1;
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
    int idx = task_id & ~(APPINST_TASK_ID_FLAG | APPINST_VIA_SHELLUI_FLAG
                            | APPINST_VIA_TIER0_FLAG | APPINST_VIA_LOCAL_FLAG);
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
    int idx = task_id & ~(APPINST_TASK_ID_FLAG | APPINST_VIA_SHELLUI_FLAG
                            | APPINST_VIA_TIER0_FLAG | APPINST_VIA_LOCAL_FLAG);
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

/* Authid constants (PS5_SHELLCORE_AUTHID, PS5_SYSTEM_INSTALL_AUTHID),
 * kernel ucred extern declarations, swap helpers (ps5_authid_acquire,
 * ps5_authid_release), and firmware detection (ps5_detect_firmware_major)
 * all come from authid.h. Legacy bgft-local name shims so existing call
 * sites don't need a mass rename: */
#define authid_acquire_shellcore(tag) ps5_authid_acquire((tag), PS5_SHELLCORE_AUTHID)
#define authid_release_shellcore(saved, tag) ps5_authid_release((saved), (tag))
#define detect_firmware_major() ps5_detect_firmware_major()

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
    /* A bare local path ("/user/data/.../x.pkg") is passed as-is — the
     * DPI daemon (ezremote_dpi.c) uses the bare path directly and it
     * works on FW 9.60. Wrapping with `file://` causes 0x80B21106
     * (parser error) on FW < 11. Already-schemed URLs (http://, file://)
     * pass through unchanged. */
    meta.uri          = url ? url : "";
    meta.ex_uri       = "";
    meta.playgo_scenario_id = "";
    meta.content_id   = "";
    meta.content_name = title ? title : "ps5upload";
    meta.icon_url     = "";

    AppInstPkgInfo pkg_info;
    memset(&pkg_info, 0, sizeof(pkg_info));
    /* pkg_info.content_id is OUTPUT — Sony's installer extracts the
     * content_id from the pkg header on return; seeding it caused
     * 0x80B21106 parser-error in testing. */

    AppInstPlayGoInfo playgo;
    memset(&playgo, 0, sizeof(playgo));

    /* Reserve the tracking slot BEFORE handing the install to Sony when
     * the caller supplied the content_id (the common case — the host
     * parses it from the pkg header). Registering only after a
     * successful InstallByPackage meant a full table orphaned a
     * Sony-side install we could never report status for; and
     * cancelling at that point is not an option (sceAppInstUtil-
     * CancelInstall in this path regressed installs with 0x80B21106 —
     * see the comment above the install call). Reserve-first fails
     * cleanly before Sony is involved. When the caller's content_id is
     * empty we can't reserve early (the cid is an OUTPUT of
     * InstallByPackage); that path keeps post-install registration and
     * the orphan caveat — empty-cid AND 16 distinct in-flight installs
     * together are vanishingly rare. */
    int32_t pre_tid = -1;
    int pre_tid_fresh = 0;
    if (content_id && content_id[0]) {
        pre_tid = appinst_task_register(content_id, &pre_tid_fresh);
        if (pre_tid < 0) {
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
    }

    /* Swap to ShellCore authid for the entire init+install window.
     * Persistent-ShellCore-authid payloads (injected into ShellUI's
     * own process) get this for free — sceAppInstUtilInitialize sees
     * ShellCore authid when it sets up its IPC handles to Sony's
     * installer daemon, and that state persists across the
     * InstallByPackage call. Calling Initialize with debugger authid
     * (0x4800) primes Sony's daemon with wrong IPC peer info, which
     * surfaces later as 0x80020003 ESRCH ("no such process") because
     * Sony tries to IPC back to a debugger-authid peer that the
     * install path doesn't know how to talk to.
     *
     * Both Init and InstallByPackage run under the same swap window
     * so init's IPC handle setup sees ShellCore authid.
     *
     * The shared `sony_api_lock` covers this entire critical section
     * including the authid swap. Without it, register.c's concurrent
     * calls into sceAppInstUtilInstallTitleDir / sceLncUtilLaunchApp
     * would race against this install's authid window AND Sony's
     * installer kernel-stub state, hitting the FW 9.60 deadlock.
     * Acquired BEFORE the swap so a concurrent caller can't observe
     * a transient ShellCore-authid state. */
    pthread_mutex_lock(&sony_api_lock);
    /* Hold kernel_rw_lock across the whole authid swap window (acquire ->
     * Init+Install under ShellCore authid -> restore) so a concurrent kernel-RW
     * swap in shellui_rpc/ptrace (sensor read), register, or boot elevation
     * — which share the global kernel-RW window — can't run at the same time.
     * Innermost lock: taken AFTER sony_api_lock, never before. */
    pthread_mutex_lock(&kernel_rw_lock);
    /* Firmware-gated install authid — the FW-10 "authority cliff". BELOW
     * FW 10 (5.10/9.60, HW-proven): ShellCore authid is the gate
     * InstallByPackage's URL pre-flight requires (other authids →
     * 0x80B22404 for http / 0x80B21106 for file://). AT/ABOVE FW 10: the
     * content-copy step AND the IPMI init handshake are gated behind the
     * SYSTEM authid (== elf-arsenal's JB_AUTHID 0x4801…013); running
     * sceAppInstUtilInitialize + InstallByPackage under ShellCore on FW 10+
     * leaves Sony's installer daemon (IPMI) half-wedged — the immediate
     * InstallByPackage call returns 0x80B2116F, and Sony's installer
     * watchdog SIGKILLs our process ~5 s later (issue #152: helper dies
     * after install rejection on FW 10.40). The DPI daemon
     * (payload/dpi/ezremote_dpi.c) already escalates to SYSTEM_AUTHID
     * before sceAppInstUtilInitialize on ALL FWs — this matches that.
     * fw==0 (unknown) → ShellCore, the proven default for the broadest
     * installed base. Both Init and InstallByPackage run under this one
     * swap window so Sony's IPC handle setup sees the right authid. */
    int fw_major = detect_firmware_major();
    uint64_t install_authid = (fw_major >= 10)
                                  ? PS5_SYSTEM_INSTALL_AUTHID
                                  : PS5_SHELLCORE_AUTHID;
    uint64_t saved_authid = ps5_authid_acquire("InstallByPackage", install_authid);
    fprintf(stderr,
            "[bgft] InstallByPackage: fw_major=%d authid=0x%llx (%s)\n",
            fw_major, (unsigned long long)install_authid,
            install_authid == PS5_SYSTEM_INSTALL_AUTHID ? "SYSTEM" : "ShellCore");

    /* Init Sony's installer subsystem under the chosen authid. The
     * pthread_once guard makes this idempotent across installs; the
     * FIRST install's pthread_once-controlled invocation sets up
     * Sony's IPC handles correctly (under SYSTEM on FW 10+, exactly as
     * elf-arsenal inits from its escalated process). */
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

    authid_release_shellcore(saved_authid, "InstallByPackage");
    pthread_mutex_unlock(&kernel_rw_lock);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        fprintf(stderr,
                "[bgft] appinst path failed: rc=%d (content_id=%s, url=%s)\n",
                rc, content_id, url);
        /* Give back the pre-reserved slot — but ONLY when this call
         * allocated it. A reused slot belongs to a still-in-flight
         * install of the same content_id (retry scenario); releasing
         * it here would strip that install's status tracking. */
        if (pre_tid >= 0 && pre_tid_fresh) appinst_task_release(pre_tid);
        return -1;
    }

    /* Pre-reserved slot (common case) or late registration when the
     * caller had no content_id and we must use Sony's OUTPUT field. */
    int32_t tid = pre_tid;
    if (tid < 0) {
        tid = appinst_task_register(pkg_info.content_id, NULL);
        if (tid < 0) {
            /* Sony accepted the install but every slot is taken by 16
             * other distinct in-flight installs — we can't track it.
             * The install itself continues (Sony owns it; the console
             * UI shows progress); only our status reporting is lost.
             * Deliberately NOT cancelling: killing a healthy install
             * over bookkeeping is worse, and cancel here regressed
             * installs in hardware testing (see comment above). */
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
    }
    /* 2.25.x: when InstallByPackage installed from a LOCAL path (bare
     * absolute path, not http://), tag the task with APPINST_VIA_LOCAL_FLAG
     * so bgft_install_status takes the synthetic-DONE bypass and never calls
     * GetInstallStatus — which segfaults the payload on the first poll for a
     * locally-registered task (hardware-confirmed on FW 5.10; the comment on
     * APPINST_VIA_LOCAL_FLAG documents the same crash on 9.60 for the
     * AppInstallPkg variant). HTTP-source installs keep real status polling. */
    if (url && url[0] == '/') {
        tid |= APPINST_VIA_LOCAL_FLAG;
    }
    *out_task_id  = tid;
    *out_err_code = 0;
    return 0;
}

/** Dedicated local-disk install path (2.20.2). Same contract as
 *  appinst_install_start, but routes through sceAppInstUtilAppInstallPkg
 *  — Sony's "this .pkg already lives on local disk" entry point — instead
 *  of InstallByPackage's URI fetcher. Used only when the caller staged
 *  the pkg under /user/data (path is a bare absolute path), where it
 *  sidesteps both the PlayGo HTTP gate (0x80B22404) and the URI-parser
 *  rejection a bare path hits in InstallByPackage.
 *
 *  `path` must be a local absolute path. On success the synthetic task_id
 *  carries APPINST_TASK_ID_FLAG (set by appinst_task_register), so status
 *  polling routes through the same GetInstallStatus path as the
 *  InstallByPackage tier — the install record lives in OUR process here
 *  (no ptrace), so GetInstallStatus is safe to call. */
static int appinst_install_start_local(const char *path,
                                        const char *content_id,
                                        int32_t *out_task_id,
                                        uint32_t *out_err_code) {
    AppInstPkgInfo pkg_info;
    memset(&pkg_info, 0, sizeof(pkg_info));
    /* pkg_info.content_id is OUTPUT — Sony fills it from the pkg header
     * on success. Seeding it caused 0x80B21106 in the InstallByPackage
     * path; same rule applies here. */

    /* Resolve the dedicated installer lazily (see typedef note above).
     * If the runtime SPRX on this firmware doesn't export it, degrade to
     * BGFT_ERR_SYMBOL_MISSING and let bgft_install_start fall through to
     * the InstallByPackage tier — never a hard failure. Resolved BEFORE
     * the lock so a missing symbol costs no authid swap. */
    sce_app_inst_util_app_install_pkg_fn app_install_pkg =
        (sce_app_inst_util_app_install_pkg_fn)dlsym(RTLD_DEFAULT,
                                                    "sceAppInstUtilAppInstallPkg");
    if (!app_install_pkg) {
        *out_err_code = BGFT_ERR_SYMBOL_MISSING;
        fprintf(stderr,
                "[bgft] sceAppInstUtilAppInstallPkg not exported on this FW "
                "— falling back to InstallByPackage\n");
        return -1;
    }

    /* Reserve the tracking slot before handing the install to Sony —
     * same reserve-first rationale as appinst_install_start: a full
     * table must fail BEFORE Sony owns a task we can't track, because
     * cancelling after the fact is off the table (regression note
     * below). The caller-parsed content_id is required on this path. */
    int32_t pre_tid = -1;
    int pre_tid_fresh = 0;
    if (content_id && content_id[0]) {
        pre_tid = appinst_task_register(content_id, &pre_tid_fresh);
        if (pre_tid < 0) {
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
    }

    /* Identical authid + lock + init discipline to appinst_install_start.
     * AppInstallPkg is gated on the same ShellCore-authid check as
     * InstallByPackage (same installer subsystem), so we hold the
     * ShellCore-authid swap across the whole init+install window under
     * sony_api_lock. No pre-install CancelInstall — testing on the
     * InstallByPackage path showed cancel flushes IPC state the install
     * call needs and regressed register success; the dedicated installer
     * shares that subsystem, so we keep the same no-cancel rule. */
    pthread_mutex_lock(&sony_api_lock);
    /* kernel_rw_lock across the authid swap window — see appinst_install_start. */
    pthread_mutex_lock(&kernel_rw_lock);
    /* SYSTEM authid (not ShellCore) for the local content install — this is
     * how elf-arsenal / the reference impl land the actual app content rather
     * than just the title record. Held across init + the AppInstallPkg call;
     * the install service captures the caller credential at request time, so
     * restoring after the synchronous return is fine — the async content copy
     * proceeds with the SYSTEM-authorised request it already accepted. */
    uint64_t saved_authid = ps5_authid_acquire("AppInstallPkg", PS5_SYSTEM_INSTALL_AUTHID);
    pthread_once(&g_appinst_init_once, appinst_init_locked);

    int rc = app_install_pkg(path, &pkg_info);

    authid_release_shellcore(saved_authid, "AppInstallPkg");
    pthread_mutex_unlock(&kernel_rw_lock);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);
    if (rc != 0) {
        *out_err_code = (uint32_t)rc;
        fprintf(stderr,
                "[bgft] appinst-local path failed: rc=0x%08X (content_id=%s, path=%s)\n",
                (unsigned)rc, content_id, path);
        /* Give back the pre-reserved slot — but only when freshly
         * allocated; a reused slot belongs to a still-in-flight
         * install of the same content_id (see appinst_install_start). */
        if (pre_tid >= 0 && pre_tid_fresh) appinst_task_release(pre_tid);
        return -1;
    }

    /* 2.20.2-fix: track the task under a KNOWN-GOOD content_id for the
     * subsequent GetInstallStatus poll. We log both so we can see on
     * hardware whether AppInstallPkg actually populated its OUTPUT field.
     * Empirically on FW 9.60 the status poll segfaulted the payload after
     * a successful register — a GetInstallStatus() on an empty/garbage
     * content_id is the prime suspect. We already parsed the real
     * content_id host-side and passed it in `content_id`, so prefer that
     * (it equals the pkg-header id Sony's installer tracks the task under)
     * and only fall back to Sony's output when the caller didn't give us
     * one. */
    fprintf(stderr,
            "[bgft] appinst-local ok: rc=0 caller_cid=%s sony_out_cid=%s path=%s\n",
            content_id ? content_id : "(null)",
            pkg_info.content_id[0] ? pkg_info.content_id : "(empty)",
            path);
    /* Pre-reserved slot (caller-supplied content_id, the normal case)
     * or late registration from Sony's OUTPUT field. Same fallback
     * caveat as appinst_install_start: late registration on a full
     * table can't track the (continuing) Sony install — and we
     * deliberately don't cancel it (regression note above). */
    int32_t tid = pre_tid;
    if (tid < 0) {
        tid = appinst_task_register(pkg_info.content_id, NULL);
        if (tid < 0) {
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
    }
    /* Tag as a local-disk install so bgft_install_status takes the
     * synthetic-DONE bypass and never calls the FW-9.60-fatal
     * GetInstallStatus on this task. See APPINST_VIA_LOCAL_FLAG. */
    *out_task_id  = tid | APPINST_VIA_LOCAL_FLAG;
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
    /* 2.20.2-fix: never hand an empty content_id to Sony's status API.
     * sceAppInstUtilGetInstallStatus("") dereferences installer state
     * keyed by content_id; an empty key segfaulted the payload on FW
     * 9.60. If we somehow tracked a task with no id, report INSTALL
     * (keep-polling) rather than crash — the install itself already
     * started when AppInstallPkg/InstallByPackage returned 0. */
    if (content_id[0] == '\0') {
        fprintf(stderr,
                "[bgft] appinst status: empty content_id for task 0x%08X "
                "— skipping GetInstallStatus, reporting INSTALL\n",
                (unsigned)task_id);
        *out_phase      = BGFT_PHASE_INSTALL;
        *out_downloaded = 0;
        *out_total      = 0;
        *out_err_code   = 0;
        return 0;
    }
    AppInstStatus st;
    memset(&st, 0, sizeof(st));

    /* GetInstallStatus also requires ShellCore authid (same gate as
     * InstallByPackage). Calling it from our default debugger authid
     * was the crash source observed pre-2.2.53-fix-round-3 — the call
     * dereferences cross-process state the kernel won't let a non-
     * ShellCore caller see, and the segfault propagates back to us.
     * Same swap+restore pattern as appinst_install_start, centralised
     * via the authid_{acquire,release}_shellcore helpers — verifying
     * the restore keeps the window during which our authid is wrong
     * as small as possible so concurrent sensor reads on another mgmt
     * thread don't see a transient bad-authid state.
     *
     * Shared `sony_api_lock` taken around the whole window: status
     * polls fire every ~1 s during an active install, so a concurrent
     * register/launch/uninstall on another mgmt thread is the
     * realistic deadlock trigger. Pre-2.2.61 the status path raced
     * those calls against Sony's installer kernel stubs. */
    pthread_mutex_lock(&sony_api_lock);
    /* kernel_rw_lock across the authid swap window — see appinst_install_start.
     * Status polls fire ~1/s during an install, so this window is the most
     * frequent concurrent partner for a sensor read's ptrace authid swap. */
    pthread_mutex_lock(&kernel_rw_lock);
    uint64_t saved_authid = authid_acquire_shellcore("GetInstallStatus");

    int rc = sceAppInstUtilGetInstallStatus(content_id, &st);

    authid_release_shellcore(saved_authid, "GetInstallStatus");
    pthread_mutex_unlock(&kernel_rw_lock);
    usleep(SONY_API_POST_SLEEP_US);
    pthread_mutex_unlock(&sony_api_lock);

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
        /* Sony's actual install state strings (observed):
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
 * 0x8099003x-class entitlement errors. The regular→debug Register
 * fallback sequence is implemented in `bgft_install_start`. Probed
 * alongside g_bgft_register and treated as an optional capability.
 *
 * Why a separate global rather than overwriting g_bgft_register: we want
 * the regular path available too — Sony-signed pkgs (the rare case our
 * users hit) need regular Register; the host-side decision of which to
 * try first depends on the pkg header's signed-vs-fake state which we
 * don't always know in advance. */
static sce_bgft_register_task_fn   g_bgft_register_debug = NULL;
/* Most recent Register-path tag, surfaced via
 * bgft_install_last_register_path(). The "none" sentinel covers two
 * distinct cases — "init never ran" and "BGFT is unavailable on this
 * FW" — which the host UI distinguishes by also reading
 * bgft_install_unavailable_reason().
 *
 * _Atomic (this and the four error diagnostics below): writes happen
 * in install paths and reads in PKG_INSTALL_STATUS handlers, which run
 * on DIFFERENT mgmt client threads with no shared lock. The values
 * only ever point at string literals / hold whole error codes, so
 * relaxed-style atomic load/store is all that's needed — it just turns
 * a formal C11 data race into defined behavior at zero practical cost. */
static const char *_Atomic g_last_register_path = "none";
/* Per-tier error codes from the most recent install attempt, surfaced
 * via bgft_install_last_*_err(). Set unconditionally on every attempt
 * (success: 0, failure: error code). Pre-2.2.54-fix-round-8 used
 * UINT32_MAX as the "not attempted" sentinel — but that VALUE
 * collides with `pt_call` returning -1 (cast to uint32 = 0xFFFFFFFF =
 * UINT32_MAX), making real tier failures appear as "tier never
 * attempted" in the diagnostic. Now using a separate `_set` boolean
 * flag so the value can be any 32-bit number including 0xFFFFFFFF. */
static _Atomic uint32_t g_last_shellui_err = 0;
static _Atomic int      g_last_shellui_err_set = 0;
static _Atomic uint32_t g_last_appinst_err = 0;
static _Atomic int      g_last_appinst_err_set = 0;
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
     * canonical workflow). We fall back to this when regular Register
     * returns 0x80990088 ("DUPLICATE_TASK") or any other rejection.
     * Probed separately so a missing symbol on one firmware doesn't
     * poison the regular-Register path. */
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
         * to defaults, which works on every FW we've tested. */
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
     * "already initialised" code is benign (Sony's daemon may have
     * init'd BGFT earlier); we treat it as success. */
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
    /* Atomic pointer load (see the declaration) — always yields one of
     * the path-tag string literals, never a torn pointer. Lock-free so
     * PKG_INSTALL_STATUS responses stay cheap. */
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
                        const char *method,
                        int32_t *out_task_id,
                        uint32_t *out_err_code) {
    if (!url || !content_id || !out_task_id || !out_err_code) {
        if (out_err_code) *out_err_code = BGFT_ERR_REGISTER_FAILED;
        return -1;
    }
    *out_task_id = -1;
    *out_err_code = 0;

    /* AppInstUtil-first dispatch (added 2.2.44). The
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

    /* ─── Host-driven single-method selector ─────────────────────────────
     * When the engine requests a SPECIFIC install method, run ONLY that
     * tier and report its raw result — no internal cascade. This is what
     * lets the engine drive a verify-and-fall-through cascade: it tries a
     * method, confirms the content actually landed under
     * /user/app/<id>/app.pkg, and only then trusts the rc==0. A no-op tier
     * (e.g. AppInstallPkg on FW 5.10, which returns 0 but copies nothing)
     * is caught by the engine's post-install filesystem check and the next
     * method is tried. An empty / "auto" method keeps the legacy internal
     * cascade below (back-compat with older engines). */
    if (method && method[0] && strcmp(method, "auto") != 0) {
        if (strcmp(method, "appinst-local") == 0) {
            int32_t t = -1; uint32_t e = 0;
            int rc = appinst_install_start_local(url, content_id, &t, &e);
            g_last_appinst_err = e; g_last_appinst_err_set = 1;
            if (rc == 0) {
                *out_task_id = t; *out_err_code = 0;
                g_last_register_path = "appinst-local";
                return 0;
            }
            *out_err_code = e ? e : BGFT_ERR_REGISTER_FAILED;
            return -1;
        }
        if (strcmp(method, "appinst-bypackage") == 0) {
            int32_t t = -1; uint32_t e = 0;
            int rc = appinst_install_start(url, content_id, title, &t, &e);
            g_last_appinst_err = e; g_last_appinst_err_set = 1;
            if (rc == 0) {
                *out_task_id = t; *out_err_code = 0;
                g_last_register_path = "appinst";
                return 0;
            }
            *out_err_code = e ? e : BGFT_ERR_REGISTER_FAILED;
            return -1;
        }
        if (strcmp(method, "shellui") == 0) {
            (void)shellui_rpc_init();
            uint32_t e = 0;
            int rc = shellui_rpc_install_pkg(url, content_id, title, &e);
            g_last_shellui_err = e; g_last_shellui_err_set = 1;
            if (rc == 0) {
                int32_t tid = appinst_task_register(content_id, NULL);
                if (tid < 0) { *out_err_code = BGFT_ERR_TASK_TABLE_FULL; return -1; }
                *out_task_id = tid | APPINST_VIA_SHELLUI_FLAG;
                *out_err_code = 0;
                g_last_register_path = "shellui-rpc";
                return 0;
            }
            *out_err_code = e ? e : BGFT_ERR_REGISTER_FAILED;
            return -1;
        }
        /* "bgft" or any unrecognised value → fall through to the legacy
         * cascade below (which ends in the BGFT IntDebug path). */
    }

    /* ─── No appinst-local Tier-0 — HARDWARE-DISPROVEN ───────────────────
     * A previous iteration ran sceAppInstUtilAppInstallPkg FIRST under the
     * SYSTEM authid (0x4801000000000013) on the theory (from elf-arsenal)
     * that it lands launchable content. Hardware testing on FW 5.10 proved
     * the opposite: AppInstallPkg returns err_code=0 ("Done") but copies
     * NOTHING — /user/app/<id>/ is never created and Sony's klog shows zero
     * installer activity. It is a silent no-op on this firmware. Trusting
     * its rc==0 and returning first short-circuited the path that actually
     * works (InstallByPackage, Tier 1 below — klog shows the full
     * PlayGoCore transfer + prepromote, and /user/app/<id>/app.pkg lands
     * with playgo locus=3). So AppInstallPkg stays the ABSOLUTE LAST RESORT
     * (try_appinst_local_last_resort:), reached only when every real
     * installer path has failed, and the engine verifies the content
     * actually landed before reporting success (never trusts rc==0 — see
     * ps5upload_core::pkg_install::verify_launchable). */

    /* ─── Tier ordering ──────────────────────────────────────────────
     *   1. **In-process AppInstUtil** (preferred): call
     *      sceAppInstUtilInstallByPackage directly with a per-call
     *      ShellCore-authid swap. No ptrace, no ShellUI freeze, no
     *      respawn flash. Works for the broad majority of pkgs (DLC,
     *      regular game pkgs, patches) when Sony's installer doesn't
     *      require an HTTP fetch.
     *
     *   2. **shellui-rpc** (fallback): ptrace into SceShellUI and call
     *      from there. Required when Sony's PlayGo whitelist gates the
     *      call on the caller's process attributes — observed as
     *      0x80B22404 (PlayGo HTTP 404 pre-flight) on FW 9.60+ for
     *      some pkg shapes. Cost: ShellUI freeze ⇒ Sony's watchdog
     *      often respawns it ⇒ visible black flash on the TV.
     *
     *   3. **Legacy BGFT** (last resort): IntDebug register variants.
     *      Kept for fakepkg installs where the modern installer path
     *      rejects on signature checks.
     *
     * Empirical observation: the PlayGo gate only engages for the
     * HTTP-fetch code paths Sony's installer takes for URLs it
     * considers "remote." Locally-staged file:// URIs (everything
     * ps5upload installs — we stage to /user/data/ps5upload/pkg_temp/
     * first) bypass most of that gate, so Tier 1 works for most pkgs
     * without the ShellUI flash. */

    /* ─── Tier 1: in-process InstallByPackage (preferred for ALL urls) ──
     * 2.25.x change — for a locally-staged pkg (`url` is a bare absolute
     * path, e.g. /user/data/tmp/<id>.pkg), we now call
     * sceAppInstUtilInstallByPackage with that path as the URI FIRST,
     * exactly as etaHEN's DPI v2 does (verified launchable on FW 10.01 /
     * 12.00). The previous order tried sceAppInstUtilAppInstallPkg first
     * for local paths; that call *installs* the content but registers the
     * title in a state the launcher rejects with "can't start the game or
     * app" (CE-) — even though install/status reports success. Only the
     * *HTTP* URI form of InstallByPackage hits the FW 9.60+ PlayGo reject
     * (0x80B22404); a bare LOCAL path is read straight off disk and
     * registers a launchable title. So: InstallByPackage(local) first;
     * AppInstallPkg becomes a fallback below if InstallByPackage fails.
     * HTTP urls also flow through here (same call, remote fetch). */
    int32_t app_tid = -1;
    uint32_t app_err = 0;
    int appinst_rc = appinst_install_start(url, content_id, title, &app_tid, &app_err);
    g_last_appinst_err = app_err;
    g_last_appinst_err_set = 1;
    if (appinst_rc == 0) {
        *out_task_id = app_tid;
        *out_err_code = 0;
        /* Tag the success path so the host's diag panel reflects
         * which tier ran. "appinst" = the in-process AppInstUtil
         * path, meaning no ShellUI flash. */
        g_last_register_path = "appinst";
        /* v2.16.1 Tier-0 scaffold (env-var gated, default-off). When
         * `PS5UPLOAD_TIER0_WORKER=1`, OR the TIER0 flag into the
         * task_id so the engine's via_tier() classifies it as
         * "tier0-worker" in the install-status response. The actual
         * worker-thread serialization + persistent-authid + IPC-to-
         * separate-helper-process changes are deferred to v2.17.0;
         * this v2.16.1 step just wires the protocol path so users
         * opting into hardware testing can observe the via= value
         * change. See APPINST_VIA_TIER0_FLAG note at top of file. */
        {
            const char *tier0_opt = getenv("PS5UPLOAD_TIER0_WORKER");
            if (tier0_opt != NULL && tier0_opt[0] == '1' && tier0_opt[1] == '\0') {
                *out_task_id |= APPINST_VIA_TIER0_FLAG;
                g_last_register_path = "tier0-worker";
            }
        }
        return 0;
    }
    fprintf(stderr,
            "[bgft] in-process InstallByPackage failed (rc=0x%08X)\n",
            (unsigned)app_err);

    /* ─── PATCH guard: never fall past InstallByPackage for a patch ───────
     * A patch (…DP) carries the SAME content_id as its base game. The
     * shellui-rpc tier calls sceAppInstUtilInstallByPackage inside ShellUI,
     * which RE-REGISTERS the content_id and WIPES the installed base
     * (HW-reconfirmed 3.3.26: a Jak X patch via shellui-rpc deleted the 3.6GB
     * base, even with the bare-path URI fix). Only the in-process
     * InstallByPackage above (and the DPI daemon) apply patches safely.
     * If in-process InstallByPackage failed for a patch, FAIL CLEANLY here
     * with that error and leave the base intact — the caller should fall
     * through to the DPI daemon (pkg_dpi_install) which also works safely.
     * (DLC …AC carries its OWN content_id, so it doesn't collide with the
     * base and may still use the fallbacks.) */
    {
        size_t pt_len = package_type ? strlen(package_type) : 0;
        const char *pt_suffix = pt_len >= 2 ? package_type + pt_len - 2 : "";
        if (strcmp(pt_suffix, "DP") == 0) {
            fprintf(stderr,
                    "[bgft] patch (package_type=%s) failed in-process "
                    "InstallByPackage (rc=0x%08X) — NOT trying shellui-rpc "
                    "(it re-registers the content_id and WIPES the base). "
                    "Failing cleanly; caller should use DPI install.\n",
                    package_type, (unsigned)app_err);
            *out_err_code = app_err ? app_err : BGFT_ERR_REGISTER_FAILED;
            return -1;
        }
    }

    /* ─── 2.27.x reorder — FW-12 "installs but won't launch" fix ─────────
     * The dedicated local-disk installer (sceAppInstUtilAppInstallPkg) USED
     * to run HERE, immediately after InstallByPackage(local) failed. It
     * installs content but can register an UNLAUNCHABLE title on some
     * firmwares ("can't start the game or app", CE-) — the exact FW-12.xx
     * symptom users report. Running it second meant a single InstallByPackage
     * rejection on 12.xx silently produced a dead tile AND reported success,
     * never reaching the two LAUNCHABLE fallbacks below (shellui-rpc, legacy
     * BGFT IntDebug). It is now the ABSOLUTE LAST RESORT — see the
     * `try_appinst_local_last_resort:` label at the end of this function. It
     * is reached only after every launchable path has failed, and on success
     * it tags g_last_register_path = "appinst-local" so the host warns the
     * title may not launch instead of showing a clean success. */

    {
        /* Tier 2: ShellUI RPC fallback. Only reached when the in-process
         * call failed — typically because Sony's PlayGo whitelist needs
         * the call to originate from inside ShellUI's process. Lazy-init
         * the RPC layer; safe to call repeatedly. */
        (void)shellui_rpc_init();
        uint32_t shellui_err = 0;
        int rc = shellui_rpc_install_pkg(url, content_id, title,
                                          &shellui_err);
        g_last_shellui_err = shellui_err;
        g_last_shellui_err_set = 1;
        if (rc == 0) {
            int32_t tid = appinst_task_register(content_id, NULL);
            if (tid >= 0) {
                /* OR in the shellui-rpc backing flag so
                 * bgft_install_status routes the synthetic-DONE
                 * bypass — never calls Sony's GetInstallStatus for
                 * cross-process installs. */
                *out_task_id = tid | APPINST_VIA_SHELLUI_FLAG;
                *out_err_code = 0;
                g_last_register_path = "shellui-rpc";
                return 0;
            }
            *out_err_code = BGFT_ERR_TASK_TABLE_FULL;
            return -1;
        }
        if (shellui_err != 0) {
            fprintf(stderr,
                    "[bgft] shellui-rpc install also failed (rc=0x%08X), falling back to legacy BGFT\n",
                    (unsigned)shellui_err);
        } else {
            fprintf(stderr,
                    "[bgft] shellui-rpc attach/mmap failed, falling back to legacy BGFT\n");
        }
    }

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
        goto try_appinst_local_last_resort;
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
    /* Register fallback ladder for fakepkg installs:
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
     *      which BGFT will treat as a re-attach.
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
        goto try_appinst_local_last_resort;
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
        goto try_appinst_local_last_resort;
    }

    *out_task_id = task_id;
    pthread_mutex_unlock(&g_mtx);
    return 0;

try_appinst_local_last_resort:
    (void)0; /* a label may not directly precede a declaration in C */
    /* ─── Last resort: local-disk AppInstallPkg (may not launch) ──────────
     * Reached only after EVERY launchable path failed: in-process
     * InstallByPackage, shellui-rpc, and legacy BGFT IntDebug. As a final
     * attempt we try sceAppInstUtilAppInstallPkg for a locally-staged pkg —
     * an install the user can re-trigger beats no install at all. This call
     * can register a title the launcher rejects ("can't start the game or
     * app") on some firmwares, so on success we report register_path
     * "appinst-local": the host treats that as "installed, but may not
     * launch" and tells the user to re-install from the PS5's
     * Settings → Package Installer if it won't boot.
     *
     * HTTP-sourced installs have nothing staged on local disk (url is an
     * http(s):// URL, not a bare path), so this is skipped for them and we
     * surface the launchable-tier failure code set by whichever path jumped
     * here.
     *
     * CRITICAL — package-type gate: AppInstallPkg installs the pkg as a FRESH
     * APP. For a base game (…GD) that's a recoverable "may not launch" tile, but
     * for a PATCH (…DP) or DLC/add-on (…AC) it OVERWRITES and WIPES the base
     * game instead of applying on top (HW-proven: a Bloodborne patch deleted the
     * installed 26 GB base on FW 9.60). A patch/DLC must NEVER be installed this
     * way — better to fail cleanly (base intact, user retries) than destroy a
     * game. So this last resort is restricted to full-app packages. */
    int last_resort_destructive_for_type = 0;
    {
        size_t pt_len = package_type ? strlen(package_type) : 0;
        const char *pt_suffix = pt_len >= 2 ? package_type + pt_len - 2 : "";
        /* …DP = (delta) patch, …AC = additional content / DLC. */
        last_resort_destructive_for_type =
            (strcmp(pt_suffix, "DP") == 0 || strcmp(pt_suffix, "AC") == 0);
    }
    if (url[0] == '/' && last_resort_destructive_for_type) {
        fprintf(stderr,
                "[bgft] NOT using appinst-local last-resort for package_type=%s "
                "(it installs as a fresh app and would WIPE the base game) — "
                "failing cleanly, base left intact\n",
                package_type ? package_type : "?");
    } else if (url[0] == '/') {
        int32_t local_tid = -1;
        uint32_t local_err = 0;
        int local_rc = appinst_install_start_local(url, content_id,
                                                    &local_tid, &local_err);
        if (local_rc == 0) {
            *out_task_id = local_tid;
            *out_err_code = 0;
            g_last_register_path = "appinst-local";
            return 0;
        }
        fprintf(stderr,
                "[bgft] appinst-local last-resort also failed (rc=0x%08X)\n",
                (unsigned)local_err);
    }
    /* *out_err_code already holds the launchable-tier failure code set by
     * the path that jumped here — surface that as the real cause. */
    return -1;
}

/* ─── Public API: install status ──────────────────────────────────── */

/* Map BGFT's internal state code to our public phase enum. Codes
 * cross-referenced from PSDevWiki + community debugging notes:
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
     * shellui-rpc-backed tasks (APPINST_VIA_SHELLUI_FLAG) take the
     * synthetic-phase bypass — return INSTALL forever, never call
     * Sony's GetInstallStatus. Background: the install was dispatched
     * from INSIDE ShellUI's process (via ptrace), so the install
     * record lives in ShellUI's IPC state, not ours. Sony's status
     * API segfaults — or under benign conditions just *blocks for a
     * long time* — when polled from a different process for the same
     * content_id. Either failure mode wedges our mgmt thread:
     *   - segfault → payload crashes mid-install
     *   - long block → mgmt thread holds sony_api_lock waiting; the
     *     engine's next 1 s status poll times out connecting to :9114
     *     → user sees "Can't reach your PS5's management service"
     *     mid-install (which is what you are seeing on DLC pkgs).
     *
     * Sonic-loader takes the same fire-and-forget approach for
     * cross-process installs. The actual install runs to completion
     * on the PS5; the user verifies via the PS5 notification panel
     * and the row transitions to done when they tap "OK" in the UI.
     *
     * An earlier "verified safe via 5 consecutive polls" change
     * removed this bypass — but 5 polls aren't enough to characterise
     * the install API's behaviour across the variety of pkg
     * metadata (DLCs, patches, themes) and firmware revisions in the
     * wild. Restored. */
    if ((task_id & APPINST_VIA_SHELLUI_FLAG) != 0) {
        /* Report DONE (not INSTALL) so the engine + UI consider the
         * row terminal. Sony's queue continues processing the install
         * in the background; the user verifies completion via the
         * PS5's notification panel. INSTALL would have left the row
         * spinning forever — there's no way to detect actual Sony
         * completion from our process without re-triggering the
         * GetInstallStatus crash that the bypass was added to avoid.
         *
         * The UI's "register_path === shellui-rpc" banner explains
         * the hand-off so the user knows where to check. */
        /* Free the task-table slot now (2.25.1). This synthetic-DONE is
         * terminal — the engine marks the row done on this response and
         * never polls again — so holding the slot only leaks it. The 16-
         * slot table would otherwise saturate after 16 distinct installs
         * in one session and reject further installs with TASK_TABLE_FULL.
         * Release is idempotent (strips the flags to find the slot). */
        appinst_task_release(task_id);
        *out_phase = BGFT_PHASE_DONE;
        *out_downloaded = 0;
        *out_total = 0;
        *out_err_code = 0;
        return 0;
    }
    /* Local-disk installs (appinst-local) take the same synthetic-DONE
     * bypass. sceAppInstUtilAppInstallPkg already accepted + started the
     * install synchronously (it returned 0); polling GetInstallStatus for
     * this task class segfaults the payload on FW 9.60 (hardware-proven,
     * reproduces even with a valid content_id), so we never call it.
     * Report DONE — Sony finishes promoting the title in the background
     * and the PS5's notification panel confirms. See APPINST_VIA_LOCAL_FLAG. */
    if ((task_id & APPINST_VIA_LOCAL_FLAG) != 0) {
        /* Free the slot (2.25.1) — same reason as the shellui bypass above:
         * the row is terminal here, so retaining the slot just leaks one of
         * the 16. Without this, installing 16+ pkgs in a session exhausts
         * the table and blocks all further installs. */
        appinst_task_release(task_id);
        *out_phase = BGFT_PHASE_DONE;
        *out_downloaded = 0;
        *out_total = 0;
        *out_err_code = 0;
        return 0;
    }
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
