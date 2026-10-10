/*
 * shellui_rpc.c — call Sony APIs from inside SceShellUI's process
 * via ptrace remote-call. See shellui_rpc.h for the rationale.
 *
 * Lifecycle:
 *   shellui_rpc_init() finds SceShellUI's pid via sysctl(KERN_PROC),
 *   resolves the Sony API addresses inside ShellUI via
 *   kernel_dynlib_handle + kernel_dynlib_dlsym, caches everything.
 *   shellui_rpc_launch_app does
 *   pt_attach → pt_call → pt_detach. Single mutex serialises since
 *   only one tracer can attach a process at a time.
 *
 * Each remote call:
 *   1. pt_attach(shellui_pid)            — SIGSTOP delivered
 *   2. (allocate scratch buffer in target if needed via pt_mmap)
 *   3. pt_call(shellui_pid, addr, args)  — remote function call
 *   4. (pt_copyout if scratch buffer holds output)
 *   5. (pt_munmap if we allocated)
 *   6. pt_detach(shellui_pid, 0)         — let ShellUI continue
 */

#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <pthread.h>
#include <unistd.h>

#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/sysctl.h>
#include <sys/types.h>

#include <ps5/kernel.h>

#include "ptrace_remote.h"
#include "runtime.h"
#include "shellui_rpc.h"

/* kinfo_proc layout from sysctl(KERN_PROC_PROC). Same as FreeBSD 11
 * mainline; PS5 hasn't changed it across the SDK-supported firmware
 * range. The pid lives at offset 72 and the thread-name (cmd) at
 * offset 447 inside each variable-length kinfo_proc entry. */
#define KINFO_PID_OFFSET     72
#define KINFO_TDNAME_OFFSET  447

static int find_pid_by_name(const char *name) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0) return -1;
    if (buf_size == 0) return -1;
    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf) return -1;
    if (sysctl(mib, 4, buf, &buf_size, NULL, 0) != 0) {
        free(buf);
        return -1;
    }
    int found = -1;
    for (uint8_t *ptr = buf; ptr < buf + buf_size;) {
        int ki_structsize = *(int *)ptr;
        if (ki_structsize <= 0 ||
            (size_t)(ptr - buf) + ki_structsize > buf_size) break;
        /* Defensive: the bounds check above proves `ki_structsize` bytes
         * fit, but we then read fixed offsets PID(72) and TDNAME(447);
         * make sure the entry is actually large enough to contain them
         * before dereferencing. On every supported FW kinfo_proc is
         * ~1088 bytes, so this never trips in practice — it just hardens
         * against a malformed/short entry. */
        if (ki_structsize <= KINFO_TDNAME_OFFSET) break;
        pid_t ki_pid = *(pid_t *)&ptr[KINFO_PID_OFFSET];
        const char *ki_tdname = (const char *)&ptr[KINFO_TDNAME_OFFSET];
        if (strcmp(ki_tdname, name) == 0) {
            found = (int)ki_pid;
            break;
        }
        ptr += ki_structsize;
    }
    free(buf);
    return found;
}

/* Cached state from shellui_rpc_init(). Guarded by g_rpc_mtx. */
static pthread_mutex_t g_rpc_mtx = PTHREAD_MUTEX_INITIALIZER;
static int g_inited = 0;
static int g_init_rc = -1;
static int g_shellui_pid = 0;

/* "We currently have ShellUI ptrace-attached" flag. Set inside
 * each RPC just after pt_attach succeeds, cleared just before
 * pt_detach. Read by shellui_rpc_emergency_detach() from a fatal
 * signal handler so we know whether to attempt cleanup. Volatile
 * sig_atomic_t because of the signal-handler access pattern. */
static volatile sig_atomic_t g_attached = 0;

/* Sony API addresses inside SceShellUI's process. Resolved once
 * during init via kernel_dynlib_handle + kernel_dynlib_dlsym. 0
 * means resolution failed; corresponding RPC call returns -1. */
static intptr_t g_addr_lnc_launch = 0;       /* sceLncUtilLaunchApp */
static intptr_t g_addr_user_get_fg = 0;      /* sceUserServiceGetForegroundUser */
/* Wrappers that keep g_attached in sync. Used in place of
 * pt_attach/pt_detach throughout. emergency_detach reads
 * g_attached to decide whether to attempt a fatal-signal-time
 * recovery detach. */
static int pt_attach_tracked(pid_t pid) {
    int rc = pt_attach(pid);
    if (rc == 0) g_attached = 1;
    return rc;
}

static int pt_detach_tracked(pid_t pid, int sig) {
    if (pt_tracee_was_lost(pid)) {
        /* Timeout recovery terminated (or lost control of) this ShellUI. Do not
         * ptrace a cached pid again; Sony will respawn it and the next resolve
         * will discover the fresh process. */
        g_attached = 0;
        g_shellui_pid = 0;
        return -1;
    }
    int rc = pt_detach(pid, sig);
    g_attached = 0;
    return rc;
}

