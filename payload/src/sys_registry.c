/* PS5 sceRegMgr wrappers. See sys_registry.h for rationale. */

#include "sys_registry.h"

#include <dlfcn.h>
#include <pthread.h>
#include <string.h>

/* Function pointer types match Sony's libSceRegMgr exports. The
 * `size` arg on the Set variants is part of Sony's signature even
 * for int values (where it's always 4) — kept in the typedef so
 * callsites can't accidentally drop it. */
typedef int (*regmgr_get_int_fn)(uint32_t key, int *out_val);
typedef int (*regmgr_set_int_fn)(uint32_t key, int val);
typedef int (*regmgr_get_str_fn)(uint32_t key, char *buf, int buf_size);
typedef int (*regmgr_set_str_fn)(uint32_t key, const char *buf, size_t len);
typedef int (*regmgr_get_bin_fn)(uint32_t key, void *buf, size_t buf_size);
typedef int (*regmgr_set_bin_fn)(uint32_t key, const void *buf, size_t len);

/* libSceRtc has its own NID space — separate dlsym site. The "tick"
 * Sony talks about is a uint64 of 1-microsecond ticks since
 * 1970-01-01T00:00:00 UTC (Sony's convention; see PS4 LibSceRtc
 * wiki). We convert to unix-seconds at the call boundary. */
typedef int (*rtc_get_network_tick_fn)(uint64_t *out_tick);

/* dlsym caches. Same pattern as sys_time.c. `g_resolved` flag tells
 * "we tried, here's the result" vs "we haven't tried yet". A NULL
 * pointer post-resolution means Sony genuinely doesn't export the
 * symbol on this firmware — every caller in this file's public API
 * surface translates that into SYS_REGISTRY_ERR_NO_SYMBOL. */
static regmgr_get_int_fn       g_get_int = NULL;
static regmgr_set_int_fn       g_set_int = NULL;
static regmgr_get_str_fn       g_get_str = NULL;
static regmgr_set_str_fn       g_set_str = NULL;
static regmgr_get_bin_fn       g_get_bin = NULL;
static regmgr_set_bin_fn       g_set_bin = NULL;
static rtc_get_network_tick_fn g_rtc_get_ntp_tick = NULL;
static pthread_once_t          g_resolve_once = PTHREAD_ONCE_INIT;

static void resolve_impl(void) {
    /* RTLD_DEFAULT walks every loaded SPRX — same pattern as
     * sys_time.c. We deliberately don't dlopen a specific .sprx
     * because the runtime export surface differs from the SDK
     * stub layout: on some firmwares libSceRegMgr.sprx is loaded
     * indirectly via SceShellCore IPC, NOT as a direct dep of
     * our payload, so dlopen("libSceRegMgr.sprx", ...) would
     * fail while RTLD_DEFAULT still finds the symbol. */
    g_get_int = (regmgr_get_int_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrGetInt");
    g_set_int = (regmgr_set_int_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrSetInt");
    g_get_str = (regmgr_get_str_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrGetStr");
    g_set_str = (regmgr_set_str_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrSetStr");
    g_get_bin = (regmgr_get_bin_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrGetBin");
    g_set_bin = (regmgr_set_bin_fn)dlsym(RTLD_DEFAULT,
                                          "sceRegMgrSetBin");
    g_rtc_get_ntp_tick = (rtc_get_network_tick_fn)dlsym(
        RTLD_DEFAULT, "sceRtcGetCurrentNetworkTick");
}

static void resolve_once(void) {
    /* pthread_once instead of the previous hand-rolled
     * double-checked lock: the unsynchronized fast-path read of a
     * plain int was a C11 data race (UB; benign on x86-64 TSO but
     * a compiler is allowed to break it). pthread_once is the
     * codebase's standard for one-shot init and gives the same
     * fast path with defined semantics. */
    pthread_once(&g_resolve_once, resolve_impl);
}

int sys_registry_get_int(uint32_t key, int *out_val,
                          uint32_t *out_err_code) {
    if (!out_val) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (!g_get_int) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    *out_val = 0;
    int rc = g_get_int(key, out_val);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_set_int(uint32_t key, int val,
                          uint32_t *out_err_code) {
    resolve_once();
    if (!g_set_int) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    int rc = g_set_int(key, val);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_get_str(uint32_t key,
                          char *buf, size_t buf_size,
                          uint32_t *out_err_code) {
    if (!buf || buf_size == 0) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    if (buf_size > 0x7FFFFFFFU) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_BUFFER_TOO_SMALL;
        return -1;
    }
    resolve_once();
    if (!g_get_str) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    buf[0] = '\0';
    int rc = g_get_str(key, buf, (int)buf_size);
    /* Defence-in-depth NUL-terminate. Sony's wrappers SHOULD NUL-pad
     * inside buf_size, but a max-length string in a too-small buffer
     * is a real ambiguity in their documentation — pin it. */
    buf[buf_size - 1] = '\0';
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_set_str(uint32_t key,
                          const char *buf, size_t len,
                          uint32_t *out_err_code) {
    if (!buf) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (!g_set_str) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    int rc = g_set_str(key, buf, len);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_get_bin(uint32_t key,
                          void *buf, size_t buf_size,
                          uint32_t *out_err_code) {
    if (!buf || buf_size == 0) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (!g_get_bin) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    int rc = g_get_bin(key, buf, buf_size);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_set_bin(uint32_t key,
                          const void *buf, size_t buf_size,
                          uint32_t *out_err_code) {
    if (!buf || buf_size == 0) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (!g_set_bin) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    int rc = g_set_bin(key, buf, buf_size);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_registry_get_ntp_tick_unix(int64_t *out_unix_seconds,
                                    uint32_t *out_err_code) {
    if (!out_unix_seconds) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NULL_ARG;
        return -1;
    }
    *out_unix_seconds = -1;
    resolve_once();
    if (!g_rtc_get_ntp_tick) {
        if (out_err_code) *out_err_code = SYS_REGISTRY_ERR_NO_SYMBOL;
        return -1;
    }
    uint64_t tick = 0;
    int rc = g_rtc_get_ntp_tick(&tick);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    if (rc != 0) return -1;
    /* Sony's tick is microseconds since 1970-01-01T00:00:00 UTC.
     * Truncate to seconds; we never need sub-second precision for
     * drift display (the wall-clock get is whole-seconds anyway). */
    *out_unix_seconds = (int64_t)(tick / 1000000ULL);
    return 0;
}
