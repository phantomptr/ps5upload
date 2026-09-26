/*
 * ptrace_remote.c — remote-process ptrace primitives + remote-call
 * thunk used by the ShellUI RPC layer.
 *
 * Surface:
 *   pt_attach / pt_detach / pt_step / pt_continue
 *   pt_getregs / pt_setregs
 *   pt_copyin / pt_copyout
 *   pt_resolve  (kernel_dynlib_resolve wrapper)
 *   pt_call     (remote function call via int3 return-trap)
 *   pt_syscall  (remote syscall via the `syscall` instruction in libkernel)
 *   pt_mmap / pt_munmap
 *
 * Each `sys_ptrace` call elevates this process's authid to
 * 0x4800000000010003 for the duration of the syscall (the only
 * authid the kernel's PT_* permission check accepts) and restores
 * the prior debugger authid on the way out.
 */

#include <errno.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include <sys/ptrace.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <sys/mman.h>

#include <ps5/kernel.h>

#include "ptrace_remote.h"
#include "ptrace_recovery.h"
#include "kernel_rw_lock.h"

/* Ptrace-allowed authid. Different from the debugger authid we set
 * at boot (0x4800000000000006): that one lets us read/write kernel
 * memory but the kernel's ptrace permission check rejects it. The
 * 0x4800000000010003 authid is what Sony's `SceTracer` services
 * have, and it's the one that satisfies the ptrace permission. We
 * swap to it ONLY for the `syscall(SYS_ptrace, ...)` call itself
 * and swap back so debugger authid stays in place for everything
 * else. */
#define PS5_PTRACE_ALLOWED_AUTHID 0x4800000000010003ll

/* How long we are willing to wait for a traced process to report a stop.
 *
 * THIS BOUND IS A CONSOLE-SAFETY MECHANISM, NOT A PERFORMANCE TUNABLE.
 * Every waitpid() here runs while SceShellUI is PT_ATTACH'd and stopped.
 * A plain blocking waitpid() that never returns leaves ShellUI stopped
 * forever — the whole console UI freezes and the only way out is holding
 * the power button. shellui_rpc_emergency_detach() does not help: it
 * runs from a fatal signal handler, and a hang raises no signal.
 *
 * That is not hypothetical. A user on FW 12.40 reported exactly this
 * ("froze my console, had to shut it down"), and FW 12.x is precisely
 * where our symbol resolution is known to be incomplete (see the
 * "add FW-12.x renamed export here" TODOs in register.c) — i.e. where
 * a remote call is most likely to never produce the stop we're waiting
 * for.
 *
 * 10 s is far beyond any healthy stop (they land in microseconds) while
 * still bounding the worst case to an annoyance rather than a reboot. */
#define PT_WAIT_TIMEOUT_MS   10000
#define PT_WAIT_POLL_US      1000

/* PID whose injected context could not be restored. This must be tracee-keyed:
 * the cheats watcher ptraces a game process independently of ShellUI, and a
 * process-global boolean could make one thread skip the other tracee's detach.
 * Atomic access keeps those independent threads from racing on the marker. */
static _Atomic pid_t g_pt_tracee_lost_pid = 0;

static int tracee_is_lost(pid_t pid) {
    return ptrace_lost_pid_matches(atomic_load(&g_pt_tracee_lost_pid), pid);
}

static void mark_tracee_lost(pid_t pid) {
    if (pid > 0) atomic_store(&g_pt_tracee_lost_pid, pid);
}

/*
 * Bounded replacement for `waitpid(pid, status, 0)`.
 *
 * Returns 0 when the stop was reported, -1 on error or timeout (with
 * errno set to ETIMEDOUT on timeout so callers can tell the two apart).
 * Polls with WNOHANG rather than blocking so we can give up.
 */
