# PS5Upload Webapp + Rust Wrapper Port Plan

This doc captures the current desktop app feature set and a proposed Rust API surface
for a Tauri-based web UI. It is the baseline to ensure feature parity across platforms.

## Feature inventory (current desktop app)

- Connection
  - Connect/disconnect to PS5
  - Auto reconnect
  - Storage list + free space
- Payload
  - Send payload (local file)
  - Check payload version
  - Download latest/current payload + send
- Transfer
  - Select local folder
  - Destination presets + custom
  - Compression modes
  - Resume modes
  - Upload progress, ETA, speed
  - Stop/cancel
  - Optimize upload (auto tuning)
- Manage
  - List remote directories
  - Navigate left/right panels
  - Rename, delete, new folder
  - Move/copy
  - Upload files/folders
  - Download files/folders (compression)
  - Extract archives
  - chmod 777
  - Operation progress
- Queue
  - Add to queue
  - Upload queue
  - Queue status
- History
  - Transfer history list
  - Resume from history
  - Clear history
- Chat
  - Room join, send/receive messages
  - Status (connected/disconnected)
- Updates
  - Check latest release
  - Download updates
  - Restart to apply
- Settings
  - Theme (dark/light)
  - Language
  - Connections, bandwidth limit
  - Auto payload check
  - Auto resume, chmod after upload
- Profiles
  - Save/apply/delete profile
  - Set default
- Logs
  - App/payload logs

## Proposed Rust command surface (Tauri invoke)

### Connection
- `connect(ip: String) -> Result<StorageState, String>`
- `disconnect() -> Result<(), String>`
- `list_storage() -> Result<Vec<StorageLocation>, String>`

### Payload
- `send_payload(path: String) -> Result<u64, String>`
- `check_payload_version() -> Result<String, String>`
- `download_payload(kind: "current"|"latest") -> Result<String, String>`

### Transfer
- `scan_source(path: String) -> Result<ScanResult, String>`
- `start_upload(req: UploadRequest) -> Result<(), String>`
- `cancel_upload() -> Result<(), String>`

### Manage
- `list_dir(path: String) -> Result<Vec<DirEntry>, String>`
- `create_dir(path: String) -> Result<(), String>`
- `delete_path(path: String) -> Result<(), String>`
- `rename_path(src: String, dst: String) -> Result<(), String>`
- `move_path(src: String, dst: String) -> Result<(), String>`
- `copy_path(src: String, dst: String) -> Result<(), String>`
- `chmod_777(path: String) -> Result<(), String>`
- `extract_archive(path: String, mode: String) -> Result<(), String>`
- `download_path(req: DownloadRequest) -> Result<(), String>`
- `upload_paths(paths: Vec<String>, dest: String) -> Result<(), String>`

### Queue
- `queue_add(item: QueueItem) -> Result<(), String>`
- `queue_remove(id: u64) -> Result<(), String>`
- `queue_start() -> Result<(), String>`

### History
- `history_list() -> Result<Vec<TransferRecord>, String>`
- `history_clear() -> Result<(), String>`
- `history_resume(record_id: String, mode: String) -> Result<(), String>`

### Chat
- `chat_connect(display_name: String) -> Result<(), String>`
- `chat_disconnect() -> Result<(), String>`
- `chat_send(message: String) -> Result<(), String>`

### Updates
- `update_check(include_prerelease: bool) -> Result<ReleaseInfo, String>`
- `update_download(kind: String) -> Result<String, String>`
- `update_apply() -> Result<(), String>`

### Config/Profile
- `config_load() -> Result<AppConfig, String>`
- `config_save(config: AppConfig) -> Result<(), String>`
- `profiles_list() -> Result<Vec<Profile>, String>`
- `profile_save(profile: Profile) -> Result<(), String>`
- `profile_delete(name: String) -> Result<(), String>`
- `profile_set_default(name: Option<String>) -> Result<(), String>`

## Event stream (Rust -> Web UI)

- `log` / `payload_log`
- `status`
- `storage_list`
- `upload_progress`
- `upload_complete`
- `download_progress`
- `download_complete`
- `manage_progress`
- `manage_complete`
- `chat_message`
- `chat_status`
- `update_status`

## UI structure (web)

- Sidebar: Connection, Profiles, Settings summary, Storage
- Main tabs: Transfer, Manage, Chat
- Utility pane: Logs + History + Queue

## Theme notes

We will approximate the PS5 console palette (white shell, black core, electric blue accent).
If you want the exact colors, provide a local image or hex codes to sample.
