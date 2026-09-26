/*
 * focus_probe.c — answer "which app is on screen right now?" without ptrace.
 *
 * See focus_probe.h for why this exists and why it is dlsym-only.
 */

#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "focus_probe.h"
#include "proc_list.h"

/* sceSystemServiceGetAppIdOfBigApp / ...MiniApp take no arguments and
 * return the app id directly (negative or 0 when nothing qualifies).
 * "Big app" is the full-screen foreground application — a game; "mini
 * app" is the overlaid system UI. Neither is declared by the payload
 * SDK, so the prototypes live here. */
typedef int (*sce_get_app_id_fn)(void);

/* sceLncUtilGetAppStatus-style getters take an app id and fill a status
 * word. Probed opportunistically: if a firmware exports one it gives a
 * second, independent read on the same question. */
typedef int (*sce_get_status_fn)(unsigned int app_id, int *out_status);

static sce_get_app_id_fn p_big_app   = NULL;
static sce_get_app_id_fn p_mini_app  = NULL;
static int focus_resolve_attempted = 0;

static void *resolve_quiet(const char *name) {
    void *p;
    dlerror();
    p = dlsym(RTLD_DEFAULT, name);
    if (!p) (void)dlerror(); /* clear, absence is expected per-firmware */
    return p;
}

/* Candidate foreground/focus getters, probed by name.
 *
 * Resolving a symbol is free and side-effect-free, so we probe broadly and
 * report what this firmware actually exports. CALLING one is a different
 * matter: these are undeclared by the SDK, so invoking a name whose real
 * signature differs from our guess corrupts the stack. Only the two
 * no-argument getters below are ever called; the rest are reported for
 * availability only, so we can pick the next step from evidence instead of
 * burning a rebuild-and-redeploy cycle per guess.
 *
 * Measured on FW 9.60: GetAppIdOfBigApp / GetAppIdOfMiniApp do NOT resolve,
 * while sceLncUtilGetAppStatus does. */
static const char *const FOCUS_CANDIDATES[] = {
    "sceSystemServiceGetAppIdOfBigApp",
    "sceSystemServiceGetAppIdOfMiniApp",
    "sceSystemServiceGetAppStatus",
    "sceSystemServiceGetForegroundAppId",
    "sceSystemServiceGetRenderingMode",
    "sceLncUtilGetAppStatus",
    "sceLncUtilGetAppId",
    "sceLncUtilGetForegroundAppId",
    "sceLncUtilGetCurrentAppId",
    "sceLncUtilGetAppStatusList",
    "sceLncUtilGetAppInfo",
    "sceShellCoreUtilGetForegroundAppId",
    "sceShellCoreUtilGetAppFocus",
    "sceApplicationGetAppId",
    "sceApplicationGetFocusedAppId",
    /* Named event flags are how ShadowMount+ reads focus, and
     * "SceShellCoreUtilAppFocus" has been CONFIRMED on FW 9.60 to hold the
     * focused app id: it read 0x6018 (the game) with the game on screen and
     * 0x0007 (SceShellUI) with the dashboard up. If these resolve we can read
     * that flag ourselves instead of scraping a third-party log. */
    "sceKernelOpenEventFlag",
    "sceKernelPollEventFlag",
    "sceKernelWaitEventFlag",
    "sceKernelCloseEventFlag",
    /* elf-arsenal (src/ps5/sys.c) launches titles with these, called
     * DIRECTLY in-process — no ptrace, no ShellUI hijack. If they resolve
     * here, our ptrace launch path (which restarts ShellUI ~20s later and
     * drops the game to the dashboard) can be replaced wholesale. Note the
     * getter's real name has "Running" in it — probing for
     * sceSystemServiceGetAppIdOfBigApp is why it looked absent. */
    "sceSystemServiceLaunchApp",
    "sceSystemServiceGetAppIdOfRunningBigApp",
    "sceSystemServiceKillApp",
    "sceUserServiceGetForegroundUser",
};
#define FOCUS_CANDIDATE_COUNT \
    (sizeof FOCUS_CANDIDATES / sizeof FOCUS_CANDIDATES[0])

static int focus_available[FOCUS_CANDIDATE_COUNT];

