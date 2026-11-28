/* The helper <-> USB adapter daemon contract (payload/usbnet/, ps5u-usbnet.elf). EXPERIMENTAL.
 *
 * A supported USB 2.5G/5G/10G Ethernet adapter (Realtek RTL8156B 0bda:8156, RTL8157 0bda:8157,
 * RTL8159 0bda:815a) is driven from user space by a separate process, like the SMB daemon and the
 * keyboard & mouse companion: a fault in a USB driver or a TCP/IP stack ends that process, never
 * the helper. The console's own kernel cannot use the adapter, so the daemon runs its own lwIP stack
 * on it, gets an address (DHCP, link-local, or the static one the helper hands it), and joins TCP
 * connections that arrive on that address to the same port on 127.0.0.1, where the helper listens:
 * AVA1 transfers (9120) always, Minion (9180) only while Minion's LAN access is on. The console's
 * own network is never touched; everything keeps working there whether the adapter works or not.
 *
 * The helper finds the adapter (usbnet.c: the /dev/ugen nodes' device descriptors, nothing else),
 * launches the daemon through the console's ELF loader (:9021), and talks to it over loopback:
 * one connection per request, one JSON object and '\n' each way, every request carrying "key" (the
 * hex secret from USBNET_KEY_PATH, 0600, written by the helper). Ops:
 *
 *   {"key","op":"ping"}    -> {"ok":true,"elf":"<id>","pid":N}
 *   {"key","op":"status"}  -> USBNET status (below)
 *   {"key","op":"config","minion":0|1,"ip":"a.b.c.d","mask":"…","gateway":"…"}  -> {"ok":true}
 *                             ip "" = DHCP (link-local when no DHCP server answers)
 *   {"key","op":"quit"}    -> {"ok":true}, then a clean stop (MAC stopped, USB transfers closed,
 *                             the adapter's configuration put back)
 *
 * Status: {"ok":true,"elf","pid","state","why","chip","usb_id","chip_version","max_mbps",
 *          "link_mbps","ip","mask","gateway","source","lease_s","uptime_s","link_changes",
 *          "rx_frames","tx_frames","rx_bytes","tx_bytes","rx_drops",
 *          "usb_errors":{"out","in","start","complete","scan"},
 *          "forwards":[{"port","listening","links","bytes"}]}
 * state: USBNET_ST_* names below. The daemon also exits by itself when no helper has asked
 * anything for USBNET_HELPER_SILENT_S (an orphan never outlives its helper by long). */
#ifndef PS5UPLOAD_USBNET_PROTO_H
#define PS5UPLOAD_USBNET_PROTO_H

#define USBNET_PORT 9185
#define USBNET_NAME "ps5u-usbnet" /* never "ps5upload…": the helper's own-instance sweep kills that */
#ifndef USBNET_DIR
#define USBNET_DIR "/data/ps5upload/usbnet"
#endif
#define USBNET_KEY_PATH USBNET_DIR "/daemon.key"
#define USBNET_LOG_PATH USBNET_DIR "/usbnet.log"
#define USBNET_PID_PATH USBNET_DIR "/usbnet.pid"
/* The helper's setting: "off" in this file turns the feature off; "ip a.b.c.d mask gateway" asks for a
 * static address (an isolated switch with no DHCP server). Absent: on, DHCP. */
#define USBNET_SETTINGS_PATH USBNET_DIR "/settings"
#define USBNET_KEY_HEX 64
#define USBNET_REQ_MAX 1024u
#define USBNET_REPLY_MAX (16u * 1024u)
#define USBNET_HELPER_SILENT_S 120

/* The ports the daemon serves on the adapter's address (each joined to 127.0.0.1, same port). */
#define USBNET_AVA1_PORT 9120
#define USBNET_MINION_PORT 9180

/* Daemon states, as named in the status JSON. */
#define USBNET_ST_STARTING "starting"       /* finding the chip, bringing it up */
#define USBNET_ST_NO_LINK "no_link"         /* up, no cable or nothing at the other end */
#define USBNET_ST_NO_ADDRESS "no_address"   /* a link, still waiting for an address */
#define USBNET_ST_READY "ready"             /* a link and an address: serving */
#define USBNET_ST_STOPPING "stopping"
#define USBNET_ST_FAILED "failed"           /* bring-up failed or the adapter was lost; "why" says */
#define USBNET_ST_UNSUPPORTED "unsupported" /* a Realtek ID this daemon does not drive (an OEM revision) */
#define USBNET_ST_NONE "none"               /* the adapter went away before the daemon found it */

/* Minion sees every connection the daemon joins to it as 127.0.0.1. So that the console's own
 * browser keeps its rights and a LAN client does not get them, the daemon starts each Minion
 * connection with one PROXY protocol v1 line naming the client:
 *   "PROXY TCP4 <client> <adapter ip> <client port> 9180\r\n"
 * Minion's server accepts the line only from a loopback peer (webui_proxy_line, webui_http.h). */
#define USBNET_PROXY_LINE_MAX 108

#endif
