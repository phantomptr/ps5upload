# tailnet

A small, gaming-only Tailscale node in C11, for the PS5 helper payload (no fork, no tun)
and, through FFI, the Rust engine. The protocol facts it follows are in the research
spec `ps5upload-docs/research/2026-11-07-tailnet-protocol-spec.md`.

Status: **M1 (control)**. Keys, `/key` over TLS, the ts2021 Noise IK channel, HTTP/2 with
HPACK, register (pre-auth key or login URL), and the map long-poll into C structs.
DERP, disco/STUN, WireGuard and the IP layer are the next milestones.

```
include/tailnet.h   the public API (one context, an event callback, start/stop/pause/resume)
src/                crypto (BLAKE2s, HKDF, Salsa20, NaCl box), json, keys, net (TCP+BearSSL),
                    noise, hpack, h2, netmap, control, tailnet (loop thread)
tools/tnctl.c       CLI: tnctl login --control URL [--authkey K] --state DIR
tests/              unit tests (make test runs them under ASan + UBSan)
testbed/            local Headscale + Linux tailscaled in Docker; m1-check.sh
```

Build: `make` (host library and `build/host/tnctl`), `make test`, `make ps5` (needs
`PS5_PAYLOAD_SDK`; the payload provides Monocypher), `make ps5-link`, `make size`.
Dependencies are the `third_party/ava1` (Monocypher) and `third_party/bearssl` submodules.

Memory: everything is allocated in `tn_create()` from the caps in `tn_config` (about
5.3 MiB with the defaults, most of it the 4 MiB MapResponse buffer and the JSON token
array); nothing is allocated after that. Credits and licences: `NOTICE`.