/* Named event flags. Unlike the Lnc/SystemService getters above, these have
 * well-known signatures, so calling them is not a guess:
 *
 *   int sceKernelOpenEventFlag(int *out_ef, const char *name);
 *   int sceKernelPollEventFlag(int ef, uint64_t pattern, uint32_t mode,
 *                              uint64_t *out_result);
 *   int sceKernelCloseEventFlag(int ef);
 *
 * "SceShellCoreUtilAppFocus" holds the APP ID OF THE FOCUSED APPLICATION.
 * Confirmed on FW 9.60 by direct observation: it read 0x6018 (the game's app
 * id) with the game on screen, and 0x0007 (SceShellUI's app id) with the
 * dashboard up. That makes it the authoritative "what is on screen" source on
 * a firmware that exports no focus getter at all.
 *
 * NOTE the id is not stable across respawns: SceShellUI is restarted often
 * and takes a new app id each time (observed 0x8007, 0xA007, 0xC007, 0xE007,
 * 0x0007). So never hardcode the dashboard's id — compare against the app id
 * of the title you care about instead. */
#define SHELL_FOCUS_FLAG_NAME "SceShellCoreUtilAppFocus"

/* Sony's wait modes: AND is 0x01, OR is 0x02. Getting these backwards asks
 * the kernel to satisfy ALL 64 bits at once, which never happens and returns
 * 0x80020010 (EBUSY) forever — a failure that reads exactly like "no app is
 * focused", so it is worth stating plainly here. */
#define EVF_WAITMODE_AND 0x01u
#define EVF_WAITMODE_OR  0x02u

typedef int (*sce_evf_open_fn)(int *out_ef, const char *name);
typedef int (*sce_evf_poll_fn)(int ef, uint64_t pattern, uint32_t mode,
                               uint64_t *out_result);
typedef int (*sce_evf_close_fn)(int ef);

static sce_evf_open_fn  p_evf_open  = NULL;
static sce_evf_poll_fn  p_evf_poll  = NULL;
static sce_evf_close_fn p_evf_close = NULL;
static int g_focus_ef = -1;
static int g_focus_ef_open_failed = 0;

/* Read the focused app id, or -1 when unavailable.
 *
 * The handle is opened once and cached: opening per poll would churn a kernel
 * object at 1 Hz for no benefit. */
static long long read_focus_app_id(int *rc_out) {
    uint64_t result = 0;
    int rc;

    if (!p_evf_open || !p_evf_poll) return -1;

    if (g_focus_ef < 0) {
        if (g_focus_ef_open_failed) return -1;
        rc = p_evf_open(&g_focus_ef, SHELL_FOCUS_FLAG_NAME);
        if (rc != 0 || g_focus_ef < 0) {
            /* Latch the failure: retrying every poll would spam the log and
             * cannot start working on its own. */
            g_focus_ef_open_failed = 1;
            g_focus_ef = -1;
            if (rc_out) *rc_out = rc;
            fprintf(stderr,
                    "[payload2] focus: open '%s' failed rc=0x%x\n",
                    SHELL_FOCUS_FLAG_NAME, (unsigned)rc);
            return -1;
        }
        fprintf(stderr, "[payload2] focus: opened '%s' ef=%d\n",
                SHELL_FOCUS_FLAG_NAME, g_focus_ef);
    }

    rc = p_evf_poll(g_focus_ef, ~(uint64_t)0, EVF_WAITMODE_OR, &result);
    if (rc_out) *rc_out = rc;
    /* The kernel fills in the current pattern even when it reports EBUSY
     * ("no bits matched"), so a non-zero result is usable regardless of rc.
     * An all-zero flag genuinely means "nothing focused" — report 0, not an
     * error, so the caller does not confuse it with a failed read. */
    if (result == 0) return rc == 0 ? 0 : 0;
    /* The app id lives in the low 32 bits; SMP decodes the same way. */
    return (long long)(result & 0xFFFFFFFFu);
}

