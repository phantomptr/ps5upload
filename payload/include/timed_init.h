#ifndef PS5UPLOAD_TIMED_INIT_H
#define PS5UPLOAD_TIMED_INIT_H

/*
 * Serialize a potentially blocking one-time initializer behind a bounded
 * wait.  A timed-out worker is deliberately left running; later callers wait
 * on that SAME attempt instead of starting a second initializer concurrently.
 * If an attempt returns a real error, the next caller may retry sequentially.
 * A successful result is cached for the lifetime of the state object.
 */

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <time.h>

#define PS5_TIMED_INIT_TIMEOUT (-0xDEAD)

typedef int (*ps5_timed_init_fn)(void);

typedef struct ps5_timed_init_state {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int phase;
    int result;
    ps5_timed_init_fn fn;
} ps5_timed_init_state_t;

enum {
    PS5_TIMED_INIT_IDLE = 0,
    PS5_TIMED_INIT_RUNNING = 1,
    PS5_TIMED_INIT_DONE = 2,
};

#define PS5_TIMED_INIT_STATE_INITIALIZER                                \
    {                                                                   \
        PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER,             \
            PS5_TIMED_INIT_IDLE, -1, NULL                               \
    }

static void *ps5_timed_init_worker(void *arg) {
    ps5_timed_init_state_t *state = (ps5_timed_init_state_t *)arg;
    int result = state->fn();

    pthread_mutex_lock(&state->mutex);
    state->result = result;
    state->phase = PS5_TIMED_INIT_DONE;
    pthread_cond_broadcast(&state->cond);
    pthread_mutex_unlock(&state->mutex);
    return NULL;
}

static int ps5_timed_init_wait(ps5_timed_init_state_t *state,
                               ps5_timed_init_fn fn,
                               uint32_t timeout_ms) {
    if (!state || !fn) return -1;

    struct timespec deadline;
    if (clock_gettime(CLOCK_REALTIME, &deadline) != 0) return -1;
    deadline.tv_sec += (time_t)(timeout_ms / 1000U);
    deadline.tv_nsec += (long)(timeout_ms % 1000U) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec += 1;
        deadline.tv_nsec -= 1000000000L;
    }

    pthread_mutex_lock(&state->mutex);

    /* Successful initialization is permanent and never rerun. */
    if (state->phase == PS5_TIMED_INIT_DONE && state->result == 0) {
        pthread_mutex_unlock(&state->mutex);
        return 0;
    }

    /* Start only when no prior attempt is still executing.  A real failure
     * may be retried later; a timed-out attempt remains RUNNING here. */
    if (state->phase != PS5_TIMED_INIT_RUNNING) {
        pthread_t tid;
        state->fn = fn;
        state->result = -1;
        state->phase = PS5_TIMED_INIT_RUNNING;
        int create_rc = pthread_create(&tid, NULL, ps5_timed_init_worker, state);
        if (create_rc != 0) {
            state->phase = PS5_TIMED_INIT_DONE;
            pthread_mutex_unlock(&state->mutex);
            return -1;
        }
        (void)pthread_detach(tid);
    }

    while (state->phase == PS5_TIMED_INIT_RUNNING) {
        int wait_rc = pthread_cond_timedwait(&state->cond, &state->mutex,
                                             &deadline);
        if (wait_rc == ETIMEDOUT) {
            pthread_mutex_unlock(&state->mutex);
            return PS5_TIMED_INIT_TIMEOUT;
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

#endif /* PS5UPLOAD_TIMED_INIT_H */
