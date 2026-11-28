/* SPDX-License-Identifier: GPL-3.0-or-later
 * lwIP options for the USB adapter daemon (payload/usbnet/README.md). Taken from tom-ps5-usbnet's
 * include/lwipopts.h (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later), whose values
 * reached ~3 Gbit/s of TCP on a PS5, with what this daemon does not use left out.
 *
 * A separate process from the helper, so this is a separate lwIP from the tailnet core's
 * (tailnet/lwip-port/lwipopts.h: NO_SYS, no Ethernet): the two never share a binary. Here lwIP runs
 * with its own tcpip thread (NO_SYS 0) and BSD-style sockets, Ethernet + ARP, DHCP with link-local
 * fallback, and buffers sized for a 10 GbE link. */
#ifndef USBN_LWIPOPTS_H
#define USBN_LWIPOPTS_H

#define NO_SYS 0
#define SYS_LIGHTWEIGHT_PROT 1
#define LWIP_TIMERS 1
#define LWIP_TCPIP_CORE_LOCKING 1
#define LWIP_COMPAT_SOCKETS 0
#define LWIP_POSIX_SOCKETS_IO_NAMES 0
#define LWIP_SOCKET 1
#define LWIP_NETCONN 1
#define LWIP_NETCONN_SEM_PER_THREAD 1
#define LWIP_NETCONN_FULLDUPLEX 1
#define LWIP_TCP_KEEPALIVE 1
#define SO_REUSE 1
#define LWIP_SOCKET_SELECT 1
#define LWIP_SOCKET_POLL 1
#define LWIP_SO_RCVTIMEO 1
#define LWIP_SO_SNDTIMEO 1
#define LWIP_SO_SNDRCVTIMEO_NONSTANDARD 0
#define LWIP_SO_LINGER 1
#define LWIP_TCP 1
#define LWIP_UDP 1  /* DHCP */
#define LWIP_RAW 0
#define LWIP_DNS 0
#define LWIP_DHCP 1
#define LWIP_AUTOIP 1 /* 169.254.x.y when no DHCP server answers */
#define LWIP_DHCP_AUTOIP_COOP 1
#define LWIP_DHCP_AUTOIP_COOP_TRIES 3
#define LWIP_IPV4 1
#define LWIP_IPV6 0
#define LWIP_ICMP 1 /* answers ping: a user can check the adapter's address from a PC */
#define LWIP_IGMP 0
#define LWIP_ARP 1
#define LWIP_ETHERNET 1
#define IP_FORWARD 0
#define IP_REASSEMBLY 0
#define IP_FRAG 0
#define LWIP_NETIF_LOOPBACK 0
#define LWIP_HAVE_LOOPIF 0
#define LWIP_SINGLE_NETIF 0

/* Memory. The heap is malloc'ed when the stack starts (net.c), so a refusal is a log line and a
 * "failed" state, not a daemon that does not load. */
#define MEM_ALIGNMENT 8
#define MEM_SIZE (16 * 1024 * 1024)
#define LWIP_RAM_HEAP_POINTER usbn_net_heap
extern unsigned char *usbn_net_heap;
#define MEMP_NUM_PBUF 512
#define MEMP_NUM_TCP_SEG 4096
#define PBUF_POOL_SIZE 4096
#define PBUF_POOL_BUFSIZE 1600
/* Two listeners (AVA1, Minion) and their connections: AVA1 opens up to 8 data lanes per session. */
#define MEMP_NUM_TCP_PCB 96
#define MEMP_NUM_TCP_PCB_LISTEN 8
#define MEMP_NUM_NETCONN 128
#define MEMP_NUM_SYS_TIMEOUT 16

/* TCP for a 10 GbE LAN: a 2 MiB window, so a stall of about 1.6 ms does not idle the link. */
#define TCP_MSS 1460
#define LWIP_WND_SCALE 1
#define TCP_RCV_SCALE 6
#define TCP_WND (2 * 1024 * 1024)
#define TCP_SND_BUF (2 * 1024 * 1024)
#define TCP_SND_QUEUELEN 4096
#define TCP_SNDLOWAT (32 * 1024)
#define TCP_LISTEN_BACKLOG 1
/* Upstream's timers lose a segment for 1.5 s at best; these bottom out at three 100 ms ticks. */
#define TCP_TMR_INTERVAL 50
#define LWIP_TCP_RTO_TIME 1000
#define LWIP_TCP_SACK_OUT 1
#define LWIP_CHECKSUM_ON_COPY 1
#define TCPIP_MBOX_SIZE 256
/* Every queued frame holds one of these; lwIP's default of 8 drops a burst before TCP sees it. */
#define MEMP_NUM_TCPIP_MSG_INPKT TCPIP_MBOX_SIZE
#define DEFAULT_TCP_RECVMBOX_SIZE 4096
#define DEFAULT_ACCEPTMBOX_SIZE 32
#define DEFAULT_THREAD_STACKSIZE 0
#define TCPIP_THREAD_STACKSIZE 0
#define TCPIP_THREAD_PRIO 0

#define LWIP_STATS 1 /* the status's drop counters */
#define LWIP_STATS_DISPLAY 0
#define MIB2_STATS 1
#define SYS_STATS 0
/* LWIP_PROVIDE_ERRNO must remain undefined: upstream tests it with #ifdef. */
#define LWIP_ERRNO_STDINCLUDE 1
#define LWIP_SOCKET_OFFSET 0
#define LWIP_CHECKSUM_CTRL_PER_NETIF 0
#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 0
#define LWIP_RAND() usbn_port_rand()
unsigned int usbn_port_rand(void);

#endif
