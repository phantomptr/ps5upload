# FAQ

Answers to the questions that come up most often while using ps5upload. In the app's FAQ
screen, the tabs are the sections below, each topic lists its questions, and the search box
finds a word or an error code (for example `0x80B2116F`) in any answer.

---

# Start here

## What ps5upload does (and doesn't)

**Q: What is ps5upload?**
An app (desktop, Android, or a self-hosted web UI) for moving games, apps and homebrew to a
jailbroken PS5 and managing the console. A small payload on the PS5, the **helper**, talks to
the app over **AVA1**, one encrypted connection on port 9120.

**Q: What does it actually do?**
- **Upload** files, folders, disk images and `.zip` / `.7z` / `.rar` archives, verified end to
  end. Uploads resume after a dropped connection or a helper restart. RAR handles multi-part and
  password-protected archives (desktop only).
- **Install packages**: PS4 `.pkg` and PS5 fake packages (base, update, DLC) through ps5upload's
  own on-console installer, from your computer (**Stream & install** or **Upload & install**),
  a NAS, a USB drive or a link.
- **Convert Games**: build an installable package from a decrypted game folder or an image, or
  write a game folder as a game image (`.exfat`, or the smaller `.ffpfsc`) that ShadowMount+
  mounts.
- **Health check and speed test**: checks the PS5, the app and the network between them, says
  what to fix, and measures how fast files move.
- **Mount** `.exfat` / `.ffpkg` images, **register and launch** games, browse and manage files.
- **Cheats, fan curve, hardware view, power and wake, payload sender, backport tools,** and an
  optional FTP server on the PS5.

**Q: What does it NOT do?**
Install **system packages** (NPXS Store or Settings updates): the PS5 freezes its installer
service on them. Use Settings → Debug Settings → Game → Package Installer on the console.

---

## Quick start for new users

New to PS5 homebrew? Read this section first. Everything here is covered in
more depth further down.

**Q: What are the pieces, in plain words?**
- **Jailbreak and ELF loader.** The jailbreak leaves an *ELF loader* on port **9021**; every
  payload is sent to it. ps5upload does not jailbreak your console.
- **kstuff.** Live kernel patches. Installing fake packages and launching them needs it.
- **ShadowMount+.** Auto-mounts game images and folders so they appear on the home screen.
- **etaHEN.** A separate homebrew environment. If it already loads kstuff for you, send only
  ps5upload; a second kstuff stacks another copy.
- **ps5upload helper.** Our payload (`ps5upload.elf`), the `helper` dot at the bottom of the
  app. The app sends it for you.

**Q: In what order do I load payloads?**
Send **elfldr** first (if your setup uses it), then **kstuff**, then
**ShadowMount+**, then **ps5upload**. The **Set up your PS5** wizard does the
last three in this order for you, with the delays the Payloads catalogue
recommends. Send them one at a time; the loader takes one file per
connection. After a reboot or rest mode the payloads are gone, so load them
again.

**Q: Which payloads must be running before I install a package?**
**nanoDNS**, **kstuff**, **ftpsrv-ps5** and **ShadowMount+**, plus the ps5upload helper. nanoDNS
and ftpsrv-ps5 are not in the Setup wizard; send them from **Payloads**. ps5upload's own
transfers and Convert do not use ftpsrv-ps5. If an install is refused, first check that kstuff
is running.

**Q: Which firmware limits apply to what I install?**
- **PS5 fake *game* packages install above firmware 11.60 (including 13.60) but are not
  playable.** PS4 fake packages work and PS5 homebrew apps launch. The app warns you. See "My
  uploaded game won't launch" for the folder-dump route.
- On **13.60**, Stream & install is the dependable route.

**Q: Quick start: from a fresh jailbreak to my first upload.**
1. Confirm the console's ELF loader is running (port 9021) and note the console's IP
   (**Settings → Network → View Connection Status**).
2. Install ps5upload (or open the web UI). Use a wired connection if you can.
3. Open **Connection**, enter the IP, click **Check**.
4. Run **Set up your PS5**: it sends kstuff and ShadowMount+, then the helper, in order. If
   something already loads kstuff, send only ps5upload.
5. Wait for the `helper` dot to turn green. If a 6-digit code appears on the console (helper
   loaded by another tool), enter it in the app once to pair.
6. Open **Upload**, choose your game, pick the drive and click **Start**. The free-space check
   runs before anything is sent.
7. Open **Library** to see it. For a `.pkg`, use **Install Package → Stream & install**. If it
   will not launch, see "My uploaded game won't launch".

---

## Getting started

**Q: Do I need the helper?**
Yes. The PS5 has to be running `ps5upload.elf` before the app can do anything beyond the
Connection tab. The app walks you through sending it on first run.

**Q: How do I send the payload?**
Open ps5upload → **Connection** tab → enter your PS5's IP → click
**Check**, then **Send payload**. The app waits up to 20 seconds
for the payload to come up, then unlocks the rest of the tabs.

**Q: How do I send a different payload (kstuff, kernel patches,
plugin scripts, etc)?**
Open the **Send payload** tab, click **Choose**, pick any `.elf`,
`.bin`, `.js`, `.lua`, or `.jar` file, set the port to whatever your
loader listens on (the screen has a built-in cheat sheet — `.elf` →
9021, `.js` → 50000, `.lua` → 9026, `.jar` → 9025 for BD-JB / BDJ),
and click **Send**. The app probes
the file, shows you whether it looks like a ps5upload payload or
something else, and records the send in a history panel so you
can replay it without re-picking the file.

**Q: The Send button says "Waiting for payload…" for a long time.**
That is the normal probe window, up to 20 seconds. If it times out, the payload likely crashed on
load; send it again.

**Q: Can I use a helper that is already running (loaded by another tool)?**
Yes. Open **Connection** while it is up and the app skips the send step. If you loaded it another
way, the console shows a 6-digit code to confirm once. If the helper is older than this app, the
app shows **Update helper**, which replaces it in one click.

**Q: Can multiple computers connect to the same PS5 at the same time?**
Yes, up to 16 sessions; each computer pairs once. Browsing and the hardware view interleave
cleanly. Two uploads to the same path will fight, since nothing locks a destination.

**Q: What is the pairing code, and which ports does ps5upload use?**
Outgoing from this computer to the PS5, two ports: **9120** (the helper, AVA1) and **9021** (the
ELF loader). Stream & install also needs the PS5 to connect **in** to this computer on **19113**
(the engine); on Windows allow ps5upload on both Private and Public networks, since a cable
straight to the console is a Public network.

| Port | On | Used for |
|---|---|---|
| 9120 TCP | PS5 | The helper: uploads, files, management (encrypted) |
| 9021 TCP | PS5 | ELF loader: sending the helper |
| 9115 TCP | PS5 | The installer the helper starts |
| 8084 TCP | PS5 | Payload Manager, when there is no ELF loader |
| 19113 TCP | this computer | The engine: the PS5 fetches packages from it during Stream & install |
| 9295, 9302 | PS5 | Remote Play pairing and wake |

Every connection is encrypted and each computer pairs with the console once. A helper
the app launched pairs by itself; one loaded another way shows a 6-digit code on the console,
which you enter in the app. If the console shows a different PS5 at an address you reuse, forget
the old one and pair again.

**Q: I updated to 6.0 and the app says the helper is old.**
6.0 and v5.x cannot talk to each other. Click **Update helper** on the Connection screen (or
reload `ps5upload.elf`) and the app replaces it. If the old helper will not exit, restart the
console and try again.

**Q: How do I cap the upload speed?**
Set `PS5UPLOAD_BANDWIDTH_MBPS` (megabytes per second) before starting the app or engine. Unset or
zero means no cap. The pre-6.0 name of this variable still works for one release.

**Q: Can I run the engine on a different machine (self-hosted)?**
Yes. Run it on a server or NAS and point the app at it in **Settings → Engine URL**. Images:
`ghcr.io/phantomptr/ps5upload-engine` (engine only) and `ghcr.io/phantomptr/ps5upload-engine-webui`
(full web UI), tagged `:latest` or `:<version>`.

- The API has **no password**. Set `PS5UPLOAD_ALLOW_IP` to the IPs or ranges allowed to use it
  (`192.168.1.0/24`); anything else gets a `403` naming the refused address. Trusted LAN only,
  never the internet.
- Start from [`engine/compose.yaml`](engine/compose.yaml) (set `PS5_ADDR`, `PS5UPLOAD_ALLOW_IP`
  and your package folder). It uses **host networking**: a stream install makes the PS5 download
  the package *from the engine*, and on Docker's bridge network the engine offers an address the
  PS5 cannot reach. Docker Desktop (macOS/Windows) has no host networking: publish port 19113 and
  set `PS5UPLOAD_PKG_HOST_IP` to the computer's LAN IP.
- Keep a volume on `/data` (saved servers, install history, artwork cache).
- With a remote engine the app's file pickers browse the **engine's** disk.

