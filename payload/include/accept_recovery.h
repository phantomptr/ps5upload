#ifndef PS5UPLOAD2_ACCEPT_RECOVERY_H
#define PS5UPLOAD2_ACCEPT_RECOVERY_H

#include <errno.h>

/*
 * What an accept loop does after accept() fails.
 *
 * There is deliberately no "give up" action. Both accept loops used to break
 * on any errno outside a short allow-list, which left the helper running but
 * serving nothing. On hardware they hit errno 163 — a Sony addition beyond
 * FreeBSD's ELAST, which no allow-list anticipates — and the user saw the
 * connection drop a few seconds after launching the helper. The only way out
 * of an accept loop is a shutdown request, checked by the caller.
 *
 * Pure so it can be tested on the host (tests/accept_recovery_selftest.c).
 */
typedef enum {
    ACCEPT_RETRY_NOW = 0,   /* interrupted / aborted handshake: just loop */
    ACCEPT_BACKOFF = 1,     /* sleep briefly, then accept again */
    ACCEPT_REBUILD = 2,     /* listener presumed dead: close and re-create it */
} accept_action_t;

/* Consecutive failures after which the listening socket is rebuilt. At the
 * callers' 100 ms back-off, about 2 s. */
#define ACCEPT_REBUILD_AFTER 20

/* `consecutive` counts this failure (1 for the first in a row). */
static inline accept_action_t accept_error_action(int err, int consecutive) {
    if (err == EINTR || err == ECONNABORTED) return ACCEPT_RETRY_NOW;
    if (consecutive >= ACCEPT_REBUILD_AFTER) return ACCEPT_REBUILD;
    return ACCEPT_BACKOFF;
}

#endif
