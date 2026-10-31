# FAQ

Answers to the questions that come up most often while using ps5upload. In the app's FAQ
screen, the tabs are the sections below, each topic lists its questions, and the search box
finds a word or an error code (for example `0x80B2116F`) in any answer.

---

# Start here

## What ps5upload does (and doesn't)

**Q: Where did a screen go?**
The sidebar is shorter; old links still land. **Collection**, **Installed games** and **Game
files** are all **Games** now: one grid with every game once, and each game's page holds
everything you can do with it; **Manage → Dump games** is **Dump this game** on each installed
game's page. **Upload** and **Install Package** (later **Send to PS5**) are **Upload & Install**,
**Convert Games** is **Workshop**, the PS5's **On-screen messages** are the **On the PS5** tab of
**Notifications**, **Network shares** and **Mesh Drive** are **Shared Folders**,
**Profile** and the Remote Play PIN screen are **Accounts**, and **Play** is **Remote Play**.
Game Activity's play time is in **Games**; **Search**, **Disk usage** ("What takes space?") and
**Console snapshots** are in **Files**; processes and ports are the **Running** tab of
**Payloads**; the **Firmware spoof check** is a card in **Console**; **Devices** and **Health
Check** are tabs of **Connection**; **Shell** is under Diagnostics; **Tasks**, **Logs** and the
**Audit log** are **Activity & Logs**; and **FAQ**, **About** and **What's new** are **Help**. A
pinned screen shows only under Pinned.

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
- **Convert games** (Workshop): build an installable package from a decrypted game folder or an
  image, or write a game folder as a game image (`.ffpkg`, `.exfat` or `.ffpfs`, optionally
  compressed to the smaller `.ffpfsc`) that ShadowMount+ mounts.
- **Health check and speed test**: checks the PS5, the app and the network between them, says
  what to fix, and measures how fast files move.
- **Games**: every game once in one grid (on this computer, installed, on the PS5's drives);
  each game's page mounts images, adds games to the home screen, plays, closes and uninstalls
  them. Browse and manage files.
- **Remote Play**: stream the PS5 into the app and play it with a controller, the keyboard or
  touch.
- **Cheats, fan curve, hardware view, power and wake, payload sender and backport tools.**

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
In this order: **kstuff** (the build with PS5 fake-package support), **a53_ppr_install_fast.elf**
(needed for PS5 fake packages) and **ShadowMount+**, plus the ps5upload helper. **Upload &
Install** shows the same list in its fake-package note at the top. Many setups also run **nanoDNS** (keeps
the console away from PSN and update servers) and **ftpsrv-ps5**; send those from **Payloads**.
ps5upload itself does not use ftpsrv-ps5. If an install is refused, first check that kstuff is
running.

**Q: Which firmware limits apply to what I install?**
- **PS5 fake *game* packages install above firmware 11.60 (including 13.60) but are not
  playable.** PS4 fake packages work and PS5 homebrew apps launch. The note at the top of
  **Upload & Install** says so before you send anything. See "My
  uploaded game won't launch" for the folder-dump route.
- On **13.60**, Stream & install is the dependable route.

**Q: Quick start: from a fresh jailbreak to my first upload.**
1. Confirm the console's ELF loader is running (port 9021) and note the console's IP
   (**Settings → Network → View Connection Status**).
2. Install ps5upload (or open the web UI). Use a wired connection if you can.
3. Open **Connection**, enter the IP, click **Check**.
4. Run **Set up your PS5**: it sends kstuff and ShadowMount+, then the helper, in order. If
   something already loads kstuff, send only ps5upload.
5. Wait for the `helper` dot to turn green. (If you turned on **Ask before a new device can
   connect** in Devices, a new device types the 6-digit code the console shows, once.)
6. Open **Upload & Install**, drop or choose your game, pick the drive and click **Start**. The
   free-space check runs before anything is sent.
7. Open **Games** to see it, and open the game for its actions (**On the PS5's drives** on its
   page). A `.pkg` goes in the same drop zone: choose **Stream & install** or **Upload &
   install** on its card. If it will not launch, see "My uploaded game won't launch".

---

## Getting started

**Q: Do I need the helper?**
Yes. The PS5 has to be running `ps5upload.elf` before the app can do anything beyond the
Connection tab. The app walks you through sending it on first run.

**Q: How do I send the payload?**
Open ps5upload → **Connection** tab → enter your PS5's IP → click
**Check**, then **Send helper**. The app waits up to 20 seconds
for the payload to come up, then unlocks the rest of the tabs.

**Q: Can the app load everything by itself after the console starts?**
Yes, on the desktop. **Quick bring-up** on the Connection screen runs your bring-up playlist,
sends the helper and runs your auto-loader playlist in one click, once your playlists are set.
Its **Bring up automatically** switch (also in **Settings → Connecting**) does that with no
clicks: when a saved PS5's loader (elfldr, port 9021) is up and the helper isn't running, the
app waits about 90 seconds so the console's own autoloader can finish, then brings it up. Once
each time the console starts; after rest mode only the helper is sent again. Off by default.

**Q: How do I send a different payload (kstuff, kernel patches,
plugin scripts, etc)?**
Open **Payloads → Send payload** (the Connection screen has an **Open Send
payload** button that goes there; Connection itself only sends the PS5Upload helper), click
**Choose**, pick any `.elf`,
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
Yes. Open **Connection** while it is up and the app skips the send step and connects. If the
helper is older than this app, a banner says so: its **Send helper** sends this app's helper
right there and waits until the new version answers.

**Q: Can multiple computers connect to the same PS5 at the same time?**
Yes: computers, phones and Docker, up to 16 at once, with no pairing code (8.0.0). **Devices**
lists them and what each is doing; installs take turns, and an upload into a folder another
device is writing into says so. See "Several devices and Mesh Drive".

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
| 9180 TCP | PS5 | PS5Upload Minion, the page the helper serves to the PS5's browser (and to your phone or PC once you allow other devices) |
| 19113 TCP | this computer | The engine: the PS5 fetches packages from it during Stream & install |
| 9295–9304 TCP/UDP | PS5 | Remote Play: pairing, wake and streaming |

To see every port in use on your PS5, and which payload usually sits on it, open **Payloads →
Running**.

Every connection is encrypted, and the console remembers each device the first time it connects.
A 6-digit code only appears when **Ask before a new device can connect** is on in **Connection →
Devices**. If a different PS5 answers at an address you reuse, forget the old one and connect
again.

**Q: The web UI says it isn't paired, and no code shows on the console.**
With an 8.0.0 helper this only happens when **Ask before a new device can connect** is on, or the
device is blocked (Devices). Let it in from a connected device (**Connection → Let another device
pair**), or press **Send helper** in the web UI: the helper it sends lets it in. A helper older
than 8.0.0 still asks for pairing; sending the 8.0.0 helper ends that.

**Q: I updated to 6.0 and the app says the helper is old.**
6.0 and v5.x cannot talk to each other. Press **Send helper** in the banner that says the helper
is older (or on the Connection screen) and the app replaces it, waiting until the new version
answers. If the old helper will not exit, restart the
console and try again.

**Q: How do I cap the upload speed?**
Set `PS5UPLOAD_BANDWIDTH_MBPS` (megabytes per second) before starting the app or engine. Unset or
zero means no cap. The pre-6.0 name of this variable still works as an alias.

**Q: Can I run the engine on a different machine (self-hosted)?**
Yes. Run it on a server or NAS and point the app at it in **Settings → Engine URL**. For a
step-by-step guide that ends with the app open on a phone, see "How do I use PS5Upload from my phone
or another computer?" below. Images:
`ghcr.io/phantomptr/ps5upload-engine` (engine only) and `ghcr.io/phantomptr/ps5upload-engine-webui`
(full web UI). Every release tag pushes `:<version>`; `:latest` and `:<major.minor>` (for example
`:7.1`) move only when a pre-release is promoted to a release, so `:latest` never points at a
pre-release.

- The API has **no password**. Set `PS5UPLOAD_ALLOW_IP` to the IPs or ranges allowed to use it
  (`192.168.1.0/24`); anything else gets a `403` naming the refused address. Trusted LAN only,
  never the internet.
- Start from [`engine/compose.yaml`](engine/compose.yaml) (set `PS5_ADDR`, `PS5UPLOAD_ALLOW_IP`
  and your package folder). It uses **host networking**: a stream install makes the PS5 download
  the package *from the engine*, and on Docker's bridge network the engine offers an address the
  PS5 cannot reach. Docker Desktop (macOS/Windows) has no host networking: publish port 19113 and
  set `PS5UPLOAD_PKG_HOST_IP` to the computer's LAN IP.
- **Map `/data` to a folder on the host** (on Unraid: `/mnt/user/appdata/ps5upload`). It holds
  the engine's pairing key as well as saved servers, install history and the artwork cache.
  Without it, recreating the container (an Unraid update or edit does) makes a new key the PS5
  has never seen, and it stops accepting the web UI.
- With a remote engine the app's file pickers browse the **engine's** disk.

**Q: After restarting or updating the Docker container, the web UI says "This PS5 is not accepting new pairings".**
The container lost its key: `/data` was not mapped to a host folder (see above), so it came back
as a new device. With an 8.0.0 helper a new device simply connects, unless approval is turned on
in Devices. Map `/data` to a host folder first so it stops happening, then press **Send helper**
in the web UI (the helper it sends lets it in), or let it in from the desktop app (**Connection →
Let another device pair**). If Send helper says the loader (port 9021) isn't running, load elfldr
on the PS5 again first.

**Q: Docker or a NAS says "Permission denied" when I save or upload.**
The image runs as UID:GID 65532 and keeps its state in `/data`. If you bind-mount a folder someone
else owns, the engine names the folder, its own UID:GID and the folder's owner. Either set `user:`
in the compose file to the folder's owner (`ls -ln` shows the numbers), or run
`chown -R 65532:65532 <host folder>`. `PS5UPLOAD_DATA_DIR` does not fix this.

**Q: I added consoles in the web UI on one computer. Why doesn't another computer see them?**
Since 7.4.2 it does: the web UI keeps your consoles (names, addresses, order, notes and Wake
keys) in the container's `/data`, so every browser that opens it sees the same list, and a change
made in one shows up in the others within about 30 seconds (at once when you switch back to that
tab). The first browser to open 7.4.2 hands over the consoles it had saved. What stays in each
browser: which console is selected, theme, language, text size and the sidebar. Before 7.4.2 the
list lived in each browser only. Anyone who can open the web UI can see the Wake keys, so keep
`PS5UPLOAD_ALLOW_IP` to your own devices.

**Q: How do automatic save backups work, and how do I restore one?**
Saves → Automatic backups: choose a folder and how many versions to keep, then turn it on. Every
ten minutes the engine copies each console's saves that changed since their last backup (a save
written in the last few minutes waits for the next round). In the desktop app this runs while the
app runs; in Docker it runs all the time, with no browser open. Use a folder the container keeps,
such as `/data/save-backups`, or a NAS share you mount into the container. To restore, close the
game, open its page in PS5Upload, go to Saves and press Restore on the version you want. The save
on the PS5 is backed up first, so you can go back.

**Q: How do I make room for a big game?**
Open Clean up and press Make room, or follow "Plan space" when an install says there isn't enough
room. Enter the size and pick the drive. The plan only suggests safe steps: removing leftovers,
moving games to another PS5 drive, and uninstalling games your Collection on this computer has a
copy of. It never deletes a game you have no copy of and never moves games to the computer.
"Last played" is when the app saw the game running, so a game played while the app was closed
shows unknown.

**Q: My uploads to the PS5 are slow. How do I find out why?**
Open Health and press Diagnose under "Diagnose slow transfers". It checks how the PS5 and this
computer are connected (cable, Wi-Fi or VPN), the cable speed, whether replies slow down while
sending and the packet size, and runs a speed test unless one ran in the last 10 minutes. The most
common causes are the PS5 on Wi-Fi (about 30 MiB/s; a cable gets about 105) and a cable or switch
port that connected at 100 Mb/s (about 11 MiB/s). In Docker, the checks of this computer's
connection only see the container, and the result says so.

**Q: Can I send a package or link to my PS5 from another app on my phone?**
Yes, on Android. Use Share in your browser, Files app or download manager and pick PS5Upload. A
link opens in Install from a link, where you choose how it is fetched and start it. A .pkg in your
phone's storage installs from where it is, with no copy. A file only the sharing app can read (for
example from Google Drive or Telegram) is copied into PS5Upload first, up to 2 GB; for anything
bigger, download it to the phone first and share it from the Files app. The iPhone app can't offer
this.

**Q: Can I double-click a .pkg to install it with PS5Upload?**
On Windows and Linux, yes, once the app is installed. On macOS, .pkg also means a Mac installer, so
PS5Upload doesn't take it over: right-click the file and choose Open With → PS5Upload. Either way
the app shows the file and asks "Send to <console>?" before it installs.

**Q: Can I use ps5upload from a web browser (no desktop app)?**
Yes. The `webui` image (above) serves the full app over HTTP; open `http://<host>:19113` from an
allowed IP. Same security rules: no password, trusted LAN only. "How do I use PS5Upload from my
phone or another computer?" below walks through it from start to finish.

- **Upload works**, but the file picker browses the **engine's** disk, not your browser machine's.
  Mount your games into the container (`-v /host/games:/pkgs:ro`) and set
  `PS5UPLOAD_BROWSE_ROOTS=/pkgs` (comma-separated for several) so the picker opens there.
  In **Upload & Install**, **From this device…** stages files from your browser machine on the
  engine, then opens the normal review before sending to the PS5. Keep the tab open until
  staging finishes. This needs free space on the engine; temporary uploads expire after seven
  days. A package staged this way offers **Stream & install** on its card like any other.
- An upload keeps running when you close the tab; reopening shows it. Only one tab runs the queue.
- **Payloads → Send payload** accepts an ELF from your browser machine and forwards it through
  the engine to the selected console's loader (9021 by default). Sending bytes does not confirm
  execution. Catalog downloads and the other payload formats still use the native app.
- Desktop-only and hidden in the browser: archive uploads, saving a save backup or downloading files to your
  computer, and attaching files to a bug
  report. Everything that works on the PS5 itself is the same as on desktop.
- The docker image can be built using `PS5UPLOAD_BASE_URL` to change the root path of the app.

**Q: How do I use PS5Upload from my phone or another computer?**
Run PS5Upload on one machine, for example a Mac tucked away next to the router, and use the whole
app from a browser anywhere on your home network, including Safari on an iPhone or iPad. This
uses the web UI Docker image, not the desktop app.

