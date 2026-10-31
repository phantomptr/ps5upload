#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/stat.h>
#include <errno.h>
#include <signal.h>
#include "runtime.h"
#include "config.h"
#include "ava1_app_gen.h"
#include "takeover_flag.h"

/* How long a new instance waits for the old one to release its ports. The old instance may be
 * flushing a multi-GiB commit; 10 s covers the common big-upload tail without making a healthy
 * takeover slower (the loop ends the moment the ports are gone). */
#define TAKEOVER_PORT_RELEASE_ATTEMPTS 100
#define TAKEOVER_PORT_RELEASE_INTERVAL_US 100000

/*
 * Takes this instance's place over whatever instance is already on the console: when one listens
 * on the AVA1 port we write <runtime dir>/takeover naming our instance id; the old instance's poll
 * thread sees a newer id within a second and exits.
 *
 * Returns 0 once the ports are free (or nothing was running), -1 when an old instance still holds
 * them; main() then escalates (reap, sweep).
 */
int runtime_try_takeover(runtime_state_t *state) {
    struct stat st;

    if (!state) return -1;
    if (state->takeover_nonce == 0) (void)takeover_nonce_new(&state->takeover_nonce);

    if (stat(state->ownership_path, &st) == 0) {
        printf("[payload2] previous ownership record found at %s\n", state->ownership_path);
    } else {
        printf("[payload2] no previous ownership record found\n");
    }
    printf("[payload2] takeover probe: ava1 port %d, instance=%llu\n", (int)AVA1_DEFAULT_PORT,
           (unsigned long long)state->instance_id);

    /* A flag left from before this instance (a crash, a reboot) must not be mistaken for a request. */
    takeover_flag_unlink(PS5UPLOAD2_RUNTIME_DIR);
    runtime_sony_wait_clear();

    state->prior_asked = 0;
    if (!takeover_port_responding((int)AVA1_DEFAULT_PORT)) {
        /* Nothing answers, but the recorded prior may still be alive: a console's rest mode left
         * older helpers running with a listener dead on the network (issue #593: after a wake the app
         * saw the helper down, the resend started a second copy). Its flag poll thread still runs, so
         * ask it through the flag all the same and leave the flag for it to see (the poll reads it
         * once a second; this instance's own flag poll removes it when it starts, after the reap).
         * The reap then gives it the asked grace. Verified on a Pro (FW 13.60): a 10.0.2 helper left
         * deaf by rest mode read the flag and exited by itself within a second. */
        int prior = state->prior_rec_pid;
        if (prior > 0 && prior != (int)getpid() && kill((pid_t)prior, 0) == 0 &&
            takeover_flag_write(PS5UPLOAD2_RUNTIME_DIR, state->takeover_nonce) == 0) {
            printf("[payload2] prior instance pid=%d is alive but not serving; asked it to exit\n", prior);
            state->prior_asked = 1;
            state->startup_reason = PS5UPLOAD2_STARTUP_TAKEOVER;
        }
        return 0;
    }
    {
        /* The old instance is still serving AVA1. */
        int ports[1] = {(int)AVA1_DEFAULT_PORT};
        state->prior_asked = 1;
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