**Q: Docker or a NAS says "Permission denied" when I save or upload.**
The image runs as UID:GID 65532 and keeps its state in `/data`. If you bind-mount a folder someone
else owns, the engine names the folder, its own UID:GID and the folder's owner. Either set `user:`
in the compose file to the folder's owner (`ls -ln` shows the numbers), or run
`chown -R 65532:65532 <host folder>`. `PS5UPLOAD_DATA_DIR` does not fix this.

**Q: Can I use ps5upload from a web browser (no desktop app)?**
Yes. The `webui` image (above) serves the full app over HTTP; open `http://<host>:19113` from an
allowed IP. Same security rules: no password, trusted LAN only.

- **Upload works**, but the file picker browses the **engine's** disk, not your browser machine's.
  Mount your games into the container (`-v /host/games:/pkgs:ro`) and set
  `PS5UPLOAD_BROWSE_ROOTS=/pkgs` (comma-separated for several) so the picker opens there.
  **Install Package → From this device** uploads a package from your browser machine instead.
- An upload keeps running when you close the tab; reopening shows it. Only one tab runs the queue.
- Desktop-only and hidden in the browser: **archive uploads**, **Payloads** (sending a file from
  disk), saving a save backup or downloading files to your computer, and attaching files to a bug
  report. Everything that works on the PS5 itself is the same as on desktop.
- The docker image can be built using `PS5UPLOAD_BASE_URL` to change the root path of the app.

**Q: What is "Stream install"?**
It installs a `.pkg` **straight from your computer**: the PS5 pulls the bytes over your network, so
nothing is copied to the console first (no package plus installed game taking room). Keep the
computer on and connected until it finishes. The PS5 must be able to reach your computer; if it
cannot, see "Stream install fails before the PS5 downloads anything", or use **Upload & install**.

**Q: Can I manage several PS5s at the same time?**
Yes. Add each console from **Connection** or the **+** on the console tab strip. Each console has
its own tab, status dot, upload queue and installs, which run independently and in parallel;
switching tabs only changes what you see. On the console side, low-level operations are serialized,
so overlapping work (a temperature read during an install) cannot crash the helper.

---

# Setup

## Supported platforms

**Q: Which desktop OSes run ps5upload?**
- **macOS** — Apple Silicon (arm64) and Intel (x86_64), shipped as
  `.dmg`.
- **Windows** — x64 and ARM64, shipped as a `-setup.exe` installer
  **or** a `.zip` containing a portable `PS5Upload.exe` (no installer,
  no admin prompt — unzip and run).
- **Linux** — x64 and arm64, shipped as `.deb`, `.rpm`, and a `.zip`
  containing `PS5Upload.AppImage`. Install the package for your distro,
  or use the AppImage anywhere — `chmod +x` and double-click. If you
  installed the `.deb` or `.rpm`, the built-in updater detects that and
  offers the same package type rather than the AppImage.
- **Android** — `.apk` (sideload), built for **ARM only** (`arm64-v8a`
  and `armeabi-v7a`). There is no x86 / x86_64 Android build, so
  Intel-based Chromebooks and emulators are not supported. Same
  interface, mobile-friendly; manages your PS5 over Wi-Fi. See the
  **Android** section below for setup, permissions, and uploading from
  your phone.

There is no 32-bit x86 (i386 / i686) build for any desktop OS.

**Q: Which PS5 firmware works?**
The helper is built against PS5 Payload SDK v0.43 and the same binary runs on **1.00 to 13.60**.
Tested on 5.10, 9.60 and 13.60 (and confirmed by users on 12.20). In practice the limit is the
**ELF loader** on port 9021, a third-party component; loader coverage is roughly 4.x to 12.x.

`.pkg` install also needs live kernel patches (kstuff / fpkg-enable). Where they are not active,
the installer says so instead of reporting a false success.

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

**If macOS says "PS5Upload is damaged and can't be opened"** — that's
the *quarantine* extended attribute Safari/Chrome/Firefox added when
the `.dmg` was downloaded. One-line fix:

```sh
xattr -dr com.apple.quarantine /Applications/PS5Upload.app
```

This is a normal macOS workflow for unsigned tools (same as
ItemzFlow, ftpsrv-mac, and most homebrew utilities — none of these
pay Apple $99/year for Developer ID + notarization). The bundle's
hash hasn't changed; you're just telling Gatekeeper "yes, I know
where this came from."

### Windows

Nothing on Windows 10 (20H1 / build 19041 or later) and Windows 11 —
both ship **Microsoft Edge WebView2** runtime by default.

On stripped installs (LTSC, Windows Server without Desktop
Experience, some N/KN editions), install WebView2 once from
<https://developer.microsoft.com/microsoft-edge/webview2/>.
One-time; runtime is shared across every WebView2 app you'll ever run.

**If Windows shows "Windows protected your PC" (SmartScreen)** —
the app isn't signed with a paid code-signing certificate, so
SmartScreen treats fresh downloads as low-reputation. Click
**More info** → **Run anyway**. SmartScreen builds reputation
silently after that; subsequent launches don't prompt.

If your IT policy blocks "Run anyway", right-click
`PS5Upload.exe` → **Properties** → tick **Unblock** at the bottom
of the General tab → **OK**. That removes the Windows
mark-of-the-web that gates SmartScreen.

Like macOS quarantine, this is normal for unsigned scene tools.
EV code-signing certs cost ~$400/year and require a hardware
token — not something a free homebrew project ships.

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

**Q: Minimum distro version? / "GLIBC_2.39 not found" or the app installs
but won't launch.**

All three Linux artifacts (`.deb`, `.rpm`, **and** the `.AppImage`) are
built in CI against **glibc 2.39**, so they need a distro that ships
glibc 2.39 or newer:

- **Ubuntu 24.04+**, **Debian 13 (trixie)+**, **Fedora 40+**,
  RHEL/Rocky/Alma 10+, current openSUSE Tumbleweed, current Arch.
- **Too old:** Ubuntu 22.04 (glibc 2.35), Debian 12 / bookworm (2.36),
  Fedora 39 (2.38), RHEL 9 (2.34).

This is a glibc floor, **not** a missing package — on an older release
the `.deb`/`.rpm` install cleanly (their package dependencies all
resolve) but the binary then fails at launch with `GLIBC_2.39 not
found`, and the AppImage hits the same wall because it bundles WebKitGTK
but still uses the host's glibc. There's no way to bundle glibc itself
into a native package. On an older distro, build from source on that
machine: `make install` then `make dist-linux` (it links against
whatever glibc that box has).

**Q: The keep-awake toggle says "error" on Linux.**

Keep-awake uses `systemd-inhibit`, which needs `systemd` + `systemd-
logind`. Present on every mainstream desktop distro. If you're on a
non-systemd distro (Alpine, Void, Gentoo OpenRC, Devuan) the toggle
won't work — everything else does.

**Q: The window opens but it's just a white/blank screen (Bazzite,
SteamOS, NVIDIA, etc.).**

This is WebKitGTK failing to render with accelerated compositing /
the DMABUF renderer on your GPU/compositor — common on gaming distros
(Bazzite, SteamOS) and NVIDIA. Fixes, easiest first:

1. **Update to the latest version — the fix is built in, and now adapts
   to your graphics stack.** The app detects what it is running on at
   startup and applies only the workarounds that stack needs:

   | Your setup | What the app does automatically |
   | --- | --- |
   | Any Linux | Disables the DMABUF renderer |
   | **NVIDIA on Wayland** | Also disables accelerated compositing |
   | **NVIDIA on Wayland, AppImage** | Also preloads your system's `libwayland-client` |

   This works for a plain double-click of `PS5Upload.AppImage`, the
   `.deb` / `.rpm` / folder build, and the `PS5Upload.sh` wrapper — with
   one exception: the `libwayland-client` preload is only applied by
   `PS5Upload.sh`, because `LD_PRELOAD` has to be set before the process
   starts. **On NVIDIA + Wayland, launch via `./PS5Upload.sh` rather
   than the bare AppImage.**

   Why it is not simply all of them, everywhere: disabling accelerated
   compositing makes WebKitGTK render the whole page in software, which
   is fine for a static window but makes **scrolling sluggish** — and
   this app's main screens are long lists (a library of hundreds of
   games, a file browser of thousands of entries). An earlier version
   set it for everyone, which is why scrolling felt heavier on Linux
   than on Android or Windows. It is now scoped to the stack that needs
   it.

2. **Still white?** Apply the compositing switch by hand — your stack may
   need it without being NVIDIA-on-Wayland:

   ```sh
   WEBKIT_DISABLE_COMPOSITING_MODE=1 ./PS5Upload.sh
   ```

   If your setup needs the `libwayland-client` preload but is not
   detected as NVIDIA (an unusual driver setup, a container), force it:

   ```sh
   PS5UPLOAD_FORCE_WAYLAND_PRELOAD=1 ./PS5Upload.sh
   ```

   Every automatic setting above is skipped if you set that variable
   yourself, so you can also turn one back *off*
   (`WEBKIT_DISABLE_DMABUF_RENDERER=0 ./PS5Upload.sh`).

