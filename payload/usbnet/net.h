/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/net.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md).
 *
 * lwIP on top of the adapter's bulk link: one Ethernet netif, an address from DHCP (link-local when
 * no server answers) or a static one. Kept apart from the USB headers, whose socket types are not
 * lwIP's. Host-built for payload/tests/usbnet_dhcp_selftest.c with a fake link. */
#ifndef USBN_NET_H
#define USBN_NET_H

#include "frame.h"

#include <stdint.h>

struct net_lease {
    char ip[16], mask[16], gateway[16], server[16];
    const char *source; /* "dhcp", "link-local", "static" or NULL */
    unsigned long lease_seconds;
};

/* Where lwIP's frames go. reserve returns room for MAX_FRAME bytes or NULL, and what it returned
 * must go to commit. */
struct link_ops {
    uint8_t *(*reserve)(void);
    void (*commit)(size_t length);
};

struct net_counts {
    unsigned long long rx_frames, tx_frames, rx_bytes, tx_bytes, rx_drops, tx_drops;
};

int net_start(const uint8_t mac[6], const struct link_ops *link);
/* Frames from the adapter, in order. Called from the receive thread. */
void net_frames(const struct frame *frames, int count);
void net_link(int up);
/* Brings the interface up and starts asking for an address, with or without a link (DHCP waits
 * for one). A static ip ("" or NULL: DHCP) is set at once. */
int net_up(const char *ip, const char *mask, const char *gateway);
/* The address the interface has now. 0 if there is one. */
int net_address(struct net_lease *lease);
void net_counts(struct net_counts *out);
/* DHCP released, the interface down. */
void net_stop(void);

#endif
