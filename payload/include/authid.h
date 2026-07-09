#ifndef PS5UPLOAD_AUTHID_H
#define PS5UPLOAD_AUTHID_H

/*
 * Shared authid constants + firmware detection + authid swap helpers.
 *
 * Consolidates the authid/firmware logic that was previously triplicated:
 *   - bgft.c:       PS5_SHELLCORE_AUTHID, PS5_SYSTEM_INSTALL_AUTHID,
 *                   authid_acquire(), authid_release_shellcore()
 *   - register.c:   REG_SHELLCORE_AUTHID, inline restore loop
 *   - dpi/ezremote_dpi.c: DPI_JB_AUTHID, DPI_SHELLCORE_AUTHID,
 *                   dpi_detect_firmware_major()
 *
 * The DPI daemon is a standalone single-file ELF that does NOT link
 * against register.c or bgft.c, so all functions here are `static inline`
 * — each translation unit gets its own copy, but the compiler optimizes
 * them into the same machine code as the old hand-inlined versions.
 *
 * Firmware detection uses the SAFE kern.version sysctl — NOT the faulting
 * kernel_get_fw_version offset read. Two parse tiers (see
 * ps5_parse_firmware_major):
 *   1. "releases/NN" — the canonical build-branch tag on production FWs
 *   2. Bare NN.NN substring — covers sandboxed debug configurations
 * Returns 0 if unknown, which errs toward the proven FW < 11 defaults.
 */

#include <stddef.h>
#include <sys/types.h>
#include <sys/sysctl.h>
#include <string.h>
#include <unistd.h>
#include <stdio.h>

/* ── Authid constants ───────────────────────────────────────────────────
 *
 * These are ucred auth_id values used by Sony's kernel to gate privileged
 * installer subsystem calls (BGFT, AppInstUtil). The default jailbreak
 * elevation sets authid to 0x4800000000000006 (debugger), which is correct
 * for ptrace + kernel R/W but rejected by Sony's install API gates. */

/* ShellCore's authid — required by sceAppInstUtilInstallByPackage and
 * sceAppInstUtilGetInstallStatus on FW < 11. Without it, these calls
 * return 0x80B22404 (HTTP_404 — Sony rejects the URL pre-flight) or
 * 0x80B21106 (parser/parameter rejection for file:// URLs). */
#define PS5_SHELLCORE_AUTHID 0x3800000000000010ULL

/* SYSTEM authid — required for:
 *  - sceAppInstUtilInitialize (BGFT init gate)
 *  - sceAppInstUtilAppInstallPkg (content-copy gate on FW >= 11)
 *  - jb_escalate_pid (the "JB_AUTHID" in elf-arsenal's jb.c)
 * ShellCore is enough to register the title metadata but the content-copy
 * step is gated behind this SYSTEM token; running AppInstallPkg under
 * ShellCore produces the "appmeta present, /user/app empty → dead tile"
 * signature. */
#define PS5_SYSTEM_INSTALL_AUTHID 0x4801000000000013ULL

/* JB (jailbreak) authid — what elf-arsenal's jb_escalate_pid sets as the
 * escalation target. Same value as PS5_SYSTEM_INSTALL_AUTHID; kept as a
 * distinct #define for semantic clarity at call sites that escalate a
 * pid rather than swap for a single Sony call. */
#define PS5_JB_AUTHID PS5_SYSTEM_INSTALL_AUTHID

/* ── Firmware detection ───────────────────────────────────────────────── */

/* Parse the kern.version string to extract the PS5 firmware major version.
 *
 * Two parse tiers:
 *   1. "releases/NN" — the canonical build-branch tag on production
 *      firmwares. Preferred because it's unambiguous.
 *   2. Bare NN.NN substring — covers firmware strings that omit the
 *      "releases/" prefix (some sandboxed debug configurations). We
 *      skip any leading "11.0" since FreeBSD's OS major (11) would
 *      otherwise be mistaken for PS5 firmware 11.x.
 *
 * Returns 0 if neither matches. */
