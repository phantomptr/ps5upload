/* The console's own virtual keyboard and mouse (libSceMouse / libSceKeyboard), as a vinput backend
 * (src/vinput_dev.c). Console-only. Today it runs in the companion process (payload/vinput/): the
 * same calls hung the whole helper (payload/vinput/README.md). It keeps no thread of its own and
 * takes no lock: one caller at a time (the companion is single-threaded; merged back into the helper
 * it would run on a thread of its own, and nothing else may call it). */
#ifndef PS5UPLOAD_VINPUT_DEV_H
#define PS5UPLOAD_VINPUT_DEV_H

#include "vinput.h"

/* Once, before anything else: sceUserServiceInitialize, the input modules loaded, their symbols
 * resolved, sceMouseInit / sceKeyboardInit. Logs through `log` (may be NULL).
 *
 * `handles_path` (may be NULL): where the open devices' handles are kept. A process that ends
 * without DeleteDevice (SIGKILL) leaves its devices behind (measured on 13.60: they keep slot 0 of
 * the user, every later device lands on slot 1, and a reader of slot 0 sees nothing), so the
 * handles a previous run left there are deleted first. */
void vinput_dev_init(void (*log)(const char *fmt, ...), const char *handles_path);
/* The backend ("ps5"). Its caps say what Init made possible. */
const vinput_backend_t *vinput_dev_backend(void);
/* About once a second: when the foreground user changed while a device is open, the devices are
 * let go, removed and made again for the new user; 1 then (whatever was held is gone). One that
 * could not be made again has no handle: its open makes it (the companion asks again). */
int vinput_dev_tick(void);

#endif
