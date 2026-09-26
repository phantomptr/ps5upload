/* Host-side test for the timeout recovery decision core. */
#include <stdio.h>

#include "../include/ptrace_recovery.h"

typedef struct fake_ctx {
    int stop_rc;
    int wait_rc;
    int restore_rc;
    int terminate_rc;
    int stop_calls;
    int wait_calls;
    int restore_calls;
    int terminate_calls;
} fake_ctx_t;

static int fake_stop(void *opaque) {
    fake_ctx_t *ctx = (fake_ctx_t *)opaque;
    ctx->stop_calls++;
    return ctx->stop_rc;
}

static int fake_wait(void *opaque) {
    fake_ctx_t *ctx = (fake_ctx_t *)opaque;
    ctx->wait_calls++;
    return ctx->wait_rc;
}

static int fake_restore(void *opaque) {
    fake_ctx_t *ctx = (fake_ctx_t *)opaque;
    ctx->restore_calls++;
    return ctx->restore_rc;
}

static int fake_terminate(void *opaque) {
    fake_ctx_t *ctx = (fake_ctx_t *)opaque;
    ctx->terminate_calls++;
    return ctx->terminate_rc;
}

static int failures = 0;

#define CHECK(expr) do { \
    if (!(expr)) { \
        fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
        failures++; \
    } \
} while (0)

int main(void) {
    const ptrace_recovery_ops_t ops = {
        fake_stop, fake_wait, fake_restore, fake_terminate
    };

    fake_ctx_t clean = {0};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &clean)
          == PTRACE_RECOVERY_RESTORED);
    CHECK(clean.stop_calls == 1);
    CHECK(clean.wait_calls == 1);
    CHECK(clean.restore_calls == 1);
    CHECK(clean.terminate_calls == 0);

    /* Failing to STOP the tracee must not mean killing it outright.
     *
     * The tracee here is usually SceShellUI, and terminating that takes the
     * whole console down — black screen, power-cord pull. A slow ShellUI (a
     * game has been hogging the CPU for two hours) looks exactly like a
     * broken one at the stop/wait step, so the expensive conclusion must not
     * be drawn from that step alone.
     *
     * Restoring the saved registers is what actually makes the tracee safe
     * to resume, and it is worth attempting even when the stop failed: on a
     * running tracee PT_SETREGS simply fails, which costs nothing and lands
     * us back at termination anyway. So: always try the restore before
     * escalating. */
    fake_ctx_t cannot_stop = {.stop_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &cannot_stop)
          == PTRACE_RECOVERY_RESTORED);
    CHECK(cannot_stop.restore_calls == 1);
    CHECK(cannot_stop.terminate_calls == 0);

    /* Same for a stop that was accepted but never reported — the timeout
     * case that actually fires on a loaded console. */
    fake_ctx_t wait_timed_out = {.wait_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &wait_timed_out)
          == PTRACE_RECOVERY_RESTORED);
    CHECK(wait_timed_out.restore_calls == 1);
    CHECK(wait_timed_out.terminate_calls == 0);

    /* But if the registers genuinely cannot be put back, the tracee must
     * still never resume with our injected stack — terminate stands. */
    fake_ctx_t stop_and_restore_failed = {.stop_rc = -1, .restore_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &stop_and_restore_failed)
          == PTRACE_RECOVERY_TERMINATED);
    CHECK(stop_and_restore_failed.restore_calls == 1);
    CHECK(stop_and_restore_failed.terminate_calls == 1);

    /* With no registers to restore (nothing was injected yet), a failed stop
     * has nothing to make safe, so termination remains the only fail-closed
     * option. */
    {
        ptrace_recovery_ops_t nores = ops;
        nores.restore_registers = NULL;
        fake_ctx_t no_regs_bad_stop = {.stop_rc = -1};
        CHECK(ptrace_recover_timed_out_tracee(&nores, &no_regs_bad_stop)
              == PTRACE_RECOVERY_TERMINATED);
        CHECK(no_regs_bad_stop.terminate_calls == 1);
    }

    fake_ctx_t cannot_restore = {.restore_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &cannot_restore)
          == PTRACE_RECOVERY_TERMINATED);
    CHECK(cannot_restore.stop_calls == 1);
    CHECK(cannot_restore.wait_calls == 1);
    CHECK(cannot_restore.restore_calls == 1);
    CHECK(cannot_restore.terminate_calls == 1);

    fake_ctx_t termination_failed = {
        .wait_rc = -1, .restore_rc = -1, .terminate_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &termination_failed)
          == PTRACE_RECOVERY_FAILED);
    CHECK(termination_failed.terminate_calls == 1);

    ptrace_recovery_ops_t no_restore = ops;
    no_restore.restore_registers = NULL;
    fake_ctx_t pre_register_change = {0};
    CHECK(ptrace_recover_timed_out_tracee(&no_restore, &pre_register_change)
          == PTRACE_RECOVERY_RESTORED);
    CHECK(pre_register_change.restore_calls == 0);

    CHECK(ptrace_lost_pid_matches(4242, 4242));
    CHECK(!ptrace_lost_pid_matches(4242, 4343));
    CHECK(!ptrace_lost_pid_matches(4242, 0));

    printf("ptrace_recovery_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
