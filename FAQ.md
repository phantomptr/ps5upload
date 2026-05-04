# FAQ

Answers to the questions that come up most often while using
ps5upload. Organized by topic — use the search box in the FAQ tab to
jump to what you need.

---

## What ps5upload does (and doesn't)

**Q: What is ps5upload?**
A cross-platform desktop app for moving files, game folders, and
disk images from your computer to a jailbroken PS5. Built around a
small custom payload that runs on the PS5 and speaks a binary
protocol (FTX2) over your LAN.

**Q: What does it actually do?**
- **Transfer** files and folders at near-wire speed, with BLAKE3
  per-shard verification and resume on drop.
- **Mount** `.exfat` and `.ffpkg` disk images natively on the PS5
  (via MDIOCATTACH + nmount). No third-party tool required.
- **Install fakepkgs** — pick a `.pkg`, click Install. Three-tier
  pipeline routes Sony's installer through ShellUI's authid via
  ptrace RPC; verified end-to-end on FW 9.60. Game pkgs (CUSA /
  PPSA / PCSA / EP / UP) install cleanly.
- **Register + launch** in the XMB — Library row's Play button
  always registers first (idempotent), retries with a DRM-type
  patch on rejection, then launches. Unmount unregisters every
  title inside the image first so the dashboard stays clean.
- **Browse** games installed on the PS5 and disk images anywhere on
  any drive.
- **File System** navigation with chmod / delete / move / copy /
  mkdir.
- **Hardware** monitoring (model, serial, uptime, RAM, CPU freq)
  and fan-threshold control.
- **Send payload** — push `.elf`, `.bin`, `.js`, or `.lua` files to
  the PS5's loader port (9021), with a replay-from-history panel.

**Q: What does it NOT do?**
Install **system pkgs** (NPXS-prefix — Store updates, Settings app
patches, built-in apps). Sony's `sceAppInstUtilInstallByPackage` is
designed for game pkgs; for NPXS the register call is accepted but
the install path freezes the PS5's mgmt service mid-flight. Use the
on-PS5 Settings → Debug Settings → Game → Package Installer for
those — that path is privileged in a way ps5upload can't replicate.
Game pkgs (UP / EP / JP / HP / CUSA / PPSA / PCSA / etc.) work fine.

---

## Supported platforms

**Q: Which desktop OSes run ps5upload?**
- **macOS** — Apple Silicon (arm64) and Intel (x86_64), shipped as
  `.dmg`.
- **Windows** — x64 and ARM64, shipped as `.zip` containing a
  portable `PS5Upload.exe`. No installer, no admin prompt — unzip
  and run.
- **Linux** — x64 and arm64, shipped as `.zip` containing
  `PS5Upload.AppImage`. Distro-agnostic (works on Ubuntu, Debian,
  Fedora, Arch, etc.) — `chmod +x` and double-click.

**Q: Which PS5 firmware works?**
ps5upload is built against PS5 Payload SDK v0.38, which resolves
kernel offsets at startup for every firmware it knows about. The
same binary runs on the full range **1.00 – 12.70**.

- **9.00 – 11.60** — all features validated on hardware.
- **1.00 – 8.60** and **12.00 – 12.70** — all features run; one
  caveat: the Hardware tab's process list shows `<pid:N>`
  placeholders instead of real command names (the kernel struct
  offset for `p_comm` isn't exposed by the SDK and our fallback
  table only has validated values for 9–11). Transfer, mount, FS
  ops, volumes, and everything else look identical.

What actually gates users in practice is the PS5-side **ELF loader**
on port 9021 — a third-party component, not part of ps5upload. Loader
coverage is roughly 4.x–12.x today.

**Q: Which PS5 models are supported?**
All models: original CFI-1xxx, Digital, Slim (CFI-2xxx), and Pro
(CFI-7xxx). Transfer, mount, volume listing, and hardware info work
on every one.

---

## Prerequisites

**Q: What do I need installed to run ps5upload?**

### macOS

