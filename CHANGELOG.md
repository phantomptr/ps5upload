# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.4] - 2026-01-14

### Added
- **Archive UX:** Confirmation prompt for archives with a trim option to drop the top-level folder when it matches the archive name.
- **Auto-Tune Connections:** Optional heuristic that reduces connections for tiny-file workloads to improve throughput and responsiveness.
- **Resource Visibility:** Client logs peak RSS after uploads; payload logs lightweight memory stats during transfers.
- **Multi-Connection Archive Note:** UI explicitly explains that multi-connection archive uploads require temporary extraction.

### Changed
- **Archive Streaming:** ZIP/7Z now stream in chunks instead of buffering whole files; RAR uses callback streaming for single-connection uploads.
- **Resume Policy:** Resume is disabled for archive uploads to avoid partial/invalid states.
- **Profiles:** Active profile auto-saves after changes (debounced) instead of requiring manual updates.
- **File Manager UI:** Move actions now reference “pane path”, rows highlight as a single full-width block, and the opposite pane path is shown.
- **Window Size:** Default and minimum window sizes increased to keep all controls visible with breathing room.

### Fixed
- **Windows Defender False Positives:** Applied binary stripping, LTO optimization, and a proper application manifest to prevent heuristics from flagging the client as malware.
- **UI Responsiveness:** Fixed "ghost clicks" on all buttons (Upload, Delete, Rename, etc.) by ensuring the interface refreshes immediately when background tasks complete.

## [1.1.3] - 2026-01-14

### Added
- **Instant Streaming Uploads:** Fresh uploads (where resume is off) now start transferring immediately while scanning proceeds in the background. This works for both single and multi-connection modes, avoiding the long "Scanning..." pause on large folders.

### Fixed
- **Payload Reliability:** Fixed a critical race condition in the writer thread that could cause transfers to hang or crash when using multiple connections.
- **System Freeze Fix:** Implemented a memory buffer pool and cache-bypass (`POSIX_FADV_DONTNEED`) in the payload to prevent the PS5 from freezing or lagging during massive transfers (e.g. 100GB+ to internal SSD).
- **UI Responsiveness:** Resolved the "ghost click" issue where buttons occasionally required two clicks or mouse movement to trigger.
- **Client Stability:** Switched to non-blocking network I/O with improved backpressure handling for smoother throughput and better cancellation responsiveness.

## [1.1.2] - 2026-01-14

### Added
- Resume uploads with options (size, size+time, SHA256) and a guided resume flow.
- Upload queue controls and history resume actions with clearer labels.
- Multi-language UI: English, 简体中文, 繁體中文, Français, Español, العربية.
- App logo integration in the UI and app icons across Windows/macOS/Linux.
- New Folder button in file manager to create directories on PS5.

### Changed
- Default connections set to 1 for maximum reliability.
- Queue list shows destination path; upload buttons are grouped and renamed for clarity.
- Improved selection contrast and note styling in dark/light themes.

### Fixed
- Multi-connection upload corruption on the payload writer.
- Clearer confirmation flows for rename/delete/move/download/overwrite.
- Chinese (简体中文, 繁體中文) and Arabic (العربية) characters now render correctly by embedding Noto Sans fonts.
- Large folder uploads (millions of files) no longer appear stuck at 0%; scanning progress is now shown with file count and size.
- Payload memory management: fixed session counters and writer state cleanup to prevent OOM issues.

## [1.1.0] - 2026-02-01

### Added
- Full file manager: browse PS5 folders, rename, move, copy/paste, delete, and chmod 777 (recursive).
- Download files and folders from PS5 with progress, ETA, and cancel.
- Dual-pane view (Left/Right) for faster navigation and moving between paths.
- Update checker with background notifications and one-click downloads.
- Payload status display and one-click send/download from the client.
- Saved profiles and settings (compression, bandwidth limits, update channel, etc.).

### Changed
- Separate progress bars and controls for uploads and downloads.
- Folder downloads are streamed directly from the PS5 (no temporary tar files).
- Optional LZ4 compression for faster transfers on slow links.
- Upload bandwidth limit option to avoid saturating your network.

### Fixed
- Auto reconnect stability and clearer connection status.
- Manage UI selection behavior (click any column to select).
