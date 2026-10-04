/* The AVA1 half of a payload exit: see include/ava1_stop.h. */
#include "ava1_stop.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_server.h"
#include "sony_api_lock.h"

static long long mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

int ava1_exit_decide(long long elapsed_ms, int sony_busy, long long base_ms, long long ceiling_ms) {
    if (elapsed_ms < base_ms) return AVA1_EXIT_WAIT;
    if (!sony_busy) return AVA1_EXIT_OK;
    return elapsed_ms >= ceiling_ms ? AVA1_EXIT_FORCED : AVA1_EXIT_WAIT;
}

int ava1_exit_test_fail_create; /* tests: pthread_create "fails" */
static volatile int g_flush_done;

static void *flush_main(void *arg) {
    (void)arg;
    ava1_data_flush_for_exit();
    __atomic_store_n(&g_flush_done, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

int ava1_exit_flush(int max_ms) {
    pthread_t t;
    pthread_attr_t attr;
    long long end = mono_ms() + max_ms;
    __atomic_store_n(&g_flush_done, 0, __ATOMIC_SEQ_CST);
    pthread_attr_init(&attr);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (ava1_exit_test_fail_create || pthread_create(&t, &attr, flush_main, NULL) != 0) {
        /* No thread: the flush would have to run here, unbounded, and thread creation fails in exactly the
         * wedged states this exit exists for. Skip it; the journals were fsynced per append. */
        pthread_attr_destroy(&attr);
        return -1;
    }
    pthread_attr_destroy(&attr);
    while (!__atomic_load_n(&g_flush_done, __ATOMIC_SEQ_CST)) {
        if (mono_ms() >= end) return -1;
        usleep(5000);
    }
    return 0;
}

int ava1_payload_stop(int conn_wait_ms, int sony_wait_ms) {
    int rc = 0;
    long long end;

    ava1_server_stop();
    end = mono_ms() + conn_wait_ms;
    while (ava1_server_conns() > 0 && mono_ms() < end) usleep(10000);
    if (ava1_server_conns() > 0) rc |= AVA1_STOP_CONNS_LEFT;
    /* The detached RPC workers outlive their session: a handler still running keeps running into
     * the exit. The sessions were told to end above; give the workers the same bounded wait, then
     * report what is left (the Sony wait below still covers one inside a Sony call). */
    end = mono_ms() + conn_wait_ms;
    while (ava1_server_rpc_inflight() > 0 && mono_ms() < end) usleep(10000);
    if (ava1_server_rpc_inflight() > 0) {
        fprintf(stderr, "[ava1] stop: %d RPC worker(s) still running after %d ms\n", ava1_server_rpc_inflight(),
                conn_wait_ms);
        rc |= AVA1_STOP_RPC_LEFT;
    }

    /* No new SESSION can start a call now (accept is closed and the sessions were told to end), but
     * the legacy ports stay open until main.c closes them after this returns, so this check is a
     * point in time, not a barrier. Taking the lock proves the last Sony call returned.
     *
     * A call that outlives `sony_wait_ms` is reported (AVA1_STOP_SONY_BUSY) but NOT abandoned: the
     * caller must not return, and the process must not exit, while a worker is inside a Sony call
     * (a cut call can wedge the console). So keep polling until the lock frees. The only other way
     * out is the exit watchdog main.c armed (runtime_arm_shutdown_watchdog, 8 s), which ends the
     * process itself. */
    end = mono_ms() + sony_wait_ms;
    for (;;) {
        if (pthread_mutex_trylock(&sony_api_lock) == 0) {
            pthread_mutex_unlock(&sony_api_lock);
            break;
        }
        if (mono_ms() >= end) rc |= AVA1_STOP_SONY_BUSY;
        usleep(10000);
    }

    ava1_data_stop();
    return rc;
}

typedef struct {
    int delay_ms;
    void (*fire)(void *);
    void *arg;
} defer_t;

static void *defer_main(void *p) {
    defer_t d = *(defer_t *)p;
    free(p);
    usleep((useconds_t)d.delay_ms * 1000u);
    d.fire(d.arg);
    return NULL;
}

int ava1_shutdown_defer(int delay_ms, void (*fire)(void *), void *arg) {
    pthread_t t;
    pthread_attr_t a;
    int rc;
    defer_t *d = malloc(sizeof *d);
    if (!d || !fire) {
        free(d);
        return -1;
    }
    d->delay_ms = delay_ms;
    d->fire = fire;
    d->arg = arg;
    if (pthread_attr_init(&a) != 0) {
        free(d);
        return -1;
    }
    (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    rc = pthread_create(&t, &a, defer_main, d);
    pthread_attr_destroy(&a);
    if (rc != 0) {
        free(d);
        return -1;
    }
    return 0;
}
