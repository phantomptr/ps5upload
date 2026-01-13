# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
