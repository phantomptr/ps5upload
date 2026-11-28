/* USB Ethernet adapter on the PS5 (EXPERIMENTAL): the helper's side. The protocol to the daemon that
 * drives the adapter, and why it is a daemon, are in usbnet_proto.h; the design and what is proven
 * on hardware in payload/usbnet/README.md.
 *
 * The helper watches the USB bus every few seconds (usbnet_scan: the /dev/ugen nodes' device
 * descriptors, a read-only ioctl, nothing else), and while a supported adapter is plugged in, the
 * feature is on and the console is not going to rest or settling after a wake (power_phase.h),
 * keeps the daemon running and asks it for its status. It never touches the console's own network.
 *
 *   - Feature off, rest mode, helper exit: the daemon is asked to quit (a clean stop), SIGKILLed by
 *     pid after USBNET_QUIT_WAIT_MS if it does not.
 *   - The daemon does not answer within USBNET_START_BUDGET_S of its launch (a bring-up stuck in a
 *     USB request): ended by pid, state "failed", not started again until the adapter is plugged in
 *     again or the feature is turned off and on.
 *   - The daemon exits by itself (the adapter was lost, bring-up failed): started again at most
 *     USBNET_MAX_RESTARTS times in a row, with backoff; then "failed" until a replug.
 *
 * The management method usbnet (AVA1 206) and Minion's /api/usbnet answer usbnet_call. Body JSON:
 *   {"op":"status"} (also an empty body)
 *   {"op":"enable"} / {"op":"disable"}          the setting (kept in USBNET_SETTINGS_PATH)
 *   {"op":"address","ip":"a.b.c.d","mask":"255.255.255.0","gateway":"a.b.c.1"}   a static address
 *   {"op":"address","ip":""}                    back to DHCP
 * Answer:
 *   {"ok":true,"experimental":true,"enabled":bool,"state":S,"error":"…",
 *    "adapter":{"usb_id":"0bda:8156","chip":"RTL8156B","max_mbps":2500}|null,
 *    "link_mbps":N,"ip":"…","mask":"…","gateway":"…","source":"dhcp|link-local|static|",
 *    "static_ip":"…","port":9120,"minion":bool,"rx_bytes":N,"tx_bytes":N,"rx_frames":N,"tx_frames":N,
 *    "errors":N,"link_changes":N,"uptime_s":N,"restarts":N,"seen":["vvvv:pppp",…]}
 * S: "off" (the setting), "none" (no supported adapter plugged in), "paused" (rest mode), "starting",
 *    "no_link", "no_address", "ready", "failed", "unsupported". "seen" lists the other USB devices'
 *    vendor:product IDs (hubs left out), so an adapter we do not know can be named in a report. */
#ifndef PS5UPLOAD_USBNET_H
#define PS5UPLOAD_USBNET_H

#include <stddef.h>
#include <stdint.h>

#define USBNET_SCAN_S 5
#define USBNET_POLL_S 2
#define USBNET_START_BUDGET_S 45
#define USBNET_QUIT_WAIT_MS 3000
#define USBNET_MAX_RESTARTS 3

/* At helper start (after the setting dirs exist): starts the watcher thread. 0 or -1 (logged). */
int usbnet_boot(void);
/* At helper exit: the daemon is asked to quit and waited for (bounded). */
void usbnet_shutdown(void);
/* power_phase.c: 1 while the console goes to rest or settles after a wake (the daemon is stopped and
 * not started), 0 after. Never blocks: the watcher thread acts on it. */
void usbnet_set_paused(int paused);
/* The method's answer into out (NUL-terminated). Returns its length, or -1. */
int usbnet_call(const char *body, size_t len, char *out, size_t cap);
/* For node.status: {"ip":"…","port":9120,"mbps":N,"chip":"…"} while the adapter serves, else null. */
int usbnet_status_brief(char *out, size_t cap);
/* The adapter's address while it serves (for Minion's Host check), else "" . */
void usbnet_current_ip(char out[16]);

/* ---- pure parts (host-tested: payload/tests/usbnet_selftest.c) ---- */

enum usbnet_chip_id { USBNET_CHIP_NONE = 0, USBNET_CHIP_8156B, USBNET_CHIP_8157, USBNET_CHIP_8159 };
/* A vendor:product this feature drives (the chip is told apart by its revision in the daemon). */
enum usbnet_chip_id usbnet_supported(uint16_t vid, uint16_t pid);
const char *usbnet_chip_name(enum usbnet_chip_id chip);
unsigned usbnet_chip_max_mbps(enum usbnet_chip_id chip);
/* "ugen<bus>.<addr>" -> 0 with bus and addr, else -1. */
int usbnet_parse_ugen(const char *name, int *bus, int *addr);
/* The settings file's text -> enabled (1/0) and a static address ("" for DHCP). 0, or -1 if a line
 * is malformed (then DHCP; an "off" line still counts). */
int usbnet_parse_settings(const char *text, int *enabled, char ip[16], char mask[16], char gw[16]);
/* A dotted IPv4 address, nothing else (no leading zeros, no spaces). */
int usbnet_ipv4_ok(const char *s);

#endif
