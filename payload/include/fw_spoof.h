#ifndef PS5UPLOAD_FW_SPOOF_H
#define PS5UPLOAD_FW_SPOOF_H

#include <stddef.h>

void fw_spoof_init(void);

/* Kernel firmware magic (BCD-ish: 9.60 is 0x09600000), read behind the
 * SIGSEGV/SIGBUS/SIGILL guard because the offset is firmware-specific and
 * faults on an unexpected layout. Returns 0 when it could not be read —
 * callers must treat 0 as "unknown" and fail closed. */
unsigned int fw_safe_kernel_version(void);

int fw_spoof_status(char *buf, size_t cap, size_t *written);

#endif
