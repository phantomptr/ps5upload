/*
 * Small, host-testable decision core for recovering a tracee after a remote
 * call timeout. The platform-specific operations stay in ptrace_remote.c;
 * this helper guarantees we report RESTORED only when stop, wait, and register
 * restoration all succeeded. Every other path attempts termination so a
 * tracee can never resume with injected registers.
 */
#ifndef PS5UPLOAD2_PTRACE_RECOVERY_H
#define PS5UPLOAD2_PTRACE_RECOVERY_H

typedef int (*ptrace_recovery_op)(void *ctx);

typedef enum ptrace_recovery_result {
    PTRACE_RECOVERY_RESTORED = 0,
    PTRACE_RECOVERY_TERMINATED = 1,
    PTRACE_RECOVERY_FAILED = 2,
} ptrace_recovery_result_t;

typedef struct ptrace_recovery_ops {
    ptrace_recovery_op request_stop;
    ptrace_recovery_op wait_stopped;
    /* Optional when the caller has not modified registers yet. */
    ptrace_recovery_op restore_registers;
    ptrace_recovery_op terminate;
} ptrace_recovery_ops_t;

/* Keep "lost tracee" state scoped to the affected pid. A process-global
 * boolean would make a ShellUI timeout suppress detach for an unrelated game
 * process being patched by the cheats watcher. */
static inline int ptrace_lost_pid_matches(int lost_pid, int candidate_pid) {
    return candidate_pid > 0 && lost_pid == candidate_pid;
}

static inline ptrace_recovery_result_t
ptrace_recover_timed_out_tracee(const ptrace_recovery_ops_t *ops, void *ctx) {
    if (!ops) return PTRACE_RECOVERY_FAILED;

    /* Best case: stop the tracee, see the stop reported, put the registers
     * back. Then it can be detached and resumed safely. */
    int stopped = ops->request_stop && ops->wait_stopped
                  && ops->request_stop(ctx) == 0
                  && ops->wait_stopped(ctx) == 0;

    /* Restoring the registers is what actually makes the tracee safe, so
     * attempt it even when the stop failed or was never reported.
     *
     * This matters because terminating is not a cheap fallback here: the
     * tracee is normally SceShellUI, and killing it takes the console down
     * to a black screen. A ShellUI that is merely *slow* — a game has had
     * the CPU for hours — is indistinguishable at the stop/wait step from
     * one that is broken, and must not be killed on that evidence alone.
     *
     * The attempt is free: on a tracee that is still running, PT_SETREGS
     * fails and we fall through to termination exactly as before. */
    if (ops->restore_registers) {
        if (ops->restore_registers(ctx) == 0) {
            return PTRACE_RECOVERY_RESTORED;
        }
    } else if (stopped) {
        /* Nothing was injected, and the tracee is stopped as asked — there
         * is nothing to make safe. */
        return PTRACE_RECOVERY_RESTORED;
    }

    /* Either there were no registers to put back and we could not stop it,
     * or the restore genuinely failed. A tracee that would resume at an
     * injected instruction/stack must not be detached. */
    if (ops->terminate && ops->terminate(ctx) == 0) {
        return PTRACE_RECOVERY_TERMINATED;
    }
    return PTRACE_RECOVERY_FAILED;
}

#endif /* PS5UPLOAD2_PTRACE_RECOVERY_H */
