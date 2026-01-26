# Changelog

All notable changes to this project are documented here.
This project follows Semantic Versioning.

## [1.3.2] - 2026-01-26

### Fixed
- RAR Temp Storage now actually controls where the archive is staged before extraction (it was previously ignored and always used the destination drive).

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
