#ifndef PS5UPLOAD2_KERNEL_RW_LOCK_H
#define PS5UPLOAD2_KERNEL_RW_LOCK_H

/*
 * Shared mutex serializing EVERY process-ucred kernel-R/W "swap window"
 * in the payload.
 *
 * The ps5sdk kernel-R/W primitives (kernel_get/set_ucred_*,
 * kernel_get_root_vnode, kernel_set_proc_*) all drive a single global
 * kernel-RW window and mutate the PROCESS-GLOBAL ucred (authid is set
 * with pid -1 / getpid() — same process). They are not documented
 * thread-safe. Because this is kernel R/W, the worst case of a concurrent
 * access is not a helper crash but a KERNEL PANIC / black-screen restart,
 * or the process being left stuck at the wrong authid (the forge-mismatch
 * state main.c documents as black-screening mid-install).
 *
 * Before this lock existed, three INDEPENDENT serialization domains each
 * swapped the global authid without excluding the others:
 *
 *   1. sony_api_lock  — bgft.c install / status / local + register.c
 *                       uninstall swap to ShellCore authid for the Sony call.
 *   2. g_rpc_mtx      — shellui_rpc.c / ptrace_remote.c swap to the
 *                       ptrace-allowed authid for each SYS_ptrace syscall
 *                       (sensor reads, ShellUI register/launch).
 *   3. boot elevation — main.c runtime_apply_ucred_jailbreak.
 *
 * A 1 Hz install-status poll (domain 1) running concurrently with a
 * "read CPU temperature" tap or a Register+Launch (domain 2) is reachable
 * in ordinary single-console use, and routine with multiple consoles —
 * both drive the same kernel-RW window at once. This lock makes every
 * swap window mutually exclusive across all three domains.
 *
 * LOCK ORDERING — this is the INNERMOST lock. It is acquired ONLY around
 * the kernel-R/W swap window itself (save authid -> set -> call that needs
 * the swapped authid -> restore), held across that whole window, and
 * released. It is ALWAYS taken AFTER any domain lock already held
 * (sony_api_lock / g_rpc_mtx), never before. Because it is innermost and
 * no thread waits for a domain lock while holding it, no hold-and-wait
 * cycle can form through it — it is deadlock-free by construction. Do NOT
 * take sony_api_lock or g_rpc_mtx while holding this lock.
 *
 * Correctness: each window saves the TRUE current authid, swaps, runs its
 * call, and RESTORES before releasing the lock. Because every window is
 * atomic under this lock and self-restoring, windows from different
 * domains may interleave harmlessly (one fully finishes, restoring the
 * canonical authid, before the next begins) — there is no need to
 * serialize whole multi-step conversations, only each individual swap.
 */

#include <pthread.h>

extern pthread_mutex_t kernel_rw_lock;

#endif /* PS5UPLOAD2_KERNEL_RW_LOCK_H */
