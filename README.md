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
  <img alt="firmware" src="https://img.shields.io/badge/PS5_firmware-9.00_–_11.60_validated_•_others_best_effort-orange" />
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
- **Compressed `.zip` uploads** — keep a game dump as a single `.zip`
  on your PC (less disk, easier to move) and upload it directly.
  ps5upload decompresses on the host and streams the files into the
  same FTX2 pipeline, so they land **already extracted** on the PS5 —
  no manual unzip, no temp copy of the whole game. The Upload screen
  previews the expansion (`zipped → extracted`, file count, space
  saved) and detects the embedded game. Decompresses one file at a
  time (large files spill to a temp file), so a 100 GB archive doesn't
  need 100 GB of RAM. ZIP only — unpack `.rar` on the PC first.
- **Native image mount** — attach `.exfat` and `.ffpkg` images on
  the PS5 (MDIOCATTACH + nmount) with no third-party helper. Every
  mount survives payload restarts and auto-reconciles on startup.
- **Browse everything** — list games anywhere on the PS5 (including
  inside mounted images), disk images, files, and volumes. Run FS
  ops (chmod, delete, move, copy, mkdir) with a real directory
  tree. Bulk delete of a 200k-file folder shows live progress
  with a working Stop button.
- **Hardware view** — model, serial, uptime, storage, RAM, and the PS5
  date/time, refreshed live; plus a fan-threshold control that rings
  through to `/dev/icc_fan` for quieter operation. Live CPU/SoC
  temperature, clock, and power are read **on demand** (a button), not
  auto-polled — each read briefly pauses the system UI, so polling them
  could destabilize the console. They're also unreliable on current
  firmware (libkernel exports drift between FW points), so a reading may
  show `—`; everything else on the panel is stable.
- **Send any payload** — push `.elf`, `.bin`, `.js`, `.lua`, or
  `.jar` files to the PS5's loader port (typical defaults: `.elf` →
  9021 elfldr, `.js` → 50000 WebKit-stage, `.lua` → 9026, `.jar` →
  9025 BD-JB / BDJ; custom loaders may listen anywhere). Recent-sends
  history with click-to-replay and per-row success/fail badges.
- **Install fakepkgs** — pick a `.pkg`, click Install. Default is
  **Stream install (DPI 2.0)**: desktop serves the bytes over HTTP +
  BGFT pulls + installs in one pass (no 2× disk space, native pause/
  resume). The legacy "upload then install" pipeline is one click
  away on the failure card for LANs where the PS5 can't reach the
  desktop's HTTP port. When the legacy path runs: bytes staged on
  PS5-local disk → install fires under
  ShellUI's authid via ptrace RPC → register / launch from the
  Library row. Verified end-to-end on FW 9.60. Game pkgs (CUSA /
  PPSA / PCSA / EP / UP) install cleanly; system pkgs (NPXS-prefix —
  Store updates, Settings) get registered fire-and-forget with
  on-PS5 verification (Sony's API isn't designed for system patches,
  use the on-PS5 Settings → Debug Settings → Game → Package
  Installer for those).
- **Register + launch** — Library row's Play button always registers
  first (idempotent if already registered), retries with DRM-type
  patch on rejection, then launches. Unmount unregisters every
  title inside the image first so the dashboard stays clean —
  no ghost tiles after unmount.

## What it doesn't do

- **System pkg patches.** `sceAppInstUtilInstallByPackage` is built
  for game pkgs; NPXS-prefix system pkgs (Store updates, Settings
  app patches) register but the install path freezes Sony's mgmt
  service mid-flight on most firmwares. Use the on-PS5 Settings →
  Debug Settings → Game → Package Installer for those.
- **`.rar` archives.** Only `.zip` is supported for compressed uploads.
  Modern scene `.rar` is typically split multi-part + encrypted, which
  isn't worth the unrar maintenance/licensing tax — and no other PS5
  homebrew tool supports it either. Unpack `.rar` on the PC first.

## A quick look

<img width="2560" height="1411" alt="Screenshot 2026-04-24 at 00 26 13" src="https://github.com/user-attachments/assets/b72b65b5-a0d4-4e63-aa79-8b5ed2246f43" />

## Install

