/*
 * shellui_rpc.c — call Sony APIs from inside SceShellUI's process
 * via ptrace remote-call. See shellui_rpc.h for the rationale.
 *
 * Lifecycle:
 *   shellui_rpc_init() finds SceShellUI's pid via sysctl(KERN_PROC),
 *   resolves the Sony API addresses inside ShellUI via
 *   kernel_dynlib_handle + kernel_dynlib_dlsym, caches everything.
 *   shellui_rpc_launch_app / shellui_rpc_get_*temp do
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
#include <pthread.h>
#include <unistd.h>

#include <sys/mman.h>
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
static intptr_t g_addr_get_cpu_temp = 0;     /* sceKernelGetCpuTemperature */
static intptr_t g_addr_get_soc_temp = 0;     /* sceKernelGetSocSensorTemperature */
static intptr_t g_addr_get_cpu_freq = 0;     /* sceKernelGetCpuFrequency */
static intptr_t g_addr_get_soc_power = 0;    /* sceKernelGetSocPowerConsumption */
/* AppInstUtil entry points used by the install RPC. We invoke these
 * INSIDE ShellUI's process via pt_call so Sony's PlayGo accepts the
 * caller context — calling them from our own payload returns
 * 0x80B22404 (PlayGo HTTP 404) at URL pre-flight regardless of
 * cred-forge, because Sony's installer whitelists ShellUI's process
 * attributes specifically. The address resolution uses
 * `kernel_dynlib_dlsym(shellui_pid, ...)` which walks ShellUI's
 * own loaded modules — `libSceAppInstUtil.sprx` is normally already
 * loaded into ShellUI's address space because Settings → Debug →
 * Install Package depends on it. */
static intptr_t g_addr_appinst_init = 0;     /* sceAppInstUtilInitialize */
static intptr_t g_addr_appinst_install = 0;  /* sceAppInstUtilInstallByPackage */
static intptr_t g_addr_appinst_cancel  = 0;  /* sceAppInstUtilCancelInstall(content_id) */
static intptr_t g_addr_appinst_terminate = 0; /* sceAppInstUtilTerminate() — paired with
                                                * Initialize to fully reset Sony's
                                                * installer state. Used as last-resort
                                                * recovery when Sony's queue is wedged
                                                * with stuck same-content_id tasks. */

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
    int rc = pt_detach(pid, sig);
    g_attached = 0;
    return rc;
}

