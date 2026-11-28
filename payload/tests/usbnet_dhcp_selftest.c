/* The USB adapter daemon's network layer on the host (payload/usbnet/net.c with its lwIP and
 * sys_arch.c, built exactly as for the console but for the host): the adapter is replaced by a fake
 * link that hands every frame lwIP sends to this test, and this test plays the DHCP server.
 *
 *   - DISCOVER goes out as soon as the interface is up; an OFFER brings a REQUEST; an ACK brings an
 *     address (after lwIP's address-conflict probes, which nobody answers here);
 *   - the lease reads back as DHCP, with its mask, gateway and server;
 *   - a static address replaces it at once; going back to DHCP asks again;
 *   - the stop releases the lease (RELEASE on the wire);
 *   - the frame and byte counters move.
 * No real network is touched: the fake link is memory. */
#include "../usbnet/net.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

void usbn_net_assert(const char *what, const char *file, int line) {
    fprintf(stderr, "lwIP assertion %s at %s:%d\n", what, file, line);
    abort();
}

static int failures;
static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

/* ---- the fake link: frames lwIP sends, queued for the test ---- */

#define QMAX 256
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static uint8_t g_slot[MAX_FRAME];
static uint8_t g_q[QMAX][MAX_FRAME];
static size_t g_qlen[QMAX];
static int g_qhead, g_qcount;

static uint8_t *fake_reserve(void) {
    pthread_mutex_lock(&g_mu); /* held until commit, as the real one is */
    return g_slot;
}

static void fake_commit(size_t len) {
    if (g_qcount < QMAX) {
        int at = (g_qhead + g_qcount) % QMAX;
        memcpy(g_q[at], g_slot, len);
        g_qlen[at] = len;
        g_qcount++;
    }
    pthread_mutex_unlock(&g_mu);
}

static size_t take_frame(uint8_t *out) {
    size_t n = 0;
    pthread_mutex_lock(&g_mu);
    if (g_qcount) {
        n = g_qlen[g_qhead];
        memcpy(out, g_q[g_qhead], n);
        g_qhead = (g_qhead + 1) % QMAX;
        g_qcount--;
    }
    pthread_mutex_unlock(&g_mu);
    return n;
}

/* ---- DHCP on the wire ---- */

static const uint8_t g_mac[6] = {0x02, 0x00, 0x00, 0x00, 0x00, 0x01};
static const uint8_t g_server_mac[6] = {0x02, 0x00, 0x00, 0x00, 0x00, 0xfe};

static uint16_t be16(const uint8_t *p) { return (uint16_t)(p[0] << 8 | p[1]); }

/* The DHCP message type of a client frame (0 if it is not one), and its xid. */
static int dhcp_type(const uint8_t *f, size_t n, uint32_t *xid) {
    const uint8_t *ip = f + 14, *udp, *bootp, *opt;
    size_t ihl;
    if (n < 14 + 20 + 8 + 240 || be16(f + 12) != 0x0800 || ip[9] != 17) return 0;
    ihl = (size_t)(ip[0] & 15) * 4;
    udp = ip + ihl;
    if (be16(udp + 2) != 67) return 0;
    bootp = udp + 8;
    *xid = (uint32_t)bootp[4] << 24 | (uint32_t)bootp[5] << 16 | (uint32_t)bootp[6] << 8 | bootp[7];
    opt = bootp + 240;
    while (opt < f + n && *opt != 255) {
        if (*opt == 0) {
            opt++;
            continue;
        }
        if (opt + 2 > f + n) break;
        if (opt[0] == 53 && opt[1] == 1) return opt[2];
        opt += 2 + opt[1];
    }
    return 0;
}

static uint16_t ip_checksum(const uint8_t *p, size_t n) {
    uint32_t sum = 0;
    size_t i;
    for (i = 0; i + 1 < n; i += 2) sum += (uint32_t)(p[i] << 8 | p[i + 1]);
    while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
    return (uint16_t)~sum;
}

