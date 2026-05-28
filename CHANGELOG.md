# Changelog

What's new in ps5upload, written for humans.

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
