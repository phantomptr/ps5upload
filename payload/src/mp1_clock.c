/* GPU and per-core CPU clock from /dev/mp1, read-only. See mp1_clock.h for the requests and the
 * safety rules. The request numbers and the meaning of GFX word 21 / the core struct are from
 * NeoEnabler (github.com/gasb07/NeoEnabler, src/gpu_control.h, src/cpu_control.h); only its
 * read paths are used here, rewritten. No device or Sony API is touched directly: the caller
 * passes the calls in (mp1_ops_t), so a host test can drive this with a fake driver. */
#include "mp1_clock.h"

#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#define MP1_PSTATE_NONE 0xFFFFFFFFu

static pthread_mutex_t mp1_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_mp1_unsupported;  /* latched on the first refusal; never cleared (but by the test) */
static int g_mp1_cache_valid;
static int64_t g_mp1_cache_ms;
static mp1_clocks_t g_mp1_cache;

uint32_t mp1_gpu_mhz_from_table(const uint32_t words[MP1_GFX_WORDS]) {
    uint32_t v = words[MP1_GFX_MHZ_WORD];
    return (v >= MP1_GPU_MIN_MHZ && v <= MP1_GPU_MAX_MHZ) ? v : 0;
}

uint32_t mp1_core_mhz_valid(uint32_t mhz) {
    return (mhz >= MP1_CORE_MIN_MHZ && mhz <= MP1_CORE_MAX_MHZ) ? mhz : 0;
}

static void mp1_blank(mp1_clocks_t *c, int state) {
    memset(c, 0, sizeof *c);
    c->state = state;
    for (int i = 0; i < MP1_CORES; i++) c->core_pstate[i] = MP1_PSTATE_NONE;
}

/* The only way a request reaches the driver: anything but the two reads is refused here, so no
 * caller mistake can turn into a clock change. */
static int mp1_get(const mp1_ops_t *ops, int fd, unsigned long req, void *buf) {
    if (req != MP1_GFX_IOCTL_GET && req != MP1_CORE_IOCTL_GET) {
        errno = EPERM;
        return -1;
    }
    return ops->get(ops->ctx, fd, req, buf);
}

/* Caller holds mp1_lock. */
static void mp1_latch(const char *what, int err) {
    g_mp1_unsupported = 1;
    g_mp1_cache_valid = 0;
    fprintf(stderr, "[mp1] %s failed (errno %d); clock reads off until the helper restarts\n",
            what, err);
}

/* "Not now", from the driver or the helper, as opposed to "no such thing": the SMU busy while a game
 * runs, a signal, out of descriptors for a moment. Anything else, known or not, is a refusal. */
int mp1_errno_transient(int err) {
    switch (err) {
    case EBUSY:
    case EAGAIN:
    case EDEADLK:
    case EINTR:
    case ETIMEDOUT:
    case EMFILE:
    case ENFILE:
    case ENOMEM:
        return 1;
    default:
        return 0;
    }
}

/* A read that failed with errno `err`. Caller holds mp1_lock. A refusal latches. "Not now" answers
 * with the last good values if there are any (zeros, that is "unknown", if not) and the next
 * explicit read tries again: one busy moment must not turn the clocks into "unsupported" for good. */
static int mp1_failed(const char *what, int err, mp1_clocks_t *out) {
    if (!mp1_errno_transient(err)) {
        mp1_latch(what, err);
        return MP1_UNSUPPORTED;
    }
    fprintf(stderr, "[mp1] %s: not now (errno %d); keeping the last values\n", what, err);
    if (g_mp1_cache_valid) *out = g_mp1_cache;
    else mp1_blank(out, MP1_OK);
    return MP1_OK;
}

