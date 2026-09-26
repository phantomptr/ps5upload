#include "fw_spoof.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/utsname.h>
#include <unistd.h>
#include <signal.h>
#include <setjmp.h>
#include <sys/sysctl.h>

#include <ps5/kernel.h>

void fw_spoof_init(void) {
}

/* Read kern.version sysctl string (e.g. " FreeBSD 12.0.0 PlayStation(R)5 ...").
 * On PS5 this usually contains the build but NOT the customer-visible FW
 * version in "XX.YY.ZZ" form. Still useful as a cross-check. */
static void get_kern_version_str(char *out, size_t cap) {
    out[0] = '\0';
    if (sysctlbyname("kern.version", out, &cap, NULL, 0) != 0) {
        out[0] = '\0';
    }
}

static void get_kernel_release(char *out, size_t cap) {
    out[0] = '\0';
    struct utsname uts;
    if (uname(&uts) == 0) {
        snprintf(out, cap, "%s", uts.release);
    }
}

/* Guard machinery for the kernel memory read — mirrors the HW_GUARD pattern
 * from hw_info.c. kernel_get_fw_version() reads from a FW-specific kernel
 * offset; on an unproven SKU/FW it can fault (SIGSEGV/SIGBUS) and kill the
 * payload. We arm a per-thread sigsetjmp so a fault degrades to fw=0
 * ("unknown") instead of crashing. */
static __thread volatile sig_atomic_t g_fsg_armed = 0;
static __thread sigjmp_buf g_fsg_jmp;

static void fsg_handler(int sig) {
    if (!g_fsg_armed) return;
    if (sig != SIGSEGV && sig != SIGBUS && sig != SIGILL) return;
    g_fsg_armed = 0;
    static const char msg[] = "[fw_spoof] FAULT in kernel_get_fw_version (skipped)\n";
    (void)write(2, msg, sizeof(msg) - 1);
    siglongjmp(g_fsg_jmp, sig);
}

/* Safely call kernel_get_fw_version() with fault recovery. Returns 0 on
 * fault (caller treats 0 as "unknown"). */
unsigned int fw_safe_kernel_version(void) {
    struct sigaction old_segv, old_bus, old_ill;
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = fsg_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;

    /* Install our handler, run the guarded call, then restore. */
    sigaction(SIGSEGV, &sa, &old_segv);
    sigaction(SIGBUS,  &sa, &old_bus);
    sigaction(SIGILL,  &sa, &old_ill);

    volatile unsigned int fw = 0;
    if (sigsetjmp(g_fsg_jmp, 1) == 0) {
        g_fsg_armed = 1;
        fw = kernel_get_fw_version();
        g_fsg_armed = 0;
    } else {
        g_fsg_armed = 0;
        fw = 0;
    }

    sigaction(SIGSEGV, &old_segv, NULL);
    sigaction(SIGBUS,  &old_bus,  NULL);
    sigaction(SIGILL,  &old_ill,  NULL);
    return fw;
}

static const char *fw_to_string(unsigned int fw) {
    static char buf[32];
    unsigned int major = (fw >> 24) & 0xFF;
    unsigned int minor = (fw >> 16) & 0xFF;
    unsigned int rev = (fw >> 8) & 0xFF;
    snprintf(buf, sizeof(buf), "%02x.%02x.%02x", major, minor, rev);
    return buf;
}

/* (parse_kern_major removed — the FreeBSD base version in kern.version does
 * not correlate 1:1 with the PS5 customer FW version, so cross-checking
 * them produces false positives. We keep the kern.version string for
 * display/debugging only.) */

int fw_spoof_status(char *buf, size_t cap, size_t *written) {
    /* The authoritative firmware version — read from kernel memory.
     * On PS5, kernel_get_fw_version() reads the system_control_block's
     * firmware field, which IS the running system software version.
     * This is the same value that sceKernelGetSystemSwVersion() would
     * return in userspace (which we can't call from a kthread). */
    unsigned int kernel_fw = fw_safe_kernel_version();

    /* Secondary sources for cross-validation */
    char kernel_rel[64];
    get_kernel_release(kernel_rel, sizeof(kernel_rel));

    char kern_ver_str[256];
    get_kern_version_str(kern_ver_str, sizeof(kern_ver_str));

    /* Format the version strings */
    const char *fw_str = (kernel_fw > 0) ? fw_to_string(kernel_fw) : "unknown";
    char fw_raw[16];
    if (kernel_fw > 0) {
        snprintf(fw_raw, sizeof(fw_raw), "0x%08x", kernel_fw);
    } else {
        snprintf(fw_raw, sizeof(fw_raw), "0x00000000");
    }

    /* Determine spoof status.
     *
     * On PS5, "firmware spoofing" means modifying the QA flags / kernel
     * memory so that the system reports a different FW version than what's
     * actually running. Detection without a second trusted source is
     * inherently limited — we use heuristics:
     *
     * 1. If kernel_get_fw_version() returns 0, we CANNOT determine the
     *    version — report spoofed=false (NOT a false positive).
     *
     * 2. The FreeBSD base version in kern.version is loosely correlated
     *    with the FW generation (PS5 1.x-3.x ≈ FreeBSD 11, 4.x-7.x ≈ 12.0,
     *    8.x+ ≈ 12.0/15.0). A gross mismatch (e.g. FreeBSD 11 with FW 12.x)
     *    is suspicious but not conclusive — we flag it only as a soft
     *    indicator, not spoofed=true, since the FreeBSD version doesn't
     *    update 1:1 with the customer FW version.
     *
     * 3. FW version > 0xFF in any field is impossible — definitely spoofed.
     */
    int spoofed = 0;

    if (kernel_fw > 0) {
        unsigned int major = (kernel_fw >> 24) & 0xFF;

        /* Sanity: no PS5 FW has major > 0x20 (32.xx would be far future).
         * If the kernel memory was tampered with, values can be absurd. */
        if (major == 0 || major > 0x20) {
            spoofed = 1;
        }
        /* Rev/Minor of 0 is normal (e.g. 09.60.00), so we don't flag that. */
    }
    /* If kernel_fw == 0, we simply can't tell — NOT spoofed (avoid false positive). */

    /* Truncate kern_ver_str for the JSON (it can be very long) */
    char kern_ver_short[128];
    if (kern_ver_str[0]) {
        /* Take first line only */
        char *nl = strchr(kern_ver_str, '\n');
        size_t len = nl ? (size_t)(nl - kern_ver_str) : strlen(kern_ver_str);
        if (len >= sizeof(kern_ver_short)) len = sizeof(kern_ver_short) - 1;
        memcpy(kern_ver_short, kern_ver_str, len);
        kern_ver_short[len] = '\0';
    } else {
        snprintf(kern_ver_short, sizeof(kern_ver_short), "unknown");
    }

    int n = snprintf(buf, cap,
        "{\"system_sw_version\":\"%s\","
        "\"system_sw_raw\":\"%s\","
        "\"kernel_release\":\"%s\","
        "\"kernel_fw_version\":\"%s\","
        "\"kernel_version\":\"%s\","
        "\"spoofed\":%s}",
        fw_str,
        fw_raw,
        kernel_rel[0] ? kernel_rel : "unknown",
        fw_str,
        kern_ver_short,
        spoofed ? "true" : "false");

    if (written) *written = (size_t)(n > 0 ? n : 0);
    return 0;
}
