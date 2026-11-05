#ifndef PS5UPLOAD_CPU_MEASURE_H
#define PS5UPLOAD_CPU_MEASURE_H

#include <stddef.h>
#include <stdint.h>

/* The CPU cores' real clock, measured: a chain of dependent adds takes one cycle per add on
 * Zen 2, so adds per second is the clock the core runs at while busy. /dev/mp1 (mp1_clock.h)
 * only knows each core's P-state target; measured on a PS5 (FW 13.60) the cores ran 1-8% under
 * it. The chip's own counters (APERF/MPERF through RDPRU) are refused to a payload (SIGILL), so
 * this file never uses them, nor any other privileged instruction.
 *
 * A payload may only run on the system's cores (logical CPUs 9, 11, 13-15 on FW 13.60, physical
 * cores 0-3); the game cores read 0 = not measured. Each core is measured once (its first
 * logical CPU), best of CPUM_RUNS runs of CPUM_ADDS adds (~6 ms each at 3 GHz), from a worker
 * thread pinned to it, so the caller's thread keeps its own affinity.
 *
 * Cost and safety: about 20 ms of one core per measured core, only for an explicit "Read
 * sensors" (never a timer); one lock; a 2 s cache. When no CPU at all accepts the pin that read has
 * nothing to measure: the last good measurement is the answer if there is one, else "unsupported",
 * and the next read tries again (a running game can refuse every core for as long as it runs). */

#define CPUM_CORES      8
#define CPUM_LOGICAL    16
#define CPUM_RUNS       3
#define CPUM_ADDS       (200000ull * 100ull)
#define CPUM_MIN_MHZ    400
#define CPUM_MAX_MHZ    5000
#define CPUM_CACHE_MS   2000

/* How the measurement reaches the CPU; the helper passes the real calls, the host test a fake.
 * run: runs fn(arg) to completion (the helper: on a fresh thread, joined), 0 when it ran.
 * pin: moves the calling thread to logical CPU `cpu`, 0 on success.
 * core_now: the physical core the calling thread runs on (APIC id >> 1), or -1.
 * chain_ns: runs CPUM_ADDS dependent adds once and returns the nanoseconds it took (0 = failed).
 * now_ms: monotonic. */
typedef struct {
    int (*run)(void *ctx, void (*fn)(void *), void *arg);
    int (*pin)(void *ctx, int cpu);
    int (*core_now)(void *ctx);
    uint64_t (*chain_ns)(void *ctx);
    int64_t (*now_ms)(void *ctx);
    void *ctx;
} cpum_ops_t;

enum {
    CPUM_OK = 0,
    CPUM_UNSUPPORTED = -1, /* no CPU accepted the pin, and nothing was measured earlier */
};

typedef struct {
    int state;                   /* CPUM_OK / CPUM_UNSUPPORTED */
    uint32_t mhz[CPUM_CORES];    /* by physical core; 0 = not measured */
} cpum_clocks_t;

/* Fill *out (zeroed first): from the 2 s cache, else a new measurement under the lock. */
int cpu_measure_read(const cpum_ops_t *ops, cpum_clocks_t *out);

/* MHz for `adds` dependent adds in `ns`, when in CPUM_MIN_MHZ..CPUM_MAX_MHZ, else 0. */
uint32_t cpum_mhz(uint64_t adds, uint64_t ns);

/* The HW_TEMPS lines: cpu_core_measured_mhz (8 values) and cpu_measure_src. snprintf's return. */
int cpu_measure_format(const cpum_clocks_t *c, char *out, size_t cap);

/* Host tests only: forget the cache. */
void cpu_measure_reset(void);

#endif /* PS5UPLOAD_CPU_MEASURE_H */
