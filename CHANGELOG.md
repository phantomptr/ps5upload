# Changelog

All notable changes to this project are documented here.
This project follows Semantic Versioning.

## [1.5.2] - 2026-02-08

### Changed
- Payload directory pre-creation now uses a `stat()` fast path: if a directory already exists on disk (e.g., during resumed uploads), it skips the full `mkdir_recursive` walk and goes straight into the cache. Reduces per-directory overhead from N `mkdir`+`chmod` syscalls to a single `stat()`.

## [1.5.1] - 2026-02-08

### Changed
- Payload file writer threads increased to 2 for better small-file I/O overlap (configurable via `FTX_FILE_WRITER_THREAD_COUNT` in `config.h`).
- Payload directory creation now uses a shared hash set across writer threads: directories are bulk pre-created per batch and cached globally, eliminating redundant `mkdir`+`chmod` syscalls.
- Desktop packing now enforces an 8MB pack floor during runs of many consecutive small files (< 64KB), preventing adaptive tuning from shrinking packs too aggressively and keeping hundreds of files per pack.

### Fixed
- Small-file transfer throughput improved significantly by reducing per-file filesystem overhead on the payload side and keeping packs fuller on the client side.

## [1.5.0] - 2026-02-08

### Changed
- Payload upload concurrency is capped at 4 for stability on PS5/FreeBSD 11.
- Desktop Transfer: Mad Max is now opt-in only (no longer auto-triggers at 64MB+).
- Desktop now keeps payload status polling enabled while Queues are active (prevents stale busy state during queue operations).

### Fixed
- Upload queue can now recover cleanly after cancelling/stopping extraction: payload status is refreshed and queue retries resume once the payload is idle.

## [1.4.9] - 2026-02-08

### Changed
- Stability-first transfer defaults: payload upload concurrency is now capped at 4 to reduce PS5/FreeBSD 11 overload.
- Desktop Transfer: **Mad Max** is now opt-in only (no longer auto-triggers based on file size).
- App (web mode) Manage upload folder behavior now matches desktop: uploading a folder targets `destRoot/<folderName>/...`.
- Lane (single-file) upload threshold lowered to 512MB for earlier large-file acceleration while staying within the 4-connection cap.

### Fixed
- App (web mode): transfer/manage uploads now report live speed consistently.
- Web bridge polling: prevent overlapping poll requests; payload status polling is paused during payload send to reduce log spam while the payload restarts.

## [1.4.8] - 2026-02-08

### Added
- New payload fast paths: parallel `UPLOAD_FAST` multi-file uploads and lane-based single-file uploads via `UPLOAD_FAST_OFFSET`.
- Pre-create directory phase before payload multi-file uploads (with safety caps to avoid huge stalls).
- App (web mode) now uses the same payload fast paths as desktop, with FTP fallback on failure.

### Changed
- Resume compatibility listing now auto-skips for very large trees to avoid long startup scans.
- Logging for pre-create and resume scanning tuned to reduce noise while keeping progress visibility.
- Connection controls are now managed automatically (no manual slider). Defaults: Payload 4, FTP 10; auto-tune adjusts based on file size/count.
- Archive uploads now emit stall hints when progress pauses early.
- RAR extraction now uses a single turbo mode (max speed) and the mode selector has been removed.
- Payload stability tuning for PS5/FreeBSD 11: bounded socket buffers and worker concurrency caps to reduce memory/budget pressure; thread stack sizes are now explicitly set for payload worker threads.

### Fixed
- Archive extraction progress handler is now available across archive workflows.
- Archive uploads now queue extraction (with temp cleanup) so extraction appears in the Extraction queue after upload finishes.
- Queued extraction now runs in turbo mode consistently.

## [1.4.6] - 2026-02-04

### Added
- Payload/desktop/app startup verification for payload send flows (upload success now also validates payload startup/version).
- New **Mad Max** payload fast path for large single-file uploads using direct raw streaming.
- New parallel offset fast path (`UPLOAD_FAST_OFFSET`) for Mad Max large-file chunk uploads.
- New **Mad Max** UI mode in the Transfer source panel (grouped with Optimize/Deep Optimize).

