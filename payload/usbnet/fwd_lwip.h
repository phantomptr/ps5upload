/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/forward_lwip.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef USBN_FWD_LWIP_H
#define USBN_FWD_LWIP_H
/* lwIP's side of fwd.c, in fwd_lwip.c: only lwIP's headers are seen
 * there. Numbers are lwIP's own sockets, ports in host order, addresses in network
 * order. -1 with errno set on failure. */
#include <stddef.h>
#include <stdint.h>

#define FWD_LWIP_TIMEOUT (-2)

/* A non-blocking listener on the adapter's address. Port 0 lets lwIP choose. */
int fwd_lwip_listen(uint16_t port, int backlog);
int fwd_lwip_port(int fd);
/* Waits for any of the listeners to be readable: ready[i] says which. Returns how many,
 * 0 on a timeout. */
int fwd_lwip_wait(const int *fds, int count, int timeout_ms, int *ready);
/* A blocking connection whose reads and writes give up after timeout_ms with
 * FWD_LWIP_TIMEOUT, so the caller can look at whether it should stop. */
int fwd_lwip_accept(int listener, uint32_t *peer, uint16_t *peer_port, int timeout_ms);
long fwd_lwip_read(int fd, void *data, size_t length);
long fwd_lwip_write(int fd, const void *data, size_t length);
/* No more writes: the far end reads to its end of file. */
void fwd_lwip_shutdown(int fd);
/* reset sends an RST rather than a FIN. */
void fwd_lwip_close(int fd, int reset);
#endif
