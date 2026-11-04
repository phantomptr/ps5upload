/* Host test for cpu_measure.c with a fake CPU: each physical core is measured once (its first
 * logical CPU that takes the pin), the best run wins, out-of-range clocks read as 0, the 2 s cache
 * holds, a missing worker thread is not fatal, and no CPU taking the pin answers "unsupported" (or
 * what was measured earlier) for that read only: the next one tries again. Layout as measured on FW
 * 13.60: only logical CPUs 9, 11, 13, 14, 15 take the pin, and the APIC id is 15 - cpu. */
#include <stdio.h>
#include <string.h>
#include "../src/cpu_measure.c"

static int fails;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            fails++;                                                   \
        }                                                              \
    } while (0)

typedef struct {
    int64_t now;
    int runs, run_fails, pins, chains;
    int allowed[CPUM_LOGICAL];
    int cur;                         /* logical CPU the "thread" is on */
    uint64_t ns_by_core[CPUM_CORES]; /* chain time for a core; 0 = failed */
    int call_in_run;                 /* chain / pin only happen inside run() */
    int outside_calls;
} fake_t;

static int fake_run(void *ctx, void (*fn)(void *), void *arg) {
    fake_t *f = ctx;
    f->runs++;
    if (f->run_fails) return -1;
    f->call_in_run = 1;
    fn(arg);
    f->call_in_run = 0;
    return 0;
}
static int fake_pin(void *ctx, int cpu) {
    fake_t *f = ctx;
    f->pins++;
    if (!f->call_in_run) f->outside_calls++;
    if (cpu < 0 || cpu >= CPUM_LOGICAL || !f->allowed[cpu]) return -1;
    f->cur = cpu;
    return 0;
}
static int fake_core_now(void *ctx) {
    fake_t *f = ctx;
    return (15 - f->cur) >> 1;
}
static uint64_t fake_chain_ns(void *ctx) {
    fake_t *f = ctx;
    f->chains++;
    if (!f->call_in_run) f->outside_calls++;
    int core = fake_core_now(ctx);
    /* Every other run is 10% slower (an interrupt): the best run must win. */
    uint64_t ns = f->ns_by_core[core];
    return (f->chains % 2) ? ns + ns / 10 : ns;
}
static int64_t fake_now(void *ctx) {
    return ((fake_t *)ctx)->now;
}

static void setup(fake_t *f, cpum_ops_t *o) {
    memset(f, 0, sizeof *f);
    f->now = 1000;
    const int ok[] = {9, 11, 13, 14, 15};
    for (int i = 0; i < 5; i++) f->allowed[ok[i]] = 1;
    /* ns for 20 M adds: 3100 MHz = 6451612 ns, etc. */
    f->ns_by_core[0] = 6451612; /* ~3100 */
    f->ns_by_core[1] = 6309148; /* ~3170 */
    f->ns_by_core[2] = 6802721; /* ~2940 */
    f->ns_by_core[3] = 1000;    /* absurd: out of range */
    o->run = fake_run;
    o->pin = fake_pin;
    o->core_now = fake_core_now;
    o->chain_ns = fake_chain_ns;
    o->now_ms = fake_now;
    o->ctx = f;
    cpu_measure_reset();
}

int main(void) {
    fake_t f;
    cpum_ops_t o;
    cpum_clocks_t c;

    /* mhz: range checks. */
    CHECK(cpum_mhz(CPUM_ADDS, 0) == 0);
    CHECK(cpum_mhz(CPUM_ADDS, 6451612) == 3100);
    CHECK(cpum_mhz(CPUM_ADDS, 1000) == 0);           /* 20 THz */
    CHECK(cpum_mhz(CPUM_ADDS, 1000000000ull) == 0);  /* 20 MHz */

    /* A measurement: cores 0-3 once each (CPU 14 is core 0's second thread), game cores 0. */
    setup(&f, &o);
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(c.state == CPUM_OK);
    CHECK(c.mhz[0] == 3100);
    CHECK(c.mhz[1] == 3170);
    CHECK(c.mhz[2] == 2940);
    CHECK(c.mhz[3] == 0); /* out of range */
    for (int i = 4; i < CPUM_CORES; i++) CHECK(c.mhz[i] == 0);
    CHECK(f.chains == 4 * CPUM_RUNS); /* four cores measured, the sibling skipped */
    CHECK(f.outside_calls == 0);      /* everything on the worker */
    CHECK(f.runs == 1);

    /* The cache: no new measurement within 2 s, one after. */
    f.now += CPUM_CACHE_MS - 1;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(f.runs == 1 && c.mhz[1] == 3170);
    f.now += 2;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(f.runs == 2);

    /* No worker thread: nothing measured, not latched. */
    setup(&f, &o);
    f.run_fails = 1;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(c.mhz[0] == 0);
    f.run_fails = 0;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(c.mhz[0] == 3100);

    /* No CPU takes the pin (a running game can hold them all): unsupported, for this read only. */
    setup(&f, &o);
    int on_console[CPUM_LOGICAL];
    memcpy(on_console, f.allowed, sizeof on_console);
    memset(f.allowed, 0, sizeof f.allowed);
    CHECK(cpu_measure_read(&o, &c) == CPUM_UNSUPPORTED);
    CHECK(c.state == CPUM_UNSUPPORTED && c.mhz[0] == 0);
    int runs = f.runs;
    f.now += 10 * CPUM_CACHE_MS;
    CHECK(cpu_measure_read(&o, &c) == CPUM_UNSUPPORTED);
    CHECK(f.runs == runs + 1); /* it tried again */
    /* The game is gone: the next read measures. */
    memcpy(f.allowed, on_console, sizeof on_console);
    f.now += 10 * CPUM_CACHE_MS;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(c.mhz[0] == 3100);

    /* It worked a minute ago, then a game holds the cores: what was measured stays the answer. */
    memset(f.allowed, 0, sizeof f.allowed);
    f.now += 10 * CPUM_CACHE_MS;
    CHECK(cpu_measure_read(&o, &c) == CPUM_OK);
    CHECK(c.state == CPUM_OK && c.mhz[0] == 3100);

    /* The lines. */
    char buf[256];
    cpum_clocks_t m = {CPUM_OK, {3100, 3170, 2940, 0, 0, 0, 0, 0}};
    int n = cpu_measure_format(&m, buf, sizeof buf);
    CHECK(n > 0 && strcmp(buf, "cpu_measure_src=add_chain\ncpu_core_measured_mhz=3100,3170,2940,0,0,0,0,0\n") == 0);
    cpum_clocks_t u = {CPUM_UNSUPPORTED, {1, 2, 3, 4, 5, 6, 7, 8}};
    n = cpu_measure_format(&u, buf, sizeof buf);
    CHECK(n > 0 && strcmp(buf, "cpu_measure_src=unsupported\ncpu_core_measured_mhz=0,0,0,0,0,0,0,0\n") == 0);

    if (fails) {
        printf("cpu_measure selftest: %d failure(s)\n", fails);
        return 1;
    }
    printf("cpu_measure selftest: ok\n");
    return 0;
}