### Changed
- Desktop "current" payload reload now prefers local `payload/ps5upload.elf` before release download.
- Mad Max now runs fixed max settings (locked profile): payload connections/workers, chunk size, no compression, unlimited bandwidth.
- Mad Max is now selectable for archive sources from the same optimize controls area.
- Payload startup defaults to a safer initialization profile to reduce firmware-dependent load crashes.
- Payload upload pipeline defaults tuned for higher throughput (larger recv buffer and deeper worker queues).

### Fixed
- Reduced false-positive payload send success by failing early when payload startup verification does not pass.
- Improved diagnostics around payload reload/startup mismatches (for example sending new ELF while old payload keeps running).

## [1.4.5] - 2026-02-03

### Added
- New `app/` runtime that serves PS5Upload in browser mode (backend + frontend in one process).
- Web bridge support so browser mode reuses the desktop UI flow.
- New app packaging workflow: `.github/workflows/app-package.yml` publishes a downloadable app zip bundle.
- New Make targets: `make app`, `make setup-app`, `make run-app`, and `make package-app`.
- Added `app/README.md` and `shared/README.md` with setup/runtime details.

### Changed
- Version update script now updates app + desktop package versions and lockfiles together.
- `make run-app` now prepares and serves the desktop UI bundle for web mode.
- Moved debug/test helpers to `scripts/test-crash-debug.js` and `scripts/test-games-scan.js`.
- Transfer protocol is now V4-only across desktop + payload for upload/download pack frames.
- Test upload tooling now targets `UPLOAD_V4`.

### Notes
- In web mode, file/folder browse/upload/download paths are resolved on the host machine running the app service.

## [1.4.4] - 2026-02-03

### Added
- New **Games** tab scan paths with defaults (`etaHEN/games`, `homebrew`) plus multiple custom scan paths.
- Storage filter toggles for Games results (show/hide by storage root).
- Duplicate game detection and grouping (by Content ID, then Title ID, then folder fallback).
- Per-game **Scan files/size** action with live progress and single active scan limit.
- Per-game **Delete game** action with in-app styled confirmation modal and in-item deleting indicator.
- Games search bar filtering by displayed metadata (title, IDs, version, path, etc.).

### Changed
- Games metadata reads now use the safer `DOWNLOAD` path instead of `DOWNLOAD_RAW` to improve payload stability.
- Games item UI refreshed: clearer action placement and stronger path visibility.

### Fixed
- Added payload-side logging/crash context for directory listing and download paths to improve debugger diagnostics.
- Fixed `gamesScanStats` frontend invocation handling and stale-preload error messaging.

## [1.4.3] - 2026-02-02

### Added
- Payload-side backpressure to pause recv when the writer queue is saturated.
- Small-file buffer pool to reduce malloc/free churn during large directory uploads.

### Changed
- File writes now verify full writes and abort on any write error to avoid silent corruption.
- File writer reuses open file descriptors for sequential chunk writes to reduce overhead.
- Finalize now waits on writer signals instead of polling sleeps for faster, safer shutdown.

## [1.4.2] - 2026-02-02

### Added
- Auto-recovery for payload uploads: waits for payload to recover, resumes by size, and continues automatically.
- Payload status now reports abort timestamp, reason, and last session id for better diagnostics.

### Fixed
- Abort handling now force-closes active upload sockets so payload doesn’t hang in an active session state.

## [1.4.1] - 2026-02-02

### Added
- Separate FTP connections control and live display of active FTP connections during transfers.
- FTP service reminder when FTP/Mix is selected (ftpsrv or etaHEN FTP service).

### Changed
- Mix/FTP uploads now abort if FTP is unavailable instead of silently falling back.
- Auto-tune/Optimize now tune both payload and FTP in Mix mode.
- Added small-file pacing safeguards for payload and FTP to reduce client/payload overload.

## [1.4.0] - 2026-02-01

### Removed
- Legacy UPLOAD (V1) and UPLOAD_V2 handlers from the payload; standard uploads are V3-only.
- Mix threshold setting and split-by-threshold behavior; mix now runs continuously until both sides meet.

### Changed
- Test upload script now targets UPLOAD_V3 only.
- Mix mode now pulls smallest files for payload and largest files for FTP, continuously, with safer fallback handling.
- FTP uploads now use a mature client library with better progress tracking and connection handling (FTP upload enabled).
- Upload speed display now uses EMA for smoother, more accurate live speed.
- Noisy per-file packing logs are filtered from the UI.
- Settings/history/queue data are normalized on load to the latest templates.
- Manage folder downloads now use a safe, sequential per-file transfer path for stability.

