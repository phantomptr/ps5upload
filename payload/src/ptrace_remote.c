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

/* Ptrace-allowed authid. Different from the debugger authid we set
 * at boot (0x4800000000000006): that one lets us read/write kernel
 * memory but the kernel's ptrace permission check rejects it. The
 * 0x4800000000010003 authid is what Sony's `SceTracer` services
 * have, and it's the one that satisfies the ptrace permission. We
 * swap to it ONLY for the `syscall(SYS_ptrace, ...)` call itself
 * and swap back so debugger authid stays in place for everything
 * else. */
#define PS5_PTRACE_ALLOWED_AUTHID 0x4800000000010003ll

static int sys_ptrace(int request, pid_t pid, caddr_t addr, int data) {
    pid_t mypid = getpid();
    uint64_t saved_authid;
    int ret;

    saved_authid = kernel_get_ucred_authid(mypid);
    if (saved_authid == 0) {
        /* Kernel R/W not available — kstuff isn't loaded or our
         * process can't read its own ucred. Skip the elevation
         * dance and just attempt the syscall; the kernel will
         * reject and the caller will get the underlying error. */
        return (int)syscall(SYS_ptrace, request, pid, addr, data);
    }

    if (kernel_set_ucred_authid(mypid, PS5_PTRACE_ALLOWED_AUTHID) != 0) {
        /* If we can't elevate, attempt the syscall anyway with our
         * existing authid; not great but better than blanket fail. */
        return (int)syscall(SYS_ptrace, request, pid, addr, data);
    }

    ret = (int)syscall(SYS_ptrace, request, pid, addr, data);

    /* Restore. If this fails the process keeps the elevated authid
     * which is bad for the rest of our code (we deliberately set
     * the debugger authid). Log and move on — the next pt_*
     * call will swap again, papering over the gap. */
    (void)kernel_set_ucred_authid(mypid, saved_authid);

    return ret;
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
    if (sys_ptrace(PT_ATTACH, pid, 0, 0) == -1) {
        return -1;
    }
    /* Block until the child reports the SIGSTOP. Without this the
     * subsequent PT_GETREGS races the kernel and returns ESRCH. */
    if (waitpid(pid, 0, 0) == -1) {
        return -1;
    }
    return 0;
}

int pt_detach(pid_t pid, int sig) {
    return (sys_ptrace(PT_DETACH, pid, 0, sig) == -1) ? -1 : 0;
}

int pt_step(pid_t pid) {
    if (sys_ptrace(PT_STEP, pid, (caddr_t)1, 0) != 0) {
        return -1;
    }
    if (waitpid(pid, 0, 0) < 0) {
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
long pt_call(pid_t pid, intptr_t addr, ...) {
    struct reg jmp_reg;
    struct reg bak_reg;
    va_list ap;

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
    if (pt_setregs(pid, &jmp_reg) != 0) return -1;

    if (pt_continue(pid, 0) != 0) {
        (void)pt_setregs(pid, &bak_reg);
        return -1;
    }
    int wstatus = 0;
    if (waitpid(pid, &wstatus, 0) < 0) {
        (void)pt_setregs(pid, &bak_reg);
        return -1;
    }
    if (!WIFSTOPPED(wstatus)) {
        (void)pt_setregs(pid, &bak_reg);
        return -1;
    }

    long rax = -1;
    if (pt_getregs(pid, &jmp_reg) == 0) rax = jmp_reg.r_rax;
    (void)pt_setregs(pid, &bak_reg);
    return rax;
}

long pt_syscall(pid_t pid, int sysno, ...) {
    struct reg jmp_reg;
    struct reg bak_reg;
    va_list ap;

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

    if (pt_setregs(pid, &jmp_reg) != 0) return -1;

    /* Single-step until we land back on the syscall site after
     * sysret. Bound to 1k steps because syscalls are fast. */
    int step_budget = 1000;
    while (jmp_reg.r_rsp <= bak_reg.r_rsp && step_budget > 0) {
        if (pt_step(pid) != 0) return -1;
        if (pt_getregs(pid, &jmp_reg) != 0) return -1;
        step_budget--;
    }
    if (step_budget == 0) {
        (void)pt_setregs(pid, &bak_reg);
        return -1;
    }

    if (pt_setregs(pid, &bak_reg) != 0) return -1;
    return jmp_reg.r_rax;
}

intptr_t pt_mmap(pid_t pid, intptr_t addr, size_t len, int prot,
                  int flags, int fd, off_t off) {
    return pt_syscall(pid, SYS_mmap, addr, len, prot, flags, fd, off);
}

int pt_munmap(pid_t pid, intptr_t addr, size_t len) {
    return (int)pt_syscall(pid, SYS_munmap, addr, len);
}