3. **Still white?** Force X11 instead of Wayland:

   ```sh
   GDK_BACKEND=x11 ./PS5Upload.sh
   ```

4. **Still white?** Fall back to software rendering (slower UI, but
   reliable — good for confirming it's a GPU-path problem):

   ```sh
   LIBGL_ALWAYS_SOFTWARE=1 ./PS5Upload.sh
   ```

You can combine these (e.g. `GDK_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1
./PS5Upload.sh`). If even software rendering shows a white screen, run
`./PS5Upload.AppImage` from a terminal and share the output — a
`WebKitWebProcess`/`WebKitNetworkProcess` crash there points at a
WebKitGTK packaging problem rather than a GPU one, which is a
different fix.

On the immutable-OS distros (Bazzite, Silverblue, etc.) the host
WebKitGTK libraries are layered with `rpm-ostree install` and need a
reboot to take effect; the AppImage bundles its own copies, but the
core GTK/Wayland/X11 client libs still come from the host.

**Don't launch with `sudo`.** None of these fixes need root, and running
the app as root writes root-owned files into `~/.ps5upload` — after
which a normal launch fails to read its own settings, which looks like a
new bug. If you have already done it:

```sh
sudo chown -R "$USER:$USER" ~/.ps5upload
```

---

## Android

**Q: After updating the app, it says "This PS5 has not accepted this app yet" or "The PS5 is not
accepting new pairings".**
To the PS5, an app updated from 5.x is a new device, and the PS5 only takes new devices for a
short while after its helper starts. Tap **Pair…**, then **Resend helper**: the app sends its
helper again, the PS5 accepts the app that sent it, and the dialog closes by itself a few
seconds later. This works the same in the desktop app.

**Q: How do I install the Android app?**
Download `PS5Upload-<ver>-android.apk` from the Releases page and open it. Android asks you to
allow installs from your browser or file manager the first time. Updates install in place.

**Q: The app's text or buttons are huge (or tiny) on my phone.**
Your phone's Display size or Font size setting scales the whole app. Open **Settings → Text
size** in ps5upload and pick a smaller percentage.

**Q: I picked a folder, `.zip` or `.pkg` on Android but it fails ("could not read", "No such file").**
Android needs **All files access** before the app can read your files. Tap **Choose folder**,
**Choose file** or **Add .pkg**, tap **Open settings**, turn on **Allow access to manage all
files** for PS5Upload, come back and tap **Retry**. You can also grant it in **Android Settings
→ Apps → PS5Upload → Permissions → All files access**.

**Q: How do I upload a big game from my phone?**
**Upload → Choose folder** (or **Choose file** for a `.zip`), pick it in the in-app browser,
choose the destination and **Start**. The file is read in place, nothing is copied first.

**Q: The `helper` dot at the bottom of the app is red.**
The dot is the PS5-side helper, not the app. Red means it is not running (or is an older version
that needs **Update helper**). Run **Set up your PS5** or send it from the **Payloads** tab. The
`engine` dot is the app's own service and is green whenever the app is open.

---

## Network setup — direct Ethernet (optional, best stability + speed)

**Q: Do I need anything special on my network?**
No. The default setup is: PS5 and computer both on the same WiFi /
LAN, the app finds the PS5 by IP. That works fine for most uses,
including multi-GB folder uploads.

**Q: When is direct Ethernet worth setting up?**
When you upload **multi-hundred-GB game folders** or **disk images**
and want the fastest, most reliable path. Running a cable straight
between the PS5 and the computer:

- **Bypasses WiFi entirely** — no congestion, no roaming, no
  retransmits, no "blip during a long upload" failure mode.
- **Bypasses your router** — no NAT, no QoS competing with whatever
  else is on the LAN, no MTU surprises.
- **Saturates the PS5's NIC** — the PS5 (and PS5 Pro) ship with a
  gigabit Ethernet port (~118 MiB/s practical ceiling). Over WiFi
  you'll see anywhere from 5 MiB/s (Wi-Fi 5 in a busy 2.4 GHz house)
  to ~60 MiB/s (Wi-Fi 6 line-of-sight); over a direct cable you'll
  pin the link.

The cost: you need a **second NIC for internet** on the computer (any
WiFi works, or a USB Ethernet adapter back to the router) because
the direct cable carries no internet route. On the PS5 the direct
cable replaces the LAN port's usual internet config, so if you want
the console to reach PSN you'll switch its network back to WiFi /
router when you're done uploading — or just leave PSN off while you
transfer (PSN isn't required for any of ps5upload's features).

### Step 1 — Cable + addresses

- **Cable:** any Cat5e or better, straight-through. Auto-MDIX on
  modern NICs means crossover cables aren't needed.
- **Addresses we'll use** (any private /24 works — pick what doesn't
  collide with your home network):
  - **Computer:** `192.168.88.1`, mask `255.255.255.0`
  - **PS5:**      `192.168.88.2`, mask `255.255.255.0`, gateway
    `192.168.88.1`
- **Why these:** both sides on the same `/24` so packets stay
  link-local; the PS5's "manual" network screen requires the
  gateway field to be set even though nothing routes anywhere — we
  point it at the computer so the form saves.

### Step 2 — Configure the COMPUTER

#### Windows 11

1. **Settings → Network & Internet → Ethernet** (the entry for the
   adapter your direct cable is plugged into — *not* WiFi).
2. **IP assignment → Edit → Manual → flip IPv4 on.**
3. Fill in:
   - **IP address:** `192.168.88.1`
   - **Subnet mask:** `255.255.255.0`
   - **Gateway:** *leave blank* — this is critical. If you put
     anything here, Windows treats the cable as a possible default
     route and may try to send internet traffic through it.
   - **Preferred DNS:** *leave blank*
4. Save. The adapter status will show "No internet" — that's
   correct, this link only carries PS5 traffic.
5. **Firewall:** set the Ethernet network profile to **Private**
   (Settings → Network & Internet → Ethernet → Network profile type
   → Private). The default Public profile blocks inbound on the
   ports the helpers reply on. If Windows Defender still prompts,
   allow `PS5Upload.exe` on Private networks.
6. **Keep WiFi on for internet** — Windows automatically prefers
   the route with a lower metric (the WiFi default gateway), so
   your browser etc. keep working.

#### macOS

1. **System Settings → Network**, pick your Ethernet interface
   (built-in on Intel Macs, USB-C/Thunderbolt-to-Ethernet adapter
   on Apple Silicon — Apple's $29 dongle or any Realtek/Intel
   2.5 GbE adapter works).
2. **Details… → TCP/IP → Configure IPv4: Manually**
   - **IP Address:** `192.168.88.1`
   - **Subnet Mask:** `255.255.255.0`
   - **Router:** *leave blank* — same reason as Windows. macOS
     respects the empty Router field and doesn't promote the cable
     to default gateway.
3. **DNS → Configure DNS Servers:** empty.
4. **OK → Apply.** The interface will show "Self-Assigned IP" or a
   yellow status if the PS5 isn't configured yet; that goes away
   once Step 3 is done.
5. **Service order:** **System Settings → Network → ⋯ → Set Service
   Order** — make sure WiFi sits **above** the Ethernet interface.
   That keeps macOS from preferring the (internet-less) cable for
   general traffic.
6. **Firewall** (System Settings → Network → Firewall): if it's on,
   allow incoming connections for `ps5upload` and `PS5Upload`. On a
   fresh install the firewall is off by default.

#### Linux (GNOME — Ubuntu, Fedora, Bazzite, Pop!_OS)

1. **Settings → Network → Wired**, click the gear next to the
   direct-cable adapter.
2. **IPv4 tab → Method: Manual**
   - **Address:** `192.168.88.1`, **Netmask:** `24` (or
     `255.255.255.0`)
   - **Gateway:** *leave blank*
3. **DNS:** blank, **Automatic** off.
4. **Apply**, then toggle the connection off/on.

Or via `nmcli` (works on every NetworkManager distro including
Bazzite / SteamOS desktop mode):

```bash
# replace eth0 with your interface name (`ip link` to find it)
sudo nmcli connection add type ethernet ifname eth0 \
  con-name ps5-direct ipv4.method manual \
  ipv4.addresses 192.168.88.1/24
sudo nmcli connection modify ps5-direct \
  ipv4.gateway "" ipv4.dns "" ipv4.never-default yes
sudo nmcli connection up ps5-direct
```

The `ipv4.never-default yes` is the Linux equivalent of leaving the
gateway blank on Win/macOS — it tells NetworkManager this connection
must never become the default route.

**KDE Plasma:** System Settings → Connections → click the wired
entry → IPv4 → Method: Manual, same values; under "Routes…" check
"Use only for resources on this connection."

