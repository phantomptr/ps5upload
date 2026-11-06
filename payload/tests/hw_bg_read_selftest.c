/* Host test for hw_bg_read.h: a read that does not come back costs one thread and holds nobody up,
 * and the next request after it finishes reads afresh. Build: see the root Makefile test-payload. */
#include <pthread.h>
#include <stdio.h>
#include <time.h>

#include "hw_bg_read.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static pthread_mutex_t g_gate = PTHREAD_MUTEX_INITIALIZER;
static int g_value;
static int g_reads;
static int g_spawns;
static int g_refuse_spawn;

static void read_value(void *out) {
    pthread_mutex_lock(&g_gate); /* held by the test: a read that does not come back */
    g_reads++;
    *(int *)out = g_value;
    pthread_mutex_unlock(&g_gate);
}

typedef struct {
    void (*fn)(void *);
    void *arg;
} start_t;

static void *thread_main(void *p) {
    start_t s = *(start_t *)p;
    s.fn(s.arg);
    return NULL;
}

static start_t g_start;

static int spawn(void (*fn)(void *), void *arg) {
    if (g_refuse_spawn) return 1;
    g_spawns++;
    g_start.fn = fn;
    g_start.arg = arg;
    pthread_t th;
    if (pthread_create(&th, NULL, thread_main, &g_start) != 0) return 1;
    pthread_detach(th);
    return 0;
}

static int g_buf;
static hw_bg_read_t g_read = HW_BG_READ_INIT(read_value, g_buf);

int main(void) {
    int got = 0;

    /* A read that answers: the request gets it, and only once. */
    g_value = 7;
    CHECK(hw_bg_read_begin(&g_read, spawn) == 1);
    (void)hw_bg_read_wait(&g_read, 5000);
    CHECK(hw_bg_read_take(&g_read, &got) == 1);
    CHECK(got == 7);
    CHECK(hw_bg_read_take(&g_read, &got) == 0);

    /* A read that does not come back: the wait ends on its budget and nothing is there to take. */
    pthread_mutex_lock(&g_gate);
    g_value = 8;
    got = 0;
    CHECK(hw_bg_read_begin(&g_read, spawn) == 1);
    int spawns = g_spawns;
    int spent = hw_bg_read_wait(&g_read, 50);
    CHECK(spent >= 50);
    CHECK(spent < 1000);
    CHECK(hw_bg_read_take(&g_read, &got) == 0);
    CHECK(got == 0);

    /* The next requests do not add a thread. */
    for (int i = 0; i < 20; i++) CHECK(hw_bg_read_begin(&g_read, spawn) == 0);
    CHECK(g_spawns == spawns);
    CHECK(hw_bg_read_take(&g_read, &got) == 0);

    /* It finishes in the end: what it read can be taken, and the next read is a new one. */
    pthread_mutex_unlock(&g_gate);
    (void)hw_bg_read_wait(&g_read, 5000);
    CHECK(hw_bg_read_take(&g_read, &got) == 1);
    CHECK(got == 8);
    g_value = 9;
    CHECK(hw_bg_read_begin(&g_read, spawn) == 1);
    (void)hw_bg_read_wait(&g_read, 5000);
    CHECK(hw_bg_read_take(&g_read, &got) == 1);
    CHECK(got == 9);

    /* A result nobody took is not handed to a later read that is still going. */
    pthread_mutex_lock(&g_gate);
    CHECK(hw_bg_read_begin(&g_read, spawn) == 1);
    CHECK(hw_bg_read_take(&g_read, &got) == 0);
    pthread_mutex_unlock(&g_gate);
    (void)hw_bg_read_wait(&g_read, 5000);
    (void)hw_bg_read_take(&g_read, &got);

    /* No thread to be had: it says so, and the next request can try again. */
    g_refuse_spawn = 1;
    CHECK(hw_bg_read_begin(&g_read, spawn) == -1);
    CHECK(hw_bg_read_wait(&g_read, 1000) == 0);
    g_refuse_spawn = 0;
    CHECK(hw_bg_read_begin(&g_read, spawn) == 1);
    (void)hw_bg_read_wait(&g_read, 5000);
    CHECK(hw_bg_read_take(&g_read, &got) == 1);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("hw_bg_read ok\n");
    return 0;
}
