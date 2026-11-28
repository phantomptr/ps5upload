/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/net.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md).
 *
 * See net.h. The receive thread holds the core lock while it feeds frames in, which is what
 * tcpip_input would do one frame at a time with LWIP_TCPIP_CORE_LOCKING_INPUT, without taking the
 * lock per frame. PS5Upload: no web server here (fwd.c serves), a static address, and counters. */
#include "net.h"

#include "lwip/autoip.h"
#include "lwip/dhcp.h"
#include "lwip/etharp.h"
#include "lwip/ip4_addr.h"
#include "lwip/netif.h"
#include "lwip/prot/dhcp.h"
#include "lwip/tcpip.h"
#include "netif/ethernet.h"

#include <stdlib.h>
#include <string.h>

#define RELEASE_FLUSH_MS 200

unsigned char *usbn_net_heap;

static struct netif g_netif;
static struct link_ops g_link;
static uint8_t g_mac[6];
static int g_up, g_static;
static struct net_counts g_counts; /* rx under the core lock; tx under it too (lwIP outputs there) */

static err_t link_output(struct netif *netif, struct pbuf *packet)
{
    uint8_t *slot;

    (void)netif;
    if (packet->tot_len > MAX_FRAME) return ERR_IF;
    slot = g_link.reserve();
    if (!slot) {
        g_counts.tx_drops++;
        return ERR_IF;
    }
    pbuf_copy_partial(packet, slot, packet->tot_len, 0);
    g_link.commit(packet->tot_len);
    g_counts.tx_frames++;
    g_counts.tx_bytes += packet->tot_len;
    return ERR_OK;
}

static err_t interface_init(struct netif *netif)
{
    netif->name[0] = 'u';
    netif->name[1] = 'e';
    netif->output = etharp_output;
    netif->linkoutput = link_output;
    netif->hwaddr_len = ETH_HWADDR_LEN;
    memcpy(netif->hwaddr, g_mac, ETH_HWADDR_LEN);
    netif->mtu = 1500;
    netif->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP | NETIF_FLAG_ETHERNET;
    return ERR_OK;
}

static void core_ready(void *semaphore)
{
    sys_sem_signal(semaphore);
}

int net_start(const uint8_t mac[6], const struct link_ops *link)
{
    sys_sem_t ready;
    struct netif *added;

    memcpy(g_mac, mac, sizeof g_mac);
    g_link = *link;
    if (!usbn_net_heap) usbn_net_heap = malloc(MEM_SIZE + 4096);
    if (!usbn_net_heap || sys_sem_new(&ready, 0) != ERR_OK) return -1;
    tcpip_init(core_ready, &ready);
    sys_sem_wait(&ready);
    sys_sem_free(&ready);
    LOCK_TCPIP_CORE();
    added = netif_add(&g_netif, IP4_ADDR_ANY4, IP4_ADDR_ANY4, IP4_ADDR_ANY4, NULL, interface_init, ethernet_input);
    if (added) netif_set_default(added);
    UNLOCK_TCPIP_CORE();
    return added ? 0 : -1;
}

void net_frames(const struct frame *frames, int count)
{
    int i;

    LOCK_TCPIP_CORE();
    for (i = 0; i < count; i++) {
        struct pbuf *packet;

        if (!g_up) {
            g_counts.rx_drops++;
            continue;
        }
        packet = pbuf_alloc(PBUF_RAW, (u16_t)frames[i].len, PBUF_POOL);
        if (!packet) {
            g_counts.rx_drops++;
            continue;
        }
        g_counts.rx_frames++;
        g_counts.rx_bytes += frames[i].len;
        if (pbuf_take(packet, frames[i].data, (u16_t)frames[i].len) != ERR_OK || g_netif.input(packet, &g_netif) != ERR_OK)
            pbuf_free(packet);
    }
    UNLOCK_TCPIP_CORE();
}

void net_link(int up)
{
    LOCK_TCPIP_CORE();
    if (up) netif_set_link_up(&g_netif);
    else netif_set_link_down(&g_netif);
    UNLOCK_TCPIP_CORE();
}

static int parse(const char *text, ip4_addr_t *out)
{
    return text && text[0] && ip4addr_aton(text, out) ? 0 : -1;
}

int net_up(const char *ip, const char *mask, const char *gateway)
{
    ip4_addr_t a, m, g;
    int result = 0;

    LOCK_TCPIP_CORE();
    g_up = 1;
    dhcp_stop(&g_netif);
    autoip_stop(&g_netif);
    if (parse(ip, &a) == 0) {
        if (parse(mask, &m) != 0) IP4_ADDR(&m, 255, 255, 255, 0);
        if (parse(gateway, &g) != 0) ip4_addr_set_zero(&g);
        netif_set_addr(&g_netif, &a, &m, &g);
        g_static = 1;
        netif_set_up(&g_netif);
    } else {
        netif_set_addr(&g_netif, IP4_ADDR_ANY4, IP4_ADDR_ANY4, IP4_ADDR_ANY4);
        g_static = 0;
        netif_set_up(&g_netif);
        result = dhcp_start(&g_netif) == ERR_OK ? 0 : -1;
    }
    UNLOCK_TCPIP_CORE();
    return result;
}

int net_address(struct net_lease *lease)
{
    memset(lease, 0, sizeof *lease);
    LOCK_TCPIP_CORE();
    if (g_static && !ip4_addr_isany_val(*netif_ip4_addr(&g_netif))) {
        lease->source = "static";
    } else if (dhcp_supplied_address(&g_netif)) {
        const struct dhcp *dhcp = netif_dhcp_data(&g_netif);

        lease->source = "dhcp";
        lease->lease_seconds = dhcp->offered_t0_lease;
        ip4addr_ntoa_r(ip_2_ip4(&dhcp->server_ip_addr), lease->server, sizeof lease->server);
    } else if (autoip_supplied_address(&g_netif)) {
        lease->source = "link-local";
    }
    if (lease->source) {
        ip4addr_ntoa_r(netif_ip4_addr(&g_netif), lease->ip, sizeof lease->ip);
        ip4addr_ntoa_r(netif_ip4_netmask(&g_netif), lease->mask, sizeof lease->mask);
        ip4addr_ntoa_r(netif_ip4_gw(&g_netif), lease->gateway, sizeof lease->gateway);
    }
    UNLOCK_TCPIP_CORE();
    return lease->source ? 0 : -1;
}

void net_counts(struct net_counts *out)
{
    LOCK_TCPIP_CORE();
    *out = g_counts;
    UNLOCK_TCPIP_CORE();
}

void net_stop(void)
{
    LOCK_TCPIP_CORE();
    dhcp_release_and_stop(&g_netif);
    autoip_stop(&g_netif);
    UNLOCK_TCPIP_CORE();
    sys_msleep(RELEASE_FLUSH_MS);
    LOCK_TCPIP_CORE();
    g_up = 0;
    netif_set_down(&g_netif);
    UNLOCK_TCPIP_CORE();
}