### Fixed
- FTP connect/auth edge cases that could block uploads.
- Mix mode fallback and V3 response parsing issues that caused false errors.
- Miscellaneous desktop/runtime errors and transfer log noise.
- Manage download stability improvements with retries and safer file enumeration.

## [1.3.11] - 2026-02-01

### Added
- Upload V3 protocol with per‑pack ACKs and replay to recover cleanly from payload hiccups.
- Direct‑Ethernet speed guide added to the FAQ.

### Changed
- Scan requests are deduped for identical sources/settings to avoid repeated scans.
- Logging defaults reduced for noisy transfer events (more debug‑only).
- Keep Awake now blocks both display sleep and app suspension for better cross‑platform reliability.

### Fixed
- Payload stalls now trigger safer recovery paths on the client.
- “Undefined” log entries are filtered out from UI logs.
- Manage Move now streams progress without closing the connection mid‑operation (better large folder moves).
- Payload remove (used by move‑across‑devices) is now iterative to avoid deep recursion crashes.
- Added detailed payload logging for Move/Copy to aid debugger investigations.
- Crash handler now captures last Move/Copy path context to pinpoint SIGBUS issues.
- Copy/move file IO now uses heap buffers to avoid stack overflows on large copies.
- Copy/Move now logs scan size, copy/remove completion, and OK send failures.

## [1.3.10] - 2026-01-30

### Added
- **Bottleneck indicator** in Payload Status to show the likely limiting side (network, payload CPU, payload disk, or client).
- **New metrics panels** (System, Network, Queue, Transfer, Tuning) with clearer grouping and reduced clutter.
- **Tuning suggestions** from the payload (suggested pack size, pacing, and rate) surfaced in the desktop UI.
- **Upload V3 protocol** with per‑pack ACKs and replay to recover cleanly from payload hiccups.

### Changed
- **Adaptive tuning** now runs continuously for stability (even if auto‑tune is off, it applies safety‑only limits).
- **Optimize/Deep Optimize** now respect scan results and update auto‑tune behavior reliably.
- **Payload metrics layout** is now stacked for easier scanning on desktop.

### Fixed
- Uploads and archive paths with **spaces/tabs/brackets** are now handled correctly.
- Reduced noisy logging by stabilizing bottleneck signals and throttling repeated messages.

## [1.3.9] - 2026-01-30

### Fixed
- Uploads with many small files (e.g., 500K+ files) no longer stall or fail due to backpressure timeouts.
- Payload now waits indefinitely for disk I/O to catch up instead of giving up after 30 seconds.
- Client now logs "Waiting for server to catch up..." when socket buffer is full, so users know the transfer is still alive.
- Enhanced adaptive upload tuning on client: Client now reacts more aggressively to payload backpressure by significantly reducing pack size and increasing inter-packet delays, vastly improving stability and preventing hangs for large transfers.
- Optimized payload memory usage: Reduced payload's internal queue depth from 10 to 4 (480MB to 192MB) to lessen memory pressure on the PS5, making it less prone to stalls during heavy load.

## [1.3.8] - 2026-01-29

### Added
- Payload upload safeguards: memory-pressure backoff and queue wait timeouts to avoid hangs under heavy load.
- Remove button for completed/stopped upload and extraction queue items.
- Auto-resume upload queue after payload restart/reconnect (fast resume mode).

### Changed
- Optimize/Deep Optimize now choose more aggressive settings for very large small-file uploads (higher connections, auto-tune, and less compression).
- Resume scan summary now appears in client logs after scanning (skipped vs. remaining).

### Fixed
- Transfer packing logs are throttled on huge uploads to reduce overhead.
- i18n duplicate key warnings resolved.

## [1.3.7] - 2026-01-29

### Added
- PS5-native system metrics (CPU, memory, threads) via kernel APIs with safe fallbacks when restricted.

### Changed
- System metrics now prefer kernel sources over sysctl for jailed environments.
- Resume scan summary now appears in client logs after scanning (skipped vs. remaining).
- Optimize/Deep Optimize now choose more aggressive settings for very large small-file uploads (higher connections, auto-tune, and less compression).

