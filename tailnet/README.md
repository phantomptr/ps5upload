# tailnet

A small, gaming-only Tailscale node in C11, for the PS5 helper payload (no fork, no tun)
and, through FFI, the Rust engine. The protocol facts it follows are in the research
spec `ps5upload-docs/research/2026-11-07-tailnet-protocol-spec.md`.

Status: **M4 (engine + apps)**. Control (M1): keys, `/key` over TLS, the ts2021 Noise IK
channel, HTTP/2 with HPACK, register (pre-auth key or login URL), the map long-poll,
lite updates with our endpoints and home DERP, Retry-After, a key-expiry warning.
Data plane (M2): one poll() thread with the UDP socket (STUN / disco / WireGuard demux),
non-blocking DERP over TLS 1.2, netcheck (home region), disco ping/pong/call-me-maybe
with Go's path timers, WireGuard (IKpsk2, zero PSK) with timers, replay window and
cookies, IPv4 checks and an ICMP echo responder. Interop with a stock tailscaled:
`testbed/m2-check.sh`.
Forwarder (M3, `src/fwd.c`): `tn_config.serve` turns the node into a server of a fixed
port list: the allow-list AND the tailnet ACL decide what gets in; UDP is relayed to
127.0.0.1, TCP is terminated in lwIP 2.2.1 (`lwip-port/`, NO_SYS, IPv4+TCP) and spliced
to 127.0.0.1 with back-pressure; a SYN is held until the local dial succeeds (closed
port = RST). The PS5 helper uses it (`payload/src/tailnet_node.c`); `testbed/m3-check.sh`
runs the same configuration against a stock tailscaled.
Engine (M4): `tn_map()` is the reverse of `serve`: a 127.0.0.1 port (TCP listener or UDP
socket) that reaches a peer's tailnet address. `engine/crates/ps5upload-tailnet` builds this
core with cc-rs for every shipping target and wraps it; the engine's router
(`ps5upload_core::route`) sends the helper, Remote Play and discovery through such mappings
when a console's home address does not answer. Platforms: `src/os.{h,c}` (POSIX, Winsock);
`testbed/cross-check.sh` compiles macOS, iOS, Android, Linux (glibc, musl), Windows (MinGW
headers + link) and the PS5; `testbed/m4-check.sh` runs the real engine against our PS5-config
node, direct and DERP-only.

```
include/tailnet.h   the public API (one context, an event callback, start/stop/pause/resume)
src/                crypto (BLAKE2s, HKDF, Salsa20, NaCl box), json, keys, net (TCP+BearSSL),
                    noise, hpack, h2, netmap, control, tailnet (control thread),
                    wg (WireGuard), disco (+STUN), ip, derp (non-blocking client),
                    dp (the data-plane poll loop, paths, netcheck, inbound filter),
                    fwd (serve rules: UDP relay + lwIP TCP splice)
lwip-port/          lwipopts.h and arch/cc.h for third_party/lwip
tools/tnctl.c       CLI: tnctl login --control URL [--authkey K] --state DIR
                    [--ping IP | --udp-echo IP:PORT] (RTT + path per probe)
tests/              unit tests (make test runs them under ASan + UBSan)
testbed/            Headscale + derper (TLS, STUN) + Linux tailscaled + a tnctl box in
                    Docker; m1-check.sh (control), m2-check.sh (data plane)
```

Build: `make` (host library and `build/host/tnctl`), `make test`, `make win` (MinGW-w64
compile check), `make ps5` (needs
`PS5_PAYLOAD_SDK`; the payload provides Monocypher), `make ps5-link`, `make size`.
Dependencies are the `third_party/ava1` (Monocypher), `third_party/bearssl` and
`third_party/lwip` submodules.

Memory: everything is allocated in `tn_create()` from the caps in `tn_config` (about
6.4 MiB with the defaults: the 4 MiB MapResponse buffer, the JSON token array, peers,
4 DERP connections with their TLS buffers); nothing is allocated after that. A PS5
helper should lower `map_msg_max`, `json_tokens_max` and `max_peers`. Credits and licences: `NOTICE`.