**Why not the desktop app?** Its engine only takes orders from its own window: a request from any
other device is refused, and it has no web version of the app to give a browser. The Docker image
`ghcr.io/phantomptr/ps5upload-engine-webui` is the same engine with the full app built in.

**1. Install Docker.** On a Mac, install [Docker Desktop](https://www.docker.com/products/docker-desktop/)
and start it. On Linux, a NAS or a mini PC, any recent Docker with Compose works.

**2. Write the compose file.** Make a folder (for example `~/ps5upload`), and in it a file
named `compose.yaml`. On **Linux**, copy [`engine/compose.yaml`](engine/compose.yaml) as it is and
edit the three values marked `EDIT`. On a **Mac or Windows PC**, Docker Desktop has no host
networking, so publish the engine's port instead and tell the engine the computer's own address:

```yaml
services:
  ps5upload:
    image: ghcr.io/phantomptr/ps5upload-engine-webui:latest
    container_name: ps5upload
    restart: unless-stopped
    ports:
      - "19113:19113"
    environment:
      PS5_ADDR: "192.168.1.50"            # your PS5's IP
      PS5UPLOAD_PKG_HOST_IP: "192.168.1.10" # this computer's IP (step 3)
      PS5UPLOAD_ALLOW_IP: "192.168.1.0/24"  # who may use it (step 5)
      PS5UPLOAD_BROWSE_ROOTS: "/pkgs"
    volumes:
      - ps5upload-data:/data
      - /Users/you/PS5:/pkgs:ro             # your packages and game folders

volumes:
  ps5upload-data:
```

- Keep port **19113** the same on both sides (`"19113:19113"`). The PS5 downloads packages from
  that port during Stream & install, and the engine tells it to use 19113.
- `PS5UPLOAD_PKG_HOST_IP` is the address the PS5 is told to download from. Without it, a stream
  install hands the PS5 an address inside Docker that it cannot reach.
- `/pkgs` is what the web UI's file picker shows: change `/Users/you/PS5` to the folder that holds
  your `.pkg` files and games. The picker browses this computer, not your phone. To send a package
  that is on the phone itself, use **Upload & Install → From this device…**, then **Stream &
  install** on its card.
- `ps5upload-data` holds the engine's pairing key and saved settings. Don't delete it, or the PS5
  sees a new device and you pair again.
- Quit the PS5Upload desktop app on this computer first: it also uses port 19113.

**3. Find the computer's IP address.** On a Mac: **System Settings → Wi-Fi** (or **Network →
Ethernet**) → **Details** → **TCP/IP**, or run `ipconfig getifaddr en0` in Terminal. Put it in
`PS5UPLOAD_PKG_HOST_IP`. Give the computer and the PS5 fixed addresses in your router if you can,
so they don't change.

**4. Start it.** In that folder run `docker compose up -d`. `docker compose pull` followed by
`docker compose up -d` updates it later.

**5. Open it on your phone.** On the same Wi-Fi, open `http://<computer's IP>:19113/` in Safari
(or any browser), for example `http://192.168.1.10:19113/`. On an iPhone, tap **Share → Add to
Home Screen** to get an app icon. If the page says **"loopback only: this engine refused"**
followed by an address, the engine saw your phone as that address. Add it to `PS5UPLOAD_ALLOW_IP`
(comma-separated, for example `"192.168.1.0/24,192.168.65.1"`) and run `docker compose up -d`
again. Docker Desktop often shows every device as one internal address like that one, which is
also the address the PS5's downloads come from, so stream installs need it allowed too.

**6. Connect to the PS5.** On the **Connection** screen press **Check**; if the helper isn't
running yet, press **Send helper**. The web UI connects by itself.

**7. Keep the computer awake.** A sleeping computer takes the engine with it, and the page stops
answering. On a Mac, turn on **System Settings → Battery → Options → Prevent automatic sleeping on
power adapter when the display is off** (desktop Macs: **Energy**), or run `caffeinate -s` in a
Terminal window and leave it open. A MacBook still sleeps when its lid is closed unless an
external display is attached, so leave the lid open. In Docker Desktop's settings, turn on
**Start Docker Desktop when you sign in to your computer** so it comes back after a restart;
`restart: unless-stopped` brings the engine back with it.

**Keep it on your home network.** The engine has **no password**: anyone who can open the page can
install, delete and read files on your PS5. `PS5UPLOAD_ALLOW_IP` limits who that is; keep it to
your own network, and never forward port 19113 on your router or open it to the internet.

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

**Q: Can I pin the screens I use most?**
Yes. Hover a screen in the sidebar and press the pin, use the pin on its row in **More**, or
"Pin …" in the command palette (Cmd/Ctrl + K). Pinned screens appear in a **Pinned** group near
the top, in the order you pinned them; drag them in the sidebar, or use **Edit** in More, to
reorder. Up to 12. Nothing changes until you pin something.

---

# Setup

## Supported platforms

**Q: Which systems run ps5upload?**
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
- **iPhone** — `-ios-unsigned.ipa` (sideload with AltStore or
  Sideloadly, iOS 15 or later, arm64). Same interface as Android. See
  the **iPhone** section below for how to install it.

There is no 32-bit x86 (i386 / i686) build for any desktop OS.

**Q: Which PS5 firmware works?**
The helper is built against PS5 Payload SDK v0.45 and the same binary runs on **1.00 to 14.10**.
Tested on 5.10, 9.60 and 13.60 (and confirmed by users on 12.20); 14.00 and 14.10 are new in
SDK v0.45 and not tested yet. In practice the limit is the
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

1. **Update to the latest version — the fix is built in, and adapts to
   your graphics stack.** The app detects what it is running on at
   startup and applies only the workarounds that stack needs:

   | Your setup | What the app does automatically |
   | --- | --- |
   | **NVIDIA driver 555 or later on Wayland, WebKitGTK 2.46 or later** (.deb / .rpm) | Nothing: full GPU rendering |
   | Any other Linux (AMD, Intel, X11, older driver or WebKitGTK) | Disables the DMABUF renderer |
   | **Other NVIDIA on Wayland** | Also disables accelerated compositing |
   | **NVIDIA on Wayland, AppImage** | Also preloads your system's `libwayland-client` |

   The AppImage always gets the workarounds: it brings its own WebKitGTK
   and Wayland libraries, and a current NVIDIA driver still showed a white
   window there (issue #285). The first row is the stack where both
   switches were measured to be unnecessary — and costly: without them
   lists scroll smoothly and Remote Play's picture has less delay and
   keeps Sharpen and upscaling. What was read and decided is logged at
   startup (`[linux] graphics: …` in the terminal output).

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
   games, a file browser of thousands of entries). It also makes Remote
   Play draw its picture without the GPU, so Sharpen and upscaling are
   off there. An earlier version set it for everyone, which is why
   scrolling felt heavier on Linux than on Android or Windows. It is now
   scoped to the stack that needs it.

2. **White, blank or flickering on a stack the app thinks is fine? Turn
   on Safe graphics mode.** It turns both switches on, whatever was
   detected. If you can see the window: **Settings › Window › Graphics ›
   Safe graphics mode**, then restart the app. If you can't, start it
   once with the flag or the variable (the flag turns it on for that
   launch; switch it on in Settings to keep it):

   ```sh
   ps5upload-desktop --safe-graphics          # .deb / .rpm
   PS5UPLOAD_SAFE_GRAPHICS=1 ./PS5Upload.sh   # .zip / AppImage
   ```

   `PS5UPLOAD_SAFE_GRAPHICS=0` turns a saved Safe graphics mode off for one
   launch. The setting is the file
   `~/.config/com.phantomptr.ps5upload/safe-graphics`; deleting it turns
   it off too.

3. **Still white?** Apply the compositing switch by hand — your stack may
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

4. **Still white?** Force X11 instead of Wayland:

   ```sh
   GDK_BACKEND=x11 ./PS5Upload.sh
   ```

5. **Still white?** Fall back to software rendering (slower UI, but
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

**Q: The Android app says "Port 9021 is not open" (or can't connect at all) on 6.x, but 5.41 worked.**
On Android 17, apps can't reach devices on your network until you allow it, and 6.x builds before
6.5.0 never asked. Update to 6.5.0 or later: it asks at launch (Android calls it **nearby
devices**), and a red **Allow PS5Upload on your local network** bar stays at the top until you
allow it. If you tapped **Don't allow**, tap **Allow** on that bar, or turn it on in **Android
Settings → Apps → PS5Upload → Permissions → Nearby devices**.

**Q: After updating the app, it says "This PS5 has not accepted this app yet" or "The PS5 is not
accepting new pairings".**
To the PS5, an app updated from 5.x is a new device. With an 8.0.0 helper it connects by itself
(unless approval is on in Devices); an older helper only takes new devices for a short while
after it starts. Tap **Pair…**, then **Resend helper**: the app sends its helper, the PS5 accepts
the app that sent it, and the dialog closes by itself a few seconds later. This works the same in
the desktop app.

**Q: How do I install the Android app?**
Download `PS5Upload-<ver>-android.apk` from the Releases page and open it. Android asks you to
allow installs from your browser or file manager the first time. Updates install in place.
Older 32-bit phones take `PS5Upload-<ver>-android-armv7.apk` instead; if the normal APK says
"App not installed" on an old phone, that is the one you need.

**Q: The app's text or buttons are huge (or tiny) on my phone.**
Your phone's Display size or Font size setting scales the whole app. Open **Settings → Text
size** in ps5upload and pick a smaller percentage.

**Q: I picked a folder, `.zip` or `.pkg` on Android but it fails ("could not read", "No such file").**
Android needs **All files access** before the app can read your files. Tap **Choose folder**,
**Choose file** or **Add .pkg**, tap **Open settings**, turn on **Allow access to manage all
files** for PS5Upload, come back and tap **Retry**. You can also grant it in **Android Settings
→ Apps → PS5Upload → Permissions → All files access**.

**Q: How do I upload a big game from my phone?**
**Upload & Install → Choose folder** (or **Choose file** for a `.zip`), pick it in the in-app browser,
choose the destination and **Start**. The file is read in place, nothing is copied first.

**Q: Every time the PS5 restarts, the app says the helper is older than the app again.**
Your PS5 starts a stored copy of the helper at boot (Payload Manager's autoload, the WebKit
Autoloader or an autoload folder), and that copy is old. Since 9.2.1, **Send helper** also replaces
those stored copies once the new helper answers: Payload Manager's storage, `ps5_autoloader`
folders on the PS5 and its USB drives, and etaHEN's payloads folder, keeping each file's name so
your boot order stays the same. The app lists what it updated; anything it couldn't update is named
with how to do it by hand (upload the new `ps5upload.elf` again in Payload Manager under the same
name, or copy it over the file in your autoload folder).

**Q: The `helper` dot at the bottom of the app is red.**
The dot is the PS5-side helper, not the app. Red means it is not running (or is an older version
that needs **Send helper**). Run **Set up your PS5**, or press **Send helper** on the Connection
screen or in the banner. The
`engine` dot is the app's own service and is green whenever the app is open.

---

## iPhone

**Q: Is there an iPhone app?**
Yes. Each release has `PS5Upload-<version>-ios-unsigned.ipa`: the same app as on Android, with the
engine built in, so the phone talks to the PS5 directly. It is not on the App Store, so you
install ("sideload") it yourself; no jailbreak is needed. You need a computer (Mac or Windows) for
the install, an Apple ID (a free one works), and iOS 15 or later. The iPhone app is new and has had
little testing on real phones, so please report what you find.

A spare Apple ID made just for sideloading is a good idea: the tool needs your Apple ID password to
sign the app, and a spare one keeps your main account (iCloud, purchases, payment) out of it. Your
iPhone stays signed in to your main account; the spare one is only typed into the tool.

**Q: How do I install it with AltStore? (recommended: it refreshes the app for you)**
1. On your computer, install **AltServer** from altstore.io and open it (on a Mac it is an icon in
   the menu bar, not a window).
2. Connect the iPhone with a cable, unlock it and tap **Trust**.
3. AltServer → **Install AltStore** → your iPhone → enter your Apple ID. If it asks for a code,
   it is the one Apple shows on your devices or texts to you.
4. On the iPhone (iOS 16 and later), turn on **Settings → Privacy & Security → Developer Mode**,
   restart, and confirm. Then **Settings → General → VPN & Device Management →** your Apple ID
   **→ Trust**.
5. Download the `.ipa` from the release page on the iPhone (or AirDrop it from your computer) so
   it is in the Files app. In AltStore: **My Apps → +** → pick the `.ipa`.
6. Open PS5Upload and allow **Local Network** when asked.

With a free Apple ID an app runs for 7 days at a time. AltStore re-signs it automatically while
your computer runs AltServer on the same Wi-Fi (or tap **Refresh All** in AltStore). If you let it
lapse, the app will not open until you refresh it; your settings stay.

**Q: Can AltStore, SideStore or Feather update the app for me?**
Yes. Add this source once, and the app shows up there with its updates:

`https://github.com/phantomptr/ps5upload/releases/latest/download/altstore.json`

In AltStore or SideStore: **Browse → Sources → +** and paste it. In Feather: **Sources → +**. The
source always lists the newest full release (not pre-releases); installing from it is the same as
installing the `.ipa` by hand. Sideloadly has no sources: download the new `.ipa` and drag it in
again.

**Q: How do I install it with Sideloadly? (quickest one-off install)**
1. Install **Sideloadly** from sideloadly.io on your computer and open it.
2. Connect the iPhone with a cable, unlock it and tap **Trust**.
3. Drag the `.ipa` onto Sideloadly, enter your Apple ID, and click **Start**.
4. On the iPhone, turn on Developer Mode and trust your Apple ID as in step 4 of AltStore above.
5. Open PS5Upload and allow **Local Network** when asked.

With a free Apple ID, run Sideloadly again with the same `.ipa` within 7 days (or turn on its
automatic refresh, which re-signs over Wi-Fi while your computer is on).

**Q: The install fails asking for a verification code, or no code arrives.**
The code comes from Apple, not from the tool. If your Apple ID is signed in on your iPhone or Mac,
the code pops up there (or use **Settings → your name → Sign-In & Security → Get Verification
Code**). A spare Apple ID signed in nowhere gets it by text message to its phone number; if none
arrives after a few failed tries, wait an hour (Apple limits texts) and try once more.

**Q: How do I get games onto the iPhone to send them?**
Copy them with the **Files** app into **On My iPhone → PS5Upload** (from iCloud Drive, a USB drive
or AirDrop). The app's file browser starts there, and exports such as logs and save backups land
there too. Keep the app open and the screen on while a transfer or install runs: iOS pauses apps
in the background.

**Q: Does it work on an iPad or an Android tablet?**
Yes: the iPhone `.ipa` installs on an iPad, and the Android `.apk` on an Android tablet. On a
wide screen the app shows the sidebar like the desktop app; held upright, a tablet starts with the
narrow icon rail so screens keep their width (the arrow at the bottom of the rail opens the full
sidebar, and the app remembers your choice). Buttons that appear on mouse hover on a computer are
shown on a touch screen.

**Q: Updating the iPhone app.**
When a new version is out, the app's update check offers the new `.ipa`. Install it the same way
over the old one (AltStore: My Apps → +; Sideloadly: Start again); your settings and pairings
stay.

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
On Ubuntu with `ufw` the default is allow-outbound / deny-inbound.
Browsing and uploads need nothing extra (outbound only), but **Stream
& install** has the PS5 connect back to the engine on 19113, so allow
that from the console:
```bash
sudo ufw allow from 192.168.88.2 to any port 19113 proto tcp
```

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
as the PS5 address. The helper send (Connection → Send helper)
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

**Q: How do I move a game to another drive on the PS5?**
In **Files**, select it and choose **Move to…**, then pick the drive's homebrew folder (or any
folder). It says first whether the move is instant (same drive) or a copy to the other drive,
and whether it fits; a copy removes the original only after checking it. **Set permissions
(777)** in the same menu makes files and folders readable and runnable.

**Q: Can I drag files between Files and my computer?**
Yes, in the desktop app. Drag files or folders (or everything you selected) out of **Files** onto
the desktop, Finder, Explorer or your Linux file manager to copy them to this computer. The first
drag fetches them; keep holding the mouse button and the drop happens when they are ready, or let
go and drag again once the app says they are ready. One drag fetches up to 4 GB; for more, right-click
→ **Download**. Drag files or folders from your computer into Files to copy them into the open
folder; the copy starts as soon as you drop (9.1.0 fixed the first drop of a session waiting for
**Resume**). To move items into another folder on the PS5 instead, hold **Alt** (**⌥ Option** on a
Mac) while you drag them onto that folder or a part of the path at the top. In the browser web UI
you can drop files (not folders) onto Files; they go to the server first, then to the PS5. The
Android and iPhone apps have no drag and drop.

**Q: Can I copy files off a disc in the PS5's drive?**
Yes. A data DVD or Blu-ray in the drive shows up in **File System** at `/mnt/disc`; browse it
and download or copy files from it like any other drive. The disc stays read-only.

**Q: I cancelled a download on the PS5, but the app still shows the install running.**
The app can't see the PS5's download list; it only notices the console stopped asking for data.
Press **Stop** (⊘) on the install in the queue: ps5upload stops sending the package and the
row goes within a few seconds. After a minute with no data asked for, the row says so.

**Q: How do I send a game from the Collection?**
Open the game and press **Send to PS5** on its page. Pick the copy, how to send a game folder
(as it is, or as one compressed image that is faster to send and smaller on the PS5), the drive
and folder, then **Send**. It starts right away; other uploads waiting in the queue keep waiting.

**Q: How do I stop my PS5 from going into rest mode?**
**Settings → Upload → Keep the PS5 awake** has three modes: **Off**; **During uploads and
installs** (default), which keeps resetting the console's auto-standby timer while any upload or
install runs, a Stream install included; and
**Always while connected**, which keeps every console with a running helper out of auto-rest while
the app is open (a ⚡ indicator shows). Resting the console by hand always works, and closing the
app returns the console to its normal schedule.

**Q: I uploaded an 81 GB file and the PS5 lost about 100 GB of free space. Does ps5upload
keep hidden copies?**
No. The extra is held by the PS5's own file system, not by anything ps5upload writes. On the
console's **internal** storage, Sony's file system sets aside a reserve that grows with what you
write: we measured about 16–20% of the bytes written on both of our consoles (a 10.4 GB file took
12.1–12.5 GB of free space), and reports put a large single image nearer 25%. The file itself
is exactly its own size. Deleting it gives back the file and the reserve within seconds. An
**extended storage** drive (M.2 or USB formatted by the PS5) has no such reserve: a file there
costs exactly its size. ps5upload's own extras are small: a game image carries 64–512 MiB of
spare space, and a transfer that failed can leave a partial file, which **Connection →
Health Check** offers to delete. **Volumes** shows what is likely to fit after the reserve, and
**Kept by ps5upload** lists everything the app itself wrote.

**Q: Can I send a game folder as one compressed image instead of thousands of files?**
Yes. In **Upload & Install**, pick the game folder: under its options, **Or send it as one game image**
builds a compressed `.ffpfsc` (or a `.ffpkg` / `.exfat`) on this computer, checks it, and then
puts it in the upload queue by itself, into the destination folder you chose (`homebrew`, which
ShadowMount+ watches, by default). One file is much quicker to send than many small ones, and
compressed it takes less room on the PS5. It needs free space on this computer for the image
while it is built; **Delete the image from this computer once it is on the PS5** cleans that up.
In **Workshop**, a finished image also has **Upload to PS5**.

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

Wake needs the app paired with the console once: **Set up wake & sign-in** on the Connection
screen turns on Remote Play, reads your account from the console and registers, with the console
awake. After that the button is **Wake & sign in** and brings the console up straight on your
user's home screen — the same thing the official Remote Play app does. Only if a console refuses
to pair, **Enter keys manually** takes a **wake code** (wakes to the user-select screen) and the
two Remote Play **session keys** (also signs your user in). (If your user has a login passcode,
the console will stop to ask for it, so sign-in needs a passcode-free user.)

**Q: Do I still have to "Register" a game after uploading it?**
Not anymore. When you upload a game folder, **"Add to PS5 home screen
when done"** is on by default — the game is registered automatically the
moment the transfer finishes (also from the upload queue), ready to
launch. If that step ever fails the upload itself is unaffected; open
the game in **Games** and, under **On the PS5's drives**, choose **Add to home screen** in the
row's **⋯** menu (the action formerly called "Register"). A folder no game links to yet is in
the list under the Games grid, with the same menu.

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
upload is over that but its bytes would still fit, **Upload & Install** says it **may not fit** and
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
Yes. Drop any `.exfat` or `.ffpkg` image. After upload, open the game in **Games** and press
**Mount** on the image under **On the PS5's drives** (an image no game links to yet is in the
list under the Games grid). The payload attaches the image via `/dev/lvd*` and mounts it at
`/mnt/ps5upload/<name>/`. The **Volumes** screen shows the result with a progress bar and
Unmount button.

