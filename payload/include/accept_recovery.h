#ifndef PS5UPLOAD2_ACCEPT_RECOVERY_H
#define PS5UPLOAD2_ACCEPT_RECOVERY_H

#include <errno.h>

/*
 * What an accept loop does after accept() fails.
 *
 * There is no "give up" action for an accept failure itself (the one exit is
 * accept_should_exit, below, once rebuilt listeners keep failing). Both accept
 * loops used to break on any errno outside a short allow-list, which left the helper running but
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

/*
 * Rebuilds in a row, with no successful accept between them, after which the
 * helper stops serving and exits so a fresh one can start.
 *
 * The exception to "never give up", and a measured one. On a FW 5.10 Phat the
 * mgmt listener hit errno 163, was rebuilt, and the brand-new listener failed
 * with 163 on its first accept(). Sony's neighbouring network errnos are
 * interface or resume events (160 ADHOC, 161 DISABLEDIF, 162 RESUME): 163
 * disables the PROCESS's sockets, which no rebuild inside it can fix, while
 * the next helper process accepts normally. Retrying forever left a helper
 * half alive (mgmt refused, transfer still answering) that the host neither
 * reached nor replaced. Three rebuilds is about 7 s of proof.
 */
#define ACCEPT_EXIT_AFTER_REBUILDS 3

/* `rebuilds` counts listener rebuilds since the last successful accept(). */
static inline int accept_should_exit(int rebuilds) {
    return rebuilds >= ACCEPT_EXIT_AFTER_REBUILDS;
}

#endif
