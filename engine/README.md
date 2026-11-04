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

The AVA1 protocol itself (crates `ava1`, `ava1-gen`, `ava1-chaos`, its C library and spec) is
[github.com/phantomptr/ava1](https://github.com/phantomptr/ava1). The Rust crate comes from
crates.io ([`ava1`](https://crates.io/crates/ava1), pinned exactly in `engine/Cargo.toml` and
`client/src-tauri/Cargo.toml`); the C library the payload builds, `ava1-gen` and the unpublished
`ava1-chaos` come from the `third_party/ava1` submodule (`git submodule update --init`), checked
out at the same release (`make ava1-version-check`).

| Crate | What it is |
|---|---|
| `ps5upload-ava1` | ps5upload over AVA1: uploads, downloads, the helper's management methods (`protocol/ps5upload-ava1/app.toml`, generated with `make ava1-app-gen`), the connection pool. |
| `ps5upload-ctest` | Builds the payload's C (the AVA1 library from the submodule plus the payload's own AVA1 code) on the host so `cargo test` checks it against the Rust side. |
| `ps5upload-core` | The bulk of the logic — socket helpers, the management seam, archive inspection and plan previews, filesystem and app RPCs, volume parsing, package install, saves, cheats, hardware, SMB, BPS patching. |
| `ps5upload-engine` | The Axum HTTP service (several hundred routes), job tracking, SSE progress events, and the desktop + mobile entry points. |
| `ps5upload-pkg` | `.pkg` parsing — headers, entries, split-file sets. |
| `ps5upload-fpkg` | Builds fake packages (Workshop). |
| `ps5-dump-forge-ufs2`, `ps5-dump-forge-pfs` | Build UFS2 and PFS game images from a folder (Convert). |
| `ps5upload-lab` | CLI for driving the console by hand over AVA1: management calls, uploads, benchmarks (`bench`), pairing and chaos tools. Useful when you want one call, not a workflow. |
| `ps5upload-tests` | Integration tests that run against in-process and loopback AVA1 peers, with no console. |
| `ps5upload-unrar` | The RAR helper program: the only binary that contains UnRAR, holding nothing else but ps5upload's own code. The engine starts it for every RAR operation and talks to it over stdin/stdout (`ps5upload-core/src/rar_helper.rs`); it is shipped next to the engine (desktop app, Docker images, release assets). It must not depend on any other crate here — see `LICENSES/UnRAR-exception.md`. |

## Working on it

```sh
cargo test --workspace --exclude ps5upload-ctest   # no PS5 required — loopback peers
cargo test -p ps5upload-ctest -- --test-threads=1   # payload AVA1 C vs Rust, on the host
cargo fmt --all -- --check      # CI fails on this; client/src-tauri has its own
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

## Environment variables

For a self-hosted engine (Docker, a NAS, or a standalone program). None is needed on a desktop.

| Variable | What it does | Default |
|---|---|---|
| `PS5_ADDR` | The console's address, for calls that don't name one | none |
| `PS5UPLOAD_ALLOW_IP` | Who else may use the engine: addresses and CIDR ranges, comma-separated | loopback only |
| `PS5UPLOAD_ENGINE_PORT` | The port it listens on | `19113` |
| `PS5UPLOAD_BROWSE_ROOTS` | Folders the web UI's file picker opens on, comma-separated | the engine's home folder |
| `PS5UPLOAD_PKG_HOST_IP` | The address the PS5 is told to download packages from, when the engine can't work it out (Docker bridge networking, several networks) | detected |
| `PS5UPLOAD_BASE_URL` | URL path prefix, such as `/ps5/`. The web page is built with it ([REVERSE_PROXY.md](../REVERSE_PROXY.md)) | none |
| `PS5UPLOAD_DEVICE_NAME` | The name the console and other devices see | the computer's name |
| `PS5UPLOAD_DATA_DIR` | Where the engine keeps its identity and state | `~/.ps5upload` |
| `PS5UPLOAD_STATE_DIR` | Install history and console snapshots | `<data dir>/state` |
| `PS5UPLOAD_CACHE_DIR` | The artwork cache | inside the data dir |
| `PS5UPLOAD_FAKELIBS_DIR` | Fake libraries for backporting | inside the data dir |
| `PS5UPLOAD_LINK_DOWNLOAD_DIR` | Where downloads from a link go | `~/Downloads/ps5upload` |
| `PS5UPLOAD_FFMPEG` | The ffmpeg program Go live uses | the one the app installs |
| `PS5UPLOAD_7Z_THREADS` | Threads for unpacking `.7z`. Memory use grows with each | `1` |
| `PS5UPLOAD_URL_THREADS` | Parallel fetches when installing from a link, 1 to 32. Setting it turns off the automatic choice | `8`, adapting up to 32 |
| `PS5UPLOAD_PKG_SESSION_MAX_AGE_SEC` | How long a Stream & install's package address stays valid, at least 60 | `7200` |

Only in the desktop app: `PS5UPLOAD_SAFE_GRAPHICS=1` (Linux white-screen fix, see the FAQ) and
`PS5UPLOAD_UPDATE_MANIFEST_URL` (test an update from another address).

## Testing

`engine/crates/ps5upload-tests/tests/` runs against a loopback AVA1 console
(the Rust job host plus a scripted management node): single-file, folder,
small-file, resume-after-drop, retry classification, exclude rules,
`.zip`/`.7z`/`.rar` streaming, hardware commands, and volume parsing. Unit
tests live beside their modules.

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

## Where the web UI's file browser opens (`PS5UPLOAD_BROWSE_ROOTS`)

The browser-based UI picks local files and folders from the *engine's* filesystem. By default
that picker opens at the engine's home directory, which in the Docker images is `/data`, the
engine's own state volume, not the folder where you mounted your packages. Set
`PS5UPLOAD_BROWSE_ROOTS` to a comma-separated list of directories (for example `/pkgs`, or
`/pkgs,/games`) and the picker starts there instead. `compose.yaml` sets it to `/pkgs`, next to
the volume mount it describes.

- Unset or blank keeps the old behaviour (the home directory).
- An entry that is not a directory the engine can see is skipped and named in the engine log
  (a warning), so a typo or a volume that was never mounted is visible. If no entry is usable
  the picker falls back to the home directory.
- It only chooses where the picker opens. It is not a sandbox: the engine can still be asked
  for any path it can read.
- `HOME` is deliberately left alone, because FPKG conversions write under `$HOME/Downloads/fpkgs`
  and a package mount is usually read-only.

## Synology and other NAS boxes (saved servers: "Permission denied")

The Docker images keep their state (saved servers, install history, artwork cache) in `/data`
and run as UID:GID 65532 unless you override `user:`. If you bind-mount a host folder that
someone else owns, writing fails and the engine says so, naming the folder, the UID:GID it runs
as, and the owner it found (#361). Fix it one of two ways:

- set `user:` in the compose file to the folder's owner (`ls -ln` on the host shows the numbers),
  for example `user: "1234:77777"`; or
- `chown -R 65532:65532 <host folder>` for the mount.

`USER_ID` / `GROUP_ID` environment variables are not read by these images; use `user:`.
The account must also be allowed to write that folder in the NAS's own permission settings.
