# Changelog

What's new in ps5upload, written for humans.

---

## 2.2.22

**Big-tree deletes work; queue uploads show speed; pack worker survives transient I/O hiccups**

- **Fix: deleting a small-file-heavy game folder no longer 502s.**
  Symptom on a real PPSA01342 (≈223k files / 19k dirs / 129 GiB):
  the FileSystem screen would surface `engine HTTP 502 Bad Gateway:
  read frame header: Resource temporarily unavailable (os error
  11)`. Root cause: the engine's per-socket I/O timeout is 30 s,
  but the payload's recursive `rm -rf` is single-threaded and
  ≈240k metadata syscalls on PS5 UFS take minutes — so the engine
  gave up on the read while the payload was still happily
  deleting in the background. `fs_delete` now uses the same
  1-hour deadline `fs_copy` and `fs_move` already do, so the
  destructive trio behaves consistently for big trees.
- **New: live progress + Stop button for big deletes.** Same
  `op_id`-tracked plumbing the cut/copy/paste flow uses
  (`FS_OP_STATUS` polling at 500 ms, `FS_OP_CANCEL` watcher at
  200 ms). The payload pre-walks `recursive_size` to compute
  total bytes, registers the op in its in-flight table, and
  reports bytes-freed as it unlinks each regular file. Cancel
  propagates between directory entries and leaves the partial
  tree intact (so a stop on a 99% delete doesn't surprise-erase
  what's left). Single-file unlinks still take the un-tracked
  fast path and don't burn one of the four in-flight-op slots.
- **New: per-item transfer speed in the upload queue.** The
  Upload screen's queue panel was showing a moving progress bar
  but no rate or ETA — making it impossible to tell whether a
  multi-hour run was healthy or stalled. Each running row now
  shows `bytes/total · MiB/s · ETA` using the same trailing-window
  EWMA the single-shot upload uses (4 samples × 500 ms = 2 s
  trailing average; flat samples are skipped so the readout
  stays stable between shard bursts instead of flickering to
  "—"). Done rows show the wall-clock-average MiB/s for
  comparison across runs. The smoother now lives in
  `lib/rollingRate` and is shared by both call sites so they
  can never visually disagree.
- **Fix: pack worker no longer aborts a 75k-shard upload on a
  single transient I/O error.** Symptom on the same PPSA01342
  workload: an upload would fail at `shard 11278/75667` with
  `pack_worker_io_error`. Root cause: one transient `open()` or
  `write()` failure (EIO/EMFILE/ENOMEM under sustained
  many-small-file pressure on PS5 UFS) flipped the pool's sticky
  worker-error flag, aborting the entire transaction — even a
  99.99% per-syscall success rate gave only ~50% chance of
  finishing. The pack worker now retries transient errnos up to
  3 times (20/50/100 ms backoff), only flipping the sticky flag
  on truly unrecoverable errors (ENOSPC/EROFS/EACCES/
  ENAMETOOLONG) or after retries are exhausted. Retry counts
  are folded into `COMMIT_TX_ACK` (`pack_open_retries`,
  `pack_write_retries`) so bench sweeps and post-mortem logs
  show how many transient hits were absorbed.
- **Internal: shared `lib/rollingRate` + `queueOps.resetRunningToPending`
  helpers.** Both pure, both with vitest coverage (14 + 4 new
  cases). Previously the EWMA logic was inline in
  `state/transfer.ts` and the stop-reset logic was inline in the
  queue store; lifting them out lets future surfaces use the
  same rate math without re-implementing it.

---

## 2.2.21

**Finish the GHA Node 24 migration**

- **Fix: 2.2.20 only got us part of the way to Node 24.** Bumping all
  `actions/*` to `@v5` cleared the warning for `checkout`,
  `setup-node`, and `cache` (whose v5 ships Node 24), but
  `upload-artifact@v5` and `download-artifact@v5` are still Node 20
  — the artifact actions chose a different major-version cadence
  for their runtime switch. `upload-artifact` is now pinned to
  `@v6` (first major to ship Node 24, pure runtime bump, no
  behavior change) and `download-artifact` to `@v7`
  (`download-artifact@v6` is still Node 20; v7 is the first Node 24
  release, again a pure runtime bump). Stayed off the latest majors
  (`upload-artifact@v7`, `download-artifact@v8`) on purpose —
  those add opt-in features (direct uploads, `skip-decompress`)
  and a behavioral change (hash-mismatch defaults to error) that
  aren't needed here, and the smaller bump means smaller blast
  radius. Five references touched across `engine-ci.yml` and
  `release.yml`.

---

## 2.2.20

**CI hygiene: workflows on Node 24**

- **All GitHub Actions usages bumped from `@v4` → `@v5`.** Every job
  was emitting "Node.js 20 actions are deprecated" warnings on
  `actions/checkout`, `actions/setup-node`, `actions/upload-artifact`,
  `actions/download-artifact`, and `actions/cache`. v5 of each runs
  on Node 24 and is a drop-in replacement for the upload/download
  patterns this repo uses (single-named uploads, no-name "download
  all to subdirs"); 25 references updated across `engine-ci.yml`
  and `release.yml`. No workflow logic changes — purely a runtime
  bump to clear the deprecation and stay ahead of the June 2 2026
  forced switch.

---

## 2.2.19

**Coverage CI green again on ubuntu-24.04**

- **Fix: coverage job failed every run with `error: no profile can
  be merged`.** GitHub's `ubuntu-24.04` runner image preinstalls
  `/usr/lib/llvm-18/bin` on `PATH`, so `cargo-llvm-cov` resolved
  `llvm-profdata` to system LLVM 18 — but current-stable `rustc`
  emits profraw v10 (LLVM 19+ internally), and llvm-18 rejected
  every file with `raw profile version mismatch: expected version
  = 9`. `scripts/coverage.mjs` now resolves `llvm-cov` /
  `llvm-profdata` from the active rustup sysroot first
  (`$(rustc --print sysroot)/lib/rustlib/<host>/bin/`), so the
  toolchain that emits the profraw is the same one that reads it.
  Caller-provided `LLVM_COV` / `LLVM_PROFDATA` env vars still win;
  PATH and well-known LLVM prefixes remain as fallbacks for hosts
  without `llvm-tools-preview`.

---

## 2.2.18

**DoS-safe payload, robust fs_op slots, move pre-flight, activity unstuck**

- **Payload now caps `BEGIN_TX` manifest bodies at 256 MiB.** Crafted
  or buggy `body_len` values used to either OOM the payload at
  `malloc` time or tie up a worker thread draining bytes from a LAN
  client until `recv_exact` timed out. Oversize manifests are now
  refused with `begin_tx_body_too_large` and the connection is
  closed (a misaligned drain after that point isn't safe). 256 MiB
  leaves an order-of-magnitude headroom over the largest small-file
  workload we validate against (PPSA01342 at 223k files ≈ 33 MiB
  manifest).
- **Recursive size walks now fail fast instead of undershooting
  `total_bytes`.** Previously, a truncated child path or a sub-walk
  failure was swallowed and `recursive_size` returned 0 anyway —
  the progress bar would then drift past 100% mid-copy. Errors now
  propagate so copies bail with `fs_copy_walk_failed` before any
  bytes move.
- **`fs_op` slot allocation is strict first-fit, no eviction.** When
  all `MAX_FS_OPS` slots are busy, new ops are now refused with
  `fs_copy_too_many_inflight` instead of evicting an in-flight op.
  Eviction silently blinded the displaced op's progress poll and
  surfaced as a confusing "404 op_id not found" mid-copy in the
  client; refusing the new op lets the client retry once a slot
  frees, with a clear reason.
- **Engine error responses now carry the full anyhow chain.** All
  `ps5_*` handlers format errors with `{e:#}` instead of `e.to_string()`,
  so the root cause (e.g. `connection refused (os error 111)`) reaches
  the client log instead of just the outermost wrapper.
- **Library Move modal pre-flights the destination.** A debounced
  `FS_LIST_DIR` probe fires as the destination resolves and warns
  inline if the path already exists — catching both name clashes
  against an unrelated folder and stale partials left behind by a
  cancelled move on exfat USB. The payload's own
  `fs_copy_dest_exists` check is still the real backstop.
- **Activity tab gains a "Clear running" button.** Drops orphaned
  running rows whose underlying op already died (engine restart,
  payload disconnect, app killed mid-op) without firing any cancel.
  Per-row Stop is still there for actually-cancellable in-flight ops.
- **Friendlier handling of pre-2.2.16 payloads.** When the client
  sees `decode FS_OP_STATUS_ACK body` (the truncated-JSON bug fixed
  in 2.2.16), it now stops polling at 2 Hz, surfaces a
  payload-refresh hint in the activity row, and tells the user to
  hit "Replace payload" or run `make send-payload`.
- **`make run-client` fails fast on missing GTK/WebKit dev libs.**
  A new pre-flight (`_check-tauri-system-deps`) runs before
  `tauri dev` on Linux and prints a copy-pasteable
  apt/dnf/pacman command if any of the system libraries Tauri's
  native crates link against are missing. Saves ~30s of cargo
  compile time before the inevitable pkg-config wall of errors.
- **Cross-platform stale-process sweep.** New
  `scripts/kill-stale-client.mjs` is a `make run-client` dependency:
  on Linux/macOS it `pkill`s leftover `ps5upload-desktop` /
  `ps5upload-engine` and frees `:1420`; on Windows it does the same
  via `taskkill` + `netstat` + `Get-CimInstance`. The POSIX branch
  uses the `[p]` regex bracket trick so `pkill` doesn't SIGTERM the
  calling `make` recipe.

---

## 2.2.17

**Cross-platform hardening + lower memory paths**

- **Windows persistence and update downloads now replace existing files
  correctly.** Tauri-side JSON stores, the user settings mirror,
  downloaded update archives, and bundled-payload extraction now use a
  shared replace helper that handles Windows' `rename()` behavior when
  the destination already exists.
- **Payload send/probe paths no longer read whole files into memory.**
  Manual payload sending streams in 64 KiB chunks with a safety cap,
  and payload probing reads only the first 512 KiB needed for signature
  detection.
- **Bundled payload extraction is now streaming and hash-stamped.**
  The desktop app no longer rereads the cached `.elf` to compare
  bytes on launch; it streams gunzip output to a temp file, validates
  ELF magic, caps decompressed size, and records a BLAKE3 stamp.
- **Updater URL validation is stricter.** Manifest and download URLs
  are parsed as URLs and only allow HTTPS, except local loopback HTTP
  for staging.
- **Release CI treats Windows ARM64 as required.** All six supported
  desktop release targets must build successfully before a release is
  published.

---

## 2.2.16

**Cross-mount move shows progress again**

- **Fix: cross-mount moves (e.g. `/data/games/CUSAxxxxx` → a USB
  stick) lost their speed/byte counter halfway through.** The engine
  was 502'ing every status poll with `decode FS_OP_STATUS_ACK body`,
  so the row's "0 / 0 — paused" never advanced. Root cause was on
  the payload side: `handle_fs_op_status` assembled the JSON
  snapshot in a 768-byte buffer, but the two paths it has to embed
  can each be up to 1024 bytes after JSON-escaping. Any move whose
  source + destination paths summed past ~600 bytes silently
  truncated the response mid-string, producing invalid JSON the
  engine couldn't parse — and `setMoveProgress` only fires on
  successful polls, so the UI just froze. Buffer is now 2560 bytes
  (room for both maxed-out paths plus the skeleton), and on the
  off-chance we ever overflow again the payload now fails loud with
  an `fs_op_status_body_overflow` error frame instead of clipping
  silently.
- Same-mount moves were unaffected — `rename(2)` finishes in
  microseconds and never polls.

---

## 2.2.15

**i18n — Activity tab + Settings keep-awake hint + OperationBar**

- **Activity screen now translates everything visible.** Title and
  description, "Clear history" button + confirm dialog, "Running
  now" / "Recent" section headers, "No activity yet" empty state,
  per-row labels (From/To, "{N} files" badge, outcome label
  Running/Done/Failed/Stopped, "avg X/s"), relative-time strings
  ("just now", "{N}m ago", "{N}h ago"), and the Stop button
  tooltip — all gated through `tr()` and defined in every locale.
- **Settings keep-awake hint translates.** The descriptive text
  under "Keep the computer awake" ("Blocks display and system sleep
  while the app is running so long uploads don't get interrupted.")
  and the "Not yet supported on this platform" warning. These had
  `tr()` calls in 2.2.13 but the keys were never defined, so every
  locale rendered the English fallback.
- **OperationBar (always-visible footer strip) translates.** "{N}
  operation(s) running", "View Activity" link, and the toggle
  button's `aria-label`. Same root cause — keys referenced but
  never defined.
- 26 new keys × 18 locales = 468 entries. Activity row's
  `formatRelative()` helper now takes `tr` as a parameter so the
  caller can pass the active translator through to the JSX render.

## 2.2.14

**i18n — sidebar nav**

- **Sidebar nav items now translate.** "Connection", "Hardware",
  "Activity", "About", and the three section headers
  ("Overview", "Workflow", "Help") had `tr()` keys (`connect`,
  `hardware`, `activity`, `about`, `nav_section_overview`,
  `nav_section_workflow`, `nav_section_help`) but the keys were
  never defined in the i18n table — every locale fell through to
  the English fallback. Added 7 keys × 18 locales = 126 entries.
  In Chinese: 连接 / 硬件 / 活动 / 关于 / 概览 / 工作流程 / 帮助.

## 2.2.13

**i18n — Connection screen + StatusBar**

- **Every visible English string on the Connection screen now goes
  through `tr()` and has a real translation in all 18 locales.**
  21 new keys added × 18 locales = 378 entries. Specifically:
  the Step 2 "Send / Resend payload" button label and its in-flight
  progress phases ("Locating ELF…", "Sending to PS5…", "Waiting for
  payload…", "Working…"), the dynamic step messages ("Port X is open
  on Y", "Payload is running on Y", "Payload not loaded yet"), the
  bundled-payload `built:` prefix, the `PS5 firmware` and `Kernel`
  labels in the connected-info block, the entire scene-tools strip
  ("Probing scene tools…", "Scene tools on Y", "refreshing", "None
  detected…"), the version-mismatch warning ("PS5 has an older
  payload than this app", body, and "Replace payload" button), and
  the `FW <ver>` prefix in the bottom status bar.
- `sendButtonLabel` was a pure helper returning English strings; it
  now takes the `tr` translator as a parameter so all four progress
  phases render in the user's locale.

**Note on remaining English in the app**

After this release, the remaining hardcoded English is concentrated
on three screens — Send Payload (top form + 13 third-party payload
descriptions + warning), Hardware tooltips (the long `kernelGet*`
disclaimer), and About (4 feature cards + credits). These will land
in 2.2.14 and 2.2.15 as separate batches to keep each diff
reviewable instead of one massive translation PR.

## 2.2.12

**Activity / OperationBar progress consistency**

- **FS paste-copy/move byte progress now reaches the Activity tab
  and global OperationBar.** Previously the in-screen banner
  showed live `bytes / total · MiB/s · %` but the cross-screen
  Activity entry stayed at 0 bytes for the whole op duration.
  The activityWiring subscriber's "early-return on no op-state
  change" guard was firing for every per-byte tick. Now the
  current-item byte counter and total mirror into the entry on
  every store mutation, so all three views (banner / Activity row
  / OperationBar row) tick in lockstep.

**i18n**

- **Upload screen drop zone fully translated.** "Drop a file or
  folder here", "Choose file", "Choose folder", the help text
  under the picker, and the page description are now wired through
  `tr()` keys (`upload_or`, `upload_drop_here`, `upload_choose_file`,
  `upload_choose_folder`, `upload_picker_hint`, `upload_description`).
  All 18 locales (en + 17) get their own translation rather than
  falling back to English. The remaining hardcoded English on
  Send-payload, Hardware tooltips, About-page features, and the
  payload list will follow in subsequent batches — there are about
  60 more keys to wire and translate, scope-limited per release
  to avoid one giant translation PR.

## 2.2.11

**Activity tab**

- **"Live progress unavailable" hint surfaces in the Activity tab too.**
  When the Library move's poller can't get FS_OP_STATUS (older
  payload missing the 2.2.7 frame handler), the activity entry's
  `error` field now carries the explanation and the Activity row
  shows it in yellow (informational, not red/failure) while the op
  is still running. Without this, the Activity tab silently showed
  "Running" with no bytes for the entire op duration — looking
  like a UI bug rather than a payload-version issue.

**Payload Playlist**

- **Per-step IP + port overrides.** Each playlist step now has
  optional `ip` and `port` inputs alongside `sleep`. Empty falls
  back to the playlist-wide IP / port entered at Run time.
  Useful for sequences that target multiple PS5s in one go (push
  GoldHEN to dev kit, then push the harness to the test kit) or
  for scene payloads bound to non-default loader ports.

## 2.2.10

**Fixes**

- **Engine no longer crashes the app on port-collision after a
  prior orphaned engine.** Symptom: `Couldn't copy to the new
  location: engine request failed: error sending request for url
  (http://127.0.0.1:19113/api/ps5/fs/copy)`. Root cause: a previous
  Tauri instance left an engine process bound to 19113; the new
  Tauri spawn's engine call panicked with `EADDRINUSE`, and
  meanwhile the orphan engine was either dead-but-still-bound or
  responding to nothing. Now: (a) the engine binary detects the
  collision, exits 0 cleanly without panicking, (b) the Tauri
  shell verifies the running engine's `/api/version` matches the
  bundled version and respawns if it doesn't — catches the case
  where an upgrade left an old-version engine running on the port.
- **Cancelled `fs_copy` cleans up the partial destination.** A
  cancelled mid-copy left half-written files in the dest, so the
  next attempt failed with `fs_copy_dest_exists`. Symptom: cancel
  Wukong move at 50% → retry → "Couldn't copy: dest exists" with
  no useful path info. The payload now `rm_rf`'s the dest tree
  on both cancel and hard-error so a retry has a clean slate.
- **New `/api/version` endpoint** on the engine returns the
  Cargo package version. Used by the Tauri shell's
  `engine_version_matches` probe at start time; can also be
  curl'd manually for debugging which engine answer'd a request.

## 2.2.9

**Activity tab improvements**

- **From / To paths shown on their own lines** instead of crammed
  into a single right-arrow detail string. Long PS5 paths
  (`/data/etaHEN/games/PPSA09519.exfat`) wrap cleanly now.
- **Stop button on running rows.** Each in-flight Activity entry
  has a Stop button that dispatches to the appropriate cancel
  mechanism: `fsOpCancel` for ops with a stored op_id (Library
  moves and FS pastes), or the relevant store's reset/cancel
  action for transfer/download/bulk-delete jobs. Means you can
  stop a running operation from the central Activity view, not
  just from the screen where you started it.
- **File count shown** when the op covers multiple items
  (e.g. "Copying 3 items" gets a "3 files" badge).
- **Library row ops now record into Activity.** Library move was
  using component-local state and never appeared in the Activity
  tab or operations bar. Now it records start/progress/end like
  the FS bulk + transfer paths, so a long Wukong move shows up in
  the always-visible bar with bytes/percent/speed.

**Fixes**

- **"Live progress unavailable" hint when payload is older than the
  app.** If the running PS5 payload predates 2.2.7's `FS_OP_STATUS`
  frame, the engine's poll returns `unsupported_frame`. The
  Library row's busy banner now surfaces a clear hint pointing the
  user at "Replace payload" on the Connection screen, instead of
  silently retrying and showing only an elapsed counter (which
  looks like a UI bug).

## 2.2.8

**Fixes**

- **Library "Move" now shows live progress + speed and has a Stop
  button.** Same treatment FS paste got in 2.2.6: the Library row
  generates an op_id, passes it to the FS_COPY frame, and runs an
  FS_OP_STATUS poller alongside so the busy banner shows
  `bytes_copied / total · NN MiB/s · NN%` instead of just an
  elapsed counter. Stop fires FS_OP_CANCEL so the in-flight copy
  bails within ~one 16 MiB buffer (sub-second on PS5 NVMe). Was
  using the pre-2.2.6 sync path which left a multi-minute move
  showing only spinner + minutes elapsed with no way to abort.

## 2.2.7

**Performance**

- **PS5-internal copy/paste throughput uncapped from `read+write` to
  `max(read,write)`.** The old `cp_rf` did `read 4 MiB → write 4
  MiB` serially per buffer. For an internal copy from `/data` (NVMe,
  ~800 MiB/s read) to `/mnt/usb0` (USB exFAT, ~30 MiB/s write), that
  pattern means the read happens during a window where the writer
  is idle (and vice versa) — observed throughput was ~21 MiB/s,
  which is below the destination's actual sustained write speed.
  The new copier runs the write on a worker thread fed by a
  double-buffered ring (16 MiB slots, two slots), so reads and
  writes overlap. Throughput is now bounded by the slower of the
  two devices instead of summed; for the user-reported 21 MiB/s
  USB-exFAT case, expect to land at whatever the drive's true
  write ceiling is. Buffer size also bumped from 4 MiB → 16 MiB to
  let the kernel batch larger contiguous writes for exFAT/USB
  controllers.

## 2.2.6

**Features**

- **Real cancel + live progress for PS5-internal copy/move.** The
  bulk-op banner now shows bytes-copied / total / speed / percent
  for each item being copy-pasted (e.g. `PPSA09519.exfat · 9.28 GiB
  / 28.0 GiB · 187 MiB/s · 51%`), and clicking Stop interrupts
  the in-flight copy within ~one disk IO (sub-second on PS5 NVMe)
  instead of waiting for the multi-GiB copy to run to completion.
  Previously a 28 GiB Stop was honored after-the-fact; now the
  payload's `cp_rf` checks a cancel flag every 4 MiB and bails
  cleanly. New protocol frames `FS_OP_STATUS` + `FS_OP_CANCEL` on
  the mgmt port carry the engine's poll/cancel requests. Per-item
  progress bar renders in the banner once the first status reply
  arrives.
- **App-wide operations bar.** Persistent footer indicator across
  every screen showing in-flight ops (count + topmost label).
  Click to expand — per-op rows with elapsed time + bytes + speed
  refreshing every second. Click "View Activity" to jump to the
  full history.
- **Activity tab.** New screen (sidebar between FAQ and Logs) that
  records the last 100 operations with start time, duration,
  outcome (Done / Failed / Stopped), bytes moved, and error
  messages where applicable. Persists across app restarts via
  localStorage. "Running now" section at the top with live tickers;
  "Recent" below sorted newest-first. Includes a Clear button.
- **Stop on file system bulk ops, downloads, and uploads.** Every
  long-running operation surface now has a Stop button: FS bulk
  delete + paste, Library row download, Library row delete /
  chmod / mount / unmount, single + multi-file uploads. Granularity
  matches the underlying RPC: real per-file cancel where supported
  (copy/move via the new protocol), best-effort stop-watching for
  the others (engine-side cancel API for transfer jobs is future
  work).
- **Speed shown wherever bytes are tracked.** Upload screen, FS
  download banner, Library row download, the new operations bar,
  and the Activity tab all render `bytes/sec` derived from
  bytes-so-far ÷ elapsed.

## 2.2.5

**Fixes**

- **Payload no longer crashes when a client disconnects mid-transfer
  or mid-RPC.** This was the single biggest cause of the "payload
  crashes during file system operations and uploads/downloads"
  reports. The payload had no `SIGPIPE` handler, so any write to a
  TCP socket whose peer had already closed (browser tab reload,
  upload cancel, network blip) delivered SIGPIPE — whose default
  disposition is *terminate the process*. The payload now ignores
  SIGPIPE; failed writes return `-1`/`EPIPE` cleanly, the handler
  closes the socket, and the accept loop continues. Existing
  in-flight transfers on other connections survive.

**Internals**

- **JSON string-field parser now handles `\\` escapes.** The hand-
  rolled `extract_json_string_field` used to truncate a value at
  the first literal `"` byte regardless of escaping. PS5 paths
  can't contain `"` in practice, but a malformed body could
  silently produce a wrong path that downstream `unlink` /
  `rename` then operated on. Robustness fix; no user-visible
  behavior change for normal traffic.

## 2.2.4

**Fixes**

- **Delete and Unmount confirms now work.** Clicking Delete on a
  `.ffpkg` (or any FileSystem entry) used to fail with `Command
  plugin:dialog|confirm not allowed by ACL`. The native browser
  `confirm()` is a no-op inside the Tauri webview and was falling
  through to a permission-gated dialog plugin. Replaced by an
  in-app modal that also handles Escape / Enter, has proper ARIA
  for screen readers, and restores keyboard focus on close.
- **Internal copy / cut of large game images no longer fails at
  30 seconds.** Copying a multi-GiB exfat or game folder
  inside the PS5 (e.g. between `/data` and an attached M.2)
  surfaced as `engine HTTP 502 Bad Gateway: read frame header`
  partway through. Engine → PS5 socket timeout for `fs_copy`
  and `fs_move` is now a 1-hour deadline so the operation has
  time to complete. Per-byte progress still requires a
  payload protocol change — the bulk-op banner shows elapsed
  time, item name, and total size during long copies.
- **Engine HTTP errors surface real diagnostics.** Failed engine
  calls used to collapse into the unhelpful `engine returned
  invalid JSON: error decoding response body`. The Tauri proxy
  now reads the body first and surfaces the actual HTTP status
  + the engine's `{"error": ...}` field, so messages like
  `dest_dir is not a directory` or `payload rejected
  FS_LIST_DIR` reach the user instead of being hidden.
- **Hung PS5 no longer wedges the reconcile loop.** Safe-mode
  reconcile (size + BLAKE3 verify) inherited the engine's 30 s
  default socket timeout per file; one stuck PS5 stalled the
  whole loop linearly. Reconcile now uses a 10 s per-file
  deadline so a stuck file fails fast and the loop continues.
- **Engine no longer stays Running after a transfer panic.**
  A panicked transfer/download/reconcile closure used to leave
  its job state on `Running` forever. The job map now
  transitions to `Failed` on Drop (via an RAII guard) so the UI
  surfaces the failure instead of spinning indefinitely.
- **Engine logger ring no longer cascades panics.** A single
  panic in any holder of the jobs lock could poison the mutex
  and crash every subsequent ticker spawn. Lock acquisition is
  now poison-safe across all four sites.
- **Connection screen step state no longer mismatches host on
  rapid IP changes.** The auto-heal effect now ignores a
  payload "up" verdict that came from a previous host, so
  changing IP doesn't briefly show the old probe's result
  attributed to the new address.
- **Translation keys for the new dialogs land in 18 languages**
  (English plus the existing 17 locales) so non-English users
  see translated copy instead of the dev-fallback English.
- **Tauri capability tightened.** Removed the wider
  `shell:default` permission; only `shell:allow-open` remains
  (used for opening external URLs from the renderer). No
  user-visible behavior change, just narrower defense-in-depth.

**Internals**

- **Payload TX-table per-slot mutex.** Every `runtime_tx_entry_t`
  slot now has a parallel `pthread_mutex_t` outside the struct
  (so `memset` on slot eviction can't corrupt it). SHARD,
  COMMIT_TX, ABORT_TX, and the takeover/shutdown teardown all
  acquire/release the per-slot mutex; eviction in
  `runtime_alloc_tx_entry` uses `pthread_mutex_trylock` so it
  skips slots a handler is still touching. Closes a class of
  use-after-free races where a concurrent COMMIT could free
  `entry->manifest_index` underneath an in-flight SHARD.
- **Manifest index lookup hardened.** `lookup_manifest_index`
  now uses overflow-safe arithmetic (`shard_seq - s >= ec`
  rather than `shard_seq >= s + ec`) and bounds-checks the
  `path_offset + plen` memcpy against the blob length, so a
  crafted manifest can't trigger an OOB read.
- **Watchdog log on stuck takeover/shutdown.** If
  `runtime_mark_active_transactions`'s teardown takes more
  than 5 seconds (likely a stuck pack-worker syscall), the
  payload now logs the wedged tx_id so operators have a
  breadcrumb instead of guessing.
- **Per-screen confirm modal extracted to a hook.** New
  `useConfirm()` returns an imperative `confirm(opts) =>
  Promise<boolean>` plus a `dialog` ReactNode for rendering.
  Library and FileSystem screens share one implementation with
  focus restore, ARIA `alertdialog`, and i18n.
- **Bulk-delete and library move-retry extracted into pure
  functions** (`lib/bulkDelete.ts`, `lib/deleteWithRetry.ts`)
  with 15 new vitest unit tests. Failure policy (continue on
  error, report first; 3 attempts with 500 ms × n linear
  backoff) is now centralized.

## 2.2.3

**Fixes**

- **Upload destination dropdown no longer shows phantom drives.** When
  nothing was plugged into the M.2 / USB slots, `/mnt/ext0` and
  `/mnt/usb0` still appeared as upload targets — the dropdown was
  union-merging a hardcoded fallback list on top of the live volume
  probe even after the probe answered. Now matches the Volumes
  screen: only writable, currently-attached drives.

## 2.2.2

**Fixes**

- **App upgrades now actually replace the bundled PS5 payload.** When
  upgrading to 2.2.1, the desktop app kept sending the cached 2.2.0
  payload to the PS5 because the cached-vs-embedded check compared
  only file length — and a patch-version bump changes the version
  string inside the ELF without changing its length. The check now
  compares bytes when lengths match, so any embedded change forces a
  re-extract on next launch. This also clears any user who got stuck
  on a 2.2.0 payload after upgrading to 2.2.1.

## 2.2.1

**Fixes**

- **Multi-GiB single-file uploads no longer load the file into RAM.**
  The 2.2.0 fix used a memory map; this one streams a single shard-
  sized buffer (~32 MiB) for the entire upload instead. Avoids the
  Windows page-cache pressure and 32-bit address-space failures that
  could surface on huge `.exfat` / `.ffpkg` images. Peak RAM is now
  bounded by the shard size, not the file size.
- **Management calls now route to the right port.** `addr` overrides
  passed from the desktop client to handlers like `cleanup`,
  `list-dir`, `volumes`, `fs/*`, `hw/*` were going to the transfer
  port `:9113` instead of the management port `:9114`. Routing is
  now consistent across every endpoint.
- **Windows folder uploads land at the right PS5 path.** Backslashes
  in relative paths were being preserved as-is; they're now
  normalized to forward slashes before joining onto the PS5
  destination root.

**Quality of life**

- **Folder uploads can exclude files.** The desktop client's
  exclude-rules UI now feeds an `excludes` list end-to-end through
  reconcile and walk, with `.DS_Store`, `Thumbs.db`, `desktop.ini`,
  `.git/**`, and `*.esbak` as the default suggestions.
- **Image uploads can mount on completion.** After the transfer
  commits, the desktop UI offers to mount the staged image via the
  PS5 kernel's LVD backend in one step.
- **Six platform bundles, not three.** `make dist-mac-x64`,
  `make dist-win-arm`, and `make dist-linux-arm` now build for the
  three architectures the 2.2.0 release dropped silently. The
  Tauri build script picks the right per-target sidecar binary.
- **Engine sidecar restarts cleanly.** If the desktop app finds a
  registered child but no listener on `:19113`, it now kills the
  stale child and respawns instead of failing the readiness probe.

**CI**

- New target-matrix job runs `cargo check` for every shipped OS/arch
  combination on every PR (compile-only, fast). Cross-arch test
  execution stays on the host job to keep PR feedback under a minute.

## 2.2.0

**Highlights**

- **In-app update check.** Settings → Updates checks GitHub once a day.
  When a new version is out, click Download and the archive lands in
  your Downloads folder — replace the app manually, no installer.
- **Resume survives app restart.** Close ps5upload mid-upload, reopen,
  and it picks up where it left off instead of starting over. Works
  for up to 24 hours per folder.
- **Wider PS5 firmware support.** One build now works on firmware 1.00
  through 12.70. Core transfer, mount, and file features work on every
  supported firmware; advanced process listing needs 9.00–11.60.

**Better uploads**

- **Folder uploads keep their name.** Drag `/Users/you/my-folder` onto
  `/data/homebrew` and it lands at `/data/homebrew/my-folder/`, not
  spilled into the parent. Won't clobber other titles.
- **Destination preview.** The Upload screen shows the exact PS5 path
  your files are about to land at, as you type.
- **Better error messages.** Upload and Volumes screens translate
  payload jargon into something actionable (“try again in a second”
  instead of `fs_list_volumes_getmntinfo_failed`).
- **Override retries too.** The Override upload path now retries on
  network flakes, matching Resume behavior.

**Fixes**

- **Resume actually resumes.** A payload bug was wiping half-finished
  transfers on reconnect. Fixed — the PS5 now holds onto partial data
  until you come back.
- **Fewer race conditions.** Thread-safety fixes in the payload,
  engine, and desktop app so concurrent operations don't step on
  each other.
- **Partial downloads clean up.** If an update download fails
  mid-stream, the leftover `.part` file gets removed instead of
  cluttering your Downloads folder.

**Cleanup**

- **Simpler release packaging.** Three downloads, one per platform:
  `.dmg` for macOS, `.zip` for Windows (portable `.exe` inside), and
  `.zip` for Linux (`.AppImage` inside). No installers to manage.
- **Removed Install / Launch / Uninstall UI.** Sony's install API
  wedges a userland payload; register games from a PS5-side tool
  instead.
- **Scene-tools strip.** Only shows payloads that are actually
  supported. NineS and kldload are no longer probed.