static int pt_waitpid_bounded(pid_t pid, int *status) {
    int elapsed_us = 0;
    const int limit_us = PT_WAIT_TIMEOUT_MS * 1000;

    for (;;) {
        int st = 0;
        pid_t r = waitpid(pid, &st, WNOHANG);
        if (r == pid) {
            if (status) *status = st;
            return 0;
        }
        if (r == -1) {
            if (errno == EINTR) continue; /* our own signal handler */
            return -1;
        }
        /* r == 0: not stopped yet. */
        if (elapsed_us >= limit_us) {
            errno = ETIMEDOUT;
            return -1;
        }
        usleep(PT_WAIT_POLL_US);
        elapsed_us += PT_WAIT_POLL_US;
    }
}

static int sys_ptrace(int request, pid_t pid, caddr_t addr, int data) {
    pid_t mypid = getpid();
    uint64_t saved_authid;
    int ret;

    /* The authid swap below mutates the PROCESS-GLOBAL ucred and drives the
     * shared kernel-RW window. Hold kernel_rw_lock across the entire
     * save -> swap -> SYS_ptrace -> restore window so it can't run
     * concurrently with a bgft/register install-authid swap, a boot elevation,
     * or another ptrace swap. Innermost lock: sys_ptrace runs with the caller
     * holding g_rpc_mtx, and kernel_rw_lock is always taken AFTER it, never
     * before — so no lock-order cycle can form. */
    pthread_mutex_lock(&kernel_rw_lock);
    saved_authid = kernel_get_ucred_authid(mypid);
    if (saved_authid == 0) {
        /* Kernel R/W not available — kstuff isn't loaded or our
         * process can't read its own ucred. No swap to serialize: drop the
         * lock and just attempt the syscall; the kernel will reject and the
         * caller will get the underlying error. */
        pthread_mutex_unlock(&kernel_rw_lock);
        return (int)syscall(SYS_ptrace, request, pid, addr, data);
    }

    if (kernel_set_ucred_authid(mypid, PS5_PTRACE_ALLOWED_AUTHID) != 0) {
        /* If we can't elevate, attempt the syscall anyway with our
         * existing authid; not great but better than blanket fail. The swap
         * didn't take, so authid is unchanged — nothing to restore. */
        pthread_mutex_unlock(&kernel_rw_lock);
        return (int)syscall(SYS_ptrace, request, pid, addr, data);
    }

    ret = (int)syscall(SYS_ptrace, request, pid, addr, data);

    /* Restore. If this fails the process keeps the elevated authid
     * which is bad for the rest of our code (we deliberately set
     * the debugger authid). Log and move on — the next pt_*
     * call will swap again, papering over the gap.
     *
     * Pre-2.2.28 the comment said "log and move on" but the actual
     * code silently `(void)`-cast the return value. A persistent
     * restore failure would leak elevated authid into between-RPC
     * code paths with no diagnostic. Now we log loudly on the FIRST
     * failure — subsequent failures within the same boot are
     * suppressed so this hot path doesn't flood logs on a permanent
     * kernel-RW outage. The user-visible signal "ptrace restore
     * leaked once" is enough to debug; the actual gap is closed by
     * the next pt_* call's elevation swap. */
    if (kernel_set_ucred_authid(mypid, saved_authid) != 0) {
        static int reported = 0;
        if (!reported) {
            reported = 1;
            fprintf(stderr,
                    "[ptrace_remote] WARN: failed to restore authid 0x%llx after "
                    "ptrace request=%d (process retains PS5_PTRACE_ALLOWED_AUTHID "
                    "until next pt_* call). Subsequent occurrences suppressed.\n",
                    (unsigned long long)saved_authid, request);
        }
    }

    pthread_mutex_unlock(&kernel_rw_lock);
    return ret;
}

typedef struct timeout_recovery_ctx {
    pid_t pid;
    const struct reg *saved_regs;
} timeout_recovery_ctx_t;

static int recovery_request_stop(void *opaque) {
    timeout_recovery_ctx_t *ctx = (timeout_recovery_ctx_t *)opaque;
    return kill(ctx->pid, SIGSTOP) == 0 ? 0 : -1;
}

static int recovery_wait_stopped(void *opaque) {
    timeout_recovery_ctx_t *ctx = (timeout_recovery_ctx_t *)opaque;
    int status = 0;
    if (pt_waitpid_bounded(ctx->pid, &status) < 0) return -1;
    return WIFSTOPPED(status) ? 0 : -1;
}

