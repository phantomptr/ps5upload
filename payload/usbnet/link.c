/* SPDX-License-Identifier: GPL-3.0-or-later
 * See link.h; also fwd.h's fwd_proxy_line, pure too, kept here so the host test needs no lwIP. */
#include "link.h"

#include "fwd.h"
#include "rtl.h"
#include "usbnet_proto.h"

#include <stdio.h>

size_t fwd_proxy_line(char *out, size_t cap, uint32_t client, uint16_t client_port, const char *server, uint16_t port)
{
    const unsigned char *c = (const unsigned char *)&client; /* network order */
    int n = snprintf(out, cap, "PROXY TCP4 %u.%u.%u.%u %s %u %u\r\n", c[0], c[1], c[2], c[3], server, (unsigned)client_port,
                     (unsigned)port);

    return n > 0 && (size_t)n < cap && (size_t)n <= USBNET_PROXY_LINE_MAX ? (size_t)n : 0;
}

int usbn_link_phy(struct usbn_link *s, int read_ok, uint16_t status)
{
    int up;
    unsigned mbps;

    if (!read_ok) {
        if (++s->unreadable < LINK_READ_TRIES) return 0;
        status = 0;
    } else {
        s->unreadable = 0;
    }
    up = rtl_phy_link(status);
    mbps = rtl_phy_mbps(status);
    if (up == s->up) {
        if (up && mbps != s->mbps) {
            s->mbps = mbps;
            return 2;
        }
        return 0;
    }
    s->up = up;
    s->mbps = up ? mbps : 0;
    s->changes++;
    return 1;
}

const char *usbn_link_state(struct usbn_link *s, int have_address, long long now_ms)
{
    if (s->up && have_address) {
        s->lost_at = 0;
        s->ready = 1;
        return USBNET_ST_READY;
    }
    if (s->ready) {
        if (!s->lost_at) s->lost_at = now_ms;
        if (now_ms - s->lost_at < LINK_FALLBACK_MS) return USBNET_ST_READY;
        s->ready = 0;
    }
    return s->up ? USBNET_ST_NO_ADDRESS : USBNET_ST_NO_LINK;
}
