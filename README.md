# ps5upload

<p align="center">
  <img src="logo.png" alt="ps5upload" width="420" />
</p>

<p align="center">
  <strong>Fast, reliable transfers from your computer to your PS5.</strong><br/>
  Transfer · Mount · Browse — designed to live alongside your PS5-side tools.
</p>

<p align="center">
  <a href="https://github.com/phantomptr/ps5upload/releases"><img alt="release" src="https://img.shields.io/github/v/release/phantomptr/ps5upload?display_name=tag&sort=semver&color=blue" /></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-GPL--3-green" /></a>
  <img alt="platforms" src="https://img.shields.io/badge/platforms-macOS_·_Linux_·_Windows-lightgrey" />
  <img alt="firmware" src="https://img.shields.io/badge/PS5_firmware-1.00_–_12.70-orange" />
  <a href="https://discord.gg/fzK3xddtrM"><img alt="discord" src="https://img.shields.io/badge/discord-join-5865F2" /></a>
</p>

---

## What it does

- **Fast transfer** — FTX2 binary protocol with BLAKE3 per-shard
  verification, small-file packing, and resume on disconnect.
  Uses your LAN flat-out. Pack worker absorbs transient
  `EIO`/`EMFILE` hiccups so a 200k-file game upload doesn't get
  killed by one unlucky syscall.
- **Upload queue** — line up multiple games or images, hit Start,
  walk away. Every running row shows live MiB/s and ETA; done
  rows show the wall-clock-average rate so you can spot a slow
  destination. Queue state survives app restarts.
- **Native image mount** — attach `.exfat` and `.ffpkg` images on
  the PS5 (MDIOCATTACH + nmount) with no third-party helper. Every
  mount survives payload restarts and auto-reconciles on startup.
- **Browse everything** — list games anywhere on the PS5 (including
  inside mounted images), disk images, files, and volumes. Run FS
  ops (chmod, delete, move, copy, mkdir) with a real directory
  tree. Bulk delete of a 200k-file folder shows live progress
  with a working Stop button.
- **Live hardware view** — model, serial, uptime, CPU frequency,
  RAM; plus a fan-threshold control that rings through to
  `/dev/icc_fan` for quieter operation.
- **Send any payload** — push `.elf`, `.bin`, `.js`, or `.lua`
  files to the PS5's loader port (9021). Recent-sends history with
  click-to-replay and per-row success/fail badges.

## What it doesn't do

- **Install / launch titles in XMB.** Sony's installer and launcher
  APIs require running inside the PS5's ShellCore process. Use a
  dedicated PS5-side installer (send it via the Send payload tab);
  ps5upload stays out of that path on purpose.

## A quick look

<img width="2560" height="1411" alt="Screenshot 2026-04-24 at 00 26 13" src="https://github.com/user-attachments/assets/b72b65b5-a0d4-4e63-aa79-8b5ed2246f43" />

## Install