static int recovery_restore_registers(void *opaque) {
    timeout_recovery_ctx_t *ctx = (timeout_recovery_ctx_t *)opaque;
    if (!ctx->saved_regs) return 0;
    return pt_setregs(ctx->pid, ctx->saved_regs) == 0 ? 0 : -1;
}

static int recovery_terminate(void *opaque) {
    timeout_recovery_ctx_t *ctx = (timeout_recovery_ctx_t *)opaque;
    /* A tracee whose injected context cannot be restored must never be
     * detached/resumed. ShellUI is supervised by the OS and can respawn; a
     * process running with our fake stack/return address can freeze the whole
     * console. */
    if (kill(ctx->pid, SIGKILL) == 0) return 0;
    /* A traced process can reject the ordinary signal path on some firmware.
     * PT_KILL is the last fail-closed option while we still own the tracee. */
    return sys_ptrace(PT_KILL, ctx->pid, 0, 0) == 0 ? 0 : -1;
}

/* Stop a running remote call, wait until ptrace confirms the stop, then restore
 * its original registers. If any link in that chain fails, terminate the
 * damaged tracee instead of detaching injected state back into execution. */
static int recover_after_remote_timeout(pid_t pid, const struct reg *saved_regs) {
    timeout_recovery_ctx_t ctx = { pid, saved_regs };
    ptrace_recovery_ops_t ops = {
        recovery_request_stop,
        recovery_wait_stopped,
        saved_regs ? recovery_restore_registers : NULL,
        recovery_terminate,
    };
    ptrace_recovery_result_t result = ptrace_recover_timed_out_tracee(&ops, &ctx);
    if (result == PTRACE_RECOVERY_RESTORED) {
        fprintf(stderr,
                "[ptrace_remote] remote call timed out; tracee %d stopped and registers restored\n",
                pid);
        return 0;
    }
    mark_tracee_lost(pid);
    fprintf(stderr,
            "[ptrace_remote] CRITICAL: tracee %d could not be safely restored; "
            "termination %s\n",
            pid,
            result == PTRACE_RECOVERY_TERMINATED ? "requested" : "FAILED");
    return -1;
}

/* The target is already stopped. Register restore must succeed before detach;
 * otherwise kill it so it cannot resume at the injected instruction/stack. */
static int restore_stopped_or_terminate(pid_t pid, const struct reg *saved_regs) {
    if (pt_setregs(pid, saved_regs) == 0) return 0;
    timeout_recovery_ctx_t ctx = { pid, saved_regs };
    (void)recovery_terminate(&ctx);
    mark_tracee_lost(pid);
    fprintf(stderr,
            "[ptrace_remote] CRITICAL: register restore failed for tracee %d; "
            "termination requested\n",
            pid);
    return -1;
}

intptr_t pt_resolve(pid_t pid, const char *nid) {
    intptr_t addr;
    /* libkernel handle = 0x1; libkernel_sys handle = 0x2001. Most
     * APIs we'd resolve live in one or the other. Try libkernel
     * first because that's where sceLncUtilLaunchApp,
     * sceKernelGetCpuTemperature etc. live on PS5. */
    if ((addr = kernel_dynlib_resolve(pid, 0x1, nid)) != 0) {
        return addr;
    }
    return kernel_dynlib_resolve(pid, 0x2001, nid);
}

int pt_attach(pid_t pid) {
    /* Never reattach a pid whose injected state could not be recovered. A
     * freshly respawned ShellUI has a different pid and starts normally. */
    if (tracee_is_lost(pid)) {
        errno = ESRCH;
        return -1;
    }
    if (sys_ptrace(PT_ATTACH, pid, 0, 0) == -1) {
        return -1;
    }
    /* Block until the child reports the SIGSTOP. Without this the
     * subsequent PT_GETREGS races the kernel and returns ESRCH. */
    if (pt_waitpid_bounded(pid, 0) == -1) {
        /* PT_ATTACH already succeeded: we are still the tracer and the
         * target is stopped. Returning without detaching leaks a frozen
         * SceShellUI and leaves the next pt_attach EBUSY under our own
         * tracer, so detach unconditionally. This now also covers the
         * timeout case — a target that never reports its stop used to
         * park us in a blocking waitpid() forever with ShellUI held
         * stopped, which is a frozen console. Best-effort: if detach
         * also fails there is nothing left to try. */
        int saved_errno = errno;
        if (recover_after_remote_timeout(pid, NULL) == 0) {
            (void)pt_detach(pid, 0);
        }
        errno = saved_errno;
        return -1;
    }
    return 0;
}

