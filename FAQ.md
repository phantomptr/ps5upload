# FAQ

Answers to the questions that come up most often while using
ps5upload. Organized by topic — use the search box in the FAQ tab to
jump to what you need.

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

## What ps5upload does (and doesn't)

**Q: What is ps5upload?**
A cross-platform desktop app for moving files, game folders, and
disk images from your computer to a jailbroken PS5. Built around a
small custom payload that runs on the PS5 and speaks a binary
protocol (FTX2) over your LAN.

**Q: What does it actually do?**
- **Transfer** files and folders at near-wire speed, with BLAKE3
  per-shard verification and resume on drop.
- **Upload a compressed archive** — `.zip`, `.7z`, or `.rar` — of a
  game, decompressed on your PC and streamed in so it lands already
  extracted on the PS5 (no manual unpack, no temp copy of the whole
  game). RAR handles **multi-part sets** (pick the first part — the
  rest are found automatically) and **password-protected** archives.
  (RAR is desktop-only; Android uploads `.zip` and `.7z`.)
- **Mount** `.exfat` and `.ffpkg` disk images natively on the PS5
  (via MDIOCATTACH + nmount). No third-party tool required.
- **Install fakepkgs** — pick a `.pkg` and it uploads, installs, and
  (optionally) cleans up the staged copy. On the Upload tab a `.pkg`
  is a first-class queue item, so it rides the same queue as your
  folders, images, and archives — add it and it ends up playable.
  Three-tier pipeline routes Sony's installer through ShellUI's authid
  via ptrace RPC; verified end-to-end on FW 9.60. Game pkgs (CUSA /
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
- **Send payload** — push `.elf`, `.bin`, `.js`, `.lua`, or `.jar`
  files to the PS5's loader port (typical defaults: `.elf` → 9021,
  `.js` → 50000, `.lua` → 9026, `.jar` → 9025 for BD-JB / BDJ), with
  a replay-from-history panel.

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
- **Android** — `.apk` (sideload). Same interface, mobile-friendly;
  manages your PS5 over Wi-Fi. See the **Android** section below for
  setup, permissions, and uploading from your phone.

**Q: Which PS5 firmware works?**
ps5upload is built against PS5 Payload SDK v0.38, which resolves
kernel offsets at startup for every firmware it knows about. The
same binary runs on the full range **1.00 – 12.70**.

- **Hardware-tested:** FW 5.10 and 9.60 (in-house) and FW 12.20
  (user-confirmed). Core features — transfer, mount, file browse,
  hardware monitor, and `.pkg` install — work across this range.
- **1.00 – 12.70 supported.** The same binary runs everywhere the SDK
  has offsets. One cosmetic caveat outside 9–11: the Hardware tab's
  process list can show `<pid:N>` placeholders instead of real command
  names (the `p_comm` struct offset isn't SDK-exposed and our fallback
  table only has validated values for 9–11). Everything else is
  identical.
- **`.pkg` install** additionally needs the console's jailbreak to have
  live kernel patches (kstuff / fpkg-enable). Where those aren't active
  (observed on some 9.60 setups), the installer says so honestly instead
  of reporting a false success — it never claims a game installed when
  the content didn't actually land.

What actually gates users in practice is the PS5-side **ELF loader**
on port 9021 — a third-party component, not part of ps5upload. Loader
coverage is roughly 4.x–12.x today.

**Q: Which PS5 models are supported?**
All models: original CFI-1xxx, Digital, Slim (CFI-2xxx), and Pro
(CFI-7xxx). Transfer, mount, volume listing, and hardware info work
on every one.

---

## Android

**Q: How do I install the Android app?**
Download `PS5Upload-<ver>-android.apk` from the Releases page and open
it on your phone. Android will ask you to allow installing from your
browser / file manager the first time — that's normal for a sideloaded
app. Updates install in place and keep your settings (builds are signed
with a stable key from v2.20.0 on). If an older build refuses to
update, uninstall it once, then install v2.20.0 or newer.

**Q: The app's text/buttons are huge (or tiny) on my phone and names get
cut off. (3.2.0+)**
This is your phone's **Display size** (or Font size) accessibility setting
scaling the whole app up. Open **Settings → Text size** in ps5upload and pick a
smaller percentage — it rescales the entire interface so list rows stop getting
clipped to "ps5-image…". It works on desktop too, and persists.