Pre-built downloads land on the
[Releases page](https://github.com/phantomptr/ps5upload/releases):

| Platform | File | How to install |
|---|---|---|
| macOS (Apple Silicon / Intel) | `PS5Upload-<ver>-mac-{arm64,x64}.dmg` | Open the `.dmg`, drag PS5Upload into Applications. See **First launch on macOS** below — Gatekeeper blocks downloaded apps the first time. |
| Windows (x64 / ARM64) | `PS5Upload-<ver>-win-{x64,arm64}.zip` | Unzip, double-click `PS5Upload.exe` — portable, no installer. See **First launch on Windows** — SmartScreen warns on first run. |
| Linux (x64 / ARM64) | `PS5Upload-<ver>-linux-{x64,arm64}.zip` | Unzip, then `chmod +x PS5Upload.sh PS5Upload.AppImage` and run **`./PS5Upload.sh`** (the wrapper — handles the FUSE-less and WebKit white-screen cases for you). Running `./PS5Upload.AppImage` directly also works if your system has libfuse2 and a happy WebKitGTK. |

### First-launch warnings (and why they're there)

ps5upload is not code-signed with paid OS certificates — same as
every other PS5 scene tool. The OS's verification layers
(Gatekeeper on macOS, SmartScreen on Windows) treat unsigned downloads
as suspicious until you allow them once. The one-time bypass:

**macOS** — "App is damaged" or "cannot be opened":
```bash
xattr -dr com.apple.quarantine /Applications/PS5Upload.app
```
That removes the *quarantine* attribute macOS slaps on every file
downloaded from a browser. Alternatively, right-click PS5Upload in
Applications → **Open** → click **Open** in the prompt. Either method
only needs to be done once per install.

**Windows** — "Windows protected your PC" SmartScreen prompt:
- Click **More info** → **Run anyway**.
- Only shown until SmartScreen builds reputation for the binary;
  subsequent launches are silent.
- If your IT policy blocks "Run anyway", unzip + right-click
  `PS5Upload.exe` → **Properties** → check **Unblock** → **OK**.

**Linux** — no equivalent warning. Just `chmod +x` and launch.

### System requirements

- **macOS** 11 (Big Sur) or newer. No dependencies — ad-hoc signed,
  see *First launch* above for the one-line quarantine bypass.
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
make install       # bootstrap dev env (auto-detects host OS)
make build         # payload ELF + engine + client UI
make run-client    # launch the Tauri dev app
```

`make install` auto-detects your OS and runs one of:

- **`make install-ubuntu`** — Debian / Ubuntu / WSL2: `apt` deps for Tauri
  (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev`,
  `libayatana-appindicator3-dev`, `libxdo-dev`, `libssl-dev`,
  `build-essential`), Node.js 22 LTS via NodeSource (only if missing),
  Rust via rustup, and PS5 Payload SDK v0.38 → `~/ps5-payload-sdk`.
- **`make install-macos`** — macOS: Xcode CLT, Homebrew, `node`, `llvm@18`
  (the only Homebrew llvm shipped with `ld.lld` — required by
  `prospero-clang`), Rust via rustup, and PS5 Payload SDK.
- **`make install-windows`** — Windows 11: Node.js LTS, Rust, VS 2022 Build
  Tools (C++ workload), WebView2 Runtime, 7-Zip, and PS5 Payload SDK
  via `winget`. Run from an elevated PowerShell (or any shell with
  `pwsh` / `powershell.exe` on PATH).

All three install scripts are idempotent — re-running them after a partial
setup is safe; each step skips if already satisfied.

For per-platform bundles only (no full dev env): `make dist-mac`,
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
   └── spawns ── ps5upload-engine (HTTP :19113)
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
- **`client/`** — Tauri 2 desktop app. Tauri IPC commands proxy to the
  sidecar HTTP engine, keeping the engine usable from CLI / CI too.

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
| **1.00 – 8.60** and **12.00 – 12.70** | All features run |

