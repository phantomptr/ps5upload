/* Host selftest for the installer daemon's listener policy (accept back-off,
 * rebind window, connection cap), its stop rule, and its monotonic bounded
 * Sony init wait (installer/inst_timed_init.h). */
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <time.h>

#include "inst_timed_init.h"
#include "jobs.h"
#include "listen_guard.h"

static int failures;

#define CHECK(expr)                                                        \
    do {                                                                   \
        if (!(expr)) {                                                     \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #expr); \
            failures++;                                                    \
        }                                                                  \
    } while (0)

static void test_accept_classify(void) {
    CHECK(inst_accept_classify(EINTR) == INST_ACCEPT_RETRY);
    CHECK(inst_accept_classify(ECONNABORTED) == INST_ACCEPT_RETRY);
    CHECK(inst_accept_classify(EAGAIN) == INST_ACCEPT_RETRY);
    CHECK(inst_accept_classify(EMFILE) == INST_ACCEPT_BUSY);
    CHECK(inst_accept_classify(ENFILE) == INST_ACCEPT_BUSY);
    CHECK(inst_accept_classify(ENOBUFS) == INST_ACCEPT_BUSY);
    CHECK(inst_accept_classify(ENOMEM) == INST_ACCEPT_BUSY);
    /* Unknown errnos (Sony's 163 once killed the helper's loops) and a dead
     * listener back off and count toward a rebuild; nothing ends the loop. */
    CHECK(inst_accept_classify(163) == INST_ACCEPT_BROKEN);
    CHECK(inst_accept_classify(EBADF) == INST_ACCEPT_BROKEN);
    CHECK(inst_accept_classify(EINVAL) == INST_ACCEPT_BROKEN);
}

static void test_listen_broken_window(void) {
    CHECK(!inst_listen_broken(0, 1000000));                 /* not failing */
    CHECK(!inst_listen_broken(1000, 1000 + INST_LISTEN_BROKEN_MS - 1));
    CHECK(inst_listen_broken(1000, 1000 + INST_LISTEN_BROKEN_MS));
    CHECK(!inst_listen_broken(5000, 4000));                 /* clock never goes back */
    CHECK(inst_mono_ms() != 0);
}

static void test_slot_cap(void) {
    volatile int n = 0;
    for (int i = 0; i < 16; i++) CHECK(inst_slot_take(&n, 16) == 1);
    CHECK(inst_slot_take(&n, 16) == 0);
    CHECK(n == 16);
    inst_slot_release(&n);
    CHECK(inst_slot_take(&n, 16) == 1);
    CHECK(n == 16);
}

static void test_stop_rule(void) {
    /* an install call in flight always refuses */
    CHECK(inst_stop_allowed(1, INST_JOB_NONE, 1) == 0);
    CHECK(inst_stop_allowed(1, INST_JOB_DONE, 1) == 0);
    /* a loopback job Sony is still reading refuses; a fully-read one does not */
    CHECK(inst_stop_allowed(0, INST_JOB_SERVING, 0) == 0);
    CHECK(inst_stop_allowed(0, INST_JOB_SERVING, 1) == 1);
    /* idle, accepted url job, finished or failed: stop */
    CHECK(inst_stop_allowed(0, INST_JOB_NONE, 1) == 1);
    CHECK(inst_stop_allowed(0, INST_JOB_ACCEPTED, 0) == 1);
    CHECK(inst_stop_allowed(0, INST_JOB_DONE, 0) == 1);
    CHECK(inst_stop_allowed(0, INST_JOB_FAILED, 0) == 1);
}

/* ── inst_timed_init ── */

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
    inst_timed_init_state_t *state;
    int result;
} waiter_args_t;

static void *waiter(void *arg) {
    waiter_args_t *args = (waiter_args_t *)arg;
    args->result = inst_timed_init_wait(args->state, blocking_initializer, 1000);
    return NULL;
}

static void test_timeout_reuses_inflight_attempt(void) {
    inst_timed_init_state_t state = INST_TIMED_INIT_STATE_INITIALIZER;

    uint64_t t0 = inst_mono_ms();
    CHECK(inst_timed_init_wait(&state, blocking_initializer, 20) ==
          INST_TIMED_INIT_TIMEOUT);
    uint64_t took = inst_mono_ms() - t0;
    CHECK(took >= 15 && took < 1000);   /* the bound holds, on the monotonic clock */

    pthread_mutex_lock(&block_mutex);
    while (!block_started) pthread_cond_wait(&block_cond, &block_mutex);
    CHECK(block_calls == 1);
    pthread_mutex_unlock(&block_mutex);

    waiter_args_t args = {.state = &state, .result = -99};
    pthread_t tid;
    CHECK(pthread_create(&tid, NULL, waiter, &args) == 0);
    inst_nap_ms(50);

    pthread_mutex_lock(&block_mutex);
    CHECK(block_calls == 1);
    block_release = 1;
    pthread_cond_broadcast(&block_cond);
    pthread_mutex_unlock(&block_mutex);

    CHECK(pthread_join(tid, NULL) == 0);
    CHECK(args.result == 0);
    CHECK(inst_timed_init_wait(&state, blocking_initializer, 20) == 0);
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
    inst_timed_init_state_t state = INST_TIMED_INIT_STATE_INITIALIZER;
    CHECK(inst_timed_init_wait(&state, fail_then_succeed, 1000) == -77);
    CHECK(inst_timed_init_wait(&state, fail_then_succeed, 1000) == 0);
    CHECK(inst_timed_init_wait(&state, fail_then_succeed, 1000) == 0);
    CHECK(retry_calls == 2);
}

int main(void) {
    test_accept_classify();
    test_listen_broken_window();
    test_slot_cap();
    test_stop_rule();
    test_timeout_reuses_inflight_attempt();
    test_real_failure_retries_sequentially();
    if (failures != 0) return 1;
    puts("installer_guard_selftest: ALL PASS");
    return 0;
}