int pt_detach(pid_t pid, int sig) {
    if (tracee_is_lost(pid)) {
        errno = ESRCH;
        return -1;
    }
    return (sys_ptrace(PT_DETACH, pid, 0, sig) == -1) ? -1 : 0;
}

int pt_step(pid_t pid) {
    if (sys_ptrace(PT_STEP, pid, (caddr_t)1, 0) != 0) {
        return -1;
    }
    /* Bounded: a single-step that never reports back would otherwise
     * hold ShellUI stopped indefinitely. The caller unwinds and the
     * RPC layer's detach runs. */
    if (pt_waitpid_bounded(pid, 0) < 0) {
        int saved_errno = errno;
        (void)recover_after_remote_timeout(pid, NULL);
        errno = saved_errno;
        return -1;
    }
    return 0;
}

int pt_continue(pid_t pid, int sig) {
    return (sys_ptrace(PT_CONTINUE, pid, (caddr_t)1, sig) == -1) ? -1 : 0;
}

int pt_getregs(pid_t pid, struct reg *r) {
    return sys_ptrace(PT_GETREGS, pid, (caddr_t)r, 0);
}

int pt_setregs(pid_t pid, const struct reg *r) {
    return sys_ptrace(PT_SETREGS, pid, (caddr_t)r, 0);
}

int pt_copyin(pid_t pid, const void *buf, intptr_t addr, size_t len) {
    struct ptrace_io_desc iod = {
        .piod_op   = PIOD_WRITE_D,
        .piod_offs = (void *)addr,
        .piod_addr = (void *)buf,
        .piod_len  = len,
    };
    return sys_ptrace(PT_IO, pid, (caddr_t)&iod, 0);
}

int pt_copyout(pid_t pid, intptr_t addr, void *buf, size_t len) {
    struct ptrace_io_desc iod = {
        .piod_op   = PIOD_READ_D,
        .piod_offs = (void *)addr,
        .piod_addr = buf,
        .piod_len  = len,
    };
    return sys_ptrace(PT_IO, pid, (caddr_t)&iod, 0);
}

/* Remote function call: set rip/rdi/rsi/rdx/rcx/r8/r9 in the
 * target, push a "saved return" onto its stack pointing at a
 * trap byte, PT_CONTINUE, waitpid catches the trap, restore
 * pre-call regs. Returns the function's rax. -1 on ptrace
 * failure.
 *
 * Why a return-trap rather than single-stepping: single-stepping
 * a multi-second Sony API call (e.g. sceLncUtilLaunchApp) costs
 * O(seconds × ptrace_round_trip) and easily times out before the
 * function returns. The breakpoint pattern lets the target run
 * at full native speed and only stops when the function's `ret`
 * lands on our trap address.
 *
 * Trap location and signal: we point the saved-return at a stack
 * address `bak_rsp - 8`. After the function's ret, rip = that
 * address. PS5 stack mappings are not executable, so the CPU
 * fetches from a non-X page and the kernel raises a fault (the
 * function has already returned at that point, so rax is set).
 * waitpid catches the resulting stopped state and we read rax.
 * We treat any stopped state as success — rax holds the function
 * return value regardless of which trap fired. The ret_target
 * memory at bak_rsp-8 is below the function's pre-call rsp;
 * writing it doesn't disturb anything live, and after we restore
 * regs to bak_reg the slot becomes "below rsp" again. */
