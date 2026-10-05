/* Remote Play pairing state machine. See include/rp_pair.h for the contract.
 *
 * Built for the console (Makefile) and for the host (ava1-ctest), so this
 * file includes nothing Sony-specific and makes every console call through
 * rp_pair_ops_t. */
#include "rp_pair.h"

#include <stdio.h>
#include <string.h>

const char *rp_pair_state_name(int state) {
    switch (state) {
        case RP_PAIR_IDLE: return "idle";
        case RP_PAIR_WAITING: return "waiting";
        case RP_PAIR_PAIRED: return "paired";
        case RP_PAIR_FAILED: return "failed";
        case RP_PAIR_TIMEOUT: return "timeout";
        default: return "unknown";
    }
}

void rp_pair_init(rp_pair_t *p) {
    memset(p, 0, sizeof(*p));
    pthread_mutex_init(&p->mtx, NULL);
    p->state = RP_PAIR_IDLE;
    p->devices_at_start = -1;
}

static void notify(const rp_pair_ops_t *ops, const char *msg, int level) {
    if (ops && ops->notify) ops->notify(ops->ctx, msg, level);
}

/* Callers hold p->mtx. */
static void clear_pin_locked(rp_pair_t *p) {
    p->pin[0] = 0;
    p->deadline_ms = 0;
}

static void fail_locked(rp_pair_t *p, const char *msg) {
    p->state = RP_PAIR_FAILED;
    clear_pin_locked(p);
    snprintf(p->err, sizeof(p->err), "%s", msg);
}

/* Expiry check. Callers hold p->mtx. Returns 1 when the PIN just expired. */
static int expire_locked(rp_pair_t *p, int64_t now_ms) {
    if (p->state != RP_PAIR_WAITING || p->deadline_ms == 0 || now_ms < p->deadline_ms) {
        return 0;
    }
    p->state = RP_PAIR_TIMEOUT;
    clear_pin_locked(p);
    snprintf(p->err, sizeof(p->err), "the PIN expired before a device paired");
    p->invalidate_owed = 1;
    return 1;
}

void rp_pair_settle(rp_pair_t *p, const rp_pair_ops_t *ops) {
    pthread_mutex_lock(&p->mtx);
    int owed = p->invalidate_owed;
    p->invalidate_owed = 0;
    pthread_mutex_unlock(&p->mtx);
    if (owed && ops && ops->invalidate) (void)ops->invalidate(ops->ctx);
}

int rp_pair_request(rp_pair_t *p, const rp_pair_ops_t *ops, int64_t now_ms) {
    /* A live PIN is replaced, not stacked: give it up first (below, once the
     * module is initialised). The console holds one pending PIN; clearing it
     * before asking for another costs nothing, so it is done every time,
     * owed or not. */
    pthread_mutex_lock(&p->mtx);
    p->gen++;
    p->state = RP_PAIR_IDLE;
    clear_pin_locked(p);
    p->err[0] = 0;
    p->invalidate_owed = 0;
    p->last_rc = 0;
    p->last_status = 0;
    p->last_err = 0;
    p->probes = 0;
    uint32_t gen = p->gen;
    pthread_mutex_unlock(&p->mtx);

    char why[128] = "";
    if (ops->prepare && ops->prepare(ops->ctx, why, sizeof(why)) != 0) {
        pthread_mutex_lock(&p->mtx);
        if (p->gen == gen) fail_locked(p, why[0] ? why : "Remote Play is not available");
        pthread_mutex_unlock(&p->mtx);
        return -1;
    }
    /* After prepare, which initialises the module the call goes to. */
    if (ops->invalidate) (void)ops->invalidate(ops->ctx);

    int devices = ops->device_count ? ops->device_count(ops->ctx) : -1;
    uint32_t pin = 0;
    int rc = ops->gen_pin ? ops->gen_pin(ops->ctx, &pin) : -1;

    pthread_mutex_lock(&p->mtx);
    if (p->gen != gen) {
        /* Cancelled while we were inside Sony. The PIN we just made is not
         * wanted: owe its invalidation, report nothing. */
        if (rc == 0) p->invalidate_owed = 1;
        pthread_mutex_unlock(&p->mtx);
        return -1;
    }
    if (rc != 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "sceRemoteplayGeneratePinCode failed: 0x%08X", (unsigned)rc);
        fail_locked(p, msg);
        pthread_mutex_unlock(&p->mtx);
        return -1;
    }
    snprintf(p->pin, sizeof(p->pin), "%08u", (unsigned)(pin % 100000000u));
    p->state = RP_PAIR_WAITING;
    p->deadline_ms = now_ms + RP_PAIR_WAIT_MS;
    p->devices_at_start = devices;
    char msg[96];
    snprintf(msg, sizeof(msg), "[ps5upload] Remote Play PIN: %s", p->pin);
    pthread_mutex_unlock(&p->mtx);

    notify(ops, msg, 0);
    return 0;
}

