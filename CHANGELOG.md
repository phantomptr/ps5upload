# Changelog

What's new in ps5upload, written for humans.

---

## 2.2.36

**Library mount/unmount UX fixes (real user reports)**

Four interlocking issues with the Mount → Unmount flow on disk
images, all reported by users. Plus a more honest top-level
description across all package metadata.

### No Unmount button after mounting at a custom path

The mountMap that drives the MOUNTED badge + Mount/Unmount button
toggle was hard-gated to mounts under `/mnt/ps5upload/`. After
2.2.25 added the user-chosen mount-point feature, mounting at e.g.
`/data/homebrew/Mafia/` left the row stuck in "Mount" state — the
volume row carried the right `source_image`, but the prefix gate
filtered it out. Dropped the prefix check so any volume with a
matching `source_image` flips the row to MOUNTED + Unmount-button.
The payload's tracker file still validates "is this our mount?" on
the unmount side, so dropping the client-side prefix gate doesn't
open a surprise-unmount path.

### Library blanks for a moment after Mount, must click Refresh

A transient race during fs_mount / fs_unmount where the kernel
mount table briefly returns zero entries was overwriting the
library's last-known state with an empty list, producing the empty
state until a manual rescan. Two fixes:

- Stale-empty guard in the refresh path: if scanLibrary returns
  zero entries when we previously had >0, only update the
  mountMap + volumes and keep the entries as-is. The next
  scheduled refresh produces a real count.
- Post-mount / post-unmount rescans now wait 400 ms before firing,
  giving the kernel time to fully propagate the new mount state
  to `getmntinfo`. This matches the timing the user-reported
  "click again, works" symptom revealed.

### First Mount click errors, retry succeeds

The lvd/md attach pipeline occasionally hits a transient driver
init or device-node race on the first mount of a session that
cleanly succeeds on the very next call (~50–500 ms later). The UI
now retries fsMount once with a 350 ms backoff before surfacing
the error, absorbing the transient without forcing the user to
click Mount twice. The second failure (real one) still surfaces
through the normal error UI.

### One mount, one place

After the mountMap fix above, the Mount button hides as soon as
the image is mounted — replaced by Unmount. So clicking Mount on
an already-mounted image isn't possible from the UI; the payload's
existing reuse-existing-mount short-circuit (returns `reused: true`
without re-attaching) was the second line of defense and stays as-is.

### Optimistic mount-state updates

Both runMount and runUnmount now optimistically update the local
mountMap before the background rescan completes, so the row's
mount badge + button label flip in the same render the user
clicks. The authoritative state from the next volumes probe
overwrites this; if for any reason it disagrees, the volumes
probe wins.

### Description cleanup

Replaced "PS5 Upload — Tauri 2 desktop client" / "PS5 Upload —
Tauri desktop shell" / the long FTX2-protocol blurb with a single
short user-facing line — "The all-in-one PS5 companion app." —
across `client/package.json`, `client/src-tauri/Cargo.toml`,
`tauri.conf.json` shortDescription + longDescription.

---

## 2.2.35

**Stale-tmp silent corruption — close the resume-path gap (real user
report)**

A user reported that 2.2.29's stale-`.ps5up2-tmp` fix didn't actually
solve their corruption: upload reports success, the destination
files are silently wrong, the same source folder transferred over
FTP launches fine. The 120-GB game was Mafia: The Old Country.

The 2.2.29 fix swept stale tmps at `BEGIN_TX` time, but only on the
**`!is_resume`** branch (`runtime.c:7468`). Resumes intentionally
preserve partial tmps so they can be picked up where they left off
— but that means a stale tmp from a *prior aborted run* of the same
destination, where some file was non-packed then but is packed now,
survives into the resumed tx. The pack worker writes correct
content directly to the file path; the COMMIT rename loop then
promotes the stale `.ps5up2-tmp` over the just-written content,
silently. FTP doesn't have this layer at all, so it just works.

Two defense-in-depth fixes:

- **Pack worker unlinks stale tmps after a successful write**
  (`runtime.c` pack pool, ~line 2010). After the pack-direct write
  to `<file>` completes, also `unlink("<file>.ps5up2-tmp")`. This
  attacks the corruption at the moment it would otherwise be
  created, regardless of whether the BEGIN_TX sweep ran. ENOENT is
  the common case and not an error.

- **COMMIT rename loop pre-checks the destination**
  (`runtime.c:7965`). Before renaming `<file>.ps5up2-tmp` →
  `<file>`, stat both and compare against the manifest's expected
  size. Only when **both** layouts hold:
    - file exists at expected size (packed/prior-run delivered it)
    - tmp exists but is *strictly smaller* than expected (stale partial)
  …unlink the tmp instead of renaming. The asymmetric size check
  avoids the false positive where the user is replacing an existing
  same-size file (in that case the new tmp holds the full new
  content, the rename runs as normal, and the new content lands).
  Legitimate resumes also satisfy `tmp_size == expected` by COMMIT,
  so this guard only catches the bug case.

Both fixes are independent of `is_resume` so the resume path
regains the same protection that fresh-tx had since 2.2.29. Logged
at `stderr` when the COMMIT pre-check fires, so future bug reports
will surface the exact path that hit the guard.

The `BEGIN_TX` sweep is still the first line of defense; these are
the safety net for the cases where it can't run (resume) or might
miss (path encoding drift between manifest and disk).

---

## 2.2.34

**Library → Game Details: PS4 (BC) titles now show cover art too**

The PS5 plays PS4 games via backwards compatibility, so the Library
can contain CUSA##### entries alongside PPSA##### ones. 2.2.33's
PROSPEROPatches integration only resolved PS5 titles; PS4 titles
silently fell through to "no remote metadata."

