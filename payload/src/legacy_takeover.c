/*
 * MIGRATION SHIM: this file is deleted in the release after the AVA1 cutover.
 *
 * The one place the payload still speaks the old binary protocol: the takeover request a new
 * instance sends to a helper that was released before the cutover. Constants are inlined on
 * purpose; no other payload code includes them.
 */
#include "legacy_takeover.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "takeover_flag.h"

#define LEGACY_MAGIC 0x32585446u /* "FTX2", little endian on the wire */
#define LEGACY_VERSION 1u
#define LEGACY_TAKEOVER_REQUEST 18u

static void put_le(unsigned char *p, uint64_t v, int bytes) {
    for (int i = 0; i < bytes; i++) p[i] = (unsigned char)((v >> (8 * i)) & 0xff);
}

void legacy_takeover_frame(unsigned char hdr[LEGACY_TAKEOVER_HEADER_LEN]) {
    put_le(hdr + 0, LEGACY_MAGIC, 4);
    put_le(hdr + 4, LEGACY_VERSION, 2);
    put_le(hdr + 6, LEGACY_TAKEOVER_REQUEST, 2);
    put_le(hdr + 8, 0, 4);  /* flags */
    put_le(hdr + 12, 0, 8); /* body_len */
    put_le(hdr + 20, 0, 8); /* trace_id */
}

static int connect_loopback(int port) {
    struct sockaddr_in a;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, (struct sockaddr *)&a, sizeof a) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

int legacy_takeover(int mgmt_port, int xfer_port, int ack_timeout_s, int attempts, int interval_us) {
    /* The management port answers at once even while the transfer port is busy; a build from
     * before the port split only has the transfer port. */
    int fd = connect_loopback(mgmt_port);
    if (fd < 0) fd = connect_loopback(xfer_port);
    if (fd < 0) return LEGACY_TAKEOVER_NONE;

    /* An old helper that accepts but never answers (a wedged Sony API) must not hang us. */
    struct timeval tv = {ack_timeout_s, 0};
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);

    unsigned char hdr[LEGACY_TAKEOVER_HEADER_LEN], resp[LEGACY_TAKEOVER_HEADER_LEN];
    legacy_takeover_frame(hdr);
    if (send(fd, hdr, sizeof hdr, 0) == (ssize_t)sizeof hdr) {
        ssize_t got = recv(fd, resp, sizeof resp, 0);
        fprintf(stderr, "[payload2] legacy takeover: reply %zd bytes\n", got);
    }
    close(fd);

    /* The transfer port can linger while a large commit flushes: both must be gone. */
    for (int i = 0; i < attempts; i++) {
        if (!takeover_port_responding(mgmt_port) && !takeover_port_responding(xfer_port))
            return LEGACY_TAKEOVER_FREED;
        usleep((useconds_t)interval_us);
    }
    return LEGACY_TAKEOVER_STUCK;
}

int legacy_takeover_old_ports(int ack_timeout_s, int attempts, int interval_us) {
    return legacy_takeover(9114, 9113, ack_timeout_s, attempts, interval_us);
}