### Fixed
- Metrics panel now reports restricted fields explicitly when the payload cannot access them.

## [1.3.6] - 2026-01-29

### Added
- Keep Awake now has an Auto mode that allows sleep after 15 minutes of inactivity (no current upload or extraction).
- Resume modes now include Fastest/Faster/Fast/Normal tiers with size-based hashing thresholds.
- Auto-tune now explains its behavior (pack size/pacing; may enable Optimize without changing connections).

### Changed
- Resume “size + time” has been removed (PS5 clock drift can make it unreliable); legacy configs are normalized to size-only.
- Upload info popup now shows the effective Optimize state for active transfers.

## [1.3.5] - 2026-01-28

### Added
- New UI languages: Vietnamese, Hindi, Bengali, Portuguese (Brazil), Russian, Japanese, Turkish, Indonesian, Thai, Korean, German, Italian.

### Changed
- Transfer/extraction UI refresh now uses 500ms polling while active for smoother updates.
- Source browse is allowed while an upload is in progress, so new items can be queued.
- Transfer reset and RAR temp storage selection stay enabled during uploads.
- UI font stack expanded for better multilingual glyph coverage.
- Language selector now uses a custom menu so bundled fonts render correctly on Linux.

### Fixed
- Upload speed readout no longer spikes to unrealistic values at the start of transfers.
- Uploads no longer fail after scanning with “config is not defined”.
- Multilingual UI no longer shows square glyphs in logs or panels on Linux.

## [1.3.4] - 2026-01-28

### Added
- Upload queue item info popup showing per-item transfer parameters.
- Progress UI now shows ETA, avg speed, elapsed time, and last update.
- Extraction queue shows a “waiting for payload status” hint before the first status update.
- Payload maintenance command for safe cleanup when idle (buffers/tmp/log rotation).

### Changed
- Extraction status polling accelerates while extractions are running and triggers immediate refresh after queue hints.
- Failed upload/extraction items show short detail summaries in the queue list.
- Desktop triggers periodic maintenance when idle and after queue transitions.
- Extraction queue Stop now retries automatically and cancels in parallel to avoid repeated clicks.
- Extraction progress messaging now shows “Starting extraction…” when bytes are still at 0.

### Fixed
- Archive uploads no longer trigger chmod on the upload destination (chmod only applies to extracted folder when enabled).
- Transfer speed/ETA now waits for a minimum time window to avoid unrealistic spikes.

## [1.3.3] - 2026-01-27

### Added
- FAQ tab with embedded, scrollable README FAQ (bundled into desktop builds).
- Per‑tab queue badges for Upload + Extraction queues.
- Manage “Reset UI” action for recovering from stuck state.

### Changed
- Desktop window is now resizable with responsive layout.
- Platform icon bundles (Windows .ico, macOS .icns, Linux hicolor) wired into release builds.
- Log level dropdown now color‑coded by level.

### Fixed
- Upload/extraction completion now logs regardless of success/failure.
- Extraction queue remains visible when disconnected (cached view).
- chmod 777 after archive extraction now targets the final extracted folder.

## [1.3.2] - 2026-01-26

### Fixed
- RAR Temp Storage now actually controls where the archive is staged before extraction (it was previously ignored and always used the destination drive).
- Many many bug fixes, e,g. Manage Tab functions

### Changed
- Payload accepts a temp storage override for RAR uploads, so archives can be staged on a different mount than the final extraction target.

## [1.3.1] - 2026-01-24

### Fixed
- Extraction queue no longer gets stuck in a “pending” state after a stop or scan failure.
- Upload path parsing hardened to prevent payload crashes on long or complex paths.
- Failed extractions keep their RARs for requeue instead of being deleted automatically.

### Changed
- Payload logs now rotate per launch and capture more startup details.
- Added “Clear tmp” and “Clear failed” actions for extraction queue maintenance.
- Added “Clear failed” for upload queue.

## [1.3.0] - 2026-01-24

### Added
- Unified **Queues** area for upload + extraction with Current/Completed/Failed tabs.
- Extraction queue view with per-item metadata (name, path, size), cover art when available, and live progress for the active item.
- Dedicated refresh for extraction and upload queues plus clear‑completed/clear‑queue actions.
- Log level filter (Debug/Info/Warn/Error) in the Logs panel with colored badges.
- Payload reset action and more detailed payload logging.
- Discord button in the header; external links now open in the system browser.
- `make clean-both` helper (clean → build payload → run desktop).

