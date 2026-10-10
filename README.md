# ps5upload

<p align="center">
  <img src="logo.png" alt="ps5upload" width="420" />
</p>

<p align="center">
  PS5 Upload gets your games, apps and homebrew onto a jailbroken PS5 quickly and reliably: uploads resume after a dropped connection, installs pick the route that works, and you can browse files, manage the console and run cheats from your computer, phone or browser.
</p>

<p align="center">
  <a href="https://github.com/phantomptr/ps5upload/releases"><img alt="release" src="https://img.shields.io/github/v/release/phantomptr/ps5upload?display_name=tag&sort=semver&color=blue" /></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-GPL--3-green" /></a>
  <img alt="platforms" src="https://img.shields.io/badge/platforms-macOS_·_Linux_·_Windows_·_Android_·_Web-lightgrey" />
  <img alt="firmware" src="https://img.shields.io/badge/PS5_firmware-1.00_–_13.60_supported_•_5.10,_9.60_&_13.60_tested-brightgreen" />
  <a href="https://discord.gg/fzK3xddtrM"><img alt="discord" src="https://img.shields.io/badge/discord-join-5865F2" /></a>
</p>

---

## What it does

- **Fast, resumable uploads** of games, folders, disk images and `.zip` / `.7z` / `.rar`
  archives (unpacked on the way, nothing extracted to your disk). Everything runs over
  **AVA1**, one encrypted connection on port 9120. Every file is verified, and an upload
  continues after a dropped connection, a helper restart or rest mode. A job that will not
  fit is refused up front, counting what is already on the console. Queue, live speed and ETA.
- **Install any package.** PS4 `.pkg` and PS5 fake packages (base, update, DLC) from your
  computer (**Stream & install** or **Upload & install**), a NAS/SMB share, a USB drive or a
  link. If the PS5 refuses one route, the app offers another.
- **Collection.** Every game in your folders (including folders on a NAS, scanned in place),
  grouped with its updates and DLC, with what each PS5 already has and one click to install the
  rest. Organize, de-duplicate and clean up the drives. Replaces PS Game Library.
- **Convert Games.** Build an installable package from a decrypted game folder or an
  image, or write a game folder as a game image (`.exfat`, `.ffpkg`, `.ffpfs`, or the smaller
  `.ffpfsc`) that ShadowMount+ mounts, and upload it in one step.
- **Browse and manage.** Files, games, disk images (mount, edit in place), saves, screenshots
  and video clips; register, launch, stop and uninstall games; copy, move (Move to…),
  delete, set permissions.
- **Cheats, fan curve, hardware view,** Remote Play pairing, payload sender with a catalogue
  and playlists, and backport tools.
- **Power.** Rest mode, reboot, shut down, and wake over the network (optionally straight
  into your signed-in user).
- **Runs everywhere:** macOS, Windows, Linux, Android, or any browser through the self-hosted
  Docker web UI. 21 languages.

## What it doesn't do

- **System package patches** (NPXS Store or Settings updates). Use the PS5's own
  Settings → Debug Settings → Game → Package Installer.
- **Sign packages with fake keys.** It only installs packages the console already trusts.

## A quick look

<img width="1790" height="1425" alt="image" src="https://github.com/user-attachments/assets/a455f9b6-a7d4-49c2-a127-bf923c61d901" />

## Install

