#ifndef PS5UPLOAD_INSTALLER_TIMED_INIT_H
#define PS5UPLOAD_INSTALLER_TIMED_INIT_H

/*
 * Serialize a potentially blocking one-time initializer behind a bounded
 * wait. A timed-out worker is deliberately left running; later callers wait
 * on that SAME attempt instead of starting a second initializer concurrently.
 * If an attempt returns a real error, the next caller may retry sequentially.
 * A successful result is cached for the lifetime of the state object.
 *
 * The installer's own copy of ../include/timed_init.h (the daemon is its only
 * user), differing in two ways:
 *  - the bound is measured on CLOCK_MONOTONIC. The shared header builds an
 *    absolute CLOCK_REALTIME deadline, so a wall-clock step (NTP after boot,
 *    the clock set by a payload) during sceAppInstUtilInitialize either cut
 *    the wait short or stretched it by the size of the jump;
 *  - its worker thread is named like every other installer thread.
 */

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <time.h>

#include "thread_name.h"

#define INST_TIMED_INIT_TIMEOUT (-0xDEAD)

typedef int (*inst_timed_init_fn)(void);

typedef struct inst_timed_init_state {
    pthread_mutex_t mutex;
    int cond_ready;          /* `cond` set up on the monotonic clock */
    pthread_cond_t cond;
    int phase;
    int result;
    inst_timed_init_fn fn;
} inst_timed_init_state_t;

enum {
    INST_TIMED_INIT_IDLE = 0,
    INST_TIMED_INIT_RUNNING = 1,
    INST_TIMED_INIT_DONE = 2,
};

/* `cond` is initialised on first use, under `mutex` (a condvar bound to a
 * clock cannot be statically initialised), so the initializer leaves it
 * zeroed. */
#define INST_TIMED_INIT_STATE_INITIALIZER                                \
    { .mutex = PTHREAD_MUTEX_INITIALIZER, .cond_ready = 0,                \
      .phase = INST_TIMED_INIT_IDLE, .result = -1, .fn = NULL }

/* Caller holds `mutex`. */
static void inst_timed_init_cond_setup(inst_timed_init_state_t *state) {
    if (state->cond_ready) return;
    state->cond_ready = 1;
#if defined(__APPLE__)
    /* Host selftests: macOS has no pthread_condattr_setclock; the wait below
     * uses the relative (monotonic) timed wait there instead. */
    pthread_cond_init(&state->cond, NULL);
#else
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&state->cond, &attr);
    pthread_condattr_destroy(&attr);
#endif
}

static void *inst_timed_init_worker(void *arg) {
    inst_thread_name(INST_THREAD_INIT);
    inst_timed_init_state_t *state = (inst_timed_init_state_t *)arg;
    int result = state->fn();

    pthread_mutex_lock(&state->mutex);
    state->result = result;
    state->phase = INST_TIMED_INIT_DONE;
    pthread_cond_broadcast(&state->cond);
    pthread_mutex_unlock(&state->mutex);
    return NULL;
}

static uint64_t inst_timed_init_now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

/* One bounded wait on `cond` until the monotonic `deadline_ns`. 0, ETIMEDOUT,
 * or another error. Caller holds `mutex`. */
static int inst_timed_init_cond_wait(inst_timed_init_state_t *state,
                                     uint64_t deadline_ns) {
    uint64_t now = inst_timed_init_now_ns();
    if (now >= deadline_ns) return ETIMEDOUT;
#if defined(__APPLE__)
    uint64_t left = deadline_ns - now;
    struct timespec rel = { (time_t)(left / 1000000000ull),
                            (long)(left % 1000000000ull) };
    return pthread_cond_timedwait_relative_np(&state->cond, &state->mutex, &rel);
#else
    struct timespec abs = { (time_t)(deadline_ns / 1000000000ull),
                            (long)(deadline_ns % 1000000000ull) };
    return pthread_cond_timedwait(&state->cond, &state->mutex, &abs);
#endif
}

static int inst_timed_init_wait(inst_timed_init_state_t *state,
                                inst_timed_init_fn fn,
                                uint32_t timeout_ms) {
    if (!state || !fn) return -1;

    uint64_t deadline_ns = inst_timed_init_now_ns() + (uint64_t)timeout_ms * 1000000ull;

    pthread_mutex_lock(&state->mutex);
    inst_timed_init_cond_setup(state);

    /* Successful initialization is permanent and never rerun. */
    if (state->phase == INST_TIMED_INIT_DONE && state->result == 0) {
        pthread_mutex_unlock(&state->mutex);
        return 0;
    }

    /* Start only when no prior attempt is still executing. A real failure
     * may be retried later; a timed-out attempt remains RUNNING here. */
    if (state->phase != INST_TIMED_INIT_RUNNING) {
        pthread_t tid;
        state->fn = fn;
        state->result = -1;
        state->phase = INST_TIMED_INIT_RUNNING;
        int create_rc = pthread_create(&tid, NULL, inst_timed_init_worker, state);
        if (create_rc != 0) {
            state->phase = INST_TIMED_INIT_DONE;
            pthread_mutex_unlock(&state->mutex);
            return -1;
        }
        (void)pthread_detach(tid);
    }

    while (state->phase == INST_TIMED_INIT_RUNNING) {
        int wait_rc = inst_timed_init_cond_wait(state, deadline_ns);
        if (wait_rc == ETIMEDOUT) {
            /* A wakeup can race the deadline: the result wins if it is in. */
            if (state->phase != INST_TIMED_INIT_RUNNING) break;
            pthread_mutex_unlock(&state->mutex);
            return INST_TIMED_INIT_TIMEOUT;
        }
        if (wait_rc != 0) {
            pthread_mutex_unlock(&state->mutex);
            return -1;
        }
    }

    int result = state->result;
    pthread_mutex_unlock(&state->mutex);
    return result;
}

#endif /* PS5UPLOAD_INSTALLER_TIMED_INIT_H */