**Firewall (`firewalld` on Fedora/Bazzite):**
```bash
sudo firewall-cmd --zone=trusted --add-interface=eth0 --permanent
sudo firewall-cmd --reload
```
On Ubuntu with `ufw` the default is allow-outbound / deny-inbound;
nothing extra needed since ps5upload only makes outbound
connections to the PS5.

### Step 3 — Configure the PS5

1. **Settings → Network → Settings → Set Up Internet Connection**.
2. **Use a LAN Cable.**
3. **Custom** (not Easy).
4. **IP Address Settings: Manual**
   - **IP Address:** `192.168.88.2`
   - **Subnet Mask:** `255.255.255.0`
   - **Default Gateway:** `192.168.88.1`
   - **Primary DNS:** `1.1.1.1` (Cloudflare — the PS5 won't actually
     reach it on this cable, but the field can't be empty and a
     real public IP avoids the DNS-timeout latency you'd get
     pointing at `192.168.88.1` since the PC isn't running a DNS
     server)
   - **Secondary DNS:** `1.0.0.1` (or leave blank — most firmwares
     accept an empty secondary; `0.0.0.0` is rejected by some)
5. **MTU Settings:** Automatic.
6. **Proxy Server:** Do Not Use.
7. Save. The PS5 will run **Test Internet Connection** automatically
   — **it will FAIL** ("Cannot connect to internet"). That's
   expected and fine. The "Obtain IP Address" / "Connect to LAN"
   steps will show **Successful** — those are what matter.

### Step 4 — Verify the link

On the computer:

```bash
# Should return replies in <1 ms — both sides see each other.
ping 192.168.88.2
```

If ping works, you're done — open ps5upload and use `192.168.88.2`
as the PS5 address. The payload-send (Connection → Send payload)
and every transfer afterwards goes over the direct cable.

### Notes / gotchas

- **PSN, the Store, game updates, online play** — all require
  internet. While the PS5 is on the direct cable, those won't work.
  Switch the PS5's network back to WiFi (or your router) when you
  want them; ps5upload remembers the last-used IP so re-pointing it
  at the WiFi IP afterwards is a one-line change in Settings.
- **PS5 Ethernet port is gigabit** — practical sustained ceiling is
  around 110–118 MiB/s for huge files.
- **2.5 GbE / 10 GbE adapters on the computer** are fine and still
  negotiate to 1 Gbps because the PS5 is the slowest link. The
  extra headroom helps if you ever swap consoles.
- **Don't share two connections at the same IP** — if your PS5 is
  on both WiFi (DHCP from the router) AND this direct cable, give
  them different IPs so the routing table doesn't get confused. The
  `192.168.88.x` range above sidesteps any conflict with the common
  `192.168.0/1.x` home subnets.
- **Resuming still works.** If you start an upload over the cable and switch to Wi-Fi (a new
  PS5 IP), start the same upload again to the new IP and it continues from what is on the console.

---

# Files and games

## Transferring

**Q: How do I stop my PS5 from going into rest mode?**
**Settings → Upload → Keep the PS5 awake** has three modes: **Off**; **During transfers**
(default), which keeps resetting the console's auto-standby timer while an upload runs; and
**Always while connected**, which keeps every console with a running helper out of auto-rest while
the app is open (a ⚡ indicator shows). Resting the console by hand always works, and closing the
app returns the console to its normal schedule.

**Q: Can I wake the PS5 from standby, or turn it off, from the app?**
Yes. The Connection screen has **Rest mode**, **Reboot**, and **Shut down**
buttons, and — when the console is asleep — a **Wake** button.

Waking uses Sony's own discovery protocol (a PS5 ignores Wake-on-LAN magic
packets), and it only works if three settings are enabled on the console
first — the app lists them, because without any one of them the wake just
fails silently:

- **Enable Remote Play** — Settings › System › Remote Play
- **Stay Connected to the Internet** — Settings › System › Power Saving ›
  Features Available in Rest Mode
- **Enable Turning On PS5 from Network** — same menu

Wake needs a one-time **wake code** (your console's Remote Play registration
key as a number). Enter it once in the wake-setup panel and it's remembered.

A plain wake powers the console on to the *user-select* screen. If you also
provide the console's two Remote Play **session keys** (registration key +
RP-Key, in the panel's advanced section), the button becomes **Wake & sign
in** and brings the console up straight on your user's home screen — the same
thing the official Remote Play app does. (If your user has a login passcode,
the console will stop to ask for it, so sign-in needs a passcode-free user.)

**Q: Do I still have to "Register" a game after uploading it?**
Not anymore. When you upload a game folder, **"Add to PS5 home screen
when done"** is on by default — the game is registered automatically the
moment the transfer finishes (also from the upload queue), ready to
launch. If that step ever fails the upload itself is unaffected; open
the **Library** and choose **"Add to home screen"** on the row (this is
the action formerly called "Register").

**Q: Where do uploads go by default?**
Under `/data/homebrew/` unless you pick a different drive in the Upload
screen. Common presets are offered: `homebrew` (recommended),
`exfat`, `ps5upload`.

**Q: Why can an upload say there is not enough space when the PS5 shows free space?**
Before anything is sent, ps5upload measures the whole job (including what an archive expands to)
and compares it with the console's free space.

A job whose bytes will not fit is refused in seconds with the shortfall in GB. A retry into the
same folder counts what is already on the console, so it does not ask for the whole folder again.
Two uploads into room for one: the second is refused at once. Free the amount shown, or pick
another destination.

**Q: Why does internal storage fill up faster than what I upload?**
The PS5 holds back extra space on its internal drive as data is written: about a fifth more than
the size of what you copy. We measured it on two consoles: a 10.4 GB upload took 12.5 GB and
12.1 GB off the free figure, and exactly 10.4 GB on an extended drive. Deleting the data gives all
of it back.

So on internal storage, plan for a game to need about 1.2 times its size. **Volumes** shows this
as **safe for new uploads**, which is what is likely to fit, not the raw free number. When an
upload is over that but its bytes would still fit, the Upload screen says it **may not fit** and
lets you go ahead; it is an estimate, so it never blocks you. If the console does run out part-way,
the partial upload is kept: free some space and press Resume. An M.2, extended or USB drive does
not have this overhead.

**Q: The PS5's storage screen shows more used, or more under "Other", than I expect.**
Two things from uploading can add to it. The first is the held-back space above, which grows with
everything stored on the internal drive and comes back when it is deleted. The second is an
upload that did not finish: its partial copy stays on the console so it can be resumed. Resume it
or delete the partial folder in **Files**; the helper also clears abandoned ones after a week.

**Q: My USB drive is too small for this game.**
The up-front check refuses it with the shortfall. Choose a destination with more room: the
internal drive (go by **safe for new uploads** in Volumes), an M.2 / extended drive, or a larger
exFAT USB drive, or free up space and click Retry.

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

**Q: Can I upload a compressed `.zip` of a game?**
Yes, and the files arrive **already extracted**. Drop the `.zip` on the Upload screen; ps5upload
decompresses it on your computer while sending, with no manual unzip. The screen previews
`zipped → extracted`, the file count and any embedded game. The destination folder is named after
the archive. Resume, excludes and the bandwidth cap work as for a folder. Only standard "Deflate"
zips are supported (Windows "Send to → Compressed folder" is fine); Deflate64, LZMA, BZip2, Zstd
and encrypted zips are rejected with a clear message.

**Q: What about `.rar` and `.7z`?**
Both work and also resume after a drop.

- **`.7z`**: LZMA2, which almost every `.7z` uses.
- **`.rar`**: including multi-part sets and password-protected archives. Pick the **first** part
  (`name.part1.rar`, or the plain `.rar` for `.r00` sets) and keep every part in one folder. A RAR
  holding several packages is unpacked straight to the console and each package installed in turn
  (base, then patch, then DLC).
- **`.rar` needs the desktop app.** The Android build and the web UI say so. `.zip` and `.7z` work
  on Android; no archive format works in the browser.

**Q: Does an archive upload need free space on my PC?**
No. The archive is decompressed and sent at the same time, so you only need room for the archive
you already have. The `PS5UPLOAD_ARCHIVE_STAGE_MB` and `PS5UPLOAD_ZIP_RAM_THRESHOLD_MB` variables
are accepted for old setups but do nothing.

**Q: Why does the Library sometimes show a game twice?**
If the same title is present both as a folder on disk and inside a
mounted disk image, both paths appear — but Library dedupes by
`title_id` and prefers the mount-backed path. Refresh the tab if
something still looks off.

**Q: Can I queue several uploads to run back-to-back?**
Yes — the Upload screen has a queue panel below the single-shot
controls. Each row shows live progress, current speed, and ETA
while running; the wall-clock-average MiB/s after it completes.
The runner processes one item at a time, and the queue persists across app restarts so a
queued item interrupted by a crash picks up cleanly when you
press Start again. Tick **Continue on failure** to keep going
when one item fails instead of stopping the whole batch.

