/* PS5 system clock get/set. See sys_time.h for rationale. */

#include "sys_time.h"

#include <dlfcn.h>
#include <pthread.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

/* Function pointer types match Sony's libSceSystemService exports. */
typedef int (*set_dt_fn)(const sce_datetime_t *);
typedef int (*get_dt_fn)(sce_datetime_t *);

/* dlsym cache. Resolved once on first call via pthread_once; subsequent
 * calls hit the cached pointers. NULL = symbol genuinely absent on this
 * firmware. The previous hand-rolled double-checked lock had a C11 data
 * race (unsynchronized read of g_resolved); pthread_once gives the same
 * fast path with defined semantics, matching sys_registry.c's pattern. */
static set_dt_fn       g_set       = NULL;
static get_dt_fn       g_get       = NULL;
static pthread_once_t  g_resolve_once = PTHREAD_ONCE_INIT;

static void resolve_impl(void) {
    /* RTLD_DEFAULT searches every loaded library, matching the
     * pattern in hw_info.c / register.c. The whole point is that
     * the lookup doesn't fail at payload init when the SDK stub
     * is link-time-bound but the runtime SPRX is missing the
     * NID — we want to find out at first call, not at boot. */
    g_set = (set_dt_fn)dlsym(RTLD_DEFAULT,
                              "sceSystemServiceSetCurrentDateTime");
    g_get = (get_dt_fn)dlsym(RTLD_DEFAULT,
                              "sceSystemServiceGetCurrentDateTime");
}

static void resolve_once(void) {
    pthread_once(&g_resolve_once, resolve_impl);
}

/* Convert a Sony date/time (UTC) to a unix epoch in seconds, or -1
 * if the date doesn't parse. Used purely as a diagnostic — the
 * payload doesn't otherwise care about epoch math.
 *
 * We use timegm rather than mktime because the PS5 stores the system
 * clock in UTC; mktime would apply the (uninitialised) local TZ on a
 * FreeBSD-flavour libc and silently give the wrong epoch. */
int64_t sys_time_sce_to_unix(const sce_datetime_t *dt) {
    if (!dt) return -1;
    if (dt->year < 1970 || dt->year > 2200) return -1;
    if (dt->month < 1 || dt->month > 12) return -1;
    if (dt->day < 1 || dt->day > 31) return -1;
    if (dt->hour > 23) return -1;
    if (dt->minute > 59) return -1;
    if (dt->second > 59) return -1;
    struct tm tm;
    memset(&tm, 0, sizeof(tm));
    tm.tm_year = (int)dt->year - 1900;
    tm.tm_mon  = (int)dt->month - 1;
    tm.tm_mday = (int)dt->day;
    tm.tm_hour = (int)dt->hour;
    tm.tm_min  = (int)dt->minute;
    tm.tm_sec  = (int)dt->second;
    time_t t = timegm(&tm);
    if (t == (time_t)-1) return -1;
    return (int64_t)t;
}

int sys_time_unix_to_sce(int64_t unix_seconds, sce_datetime_t *out) {
    if (!out) return -1;
    /* Negative epochs are pre-1970, which sce_datetime_t's unsigned
     * year cannot express and no console clock should hold. */
    if (unix_seconds < 0) return -1;
    time_t t = (time_t)unix_seconds;
    struct tm tm;
    if (!gmtime_r(&t, &tm)) return -1;
    if (tm.tm_year > 2200 - 1900) return -1;
    memset(out, 0, sizeof(*out));
    out->year   = (uint16_t)(tm.tm_year + 1900);
    out->month  = (uint16_t)(tm.tm_mon + 1);
    out->day    = (uint16_t)tm.tm_mday;
    out->hour   = (uint16_t)tm.tm_hour;
    out->minute = (uint16_t)tm.tm_min;
    out->second = (uint16_t)tm.tm_sec;
    return 0;
}

int sys_time_get(sce_datetime_t *out, uint32_t *out_err_code) {
    if (!out) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (g_get) {
        memset(out, 0, sizeof(*out));
        int rc = g_get(out);
        if (rc == 0) {
            if (out_err_code) *out_err_code = 0;
            return 0;
        }
        /* Fall through to the kernel clock rather than reporting
         * failure — a rejected IPC does not mean the clock is
         * unreadable. Keep the SCE code if the fallback fails too. */
        if (out_err_code) *out_err_code = (uint32_t)rc;
    } else if (out_err_code) {
        *out_err_code = SYS_TIME_ERR_NO_SYMBOL;
    }

    /* Kernel wall clock. On FW 9.60 the SCE getter is not exported, so
     * this is the only way the console can report its own time. */
    struct timeval tv;
    if (gettimeofday(&tv, NULL) == 0 &&
        sys_time_unix_to_sce((int64_t)tv.tv_sec, out) == 0) {
        if (out_err_code) *out_err_code = 0;
        return 0;
    }
    return -1;
}

