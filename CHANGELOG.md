# Changelog

What's new in ps5upload, written for humans.

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
