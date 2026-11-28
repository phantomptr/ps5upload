# ps5u-usbnet: USB Ethernet adapters on the PS5 (experimental)

The PS5's own port is gigabit (about 110–120 MB/s). A USB 2.5G/5G/10G adapter can be faster, but
the console's kernel has no driver for one: the adapter only works if a program drives it from user
space and brings its own TCP/IP stack. This daemon does that for PS5Upload, so AVA1 transfers (and
Minion, when its LAN access is on) also answer on the adapter's own address. The console's network
is never touched: everything keeps working on it, whether the adapter works or not.

Credits: the driver is adapted from tom-ps5-usbnet (GPL-3.0-or-later); see [NOTICE.md](NOTICE.md).
Each adapted file says which file it came from.

## What the reference proves on hardware

Read from its source, README, commits and issues (v0.1.0 to 0.3.7, Nov 2026), not run here:

| Fact | Evidence |
|---|---|
| A payload can drive a USB NIC from user space on a PS5: `/dev/ugenB.A` nodes, the FreeBSD `USB_GET_DEVICE_DESC` / `USB_SET_CONFIG` / `USB_DO_REQUEST` / `USB_FS_*` ioctls, no kernel driver, no kernel write | its `adapter.c`, `vendor.c`; tested on a PS5 Slim CFI-2200, FW 12.40 |
| Chips: RTL8159 `0bda:815a` (10G), RTL8156B/BG `0bda:8156` (2.5G), RTL8157 `0bda:8157` (5G); the chip is told apart by PLA_TCR0's version (8156B 0x7400/0x7410/0x7420, 8157 0x1030, 8159 0x2020). The first RTL8156 (0x7020/0x7030) and OEM-ID adapters are not driven | `rtl.c` `rtl_identify_target`; issue 1 (an 8156B `REV_3104` was not found by 0.1.0, fixed in 0.2.0) |
| Bring-up is Linux `r8152`'s `r8156_init` / `rtl8156_up` / `rtl8156_enable` minus the PHY firmware, flash, power-saving and wake-on-LAN writes: the adapter's own flash brings the PHY up | `vendor_regs.c` tables |
| Every run on a console went through the ECM configuration's data interface once before the vendor configuration (not proven needed, never run without) | `adapter_pass_through_ecm` comment |
| Throughput: ~3.2 Gbit/s TCP up, ~3.0 down on the RTL8159 at 10G; 370–440 MB/s to an M.2 drive; 2.5G RTL8156B ~141 MB/s; 5G RTL8157 ~124 MB/s to extended storage. User-space USB (one bulk OUT in flight, 8 bulk IN pending) is the limit, not the link | README "实测速度"; `run_network` comment: two OUT transfers in flight were slower *and were in use when an early build powered the console off* |
| lwIP with a 2 MiB window, window scaling, SACK, 4096 pool buffers, 50 ms TCP timer | `lwipopts.h` |
| Link watch: PLA_PHYSTATUS once a second; a failed read counts as "no link" only after 3 in a row; a dropped link moves the page to the console's network after 3 s; it comes back as soon as link + address return | `main.c` `watch_link`, `watch_page` (0.3.2) |
| A bulk OUT that fails is retried with the endpoint reset; 8 failures in a row over 5 s = adapter lost | 0.3.1, from an RTL8157 log with `bulk OUT status: 18` |
| Rest mode: the monotonic clock stops; the reference reopens its own kernel listener after a clock jump. With the adapter, it did not survive (the run ends and asks for a replug) | `run_native`, README |
| After its exit, the README says to unplug and replug the adapter before the next run. Why is not written down | README "使用" step 5 |
| Known problems: "adapter lost" mid-transfer on an RTL8157 (issue 7, 0.3.2: some 100 GB copies fine, others failing after 1–30 GB); console power-offs during development, cause not found; "don't unplug while running"; no static address yet (issue 6: a switch with no DHCP server) | issues 6, 7; README "风险提醒" |
| Only TCP; the console's own services see every client as 127.0.0.1 | `forward.c` |

## How PS5Upload uses it

```
 PC ──LAN──▶ USB adapter ──USB bulk──▶ ps5u-usbnet (lwIP, its own address)
                                            │ joins each TCP connection to
                                            ▼
                                  127.0.0.1:9120 AVA1 / :9180 Minion  (the helper)
```

