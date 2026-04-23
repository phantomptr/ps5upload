#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>
#include <pthread.h>
#include "config.h"
#include "runtime.h"
#include "register.h"

/*
 * PS5 toast notification — pops the message in the top-right corner of
 * the PS5 UI. Linked straight against the libkernel symbol the legacy
 * payload used; there's no public header for it in the SDK so we
 * forward-declare. Structure layout (particularly the 45-byte header)
 * is reverse-engineered and reproduced here verbatim from the legacy
 * payload to keep the ABI call identical.
 */
typedef struct ps5_notify_req {
    char _reserved[45];
    char message[3075];
} ps5_notify_req_t;
int sceKernelSendNotificationRequest(int, ps5_notify_req_t *, size_t, int);

static void pop_notification(const char *message) {
    ps5_notify_req_t req;
    memset(&req, 0, sizeof(req));
    strncpy(req.message, message, sizeof(req.message) - 1);
    (void)sceKernelSendNotificationRequest(0, &req, sizeof(req), 0);
}

/*
 * Global pointer to the runtime state so signal handlers can close the
 * listening socket and release port 9113 before the process dies.
 * A crashed payload that keeps the port open prevents the next payload
 * from binding (and makes every incoming connection get RST'd).
 */
static runtime_state_t *g_state = NULL;

static void handle_fatal(int sig) {
    /* Close the listener immediately so the port is freed. */
    if (g_state) {
        runtime_cleanup_listener(g_state);
    }
    /* Re-raise so the default handler runs (core dump, proper exit code). */
    signal(sig, SIG_DFL);
    raise(sig);
}

int main(void) {
    int rc = 0;
    runtime_state_t state = {0};
    g_state = &state;

    /* Release port on crash or external kill. */
    signal(SIGSEGV, handle_fatal);
    signal(SIGABRT, handle_fatal);
    signal(SIGBUS,  handle_fatal);
    signal(SIGTERM, handle_fatal);
    signal(SIGHUP,  handle_fatal);

    /* NOTE on toasts: sceKernelSendNotificationRequest is only called
     * AFTER runtime_init completes below. An earlier attempt to fire a
     * "loading..." toast at the top of main seemed to prevent the
     * payload from coming up on some setups -- matching the exact
     * ordering of the last-known-working 2.1.0 build is safer than
     * adding diagnostics that could themselves regress startup. */

    if (runtime_ensure_directories() != 0) {
        fprintf(stderr, "runtime_ensure_directories failed\n");
        return 1;
    }

    if (runtime_init(&state) != 0) {
        fprintf(stderr, "runtime_init failed\n");
        return 1;
    }

    /* NOTE: register_module_init() is NOT called at startup -- the
     * dlopen path for Sony's sprx libraries has been observed to hang
     * or crash on some firmware configurations, which would prevent
     * the payload from ever reaching runtime_try_takeover. The module
     * self-initialises lazily on the first APP_* request (see
     * register.c's public entry points), so a broken dlopen only
     * fails the register/launch/list call, not the whole payload.
     *
     * However, we DO call register_services_init() eagerly from the
     * main thread (see below, after takeover). That function invokes
     * sceUserServiceInitialize + sceAppInstUtilInitialize +
     * sceLncUtilInitialize so Sony's services are established in
     * main-thread context. Without this, calling the installer from
     * a spawned HTTP handler thread hits a kernel-lock deadlock on
     * firmware 9.60. */

    if (runtime_try_takeover(&state) != 0) {
        fprintf(stderr, "runtime_takeover failed — another instance may still own the port\n");
        return 1;
    }

    if (runtime_write_ownership(&state) != 0) {
        fprintf(stderr, "runtime_write_ownership failed\n");
        return 1;
    }

    /* Startup-time calls to register_services_init() and
     * runtime_reconcile_mounts() are deliberately disabled. Both
     * invoke Sony APIs (dlopen of libSceAppInstUtil.sprx,
     * getmntinfo on potentially-stale entries) that have been
     * observed to hang on some firmware/loader combinations,
     * preventing the payload from ever reaching
     * runtime_server_loop() below — which means the listener
     * never binds :9113/:9114 and the client times out waiting
     * for the payload to boot.
     *
     * register_services_init still runs lazily on first APP_*
     * request (via register_module_init in register.c), and
     * runtime_reconcile_mounts can be invoked on-demand from a
     * future Volumes → Refresh action. Moving them out of the
     * startup path trades a ~100ms per-request delay on their
     * first use for a payload that actually comes up reliably. */

    printf("ps5upload2 payload ready on ports transfer=%d mgmt=%d (instance=%llu)\n",
           state.runtime_port, state.mgmt_port,
           (unsigned long long)state.instance_id);
    /* One-shot toast on startup — makes the user see on the TV/monitor
     * that the ELF actually loaded and is listening, without needing to
     * pull out a laptop to probe the port. Includes version + author so
     * the console screen is enough to identify *which* build is running
     * (useful when debugging across revisions). */
    {
        char banner[192];
        snprintf(banner, sizeof(banner),
                 "PS5Upload v%s by %s\nready on %d/%d",
                 PS5UPLOAD2_VERSION, PS5UPLOAD2_AUTHOR,
                 state.runtime_port, state.mgmt_port);
        pop_notification(banner);
    }
    /* Spawn the management listener thread BEFORE entering the transfer
     * loop. The mgmt loop owns :9114 and answers STATUS/TAKEOVER/etc.
     * while the transfer loop is busy inside a long upload on :9113. */
    if (pthread_create(&state.mgmt_thread, NULL,
                       runtime_mgmt_server_loop, &state) != 0) {
        fprintf(stderr, "pthread_create(mgmt) failed\n");
        return 1;
    }
    state.mgmt_thread_started = 1;

    rc = runtime_server_loop(&state);

    /* Ask the mgmt thread to exit by closing its listener. accept()
     * returns with EBADF, mgmt loop sees shutdown_requested and
     * exits. Ordering matters: set shutdown_requested first so the
     * mgmt loop's post-accept check short-circuits. */
    state.shutdown_requested = 1;
    if (state.mgmt_listener_fd >= 0) {
        close(state.mgmt_listener_fd);
        state.mgmt_listener_fd = -1;
    }
    if (state.mgmt_thread_started) {
        pthread_join(state.mgmt_thread, NULL);
    }

    (void)runtime_clear_ownership(&state);
    return rc == 0 ? 0 : 1;
}