Pre-built downloads land on the
[Releases page](https://github.com/phantomptr/ps5upload/releases):

| Platform | File | How to install |
|---|---|---|
| macOS (Apple Silicon / Intel) | `PS5Upload-<ver>-mac-{arm64,x64}.dmg` | Open the `.dmg`, drag PS5Upload into Applications. See **First launch on macOS** below — Gatekeeper blocks downloaded apps the first time. |
| Windows (x64 / ARM64) | `PS5Upload-<ver>-win-{x64,arm64}.zip` | Unzip, double-click `PS5Upload.exe` — portable, no installer. See **First launch on Windows** — SmartScreen warns on first run. |
| Linux — Debian / Ubuntu (x64 / ARM64) | `PS5Upload-<ver>-linux-{x64,arm64}.deb` | `sudo apt install ./PS5Upload-<ver>-linux-<arch>.deb` — installs a normal app with a menu entry; pulls in the WebKitGTK deps automatically. |
| Linux — Fedora / RHEL / Bazzite (x64 / ARM64) | `PS5Upload-<ver>-linux-{x64,arm64}.rpm` | `sudo dnf install ./PS5Upload-<ver>-linux-<arch>.rpm` (Bazzite/Silverblue: `rpm-ostree install`) — menu entry + auto deps. |
| Linux — any distro (x64 / ARM64) | `PS5Upload-<ver>-linux-{x64,arm64}.zip` | Universal fallback (no install). Unzip, then `chmod +x PS5Upload.sh PS5Upload.AppImage` and run **`./PS5Upload.sh`** (the wrapper — handles the FUSE-less and WebKit white-screen cases for you). Running `./PS5Upload.AppImage` directly also works if your system has libfuse2 and a happy WebKitGTK. |
| Linux — NixOS (x64 / ARM64) | no release artifact | Packaged in [NUR](https://github.com/GriefNorth/nur-packages) as `ps5upload` — see **NixOS** below. |
| Android | `PS5Upload-<ver>-android.apk` | Enable "install unknown apps" for your browser/file manager, then open the `.apk`. Same interface, mobile-friendly; connects to and manages your PS5 over Wi-Fi. On Android 17, allow local network access ("nearby devices") when asked. |

### NixOS

No NixOS artifact is published on the Releases page, but ps5upload is
packaged in [GriefNorth's NUR](https://github.com/GriefNorth/nur-packages) as
`ps5upload` (x64 and ARM64) — it wraps the same `.AppImage` and needs no
FUSE. With flakes:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nur.url = "github:GriefNorth/nur-packages";
  };

  outputs = { nixpkgs, nur, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [
            nur.packages.${pkgs.system}.ps5upload
          ];
        })
      ];
    };
  };
}
```

Then `nixos-rebuild switch --flake .#myhost` and launch **ps5upload** from
your app menu. Without flakes, call the package straight out of the repo —
and note that ps5upload bundles UnRAR for `.rar` support, so nixpkgs needs
`config.allowUnfree = true`:

```nix
{ pkgs, ... }:
{
  nixpkgs.config.allowUnfree = true;

  environment.systemPackages = [
    (pkgs.callPackage
      ((builtins.fetchTarball
        "https://github.com/GriefNorth/nur-packages/archive/refs/heads/main.tar.gz")
        + "/pkgs/ps5upload")
      { })
  ];
}
```

Or try it without installing anything:

```bash
nix run github:GriefNorth/nur-packages#ps5upload
```

