/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/forward_lwip.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
/* See fwd_lwip.h. lwIP's socket names and constants differ from libc's, so they are
 * only spelled here. One thread reads a connection while another writes or shuts it
 * down: LWIP_NETCONN_FULLDUPLEX with LWIP_NETCONN_SEM_PER_THREAD keeps concurrent
 * read, write and shutdown on the same socket safe across threads. */
#include "fwd_lwip.h"

#include "lwip/sockets.h"
#include "lwip/sys.h"

#include <errno.h>
#include <string.h>

int fwd_lwip_listen(uint16_t port, int backlog)
{
    struct sockaddr_in address;
    int fd = lwip_socket(AF_INET, SOCK_STREAM, 0), on = 1, saved;
    u32_t nonblocking = 1;

    if (fd < 0) return -1;
    lwip_setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    memset(&address, 0, sizeof address);
    address.sin_len = sizeof address;
    address.sin_family = AF_INET;
    address.sin_port = lwip_htons(port);
    if (lwip_bind(fd, (struct sockaddr *)&address, sizeof address) == 0 && lwip_listen(fd, backlog) == 0 &&
            lwip_ioctl(fd, (long)FIONBIO, &nonblocking) == 0)
        return fd;
    saved = errno;
    lwip_close(fd);
    errno = saved;
    return -1;
}

int fwd_lwip_port(int fd)
{
    struct sockaddr_in address;
    socklen_t length = sizeof address;

    if (lwip_getsockname(fd, (struct sockaddr *)&address, &length) != 0) return -1;
    return lwip_ntohs(address.sin_port);
}

int fwd_lwip_wait(const int *fds, int count, int timeout_ms, int *ready)
{
    struct pollfd wait[64];
    int i, result;

    if (count > (int)(sizeof wait / sizeof wait[0])) count = (int)(sizeof wait / sizeof wait[0]);
    for (i = 0; i < count; i++) {
        wait[i].fd = fds[i];
        wait[i].events = POLLIN;
        wait[i].revents = 0;
        ready[i] = 0;
    }
    if (!count) {
        sys_msleep((u32_t)timeout_ms);
        return 0;
    }
    result = lwip_poll(wait, (nfds_t)count, timeout_ms);
    for (i = 0; result > 0 && i < count; i++) ready[i] = (wait[i].revents & (POLLIN | POLLERR)) != 0;
    return result;
}

int fwd_lwip_accept(int listener, uint32_t *peer, uint16_t *peer_port, int timeout_ms)
{
    struct sockaddr_in from;
    socklen_t length = sizeof from;
    struct timeval timeout = {timeout_ms / 1000, (timeout_ms % 1000) * 1000};
    int fd = lwip_accept(listener, (struct sockaddr *)&from, &length), on = 1;
    int idle = 30, interval = 10, count = 6;
    u32_t blocking = 0;

    if (fd < 0) return -1;
    *peer = from.sin_addr.s_addr;
    *peer_port = lwip_ntohs(from.sin_port);
    lwip_ioctl(fd, (long)FIONBIO, &blocking);
    lwip_setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
    lwip_setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
    lwip_setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on);
    lwip_setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &on, sizeof on);
    lwip_setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &idle, sizeof idle);
    lwip_setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &interval, sizeof interval);
    lwip_setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &count, sizeof count);
    return fd;
}

long fwd_lwip_read(int fd, void *data, size_t length)
{
    ssize_t n = lwip_read(fd, data, length);

    if (n < 0) {
        int code = errno;

        if (code == EWOULDBLOCK || code == EAGAIN || code == EINTR) return FWD_LWIP_TIMEOUT;
        if (code == ENOMEM || code == ENOBUFS) {
            sys_msleep(10);
            return FWD_LWIP_TIMEOUT;
        }
        errno = code;
    }
    return (long)n;
}

long fwd_lwip_write(int fd, const void *data, size_t length)
{
    ssize_t n = lwip_write(fd, data, length);

    if (n < 0) {
        int code = errno;

        if (code == EWOULDBLOCK || code == EAGAIN || code == EINTR) return FWD_LWIP_TIMEOUT;
        if (code == ENOMEM || code == ENOBUFS) {
            sys_msleep(10);
            return FWD_LWIP_TIMEOUT;
        }
        errno = code;
        return -1;
    }
    if (n == 0) return FWD_LWIP_TIMEOUT;
    return (long)n;
}

void fwd_lwip_shutdown(int fd)
{
    lwip_shutdown(fd, SHUT_WR);
}

void fwd_lwip_close(int fd, int reset)
{
    if (reset) {
        struct linger now = {1, 0};

        lwip_setsockopt(fd, SOL_SOCKET, SO_LINGER, &now, sizeof now);
    }
    lwip_close(fd);
}
