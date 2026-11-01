/* The console's own virtual keyboard and mouse as a vinput backend (src/vinput_ps5.c): the relay to
 * the companion process (src/vinput_relay.c, payload/vinput/). On for every firmware (tested on
 * 13.60) unless switched off (built with -DVINPUT_PS5=0, or VINPUT_PS5_DISABLE_FILE exists): then
 * vinput_ps5_backend() returns NULL and vinput.c's stub stays. Console-only. */
#ifndef PS5UPLOAD_VINPUT_PS5_H
#define PS5UPLOAD_VINPUT_PS5_H

#include "vinput.h"

#define VINPUT_PS5_DISABLE_FILE "/data/ps5upload/vinput.disable"

/* The backend, or NULL while it is off (see above). Logs which, once per call. */
const vinput_backend_t *vinput_ps5_backend(void);

#endif