/* An OFFER (2) or ACK (5) for 192.168.77.50 from 192.168.77.1. */
static size_t server_reply(uint8_t *f, int type, uint32_t xid) {
    uint8_t *ip = f + 14, *udp = ip + 20, *b = udp + 8, *o = b + 240;
    size_t blen, ulen, total;
    memset(f, 0, 600);
    memset(f, 0xff, 6);
    memcpy(f + 6, g_server_mac, 6);
    f[12] = 0x08;
    b[0] = 2;
    b[1] = 1;
    b[2] = 6;
    b[4] = (uint8_t)(xid >> 24);
    b[5] = (uint8_t)(xid >> 16);
    b[6] = (uint8_t)(xid >> 8);
    b[7] = (uint8_t)xid;
    b[16] = 192, b[17] = 168, b[18] = 77, b[19] = 50; /* yiaddr */
    b[20] = 192, b[21] = 168, b[22] = 77, b[23] = 1;  /* siaddr */
    memcpy(b + 28, g_mac, 6);
    b[236] = 99, b[237] = 130, b[238] = 83, b[239] = 99;
    *o++ = 53, *o++ = 1, *o++ = (uint8_t)type;
    *o++ = 54, *o++ = 4, *o++ = 192, *o++ = 168, *o++ = 77, *o++ = 1;
    *o++ = 51, *o++ = 4, *o++ = 0, *o++ = 0, *o++ = 0x0e, *o++ = 0x10; /* 3600 s */
    *o++ = 1, *o++ = 4, *o++ = 255, *o++ = 255, *o++ = 255, *o++ = 0;
    *o++ = 3, *o++ = 4, *o++ = 192, *o++ = 168, *o++ = 77, *o++ = 1;
    *o++ = 255;
    blen = (size_t)(o - b);
    if (blen < 300) blen = 300;
    ulen = 8 + blen;
    udp[0] = 0, udp[1] = 67, udp[2] = 0, udp[3] = 68;
    udp[4] = (uint8_t)(ulen >> 8), udp[5] = (uint8_t)ulen; /* checksum 0: none */
    ip[0] = 0x45;
    ip[2] = (uint8_t)((20 + ulen) >> 8), ip[3] = (uint8_t)(20 + ulen);
    ip[8] = 64, ip[9] = 17;
    ip[12] = 192, ip[13] = 168, ip[14] = 77, ip[15] = 1;
    ip[16] = 255, ip[17] = 255, ip[18] = 255, ip[19] = 255;
    {
        uint16_t c = ip_checksum(ip, 20);
        ip[10] = (uint8_t)(c >> 8), ip[11] = (uint8_t)c;
    }
    total = 14 + 20 + ulen;
    return total;
}

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/* Waits up to ms for a client DHCP message of `type`; other frames (ARP probes) are dropped. */
static int wait_for(int type, uint32_t *xid, int ms) {
    static uint8_t f[MAX_FRAME];
    long long until = now_ms() + ms;
    while (now_ms() < until) {
        size_t n = take_frame(f);
        if (!n) {
            usleep(5000);
            continue;
        }
        if (dhcp_type(f, n, xid) == type) return 1;
    }
    return 0;
}

static void feed(const uint8_t *f, size_t n) {
    struct frame fr = {f, n};
    net_frames(&fr, 1);
}

/* The server's answer to an ARP request (whatever address it asks for). */
static size_t arp_reply(uint8_t *out, const uint8_t *req) {
    memset(out, 0, 60);
    memcpy(out, req + 6, 6);
    memcpy(out + 6, g_server_mac, 6);
    out[12] = 0x08, out[13] = 0x06;
    out[14] = 0, out[15] = 1, out[16] = 0x08, out[17] = 0, out[18] = 6, out[19] = 4, out[20] = 0, out[21] = 2;
    memcpy(out + 22, g_server_mac, 6);
    memcpy(out + 28, req + 38, 4); /* the asked-for address is ours */
    memcpy(out + 32, req + 22, 6);
    memcpy(out + 38, req + 28, 4);
    return 60;
}

