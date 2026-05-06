#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <errno.h>
#include "runtime.h"

/* Per-call recv/send deadline on the takeover handshake. The old
 * payload's mgmt thread might accept the TCP connection but be
 * wedged before answering (kernel-deadlocked Sony APIs are a
 * documented FW 9.60 failure mode), in which case a blocking
 * recv() hangs the new payload in main() forever. 2 s is generous
 * for a loopback round-trip; anything slower than that and we'd
 * rather fall through to the port-poll loop below than block.
 */
#define TAKEOVER_RECV_TIMEOUT_SEC 2

/* How long to wait for both ports to be released after the ACK.
 * The old payload may be flushing a multi-GiB COMMIT_TX on the
 * transfer port — the historical 2 s ceiling timed out under that
 * load and surfaced as "takeover failed" even when the old payload
 * eventually did exit cleanly. Bumping to 10 s covers the
 * common-case big-upload tail without making a healthy takeover
 * feel any slower (the loop short-circuits the moment both ports
 * are gone).
 */
#define TAKEOVER_PORT_RELEASE_ATTEMPTS  100
#define TAKEOVER_PORT_RELEASE_INTERVAL_US 100000

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

    /* Bound the recv() below — without a deadline, a wedged old
     * payload that accept()s but never replies blocks the new
     * payload in main() forever. SO_RCVTIMEO is enough; we don't
     * need an SO_SNDTIMEO because the kernel queues 28 bytes
     * locally without ever crossing into the peer. */
    {
        struct timeval tv;
        tv.tv_sec = TAKEOVER_RECV_TIMEOUT_SEC;
        tv.tv_usec = 0;
        (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    }

    /* Send a proper binary FTX2 TAKEOVER_REQUEST frame. */
    build_takeover_frame(hdr);
    if (send(fd, hdr, sizeof(hdr), 0) != (ssize_t)sizeof(hdr)) {
        perror("[payload2] takeover send");
        close(fd);
        /* Port was open; try to wait for it to close anyway. */
    } else {
        /* Read the TAKEOVER_ACK (or any response). The recv timeout
         * above guarantees we don't hang indefinitely if the peer is
         * wedged — we'll fall through to the port-poll loop below,
         * which is itself bounded. */
        got = recv(fd, resp, sizeof(resp), 0);
        if (got > 0) {
            printf("[payload2] takeover ACK received (%zd bytes)\n", got);
        } else if (got == 0) {
            printf("[payload2] takeover: peer closed without ACK\n");
        } else if (errno == EAGAIN || errno == EWOULDBLOCK) {
            printf("[payload2] takeover: ACK timed out after %ds — proceeding to port poll\n",
                   TAKEOVER_RECV_TIMEOUT_SEC);
        } else {
            printf("[payload2] takeover recv error: %s — proceeding to port poll\n",
                   strerror(errno));
        }
        close(fd);
    }

    /* Wait up to ~10 s for the old runtime to release BOTH ports. The
     * transfer port may linger longer than mgmt if a large COMMIT_TX
     * is still flushing — we need both gone before we can bind them.
     * Pre-2.2.61 this was 2 s, which timed out under big-upload tails
     * even though the old payload would have exited cleanly given a
     * little more time. */
    for (attempts = 0; attempts < TAKEOVER_PORT_RELEASE_ATTEMPTS; attempts++) {
        int mgmt_gone = !runtime_port_responding(state->mgmt_port);
        int xfer_gone = !runtime_port_responding(state->runtime_port);
        if (mgmt_gone && xfer_gone) {
            state->startup_reason = PS5UPLOAD2_STARTUP_TAKEOVER;
            printf("[payload2] takeover completed after %d checks\n", attempts + 1);
            return 0;
        }
        usleep(TAKEOVER_PORT_RELEASE_INTERVAL_US);
    }

    fprintf(stderr,
            "[payload2] takeover timed out — ports %d/%d still occupied after %d.%ds\n",
            state->runtime_port, state->mgmt_port,
            (TAKEOVER_PORT_RELEASE_ATTEMPTS * TAKEOVER_PORT_RELEASE_INTERVAL_US) / 1000000,
            ((TAKEOVER_PORT_RELEASE_ATTEMPTS * TAKEOVER_PORT_RELEASE_INTERVAL_US) / 100000) % 10);
    /* Return -1 so main() does not attempt to bind over a live peer. */
    return -1;
}