int rp_pair_poll(rp_pair_t *p, const rp_pair_ops_t *ops, int64_t now_ms) {
    pthread_mutex_lock(&p->mtx);
    int expired = expire_locked(p, now_ms);
    int s = p->state;
    uint32_t gen = p->gen;
    int before = p->devices_at_start;
    pthread_mutex_unlock(&p->mtx);

    if (expired) notify(ops, "[ps5upload] Remote Play PIN expired", 1);
    rp_pair_settle(p, ops);
    if (s != RP_PAIR_WAITING) return s;

    uint32_t status = 0, err = 0;
    int rc = ops->confirm ? ops->confirm(ops->ctx, &status, &err) : -1;
    /* The table only grows when a registration lands, so growth is proof of
     * pairing even if ConfirmDeviceRegist never says so. Only read it when
     * Confirm has not already decided. */
    int grew = 0;
    if (!(rc == 0 && (status == RP_CONFIRM_PAIRED || status == RP_CONFIRM_FAILED ||
                      status == RP_CONFIRM_ABORTED)) &&
        before >= 0 && ops->device_count) {
        int now_devices = ops->device_count(ops->ctx);
        grew = now_devices > before;
    }

    char msg[128] = "";
    int level = 0;
    pthread_mutex_lock(&p->mtx);
    if (p->gen != gen || p->state != RP_PAIR_WAITING) {
        /* A cancel or a new request happened while we were inside Sony:
         * this answer belongs to a PIN nobody is waiting on any more. */
        s = p->state;
        pthread_mutex_unlock(&p->mtx);
        return s;
    }
    p->last_rc = rc;
    p->last_status = status;
    p->last_err = err;
    p->probes++;
    if (rc == 0 && status == RP_CONFIRM_PAIRED) {
        p->state = RP_PAIR_PAIRED;
        clear_pin_locked(p);
        snprintf(msg, sizeof(msg), "[ps5upload] Remote Play: device paired");
    } else if (rc == 0 && (status == RP_CONFIRM_FAILED || status == RP_CONFIRM_ABORTED)) {
        const char *reason = err == RP_ERR_PIN_INVALID ? "the PIN was entered wrong"
                           : err == RP_ERR_ACCOUNT_INVALID ? "the account id was not accepted"
                           : status == RP_CONFIRM_ABORTED ? "the console ended the registration"
                           : "the console refused the registration";
        char e[128];
        snprintf(e, sizeof(e), "pairing failed: %s (status %u, 0x%08X)", reason,
                 (unsigned)status, (unsigned)err);
        fail_locked(p, e);
        /* The console has finished with this PIN; telling it again is harmless
         * and makes sure it is gone. */
        p->invalidate_owed = 1;
        snprintf(msg, sizeof(msg), "[ps5upload] Remote Play pairing failed: %s", reason);
        level = 2;
    } else if (grew) {
        p->state = RP_PAIR_PAIRED;
        clear_pin_locked(p);
        snprintf(msg, sizeof(msg), "[ps5upload] Remote Play: device paired");
    }
    /* Anything else (a Sony error from the probe, or a status that is not
     * final) leaves the PIN waiting: the deadline is what ends it. */
    s = p->state;
    pthread_mutex_unlock(&p->mtx);

    if (msg[0]) notify(ops, msg, level);
    rp_pair_settle(p, ops);
    return s;
}

int rp_pair_cancel(rp_pair_t *p) {
    pthread_mutex_lock(&p->mtx);
    p->gen++;
    if (p->state == RP_PAIR_WAITING) p->invalidate_owed = 1;
    p->state = RP_PAIR_IDLE;
    clear_pin_locked(p);
    p->err[0] = 0;
    p->last_rc = 0;
    p->last_status = 0;
    p->last_err = 0;
    p->probes = 0;
    int owed = p->invalidate_owed;
    pthread_mutex_unlock(&p->mtx);
    return owed;
}

void rp_pair_view(rp_pair_t *p, int64_t now_ms, rp_pair_view_t *out) {
    pthread_mutex_lock(&p->mtx);
    out->state = p->state;
    snprintf(out->pin, sizeof(out->pin), "%s", p->pin);
    snprintf(out->err, sizeof(out->err), "%s", p->err);
    out->seconds_left = 0;
    if (p->state == RP_PAIR_WAITING && p->deadline_ms > now_ms) {
        /* Round up: a PIN with 0.4 s left is still live, and "0 s left"
         * next to "waiting" reads as a contradiction. */
        out->seconds_left = (int)((p->deadline_ms - now_ms + 999) / 1000);
    }
    out->last_rc = p->last_rc;
    out->last_status = p->last_status;
    out->last_err = p->last_err;
    out->probes = p->probes;
    pthread_mutex_unlock(&p->mtx);
}
