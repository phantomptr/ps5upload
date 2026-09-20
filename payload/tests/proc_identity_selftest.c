/* Our process name is whichever thread the kernel picks as the kinfo_proc
 * representative — issue #289's kernel log showed the PS5 calling us
 * "ps5upload-wake", not the main thread's name. So identity is a prefix
 * test, never an exact string compare. */
#include "proc_identity.h"

#include <stdio.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    /* Every thread we create. */
    check(proc_name_is_ours("ps5upload.elf"), "main thread");
    check(proc_name_is_ours("ps5upload-wake"), "wake watchdog thread");
    check(proc_name_is_ours("ps5upload-fan"), "fan thread");
    check(proc_name_is_ours("ps5upload-smp"), "smp thread");

    /* The generic name elfldr gives every raw-streamed payload. Matching it
     * would put kstuff, nanoDNS and every other payload in SIGKILL range. */
    check(!proc_name_is_ours("payload.elf"), "generic loader name is NOT ours");

    /* Bystanders. */
    check(!proc_name_is_ours("pldmgr.elf"), "payload manager is not ours");
    check(!proc_name_is_ours("elfldr.elf"), "elfldr is not ours");
    check(!proc_name_is_ours("SceShellUI"), "system process is not ours");
    check(!proc_name_is_ours("shadowmountplus.elf"), "SMP is not ours");

    /* Defensive. */
    check(!proc_name_is_ours(NULL), "NULL is not ours");
    check(!proc_name_is_ours(""), "empty string is not ours");
    check(!proc_name_is_ours("ps5uploa"), "truncated prefix is not ours");

    printf("\nproc_identity_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
