# Changelog

What's new in ps5upload, written for humans.

---

## 4.1.1

A security and reliability patch. Path-traversal and buffer-overflow fixes,
Android mDNS discovery that actually works, and a hardened CI pipeline.

- **Fixed: path traversal in payload runtime.** The PS5 payload's
  `is_path_allowed()` validation had edge cases that could allow crafted
  paths to escape the writable-roots allowlist. Tightened the validation
  logic to prevent directory traversal via `..` sequences and symlink-style
  overrides.
- **Fixed: buffer overflow in `drive_sensors.c` (CRITICAL).** A fixed-size
  buffer in the drive-sensors path could overflow when parsing SMART
  responses with unexpectedly long model/serial fields. Capped the copy
  length to prevent stack corruption on the PS5 side.
- **Fixed: `fan_curve.c` truncation.** Fan-curve duty-cycle values could be
  truncated when converting between integer formats, potentially applying a
  slightly wrong fan speed at certain temperature points. Added proper
  bounds checking on the conversion path.
- **Fixed: async unmount cleanup.** An unmount initiated while a transfer
  was still winding down could leave stale state, blocking the next mount.
  The unmount path now properly awaits async cleanup before signaling
  completion.
- **Fixed: localStorage credentials sweep.** 57 call sites storing sensitive
  data (engine URL, tokens) in `localStorage` were audited and migrated to
  Electron/Tauri `safeStorage` (encrypted at rest with OS keychain) where
  applicable, or cleared after use.
- **New: Android mDNS discovery via MulticastLock.** Most Android handsets
  silently filter Wi-Fi multicast frames at the firmware level to save
  power, so mDNS-based PS5 discovery found nothing even though the console
  was right there on the same LAN. The app now acquires a
  `WifiManager.MulticastLock` around the mDNS browse via JNI, so multicast
  frames (224.0.0.251:5353) are delivered reliably. The LAN-sweep fallback
  still catches what mDNS misses.
- **Improved: Android CI now builds both arm64 and armv7.** The release
  pipeline ships an armv7 APK, but CI only ever built arm64 — so an armv7
  breakage could ship undetected. Both ABIs are now compiled on every PR.
- **Improved: NDK version pinned in CI.** The Android NDK version was
  picked by taking the newest installed on the runner image, which meant a
  runner-image rotation could silently break the build (new clang warnings
  → errors, libc symbol changes). The NDK is now pinned to a known-good
  major version (r27) with a fallback + warning.
