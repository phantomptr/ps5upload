#ifndef PS5UPLOAD_FW_SPOOF_H
#define PS5UPLOAD_FW_SPOOF_H

#include <stddef.h>

void fw_spoof_init(void);

/* Kernel firmware magic (BCD-ish: 9.60 is 0x09600000), read behind the
 * SIGSEGV/SIGBUS/SIGILL guard because the offset is firmware-specific and
 * faults on an unexpected layout. Returns 0 when it could not be read —
 * callers must treat 0 as "unknown" and fail closed. */
unsigned int fw_safe_kernel_version(void);
/* The fatal-signal handler calls this first (like hw_guard_try_recover): a fault inside the
 * guarded read jumps back to it and does not return; 0 for any other fault. */
int fw_spoof_try_recover(int sig);

int fw_spoof_status(char *buf, size_t cap, size_t *written);

#endif
