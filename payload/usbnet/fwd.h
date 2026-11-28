/* SPDX-License-Identifier: GPL-3.0-or-later
 * The adapter's ports (PS5Upload). Adapted from tom-ps5-usbnet's port forwarding
 * (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later, include/forward.h and
 * src/forward.c): the console's services listen on the kernel's sockets, which the adapter's
 * address never reaches (that address is lwIP's), so each port here listens on lwIP and joins every
 * connection to the same port on 127.0.0.1. TCP only.
 *
 * Only PS5Upload's own ports: AVA1 (USBNET_AVA1_PORT) always, Minion (USBNET_MINION_PORT) while
 * fwd_set_minion(1) (the helper says Minion's LAN access is on). A Minion connection starts with a
 * PROXY v1 line naming the client (usbnet_proto.h), so Minion never takes a LAN client for the
 * console's own browser. Nothing here logs an address. */
#ifndef USBN_FWD_H
#define USBN_FWD_H

#include <stddef.h>
#include <stdint.h>

#define FWD_RULES 2

struct fwd_status {
    uint16_t port;
    int on, listening;
    unsigned links;
    uint64_t bytes;
};

/* Listens until fwd_stop(). address is the adapter's, dotted (the PROXY line's destination). */
int fwd_start(const char *address);
void fwd_stop(void);
void fwd_set_minion(int on);
/* Called (from a connection thread) when a kernel-side socket call fails with an errno that says
 * the console's network is going away (163 on the PS5 while it goes to rest). */
void fwd_set_down_hook(void (*hook)(int err));
void fwd_get_status(struct fwd_status out[FWD_RULES]);
/* The PROXY v1 line for one client (pure; tests). Returns its length, or 0 if it does not fit. */
size_t fwd_proxy_line(char *out, size_t cap, uint32_t client, uint16_t client_port, const char *server, uint16_t port);

#endif