The process-list feature (Hardware tab's process snapshot) reads
`kinfo_proc` via `sysctl(KERN_PROC_PROC)` with field offsets that
have been stable across every SDK-supported firmware — pid at
byte 72, thread name at byte 447. No firmware-specific fallback
table required; real command names appear across the full 1.00 –
12.70 range. Transfer, mount, file browse, hardware monitor
(except CPU/SoC temps, which Sony gates on a different credential
check unrelated to firmware), and FS ops work identically across
all supported firmwares.

**What actually gates users in practice is the ELF loader** on
port 9021 — a third-party component, not part of ps5upload. The
ecosystem's real-world coverage is roughly **4.x through 12.x**;
below 4.x is obscure and above 12.70 is future work.

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

**Q: How do I install / launch a game from the Library tab?**
* The 2.2.26 Library row exposes **Mount** for `.exfat` / `.ffpkg` /
  `.ffpfs` images plus **Register** / **Register (patch DRM)** /
  **Launch** / **Unregister** buttons on the games inside.
* **Mount** is hardware-validated on FW 9.60 — a 76 GiB UFS
  `.ffpkg` mounts on `/dev/lvd1`, appears in Volumes with the
  correct `source_image`, and unmounts cleanly. The new round's
  payload uses compile-time `-lSce*` linkage so the rtld
  initialises the Sony sprx state via `DT_NEEDED` before main()
  runs.
* **Register** is hardware-validated: pointing it at a folder
  game with `eboot.bin` + `sce_sys/param.json` succeeds end to
  end — `sceAppInstUtilAppInstallTitleDir` returns 0, the title
  appears in `app.db` with the correct title name, and a nullfs
  bind is installed at `/system_ex/app/<title_id>`. Idempotent —
  re-registering the same path returns the same result.
* **Register (patch DRM)** rewrites the source's
  `sce_sys/param.json`'s `applicationDrmType` to `"standard"`
  before staging — needed for PSN-extracted dumps that ship with
  `"PSN"` or `"disc"`. Modifies the source file in place; only
  use when a normal Register fails with a DRM error.
* **Launch** routes `sceLncUtilLaunchApp` through a ptrace RPC
  into `SceShellUI` so Sony's `getpid() == SceShellUI.pid`
  caller-context check passes natively. Hardware-validated on
  FW 9.60: hitting Launch on a registered title actually starts
  the game (SoC power draw confirms the title comes up). Falls
  back to a direct call when ShellUI RPC isn't available.
* **Listing inside an active mount** (`/mnt/ps5upload/<name>/...`)
  is gated by the PS5 sandbox / LVD mount permission set rather
  than payload privilege. We re-check after every credential
  elevation; treat "ENOTDIR descending into a mount" as
  expected for now. Other PS5-side tools see the mount via
  `/mnt/ps5upload/`, and the source path is recorded in our
  tracker so reconcile-on-next-boot keeps state consistent.
* Live CPU/SoC temperature, clock, and SoC power are read **on demand**
  (the Hardware tab's "Read sensors" button), not on a timer. Each read
  briefly ptrace-pauses the system UI, and doing that on a loop could
  destabilize the console — it powered some consoles off — so the auto-
  refresh now covers only the ptrace-free data (info, uptime, storage,
  date/time). These readings are also unreliable across firmware
  revisions: libkernel exports them by NID-only on some FW points and the
  call returns garbage or fails, so a persistent `—` usually means your
  firmware's exports are NID-only, not that the payload is broken.

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
  * **Windows:** `%APPDATA%\com.phantomptr.ps5upload`
  * **macOS:** `~/Library/Application Support/com.phantomptr.ps5upload`
  * **Linux:** `~/.local/share/com.phantomptr.ps5upload`

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

> **Use this software entirely at your own risk.** It is provided
> "as is", without warranty of any kind, express or implied,
> including but not limited to warranties of merchantability, fitness
> for a particular purpose, and non-infringement.

You are solely responsible for how, where, and on what hardware you
use this tool. By downloading, installing, or running it, you
acknowledge and accept that:

- **It interacts with a modified PS5.** This tool only works on a
  console that has been jailbroken / has kernel exploits loaded by
  the user. Modifying console state, bypassing platform integrity
  checks, or running unsigned code may void your manufacturer
  warranty, violate the platform's terms of service, and — under
  certain operations — leave your console unrecoverable without a
  reinstall. You took those steps before this tool entered the
  picture; this tool does not put you in that state and cannot
  reverse it.
- **It writes to your PS5's filesystem and can install / register
  packages with Sony's installer.** Mistakes can corrupt the
  console's app database, leave orphaned mount points, or wedge
  Sony's mgmt service mid-install. Recovery normally means a
  reboot or — worst case — a factory reset. Back up anything
  important before bulk operations.
- **It is intended for use only with content you legally own and
  hardware that belongs to you.** Using it to install, mount, or
  distribute software you do not have the legal right to use is
  your responsibility, not the project's.
- **No support is guaranteed.** This is a free, volunteer-built
  tool. The author may answer questions on Discord but is under no
  obligation to provide fixes, updates, or compensation if anything
  goes wrong.

If any of the above is not acceptable to you, do not use this
software.

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

## Author

Created and maintained by **PhantomPtr**.

* [Follow me on X (@phantomptr)](https://x.com/phantomptr)

## Support

If you find this tool useful, consider buying me a coffee!

* Discord server: [https://discord.gg/fzK3xddtrM](https://discord.gg/fzK3xddtrM)
* Support me on Ko-fi: [https://ko-fi.com/B0B81S0WUA](https://ko-fi.com/B0B81S0WUA)

[![Support me on Ko-fi](https://storage.ko-fi.com/cdn/kofi3.png?v=3)](https://ko-fi.com/B0B81S0WUA)
