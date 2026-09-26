#include <pthread.h>
#include <stdio.h>
#include <time.h>

#include "timed_init.h"

static int failures;

#define CHECK(expr)                                                        \
    do {                                                                   \
        if (!(expr)) {                                                     \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #expr); \
            failures++;                                                    \
        }                                                                  \
    } while (0)

static pthread_mutex_t block_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t block_cond = PTHREAD_COND_INITIALIZER;
static int block_started;
static int block_release;
static int block_calls;

static int blocking_initializer(void) {
    pthread_mutex_lock(&block_mutex);
    block_calls++;
    block_started = 1;
    pthread_cond_broadcast(&block_cond);
    while (!block_release) pthread_cond_wait(&block_cond, &block_mutex);
    pthread_mutex_unlock(&block_mutex);
    return 0;
}

typedef struct waiter_args {
    ps5_timed_init_state_t *state;
    int result;
} waiter_args_t;

static void *waiter(void *arg) {
    waiter_args_t *args = (waiter_args_t *)arg;
    args->result = ps5_timed_init_wait(args->state, blocking_initializer, 1000);
    return NULL;
}

static void short_pause(void) {
    struct timespec ts = {0, 50000000L};
    nanosleep(&ts, NULL);
}

static void test_timeout_reuses_inflight_attempt(void) {
    ps5_timed_init_state_t state = PS5_TIMED_INIT_STATE_INITIALIZER;

    CHECK(ps5_timed_init_wait(&state, blocking_initializer, 20) ==
          PS5_TIMED_INIT_TIMEOUT);

    pthread_mutex_lock(&block_mutex);
    while (!block_started) pthread_cond_wait(&block_cond, &block_mutex);
    CHECK(block_calls == 1);
    pthread_mutex_unlock(&block_mutex);

    waiter_args_t args = {.state = &state, .result = -99};
    pthread_t tid;
    CHECK(pthread_create(&tid, NULL, waiter, &args) == 0);
    short_pause();

    pthread_mutex_lock(&block_mutex);
    CHECK(block_calls == 1);
    block_release = 1;
    pthread_cond_broadcast(&block_cond);
    pthread_mutex_unlock(&block_mutex);

    CHECK(pthread_join(tid, NULL) == 0);
    CHECK(args.result == 0);
    CHECK(ps5_timed_init_wait(&state, blocking_initializer, 20) == 0);
    CHECK(block_calls == 1);
}

static pthread_mutex_t retry_mutex = PTHREAD_MUTEX_INITIALIZER;
static int retry_calls;

static int fail_then_succeed(void) {
    pthread_mutex_lock(&retry_mutex);
    retry_calls++;
    int call = retry_calls;
    pthread_mutex_unlock(&retry_mutex);
    return call == 1 ? -77 : 0;
}

static void test_real_failure_retries_sequentially(void) {
    ps5_timed_init_state_t state = PS5_TIMED_INIT_STATE_INITIALIZER;
    CHECK(ps5_timed_init_wait(&state, fail_then_succeed, 1000) == -77);
    CHECK(ps5_timed_init_wait(&state, fail_then_succeed, 1000) == 0);
    CHECK(ps5_timed_init_wait(&state, fail_then_succeed, 1000) == 0);
    CHECK(retry_calls == 2);
}

int main(void) {
    test_timeout_reuses_inflight_attempt();
    test_real_failure_retries_sequentially();
    if (failures != 0) return 1;
    puts("timed_init_selftest: ALL PASS");
    return 0;
}
