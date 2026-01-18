# Changelog

All notable changes to this project are documented here.
This project follows Semantic Versioning.

## [1.1.7] - 2026-01-17

### Added
- Integrated `unrar` directly into the payload for server-side RAR extraction.

### Changed
- Removed client-side archive extraction. All ZIP and 7Z archives are now streamed directly to the PS5.
- RAR archives are now uploaded as-is and extracted by the PS5 payload.

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
