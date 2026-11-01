/* The helper's keyboard & mouse backend: a relay to the companion process (src/vinput_relay.c,
 * protocol include/vinput_wire.h). The helper never calls libSceMouse / libSceKeyboard itself.
 *
 * A session that opens while the companion is not running starts it (on its own thread) and answers
 * VINPUT_E_STARTING ("vinput_starting"); the engine asks again until it is up. Every request waits
 * at most call_timeout_ms; a companion silent for silent_ms is ended by pid and started again,
 * with backoff, at most max_fails times in a row, then it is "unavailable"
 * (vinput_companion_failed). A companion that never gets through Init (launched, never answers:
 * an Init that hangs or dies, as an untested firmware might) is ended by pid and NOT started again
 * on its own: "unavailable" (vinput_companion_failed) until a fresh open (one at least 2 s after the
 * previous ask: the user turning the mode on again), and never while that companion still lives
 * (stuck unkillably in Init). No loader on :9021 is "unavailable" with vinput_needs_loader (asked
 * again after loader_retry_ms). Devices the session had open are made again once a restarted
 * companion answers. Host-tested over a fake companion (payload/tests/vinput_relay_selftest.c). */
#ifndef PS5UPLOAD_VINPUT_RELAY_H
#define PS5UPLOAD_VINPUT_RELAY_H

#include "vinput.h"

/* launch's answers besides 0 (sent) and -1 (failed). */
#define VRELAY_NO_LOADER (-2)

typedef struct {
    /* Sends the companion's ELF to the loader. 0, VRELAY_NO_LOADER, or -1. */
    int (*launch)(void *ctx);
    /* Ends the companion with this pid: asked to let go first (SIGTERM), killed if it has not
     * gone after a short wait. Called without the relay's lock (on its thread). */
    void (*kill)(void *ctx, int pid);
    /* 1 while pid lives. NULL: kill(pid, 0). */
    int (*alive)(void *ctx, int pid);
    void *ctx;
    int port;                              /* 0: VWIRE_PORT */
    const char *token_path, *pid_path;     /* NULL: VWIRE_TOKEN_PATH / VWIRE_PID_PATH */
    int call_timeout_ms;                   /* 0: 200 */
    int silent_ms;                         /* 0: 1000 */
    int hb_ms;                             /* 0: 300 */
    int connect_wait_ms;                   /* 0: 8000 (the loader, then the companion's Init) */
    int max_fails;                         /* 0: 3 */
    int backoff_ms;                        /* 0: 1000 (doubled each failure) */
    int healthy_ms;                        /* 0: 30000: running this long forgets past failures */
    int loader_retry_ms;                   /* 0: 10000 */
} vrelay_cfg_t;

/* Sets the launcher and the timings. Before first use (runtime.c; the tests between runs, after
 * vinput_relay_reset). */
void vinput_relay_configure(const vrelay_cfg_t *cfg);
/* The backend ("relay"). */
const vinput_backend_t *vinput_relay_backend(void);
/* Tests: drops the companion connection, ends the relay thread and forgets everything. */
void vinput_relay_reset(void);
/* The companion's pid while connected (0 otherwise). */
int vinput_relay_pid(void);

#endif
