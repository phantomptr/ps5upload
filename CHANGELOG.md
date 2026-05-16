# Changelog

What's new in ps5upload, written for humans.

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