- **Improved: Android release failures are no longer silent.** The
  `build-android` release job kept `continue-on-error` (a flaky APK build
  shouldn't block the desktop release) but now emits a prominent warning
  annotation so the failure is never missed.
- **Security: dependency updates.** `@babel/core` 7.29.0 → 7.29.7 (CVE
  fix), `tokio` 1.52 → 1.53 (engine + client), `lucide-react` 1.24 → 1.25,
  Docker build stage `node` 22 → 26-alpine.
- **Improved: Docker hardening.** The engine Docker image now runs as a
  non-root user with a read-only filesystem and minimal capabilities.
- **Improved: SPDX license identifiers.** All source files now carry
  proper SPDX license identifiers for clarity and license-scanning tools.
- **Improved: CI webui existence check.** A CI step now verifies the
  `webui/` folder exists (rust-embed v8 requires it at compile time) before
  building, with a clear error message instead of a cryptic compile failure.
- **New: `npm run dist:android` script.** Convenience script to build a
  dual-ABI (arm64 + armv7) debug APK locally.

---

## 4.1.0

A reliability release. The desktop app now recovers on its own when the PS5's
helper goes offline, and PS4-format saves get a clear restore warning.

- **New: automatic reconnection after rest mode and network drops.** When the
  PS5's helper goes offline — rest mode wake, a WiFi switch, a payload crash —
  the desktop app now periodically tries to re-send it in the background. The
  helper, your pinned fan threshold, and the upload port come back by
  themselves once the console is reachable again, without clicking Connect. On
  by default (Settings → "Reconnect automatically after rest mode / network
  drops"). Browser sessions are unaffected (no bundled ELF to push). Includes
  an immediate re-probe on the browser `online` event for fast recovery after a
  network change.
- **Fixed: PS4 save restore warning.** PS4-format saves (PS4 games running via
  backward compatibility on PS5) use sealed PFS images. The PS4 emulator caches
  save data internally, so a raw file copy back to the console doesn't always
  trigger a remount — a restored PS4 save may not appear in-game until the game
  is closed/reopened or the console is restarted. The Saves screen now shows a
  warning banner for PS4 saves, the restore confirmation dialog includes a
  PS4-specific note, and the post-restore notification hints about the
  workaround. *(Issue #198.)*

---

## 4.0.0

A major release. Four new PS5 features, a self-hosted browser web UI, and a
big reliability fix so the desktop app always opens.

- **New: Remote Play PIN generator.** Generate a Remote Play PIN and see the
  account ID needed to pair Chiaki / pxplay — right from ps5upload, without
  digging through the PS5's hidden Remote Play settings. Shows the PIN, a
  countdown timer, and pairing status; account ID is auto-detected from the
  console.
- **New: Fan Curve editor.** Beyond the single-threshold fan pinning, you can
  now define a multi-point temperature → duty-cycle curve for smoother fan
  behavior. The curve is pinned through the same hardware path as the fixed
  threshold, so it survives game launches and is re-applied automatically.
- **New: Persistent on-PS5 notifications.** Payload-side events (backups, fan
  actions, errors) are now written to a store on the console that survives
  payload restarts, so the next time you connect you see everything that
  happened while you weren't looking.
- **New: Backup & restore snapshots.** Tag-based snapshots of files or
  directory trees on the PS5 (e.g. the app database), with list, restore, and
  delete — a safety net before risky operations.
- **New: self-hosted browser web UI.** The engine can now serve the full UI in
  a browser, published as a Docker image
  (`ghcr.io/phantomptr/ps5upload-engine-webui`). Native-only features (host
  file/folder pickers, etc.) are gracefully gated out in the browser client.
- **New: drive sensors + permanent fan speed status.** Per-drive SMART/temp
  readings and a persistent fan-speed indicator on the Hardware screen.
- **New: user ID management.** Create and delete local PS5 user accounts from
  the Profile screen.
- **Fixed: the desktop app now always opens.** If the engine's default port
  (19113) is held by something the app can't reclaim — e.g. a standalone
  `ps5upload-engine` a user launched by mistake — the app now falls back to a
  free port instead of failing to start, and the UI follows the engine to
  wherever it actually bound.
- **Changed: cleaner sidebar.** The navigation was consolidated to remove a
  duplicate "System" heading and group everything under clear sections.
- Plus dependency updates and a round of v4.0.0 audit fixes.

---

## 3.4.0

A reliability and feature release. The big themes: transfer and install
robustness on all firmwares, a new hardware sensor, the web UI engine image,
and a payload code consolidation.

- **New: M.2 NVMe temperature sensor.** The Dashboard and Hardware screens now
  show the temperature of an installed M.2 SSD expansion drive, alongside the
  existing CPU and SoC readings. The value is read from the same SoC sensor
  sweep that already feeds the SoC temp — zero extra API calls. An empty slot
  simply shows no reading.
- **Fixed: transfer stream ID collisions.** The FNV-1a hash used to identify
  active transfer streams could collide across concurrent jobs, causing one
  transfer to accidentally cancel another. The hash is now correctly computed
  and the cancel registry hardened so a dead stream's cancel signal can never
  fire. *(Issue #164.)*
- **Fixed: RFC 9110 suffix byte-ranges.** Range requests of the form
  `bytes=-N` (last N bytes) were rejected; now handled correctly per spec,
  so more HTTP clients can resume partial downloads from the engine.
- **Fixed: dashboard polling starved transfers.** The Dashboard's 5-second
  auto-poll was never paused during an active upload, adding unnecessary load
  and occasionally slowing transfers on slower connections. Polling now
  pauses while any transfer is in flight and resumes on completion.
- **Fixed: package install settle fraction too aggressive.** The install
  progress tracker considered an install "settled" at 97%; raised to 99% so
  the UI doesn't report success before Sony's installer has actually finished
  writing data.
- **Fixed: DPI daemon authid on FW 11+.** The fallback install daemon now
  correctly acquires ShellCore credentials on firmware 11 and above (not just
  below 11), using a shared two-tier firmware detection routine. *(Issues #152
  and #164.)*
- **Fixed: a batch of payload hardening fixes.** Profile loading now rejects
  symlinks (`O_NOFOLLOW`), `sys_time` uses `pthread_once` for thread-safe
  one-time init, and the shellui RPC handler fixed for copyout overflow and
  stack buffer issues.
- **New: `ps5upload-engine-webui` Docker image.** The engine + full React web
  UI is now published as a separate multi-arch (amd64 + arm64) image on GHCR,
  in lockstep with the plain engine image on every release. No manual
  `Dockerfile.webui` build needed.
- **Internal: authid/firmware code consolidated.** The ShellCore/System-Install/
  JB authid constants and the firmware-major detection logic — previously
  triplicated across bgft.c, register.c, and the DPI daemon — are now in a
  single shared header (`payload/include/authid.h`). The DPI daemon now uses
  the full two-tier firmware parser, matching the rest of the codebase.
- **Internal: removed unused frontend dependencies.** `@tauri-apps/plugin-fs`
  and `@tauri-apps/plugin-shell` npm packages were never imported by any
  frontend code; removed to shrink the install footprint. (The Rust plugins
  remain registered.)
- **Internal: engine URL lock poison recovery.** The RwLock guarding the
  runtime-configurable engine URL now recovers from poison instead of
  propagating a panic, so a thread failure elsewhere can't wedge the settings.

## 3.3.26

A reliability release focused on fixing package installs on older firmware (FW < 11).

- **Fixed: package installs that failed with error 0x80B21106 on FW < 11.** The install
  helper was wrapping local file paths in `file://` before handing them to Sony's
  installer, but the installer's URI parser rejects that scheme on older firmware and
  returns a parser error — producing hollow, metadata-only installs that would never
  launch. Bare local paths are now passed through directly, matching the standalone DPI
  daemon's behaviour. *(HW-verified on a PS5 Pro FW 9.60 and a PS5 Fat.)*
- **Fixed: the DPI fallback daemon now self-escalates to the right credentials.** On
  FW < 11, `InstallByPackage` needs ShellCore authority to succeed. The daemon (loaded
  fresh via the elfldr port) now self-escalates its own credentials on startup and swaps
  to ShellCore before each install, so the fallback path lands real data instead of
  silently failing. *(Issues #152 and #164 — the "helper dies ~4s after a rejected
  install" symptom is gone.)*
- **Fixed: Stream (beta) no longer offered on firmware where it hangs.** Stream install
  pulls a `.pkg` straight from your PC over HTTP without staging it first, but on
  FW < 11 Sony's PlayGo pre-flight check never returns without kernel patches we don't
  have for those firmwares. The Stream button is now disabled with an explanatory
  tooltip when the connected PS5 is on FW < 11 — use the normal Upload → Install (staged)
  instead, which works perfectly everywhere.
- **New: persistent fan-control threshold.** Set a custom fan speed on the Fan Control
  tab and it survives a reboot — the payload restores it at boot.

## 3.3.25

A big feature + reliability release.

- **Fixed: PKG updates that crashed the on-console helper.** On some firmware,
  trying to install a game update could kill the PS5-side helper a few seconds
  later, so the update never applied. The root cause was the install-fallback
  daemon calling Sony's installer without initialising it first. It now
  initialises properly (with a boot-timing wait and retry), so a rejected
  update no longer takes the helper down. *(Verified on real FW 5.10 + 9.60
  hardware — the helper survives now.)*
- **New: manage ps5upload from a web browser.** The engine can serve the full
  app over HTTP, so a NAS/Docker user can drive their PS5 from any browser on
  the LAN — no desktop app needed.
- **New: install a .pkg straight from your PC — "Stream (beta)".** Installs a
  package over HTTP without staging the whole file on the PS5 first, so you can
  install even when disk space is tight. Labeled beta; the normal
  upload-then-install path is still the reliable default.
- **New: your own payload repos.** Add any GitHub/Gitea repo to the Payloads
  tab and ps5upload tracks its releases and caches the ELF locally — no more
  keeping a pile of `.elf` files on your PC.
- **New: repo-based playlists.** A payload playlist can now pull from a repo at
  run time instead of a local file, so a boot sequence needs no local files.
- **New: video clips.** Browse and download the PS5's gameplay video clips,
  like the Screenshots tab.
- **New: "Install all" for packages.** Install every staged package in one tap,
  in the right order (base game → update → DLC).
- **New: rest mode after uploads.** Optionally put the PS5 to sleep once its
  upload queue finishes — handy for an overnight queue. Off by default.
- **New: restore a save straight from a USB drive** on the PS5 (the counterpart
  to Save-to-USB), plus **find unused games** on the Installed Apps screen —
  sort by play time and surface the ones you haven't touched, then uninstall.
- **Fixed: a dead payload in the catalogue** (the Lapy JB Daemon source had
  moved) and a batch of **UI text that always showed in English** even in other
  languages.
- Routine dependency and CI updates.

## 3.3.24

- **New: back up a PS5 save straight to a USB drive.** From the Saves screen you
  can now copy a save (or all of them) to a USB drive plugged into the PS5 — each
  backup lands in its own timestamped folder so nothing gets overwritten. There's
  a configurable save path in Settings too. *(Thanks to @Twice6804 for the
  contribution.)*
- **Fixed: a few payloads in the catalogue showed "Not Found."** ps5-app-dumper
  and Itemzflow had moved to different repos and were 404ing in the Payloads
  catalogue — both now point at their current homes.
- Routine dependency and CI updates.

## 3.3.23

- **Fixed: game updates that couldn't install on newer firmware.** When the PS5's
  in-app installer turned an update away (a common first step on firmware 11/12),
  the tool was deleting the staged update file a split second too early — so the
  fallback installer (the one that actually lands updates) had nothing left to
  install. The update file is now kept until the whole install cascade has had its
  turn, so updates can fall through to the path that works. Your base game is never
  touched either way. *(Needs a final confirmation on real firmware-12 hardware.)*
- **Fixed: the "Close game" button did nothing on some firmware.** On firmware
  12.20, the PS5's clean "close app" call is rejected, and the tool was giving up
  there instead of trying its backup way of stopping the game. It now falls through
  to the backup stop so the button actually closes the game.
- **Faster uploads while the app is open.** The Installed Apps and Library screens
  quietly check what's running on your PS5 every few seconds. That check shares a
  channel with uploads, and on big multi-part games (exfat/ShadowMount dumps) it
  could drag transfer speed down. Those checks now pause while an upload to that
  console is running, and resume the moment it finishes.
- Routine dependency and CI updates.

## 3.3.22

- **See what's playing, and stop it.** Installed Apps now shows a "Playing" badge
  on whatever game is currently running, and the Play button turns into **Close
  game** so you can stop it right from the app (with a confirm — it's the same
  as quitting on the console).
- **More patience when launching a game.** A first launch (just-installed, or a
  cold start) can take a while to come up. Pressing Play now shows "Starting…"
  and waits for the game to actually appear before saying it's playing — and it
  won't let you fire a second launch into a game that's still starting (which is
  what could knock it back down). If it's taking a while, you get a calm "give
  it a moment" note rather than an error.

## 3.3.21

- **Fixed: moving files from USB to the internal SSD crashed the console.** Cut
  & paste from a USB drive to internal storage was reliably kernel-panicking the
  PS5 (a hard crash + reboot). The tool was asking the console to *rename* the
  file across drives, which this kernel can't do — it panics instead of
  reporting the error. ps5upload now detects a cross-drive move up front and
  completes it the safe way (copy, then remove the original) — the same thing it
  already did for copies. **Hardware-verified: the move that used to crash the
  console now completes without a hitch.** (Same fix applied to the shell tab's
  `mv` command.)

## 3.3.20

- **Installs wait for the PS5 to be ready — fewer "couldn't be applied" errors.**
  Right after an install the PS5 has a brief recovery moment (the screen-black
  blink); starting the next install during it was getting rejected with a
  transient error, so you'd have to wait and retry by hand. ps5upload now checks
  the console is settled before it installs, and if the PS5 says it's busy it
  waits and retries automatically instead of failing — so back-to-back updates
  and DLC just work. (Shows "Waiting for the PS5 to be ready…" while it waits.)

## 3.3.19

- **"Installed" no longer leaks between consoles.** If you staged the same .pkg
  on more than one PS5 and installed it on just one, the others wrongly showed
  it as already installed (Reinstall). Each console now tracks its own installs,
  so a package reads as installed only on the console you actually installed it
  on.

## 3.3.18

- **Cancel a single upload without stopping the whole queue.** The item that's
  actively uploading now has a Cancel button. It stops just that transfer (the
  partial upload stays resumable) and the rest of that console's queue keeps
  going — handy when one big item is hogging the line. ("Stop" still halts
  everything as before.)
- **A ready-to-run engine Docker image.** The self-hosted transfer engine is
  now published as an official multi-arch image at
  `ghcr.io/phantomptr/ps5upload-engine` (`:latest` or pin `:<version>`), so you
  can run it on a NAS or home server without building from source. As always:
  the engine has no password — keep it on a trusted LAN, never the internet.
  Thanks to @Twice6804 for the contribution.

## 3.3.17

UI polish pass — tighter on phones and small windows.

- **"More actions" menus never run off the bottom of the screen.** A row's ⋮
  menu now opens upward when it's near the bottom edge and scrolls inside itself
  if it's very long, instead of spilling past the window.
- **No more scrolling the page behind a dialog.** When a pop-up (a file/image
  preview, an upload prompt, any modal) is open, the content behind it stays
  put — especially noticeable on touchscreens.
- **More of the app is ready for translation.** The new "Install from USB /
  external drive" labels are now in the translation catalog, and a check was
  added so future text can't quietly go missing from it.
- Verified the whole app stays clean with no cut-off or off-edge content across
  phone, tablet, and desktop widths.

## 3.3.16

- **You control when USB / external drives are scanned.** The "Install from USB
  / external drive" section has a new **"Automatically scan when this tab
  opens"** checkbox. Leave it on (the default) and it works as before; turn it
  off and nothing is scanned until you click **Scan** — handy when no drive is
  plugged in, or to skip the check each time you open the tab. The Scan button
  is always there for an on-demand look.

## 3.3.15

- **An installed update or DLC now shows "Reinstall."** After you install a
  patch or add-on from the Install Package tab (or via the upload queue), its
  row correctly reads "Installed · Reinstall" instead of still offering
  "Install." Base game, update, and DLC are each tracked on their own — the
  console can only confirm the base game is present, so ps5upload now remembers
  the specific updates and DLC you've installed itself (and remembers across
  restarts).

## 3.3.14

- **Upload progress no longer overshoots the file size.** If an upload's
  connection blipped and resumed, the progress could read past 100% — e.g.
  "36 GiB / 24.6 GiB". The progress now caps at the file size (the upload itself
  was always fine; this was just the counter double-counting resent bytes).

## 3.3.13

Smoother, smarter installs — especially when queueing a game with updates + DLC.

- **Installs now go in the right order: base game → update → DLC.** Queue a game
  with its updates and add-ons and they install base-first, so an update or DLC
  never tries to install before the game it belongs to (which wasted the upload
  and space). Items within the same kind keep the order you added them.
- **A heads-up before an install that won't fit.** ps5upload now checks free
  space up-front and, if a package is clearly too big for what's left, tells you
  to free up space *before* you wait through a doomed install (estimate-based —
  the real installed size can vary a little).
- **No more spurious "Delete failed" after a successful install.** The staged
  package is now removed with a brief wait + retry (the PS5 can hold the file
  for a moment right after installing), and if cleanup still can't happen it's
  left quietly for "Clear finished" instead of throwing an error at you.
- **Queues of small updates/DLC no longer stall on FW 12.** After each install
  the app waits for the console to settle (the brief "screen goes black" blip on
  FW 12) before starting the next item's upload.

## 3.3.12

- **Download a PS5 file or folder straight to a `.zip`.** In the File System
  browser, each entry now has a "Download as ZIP" button next to Download. It
  streams the file(s) from the PS5 and zips them on the fly — no temporary copy,
  no waiting for a separate compress step. Great for grabbing a save folder, a
  homebrew folder, or a single big file as one tidy archive.

## 3.3.11

Installs you can watch from anywhere, plus a few handy touches.

- **Installs now show in the activity bar at the bottom, with a live %.** Start
  an install and it appears in the bottom activity strip just like uploads and
  downloads do — so you can browse the rest of the app and still see what's
  progressing and how far along it is.
- **A toast pops on the PS5 itself when an install finishes** (in addition to
  the in-app notification), so you get the "done" confirmation on the console
  screen even when the app isn't in front of you.
- **Drag a payload file onto the Send file screen** to fill in its path — no
  need to click Choose every time.
- **The File System returns to /data when a folder no longer exists.** Land on a
  remembered path that's since been deleted (or an unplugged drive) and it now
  drops you back to /data with a note, instead of a stuck error.

## 3.3.10

Smoother installs and a few UI fixes.

- **Installs no longer cry failure while the PS5 is still installing.** On newer
  firmware the PS5 registers a game and then downloads/extracts it in the
  background (its own "Downloading…" tile). ps5upload used to give up after two
  minutes and show a scary "nothing was installed" error even though the install
  was fine. It now waits much longer for the console to finish, the message
  (if it ever does time out) explains the background install instead of alarming
  you, and you get a notification when a title finishes and is ready to play.
- **The Install Package screen no longer flashes "Install" on every package**
  for a split second when you open it — it remembers which titles are installed.
- **Fixed the Play button on disc-image titles** being squeezed and clipped to
  "Pla" on wide windows.

## 3.3.9

A clearer, more helpful "Install from USB / external drive" section.

- **The USB / external drive section is redesigned.** It used to hide itself
  whenever a scan found nothing — so it "popped in" only after results arrived,
  and when a drive was empty the whole thing (Refresh button included)
  disappeared, leaving no way to look again. It's now always present with clear
  **Scanning…**, **nothing found**, and **list** states, a refresh that's always
  available, and a short explanation of what it does (it copies the package onto
  the console first — your drive's copy is left untouched — then installs it).
- **External packages now show real details.** Each one is read on demand for
  its cover art, real game title, version (e.g. `v01.02`), and an Update/DLC
  badge — so two packages for the same game (a base and its update) are finally
  distinguishable instead of both just showing a code. The list still appears
  instantly; the details fill in as it reads each package. (PS5-native packages
  that can't be read keep their filename.)
- **Your uploaded packages show their version too.** The package version (and
  whether it's a base game, update, or DLC) now appears on packages already in
  your library, read straight from the package on the console — so it shows even
  for packages you added before this update, with no need to re-upload them.
- **PS4 game updates now install.** Applying a game's update (its patch) used
  to fail on some firmware with a confusing "PKG header — corrupt or wrongly
  named" — the update shares the base game's ID, and the usual install path
  couldn't apply it. ps5upload now routes updates through the PS5's own update
  installer, which applies the update on top of your game **without touching the
  base** (hardware-confirmed: a Jak X update applied with the 3.8 GB base game
  fully intact). If an update still can't apply — usually because it doesn't
  match your installed version — you get a clear message instead of a scary one,
  and your base game is never at risk.
- **Installing some USB / external packages no longer fails to register.**
  Packages whose name already includes the game's ID were staged under the wrong
  filename and rejected by the PS5; ps5upload now reads the real ID from the
  package first and stages it correctly.

## 3.3.8

Tell a game's updates apart at a glance — and stop an update from looking
"installed" when it isn't.

- **An update/DLC no longer shows "Installed · Reinstall" just because the base
  game is installed.** A PS4 update shares the base game's title id, so once the
  base was on the console its never-installed update inherited the "installed"
  badge and a "Reinstall" button. Add-ons now always read as installable —
  only the base game shows as installed (its install state is the one the PS5
  can actually confirm).
- **Each package now shows its version and original filename.** Updates share a
  ContentID *and* a title, so they used to look identical apart from the
  "update" badge. The library now shows the real package version (e.g. `v01.04`,
  read straight from the package) plus the filename you uploaded — so you can
  tell `[v01.00]` from `[v01.04]` at a glance. (Newer uploads carry this; an
  already-staged package shows it again once re-uploaded.)

## 3.3.7

The 3.3.6 data-loss fix now actually covers PS4 game updates — plus clearer
messaging when an install fails.

- **Installing a PS4 game's update (patch) can no longer delete the base game.**
  3.3.6 added this protection, but it relied on the update being labelled as one
  in a way the install couldn't always see — so a PS4 patch still looked like a
  full game, reinstalled the shared ID, and **wiped the installed base**
  (hardware-confirmed on two consoles: a Jak X patch deleted its 3.8 GB base).
  The app now reads the real "update vs. full game" flag **straight from the
  package on the console** at install time, so an update is recognised no matter
  how it reached the PS5 (uploaded, copied from USB, or picked from the file
  browser) — while a normal full-game re-install still works as before. Verified
  on real hardware, internal *and* extended storage: the base game stays put and
  an update that can't apply on top simply fails harmlessly.
- **Clearer guidance when an install fails or stalls.** A failed install no
  longer blames free space, tells you the empty tile it left behind is safe to
  delete, and points to the PS5's own Package Installer for stubborn packages.
  An "out of space" error from the PS5 now notes it can mean fragmented storage
  even when space is free (rebuild the database from Safe Mode).
- **The Library no longer flickers.** When nothing was running, the "Running
  apps" panel flashed in and out every few seconds as it refreshed; it now stays
  hidden until there's actually something to show.
- **The Avatar changer shows the selected user's current avatar.** The picture
  box now loads the chosen user's existing avatar by default (instead of a blank
  placeholder), so you can see what's there before replacing it. (Picking
  different users was already supported.)
- **Screenshots show thumbnails.** Each row now shows a small preview of the
  shot instead of a generic icon. Previews decode lazily as you scroll (PS5
  screenshots are HDR JPEG XR, so this is desktop-only — same as Convert/Preview)
  and are cached so each decodes once.
- **Run the engine separately / point the app at a remote engine.** You can now
  host the transfer engine elsewhere (including a tiny Docker image) and set its
  URL in Settings; the app talks to it instead of the bundled one. Game/app
  cover art now loads from a remote engine too. Thanks @Twice6804. *(Security:
  the engine's API has no password — only use the "allow extra IP" option on a
  trusted home network, never expose the engine to the internet.)*

## 3.3.6

A data-loss fix: installing a game's update can no longer delete the game.

- **Installing an update (patch) can never overwrite or delete your base game.**
  A game's update shares the same ID as the game itself, so the installer could
  end up *re-installing that ID* — wiping the installed game instead of patching
  it (hardware-confirmed: a 26 GB game was deleted by installing its update). An
  update now installs **only** via the safe path that applies it on top; if that
  can't apply it, the update **fails harmlessly and your game stays completely
  intact** — never replaced or removed. Base games and DLC are unaffected.
  *(If an update won't install through the app on your firmware, install it from
  the PS5's own Package Installer — your game is safe either way.)*
- **Installed Apps cards line up evenly.** Tiles with a longer two-line name no
  longer knocked their neighbours out of alignment.

## 3.3.5

USB install fixes — no more stuck "downloading" tile, and a copy that survives a flaky connection.

- **Installing a big game from USB no longer leaves a broken "Downloading…"
  tile.** 3.3.4 tried to install straight off the USB drive, which made the PS5
  register the game as a download it then streamed off USB at a crawl (a 25 GB
  game showed *"Downloading… 50 hours left"* and left an undeletable tile eating
  space). Installs now always copy to internal storage first and never trigger
  that. *(If you have a stuck Bloodborne/other tile from 3.3.4: delete it from
  the PS5 to reclaim the space — your PKG on the USB drive is untouched.)*
- **The USB→internal copy survives connection drops.** A 25 GB copy used to die
  the instant the PS5 connection blipped — even though the console was still
  copying fine. The copy is now tracked by how much data has actually landed, so
  a dropped connection (or Wi-Fi hiccup) no longer aborts a healthy copy, and a
  **live percentage** shows throughout. *(For very large games, uploading from a
  PC is still the most reliable path — that transfer is fully resumable.)*

## 3.3.4

Install verification + extended-storage fixes.

- **Installs to an extended / M.2 SSD are recognized correctly.** If your PS5
  installs games to an extended drive (not internal storage), the app used to
  report the install as *failed* — even though the game installed perfectly and
  plays — because it only looked for the game in internal storage. It now
  confirms an install by the actual data written to **any** drive, so
  extended-storage installs are no longer wrongly flagged as failures. (If you
  hit this before: the game was fine all along — just launch it.)
- **A "hollow" install still can't masquerade as success.** The app confirms a
  title's content actually landed before reporting success, and tells you
  honestly if it didn't — now measured by what was really written, so it's
  accurate on every drive *and* still catches a genuinely empty "dead tile."
- **Firmware-aware installs.** On newer firmware (11.xx/12.xx) the installer runs
  under the correct system authority so content lands properly; on FW 9.60 and
  below the proven path is unchanged.
- **Big games install from USB without the slow copy.** Installing a PKG from a
  USB drive used to copy the entire file to internal storage first — a single
  blocking step that, for a 25 GB game, could run for hours and then fail. The
  app now installs **directly from the USB path** when it can, falling back to a
  copy only if needed, with a live install **percentage** throughout.

## 3.3.3

A big-install data-loss fix, plus the in-app updater.

- **Large PKGs (25 GB, 100 GB, 200 GB+) install reliably now.** The tool used to
  decide an install was "done" after a fixed ~100-second timer, then delete the
  uploaded PKG — but Sony's installer reads that PKG for the *entire* install,
  which for a big game takes many minutes. So a 25 GB game would upload, get
  deleted mid-install, and leave you with no game and no PKG (the reported
  Bloodborne case on FW 12.20). The tool now **watches the install actually
  finish** instead of guessing: it tracks the title landing on the console and
  the bytes writing to disk, with no size limit, and only deletes the staged PKG
  once the install is genuinely complete. A live install **percentage** shows
  while a big title installs in the background.
- **Stalled installs keep your PKG.** If an install stops making progress before
  it finishes, the tool now says so and **keeps the PKG on the PS5** so you can
  retry — it never deletes a package for an install that didn't complete.
- **In-app update works on Android (and everywhere).** "Download update" used to
  fail on Android (it tried to launch a desktop-style opener); it now opens the
  release via the proper system handler on every platform, with OS/arch detected
  automatically.
- **Processes screen won't offer to kill the tool itself.** The helper's own
  process is now marked and its Kill/Restart actions disabled — no more
  confusing "Operation not permitted" when trying to kill it.

## 3.3.2

A data-loss fix.

- **"Auto Delete after installation" is now actually respected.** With it OFF,
  an uploaded PKG was still being deleted after install — because the engine
  always cleaned up the staged file regardless of the setting. It now keeps the
  PKG when you ask it to. The same fix stops the File System → Install action
  from deleting a PKG you point it at in place.
- **Better install logging.** Bug reports now capture your Auto Install / Auto
  Delete settings and log the exact install/delete decision for each PKG, so
  issues like this are diagnosable without a repro.

## 3.3.1

A polish + fixes release.

- **Hardware temperatures now show °F as well as °C** (e.g. `62°C / 144°F`).
- **Bigger checkboxes.** Checkboxes everywhere (Settings included) were tiny;
  they're now comfortably sized and accent-coloured, and larger still on touch.
- **File System drive list is consistent.** `/data` used to look dimmer than
  your USB/external drives; now every drive reads equally clearly, with just the
  border + icon marking which is active/external/internal.
- **Quieter PS5 kernel log.** The Hardware screen used to query `hw.physmem`,
  which the PS5 rejects and logs as an error on every read. It now uses only
  approved sources for the same RAM figure — no more log spam.
- Fixed a handful of UI strings that weren't translatable.

## 3.3.0

A big feature release.

- **Process manager (new Processes tab).** A live task-manager for your PS5:
  see every running process with its memory and thread count, **Kill** anything,
  or **Restart** a game (closes + relaunches it). User payloads and games show by
  default; a toggle reveals system processes, and killing a system or game
  process asks first. Hardware-verified.
- **Quick bring-up.** One tap on the Connection screen runs your bring-up
  payloads (kernel R/W, SMP), sends the helper, and waits until the PS5 is ready
  — instead of sending each one by hand every boot. Configure the chain in
  Payloads → Playlists.
- **Auto-loader.** Pick a playlist that runs by itself whenever a PS5's helper
  becomes ready (after first-time setup or a reconnect).
- **Drag-and-drop payloads into a playlist.** Drag one or more payloads onto the
  Playlists panel to build a playlist in one gesture (or "From files…").
- **Unified file/folder picker.** The Upload screen's two buttons are now one
  **Browse** menu, and drag-drop auto-detects file vs folder.
- **Game art on the upload queue.** Queue rows now show the game's cover, name,
  and PS4/PS5 badge so you can tell what's what at a glance.
- **Faster Library.** Big libraries no longer lag on search and refresh —
  rows are memoized, the list reconciles in place, search is debounced, and
  thumbnails load as you scroll.
- Plus a sweep of bug, logging, performance, and cross-platform fixes from a
  multi-perspective review (kill/playlist failures now leave a trace in bug
  reports, drag-drop hit-testing, process-poll throttling, and more).

## 3.2.9

- **Shutdown now fully powers off the PS5.** The Shutdown button was putting the
  console into rest mode instead of turning it off, because the API it used
  respects the system's rest-mode setting. It now does a true power-off (use the
  separate **Rest mode** button if you want standby). Hardware-verified.

## 3.2.8

- **More large-Text-size layout fixes on the Payloads screen.** 3.2.6 fixed the
  send form at the top; this fixes the rest. The payload catalog cards no
  longer crush a payload's name to one letter per line, and playlist steps keep
  the payload path visible instead of hiding it behind the controls. Verified
  across a range of screen sizes and text scales.

## 3.2.7

- **Download files from mounted disc images.** Browsing a ShadowMount+ game
  under `/mnt/shadowmnt` worked, but downloading a file from it (e.g.
  `eboot.bin`) failed with "path not allowed". You can now read/download files
  from mounted disc images.
- **Preview SDR screenshots.** Consoles that save screenshots as `.jpg` (rather
  than HDR `.jxr`) hit a "not a JPEG XR file" error on Preview. `.jpg`/`.png`
  screenshots now preview and save directly; only HDR `.jxr` shots are
  converted.

## 3.2.6

- **Properly fixed the Payloads "Send file" screen at large Text sizes.** The
  previous attempt only fixed part of it — on some tablets (especially with a
  bigger Text-size setting) the form still collapsed and the labels wrapped one
  letter per line. The whole screen now reflows correctly at any text size and
  window width. Verified across a range of phone, tablet, and desktop sizes.

## 3.2.5

A stability + bug-fix release from a round of user reports.

**The helper is much better behaved.**

- It no longer risks interfering with your *other* homebrew. A safety bug
  could, after a reboot, target an unrelated tool you'd autoloaded (a cheat
  loader, nanoDNS) when clearing a stale instance — it now only ever touches
  an instance from the current boot session.
- "Helper keeps dropping randomly" is fixed: the app no longer flips to
  "Helper isn't running" on a single missed status check, and the helper
  itself survives transient network hiccups instead of exiting.
- Spurious "Delete failed" errors after installing a package (when
  auto-delete is on) are gone — a file that's already gone or still briefly
  in use is no longer treated as a hard error.

**UI fixes.**

- Deleting a PS5 in Manage PS5s now shows the confirmation dialog on top
  instead of just dimming the background.
- Larger Text-size settings no longer break the Payloads send screen
  layout.
- The Library no longer flickers a wall of placeholder rows during a scan.
- Installed Apps gained an **Open folder** button (for homebrew/disc titles)
  that jumps to the app's folder in the File System browser.

**Payloads.**

- Added **CheatRunner** to the catalog.
- Added a **search box** to the payload catalog.
- A payload source that's temporarily unreachable now shows a quiet note
  instead of a loud error, so it no longer looks broken or removed.

## 3.2.4

A big polish release built from real-user feedback and a full UI/UX pass.

**Fewer reasons to restart your PS5.** If the helper gets stuck, the app now
detects and force-clears the previous instance on its own before sending the
new one — so you usually don't have to reboot the console or kill it by hand
anymore. Verified on FW 5.10 and 9.60.

**Packages & USB.**

- Install a `.pkg` straight from the **File System** browser — right where the
  file sits, no detour through the scan.
- PS4 packages now show up reliably with the correct PS4/PS5 badge, and
  scanning USB/external drives for packages is much faster.
- The "copying to your PS5" notice now explains it's a temporary stage that's
  removed after install, so it won't fill up your SSD.

**Save data & screens.**

- Save data shows the **game's name** next to its ID (e.g. "Saros
  (PPSA07631)"), and missing save thumbnails now fall back to cover art.
- Unplugging a USB drive in the File System browser drops you back to `/data`
  instead of throwing an error.
- Installed Apps no longer flashes a "needs ShadowMount+" warning on load, and
  Play is clearly disabled (not just failing) for disc games that need it.

**A clearer, more consistent app.**

- **Payloads** moved next to **Connection** in the sidebar — sending the helper
  is a setup step, so it now lives with "get started".
- Added an **automatic update check** (on by default) that notifies you when a
  new version is out.
- Tidier loading states, consistent modals, clearer labels and terminology, a
  redesigned bottom status bar, and accessibility improvements throughout — in
  all 18 languages.

## 3.2.3

- **Clearer status bar.** The strip along the bottom used to cram five
  things together (`engine | Fat | v3.2.0 | PS5 | FW 5.10`), which was hard
  to read. It now shows just the two things that matter at a glance — the
  **Engine** and your **PS5** (with its firmware) — neatly separated. The
  console name dims when it's not connected, and the helper version and
  kernel details are tucked into the tooltip when you hover.

## 3.2.2

- **New look, and a new theme.** The two main themes are now **PS5 Dark** and
  **PS5 Light** — coloured after the console's own black- and white-plastic
  panels with the PlayStation-blue accent. And there's a brand-new **Rose**
  theme: warm, bright, and soft, built around a bold rose accent. Pick any of
  them (plus OLED) in **Settings → Appearance**.
- **Tidier releases.** The engine now carries the same version number as the
  app, and each release also ships the engine binary on its own for every
  platform.

## 3.2.1

- **Save Data now shows game cover art.** The save thumbnails were trying to
  read each save's own icon, which lives inside a sealed container and always
  failed — so every save showed a plain icon and the log filled with warnings.
  They now use the game's cover, which actually displays.
- **No more harmless-but-noisy errors in the log.** Opening Install Package
  logged a string of `502` errors while checking for update/DLC folders that
  didn't exist yet. The check is now done without those false errors.

## 3.2.0

- **Install games straight from a USB or external drive.** Plug a drive with
  `.pkg` files into the PS5 and install them right from the app — no uploading
  from your computer first. A new **External Packages** section finds them
  automatically. (The app copies the package onto the console before installing,
  because the PS5's installer can't read a USB drive directly.)
- **PS4 / PS5 badges on every package**, so you can tell at a glance which
  console a `.pkg` is for.
- **Installs are honest about whether they actually worked.** The app now
  confirms the game really landed on the console before saying "installed" — and
  reworked which install method it uses under the hood, so packages that used to
  quietly fail now install correctly. (Verified end-to-end on real hardware
  across multiple firmwares.)
- **Base games and their updates no longer clash.** A game and its update share
  the same ID; the app keeps them apart so one never overwrites the other, and
  warns you if you try to install an update before its base game is on the
  console.
- **A friendlier, more visual interface.** Preview a screenshot before
  downloading it, search inside a single game instead of the whole console, see
  all your drives at the top of the File System tab, get a heads-up banner when a
  new version is out, and reach quick actions (Open folder, Copy details) from a
  menu on each package. Game artwork shows up in more places.
- **Adjust the text size.** If the app renders too large (common on some Android
  phones with a big display-size setting), Settings now has a Text size control
  that resizes the whole interface.
- **Now officially covers firmware up to 12.70.**

## 3.1.6

- **Renaming a profile sticks now.** Renaming a console user updated the name
  everywhere except the PS5 home screen, which kept showing the old name after a
  reboot — even though the console still said the new name was "already taken."
  The rename now also updates the home-screen display name so the two match, and
  your avatar is left untouched. (Verified end-to-end on a real console.)
- **No more shutdown moving a game from an external SSD to internal.** A
  cross-volume move falls back to a local copy, and the faster copy path added
  in 3.1.4 could flood the kernel with un-flushed writes during a multi-GB game
  and panic the console. The copy now flushes on a fixed cadence so that backlog
  can't build up. (Stress-tested with a sustained multi-GB copy on real
  hardware.)
- **Package installs stop claiming success when they didn't.** On FW 12.x an
  install could be accepted and then fail in the background, leaving a
  "corrupted" tile while the app showed "Installed." The app now waits for the
  install to actually finish and surfaces a real error — pointing you to the
  PS5's own Package Installer — instead of a false success. The Cancel button
  can no longer interrupt a real install mid-way either.
- **Android: bug reports and exports save again.** Saving a bug report, crash
  bundle, settings / search / stats / log export, or save-data backup failed on
  Android ("No such file or directory"). They now land in your Downloads folder,
  each with its own filename.
- **A pile of reliability and UI fixes.** Switching consoles no longer leaves
  stale info on the Profile, Hardware, Screenshots, or Upload screens; failed
  exports now tell you instead of silently doing nothing; the USB autoloader no
  longer gets stuck after a failed scan; the command palette scrolls to keep the
  selected item in view; `.rar` uploads land in a correctly named folder; the
  engine re-extracts itself if its file goes missing; and the settings backup
  now captures every preference (and stops losing your play-time on restore).
  Plus internal fixes to large uploads, the transfer engine, and broader
  translation coverage.

---

## 3.1.5

- **Stop a running upload, for real.** The Stop button on the Activity page now
  actually halts the transfer on the spot — before, it only stopped *showing*
  the upload while the PS5 kept receiving it in the background. Works for both
  one-off uploads and queued ones; whatever already landed stays on the PS5, so
  you can resume later if you want. (Verified on a real console mid-transfer.)

---

## 3.1.4

- **Game details open on Android now.** Tapping a game's **Details** in the
  Library did nothing on phones — the detail popup was being positioned to a
  scrolled area instead of the screen, so it opened off-screen. It now opens
  centered, every time.
- Internal: green CI again (code formatting, translation coverage, and the
  RAR test fixtures) — no user-facing change.

---

## 3.1.3

- **Each PS5 keeps its own upload, even when you switch.** Pick a file and set
  up an upload for one console, hop to another console in the top tabs, then
  hop back — your picked file, password, and options are right where you left
  them. The cut/copy clipboard and your place in the File System are remembered
  per console too, so switching never loses what you were doing.
- **No more accidental reloads.** Right-clicking no longer pops the WebView's
  Back / Reload / Inspect menu (which could restart the app mid-upload and make
  it look like everything stopped), and the reload shortcuts are blocked in the
  packaged app. Right-click still works inside text fields for copy/paste.

---

## 3.1.2

- **Windows packaging fixed.** The new `.rar` support pulled in a library that
  didn't link cleanly on Windows, which broke the 3.1.0 / 3.1.1 Windows builds
  (so those never shipped). Fixed — this release packages on every platform
  again, and includes everything from 3.1.0 and 3.1.1 below.

---

## 3.1.1

Fixes and polish from your feedback.

- **Community payloads download again.** Fixed the download for NP Fake
  Sign-in and the other earthonion payloads (their host moved), and **added
  Ghostpad** (virtual controller / input redirection) to the payload list.
- **Library details open on your phone.** On Android the game row's
  Play / Details / ⋯ buttons could be pushed off the edge of the screen and
  were untappable — the row now wraps so every action is reachable.
- **A clearer Activity log.** Clearing history no longer removes uploads that
  are still running; you can **delete individual entries**; each row has a
  **View** button that shows the full details; and an upload that hasn't
  started moving bytes now says **"Preparing…"** (compressed `.rar`/`.7z`
  archives are extracted on your PC first, so there's a wait before the speed
  appears) instead of a confusing "Uploading…" with no progress.
- **Upload button tidy-up.** The button just says **Upload** again; which PS5
  you're sending to is shown as a small chip next to it, so a long console
  name no longer stretches the button.
- **Profile changes tell you to reboot.** After changing a console avatar or a
  local username, a note reminds you the change only shows on the PS5 after a
  restart.

---

## 3.1.0

More formats, faster big uploads, and packages that install themselves.

- **Upload `.7z` and `.rar` archives, not just `.zip`.** Pick a compressed
  game dump and it's decompressed on your PC and streamed in — it lands
  already extracted on the PS5. RAR also handles **multi-part sets** (just
  pick the first part — the rest are found automatically) and
  **password-protected** archives. (RAR is desktop-only; Android does `.zip`
  and `.7z`.)
- **Packages install themselves — in your queue.** Add a `.pkg` on the Upload
  tab and it uploads, installs, and (optionally) deletes the staged copy as
  one step, right alongside your folders, images, and archives. Add it to the
  queue and it ends up playable — no separate trip to the Install Package tab
  (which is still there as a package manager).
- **Big uploads stay fast the whole way.** Fixed a slowdown where a large
  single file — like a 150 GB disk image — would crawl to a few KB/s partway
  through and only recover if you restarted. It now holds full speed from
  start to finish.
- **Every PS5 keeps its own everything.** Switch consoles and the library,
  uploads, saves, and installed-apps lists all switch with you — no leftover
  data from the other console, and no risk of a Restore or Uninstall landing
  on the wrong PS5 mid-switch.
- **Profile pictures + rename local users.** Set a console avatar and rename
  offline accounts right from the Profile screen.
- **Text fits everywhere.** Fixed text wrapping one letter per line in tight
  or large-font layouts (the Register overflow menu and others) — on every OS.
- **Steadier under the hood.** Fixed a payload crash during profile reads and
  tightened a few rough edges.

---

## 3.0.0

The big redesign. Same tool, sharper look, fewer steps, fewer surprises.

- **A fresh look across the whole app.** Deeper, richer dark theme with real
  depth and a more vivid PlayStation blue; refreshed light and OLED themes;
  smooth animations on dialogs, menus, and page changes (and the four
  PlayStation shapes as the new loading mark). Every button and field now
  shows a visible focus ring for keyboard users, and motion respects your
  system's reduced-motion setting.
- **Keep your PS5 awake — your choice.** Settings → Upload now has a
  three-way "Keep the PS5 awake" option: **Off**, **During transfers**
  (default, protects long uploads from rest mode), or **Always while
  connected** — your PS5 never auto-enters rest mode while the app is open.
  Putting the console to rest manually always still works. A small ⚡ in the
  status bar shows when always-on is active. Verified on real consoles.
- **Games go straight to your home screen.** Upload a game folder and it's
  registered on the PS5 automatically when the transfer finishes (works from
  the queue too) — no more hunting for "Register" in the Library. You can
  turn this off per upload, and the menu is now called **"Add to home
  screen"** so it says what it does.
- **No more dead ends.** Every screen now tells you exactly what's wrong and
  how to fix it — no console set up yet, console offline, helper not
  running, or the engine not responding — with a button that takes you
  there. New installs land directly on the Connection screen.
- **Errors can't slip past you anymore.** When a delete, move, mount, rename
  or download fails, the error now also lands in the notification inbox —
  navigating away no longer makes it vanish.
- **Quality of life.** Loading screens show smooth placeholders instead of
  blank space; huge game libraries render instantly ("Show all" expands the
  rest); scheduled power-ticks now always target the console you created
  them for; Dashboard moved next to Connection where you'd look for it.
- **Fully translated.** Every string in the app is now translated in all
  18 languages — no English fallbacks left anywhere.

---

## 2.30.0

- **Running several PS5s hard at once won't crash a console.** When work
  overlapped on one console — reading its live temperature while a package was
  installing, or registering a game on one PS5 while another was mid-install —
  the helper's low-level kernel operations could collide and, in the worst
  case, black-screen and restart the console. Those operations are now
  serialized internally, so heavy concurrent use across consoles stays stable.
  Verified on real hardware (PS5 and PS5 Pro) under a thousand-plus
  concurrent operations.
- **One busy console no longer freezes the app for the others.** A slow or
  very large upload (especially from a network drive) used to be able to stall
  the desktop engine for *every* connected console at once. Each console's
  work is now handled independently, so a busy PS5 keeps to itself.
- **The Upload screen always shows the right console.** Switching tabs mid-
  upload now correctly shows each console's own progress, and the "stay awake"
  and folder-comparison helpers follow the console you're actually uploading
  to. Switching consoles also clears the previous console's game list
  immediately, so an action can never land on the wrong PS5.

---

## 2.29.1

- **Connecting two (or more) PS5s at once is stable again.** A status-checking
  bug introduced in 2.29.0 could flood the consoles with rapid back-to-back
  reconnect checks, which knocked the second console offline a few seconds
  after it connected. The app now checks each console at a steady pace, so
  every PS5 stays connected.
- **Recover a crash log after the helper drops.** In the bug report, when the
  helper is disconnected you can now reconnect it in one tap to grab the PS5
  kernel log — it survives the helper crash, so you can still capture what went
  wrong (just don't reboot the console first, which clears it).

---

## 2.29.0

- **Run multiple PS5s at the same time.** When you have more than one console,
  a tab strip appears at the top — one tab per PS5, each with its own live
  status. Switch between them freely; every console keeps its own uploads and
  package installs running in parallel in the background. Installing on one
  console no longer makes the others wait — start work on each independently.
- **Big uploads are reliable again.** Fixed a crash that could drop the
  connection partway through very large uploads (you'd have had to re-send the
  helper). On top of that, if a transfer ever drops, the app now automatically
  re-deploys the helper and resumes from where it left off instead of giving up.

---

## 2.28.3

- **The app fits your phone screen now.** Pages that used to run off the right
  edge on Android (Settings and others) always fit the screen width — only
  vertical scrolling, the way it should be. This holds in every language,
  including the longer ones.
- **Easier to tap.** Buttons and controls are now finger-sized (44px) on phones
  and tablets, while desktop keeps its compact layout.

---

## 2.28.2

- Docs only: added a troubleshooting entry for the Hardware screen dropping
  the connection (the actual fix shipped in 2.28.1) with the recovery steps.
  No app changes from 2.28.1.

---

## 2.28.1

- **Fixes a crash that could drop the PS5 connection when opening the Hardware
  screen.** On some console/loader combinations a single misbehaving hardware
  reading could take the whole helper down (you'd see "connection refused" and
  have to re-send the payload). Now a reading that misbehaves on a given
  firmware just shows as "unavailable" for that one field instead of dropping
  the connection — and the helper logs exactly which reading misbehaved so
  the cause is easy to pin down.
- Built against the latest PS5 payload SDK.

---

## 2.28.0

- **Fixes games that install but won't start on newer firmware.** Installing a
  package now always prefers the methods that produce a *launchable* game and
  only falls back to the riskier last-resort method when everything else
  fails. If it ever has to use that fallback, the app clearly warns you — and
  points you to the PS5's own Package Installer — instead of showing a
  misleading "Installed." This targets the "can't start the game or app" error
  some people hit on recent firmware.
- **Smoother upload speed readout.** Speed and ETA now update continuously as
  data goes out instead of jumping once per chunk, so they no longer sawtooth
  on Wi-Fi or slower links.
- Plus better firmware diagnostics and more install-path test coverage.

---

## 2.27.1

- Routine dependency updates (frontend libraries and a networking crate). No
  user-facing changes — just keeping things current and secure.

---

## 2.27.0

- **Report a bug without leaving the app.** A new **Bug Report** page (under
  Diagnostics) lets you describe what happened, attach screenshots, and bundle
  the app's logs plus a snapshot of your PS5 into a single `.zip` to post on
  Discord — so issues come with enough detail to actually fix. Your PS5's IP and
  serial are stripped by default.
- **Capture screenshots in one click.** A camera button in the status bar grabs
  whatever screen you're on and saves it; on the Bug Report page you pick which
  captures to include. Works on every platform.
- **Much better logging.** The app now keeps a detailed log on disk (with an
  adjustable detail level) that survives a crash, and it captures engine and
  helper errors and crashes — so a bug report contains what actually went wrong
  instead of a blank. You choose how much of the recent log to package.
- **The helper no longer leaves a stuck copy behind.** Resending the helper
  after it crashed could spawn a second copy that couldn't be removed; it now
  cleans up the old one on startup. (If one is ever truly stuck, a quick PS5
  reboot clears it.)
- Plus connection-stability fixes for the Hardware screen and assorted polish.

---

## 2.26.1

- **Turn PS5 screenshots into normal pictures.** The PS5 saves screenshots as
  HDR `.jxr` files that most photo viewers and browsers can't open. The
  Screenshots screen now has a **Convert** button on each shot that turns it into
  a regular `.png` you can open and share anywhere — no extra apps or extensions
  needed. (Desktop app.)
- **Fan speed in the Hardware screen.** "Read sensors" now also shows the
  console's current fan duty, next to temperatures and CPU clock.
- **Reading sensors is safe on every console.** The one sensor that could freeze
  the helper on some firmware (power draw) is no longer read, so live readings
  can never knock the connection offline — on any PS5 model or firmware.
- Screenshots that used to appear twice (the full-size shot and its thumbnail)
  now show as a single row.
- Build and packaging fixes so the app keeps building cleanly on Windows, macOS,
  and Linux.

---

## 2.25.5

- **System → Hardware no longer drops the connection.** On some consoles,
  opening the Hardware screen (or the dashboard's live readouts) could break the
  connection to the helper and force you to send it again. Live temperatures and
  CPU clock are now read in a way that's safe on every firmware, so the
  connection stays put. (Power draw now shows as "unavailable" on firmware where
  reading it isn't safe — a deliberate trade so it can never knock the helper
  offline.)
- **File search can't run the console out of memory.** Indexing a very large
  drive is now capped, so a big game library won't exhaust the helper's memory;
  the results just show as partial when the limit is hit.
- Hardened the helper against malformed and out-of-range values so it stays
  stable under unusual conditions.

---

## 2.25.4

- **Plays nice with ShadowMount+.** If you run ShadowMount+, ps5upload now hands
  games off to it instead of doing its own thing and clashing. Uploading or
  registering a game adds it to ShadowMount+'s install list and lets SMP do the
  mounting + registering — so trophies, disc images, and nested PFS all work,
  and the two tools stop fighting over the same title. Falls back to ps5upload's
  own mount/register when ShadowMount+ isn't running.
- **nanoDNS support.** Added the nanoDNS payload — a tiny on-console DNS server
  that blocks PlayStation Network / update domains (and can redirect any domain)
  — to the Payloads list, plus a new **nanoDNS** screen to edit its config and a
  guide for pointing your PS5's DNS at it.
- **More disc-image formats.** You can now upload `.ffpfs` and `.ffpfsc` (PFS and
  compressed/nested PFS) images, alongside the existing `.exfat` and `.ffpkg`.
- Refreshed the payload info for ShadowMount+ and kstuff-lite (firmware 12.xx),
  and fixed a couple of small Installed Apps glitches.

---

## 2.25.3

- **A much better Installed Apps screen.** Every game now has a **Play** button
  to launch it straight from the app. Titles are grouped by what they are —
  installed games & apps, disc images, folder homebrew, and system — and each is
  tagged **PS4** or **PS5**. The screen also warns you when **kstuff** isn't
  active (so you know games won't launch) and when a disc image needs
  **ShadowMount+**, with a one-tap button to send it to the console.

---

## 2.25.2

- **Installed packages actually launch now.** Installing a `.pkg` from the
  Install Package page registered the game's icon but never laid down its
  actual content — so the tile appeared on the PS5 but launching it failed with
  "can't start the game or app." Installs now go through the path that installs
  the full game content (the same approach etaHEN uses), so the game launches.

---

## 2.25.1

- **Installed games actually launch now.** Some games installed without any
  error but wouldn't start ("can't start the game or app"). They now install
  with their full content and launch normally.
- **Interrupted uploads resume for real.** If a big folder upload drops partway
  through, it keeps everything that already made it across and continues from
  there — instead of re-sending the whole game every time — and it holds onto
  that progress across several interruptions in a row.
- **Manage several PS5s at once.** The upload queue is now grouped by console:
  each PS5 has its own Start and Stop, consoles upload in parallel, and you can
  reorder one console's games while another console is still uploading.
- **Translated into every supported language.**

---

## 2.25.0

- **More reliable uploads by default.** Sending several streams at once could
  crash the PS5 helper mid-upload on some consoles, so uploads now use a single
  stream by default. You can still turn streams up in Settings — with a heads-up
  that it's less stable — but the single-stream path is the rock-solid one.
- **Uploads recover on their own.** If a transfer drops mid-upload — most often
  because the helper crashed — the app now re-sends the helper and picks up
  exactly where it left off, retrying a few times before giving up. Real
  problems like the PS5 running out of space still stop right away. On by
  default.
- **Keeps your PS5 awake while uploading.** Long uploads used to die when the
  console slipped into rest mode. The app now keeps the PS5 awake for the
  duration of a transfer. On by default.
- **Finds your PS5 across every network.** "Find PS5s on the network" now scans
  all of your computer's connections at once (Ethernet + Wi-Fi), so it locates
  your console even when it's not on your computer's main network.
- **Reset everything.** A new button under Settings → Data & reset wipes all
  local app data — settings, saved consoles, history, and caches — and starts
  fresh. Nothing on your PS5 is touched.
- **Cleaner, easier-to-read interface.** Bigger text and logo, a tidier layout
  that wastes less space, and friendlier "nothing here yet" screens. Settings is
  reorganized into clear sections, and the upload-speed limit is now a proper
  control there.

---

## 2.24.0

- **Much faster downloads (backup console → PC).** Saving a game to your PC
  used to reconnect for every small chunk; it now keeps one connection open
  and reads ahead, and pulls folders over several connections at once. A big
  single file roughly doubled to about full network speed, and folder backups
  are much quicker. It also resumes from where it left off if the connection
  drops. Nothing new to send to your PS5 — it's all in the app.
- **See which console each upload is for.** Every item in the upload and
  install queues now shows its target console, so a mixed queue isn't a
  guessing game.
- **Upload to several consoles at once (optional).** New setting under
  Settings → Upload to send queued games to different consoles in parallel
  instead of one console at a time. Off by default.
- **Pick a payload version — and downgrade.** The Payloads catalog now lets
  you choose any past release of a payload, not just the latest, and flags
  pre-release builds as possibly unstable. Handy for rolling back when a fresh
  build misbehaves.
- **Adding a game's update no longer clashes with its base.** A base game and
  its update share an ID, which made the app treat an added update as the base
  again. They're now kept separate, with clear "Update" / "DLC" labels.
- **Tidy up the staged-package list.** New "Clear finished" and "Clear all"
  buttons, plus an optional "auto-delete after install."

---

## 2.23.11

- **Uploads no longer abort with a "buffer space" error on Windows.** During a
  fast multi-stream upload, Windows could briefly run out of network buffers
  (`os error 10055`), and the app treated that as fatal — stopping the whole
  transfer partway through. It now waits a moment and retries the connection
  automatically, so a temporary hiccup no longer ends your upload. Engine-only
  fix; no need to re-send the payload to your PS5.

---

## 2.23.10

- **Faster uploads from network drives / NAS.** The app now reads the next chunk
  from your source while the current one is still going out over the network, so
  the connection no longer sits idle waiting on a slow disk read. No change if
  your files are on a fast local drive (there the PS5 or the network is the
  limit) — this specifically helps slow or network sources.
- **More stable uploads of huge multi-file folders.** Removed per-file memory
  churn in the PS5 payload's write path that could fragment the console's memory
  on very large folders — especially now that uploads use several parallel
  streams. (Requires sending the updated payload to your PS5.)

---

## 2.23.9

- **Faster uploads — large folders now send over several connections at once.**
  A single upload stream is limited by the console's per-connection write speed
  (around 40 MB/s on non-Pro PS5s — a single-thread limit, not your network or
  SSD). Uploads now split a folder's files across up to 4 parallel streams,
  which adds up to a much higher total. Measured on wired gigabit: ~1.7× faster
  on a PS5 (fat) and ~1.4× on a PS5 Pro (which was already close to the network
  limit). It turns on automatically and falls back to a single stream on older
  payloads; you can change it under Settings → Upload → Parallel upload streams.
  (Tiny-file folders are limited by the console's filesystem, not bandwidth, so
  they don't speed up — this helps games with real-sized files.)

---

## 2.23.8

- **Uploading from a network drive no longer crashes the helper.** When the
  source folder lives on a network share (SMB/UNC), scanning it can take
  minutes. The app was sometimes running several of those scans at once — a
  background "diff vs PS5" preview on top of your actual upload — which fought
  over the same share and could take the helper down on big games. Now only one
  scan runs at a time, and the preview pauses while a transfer is in progress
  and resumes when it's done.

---

## 2.23.7

- **Queued uploads are more reliable.** Before running a queue, the app now
  makes sure your PS5 is on the matching helper, and it paces jobs so a long
  list no longer crashes the helper partway through.
- **Install and upload now take turns instead of colliding.** Starting an
  install (or adding a .pkg) while something is uploading no longer drops the
  connection — it waits in line and starts automatically when the current
  transfer finishes, and the screen tells you it's waiting (with a Cancel
  option). The transfer also no longer needlessly blips during a package
  install.

---

## 2.23.6

- **No more phantom crash reports during uploads.** A harmless browser-engine
  hiccup that could happen as a screen changed mid-upload was being recorded
  as a crash. It's now ignored, and genuine errors capture more detail to make
  them easier to track down.

---

## 2.23.5

- **Android build restored.** The 2.23.4 window-placement fix didn't compile
  for Android, so that release shipped desktop-only (no APK). The same fixes
  — a centered, on-screen window and a working "Open folder" — are back on
  Android too.
- **Logs: Copy and Download work again.** "Copy" now reliably puts the log on
  your clipboard, and both buttons confirm when they're done.
- **Removed the experimental Date & Time settings** from the Hardware screen.
- **Fewer phantom crash reports.** A harmless internal hiccup is no longer
  recorded as a crash.

---

## 2.23.4

- **Fix: the crash-report "Open folder" and "Clear" buttons (Settings →
  Diagnostics) no longer stay greyed out.** "Open folder" now reliably opens
  the reports folder — and if your OS blocks that (or on Android, where the
  folder is private to the app), it shows you the exact path instead. The
  folder location is now also shown under the report count.

---

## 2.23.3

- **Big uploads are much more stable.** Large game folders (thousands of
  files) no longer crash the on-PS5 helper mid-upload — the app now talks to
  the console one directory scan at a time instead of flooding it. Validated
  on real hardware across folder, `.zip`, and `.exfat` disc-image uploads.
- **Unusual file/folder names work now.** Names containing characters like
  `}`, or very long paths, no longer get the whole upload rejected with a
  cryptic error. (Re-send the payload to your PS5 to pick this up —
  Connection → Send payload.)
- **One-click crash reporting.** If something goes wrong, the app now saves a
  detailed report automatically and gives you a **"Report this crash"**
  button (also in Settings → Diagnostics) that packages everything into a
  `.zip` and opens our Discord so you can post it.
- **Fully translated.** All 18 languages now cover every screen, including
  error and troubleshooting messages (previously English-only).
- Smaller fixes: an `etaHEN/games` upload destination preset, and "Save logs"
  in the Logs tab works again.

---

## 2.22.0

- **Android: the screen now stays on during transfers.** While an upload,
  download, or install is running, your phone or tablet no longer dims or
  sleeps and drops the transfer mid-stream. Settings → Keep Awake also works
  on Android now, to hold the screen on while the app is open. Desktop
  already did this; Android has caught up. Verified on-device (Pixel 9,
  Android 16).

---

## 2.21.3

- **Fix: a playlist step ran off the edge on phones.** In Payloads →
  Playlists, a step's path plus its per-step IP / port / sleep fields and
  buttons were packed into one row that overflowed narrow screens. The
  controls now wrap neatly underneath on small screens; desktop is
  unchanged.

---

## 2.21.2

- **Fix: the Android app still crashed on launch — now fixed for real.** It
  opened, then closed after about a second. The 2.21.1 fix turned out to
  address an unrelated issue; the actual cause was a native crash in the
  startup file-access permission check, which read an Android system
  context that Tauri doesn't set up. It now obtains that context itself.
  Verified working on-device (Pixel 9, Android 16). The app is fully
  self-contained — it runs its own engine on the phone and talks to your
  PS5 over Wi-Fi; it never needs the computer.

---

## 2.21.1

- **Fix: the Android app crashed on launch.** The 2.21.0 build shipped
  modern JavaScript that older Android System WebViews couldn't run, so
  the app opened to the first screen and then closed. Builds now target a
  broadly-compatible JavaScript level, so it launches on those devices.
  (Desktop was unaffected. The Android app is fully self-contained — it
  runs its own engine on-device and talks to your PS5 over Wi-Fi; it
  never needs the computer.)

---

## 2.21.0

- **Install Package is now a package library.** Upload a `.pkg` to your
  PS5 once and it stays there. The screen shows all your uploaded
  packages with cover art and size, and you can **Install**, **Reinstall**,
  or **Delete** any of them in a click — no need to re-upload to install
  again. There's no install-method picker anymore: installs go through the
  DPI daemon, the most reliable path on current firmware.
- **Under-the-hood updates.** Refreshed the desktop and UI toolchain
  (React, Tauri, build tooling) and dependencies to their current releases.

---

## 2.20.2

- **Package install now works on firmware 9.60+.** Installing a `.pkg`
  no longer fails (or knocks the payload offline) on consoles where
  Sony's installer rejects the streamed install. Both **Stream** and
  **Upload & install** complete reliably, and the app falls back from
  Stream to Upload automatically when needed.
- **New: Installed Apps screen.** See everything installed on your PS5
  with cover art, grouped by how it got there — installed from a
  package vs. mounted/registered from a folder or disc image — and
  uninstall any title in a click. Works on desktop and Android.
- **Android: SD cards and USB-OTG drives now show up.** The in-app file
  picker lists removable storage alongside internal storage, so you can
  upload a game straight from a memory card or a plugged-in USB drive.

---

## 2.20.1

- **Android setup is clearer.** The app now proactively explains the
  required All files access permission, rechecks it after returning from
  Android settings, and documents the common Android upload/install issues
  in the FAQ.
- **Engine startup diagnostics are easier to find.** If the local engine
  fails to start, PS5 Upload now records the reason in the app log and
  status tooltip instead of only showing a red engine dot.
- **Release and docs cleanup.** Updated stale release workflow references,
  marked the Android feasibility doc as historical, and tightened ignored
  Android build artifacts.

---

## 2.20.0

- **Android: pick game folders and files from your phone.** "Choose
  folder" and "Choose file" now work on Android — browse your phone's
  storage in an in-app file browser and upload a game folder or a `.zip`
  straight to the PS5, with no copying. PS5 Upload asks once for
  permission to read your files. The same picker fixes file/folder
  selection across the whole app on Android: Library downloads, Install
  Package, Save-data restore, Screenshots, Payloads, and File System.
  (Desktop is unchanged — it keeps using the native file dialogs.)

- **ShadowMount+ (and other zip-packaged payloads) now install.** The
  "Set up your PS5" one-click chain and the payload catalogue failed on
  ShadowMount+ with "downloaded asset is not an ELF" — newer ShadowMount+
  releases ship the payload inside a `.zip` instead of as a bare `.elf`.
  PS5 Upload now detects a zip-packaged payload, extracts the real `.elf`
  from inside it, and sends that. Affected the recommended chain on every
  platform (desktop and Android).

- **Android: your settings now survive app updates.** Earlier Android
  builds were each signed with a throwaway key, so installing a newer
  version meant uninstalling the old one first — which wiped all your
  PS5 Upload settings. Releases are now signed with a single stable key,
  so new versions install **in place** and your settings carry over,
  just like on the computer. (One-time step: uninstall the current build
  and install this one; every update after that keeps your settings.)
- **Settings included in device backup.** Your settings are now part of
  Android's backup/restore, so they can also come back after a full
  reinstall or a move to a new phone.

---

## 2.19.1

- **Android: fixed the engine showing red.** On the 2.19.0 Android build
  the app couldn't reach its own built-in engine, so the status bar
  showed **engine** in red and transfers couldn't start. Release Android
  builds block plain-HTTP loopback traffic by default; the app now
  permits it for its own local engine only (everything else stays
  blocked). Desktop builds were never affected.

---

## 2.19.0

The notifications-everywhere release — plus the first Android build.

- **System notifications.** PS5 Upload can now post to your computer's
  notification center (macOS, Windows, Linux — and the Android shade)
  when a transfer finishes or fails while the app is in the background,
  so you don't have to keep it in front of you. Toggle it under
  **Settings → Notifications**.
- **The PS5 tells you too.** Uploads now flash a "started" and a
  "complete" message on the console screen itself.
- **No more sleep-interrupted transfers.** Your computer is kept awake
  automatically for the duration of any upload, download, or install.
  The Settings switch is reworded to make clear it keeps the machine
  awake while PS5 Upload is open.
- **Android (preview).** The first experimental Android build — the same
  interface, with mobile-friendly navigation, safe-area handling, and our
  app icon. It connects to and manages your PS5 over Wi-Fi today; download
  the `.apk` from the release assets to try it. Treat it as early access.
- Reliability fixes across notifications and the release pipeline.

---

## 2.18.7

- **Choose where a `.zip` unpacks.** When you pick a `.zip`, the Upload
  screen now asks **"Where should the .zip unpack?"** with two options,
  each showing a live preview of where the files will land:
  - **Put everything in a new folder named after the zip** (the default,
    unchanged from before) — best for a plain `.zip` of loose game files.
  - **Extract the contents straight into the destination** — no wrapper
    folder. Use this when the `.zip` already contains the game's own
    folder (e.g. `CUSA12345/`), so it doesn't end up double-nested.
  The choice applies to one-shot uploads, queued uploads, and mirroring
  to other consoles alike.

---

## 2.18.6

A correctness + hardening sweep — 16 fixes found by a deep multi-agent
audit across the engine, desktop app, and tooling, plus a release-pipeline
fix so new versions publish automatically.

- **Releases now publish on their own.** Tagging a release used to need a
  separate manual "publish" step that was easy to forget (v2.18.5 was
  tagged but never published because of it). The publish workflow now
  fires automatically on the version tag, with the manual trigger kept as
  a fallback.
- **Hostile/corrupt `.zip` files can no longer crash the preview.** A
  crafted ZIP64 archive could make the engine abort while inspecting it
  (a huge declared entry count, or an overflowing offset). Both are now
  clamped/checked.
- **Windows: `.ffpkg` extraction can't escape the chosen folder.** A
  malicious archive entry named like `C:evil.exe` could write outside the
  destination on Windows. Such names are now rejected.
- **IPv6 PS5 addresses work.** The address helper used to mangle IPv6
  literals (e.g. `fe80::1` became `fe80`), breaking every call to an
  IPv6-only console. IPv6 is now handled and bracketed correctly.
- **Several panels stop silently failing when you paste an `ip:port`.**
  The Saves thumbnails, Library panels, Disk Usage, Dashboard sensors, and
  Kernel Log now go through the canonical address helper, so a host typed
  with a port no longer produces a broken `ip:port:9114`.
- **Destructive menu items show as red again.** Delete/uninstall items in
  overflow menus referenced a non-existent color and rendered like normal
  items.
- **Better error guidance for stuck PS5 downloads** (`0x80B22101`) — the
  specific "clear the notification and retry" message is shown instead of
  a generic one.
- **Windows update/metadata fetches are memory-bounded even without a
  Content-Length**, the cross-device "file is still on disk" message now
  shows on Windows, the keep-awake toggle can't abort config loading, the
  Linux USB-drive picker no longer lists fixed/network mounts, archive
  uploads correctly clear their resume marker, a window-listener leak is
  closed, and the i18n prune tool works against the current locale layout.

---

## 2.18.5

- **`.zip` uploads — much faster scanning, with a live progress count.**
  Dropping a `.zip` game dump used to show "Inspecting…" with no
  feedback while the app walked the archive. On big dumps (60 GB+
  with tens of thousands of files), this could take long enough to
  trip the same "engine request failed: error sending request" timeout
  that 2.18.4 fixed for folder deletes — the user saw a baffling error
  even though nothing was actually broken. 2.18.5 makes the scan
  **dramatically faster** (effectively instant once the disk is
  awake) and the few seconds you do wait now show a live "Scanning
  archive… N entries" counter so you can see the app is working.
- **Clearer errors when a `.zip` can't be read.** Bad path, wrong
  file type, unsupported compression method — the message now tells
  you exactly what's wrong instead of a generic "engine request
  failed."
- **Fix: the "extracted" size in the .zip preview was sometimes
  blank.** Archives made by some tools (notably `bsdtar` on macOS/Linux
  and anything using libarchive) set a flag that the old code couldn't
  read past, so the Upload card would show file count but a blank
  "extracted" size. Now you see the real expanded size for every
  archive.
- No payload changes needed; this is a desktop-app-only release.

---

## 2.18.4

- **Hotfix for "engine request failed" on huge folder deletes/copies.**
  Deleting (or copying / moving) a folder with tens of thousands of
  files on the PS5 surfaced as `engine request failed: error sending
  request for url (http://127.0.0.1:19113/api/ps5/fs/delete)` after
  about a minute — even though the operation was still running and
  eventually succeeded on the console. Cause: the desktop app's
  HTTP client to its own embedded engine had a 60-second ceiling
  that wasn't long enough for big-tree operations (the PS5 needs
  many minutes to walk and unlink tens of thousands of inodes).
  v2.18.4 raises the ceiling to one hour for the three destructive
  endpoints (`fs/delete`, `fs/copy`, `fs/move`), matching the
  engine's own internal deadline. Other endpoints stay at 60 s so
  a wedged sidecar still surfaces fast.
- No payload changes needed; this is a desktop-app-only fix.

---

## 2.18.3

- **Hotfix for multi-file upload crash.** A user reported uploading a
  large game folder (~46,000 files / ~13 GB on disk) with v2.18.2
  consistently fails partway through with "PS5 stopped responding."
  Reproduced and tracked down: the buffer-size bump shipped in
  v2.18.2 (per-shard I/O buffer from 4 MiB to 8 MiB on the PS5)
  doubled the per-shard malloc/free pressure on the multi-file
  upload path. On folders with many non-packed files (each one
  separately spawning the on-PS5 writer thread + 2 × 8 MiB buffer),
  the PS5's heap fragmented faster than its allocator could
  compact and the payload listener died after roughly 5 minutes.
- **What changed:** the buffer is reverted to 4 MiB. v2.18.2's
  measured speed change on the original PS5 was within run-to-run
  noise anyway, so reverting costs you nothing observable.
- **Reload the payload** after upgrading. The buffer-size lives in
  the on-PS5 ELF, not in the desktop app.

---

## 2.18.2

- **Background:** a user reported sustained single-file upload speed
  dropping from "100 MB/s before" to "30 MB/s now" after switching
  console hardware. We measured carefully and the cause is the
  underlying PS5 model's internal-SSD write speed — the original
  PS5 sustains ~30 MB/s on this code path; the PS5 Pro sustains
  meaningfully more. Both numbers are network-fast (the host pushes
  bytes into the PS5 at gigabit line rate) but disk-bound at the
  console end.
- **What changed in this release:** the transfer layer was tuned
  toward fewer/larger frames and bigger writer-thread buffers on the
  PS5 (64 MiB shards, 256 MiB inflight, 8 MiB writer slots, up from
  32/64/4 respectively). On a 10 GB `.exfat` over wired gigabit the
  measured wall-clock change was within run-to-run noise — small
  positive on the writer-wait portion, small negative on the
  per-shard overhead, net-neutral. The changes ship anyway because
  they are at worst neutral on every console class and are a
  better default for any future console with a slower destination.
- **What this release does NOT do:** there is no headline speed
  improvement for original-PS5 single-file uploads. We are honest
  about that. Going materially past the current ceiling needs
  payload-side changes deeper than buffer tuning (e.g. a third
  writer-slot for more producer headroom, or a different write
  pattern); we'll trial that in a future release.
- **Reload the payload** after upgrading to pick up the writer-slot
  change. An older payload still pairs fine with this app.

---

## 2.18.1

- **Picking a large game folder now shows what the app is doing.**
  When you pick a folder with tens of thousands of files, the
  Upload + "Add to queue" buttons used to grey out for ~30 seconds
  with no visible feedback. The source card now shows a clear
  "Scanning game folder…" banner with a hint explaining the wait,
  and hovering the disabled buttons tells you why.

---

## 2.18.0

- **The "Finalizing on PS5" wait now shows a live counter.** When a
  big folder upload reaches 100% and enters the PS5-side commit
  phase, the row used to sit on a generic "Finalizing on PS5"
  badge with no movement for 10–30 minutes. The PS5 now streams
  per-file progress back to the app while it's committing, so you
  see "Finalizing on PS5 — 12,400 / 84,216 files" climbing through
  the wait. Same signal in the Upload screen banner, the Upload
  Queue row, and the Activity tab.
- **Old payloads still work.** If you haven't pushed the new PS5
  payload yet, the upload still completes — you just see the same
  countless "Finalizing on PS5" badge as before. New payloads
  enable the counter automatically; nothing for users to toggle.

---

## 2.17.9

- Internal lab tool fix only. The `ps5upload-lab transfer-dir`
  command's post-commit verification step was calling the wrong
  PS5 port and showing a spurious error after every successful
  multi-file upload. The fix lives in the developer lab tool;
  no user-facing change.

---

## 2.17.8

- Internal cleanup pass after the 2.17.3 → 2.17.7 rapid-fire shipping.
  No user-visible behaviour changes — comments and inline rationale
  brought in line with reality (the commit-timeout helper now covers
  both single- and multi-file paths, the begin-timeout helper covers
  only multi-file as designed, throughput recording's known bias is
  documented, and the unused `MAX_AGE_MS` constant carries an explicit
  "reserved for P3" note so future readers don't try to wire it
  without context).

---

## 2.17.7

- **Uploading very large game folders no longer fails before the
  first file leaves your computer.** Folders with tens of thousands
  of files build a multi-megabyte "manifest" the PS5 has to receive
  and parse before any upload bytes are sent. On real hardware that
  parse step can take longer than the previous 30-second cap on a
  single request — meaning the upload failed instantly with
  "Resource temporarily unavailable." The app now waits up to 5
  minutes for the PS5 to acknowledge that manifest before giving
  up. Once acknowledged, normal transfer continues unchanged.
- Verified end-to-end on a 169,401-file / 162 GB game folder upload
  (Ghost of Yotei). Total upload time on the test rig: 3 h 13 m,
  with 19.5 min of post-100% PS5 commit (still well within the
  30-minute commit window we shipped in 2.17.5).

---

## 2.17.6

- **Upload screen now tells you how long a huge folder will take
  *before* you click Upload.** For folders with 10,000+ files (game
  dumps with tens of thousands of files inside), the source-info
  card now shows a transfer-time estimate, a PS5-commit-time
  estimate, and a total — so the multi-minute wait after the
  progress bar hits 100% isn't a surprise. A short hint tells you
  Resume mode will save an hour or more on re-uploads.
- **The estimate sharpens after your first upload to each PS5.** The
  app remembers the throughput from your last successful upload to
  each PS5 host and uses that figure next time, instead of a generic
  default.

---

## 2.17.5

- **Huge game-folder uploads no longer fail during the "Finalizing
  on PS5" wait.** A user-reported 85,000-file upload (Ghost of
  Yotei) reached 100%, sat on the new "Finalizing on PS5" indicator
  for 10–15 minutes, and then said "upload failed." The PS5 was
  actually still committing — the app just stopped waiting too
  early. The wait window is now long enough to outlast realistic
  worst-case commit times (30 min cap), with TCP keeping the dead-
  PS5 case detectable as before.

---

## 2.17.4

- **The "kstuff-lite (EchoStretch)" entry in the Payloads catalog
  now points at the correct repository.** It was previously linking
  to `EchoStretch/kstuff` (the full build); the entry's display name
  said "kstuff-lite" so the download was a different artifact than
  the one named. Now correctly resolves to
  `EchoStretch/kstuff-lite`.

---

## 2.17.3

- **Uploading a game folder with tens of thousands of files no
  longer freezes the app.** The per-file status list was trying to
  render every entry to the DOM on every poll tick — for a 50,000-
  file folder that pinned the main thread until the upload finished.
  The list now shows a moving window around the file currently
  being sent (current row + a slice of what's coming up + a slice of
  what just finished). Small folders are unchanged.

---

## 2.17.2

- **Rebooting your PS5 from the Hardware tab no longer shows a fake
  error.** The reboot itself always worked, but the app would
  display `power: EOF while parsing an object at line 1 column 28`
  right after, because the payload's "ok, rebooting now" reply was
  missing one byte. Fixed on both ends.

---

## 2.17.1

- **Big multi-file uploads no longer look frozen at 100%.** When all
  the bytes have reached the PS5 but it's still committing the file
  index — which can take many minutes for folders with tens of
  thousands of files — the row now shows a clear "Finalizing on PS5"
  badge with a hint telling you not to close the app. Same signal on
  the Upload screen, Upload Queue, and Activity tab.
- **Average-speed readouts on past uploads now show the actual number**
  instead of the literal text `{speed}`. Cosmetic bug across every
  language.

---

## 2.17.0

The v2.16.1 release pipeline didn't make it out — an ESLint failure on the
preflight banner string blocked the release run. 2.17.0 ships everything
that was meant for 2.16.1, plus the fix.

- **Game folders uploaded with ps5upload now launch first try.** Previously
  some folders would land on the PS5 but refuse to launch with `CE-107750-0`,
  while the same folder over FTP worked. Fixed at the source: files now land
  with the right permissions inherently, no after-the-fact step.
- **Live sensors auto-update again.** Temperatures, clock, and power readings
  refresh every 5 seconds on the Hardware screen via a new direct-read path
  that no longer briefly suspends the PS5 UI. The manual "Read sensors"
  button has been removed — it's automatic now.
- **"Find PS5s on the network" now finds yours even when your router
  suppresses mDNS.** A short LAN sweep runs as a fallback so the button never
  comes back empty just because of an unfriendly access point.
- **Hardware → PS5 system log.** Optional collapsible panel that shows the
  PS5's kernel log (the same data underlying `dmesg`). Handy when diagnosing
  homebrew payload issues without leaving the app.
- **Upload preflight no longer fails silently.** If the destination probe
  can't reach the PS5, you now see a clear inline error instead of the
  Upload button briefly spinning and then doing nothing.
- **Install Package: much more reliable.** The "Stream" install method now
  actually works (a v2.16.0 regression made every streaming install 500;
  fixed). System pkgs (NPXS-prefix) get a clear amber badge in the queue
  before you try them. Cancelling a multi-GB install asks for confirmation
  first. Failed installs show actionable messages for the common Sony
  error codes ("clear PS5 notifications", "out of space", etc.) instead of
  raw hex. The install panel also shows which path (in-process,
  ShellUI-RPC, or legacy BGFT) accepted your request, and the URL the PS5
  fetches the pkg from uses the pkg's canonical content-id as the filename.
- **First-install no longer looks frozen.** When you click Start on the
  install queue, a banner now shows "Preparing PS5 — checking / pushing
  payload…" while the desktop verifies / refreshes the payload, instead of
  ~30 seconds of silent waiting.
- **Stopping the queue mid-install no longer orphans the row.** Rows that
  were "running" when you click Stop now correctly go back to "pending" so
  the next Start picks them up.
- **Install preflight banner internals.** Refactored to carry
  `{ message, ownerRunId }` instead of embedding the run-id via a zero-width
  space in the banner string. Same user-visible behaviour; fixes the
  no-irregular-whitespace ESLint failure that blocked the v2.16.1 release
  pipeline.

---

## 2.16.0

- **Folder uploads survive network blips.** Multi-hundred-GB game folders that
  used to die on a single transient drop now reconnect and resume automatically
  — same resilience single-file uploads always had. A 7.3 GiB / 1007-file game
  folder now uploads cleanly in one shot on real hardware.
- **Much faster on big folders.** Multi-GB files inside a folder upload now
  preallocate disk space up front, removing the slow-down that hit long
  transfers mid-stream. ~40 MiB/s sustained over gigabit Ethernet in testing.
- **Honest error messages on upload failures.** When something actually goes
  wrong mid-transfer (drive full, drive disconnected, etc.) the app now tells
  you *why* instead of the generic "PS5 stopped responding."
- **macOS junk filtered out.** Uploads from external drives no longer ship
  `._*` AppleDouble metadata files into your PS5 game folders.
- **New docs: direct-Ethernet setup.** The FAQ now has a per-OS guide
  (Windows 11, macOS, Linux) for cabling the PS5 straight to your computer —
  the most stable + fastest upload path for huge games.

---

## 2.15.0

- **Folders with lots of tiny files now upload reliably.** Games like Astro
  Bot could fail with a `packed_unsupported` error — most often when resuming.
  Fixed.
- **Big uploads no longer die when your computer sleeps.** While an upload,
  download, or install is running, ps5upload keeps the computer awake
  automatically (macOS, Linux, Windows), then lets it sleep when idle. The
  Settings → Keep Awake toggle still works on its own for idle use.
- **Native Linux packages.** Releases now ship a `.deb` (Debian/Ubuntu) and
  `.rpm` (Fedora/RHEL/Bazzite) alongside the AppImage. These need a recent
  distro (glibc 2.39+: Ubuntu 24.04+, Debian 13+, Fedora 40+).
- **Safer folder uploads.** If the PS5 dropped mid-upload (rest mode or power
  loss), a transfer could finish "successfully" with a file that was secretly
  incomplete. It now fails clearly instead, and Resume re-sends only what's
  missing — even after a full power-off.
- **Docs:** how to keep long uploads alive (the PS5's own rest-mode timer)
  and when to use Resume.

---

## 2.14.0

A stability-focused release.

- **Fixed: the Hardware screen could power off the PS5.** Live temperature,
  clock, and power are now read on demand instead of auto-polling; system info,
  uptime, storage, and the clock stay live.
- **Linux white screen fixed** for the AppImage on double-click (Ubuntu /
  SteamOS / NVIDIA), not just when launched via `PS5Upload.sh`.
- **Diagnostics and exports now save** — the bug-report bundle and the
  Settings / Search / Stats exports were failing silently.
- **Send Payload** now enables the Send button on the first file pick.
- **Payload playlists** can be reordered, with a one-click "recently run" list.
- **Zip uploads** show a clear message for unsupported compression, and large
  archives no longer hang while being inspected.
- **Clearer install errors** when the desktop engine or PS5 helper isn't ready.
- Plus a batch of safety and correctness fixes under the hood, including two
  PS5-payload memory-safety fixes.

---

## 2.13.0

**Upload a `.zip` and it lands extracted on the PS5.** Keep a game dump as a
single compressed `.zip` on your PC — smaller and easier to move around — and
upload it directly. ps5upload decompresses it on your computer and streams the
files straight into the fast-transfer pipeline, so they arrive already
extracted on the console — no manual unzip, and no temporary full-size copy
on your disk. The Upload screen previews what the archive expands to (e.g.
"12 GB zipped → 47 GB extracted · 1,204 files") and detects the game's title
from its `param.json`. Resume, excludes, the bandwidth cap, and multi-console
mirroring all work just like folder uploads. ZIP only — `.rar` stays
unsupported (modern scene `.rar` is split + encrypted; unpack it first).

**Reliability fixes (verified on real hardware):**

- Folder downloads no longer double-nest (files landed at `…/foo/foo/`) and
  no longer stop at 256 files — large directories now paginate correctly.
- Uploading a single empty (0-byte) file works instead of failing at commit.
- Upload-queue failures show the plain-language hint (e.g. "PS5 ran out of
  space — click Retry to resume") instead of a raw error, and per-file
  progress stays accurate when a transfer resumes.

**Polish across the app:**

- Rename (single and bulk) refuses to overwrite an existing file, matching
  the guard the Move dialog already had.
- Fixed several stale-data-after-host-switch bugs on the Hardware and
  Volumes screens, and a Power control that showed a success line and an
  error at the same time.
- Disk Usage shows a drillable list for a folder whose children are all
  subfolders, instead of a blank panel.
- Smaller touches: a first-run installer Cancel button, correct Library
  spinner labels, bulk screenshot/rename that continue past a failed item,
  and localized Connection step messages.

**Hardening.** Untrusted `.ffpkg`/UFS2 images and upload manifests are now
validated before use — path-allowlist checks, bounded allocations, and depth
caps — and hosting a large package over HTTP serves it completely instead of
silently truncating.

**Linux white-screen fix (Bazzite / SteamOS / NVIDIA).** The `PS5Upload.sh`
launcher now disables WebKitGTK's accelerated compositing path, so the app no
longer opens as a blank white window on affected GPU/compositor stacks.
Launch via `./PS5Upload.sh` (the recommended entry point), and see the FAQ's
"white screen on Linux" entry for the rare stack that needs more.

---

## 2.12.1

Audit-fix + CI repair point release. 2.12.0's release build failed
at link time because the bundled SDK pin (v0.38) predated the
`getloadavg()` libc symbol our new load-average telemetry depends on
(added in SDK v0.39). Fixing that plus seven adversarial-audit
findings from a multi-agent pass-2 sweep:

- Bumped pinned ps5-payload-sdk to v0.39 in CI + release workflows
  so payload links cleanly against the version we develop with.
- Hardened the SMP-meta control parser against embedded-NUL/control
  byte injection that could false-match action prefixes.
- Closed a race in the SMP-meta worker initializer where a concurrent
  caller could see "watcher started" while the pthread had actually
  failed to create.
- Added `O_NOFOLLOW`/`O_EXCL` to the appmeta heal copy so a hostile
  package can't symlink-redirect writes into system paths.
- Serialized the fan threshold ioctl + pin update so concurrent
  callers can't leave the kernel and the auto-reapply pin out of
  sync.
- Tightened the SMP-meta run-now trigger so triggers arriving during
  a sweep are no longer dropped.
- Made the SMP-meta interval setter refuse the update when the JSON
  payload omits the interval field, instead of clamping to the
  floor.
- Added a static repo-host allowlist for the catalog (github.com +
  git.earthonion.com) so a hypothetical malicious catalog PR can't
  silently point at an arbitrary HTTPS host.

---

## 2.12.0

High-level fixes and packaging cleanup:

- Shell is now stateful by session: `cd /data`, then `ls` or `pwd`,
  keeps the expected working directory across commands.
- Fixed shell argument parsing so commands with paths, like
  `cd /data` and `ls /data`, no longer collapse to the first word.
- Added live-test coverage for shell cwd persistence and a lab CLI
  command for direct shell testing against a running payload.
- Improved Linux release guidance around AppImage/WebKitGTK startup
  issues on Fedora/Bazzite-style systems.

---

## 2.11.0

Phase 1 of a 4-phase design coherence pass surfaced by a 4-agent
audit (IA, workflow walkthroughs, conceptual model, cross-feature
inconsistency). The other 3 phases (rename + IA restructure +
shared primitive extraction) are intentionally scoped to follow-up
releases — they touch many call sites and want their own focused
verification windows.

This release lands 5 discrete fixes that fell out of the audit as
real user-visible bugs, none of which were on the radar before the
review:

- **The OperationBar at the bottom now lights up for Upload Queue
  and Install Queue runs.** Previously `activityWiring.ts` only
  subscribed to 3 of the relevant stores; queue runs (Upload Queue
  Start, Install Queue Start) fired real engine transfers but the
  ActivityBar / Activity tab stayed dark — "where do I look to see
  what's happening?" had inconsistent answers depending on which
  surface kicked off the work. Now every queue item start/progress/
  terminal forwards into activityHistory, so cross-screen "what's
  in flight" reads from a single source again.
- **The Upload screen and the Upload Queue panel are now mutually
  exclusive.** The PS5 payload's transfer port is single-client, so
  starting the queue while a one-shot upload is in flight (or vice
  versa) would block at the socket while both UIs displayed
  "running". The Upload button now disables with an explanatory
  tooltip while the queue runs; the queue Start button does the
  symmetric disable on one-shot in-flight. "Add to queue" stays
  enabled because it's a staging action, not a network one.
- **AppShell drag-drop now uses `safeUnlisten`** to match Upload +
  InstallPackage. Was the lone holdout using a bare `try/catch`
  that the global unhandled-rejection handler from 2.7.1 only
  caught after-the-fact; this brings every drag-drop site to the
  same pattern and prevents the next regression of forgetting it.
- **First-Run wizard step numbers are now 1/2/3** instead of 1/3/4.
  `SetupCard index={3}` was a leftover from a removed step 2 — the
  numbers in the UI looked like a typo without a footnote.
- **Default PS5 host is now empty** (was hardcoded
  `192.168.137.2`, the USB-tether-on-Windows-ICS gateway). Wrong
  for ~95% of users, who clicked Check on first launch, got a red
  error, and may not have noticed the field already had a value.
  Empty default lets the placeholder (`192.168.1.50`) do its job
  and forces the user to read what they're typing. The Discover
  panel remains the recommended onboarding.

### Deferred to Phase 2-4

- **B3** (real cancel — payload-side ABORT_TX frame) needs a
  C-payload change plus hardware verification; held for a focused
  payload session.
- **B4** (open-coded poll loops in Library/FileSystem) and **B8**
  (dual `toMgmtAddr` signatures) get fixed when the Phase 3
  primitives (`useCancelableJob`, `lib/addr.ts`) land — extracting
  them now would mean two refactors of the same code.
- **Phase 2 + 4** (vocabulary cleanup + sidebar IA restructure +
  Send Payload / Payload library merge with tabs + Settings split)
  is a separate i18n-heavy release with ~300 translation key
  changes; held until the primitive refactor is in (Phase 3) so
  renames land on stable shapes.
- **Phase 3** (extract `useCancelableJob`, `lib/addr.ts`,
  `useStaleHostGuard`) is a substantial refactor pass: ~300 new
  LOC of primitives, ~600+ LOC of duplication removed, touches 5
  long-running-action stores and ~40 call sites. Wants its own
  focused session with careful test coverage.

The full audit lives in this session's notes; the design report
identified ~20 actionable items across the 4 phases, with credit
also given to 5 things the team has clearly designed exceptionally
well (the OperationBar contract, Saves' handleRestore host guard,
transfer.ts' resume architecture, Connection's VersionBlock
rechecking UX, InstallPackage's diagnostic block).

---

## 2.10.0

Adds a full **Date & Time settings** panel to the Hardware screen
that exposes the PS5's timezone, daylight-saving policy, NTP
auto-sync flag, date/time format preference, and tzdata version —
all read/writable from ps5upload. To the best of our research (see
`reference_ps5_date_registry_keys.md`), ps5upload 2.10.0 is the
first public PS5 homebrew project to write to the
`SCE_REGMGR_ENT_KEY_DATE_*` registry namespace. Write side is
marked experimental until per-key behaviour is confirmed on real
hardware.

**Important — what this does and doesn't do.** Six community-
known gotchas are surfaced inline as an expandable warnings panel:

- The PS5 has **two clocks**: the user-visible wall clock (which
  this panel reads and writes) plus a SAMU-protected secure RTC
  that signs trophies and licenses. **Setting the wall clock
  cannot fake trophy timestamps.**
- Setting the clock far in the past breaks PSN sign-in (TLS cert
  `notBefore` validation fails).
- Setting the clock far in the future breaks game cert validation
  (GTAV-stuck-at-90%-load class).
- "Use Sony's NTP" silently re-syncs the wall clock on every
  reboot — manual time doesn't persist unless you turn it OFF
  first.
- DST rules are bundled in the firmware's tzdata; recently-changed
  regions (Lebanon 2023, Mexico 2022) may be wrong until the next
  firmware update.
- Write side is novel territory; Sony's Settings will reset any
  field that misbehaves.

### What landed (new code paths)

- New payload module `payload/src/sys_registry.c` — generic
  `sceRegMgrGet/SetInt`, `GetStr`, and `sceRtcGetCurrentNetworkTick`
  wrappers via `dlsym(RTLD_DEFAULT, ...)`, same envelope our
  existing `sys_time.c` already uses for
  `sceSystemServiceSet/GetCurrentDateTime`.
- New FTX2 frames 136-139 (`TimeStateGet`/`Ack`/`Set`/`Ack`) and
  matching `runtime.c` handlers (`handle_time_state_get` /
  `handle_time_state_set`) that read every DATE key best-effort and
  surface per-field availability flags so the desktop can degrade
  gracefully when a key isn't reachable on the user's firmware.
- New Rust types `PsTimeState`, `PsTimeStateSetRequest`,
  `PsTimeStateSetResult` in `ps5upload-core/src/sys_time.rs`; new
  `ps5_time_state_get` / `ps5_time_state_set` engine functions; new
  axum routes `/api/ps5/time/state/get` and `/api/ps5/time/state/set`.
- New Tauri commands `ps5_time_state_get` / `ps5_time_state_set`
  registered in `lib.rs::invoke_handler`.
- New `DateTimeStateCard` component on the Hardware screen.
  Renders below the existing "System time" card. Tz/DST/format are
  staged in a pending-edit buffer and committed in one "Apply"
  click — same UX shape as Sony's Settings. The Apply response
  surfaces per-field rc + err_code so the user sees exactly which
  writes Sony accepted and which were rejected.
- **NTP-drift indicator**: `sceRtcGetCurrentNetworkTick` (cached
  NTP-derived tick, not a fresh sync) shown next to the wall clock
  so a large divergence visibly flags "your manual time has drifted
  from what NTP would say".
- 41 new i18n keys, translated into all 17 non-English locales.
- New memory note `reference_ps5_date_registry_keys.md` documents
  per-key hardware-verification status and the rationale for which
  DATE namespace keys we deliberately don't expose (devkit /
  unknown blob formats).

### What the panel intentionally does NOT do (research, then deferred)

- **Secure-RTC manipulation** (trophy timestamp clock) — requires
  kernel patches and per-firmware offset tables, out of scope.
- **NTP-server override** (`DATE_rtc_net`) — bin8 blob format isn't
  reverse-engineered yet; wrong write could brick the NTP daemon
  until Settings → Init.
- **Synthetic NTP-tick injection** (`sceRtcSetCurrentNetworkTick`) —
  unknown impact on Sony's sync daemon and trophy / license state.

These are documented in the research note for a possible later
release after community verification.

---

## 2.9.0

A 4-agent self-audit (race conditions, security, silent failures,
resource cleanup) surfaced ~30 issues. This release bundles the 4
data-loss-class bugs, 7 medium-severity security findings, the
6-screen host-stale-clobber family, and 5 standalone correctness
fixes. Total: 13 real fixes, with a long list of patterns the agents
investigated and verified clean.

### Data-loss / silent-corruption fixes (4)

- **Payload shard-write OOM no longer reports success.** Under PS5
  RAM pressure (kstuff loaded + other payloads alive + ShellUI bloat),
  `runtime_write_shard_persistent`'s malloc-fallback path was
  returning `0` from `drain_shard_data`, which the dispatcher
  interpreted as "shard persisted" — SHARD_ACK fired, the host
  advanced its cursor, the missing bytes were never retried, and
  the user saw "Upload complete" on a corrupt file. All 4 OOM /
  open-failure paths now drain-then-return `-1` with a stderr log,
  so the tx aborts cleanly and the user sees a real error. This
  bug was silently corrupting uploads for unknown duration.
- **Saves restore no longer wipes the wrong PS5 if the user
  switches roster mid-flow.** Restore goes confirm → file dialog
  → unzip → wipe → upload (often 30+ seconds for big saves). The
  recursive `fsDelete` previously used whatever IP was current at
  await-resolution time, so a roster swap during the dialog would
  silently target a different console. Now snapshots the host at
  click time and refuses with an explicit "Host changed during
  restore" error if it's no longer the same.
- **FileSystem screen no longer deletes the wrong file after fast
  navigation.** A slow `ps5_list_dir` (1-3s on big `/data` trees)
  resolving AFTER the user navigated to a deeper folder would
  display the OLDER directory's contents under the NEWER URL. A
  per-row Delete click joins the CURRENT path with the displayed
  name — so the user thinking they're deleting `/data/foo`'s
  "screenshots" could actually delete `/data/homebrew/screenshots`
  if a coincidentally-named file existed there. The refresh now
  drops stale results via probed-host + probed-path guards.
- **`transfer.ts` mount no longer leaves RW when user picked RO.**
  Starting a second image upload with a different `mountReadOnly`
  flag while a previous `fsMount` was in flight produced a race
  where the OLDER mount could win the same mount point. Now
  best-effort unmounts the superseded mount before returning,
  closing the silent RO/RW divergence window.

### Security hardening (3 Medium → 0)

- **`/pkg-host/*` URL is now bound to the PS5 it was issued for.**
  Previously any host on the LAN that could observe the PS5↔engine
  TCP stream (promiscuous WiFi, ARP-spoof, SOHO router admin) could
  recover the session UUID from the first GET and hammer the URL
  with Range requests to drive 16 MiB allocations per call, OOMing
  the engine and potentially the Tauri shell. Now compared against
  the session's recorded `ps5_mgmt_addr`; loopback callers still
  allowed for dev workflows. Logs every reject with peer + expected.
- **Payload `is_path_allowed` now blocks symlink-escape.** The
  lexical check confirmed paths started with `/data`/`/mnt/...`,
  but a symlink in a user-mounted `.ffpkg` (e.g.
  `/mnt/ps5upload/usermount/evil → /system_ex`) escaped — the
  subsequent open() followed the symlink and operated on the
  forbidden target. Same CWE-59 class as CVE-2007-2374. Now calls
  `realpath()` and re-validates the canonical form; symlinks
  pointing outside the allowlist refuse with a stderr log.
- **Engine planner's `spawn_blocking` cleanups now timeout at 10s.**
  The 3 staging-file cleanup tasks in `pkg_install.rs` used bare
  `fs_delete` with the default 30s socket timeout PLUS waiting for
  ACK; a wedged PS5 (kernel hang, payload crashed, unreachable
  LAN) parked a blocking-pool worker AND a TCP socket per call.
  Repeated register-rejects could stack and exhaust the 512-thread
  pool. Now uses `fs_delete_with_timeout(Some(10s))`.

### Host-stale-clobber pattern across 6 screens

Ported `Library`'s `probedHost` + `isStale()` guard to:
**Saves, Volumes, Screenshots, Hardware (both `refresh` and
`refreshPs5`), SendPayload (`probeFile`)**, and **InstallPackage
(`addPkgPath`)**. Each had the same shape — slow async call
resolves after the user switched PS5, OLD host's data lands in
state attributed to NEW host. Most were P1 (misleading UI), but
combined with the Saves restore P0 they enabled the
"wipe-wrong-console" cascade above. Six bugs closed by adopting one
proven template.

### Other correctness + DX wins

- **Payload `posix_fallocate` ENOSPC now aborts the tx immediately**
  instead of silently falling back to sparse `ftruncate`. Front-of-
  60GB-upload disk-full is information the user needs NOW, not 50
  GB later via a piecewise "open failed" message.
- **Save backup no longer produces empty zips on `file_type()`
  failure.** `flatten_wrapper_subdirs`'s 4 `.unwrap_or(false)` sites
  silently treated I/O errors as "not a match," sometimes descending
  past real data layers and producing empty backups that the user
  was told succeeded. Now propagates the error with path context.
- **Engine planner skips + warns on metadata failure** instead of
  defaulting to `size=0` (which corrupted the `total_bytes`
  denominator and surfaced later as a misleading "open <path>: io
  error" mid-transfer).
- **`engineLogsTail` now escalates after 10s of failures.**
  Previously retried silently forever — a permanently-broken
  bridge (engine binary corrupt, wrong-arch, hung sidecar) hid the
  very logs the user would need to diagnose it. Now emits one
  `log.error` after 10 consecutive failures, with a recovery log
  if it comes back.
- **Payload binaries no longer git-tracked.** `payload/ps5upload.elf*`
  are now `.gitignore`d. CI rebuilds them from C source on every
  release (`make payload`), and the desktop build script errors
  with a clear "Build it first: make -C payload all" if missing.
  Closes the "binary churn after CI run" friction we hit twice in
  2.8.0 development AND the risk of a contributor shipping a
  desktop build that embeds a stale payload.

### Things the audit verified clean (NOT bugs)

For credit + future reference: agent findings that turned out to
be false positives — `Hardware` tickers DO gate on
`useDocumentVisible()` (just at the call site, not the deps),
`extract_json_string_field` DOES handle JSON escape sequences
correctly (via `json_string_end`/`json_copy_unescaped_string`
helpers, just at a different layer), and the existing
`payloads_release` stale-cache already handles network errors
(but didn't handle HTTP 4xx/5xx, which 2.8.0 fixed). Two false
positives out of ~30 findings — trust-but-verify rule paid off.

---

## 2.8.0

This release lifts a handful of ideas from sonicloader (sister
project) after a deep cross-read of their PKG install path. Three
concrete improvements landed; the big one — bundling DPI as a new
install tier — is split into a catalogue entry now and a full
install-runner integration in a follow-up release.

Also fixes a long-latent Upload-screen race that was held back from
2.7.x for verification.

- **The Upload screen no longer shows the wrong file/folder in the
  destination preview after picking a second source.** When
  inspectFolder for the first pick resolved AFTER the user had
  already picked a different source, its closure-captured `path`
  would overwrite the newer source.path — the destination preview
  then rendered the OLD name even though the user had moved on, and
  hitting Upload would land the wrong source at the displayed path.
  Fixed by guarding the post-await `set()` with `get().source?.path
  === path` so stale inspect results are dropped. 4 unit tests
  cover the race patterns (folder→folder, folder→file, stale
  failure, no-race control).

- **Staged-install PKGs now land on the PS5 as `<ContentID>.pkg`
  instead of `<queue-id>_<ts>.pkg`.** Sony's installer keys on the
  basename for some FW points and silently rejects mismatched
  names — a class of "PKG installer rejected the file" failures
  that looked like the PKG was bad but were actually a naming
  mismatch. Fix is symmetric with sonicloader's
  `canonicalise_pkg_filename` (homebrew.c:436). Falls back to the
  legacy `<id>_<ts>.pkg` shape when the PKG header has no parseable
  ContentID, so malformed homebrew PKGs still get a shot at install.
- **The Payloads release info now survives a GitHub outage.** The
  cache was already saving release JSON on disk and falling back on
  network errors, but a 403 (rate-limited) or 5xx (Cloudflare
  hiccup) bypassed it — the user saw "fetch failed" even when a
  perfectly good cached snapshot was sitting there. Now any non-2xx
  response (or malformed body, or read error) also falls back to
  the cached snapshot, with a yellow "couldn't refresh — showing
  cached" banner so it's clear the data might be stale. Pattern
  ported from sonicloader's `src/releases.c:383-409` with the
  `refreshError` field name kept.
- **`ezremote-dpi` (cy33hc/ps5-ezremote-dpi) is now in the payload
  catalogue.** DPI is a long-lived install daemon that owns Sony's
  install state machine for a PKG's full lifetime, sidestepping
  the class of "install accepted then evaporates" bugs that hit
  cross-process callers. Install it from the Library tab. In a
  follow-up release the install runner will offer "DPI" as a new
  install method that proxies through it; this release lands the
  catalogue entry so anyone can install DPI today.

Sonicloader fields we explicitly verified we don't need to port
(already covered): persistent notification inbox (we have
`state/notifications.ts` with the same 64-entry ring + persistence
shape), /data → /user/data sandbox path rewrite (already in
`payload/include/config.h`), and the FTX2 wire protocol (strictly
more capable than their loopback DPI socket / chunked HTTP).

---

## 2.7.2

- **drakmor/kstuff-lite is now in the payload catalogue.** It's a
  fork of EchoStretch's kstuff-lite with a hot path for `.ffpkg`
  (UFS) mounting — measured 3-4× faster end-to-end — and lower
  overhead in the repeated mount/unmount cycles that ShadowMount+
  exercises heavily. Narrower firmware range (3.00 → 10.01) than
  the default kstuff (which covers 1.00 → 12.x via runtime NID
  resolution), so it's a sibling option, not a replacement. Browse
  to the Payloads tab to install. Pick only one kstuff variant —
  both write the same `/data/kstuff.elf` marker, and autoload runs
  whichever you put in your autoload list.
- **Library tab now nudges you toward the faster build if you
  actually use `.ffpkg` / `.exfat` images.** A small dismissible
  tip appears under the "Disk images" section header (only when
  you have at least one image in your library) with a one-click
  link to the drakmor repo. Translated in all 17 non-English
  locales.

---

## 2.7.1

- **Resending the payload now shuts down the running one first.**
  Before re-uploading to `:9021`, the desktop opens `:9114` and
  sends a graceful `Shutdown` frame (200 ms total budget). The old
  payload exits cleanly, the new ELF mmaps into a fresh process,
  and the cascade of stale-state errors after every resend is gone
  (most visibly the `pkg_install` retry storm on the Install Package
  screen, which was the old payload still answering long after a
  new one had been "loaded"). If nothing's listening on `:9114`
  (first boot, payload crashed), the pre-shutdown is a no-op and
  the send proceeds.
- **Sidebar no longer triggers React's "setState during render"
  warning.** A dev-only i18n missing-key warning was calling
  `console.warn` synchronously from inside `t()`; the patched
  `console.warn` in the log capture wrote into the logs store, and
  the Sidebar — which reads error-count from that same store —
  closed the loop. Warning is now deferred via `queueMicrotask`,
  so it still fires once per missing key but outside the render
  frame.
- **Tauri listener-teardown race is swallowed quietly.** When a
  webview unmounts mid-`unregisterListener` the inner Promise
  rejects with `TypeError: undefined is not an object (evaluating
  'listeners[eventId].handlerId')`. The listener is already gone —
  exactly what we wanted — so the global `unhandledrejection`
  handler now intercepts that specific message, prevents the
  default ERROR banner, and logs at `debug` instead.
- **Three missing i18n keys added in all 17 non-English locales:**
  `queue_strategy_overwrite`, `queue_strategy_resume`, and the
  pluralized `logged_error_one` / `logged_error_many` used by the
  Sidebar error chip. These were template-literal lookups the
  phantom extractor can't auto-detect; non-English users were
  seeing English fallbacks on the Install Package strategy buttons
  and the Logs nav badge.

---

## 2.7.0

- **New "Sync time" card on the Hardware screen.** Shows the PS5
  clock alongside your PC's clock plus the live drift between them
  (updates once a second). One click sets the PS5 system clock to
  match your PC's UTC time. Confirmation prompt before the set
  (clock changes can affect trophies, save timestamps, and DRM
  checks).
- The set goes through Sony's `sceSystemServiceSetCurrentDateTime`,
  which lives in `SceShellCore` IPC. Requires a ucred-elevated
  loader (kstuff or equivalent) — without it, Sony's authid check
  rejects the call and the desktop surfaces the Sony err_code with
  a hint to reload via kstuff.
- The payload bookends every set with a get-before + get-after and
  reports both unix epochs. On some firmwares the SDK stub returns
  rc=0 but the underlying syscall is a no-op; the desktop detects
  this (post-set unix more than 5 s away from the requested target)
  and renders a clear "PS5 reported success but the clock didn't
  actually move" warning instead of a misleading success message.

---

## 2.6.0

- **Stream install (DPI 2.0) is the new default for Install Package.**
  The desktop now serves the `.pkg` over HTTP and BGFT pulls + installs
  it in one pass — no upload step, no 2× disk space, native pause/
  resume from BGFT itself. The previous "upload then install" path
  stays available as a one-click fallback on the failure card for the
  rare LAN topologies where the PS5 can't reach the desktop's HTTP
  port (firewall, segregated VLAN).
- A new segmented control on the Install Package screen lets you pick
  the default install method; per-row badges (`stream` / `staged`)
  show at a glance which path each queued item is using.
- The engine-side HTTP serve route, BGFT URL handover, and Range
  support were already in tree — this release wires the UI choice
  through so the path actually gets used by default.

---

## 2.5.2

- **Windows `.exe` launches on a freshly-installed Windows 11**,
  including arm64. Previous releases dynamically linked the MSVC C++
  runtime (`VCRUNTIME140.dll`, `MSVCP140.dll`), which isn't preinstalled
  on a vanilla Windows install — especially Windows 11 on arm64, where
  almost nothing has pulled the Visual C++ Redistributable in yet. Users
  saw "The code execution cannot proceed because VCRUNTIME140.dll was
  not found." Both binaries shipped to Windows (`PS5Upload.exe` and the
  `ps5upload-engine.exe` sidecar) now statically link the MSVC CRT via
  `+crt-static`, so they're self-contained — no redistributable
  required.
- **Linux `.AppImage` launches on a freshly-installed Ubuntu 24.04+**.
  Ubuntu 24.04 dropped `libfuse2` from the default install, which the
  AppImage's type-2 self-mount needs at startup — so on a brand-new
  desktop the AppImage just failed silently. The release zip now also
  contains a `PS5Upload.sh` launcher that sets
  `APPIMAGE_EXTRACT_AND_RUN=1` so the AppImage self-extracts to `/tmp`
  instead of fuse-mounting — no apt install, no libfuse2, no kernel
  module. Users with libfuse2 already installed can still run the
  `.AppImage` directly if they prefer.
- A new **Fresh-Install Verification Matrix** in `TESTING.md` documents
  the historical "ships green, fails on fresh install" failure modes
  per platform/arch and the hands-on test pass that catches them before
  tagging.

---

## 2.5.1

- **Windows release `.zip` opens in Windows Explorer again.** The
  2.5.0 release workflow used `tar -a -c -f *.zip` (bsdtar /
  libarchive) to wrap `PS5Upload.exe`. bsdtar streams output, so it
  sets the zip local-header flag bit 3 ("data descriptor follows") —
  Windows Explorer's built-in unzip rejects bit-3 zips on many
  builds with "the compressed (zipped) folder is invalid", even
  though 7-Zip / WinRAR / unzip open them fine. The workflow now
  uses PowerShell's `Compress-Archive` (`.NET ZipFile`) which writes
  sizes + CRC in the header — guaranteed openable by every Windows
  Explorer build. If you grabbed 2.5.0 and saw the "invalid folder"
  error, this is the fix.

---

## 2.5.0

- **`.jar` payloads** can now be sent — both from the Send Payload
  screen and as steps inside playlists. Useful for BD-JB / BDJ-runtime
  loaders. A new collapsible reference under the port field lists
  typical loader ports per format (`.elf` → 9021 elfldr, `.js` →
  50000 WebKit-stage, `.lua` → 9026, `.jar` → 9025 BD-JB) with a
  note that custom loaders may listen on any port.
- **Sidebar nav and other data-driven UI** now translate correctly in
  all 17 non-English locales. Labels like "Dashboard", "Save data",
  "Screenshots", "Disk usage", "Payload library", "Kernel log", and
  "Shell" used to silently fall back to English even when the locale
  had a translation, because the keys never made it into the
  canonical English dictionary. A dev-mode console warning now
  catches any future occurrences of the same gap.
- **Full translation refresh** across all non-English locales —
  roughly 190 keys per locale of pending backlog caught up, including
  Connection screen, Upload status messages, Install Package
  diagnostics, and all the small UI fragments that had been showing
  in English for months.
- **Multi-pass bug sweep** fixed several real correctness issues
  across the engine, the Tauri shell, and the PS5 payload metadata
  parsers — including a payload-version flicker on transient probe
  misses, a library-refresh race on host switch, a Windows port-
  killer that could `taskkill` an unrelated PID, an unsafe drive-path
  acceptance in the USB autoloader wizard, and three corruption-error
  paths in the PKG / UFS2 parsers that always surfaced as a generic
  EOF instead of the descriptive `BlockOutOfRange` variant.
- **Internal**: every user-visible string in JSX is now enforced at
  build time to route through the translator — adding a hardcoded
  label is a lint error, not a silent gap.

---

## 2.4.0

- Save data backup is now a clean `<title_id>.zip` with just the image
  and sealed key inside — matches the format save-resigning tools use.
  Restores re-add the on-PS5 prefix automatically, so backups and
  resigner output are now interchangeable.
- Save backup correctly handles PS2/PSP-Classic titles whose layout on
  PS5 hides the real data inside nested wrapper folders; the backup
  zip now contains the actual save, not an empty folder or duplicate
  copies.
- Single-file uploads (.pkg / .ffpkg / large images) can now resume
  from where they stopped after a wifi drop or other failure — the
  next click of Retry picks up at the last acknowledged shard instead
  of restarting from zero. Retry budget for single-file uploads
  raised to survive several wifi blips per long upload.
- Pre-flight free-space check on the destination drive before a
  single-file upload starts. If the drive can't fit the file, you
  see a clear "needs X more GB free" message in seconds instead of
  the upload silently dying hours later.
- Failed-upload error cards now show a humanized hint (e.g. "PS5 ran
  out of free space — free space and click Retry") with the raw
  payload error tucked in a collapsible "raw error" expander for
  debugging.
- Install Package: better warning that the PS5 screen may go black
  during install — that's normal, the install keeps running.
- Install Package: queue progress now survives a tab switch — opening
  another tab and coming back no longer resets the in-flight bar.
- Listener cleanup race fixed across screens — no more occasional
  "TypeError: undefined is not an object" console errors during
  navigation or hot-reload.
- Startup is faster: language packs (18 locales, ~1 MB total) now
  lazy-load on demand instead of all-eagerly. The main JS bundle
  drops from ~1 MB to ~200 KB.

## 2.3.0

- Install Package: tries the in-process installer first (no home-screen
  flash on most pkgs); falls back to the ShellUI route only when Sony's
  installer requires it.
- Install Package: cleaner status — a row succeeds, fails, or rolls
  back. No more low-level "we couldn't extract pkg metadata" notes; if
  it fails, the error tells you why.
- Better error for DLC installs whose base game isn't installed yet
  (was previously a misleading "BGFT not available" message).
- Shell tab now works on PS5 (no `/bin/sh` on the console — built-in
  commands: `help`, `ls`, `cat`, `stat`, `ps`, `mount`, `df`, `id`,
  `uname`, `env`, `sysctl`, `hostname`, `echo`, `sleep`).
- Save data + Screenshots tabs now populate correctly (older builds
  used wrong filesystem paths and hex-vs-decimal user-ID parsing).
- Hardware → Network: lists real interfaces (eth0 / wlan / lo) via
  the FreeBSD getifaddrs fallback when Sony's API returns empty.
- Disk Usage: better text visibility — small folders no longer get
  cells too narrow to read, and the layout fills the window.
- Kernel Log: optional filter panel groups Sony's routine PSN /
  storage / framework chatter so you can focus on real issues
  (default: show everything; click Filters → "Hide Sony noise" to
  collapse).
- Mount modal: warns up front when the chosen path is in the Sony-
  reserved `/mnt/usb*` or `/mnt/ext*` namespaces, with one click to
  switch to `/mnt/ps5upload/`.
- Bundled-payload extraction now handles concurrent app launches +
  shows the real error if extraction fails (no more misleading
  "run make payload" hint).
- Smaller fixes: GoldHEN entry removed from the payload library
  (PS4-only); Compare PS5s tab removed.

---

## 2.2.61

**13-pass audit: boot reliability, install correctness, DoS hardening**

- Takeover handshake no longer hangs on a wedged previous payload
  (added a 2 s `recv` deadline) and tolerates big-COMMIT_TX tails
  (port-release wait bumped from 2 s to 10 s).
- Install path serializes correctly across files: the AppInstUtil
  install-start and status-poll calls in `bgft.c` now share the same
  Sony-API mutex as register / launch / uninstall in `register.c`,
  closing a FW 9.60 deadlock window where a status poll concurrent
  with a register on another mgmt thread could wedge the calling
  thread inside Sony's kernel stub.
- Browser-launch RPC now serializes against the rest of the Sony
  install/launch surface (was the only call site missing the mutex).
- pkg-host range responses capped at 16 MiB per request, preventing
  a malicious LAN client from forcing a multi-GB allocation by
  requesting `bytes=0-{total-1}` on a large pkg.
- ELF-loader (port 9021) destination rejects non-ELF files before
  connecting; .bin / .js / .lua flows on custom loader ports
  unaffected. Closes a silent wrong-file-picked failure where the
  loader received garbage and the UI showed "send succeeded" with
  no payload coming up.
- Boot-failure reasons now surface as PS5 toast notifications
  instead of vanishing into stderr (which is invisible on a
  :9021-loaded payload).
- Cleanup-on-failure for `pthread_create(mgmt)` no longer leaves a
  stale ownership record.
- Deleted ~159 LOC of dead code (`payload_loader.rs`); fixed several
  stale comments (notably the "we eagerly call
  register_services_init" claim that contradicted the actual code).
- Tests: +21 pinned (Rust connection.rs partial-write retry,
  probes.rs ELF-magic + size-cap + non-default-port behaviour,
  pkg-host range cap), 255 client TS tests still green.

---

## 2.2.60

**Install Package, Library Play / Unmount, sensor fixes**

- Install Package works end-to-end on FW 9.60: a three-tier pipeline
  routes Sony's installer through ShellUI's authid via ptrace RPC for
  game pkgs (CUSA / PPSA / PCSA / EP / UP). NPXS-prefix system pkgs
  get a fire-and-forget path with on-PS5 verification.
- Library Play always registers first (idempotent), retries with a
  DRM-type patch on rejection, then launches.
- Unmount unregisters every title inside the image first, so the
  dashboard stays clean — no ghost tiles, no stale title-id errors.
- Hardware sensors stop drifting over time: bgft.c restore is now
  retry-with-verify; the sensor retry path re-arms the debugger
  authid.

---

## 2.2.50 – 2.2.59

**Install Package: optional `file://` flow, mount visibility, audit
fixes**

- Install Package gains an optional PS5-side path (`file://` flow)
  for upload-then-install workflows.
- Mounted `.ffpkg` / `.exfat` at user-chosen paths surface in
  Volumes and the Library mount badge correctly.
- AppInstUtil error code humanization with actionable copy.
- Mount errors now go through a humanizer (especially nmount EPERM).
- Multi-pass audits across the install / mount paths: 9 + 4 fixes
  spanning SSRF defense, pagination, JSON parsing, retry semantics.

---

## 2.2.31 – 2.2.49

**Install Package tab, Library polish, switch to AppInstUtil**

- New Install Package tab: drop `.pkg` files, queue serial installs.
- Switched primary install backend from BGFT to AppInstUtil
  (compile-time linked) with synthetic task IDs to route status
  polls.
- BGFT: try multiple library paths and symbol-name variants before
  giving up.
- Library row: 7+ buttons collapsed into one primary action +
  Details + overflow menu. Play auto-registers; 60 s launch timeout.
- Library Game Details: cover art for both PS5 (`PPSA…`) and PS4
  (`CUSA…`) titles via title-id prefix routing, with hostname-allowlist
  SSRF defense and a per-platform cover-host regex.
- 18 languages reach 100%+ string coverage.
- Engine: graceful axum shutdown, install-queue persist debouncing,
  PKG session GC on a longer schedule.

---

## 2.2.26 – 2.2.30

**Live sensors and Launch on FW 9.60**

- Hardware tab returns real CPU / SoC temperature, frequency, and
  power readings on FW 9.60. Sony gates these APIs on caller pid =
  `SceShellUI.pid`; we route the calls through a ptrace-based RPC
  into ShellUI itself so the check passes.
- Library Launch starts games on FW 9.60 via the same RPC path.
- Process list switched to `sysctl(KERN_PROC_PROC)` with stable
  `kinfo_proc` offsets — real names show up, no per-firmware table
  needed.
- Audit pass: 27 bug fixes across payload, engine, and client —
  COMMIT_TX corruption guard, single-file resume reopen, spool
  fallback heap migration, atomic DRM patch, updater host pinning,
  download path-traversal block, JSON-escape-safe app register.
- Load order doesn't matter anymore — the loader can land before or
  after our payload; the next request picks up the new privilege.

---

## 2.2.25

**Library mount picker + Library search bar**

- Mount picker: pick volume, subpath, and name. Resolved path
  appears in real time. Last-used selection persists per host.
- Live search across Library: matches name, title ID, path, scope,
  volume; multi-word AND across fields.
- Payload `mount_point` field plumbed through engine, Tauri command,
  and TS API end-to-end. Older payloads gracefully degrade to the
  legacy name-only path.
- Audit hardening: hex-escape mount-tracker keys (collision-proof),
  zero-init out-buffers, picker syncs with live volume probe.

---

## 2.2.20 – 2.2.24

**Move-progress accuracy, FileSystem volume picker, engine lifecycle**

- FileSystem screen gets a volume picker dropdown above the
  breadcrumb. Last-browsed path persists per host.
- PS5 host now persists across launches.
- Library Move shows live byte progress on current payloads;
  threshold-specific banner when the payload is older than the
  app.
- Connection screen flips to fresh version + kernel data
  immediately on Replace payload, with a "rechecking…" spinner
  while the probe is in flight.
- OperationBar covers every in-flight op (uploads, downloads,
  FileSystem ops, Library actions).
- Engine lifecycle: stdin-EOF watcher prevents orphans on
  ungraceful exits, port-killer reaper cleans up pre-existing
  orphans, single-instance plugin keeps double-launch from
  killing the first instance's engine.
- CI: workflows on Node 24, coverage job green again on
  ubuntu-24.04 with rustup-resolved llvm-cov.

---

## 2.2.16 – 2.2.19

**Cross-mount move progress, payload DoS-safety, FS_OP slots**

- Cross-mount moves keep their speed/byte counter — buffer for
  `FS_OP_STATUS` JSON snapshot bumped to fit two maxed-out
  PS5-path strings.
- Payload caps `BEGIN_TX` manifest bodies at 256 MiB; oversize
  refused with `begin_tx_body_too_large` and the connection
  closed cleanly.
- `fs_op` slot allocation is strict first-fit; no eviction.
  Refuses new ops with a clear reason instead of silently blinding
  an in-flight op's progress poll.
- Library Move modal pre-flights the destination via debounced
  `FS_LIST_DIR`; warns inline on existing-path collisions.
- Engine error responses carry the full anyhow chain so the root
  cause reaches the client.
- `make run-client` fails fast on missing GTK / WebKit dev libs
  with a copy-pasteable apt / dnf / pacman command.

---

## 2.2.10 – 2.2.15

**Activity tab, OperationBar, internationalization**

- Activity tab: persistent record of the last 100 operations with
  start time, duration, outcome, bytes moved. "Running now" /
  "Recent" sections; Clear button.
- OperationBar (always-visible footer strip) shows in-flight ops
  across screens; click to expand per-op rows.
- Stop button on every long-running surface: FS bulk delete +
  paste, Library row download / delete / chmod / mount / unmount,
  uploads.
- PS5-internal copy/paste throughput: read+write overlap via a
  worker thread + double-buffered ring (16 MiB × 2). Throughput
  bounded by the slower device, not the sum.
- Live progress + Stop on Library Move via `FS_OP_STATUS` polling
  and `FS_OP_CANCEL`.
- Engine `/api/version` endpoint; cancelled `fs_copy` cleans the
  partial destination so retries have a clean slate.
- 18 languages: Activity tab, Settings keep-awake hint,
  OperationBar, Connection screen + StatusBar, sidebar nav, Upload
  drop zone — every visible string runs through `tr()`.

---

## 2.2.0 – 2.2.9

**Resume, in-app updates, broader firmware coverage**

- One build covers PS5 firmware 1.00 – 12.70. Core transfer,
  mount, and file features work on every supported firmware.
- In-app update check (Settings → Updates): polls GitHub once a
  day; archives land in Downloads — replace the app manually, no
  installer.
- Resume survives app restart: close mid-upload, reopen, pick up
  where you left off (24 h window).
- Folder uploads keep their name; destination preview shows the
  exact PS5 path as you type.
- Cross-platform six bundles: macOS arm64 + x64, Windows x64 +
  arm64, Linux x64 + arm64.
- Payload no longer crashes when a client disconnects mid-transfer
  (SIGPIPE ignored).
- Multi-GiB single-file uploads stream a single shard-sized buffer
  instead of memory-mapping; peak RAM bounded by shard size.
- Folder excludes (`.DS_Store`, `Thumbs.db`, `*.esbak`, `.git/**`).
- Image uploads can mount on completion via the PS5 kernel's LVD
  backend.
- Engine sidecar restarts cleanly when the prior child is
  unresponsive.
- Real cancel + live progress for PS5-internal copy / move.
- Better humanized error messages on Upload and Volumes screens.

---

## 2.1 and earlier

**Direct-write transfer pipeline, single-file resume, FTX2**

- Single-file transfer: direct-write to `.ps5up2-tmp` sibling;
  atomic rename on COMMIT — no spool overhead.
- Multi-file transfer: manifest JSON + binary search per shard;
  packed-shard worker pool absorbs transient I/O hiccups.
- BLAKE3 per-shard verification.
- Reconcile: size + hash compare for resume.
- FTX2 binary protocol with a 28-byte LE frame header
  (magic / version / type / flags / body_len / trace_id), plus
  86 frame types covering Tx lifecycle, FS ops, mount, app
  register / unregister / launch, hardware, pkg install.