Title-id prefix routing (per
[psdevwiki Title ID](https://www.psdevwiki.com/ps5/Title_ID) /
[PS4 Title ID](https://www.psdevwiki.com/ps4/Title_ID)):

- **PPSA#####** → PS5 → `prosperopatches.com` (PROSPEROPatches)
- **CUSA#####** → PS4 → `orbispatches.com` (ORBISPatches)
- Any other prefix (PCSA, NPXS, etc.) — no upstream queried; the
  modal shows the local `param.json` row only.

The `<title>` parser now strips both the `TITLEID:` prefix and the
optional ` | sitename` suffix the upstream pages add (PROSPEROPatches
omits it, ORBISPatches appends ` | ORBISPatches.com`). The cover-host
allowlist regex is per-platform — even if the page's `<meta>` tag
were tampered with, we still won't accept an image URL that doesn't
match the platform's own CDN (`cdn.prosperopatches.com` for PS5,
`cdn.orbispatches.com` for PS4).

The "View on …" button label is now dynamic so PS4 titles read
"View on ORBISPatches" and PS5 titles read "View on PROSPEROPatches"
— both open the title's full patch page in the user's browser.

CSP `img-src` whitelisted `cdn.orbispatches.com` alongside the
existing `cdn.prosperopatches.com`. The Rust-side `title_meta_fetch`
allowlist now contains both upstream hosts.

---

## 2.2.33

**Library → Game Details: switch metadata source to PROSPEROPatches**

The PSN Store endpoints we hit in 2.2.32 are gated and rate-limited
in ways that made cover art unreliable in practice. Replaced the
backing source with [PROSPEROPatches](https://prosperopatches.com),
a public PS5 title-info site, and rebuilt the modal around what it
exposes:

- **Cover art** scraped from the page's `<meta name="twitter:image">`
  tag, served by `cdn.prosperopatches.com`. Whitelisted only this
  CDN host in the renderer CSP `img-src`; the previous PSN
  whitelist was removed.
- **Display title** scraped from the `<title>TITLEID: Name</title>`
  tag.
- **"Search PSN Store" button** → **"View on PROSPEROPatches"**.
  Clicking opens `https://prosperopatches.com/<titleId>` in the
  user's browser via `openExternal`, where the full patch list and
  publisher info are visible.

The fetch still goes through a Rust-side Tauri command (renamed
`psn_fetch` → `title_meta_fetch`) with a hostname allowlist
(`prosperopatches.com` only) acting as SSRF defense, a polite
User-Agent identifying the app + version, an 8 s timeout, and a
1 MiB body cap.

Modal fields that PROSPEROPatches doesn't expose (description,
genres, age rating, publisher chip) were removed from the modal —
the local `param.json` info row already covers what's needed for
on-disk identification, and removing the dead JSX keeps the modal
honest about what it can actually show.

Files renamed for accuracy: `client/src/lib/psnDetails.ts` →
`titleDetails.ts`, `client/src-tauri/src/commands/psn.rs` →
`title_meta.rs`. Cache localStorage key migrated from
`ps5upload.psn.cache` → `ps5upload.titleinfo.cache` so stale PSN
entries don't shadow the new shape.

---

## 2.2.32

**Fixed: Library → Game Details modal — cover art and "Search PSN
Store" button**

Two adjacent bugs in the same modal:

- **No cover art available.** The renderer's `fetch()` to PSN's
  `valkyrie-api` and `chihiro/titlecontainer` endpoints was being
  blocked by the webview CSP `connect-src` whitelist (PSN domains
  weren't listed), and even if widened the cross-origin response
  wouldn't have satisfied the webview's CORS policy. The fetch is
  now routed through a Rust-side `psn_fetch` Tauri command — the
  request is issued from the desktop process, so neither CSP nor
  CORS apply. A hostname allowlist (`store.playstation.com` only)
  is enforced server-side as SSRF defense, and the response body is
  capped at 1 MiB.
- **Search PSN Store button did nothing.** The `<a target="_blank">`
  pattern doesn't open externally in Tauri 2's webview; the click
  was a silent no-op. Replaced with a `<button onClick>` that calls
  `openExternal()` from `@tauri-apps/plugin-shell`, matching every
  other external-link affordance in the app.

CSP `img-src` widened to whitelist `image.api.playstation.com` and
`*.playstation.{com,net}` so the cover-art `<img>` tag can render
the URLs once the API fetch returns them.

---

## 2.2.31

**New: Install Package tab — install `.pkg` files via BGFT, no third-party loaders required**

A new sidebar tab between Upload and Library. Drag-drop or browse
for `.pkg` files (single or split-pkg `<base>.pkg` + `<base>.pkg.0`,
`<base>.pkg.1`, ...). Each picked file is parsed for metadata
(content_id, title from PARAM.SFO, category, icon) and added to a
queue. The queue runs sequentially — one BGFT install at a time on
the PS5 — and persists across app restarts via localStorage.

**How it works**: ps5upload's host app spins up an HTTP listener on
the same port as the engine (default 19113), hands the PS5 a
`http://<pc-ip>:<port>/pkg-host/<session>/file.pkg` URL, and tells
the payload to call `sceBgftServiceDownloadRegisterTask` +
`sceBgftServiceIntDownloadStartTask`. Sony's BGFT service in PS5
firmware fetches the bytes from our HTTP server with Range
requests, decrypts with the device's own keys, and installs to
`/user/app/<title-id>`. Progress is polled from BGFT every second
and surfaced in the queue UI.

**Prerequisites — exactly these and nothing else**:
- kstuff loaded (kernel R/W, same as everything else ps5upload does)
- ps5upload payload running on `:9114` mgmt port

No third-party loader required. The payload calls Sony's BGFT
directly using the debugger authid that kstuff already grants us.

**Split-pkg support**: pick the lead `<base>.pkg` and the engine
auto-detects siblings, then serves them as a single virtual file
to BGFT via HTTP-Range mapping that crosses part boundaries on
the fly. No on-disk concatenation; no double-write.

**Error handling**: BGFT's well-known error codes are mapped to
user-facing messages in the queue rows:

| Code | Message |
|---|---|
| `0x80990088` | This title is already installed |
| `0x80990085` | Need defragmented free space — Settings → Storage → Free up space |
| `0x80990039` / `0x80A30026` | Out of free space |
| `0x80990086` | Leftover download in notifications — clear it from PS5 first |
| `0x80990036` | DRM mismatch — this PKG isn't valid for this console |

Plus engine-side sentinels (`0xE0000001..`) for "BGFT unavailable
on this firmware" diagnostics.

**Non-standard PKG handling**: if the magic bytes aren't the stock
`\x7FCNT`, the queue still accepts the file and surfaces a yellow
caution row showing the actual magic. BGFT decides whether to
accept it; the host doesn't pre-reject. Keeps community FPKG
variants working without manual overrides.

**Hardware validation**: the install-end-to-end path needs
hardware testing on FW 9.60 + a real `.pkg`. Parser, HTTP serving,
queue UI, frame protocol, and `-Werror` payload build all clean
host-side. The first user with a working `.pkg` to test will tell
us whether `libSceBgft.sprx` exports + authid expectations match
what we implemented; if not, the engine surfaces a structured
diagnostic via `bgft_install_unavailable_reason()`.

**Code paths added**:
- `engine/crates/ps5upload-pkg` — new crate, PKG header parser
- `engine/crates/ps5upload-engine/src/pkg_install.rs` — sessions, HTTP Range, route handlers
- `engine/crates/ps5upload-core/src/pkg_install.rs` — payload client + err_code mapping
- `payload/src/bgft.c` + `payload/include/bgft.h` — sceBgft* bindings
- `payload/src/runtime.c` — PKG_INSTALL + PKG_INSTALL_STATUS handlers
- `engine/crates/ftx2-proto/src/lib.rs` — opcodes 82-85
- `client/src/screens/InstallPackage` — UI
- `client/src/state/installQueue.ts` — Zustand store + worker loop
- `client/src/layout/Sidebar.tsx` + `App.tsx` — nav

---

## 2.2.30

**Misleading "all strategies failed" error after a successful launch**

User report: clicking Launch on a registered title actually starts
the game (you can see it on screen) but the UI surfaces:

> engine HTTP 502 Bad Gateway: payload rejected APP_LAUNCH:
> launch_all_strategies_failed: param=1 null=1 sys=1

This is now fixed.

**Root cause**: the launch path tries `sceLncUtilLaunchApp` via a
ptrace remote-call into ShellUI (the canonical FW-9.60 path). When
the launcher accepts the call, it signals ShellUI in a way that
races our `waitpid` cleanup — `pt_call` sees a non-stopped wait
state and returns `-1`, which the caller couldn't distinguish from
"couldn't even attach to ShellUI." The launch fallback chain then
ran the in-process direct calls (param / null / sys), all three of
which failed because the game was already in the middle of
launching. Result: misleading error, even though the launch
succeeded.

**Fix**: `pt_call` now sets a thread-local "dispatched" flag the
moment `pt_continue` returns cleanly (i.e. the remote function was
actually invoked). `shellui_rpc_launch_app` reads the flag after
`pt_call` returns -1 and distinguishes:

- **dispatched=0**: pre-call failure (couldn't attach / mmap /
  setregs). Function never ran. Returns -1; caller falls through
  to in-process strategies.
- **dispatched=1**: function was invoked but post-call cleanup hit
  the race. Returns a new -2 sentinel meaning "soft success — game
  most likely launched, result uncertain."

`launch_title` treats -2 as success and **stops trying the
in-process fallback** in that case. The fallback would only race a
running launch and produce the misleading error. Pre-dispatch
failures still fall through to in-process as before, so the
no-kstuff / missing-symbol cases still get the best-effort try.

**No false-positive risk for the dispatched=1 case**: if the launch
genuinely failed Sony-side after dispatch, the error is visible on
the PS5 screen anyway (no game appears) — preferring an honest
soft-success message over a confidently-wrong "all strategies
failed" matches what users expect.

This complements the audit-pass discipline: real user reports
surface bugs that static auditing misses. The pt_call return-code
ambiguity wasn't visible from local code review — it took a hardware
reproduction with the launcher's actual signalling pattern.

---

## 2.2.29

**Two critical fixes from real user reports**

Same-day patch on top of 2.2.28's audit pass. Two distinct user
reports surfaced two distinct bugs — neither was caught by the
2.2.28 audit. Thanks to the reporters; both are real, both bite
in normal use, and both are fixed here.

### Stale-tmp overwrites correct content on multi-file uploads

**Symptom**: upload reports success, but the uploaded files are
silently corrupt — game won't launch, even though the same files
sent over FTP work fine.

**Root cause**: in multi-file direct mode (folder uploads), each
non-packed file gets a `<path>.ps5up2-tmp` during transfer; on
COMMIT, every manifest entry's tmp is renamed to its final path.
Packed records (small files, written via the pack worker) write
directly to `<path>` with no tmp. The bug: if a *prior aborted
upload* of the same destination left `<path>.ps5up2-tmp` files on
disk, the next fresh BEGIN_TX did NOT clean them up. When the new
upload's pack worker wrote correct content directly to `<path>`,
the COMMIT rename loop then promoted the *stale* tmp from the
aborted run over the freshly-correct content. Client saw
`commit_tx_ack: success`, destination had stale bytes.

The single-file path already cleaned its tmp on fresh BEGIN_TX
(line 7270 of runtime.c); multi-file mode was missing the
equivalent sweep. Now iterates the manifest at BEGIN_TX time and
unlinks each `<file>.ps5up2-tmp` before any shard arrives.
Resume-flag-set BEGIN_TX intentionally skips the sweep (resume
needs the partial tmps preserved to be useful).

### Throughput collapse from 60 MiB/s to 2-3 MiB/s on multi-GB uploads

**Symptom**: large file uploads start fast (~60 MiB/s) and a few
minutes in drop to ~2-3 MiB/s and stay there. v1.5.x didn't have
this regression but its loader is no longer compatible.

**Root cause**: the transfer write path used `ftruncate(fd,
total_bytes)` to "pre-allocate" the destination. On PS5 UFS this
creates a *sparse* file: the file size is set, but no blocks are
actually allocated. Each subsequent shard write is a "first write
to a fresh region" which forces the kernel to allocate the block,
update the bitmap, dirty an indirect block, and journal the
metadata. After enough writes, the dirty-buffer threshold is hit
and writes start blocking on writeback. The 25-30× slowdown is
classic sustained-write throttle behaviour.

`runtime_write_shard_persistent` and `runtime_write_shard_to_path`
now use `posix_fallocate(fd, 0, total_bytes)` which pre-allocates
all blocks in a single batch. Subsequent shard writes only update
content — no metadata churn, no dirty-buffer pressure beyond the
data itself. Falls back to `ftruncate` on filesystems that don't
support fallocate (exfat, fuse, NFS-mounted).

This matches the pattern `cp_rf` already uses (line 4635: "reserve
contiguous dst extents up-front"). The transfer path was simply
missed when that migration happened. Pack worker writes are
unchanged — small per-file writes don't benefit from fallocate.

### Verification

Both fixes are confirmed via local rebuild + 88 engine tests + 7
Tauri tests + 200 client vitest, plus a 50-iteration verify pass
focused on edge cases of both code paths (resume interaction,
restart interaction, packed-vs-non-packed mix, malformed
manifest, ENOSPC fallback, empty-manifest, off-by-one).

---

## 2.2.28

**Audit pass — 27 bug fixes across payload, engine, and client**

This release is the result of a multi-pass static audit covering every
component (PS5 payload, Rust engine, Tauri desktop client, scripts,
build) and a fresh look at the failure modes each tier handles. Most
fixes close gaps that wouldn't surface in normal use but could bite
under specific failure conditions (mid-transfer disk error, hostile
input, edge-case resume).

**No behavior change for the happy path.** Every successful upload,
mount, register, or hardware read works exactly the same as 2.2.27.

### High-impact fixes

- **COMMIT_TX no longer silently corrupts on writer error.** When the
  PS5-side disk write reported an I/O error mid-shard, the commit path
  would log a warning and rename the partial tmp file over your good
  destination anyway, then report success. Now the commit refuses the
  rename, leaves the destination untouched, preserves the partial at
  `.ps5up2-tmp` for inspection, and surfaces a structured
  `direct_writer_io_error` (or `direct_rename_failed` / `manifest_missing`
  / `direct_multi_rename_failed` / `spool_apply_failed`) to the client.
- **Single-file direct-mode resume now actually works.** Resume of an
  interrupted single-file upload had a long-standing latent bug where
  the tmp file's fd wasn't reopened after ephemeral teardown — every
  shard write hit `EBADF` and the resume failed end-to-end. The reopen
  path now uses `O_APPEND` so previously-acknowledged shards' bytes are
  preserved on disk while new shards land at end-of-file.
- **Spool fallback no longer corrupts >60-file uploads.** The 2.1
  direct-mode rewrite migrated the manifest blob to heap; the spool
  fallback (used when direct mode can't engage) was missed and still
  used a 8 KiB stack buffer that truncated any real-game manifest
  mid-object. Migrated to the same heap-allocated path.
- **DRM-type patch (Register → Patch DRM) is now atomic.** The
  rewrite of `param.json` was non-atomic: a power loss between
  `fopen("wb")` truncate and `fclose` left the source file zero-length
  with no recovery path. Now writes to `.new`, fsyncs, then atomic
  rename. Also tightened the JSON key match so a nested field mention
  of `applicationDrmType` can't be matched accidentally.

### Security + integrity fixes

- **Updater asset URLs are now host-pinned.** The check + download
  flow previously trusted any HTTPS URL the manifest contained; a
  poisoned `latest.json` could have redirected downloads to an
  attacker-controlled HTTPS server (TLS being the only integrity
  boundary). Pin to `github.com` + `*.githubusercontent.com` for
  production. Loopback HTTP and an env-overridden staging URL still
  work for dev testers.
- **Updater pre-release ordering is now correct.** A user on
  `2.2.0-rc1` would never see `2.2.0` GA as available because
  `is_newer` ignored pre-release suffixes. Per-semver pre-release
  ordering (pre < GA) now decides ties on numeric equality.
- **Download path traversal blocked.** A buggy or hostile modified
  PS5 payload could have returned `FS_LIST_DIR` entries containing
  `..` or `C:` in names; the host-side `local_dest_for` would have
  blindly traversed. Components are now validated to reject `.`,
  `..`, embedded `/ \ : NUL`, and leading `\\\\`.
- **App-register response is JSON-escape-safe.** A malicious homebrew
  `param.json` with `titleId` containing `"` or `\` could have
  produced malformed `APP_REGISTER_ACK` JSON. Title IDs are now
  escaped before interpolation.

### Robustness + UX fixes

- **Reconcile no longer aborts on a single hash error.** A flaky USB
  drive or one stale-permission file used to fail the entire
  reconcile pre-check. Per-file hash failures are now treated as
  "must re-send" — one extra file goes to the upload list instead of
  blocking the whole session.
- **Stop button now stops mid-walk.** `recursive_size` (used by
  FS_COPY and FS_DELETE pre-walks) didn't honour cancel; a Stop
  click on a 200k-file tree had multi-minute latency before the copy
  loop noticed. Cancel-check at directory boundaries makes Stop
  responsive.
- **Stop during image-mount-after-upload is consistent.** Previously,
  clicking Stop right as the engine reported `done` could let the
  image-mount call complete on the PS5 while the UI silently
  returned to idle — leaving a real mount no row showed. Now skipped
  cleanly.
- **`fs_op` slot watchdog reclaims stuck slots.** A worker that crashed
  without releasing leaked one of the 4 slots forever. After 24 h of
  inactivity, a stuck slot is reclaimed with a log warning.
- **Multi-file COMMIT now surfaces partial-rename failures.** If
  `N` of `M` rename calls failed, the commit used to report success
  silently. Now emits `direct_multi_rename_failed` with the count
  and detail so the client knows.

### Smaller fixes worth noting

- Path allowlist (`is_path_allowed`, `cleanup_path_allowed`) no longer
  rejects legitimate filenames that contain `..` substrings (e.g.
  `My..Game`). Component-scoped check, not substring.
- `instance_id` now mixes nanoseconds + pid — sub-second double-loads
  produce distinct IDs (was wall-clock seconds only).
- Ownership record write is now atomic (tmp + fsync + rename).
- `remove_recursive_path` got the same 64-level depth cap as the
  other recursive walkers.
- `walk_remote_dir` (engine-side download walker) got a 64-level
  depth cap to bound stack against PS5 directory cycles.
- `engineApi.ping()` got a 2 s timeout — a wedged engine no longer
  hangs the status indicator forever.
- `user_config_save` now cleans up its `.tmp` file on every error
  path (was leaking on `write_all`/`sync_all` failures).
- `compareVersions` (TS-side semver) no longer treats `+build`
  metadata as a pre-release suffix.
- `apply_failed` is now a recognised terminal tx state — without
  this, a slot stuck in `apply_failed` would never get evicted by
  `runtime_alloc_tx_entry` and after 32 such failures the table
  would saturate.
- `engineLogBridge` now cleans up its setInterval on Vite HMR
  re-evaluation in dev (was accumulating timers across saves).
- `ptrace_remote` authid restore failure now logs once (was silently
  `(void)`-cast despite the comment saying "log and move on").

### Documentation

- README: corrected stale claim about process-list firmware coverage
  (it's stable across all SDK-supported firmwares — `sysctl`-based
  with stable `kinfo_proc` offsets).
- README: simplified the firmware-coverage table.
- `bench/README.md`: documented the historical port drift in baseline
  JSONs (`9114` → `19113`) so reviewers don't try to "fix" them.
- `engine/.../cleanup.rs`: corrected the path doc (now
  `/data/ps5upload/tests/`, was the obsolete `-bench`/`-sweep`/
  `-smoke` triple).

---

## 2.2.27

**Load order doesn't matter anymore — kstuff after ps5upload still works**

The 2.2.26 release got the ShellUI RPC working but only when
kstuff was already loaded *before* our payload booted. If you
sent ps5upload first and kstuff afterwards, the ucred jailbreak
silently no-op'd at startup and the only fix was a PS5 reboot.

- **Fix: per-request ucred elevation retry.** The payload now
  re-applies the full ucred jailbreak on every incoming frame
  until elevation succeeds. Send kstuff at any time during the
  session and the next Hardware-tab refresh, Launch click,
  Register, or any FS op picks it up — no reboot, no re-send of
  ps5upload needed.
- **No behavior change for the working case.** When kstuff is
  loaded before the payload, elevation succeeds at startup and
  the retry path is a single branch (early-out) per request.

---

## 2.2.26

**Live sensors and working Launch on FW 9.60**

The Hardware tab now shows real CPU/SoC temperature and SoC power
readings on FW 9.60, and the Library tab's **Launch** button
actually starts the game. Both used to come back blank because
Sony's launcher and sensor APIs reject any caller that isn't
`SceShellUI`. The payload now routes those calls through a
ptrace-based RPC into ShellUI itself, so they pass the
caller-context check natively.

- **Fix: live sensor readings on FW 9.60.** CPU temperature, SoC
  temperature, CPU frequency, and SoC power draw all return real
  values now. The Hardware tab's auto-refresh tick (every 5s)
  picks up changes as the workload varies.
- **Fix: Library → Launch starts games on FW 9.60.** Hitting
  Launch on a registered title actually boots the game (SoC power
  jumps from idle to in-game as the title comes up). Previously
  every Launch returned a generic "all strategies failed".
- **Fix: process list shows real names.** PROC_LIST switched from
  a kernel-walk-with-per-firmware-offsets approach to
  `sysctl(KERN_PROC_PROC)`. Names like `SceShellUI`, `kstuff.elf`,
  `payload.elf` now show up correctly instead of `?` placeholders.
- **Hardening.** Full ucred jailbreak (uid=0, all capabilities,
  debugger authid) plus sandbox escape (rootdir + jaildir set to
  the kernel root vnode) applied at startup. Crash-time signal
  handler now best-effort-detaches any active ptrace attach so a
  payload SIGSEGV mid-RPC won't freeze SceShellUI.
- **Audit and cleanup pass.** Every FS / HW / app handler reviewed
  for malloc/free symmetry and bounds checks. Per-firmware
  `p_comm` offset table removed (sysctl path replaces it). User
  now gets the actual Sony error code on Launch failures
  (`launch_sony_error_0x<code>`) instead of a generic
  "all strategies failed".

Hardware-validated on FW 9.60 / CFI-7019. New unit + integration
tests added for the proc_list parser shapes and the FRAME_PROC_LIST
round-trip; the hardware-side smoke script now exercises HW_TEMPS
and PROC_LIST so a regression in the ShellUI RPC path lights up
immediately.

---

## 2.2.25

**Library mount picker + Library search bar**

### Library search bar

The Library used to render every game and disk image as a flat
list with no way to narrow it down — a 60-row library on a
loaded PS5 meant a lot of scrolling to find one title. Added a
live search input above the games + images sections that filters
as you type:

- Matches against `name` (folder/file name), `titleId` (the
  PPSA01342-style ID from `sce_sys/param.json`), absolute `path`,
  scan `scope` (`homebrew`, `games`, …), and `volume`
  (`/data`, `/mnt/ext1`, …). Case-insensitive.
- Multi-word queries AND-match across those fields, so
  `dead ext1` finds Dead Space on `/mnt/ext1` but not the copy
  on `/data`. Whitespace is collapsed so `dead   space` works.
- Clear button + "matched / total" count appear once a query is
  active. Empty queries return the input array reference
  unchanged (cheap memo path), so typing into the box doesn't
  re-render every row.
- Section headers (Games / Disk images) hide when their section
  has no matches; a "No matches" empty-state card appears when
  the whole library filters to zero.

The filter logic lives in `lib/libraryFilter.ts` (+11 unit
tests). Search query is transient — closes with the screen, so
no stale-filter surprises on the next visit.

### Library mount: pick where the image goes (volume + subpath + presets)

Mounting a `.exfat` or `.ffpkg` image used to drop it under a fixed
`/mnt/ps5upload/<derived-name>/` location with no user input. A few
users wanted to land mounts elsewhere — typically under a community
scan-path on an external drive so PS5 game scanners pick them up
automatically, or under `/data/<name>/` to keep everything on internal
storage. This release wires up that choice using the same UX as the
Upload screen's destination picker.

### What changed in the UI

The Library Mount button now opens a modal with three inputs:

- **Volume** — dropdown of every writable volume the PS5 reports
  (`/data`, `/mnt/ext1`, `/mnt/usb0`, …) with a free-space readout
  next to each path.
- **Subpath** — free-form text with preset chips matching the
  Upload screen, including `homebrew` (the recommended default) and
  `ps5upload` (the legacy default).
- **Name** — auto-derived from the image filename (`Dead
  Space.exfat` → `Dead Space`), editable for renames or
  normalization. No slashes.

The resolved path appears under the inputs in real time, plus a
soft warning when the chosen path is outside `/mnt/ps5upload/` —
third-party PS5 game scanners typically only scan that root, so a
mount under `/mnt/ext1/foo/` will work for the payload but may not
show up in those scanners.

The volume + subpath the user picks is persisted per-host (same
shape as the FileSystem last-path persistence shipped in 2.2.24),
so the next Mount on this PS5 opens the modal with the same
selection. Fresh installs default to the first available volume +
the `ps5upload` subpath, replicating the pre-2.2.25 behavior so
nobody's existing workflow breaks.

### Payload changes (`runtime.c`)

- `handle_fs_mount` accepts a new optional `mount_point` JSON field
  (full absolute path). When provided, the payload mounts at exactly
  that path instead of the legacy `/mnt/ps5upload/<name>/` root.
  Validated against the same `is_path_allowed` allowlist every other
  FS-mutation frame uses (`/data`, `/mnt/ext*`, `/mnt/usb*`,
  `/mnt/ps5upload/*`); paths outside that allowlist (or the
  `/mnt/ps5upload` namespace root itself) are rejected with the
  same `fs_mount_path_not_allowed` / `fs_mount_bad_mount_point`
  errors. Backward-compatible: omitting `mount_point` keeps the
  legacy `mount_name`-based path, so older clients still work.

- New `fs_mount_mkdir_p` helper handles deep mount paths
  (`/mnt/ext1/games/<title>/foo`) by creating each intermediate
  directory if missing. Existing dirs are tolerated (EEXIST
  ignored at every segment).

- `handle_fs_unmount` relaxed: was hardcoded to require the
  `/mnt/ps5upload/` prefix; now also accepts any path that has a
  matching tracker file (proof we mounted it). Still rejects
  arbitrary paths the user wasn't authorized to ask about, so a
  malicious request can't unmount Sony-managed mounts or `/data`.

- The `mount_tracker_*` family migrated from name-keyed
  (`<name>.src` only) to mount-point-keyed via a new
  `mount_tracker_key()` helper. Legacy `/mnt/ps5upload/<name>`
  trackers keep their existing keys (no migration / file rename
  needed); new user-chosen paths hex-escape every non-alphanumeric
  byte (`_HH` per byte) so `/mnt/ext1/games/foo` writes to
  `mnt_2fext1_2fgames_2ffoo.src`. The hex form is collision-proof
  — a flat `/`→`_` substitution would have made
  `/mnt/ext1/foo_bar` and `/mnt/ext1/foo/bar` map to the same
  tracker file. Reconciliation on payload startup now scans
  **all** mounts whose tracker exists (not just `/mnt/ps5upload/`)
  so user-chosen mounts also get the dev-node-gone cleanup pass
  after a reboot.

- `FS_LIST_VOLUMES` "is this our mount?" detection now checks both
  the legacy `/mnt/ps5upload/` prefix AND tracker presence, so a
  user-mounted `/mnt/ext1/games/foo` shows the source-image
  string and bypasses the placeholder-volume filters the same way
  legacy mounts do.

### Engine + Tauri shell

`fs_mount(addr, image_path, mount_name, mount_point)` — the new
`mount_point` parameter is plumbed end-to-end through the Rust
engine, the Tauri command, and the TS API
(`fsMount(addr, imagePath, { mountName, mountPoint })`). All three
existing callers (Library, transfer, uploadQueue) keep working
unchanged because the new param is optional.

### Compatibility

A 2.2.25 client talking to a pre-2.2.25 payload detects the version
mismatch via the existing `payloadVersion` probe and **hides the
volume + subpath rows in the modal**, falling back to a
name-only form (the 2.2.24 behavior). A small banner in the modal
explains the limitation and points at "Replace payload" on the
Connection screen. No silent failure; no protocol version errors.

### Audit hardening (4 review passes)

Four cross-layer audit passes after the initial implementation
caught 12 issues, all fixed in this release:

- **Payload race fix (already in the design):** `mount_tracker_key`
  uses hex-escape encoding instead of a flat `/`→`_` substitution
  so paths can't collide; `mount_tracker_read` zero-inits its
  out-buffer before the read syscall and rejects buffers smaller
  than 2 bytes (so a corrupted zero-byte tracker file can't return
  uninitialized stack memory).
- **Mount UX:** the picker's `volume` state now stays in sync with
  the live `dropdownPaths` — if the user's saved volume isn't in
  the current dropdown (drive ejected mid-session, or saved on a
  different PS5 with a different layout), the modal snaps to the
  first available rather than rendering a blank `<select>`.
  `MountModal.onConfirm` passes the modal's own `(volume,
  subpath)` state to `runMount` directly so the persistence write
  can't silently skip when `volumes` is mid-load.
- **Activity log:** the legacy mount path now patches the activity
  entry's `toPath` with the actual `res.mount_point` once
  FS_MOUNT_ACK lands; previously the row showed "From: …" with no
  destination. The `useActivityHistoryStore.start()` capping at
  100 entries now prefers evicting *terminal* entries over running
  ones — running ops can't fall off the OperationBar's tail.
- **Library row UX:** Mount button is pre-disabled when
  `entry.imageFormat` is null (no point letting the click round-
  trip an `fs_mount_unsupported_format` error). Search bar
  Escape clears the query; both Move and Mount modals close on
  Escape.
- **Stale state on Connection screen unmount:** the
  `payloadProbing` flag is cleared explicitly in the cleanup
  effect so a "rechecking…" banner doesn't latch in the store
  after navigation. (Carried over from late 2.2.24 work.)
- **Hardcoded volume bugs:** `/mnt/ps5upload` removed from the
  mount picker's `FALLBACK_VOLUMES` list because the payload
  rejects mounting at the namespace root itself.

### i18n sweep

Phantom keys (referenced in code with inline-fallback strings
but missing from the `en` dict) had grown to 73 over the v2.2.x
cycle — the existing `i18n-prune-unused.mjs` regex couldn't catch
template-literal fallbacks or multi-line `tr()` calls. This
release does the cleanup:

- **Wrote a custom auto-converter** that handles 2.2.x's three
  shapes: single-line plain-string fallbacks (existing tooling),
  multi-line plain-string fallbacks, and template-literal
  fallbacks with `${expr}` interpolation. The template-literal
  cases also rewrote 30+ `tr()` call sites in place to use the
  standard `{var}` substitution form — the en dict needs the
  static-template form to round-trip translations.
- **70 of the 73 phantoms** are now in the en dict. The
  remaining 3 are dynamic-plural / runtime-conditional cases
  (`fs_bulk_stop_tooltip` ternary, `playlist_step_count` /
  `queue_excludes` "1 step" vs "2 steps" cases) that would need
  ICU plural rules — out of scope for this pass, kept on the
  i18n-coverage allowlist.
- **Vietnamese (vi) translations** — all 94 newly-added EN
  keys translated via the existing `i18n-fill-missing.mjs`
  MyMemory-API integration. **Hindi (hi)** got ~40 of the 94
  before MyMemory's free-tier daily quota throttled the rest.
  The other 15 locales are still allowlisted for the new keys;
  re-running the script after the quota resets (or with a
  different translation backend via `TRANSLATE_URLS`) will fill
  them in. Idempotent — already-translated keys are skipped on
  the next run.
- **One TypeScript dup-key bug** caught immediately by `tsc`
  (both my converters independently wrote `connection_block_newer`
  to en); deduplicated.

### Verification

- `vitest`: **192 / 192** passed (164 from 2.2.24 + 17 new for
  `mountDest` + 11 new for `libraryFilter`).
- `tsc --noEmit`: clean.
- `eslint`: clean.
- `i18n-coverage` (18 languages): clean.
- Engine `cargo fmt --check`, `cargo clippy -D warnings`, and
  `cargo test --workspace`: 77 tests pass, no warnings.
- Tauri shell `cargo clippy -D warnings`: clean.
- Payload built clean with `prospero-clang -Wall -Wextra -Werror`.

---

## 2.2.24

**Move-progress accuracy + FileSystem volume picker + global activity coverage**

A grab-bag release driven by user reports about the FileSystem and
Library screens: a misleading move-progress banner, a clunky way to
jump between volumes, preferences that didn't survive a reload, and
in-flight operations the OperationBar didn't surface.

### Library Move stops gaslighting users on the latest payload

A user reported that moving Dead Space (PPSA03845) from `/mnt/ext1` to
`/mnt/usb0` showed *"Live progress unavailable — your PS5 payload is
older than this app"* even though the Connection card confirmed
`Payload v2.2.23 matches this app`. Two independent bugs combined to
produce that:

- **Fix (payload): the FS_OP slot was registered *after* the
  recursive_size pre-walk.** On small-file-heavy trees (PPSA01342:
  223k files / 19k dirs), the walk could outrun the client poller's
  250 ms initial delay. The first `FS_OP_STATUS` poll then landed on
  a not-yet-registered op — the engine surfaced that as a transient
  error which the client mis-attributed to an old payload, latching
  the warning banner permanently for the rest of the move. The
  payload now registers the slot up front with `total_bytes=0` and
  patches the total in via a new `fs_op_set_total` once the walk
  completes, so the poller sees `found:true` from the very first
  call. Same fix applied to `handle_fs_delete` (the delete-progress
  path added in 2.2.22 had the identical race shape).

- **Fix (client): replaced brittle error-string matching with a
  version-aware decision.** The Library Move poller used to set the
  unsupported-payload flag whenever the engine error contained
  `"unsupported_frame"` or `"decode FS_OP_STATUS_ACK body"`. Both
  substrings can appear in transient errors on a *current* payload
  (race-window parses, mid-flight reconnects), so a single bad poll
  would gaslight users on the latest build into thinking they
  needed to replace the payload. The poller now consults the
  Connection screen's STATUS-probed `payloadVersion`: if it's known
  to be `< 2.2.7` or `< 2.2.16`, latch immediately with the
  matching threshold-specific banner (honest); if it's current,
  swallow up to 5 consecutive transient failures then stop polling
  silently with no banner. The version-string fallback (no probe
  data + repeated old-payload-shaped errors) still works for very
  old builds that don't report version. New helper
  `lib/movePollerPolicy.ts` (+16 unit tests) keeps the policy
  pure-functional so we can test it without mocking React state.

- **Fix (client): the warning banner now names the specific
  threshold the running payload is missing** (`predates the 2.2.16
  FS_OP_STATUS_ACK body fix` or `predates 2.2.7 FS_OP_STATUS`)
  rather than the generic `"older than this app"` string. When it
  *does* fire, the user can see exactly which fix they need.

Net effect: on a current payload (2.2.16+), Move shows live
byte-progress reliably and never falsely accuses the payload of
being old. On a genuinely old payload, the banner is more
specific about what's missing. The actual copy and cancel paths
are untouched — same 16 MiB inner loop, same atomic counters,
same `FS_OP_CANCEL` semantics.

### New: FileSystem volume picker

The FileSystem screen used to start every session at hardcoded
`/data` and only let users navigate via breadcrumbs + parent-up.
Jumping between volumes (`/data` → `/mnt/ext1` → `/mnt/usb0`) meant
walking up to `/`, then back down — tedious for any move-from-internal
workflow. Added a dropdown above the breadcrumb that lists every
writable, non-placeholder volume with its free-space readout
(`/mnt/ext1 · 412 GiB free`). Selecting a volume jumps to its
root. Same `fetchVolumes()` data Library's Move modal already uses,
filtered identically. The picker hides itself entirely when the
PS5 has no writable volumes, so it doesn't add noise on a stock
or partially-mounted machine.

### Preferences now actually persist

Two settings users expected to survive a reload silently didn't:

- **PS5 host**. `connection.host` was hardcoded to `192.168.137.2`
  on every fresh launch. Most users have one PS5 IP they reuse;
  retyping it every session is friction with no upside. The store
  now reads the last-typed value from localStorage on init and
  writes back through `setHost`.
- **Last-browsed FileSystem path**. Persisted per-host so a user
  with two PS5s remembers each console's last directory
  independently. Trims whitespace on the host key so typoed-then-
  fixed inputs don't fragment storage. Defaults safely to `/data`
  when no entry exists or localStorage is corrupted (malformed
  JSON, schema bump, partially-truncated map).

For confirmation, the prefs that *were* already persisted (language
and theme via localStorage, activity history with 100-entry cap,
upload queue via Tauri storage) all continue to work as before —
the audit found no other expected-to-persist setting that's
silently lost.

### Connection screen no longer shows stale version data after Replace payload

A user reported that after sending a fresh payload from the
Connection screen, the version block kept showing the *old* version
number for a few seconds before flipping to the new one. The
ambiguity ("did the Replace not take?") was made worse by the fact
that there's no visible "we're checking" cue — the numbers just sat
there until the next 10 s background poll happened to land.

Two coupled fixes:

- **In-flight version flush.** `handleSend`'s probe loop already
  calls `payloadCheck`, which returns the freshly-booted payload's
  `payloadVersion` + `ps5Kernel` along with the reachability flag.
  The original code consumed only `reachable` and discarded the
  rest, leaving the store with the old values until the next
  AppShell poll. Now the moment the payload answers, the probe
  writes the new version + kernel into the store — the version
  block flips to the new numbers in lock-step with step2 going
  "ok," not 10 s later.

- **"Rechecking…" indicator.** Added a `payloadProbing` flag to the
  connection store. `handleSend` flips it on at the start (and
  clears `payloadVersion` / `ps5Kernel` so users don't squint at
  numbers known to be stale); `handleSend`'s in-line probe and
  AppShell's tick both clear it the moment they land a result.
  While set, the VersionBlock renders with a small spinner +
  "rechecking…" badge, value rows are dimmed in italics, and the
  outdated-payload nudge is suppressed (the version we'd be
  comparing against is mid-flight). When there's no current value
  yet, the row reads "Probing…" — same effect for the very-first
  send when there was nothing to dim.

Result: clicking Replace payload now produces immediate, honest
feedback. Either you see the new numbers fast, or you see a clear
"we're checking" cue. No more ambiguous "is this stale?" reads.

### OperationBar now shows every in-flight operation

The global activity strip surfaces an op while it's running so the
user has a global signal that "something is still happening" even
after navigating away from the screen that started it. Coverage was
patchy: uploads, downloads (FileSystem), bulk delete, paste-copy,
paste-move, and Library Move were tracked; Library Download,
Library Delete, Library Chmod, Library Mount, Library Unmount, and
single-item FileSystem Delete went silent. A 100 GB Library
Download or a recursive chmod on a 100k-file game folder is the
exact case where users expect a global "still running" indicator,
and exactly the case where they didn't get one.

Each of those handlers now opens an activity entry on entry and
closes it (`done` / `failed` / `stopped`) on every exit path. The
download poller mirrors live byte-progress into the entry so the
OperationBar shows the same `bytes / total · MiB/s` it shows for
uploads. No new activity-store changes; the entries pre-existed in
the `ActivityKind` enum (`library-chmod`, `library-mount`,
`library-unmount`) but were unused.

### Activity log + state-leak hygiene (second-pass audit)

A second-pass review surfaced three lifecycle bugs in the new code
that the first audit missed. All three are about state that gets
set but never cleared on the un-happy path:

- **Library Move's "done" finish leaked the poller's progress
  note.** While a Move was running, the FS_OP_STATUS poller could
  write a "Live progress unavailable — payload predates 2.2.16…"
  string into the activity entry's `error` field (the field
  doubles as a "note" while `outcome === running`). When the move
  ultimately succeeded, the success-finish call passed no extras,
  so `finish()`'s spread carried the running-state note into the
  terminal record — Activity tab rendered a green checkmark next
  to a red-looking "predates 2.2.16" line. `finish()` for the
  success path now explicitly clears `error: undefined`.
- **`payloadProbing` flag leaked on Connection unmount.** The
  cleanup effect cancels the poll handle but didn't reset the
  probing flag. A user clicking Replace and then immediately
  navigating away would leave the "rechecking…" badge latched in
  the store, only clearing on the next AppShell tick (up to 10 s
  later). Cleanup now sets `payloadProbing: false` unconditionally;
  no-op when the flag was already false, fixes the leak when it
  wasn't.
- **`handleSend`'s probe could pollute the store after a
  mid-flight host change.** If the user clicked Replace, then
  retyped the IP into the host field while `sendPayload` was still
  running, the probe loop was closed-over the OLD host and would
  eventually write that host's `payloadVersion` + `payloadStatusHost`
  into the store — overwriting the newly-typed host's state. The
  probe now compares its captured `host` to
  `useConnectionStore.getState().host` before writing. If they've
  diverged, it returns "ok" (the probe loop exits cleanly) but
  skips the write — AppShell's host-change effect handles the new
  host instead.
- **Library Download orphaned activity entries on
  navigate-away.** The download poller's three `mountedRef` short-
  circuits exited the loop without closing the activity entry —
  the engine job continues server-side, but the entry stayed
  `running` until app restart. Worse, hitting Download again after
  navigating back created a *second* "running" entry for the same
  file. The exits now share a `bailOnUnmount()` helper that
  finishes the entry as `stopped` with the same "engine job may
  continue" wording the user-Stop path uses.

### Other

- **Volume picker edge cases.** Fixed two cases the longest-prefix
  match got wrong: a path with a trailing slash (`/data/`) now
  matches volume `/data` correctly, and the root path `/` no
  longer arbitrarily picks the longest-named volume — it falls
  through to "(custom path)" since `/` isn't really under any
  volume root.
- **i18n.** Added the new English keys for the volume picker, the
  Connection rechecking badge, and the threshold-specific move-
  progress banners to `i18n.ts`. Updated
  `scripts/i18n-known-missing.json` to allowlist them in the 17
  non-English locales (translation deferred); the i18n coverage
  CI is green.
- **CI: bumped `softprops/action-gh-release` from v2 → v3** (Node
  20 → Node 24). v2 still works but emits the GHA Node 20
  deprecation warning. v3.0.0 was published 2026-04-12; bumping
  ahead of GitHub's June forced-Node-24 cutoff so the release
  workflow's run page is warning-free.
- **CI: cleared pre-existing red on `engine-ci`.** The
  `rust (engine workspace)` job had been failing on every
  release commit since at least v2.2.23 because of latent
  `cargo fmt --check` drift in the engine's import block, and
  the `version sync` job was newly red on this tag because
  `client/package-lock.json` carried a stale `2.2.23` reference.
  Both fixed (cargo fmt + `node scripts/update-version.js`); the
  v2.2.24 tag now sits on a commit with full green CI.
- **FAQ.** Added entries for the volume picker, the per-host
  last-browsed path persistence, OperationBar coverage, the
  "Live progress unavailable on the latest payload" false-
  positive (and how 2.2.24 resolves it), and the Connection
  screen's new "rechecking…" indicator.

### Verification

- `vitest` 164 passed (was 155 + 9 new for `fsLastPath`).
- `tsc --noEmit` clean.
- `eslint` clean.
- `node scripts/i18n-coverage.mjs` clean (18 languages).
- Engine `cargo test` 77 passed.
- Tauri shell `cargo check` + `cargo clippy -D warnings` clean.
- Payload built clean with `-Wall -Wextra -Werror`.

---

## 2.2.23

**Engine lifecycle hardening: orphans, double-launches, and a real body-size cap**

Three small fixes paying down the long tail of "engine got stuck and
the desktop can't talk to it" failure modes that used to require a
manual `taskkill` to recover from.

- **Fix: desktop dying ungracefully no longer leaves the engine
  orphaned holding port 19113.** Symptom: any time the desktop shell
  exited without running its `Drop` impls — `taskkill /F`, segfault,
  panic, OOM-killer, power-cycle recovery — the engine kept running
  in the background. Next launch couldn't bind 19113 and the user
  saw "engine did not become ready" with no obvious cause until they
  manually killed the orphan from a terminal. The engine now spawns
  with a piped stdin handle and runs a watcher thread that exits the
  process on EOF; the OS unconditionally closes the parent's pipe
  handles when the parent dies (any way it dies), so the engine
  notices instantly and releases the port. No FFI, no per-OS code —
  same behavior on Windows, macOS, Linux.
- **Fix: existing orphans from prior crashes are auto-reaped at
  startup.** The stdin-EOF watcher above prevents future orphans, but
  a user upgrading from an older build still has the orphan from
  their last session holding 19113. The desktop now shells out to a
  per-OS port-killer (`Stop-Process` on Windows, `lsof | kill` on
  macOS, `fuser -k` on Linux) when it detects the port is held by
  something that isn't a healthy version-matched engine. So the
  *first* launch after this update auto-cleans whatever's stuck;
  from then on the watcher prevents new orphans.
- **Fix: double-clicking the desktop icon no longer breaks the
  first instance.** Previously a second launch would race on 19113,
  and (worse, after this release's orphan-reaper) would actively
  kill the first instance's engine and leave its UI talking to a
  dead sidecar. Adopted `tauri-plugin-single-instance`: a second
  launch focuses the existing window and exits cleanly. Standard
  Tauri pattern; ~10 LOC.
- **Robustness: explicit 64 MiB body-size cap on the engine HTTP
  router.** Was relying on axum's implicit 2 MiB default, which is
  borderline for a 100k-entry `TransferFileListReq` and could
  silently change with an axum upgrade. Now documented in the code
  alongside the CORS layer; generous enough for the largest
  legitimate payload, small enough that a runaway local request
  can't OOM the engine on a 4 GB box.
- **Tauri shell shutdown switched from per-window `CloseRequested`
  to app-level `RunEvent::Exit`.** Same effect for today's
  single-window app, but a future multi-window scenario won't kill
  the engine on first-window-close.

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
  a loader to dev kit, then push the harness to the test kit) or
  for payloads bound to non-default loader ports.

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
  (`/data/homebrew/games/PPSA09519.exfat`) wrap cleanly now.
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
  supported on current firmware; legacy probe targets were dropped.