Nothing. macOS 11 (Big Sur) or newer runs the app as-is. First launch:
right-click `PS5Upload.app` → **Open** → **Open** again in the
Gatekeeper dialog (the app is ad-hoc signed, not notarized).
Subsequent launches don't prompt.

### Windows

Nothing on Windows 10 (20H1 / build 19041 or later) and Windows 11 —
both ship **Microsoft Edge WebView2** runtime by default.

On stripped installs (LTSC, Windows Server without Desktop
Experience, some N/KN editions), install WebView2 once from
<https://developer.microsoft.com/microsoft-edge/webview2/>.
One-time; runtime is shared across every WebView2 app you'll ever run.

### Linux — Debian, Ubuntu, Mint, Pop!_OS

```sh
sudo apt-get update
sudo apt-get install -y \
  libfuse2 \
  libgtk-3-0 \
  libwebkit2gtk-4.1-0 \
  libsoup-3.0-0 \
  libjavascriptcoregtk-4.1-0 \
  libappindicator3-1 \
  librsvg2-2
```

- `libfuse2` is needed because `.AppImage` self-mounts via FUSE2 at
  startup. On Ubuntu 24.04 the package name resolves to
  `libfuse2t64` — the above still works via apt's virtual-package
  resolution.
- WebKit2GTK **4.1** is what Tauri 2 links against. Ubuntu 22.04
  and earlier only have 4.0 — upgrade to 24.04+ or build Tauri
  4.0-compatible yourself.

### Linux — Fedora, RHEL, CentOS, Rocky, Alma

```sh
sudo dnf install -y \
  fuse \
  gtk3 \
  webkit2gtk4.1 \
  libsoup3 \
  javascriptcoregtk4.1 \
  libappindicator-gtk3 \
  librsvg2
```

- On RHEL / Rocky / Alma 9: enable EPEL first
  (`sudo dnf install -y epel-release`).
- RHEL / CentOS / Rocky / Alma **8** ship webkit2gtk3 (the 4.0
  series). ps5upload targets 4.1 and won't run on 8.x without a
  manual webkit2gtk4.1 backport — 9.x is the minimum.

### Linux — Arch, Manjaro, EndeavourOS

```sh
sudo pacman -S fuse2 gtk3 webkit2gtk-4.1 libsoup3 \
               libappindicator-gtk3 librsvg
```

### Linux — why the long list?

`.AppImage` bundles webkit and GTK inside the image, but a few core
libs (libc, libgcc, X11 / Wayland client libs, FUSE userspace) are
expected to come from the host so the image stays portable across
distros. Modern desktop Linux installs have most of these already;
the explicit list covers stripped / server images and fresh
container shells.

**Q: The keep-awake toggle says "error" on Linux.**

Keep-awake uses `systemd-inhibit`, which needs `systemd` + `systemd-
logind`. Present on every mainstream desktop distro. If you're on a
non-systemd distro (Alpine, Void, Gentoo OpenRC, Devuan) the toggle
won't work — everything else does.

---

## Getting started

**Q: Do I need the payload?**
Yes. The PS5 has to be running `ps5upload.elf` before the app can
do anything beyond the Connection tab. The app walks you through
sending it on first run.

**Q: How do I send the payload?**
Open ps5upload → **Connection** tab → enter your PS5's IP → click
**Check**, then **Send payload**. The app waits up to 20 seconds
for the payload to come up, then unlocks the rest of the tabs.

**Q: How do I send a different payload (kstuff, kernel patches,
plugin scripts, etc)?**
Open the **Send payload** tab, click **Choose**, pick any `.elf`,
`.bin`, `.js`, or `.lua` file, and click **Send**. The app probes
the file, shows you whether it looks like a ps5upload payload or
something else, and records the send in a history panel so you
can replay it without re-picking the file.

**Q: The Send button is greyed out and says "Waiting for payload…"
for a long time.**
That's the normal probe window — up to 20 seconds for the PS5 to
come online after the bytes finish sending. The elapsed time is
shown in the button label. If it times out, the payload likely
crashed on load; send it again.