**Q: I picked a game folder, a `.zip`, or a `.pkg`, but it fails — "could
not read", "No such file or directory", "not an ELF", or it just won't
send / install.**
Android needs **All files access** before the app can read your files.
This is the single most common Android issue.
- On **v2.20.0+** the app asks for it: when you tap **Choose folder**,
  **Choose file**, or **Add .pkg**, an in-app file browser opens. The
  first time, tap **Open settings**, turn on **"Allow access to manage
  all files"** for PS5Upload, come back, and tap **Retry**. Then browse
  your storage and pick the file/folder.
- On **older builds (≤ v2.19.x)** the app used Android's system file
  picker, which only returns a `content://` link the engine can't open —
  that's exactly why loading failed. **Update to v2.20.0+.**
- You can grant it any time: **Android Settings → Apps → PS5Upload →
  Permissions → All files access → Allow** (the app also nudges you on
  first launch).

**Q: How do I upload a big game (a folder or a `.zip`) from my phone?**
Open **Upload → Choose folder** (or **Choose file** for a `.zip` dump),
pick it in the in-app browser, choose the destination, and **Start**.
With All-files access the engine reads the file in place — nothing is
copied — so a multi-GB / tens-of-thousands-of-files game streams
straight to the PS5 over Wi-Fi.

**Q: "ShadowMount+ — downloaded asset is not an ELF (first bytes
[50, 4b, 03, 04])."**
Fixed in **v2.19.3+**. ShadowMount+ now ships its payload inside a
`.zip`; the app extracts the real `.elf` automatically. Update, then
**Send to PS5** (with the payload chain loaded — see the next answer).
`50 4b 03 04` is just the "PK" signature of a zip file.

**Q: The `helper` dot at the bottom of the app is red.**
That dot is the **PS5-side `ps5upload` payload**, not the app itself.
Red means it isn't running on the console yet, so Install Package,
mounts, and file-system actions can't complete. Load the recommended
chain first — **Set up your PS5**, or the **Payloads** tab (kstuff →
ShadowMount+ → ps5upload). Once the payload is running, `helper` turns
green. The other dot, `engine`, is the app's own service and is green
whenever the app is open.

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

1. **Update to the latest version — the fix is now built in.** As of
   2.14.0 the app sets `WEBKIT_DISABLE_COMPOSITING_MODE=1` and
   `WEBKIT_DISABLE_DMABUF_RENDERER=1` itself at startup, so a plain
   double-click of `PS5Upload.AppImage` (or the `.deb` / folder build)
   should render correctly — no wrapper needed. On older builds, launch
   via `./PS5Upload.sh` (shipped in the release `.zip` next to
   `PS5Upload.AppImage`), which sets the same vars. (If you ran
   `WEBKIT_DISABLE_DMABUF_RENDERER=1 ./PS5Upload.AppImage` and it didn't
   help, the missing piece was the *compositing-mode* var — both are now
   set automatically.)

2. **Still white?** Force X11 instead of Wayland:

   ```sh
   GDK_BACKEND=x11 ./PS5Upload.sh
   ```