**Q: How do I jump between volumes in the File System tab?**
The **Volume** dropdown above the breadcrumb lists every writable volume with its free space.
Pick one to jump to its root.

**Q: Does the File System tab remember where I was last?**
Yes, per console. The PS5 IP is remembered too.

**Q: Where can I see what is running across screens?**
The strip at the bottom of the window shows every in-flight operation (uploads, downloads, copy,
paste, delete, Library actions) with elapsed time, progress and speed. Copy and paste, Add files
and "Finishing on the console" show progress and an ETA, and **Cancel copy** stops a console copy
and cleans up only what it created. Click the strip for the full Activity tab.

**Q: My big upload stopped partway through. What now?**
A dropped connection, a helper restart or rest mode no longer ends the job: the app waits for the
console to come back and continues from what is already safely on it. If it gave up, press Start
again (or pick **Resume** at the prompt). On external USB/exFAT, a console that lost power
mid-write can leave a file inconsistent; if a finished upload looks wrong, use **Override** and
upload again.

To avoid interruptions on long transfers:
- **Your computer** is kept awake automatically while a transfer runs (**Settings → Keep Awake**
  also keeps it awake while the app is idle; greyed out on non-systemd Linux).
- **The PS5** has its own rest timer: raise it under **Settings → System → Power Saving → Set Time
  Until PS5 Turns Off**, or use **Settings → Upload → Keep the PS5 awake** in the app.
- **Flaky Wi-Fi?** A direct Ethernet cable is the most stable and fastest path (see the direct
  Ethernet section above).

---

## Mount + unmount

**Q: I uploaded a game *folder*, but ShadowMount+ won't mount it, yet `.exfat` images mount fine.**
ShadowMount+ mounts game **image files** (`.ffpkg` / `.exfat` / `.ffpfs`), not loose folders. Two
ways forward:

- **Register the folder in ps5upload.** In **Library**, on a folder with `eboot.bin` and
  `sce_sys/param.json` at its root, click **Register**, then **Launch**. If a PSN- or
  disc-extracted dump fails with a DRM error, use **Register (patch DRM)**. If the home-screen tile
  is blank, enable the ShadowMount+ metadata healer in ps5upload.
- **Give ShadowMount+ an image.** Upload the game as an `.exfat` or `.ffpkg` image.

**Library → ShadowMount+ panel → debug log** shows why a given item did not mount.

**Q: How do I find one game in a long library?**
Use the search bar in **Library**: a name fragment, a title ID prefix (`PPSA…`, `CUSA…`) or a path
fragment. Several words must all match.

**Q: Can I pick where a `.exfat` / `.ffpkg` mounts?**
Yes. The Library **Mount** button opens a dialog with a volume, a subpath and a name (taken from
the image filename). Your last choice is remembered per console. Some PS5 game scanners only look
in `/mnt/ps5upload/`, so the dialog warns when you mount elsewhere.

**Q: A mount from a previous session is still showing after I re-sent the helper.**
Expected: mounts are held by the PS5 kernel and survive helper restarts. Only a PS5 reboot clears
them. On startup the helper unmounts any mount whose backing device is gone.

**Q: The Library has a `MOUNTED` badge on a `.exfat` file. What does
that mean?**
The file is currently attached at `/mnt/ps5upload/<name>/`, and the
Mount button has flipped to Unmount. The Volumes tab shows the
mapping explicitly, with the source image path under each mount.

**Q: Does Unmount leave ghost tiles in the PS5 dashboard?**
No. Unmount unregisters every title inside the image first, then unmounts.

**Q: Can I edit what's inside a mounted image — replace files, add DLC,
or apply a backport patch?**
Yes. Mount the image **read-write**, then edit it in the **File System**
tab like any other folder on the console.

- **Library → the image → Mount**, and leave **"Mount read-only"**
  unchecked. (In the upload flow the same choice is the *"Mount
  read-only"* sub-option under *Mount after upload*.)
- Browse to the mount — it appears under `/mnt/ps5upload/<name>`.
- **Add files** copies files from your computer into the folder you're
  looking at. The **Replace** button on a file row overwrites just that
  file, keeping its name — that's how you drop in a patched `eboot.bin`
  or a rebuilt `.prx`.
- **Unmount when you're done** so the filesystem is flushed cleanly.

This is what makes the backport workflow possible when a game ships as
an image rather than a folder: the SDK Version Changer (step 2) rewrites
`eboot.bin` and every `prx`/`sprx` **in place**, and BackPork (step 3)
needs a `fakelib/` folder created *inside* the title. Neither can happen
through a read-only mount. Adding DLC or replacing assets works the same
way.

**Read this part before you start.** Edits go straight into the image
file — there is no undo and no staging copy. A bad `eboot.bin` or a
corrupted `sce_sys/param.json` can leave the title unbootable, and if
the image is your only copy of the game, it is gone. Copy the image
first if it matters to you. Two more things worth knowing: the PS5 can
write save data into a read-write mounted image on its own (which is why
read-only is the default and the safe choice for anything you just want
to *play*), and on some firmwares the kernel refuses a read-write mount
of a UFS `.ffpkg` regardless of what you asked for — when that happens
the app tells you the mount came back read-only, and writes will fail
until you convert or re-create the image.

**Q: ShadowMount+ mounts my game read-only — how do I edit it?**
Use **Library → the image → ⋯ → Edit files…**. That is a *checkout*: the
app moves the image out of ShadowMount+'s scan folder, waits for
ShadowMount+ to let go of it, and mounts it read-write where you choose.
When you press **Finish editing** it unmounts (which is what flushes your
changes into the image file), moves the image back, and ShadowMount+
picks it up and re-registers it within about a minute.

The detour is not busywork. ShadowMount+ mounts everything it manages
read-only by default, and it re-attaches any image whose mount
disappears on its next scan sweep (15 s by default) — so simply
unmounting its mount and re-mounting the image yourself would leave two
attachments on one image file, one of them writable. Moving the image
out of its view is the only way to get exclusive access.

Two consequences worth knowing before you start:

- **While the session is open, the game is gone from the PS5 home
  screen.** ShadowMount+ can't see the image where it now is, so the
  tile disappears until you finish. The app shows a standing banner on
  the Games and Files screens for exactly this reason.
- **An interrupted session is recoverable.** The checkout is journalled
  on the console itself, so if the app crashes, the console reboots, or
  you just close the window mid-edit, the banner comes back the next
  time you connect — from any machine — and **Finish editing** still
  puts the image back.

Everything in the previous answer about there being no undo applies with
full force here: you are editing a real game image in place.

**Q: Can I unmount while a game is running?**
No — the kernel refuses with `EBUSY` because a process inside the
mount has files open. The UI surfaces this as: *"the game inside
this image is currently running on the PS5. Exit it (PS Home →
close the game) and try again."* Same protection applies whether
you trigger Unmount from the Library tab or the Volumes tab.

---

## My uploaded game won't launch

This is almost never a bug in ps5upload: the transfer worked, and Sony's side
refuses to start the title. Check these in order.

1. **A PS5 fake game on firmware above 11.60.** The package installs, but PS5
   fake game packages can't be played on firmware above 11.60. This is the case
   for a PS5 game installed from a fake package on FW 11.61, 12.xx or 13.60.
   PS4 packages are fine, and PS5 homebrew apps (Itemzflow and similar) launch.
   The app shows a warning for this combination on Install, Convert and when a
   launch fails. Use a PS4 package, or the folder-dump route below.
2. **"View product" or "missing base entitlement".** Install the base game
   before its update or DLC. A package that needs an entitlement the console
   does not have will always show "View product".
3. **kstuff or ShadowMount+ is not running.** Launching needs kernel access.
   The Installed screen says when the helper has none; load kstuff and
   reconnect. Disc-image titles need ShadowMount+ running.
4. **Convert to exFAT + ShadowMount+ (reported working on FW 13.60).** Put the
   decrypted game folder on the console's drive (or convert it to an `.exfat`
   image with **Convert**), let ShadowMount+ mount it, and launch from the
   console's home screen. This does not depend on the package installer.
5. **A launch the app sent is not the same as a game that started.** The
   console accepts a launch and may take a while on the first start. If the
   game never appears, close it from the PS5 and start it from there.

# Installing packages

## Install Package

**Q: Can I install fake packages from the desktop?**
Yes, with **Install Package**. It has two tabs, for the two ways a package can reach the PS5:

- **Stream & install**: the PS5 installs straight from this computer or from a link. No copy is
  put on the PS5 first, so it needs no spare space there. This is the most reliable way.
- **Upload & install**: the package is copied to the PS5 first
  (`/user/data/ps5upload/pkg_library/`), then installed from that copy. The copies stay listed
  with cover art and size, and each has **Install**, **Reinstall** and **Delete**.

