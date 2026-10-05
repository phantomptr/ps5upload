#ifndef PS5UPLOAD_RP_PAIR_H
#define PS5UPLOAD_RP_PAIR_H

/* Remote Play pairing state: the pure part, with no Sony call of its own.
 *
 * Everything that touches the console goes through rp_pair_ops_t, so the
 * host-built ava1-ctest suite can drive this file with fake Sony functions
 * (engine/crates/ava1-ctest/tests/rp_pair.rs). remoteplay.c supplies the
 * real ones and owns the locking:
 *
 *   - functions marked [sony] call ops; the caller holds sony_api_lock;
 *   - functions marked [no sony] never call ops and need no Sony lock.
 *
 * The struct's own mutex guards its fields. No op is ever called while it
 * is held, so a slow or stuck Sony call never blocks a reader or a cancel.
 *
 * States, as reported to the engine:
 *   idle     nothing pending
 *   waiting  a PIN is live; seconds_left counts down to its expiry
 *   paired   a device registration was confirmed (ConfirmDeviceRegist said
 *            so, or the paired-device table grew while the PIN was live)
 *   failed   the request or the registration failed; err says why
 *   timeout  the PIN expired with no registration
 *
 * A cancel or a new request bumps the generation. A probe that was already
 * inside a Sony call when that happened has its answer dropped, so a late
 * "paired" can never overwrite the idle state a cancel left. */

#include <pthread.h>
#include <stddef.h>
#include <stdint.h>

#define RP_PAIR_IDLE    0
#define RP_PAIR_WAITING 1
#define RP_PAIR_PAIRED  2
#define RP_PAIR_FAILED  3
#define RP_PAIR_TIMEOUT 4

/* How long a PIN stays usable. Our own deadline: Sony reports no expiry, so
 * at the deadline we invalidate the PIN ourselves (NotifyPinCodeError). */
#define RP_PAIR_WAIT_MS (300 * 1000)

/* sceRemoteplayConfirmDeviceRegist's out-status (observed values). */
#define RP_CONFIRM_PAIRED   2u
#define RP_CONFIRM_FAILED   3u
#define RP_CONFIRM_ABORTED  4u

/* The reasons Sony gives with RP_CONFIRM_FAILED that a person can act on. */
#define RP_ERR_PIN_INVALID      0x80FC1047u
#define RP_ERR_ACCOUNT_INVALID  0x80FC1040u

typedef struct rp_pair_ops {
    void *ctx;
    /* Checks and setup before a PIN (module init, a signed-in account).
     * 0 = go; otherwise writes the reason to err. May be NULL. */
    int (*prepare)(void *ctx, char *err, size_t err_cap);
    /* sceRemoteplayGeneratePinCode: 0 and the PIN, or Sony's error. */
    int (*gen_pin)(void *ctx, uint32_t *pin);
    /* sceRemoteplayConfirmDeviceRegist: 0 and status/err, or Sony's error. */
    int (*confirm)(void *ctx, uint32_t *status, uint32_t *err);
    /* Make the live PIN unusable (sceRemoteplayNotifyPinCodeError). */
    int (*invalidate)(void *ctx);
    /* Paired devices in the registration table, or -1 if unreadable. May be NULL. */
    int (*device_count)(void *ctx);
    /* On-console toast. May be NULL. level: 0 info, 1 warn, 2 error. */
    void (*notify)(void *ctx, const char *msg, int level);
} rp_pair_ops_t;

typedef struct rp_pair {
    pthread_mutex_t mtx;
    int state;
    uint32_t gen;            /* bumped by request and cancel */
    char pin[12];            /* 8 digits while a PIN is live, else empty */
    char err[128];
    int64_t deadline_ms;     /* monotonic ms; 0 = none */
    int devices_at_start;    /* device_count when the PIN was made, -1 unknown */
    int invalidate_owed;     /* a PIN was given up but Sony has not been told yet */
    /* The last ConfirmDeviceRegist answer, for diagnostics. */
    int last_rc;
    uint32_t last_status;
    uint32_t last_err;
    uint32_t probes;
} rp_pair_t;

typedef struct rp_pair_view {
    int state;
    char pin[12];
    char err[128];
    int seconds_left;
    int last_rc;
    uint32_t last_status;
    uint32_t last_err;
    uint32_t probes;
} rp_pair_view_t;

/* [no sony] */
void rp_pair_init(rp_pair_t *p);

/* [sony] Make a new PIN, replacing a live one (which is invalidated first).
 * 0 = waiting; -1 = failed (the reason is in the state). */
int rp_pair_request(rp_pair_t *p, const rp_pair_ops_t *ops, int64_t now_ms);

/* [sony] One pairing probe while waiting: expiry, then ConfirmDeviceRegist,
 * then the device table. Also settles an owed invalidation. Returns the
 * state. Does nothing to Sony in idle/paired/failed/timeout. */
int rp_pair_poll(rp_pair_t *p, const rp_pair_ops_t *ops, int64_t now_ms);

/* [no sony] Back to idle at once. Returns 1 when the live PIN still has to
 * be invalidated on the console (rp_pair_settle), else 0. */
int rp_pair_cancel(rp_pair_t *p);

/* [sony] Tell the console about a PIN we gave up (cancel, expiry). Only
 * ops->invalidate is used. */
void rp_pair_settle(rp_pair_t *p, const rp_pair_ops_t *ops);

/* [no sony] A consistent copy for reporting. */
void rp_pair_view(rp_pair_t *p, int64_t now_ms, rp_pair_view_t *out);

const char *rp_pair_state_name(int state);

#endif
