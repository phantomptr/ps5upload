#ifndef PS5UPLOAD2_PROC_IDENTITY_H
#define PS5UPLOAD2_PROC_IDENTITY_H

#include <string.h>

/*
 * Is this process name one of OURS?
 *
 * Every thread the payload creates shares the "ps5upload" prefix:
 * "ps5upload.elf" (main), "ps5upload-wake", "ps5upload-fan", "ps5upload-smp".
 *
 * A prefix test rather than an exact compare is REQUIRED. The kinfo_proc
 * record returned by sysctl(KERN_PROC_*) carries the name of whichever
 * thread the kernel treats as representative, and that is not reliably the
 * main thread: the SceShellCore FMEM dump in issue #289 listed our process
 * as "ps5upload-wake". An exact compare against the main thread's name is
 * exactly the bug that stopped runtime_reap_prior_instance from reaping its
 * own wedged predecessor.
 *
 * It deliberately does NOT match "payload.elf" — the generic name elfldr
 * gives every raw-streamed payload (elfldr.c:704 -> uri_get_filename falls
 * back to the literal when there is no URI). Matching that would put kstuff,
 * ShadowMountPlus, nanoDNS and every other payload on the console inside our
 * SIGKILL radius.
 */
#define PS5UPLOAD2_PROC_PREFIX     "ps5upload"
#define PS5UPLOAD2_PROC_PREFIX_LEN 9

static inline int proc_name_is_ours(const char *name) {
    if (!name) return 0;
    return strncmp(name, PS5UPLOAD2_PROC_PREFIX,
                   PS5UPLOAD2_PROC_PREFIX_LEN) == 0 ? 1 : 0;
}

#endif /* PS5UPLOAD2_PROC_IDENTITY_H */