void shellui_rpc_emergency_detach(void) {
    /* Best-effort. Called from fatal-signal context so we cannot
     * use pthread_mutex (it's not async-signal-safe) — we just
     * peek the volatile flag and the cached pid, both
     * sig_atomic_t / int. If we believe a target is attached,
     * attempt PT_DETACH directly without touching the mutex.
     * Worst case: another thread also detaches, ours fails with
     * EBUSY, no harm done. Better case: we crashed solo while
     * holding the attach, our detach unfreezes ShellUI before
     * the kernel cleans us up. */
    if (g_attached && g_shellui_pid > 0) {
        (void)pt_detach_tracked(g_shellui_pid, 0);
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
        /* 2.2.52-fix: AppInstUtil exports the install/initialize
         * symbols. Without this entry, every `sceAppInstUtil*` lookup
         * inside ShellUI failed silently → shellui-rpc install tier
         * returned 0xE0000002 (SYMBOL_MISSING) and we fell through to
         * the in-process AppInstUtil path that hits 0x80B22404 on
         * FW 9.60+. */
        { "libSceAppInstUtil.sprx" },
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
    /* Resolve every Sony API address inside SceShellUI. We don't
     * fail the whole init if a single sensor symbol is missing —
     * the corresponding RPC just returns -1. Only sceLncUtilLaunchApp
     * is required for the init-success contract; without launch we
     * can't justify the cost of attaching to ShellUI at all. */
    g_addr_user_get_fg     = resolve_in_target(pid, "sceUserServiceGetForegroundUser");
    g_addr_lnc_launch      = resolve_in_target(pid, "sceLncUtilLaunchApp");
    g_addr_get_cpu_temp    = resolve_in_target(pid, "sceKernelGetCpuTemperature");
    g_addr_get_soc_temp    = resolve_in_target(pid, "sceKernelGetSocSensorTemperature");
    g_addr_get_cpu_freq    = resolve_in_target(pid, "sceKernelGetCpuFrequency");
    g_addr_get_soc_power   = resolve_in_target(pid, "sceKernelGetSocPowerConsumption");
    /* AppInstUtil — used by `shellui_rpc_install_pkg`. Best-effort
     * because the install RPC is optional (BGFT/AppInstUtil from our
     * own process is the fallback path; if BOTH are missing we
     * surface a useful "FW doesn't expose installer" error rather
     * than fail init). On older firmwares libSceAppInstUtil.sprx may
     * not be loaded into ShellUI's address space, in which case
     * resolution returns 0 and the install RPC short-circuits. */
    g_addr_appinst_init    = resolve_in_target(pid, "sceAppInstUtilInitialize");
    g_addr_appinst_install = resolve_in_target(pid, "sceAppInstUtilInstallByPackage");
    /* sceAppInstUtilCancelInstall(content_id) — used to clear stuck
     * same-content_id tasks from Sony's installer queue before
     * registering a new install. Without this, repeated install
     * attempts pile up tasks that Sony can't process and eventually
     * wedge the entire PS5. Resolved here so install_pkg can call
     * it via pt_call before the actual register. */
    g_addr_appinst_cancel  = resolve_in_target(pid, "sceAppInstUtilCancelInstall");
    g_addr_appinst_terminate = resolve_in_target(pid, "sceAppInstUtilTerminate");
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

int shellui_rpc_pid(void) {
    pthread_mutex_lock(&g_rpc_mtx);
    int r = g_shellui_pid;
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

/* Attach + run a single function call inside SceShellUI + detach.
 * Acquires g_rpc_mtx for the duration so concurrent callers from
 * different mgmt threads don't both try to ptrace. The mutex
 * release at the end guarantees ShellUI is detached before the
 * next caller can attach.
 *
 * `*ok_out` (if non-NULL) reports whether the RPC machinery itself
 * (attach + dispatch + detach) ran cleanly — NOT whether the
 * remote function succeeded. Set to 1 once we make it past the
 * attach step, regardless of pt_call's return value; 0 means we
 * couldn't even attach. Callers use this to distinguish an "RPC
 * couldn't run" failure (retry maybe helps) from "RPC ran, the
 * remote function returned <rc>" (treat <rc> as the result).
 * Sensor reads check `ok_out && rc > 0`; the launch path uses
 * `pt_call_was_dispatched()` for the finer-grained
 * dispatched-but-waitpid-raced detection. */
static long do_remote_call(intptr_t addr, uint64_t a0, uint64_t a1,
                            uint64_t a2, uint64_t a3, uint64_t a4,
                            uint64_t a5, int *ok_out) {
    if (ok_out) *ok_out = 0;
    if (addr == 0 || g_shellui_pid <= 0) return -1;
    pthread_mutex_lock(&g_rpc_mtx);
    if (attach_with_refresh_locked() != 0) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }
    long rc = pt_call(g_shellui_pid, addr, a0, a1, a2, a3, a4, a5);
    /* Always detach even if the call failed — leaving ShellUI
     * stopped would freeze the entire UI. */
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    if (ok_out) *ok_out = 1;
    return rc;
}

/* Helper for the common sensor pattern: attach → mmap scratch →
 * call fn(scratch[, extra_arg]) → copy result out → munmap →
 * detach. `arg_is_first` selects argument order:
 *   1 → call(scratch, 0, 0, 0, 0, 0)            (single int* out)
 *   0 → call(extra_arg, scratch, 0, 0, 0, 0)    (id, int* out)
 *
 * `out_len` selects how many bytes to copy back (4 for int, 4 for
 * uint32_t — both are handled identically since int is 32-bit on
 * PS5 prospero-clang).
 *
 * Returns 0 on success with the result written to `out_buf`,
 * -1 on any failure. */
static int rpc_call_with_int_scratch(intptr_t fn_addr,
                                      uint64_t extra_arg,
                                      int arg_is_first,
                                      void *out_buf, size_t out_len) {
    if (fn_addr == 0 || !shellui_rpc_ready() || !out_buf || out_len == 0) {
        return -1;
    }
    if (out_len > sizeof(uint64_t)) return -1;
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
    long rc;
    if (arg_is_first) {
        rc = pt_call(g_shellui_pid, fn_addr, (uint64_t)scratch,
                     0, 0, 0, 0, 0);
    } else {
        rc = pt_call(g_shellui_pid, fn_addr, extra_arg,
                     (uint64_t)scratch, 0, 0, 0, 0);
    }
    int copy_ok = 0;
    if (rc == 0) {
        if (pt_copyout(g_shellui_pid, scratch, out_buf, out_len) == 0) {
            copy_ok = 1;
        }
    }
    (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    return (rc == 0 && copy_ok) ? 0 : -1;
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
    if (rc == 0) {
        (void)pt_copyout(g_shellui_pid, scratch, &user_id, sizeof(user_id));
    }
    (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    if (rc != 0) return -1;
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
    (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
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
    if (rc == -1 && dispatched) {
        return -2;
    }
    return (int)rc;
}

/* 2.2.55 + 2.2.59: sensor reads retry-with-reresolve + authid-rearm.
 *
 * Original failure mode (2.2.55 fixed): ShellUI respawns silently
 * invalidate cached symbol addresses; first sensor read after a fresh
 * payload push works, subsequent reads return garbage.
 *
 * Persistent failure mode (2.2.59 fix): even with the symbol re-resolve,
 * sensor reads stop working FOREVER after an NPXS install attempt or
 * any ptrace path that mutates our process's ucred authid. Path:
 *   bgft.c::appinst_install_start saves authid, swaps to ShellCore,
 *   calls install, restores authid. The restore is best-effort; if
 *   it fails (sceKernelSetUcredAttrs returned non-zero, race vs.
 *   another thread doing kernel R/W, etc.), our process is stuck
 *   with ShellCore authid 0x3800… instead of debugger authid 0x4800….
 *   Subsequent pt_attach for sensor reads fails inside sys_ptrace's
 *   authid swap because ShellCore authid isn't on the ptrace allowlist.
 *
 * The fix: on every retry path, re-apply the debugger authid directly.
 * Idempotent + cheap when authid is already correct (kernel_set_ucred_*
 * is a single sysctl-style call). When authid was wrong, this restores
 * it and the retry pt_attach succeeds.
 *
 * Don't loop forever: try, fail → reresolve+rearm+retry → fail → -1.
 * If a FW doesn't expose the symbol at all (NID-only on some 9.x points),
 * burning more pt_attach cycles won't help. */
extern int kernel_set_ucred_authid(pid_t pid, uint64_t authid);
#define PS5_DEBUGGER_AUTHID  0x4800000000000006ull

static void force_reresolve(void) {
    /* Re-arm the debugger authid before re-resolve. If a prior
     * bgft.c install path failed to restore on its way out, our
     * process authid is wrong and the pt_attach inside the next
     * resolve pass will fail too — making the symbol re-resolve
     * itself a no-op. Restoring authid first is the prerequisite. */
    (void)kernel_set_ucred_authid(-1, PS5_DEBUGGER_AUTHID);
    pthread_mutex_lock(&g_rpc_mtx);
    (void)shellui_rpc_resolve_locked();
    pthread_mutex_unlock(&g_rpc_mtx);
}

int shellui_rpc_get_cpu_temp(int *out_celsius) {
    /* sceKernelGetCpuTemperature(int *out) — single int-out arg. */
    int rc = rpc_call_with_int_scratch(g_addr_get_cpu_temp, 0, 1,
                                       out_celsius, sizeof(*out_celsius));
    if (rc == 0) return 0;
    force_reresolve();
    return rpc_call_with_int_scratch(g_addr_get_cpu_temp, 0, 1,
                                     out_celsius, sizeof(*out_celsius));
}

int shellui_rpc_get_soc_temp(int *out_celsius) {
    /* sceKernelGetSocSensorTemperature(int sensor_id, int *out).
     * Sensor id 0 is the primary SoC sensor. */
    int rc = rpc_call_with_int_scratch(g_addr_get_soc_temp, 0, 0,
                                       out_celsius, sizeof(*out_celsius));
    if (rc == 0) return 0;
    force_reresolve();
    return rpc_call_with_int_scratch(g_addr_get_soc_temp, 0, 0,
                                     out_celsius, sizeof(*out_celsius));
}

int shellui_rpc_get_cpu_freq_hz(long *out_hz) {
    /* sceKernelGetCpuFrequency() returns the frequency directly in
     * rax (no out-param). */
    if (!out_hz) return -1;
    /* First attempt with current cached symbol. */
    if (g_addr_get_cpu_freq != 0 && shellui_rpc_ready()) {
        int ok = 0;
        long rc = do_remote_call(g_addr_get_cpu_freq, 0, 0, 0, 0, 0, 0, &ok);
        if (ok && rc > 0) { *out_hz = rc; return 0; }
    }
    /* Retry once after re-resolve. */
    force_reresolve();
    if (g_addr_get_cpu_freq == 0 || !shellui_rpc_ready()) return -1;
    int ok = 0;
    long rc = do_remote_call(g_addr_get_cpu_freq, 0, 0, 0, 0, 0, 0, &ok);
    if (!ok || rc <= 0) return -1;
    *out_hz = rc;
    return 0;
}

int shellui_rpc_get_soc_power_mw(uint32_t *out_mw) {
    /* sceKernelGetSocPowerConsumption(uint32_t *out). */
    int rc = rpc_call_with_int_scratch(g_addr_get_soc_power, 0, 1,
                                       out_mw, sizeof(*out_mw));
    if (rc == 0) return 0;
    force_reresolve();
    return rpc_call_with_int_scratch(g_addr_get_soc_power, 0, 1,
                                     out_mw, sizeof(*out_mw));
}

/* ── shellui_rpc_install_pkg ─────────────────────────────────────────
 *
 * Install a fakepkg by remotely calling
 * `sceAppInstUtilInstallByPackage(MetaInfo*, AppInstPkgInfo*,
 *  PlayGoInfo*)` inside SceShellUI's process. Struct layouts match
 * Sony's AppInstUtil ABI (psdevwiki reference). Sony's PlayGo
 * whitelists ShellUI's process attributes for HTTP fetch, so the
 * same call that returns 0x80B22404 (HTTP 404 pre-flight) from our
 * payload's own context succeeds when issued from ShellUI's. */

#define APPINST_LANGUAGE_SIZE          8
#define APPINST_PLAYGO_SCENARIO_SIZE   3
#define APPINST_CONTENTID_SIZE         0x30
#define APPINST_NUM_LANGUAGES          30
#define APPINST_NUM_IDS                64

/* MetaInfo — first arg to sceAppInstUtilInstallByPackage. 6 pointers,
 * absolute (in target address space). Sony reads these fields as
 * NUL-terminated strings; we point them at offsets within the
 * scratch buffer (see layout below). 48 bytes. */
typedef struct {
    intptr_t uri;
    intptr_t ex_uri;
    intptr_t playgo_scenario_id;
    intptr_t content_id;
    intptr_t content_name;
    intptr_t icon_url;
} appinst_meta_t;

/* AppInstPkgInfo — second arg. Sony fills the content_id field as
 * an OUTPUT; we leave it zeroed. 56 bytes (0x30 + 2 ints, 4-byte
 * aligned, no struct padding tail because content_id ends on 8-byte
 * boundary already). */
typedef struct {
    char    content_id[APPINST_CONTENTID_SIZE];
    int32_t content_type;
    int32_t content_platform;
} appinst_pkg_info_t;

/* PlayGoInfo — third arg. Bulk of the scratch. Sony reads the
 * arrays for downloadable-content playgo scenario routing; for a
 * vanilla install the zero-init is correct (no per-language /
 * scenario splits). Total 9984 bytes. */
typedef struct {
    char  languages[APPINST_NUM_LANGUAGES][APPINST_LANGUAGE_SIZE];        /* 240 */
    char  playgo_scenario_ids[APPINST_NUM_IDS][APPINST_PLAYGO_SCENARIO_SIZE]; /* 192 */
    char  content_ids[APPINST_NUM_IDS][APPINST_CONTENTID_SIZE];            /* 3072 */
    unsigned char  unknown[6480];                                          /* 6480 */
} appinst_playgo_t;

/* Scratch layout in ShellUI's address space:
 *   [0          ] meta            (48 bytes)
 *   [48         ] pkg_info        (56 bytes)
 *   [104        ] padding to 16-byte align playgo
 *   [112        ] playgo          (9984 bytes)
 *   [10096      ] strings…
 *
 * Each MetaInfo pointer is set to SCRATCH + string-offset. Strings
 * are NUL-terminated; Sony reads them with strlen-style probes. */
#define APPINST_OFF_META     0
#define APPINST_OFF_PKGINFO  48
#define APPINST_OFF_PLAYGO   112
#define APPINST_OFF_STRINGS  (APPINST_OFF_PLAYGO + sizeof(appinst_playgo_t))
#define APPINST_SCRATCH_SIZE 0x4000   /* 16 KiB — fits the structs + strings comfortably */

int shellui_rpc_install_pkg(const char *url,
                             const char *content_id,
                             const char *title,
                             uint32_t *out_err_code) {
    if (out_err_code) *out_err_code = 0;
    if (!url || url[0] == '\0') return -1;
    if (!shellui_rpc_ready()) return -1;
    if (g_addr_appinst_install == 0) {
        /* Symbol cache is stale — most often because ShellUI
         * respawned between calls (Sony does this when a heavy
         * install task either crashes or finishes its lifecycle).
         * The new ShellUI's libSceAppInstUtil may be loaded but at
         * a different address, OR not loaded yet. Retry the resolve
         * once; if it's still missing, fall through to the
         * SYMBOL_MISSING surface so the diagnostic panel can tell
         * the user to retry / reboot. */
        pthread_mutex_lock(&g_rpc_mtx);
        (void)shellui_rpc_resolve_locked();
        pthread_mutex_unlock(&g_rpc_mtx);
        if (g_addr_appinst_install == 0) {
            if (out_err_code) *out_err_code = 0xE0000002u; /* SYMBOL_MISSING */
            return -1;
        }
    }

    /* Bounds-check string lengths so the local buffer below can
     * never overflow APPINST_SCRATCH_SIZE. The url field is the
     * only realistic-large input; cap it at 2 KiB which fits any
     * sensible real-world URL with margin. */
    const size_t MAX_URL  = 2048;
    const size_t MAX_TITLE = 512;
    const size_t MAX_CID  = APPINST_CONTENTID_SIZE - 1;
    size_t url_len   = strnlen(url, MAX_URL + 1);
    size_t title_len = title ? strnlen(title, MAX_TITLE + 1) : 0;
    size_t cid_len   = content_id ? strnlen(content_id, MAX_CID + 1) : 0;
    if (url_len > MAX_URL || title_len > MAX_TITLE || cid_len > MAX_CID) {
        if (out_err_code) *out_err_code = 0xE0000010u;  /* arg too long */
        return -1;
    }

    pthread_mutex_lock(&g_rpc_mtx);
    if (attach_with_refresh_locked() != 0) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    intptr_t scratch = pt_mmap(g_shellui_pid, 0, APPINST_SCRATCH_SIZE,
                                PROT_READ | PROT_WRITE,
                                MAP_ANON | MAP_PRIVATE, -1, 0);
    if (scratch == -1 || scratch == 0) {
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    /* Build the scratch image in our local memory, then copy in.
     * Strings are concatenated tightly after the playgo struct;
     * MetaInfo pointers are set to absolute target addresses. */
    static char local_buf[APPINST_SCRATCH_SIZE];
    memset(local_buf, 0, sizeof(local_buf));

    appinst_meta_t *meta = (appinst_meta_t *)(local_buf + APPINST_OFF_META);
    /* pkg_info kept zeroed via the memset above. The 2.2.53-fix-round-2
     * attempt to seed pkg_info.content_id (matching the in-process
     * appinst path) regressed real installs to 0x80B21106 — confirmed
     * by direct curl test against the user's 41 MB Store pkg: empty
     * pkg_info.content_id → err_code=0 (Sony accepted), seeded
     * pkg_info.content_id → 0x80B21106 (Sony rejected). Sony's
     * shellui-resident installer apparently treats pkg_info.content_id
     * as OUTPUT after all; the in-process path may work despite the
     * seed because of a different code branch inside Sony's lib.
     * Either way, leaving it zeroed is what works for shellui-rpc. */

    /* Compose strings at a single sliding offset into local_buf,
     * starting after PlayGoInfo. Track the offset so each new
     * pointer lands at the right absolute address in the target. */
    size_t off = APPINST_OFF_STRINGS;
    intptr_t str_uri = scratch + (intptr_t)off;
    memcpy(local_buf + off, url, url_len);
    local_buf[off + url_len] = '\0';
    off += url_len + 1;

    intptr_t str_ex_uri = scratch + (intptr_t)off;
    local_buf[off++] = '\0';

    intptr_t str_scen = scratch + (intptr_t)off;
    local_buf[off++] = '\0';

    intptr_t str_cid_in = scratch + (intptr_t)off;
    if (cid_len > 0) {
        memcpy(local_buf + off, content_id, cid_len);
        off += cid_len;
    }
    local_buf[off++] = '\0';

    intptr_t str_name = scratch + (intptr_t)off;
    if (title_len > 0) {
        memcpy(local_buf + off, title, title_len);
        off += title_len;
    } else {
        /* Empty title would flicker the PS5's install notification
         * with a blank line. Use "ps5upload" as a fixed fallback so
         * the source is obvious in the user's Settings →
         * Notifications. */
        const char *fallback = "ps5upload";
        size_t fl = strlen(fallback);
        memcpy(local_buf + off, fallback, fl);
        off += fl;
    }
    local_buf[off++] = '\0';

    intptr_t str_icon = scratch + (intptr_t)off;
    local_buf[off++] = '\0';

    /* Defensive overflow check — we sized local_buf to fit the
     * input bounds, so this is belt-and-suspenders, not a hot
     * path. */
    if (off > APPINST_SCRATCH_SIZE) {
        (void)pt_munmap(g_shellui_pid, scratch, APPINST_SCRATCH_SIZE);
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        if (out_err_code) *out_err_code = 0xE0000011u;  /* layout overflow */
        return -1;
    }

    /* Wire MetaInfo pointer fields. After this the local_buf is a
     * complete, ready-to-copy image of what Sony's installer will
     * dereference once it's in ShellUI's address space. */
    meta->uri                = str_uri;
    meta->ex_uri             = str_ex_uri;
    meta->playgo_scenario_id = str_scen;
    meta->content_id         = str_cid_in;
    meta->content_name       = str_name;
    meta->icon_url           = str_icon;

    /* Push the entire image to ShellUI's scratch in one shot. */
    if (pt_copyin(g_shellui_pid, local_buf, scratch,
                  APPINST_SCRATCH_SIZE) != 0) {
        (void)pt_munmap(g_shellui_pid, scratch, APPINST_SCRATCH_SIZE);
        (void)pt_detach_tracked(g_shellui_pid, 0);
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    /* DO NOT call sceAppInstUtilTerminate here — direct testing showed
     * it hangs ShellUI within seconds (PS5 mgmt port stops responding,
     * full reboot required). Terminate is too aggressive for an
     * already-running ShellUI process: it tears down state that
     * ShellUI is using for its own UI rendering, not just the
     * installer queue. The wedged-Sony-queue problem stays an
     * open issue (cleared by PS5 reboot only). */

    /* Init AppInstUtil first (idempotent). The "already initialised"
     * code 0x80990001 is benign — Sony's daemon may have init'd it
     * earlier when the user opened Settings → Debug Settings.
     * Failure here is non-fatal: InstallByPackage may still work
     * if Sony's lazy-init kicks in on the install call. */
    if (g_addr_appinst_init != 0) {
        long init_rc = pt_call(g_shellui_pid, g_addr_appinst_init,
                               0, 0, 0, 0, 0, 0);
        (void)init_rc;
    }

    /* Pre-install cancel: clear any stuck same-content_id task from
     * Sony's installer queue. Without this, repeated install attempts
     * pile up duplicates that wedge the PS5 (network stack stops
     * responding after ~7 retries — observed during 2.2.53-fix-round-3
     * testing). The cancel call is best-effort; we ignore the rc
     * because (a) "no such task" is a normal/benign case and (b) the
     * call needs the content_id pointer in ShellUI's address space,
     * so we use the same str_cid_in scratch slot we built above. */
    /* Single best-effort cancel. Multi-cancel was tested in
     * 2.2.54-fix-round-13 and made things worse: the extra pt_calls
     * stress ShellUI's process and increase the chance of waitpid-
     * race failures on the subsequent install call. One cancel
     * gives us a chance to clear an obvious duplicate without
     * piling on more ptrace activity. */
    if (g_addr_appinst_cancel != 0 && cid_len > 0) {
        long cancel_rc = pt_call(g_shellui_pid, g_addr_appinst_cancel,
                                  (uint64_t)str_cid_in, 0, 0, 0, 0, 0);
        (void)cancel_rc;
    }

    /* The actual install call. Three arg pointers all point into
     * the scratch buffer we just pushed. */
    long install_rc = pt_call(
        g_shellui_pid, g_addr_appinst_install,
        (uint64_t)(scratch + APPINST_OFF_META),
        (uint64_t)(scratch + APPINST_OFF_PKGINFO),
        (uint64_t)(scratch + APPINST_OFF_PLAYGO),
        0, 0, 0);
    int dispatched = pt_call_was_dispatched();

    (void)pt_munmap(g_shellui_pid, scratch, APPINST_SCRATCH_SIZE);
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);

    if (install_rc == 0) {
        if (out_err_code) *out_err_code = 0;
        return 0;
    }
    /* 2.2.54-fix-round-8: soft-success on dispatched-but-rax-unreadable.
     * Sony's installer call inside ShellUI signals the process in a
     * way that sometimes races our waitpid (same pattern as
     * sceLncUtilLaunchApp documented in shellui_rpc_launch_app's
     * rc=-2 branch). When pt_call_was_dispatched() returns true but
     * install_rc came back as -1, the install request DID make it
     * into Sony's installer queue — Sony processed the call, just
     * the post-call cleanup failed. Treating this as a hard failure
     * would cause the bgft.c fall-through to in-process appinst,
     * which then collides with Sony's already-queued task and
     * returns 0x80B21106. Treat as success so the caller takes the
     * shellui-rpc accept branch instead. */
    if (dispatched && install_rc == -1) {
        if (out_err_code) *out_err_code = 0;
        return 0;
    }
    /* Non-zero rax — Sony's install error code. Surface verbatim
     * so the host humanizer can map it (0x80A2FFxx for AppInstaller
     * errors, 0x80B22xxx for PlayGo, etc.). */
    if (out_err_code) *out_err_code = (uint32_t)install_rc;
    return 1;
}