int mp1_clocks_read(const mp1_ops_t *ops, mp1_clocks_t *out) {
    if (!out) return MP1_UNSUPPORTED;
    mp1_blank(out, MP1_UNSUPPORTED);
    if (!ops || !ops->open_ro || !ops->get || !ops->close_fd || !ops->now_ms) return MP1_UNSUPPORTED;

    pthread_mutex_lock(&mp1_lock);
    if (g_mp1_unsupported) {
        pthread_mutex_unlock(&mp1_lock);
        return MP1_UNSUPPORTED;
    }
    int64_t now = ops->now_ms(ops->ctx);
    if (g_mp1_cache_valid && now >= g_mp1_cache_ms && now - g_mp1_cache_ms < MP1_CACHE_MS) {
        *out = g_mp1_cache;
        pthread_mutex_unlock(&mp1_lock);
        return MP1_OK;
    }

    mp1_clocks_t res;
    mp1_blank(&res, MP1_OK);

    int fd = ops->open_ro(ops->ctx);
    if (fd < 0) {
        int rc = mp1_failed("open /dev/mp1", errno, out);
        pthread_mutex_unlock(&mp1_lock);
        return rc;
    }

    /* 256 zeroed, 8-byte-aligned bytes for a request that moves 108. */
    _Alignas(8) uint32_t words[MP1_GFX_BUF_WORDS];
    memset(words, 0, sizeof words);
    if (mp1_get(ops, fd, MP1_GFX_IOCTL_GET, words) != 0) {
        int e = errno;
        ops->close_fd(ops->ctx, fd);
        int rc = mp1_failed("GFX clock read", e, out);
        pthread_mutex_unlock(&mp1_lock);
        return rc;
    }
    for (int i = MP1_GFX_WORDS; i < MP1_GFX_BUF_WORDS; i++) {
        if (words[i] != 0) {
            ops->close_fd(ops->ctx, fd);
            fprintf(stderr, "[mp1] GFX read wrote past %d bytes (word %d = 0x%08x)\n",
                    MP1_GFX_WORDS * 4, i, (unsigned)words[i]);
            mp1_latch("GFX clock read size", 0);
            pthread_mutex_unlock(&mp1_lock);
            return MP1_UNSUPPORTED;
        }
    }
    memcpy(res.gfx_raw, words, sizeof res.gfx_raw);
    res.gpu_mhz = mp1_gpu_mhz_from_table(res.gfx_raw);

    for (uint32_t c = 0; c < MP1_CORES; c++) {
        _Alignas(8) uint32_t q[3] = { c, 0, 0 };
        if (mp1_get(ops, fd, MP1_CORE_IOCTL_GET, q) != 0) {
            int e = errno;
            ops->close_fd(ops->ctx, fd);
            int rc = mp1_failed("CPU core clock read", e, out);
            pthread_mutex_unlock(&mp1_lock);
            return rc;
        }
        res.core_raw_mhz[c] = q[2];
        res.core_mhz[c] = mp1_core_mhz_valid(q[2]);
        res.core_pstate[c] = q[1] <= MP1_CORE_PSTATE_MAX ? q[1] : MP1_PSTATE_NONE;
    }
    ops->close_fd(ops->ctx, fd);

    g_mp1_cache = res;
    g_mp1_cache_ms = now;
    g_mp1_cache_valid = 1;
    *out = res;
    pthread_mutex_unlock(&mp1_lock);
    return MP1_OK;
}

void mp1_clocks_reset(void) {
    pthread_mutex_lock(&mp1_lock);
    g_mp1_unsupported = 0;
    g_mp1_cache_valid = 0;
    g_mp1_cache_ms = 0;
    pthread_mutex_unlock(&mp1_lock);
}

__attribute__((format(printf, 4, 5)))
static void mp1_put(char *out, size_t cap, size_t *n, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(*n < cap ? out + *n : NULL, *n < cap ? cap - *n : 0, fmt, ap);
    va_end(ap);
    if (r > 0) *n += (size_t)r;
}

int mp1_clocks_format(const mp1_clocks_t *c, int debug, char *out, size_t cap) {
    if (!c || !out || cap == 0) return -1;
    size_t n = 0;
    out[0] = '\0';
    int ok = c->state == MP1_OK;
    mp1_put(out, cap, &n, "gpu_clock_src=%s\ncpu_core_mhz=", ok ? "mp1" : "unsupported");
    uint32_t lo = 0, hi = 0;
    for (int i = 0; i < MP1_CORES; i++) {
        uint32_t v = ok ? c->core_mhz[i] : 0;
        mp1_put(out, cap, &n, "%s%u", i ? "," : "", (unsigned)v);
        if (v == 0) continue;
        if (lo == 0 || v < lo) lo = v;
        if (v > hi) hi = v;
    }
    mp1_put(out, cap, &n, "\ncpu_core_mhz_min=%u\ncpu_core_mhz_max=%u\n", (unsigned)lo,
            (unsigned)hi);
    if (debug && ok) {
        mp1_put(out, cap, &n, "mp1_clk_raw=");
        for (int i = 0; i < MP1_GFX_WORDS; i++)
            mp1_put(out, cap, &n, "%s%08x", i ? "," : "", (unsigned)c->gfx_raw[i]);
        mp1_put(out, cap, &n, "\nmp1_core_raw=");
        for (int i = 0; i < MP1_CORES; i++) {
            if (c->core_pstate[i] == MP1_PSTATE_NONE)
                mp1_put(out, cap, &n, "%s-:%u", i ? "," : "", (unsigned)c->core_raw_mhz[i]);
            else
                mp1_put(out, cap, &n, "%s%u:%u", i ? "," : "", (unsigned)c->core_pstate[i],
                        (unsigned)c->core_raw_mhz[i]);
        }
        mp1_put(out, cap, &n, "\n");
    }
    return n > (size_t)0x7fffffff ? -1 : (int)n;
}
