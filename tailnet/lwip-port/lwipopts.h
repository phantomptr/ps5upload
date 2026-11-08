/* lwIP options for tailnet's forwarder (src/fwd.c): NO_SYS (driven by the data-plane
 * poll loop), IPv4 + TCP only, one netif (the tailnet address), MSS for the 1280-byte
 * tunnel MTU (spec 10). UDP is relayed by fwd.c itself, not by lwIP. */
#ifndef TN_LWIPOPTS_H
#define TN_LWIPOPTS_H

#define NO_SYS 1
#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 1 /* the system's htons & co. */
#define LWIP_TIMERS 1
#define SYS_LIGHTWEIGHT_PROT 0
#define LWIP_SOCKET 0
#define LWIP_NETCONN 0
#define LWIP_SINGLE_NETIF 1

#define LWIP_IPV4 1
#define LWIP_IPV6 0
#define LWIP_ARP 0
#define LWIP_ETHERNET 0
#define LWIP_ICMP 0     /* the core answers echo requests itself */
#define LWIP_RAW 0
#define LWIP_UDP 0
#define LWIP_DHCP 0
#define LWIP_AUTOIP 0
#define LWIP_ACD 0
#define LWIP_IGMP 0
#define LWIP_DNS 0
#define IP_FORWARD 0
#define IP_REASSEMBLY 0
#define IP_FRAG 0
#define LWIP_STATS 0
#define LWIP_NETIF_STATUS_CALLBACK 0
#define LWIP_NETIF_LINK_CALLBACK 0

#define LWIP_TCP 1
#undef TCP_MSS /* <netinet/tcp.h> has its own (a socket option number) */
#define TCP_MSS 1240                       /* 1280 - 20 (IPv4) - 20 (TCP) */
#define TCP_WND (32 * TCP_MSS)
#define TCP_SND_BUF (32 * TCP_MSS)
#define TCP_SND_QUEUELEN (4 * TCP_SND_BUF / TCP_MSS)
#define LWIP_TCP_KEEPALIVE 1
#define TCP_QUEUE_OOSEQ 1
#define LWIP_TCP_SACK_OUT 0
#define LWIP_WND_SCALE 0

/* Memory: from malloc, only while a node runs (no BSS when Tailscale is off; big
 * static pools also made Mach-O linkers warn about their alignment). Counts below cap
 * what is in use at once. */
#define MEM_LIBC_MALLOC 1
#define MEMP_MEM_MALLOC 1
#define MEM_ALIGNMENT 8
#define MEM_SIZE (256 * 1024)
#define MEMP_NUM_TCP_PCB 16
#define MEMP_NUM_TCP_PCB_LISTEN 4
#define MEMP_NUM_TCP_SEG (2 * TCP_SND_QUEUELEN)
#define MEMP_NUM_PBUF 64
#define PBUF_POOL_SIZE 96
#define PBUF_POOL_BUFSIZE 1536
#define MEMP_NUM_SYS_TIMEOUT 8

#define CHECKSUM_GEN_IP 1
#define CHECKSUM_GEN_TCP 1
#define CHECKSUM_CHECK_IP 1
#define CHECKSUM_CHECK_TCP 1

unsigned tn_lwip_rand(void);
#define LWIP_RAND() tn_lwip_rand()

#endif