3. **Still white?** Fall back to software rendering (slower UI, but
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
  around 110–118 MiB/s for huge files. The 2.16.0 multi-file
  `posix_fallocate` fix is what keeps multi-GB transfers from
  collapsing under that ceiling; before that fix, throughput
  degraded the longer a single file ran.
- **2.5 GbE / 10 GbE adapters on the computer** are fine and still
  negotiate to 1 Gbps because the PS5 is the slowest link. The
  extra headroom helps if you ever swap consoles.
- **Don't share two connections at the same IP** — if your PS5 is
  on both WiFi (DHCP from the router) AND this direct cable, give
  them different IPs so the routing table doesn't get confused. The
  `192.168.88.x` range above sidesteps any conflict with the common
  `192.168.0/1.x` home subnets.
- **Resuming an interrupted upload still works** — if you start an
  upload over the direct cable, swap to WiFi mid-way (PS5 picks up
  a different IP), the engine's resume-by-tx_id flow lets you
  re-target the WiFi IP and pick up where it left off.

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
`.bin`, `.js`, `.lua`, or `.jar` file, set the port to whatever your
loader listens on (the screen has a built-in cheat sheet — `.elf` →
9021, `.js` → 50000, `.lua` → 9026, `.jar` → 9025 for BD-JB / BDJ),
and click **Send**. The app probes
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

**Q: Can I run the transfer engine on a different machine (remote /
self-hosted engine)? (3.3.7)**
Yes. The app normally launches its transfer engine as a bundled background
process on `127.0.0.1:19113`, but you can run that engine somewhere else — a
home server, a NAS — and point the app at it in **Settings → Engine URL**.
An official multi-arch image (`linux/amd64` + `linux/arm64`) is published at
`ghcr.io/phantomptr/ps5upload-engine` — use `:latest` for the newest release
or `:<version>` to pin (e.g. `docker pull ghcr.io/phantomptr/ps5upload-engine:3.3.16`). When the URL isn't loopback, the app skips the
bundled engine and talks to your remote one (cover art included). Two things to
know: (1) the engine's API has **no password**, so to let a remote machine reach
it you set `PS5UPLOAD_ALLOW_IP` to that machine's IP — do this **only on a
trusted home network**, never expose the engine to the internet (it can read,
write, and delete files on your PS5); and (2) the engine still needs a network
route to the PS5 (`PS5_ADDR`). Leave Engine URL blank/loopback for the normal
all-in-one mode.

**Q: Opening the Hardware screen drops the connection ("Couldn't read
hardware info — connection refused").**
A single hardware reading that misbehaves on a particular firmware or
ELF-loader could take the whole helper down — you'd see "connection
refused" and have to re-send the payload. Fixed in 2.28.1: a reading that
misbehaves now just shows as a dash for that one field instead of dropping
the connection, and the helper logs exactly which reading misbehaved so a
bug report pinpoints it. If you're on an older build, update; if a console
ever still drops, re-send the payload from the Connection tab (reboot the
PS5 only if it won't reconnect).

**Q: Can I manage several PS5s at the same time?**
Yes (2.29.0+). Add each console — the Connection screen, or the **+** on the
console tab strip — and a tab appears at the top for every PS5, each with its
own live status dot. Click a tab to view and operate that console. Each
console runs its own upload queue and its own package installs **independently
and in parallel**: start an upload or install on one, switch tabs, start
another — they don't wait for each other. (Switching tabs only changes what
you're looking at; every console's work keeps running in the background.) The
single-console layout is unchanged until you add a second PS5.

**Q: Is it safe to do many things on several consoles at the same time?**
Yes — and 2.30.0 hardened this further. The helper running on each PS5 now
serializes its low-level kernel operations internally, so things that happen
to overlap — reading a console's live CPU/SoC temperature while a package is
installing, registering a game on one console while another is mid-install —
can't collide and crash the helper or black-screen the console. On the desktop
side, a slow or very large upload to one console no longer holds up the others:
each console's requests are handled on their own, so one busy PS5 can't freeze
the app for the rest.

---

## Transferring

**Q: How do I stop my PS5 from going into rest mode?**
**Settings → Upload → "Keep the PS5 awake"** has three modes:

- **Off** — the PS5 rests on its own schedule.
- **During transfers** (default) — while an upload is running, the app
  periodically resets the console's auto-standby timer so a long
  transfer can't be killed by rest mode.
- **Always while connected** — for as long as the app is open, every
  console whose helper is running is kept out of auto-rest entirely. A
  small ⚡ "keeping awake" indicator appears in the status bar so you
  always know it's active.

In every mode, putting the PS5 to rest manually (controller / console
button) still works immediately — the app only defers the *idle* timer,
it never blocks an explicit rest request. Closing the app returns the
console to its normal power schedule.

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
Yes — and the files arrive **already extracted** on the PS5. Keep the
game as a single `.zip` on your PC (less disk, easier to move), drop it
on the Upload screen, and ps5upload decompresses it on your computer
while streaming the files into the normal transfer pipeline. There's no
manual unzip step, and no temporary copy of the whole game on your disk
— it decompresses one file at a time (large files briefly spill to a
temp file), so a 100 GB archive doesn't need 100 GB of free RAM. The
screen previews what the archive expands to (`zipped → extracted`, file
count, and the % saved on disk) and detects the embedded game if it has
a `sce_sys/param.json`. The destination folder is named after the
`.zip` (minus the extension). Resume, excludes, the bandwidth cap, and
mirroring to other consoles all work the same as a folder upload.

A note on space savings: PS5 game data is often already compressed, so
DEFLATE (ZIP's method) sometimes shrinks a dump a lot and sometimes
barely at all — the preview's "saves N%" shows the real number for your
archive before you commit to sending it.

Since 2.18.5 the scan is effectively instant on any size archive, and
while it's reading the file you'll see a **"Scanning archive… N
entries"** counter so you can tell the app is working — useful when
the archive is on a slow USB drive that takes a moment to spin up.

**Q: What about `.rar` / `.7z`?**
Only `.zip` is supported. Modern scene `.rar` is usually split
multi-part + encrypted (and no other PS5 homebrew tool handles it
either), so unpack those on the PC and re-zip, or upload the extracted
folder directly.

If you re-zip with a tool on your phone or with 7-Zip, pick the
**standard "Deflate"** method (Windows's built-in "Send to →
Compressed (zipped) folder" always does the right thing). Deflate64,
LZMA, BZip2, Zstd and AES-encrypted zips are rejected with a clear
message because the in-app decompressor only handles standard Deflate.

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

**Q: My big upload dies partway through. Why, and how do I stop it?**
A multi-hour upload holds one TCP connection open the whole time. If
**either end goes to sleep**, that connection drops and the transfer
fails (you'll see a network/connection error, or — on external
USB/exFAT targets — a file that looks done but is incomplete). Two
things go to sleep:

1. **Your computer.** ps5upload now keeps the computer awake
   **automatically** while any upload, download, or install is running
   (macOS `caffeinate`, Linux `systemd-inhibit`, Windows
   `SetThreadExecutionState`) and releases it when the queue goes idle.
   You don't have to do anything. If you want the machine to also stay
   awake while the app is open but *idle*, turn on **Settings → Keep
   Awake**. (On non-systemd Linux that toggle is greyed out — the OS has
   no inhibitor to call.)

2. **The PS5 itself.** The console's rest-mode timer is **console-side**
   — ps5upload can't see or stop it from your computer. For a long
   transfer, raise or disable it first:
   **Settings → System → Power Saving → Set Time Until PS5 Turns Off**,
   and set **both** *During Media Playback* and *During Gameplay /
   General Use* to a longer value (or **Don't Turn Off**). A quick
   workaround if you'd rather not change settings: tap the controller
   every so often during the transfer — any input resets the idle timer
   (this is what some users do already).

If a transfer does get cut off, you usually don't lose the bytes already
sent: re-run it and pick **Resume** at the prompt (or press Start again
on the queued row) — it size-compares what's on the PS5 and only sends
what's missing. On external USB/exFAT, prefer **Override** + a fresh run
if a "completed" upload looks wrong, since a console that lost power
mid-write can leave the destination in an inconsistent state.

If your network itself is the unreliable bit (flaky WiFi, congested
router, multi-hundred-GB transfers), a one-time **direct Ethernet
cable** between the computer and PS5 gives the most stable + fastest
path — see the **Network setup — direct Ethernet** section above for
per-OS instructions and addressing.

---

## Mount + unmount

**Q: I uploaded a game *folder* (dump), but ShadowMount+ won't mount it
— yet `.exfat` images mount fine. Why?**
Because **ShadowMount+ mounts game *image files* (`.ffpkg` / `.exfat` /
`.ffpfs`), not loose extracted folders.** It watches `/mnt/usb*`,
`/mnt/ext*`, and `/data` for those image files and auto-mounts +
registers them — so your `.exfat` uploads "just work," but a raw dump
folder sitting in those paths is invisible to it. This is a workflow
difference, not a corruption bug (your upload completed correctly).

You have two ways forward:

- **Keep the folder — register it directly in ps5upload (no ShadowMount+
  needed).** In the **Library** tab, on the uploaded folder game (it
  needs `eboot.bin` + `sce_sys/param.json` at its root), click
  **Register**, then **Launch**. This installs the title into the PS5's
  app database with a bind mount and starts it — the native,
  hardware-validated path for folder dumps. If a PSN- or disc-extracted
  dump fails Register with a DRM error, use **Register (patch DRM)**
  (rewrites `applicationDrmType` to `standard`). If the home-screen tile
  is blank after registering, enable the ShadowMount+ metadata healer in
  ps5upload (it copies `icon0.png`/art into `/user/appmeta`).
- **Keep using ShadowMount+ — give it an image, not a folder.** Package
  the game as an `.exfat` (or `.ffpkg`) image and upload *that* — exactly
  what already works for you. Don't upload the extracted folder if
  ShadowMount+ is your launcher.

On the **"old `config.ini` from a 1.xx version"**: that's *not* what's
blocking the folder mount (the folder-vs-image distinction above is), and
since your `.exfat` images mount fine your config is clearly functional.
But mixing a stale ShadowMount+ config/autotune with a newer
ShadowMount+ build is worth tidying: ShadowMount+ regenerates defaults if
they're absent, so you can back up and delete `/data/shadowmount/config.ini`
and `/data/shadowmount/autotune.ini` (the autotune file holds
version-specific, learned per-game timings — least worth keeping across
versions) and let the current build recreate them. Check
**Library → ShadowMount+ panel → debug log** for the exact reason a given
item didn't mount.

**Q: How do I find one game in a long library?**
The Library tab has a search bar above the games + images
sections (2.2.25+). Type a name fragment, a title ID prefix
(`PPSA…`, `CUSA…`), or a path fragment (`ext1`) — matching is
live, case-insensitive, and runs across all of `name`, `titleId`,
absolute `path`, scan `scope`, and `volume`. Multi-word queries
AND-match across fields, so `space ext1` narrows results to
titles named "space" on `/mnt/ext1`.

**Q: Can I pick where a `.exfat` / `.ffpkg` mounts?**
Yes (2.2.25+). The Library Mount button opens a modal with the
same UX as the Upload screen's destination picker:

- **Volume** — pick any writable PS5 volume from the dropdown
  (`/data`, `/mnt/ext1`, `/mnt/usb0`, …). Free-space readout
  appears next to each.
- **Subpath** — free-form, with the same preset chips as
  Upload (`homebrew`, `exfat`, `ps5upload`).
- **Name** — auto-derived from the image filename (e.g.
  `MyGame.exfat` → `MyGame`), editable.

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
Yes. **Install Package** is a package library: click **Add .pkg**,
pick the file, and it uploads once to
`/user/data/ps5upload/pkg_library/` on the PS5 and stays there. The
screen lists every uploaded package with cover art and size, and each
row has **Install**, **Reinstall**, and **Delete** — so you can install
again any time without re-uploading. Installs run through the **DPI
daemon** on `:9040`, which calls Sony's `sceAppInstUtilAppInstallPkg`
from a clean loader process (the path that isn't gated on current
firmware). Installing briefly swaps the ps5upload payload for the DPI
loader and restores it when done, so the connection may blip for a few
seconds. Hardware-validated on FW 9.60 with regular game pkgs (UP / EP /
JP / HP / CUSA / PPSA / PCSA / etc.).

**Q: Can I install a package that's already on a USB / external drive? (3.2.0+)**
Yes. Plug the drive into the PS5, open **Install Package**, and any `.pkg`
files on connected USB / external drives show up in an **External Packages**
section — each with a PS4/PS5 badge and an **Install** button. No upload from
your computer needed. Under the hood the app copies the package from the drive
to internal storage first and installs from there, because Sony's installer
can't read the exfat USB mount directly (it accepts the request but installs
nothing). The copy is on-console (drive-to-drive), so it's fast.

**Q: I have a base game and an update — does the order matter? (3.2.0+)**
Yes — install the **base game first**, then the update. A base game and its
update share the same ID, so the app keeps them in separate places and never
lets one overwrite the other (the base lands in `/user/app/<id>/`, the update in
`/user/patch/<id>/`). If you try to install an update before its base is on the
console, the app warns you — Sony's installer would otherwise accept it and
silently install nothing.

**Q: The app says a package is "installed but may not launch" — what's that?**
The app confirms a game actually landed on the console before reporting success.
If the install was accepted but the content never appeared (a firmware/jailbreak
that lacks the kernel patches fakepkg installs need), it tells you honestly
instead of a false "installed." If a title genuinely won't start, reinstall from
the console: **Settings → Debug Settings → Game → Package Installer**.

**Q: The app said the install failed, but the game is on my PS5 and plays fine. (3.3.4)**
This was a verification bug — fixed in 3.3.4. If your PS5's install location is set
to an **extended / M.2 SSD**, games install to that drive (`/mnt/ext…`) instead of
internal storage, and the app used to look only in the internal location — so it
wrongly reported "install failed" even though the game installed perfectly. 3.3.4
confirms the install by the actual data written to *any* drive, so extended-storage
installs are recognized correctly. If you saw this on an older version, the game was
fine all along — just launch it.

**Q: I'm installing a game update (patch) — is my installed game safe? (3.3.7)**
Yes. A game's update carries the *same ID* as the game, so an older build could end
up re-installing that ID and wiping the game instead of patching it. An update only
installs via the safe path that applies it *on top* of your game; if that path can't
apply it on your firmware, the update **fails harmlessly and your installed game is
left completely intact** — never overwritten or deleted. (3.3.6 added this but only
recognised updates labelled a certain way, so some **PS4** patches still slipped
through and wiped the base — hardware-confirmed. **3.3.7** reads the real
"update vs. full game" flag straight from the package on the console, so an update
is recognised no matter how it reached the PS5 — uploaded, copied from USB, or
picked in the File System tab — while a normal full-game *re-install* still works.)
If an update won't go through the app, install it from the PS5's own **Package
Installer** (Settings → Debug Settings → Game → Package Installer) — your base game
is safe either way. Always install the **base game first**, then the update.

**Q: A system (NPXS) pkg won't install. What do I do?**
System app pkgs (NPXS-prefix content_id — Store updates, Settings
patches, built-in apps) aren't what the DPI installer is built for and
often won't complete. Install those from the console itself: **on-PS5
Settings → Debug Settings → Game → Package Installer** — a privileged
code path ps5upload can't replicate.

**Q: Does Play in the Library register the title first?**
Yes (2.2.55+). Always-register-first: the Play button calls
`appRegister` (idempotent if already registered), retries with a
DRM-type patch on rejection, waits 600 ms for the app.db commit to
land, then calls `appLaunch`. Pre-fix the flow tried Launch first
and registered as a fallback — which surfaced misleading "not
registered" errors before registration kicked in.

**Q: Can I install a split pkg (`*.0`, `*.1`, …)?**
Not through Install Package — the DPI installer takes a single staged
file, so split sets are rejected with a note when you add them. Pick
the single lead `.pkg` only.

**Q: Where do uploaded packages live, and are they cleaned up?**
They live at `/user/data/ps5upload/pkg_library/<ContentID>.pkg` and
**persist** — nothing auto-deletes them, so you can reinstall any time
without re-uploading. Remove one with the **Delete** button on its row
(or from the File System tab); that frees the PS5 disk space. (This is a
change from older builds, where the staged file was deleted right after
each install.)

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

**Q: An upload fails with "Upload failed — BeginTx rejected (Error):
manifest_invalid".**
The PS5 refused the list of files before any data was sent. Two
common causes:
1. **A file or folder name contains an unusual character — most often
   a `}`.** Older payloads mis-read it and rejected the whole upload.
   The 2.23.0 payload reads these names correctly, so **reload the
   payload** (Connection → Send payload) and retry.
2. **A single destination path is too long** (the PS5 caps paths at
   512 bytes). From 2.23.0 the app catches this before the upload and
   names the offending file so you can shorten or rename it.
If you can't reload the payload right now, rename the offending file
or folder (drop the `}`, or shorten a deeply-nested path) and try
again.

**Q: Deleting a huge game folder used to fail with a "502 Bad
Gateway" error.**
Fixed in 2.2.22. The recursive walk on a small-file-heavy folder
(roughly 200k files / 20k dirs) takes minutes on PS5
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
