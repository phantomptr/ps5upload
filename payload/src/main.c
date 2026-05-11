#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <time.h>
#include <unistd.h>
#include <pthread.h>
#include <dlfcn.h>
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
 * PS5 toast notification — pops the message in the top-right corner
 * of the PS5 UI. The SDK doesn't expose a header for
 * sceKernelSendNotificationRequest, so we forward-declare it and
 * provide the structure layout (45-byte reserved header + 3075-byte
 * message body) that the kernel ABI expects.
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
typedef int (*sce_send_notification_fn)(int, ps5_notify_req_t *, size_t, int);

/* Resolved at first toast — the SDK stub for libkernel_web exports
 * sceKernelSendNotificationRequest, but the actual on-PS5 SPRX may
 * not, and a missing symbol with compile-time linkage kills the
 * binary at rtld lib_init time (binary never reaches main, no port
 * bind, silent failure). dlsym pattern lets us tolerate the absence
 * gracefully — a pop_notification call just becomes a no-op. */
void pop_notification(const char *message) {
    if (!message || !*message) return;
    static sce_send_notification_fn p_send = NULL;
    static int resolved = 0;
    if (!resolved) {
        resolved = 1;
        p_send = (sce_send_notification_fn)
            dlsym(RTLD_DEFAULT, "sceKernelSendNotificationRequest");
    }
    if (!p_send) return;
    ps5_notify_req_t req;
    memset(&req, 0, sizeof(req));
    strncpy(req.message, message, sizeof(req.message) - 1);
    (void)p_send(0, &req, sizeof(req), 0);
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

/* File-based startup trace. Writes to /data/ps5upload2/startup.log
 * so a stuck startup can be diagnosed without depending on the
 * mgmt port coming up (which is exactly what we're trying to
 * diagnose). The user can FTP into /data/ps5upload2/ and read this
 * file from any other tool with FS access (e.g. ftpsrv payload).
 *
 * Best-effort: if /data/ps5upload2/ doesn't exist yet, the open
 * call fails and we silently move on. No allocations, no locks —
 * safe to call from anywhere in the startup path including the
 * pre-runtime_init window. */
static void startup_trace(const char *stage) {
    /* /data/ps5upload/ is created by an earlier payload run (or by
     * runtime_ensure_directories below); on a brand-new console it
     * may not exist yet on the very first ENTER_MAIN call, so we fall
     * back to /data/ which is always writable.
     *
     * Earlier path "/data/ps5upload2/..." was a stale-name bug (the
     * runtime root is /data/ps5upload without the "2"); the trace was
     * silently never written, hiding a separate startup crash for
     * weeks. Keep this path consistent with PS5UPLOAD2_RUNTIME_ROOT. */
    FILE *fp = fopen(PS5UPLOAD2_RUNTIME_ROOT "/startup.log", "a");
    if (!fp) {
        fp = fopen("/data/ps5upload_startup.log", "a");
        if (!fp) return;
    }
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        ts.tv_sec = time(NULL);
        ts.tv_nsec = 0;
    }
    fprintf(fp, "%lld.%03ld %s\n",
            (long long)ts.tv_sec, (long)(ts.tv_nsec / 1000000), stage);
    fclose(fp);
}

int main(void) {
    int rc = 0;
    runtime_state_t state = {0};
    g_state = &state;
    startup_trace("ENTER_MAIN");

    /* Run the elevation once at startup. May fail silently if
     * kernel R/W isn't yet available (kstuff not loaded). In
     * that case shellui_rpc_init() will retry on first sensor
     * read so users who load kstuff later are still served. */
    runtime_apply_ucred_jailbreak();
    startup_trace("UCRED_JAILBREAK_DONE");

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

    startup_trace("BEFORE_ENSURE_DIRECTORIES");
    if (runtime_ensure_directories() != 0) {
        startup_trace("ENSURE_DIRECTORIES_FAILED");
        fprintf(stderr, "runtime_ensure_directories failed\n");
        pop_notification("PS5Upload failed: cannot create payload directories");
        return 1;
    }
    startup_trace("ENSURE_DIRECTORIES_DONE");

    /* Sweep orphan Tier-1 staging files. Crash-recovery only;
     * the desktop-side post-install delete handles steady-state. */
    runtime_sweep_stale_pkg_temp();
    startup_trace("SWEEP_DONE");

    if (runtime_init(&state) != 0) {
        startup_trace("RUNTIME_INIT_FAILED");
        fprintf(stderr, "runtime_init failed\n");
        pop_notification("PS5Upload failed: runtime_init (port bind?)");
        return 1;
    }
    startup_trace("RUNTIME_INIT_DONE");

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

    startup_trace("BEFORE_TAKEOVER");
    if (runtime_try_takeover(&state) != 0) {
        startup_trace("TAKEOVER_FAILED");
        fprintf(stderr, "runtime_takeover failed — another instance may still own the port\n");
        pop_notification("PS5Upload failed: a previous instance still holds the port — restart the PS5");
        return 1;
    }
    startup_trace("TAKEOVER_DONE");

    if (runtime_write_ownership(&state) != 0) {
        startup_trace("WRITE_OWNERSHIP_FAILED");
        fprintf(stderr, "runtime_write_ownership failed\n");
        pop_notification("PS5Upload failed: cannot write ownership record");
        return 1;
    }
    startup_trace("WRITE_OWNERSHIP_DONE");

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
    startup_trace("TOAST_DONE");
    /* Spawn the management listener thread BEFORE entering the transfer
     * loop. The mgmt loop owns :9114 and answers STATUS/TAKEOVER/etc.
     * while the transfer loop is busy inside a long upload on :9113. */
    if (pthread_create(&state.mgmt_thread, NULL,
                       runtime_mgmt_server_loop, &state) != 0) {
        startup_trace("MGMT_THREAD_FAILED");
        fprintf(stderr, "pthread_create(mgmt) failed\n");
        pop_notification("PS5Upload failed: cannot start management thread");
        /* We've already taken over and written ownership; the kernel
         * will reap the bound sockets at exit, but a stale ownership
         * record would mislead the next payload's startup probe. */
        (void)runtime_clear_ownership(&state);
        return 1;
    }
    state.mgmt_thread_started = 1;
    startup_trace("MGMT_THREAD_SPAWNED");

    rc = runtime_server_loop(&state);
    startup_trace("SERVER_LOOP_EXITED");

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
