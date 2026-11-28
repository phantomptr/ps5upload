/* SPDX-License-Identifier: GPL-3.0-or-later
 * The adapter's link and what the daemon reports, as pure decisions (host-tested in
 * payload/tests/usbnet_selftest.c). The timings follow tom-ps5-usbnet's src/main.c
 * (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later), which settled them on a PS5:
 *
 *   - the PHY status register is read once a second; a read that fails is the chip not answering
 *     over USB, not the cable, and only counts as "no link" after LINK_READ_TRIES in a row;
 *   - a link that goes away is reported only after LINK_FALLBACK_MS, so a cable that blinks does
 *     not send every transfer back and forth between the adapter and the console network;
 *   - a link that comes back is reported at once (with an address).
 *
 * The console's own network is not this daemon's to switch: "fallback" is the engine choosing the
 * console's address while the state is not "ready" (usbnet_proto.h). */
#ifndef USBN_LINK_H
#define USBN_LINK_H

#include <stdint.h>

#define LINK_READ_TRIES 3
#define LINK_FALLBACK_MS 3000

struct usbn_link {
    int up;            /* the link as last decided */
    unsigned mbps;     /* its rate while up */
    int unreadable;    /* failed status reads in a row */
    unsigned changes;  /* up/down changes since the start */
    long long lost_at; /* when the adapter stopped being usable (0: it is, or never was) */
    int ready;         /* what was last reported: 1 "ready" */
};

/* One PHY status read: read_ok 0 for a failed read. Returns 1 when the link went up or down,
 * 2 when only its rate changed, 0 otherwise. */
int usbn_link_phy(struct usbn_link *s, int read_ok, uint16_t status);
/* The state to report now (a USBNET_ST_* name): "ready", "no_link" or "no_address". */
const char *usbn_link_state(struct usbn_link *s, int have_address, long long now_ms);

#endif
