#ifndef PS5UPLOAD_HW_TEMPS_FLIGHT_H
#define PS5UPLOAD_HW_TEMPS_FLIGHT_H

/* The basic sensors' read: one reader at a time, and nobody waits for it.
 *
 * HW_TEMPS has unattended callers (the Dashboard every 5 s, the console page every 10 s, the Play
 * tab). The Sony getters behind it run inline on the caller's thread and can stall (a report from
 * 2026-11: "when I run a game temp won't load"), so they must not run under a lock others queue on:
 * every later poll would hang behind the first, until the engine's 30 s timeout called the helper
 * unresponsive. Here the mutex only guards this state. The first caller that finds nothing fresh
 * becomes the reader and makes the calls with no lock held; a caller arriving meanwhile is answered at
 * once with the last values, marked stale. Pure logic, host-tested (hw_temps_flight_selftest.c). */
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* A finished read answers for this long, so a double-click coalesces into one round of calls. */
#define HW_TEMPS_CACHE_MS 1000
/* A read this long is worth a log line. */
#define HW_TEMPS_SLOW_MS 500

/* What a getter said: "0x%08x" its return code, "fault" (recovered by the guard), "missing" (no
 * such symbol on this firmware), "range" (returned 0 with a value outside the physical bounds), or
 * "none" (no read has finished yet). */
#define HW_TEMPS_RC_MAX 16

typedef struct {
    int cpu_temp, soc_temp, m2_temp;
    long cpu_freq_mhz;
    char cpu_temp_rc[HW_TEMPS_RC_MAX];
    char soc_temp_rc[HW_TEMPS_RC_MAX];
    /* How long the last finished read took; on a stale answer, how long the one in flight has run. */
    uint32_t read_ms;
} hw_temps_sample_t;

typedef struct {
    pthread_mutex_t mu;
    int have;                /* `last` holds a finished read */
    int reading;             /* a thread is inside the getters */
    int stall_said;          /* this flight's stall was reported */
    int64_t last_ms;         /* monotonic: when `last` finished */
    int64_t started_ms;      /* monotonic: when the read in flight started */
    const char *in_call;     /* the getter the reader is in (a string literal), for the stall line */
    hw_temps_sample_t last;
} hw_temps_flight_t;

#define HW_TEMPS_FLIGHT_INIT {PTHREAD_MUTEX_INITIALIZER, 0, 0, 0, 0, 0, NULL, {0, 0, 0, 0, "", "", 0}}

enum {
    HW_TEMPS_READ_NOW = 0, /* the caller is the reader: make the calls, then hw_temps_flight_end */
    HW_TEMPS_FRESH = 1,    /* *out is a read finished within HW_TEMPS_CACHE_MS */
    HW_TEMPS_STALE = 2,    /* another thread is reading; *out is the last read it had, if any */
};

static inline void hw_temps_sample_blank(hw_temps_sample_t *s) {
    memset(s, 0, sizeof *s);
    memcpy(s->cpu_temp_rc, "none", 5);
    memcpy(s->soc_temp_rc, "none", 5);
}

static inline int hw_temps_flight_begin(hw_temps_flight_t *f, int64_t now_ms, hw_temps_sample_t *out) {
    int rc;
    pthread_mutex_lock(&f->mu);
    if (f->have && now_ms >= f->last_ms && now_ms - f->last_ms < HW_TEMPS_CACHE_MS) {
        *out = f->last;
        rc = HW_TEMPS_FRESH;
    } else if (f->reading) {
        if (f->have) *out = f->last;
        else hw_temps_sample_blank(out);
        out->read_ms = now_ms > f->started_ms ? (uint32_t)(now_ms - f->started_ms) : 0;
        rc = HW_TEMPS_STALE;
    } else {
        f->reading = 1;
        f->stall_said = 0;
        f->started_ms = now_ms;
        hw_temps_sample_blank(out);
        rc = HW_TEMPS_READ_NOW;
    }
    pthread_mutex_unlock(&f->mu);
    return rc;
}

/* The reader's next call, for the stall line. No lock: a pointer to a literal, read for a log. */
static inline void hw_temps_flight_in_call(hw_temps_flight_t *f, const char *label) {
    __atomic_store_n(&f->in_call, label, __ATOMIC_RELAXED);
}

/* The reader is done: *s becomes the last read, and gets how long it took. */
static inline void hw_temps_flight_end(hw_temps_flight_t *f, int64_t now_ms, hw_temps_sample_t *s) {
    pthread_mutex_lock(&f->mu);
    s->read_ms = now_ms > f->started_ms ? (uint32_t)(now_ms - f->started_ms) : 0;
    f->last = *s;
    f->last_ms = now_ms;
    f->have = 1;
    f->reading = 0;
    pthread_mutex_unlock(&f->mu);
}

/* For a caller answered stale: the getter a read has been in for HW_TEMPS_SLOW_MS or more, once per
 * read (a getter stuck for good must not write a line per poll), else NULL. */
static inline const char *hw_temps_flight_stall_once(hw_temps_flight_t *f, int64_t now_ms) {
    const char *who = NULL;
    pthread_mutex_lock(&f->mu);
    if (f->reading && !f->stall_said && now_ms >= f->started_ms && now_ms - f->started_ms >= HW_TEMPS_SLOW_MS) {
        f->stall_said = 1;
        who = __atomic_load_n(&f->in_call, __ATOMIC_RELAXED);
        if (!who) who = "(starting)";
    }
    pthread_mutex_unlock(&f->mu);
    return who;
}

#endif /* PS5UPLOAD_HW_TEMPS_FLIGHT_H */