Pick a PS4 `.pkg` or a PS5 `.fpkg` (a fake package often keeps the ordinary `.pkg` extension).
What is installing or waiting shows in the queue at the top of the screen and in **Tasks**, and
keeps going when you open another screen.

**Q: Stream & install or Upload & install: which one?**
Use **Stream & install** unless you have a reason not to. Use **Upload & install** when the PS5
cannot reach this computer (Stream fails with a network error: see the next question), when you
install from a phone, or when you want to keep packages on the PS5 to reinstall later without the
computer. Upload needs free space on the PS5 for the copy as well as for the installed game.

**Q: How do I install from a download link, and which of the three ways do I pick?**
On the **Stream & install** tab, paste a direct link to a `.pkg` under **Install from a link**.
A link that serves a package installs even without ".pkg" in its address. Then choose who fetches
it:

- **Stream through this computer** (the default): this computer downloads over several
  connections and passes it straight to the PS5. Usually the fastest, nothing is saved to disk,
  and it works when only this computer can reach the link (a private server, a VPN). Keep the
  computer awake until it finishes.
- **The PS5 downloads it**: the PS5 fetches the link by itself and you can close the app once it
  has started. The PS5 must be able to reach the link, and its progress shows on the PS5, not in
  the app. If the PS5 cannot fetch it, the app streams it through this computer instead.
- **Download here first, then install**: this computer saves the whole package, then installs
  it. The slowest and it needs disk space, but a link that expires or drops part-way only costs
  a retry of the download.

**Skip the certificate check** is offered for the two ways where this computer makes the
connection (your own server, or a site with an out-of-date certificate). It cannot apply when
the PS5 downloads the link itself.

You can give a link a **name**. The name is shown in the queue, in Tasks and under **Recent
links** instead of the address, so with several links in play you can tell them apart and retry
the right one. Recent links are kept per console; use one again with **Use**, rename it or
remove it.

**Q: Can I install packages that are inside a ZIP, 7z or RAR?**
Yes: **Upload & install → Install packages from an archive**. Only the `.pkg` files in the
archive are unpacked, straight to the PS5 (nothing is extracted on your computer), and installed
one by one: the base game first, then its update, then DLC. If an install fails, the unpacked
files stay on the PS5 so you can retry.

- A **multi-part RAR** (`.part1.rar`, `.part2.rar`, …) works: choose the first part and keep the
  others in the same folder.
- A **password** is asked for when a RAR needs one. Password-protected ZIP and 7z files are not
  supported.
- If the archive is **behind download links**, open "The archive is behind download links" in
  the same card and paste the link, or one link per part. The files are downloaded to this
  computer first (`Downloads/ps5upload`), which needs room for all of them.

Every install runs through ps5upload's own on-console installer (port `:9115`). The app sends it
when it is not already running, alongside the helper, so the connection does not drop. An install
is reported done only once the console has pulled the whole package and the result checks out.
A link that serves a `.pkg` installs even without ".pkg" in the URL.

**Q: Stream install fails before the PS5 downloads anything?**
The PS5 pulls the package from your computer, so it must be able to reach it. Allow ps5upload
through the firewall (on Windows, for both Private and Public networks), keep both on the same
network with any VPN off, and set the PS5's Proxy Server to "Do Not Use" (Settings → Network →
Settings → Set Up Internet Connection → your connection → Advanced Settings). On Windows the app
names the network adapter and whether Windows treats it as Public, with one-click fixes.
**Upload & install** works without this connection.

**Q: How do I turn a game folder into an installable package? (Convert Games)**
Open **Convert Games**, pick a decrypted game folder or an `.exfat` / `.ffpkg` image. The app
checks it has what a launchable package needs and builds a fake package on your computer,
compressed the way Sony's packages are. It reads games from the console through the helper, so
ftpsrv is not needed. Then choose **Stream install** or **Upload & install**, or keep the package.
The console needs kstuff, `a53_ppr_install_fast.elf` and `shadowmountplus.elf` loaded first. Keep
the game files you converted from.

**Q: How do I turn a game folder into a game image (`.exfat` or `.ffpfsc`)?**
Open **Convert Games**, pick the game folder on your computer, and under "Or make a game image
instead of a package" choose:

- **Make game image (.exfat)**: one file, the same size as the folder, and the most compatible.
- **Make compressed image (.ffpfsc)**: usually 40–60% smaller. It takes longer and needs room
  for both files while it is made; only the compressed one is kept.

Nothing is installed. Copy the image to a folder ShadowMount+ watches on the PS5 (for example
`/data/homebrew`, with **Upload**), and ShadowMount+ mounts it and puts the game on the home
screen. The image is read back and checked against the folder before it is kept. The converter
is part of the engine, so it is the same on every desktop platform; it was checked on macOS,
and an image it made ran a game on a PS5 on firmware 13.60. A folder that is on the console or on a saved server cannot be made
into an image from here; copy it to this computer first.

**Q: Can I install a package that is already on a USB drive?**
Yes. Plug the drive into the PS5; packages on it show in **Install Package → External Packages**
with an **Install** button. The app first copies the package to internal storage on the console
(drive to drive, fast), because Sony's installer cannot read the exFAT USB mount directly.

**Q: What is the difference between `.pkg`, `.fpkg`, and `.ffpkg` here?**
`.pkg` and `.fpkg` are accepted by **Install Package**. The app validates their
actual header instead of trusting the suffix: PS4-style packages use a CNT
container, while a finalized PS5 package uses an FIH envelope with an embedded
CNT. `.ffpkg` and `.ffpfs` are UFS filesystem images and remain in the mount /
File System workflow; they are not sent to Sony's package installer.

For a PS5 FIH package the app also reports **fake/debug** versus **retail** from
the envelope. Installing a retail package can be useful for reinstalling owned
content or applying an official update, but installation does not grant a
license—the console still needs a valid entitlement to launch licensed content.
PS4 CNT fake-versus-retail classification requires a deeper cryptographic probe,
so the app leaves it unknown rather than guessing from `.pkg` or a filename.

The A53/PPR patches are a separate mount-time PFS-key path; they do not disable
Sony's package-registration policy. Package installation still relies on
kstuff's ShellCore installer patches and Sony's AppInst installer.

**Q: I have a base game and an update. Does the order matter?**
Yes: install the **base first**, then the update, then DLC. They share an ID but live in separate
places (`/user/app/<id>/` and `/user/patch/<id>/`) and never overwrite each other. The app warns if
you try an update before its base, since Sony's installer would accept it and install nothing.

**Q: The app says a package is "installed but may not launch".**
The app confirms the game landed before reporting success. If the install was accepted but the
content never appeared (usually missing kernel patches), it says so. If a title will not start,
reinstall from the console's **Package Installer** (Settings → Debug Settings → Game).

**Q: I am installing an update. Is my installed game safe?**
Yes. The app reads the real "update vs full game" flag from the package and applies an update on
top of your game, then checks the game's version went up and tells you if it did not. If an
update will not go through the app, use the console's **Package Installer** (Settings → Debug
Settings → Game → Package Installer).

**Q: "Port 9021 is not open on <ip>", but I loaded an ELF loader.**
9021 belongs to the **ELF loader**, a separate jailbreak component, so the problem is upstream of
ps5upload. In order:

1. **Is 9021 your loader's port?** `elfldr` uses 9021; some bundles use another port or have no
   loader. If yours differs, change the port in Settings.
2. **Did it start?** Autoloaders silently skip failed payloads. Re-run the jailbreak and watch for
   the loader's notification.
3. **Did the console sleep or reboot?** Everything jailbreak-side is lost on reboot and usually on
   rest mode.
4. **Right IP?** Check Settings → Network → Connection Status; a stale DHCP lease is common.
5. **Can your computer reach it?** "AP / client isolation" on the router blocks device-to-device
   traffic; wired Ethernet avoids it.

If **Find PS5s on the network** finds the console but 9021 is closed, the loader is the problem;
if discovery finds nothing, it is the network.

**Q: A PS4 `.pkg` update won't install.**
If the console rejects one, that is Sony's verdict. Usual reasons: the **base game is not
installed**; a **version mismatch** (a 1.05 patch will not apply over a 1.02 base); a **different
content id or region**; or the jailbreak **lacks live kernel patches** (kstuff / fpkg-enable).

**Self-hosted / web UI:** the engine must carry the installer ELF. Released engine binaries, the
official Docker images and an engine built from a full checkout include it; otherwise point
`PS5UPLOAD_PAYLOAD_DIR` at a folder that holds `ps5upload-installer.elf`.

**Q: Can I move a save from one PS5 to another?**

Yes, and you don't need a USB stick for it. There's no one-click
"send to my other console" button, but the two-step route works today:

1. On the source console, open **Saves**, find the title, and press
   **Download**. You get a `.zip` on your PC.
2. Switch to the other console in the roster sidebar, find the same
   title in **Saves**, press **Restore**, and pick that `.zip`.

