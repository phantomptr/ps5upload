# Engine

The Rust workspace: everything between the desktop app and the PS5.

The engine is a local HTTP service. The Tauri app doesn't talk to the
console directly — it calls the engine, which speaks FTX2 to the payload.
That split is deliberate: the same API is reachable from a browser, a
script, or CI, so nothing the app can do is locked inside the app. See
the self-hosted-engine entries in [`../FAQ.md`](../FAQ.md).

```
client (Tauri/React)  ──HTTP──▶  ps5upload-engine :19113
                                      │ FTX2 binary framing
                                      ▼
                            payload  :9113 transfer
                                     :9114 management
```

## Crates

| Crate | What it is |
|---|---|
| `ftx2-proto` | Wire format: frame type IDs, header layout, constants. Shared by everything that speaks FTX2. |
| `ps5upload-core` | The bulk of the logic — connection handling and socket tuning, transfer/resume/verification, filesystem and app RPCs, volume parsing, package install, saves, cheats, hardware, SMB, BPS patching. |
| `ps5upload-engine` | The Axum HTTP service (~100 routes), job tracking, SSE progress events, and the desktop + mobile entry points. |
| `ps5upload-pkg` | `.pkg` parsing — headers, entries, split-file sets. |
| `ps5upload-lab` | CLI for driving the payload's control channel by hand. Useful when you want one frame, not a workflow. |
| `ps5upload-tests` | Integration tests against an in-process mock FTX2 server. |
| `ps5upload-bench` | Throughput benchmarks. |

## Working on it

```sh
cargo test --workspace          # no PS5 required — mock server
cargo build --release -p ps5upload-engine
```

Run it standalone and point it at a console:

```sh
PS5_ADDR=<ip>:9113 cargo run -p ps5upload-engine
curl "http://127.0.0.1:19113/api/ps5/status?addr=<ip>:9114"
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
- **Two ports, two roles.** Transfer frames go to 9113, everything else to
  9114. The wrong port returns `wrong_port` rather than working oddly.
