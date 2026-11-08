# tailnet

A small, gaming-only Tailscale node in C11, for the PS5 helper payload (no fork, no tun)
and, through FFI, the Rust engine. The protocol facts it follows are in the research
spec `ps5upload-docs/research/2026-11-07-tailnet-protocol-spec.md`.

Status: **M2 (data plane)**. Control (M1): keys, `/key` over TLS, the ts2021 Noise IK
channel, HTTP/2 with HPACK, register (pre-auth key or login URL), the map long-poll,
lite updates with our endpoints and home DERP, Retry-After, a key-expiry warning.
Data plane (M2): one poll() thread with the UDP socket (STUN / disco / WireGuard demux),
non-blocking DERP over TLS 1.2, netcheck (home region), disco ping/pong/call-me-maybe
with Go's path timers, WireGuard (IKpsk2, zero PSK) with timers, replay window and
cookies, IPv4 checks and an ICMP echo responder. Interop with a stock tailscaled:
`testbed/m2-check.sh`.

```
include/tailnet.h   the public API (one context, an event callback, start/stop/pause/resume)
src/                crypto (BLAKE2s, HKDF, Salsa20, NaCl box), json, keys, net (TCP+BearSSL),
                    noise, hpack, h2, netmap, control, tailnet (control thread),
                    wg (WireGuard), disco (+STUN), ip, derp (non-blocking client),
                    dp (the data-plane poll loop, paths, netcheck)
tools/tnctl.c       CLI: tnctl login --control URL [--authkey K] --state DIR
                    [--ping IP | --udp-echo IP:PORT] (RTT + path per probe)
tests/              unit tests (make test runs them under ASan + UBSan)
testbed/            Headscale + derper (TLS, STUN) + Linux tailscaled + a tnctl box in
                    Docker; m1-check.sh (control), m2-check.sh (data plane)
```

Build: `make` (host library and `build/host/tnctl`), `make test`, `make ps5` (needs
`PS5_PAYLOAD_SDK`; the payload provides Monocypher), `make ps5-link`, `make size`.
Dependencies are the `third_party/ava1` (Monocypher) and `third_party/bearssl` submodules.

Memory: everything is allocated in `tn_create()` from the caps in `tn_config` (about
6.4 MiB with the defaults: the 4 MiB MapResponse buffer, the JSON token array, peers,
4 DERP connections with their TLS buffers); nothing is allocated after that. A PS5
helper should lower `map_msg_max`, `json_tokens_max` and `max_peers`. Credits and licences: `NOTICE`.