/* Plain static (not __thread): the SDK's emutls implementation
 * prevents the binary from loading on PS5 firmware (rtld lib_init
 * fails when emutls symbols are present, even before main runs).
 * Acceptable here because pt_call is invoked under sony_api_lock —
 * only one ptrace conversation runs at a time, so this flag's
 * read/write are already serialized.
 *
 * 1 means "remote function was actually dispatched (pt_continue
 * returned 0)" — even if a later cleanup step (waitpid / getregs)
 * failed and pt_call returned -1, the call itself made it into the
 * target. Read via pt_call_was_dispatched(). */
static int g_pt_call_dispatched = 0;
static int g_pt_call_timed_out = 0;

int pt_call_was_dispatched(void) {
    return g_pt_call_dispatched;
}

int pt_call_timed_out(void) {
    return g_pt_call_timed_out;
}

int pt_tracee_was_lost(pid_t pid) {
    return tracee_is_lost(pid);
}

long pt_call(pid_t pid, intptr_t addr, ...) {
    struct reg jmp_reg;
    struct reg bak_reg;
    va_list ap;

    g_pt_call_dispatched = 0;
    g_pt_call_timed_out = 0;
    if (tracee_is_lost(pid)) {
        errno = ESRCH;
        return -1;
    }
    if (pt_getregs(pid, &bak_reg) != 0) return -1;

    memcpy(&jmp_reg, &bak_reg, sizeof(jmp_reg));
    jmp_reg.r_rip = addr;

    va_start(ap, addr);
    jmp_reg.r_rdi = va_arg(ap, uint64_t);
    jmp_reg.r_rsi = va_arg(ap, uint64_t);
    jmp_reg.r_rdx = va_arg(ap, uint64_t);
    jmp_reg.r_rcx = va_arg(ap, uint64_t);
    jmp_reg.r_r8  = va_arg(ap, uint64_t);
    jmp_reg.r_r9  = va_arg(ap, uint64_t);
    va_end(ap);

    /* Stack ret-slot. Setting jmp_reg.r_rsp = ret_slot makes the
     * callee see a normal "8-byte saved-return area immediately
     * above me" view. Its `ret` will pop our value into rip. */
    intptr_t ret_slot = (intptr_t)bak_reg.r_rsp - 8;
    uint64_t ret_target = (uint64_t)ret_slot;
    if (pt_copyin(pid, &ret_target, ret_slot,
                  sizeof(ret_target)) != 0) {
        return -1;
    }
    jmp_reg.r_rsp = ret_slot;
    if (pt_setregs(pid, &jmp_reg) != 0) {
        /* Best-effort restore of the original registers in case the
         * failed PT_SETREGS partially applied — keeps this path
         * consistent with every other failure branch below, which all
         * restore bak_reg before returning. */
        (void)restore_stopped_or_terminate(pid, &bak_reg);
        return -1;
    }

    if (pt_continue(pid, 0) != 0) {
        (void)restore_stopped_or_terminate(pid, &bak_reg);
        return -1;
    }
    /* Past this point the remote function call has been dispatched
     * into the target. Even if waitpid / getregs below fail (e.g.
     * sceLncUtilLaunchApp causes ShellUI to be signalled in a way
     * that races our waitpid), the call itself ran. */
    g_pt_call_dispatched = 1;
    int wstatus = 0;
    /* Bounded. This is the riskiest of the three waits: the remote
     * function is already running inside ShellUI, so if it blocks or
     * never traps back (a mis-resolved address on an untested firmware
     * is the obvious way), a blocking waitpid() would hold ShellUI
     * stopped forever — i.e. freeze the console. On timeout we restore
     * the saved registers and unwind so the RPC layer's detach runs and
     * ShellUI resumes, even though the call's outcome is unknown. */
    if (pt_waitpid_bounded(pid, &wstatus) < 0) {
        int saved_errno = errno;
        if (saved_errno == ETIMEDOUT) g_pt_call_timed_out = 1;
        (void)recover_after_remote_timeout(pid, &bak_reg);
        errno = saved_errno;
        return -1;
    }
    if (!WIFSTOPPED(wstatus)) {
        /* Exited/signalled targets have no registers to restore and must not be
         * detached using a potentially-reused cached pid. */
        mark_tracee_lost(pid);
        return -1;
    }

    long rax = -1;
    if (pt_getregs(pid, &jmp_reg) == 0) rax = jmp_reg.r_rax;
    if (restore_stopped_or_terminate(pid, &bak_reg) != 0) return -1;
    return rax;
}

