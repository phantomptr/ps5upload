#ifndef PS5UPLOAD_HW_BG_READ_H
#define PS5UPLOAD_HW_BG_READ_H

/* A device read that may not come back, kept off the request's thread.
 *
 * Read sensors asks for the clocks (mp1_clock.c, cpu_measure.c). Measured on a Pro with a game
 * running (FW 13.60, 2026-11): that read did not return until the game closed, so the engine waited
 * out its 30 s and one mgmt worker was gone per press. Here the read runs on its own thread, one per
 * read at most, and the request waits a short while for it. A read that is still going costs that
 * one thread and the answer says it is busy; the next request does not start another or wait for
 * it. Once it finishes, the next request reads afresh. Pure logic, host-tested
 * (hw_bg_read_selftest.c). */
#include <pthread.h>
#include <stddef.h>
#include <string.h>
#include <time.h>

/* Fills `out`: the buffer the read was set up with. Runs with no lock held. */
typedef void (*hw_bg_read_fn)(void *out);
/* Starts fn(arg) on a detached thread: 0, or nonzero when no thread could be had. */
typedef int (*hw_bg_spawn_fn)(void (*fn)(void *), void *arg);

typedef struct {
    pthread_mutex_t mu;
    int running; /* a thread is inside `read`, and owns `buf` until it is done */
    int have;    /* `buf` holds a finished read that nobody has taken */
    hw_bg_read_fn read;
    void *buf;
    size_t size;
} hw_bg_read_t;

#define HW_BG_READ_INIT(fn, buffer) {PTHREAD_MUTEX_INITIALIZER, 0, 0, (fn), &(buffer), sizeof(buffer)}

/* How often a waiting request looks. */
#define HW_BG_READ_STEP_MS 10

static inline void hw_bg_read_thread(void *arg) {
    hw_bg_read_t *r = arg;
    r->read(r->buf);
    pthread_mutex_lock(&r->mu);
    r->have = 1;
    r->running = 0;
    pthread_mutex_unlock(&r->mu);
}

/* Starts a read unless one is still going. 1 = started, 0 = one is still going, -1 = no thread. */
static inline int hw_bg_read_begin(hw_bg_read_t *r, hw_bg_spawn_fn spawn) {
    pthread_mutex_lock(&r->mu);
    if (r->running) {
        pthread_mutex_unlock(&r->mu);
        return 0;
    }
    r->running = 1;
    r->have = 0;
    pthread_mutex_unlock(&r->mu);
    if (spawn(hw_bg_read_thread, r) != 0) {
        pthread_mutex_lock(&r->mu);
        r->running = 0;
        pthread_mutex_unlock(&r->mu);
        return -1;
    }
    return 1;
}

/* Waits up to budget_ms for the read to finish: the milliseconds spent. */
static inline int hw_bg_read_wait(hw_bg_read_t *r, int budget_ms) {
    int spent = 0;
    for (;;) {
        pthread_mutex_lock(&r->mu);
        int running = r->running;
        pthread_mutex_unlock(&r->mu);
        if (!running || spent >= budget_ms) return spent;
        struct timespec ts = {0, HW_BG_READ_STEP_MS * 1000000L};
        nanosleep(&ts, NULL);
        spent += HW_BG_READ_STEP_MS;
    }
}

/* Copies a finished read into `out`, once: 1, or 0 when there is none to take. */
static inline int hw_bg_read_take(hw_bg_read_t *r, void *out) {
    pthread_mutex_lock(&r->mu);
    int ok = !r->running && r->have;
    if (ok) {
        memcpy(out, r->buf, r->size);
        r->have = 0;
    }
    pthread_mutex_unlock(&r->mu);
    return ok;
}

#endif