### Changed
- Payload command/status handling now runs on a dedicated thread and is prioritized over transfers.
- Archive uploads now surface “queued for extraction” status with improved queue labeling.
- Connect/reload buttons have short cooldowns to prevent accidental repeats.
- UI button colors and light/dark contrast tuned for clarity.
- Payload logs now rotate per launch with timestamped archives.

## [1.2.3] - 2026-01-24

### Fixed
- Bug fixes for the new UI.

## [1.2.2] - 2026-01-20

### Fixed
- Payload status JSON buffer bounds to prevent malformed responses and connection resets.
- Reduced UI work during transfers by moving polling and debounced saves into Rust.

### Changed
- Connection, payload status, and manage list polling now run in Rust with snapshot/event updates.

## [1.2.1] - 2026-01-20

### Fixed
- UI freeze during uploads and when switching between tabs by making `transfer_start`, `transfer_scan`, `port_check`, `storage_list`, `payload_status`, and `manage_list` commands asynchronous.
- Performance Improvements
- Bug Fixes

## [1.2.0] - 2026-01-18

### Added
- Override-on-conflict toggle for uploads and queue processing.
- Queue controls for clearing completed items and clearing the full queue.
- Manage browser sorting (name/size/modified) and Type column.
- `<New Profile>` dropdown option with modal creation flow.
- Port checks before connect and payload actions.
- UI redesign across transfer/manage/logs and window controls.

### Fixed
- Queue duplicate detection and clearer queue status behavior on conflicts.
- Chat status display styling and default random `user-` names.
- Window control buttons and general spacing/overflow issues across panels.

### Changed
- Actions panel now matches the browser width on Manage.
- Transfer controls start uploads without a manual connect step.

## [1.1.9] - 2026-01-18

### Added
- Turbo RAR extraction mode with minimal throttling for maximum speed.
- Manage extract now supports RAR-only explicitly with UI guidance.
- RAR metadata probing with cover/title display for local and remote archives where available.
- RAR mode selector (Normal/Safe/Turbo) in archive confirmation and transfer settings.
- RTL-friendly text input alignment for Arabic.

### Fixed
- RAR upload temp file creation now has a safe fallback if mkstemp is unavailable.
- Move/Copy operations now use iterative traversal to avoid deep recursion issues.
- RAR uploads now retry with Normal mode if the payload rejects Safe/Turbo commands.

### Changed
- Copy buffer size increased for better throughput.
- UI contrast and text legibility improved for readability.

## [1.1.8] - 2026-01-18

### Added
- Manage-side archive extraction to a chosen destination with progress (RAR only).
- Heartbeat/progress updates and cancellation for MOVE/COPY/EXTRACT operations.
- Unified progress UI for download/move/extract with a single status block.

### Fixed
- Prevented auto payload checks from interrupting long Manage operations.
- Improved large archive size reporting and progress stability.
- RAR extraction now enforces safe paths to avoid directory traversal.

### Changed
- Move/copy directory traversal is now iterative to avoid deep recursion issues.

## [1.1.7] - 2026-01-18

### Added
- **Automatic Storage Check:** The app now checks exactly how much space is left on your PS5 drive before starting an upload. This prevents "disk full" errors during transfer.
- **Smart Queue Pathing:** Transfers in the queue now remember exactly where they were supposed to go. If you change your selected storage device in the UI, pending transfers will still use their original target path.
- **Server-Side RAR Extraction:** RAR archives are now sent directly to the PS5 and extracted there. This includes live progress updates and handles filenames with spaces much better.
- **Improved Payload Support:** The payload sender now supports both `.elf` and `.bin` files.

### Fixed
- **Large File Fix:** Fixed a bug where uploading RAR files larger than 2GB would get stuck.
- **Better Error Reporting:** If the PS5 runs out of space or has an error while you are sending a RAR file, the client will now tell you immediately instead of hanging.
- **Clarified Overwrite Warnings:** When extracting an archive to an existing folder, the app now clearly explains that files will be merged and existing ones might be overwritten.

