#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/stat.h>
#include <errno.h>
#include "runtime.h"
#include "config.h"
#include "ava1_gen.h"
#include "legacy_takeover.h"
#include "takeover_flag.h"

/* How long a new instance waits for the old one to release its ports. The old instance may be
 * flushing a multi-GiB commit; 10 s covers the common big-upload tail without making a healthy
 * takeover slower (the loop ends the moment the ports are gone). */
#define TAKEOVER_ACK_TIMEOUT_SEC 2
#define TAKEOVER_PORT_RELEASE_ATTEMPTS 100
#define TAKEOVER_PORT_RELEASE_INTERVAL_US 100000

/*
 * Takes this instance's place over whatever instance is already on the console.
 *
 *   1. An instance from before the cutover (the old binary protocol, on two ports of its own): legacy_takeover.c
 *      sends its takeover request. That file is the migration shim and goes in the release after
 *      the cutover.
 *   2. An AVA1-era instance (it listens on the AVA1 port): the flag file. We write
 *      <runtime dir>/takeover naming our instance id; the old instance's poll thread sees a newer
 *      id within a second and exits.
 *
 * Returns 0 once the ports are free (or nothing was running), -1 when an old instance still holds
 * them; main() then escalates (reap, sweep).
 */
int runtime_try_takeover(runtime_state_t *state) {
    struct stat st;
    int rc = 0;

    if (!state) return -1;
    if (state->takeover_nonce == 0) (void)takeover_nonce_new(&state->takeover_nonce);

    if (stat(state->ownership_path, &st) == 0) {
        printf("[payload2] previous ownership record found at %s\n", state->ownership_path);
    } else {
        printf("[payload2] no previous ownership record found\n");
    }
    printf("[payload2] takeover probe: ava1 port %d, instance=%llu\n", (int)AVA1_DEFAULT_PORT,
           (unsigned long long)state->instance_id);

    rc = legacy_takeover_old_ports(TAKEOVER_ACK_TIMEOUT_SEC, TAKEOVER_PORT_RELEASE_ATTEMPTS,
                                   TAKEOVER_PORT_RELEASE_INTERVAL_US);
    if (rc == LEGACY_TAKEOVER_STUCK) {
        fprintf(stderr, "[payload2] takeover timed out: the old helper's ports are still occupied\n");
        return -1;
    }
    if (rc == LEGACY_TAKEOVER_FREED) state->startup_reason = PS5UPLOAD2_STARTUP_TAKEOVER;

    /* A flag left from before this instance (a crash, a reboot) must not be mistaken for a request. */
    takeover_flag_unlink(PS5UPLOAD2_RUNTIME_DIR);

    if (takeover_port_responding((int)AVA1_DEFAULT_PORT)) {
        /* The old instance is still serving AVA1 (an AVA1-era instance, or a transitional one
         * whose legacy ports were just freed but whose AVA1 server is still closing). */
        int ports[1] = {(int)AVA1_DEFAULT_PORT};
        if (takeover_flag_request(PS5UPLOAD2_RUNTIME_DIR, state->takeover_nonce, ports, 1,
                                  TAKEOVER_PORT_RELEASE_ATTEMPTS,
                                  TAKEOVER_PORT_RELEASE_INTERVAL_US) != 0) {
            fprintf(stderr, "[payload2] takeover timed out: ava1 port %d still occupied\n",
                    (int)AVA1_DEFAULT_PORT);
            return -1;
        }
        state->startup_reason = PS5UPLOAD2_STARTUP_TAKEOVER;
    }
    return 0;
}
