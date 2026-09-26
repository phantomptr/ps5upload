/*
 * hw_guard_selftest.c — host-compilable proof that the hw_info.c fault-guard
 * mechanism (per-thread sigsetjmp + fatal-signal handler + siglongjmp)
 * actually RECOVERS from a SIGSEGV/SIGBUS inside a guarded call instead of
 * killing the process. It mirrors the exact structure used in
 * payload/src/hw_info.c (the PS5 SDK target can't run host tests, and a real
 * Sony getter can't be made to fault on demand, so this validates the
 * technique on a normal POSIX host).
 *
 * Build + run:
 *   cc -O2 -Wall -Wextra -Werror -o /tmp/hw_guard_selftest \
 *       payload/tests/hw_guard_selftest.c && /tmp/hw_guard_selftest
 * Exit 0 = all guard recoveries worked and the process stayed alive.
 */
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* ---- mirror of the hw_info.c guard machinery ------------------------- */
static __thread sigjmp_buf            g_hwg_jmp;
static __thread volatile sig_atomic_t g_hwg_armed = 0;
static __thread const char           *g_hwg_call = "";
static __thread size_t                g_hwg_call_len = 0;

static int hw_guard_try_recover(int sig) {
    if (!g_hwg_armed) return 0;
    if (sig != SIGSEGV && sig != SIGBUS && sig != SIGILL) return 0;
    g_hwg_armed = 0;
    static const char pfx[] = "[selftest] FAULT in guarded call: ";
    static const char sfx[] = " (recovered)\n";
    (void)write(2, pfx, sizeof(pfx) - 1);
    if (g_hwg_call && g_hwg_call_len) (void)write(2, g_hwg_call, g_hwg_call_len);
    (void)write(2, sfx, sizeof(sfx) - 1);
    siglongjmp(g_hwg_jmp, sig);
    return 1; /* unreachable */
}

static void handle_fatal(int sig) {
    if (hw_guard_try_recover(sig)) return; /* unreachable on recover */
    /* In the real payload this would clean up + re-raise; here a genuine
     * (unguarded) fault means the test itself is broken — fail loudly. */
    (void)write(2, "[selftest] UNGUARDED fatal signal — FAIL\n", 41);
    _exit(2);
}

#define HW_GUARD(label, stmt)                                              \
    do {                                                                   \
        g_hwg_call = (label);                                              \
        g_hwg_call_len = sizeof(label) - 1;                                \
        if (sigsetjmp(g_hwg_jmp, 1) == 0) {                                \
            g_hwg_armed = 1;                                               \
            stmt;                                                          \
            g_hwg_armed = 0;                                               \
        } else {                                                           \
            g_hwg_armed = 0;                                               \
        }                                                                  \
    } while (0)

/* ---- the tests ------------------------------------------------------- */
static int failures = 0;
static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    signal(SIGSEGV, handle_fatal);
    signal(SIGBUS, handle_fatal);
    signal(SIGILL, handle_fatal);

    /* 1. A NULL-deref write inside the guard must be recovered, the result
     *    must keep its safe default, and the process must stay alive. */
    {
        volatile int got = 0;
        HW_GUARD("null_deref_write", { *(volatile int *)0 = 1; got = 1; });
        check(got == 0, "null-deref recovered, default preserved (got==0)");
    }

    /* 2. A successful guarded call must run normally and disarm. */
    {
        volatile int got = 0;
        int sink = 0;
        HW_GUARD("ok_call", { sink = 42; got = 1; });
        check(got == 1 && sink == 42, "successful guarded call runs + disarms");
    }

    /* 3. A wild-pointer read inside the guard must be recovered too. */
    {
        volatile long v = -1;
        HW_GUARD("wild_read", { v = *(volatile long *)(uintptr_t)0xdead0000; });
        check(v == -1, "wild-pointer read recovered, default preserved");
    }

    /* 4. After recovery the guard must still work (state not wedged). */
    {
        volatile int got = 0;
        int sink = 0;
        HW_GUARD("post_recovery_ok", { sink = 7; got = 1; });
        check(got == 1 && sink == 7, "guard still works after a recovery");
    }

    /* 5. We are still alive to print this — the whole point. */
    check(1, "process survived all faults");

    printf("\nhw_guard_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