### Changed
- RAR is now streamed or sent as-is to the PS5 without needing extraction on your computer first.

## [1.1.6] - 2026-01-14

### Added
- Chat tab with global chat, display names, and built-in key.
- Auto-generated chat display names to avoid collisions.
- Auto-optimize upload settings and clearer compression descriptions.
- Archive fast-extract toggle and trim behavior that ignores Name when enabled.
- Optional RAR streaming (single-connection) alongside extract-first mode.
- Free-space check before upload starts.
- Queue items now remember their target storage device.
- In-app self-update flow with restart prompt.

### Changed
- Manage UI simplified to a single pane with a destination picker.
- Upload status now reflects the real phase (scanning/extracting/uploading).
- Chat panel now fills the center pane.

### Fixed
- Upload cancel now starts cleanly on a new folder without getting stuck.
- UI stays responsive during heavy uploads.
- Prevent uploads from starting when the target drive is nearly full.
- Chat messages now deserialize correctly and show up across clients.
- `make run-client` runs the correct binary when extra tools are present.

## [1.1.5] - 2026-01-14

### Added
- Auto-tune toggle for small-file connection tuning.
- Archive trim prompt when the top-level folder matches the archive name.
- Scanning note that explains the total size is still being calculated.
- Payload logs lightweight memory stats; client logs peak RSS after uploads.

### Changed
- File manager selection highlights the full row.
- Default and minimum window sizes increased for better layout.
- Multi-connection archives always extract to temp; trim applies after extraction.
- Non-blocking response reads retry with timeout to avoid false 10035 errors.

### Fixed
- ZIP/7Z streaming and RAR callback streaming for smoother single-connection uploads.
- Resume disabled for archive uploads to avoid partial states.

## [1.1.4] - 2026-01-14

### Added
- Archive confirmation prompt with an optional trim setting.
- Auto-tune connections for small-file workloads.
- Client peak RSS logging and payload memory telemetry.
- UI note explaining multi-connection archive uploads require temp extraction.

### Changed
- ZIP/7Z stream in chunks; RAR uses callback streaming for single connections.
- Resume disabled for archive uploads.
- Profile changes auto-save after a short debounce.
- File manager selection uses a single full-width highlight and shows the other pane path.
- Default and minimum window sizes increased.

### Fixed
- Windows Defender false positives by tightening release packaging.
- Buttons no longer require a second click to trigger.

## [1.1.3] - 2026-01-14

### Added
- Streaming upload starts immediately while scanning continues in the background.

### Fixed
- Multi-connection payload writer race that could hang or crash transfers.
- PS5 freeze risk during huge transfers by using a buffer pool and cache bypass.
- UI buttons occasionally needing two clicks.
- Network I/O backpressure and cancellation responsiveness.

## [1.1.2] - 2026-01-14

### Added
- Resume uploads with size/mtime/SHA256 modes and a guided flow.
- Upload queue, history, and resume actions with clearer labels.
- Multi-language UI: English, 简体中文, 繁體中文, Français, Español, العربية.
- App logo integration and platform icons.
- New Folder button in the file manager.

### Changed
- Default connections set to 1 for reliability.
- Queue list shows destination path; upload buttons grouped for clarity.
- Improved selection contrast and note styling in light/dark themes.

### Fixed
- Multi-connection upload corruption in the payload writer.
- Clearer confirmation flows for rename/delete/move/download/overwrite.
- CJK and Arabic text rendering by embedding Noto Sans fonts.
- Scanning progress now shows file count and size for large folders.
- Payload session cleanup to reduce OOM risk.

## [1.1.0] - 2026-02-01

### Added
- File manager: browse, rename, move, copy/paste, delete, and chmod 777.
- Download files and folders with progress, ETA, and cancel.
- Dual-pane view for faster navigation and moves.
- Update checker with background notifications and one-click downloads.
- Payload status display and one-click send/download from the client.
- Saved profiles and settings (compression, bandwidth, update channel).

### Changed
- Separate progress bars and controls for uploads and downloads.
- Folder downloads stream directly from the PS5 (no temp tar files).
- Optional LZ4 compression for slow links.
- Upload bandwidth limit to avoid saturating the network.

### Fixed
- Auto-reconnect stability and clearer connection status.
- Manage UI selection behavior (click any column to select).