static void resolve_focus_apis(void) {
    size_t i;
    if (focus_resolve_attempted) return;
    focus_resolve_attempted = 1;

    for (i = 0; i < FOCUS_CANDIDATE_COUNT; i++) {
        focus_available[i] = resolve_quiet(FOCUS_CANDIDATES[i]) ? 1 : 0;
        fprintf(stderr, "[payload2] focus probe: %s=%s\n",
                FOCUS_CANDIDATES[i], focus_available[i] ? "yes" : "no");
    }

    p_evf_open  = (sce_evf_open_fn)resolve_quiet("sceKernelOpenEventFlag");
    p_evf_poll  = (sce_evf_poll_fn)resolve_quiet("sceKernelPollEventFlag");
    p_evf_close = (sce_evf_close_fn)resolve_quiet("sceKernelCloseEventFlag");
    (void)p_evf_close; /* handle is cached for process lifetime */

    /* Safe to call: documented as taking no arguments. */
    p_big_app  = (sce_get_app_id_fn)resolve_quiet(
        "sceSystemServiceGetAppIdOfBigApp");
    p_mini_app = (sce_get_app_id_fn)resolve_quiet(
        "sceSystemServiceGetAppIdOfMiniApp");
}

/* CLOCK_MONOTONIC millis so a caller sampling every second can spot a
 * gap (helper restarted, console slept) rather than reading a stalled
 * value as a steady one. */
static long long monotonic_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return -1;
    return (long long)ts.tv_sec * 1000LL + (long long)(ts.tv_nsec / 1000000L);
}

int focus_probe_get_json(char *buf, size_t cap, size_t *written_out,
                         const char **err_out) {
    int big = 0, mini = 0;
    int n;
    size_t off = 0, i;

    if (!buf || cap == 0) {
        if (err_out) *err_out = "focus_probe_bad_args";
        return -1;
    }

    resolve_focus_apis();

    if (p_big_app)  big  = p_big_app();
    if (p_mini_app) mini = p_mini_app();

    /* Emit the whole candidate table so a single probe tells us what this
     * firmware can answer, rather than one guess per redeploy. */
    n = snprintf(buf, cap, "{\"ok\":true,\"apis\":{");
    if (n < 0 || (size_t)n >= cap) {
        if (err_out) *err_out = "focus_probe_buf_too_small";
        return -1;
    }
    off = (size_t)n;
    for (i = 0; i < FOCUS_CANDIDATE_COUNT; i++) {
        n = snprintf(buf + off, cap - off, "%s\"%s\":%s",
                     i ? "," : "", FOCUS_CANDIDATES[i],
                     focus_available[i] ? "true" : "false");
        if (n < 0 || (size_t)n >= cap - off) {
            if (err_out) *err_out = "focus_probe_buf_too_small";
            return -1;
        }
        off += (size_t)n;
    }
    /* App scheduler state — the part that actually answers "is the game on
     * screen?" on a firmware with no focus getter. See proc_list.h. */
    n = snprintf(buf + off, cap - off, "},\"apps\":");
    if (n < 0 || (size_t)n >= cap - off) {
        if (err_out) *err_out = "focus_probe_buf_too_small";
        return -1;
    }
    off += (size_t)n;
    {
        size_t app_written = 0;
        const char *app_err = NULL;
        if (proc_list_app_states_json(buf + off, cap - off, &app_written,
                                      &app_err) != 0) {
            /* Degrade rather than fail the whole probe: the symbol map is
             * still useful without the state list. */
            n = snprintf(buf + off, cap - off, "[]");
            if (n < 0 || (size_t)n >= cap - off) {
                if (err_out) *err_out = "focus_probe_buf_too_small";
                return -1;
            }
            off += (size_t)n;
        } else {
            off += app_written;
        }
    }

    {
        int frc = 0;
        long long fid = read_focus_app_id(&frc);
        n = snprintf(buf + off, cap - off,
                     ",\"focus_app_id\":%lld,\"focus_rc\":%d,"
                     "\"focus_available\":%s",
                     fid, frc,
                     (p_evf_open && p_evf_poll && !g_focus_ef_open_failed)
                         ? "true" : "false");
        if (n < 0 || (size_t)n >= cap - off) {
            if (err_out) *err_out = "focus_probe_buf_too_small";
            return -1;
        }
        off += (size_t)n;
    }

    n = snprintf(buf + off, cap - off,
                 ",\"big_app\":%s,\"mini_app\":%s,"
                 "\"big_app_id\":%d,\"mini_app_id\":%d,"
                 "\"monotonic_ms\":%lld}",
                 p_big_app ? "true" : "false",
                 p_mini_app ? "true" : "false",
                 big, mini, monotonic_ms());
    if (n >= 0 && (size_t)n < cap - off) off += (size_t)n;

    if (written_out) *written_out = off;
    return 0;
}
