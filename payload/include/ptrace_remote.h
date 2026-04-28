/*
 * Remote-process ptrace primitives + remote function call.
 *
 * Each ptrace syscall is wrapped to elevate this process's
 * ucred authid to 0x4800000000010003 (the only authid the
 * kernel's PT_* permission check accepts) for the duration of
 * the syscall, then restore the prior debugger authid
 * (0x4800000000000006) on the way out.
 *
 * Why the elevated wrapper instead of a bare syscall: from a
 * userland payload our authid is the debugger authid, which the
 * kernel rejects for PT_*. The 0x4800000000010003 authid is
 * accepted. We swap in for the syscall and swap back so other
 * code keeps the debugger authid that the rest of the payload
 * depends on (kernel R/W elevation, sandbox escape).
 *
 * Remote function call (`pt_call`) pattern:
 *   1. snapshot target's regs
 *   2. set rip = function_addr, rdi/rsi/.../r9 = args
 *   3. write trap_addr as the saved-return on the target's stack
 *      and overwrite the byte at trap_addr with int3 (0xCC)
 *   4. PT_CONTINUE; waitpid catches the SIGTRAP from int3
 *      (function ret pops trap_addr → rip → kernel reports trap)
 *   5. read rax, restore stack byte + pre-call regs
 *
 * Sony API addresses come from
 * `kernel_dynlib_resolve(pid, handle, NID)` which returns the
 * library-mangled-symbol's address inside the target process.
 */

#ifndef PS5UPLOAD2_PTRACE_REMOTE_H
#define PS5UPLOAD2_PTRACE_REMOTE_H

#include <stdint.h>
#include <sys/types.h>
#include <machine/reg.h>

/* Attach to a target process. Blocks via waitpid until the SIGSTOP
 * is delivered. Returns 0 on success, -1 on failure. */
int pt_attach(pid_t pid);

/* Detach with optional resume signal. */
int pt_detach(pid_t pid, int sig);

/* Single-step + continue helpers. */
int pt_step(pid_t pid);
int pt_continue(pid_t pid, int sig);

/* Register get/set wrappers around PT_GETREGS / PT_SETREGS. */
int pt_getregs(pid_t pid, struct reg *r);
int pt_setregs(pid_t pid, const struct reg *r);

/* Copy bytes between our process and the target via PT_IO. */
int pt_copyin(pid_t pid, const void *buf, intptr_t addr, size_t len);
int pt_copyout(pid_t pid, intptr_t addr, void *buf, size_t len);

/* Resolve a Sony NID-mangled symbol name to its absolute address
 * inside the target process. Returns 0 on lookup failure. Wraps
 * `kernel_dynlib_resolve` against both libkernel (handle 0x1) and
 * libkernel_sys (0x2001). */
intptr_t pt_resolve(pid_t pid, const char *nid);

/* Call function `addr` inside the target process with up to six
 * register-passed args (System-V ABI: rdi, rsi, rdx, rcx, r8, r9).
 * Returns the function's rax on success, -1 on ptrace failure.
 * Caller must already be attached + waitpid'd via `pt_attach`. */
long pt_call(pid_t pid, intptr_t addr, ...);

/* Issue a syscall in the target process. The trampoline is the
 * `syscall` instruction inside libkernel.sprx, located via the
 * known-NID `HoLVWNanBBc` plus a 0xa byte offset to land on the
 * raw `syscall` opcode. */
long pt_syscall(pid_t pid, int sysno, ...);

/* mmap inside the target process (calls pt_syscall(SYS_mmap, ...)).
 * Returns the mapped address in the target's address space, or
 * -1 on failure. */
intptr_t pt_mmap(pid_t pid, intptr_t addr, size_t len, int prot,
                  int flags, int fd, off_t off);
int pt_munmap(pid_t pid, intptr_t addr, size_t len);

#endif /* PS5UPLOAD2_PTRACE_REMOTE_H */