static inline int ps5_parse_firmware_major(const char *kv) {
    if (!kv) return 0;
    const char *p = strstr(kv, "releases/");
    if (p) {
        p += strlen("releases/");
        int major = 0;
        int consumed = 0;
        while (*p >= '0' && *p <= '9') {
            major = major * 10 + (*p - '0');
            consumed++;
            p++;
        }
        if (consumed > 0) return major;
    }
    /* Fallback: first NN.NN after the FreeBSD "11.0" prefix. */
    const char *scan = kv;
    if (strncmp(scan, "FreeBSD 11.0", 12) == 0) scan += 12;
    while (*scan) {
        if (scan[0] >= '0' && scan[0] <= '9') {
            int major = 0;
            int consumed = 0;
            const char *q = scan;
            while (*q >= '0' && *q <= '9') {
                major = major * 10 + (*q - '0');
                consumed++;
                q++;
            }
            /* Require NN.NN shape to avoid matching an arbitrary number. */
            if (consumed > 0 && *q == '.' &&
                q[1] >= '0' && q[1] <= '9' && q[2] >= '0' && q[2] <= '9' &&
                (q[3] < '0' || q[3] > '9')) {
                return major;
            }
            scan = q;
        }
        scan++;
    }
    return 0;
}

/* Detect the PS5 firmware major version (9, 10, 11, 12, …) via the SAFE
 * kern.version sysctl. Returns 0 if unknown.
 *
 * Used to gate the InstallByPackage authid at the FW-11 "authority cliff":
 * below FW 11, InstallByPackage runs under ShellCore authid; at/above
 * FW 11, it requires SYSTEM authid (swapping to ShellCore registers the
 * title but lands no content — the "hollow dead-tile" bug). */
static inline int ps5_detect_firmware_major(void) {
    char buf[256];
    size_t sz = sizeof(buf);
    if (sysctlbyname("kern.version", buf, &sz, NULL, 0) != 0)
        return 0;
    buf[sizeof(buf) - 1] = '\0';
    return ps5_parse_firmware_major(buf);
}

/* ── Authid swap helpers ────────────────────────────────────────────────
 *
 * These require kernel R/W primitives (kernel_get_ucred_authid /
 * kernel_set_ucred_authid). The caller's translation unit must have
 * these symbols visible — either via <ps5/kernel.h> (DPI daemon) or via
 * the extern declarations in runtime.h (main payload). */

/* Forward-declare the kernel functions so the helpers below compile without
 * a separate extern block. If <ps5/kernel.h> or runtime.h has already
 * declared them, the declarations must match exactly (same return type
 * and parameter types). These match the signatures in ps5/kernel.h. */
extern uint64_t kernel_get_ucred_authid(pid_t pid);
extern int kernel_set_ucred_authid(pid_t pid, uint64_t authid);

/* Swap process authid to `target_authid` for one Sony API call.
 * Returns the saved authid on success, or 0 if the swap didn't happen
 * (kernel R/W unavailable or write failure). Pass the return value to
 * ps5_authid_release() to undo the swap.
 *
 * `site_tag` is a short identifier (e.g. "InstallByPackage") used only
 * in the warn log line so multi-call traces are diagnosable.
 *
 * `kernel_get_ucred_authid` and `kernel_set_ucred_authid` must be
 * declared by the including TU (via <ps5/kernel.h> or extern). */
static inline uint64_t ps5_authid_acquire(const char *site_tag,
                                           uint64_t target_authid) {
    pid_t mypid = getpid();
    uint64_t saved = kernel_get_ucred_authid(mypid);
    if (saved == 0) {
        /* Kernel R/W unavailable — caller's call will run with whatever
         * authid the process currently has. Better than aborting the
         * install when the only failure mode is "no jailbreak yet." */
        return 0;
    }
    if (kernel_set_ucred_authid(mypid, target_authid) != 0) {
        fprintf(stderr,
                "[%s] WARN: failed to swap to authid 0x%llx; "
                "Sony call will run with current authid (0x%llx)\n",
                site_tag, (unsigned long long)target_authid,
                (unsigned long long)saved);
        return 0;
    }
    return saved;
}

/* Restore prior authid with retry-and-verify (3 attempts). No-op if
 * `saved == 0` (acquire didn't actually swap). On persistent restore
 * failure, the shellui_rpc sensor path's `force_reresolve_locked`
 * re-arms debugger authid as a final safety net. */
static inline void ps5_authid_release(uint64_t saved, const char *site_tag) {
    if (saved == 0) return;
    pid_t mypid = getpid();
    for (int attempt = 0; attempt < 3; attempt++) {
        (void)kernel_set_ucred_authid(mypid, saved);
        if (kernel_get_ucred_authid(mypid) == saved) return;
    }
    fprintf(stderr,
            "[%s] WARN: failed to restore authid 0x%llx after "
            "Sony call (3 attempts); shellui_rpc will rearm.\n",
            site_tag, (unsigned long long)saved);
}

#endif /* PS5UPLOAD_AUTHID_H */
