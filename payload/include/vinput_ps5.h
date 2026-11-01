/* The console's own virtual keyboard and mouse as a vinput backend (src/vinput_ps5.c): the relay to
 * the companion process (src/vinput_relay.c, payload/vinput/). OFF by default: vinput_ps5_backend()
 * returns NULL (keep vinput.c's stub) unless the firmware is on the allow-list (13.60 only) AND it
 * is not switched off (built with -DVINPUT_PS5=0, or VINPUT_PS5_DISABLE_FILE exists). Console-only. */
#ifndef PS5UPLOAD_VINPUT_PS5_H
#define PS5UPLOAD_VINPUT_PS5_H

#include <string.h>

#include "vinput.h"

#define VINPUT_PS5_DISABLE_FILE "/data/ps5upload/vinput.disable"

/* 1 when the firmware string ("13.60") is one the device calls were probed on. Host-tested. */
static inline int vinput_ps5_firmware_allowed(const char *fw) {
    static const char *const allowed[] = {"13.60", NULL};
    for (int i = 0; fw && allowed[i]; i++)
        if (strcmp(fw, allowed[i]) == 0) return 1;
    return 0;
}
/* The backend, or NULL while it is off (see above). Logs which, once per call. */
const vinput_backend_t *vinput_ps5_backend(void);

#endif