void shellui_rpc_emergency_detach(void) {
    /* Best-effort. Called from fatal-signal context so we cannot
     * use pthread_mutex (it's not async-signal-safe) — we just
     * peek the volatile flag and the cached pid, both
     * sig_atomic_t / int. If we believe a target is attached,
     * attempt PT_DETACH directly without touching ANY mutex.
     *
     * MUST NOT route through pt_detach()/sys_ptrace(): that path
     * takes kernel_rw_lock for the authid swap, and if the fatal
     * signal fired while another thread held that lock (mid-install
     * authid swap, sensor read, another ptrace), locking it here
     * deadlocks the signal handler forever — the listener ports are
     * never released and the console needs a reboot. A raw
     * syscall(2) is async-signal-safe; PT_DETACH of our own tracee
     * doesn't need the elevated-authid swap (the attach already
     * established the tracer relationship), and if the kernel
     * rejects it anyway we're no worse off — this is a dying
     * process's last courtesy to ShellUI, not a guaranteed path.
     *
     * Worst case: another thread also detaches, ours fails with
     * EBUSY, no harm done. Better case: we crashed solo while
     * holding the attach, our detach unfreezes ShellUI before
     * the kernel cleans us up. */
    if (g_attached && g_shellui_pid > 0) {
        (void)syscall(SYS_ptrace, PT_DETACH, g_shellui_pid, (caddr_t)0, 0);
        g_attached = 0;
    }
}

/* Resolve a symbol inside `pid` by trying each library handle in a
 * fallback list. Returns 0 on miss. The first hit wins. */
static intptr_t resolve_in_target(pid_t pid, const char *sym_name) {
    /* Common Sony library handles. libkernel = 0x1 always; the
     * others are assigned sequentially by the loader and may
     * differ across firmware/process — `kernel_dynlib_handle`
     * looks them up by basename so we don't have to hardcode. */
    static const struct {
        const char *basename;
    } libs[] = {
        { "libkernel.sprx" },
        { "libkernel_sys.sprx" },
        { "libkernel_web.sprx" },
        { "libSceSystemService.sprx" },
        { "libSceUserService.sprx" },
        { "libSceLncUtil.sprx" },
        { "libSceLncService.sprx" },
        { "libSceLncServiceJvm.sprx" },
    };
    for (size_t i = 0; i < sizeof(libs)/sizeof(libs[0]); i++) {
        uint32_t handle = 0;
        if (kernel_dynlib_handle(pid, libs[i].basename, &handle) != 0) {
            continue;
        }
        intptr_t addr = kernel_dynlib_dlsym(pid, handle, sym_name);
        if (addr != 0) return addr;
    }
    return 0;
}

/* Re-resolve SceShellUI's pid + symbol addresses. Called both for the
 * first init and any time pt_attach to the cached pid fails — which
 * happens whenever ShellUI restarts (e.g., after exiting a launched
 * game it sometimes respawns with a fresh pid). Caller must hold
 * g_rpc_mtx. */
static int shellui_rpc_resolve_locked(void) {
    int pid = find_pid_by_name("SceShellUI");
    if (pid <= 0) {
        /* Lookup failed — likely a transition gap where the old
         * ShellUI just exited and the new one hasn't appeared in
         * the process table yet. Null out g_shellui_pid so the
         * next caller can't try pt_attach against whatever the
         * cached value was (which would just fail and burn cycles
         * in attach_with_refresh_locked's two-strikes retry). The
         * next call will see g_shellui_pid <= 0 and route through
         * the resolve path from scratch. */
        g_shellui_pid = 0;
        g_init_rc = -1;
        return -1;
    }
    g_shellui_pid = pid;
    /* Resolve the Sony API addresses inside SceShellUI. Only sceLncUtilLaunchApp
     * is required for the init-success contract; without launch we
     * can't justify the cost of attaching to ShellUI at all. */
    g_addr_user_get_fg     = resolve_in_target(pid, "sceUserServiceGetForegroundUser");
    g_addr_lnc_launch      = resolve_in_target(pid, "sceLncUtilLaunchApp");
    g_init_rc = (g_addr_lnc_launch == 0) ? -2 : 0;
    return g_init_rc;
}