Pre-built downloads land on the
[Releases page](https://github.com/phantomptr/ps5upload/releases):

| Platform | File | How to install |
|---|---|---|
| macOS (Apple Silicon / Intel) | `PS5Upload-<ver>-mac-{arm64,x64}.dmg` | Open the `.dmg`, drag PS5Upload into Applications |
| Windows (x64 / ARM64) | `PS5Upload-<ver>-win-{x64,arm64}.zip` | Unzip, double-click `PS5Upload.exe` — portable, no installer |
| Linux (x64 / ARM64) | `PS5Upload-<ver>-linux-{x64,arm64}.zip` | Unzip, `chmod +x PS5Upload.AppImage`, then `./PS5Upload.AppImage` |

### System requirements

- **macOS** 11 (Big Sur) or newer. No dependencies — ad-hoc signed,
  right-click → Open on first launch to bypass Gatekeeper.
- **Windows** 10 (build 19041+) or Windows 11. Ships with the
  Microsoft Edge WebView2 runtime by default; LTSC / stripped
  installs may need
  [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)
  installed once.
- **Linux** needs a few system libraries that the `.AppImage` expects
  on the host (libfuse2, gtk3, webkit2gtk 4.1, libsoup3,
  libappindicator, librsvg2). Install commands for
  Debian/Ubuntu/Fedora/RHEL/Arch are in
  [the FAQ](FAQ.md#prerequisites).

The app checks GitHub for updates once per launch (Settings → Updates)
and downloads a fresh archive to your Downloads folder when you click
Download — replace the old app manually and relaunch.

Building from source:

```bash
git clone https://github.com/phantomptr/ps5upload.git
cd ps5upload
make dist          # one-shot: payload + engine + client + bundle
```

Platform-specific bundle targets: `make dist-mac`,
`make dist-mac-x64`, `make dist-linux`, `make dist-linux-arm`,
`make dist-win`, and `make dist-win-arm`.

## Quick start

1. Launch ps5upload on your computer.
2. Open the **Connection** tab and enter your PS5's IP address.
3. Click **Check**, then **Send payload**.
4. Once the third step turns green, go to any other tab — you're
   connected.

The payload stays loaded until the PS5 reboots or goes into rest mode.

## Architecture

```
client/ (Tauri 2 · React · TypeScript)
   │
   ├── in-process ── engine crates (ps5upload-core, ftx2-proto)
   │
   └── OR spawns ── ps5upload-engine (HTTP :19113, optional)
                            │
                            ▼  FTX2 binary framing
                  payload/ps5upload.elf  (PS5 C payload, ports 9113 + 9114)
```

Three layers:

- **`payload/`** — C payload that runs on the PS5 (FreeBSD 11).
  Ports 9113 (transfer) + 9114 (management). Handles FTX2 framing,
  BLAKE3 verification, mount pipelines, and FS ops.
- **`engine/`** — Rust workspace with the protocol types, transfer
  logic, HTTP service, lab CLI, mock server, and benchmarks.
- **`client/`** — Tauri 2 desktop app. Uses the engine crates
  in-process via Tauri IPC; the HTTP engine is available as a
  separate process for CLI / CI use.

## Build

All workflows go through the root `Makefile` (see `make help`).

| Target | What it does |
|---|---|
| `make build` | Payload + engine + client |
| `make payload` | PS5 ELF at `payload/ps5upload.elf` (requires `PS5_PAYLOAD_SDK` env var) |
| `make engine` | `cargo build --workspace` |
| `make client` | `vite build` in `client/` |
| `make run-client` | Tauri dev (vite + Rust main process) |
| `make run-engine` | `ps5upload-engine` on `localhost:19113` |
| `make send-payload` | Send the built ELF to `PS5_HOST:PS5_LOADER_PORT` |
| `npm run validate` | Full non-hardware quality gate |
| `make quality` | Same full non-hardware quality gate via Make |
| `npm run coverage` | Generate frontend + Rust coverage reports |
| `make coverage` | Same coverage reports via Make |
| `make test` | Script syntax + engine tests + payload validation + client build |
| `make test-engine` | `cargo test --workspace` (no hardware needed) |
| `make dist` | Tauri bundle under `client/src-tauri/target/release/bundle/` |

## Test

Unit and integration tests run entirely against an in-process mock
FTX2 server — no PS5 needed:

```bash
make test-engine
```

Full local quality gate and coverage reports:

```bash
npm run validate
npm run coverage
```

Real-hardware smoke test (requires payload already loaded):

```bash
npm run smoke:hardware
```

See [`TESTING.md`](TESTING.md) for the complete mock-test, coverage,
cross-platform, and live-PS5 validation workflow.

## Tech stack

- **Payload** — C (FreeBSD 11), prospero-clang toolchain
- **Engine** — Rust (edition 2021), tokio + axum 0.8
- **Desktop client** — Tauri 2, React, TypeScript, Zustand,
  Tailwind CSS v4, Vite
- **Protocol** — FTX2 (custom binary framing, BLAKE3 shard
  verification)

## Supported platforms

**Desktop client**

| | x64 | arm64 |
|---|---|---|
| **macOS**   | ✓ | ✓ |
| **Linux**   | ✓ | ✓ |
| **Windows** | ✓ | ✓ |

**PS5 payload** — every firmware the PS5 Payload SDK supports,
currently **1.00 through 12.70** on every console model (original
CFI-1xxx, Slim CFI-2xxx, Pro CFI-7xxx, Digital). Built against SDK
v0.38, which ships per-firmware kernel offsets and resolves them at
payload startup via `kernel_get_fw_version()` — the same binary
runs on every supported firmware without per-release rebuilds.

| Range | Feature coverage |
|---|---|
| **9.00 – 11.60** | All features validated on hardware |
| **1.00 – 8.60** and **12.00 – 12.70** | All features run; one caveat below |

**One caveat outside 9–11**: the payload's process-list feature
(Hardware tab's process snapshot) displays entries as `<pid:N>`
placeholders instead of real command names. Why: the `p_comm` struct
offset isn't in the SDK's runtime-resolved table, and our per-
firmware fallback table only has a validated value for 9.x–11.x.
Everything else — transfer, mount, file browse, hardware monitor
(except CPU/SoC temps, which Sony gates on a different credential
check unrelated to firmware), FS ops — works identically across all
supported firmwares.

**What actually gates users in practice is the ELF loader** on
port 9021 (BD-JB, GoldHen, etaHEN, kstuff-lite, etc.) — a
scene-provided component, not part of ps5upload. That ecosystem's
real-world coverage is roughly **4.x through 12.x**; below 4.x is
obscure and above 12.70 is future work.

## FAQ

**Q: "Connection Refused" or it won't connect?**
* Did you load the payload first? The PS5 stops listening after a
  reboot or rest-mode cycle — send the payload again from the
  **Connection** tab.
* Is your computer's firewall blocking outbound connections to
  port 9113 / 9114 / 9021 on your PS5?
* Your computer and PS5 don't have to be on the same subnet, but
  there has to be a route to the IP.

**Q: Resume gets stuck on "Checking what's already on your PS5…"?**
* Reconcile scoped to the *local* tree's parent directories, so a
  single-file upload into a folder that already holds other games
  is now one `FS_LIST_DIR` call instead of a recursive walk of the
  whole destination. Update to the latest build.
* Safe-mode reconcile still hashes every same-size remote file via
  BLAKE3 — that's ~2–3 s per GiB on PS5 UFS. Use Fast mode for
  single-file or large-file transfers.

**Q: Do I need a LAN cable?**
* Not strictly, but Wi-Fi caps throughput well below what the PS5
  NIC can actually do. Plug in an Ethernet cable for the best
  experience.

**Q: Can I use this over the Internet?**
* Yes, technically. If you forward ports 9113 / 9114 to your PS5
  it will work. However, the FTX2 protocol is optimised for speed,
  not for authentication — we don't recommend exposing an
  exploited PS5 to the open Internet.

**Q: The Library tab doesn't have Install / Run buttons anymore —
where'd they go?**
* Sony's `sceAppInstUtil*` and `sceLncUtilLaunchApp` APIs wedge
  in-kernel when called from a standalone userland payload on
  firmware 9.60. Install / Run / Uninstall have been re-gated out
  of the UI on purpose. Install PKGs with a dedicated PS5-side
  installer (send it via **Connection → Send payload**) and launch
  game dumps straight from the PS5's XMB.

**Q: "No writable storage found"?**
* The tool blocks writes to read-only system partitions. If you
  want to write to a USB drive, make sure it's formatted (exFAT is
  best) and plugged in *before* you load the payload.

**Q: macOS: "App is damaged" or "Unidentified Developer"?**
* This is normal for unsigned apps. Right-click the app, select
  **Open**, then click **Open** in the dialog.
* If macOS still blocks it, **System Settings → Privacy &
  Security → Open Anyway**.
* Last resort — remove the quarantine flag:
  ```bash
  xattr -dr com.apple.quarantine /Applications/ps5upload.app
  ```
* No Apple Developer account is required; the app is intentionally
  unsigned.

**Q: Where are config and logs saved?**
* The desktop client uses the OS app-data directory:
  * **Windows:** `%APPDATA%\ps5upload`
  * **macOS:** `~/Library/Application Support/ps5upload`
  * **Linux:** `~/.local/share/ps5upload`

**Q: Does this work on PS4?**
* No. The payload is compiled specifically for the PS5 (FreeBSD
  11, Zen 2) and calls PS5-only kernel entry points.

**Q: What about older firmware (≤ 9.00)?**
* The FTX2 payload itself doesn't call firmware-gated APIs, but
  the ELF loader workflow on port 9021 depends on what your
  jailbreak exposes. Patches welcome.

**Q: Can I run ps5upload headless / over SSH?**
* Not the GUI. The `ps5upload-engine` binary speaks HTTP on
  `:19113` and exposes the full transfer / reconcile / FS API, so
  you can script transfers from a terminal or CI job without ever
  opening the desktop client.

## Contributing

- Report bugs:
  [GitHub Issues](https://github.com/phantomptr/ps5upload/issues)
- Pull requests welcome — please read
  `.github/PULL_REQUEST_TEMPLATE.md` and run `make test` locally
  before opening.

## Disclaimer

This software is for educational purposes. It is intended for use
with legally obtained software and homebrew applications on hardware
you own.

**Use at your own risk.** The authors are not responsible for any
data loss, bricked hardware, or other issues that arise from using
this tool.

## Third-Party Libraries

This software builds on the following open-source projects:

**Desktop client (Tauri 2 + React):**
* [Tauri](https://tauri.app/) — Rust-backed cross-platform desktop runtime
* [React](https://react.dev/) — UI library
* [Zustand](https://github.com/pmndrs/zustand) — Client state management
* [Tailwind CSS](https://tailwindcss.com/) — Styling
* [Vite](https://vitejs.dev/) — Build + dev server
* [lucide-react](https://lucide.dev/) — Icons
* [react-router](https://reactrouter.com/) — Routing

**Engine (Rust):**
* [tokio](https://tokio.rs/) — Async runtime
* [axum](https://github.com/tokio-rs/axum) — HTTP service
* [serde](https://serde.rs/) — Serialization
* [anyhow](https://github.com/dtolnay/anyhow) — Error handling
* [uuid](https://github.com/uuid-rs/uuid) — Job IDs

**Payload (PS5):**
* [PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk) — Open-source SDK for PS5 payload development
* [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) — Fast cryptographic hashing (per-shard verification)

## License

GNU General Public License v3.0 (GPLv3).
Free to use, free to modify. See [`LICENSE`](LICENSE).

## Credits

Created by **PhantomPtr**.

* [Follow me on X (@phantomptr)](https://x.com/phantomptr)

ps5upload builds on work and conventions from the PS5 homebrew
ecosystem (ELF loader port 9021, FreeBSD 11 mount layout, Sony
kernel API signatures). Specific attributions live in the commits
that introduced each piece.

## Support

If you find this tool useful, consider buying me a coffee!

* Discord server: [https://discord.gg/fzK3xddtrM](https://discord.gg/fzK3xddtrM)
* Support me on Ko-fi: [https://ko-fi.com/B0B81S0WUA](https://ko-fi.com/B0B81S0WUA)

[![Support me on Ko-fi](https://storage.ko-fi.com/cdn/kofi3.png?v=3)](https://ko-fi.com/B0B81S0WUA)