static void *stopper(void *unused) {
    (void)unused;
    net_stop();
    return NULL;
}

int main(void) {
    static const struct link_ops link = {fake_reserve, fake_commit};
    static uint8_t reply[600];
    struct net_lease lease;
    struct net_counts counts;
    uint32_t xid = 0;
    long long until;
    int got = 0;

    printf("usbnet network layer (lwIP DHCP over a fake link)\n");
    check(net_start(g_mac, &link) == 0, "the stack starts");
    net_link(1);
    check(net_up(NULL, NULL, NULL) == 0, "DHCP starts");
    check(wait_for(1, &xid, 3000), "DISCOVER goes out");
    feed(reply, server_reply(reply, 2, xid));
    check(wait_for(3, &xid, 3000), "an OFFER brings a REQUEST");
    feed(reply, server_reply(reply, 5, xid));
    until = now_ms() + 15000; /* lwIP probes the address before it takes it */
    while (now_ms() < until && !(got = net_address(&lease) == 0)) {
        uint8_t f[MAX_FRAME];
        (void)take_frame(f);
        usleep(20000);
    }
    check(got && !strcmp(lease.source, "dhcp") && !strcmp(lease.ip, "192.168.77.50"), "an ACK brings the address");
    check(got && !strcmp(lease.mask, "255.255.255.0") && !strcmp(lease.gateway, "192.168.77.1") &&
              !strcmp(lease.server, "192.168.77.1") && lease.lease_seconds == 3600,
          "mask, gateway, server and lease time");
    net_counts(&counts);
    check(counts.rx_frames >= 2 && counts.tx_frames >= 2 && counts.rx_bytes > 600, "frames and bytes counted");

    check(net_up("10.9.8.7", "255.0.0.0", "10.0.0.1") == 0 && net_address(&lease) == 0 && !strcmp(lease.source, "static") &&
              !strcmp(lease.ip, "10.9.8.7") && !strcmp(lease.gateway, "10.0.0.1"),
          "a static address replaces it at once");
    check(net_up("", "", "") == 0 && net_address(&lease) != 0, "back to DHCP: no address until a server answers");
    check(wait_for(1, &xid, 3000), "and DISCOVER goes out again");
    feed(reply, server_reply(reply, 2, xid));
    check(wait_for(3, &xid, 3000), "REQUEST");
    feed(reply, server_reply(reply, 5, xid));
    until = now_ms() + 15000;
    got = 0;
    while (now_ms() < until && !(got = net_address(&lease) == 0)) usleep(20000);
    check(got && !strcmp(lease.source, "dhcp"), "bound again");
    /* The RELEASE is unicast to the server: net_stop runs on a thread while this one answers the
     * ARP request for the server and watches for the RELEASE. */
    {
        pthread_t t;
        static uint8_t f[MAX_FRAME];
        int released = 0;
        pthread_create(&t, NULL, stopper, NULL);
        until = now_ms() + 2000;
        while (now_ms() < until && !released) {
            size_t n = take_frame(f);
            if (!n) {
                usleep(2000);
                continue;
            }
            static const uint8_t server_ip[4] = {192, 168, 77, 1};
            if (be16(f + 12) == 0x0806 && be16(f + 20) == 1 && !memcmp(f + 38, server_ip, 4)) feed(reply, arp_reply(reply, f));
            else if (dhcp_type(f, n, &xid) == 7) released = 1;
        }
        pthread_join(t, NULL);
        check(released, "the stop releases the lease");
    }
    printf("%s (%d failed)\n", failures ? "FAILED" : "ok", failures);
    return failures ? 1 : 0;
}