**Backup to USB** / **Restore from USB** does the same thing without the
PC in the middle, if you'd rather carry a drive between the consoles.
Either way there are "all titles" buttons at the top to move everything
at once.

Two things to know before you do it:

- **The target console must already list a save for that title.** Restore
  writes into an existing save's folder; it can't conjure a save slot out
  of nothing. So install the game on the second console and launch it once
  — far enough for it to create its own save — then restore over it.
- **Restore wipes the current save first, and there's no rollback.** If
  the upload is interrupted partway, that save can be left empty. If the
  save on the target console matters, download it first.

For PS4-format saves, the emulator may not notice the new data right
away; close and reopen the game, or restart the console, if it doesn't
show up.

**Q: Can I install a split package (`*.0`, `*.1`, …)?**
Not through Install Package: it takes a single package file. Pick the single lead `.pkg`.

**Q: Where do uploaded packages live, and are they cleaned up?**
At `/user/data/ps5upload/pkg_library/<ContentID>.pkg`, and they stay so you can reinstall. Remove
one with **Delete** on its row, or turn on **Auto-delete each package from the PS5 after it
installs**. A **Stream install** never leaves a copy on the PS5.

To keep the copies on another drive (an M.2 or USB drive), open **Volumes** and press **Store
package copies here** on that drive. This only moves where the copies are kept. It does not
change where games are installed: the PS5 decides that in its own Storage settings.

**Q: What is ps5upload keeping on my PS5, and how do I clean it up?**
**Volumes → Kept by ps5upload** lists the folders the app writes on each drive (package copies,
package temp files, save backups, test files) and how much each holds. **Clean up** empties the
temp and test folders. Your package copies and save backups are only opened from there, never
deleted. **Health Check → Leftover files** also removes unfinished files from interrupted
transfers, package temp copies older than an hour and old speed-test files, and shows the list
before deleting anything.

---

## Install routes: what works for what (support matrix)

Most "the installer is broken" reports are really "this route doesn't work for
this kind of content on this firmware". Find your content in the left column,
then use a route marked **works**. Error toasts in the app link here.

The routes:

- **Stream & install** — the PS5 downloads the package from this computer (or
  the engine) over your network. Nothing is staged on the console.
- **Upload & install** — the package is copied to the console first and installed
  from there. A phone can only use this route.
- **Folder dump + ShadowMount+** — a decrypted game folder on the console's
  drive, mounted by ShadowMount+. No package, no installer.
- **exFAT image + ShadowMount+** — the same game as a single `.exfat` /
  `.ffpkg` image, mounted by ShadowMount+.

| Content | Stream & install | Upload & install | Folder dump + ShadowMount+ | exFAT image + ShadowMount+ |
|---|---|---|---|---|
| **PS4 package** (base game) | Works. The most reliable route; PS4 fake packages play on every firmware. | Works on FW 13.60 from 6.3.1 (a 3.6 GiB base installed over itself on both of our consoles). Before 6.3.1 Sony refused it with `0x80B2116F`; see the firmware note below. | Works (PS4 folder dumps). | Works. |
| **PS5 package** (fake / FPKG game) | Installs. **On firmware above 11.60 the game installs but cannot be played.** | Same as Stream. | Works for a decrypted PS5 dump — the route people use on 13.60. | Works — also the route used on 13.60. |
| **PS5 homebrew app** (for example Itemzflow, `IV0002-ITEM00001`) | Works and launches, including on FW 13.60. | Works. | n/a | n/a |
| **Patch / update** | Works (verified: a PS4 base plus its patch, streamed). Install the base first. | Works on FW 13.60 from 6.3.1 (a PS4 1.00 base took its 1.04 patch this way). Install the base first. | n/a — copy the update into the game folder. | n/a |
| **DLC** | Works after its base is installed. | Works after its base is installed. | n/a | n/a |
| **FPKG made by Convert** | Same as the content type above. | Same as the content type above. | Convert output is a package, not a folder. | Convert can start from an `.exfat` image. |
| **Folder dump** | n/a | n/a | **Works** — see "My uploaded game won't launch" for the recipe. | Convert the folder to an `.exfat` image first. |

Firmware notes:

- **Upload & install (`0x80B2116F`):** fixed in 6.3.1. It was our bug, not a limit of the PS5:
  the install request we handed Sony was a few bytes short, so Sony read leftover memory and
  sometimes took the package for a patch. That refused every Upload & install, and made some
  Stream installs fail at random until the helper was sent again. Measured on FW 13.60 (two
  consoles): refused every time before, accepted every time after. FW 9.60, 11.20 and 5.10
  (`0x80B2150F` there) showed the same refusal and should be fixed by the same change, but we
  have not been able to test them since. If you still see it, the app serves the package from
  the engine as a second try, and offers **Retry with Stream**.

### What the common messages mean

- **"This PS5 never reached this computer" (`0x80431064`, `0x80431068`,
  `0x8041013d`).** The console could not open a connection to the engine.
  Allow ps5upload through the firewall (on Windows, for both Private and Public
  networks), keep the PS5 and the computer on the same network with any VPN
  off, and set the PS5's Proxy Server to "Do Not Use". If the address in the
  message is not your computer's LAN address (a VPN, virtual-machine or
  container address), set `PS5UPLOAD_PKG_HOST_IP` to the LAN IP (for example
  `192.168.1.20`) and restart the engine. In Docker, use host networking or set
  `PS5UPLOAD_PKG_HOST_IP` to the Docker host's LAN IP and publish port 19113.
  **Upload & install** works without this connection.
- **Proxy blocked the stream (`0x80431084`).** In the PS5's network Advanced
  Settings set Proxy Server to "Do Not Use", or use Upload & install.
- **`0x80B2116F` (or `0x80B2150F` on FW 5.10).** Sony took the package for a
  patch and refused it. Before 6.3.1 this was a bug in ps5upload's install
  request, not a problem with the file or the route: update to 6.3.1 or later
  and let the app send its helper again. If it still appears, use **Stream &
  install** from a computer and send us a bug report.
- **`E2-80B22410`.** An error code the PS5's own installer or system software
  shows; ps5upload does not generate it and we have no confirmed single cause.
  It has been reported on packages the console would not accept as built.
  Try Stream & install, make sure the package is complete and matches the
  console (PS4 vs PS5), and send a bug report from the app if it persists.
- **`CE-108255-1`.** Another code the PS5 shows itself, usually when a game or
  app fails to start rather than when an install fails. See "My uploaded game
  won't launch" below; if it appears right after an install, the checks there
  apply.
- **"View product" instead of Play, or "missing base entitlement".** The
  console treats the title as one you have not bought. An update or DLC
  installed without its base game, or a package whose entitlement is not
  present, shows this. Install the base game first, or use a method that
  doesn't rely on an entitlement (see below).
- **"This PS4 game isn't playable on PS5".** The console's own message for a
  PS4 title it will not run. It has been seen with an update installed without
  its base and with fake-package support not loaded. Install the complete
  package (base first) with kstuff running, and see "My uploaded game won't
  launch" below.

# Console and tools

## Console tools

**Q: Where are my PS5 screenshots and video clips?**
In **Screenshots & clips**: one screen with a tab for each. Preview and download them to your
computer; to delete one, use **Files**.

**Q: What does the Game Activity Tracker actually track?**
Any game you launch, however you launch it — with your controller, from
the PS5 dashboard, or from this app. It works by watching the console's
process list, so it has no idea *how* a game was started.

Two things to know before you trust the numbers:

* It only counts time **while the payload is loaded**. Reboot or rest
  mode stops the payload, and play time during that window is never seen.
* It polls every 30 seconds, so a very short session can be missed
  entirely. Your history survives re-sending the payload.

The **Recently Played** list is separate — it reads the console's own app
database, so it shows titles regardless of whether the tracker was
running. It is not Sony's play-time record; the console does not expose
that to us.

**Q: What is the Cheats screen?**
It applies memory patches to a **running** game. A watcher polls every few seconds for a game
process, applies always-on patches and re-applies the ones you toggled. Cheat files download from
community repositories inside the screen. Native JSON, SHN trainer XML and encrypted MC4 files all
work, with game names, filters and a notification when cheats load. Cheats change nothing on disk
and stop when the game does.

**Q: What is the Remote Play screen?**
It requests a Remote Play registration from the console so you can pair a client without the PS5's
own menus. It shows the pending PIN with a countdown, reads "waiting" until a device really pairs,
and can be cancelled. It does not stream anything; use Sony's Remote Play app or Chiaki.

**Q: What does the fan curve do?**
It sends a custom temperature-to-fan curve to the console. **Restore default** returns to the
stock behaviour.

**Q: What is nanoDNS?**
A small DNS server that runs **on the console** (from the Payloads
catalogue, not built in). It blocks PSN and update domains by default and
can redirect any domain to a machine on your LAN — useful for staying
offline-friendly while jailbroken. This app's nanoDNS screen edits its
config: it detects which version is running and preserves your existing
rules, comments and custom resolvers rather than rewriting the file.