long pt_syscall(pid_t pid, int sysno, ...) {
    struct reg jmp_reg;
    struct reg bak_reg;
    va_list ap;

    if (tracee_is_lost(pid)) {
        errno = ESRCH;
        return -1;
    }

    /* The `syscall` instruction lives at a known offset inside the
     * libkernel symbol with NID `HoLVWNanBBc`. Adding 0xa lands on
     * the raw `syscall` opcode (skipping the prologue). Syscalls
     * return promptly so single-stepping is fine here; we use the
     * simple step-until-rsp-moves pattern. */
    intptr_t addr = pt_resolve(pid, "HoLVWNanBBc");
    if (addr == 0) return -1;
    addr += 0xa;

    if (pt_getregs(pid, &bak_reg) != 0) return -1;

    memcpy(&jmp_reg, &bak_reg, sizeof(jmp_reg));
    jmp_reg.r_rip = addr;
    jmp_reg.r_rax = sysno;

    /* FreeBSD syscall ABI: rdi/rsi/rdx/r10/r8/r9 (note r10 not rcx). */
    va_start(ap, sysno);
    jmp_reg.r_rdi = va_arg(ap, uint64_t);
    jmp_reg.r_rsi = va_arg(ap, uint64_t);
    jmp_reg.r_rdx = va_arg(ap, uint64_t);
    jmp_reg.r_r10 = va_arg(ap, uint64_t);
    jmp_reg.r_r8  = va_arg(ap, uint64_t);
    jmp_reg.r_r9  = va_arg(ap, uint64_t);
    va_end(ap);

    if (pt_setregs(pid, &jmp_reg) != 0) {
        /* Mirror pt_call: best-effort restore of the original regs in
         * case the failed PT_SETREGS partially applied, and so a later
         * PT_DETACH doesn't resume the target at the syscall site with
         * junk argument registers. */
        (void)restore_stopped_or_terminate(pid, &bak_reg);
        return -1;
    }

    /* Single-step until we land back on the syscall site after
     * sysret. Bound to 1k steps because syscalls are fast. */
    int step_budget = 1000;
    while (jmp_reg.r_rsp <= bak_reg.r_rsp && step_budget > 0) {
        if (pt_step(pid) != 0) {
            if (!tracee_is_lost(pid)) {
                (void)restore_stopped_or_terminate(pid, &bak_reg);
            }
            return -1;
        }
        if (pt_getregs(pid, &jmp_reg) != 0) {
            (void)restore_stopped_or_terminate(pid, &bak_reg);
            return -1;
        }
        step_budget--;
    }
    if (step_budget == 0) {
        (void)restore_stopped_or_terminate(pid, &bak_reg);
        return -1;
    }

    if (restore_stopped_or_terminate(pid, &bak_reg) != 0) return -1;
    return jmp_reg.r_rax;
}

intptr_t pt_mmap(pid_t pid, intptr_t addr, size_t len, int prot,
                  int flags, int fd, off_t off) {
    long r = pt_syscall(pid, SYS_mmap, addr, len, prot, flags, fd, off);
    /* On syscall failure the kernel writes -errno into rax (e.g.
     * -ENOMEM under memory pressure, -EINVAL, -EACCES). Every caller
     * checks `scratch == -1 || scratch == 0`, so a -12 (0xFFFFFFFFFFFFFFF4)
     * sails past as a "valid pointer" and the next pt_copyin/pt_call
     * dereferences it inside SceShellUI → ShellUI crash, and Sony's
     * watchdog can cascade into our payload. Collapse the whole errno
     * range here so every caller is fixed in one place. Valid mmap
     * results are page-aligned and >= 0x1000, never in [-4095,-1]. */
    if ((unsigned long)r >= (unsigned long)-4095) return -1;
    return r;
}

int pt_munmap(pid_t pid, intptr_t addr, size_t len) {
    return (int)pt_syscall(pid, SYS_munmap, addr, len);
}