The Nix expression pins one release version, so bump it there when a new
ps5upload comes out. Prefer to run the AppImage yourself? Prefer the
`./PS5Upload.sh` wrapper from the zip above — on NixOS the bare
`.AppImage` usually renders an empty window, because its bundled
`libwayland-client` shadows the host one and WebKitGTK then fails to create
an EGL display. The Nix package works around that for you.

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
- **Linux** — the `.deb` and `.rpm` packages **pull in their
  dependencies automatically** (WebKitGTK 4.1, gtk3, libsoup3, etc.) via
  the package manager, so they're the easiest route on Debian/Ubuntu and
  Fedora/RHEL/Bazzite. The universal `.AppImage` instead expects those
  libraries already on the host (libfuse2, gtk3, webkit2gtk 4.1,
  libsoup3, libappindicator, librsvg2); install commands for
  Debian/Ubuntu/Fedora/RHEL/Arch are in
  [the FAQ](FAQ.md#prerequisites).
  **Minimum distro version:** all three Linux artifacts are built against
  **glibc 2.39**, so they need a reasonably recent distro — **Ubuntu
  24.04+, Debian 13+ (trixie), Fedora 40+**, or equivalent. (This is a
  glibc floor, not a package dependency — the `.deb`/`.rpm` install fine
  on older releases but the app won't launch, and the AppImage bundles
  WebKitGTK yet still uses the host's glibc.) On an older distro, build
  from source with `make dist-linux` on that machine.

The app checks GitHub for updates once per launch (Settings → Updates)
and downloads a fresh archive to your Downloads folder when you click
Download. Replace the old app and relaunch; the app then offers to update the helper.

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
  Rust via rustup, and checksum-verified PS5 Payload SDK v0.43 →
  `~/ps5-payload-sdk`.
- **`make install-macos`** — macOS: Xcode CLT, Homebrew, `node`, current
  `llvm` (LLVM 22 at this release), Rust via rustup, and checksum-verified
  PS5 Payload SDK v0.43.
- **`make install-windows`** — Windows 11: Node.js LTS, Rust, VS 2022 Build
  Tools (C++ workload), WebView2 Runtime, 7-Zip, and PS5 Payload SDK
  via `winget`. Run from an elevated PowerShell (or any shell with
  `pwsh` / `powershell.exe` on PATH).

All three install scripts are idempotent — re-running them after a partial
setup is safe. A stale or unmarked SDK is upgraded to the repository pin and
moved to a timestamped backup instead of being deleted. Local and GitHub
Actions builds read the same tag and official release checksum from
`scripts/ps5-sdk.env`.

For per-platform bundles only (no full dev env): `make dist-mac`,
`make dist-mac-x64`, `make dist-linux`, `make dist-linux-arm`,
`make dist-win`, and `make dist-win-arm`.

## Quick start

1. Jailbreak the PS5 and keep an ELF loader running on port **9021**.
2. Launch ps5upload and open **Connection**. Enter the PS5's IP address.
3. Click **Check**, then **Send payload**. The app launches the helper and pairs with it by
   itself.
4. If you loaded the helper another way, the console shows a **6-digit code**. Enter it in the
   app once; the computer is remembered.

The helper stays until the PS5 reboots or enters rest mode. The app only needs ports **9120**
(the helper) and **9021** (the loader); allow both in your firewall. 6.0 cannot talk to a
v5.x helper: the app offers **Update the helper** and replaces it in one click.

## Architecture

```
client/ (Tauri 2 · React · TypeScript)   Android app · Docker web UI
   │
   └── spawns ── ps5upload-engine (HTTP :19113)
                          │
                          ▼  AVA1 (Noise-encrypted, port 9120)
                payload/ps5upload.elf  (PS5 C payload)
```

- **`payload/`**: C payload on the PS5 (FreeBSD 11). Transfers, BLAKE3 verification, mounts,
  file operations and every management call. Spec: [`protocol/ava1/SPEC.md`](protocol/ava1/SPEC.md).
- **`engine/`**: Rust workspace with the AVA1 client, transfer logic, HTTP API and lab tools.
- **`client/`**: Tauri 2 desktop app (and the Android and web builds), talking to the engine.

## Build and test

All workflows go through the root `Makefile` (`make help`).

| Command | What it does |
|---|---|
| `make build` | Payload + engine + client |
| `make run-client` | Tauri dev app |
| `make run-engine` | Engine on `localhost:19113` |
| `make test-engine` | Rust tests, no PS5 needed |
| `make test-payload` | Console C code compiled and run on the host |
| `npm run validate` | Full non-hardware gate |
| `make dist` | Tauri bundle |

See [`TESTING.md`](TESTING.md) for the full workflow, and [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Tech stack

- **Payload**: C (FreeBSD 11), prospero-clang, PS5 Payload SDK v0.43
- **Engine**: Rust, tokio + axum
- **Client**: Tauri 2, React, TypeScript, Zustand, Tailwind CSS v4, Vite
- **Protocol**: AVA1 (Noise XX handshake, ChaCha20-Poly1305, BLAKE3 verification)

## Supported platforms

- **Desktop:** macOS, Linux and Windows, x64 and arm64.
- **Android:** the `.apk` from Releases.
- **Web UI:** the self-hosted Docker image, in any browser on your LAN (see below).
- **PS5 firmware:** 1.00 through 13.60 on every model; one payload binary for all. Tested on
  5.10, 9.60 and 13.60. In practice the limit is your jailbreak's ELF loader on port 9021.

Fake package install needs live kernel patches (kstuff / fpkg-enable) on the console; where
they are missing the app says so instead of reporting a false success.

## Self-hosted engine and web UI

Run the engine on a NAS or server and use it from a browser, or point the desktop app at it
with **Settings → Engine URL**.

- Images: `ghcr.io/phantomptr/ps5upload-engine-webui` (full web UI) and
  `ghcr.io/phantomptr/ps5upload-engine` (engine only), tagged `:latest` or `:<version>`.
- Start from [`engine/compose.yaml`](engine/compose.yaml). It uses host networking (stream
  installs need the PS5 to reach the engine). Set `PS5_ADDR` and `PS5UPLOAD_ALLOW_IP`.
- Open `http://<host>:19113`. The file picker browses the engine's disk; mount your games
  folder into the container and set `PS5UPLOAD_BROWSE_ROOTS`.
- Map `/data` to a host folder (on Unraid, `/mnt/user/appdata/ps5upload`): it holds the pairing
  key, so the PS5 keeps trusting the web UI after the container is recreated.
- An upload keeps running when you close the tab.
- **No password.** Anyone allowed can read, write and delete on your PS5. Keep it on a trusted
  LAN, never on the internet.
- You may build the docker container with the `PS5UPLOAD_BASE_URL` to host the webui through a
  reverse proxy. This cannot be changed on runtime.

## FAQ

Common questions (connection problems, install errors, USB drives, firmware) are in
[`FAQ.md`](FAQ.md), also available inside the app.

## Contributing

- Report bugs:
  [GitHub Issues](https://github.com/phantomptr/ps5upload/issues)
- Pull requests welcome. Read [`CONTRIBUTING.md`](CONTRIBUTING.md) and run the checks in
  [`TESTING.md`](TESTING.md) before opening.

## Disclaimer

> **ps5upload is provided for research, educational, interoperability,
> and homebrew development purposes only.** It is not affiliated with,
> endorsed by, or connected to Sony Interactive Entertainment Inc.
> "PlayStation", "PS4" and "PS5" are trademarks of Sony Interactive
> Entertainment Inc., used here for identification only.
>
> **Use this software entirely at your own risk.** It is provided
> "as is", without warranty of any kind, express or implied,
> including but not limited to warranties of merchantability, fitness
> for a particular purpose, and non-infringement.

**➡ Full terms: [DISCLAIMER.md](DISCLAIMER.md).** By downloading,
building, installing, or using this software you accept them in full.
If you do not agree, do not use it.

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
- **No piracy.** This tool must not be used to copy, distribute, or
  run software you do not lawfully own or are licensed to use. The
  authors do not condone or assist with piracy in any form, and
  requests for such help will be refused.
- **You accept the real risks.** Operating a modified console can
  permanently damage it, irreversibly destroy saves and licences, and
  get your account or console banned from online services. Back up
  anything you cannot afford to lose *before* you begin.
- **Indemnity.** You agree to hold the authors, contributors, and
  distributors harmless from any claim arising out of your use or
  misuse of this software.

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
* [zip](https://github.com/zip-rs/zip2) + [sevenz-rust2](https://crates.io/crates/sevenz-rust2) — pure-Rust `.zip` / `.7z` extraction
* [unrar / unrar_sys](https://crates.io/crates/unrar) — `.rar` extraction on **desktop** (bundles the **UnRAR** source by Alexander Roshal; used to *extract only*, never to compress). See the required notice and the GPLv3 §7 linking exception in [`LICENSES/`](LICENSES/). The `unrar` wrapper (MIT OR Apache-2.0) is **vendored** at [`third_party/unrar`](third_party/unrar) carrying one local fix for an out-of-bounds read on multi-volume archives — upstream 0.5.8 has no fix; see that directory's README.

**Payload (PS5):**
* [PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk) — Open-source SDK for PS5 payload development; its startup code is **vendored** at [`payload/third_party/sdk-crt`](payload/third_party/sdk-crt) with one fix so payloads start on firmware where two kernel lookups fail (FW 5.50)
* [elfldr](https://github.com/ps5-payload-dev/elfldr) (GPLv3) — the ELF loader; a patched copy is **vendored** at [`third_party/elfldr`](third_party/elfldr) (fix for a loader that hangs on a silent client)
* [Monocypher](https://github.com/LoupVaillant/Monocypher) (CC0 / BSD-2) — AVA1's cryptography on the console (X25519, ChaCha20-Poly1305, BLAKE2b)
* [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) (CC0 / Apache-2.0) — fast hashing for AVA1 file verification
* [SQLite](https://sqlite.org/) (public domain) — reading the console's app database
* [tiny-AES-c](https://github.com/kokke/tiny-AES-c) (Unlicense) — MC4 cheat decryption

## Thanks

* [elf-arsenal](https://git.etawen.dev/soniciso/elf-arsenal) (soniciso, Sanad) — parts of the drive sensor, cheat and wake-watchdog code
* [PS5 Dump Forge](https://github.com/quer3q/ps5-dump-forge) (quer3q) — the UFS2 (`.ffpkg`) and PFS (`.ffpfs`) image writers, the streaming `.ffpfsc` container, reader and safety fixes to the package crates, and the ideas behind hash-checked images and spare space for read-write mounts
* [PROSPEROPatches](https://prosperopatches.com/), [ORBISPatches](https://orbispatches.com/) and [TMDB](https://www.themoviedb.org/) — game details and artwork
* [etaHEN PS5_Cheats](https://github.com/etaHEN/PS5_Cheats) and GoldHEN — cheat files
* Everyone who contributed code, translations and docs, and the Discord testers

ps5upload stands on the shoulders of the **PS5 homebrew scene**. Huge
thanks to everyone who makes this ecosystem possible — the exploit and
kernel researchers, the ELF/payload loader authors, the PS5 Payload SDK
maintainers, the homebrew tool developers whose work this lives alongside,
and everyone in the community who tests, reports bugs, and shares
knowledge. None of this exists without your collective effort. 🙏

## License

GNU General Public License v3.0 (GPLv3).
Free to use, free to modify. See [`LICENSE`](LICENSE).

**`.rar` support / UnRAR:** the desktop build bundles the UnRAR source (used
only to *extract* RAR — never to compress or build a RAR-compatible archiver).
A GPLv3 §7 linking exception covers combining it with this GPL code, and
UnRAR's own license is reproduced as required. See
[`LICENSES/UnRAR-exception.md`](LICENSES/UnRAR-exception.md) and
[`LICENSES/UnRAR-license.txt`](LICENSES/UnRAR-license.txt).

## Author

Created and maintained by **PhantomPtr**.

* [Follow me on X (@phantomptr)](https://x.com/phantomptr)

## Support

If you find this tool useful, consider buying me a coffee!

* Discord server: [https://discord.gg/fzK3xddtrM](https://discord.gg/fzK3xddtrM)
* Support me on Ko-fi: [https://ko-fi.com/B0B81S0WUA](https://ko-fi.com/B0B81S0WUA)

[![Support me on Ko-fi](https://storage.ko-fi.com/cdn/kofi3.png?v=3)](https://ko-fi.com/B0B81S0WUA)
