/* The keyboard & mouse companion's server loop (src/vinput_companion.c): the protocol of
 * include/vinput_wire.h over one loopback connection, single-threaded, driving a device backend
 * (vinput_backend_t: on the console src/vinput_dev.c, in the host tests a fake). The console's
 * main is payload/vinput/main.c. Host-tested (payload/tests/vinput_relay_selftest.c). */
#ifndef PS5UPLOAD_VINPUT_COMPANION_H
#define PS5UPLOAD_VINPUT_COMPANION_H

#include "vinput.h"
#include "vinput_wire.h"

typedef struct {
    int port;               /* 0: VWIRE_PORT */
    const char *token_path; /* NULL: VWIRE_TOKEN_PATH */
    const char *pid_path;   /* NULL: VWIRE_PID_PATH; "" writes none */
    long long hb_timeout_ms, idle_exit_ms, auth_wait_ms; /* 0: the VWIRE_* defaults */
    const vinput_backend_t *dev;
    /* Before the port listens (after it is bound, so a second instance never gets here). NULL: none. */
    void (*dev_init)(void);
    /* About once a second; 1 when the devices were made again (a new foreground user): whatever
     * was held is gone, and each open device is opened again (a no-op when the backend has it);
     * one it could not make is asked for again every tick until it is back. NULL: none. */
    int (*dev_tick)(void);
    void (*log)(const char *fmt, ...);
    /* Set to end the loop (the console's SIGTERM handler; tests); set `hang` to stop answering (a
     * stuck companion, tests). */
    volatile int *stop;
    volatile int *hang;
} vcomp_cfg_t;

/* Exit codes. */
#define VCOMP_EXIT_DONE 0     /* the helper left, idle, or nobody came */
#define VCOMP_EXIT_NO_TOKEN 1 /* no token file: not started by a helper */
#define VCOMP_EXIT_BUSY 2     /* the port is taken: another companion runs */

int vcomp_run(const vcomp_cfg_t *cfg);

#endif
