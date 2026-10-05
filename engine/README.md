# Engine

The Rust workspace: everything between the desktop app and the PS5.

The engine is a local HTTP service. The Tauri app doesn't talk to the
console directly — it calls the engine, which speaks AVA1 to the payload.
That split is deliberate: the same API is reachable from a browser, a
script, or CI, so nothing the app can do is locked inside the app. See
the self-hosted-engine entries in [`../FAQ.md`](../FAQ.md).

```
client (Tauri/React)  ──HTTP──▶  ps5upload-engine :19113
                                      │ AVA1 (Noise XX, sealed frames)
                                      ▼
                            payload  :9120
```

## Crates

| Crate | What it is |
|---|---|
| `ava1` | The AVA1 protocol: frames, generated message codecs, Noise handshake, sessions, lanes, pairing. Spec: `protocol/ava1/SPEC.md`. |
| `ava1-gen` | Generates the Rust and C codecs from `protocol/ava1/schema/ava1.toml`. Run it after editing the schema. |
| `ava1-ctest` | Builds the payload's AVA1 C on the host so `cargo test` checks C against Rust. |
| `ava1-chaos` | A misbehaving TCP proxy (latency, bandwidth caps, blackholes, kills) for resilience tests. |
| `ps5upload-core` | The bulk of the logic — connection handling and socket tuning, transfer/resume/verification, filesystem and app RPCs, volume parsing, package install, saves, cheats, hardware, SMB, BPS patching. |
| `ps5upload-engine` | The Axum HTTP service (~100 routes), job tracking, SSE progress events, and the desktop + mobile entry points. |
| `ps5upload-pkg` | `.pkg` parsing — headers, entries, split-file sets. |
| `ps5upload-lab` | CLI for driving the payload's control channel by hand. Useful when you want one frame, not a workflow. |
| `ps5upload-tests` | Integration tests that run against in-process and loopback AVA1 peers, with no console. |
| `ps5upload-bench` | Throughput benchmarks. |

## Working on it

```sh
cargo test --workspace          # no PS5 required — loopback peers
cargo build --release -p ps5upload-engine
```

Run it standalone and point it at a console:

```sh
PS5_ADDR=<ip> cargo run -p ps5upload-engine
curl "http://127.0.0.1:19113/api/ps5/status?addr=<ip>"
```

`PS5UPLOAD_ALLOW_IP` lets another machine reach it. The API is
**unauthenticated and can read, write and delete files on the console** —
keep it on a trusted LAN.

## Testing

`engine/crates/ps5upload-tests/tests/` runs against a loopback mock
server: single-file, streaming, directory, packed small-file shards,
resume-after-drop, retry classification, digest mismatch, exclude rules,
`.zip`/`.7z` streaming, hardware commands, and volume parsing. Unit tests
live beside their modules.

Anything touching real transfer behaviour still needs hardware — see
[`../TESTING.md`](../TESTING.md).

## Things that will bite you

- **The payload emits JSON; serde parses it.** Field names must be
  `snake_case` or serde silently leaves the field at its default — a zero
  or an empty string, not an error. One malformed byte rejects the whole
  response, so anything interpolated into a payload-side JSON string has
  to be escaped first.
- **`ok: false` inside a 200 is a refusal, not a success.** Several
  payload commands answer that way. `client/src/api/ps5.ts` has an
  `assertOk()` for action endpoints; status endpoints keep `ok:false` as
  data because there it means "unsupported on this console".
- **One port, one session.** Everything goes to 9120 as AVA1: management
  calls on the session's control connection, bulk data on its lanes.
- **A stream install has the PS5 fetch the package from the engine.** The
  engine tells the console an address to connect to; behind Docker's bridge
  network, a VPN or a virtual adapter that address is one the PS5 cannot
  reach. `PS5UPLOAD_PKG_HOST_IP` pins the LAN IP the console is given (publish
  port 19113 when the engine is in a container; `compose.yaml` uses host
  networking instead). Every stream-unreachable failure — the pre-install
  reach check, a Sony refusal before any byte was fetched, and an accepted
  install the console never fetched from — ends in `install/mod.rs`
  `stream_unreachable_hint_for`, which names this variable.
