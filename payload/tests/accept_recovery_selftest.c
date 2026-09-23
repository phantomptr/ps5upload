/*
 * Host self-test for accept_recovery.h.
 *
 * Pins the rule that fixed "connects for a few seconds, then disconnects":
 * an accept() failure is never a reason to stop serving. Reproduced on a
 * FW 5.10 console under sustained load — both accept loops got errno 163
 * (beyond FreeBSD's ELAST) and the old code broke out of them.
 */
#include <assert.h>
#include <errno.h>
#include <stdio.h>

#include "../include/accept_recovery.h"

int main(void) {
    /* An interrupted call or an aborted handshake is not a failure of the
     * listener: loop immediately. */
    assert(accept_error_action(EINTR, 1) == ACCEPT_RETRY_NOW);
    assert(accept_error_action(ECONNABORTED, 1) == ACCEPT_RETRY_NOW);

    /* The errno from the field. It must back off and retry... */
    assert(accept_error_action(163, 1) == ACCEPT_BACKOFF);
    assert(accept_error_action(163, ACCEPT_REBUILD_AFTER - 1) == ACCEPT_BACKOFF);
    /* ...and only after a sustained run rebuild the listener. */
    assert(accept_error_action(163, ACCEPT_REBUILD_AFTER) == ACCEPT_REBUILD);
    assert(accept_error_action(163, ACCEPT_REBUILD_AFTER + 100) == ACCEPT_REBUILD);

    /* Every errno, known or not, lands on a way to keep serving. The old
     * allow-list covered EMFILE/ENFILE/ENOBUFS/ENOMEM and broke on the rest. */
    for (int err = 1; err <= 512; err++) {
        for (int n = 1; n <= ACCEPT_REBUILD_AFTER + 1; n++) {
            accept_action_t a = accept_error_action(err, n);
            assert(a == ACCEPT_RETRY_NOW || a == ACCEPT_BACKOFF || a == ACCEPT_REBUILD);
        }
    }

    /* The rebuild threshold is short enough that a dead listener costs a
     * couple of seconds, not a helper. */
    assert(ACCEPT_REBUILD_AFTER > 1 && ACCEPT_REBUILD_AFTER <= 50);

    printf("accept recovery: every accept() failure keeps the helper serving\n");
    return 0;
}
