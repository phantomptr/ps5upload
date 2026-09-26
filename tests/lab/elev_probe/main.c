/* Standalone elevation-probe payload.
 *
 * Does nothing except call the SDK's kernel_set_ucred_authid helper
 * and log before/after authid values to klog. Used to verify whether
 * self-elevation is safe on a given loader chain without risking
 * the main ps5upload payload's startup.
 *
 * Deploy:
 *   nc -w 1 <PS5_IP> 9021 < elev_probe.elf
 *
 * Then check klog (PS5 notification / ps5-payload-dev klogsrv) for
 * the [elev_probe] lines. If the payload SIGSEGVs, there won't be
 * any output — that's the signal the SDK kernel helpers don't work
 * on this loader chain.
 */

#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#include <ps5/kernel.h>
#include <ps5/klog.h>

#define PS5_ELEVATED_AUTHID 0x4801000000000013LL

int main(void) {
    pid_t pid = getpid();
    klog_printf("[elev_probe] starting, pid=%d\n", (int)pid);

    /* Pre-check: can we READ our own authid? If the kernel R/W
     * primitives aren't wired up, this call is the first place
     * we'd crash. */
    klog_puts("[elev_probe] calling kernel_get_ucred_authid...");
    uint64_t before = kernel_get_ucred_authid(pid);
    klog_printf("[elev_probe] authid before: 0x%016llx\n",
                (unsigned long long)before);

    if (before == 0) {
        klog_puts("[elev_probe] authid read returned 0 — kernel R/W "
                  "likely not initialized on this loader. Bailing.");
        return 1;
    }

    /* Now try the elevation itself. */
    klog_puts("[elev_probe] calling kernel_set_ucred_authid...");
    int32_t rc = kernel_set_ucred_authid(pid, PS5_ELEVATED_AUTHID);
    klog_printf("[elev_probe] kernel_set_ucred_authid rc=%d\n", (int)rc);

    /* Verify by reading back. */
    uint64_t after = kernel_get_ucred_authid(pid);
    klog_printf("[elev_probe] authid after:  0x%016llx\n",
                (unsigned long long)after);

    if (after == (uint64_t)PS5_ELEVATED_AUTHID) {
        klog_puts("[elev_probe] SUCCESS: elevation worked on this loader");
    } else {
        klog_puts("[elev_probe] FAILED: authid did not change after set");
    }

    /* Caps, separately, so a failure here doesn't mask the authid
     * result above. */
    klog_puts("[elev_probe] calling kernel_set_ucred_caps...");
    uint8_t full_caps[16];
    memset(full_caps, 0xff, sizeof(full_caps));
    int32_t rc_caps = kernel_set_ucred_caps(pid, full_caps);
    klog_printf("[elev_probe] kernel_set_ucred_caps rc=%d\n", (int)rc_caps);

    klog_puts("[elev_probe] exiting cleanly");
    return 0;
}