int shellui_rpc_init(void) {
    pthread_mutex_lock(&g_rpc_mtx);
    /* Short-circuit only when a previous init *succeeded*. If a
     * previous init failed (typically because kstuff hadn't been
     * loaded yet and the symbol resolves all returned 0), we
     * retry — this is the path that lets the payload "wake up"
     * once the user loads kstuff after we'd already booted. */
    if (g_inited && g_init_rc == 0) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return 0;
    }
    /* (Re-)apply the ucred jailbreak. If kernel R/W is now
     * available where it wasn't at process start, this transitions
     * us from "userland authid" to the debugger authid — without
     * which the ptrace authid swap inside pt_attach can't elevate
     * to PS5_PTRACE_ALLOWED_AUTHID and every PT_ATTACH would
     * EPERM. Idempotent and cheap. */
    runtime_apply_ucred_jailbreak();
    g_inited = 1;
    int rc = shellui_rpc_resolve_locked();
    pthread_mutex_unlock(&g_rpc_mtx);
    return rc;
}

int shellui_rpc_ready(void) {
    pthread_mutex_lock(&g_rpc_mtx);
    int r = (g_inited && g_init_rc == 0) ? 1 : 0;
    pthread_mutex_unlock(&g_rpc_mtx);
    return r;
}

/* Attach to ShellUI with a single re-resolve retry on failure. ShellUI
 * occasionally restarts (after exiting a game, after some menu
 * transitions, after a register-driven app.db update, etc.) and our
 * cached pid then refers to a dead process. Without this retry, the
 * first launch attempt after a respawn would always fail and the user
 * would have to click again — exactly the "first time fails, second
 * time works" pattern reported on FW 9.60.
 *
 * Caller must hold g_rpc_mtx. Returns 0 on attach success, -1 if both
 * the original attach and the re-resolved attach failed. */
static int attach_with_refresh_locked(void) {
    if (g_shellui_pid > 0 && pt_attach_tracked(g_shellui_pid) == 0) {
        return 0;
    }
    /* First attach failed. Re-resolve and try once more. The promise
     * in the resolve_locked comment was unfulfilled before this
     * helper existed: pt_attach failures returned -1 immediately and
     * left the caller to (maybe) re-init manually. */
    if (shellui_rpc_resolve_locked() != 0 || g_shellui_pid <= 0) {
        return -1;
    }
    if (pt_attach_tracked(g_shellui_pid) == 0) {
        return 0;
    }
    /* Second attach failed too. Null the cached pid so the NEXT
     * RPC starts from a clean resolve — without this, ShellUI's
     * post-respawn transition window would leave us pinned to a
     * stale or half-initialized pid and every Hardware-tab poll
     * (5s) would retry the same dead handle. The next call's
     * resolve will see the fresh pid once Sony's respawn
     * settles. Pairs with the "lookup failed" null in
     * shellui_rpc_resolve_locked. */
    g_shellui_pid = 0;
    return -1;
}

/* Ask ShellUI to call sceUserServiceGetForegroundUser(&out_user)
 * via a scratch buffer in ShellUI's address space. */
static int remote_get_foreground_user(int *out_user) {
    if (g_addr_user_get_fg == 0 || g_shellui_pid <= 0) return -1;
    pthread_mutex_lock(&g_rpc_mtx);
    int err_attach = pt_attach_tracked(g_shellui_pid);
    if (err_attach != 0) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }
    intptr_t fn_addr = g_addr_user_get_fg;
    intptr_t scratch = pt_mmap(g_shellui_pid, 0, 0x1000,
                                PROT_READ | PROT_WRITE,
                                MAP_ANON | MAP_PRIVATE, -1, 0);
    if (scratch == -1 || scratch == 0) {
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }
    long rc = pt_call(g_shellui_pid, fn_addr,
                       (uint64_t)scratch, 0, 0, 0, 0, 0);
    int user_id = 0;
    int copy_ok = 0;
    if (rc == 0) {
        copy_ok = (pt_copyout(g_shellui_pid, scratch, &user_id, sizeof(user_id)) == 0);
    }
    (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    if (rc != 0) return -1;
    if (!copy_ok) return -1;
    *out_user = user_id;
    return 0;
}