Point the console's DNS at it once it's running.

**Q: What is the Backup screen?**
It snapshots console-side files to a tagged copy on the PS5 and restores
them later. It is meant for the small, breakable things — app database
state and configuration — before a bulk operation, not as a game backup.
For save data specifically, use the Save data screen.

---

## FTP, SMB, metadata, and backport helpers

**Q: My FTP client says "Cannot rename across devices". Why?**

Because the alternative was crashing your console.

On the PS5, renaming a file from one drive to another (say a USB drive to
internal storage) doesn't fail cleanly the way it does on a PC — it panics
the kernel and locks the console up hard. Many FTP clients implement
"move" as a rename, so dragging a file between two folders on different
drives would trigger it.

The server now checks first and refuses with a 553 instead. To move a file
between drives, copy it to the new location and delete the original —
that's byte-level I/O and is perfectly safe. Renaming *within* one drive
still works normally.

The same protection applies to the File Manager and to `mv` in the Shell
screen.

**Q: What is the FTP Server screen for?**
It starts a small **FTP server on the PS5** (like `ftpsrv.elf`), so
FileZilla, curl, or another PC can connect **to the console**. Default
port is **2122** so it does not fight with ftpsrv on **2121**. Use this
for interop with other tools — for bulk game uploads, prefer the
Upload tab (AVA1 is faster and resumes).

**Q: What is the SMB Browser for?**
It browses a **Windows share or Samba NAS from your computer** (not
on the PS5). You can download a file to this PC, or **upload a file
or whole folder straight to the PS5** in one step: the engine streams
straight to the PS5 over AVA1, resuming if the link drops. Destination
works like Upload — set a parent path such as `/data/homebrew` and the
source name is appended.

**Q: What is Game Metadata (was “TMDB”)?**
Not The Movie Database. It looks up a title ID’s display name. Names
come from the console’s app database when the title is installed;
the old PlayStation Store scrape no longer works. Most users never
need this screen — Library already shows titles.

**Q: SDK Changer vs Fakelib / BackPork — is that “backport”?**
Related halves of a workflow, not one magic button:

1. **SDK Changer** — rewrites a folder dump’s declared firmware/SDK
   when the files are **not** signed SELFs.
2. **Fakelib** — put newer system libraries in `game/fakelib/`,
   optionally apply a **BPS** patch first.
3. **Runtime** — **BackPork** *or* **ShadowMount+** mounts that
   `fakelib` when the game launches (don’t run both).

We help prepare and ship files; we do **not** guarantee every high-FW
title will boot on an older console.

---

## Advanced

**Q: Can I run the engine standalone (without the desktop app)?**
Yes — `ps5upload-engine` is a self-contained HTTP server listening
on `localhost:19113`. The desktop app uses it under the hood; CLI
users can hit the `/api/*` endpoints directly.

**Q: Can I write my own client against the payload?**
Yes. AVA1 is specified in [`protocol/ava1/SPEC.md`](protocol/ava1/SPEC.md); every message is
defined once in `protocol/ava1/schema/ava1.toml`, and `protocol/ava1/vectors/` holds byte-exact test
vectors. You must pair with the console: it shows a six-digit code your client must display too.

**Q: How do I contribute a translation?**
Edit `client/src/i18n/locales/` (English, `en.ts`, is the source of truth) or use
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
The app checks GitHub once per launch and shows a dot on the Settings entry when a newer version
exists. **Settings → Updates → Download** saves the archive for your platform to your Downloads
folder (the Windows installer updates itself). Quit, replace the old app, relaunch, then use
**Update helper** if the app asks. Remember to update the app and the helper together.

# Help

## Troubleshooting

**Q: Something is not working. Where do I start?**
Open **Health Check** (also on **Home**, which shows anything that needs attention). It checks,
for the console you have selected:

- **Connection and helper**: the helper answers, and its version matches the app.
- **Network**: the PS5's payload loader port (9021) is open; the PS5 can connect back to this
  computer (what **Stream & install** needs: a failure here is almost always a firewall); how
  long the helper takes to answer; whether ps5upload's installer is running on the PS5 (it
  starts with your first install).
- **This computer**: its data folder can be written. In Docker, whether the engine is handing
  the PS5 an address it can actually reach.
- **Storage, clock, Remote Play**: free space, the folders the app needs, the console's clock.

Every problem comes with what to do about it, and some have a one-click fix. The same checks
run in the desktop app, on Android and in the Docker web UI, because the engine does them.

**Q: My transfers are slow. How fast should they be?**
Run the **Speed test** in **Health Check**. It sends a test file to the PS5 and reads it back
the same way a real copy goes, then removes it from both machines. It measures your own network
between the computer and the PS5, not your internet.

- **90–115 MB/s**: a full gigabit link, as fast as the PS5's network port goes.
- **Around 11 MB/s**: a 100 Mbit link somewhere in the path. Use a Cat5e or better cable and
  check that every switch or router port in between is gigabit.
- **Anything in between, or lower**: usually Wi-Fi on either side, a VPN, or other traffic. A
  network cable to the PS5 is the fix.

**Q: The payload isn't responding; ports appear open but connections
get reset.**
The payload may have wedged on a Sony API call. Recovery:
1. PS5 Settings → Network → disable / re-enable Wi-Fi or Ethernet.
2. If that doesn't clear it, reboot the PS5.
3. Re-send the payload.

**Q: Can't connect, or it connects and then drops within seconds.**
- Did you load the helper? It is gone after a reboot or rest mode. Send it again from **Connection**.
- Is your firewall blocking ports **9120** and **9021** to the PS5?
- If you load ps5upload with an autoloader or PLDMGR, put **elfldr first** in the list, before
  `ps5upload.elf`; otherwise it connects and drops after a few seconds.
- Your computer and the PS5 need a route to each other but not the same subnet.

**Q: Launch from the Library did nothing.**
Launch starts the game; the first start can take a while. If it never appears, close it from the
PS5 and start it there. See "My uploaded game won't launch".

**Q: I see errors in the status bar but don't know what happened.**
Open the **Logs** tab. Every runtime error, failed API call, and
console warning ends up there with timestamps and expandable
detail. Click **Copy** or **Download** to grab a plain-text dump
for a bug report.

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

## Reporting a bug / sending logs

**Q: How do I report a bug so it actually gets fixed?**
Open **Diagnostics → Bug report** in the sidebar. Write what happened
(steps to reproduce, firmware, jailbreak/loader, file sizes), attach
screenshots if you have them, then click **Create bug report (.zip)**.
Post the `.zip` in the **#bugs-report** channel on the Discord. A zip
with logs is worth ten "it doesn't work" messages — it usually has the
exact error in it.

**Q: What's in the bundle?**
A single `.zip` containing only diagnostics (never your games or app
data):

- `report.json` — app version, OS, your description, and a snapshot of
  the connected PS5 (model, firmware, storage, running processes,
  loaded modules).
- `logs/app.jsonl` — the app's log for the time window you pick.
- `logs/engine.log` — the transfer engine's full log (survives a crash).
- `crash-reports/` — anything auto-collected when something errored.
- `ps5/klog.txt`, `ps5/syslog.txt` — the PS5 kernel logs (if a console
  was connected).
- `images/` — the screenshots you attached.

**Q: It's intermittent / hard to reproduce. How do I capture it?**
On the Bug report page set **Recording level** to **Debug** or
**Trace**, reproduce the problem, then create the report. The app keeps
a rolling on-disk log (under `~/.ps5upload/logs/`), so pick a **time
window** (last 1–120 minutes) that covers when it happened — you don't
have to catch it live. The level also lives on the **Logs** tab.

**Q: Is it safe to post publicly?**
Yes by default — **Redact IPs & serial** is on, which strips your PS5's
IP address and serial number from the bundle. Untick it only if a
maintainer asks for the raw values in a private channel.

**Q: Where are the logs kept, and do they fill my disk?**
`~/.ps5upload/logs/` (one file per day). They're capped — files older
than a few days are dropped and the folder is size-limited — so they
can't grow without bound. Use **Open logs folder** on the Bug report or
Logs page to find them.

---

## Disclaimer

**Use this software entirely at your own risk.** Provided "as is",
without warranty of any kind.

- It interacts with a **modified PS5** — a jailbroken console with
  kernel exploits already loaded by you. Modifying console state,
  bypassing platform integrity checks, or running unsigned code can
  void your manufacturer warranty, violate the platform's terms of
  service, and, under certain operations, leave your console
  unrecoverable without a reinstall.
- It writes to your PS5 filesystem and can install / register
  packages with Sony's installer. Mistakes can corrupt the app
  database or leave Sony's mgmt service wedged. Back up important
  saves before bulk operations.
- It is intended only for content you legally own and hardware that
  belongs to you. How you use it is your responsibility.
- No support guaranteed — free volunteer-built tool.

If those terms aren't acceptable, do not use this software.

---