**Q: Can I upload a compressed `.zip` of a game?**
Yes, and the files arrive **already extracted**. Drop the `.zip` on **Upload & Install**; ps5upload
decompresses it on your computer while sending, with no manual unzip. The screen previews
`zipped → extracted`, the file count and any embedded game. The destination folder is named after
the archive. Resume, excludes and the bandwidth cap work as for a folder. Only standard "Deflate"
zips are supported (Windows "Send to → Compressed folder" is fine); Deflate64, LZMA, BZip2, Zstd
and encrypted zips are rejected with a clear message.

**Q: What about `.rar` and `.7z`?**
Both work and also resume after a drop. A resumed archive is read again from the start to reach
the part not sent yet; meanwhile the progress reads "Skipping data the console already has".

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

**Q: Why does a game's page list it more than once under On the PS5's drives?**
The Games grid shows each game once, by title ID. Its page lists every folder and image on the
PS5's drives it is in, one row each: a game inside a mounted image shows both the image and the
game folder inside it. A folder of a game the PS5 also has installed from a package is marked
**Also installed**: it is a second copy you can delete (or copy to your Collection first).

**Q: Can I queue several uploads to run back-to-back?**
Yes. **Upload & Install** has one queue for the console, below the drop zone: uploads and
installs, including what Workshop and a game's page hand to it. Each row shows live progress,
current speed and time left while running, and the average speed after it completes. The queue
runs one item at a time and persists across app restarts, so an item interrupted by a crash
picks up cleanly when you press Start again. Tick **Continue on failure** to keep going when one
item fails instead of stopping the whole batch.

**Start the queue when you add something** (on the queue panel and in Settings › Uploads) decides
what Install, Upload now and Send do: on (the default), they start the console's whole queue
from the top, in order, not only what you just added; off, they only add to the queue and nothing
runs until you press **Start queue**. Something added while the queue runs waits its turn at the
end. **Add to queue** never starts anything.

**Q: How do I jump between volumes in the File System screen?**
The **Volume** dropdown above the breadcrumb lists every writable volume with its free space.
Pick one to jump to its root.

**Q: Does the File System screen remember where I was last?**
Yes, per console. The PS5 IP is remembered too.

**Q: Where can I see what is running across screens?**
In the progress bar at the bottom of the window (on phones, a pill above the tab bar). It names
the running job with its percentage, speed and time left, and how many more are running or
waiting: uploads, installs, downloads, copies, game actions and dumps, but also helper sends,
payload playlists, cheat downloads and the app's own update. Click it for a panel with
**Running**, **Waiting** and **Just finished**: **Cancel** or **Retry** a job there, **Open its
screen** to go to the screen it belongs to, or **See all activity** for **Activity & Logs →
Tasks**, whose **History** and **Stats** views cover the last 100 operations. **Cancel copy**
stops a console copy and cleans up only what it created.

**Q: Where are my notifications?**
Under the bell at the top (on phones too). It previews the newest of what ps5upload has told you
(uploads and installs finishing or failing, updates); **See all** opens **Notifications**, which
has three tabs: **This app** (that list), **On the PS5** (messages ps5upload has put on the PS5's
screen, such as a Remote Play PIN or a cheat switched on, read back from the helper; not the PS5's
own notification panel) and **Settings**. Looking at a list marks it read.

**Q: My big upload stopped partway through. What now?**
A dropped connection, a helper restart or rest mode no longer ends the job: the app waits for the
console to come back and continues from what is already safely on it. If it gave up, press Start
again (or pick **Resume** at the prompt). On external USB/exFAT, a console that lost power
mid-write can leave a file inconsistent; if a finished upload looks wrong, use **Override** and
upload again.

To avoid interruptions on long transfers:
- **Your computer** is kept awake automatically while any upload or install runs, including a
  Stream install (the console fetches the package from this computer, so it must not sleep).
  **Settings → Keep Awake** also keeps it awake while the app is idle; greyed out on non-systemd
  Linux.
- **The PS5** has its own rest timer: raise it under **Settings → System → Power Saving → Set Time
  Until PS5 Turns Off**, or use **Settings → Upload → Keep the PS5 awake** in the app.
- **Flaky Wi-Fi?** A direct Ethernet cable is the most stable and fastest path (see the direct
  Ethernet section above).

---

## Mount + unmount

**Q: I uploaded a game *folder*, but ShadowMount+ won't mount it, yet `.exfat` images mount fine.**
ShadowMount+ mounts game **image files** (`.ffpkg` / `.exfat` / `.ffpfs`), not loose folders. Two
ways forward:

- **Add the folder to the home screen in ps5upload.** Open the game in **Games** (a folder with
  `eboot.bin` and `sce_sys/param.json` at its root); under **On the PS5's drives**, choose **Add
  to home screen** in the folder's **⋯** menu, then **Play**. If a PSN- or disc-extracted dump
  fails with a DRM error, use **Add to home screen (patch DRM)**. If the home-screen tile is
  blank, use **Heal home-screen metadata** in the same menu (desktop app), or enable the
  ShadowMount+ metadata healer.
- **Give ShadowMount+ an image.** Send the game as an `.exfat` or `.ffpkg` image.

**Payloads → On your PS5 → ShadowMount+ → Manage** shows its `debug.log`, which says why a given
item did not mount.

**Q: How do I find one game in a long library?**
Use the search box above the **Games** grid: part of the name, a title ID (`PPSA…`, `CUSA…`), a
content ID or part of a copy's path. The list of game files no game links to has its own search
box.

**Q: Can I pick where a `.exfat` / `.ffpkg` mounts?**
Yes. The **Mount** button on an image (a game's page, under **On the PS5's drives**) opens a
dialog with a volume, a subpath and a name (taken from the image filename). Your last choice is
remembered per console. Some PS5 game scanners only look in `/mnt/ps5upload/`, so the dialog
warns when you mount elsewhere.

**Q: A mount from a previous session is still showing after I re-sent the helper.**
Expected: mounts are held by the PS5 kernel and survive helper restarts. Only a PS5 reboot clears
them. On startup the helper unmounts any mount whose backing device is gone.

**Q: An image on a game's page has a `mounted` badge. What does that mean?**
The file is currently attached at `/mnt/ps5upload/<name>/`, and the
Mount button has flipped to Unmount. The **Volumes** screen shows the
mapping explicitly, with the source image path under each mount.

**Q: Does Unmount leave ghost tiles in the PS5 dashboard?**
No. Unmount unregisters every title inside the image first, then unmounts.

**Q: Can I edit what's inside a mounted image — replace files, add DLC,
or apply a backport patch?**
Yes. Mount the image **read-write**, then edit it in the **File System**
tab like any other folder on the console.

