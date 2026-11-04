/* Host test for the basic sensors' single read (hw_temps_flight.h): a stalled getter never blocks
 * another caller, the 1 s cache holds, and a stale answer says what it is. Build: see the root
 * Makefile test-payload. */
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "hw_temps_flight.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static void sample_of(hw_temps_sample_t *s, int cpu) {
    s->cpu_temp = cpu;
    s->soc_temp = cpu + 1;
    s->m2_temp = 0;
    s->cpu_freq_mhz = 800;
    snprintf(s->cpu_temp_rc, sizeof s->cpu_temp_rc, "0x00000000");
    snprintf(s->soc_temp_rc, sizeof s->soc_temp_rc, "0x80020016");
}

static void states(void) {
    hw_temps_flight_t f = HW_TEMPS_FLIGHT_INIT;
    hw_temps_sample_t a, b;

    /* The first caller reads. */
    CHECK(hw_temps_flight_begin(&f, 1000, &a) == HW_TEMPS_READ_NOW);
    CHECK(strcmp(a.cpu_temp_rc, "none") == 0);

    /* Another, meanwhile, is answered at once, with nothing to show yet and how long it has run. */
    CHECK(hw_temps_flight_begin(&f, 1400, &b) == HW_TEMPS_STALE);
    CHECK(b.cpu_temp == 0 && strcmp(b.cpu_temp_rc, "none") == 0);
    CHECK(b.read_ms == 400);

    /* Finished: the time it took, the values, and they answer for a second. */
    sample_of(&a, 50);
    hw_temps_flight_end(&f, 1600, &a);
    CHECK(a.read_ms == 600);
    CHECK(hw_temps_flight_begin(&f, 1600 + HW_TEMPS_CACHE_MS - 1, &b) == HW_TEMPS_FRESH);
    CHECK(b.cpu_temp == 50 && b.soc_temp == 51 && b.read_ms == 600);
    CHECK(strcmp(b.soc_temp_rc, "0x80020016") == 0);

    /* Past the second the next caller reads, and one arriving meanwhile gets the last values. */
    CHECK(hw_temps_flight_begin(&f, 1600 + HW_TEMPS_CACHE_MS, &a) == HW_TEMPS_READ_NOW);
    CHECK(hw_temps_flight_begin(&f, 1600 + HW_TEMPS_CACHE_MS + 30000, &b) == HW_TEMPS_STALE);
    CHECK(b.cpu_temp == 50 && b.read_ms == 30000);
    sample_of(&a, 60);
    hw_temps_flight_end(&f, 40000, &a);

    /* The monotonic clock never runs back, but a value from the future must not read as fresh. */
    CHECK(hw_temps_flight_begin(&f, 39999, &b) == HW_TEMPS_READ_NOW);
    hw_temps_flight_end(&f, 39999, &b);
}

static void stall_lines(void) {
    hw_temps_flight_t f = HW_TEMPS_FLIGHT_INIT;
    hw_temps_sample_t a;
    const char *who;

    CHECK(hw_temps_flight_stall_once(&f, 5000) == NULL); /* nothing in flight */
    CHECK(hw_temps_flight_begin(&f, 1000, &a) == HW_TEMPS_READ_NOW);
    CHECK(hw_temps_flight_stall_once(&f, 1000 + HW_TEMPS_SLOW_MS - 1) == NULL);
    who = hw_temps_flight_stall_once(&f, 1000 + HW_TEMPS_SLOW_MS);
    CHECK(who && strcmp(who, "(starting)") == 0);
    CHECK(hw_temps_flight_stall_once(&f, 90000) == NULL); /* once per read, however long it lasts */
    hw_temps_flight_end(&f, 90000, &a);

    CHECK(hw_temps_flight_begin(&f, 100000, &a) == HW_TEMPS_READ_NOW);
    hw_temps_flight_in_call(&f, "sceKernelGetCpuTemperature");
    who = hw_temps_flight_stall_once(&f, 101000);
    CHECK(who && strcmp(who, "sceKernelGetCpuTemperature") == 0);
    hw_temps_flight_end(&f, 101000, &a);
}

/* The point of it all, with real threads: a getter that does not return holds nobody up. */
static hw_temps_flight_t g_f = HW_TEMPS_FLIGHT_INIT;
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_cv = PTHREAD_COND_INITIALIZER;
static int g_in_getter, g_release;

static void *stuck_reader(void *arg) {
    hw_temps_sample_t s;
    (void)arg;
    CHECK(hw_temps_flight_begin(&g_f, 1000, &s) == HW_TEMPS_READ_NOW);
    pthread_mutex_lock(&g_mu);
    g_in_getter = 1;
    pthread_cond_broadcast(&g_cv);
    while (!g_release) pthread_cond_wait(&g_cv, &g_mu);
    pthread_mutex_unlock(&g_mu);
    sample_of(&s, 70);
    hw_temps_flight_end(&g_f, 9000, &s);
    return NULL;
}

static void stalled_getter_blocks_nobody(void) {
    pthread_t t;
    hw_temps_sample_t s;
    CHECK(pthread_create(&t, NULL, stuck_reader, NULL) == 0);
    pthread_mutex_lock(&g_mu);
    while (!g_in_getter) pthread_cond_wait(&g_cv, &g_mu);
    pthread_mutex_unlock(&g_mu);
    /* The getter is stuck. If the state lock were held across it, this line would hang forever (and
     * the run-limited wrapper would fail the test by name). */
    for (int i = 0; i < 100; i++) CHECK(hw_temps_flight_begin(&g_f, 2000 + i, &s) == HW_TEMPS_STALE);
    pthread_mutex_lock(&g_mu);
    g_release = 1;
    pthread_cond_broadcast(&g_cv);
    pthread_mutex_unlock(&g_mu);
    pthread_join(t, NULL);
    CHECK(hw_temps_flight_begin(&g_f, 9000, &s) == HW_TEMPS_FRESH);
    CHECK(s.cpu_temp == 70 && s.read_ms == 8000);
}

int main(void) {
    states();
    stall_lines();
    stalled_getter_blocks_nobody();
    if (failures) {
        fprintf(stderr, "hw_temps_flight_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("hw_temps_flight_selftest: ok\n");
    return 0;
}
