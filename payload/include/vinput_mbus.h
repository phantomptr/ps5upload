/* libSceMbus for the keyboard & mouse companion's BIND (src/vinput_mbus.c): Play Together's guest
 * pads seated in a console user. Console-only, companion-only: sceMbusInit froze the helper when
 * it ran there (payload/vinput/README.md). No thread, no lock: the companion is single-threaded. */
#ifndef PS5UPLOAD_VINPUT_MBUS_H
#define PS5UPLOAD_VINPUT_MBUS_H

#include "vinput_companion.h"

/* libSceMbus loaded and its functions resolved (logged through `log`, may be NULL). The backend's
 * init then calls sceMbusInit(0, 0). NULL when a function is missing (BIND then answers
 * VWIRE_RC_UNSUPPORTED). */
const vcomp_mbus_t *vinput_mbus_backend(void (*log)(const char *fmt, ...));

#endif
