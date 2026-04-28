#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>
#include <pthread.h>
#include <ps5/kernel.h>
#include "config.h"
#include "runtime.h"
#include "register.h"
#include "shellui_rpc.h"

/* Sony "debugger" / system-process authid. Setting our process's
 * ucred authid to this value grants the credentials Sony's kernel
 * stubs check before allowing sensor reads / launches / installs
 * from a userland payload. The elevation requires kernel R/W
 * primitives, which the SDK's `kernel_copyin/copyout` provide once
 * a kstuff-style loader has exposed kernel access — kstuff (or an
 * equivalent kernel-RW loader) must run before our payload. */
#define PS5_SYSTEM_PROCESS_AUTHID  0x4800000000000006ull

/*
 * PS5 toast notification — pops the message in the top-right corner of
 * the PS5 UI. Linked straight against the libkernel symbol the legacy
 * payload used; there's no public header for it in the SDK so we
 * forward-declare. Structure layout (particularly the 45-byte header)
 * is reverse-engineered and reproduced here verbatim from the legacy
 * payload to keep the ABI call identical.
 *
 * `pop_notification` is exported (declared in runtime.h) so handlers
 * in runtime.c can fire toasts on user-visible state changes (mount,
 * unmount, register, launch). The toast gives the user feedback on
 * the PS5 screen even when the desktop client is closed.
 */
typedef struct ps5_notify_req {
    char _reserved[45];
    char message[3075];
} ps5_notify_req_t;
int sceKernelSendNotificationRequest(int, ps5_notify_req_t *, size_t, int);

void pop_notification(const char *message) {
    if (!message || !*message) return;
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

/*
 * Return code from the kernel_set_ucred_authid() elevation in main().
 * 0 = elevation succeeded (kstuff was loaded → kernel R/W available).
 * < 0 = elevation failed (no kernel R/W; Sony APIs that need
 * elevation will reject calls). Exposed via STATUS_ACK so clients
 * can warn the user "load kstuff first" without probing Sony APIs.
 */
int g_ucred_elevation_rc = -1;

static void handle_fatal(int sig) {
    /* If we crashed mid-RPC, we may be holding a ptrace attach to
     * SceShellUI. Without a detach the kernel keeps ShellUI in
     * SIGSTOP until our process is fully reaped, which freezes the
     * PS5 UI for the duration. Best-effort detach first; the call
     * is signal-safe (no mutex; just a kernel ioctl path). */
    shellui_rpc_emergency_detach();
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

    /* Full ucred jailbreak — Sony's caller-context check rejects
     * userland payloads with "wrong credentials" even when authid
     * alone is set. The kernel checks several ucred fields, so
     * we elevate all of them:
     *
     *   uid + ruid + svuid = 0      (root)
     *   rgid + svgid = 0
     *   sceCaps[0..15] = 0xFF       (every Sony capability bit set)
     *   sceAttr = 0x80000000        (system-process attribute byte)
     *   sceAuthID = 0x4800000000000006   (debugger authid)
     *
     * The PS5 SDK exposes per-field helpers (kernel_set_ucred_uid,
     * kernel_set_ucred_caps, etc.) which wrap the kernel R/W
     * primitives so the per-firmware ucred-offset constants can
     * update without us tracking them by hand.
     *
     * pid -1 = current process; the helpers translate to getpid().
     * Each call is best-effort; if any returns non-zero we record
     * it but keep going — partial elevation is better than none.
     *
     * Done at the very top of main, BEFORE any sprx init, so every
     * subsequent service init + Sony API call runs with full
     * jailbreak in place. Result aggregated in
     * `g_ucred_elevation_rc` (0 = full elevation success) for
     * STATUS_ACK to surface. */
    {
        int rc = 0;
        rc |= kernel_set_ucred_uid(-1, 0);
        rc |= kernel_set_ucred_ruid(-1, 0);
        rc |= kernel_set_ucred_svuid(-1, 0);
        rc |= kernel_set_ucred_rgid(-1, 0);
        rc |= kernel_set_ucred_svgid(-1, 0);
        /* Sce caps — 16 bytes of 0xFF == every capability bit. */
        static const uint8_t k_full_caps[16] = {
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        };
        rc |= kernel_set_ucred_caps(-1, k_full_caps);
        /* sceAttr: byte 3 = 0x80 of the 8-byte attrs field. In
         * little-endian that's the low 32-bit half having
         * 0x80000000 in its high byte → full uint64 = 0x80000000.
         * This is the system-process attribute Sony's caller checks
         * inspect alongside authid. */
        rc |= kernel_set_ucred_attrs(-1, 0x80000000ull);
        /* sceAuthID — last because if anything earlier failed we
         * still want at least authid set. */
        rc |= kernel_set_ucred_authid(-1, PS5_SYSTEM_PROCESS_AUTHID);

        /* Sandbox escape: re-root the process at the kernel's root
         * vnode + jaildir. This differentiates "running in jail"
         * (Sony APIs see us as a sandboxed process and refuse)
         * from "running with system access" (they accept). The
         * SDK exposes both helpers in `<ps5/kernel.h>`. With full
         * ucred elevation + sandbox escape, the only remaining
         * caller-context check Sony enforces for sensors / launch
         * is the getpid()==SceShellUI test — which the ShellUI
         * ptrace RPC layer (shellui_rpc.c) handles. */
        intptr_t root_vnode = kernel_get_root_vnode();
        if (root_vnode != 0) {
            rc |= kernel_set_proc_rootdir(-1, root_vnode);
            rc |= kernel_set_proc_jaildir(-1, root_vnode);
        }
        g_ucred_elevation_rc = rc;
    }

    /* Release port on crash or external kill. */
    signal(SIGSEGV, handle_fatal);
    signal(SIGABRT, handle_fatal);
    signal(SIGBUS,  handle_fatal);
    signal(SIGTERM, handle_fatal);
    signal(SIGHUP,  handle_fatal);
    /* CRITICAL: ignore SIGPIPE. Without this, every client disconnect
     * mid-write (TCP RST during a SHARD, control client closing while
     * we're sending an ACK, browser tab reload mid-FS_LIST_DIR
     * response) delivers SIGPIPE whose default disposition is
     * terminate. The payload would then die mid-transfer, leaving
     * the user wondering why "the payload crashes during uploads."
     * With this ignore in place, write() returns -1/EPIPE, and the
     * handler closes the socket and the next iteration of the accept
     * loop continues normally. */
    signal(SIGPIPE, SIG_IGN);

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