- **A separate process**, like the SMB daemon and the keyboard & mouse companion: a USB driver and a
  TCP/IP stack never run inside the helper, so a fault ends this daemon, not the helper. It also
  keeps this lwIP (sockets, Ethernet, DHCP) apart from the tailnet core's (NO_SYS, no Ethernet).
- **The helper finds the adapter** (`src/usbnet.c`): every 5 s it lists `/dev/ugen*` and reads the
  device descriptor of a node it has not seen (a read-only ioctl). While a supported adapter is
  there, the setting is on and the console is not going to rest or settling after a wake, it
  launches this ELF through the console's loader (:9021) and polls it over 127.0.0.1:9185 (a
  key-checked JSON line, `include/usbnet_proto.h`).
- **The daemon** (`main.c`) follows the reference's `run()`: identify the chip, find the vendor
  configuration, read the MAC from the ECM string descriptor, the ECM pass-through, bring-up,
  USB transfers, lwIP, link wait, filter, DHCP. Then it serves only PS5Upload's ports (`fwd.c`):
  AVA1 9120 always, Minion 9180 only while Minion's LAN access is on, with a PROXY line so Minion
  sees the real client (and never takes it for the console's own browser).
- **Address**: DHCP, link-local (169.254.x.y) when no server answers, or a static one set in the
  app (Settings › This PS5) for a switch with no DHCP server.
- **Fallback is the engine's**: the console's address always works. The engine prefers the adapter's
  address only while a quick TCP probe from that computer reaches it, and goes back to the console's
  address silently when it does not (cable out, no address, rest). The daemon reports "ready" ->
  "no_link" only after 3 s, so a blinking cable does not bounce transfers between the two.
- **Rest mode**: the earliest signs the helper has (power_phase.h: our standby request, the network
  probe failing with errno 163, a clock jump) stop the daemon cleanly (MAC stopped, transfers closed,
  configuration put back); it is started again only once the console is awake and settled. The
  daemon also stops by itself on errno 163 from a loopback socket or a clock jump. Open sockets in a
  frozen process crashed consoles in rest before (tailnet), so nothing is left running into rest.
- **Helper exit**: the daemon is asked to quit and waited for; one that outlives its helper quits by
  itself after 120 s without a word.
- **Time boxes**: every USB step has its own timeout (configuration change 5 s, link 15 s, polls of
  50 × 2 ms); a daemon silent 45 s after its launch is killed by pid and not started again until the
  adapter is plugged in again or the feature is turned off and on. Three exits in a row: the same.
- **Status**: chip, revision, USB ID, link rate, address and how it was got, frames, bytes, USB
  error counts by kind, link changes, forwarded connections and bytes; its log is
  `/data/ps5upload/usbnet/usbnet.log` (never a MAC address). The helper shows it through the
  `usbnet` method (Settings › This PS5) and Minion's Settings.

Left out of the reference on purpose: its web file manager, NAS client, PKG install, port forwarding
for other payloads' ports, FTP rewriting, and its own fallback page (PS5Upload has all of these
already, on the console's network).

## Not proven yet (no adapter here)

- Everything with an adapter on a console: bring-up through this daemon, the speed through the
  helper's AVA1 (the 127.0.0.1 splice costs one more copy than the reference's own server), and the
  "replug before the next run" question: the daemon is stopped at rest and helper exit, so a second
  bring-up without a replug is what users will hit. If it fails, the status says to replug.
- A cable pulled mid-transfer: AVA1 resumes on the console's address after the session drops; the
  time that takes was not measured.
- What the console does with the USB bus in rest mode.

Measured: with no adapter, the helper starts, the status says "none", transfers are unchanged.

## Build and test

`make` here (needs `PS5_PAYLOAD_SDK`), or the payload's own build, which embeds the stripped ELF.
Host tests (`make test-payload` at the root): `payload/tests/usbnet_selftest.c` (IDs, descriptors
from a real adapter, bulk aggregates, register requests, the link debounce, the PROXY line) and
`payload/tests/usbnet_dhcp_selftest.c` (this lwIP and `net.c` on the host, over a fake link, getting,
reading back and releasing a DHCP lease, and a static address).
