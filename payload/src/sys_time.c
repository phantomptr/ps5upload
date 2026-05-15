/* PS5 system clock get/set. See sys_time.h for rationale. */

#include "sys_time.h"

#include <dlfcn.h>
#include <pthread.h>
#include <string.h>
#include <time.h>

/* Function pointer types match Sony's libSceSystemService exports. */
typedef int (*set_dt_fn)(const sce_datetime_t *);
typedef int (*get_dt_fn)(sce_datetime_t *);

/* dlsym cache. Resolved once on first call; subsequent calls hit the
 * cache. NULL = not-yet-resolved AND symbol absent both look the same
 * from the cache layer, which is why we keep a separate `g_resolved`
 * flag — `g_set == NULL` after a resolution attempt means the symbol
 * is genuinely missing on this firmware. */
static set_dt_fn       g_set       = NULL;
static get_dt_fn       g_get       = NULL;
static int             g_resolved  = 0;
static pthread_mutex_t g_init_lock = PTHREAD_MUTEX_INITIALIZER;

static void resolve_once(void) {
    if (g_resolved) return;
    pthread_mutex_lock(&g_init_lock);
    if (!g_resolved) {
        /* RTLD_DEFAULT searches every loaded library, matching the
         * pattern in hw_info.c / register.c. The whole point is that
         * the lookup doesn't fail at payload init when the SDK stub
         * is link-time-bound but the runtime SPRX is missing the
         * NID — we want to find out at first call, not at boot. */
        g_set = (set_dt_fn)dlsym(RTLD_DEFAULT,
                                  "sceSystemServiceSetCurrentDateTime");
        g_get = (get_dt_fn)dlsym(RTLD_DEFAULT,
                                  "sceSystemServiceGetCurrentDateTime");
        g_resolved = 1;
    }
    pthread_mutex_unlock(&g_init_lock);
}

/* Convert a Sony date/time (UTC) to a unix epoch in seconds, or -1
 * if the date doesn't parse. Used purely as a diagnostic — the
 * payload doesn't otherwise care about epoch math.
 *
 * We use timegm rather than mktime because the PS5 stores the system
 * clock in UTC; mktime would apply the (uninitialised) local TZ on a
 * FreeBSD-flavour libc and silently give the wrong epoch. */
static int64_t sce_dt_to_unix(const sce_datetime_t *dt) {
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

int sys_time_get(sce_datetime_t *out, uint32_t *out_err_code) {
    if (!out) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NULL_ARG;
        return -1;
    }
    resolve_once();
    if (!g_get) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NO_SYMBOL;
        return -1;
    }
    memset(out, 0, sizeof(*out));
    int rc = g_get(out);
    if (out_err_code) *out_err_code = (uint32_t)rc;
    return rc == 0 ? 0 : -1;
}

int sys_time_set(const sce_datetime_t *dt,
                 uint32_t *out_err_code,
                 int64_t *out_prior_unix,
                 int64_t *out_new_unix) {
    if (!dt) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NULL_ARG;
        return -1;
    }
    if (out_prior_unix) *out_prior_unix = -1;
    if (out_new_unix)   *out_new_unix   = -1;

    resolve_once();

    /* Capture the prior clock value (best-effort) BEFORE the set, so
     * the desktop can compute drift / detect stub-no-op set calls.
     * If get itself fails we still proceed to the set — we'd rather
     * the user's set attempt go through with no diagnostic data than
     * fail the whole call on a get-side issue. */
    if (g_get && out_prior_unix) {
        sce_datetime_t prior;
        memset(&prior, 0, sizeof(prior));
        if (g_get(&prior) == 0) {
            int64_t u = sce_dt_to_unix(&prior);
            if (u >= 0) *out_prior_unix = u;
        }
    }

    if (!g_set) {
        if (out_err_code) *out_err_code = SYS_TIME_ERR_NO_SYMBOL;
        return -1;
    }

    int rc = g_set(dt);
    if (out_err_code) *out_err_code = (uint32_t)rc;

    /* Capture the post-set clock so the desktop can decide whether
     * the call took. Some firmware/SPRX combos return rc=0 from
     * set but the underlying syscall is a no-op — comparing
     * new_unix against the requested epoch reveals that. */
    if (g_get && out_new_unix) {
        sce_datetime_t after;
        memset(&after, 0, sizeof(after));
        if (g_get(&after) == 0) {
            int64_t u = sce_dt_to_unix(&after);
            if (u >= 0) *out_new_unix = u;
        }
    }

    return rc == 0 ? 0 : -1;
}
