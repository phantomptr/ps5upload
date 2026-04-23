#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <errno.h>
#include "runtime.h"

/* ── FTX2 frame helpers (local copies; takeover.c has no other includes) ── */

#define TAKEOVER_MAGIC       0x32585446u
#define TAKEOVER_VERSION     1u
#define TAKEOVER_FRAME_TYPE  18u   /* FTX2_FRAME_TAKEOVER_REQUEST */
#define TAKEOVER_HEADER_LEN  28u

static void write_le16(unsigned char *p, uint16_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >> 8) & 0xff);
}
static void write_le32(unsigned char *p, uint32_t v) {
    p[0] = (unsigned char)(v & 0xff);
    p[1] = (unsigned char)((v >>  8) & 0xff);
    p[2] = (unsigned char)((v >> 16) & 0xff);
    p[3] = (unsigned char)((v >> 24) & 0xff);
}
static void write_le64(unsigned char *p, uint64_t v) {
    int i = 0;
    for (i = 0; i < 8; i++) p[i] = (unsigned char)((v >> (8 * i)) & 0xff);
}

/* Build a 28-byte FTX2 TAKEOVER_REQUEST frame with no body. */
static void build_takeover_frame(unsigned char hdr[TAKEOVER_HEADER_LEN]) {
    write_le32(hdr + 0,  TAKEOVER_MAGIC);
    write_le16(hdr + 4,  TAKEOVER_VERSION);
    write_le16(hdr + 6,  TAKEOVER_FRAME_TYPE);
    write_le32(hdr + 8,  0);   /* flags */
    write_le64(hdr + 12, 0);   /* body_len */
    write_le64(hdr + 20, 0);   /* trace_id */
}

static int runtime_port_responding(int port) {
    struct sockaddr_in addr;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return 0;

    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        return 0;
    }
    close(fd);
    return 1;
}

int runtime_try_takeover(runtime_state_t *state) {
    struct stat st;
    struct sockaddr_in addr;
    int fd = -1;
    unsigned char hdr[TAKEOVER_HEADER_LEN];
    unsigned char resp[TAKEOVER_HEADER_LEN];
    ssize_t got = 0;
    int attempts = 0;
    /* Takeover now targets the management port. The transfer port may
     * be blocked inside an active upload — but the mgmt listener runs
     * in its own pthread and answers TAKEOVER_REQUEST immediately. The
     * old payload receives it, sets shutdown_requested, and both its
     * listeners exit on the next accept-return. */
    int target_port = 0;
    int transfer_port = 0;

    if (!state) return -1;
    target_port = state->mgmt_port;
    transfer_port = state->runtime_port;

    if (stat(state->ownership_path, &st) == 0) {
        printf("[payload2] previous ownership record found at %s\n",
               state->ownership_path);
    } else {
        printf("[payload2] no previous ownership record found\n");
    }
    printf("[payload2] takeover probe on mgmt port=%d (transfer=%d) instance=%llu\n",
           target_port, transfer_port,
           (unsigned long long)state->instance_id);

    fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("[payload2] takeover socket");
        return -1;
    }

    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)target_port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        /* Fall back to the transfer port in case this is a pre-split
         * legacy payload (single-port builds only bind :9113). */
        fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) {
            perror("[payload2] takeover socket (fallback)");
            return -1;
        }
        addr.sin_port = htons((uint16_t)transfer_port);
        if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
            close(fd);
            printf("[payload2] no active runtime on loopback — fresh start\n");
            return 0;
        }
        target_port = transfer_port;
        printf("[payload2] takeover: mgmt port unreachable, targeting legacy transfer port %d\n",
               target_port);
    }

    /* Send a proper binary FTX2 TAKEOVER_REQUEST frame. */
    build_takeover_frame(hdr);
    if (send(fd, hdr, sizeof(hdr), 0) != (ssize_t)sizeof(hdr)) {
        perror("[payload2] takeover send");
        close(fd);
        /* Port was open; try to wait for it to close anyway. */
    } else {
        /* Read the TAKEOVER_ACK (or any response). */
        got = recv(fd, (char *)resp, sizeof(resp), 0);
        if (got > 0) {
            printf("[payload2] takeover ACK received (%zd bytes)\n", got);
        } else {
            printf("[payload2] takeover: peer closed without ACK\n");
        }
        close(fd);
    }

    /* Wait up to 2 s for the old runtime to release BOTH ports. The
     * transfer port may linger longer than mgmt if a large COMMIT_TX
     * is still flushing — we need both gone before we can bind them. */
    for (attempts = 0; attempts < 20; attempts++) {
        int mgmt_gone = !runtime_port_responding(state->mgmt_port);
        int xfer_gone = !runtime_port_responding(state->runtime_port);
        if (mgmt_gone && xfer_gone) {
            state->startup_reason = PS5UPLOAD2_STARTUP_TAKEOVER;
            printf("[payload2] takeover completed after %d checks\n", attempts + 1);
            return 0;
        }
        usleep(100000);
    }

    fprintf(stderr,
            "[payload2] takeover timed out — ports %d/%d still occupied\n",
            state->runtime_port, state->mgmt_port);
    /* Return -1 so main() does not attempt to bind over a live peer. */
    return -1;
}
