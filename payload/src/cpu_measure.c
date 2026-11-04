/* The CPU cores' real clock, measured with a timed chain of adds: see cpu_measure.h. */

#include "cpu_measure.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>

static pthread_mutex_t cpum_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_cpum_said_no_pin; /* the log line for "no CPU took the pin" is out */
static int g_cpum_cache_valid;
static int64_t g_cpum_cache_ms;
static cpum_clocks_t g_cpum_cache;

uint32_t cpum_mhz(uint64_t adds, uint64_t ns) {
    if (ns == 0) return 0;
    /* adds per microsecond = MHz */
    uint64_t mhz = adds * 1000ull / ns;
    if (mhz < CPUM_MIN_MHZ || mhz > CPUM_MAX_MHZ) return 0;
    return (uint32_t)mhz;
}

typedef struct {
    const cpum_ops_t *ops;
    cpum_clocks_t *out;
    int pinned; /* how many logical CPUs accepted the pin */
} cpum_job_t;

/* On the worker thread: each logical CPU in turn, the first one of each physical core measured. */
static void cpum_job(void *arg) {
    cpum_job_t *j = arg;
    const cpum_ops_t *o = j->ops;
    int done[CPUM_CORES] = {0};
    for (int cpu = 0; cpu < CPUM_LOGICAL; cpu++) {
        if (o->pin(o->ctx, cpu) != 0) continue;
        j->pinned++;
        int core = o->core_now(o->ctx);
        if (core < 0 || core >= CPUM_CORES || done[core]) continue;
        done[core] = 1;
        uint64_t best = 0;
        for (int r = 0; r < CPUM_RUNS; r++) {
            uint64_t ns = o->chain_ns(o->ctx);
            if (ns && (best == 0 || ns < best)) best = ns;
        }
        j->out->mhz[core] = cpum_mhz(CPUM_ADDS, best);
    }
}

int cpu_measure_read(const cpum_ops_t *ops, cpum_clocks_t *out) {
    if (!ops || !out) return CPUM_UNSUPPORTED;
    memset(out, 0, sizeof *out);
    pthread_mutex_lock(&cpum_lock);
    int64_t now = ops->now_ms(ops->ctx);
    if (g_cpum_cache_valid && now - g_cpum_cache_ms >= 0 && now - g_cpum_cache_ms < CPUM_CACHE_MS) {
        *out = g_cpum_cache;
        pthread_mutex_unlock(&cpum_lock);
        return CPUM_OK;
    }
    cpum_clocks_t fresh;
    memset(&fresh, 0, sizeof fresh);
    cpum_job_t job = {ops, &fresh, 0};
    if (ops->run(ops->ctx, cpum_job, &job) != 0) {
        /* No worker this time (out of threads): nothing measured, try again next time. */
        out->state = CPUM_OK;
        pthread_mutex_unlock(&cpum_lock);
        return CPUM_OK;
    }
    if (job.pinned == 0) {
        /* Not remembered: a game can refuse every core for as long as it runs, and this costs a
         * thread and a few calls per explicit read. What worked earlier stays the answer. */
        int was_said = g_cpum_said_no_pin;
        int rc = CPUM_UNSUPPORTED;
        g_cpum_said_no_pin = 1;
        if (g_cpum_cache_valid) {
            *out = g_cpum_cache;
            rc = CPUM_OK;
        } else {
            out->state = CPUM_UNSUPPORTED;
        }
        pthread_mutex_unlock(&cpum_lock);
        if (!was_said) printf("[cpu_measure] no CPU accepts a pinned thread right now: nothing measured\n");
        return rc;
    }
    g_cpum_said_no_pin = 0;
    fresh.state = CPUM_OK;
    g_cpum_cache = fresh;
    g_cpum_cache_ms = ops->now_ms(ops->ctx);
    g_cpum_cache_valid = 1;
    *out = fresh;
    pthread_mutex_unlock(&cpum_lock);
    return CPUM_OK;
}

int cpu_measure_format(const cpum_clocks_t *c, char *out, size_t cap) {
    if (!c || !out || cap == 0) return -1;
    int ok = c->state == CPUM_OK;
    uint32_t v[CPUM_CORES];
    for (int i = 0; i < CPUM_CORES; i++) v[i] = ok ? c->mhz[i] : 0;
    return snprintf(out, cap, "cpu_measure_src=%s\ncpu_core_measured_mhz=%u,%u,%u,%u,%u,%u,%u,%u\n",
                    ok ? "add_chain" : "unsupported", (unsigned)v[0], (unsigned)v[1],
                    (unsigned)v[2], (unsigned)v[3], (unsigned)v[4], (unsigned)v[5],
                    (unsigned)v[6], (unsigned)v[7]);
}

void cpu_measure_reset(void) {
    pthread_mutex_lock(&cpum_lock);
    g_cpum_said_no_pin = 0;
    g_cpum_cache_valid = 0;
    memset(&g_cpum_cache, 0, sizeof g_cpum_cache);
    pthread_mutex_unlock(&cpum_lock);
}