- Open the game in **Games**, press **Mount** on the image under **On the PS5's drives**, and
  leave **"Mount read-only"** unchecked. (In the upload flow the same choice is the *"Mount
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

**Q: Which image formats can I edit?**

| Format | Play on the console | Edit |
| --- | --- | --- |
| `.exfat` | ShadowMount+ or compatible integrated mount service | Game page → On the PS5's drives → Edit files; or mount locally on a supported OS |
| `.ffpkg`, `.ffpfs` | ShadowMount+ or compatible integrated mount service | Game page → On the PS5's drives → Edit files, when the console permits a writable mount |
| `.ffpfsc` | ShadowMount+ or compatible integrated mount service | Open its folder in Files → Decompress to .exfat; edit the extracted image, then recompress on the computer if desired |
| `.pkg` | Upload & Install | Workshop supports unpacking packages made with the debug key; arbitrary encrypted retail packages are not editable sources |

Compressed images are listed under **On the PS5's drives** alongside uncompressed images.
Decompression keeps the original file and requires space for the full extracted image. The
direct Edit files action is not offered for compressed containers. A successful read-only mount
does not establish that a firmware permits writable mounting.

**Q: Can I look inside a game image without mounting it on the PS5?**
Yes: **Workshop → Look Inside** opens any image ps5upload reads (`.exfat`, `.ffpkg`,
`.ffpfs`, `.ffpfsc` and debug `.pkg`) that is on the machine the engine runs on, without a
mount, wherever the engine runs. Browse its folders with their sizes and save any file or
folder to the computer (checked against the image). For images (not packages) you can also add,
replace or delete files and write an edited copy next to the original; the copy is read back
and checked, and the image you opened is never changed.

**Q: Can I queue more files or convert a folder of packages?**

In Files, Add files and Add folder remain available while a copy runs. Further picks keep
their destination and conflict choices in the queue; a failure or cancellation pauses it.
Pending picks can be removed. After an app restart, review the saved queue and press Resume.

Workshop → ① Game → **Add games from a folder…** accepts `PS5 Games/<game>/game.pkg`, alongside
actual game folders and supported images/archives. Generic `game.pkg` outputs use the parent
folder's name. Review the queue before starting; unsupported/encrypted inputs fail individually,
and existing output files are not overwritten. **Upload & Install** accepts multiple packages
(from this computer, a saved server, a phone, or **From this device…** in the browser), reads
their headers first, and queues bases before updates and DLC. Each queued item shows the game's
icon and name from the moment it is added, and the queue header says how much is waiting and
whether it fits on the drive it installs to (an estimate; each install still checks).

**Q: A transfer or conversion gets slow near the end. What should I report?**

Include app/helper versions, source and destination drive types, file count and size range,
and the stage shown when it slows (transfer, compression or verification). Export a bug report
while it is happening. Small-file work and verification can behave very differently from large
sequential copies; a percentage alone cannot identify the bottleneck. Conversion logs now
record planning, writing and verification times, sizes, and compression settings. Comparisons
need the same source, settings and storage. The current small-file and compression reports
remain investigations; no throughput improvement is claimed for this release.

**Q: ShadowMount+ mounts my game read-only — how do I edit it?**
Open the game in **Games** and press **Edit files** on an uncompressed image under **On the
PS5's drives**. That is a *checkout*: the
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

**Q: What does ps5upload use from ShadowMount+ 1.7beta8 and later?**
ShadowMount+ 1.7beta8 added its own control interface (on the console's port 10101). When it
answers, ps5upload uses it:

- **Backporting writes into an image through it.** The app asks ShadowMount+ to mount that one
  game read-write, writes the changes, and asks it to unmount again. It takes a fraction of a
  second, works whether or not the image was mounted before (with ShadowMount+'s default
  settings no image stays mounted while idle, which made the older method fail with "has not
  mounted"), and leaves `config.ini` alone. While it is held, ShadowMount+ postpones its scans
  instead of releasing it. Older ShadowMount+ versions, or one whose interface is limited to the
  console, still get the older method.
- **The ShadowMount+ panel shows its exact version** (for example 1.7beta11).

Two things worth knowing about ShadowMount+ 1.7beta8 and later:

- Its control interface is open to your local network by default, with no password. Anyone on
  the same network can use it. Set `api_bind_address=127.0.0.1` in
  `/data/shadowmount/config.ini` to keep it to the console (ps5upload then uses the older
  methods), or keep the console on a network you trust.
- Betas 2 to 9 closed running self-made (fake) packages when the console went to rest mode;
  1.7beta10 stopped doing that. If a game made with ps5upload's package builder closes on rest,
  update ShadowMount+.

**Q: Can I unmount while a game is running?**
No — the kernel refuses with `EBUSY` because a process inside the
mount has files open. The UI surfaces this as: *"the game inside
this image is currently running on the PS5. Exit it (PS Home →
close the game) and try again."* Same protection applies whether
you trigger Unmount from a game's page or the Volumes screen.

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
   **Games** says so above the grid when the helper has none; load kstuff and
   reconnect. Disc-image titles need ShadowMount+ running.
4. **Convert to exFAT + ShadowMount+ (reported working on FW 13.60).** Put the
   decrypted game folder on the console's drive (or convert it to an `.exfat`
   image with **Workshop**), let ShadowMount+ mount it, and launch from the
   console's home screen. This does not depend on the package installer.
5. **A launch the app sent is not the same as a game that started.** The
   console accepts a launch and may take a while on the first start. If the
   game never appears, close it from the PS5 and start it from there.

## Games and the Collection

**Q: What does the Games screen show?**
One grid with every game once: your copies on this computer and saved servers (the Collection),
the games the selected PS5 has installed, and the games on its drives. Chips filter it by what is
on that PS5: **Installed**, **Not installed**, **Updates & DLC to install** (installed games for
which this computer has a newer update, or DLC the PS5 doesn't have yet), **On the PS5's
drives**, **Never seen played** and **Needs newer firmware**. Hover a chip for what it counts.

A game the PS5 has installed has **Play** on its card (and in the table view), or **Close** while
it runs, so you can start it without opening its page. Play waits for ShadowMount+ when the game
runs from a disk image.

Open a game for everything you can do with it:

- **Consoles**: a row for each of your consoles, with **Play** (or **Close game** while it runs)
  and **Install** or **Send to PS5** where it is missing.
- **On <console>**: the installed game's upkeep: **Suspend** and **Resume** while it runs,
  **Uninstall**, **Hide from home screen** / **Show on home screen**, **Fix permissions**,
  **Backport**, **Copy to Collection** and **Dump this game** (copy it to a USB drive or the
  console, a plain copy or with its executables decrypted; its progress shows on the page and in
  the progress bar). It warns when the game needs ShadowMount+ running.
- **On the PS5's drives**: each folder and image the game is in, with **Play**, **Mount**,
  **Unmount**, **Edit files**, **Details**, and in **⋯**: **Add to home screen** (and **patch
  DRM**), **Remove from home screen**, **Backup to folder…**, **Permission 777**, **Convert…**,
  **Make it smaller…**, **Copy to Collection**, **Download**, **Move** and **Delete**. A
  compressed `.ffpfsc` has **Open in Files** to decompress it.
- **On this computer**: your copies, to send, check or convert.

Game files on the PS5 that no game links to yet (an image nothing was registered from, a folder
whose title ID can't be read) are counted in a line under the grid; **Show them** lists them
with the same actions. To dump an installed game to a drive, open it and use **Dump this game**
under **On <console>**.

**Q: What is the Collection?**
It is what the **Games** grid shows of this computer: every game you keep on its drives and
saved servers, grouped by game: its full copies, its updates and DLC, the duplicates and the
space they take. The same grid also has the games the PS5 has installed and the games on its
drives. Add the folder (or folders) that holds your games under **Games → Folders**; anything
inside, up to six folders deep, is found by what it is (packages, game folders, `.exfat` /
`.ffpkg` / `.ffpfs` / `.ffpfsc` images, `.zip` / `.7z` / `.rar` archives), however the folders
are named. Each item is read from the file itself, so nothing needs renaming first. A drive, an
external disk or a mounted network share all work. It is PS Game Library, built into ps5upload:
if you used it, **Import from PS Game Library** in the **⋯** menu brings its index over.

**Q: Can the Collection read my NAS without mounting it?**
Yes. Save the server under **Shared Folders** (SMB, FTP, FTPS, SFTP or WebDAV), then in **Games →
Folders** press **Add a folder on <server>** and pick the folder. It is scanned where it is:
packages, game folders and covers are read over the network, and nothing is copied to this
computer. Its games install straight from the server, and **Convert…** reads them in place.
**Organize collection** works there too, renaming on the server itself; Move to Trash, junk
clean-up and Copy install link work on this computer's folders only. A mounted share also still
works as an ordinary folder.

**Q: Does it keep the computer awake or rescan all the time?**
No. **Look for changes automatically** (in Folders) rescans only while the app is open, never
keeps the computer awake, and after the computer wakes the wait starts over instead of scanning
at once. A rescan reads only what changed, so it takes seconds. **Cmd/Ctrl + R** in
**Games** scans now.

**Q: What do "Duplicate" and "Reclaimable" mean?**
A game is a duplicate when more than one **full copy** of it is on disk (updates and DLC never
count). Reclaimable is the space held by every full copy but the largest. In a duplicate's
details, **Keep the largest copy, free …** moves the others to the Trash.

**Q: Can it install games on my PS5?**
Yes. With a console connected, every card shows whether that PS5 has the game ("Installed ·
1.04") or a newer update the collection holds, and the chips filter to games it lacks (**Not
installed**) or has updates and DLC to install for (**Updates & DLC to install**). A game's details offer **Install on this PS5** (with its update
and DLC), **Install update**, **Install DLC**, and **⋯ → Bring this PS5 up to date** queues every
newer update and missing DLC at once. Installs go through the normal install queue (base, then
update, then DLC), streamed from this computer. A game kept only as a folder, image or archive is
sent with **Send to PS5** on its page or converted with **Convert…** instead.

**Q: What is "Copy install link"?**
A link a PS5 can install the package from by itself, for a package installer or browser running
on the console. It works for the PS5 it was made for; in **Serving n** (Games header) you can
let any device on your network fetch it instead, for an installer on another console. The link
carries a random code, there is no folder listing, and only the packages you share can be
reached. A link lasts until you stop it (one at a time, or **Stop all**) or the app quits;
nothing stays published after that. **Copy all links** copies every one at once.

**Q: What does Organize collection do, and can I undo it?**
It organizes packages, images (`.exfat`, `.ffpkg`, `.ffpfs`, `.ffpfsc`) and extracted game folders
under `Platform / Game ID - Title`, relative to each configured collection root. Packages and
images are named `Title - Game ID - vVersion - Region - KIND` with their original format's
extension. Extracted game folders move intact with the same name and no added extension;
their internal files are never individually organized. Archives must be extracted first.
DLC is filed with its game, split packages (`_0`, `_1`, …) move together, and inferred package
kinds are marked `_UNSURE`. Extra copies of the same release and format are marked `DUPLICATE`.

You see every move first and can untick any. Items with unreadable or missing metadata are
listed as left alone, with a reason. Already organized items stay put. Existing names are
reserved rather than merged or overwritten. Moves rename items, never copy them. Every run
can be undone from **Earlier runs** in the same window. A collection root that is itself an
extracted game stays put; choose its parent folder to organize it.

A folder on a saved server (SMB, FTP, SFTP, WebDAV) is organized on that server. Items stay
within their configured root, and a connection that drops stops the run where it is, with
Undo still putting back whatever moved. On WebDAV emptied folders are left in place because
its directory delete could remove contents. FTP checks destination names before renaming;
another program creating the exact same destination between that check and the rename could
have its file replaced.

**Q: What are the junk files, and is cleaning them safe?**
A Mac writes a hidden `._` file beside every file it copies to an exFAT drive (each one takes a
whole cluster), plus `.DS_Store`, `.localized` and `Icon` files. No console reads them. **⋯ →
Clean up junk files** finds them, shows what it found and removes them when you confirm. A `._`
file is removed only when it carries the Mac's signature; a file that merely starts with `._`
is left alone. **Remove new `._` files after each scan** keeps them from piling up. On a Mac the
same window can stop Finder writing `.DS_Store` on network and USB drives, and turn Spotlight
off for the drive your games are on (macOS asks for your password).

**Q: Where does Move to Trash put files?**
In the system Trash (Recycle Bin on Windows), so they can be put back until it is emptied. Only
a copy the collection knows can be moved, and you confirm first. Docker, a server without a
desktop, Android and iPhone have no Trash: there the button is hidden unless you turn on **Allow
deleting copies for good** in **Games → Folders**. Then it reads **Delete…**, asks you to
confirm every time, and the copy is gone for good.

**Q: Does it work in the web UI, Docker and Android?**
Yes. The Collection runs in the engine, so in the web UI or Docker it lists the games on the
engine's disks (add the folder from the in-app browser). On Android the folder is picked with
the in-app browser too; the online title lookup for a game that names nothing is not available
there.

**Q: What does the ⋯ menu on a game in the Games grid do?**
It works on that game without opening its page. **Install update** and **Install missing DLC**
appear when the connected PS5 has the game but lacks what this computer has. **Convert…** opens
the best copy in Workshop (a folder before an image, archive or base package). **Make it
smaller…** opens a big PS5 folder or plain image in Workshop set to a compressed image (.ffpfsc).
**Cheats…** opens Cheats with the game selected. **Backport…** appears when the game is
installed but asks for newer firmware than the PS5 has. **Check** reads its copies back.

**Q: What does "Check the copy" check?**
That the copy is whole and readable before you send it. A PS5 debug package is checked against
its own digests. A .zip, .7z or .rar has every file unpacked in memory and its checksum checked.
A game folder or image has every file read back, and must have `eboot.bin` and its param file.
Anything else (a retail or PS4 package, a copy on a server) is read from start to end, which
finds a copy that is cut short or on a failing drive, but not one whose bytes are wrong. The
result appears under the copy on the game's page. Checks run one at a time on this computer.

**Q: How do I send several games, or send to more than one PS5?**
Press **Select**, pick games, then **Send to PS5…** and tick the consoles. Each PS5 gets only what
it lacks: the game's package, or its folder, image or archive, plus newer updates and DLC for the
games it has. Games it already has, up to date, are left alone. The same bar queues the picked
games in Workshop or checks their copies.

**Q: What are "Needs FW", "Cheats" and "Could be smaller"?**
"Needs FW 10.00" means the game asks for newer firmware than the connected PS5 has; it may not
start unless it is backported. "Cheats" means the cheat repositories have cheats for it. "Could
be smaller" lists PS5 games of 4 GiB or more kept as a folder or uncompressed image; **Queue them
in Workshop** builds compressed images of them. How much room that saves depends on the game.
After updating, the firmware needed shows once the next scan has read each item again.

**Q: Can I get a game from my PS5 into the Collection?**
Yes, when the PS5 has it as a folder or image (a dump). Open the game in **Games**: under **On
the PS5's drives**, **⋯ → Copy to Collection** downloads it into your first Collection folder on
this computer and scans. Under **On <console>**, a game that runs from a folder or image has
the same item. A game installed
from a package cannot be copied this way: its installed files are encrypted.

# Installing packages

## Install a package

**Q: Can I install fake packages from the desktop?**
Yes, in **Upload & Install**. Drop or choose a PS4 `.pkg` or a PS5 `.fpkg` (a fake package
often keeps the ordinary `.pkg` extension) in the same drop zone as everything else; its card
offers the two ways a package can reach the PS5:

- **Stream & install** (recommended): the PS5 installs straight from this computer. No copy is
  put on the PS5 first, so it needs no spare space there. This is the most reliable way.
- **Upload & install**: the package is copied to the PS5 first
  (`/user/data/ps5upload/pkg_library/`), then installed from that copy. The copies stay listed
  under **Already on the PS5**, each with **Install**, **Reinstall** and **Delete**.

The note at the top of the screen lists the payloads fake packages need, and says that **PS5
fake packages are not supported on firmware later than 11.60**: they install, but the game does
not start. PS4 fake packages are fine. What is installing or waiting shows in the screen's queue
and in the progress bar, and keeps going when you open another screen. Installs from a link or
an archive (**From a Link or an Archive**) and from the PS5 or a USB drive (**Already on the
PS5**) are in the small "Or install a package" row under the drop zone.

**Q: Stream & install or Upload & install: which one?**
Use **Stream & install** unless you have a reason not to. Use **Upload & install** when the PS5
cannot reach this computer (Stream fails with a network error: see the next question), when you
install from a phone, or when you want to keep packages on the PS5 to reinstall later without the
computer. Upload needs free space on the PS5 for the copy as well as for the installed game.

**Q: Can I Stream & install several packages at once?**
Yes. Pick or drop them together in **Upload & Install**: the list's main button is **Stream &
install** with how many packages there are. They install one after another, a game before its
update and DLC whatever order you picked them in, and anything else in the same list (game
folders, images, files) uploads as usual in the same click. One package's main button is **Stream & install** too;
**Upload & install** sits next to it for when the PS5 can't reach this computer.

**Q: How do I install from a download link, and which of the three ways do I pick?**
In **Upload & Install**, open **From a Link or an Archive** (or **From a link** under the drop
zone) and paste a direct link to a `.pkg` under **Install from a link**.
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
Yes: **Upload & Install → From a Link or an Archive → Install packages from an archive**. Only the `.pkg` files in the
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
Also asked as: **Stream & install doesn't work for a PS4 package, but file transfer works.**
Uploads and file transfers are connections from this computer to the PS5. **Stream & install**
is the other way round: the PS5 connects back to this computer and downloads the package from
the engine, on port **19113** by default. So a stream install can fail where everything else
works. Check, in this order:

- **The firewall.** Allow ps5upload through this computer's firewall; on Windows, for both
  **Private and Public** networks (Windows often treats a home network as Public). On Windows a
  failed install names the network adapter and whether Windows treats it as Public, with
  one-click fixes. In Docker, publish port 19113 and set `PS5UPLOAD_PKG_HOST_IP`.
- **The same network**, with any **VPN off** on this computer.
- **The PS5's proxy** set to **Do Not Use** (Settings → Network → Settings → Set Up Internet
  Connection → your connection → Advanced Settings).
- **The ELF loader on port 9021** still running: the app sends its on-console installer through
  it when the installer is not already running.

To test the connection, open **Connection › Health Check**: its **PS5 can reach this computer**
row has the PS5 try to connect to this computer and says why it could not. A failed stream install in the queue explains this and
offers **Retry via upload**, which is **Upload & install**: it copies the package to the PS5
first and installs from that copy, so it works without the PS5 reaching this computer (it needs
free space on the PS5 for the copy).

**Q: How do I turn a game folder into an installable package? (Workshop)**
Open **Workshop** (or press **Convert first** on a game, folder or image in **Upload &
Install**). Workshop is one column, top to bottom: **① Game** (the only place games are added),
**② What to make**, **③ Options**, **④ Build & install** and **⑤ Queue**. Pick a decrypted game
folder or an `.exfat` / `.ffpkg` image under ①. The app checks it has what a launchable package
needs and builds a fake package on your computer, compressed the way Sony's packages are. It
reads games from the console through the helper, so ftpsrv is not needed. Then choose **Stream
install** or **Upload & install**, or keep the package. Several games, or a folder of games, go
into the queue and are made one after another; a game set to install or upload goes on to
**Upload & Install**'s queue once it is made.
Before installing it, load the payloads under "Which payloads must be running before I install a
package?". Keep the game files you converted from.

**Q: How do I turn a game folder into a game image (`.ffpkg`, `.exfat`, `.ffpfs` or `.ffpfsc`)?**
Open **Workshop**, pick the game folder on your computer under **① Game**, choose **A game
image** under **② What to make** and the format under **③ Options**, then **Make game image**:

- **.ffpkg (UFS2), recommended**: what ShadowMount+ recommends for most games.
- **.exfat**: for the games that only work like content on an external drive.
- **.ffpfs (PFS), experimental**: experimental in ShadowMount+ 1.7. File names inside must be
  plain ASCII (the app lists any that are not before it starts), and the image's own name is
  kept to 63 characters, the longest ShadowMount+ mounts.
- **Compress it into a .ffpfsc** (any format): usually 40–60% smaller, slower to make, and
  always mounted read-only. It is compressed as it is written, so only the compressed file is
  ever on disk. Three levels: **Fast** (quickest, a little larger), **Balanced** (the default:
  almost as small, several times faster to make) and **Smallest** (a second, stronger encoder on
  every block, several times slower for a little more). The level only trades build time for
  size: every level writes the same kind of blocks and checks each one against the console's
  decoder rules, so none is more stable than another. If Balanced saves you only a gigabyte or
  so on a game, Fast is a fine choice.
- **AMPR LZ4 asset packs** (`.exfat` only, not with `.ffpfsc`): for games that read their files
  through AMPR, the data is packed into LZ4 asset packs that the game's own `ampr_emu`
  (`fakelib/libSceAmpr.sprx`) reads back, so the image is smaller while executables, system
  files and already-compressed media stay as they are. The option says when a game cannot use
  it; compress those into a `.ffpfsc` instead.

`.ffpkg` and `.exfat` images keep some free space (0.5% of the game, between 64 and 512 MiB)
so a read-write mount has room for saves or a small patch.

Every image is checked twice: before writing (names and size), and after: it is read back
through its own reader, every name, size and folder is checked in full, and a seeded sample of
the file data (about 1%) is compared byte for byte with the source. Only then does it get its
name. Tick **Full verification** to re-read every byte of the output instead; it takes longer.

Nothing is installed. Copy the image to a folder ShadowMount+ watches on the PS5 (for example
`/data/homebrew`, with **Upload & Install**), and ShadowMount+ mounts it and puts the game on the home
screen. The `.ffpkg` and `.ffpfs` writers and the streaming `.ffpfsc` container come from quer3q's
PS5 Dump Forge; the `.exfat` writer is ps5upload's own. On a PS5 on firmware 13.60 with
ShadowMount+ 1.7, a game image made by the app in each of `.exfat`, `.ffpkg`, `.ffpfs` and a
compressed `.ffpfsc` mounted, registered and ran its game. A folder that is on the console or on
a saved server cannot be made into an image from here; copy it to this computer first.

**Q: Can I install a package that is already on a USB drive?**
Yes. Plug the drive into the PS5; packages on it show in **Upload & Install → Already on the PS5**
(or **On the PS5 or USB** under the drop zone) with an **Install** button. The app first copies the package to internal storage on the console
(drive to drive, fast), because Sony's installer cannot read the exFAT USB mount directly.

**Q: What is the difference between `.pkg`, `.fpkg`, and `.ffpkg` here?**
`.pkg` and `.fpkg` are accepted by **Upload & Install**. The app validates their
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

**Q: How do I back up my users and account IDs?**

Accounts › **Back up user information** › **Download user information (.txt)**. The file lists
every user on the PS5 with their user ID, account ID (hex, decimal and base64), how the account
is activated and its sign-in details, and how to restore an account ID. A save belongs to the
account ID, not the user's name: after a reset, or a crash that loses a user, add a user on the
PS5, give it the old account ID in Accounts › Users (activate it), then restore its saves from a
backup. Keep the file private. It has no passwords or Remote Play keys. Make one again whenever
you add a user or change an account.

**Q: Can I move my saves from a PS4 to my PS5?**
Experimental, from 9.9.0: Saves › **Move saves from a PS4**. Turn on the jailbroken PS4's FTP
server (GoldHEN's, usually port 2121), enter its address, pick its user and games, and copy them
to this computer (your save-backups folder; set one in Automatic backups first). Then pick the
PS5 user: it must have the PS4 account's ID (Accounts sets it; a save follows the account ID),
and each game must already have a save on that user (start it once on the PS5 and save). Each
PS5 save is backed up before it is replaced, so you can put it back from the game's save backups.
The PS4 is never written to, and no users, system files or trophies are changed. PS4-format saves
on a PS5 have mixed reports: try one game first. Moving trophies is not offered: it needs
renaming system folders and rebuilding the database, which can leave a user unusable.

**Q: Can a friend's save work on my PS5 under my account?**
Experimental, from 10.0.1, PS5 saves only. A save is tied to the account that made it, so copying
your friend's save files over, or switching to their account ID, doesn't work. Instead your
friend opens the save in PS5Upload and presses **Share** (Saves, on the game): the app saves the
save's contents, decrypted, as a `.zip` without the owner's account details. On your PS5, start the
game once and save so you have a save of your own, close it, then press **Use a shared save** on
that game and pick the `.zip`. The contents go into your own save, and the PS5 encrypts it again
as yours: no account ID change needed. Your save is backed up first (set a folder in Saves ›
Automatic backups) and put back if anything fails. Both consoles need PS5Upload, and the game
must be the same title ID (same region). Some games check more inside the save itself; if one
won't load it, restore your backup from the game's save backups.

**Q: Can I move a save between consoles, profiles or game regions?**

Saves backup and restore copies the existing encrypted save files (to give a save to another
account, see **Share** above). It does **not**
re-sign saves, change their owning account, or convert title IDs/regions. A backup
must match the target title and account, and the target environment must accept
its encryption. Renaming a GTA V (or other game's) title folder does not convert it.
Cross-console compatibility is not guaranteed, even with matching title IDs.

Download a backup of the target save before trying a restore. The target must
already list a save for that title: launch the game once to create it. Restore
wipes the current save first, with no automatic rollback if an upload is interrupted.

PS4-format restore has unresolved reliability reports. Closing the game or
restarting the console may help some cases, but is not a verified fix. If a restore
still fails, include the title ID, game/app/helper versions, backup origin, whether
the account and console match, and the exact error or a bug-report bundle.

**Q: Where does an Android save backup go?**

The **Backup** button writes a dated `.zip` to Android's **Download** folder
(`/storage/emulated/0/Download`). The success notification shows the full path.
Open your phone's Files app → Downloads and look for the title ID/game name.
**Save to USB** is a separate action using the configured PS5 USB backup path.

**Q: Can I install a split package (`*.0`, `*.1`, …)?**
Not through Upload & Install: it installs a single package file. Pick the single lead `.pkg`.

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

**Q: How do I get space back on the PS5?**
Open **Clean up** (in the sidebar; also in PS5Upload Minion on the console). It finds what takes
space for no reason: updates of games that are gone, leftover `.pkg` files on any drive (your
package library is skipped), ps5upload's own temp files, download files of removed games and
leftovers of removed games. Updates of games you still have are listed too, unticked, with a
warning that the game goes back to its base version. You see sizes and a total first, nothing
is removed without a confirm, and saves are never touched.

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
- **"The PS5 declined the install."** Under it the app shows Sony's error code and what
  it means, and how far the console got: refused before downloading anything (the package
  itself), stopped part-way (the connection), or refused after downloading it all (free space,
  or the content failed the PS5's checks). No code at all? Check the PS5's Notifications.
- **`0x80B21104`.** The PS5's installer would not accept this particular
  package; other packages install on the same console. Reported with recent
  PS5 games on older firmware (a 2026 game on FW 9.60). It is not the
  package's "required firmware" field: in our test a package demanding FW 14.00
  installed on a 13.60 console. The likely causes are a package built with
  tools newer than the console's firmware reads, or one that is not a
  fake-signed (FPKG) build. Use a package made for your firmware (a backport),
  or install the game without the installer: copy its game folder, or a game
  image for ShadowMount+, with **Upload & Install**.
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
computer, and sort either tab by date (newest or oldest first) or by size; to delete one, use
**Files**.

**Q: Downloading a screenshot or folder under `/original_user` fails.**
On some setups (kstuff with ShadowMount+, for example) the console's `/user` resolves to
`/original_user`, the same data under its other name, and the helper refused to read from there
(#531). Since 9.0.1 it reads `/original_user` (read-only: writes still go to `/user`), so those
downloads work again once the PS5 runs the current helper.

**Q: What does the play time in Games actually track?**
**Games** can sort by **Recently played**, and each game's page says how long it was
played. That counts any game you launch, however you launch it — with your controller, from
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

**Q: Do my cheats come back after the PS5 restarts?**
Yes, from 9.5.0, while the **cheat engine** is on. The helper remembers the engine switch, so once it
is on (or you switch a cheat on, which turns it on), the cheats you switched on, and the game's
patches, apply by themselves each time their game starts, also after a reboot, with only the
autoloaded helper running. Turn the engine off (in the app or in Minion › Cheats) and it stays off.
Before 9.5.0 it started off with every helper start, so after a reboot Minion showed your switches as
On while nothing was applied (#550). While the engine is off, Minion now marks those switches
**Saved** and has a button to turn the engine on.

If a game closes within 30 seconds of its cheats being applied, twice in a row, or the console or
helper stops during those 30 seconds, the helper pauses that game's cheats and says so on the TV, so
a cheat that crashes a game can't do it again on every start. Switch any of that game's cheats to
resume.

**Q: What is Play? Where did Remote Play go?**
From 10.0.0, Remote Play is called **Play** and sits right under Home in the sidebar (and in the phone tab bar). It is the same thing: your PS5's screen streamed into the app, played with a controller, the keyboard or touch, and it wakes the console from rest mode on its own. Home shows a **Play** card (Play, or Continue the last game); a game's page has **Play** (start the game and stream it here) next to **Launch on PS5** (start it on the TV only); search has "Play {game}". Everything else in the app works without it, and the Home card can be hidden (Play › Set up brings it back). "Remote Play" is still the name of Sony's feature underneath, so the console's own setting and Sony's PS Remote Play app keep that name.

**Q: How do I set Play up? (np-fake-signin, pairing, and the rest)**
Open **Play › Set up** (or Set up Play on Home). It is a checklist that reads the console every few seconds and ticks each step by itself: connect to the PS5 (the helper running), sign in to a user, give that user an account ID (Accounts), sign the account in offline with np-fake-signin (run from the checklist; restart the PS5 afterwards and the step ticks once it is back), turn Remote Play on (one button, and allow it for the user on newer firmware), let the PS5 be woken from rest mode (only the console shows these settings, so you tick that one yourself), pair this app (one button), and play. The open step is always the next one to do.

**Q: How does Remote Play work on a phone?**
Held upright, the picture is at the top and its controls are under it: **Full screen**, **Stop**
and a **⋯** menu with Stats, Screenshot, Record a clip, Picture in picture, Settings and volume. In full screen held upright,
the on-screen controller sits below the picture. Turn the phone sideways while playing and it
goes full screen by itself (turn it back to return to the page); the only control then is a small
**⋯** handle at the top. Taps on the picture or the controller are always play: they never leave
full screen or stop the stream. Back leaves full screen and keeps playing; **Stop** is in the ⋯
menu. The screen stays awake while you play, and Settings has the controller's size and opacity.
In the Android app, full screen hides the status and navigation bars (swipe from an edge to see them) and holds the phone in landscape.

**Q: Part of the Remote Play picture is cut off, or it looks stretched.**
**Picture size** decides how the picture fits the screen: **Fit** shows the whole picture with black bars where the shapes differ (the default), **Fill** fills the screen and crops the edges, **Stretch** stretches the picture to the whole screen. It shows in full screen: on the page the picture's box is the picture's own shape, so all three look the same there. **Fit** never cuts anything off: like a video player, the whole picture always shows, with black bars at the sides or top and bottom when the shapes differ. On a 16:9 screen (the picture's own shape) all three look the same. Change it in full screen without leaving it: the **Picture size** menu on the bar (desktop), or the Fit / Fill / Stretch row in the **⋯** menu (phones). It's also in Play › Settings, and it applies at once. Before 9.2.0, the Windows app could show the picture enlarged with its right and bottom cut off (#538); update to fix it.

**Q: On Windows the Play picture is enlarged and cut off on the right and bottom, whatever the picture size.**
That was a Windows graphics-driver bug in hardware HEVC decoding (the app, Edge, Opera GX and
other Chromium browsers): it reported a 1080p picture as smaller than it is, so only its top-left
part was drawn. 720p was not affected. From 10.0.1 the app reads the real size from the stream and
draws all of it, switching to software decoding by itself where that can't be put right. If it
still looks wrong, set Play › Settings › **Video decoding** to **Software** or **Video format** to
**H.264** (they apply the next time you start), and send a bug report.

**Q: Does my phone controller (Backbone, Razer Kishi, GameSir, Scuf Nomad...) work with Remote Play?**
Yes. Phone controllers, USB-C or Bluetooth, are game controllers to the phone, so Remote Play reads them like any other: connect it, press a button, and it plays (the on-screen controller hides). If the browser doesn't recognise one, it still plays in the usual layout; if a button is wrong, open Play › Settings › Controllers › **Map buttons** and press each button as asked (any step can be skipped). **Set an extra button** gives a back button or paddle any PS5 button. Mappings are saved per controller. In the Android app the app also hands controllers to the page itself, for phones whose system browser misses them; on iPhone the app makes sure the page has the focus controllers need.

**Q: What is the Game panel in Remote Play?**
The **Game** button on the stream bar (or **Game** in the phone's **⋯** menu) opens a panel over
the picture; the game keeps playing. **Game** shows what is running and has **Snapshot save now**
(a backup of its save taken while you play, kept with the game's save backups as "Taken while
playing"; set a folder in Saves › Automatic backups first). **Cheats** switches the running game's
cheats. **Console** shows temperatures and has **Fan boost**: 30 minutes at a 55 °C fan target,
undone by itself when you turn it off, when the stream ends, or after 30 minutes, even if the app
closed in between. The panel reads the console only while it is open. Restoring a save is refused
while its game runs: close the game first.

**Q: Can I start a game straight onto Remote Play?**
Yes, from 9.7.0. On a game's page, **Play on Remote Play** (next to Play, for the connected
console once it is set up for Remote Play) starts the stream, waking the console if it sleeps,
waits for PS5Upload's helper, then starts the game on the stream. While you play, the Game
panel's **Play something else** starts another of the console's games (the running one is closed
first, after you say so). If the helper does not come back after a wake, send it from
Connection and press Play again.

**Q: Can I install a package while I play?**
Drop the .pkg or .fpkg on the Remote Play picture. By default it installs when you stop playing,
because an install shares the network with the stream and can make it stutter; **Install now**
starts it at once. Progress shows in Activity like any install.

**Q: Can friends watch me play from their own phones?**
Yes, from 9.11.0. While the computer plays, open the phone panel on the stream's bar and choose **To watch**: anyone on the same network scans that code and the stream plays in their browser (tap **Sound** for audio), up to four at once. They only watch; **Hand the controller** next to a watcher lets them play, and **Take the controller back** ends it. A watcher's device has to play the stream's video: the computer picks HEVC when it can play it, and a watcher's device that plays only H.264 says so instead of showing the picture. Each watcher gets a full copy of the stream from your computer.

**Q: The phone link doesn't open, or the watch page says "The picture could not be played here."**
On Android 17 or later, a browser can't reach devices on your own network until it has the **Nearby devices** permission: Settings › Apps › Chrome (or your browser) › Permissions › Nearby devices › Allow. Without it the page never loads, even with every firewall off. Before 10.0.2 the watch page could only show the picture on the computer itself: a phone opens the link as plain `http://`, where browsers turn off the video decoder it used, so H.264 and HEVC both failed. From 10.0.2 a phone plays it through the browser's own video player instead.

**Q: Can I use my phone as the controller for Remote Play on my computer?**
Yes, from 9.5.0. While the computer plays, press the phone button on the stream's bar (**Use a phone as a controller**) and scan the QR code with the phone's camera: the phone opens a controller, full screen and sideways, that plays along with the computer's own controls (up to four phones). Or, in the PS5Upload app on the phone, Remote Play shows that the computer is playing: tap **Use this device as its controller** and allow it on the computer. A controller attached to the phone works through it too. Only the controller page and its connection are open to your network, and they need the QR code's key or your OK on the computer; **Close the link** lets every phone go. Rumble vibrates an Android phone.

**Q: Motion controls?**
Play › Settings › **Motion controls (experimental)** (phones) sends the phone's gyro and tilt as the controller's motion sensors, for games with gyro aiming; the phone controller page has a **Motion** button too. Browsers give motion only to secure pages, so it works in the apps but not on the QR code's page in a phone browser.

**Q: Can Remote Play save what just happened (instant replay)?**
Yes, from 9.8.0. Play › Settings › **Instant replay**: choose how much to keep (15, 30 or
60 seconds). While you play, PS5Upload keeps those last seconds on this computer as the PS5 sent
them; **Save replay** (the clock-with-arrow button on the bar, or in the phone's menu) saves them
as an MP4 with sound, where your screenshots and clips go. Nothing is saved until you press it,
and turning the setting off drops what was kept.

**Q: Can I use turbo, macros or a mouse in Remote Play?**
Yes, from 9.11.0, per game: open the **Game** panel on the stream bar › **Controls** while the
game runs. Turbo repeats chosen buttons while you hold them; Mouse aim moves the right stick with
the mouse once you click the picture (Esc lets the pointer go); a macro plays a few presses from
one key. Each game keeps its own, on this device. The PS5 sees an ordinary controller.

**Q: Can I sharpen the Remote Play picture or crop an ultrawide game?**
Play › Settings › **Picture**: Sharpen, Brightness, Contrast, Colour, Crop to 21:9 and
Sharp pixels, applied at once. If the device cannot keep up (or has no WebGL 2) they turn off by
themselves and say so.

**Q: Can Remote Play upscale the picture to my screen?**
Yes, from 9.11.0 (experimental): Play › Settings › Picture › **Upscale to the screen (FSR 1)**. The PS5 streams at most 1080p; on a bigger or 4K screen the picture is normally scaled up by plain smoothing, which looks soft. With this on, each frame is upscaled on the graphics card to the picture's own size on your screen with AMD FidelityFX Super Resolution 1 (edge-aware upscaling, then sharpening). It is plain shader code, so it works the same on AMD, NVIDIA, Intel and Apple GPUs, and phones. **Sharpen** sets how crisp it is. It does nothing when the picture is not shown bigger than the stream, and it turns itself off with a notice if the device can't keep up. Screenshots and clips are saved at the upscaled size. NVIDIA's RTX Video Super Resolution and Apple's MetalFX can't be used here: they only work on video players or native apps, not on the app's stream picture.

**Q: How do I take a screenshot or record a clip in Remote Play?**
Use the **camera** (Screenshot) and **video** (Record a clip) buttons on the picture's bar (desktop, also in full screen), or in the **⋯** menu on a phone. A screenshot is a PNG at the stream's own size (1920×1080 or 1280×720), whatever size the picture is shown at. A clip records the picture and the game's sound (also while the player is muted); a red **Recording** light with the time shows on the picture, and pressing it (or the button again) stops and saves the clip. Clips are MP4 where the device records MP4, else WebM, and stop by themselves after 10 minutes (they are held in memory until saved). Files are named like `PS5 Living room 2026-10-30 21-14-05.png`. On a phone they go to the photo library (**Play › Settings › Save screenshots and clips to Photos**, on by default): the iPhone and iPad app adds them to **Photos** (it asks once, for adding only; it never reads your photos), the Android app to the gallery's **PS5Upload** album (Pictures/PS5Upload and Movies/PS5Upload, Android 10 and later), and a phone's browser shows **Save to Photos**, which opens the share sheet where **Save Image** or **Save Video** puts it there. With that off, if Photos access was refused, or on older Android, the apps save into **Downloads** (the iPhone app into its folder in the Files app). The desktop app saves straight into **Downloads**, and the web UI on a computer downloads them through the browser. Recording needs a device that can record video from a web page; where it can't, only Screenshot shows.

**Q: How much delay does Remote Play have, and what does Low latency do?**
With **Stats** on, the picture shows a **Latency** line, for example `Latency ≈ 9 ms engine + 4 ms decode`: the round trip between this device and the engine (measured once a second; near zero in the desktop app, a network hop in the web UI or on a phone) and how long a frame takes from arriving to being drawn. The leg from the PS5 to the engine isn't included: the two have no shared clock, so it can't be measured honestly. **Play › Settings › Low latency** (off by default) uses the smallest sound buffer, keeps the engine's picture queue for this device at 2 frames instead of 6, and skips to the next whole picture sooner when decoding falls behind. Frames are drawn as soon as they're decoded either way. The trade: on a busy network more frames are dropped and the sound can crackle. The sound part applies at once; the picture part at the next start.

**Q: Remote Play says "The network is dropping frames". What should I do?**
With **Lower quality on a busy network** on (the default), when a 1080p stream keeps losing frames for about 10 seconds the picture offers **Switch to 720p** once per session. 720p needs about two-thirds the bandwidth. Switching restarts the stream, because the resolution is fixed for a session, and the PS5 holds its last session for about 12 seconds after it ends, so expect roughly 15 seconds of waiting; the app waits that out and starts again by itself (**Cancel** stops it). It's offered rather than done for you because that pause can come at a bad moment in a game. The choice is kept (Picture becomes 720p in Settings); switch back there. It never steps down twice or back up by itself. Turn the setting off in Play › Settings to never be asked. A wired connection for the PS5 (and 5 GHz Wi-Fi for this device) helps most.

**Q: Can I use voice chat (a microphone) in Remote Play?**
Experimental, from 9.5.0. First turn on **Microphone (experimental)** in Play › Settings
(off by default; it applies from the next start). Without it, a session starts exactly as before.
Then press the **Microphone** button on the picture's bar (desktop and full screen), or **Mic** in
the **⋯** menu (phones). It's off until you press it; the first press asks
for permission to use the mic. While it's on, the icon has a red dot and your voice is sent to the
PS5 the way chiaki-ng sends it, meant for party and game chat. Press it again to turn it
off: the mic is released and the console's mic is muted. It also turns off when the stream stops
or moves to another window. Echo cancellation and noise suppression are on, so headphones aren't
required, but they help. The button is missing where the browser can't encode the sound the way
the PS5 expects (it needs WebCodecs audio encoding with Opus: current Chrome and Edge have it;
other browsers and the Linux desktop app may not). Nothing about the mic is saved.

It's experimental: it follows chiaki-ng's microphone code byte for byte (the same 48 kHz Opus
frames, packets and encryption). If your friends can't hear you, or the stream misbehaves while the
mic is on, turn it off and tell us in a GitHub issue. The desktop apps ask the system for the mic (on
macOS: System Settings › Privacy & Security › Microphone; on Android and iPhone, the usual
permission prompt). If you said no once, allow it there and press the button again.

**Q: Which browser should I use for Remote Play in the web UI?**
Chrome or Edge (on Android, Chrome). Remote Play is made and tested for them. In Firefox and Safari, Esc always leaves full screen, so it can't be the controller's ○ there; Firefox has no picture in picture, and Safari's shows the picture only. The web UI says so on the Remote Play page in those browsers. On iPhone and iPad every browser is Safari underneath, and Remote Play works there. The desktop and Android apps don't depend on your browser.

**Q: Does Remote Play turn on Game Mode?**
Where the system lets an app ask for it, yes, and only while the picture plays:
- **Mac** (macOS 14 and later): the app counts as a game, so full screen turns on Game Mode (the Mac shows it in the menu bar) and leaving full screen turns it off. The computer and screen also stay awake while you play.
- **iPhone and iPad** (iOS and iPadOS 18 and later): Game Mode turns on while the app fills the screen, whether or not a stream plays.
- **Windows**: Windows decides by itself which apps get Game Mode; an app can't ask. The computer and screen stay awake while you play. To have Windows treat the app as a game, press Win+G while it's open, open Settings › General and tick **Remember this is a game**.
- **Linux**: the app turns on GameMode (Feral's `gamemoded`) while you play, if it is installed; nothing else is needed. The computer and screen also stay awake.
- **Android**: the screen stays on, the device keeps a steady speed instead of bursts that slow down when it gets warm (on devices that support it), and a TV or monitor on HDMI is asked for its game mode (Android 11 and later). The app doesn't call itself a game on Android: there that only gives the system's game tools a say over the app.
- **Web UI**: a browser tab can't ask for any of this.

**Q: Remote Play on Linux says there is no H.264 decoder ("No decoder found for codec avc1…").**
On Linux the app's window plays video through the system's GStreamer, and some distributions
(Arch, CachyOS and others) don't install its H.264 decoder by default (#548). From 9.4.1 the `.zip`
(AppImage) carries its own GStreamer and decoder, so it plays wherever it runs. With the `.deb` or
`.rpm`, or from 9.4.0 and earlier, install the decoder and restart the app:
- Arch, CachyOS, Manjaro: `sudo pacman -S gst-libav`
- Debian, Ubuntu, Mint: `sudo apt install gstreamer1.0-libav` (the 9.4.1 `.deb` recommends it, so a normal install brings it)
- Fedora: `sudo dnf install gstreamer1-plugin-openh264` (the 9.4.1 `.rpm` recommends it)

Chrome and Chromium carry their own decoder, so Remote Play also works from them through the web UI.
The app now checks for a decoder before it wakes the console and, without one, says what to install.

**Q: What does Play (Remote Play) do, step by step?**
Remote Play streams a PS5 into the app (desktop, web UI and Android) and lets you play it with a
controller connected to the computer, the keyboard, or touch. It wakes a console in rest mode and
signs your user in on its own, and keeps playing while you use other screens. One console streams
at a time. **Play › Set up** has the same steps as a checklist that ticks itself and does what it
can (see "How do I set Play up?"):

1. **Sign in to a user on the PS5.** Use a user without a login passcode; a passcode stops the
   automatic sign-in after a wake.
2. **Give that user an account ID.** A PSN-linked user has one; an offline user gets one in
   Accounts › Users (write it down: saves made afterwards are tied to it).
3. **Sign the account in to PlayStation Network (offline)** if the console's own Remote Play
   setting is greyed out (see the next question), then restart the PS5.
4. **Turn Remote Play on**: Settings › System › Remote Play. The app can switch it on, and on
   firmware 10.00 and later allow it for the user.
5. **Let the PS5 be woken**: Settings › System › Power Saving › Features Available in Rest Mode ›
   Stay Connected to the Internet and Enable Turning On PS5 from Network.
6. **Pair the app**: the Pair button in Play › Set up (or Connection › Wake & sign in › Set up).
   One click; the keys stay on this computer. Do it again after resetting the console or
   changing the user.
7. **Play**: Play › Start.

If the picture stays black or stutters, use a wired connection or 5 GHz Wi-Fi, try 720p or 30 fps,
and let the app through this computer's firewall on the local network (TCP and UDP 9295–9304). A
browser needs WebCodecs (a current Chrome, Edge or Safari). To use Sony's PS Remote Play app or
chiaki-ng instead, **Request PIN** on the same page registers a device without the PS5's menus.
The same page lists what to check if it still does not work.

Remote Play shows the console selected in the app. If another console is streaming, it keeps
playing and the page offers it (**Show it**); **Start** on this one stops that stream first. The
toolbar on the picture has mute and a volume slider.

A PS5 plays Remote Play with one device at a time. When another device with ps5upload (a phone,
another computer) is playing the selected console, the Remote Play page says so before you
start, every device gets a short note when someone starts, and **Connection › Devices** lists it
as that device's activity. Starting on a second device ends the first one's session.

On the keyboard, **Esc** is Circle. In full screen a quick press is still Circle, and holding Esc
leaves full screen (so backing out of a game menu never drops you out of full screen). In Safari
and Firefox the browser itself leaves full screen on any Esc; use Backspace for Circle there.

On a phone or tablet the Remote Play page is view only: the picture shows, but taps never reach
the console. **Full screen** is where the on-screen controller is. A controller or keyboard
connected to the phone or tablet plays on the page too.

**Picture in picture** puts the stream in a small window that stays on top while you use other
apps. The desktop app opens its own small always-on-top window; Chrome and Edge use the
browser's picture-in-picture window. Safari's picture in picture shows the picture only: the
keyboard and controller play it while the app's window is in front. Firefox and phones do not
offer it. A controller keeps playing the small window while another app is in front in the
Windows app and in Chrome and Edge; in the macOS and Linux apps, click the small window first.

**Q: How do I change a user's avatar, name or account ID?**
In **Accounts › Users**. Each user on the PS5 is one row: click the picture to change the avatar
(crop or fit any image), edit the name (16 characters at most), and see whether the user is
active and its account ID, in hex and in the base64 form Remote Play uses. A user can be deleted
from its row. Account slots that hold no user are listed under **Other account slots**. Restart
the PS5 to see a new avatar or name there. To add a user, do it on the PS5: Switch User, then Add
User.

**Q: The PS5 won't let me turn on its own Remote Play setting.**
The PS5 offers Settings › System › Remote Play only to an account signed in to PlayStation
Network. Open **Accounts** › **Sign in to PlayStation Network (offline)**: it checks that the
signed-in user has an activated account, then fetches earthonion's np-fake-signin and runs it on
the PS5 with one button. Nothing is sent to Sony. Restart the PS5 afterwards. Before it runs, the
helper keeps a copy on the PS5 of every setting and file np-fake-signin changes; **Undo sign-in**
on the same card puts them back (restart again afterwards). In the web UI, send np-fake-signin
from **Payloads** instead (Undo works there too, but nothing is kept unless the desktop app ran
the sign-in).

**Q: After signing in from Accounts, the PS5 asks to "Temporarily Disable PS5 Console
Restrictions" before games open.**
np-fake-signin copies a template account's PlayStation Network settings onto yours, among them a
1983 birth date and an "offline adult" flag that is off, and on some consoles the PS5 then asks for
that before any game starts (upstream np-fake-signin issue #1 reports the same; its own undo,
Settings › Users and Accounts › Other › Sign out, can stay greyed out). If the sign-in ran from
this version of the app, press **Undo sign-in** on Accounts and restart. If it ran before, there is
no copy to put back: the Accounts card says so, and Settings › Family and Parental Controls › PS5
Console Restrictions (passcode 0000 unless you changed it) is where the restrictions are lifted.

**Q: What does the fan curve do?**
The PS5 has one fan setting: the temperature its fan control works to hold (lower is louder).
The curve sets it to the lowest temperature at which you ask for 100%, up to 80 °C, and keeps it
there across game launches and helper restarts. A curve that never asks for 100% at 80 °C or
below (the default one) leaves the fan to the console's own setting (91 °C on firmware 13.60).
**Restore default** does that. ShadowMount+ 1.7 can set the same value (`fan_target_temperature`
in its `config.ini`); use one or the other, or each overwrites the other.

**Q: What do the GPU clock and CPU clocks on the Hardware screen mean?**
All of them are read only when you press **Read sensors** (never in the background), and the
helper never changes a clock.

- **GPU clock** is the graphics clock the console is running at that moment, from the PS5's power
  controller.
- **CPU clock (measured)** is how fast the CPU really runs. The helper times a run of simple
  instructions on each core it is allowed to use and works out the clock from that (about 20 ms per
  core). It only works on the four system cores; the four game cores are kept for games and
  can't be measured from outside a game. On a PS5 at the home screen this reads about 2.8–3.2 GHz.
- **CPU target clock** is the speed the power controller has set for those cores (3.2 GHz at the
  home screen). The cores can run below it, which is why the measured clock is often lower. Hover
  over either row to see every core; the game cores show their target only (800 MHz when idle).
- **Reference clock** is a fixed rate the system reports (800 MHz on every console we've seen); it
  is not how fast the CPU runs.

The PS5 CPU's "up to 3.5 GHz" applies to the game cores while a game runs, so don't expect to see it
here. The power controller reads come from NeoEnabler, whose users have tried them only on firmware
13.20 and 13.60. On other firmware the console may not answer: the rows then say so, and the
helper stops asking until it restarts. If you see numbers that look wrong, please report them with
your firmware version.

**Q: What is nanoDNS?**
A small DNS server that runs **on the console** (from the Payloads
catalogue, not built in). It blocks PSN and update domains by default and
can redirect any domain to a machine on your LAN — useful for staying
offline-friendly while jailbroken. Its card on **Payloads → On your PS5** (**Manage**) edits its
config: it detects which version is running and preserves your existing
rules, comments and custom resolvers rather than rewriting the file.

Point the console's DNS at it once it's running.

**Q: Where are console snapshots?**
In **Files**: **Snapshot…** on a file or folder (or several selected) copies it to a tagged copy
on the PS5, and **More → Snapshots** lists them to restore later. It is meant for the small, breakable things — app database
state and configuration — before a bulk operation, not as a game backup.
For save data specifically, use the Save data screen.

---

## PS5Upload Minion (on the console)

**Q: What is PS5Upload Minion?**
A page that runs on the PS5 itself, in the PS5's web browser, for when your PC isn't nearby.
The PS5Upload helper serves it on port 9180. It has seven sections, on the left:

- **Home**: the running game (with Close game), the tasks that are running (with Cancel) and
  your drives.
- **Games**, with two views. **Installed**: start, close, hide or uninstall your games, with
  each game's version, size and where it is stored. **On Drives & Shares**: press **Scan** and it
  looks through your network shares and this PS5's drives for
  games (`.pkg` files, game folders and disk images) and shows them as a cover grid with title,
  version, type and the system software they need. Install a package from the TV (one on a
  share is copied to the PS5 first and the copy removed afterwards), or copy a game folder or
  image to a drive's `homebrew` folder and add it to the home screen. Results are kept on the
  PS5, and a scan stops at a size limit so a huge share cannot hang it.
- **Saves**: back up a game's save and put it back later, to the internal drive or a USB drive.
- **Files**: browse, copy, move, rename, delete, view, verify and search; set permissions, make
  and edit text files. **Network shares** adds an SMB share (a PC or NAS) you can browse like a
  drive and copy to and from. **Extract** unpacks a `.zip`, `.7z`, `.rar` (multi-part too) or
  `.tar` on the console into a folder there (password-protected `.zip` and `.7z` work,
  password-protected `.rar` does not). Pick a `.pkg` on internal storage or USB to see its icon,
  title, version and type and **Install** it (a USB package is copied to internal storage first,
  then the copy removed). **Load payload** sends an `.elf` or `.bin` on the console to the
  payload loader, after a confirm.
- **Cheats** for the running game, including **Find cheats** to download them to the console.
- **System**, with three views. **Fan**: temperatures, fan speed and the fan target.
  **Processes**: what runs now; stop a game or a payload (system processes cannot be stopped).
  **Clean up**: get space back from updates of removed games, leftover packages, temp files and
  leftovers of removed games (the same as the desktop's Clean Up screen).
- **Settings**: theme, language, full screen, the tips, and the switches below.

**Q: How do I open it?**
Pick the **PS5Upload Minion** tile on the PS5 home screen. You can also open
`http://127.0.0.1:9180/` in the PS5's web browser. Either way the PS5Upload helper must be
loaded (send it from the app, or from your payload loader), and it must be the 7.1.0 helper or
newer.

**Q: The tile shows "PS5Upload Minion isn't running".**
The helper isn't loaded, for example after a reboot or rest mode. Load the PS5Upload helper from
the app on your PC or from your payload loader; the page notices and opens by itself (or press
**Try again**). It is the browser's saved copy of the page saying the helper didn't answer;
nothing is wrong with your files. If you see the browser's own error page instead, the browser
had no saved copy: load the helper the same way and pick the tile again.

**Q: How do I use it with the controller?**
✕ selects what is highlighted, ○ goes back one step (out of a dialog, up a folder, back to
Home), and △ opens the actions for the highlighted item; the **⋯** button on each row does the
same. The PS5's pointer works too: move it with the left stick, and ✕ clicks what it is over.
Every action has a button on screen. **Help**, at the bottom of the left bar, lists the buttons
that work on the current screen, and **Settings** shows the first-time tips again.

**Q: Can I use it from my phone or PC?**
Yes. On the console, open **Settings → Allow other devices on the network** and confirm. Then
open `http://<console-ip>:9180/` in a browser on the same network (the console's address is the
one you use in the app). There is no password: while this is on, **anyone on your network can
use the page and manage your files**, so turn it off when you're done. Turning it off
disconnects the other devices; the console keeps access. While it is on, text boxes on the
console show a QR code ("Continue on your phone") so you can type on your phone instead.

**Q: How do I hide or bring back the tile?**
In **Settings**, switch off **Show PS5Upload Minion on the home screen** to remove the tile, and
switch it on again to bring it back. With the tile hidden you can still open
`http://127.0.0.1:9180/` in the PS5's browser. The tile (title id `PSUP00001`) is left out of
the app's game list; an older engine or Docker image may still list it.

**Q: How do I turn PS5Upload Minion off completely?**
On the console, **Settings → Turn off PS5Upload Minion**; in the app on your PC, **Settings →
PS5Upload Minion** has the same On/Off switch. Off removes the home-screen tile and stops the
page on port 9180, and it stays off after a restart (the helper keeps the choice in its
`webui.json`). The PC app keeps working, and only it can turn Minion back on.

**Q: How do I remove PS5Upload Minion completely?**
In the app on your PC, **Settings → PS5Upload Minion → Remove Minion from this PS5**. It turns
Minion off (no tile, no page on port 9180) and deletes what Minion keeps on the PS5: its Files
favourites and recent folders, its cache of games on drives and shares, and its controller log.
It keeps saved network shares, cheats and backups, which the PC app uses too. Minion is part of
the helper, so nothing is installed again when the helper starts: the helper reads its `webui.json`, which says
off. If that file is lost (the `/data/ps5upload` folder was wiped), the next connection from a
computer that turned Minion off turns it off again. You can turn Minion back on from the same
place at any time. The same Settings section can also let other devices on your network open
Minion, and shows the address to open.

**Q: Which network shares does Minion's Games see, and where do I set them up?**
The SMB shares kept on the PS5: add them in Minion's **Files → Network shares**, or from the
PC in **Shared Folders → Shares on the PS5**, which adds, edits and removes them and can copy a server
you already saved (type the password again: the PS5 keeps its own encrypted copy). Minion
browses them in Files, and **Games → On Drives & Shares** shows the games on them.

---

## Shared folders, safe moves, and backport helpers

**Q: Why can't I rename a file from one PS5 drive to another?**

Because the alternative was crashing your console.

On the PS5, renaming a file from one drive to another (say a USB drive to
internal storage) doesn't fail cleanly the way it does on a PC — it panics
the kernel and locks the console up hard.

So ps5upload never does it. **Move to…** in File System and the game
screens copies the file to the other drive, checks the copy is whole, then
deletes the original; `mv` in the Shell screen refuses a cross-drive rename.
Renaming *within* one drive works normally.

**Q: What is the Shared Folders screen for?**
It keeps your **NAS and servers** (SMB, FTP, FTPS, SFTP, WebDAV) in one place. Anywhere you
browse for a game or file, pick from them with the ▾ next to **Browse**: upload a file or folder
from a share straight to the PS5, or install a package from it, without copying it to this
computer first. The Collection can also scan a saved server in place. Its **Shares on the PS5**
card manages the SMB shares PS5Upload Minion uses on the console (see "PS5Upload Minion").

**Q: My SFTP or FTP start folder says "No such file", or I can't leave my home folder.**
Before 9.11, SFTP and FTP servers were browsed from the folder the server signs you in to: `/`
meant that folder, so a start folder like `/mnt/games` was looked for inside it and nothing above
it could be reached. From 9.11 a new SFTP or FTP server uses the server's own paths, as an SSH or
FTP client shows them (**Browse the whole server**, on by default): type the start folder exactly
as `pwd` would print it, and browse anywhere the account may read. With no start folder it opens
where the server signs you in. A server saved earlier keeps the old behaviour until you edit it
and turn **Browse the whole server** on; games, favorites and links already saved from it use the
old paths, so add those again afterwards.

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

## Several devices and Mesh Drive

**Q: Can my computer, phone and Docker setup all use the PS5 at the same time?**
Yes, from 8.0.0. Send the helper from an 8.0.0 app once; after that, any device on your network
that runs PS5Upload connects without a code. **Devices** lists them all, with what each one is
doing. The footer shows how many others are connected.

**Q: Do I still need to pair?**
No, unless you want to. Devices paired before 8.0.0 keep working. To make new devices ask first,
turn on **Ask before a new device can connect** in **Devices**. A new device then types the code
shown on the TV, or a connected device lets it in from Connection (**Let another device pair**).

**Q: How do I keep a device out?**
Open **Devices** and press **Block** next to it. It is disconnected and can't connect again until
you unblock it. **Remove** only takes a device that is offline off the list; it comes back as new
if it connects again.

**Q: Another device is uploading. Will sending the helper stop it?**
Sending the helper restarts it for every device. The app asks first and says who is busy, and the
others are told. Uploads pick up where they left off once the helper is back.

**Q: The app says another device sent a newer helper.**
Update this app instead of sending its helper. Sending an older helper would take the newer one's
fixes away from every device.

**Q: Do my settings follow me to a new device?**
Yes. Your consoles, sidebar pins and upload, install, convert and link settings are shared through
each console. A new device picks them up the first time it connects (the app offers to reload so
they take effect). Theme, language, text size, the save folder and the bandwidth cap stay per
device.

**Q: Two devices changed the same setting. Which one wins?**
The later change, on every device, whichever syncs first (8.0.1: each change carries a hybrid
logical clock, so two devices with slightly different clocks still agree). A console removed on
one device stays removed. Settings a new device had before it ever connected don't overwrite what
your other devices already agreed on: it takes theirs.

**Q: Are my settings backed up?**
Yes (8.0.1). Once a day, and before a sync from your other devices or a restore changes several
settings at once, the app saves a copy. The engine keeps the last 14 (in Docker, in its `/data`
volume); each console you use keeps every app's latest copy. Open **Settings › Backup / restore** to
see both lists, press **Back up now**, or **Restore** one: the app keeps a copy of the settings
it replaces first, then reloads. Each list is grouped by the device that made the copies;
back-to-back copies that are alike fold into one row (Restore uses the newest), and the newest
three rows show, with **Show all** for the rest. A wiped or new device can restore another device's copy from the
console list. **Export bundle** still saves everything to a file you keep.

**Q: What is Mesh Drive?**
A folder kept the same on every device that joins it, with its home in a folder on the console.
Open **Shared Folders** (on a phone, under More) and press **New Mesh Drive**:

1. Name the drive.
2. Choose its home on the console: **Make a new folder** (pick where, and its name) or **Use a
   folder that is there**. Files already in it join the drive.
3. Choose the folder on this device that keeps it.
4. Optionally, a password.

The dialog checks each choice as you make it (a folder name already taken, a folder that belongs
to another drive or sits inside one, the app's own data folder) and Create stays off until both
folders are usable. On each other device, open Mesh Drive and press **Keep on this device…** on
the drive, then pick a folder there. Each device syncs with the console every half minute while
the helper runs; Docker keeps syncing with no app open, phones only while the app is open.

**Q: Do I need another device to let a new one in?**
No (8.0.1). Any device joins from the Mesh Drive page. To keep others out, give the drive a password when you
make it, or later from the drive's ⋯ menu (**Set a password…**); a device joining types it once.
The console keeps only a salted hash of it. Any device in the drive can change or remove the
password, and so can anyone who knows it, so it never locks you out when a device is gone. The
password is a lock on joining, not encryption: the files on the console are readable with any
file tool, as before.

**Q: How do I see who is in a drive?**
Each drive card lists **Devices in this drive**: name, kind (Desktop, Android, iOS, Docker), app
version, its folder, how many files and how much it holds, when it last synced and any problem
it has, with a dot for online. That list comes from the console, so every device sees the same
one. Press **Remove** next to a device that is gone for good; its files stay where they are.

**Q: How do I open the drive's folder?**
On a computer, press **Open folder** on the drive card (Finder on a Mac, Explorer on Windows). **Open on
console** opens the drive's home in Files, and Files lists every Mesh Drive in its **Drives** row
(next to `/data` and your USB drives) so you can jump to one from there. The card also shows the drive's
file count, size, conflict copies and the free space left on the console.

**Q: Mesh Drive with Docker or the web UI: where are the files?**
On the engine's disk, inside the container. Map a host folder into the container (for example
`-v /mnt/user/share/mesh:/mesh` on Unraid) and pick `/mesh/...` in the web UI's folder picker,
which browses the container. Keep `/data` mapped too; the engine records which folders it syncs
there. Every browser that opens the web UI sees the same drives, because the engine is the
device.

**Q: What happens if the same file changes on two devices?**
Both are kept. The one that synced second is renamed `name (conflict from <device> <date>).ext` and
reaches every device. A file you save while it is being synced is never overwritten; it is synced
on the next pass.

**Q: Some files are listed as "left out".**
The drive card says how many and lists them when you hover. Either they failed three times in a
row (they are tried again after ten minutes; the card says why), or this device cannot store their
names: Windows refuses names with `: * ? " < > |` or ending in a dot, and macOS and Windows treat
`Photo.jpg` and `photo.jpg` as one file. Rename them on the device that made them.

**Q: I deleted a file by mistake.**
Deleted files go to the drive's `.ps5upload-trash` folder (on each device and on the console),
sorted by date, for 30 days. Deleting many files at once waits for you to confirm on the Mesh Drive
page (**Review the deletes**).

**Q: I unplugged the drive my Mesh Drive folder is on. Will it delete everything?**
No. Every drive folder holds a `.ps5upload-mesh` file. A folder without it counts as offline, and
nothing is copied to or deleted from it until it is back.

**Q: What happens when the console goes to rest mode, or is turned off?**
Nothing is lost (8.2.0). Your devices keep their folders; changes made on them wait and go up
when the console is back. Mesh Drive says what is going on: **Console asleep** (rest mode),
**Helper not running** (the console woke up, but the helper does not survive rest mode), or
**Console away** (off, or on another network), with how long it has been away.

- A sync that is running when the console goes to sleep stops by itself within about a
  minute and keeps the files it already copied. Nothing counts as a failed file.
- When the console wakes, Mesh Drive notices within one round (about half a minute) and offers
  **Send helper**; once the helper runs, syncing carries on where it stopped.
- **Wake console** shows when the app has the console's wake setup (Connection › Power).
- If the console you have selected is asleep, the Mesh Drive page still lists **Your folders**
  with their state.
- With the drive linked to a second console, devices sync through that one while their own
  sleeps.

**Q: I have two consoles. Can a drive be on both?**
Yes (8.1.0). Each console is the master of its own circle: the devices that sync the drive
through it. On the drive's card, open ⋯ › **Link to another console…**, pick the other console
from your list and a home folder on it. From then on:

- devices sync with their own console, and the two consoles are kept the same, file by file,
  directly between them (a computer or Docker in the drive does it; Docker keeps doing it with no
  app open);
- a device whose console is off syncs through the linked one until its own is back;
- the card lists each linked console with its state and how many devices use it.

A console holds a drive once, so links never form a loop. **Unlink** leaves each console with its
copy as a drive of its own; no files are deleted.

**Q: How do I settle a conflict?**
When a file changed in two places, both versions are kept and the drive card shows **Settle
conflicts**. It lists each copy beside the file it is a copy of, with who made it, sizes and
times. Choose **Keep the original**, **Keep the copy** (it takes the original's name) or **Keep
both** (the copy gets a name of its own). Every device and linked console follows; what goes
stays in the drive's trash for 30 days.

**Q: I deleted a drive on one device. What happens on the others?**
They stop syncing it on their next pass and free their folder for another drive. No files are
deleted anywhere.

**Q: Can I download from the PS5 straight to my NAS?**
Yes (8.0.0). Save the NAS under **Shared Folders** (SMB, FTP, FTPS, SFTP or WebDAV). Then, in **Files**,
open a file or folder's menu and choose **Download to <your NAS>**, and pick the folder on it.
The bytes pass through the engine on the way (the Docker container, if you use the web UI on the
NAS), one file at a time, so it only needs room for the largest single file. If the NAS folder
is also mounted into the container, plain **Download** into that folder works too.

**Q: Docker: the console lists my container as a new device after every update.**
Map `/data` to a folder or named volume (`-v /your/folder:/data`). Without that, every update
starts the container with a new identity. The Devices screen warns when this is the case.

## Advanced

**Q: Can I run the engine standalone (without the desktop app)?**
Yes — `ps5upload-engine` is a self-contained HTTP server on port 19113. It binds every
interface (`0.0.0.0:19113`, so the PS5 can fetch packages from `/pkg-host/` during a Stream
install), but everything else answers only loopback callers and the addresses in
`PS5UPLOAD_ALLOW_IP`; any other source gets a `403`. The desktop app uses it under the hood;
CLI users can hit the `/api/*` endpoints directly.

**Q: Can I write my own client against the payload?**
Yes. The protocol, AVA1, is its own open-source project:
[github.com/phantomptr/ava1](https://github.com/phantomptr/ava1) (MIT or Apache-2.0). It has the
specification, the schema every message is defined in, byte-exact test vectors, and a Rust crate
(`ava1` on crates.io) you can build a client with. The helper's own methods (files, apps, saves and
so on) are in `protocol/ps5upload-ava1/` in this repository. You must pair with the console: it
shows a six-digit code your client must display too.

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

Every release file is scanned with ClamAV before it is published, and each one carries a
GitHub build attestation: proof that it was built by this repository's release workflow from
the tagged source and not changed since. With the [GitHub CLI](https://cli.github.com) you can
check a download yourself:
`gh attestation verify PS5Upload-<ver>-win-x64.zip -R phantomptr/ps5upload`. If that passes, a
single vendor's flag is a false positive; please report it to that vendor (and tell us which).

**Q: How do updates work?**
The app checks GitHub once per launch and shows a dot on the Settings entry when a newer version
exists. **Settings → Updates → Download** saves it to your Downloads folder. Where the app can
replace itself (Windows, macOS when the app's folder is writable, a Linux AppImage) the button
becomes **Install and restart**: the app quits, the new version takes its place and opens. On
Windows the setup itself starts (9.1.0): it shows a progress bar, closes the app, installs over
it and starts the new version, and if something goes wrong it says so. A `.deb` / `.rpm` shows
the command to run instead. Then press **Send helper** in the banner if
the app says the helper is older.

**Q: On Windows, "Install and restart" closed the app and nothing was installed.**
Fixed in 9.1.0 (#533). From 8.6.1 to 9.0.1 the update ran the setup through a hidden PowerShell
script, which security software or a group policy can block (and which broke on user folders
with non-English letters); by then the app had quit, so nothing was installed or shown. Install
9.1.0 by hand once: download the Windows setup from the release on GitHub and run it. From then
on, Install and restart starts the setup directly.

**Q: "Install and restart" froze the app on my Mac.**
Fixed in 8.3.0. Quitting could wait forever on its clean-up, so the old app sat "not responding"
and the update waited for it. Quitting now finishes within a few seconds whatever happens, and
the update closes an app that still has not quit after 30 seconds. On the version that froze,
force-quit the app (Option-Command-Esc): the update then finishes by itself and opens the new
version.

**Q: Can the app keep running in the system tray?**
Yes (8.3.0, desktop). **Settings → Window → System tray**:

- **Windows and Linux**: **Close to the system tray** and **Minimize to the system tray**. Click
  the tray icon to bring the window back; right-click it for **Quit PS5Upload**.
- **macOS**: **Keep running in the menu bar when the window is closed**. Closing hides the
  window and its Dock icon; the menu bar icon has **Show PS5Upload** and **Quit PS5Upload**.

Transfers, Mesh Drive and the engine keep going while the window is hidden. On Linux the tray
needs `libayatana-appindicator3` (most desktops have it); without it the switches stay off and
the window closes as usual.

# Help

## Troubleshooting

**Q: On Windows the app window is completely black.**
Usually an overlay hooking the app's WebView: MSI Afterburner / RivaTuner Statistics Server
(RTSS) does this. Close RTSS, or in RTSS add `msedgewebview2.exe` (and `ps5upload-desktop.exe`)
and set **Application detection level** to **None** for them, then reopen ps5upload.

**Q: Something is not working. Where do I start?**
Open **Health Check** (also on **Home**, which shows anything that needs attention). It checks,
for the console you have selected:

- **Connection and helper**: the helper answers, and its version matches the app.
- **Network**: the PS5's payload loader port (9021) is open; the PS5 can connect back to this
  computer (what **Stream & install** needs: a failure here is almost always a firewall); how
  long the helper takes to answer; whether ps5upload's installer is running on the PS5 (the
  check starts it when it is not, and says why when it cannot).
- **This computer**: its data folder can be written. In Docker, whether the engine is handing
  the PS5 an address it can actually reach.
- **Storage, clock, Remote Play**: free space, the folders the app needs, the console's clock.

Every problem comes with what to do about it, and some have a one-click fix. The same checks
run in the desktop app, on Android and in the Docker web UI, because the engine does them.

**Q: "The app cannot reach its engine on this computer". What now?**
The engine is the part of the app that talks to the PS5; it runs in the background on your
computer. When the app cannot reach it, a notice at the top of every screen says why, from what
it can see: the engine stopped, its program is missing (security software quarantined it), its
address is held by something that does not answer (another program, or a firewall or antivirus
filtering local connections), or requests went to a proxy. From 10.0.1 the desktop app also
restarts an engine that stopped on its own (once every two minutes at most) and shows why it
stopped. Press **Restart engine** if the notice stays. If it keeps
happening, allow `ps5upload-engine` in your firewall or antivirus. "Payload didn't come up" with
`error sending request for url (http://127.0.0.1:…)` was this problem, not the PS5: the app now
says so.

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

**Q: Launch from Games did nothing.**
Launch starts the game; the first start can take a while. If it never appears, close it from the
PS5 and start it there. See "My uploaded game won't launch".

**Q: I see errors in the status bar but don't know what happened.**
Open **Activity & logs → Logs**. Every runtime error, failed API call, and
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