**Q: Can I use a payload that's already running on the PS5
(loaded by another tool, or by my previous ps5upload session)?**
Yes — open the Connection tab while the payload is up and the
app skips Step 2 automatically. The Connected card shows the
payload version it detected; if that version is older than what
this build of ps5upload ships, you'll see a warning with a
"Replace payload" button. The bundled payload always carries the
fixes the app expects.

**Q: Can multiple computers connect to the same PS5 payload at the
same time?**
Yes. The payload's TCP listeners on ports 9113 and 9114 accept
concurrent connections, so two laptops both running ps5upload
against one PS5 is supported. Read-only operations (browse, hardware
monitor) interleave cleanly. The thing to watch for is *destination
races*: two simultaneous uploads writing to the same path will
fight — the payload doesn't lock by destination, it commits the
shards each transfer ACKs in arrival order. For routine use ("one
person uploading, another browsing"), no coordination is needed.

---

## Transferring

**Q: Where do uploads go by default?**
Under `/data/homebrew/` unless you pick a different drive in the Upload
screen. Common presets are offered: `homebrew` (recommended),
`exfat`, `ps5upload`.

**Q: What happens when the destination already has files?**
The app asks: **Override**, **Resume**, or **Cancel**.
- **Override** — wipe destination and start fresh.
- **Resume** — size-compare remote files to local; re-upload only
  what differs. Faster for re-running a big transfer.
- **Cancel** — abort.

Set **Settings → Always overwrite** if you want to skip the prompt.

**Q: Can I upload a disk image?**
Yes. Drop any `.exfat` or `.ffpkg` image. After upload, open the
**Library** tab and hit **Mount** on the row — the payload attaches
the image via `/dev/lvd*` and mounts it at `/mnt/ps5upload/<name>/`.
The Volumes tab shows the result with a progress bar and Unmount
button.

**Q: Why does the Library sometimes show a game twice?**
If the same title is present both as a folder on disk and inside a
mounted disk image, both paths appear — but Library dedupes by
`title_id` and prefers the mount-backed path. Refresh the tab if
something still looks off.

**Q: Can I queue several uploads to run back-to-back?**
Yes — the Upload screen has a queue panel below the single-shot
controls. Each row shows live progress, current speed, and ETA
while running; the wall-clock-average MiB/s after it completes.
The runner processes one item at a time (the PS5 transfer port is
single-client), and the queue persists across app restarts so a
queued item interrupted by a crash picks up cleanly when you
press Start again. Tick **Continue on failure** to keep going
when one item fails instead of stopping the whole batch.

**Q: How do I jump between PS5 volumes in the File System tab?**
The toolbar has a **Volume** dropdown above the breadcrumb
(2.2.24+) that lists every writable, non-placeholder volume on
the PS5 with its free-space readout (e.g. `/mnt/ext1 · 412 GiB
free`). Pick one and the screen jumps to that volume's root —
no more walking up to `/` and back down. The picker hides on
machines with no writable volumes.

**Q: Does the File System tab remember where I was last?**
Yes (2.2.24+). The last-browsed path is persisted to
localStorage **per host**, so two PS5s on the same desktop each
remember their own location independently. The PS5 IP itself is
also persisted now — no more retyping it on every launch.

**Q: Where can I see what's currently running across screens?**
The OperationBar (the strip at the bottom of the window) shows
every in-flight operation as long as one is running, with the
elapsed time, bytes/total, and live MiB/s. As of 2.2.24 it
covers uploads, downloads, FileSystem cut/copy/paste, FileSystem
delete, and Library actions (Move, Delete, Chmod, Mount,
Unmount, Download). Click it for the full Activity tab with
historical outcomes.

---

## Mount + unmount

**Q: How do I find one game in a long library?**
The Library tab has a search bar above the games + images
sections (2.2.25+). Type a name fragment ("dead"), a title ID
("PPSA01342"), or a path fragment ("ext1") — matching is live,
case-insensitive, and runs across all of `name`, `titleId`,
absolute `path`, scan `scope`, and `volume`. Multi-word queries
AND-match across fields, so `dead ext1` finds Dead Space on
`/mnt/ext1` specifically.

**Q: Can I pick where a `.exfat` / `.ffpkg` mounts?**
Yes (2.2.25+). The Library Mount button opens a modal with the
same UX as the Upload screen's destination picker:

- **Volume** — pick any writable PS5 volume from the dropdown
  (`/data`, `/mnt/ext1`, `/mnt/usb0`, …). Free-space readout
  appears next to each.
- **Subpath** — free-form, with the same preset chips as
  Upload (`homebrew`, `exfat`, `ps5upload`).
- **Name** — auto-derived from the image filename (`Dead
  Space.exfat` → `Dead Space`), editable.

The resolved final path appears under the inputs in real time.
Your last-used volume + subpath is persisted per host so the
next Mount on the same console opens with the same selection.

Heads-up: third-party PS5 game scanners typically only scan
`/mnt/ps5upload/` for installed games, so mounting outside that
root works for the payload but the game may not show up in
those scanners. The modal shows a soft warning when the
resolved path is outside `/mnt/ps5upload/`.

Pre-2.2.25 payloads only honor a `mount_name` (no volume picker)
and always anchor mounts under `/mnt/ps5upload/`. The modal
detects the older payload, hides the volume + subpath rows, and
shows a "Replace payload to enable" banner.

**Q: A mount from a previous session is still showing after I
re-sent the payload.**
That's expected — `/mnt/ps5upload/*` mounts are held by the PS5
kernel and survive payload restarts. Only a PS5 hard reboot clears
them. At payload startup we reconcile: any mount whose backing
device is gone is unmounted automatically.

**Q: The Library has a `MOUNTED` badge on a `.exfat` file. What does
that mean?**
The file is currently attached at `/mnt/ps5upload/<name>/`, and the
Mount button has flipped to Unmount. The Volumes tab shows the
mapping explicitly, with the source image path under each mount.

**Q: Does Unmount leave ghost tiles in the PS5 dashboard?**
No (2.2.60+). Unmount unregisters every title inside the image
first (Sony's appinst Uninstall removes the app.db row), waits 400
ms for the commit to land, then unmounts. Pre-fix the kernel
unmount worked but app.db was left with stale rows pointing at the
now-gone path, surfacing as ghost tiles or `0x80980103 invalid
title id` on the next launch attempt.

**Q: Can I unmount while a game is running?**
No — the kernel refuses with `EBUSY` because a process inside the
mount has files open. The UI surfaces this as: *"the game inside
this image is currently running on the PS5. Exit it (PS Home →
close the game) and try again."* Same protection applies whether
you trigger Unmount from the Library tab or the Volumes tab.

---

## Install Package

**Q: Can I install fakepkgs from the desktop?**
Yes (2.2.55+). Open **Install Package**, click **Add .pkg**, pick
the file, click **Start**. The pipeline:
1. Engine uploads the bytes to `/user/data/ps5upload/pkg_temp/` on
   the PS5 (Sony's allowlisted staging path).
2. Sony's installer (`sceAppInstUtilInstallByPackage`) is invoked
   under ShellUI's authid via ptrace RPC — bytes never traverse
   the PlayGo HTTP path that rejects our process.
3. Engine post-install cleanup deletes the staging file.

Hardware-validated on FW 9.60 with regular game pkgs (UP / EP / JP
/ HP / CUSA / PPSA / PCSA / etc.).

**Q: My install said "completed" but the row says "verify on PS5".
Why?**
The pkg has an NPXS-prefix content_id (`IV0002-NPXS39041_…` etc.) —
that's a system app pkg (Store update, Settings patch, built-in
app). Sony accepts the register call but
`sceAppInstUtilInstallByPackage` isn't designed for system patches;
the install path tends to freeze the PS5's mgmt service mid-flight.
We fire-and-forget the register and let the user verify on the PS5
itself (notification panel / Settings → Notifications → Downloads).
For system pkgs the canonical path is **on-PS5 Settings → Debug
Settings → Game → Package Installer** — that's a privileged code
path ps5upload can't replicate.

**Q: Does Play in the Library register the title first?**
Yes (2.2.55+). Always-register-first: the Play button calls
`appRegister` (idempotent if already registered), retries with a
DRM-type patch on rejection, waits 600 ms for the app.db commit to
land, then calls `appLaunch`. Pre-fix the flow tried Launch first
and registered as a fallback — which surfaced misleading "not
registered" errors before registration kicked in.

**Q: Can I install a split pkg (`*.0`, `*.1`, …)?**
Yes. The engine detects split-pkg sets when you pick the lead
file. For split sets the install runs through the Tier-2 HTTP
host path (the engine serves the bytes; ShellUI fetches them) —
single uploads stay on Tier-1 (raw path on PS5 disk).

**Q: Where does the staging file go and when does it get cleaned
up?**
Staging path: `/user/data/ps5upload/pkg_temp/<id>_<unix-ms>.pkg`.
Cleanup happens on:
- Terminal install phase (Done | Error) — engine fs_delete after
  the status poll reports terminal.
- Register reject — engine fs_delete immediately (Sony rejected
  the register, the file is otherwise leaked until 24 h).
- User cancel — engine fs_delete on the cancel path.
- Payload startup — sweeps any `*.pkg` older than 24 h as a
  crash-recovery safety net.

---

## Troubleshooting

**Q: The payload isn't responding; ports appear open but connections
get reset.**
The payload may have wedged on a Sony API call. Recovery:
1. PS5 Settings → Network → disable / re-enable Wi-Fi or Ethernet.
2. If that doesn't clear it, reboot the PS5.
3. Re-send the payload.

**Q: Why doesn't Launch from the Library tab actually start the game?**
It does, on every firmware we've validated. Register
(`sceAppInstUtilAppInstallTitleDir`) runs from our own process
because the full credential jailbreak we apply at startup
satisfies its caller-context check. Launch
(`sceLncUtilLaunchApp`) checks `getpid() == SceShellUI.pid`
specifically, so the payload routes Launch through the same
ptrace RPC mechanism it uses for sensors — the call runs from
inside SceShellUI's address space where the check passes.
Hardware-validated on FW 9.60: hitting Launch starts the game
(SoC power draw jumps from idle ~30 mW to ~170 mW within a
few seconds of the title coming up).

**Q: I see errors in the status bar but don't know what happened.**
Open the **Logs** tab. Every runtime error, failed API call, and
console warning ends up there with timestamps and expandable
detail. Click **Copy** or **Download** to grab a plain-text dump
for a bug report.

**Q: Deleting a huge game folder used to fail with a "502 Bad
Gateway" error.**
Fixed in 2.2.22. The recursive walk on a small-file-heavy folder
(e.g. PPSA01342 with ~223k files / 19k dirs) takes minutes on PS5
UFS — long enough that the engine's old 30 s socket timeout fired
mid-walk while the payload was still deleting in the background.
Now `fs_delete` uses the same 1-hour deadline `fs_copy` already
does, **and** the operation reports live progress (bytes freed)
to the bulk-delete banner with a Stop button that cleanly bails
between directory entries.

**Q: An upload of a small-file-heavy game failed with
`pack_worker_io_error` partway through.**
Fixed in 2.2.22. The payload's pack worker used to flip a sticky
worker-error flag on the very first transient `open()` or
`write()` failure, aborting a 75k-shard transaction outright. It
now retries transient errnos (EIO/EMFILE/ENOMEM/EINTR/EAGAIN)
up to 3 times with 20/50/100 ms backoff before giving up.
Unrecoverable errors (ENOSPC/EROFS/EACCES/ENAMETOOLONG) still
fail fast — there's no point retrying a full disk. The retry
counts surface in `COMMIT_TX_ACK` so post-mortem logs show
exactly how many transient hits were absorbed.

**Q: Library Move shows "Live progress unavailable — your PS5
payload is older than this app" but I'm on the latest payload.**
Fixed in 2.2.24. Two coupled bugs produced the false positive:

- The payload registered the in-flight FS_OP slot *after* the
  recursive_size pre-walk. On small-file-heavy trees the walk
  outran the client's 250 ms initial poll delay, so the first
  `FS_OP_STATUS` poll landed on a not-yet-registered op — the
  engine surfaced that as a transient parse error, which the
  client mis-attributed to an old payload. The payload now
  registers up front with `total_bytes=0` and patches the total
  in via `fs_op_set_total` once the walk completes.
- The client used brittle substring matching on error text
  (`"unsupported_frame"` / `"decode FS_OP_STATUS_ACK body"`),
  which can appear in transient errors even on a current
  payload. It now consults the running payload's reported
  version: known-old payloads latch a threshold-specific banner
  (`predates 2.2.16` or `predates 2.2.7`); current payloads
  tolerate up to 5 consecutive transient failures and stop
  silently with no banner — never the misleading "older than
  this app" string.

If you saw this on 2.2.23 or earlier, click **Replace payload**
on the Connection screen once you're on 2.2.24+ and the move
runs cleanly thereafter.

**Q: After clicking Replace payload, the version number on the
Connection screen still shows the old one for a few seconds.**
Fixed in 2.2.24. The probe loop already had the new version +
kernel from `payloadCheck` but was discarding both, leaving the
store with the old values until the next 10-second background
tick. Two fixes:

- The probe now writes the freshly-booted payload's version +
  kernel into the store the moment it answers — the version
  block flips to the new numbers in lock-step with step 2 going
  "ok," not 10 s later.
- A new "rechecking…" badge with a spinner renders in the
  VersionBlock while a probe is in flight. Stale numbers are
  dimmed in italics (or the row reads "Probing…" if there's no
  prior value), and the outdated-payload nudge is suppressed
  during the recheck since the comparison would be against
  in-flight data.

**Q: Where are app settings saved?**
- **macOS**: `~/Library/Application Support/com.phantomptr.ps5upload/`
- **Windows**: `%APPDATA%\com.phantomptr.ps5upload\`
- **Linux**: `~/.local/share/com.phantomptr.ps5upload/`

The path is shown in Settings → Storage.

**Q: Where does the send-payload history go?**
Same folder as above, in `send_payload_history.json`. Cleared via
the Clear button in the Recent sends panel on the Send Payload
tab. Duplicate sends (same path + host + port) refresh the
timestamp in place instead of piling up new rows.

---

## Advanced

**Q: Can I run the engine standalone (without the desktop app)?**
Yes — `ps5upload-engine` is a self-contained HTTP server listening
on `localhost:19113`. The desktop app uses it under the hood; CLI
users can hit the `/api/*` endpoints directly.

**Q: Can I write my own client against the payload?**
Yes. The FTX2 binary protocol is defined in
`engine/crates/ftx2-proto/src/lib.rs` — all frame types, body
shapes, and flag bits are documented there. The mock server in
`engine/crates/ps5upload-tests/tests/mock_server.rs` is a
reference implementation of the minimum subset needed for
transfer, which you can read as example protocol code.

**Q: How do I contribute a translation?**
Edit the strings in `client/src/i18n.ts` or use the helper script at
`scripts/translate-i18n.py`. PRs welcome.

**Q: Why is my anti-virus flagging the app?**
Tauri apps often trigger false positives because they bundle a
small web runtime. Release builds are unsigned (no paid certs), so
Windows SmartScreen and macOS Gatekeeper will warn on first run
until the binary accumulates reputation. Click "More info → Run
anyway" (Windows) or right-click → Open (macOS) once. Grab the
download straight from the
[Releases page](https://github.com/phantomptr/ps5upload/releases)
(not a mirror) and report any AV false positive to your vendor.

**Q: How do updates work?**
The app checks GitHub once per launch (24h-cached) and shows a
dot on the Settings entry in the sidebar when a newer version is
available. Settings → Updates → **Download** streams the
platform-appropriate archive to your Downloads folder and opens
Finder/Explorer/Files. Quit the app, replace the old one with the
new download, and relaunch. No automatic install, no signing cert
needed — the download URL is the GitHub release page.
