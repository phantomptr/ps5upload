/* Fake Sony functions for payload/src/rp_pair.c (Remote Play pairing state).
 *
 * One rp_pair_t and one set of fakes, driven from tests/rp_pair.rs through the
 * rpt_* functions. The fakes record how often each Sony call was made, and the
 * confirm fake can be made to block until released, which is how the tests
 * show a cancel returning while a probe is stuck inside Sony. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "rp_pair.h"

static rp_pair_t g_p;
static int g_init_done;

static int f_prepare_rc;
static int f_gen_rc;
static uint32_t f_pin;
static int f_confirm_rc;
static uint32_t f_status;
static uint32_t f_err;
static int f_devices;

static int n_prepare, n_gen, n_confirm, n_invalidate, n_device_count, n_notify;
static char last_notify[160];

static pthread_mutex_t f_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t f_cv = PTHREAD_COND_INITIALIZER;
static int f_block_confirm;
static int f_in_confirm;

static int fk_prepare(void *ctx, char *err, size_t cap) {
    (void)ctx;
    n_prepare++;
    if (f_prepare_rc != 0) snprintf(err, cap, "fake prepare refused");
    return f_prepare_rc;
}

static int fk_gen_pin(void *ctx, uint32_t *pin) {
    (void)ctx;
    n_gen++;
    if (f_gen_rc == 0) *pin = f_pin;
    return f_gen_rc;
}

static int fk_confirm(void *ctx, uint32_t *status, uint32_t *err) {
    (void)ctx;
    pthread_mutex_lock(&f_mtx);
    n_confirm++;
    f_in_confirm = 1;
    pthread_cond_broadcast(&f_cv);
    while (f_block_confirm) pthread_cond_wait(&f_cv, &f_mtx);
    f_in_confirm = 0;
    *status = f_status;
    *err = f_err;
    int rc = f_confirm_rc;
    pthread_mutex_unlock(&f_mtx);
    return rc;
}

static int fk_invalidate(void *ctx) {
    (void)ctx;
    n_invalidate++;
    return 0;
}

static int fk_device_count(void *ctx) {
    (void)ctx;
    n_device_count++;
    return f_devices;
}

static void fk_notify(void *ctx, const char *msg, int level) {
    (void)ctx;
    (void)level;
    n_notify++;
    snprintf(last_notify, sizeof(last_notify), "%s", msg);
}

static rp_pair_ops_t ops(void) {
    rp_pair_ops_t o;
    memset(&o, 0, sizeof(o));
    o.prepare = fk_prepare;
    o.gen_pin = fk_gen_pin;
    o.confirm = fk_confirm;
    o.invalidate = fk_invalidate;
    o.device_count = fk_device_count;
    o.notify = fk_notify;
    return o;
}

void rpt_reset(void) {
    /* Nothing else touches g_p between tests, so a fresh init is safe. */
    if (g_init_done) pthread_mutex_destroy(&g_p.mtx);
    rp_pair_init(&g_p);
    g_init_done = 1;
    f_prepare_rc = 0;
    f_gen_rc = 0;
    f_pin = 12345678u;
    f_confirm_rc = 0;
    f_status = 0;
    f_err = 0;
    f_devices = 0;
    n_prepare = n_gen = n_confirm = n_invalidate = n_device_count = n_notify = 0;
    last_notify[0] = 0;
    f_block_confirm = 0;
    f_in_confirm = 0;
}

void rpt_set_prepare(int rc) { f_prepare_rc = rc; }
void rpt_set_gen(int rc, uint32_t pin) { f_gen_rc = rc; f_pin = pin; }
void rpt_set_devices(int n) { f_devices = n; }

void rpt_set_confirm(int rc, uint32_t status, uint32_t err) {
    pthread_mutex_lock(&f_mtx);
    f_confirm_rc = rc;
    f_status = status;
    f_err = err;
    pthread_mutex_unlock(&f_mtx);
}

void rpt_block_confirm(int on) {
    pthread_mutex_lock(&f_mtx);
    f_block_confirm = on;
    pthread_cond_broadcast(&f_cv);
    pthread_mutex_unlock(&f_mtx);
}

/* Wait (bounded) until a probe is inside the confirm fake. 1 = it is. */
int rpt_wait_in_confirm(int ms) {
    pthread_mutex_lock(&f_mtx);
    for (int i = 0; i < ms && !f_in_confirm; i++) {
        pthread_mutex_unlock(&f_mtx);
        struct timespec ts = {0, 1000000};
        nanosleep(&ts, NULL);
        pthread_mutex_lock(&f_mtx);
    }
    int in = f_in_confirm;
    pthread_mutex_unlock(&f_mtx);
    return in;
}

int rpt_request(int64_t now_ms) {
    rp_pair_ops_t o = ops();
    return rp_pair_request(&g_p, &o, now_ms);
}

int rpt_poll(int64_t now_ms) {
    rp_pair_ops_t o = ops();
    return rp_pair_poll(&g_p, &o, now_ms);
}

int rpt_cancel(void) { return rp_pair_cancel(&g_p); }

void rpt_settle(void) {
    rp_pair_ops_t o = ops();
    rp_pair_settle(&g_p, &o);
}

/* The view, flattened for Rust: state, seconds_left, probes, last_status. */
void rpt_view(int64_t now_ms, int *state, int *seconds_left, char *pin, size_t pin_cap,
              char *err, size_t err_cap, uint32_t *probes, uint32_t *last_status) {
    rp_pair_view_t v;
    rp_pair_view(&g_p, now_ms, &v);
    *state = v.state;
    *seconds_left = v.seconds_left;
    snprintf(pin, pin_cap, "%s", v.pin);
    snprintf(err, err_cap, "%s", v.err);
    *probes = v.probes;
    *last_status = v.last_status;
}

/* prepare, gen, confirm, invalidate, device_count, notify */
void rpt_counts(int out[6]) {
    pthread_mutex_lock(&f_mtx);
    out[0] = n_prepare;
    out[1] = n_gen;
    out[2] = n_confirm;
    out[3] = n_invalidate;
    out[4] = n_device_count;
    out[5] = n_notify;
    pthread_mutex_unlock(&f_mtx);
}

const char *rpt_last_notify(void) { return last_notify; }

const char *rpt_state_name(int s) { return rp_pair_state_name(s); }
