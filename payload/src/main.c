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
/* The ShellCore authid (0x3800000000000010) is intentionally NOT
 * defined as a constant here. A 2.2.54-era diagnostic kept the
 * process permanently elevated to ShellCore authid; that turned
 * out to forge a (pid, authid) pair the kernel later cross-checks,
 * causing PS5 black-screen + auto-restart mid-install. We default
 * back to debugger authid (above) and only swap to ShellCore for
 * the install/status window inside bgft.c::appinst_install_*.
 * The full reasoning is in `runtime_apply_ucred_jailbreak` below. */

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
 *
 * `volatile` so the read in runtime.c's STATUS_ACK / PKG_INSTALL_ACK
 * builders can't be hoisted into a register and cached across an
 * intervening `runtime_apply_ucred_jailbreak()` call on the same
 * frame-dispatch thread. Multiple connection threads
 * (mgmt + transfer + concurrent mgmt clients) read this concurrently
 * and the dispatcher writes it on every frame. On x86-64 the int
 * read/write is naturally atomic at the hardware level but the C
 * abstract machine doesn't guarantee that without `volatile` (or
 * `_Atomic`); the qualifier preserves the int-vs-int extern type
 * shape callers already use.
 */
volatile int g_ucred_elevation_rc = -1;

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

/* Full ucred jailbreak — Sony's caller-context check rejects
 * userland payloads with "wrong credentials" even when authid
 * alone is set. The kernel checks several ucred fields, so we
 * elevate all of them:
 *
 *   uid + ruid + svuid = 0      (root)
 *   rgid + svgid = 0
 *   sceCaps[0..15] = 0xFF       (every Sony capability bit set)
 *   sceAttr = 0x80000000        (system-process attribute byte)
 *   sceAuthID = 0x4800000000000006   (debugger authid)
 *
 * Plus a sandbox escape: re-root the process at the kernel's root
 * vnode + jaildir.
 *
 * Idempotent + cheap when already elevated: the early-out short-
 * circuits the entire body. Safe to call from any hot path;
 * runtime.c's frame dispatcher invokes it on every incoming
 * frame so users who load kstuff *after* our payload was already
 * running pick up kernel R/W on the next request — Launch,
 * Register, sensors, all of them — without needing to reboot the
 * PS5 or re-send the payload.
 *
 * Each kernel write is best-effort; if any returns non-zero we
 * record it but keep going — partial elevation is better than
 * none. Result aggregated in `g_ucred_elevation_rc`
 * (0 = full success). */
void runtime_apply_ucred_jailbreak(void) {
    /* Already elevated — nothing to do. This is the steady-state
     * branch once kernel R/W is available, so it has to be cheap. */
    if (g_ucred_elevation_rc == 0) return;

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
    /* sceAttr: byte 3 = 0x80 of the 8-byte attrs field. */
    rc |= kernel_set_ucred_attrs(-1, 0x80000000ull);
    /* sceAuthID — last so if earlier writes fail we at least
     * leave authid in a known state. */
    /* 2.2.54-fix-round-12: REVERTED permanent ShellCore authid.
     * Reason: PS5 was black-screening + auto-restarting mid-install
     * during user testing. Theory: Sony's kernel validates pid+authid
     * pairs internally on some IPC paths. Setting our process's
     * authid to ShellCore (0x3800...) while we're NOT actually
     * SceShellCore (pid 58) creates a forge that's accepted at the
     * authid gate but later mismatch checks corrupt kernel state ->
     * watchdog or panic -> auto-restart.
     *
     * Payloads injected directly into SceShellCore's process can keep
     * permanent ShellCore authid (because authid AND pid both match
     * Sony's expectations). We're a separate :9021-loaded ELF; per-call
     * swap (in bgft.c::appinst_install_start and ::appinst_install_status)
     * is the safe pattern. Default back to debugger authid for kernel
     * R/W and ptrace; swap to ShellCore only for the install/status
     * window then restore. */
    rc |= kernel_set_ucred_authid(-1, PS5_SYSTEM_PROCESS_AUTHID);

    intptr_t root_vnode = kernel_get_root_vnode();
    if (root_vnode != 0) {
        rc |= kernel_set_proc_rootdir(-1, root_vnode);
        rc |= kernel_set_proc_jaildir(-1, root_vnode);
    } else {
        /* No root vnode means kernel R/W isn't available at all. */
        rc |= -1;
    }
    g_ucred_elevation_rc = rc;
}

int main(void) {
    int rc = 0;
    runtime_state_t state = {0};
    g_state = &state;

    /* Run the elevation once at startup. May fail silently if
     * kernel R/W isn't yet available (kstuff not loaded). In
     * that case shellui_rpc_init() will retry on first sensor
     * read so users who load kstuff later are still served. */
    runtime_apply_ucred_jailbreak();

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
        pop_notification("PS5Upload failed: cannot create payload directories");
        return 1;
    }

    /* Sweep orphan Tier-1 staging files. Crash-recovery only;
     * the desktop-side post-install delete handles steady-state. */
    runtime_sweep_stale_pkg_temp();

    if (runtime_init(&state) != 0) {
        fprintf(stderr, "runtime_init failed\n");
        pop_notification("PS5Upload failed: runtime_init (port bind?)");
        return 1;
    }

    /* No eager Sony-service init at startup. Both `register_module_init`
     * (dlopen + dlsym for libSceAppInstUtil/Lnc/UserService) and
     * `register_services_init` (sceUserServiceInitialize +
     * sceAppInstUtilInitialize + sceLncUtilInitialize) have been
     * observed to hang on some firmware/loader combinations,
     * preventing the payload from ever reaching
     * `runtime_try_takeover` and binding :9113/:9114. With the
     * payload listener never up, the desktop times out waiting for
     * the payload to boot.
     *
     * The Sony service inits run lazily inside the relevant entry
     * points in register.c — `register_title_from_path` calls
     * `sceAppInstUtilInitialize` before invoking the installer, and
     * `launch_title` calls `sceUserServiceInitialize` +
     * `sceLncUtilInitialize` before launching. The serialization
     * mutex `g_sony_api_mtx` covers the kernel-lock concern that
     * `register_services_init` was originally added to address. */

    if (runtime_try_takeover(&state) != 0) {
        fprintf(stderr, "runtime_takeover failed — another instance may still own the port\n");
        pop_notification("PS5Upload failed: a previous instance still holds the port — restart the PS5");
        return 1;
    }

    if (runtime_write_ownership(&state) != 0) {
        fprintf(stderr, "runtime_write_ownership failed\n");
        pop_notification("PS5Upload failed: cannot write ownership record");
        return 1;
    }

    /* `runtime_reconcile_mounts` is also deliberately not called at
     * startup. It walks `getmntinfo` on potentially-stale entries
     * which has been observed to hang on some firmware/loader
     * combinations. It is available on-demand from a future Volumes
     * → Refresh action. Trades a ~100ms per-request delay on first
     * use for a payload that actually comes up reliably. */

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
        pop_notification("PS5Upload failed: cannot start management thread");
        /* We've already taken over and written ownership; the kernel
         * will reap the bound sockets at exit, but a stale ownership
         * record would mislead the next payload's startup probe. */
        (void)runtime_clear_ownership(&state);
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
