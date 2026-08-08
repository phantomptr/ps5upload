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

    fake_ctx_t cannot_stop = {.stop_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &cannot_stop)
          == PTRACE_RECOVERY_TERMINATED);
    CHECK(cannot_stop.wait_calls == 0);
    CHECK(cannot_stop.restore_calls == 0);
    CHECK(cannot_stop.terminate_calls == 1);

    fake_ctx_t cannot_restore = {.restore_rc = -1};
    CHECK(ptrace_recover_timed_out_tracee(&ops, &cannot_restore)
          == PTRACE_RECOVERY_TERMINATED);
    CHECK(cannot_restore.stop_calls == 1);
    CHECK(cannot_restore.wait_calls == 1);
    CHECK(cannot_restore.restore_calls == 1);
    CHECK(cannot_restore.terminate_calls == 1);

    fake_ctx_t termination_failed = {.wait_rc = -1, .terminate_rc = -1};
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