int shellui_rpc_launch_app(const char *title_id, int user_id_hint) {
    if (!title_id || !shellui_rpc_ready()) return -1;

    /* Foreground user resolution. The caller may pass a known-good
     * user_id from sceUserServiceGetForegroundUser in our own process
     * (which `register.c::launch_title` does eagerly before either
     * launch path). If that's non-zero, trust it. Falling back to
     * remote_get_foreground_user covers callers that don't have one
     * already, but on first-launch-after-register this remote query
     * has been seen to return 0 transiently (ShellUI's foreground
     * tracker was being updated mid-register), causing Sony's
     * launcher to reject with 0x8094000F until the second click —
     * the "first launch fails, second launch works" symptom. */
    int user_id = user_id_hint;
    if (user_id <= 0) {
        (void)remote_get_foreground_user(&user_id);
    }

    /* Allocate scratch buffers inside ShellUI for:
     *   - title_id string  (10 bytes + NUL)
     *   - LncAppParam      (24-byte struct, layout below) */
    pthread_mutex_lock(&g_rpc_mtx);
    if (attach_with_refresh_locked() != 0) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }
    intptr_t scratch = pt_mmap(g_shellui_pid, 0, 0x1000,
                                PROT_READ | PROT_WRITE,
                                MAP_ANON | MAP_PRIVATE, -1, 0);
    if (scratch == -1 || scratch == 0) {
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    /* Layout in scratch:
     *   [0  .. 32) — title_id NUL-terminated
     *   [32 .. 64) — LncAppParam (24 used + pad) */
    char tid_buf[32];
    memset(tid_buf, 0, sizeof(tid_buf));
    strncpy(tid_buf, title_id, sizeof(tid_buf) - 1);

    struct lnc_app_param {
        uint32_t sz;
        int32_t  user_id;
        uint32_t app_opt;
        uint64_t crash_report;
        uint64_t check_flag;
    } __attribute__((packed)) param;
    memset(&param, 0, sizeof(param));
    param.sz = (uint32_t)sizeof(param);
    param.user_id = user_id;
    intptr_t param_addr = scratch + 32;

    /* Stage both args before pt_call. If either copyin fails the remote
     * scratch is partial and pt_call would dereference garbage — bail
     * out cleanly instead of letting Sony's launcher crash and take
     * SceShellUI down with it (full UI loss until reboot). */
    if (pt_copyin(g_shellui_pid, tid_buf, scratch, sizeof(tid_buf)) != 0
     || pt_copyin(g_shellui_pid, &param, param_addr, sizeof(param)) != 0) {
        (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    /* Call sceLncUtilLaunchApp(title_id, NULL, &param) inside ShellUI. */
    long rc = pt_call(g_shellui_pid, g_addr_lnc_launch,
                       (uint64_t)scratch, 0, (uint64_t)param_addr, 0, 0, 0);
    int dispatched = pt_call_was_dispatched();
    int timed_out = pt_call_timed_out();
    int tracee_lost = pt_tracee_was_lost(g_shellui_pid);
    /* DELIBERATELY NOT unmapped, but note what this does and does NOT fix.
     *
     * sceLncUtilLaunchApp is ASYNCHRONOUS: it returns once the launch is
     * queued, while Sony's launcher goes on reading the title_id string and
     * the LncAppParam struct — BOTH of which live in this scratch page.
     * Unmapping immediately after pt_call therefore hands the launcher a
     * pointer to memory we just took away. That is a genuine use-after-unmap
     * regardless of what it happens to break, which is why the page is now
     * left mapped.
     *
     * IT DOES NOT FIX THE FOCUS DROP. Measured on FW 9.60 with this change
     * live, a launch through this path still put the game on screen for ~19s
     * and then dropped to the dashboard, with SceShellUI's app id changing
     * (8199 -> 16391) — i.e. ShellUI still restarted and the fresh instance
     * still took the screen. So something else in the attach/call/detach
     * sequence is destabilising ShellUI; the scratch page was not it.
     *
     * What IS established, same title, minutes apart:
     *   launched through this ptrace path -> on screen ~20s, then dropped
     *   launched from the console UI      -> 100s+ steady, never dropped
     * The launch path is the only variable, so the cause is in here
     * somewhere. Registers are restored correctly on the success path
     * (pt_call -> restore_stopped_or_terminate), so the remaining suspects
     * are the attach/detach itself rather than the injected frame.
     *
     * Cost of leaving it mapped: one 4 KiB page per launch in ShellUI's
     * address space, reclaimed wholesale whenever ShellUI restarts. Cheap
     * enough that correctness wins. Do not reclaim it here without first
     * proving the launcher is done with it.
     */
    (void)scratch;
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    /* Three return shapes:
     *   rc == 0           — Sony's launcher accepted the call. Game launched.
     *   rc > 0            — Sony returned an error code (title not found, no
     *                       foreground user, etc.). Surface the code.
     *   rc == -1          — pt_call hit a failure. Two sub-cases:
     *     dispatched == 0 — pre-call failure (couldn't attach / mmap / setregs).
     *                       Function never ran; caller may retry or fall back.
     *     dispatched == 1 — function WAS invoked but post-call cleanup hit a
     *                       race (waitpid returned non-stopped because the
     *                       launcher signalled ShellUI's process). The launch
     *                       most likely succeeded — return -2 so the caller
     *                       can treat this as a soft success rather than
     *                       falling through to in-process strategies which
     *                       would race the running launch and produce a
     *                       misleading "all strategies failed" error. */
    if (rc == -1 && dispatched && !timed_out && !tracee_lost) {
        return -2;
    }
    return (int)rc;
}
