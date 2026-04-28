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

int shellui_rpc_init(void) {
    pthread_mutex_lock(&g_rpc_mtx);
    if (g_inited) {
        pthread_mutex_unlock(&g_rpc_mtx);
        return g_init_rc;
    }
    g_inited = 1;

    g_shellui_pid = find_pid_by_name("SceShellUI");
    if (g_shellui_pid <= 0) {
        g_init_rc = -1;
        pthread_mutex_unlock(&g_rpc_mtx);
        return -1;
    }

    /* Resolve every Sony API address inside SceShellUI. We don't
     * fail the whole init if a single sensor symbol is missing —
     * the corresponding RPC just returns -1. Only sceLncUtilLaunchApp
     * is required for the init-success contract; without launch we
     * can't justify the cost of attaching to ShellUI at all. */
    g_addr_user_get_fg = resolve_in_target(g_shellui_pid, "sceUserServiceGetForegroundUser");
    g_addr_lnc_launch  = resolve_in_target(g_shellui_pid, "sceLncUtilLaunchApp");
    g_addr_get_cpu_temp  = resolve_in_target(g_shellui_pid, "sceKernelGetCpuTemperature");
    g_addr_get_soc_temp  = resolve_in_target(g_shellui_pid, "sceKernelGetSocSensorTemperature");
    g_addr_get_cpu_freq  = resolve_in_target(g_shellui_pid, "sceKernelGetCpuFrequency");
    g_addr_get_soc_power = resolve_in_target(g_shellui_pid, "sceKernelGetSocPowerConsumption");

    if (g_addr_lnc_launch == 0) {
        g_init_rc = -2;
    } else {
        g_init_rc = 0;
    }
    pthread_mutex_unlock(&g_rpc_mtx);
    return g_init_rc;
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

/* Attach + run a single function call inside SceShellUI + detach.
 * Acquires g_rpc_mtx for the duration so concurrent callers from
 * different mgmt threads don't both try to ptrace. The mutex
 * release at the end guarantees ShellUI is detached before the
 * next caller can attach.
 *
 * `*ok_out` (if non-NULL) is set to 1 on a clean attach + call +
 * detach, 0 on any failure. Distinguishes a -1 rax (function
 * legitimately returned -1) from a -1 we'd return on RPC failure. */
static long do_remote_call(intptr_t addr, uint64_t a0, uint64_t a1,
                            uint64_t a2, uint64_t a3, uint64_t a4,
                            uint64_t a5, int *ok_out) {
    if (ok_out) *ok_out = 0;
    if (addr == 0 || g_shellui_pid <= 0) return -1;
    pthread_mutex_lock(&g_rpc_mtx);
    if (pt_attach_tracked(g_shellui_pid) != 0) {
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
    if (pt_attach_tracked(g_shellui_pid) != 0) {
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
    /* Allocate a 4-byte scratch buffer in ShellUI for the int*
     * out param. Round to a page since pt_mmap takes page-sized
     * inputs. */
    pthread_mutex_lock(&g_rpc_mtx);
    int err_attach = pt_attach_tracked(g_shellui_pid);
    if (err_attach != 0) {
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
    long rc = pt_call(g_shellui_pid, g_addr_user_get_fg,
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

int shellui_rpc_launch_app(const char *title_id) {
    if (!title_id || !shellui_rpc_ready()) return -1;

    /* Get foreground user via a separate RPC (ShellUI is the
     * authoritative owner of foreground-user state too). Fall
     * back to user 0 if that fails — some firmwares accept it. */
    int user_id = 0;
    (void)remote_get_foreground_user(&user_id);

    /* Allocate scratch buffers inside ShellUI for:
     *   - title_id string  (10 bytes + NUL)
     *   - LncAppParam      (24-byte struct, layout below) */
    pthread_mutex_lock(&g_rpc_mtx);
    if (pt_attach_tracked(g_shellui_pid) != 0) {
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
    (void)pt_copyin(g_shellui_pid, tid_buf, scratch, sizeof(tid_buf));

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
    (void)pt_copyin(g_shellui_pid, &param, param_addr, sizeof(param));

    /* Call sceLncUtilLaunchApp(title_id, NULL, &param) inside ShellUI. */
    long rc = pt_call(g_shellui_pid, g_addr_lnc_launch,
                       (uint64_t)scratch, 0, (uint64_t)param_addr, 0, 0, 0);
    (void)pt_munmap(g_shellui_pid, scratch, 0x1000);
    (void)pt_detach_tracked(g_shellui_pid, 0);
    pthread_mutex_unlock(&g_rpc_mtx);
    return (int)rc;
}

int shellui_rpc_get_cpu_temp(int *out_celsius) {
    /* sceKernelGetCpuTemperature(int *out) — single int-out arg. */
    return rpc_call_with_int_scratch(g_addr_get_cpu_temp, 0, 1,
                                     out_celsius, sizeof(*out_celsius));
}

int shellui_rpc_get_soc_temp(int *out_celsius) {
    /* sceKernelGetSocSensorTemperature(int sensor_id, int *out).
     * Sensor id 0 is the primary SoC sensor. */
    return rpc_call_with_int_scratch(g_addr_get_soc_temp, 0, 0,
                                     out_celsius, sizeof(*out_celsius));
}

int shellui_rpc_get_cpu_freq_hz(long *out_hz) {
    /* sceKernelGetCpuFrequency() returns the frequency directly in
     * rax (no out-param). do_remote_call's `ok` flag distinguishes
     * "RPC failed" (ok=0, rc=-1) from "function legitimately
     * returned -1" — for this API any non-positive return is
     * treated as failure since CPU frequency is always positive. */
    if (g_addr_get_cpu_freq == 0 || !shellui_rpc_ready() || !out_hz) return -1;
    int ok = 0;
    long rc = do_remote_call(g_addr_get_cpu_freq, 0, 0, 0, 0, 0, 0, &ok);
    if (!ok || rc <= 0) return -1;
    *out_hz = rc;
    return 0;
}

int shellui_rpc_get_soc_power_mw(uint32_t *out_mw) {
    /* sceKernelGetSocPowerConsumption(uint32_t *out). */
    return rpc_call_with_int_scratch(g_addr_get_soc_power, 0, 1,
                                     out_mw, sizeof(*out_mw));
}