int sys_time_needs_fallback(int have_set_symbol,
                            int set_rc,
                            int64_t target_unix,
                            int64_t observed_unix) {
    if (!have_set_symbol) return 1;
    if (set_rc != 0) return 1;
    /* No read-back: we have no evidence the set failed, so trust it
     * rather than writing the clock a second time. */
    if (observed_unix < 0) return 0;
    /* Magnitude of the difference, computed in unsigned arithmetic so
     * that a request-controlled `target_unix` cannot overflow the
     * subtraction. Signed overflow is undefined behaviour in C, not
     * merely a wrong comparison; ordering the operands first keeps the
     * unsigned result exact. */
    uint64_t diff = (observed_unix >= target_unix)
                        ? (uint64_t)observed_unix - (uint64_t)target_unix
                        : (uint64_t)target_unix - (uint64_t)observed_unix;
    return diff > (uint64_t)SYS_TIME_SET_TOLERANCE_SEC;
}

/* Read the wall clock without going through SCE. Used to verify the
 * settimeofday fallback, which is exactly the path taken when the SCE
 * getter is missing too. Returns -1 if unavailable. */
static int64_t kernel_wall_clock_unix(void) {
    struct timeval tv;
    if (gettimeofday(&tv, NULL) != 0) return -1;
    return (int64_t)tv.tv_sec;
}

int sys_time_set(const sce_datetime_t *dt,
                 uint32_t *out_err_code,
                 int64_t *out_prior_unix,
                 int64_t *out_new_unix,
                 int *out_used_fallback) {
    if (!dt) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NULL_ARG;
        return -1;
    }
    if (out_prior_unix)    *out_prior_unix    = -1;
    if (out_new_unix)      *out_new_unix      = -1;
    if (out_used_fallback) *out_used_fallback = 0;

    resolve_once();

    /* Capture the prior clock value (best-effort) BEFORE the set, so
     * the desktop can compute drift / detect stub-no-op set calls.
     * If get itself fails we still proceed to the set — we'd rather
     * the user's set attempt go through with no diagnostic data than
     * fail the whole call on a get-side issue. */
    if (out_prior_unix) {
        int64_t prior = -1;
        if (g_get) {
            sce_datetime_t p;
            memset(&p, 0, sizeof(p));
            if (g_get(&p) == 0) prior = sys_time_sce_to_unix(&p);
        }
        if (prior < 0) prior = kernel_wall_clock_unix();
        *out_prior_unix = prior;
    }

    uint32_t ec = 0;
    int rc;
    if (g_set) {
        rc = g_set(dt);
        ec = (uint32_t)rc;
    } else {
        rc = -1;
        ec = SYS_TIME_ERR_NO_SYMBOL;
    }

    /* Capture the post-set clock so we (and the desktop) can decide
     * whether the call took. Some firmware/SPRX combos return rc=0
     * from set but the underlying syscall is a no-op. */
    int64_t observed = -1;
    if (g_get) {
        sce_datetime_t after;
        memset(&after, 0, sizeof(after));
        if (g_get(&after) == 0) observed = sys_time_sce_to_unix(&after);
    }

    /* Fallback: set the kernel wall clock directly.
     *
     * This is the only path the ps5-date-time-sync reference uses, and
     * it works in cases the SCE call cannot — the symbol may be absent,
     * ShellCore may reject the IPC on a non-elevated loader, or the
     * SDK stub may be a no-op. It still requires an elevated ucred, so
     * it is not a way around needing kstuff; it is a second door into
     * the same room, and on several firmwares only one of the two is
     * open.
     *
     * Guarded on a parseable target: a garbage date must not reach
     * settimeofday, which would happily install it. */
    int64_t target = sys_time_sce_to_unix(dt);
    if (target >= 0 &&
        sys_time_needs_fallback(g_set != NULL, rc, target, observed)) {
        struct timeval tv;
        memset(&tv, 0, sizeof(tv));
        tv.tv_sec  = (time_t)target;
        tv.tv_usec = 0;
        if (settimeofday(&tv, NULL) == 0) {
            int64_t after_fb = -1;
            if (g_get) {
                sce_datetime_t after;
                memset(&after, 0, sizeof(after));
                if (g_get(&after) == 0) after_fb = sys_time_sce_to_unix(&after);
            }
            if (after_fb < 0) after_fb = kernel_wall_clock_unix();
            /* Only claim success if the clock actually moved. A
             * settimeofday that returns 0 without taking effect must
             * not be reported as a successful set. */
            if (!sys_time_needs_fallback(1, 0, target, after_fb)) {
                observed = after_fb;
                rc = 0;
                ec = 0;
                if (out_used_fallback) *out_used_fallback = 1;
            } else if (rc == 0) {
                /* SCE claimed success, the fallback ran, and the clock
                 * is still wrong. Say so rather than reporting ok. */
                rc = -1;
                ec = SYS_TIME_ERR_FALLBACK;
            }
        } else if (rc != 0 && ec == 0) {
            ec = SYS_TIME_ERR_FALLBACK;
        }
    }

    if (out_new_unix) *out_new_unix = observed;
    if (out_err_code) *out_err_code = ec;
    return rc == 0 ? 0 : -1;
}
