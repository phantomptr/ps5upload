use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FTX2_MAGIC: u32 = u32::from_le_bytes(*b"FTX2");
pub const FTX2_VERSION_1: u16 = 1;
pub const FRAME_HEADER_LEN: usize = 28;
pub const TX_META_LEN: usize = 24;
pub const SHARD_HEADER_LEN: usize = 64;
pub const SHARD_ACK_LEN: usize = 48;

// ─── ShardHeader.flags bit constants ─────────────────────────────────────────
/// Shard body carries multiple self-describing file records (pack shard).
/// When set, `ShardHeader.record_count` > 1 and the body layout is:
///   N × { u32 path_len, u32 data_len, path_len bytes, data_len bytes }
/// BLAKE3 digest still covers the entire body.
pub const SHARD_FLAG_PACKED: u32 = 1 << 0;
/// Per-packed-record prefix length: u32 path_len + u32 data_len.
pub const PACKED_RECORD_PREFIX_LEN: usize = 8;

// ─── Frame type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum FrameType {
    Hello = 1,
    HelloAck = 2,
    Error = 3,
    BeginTx = 10,
    BeginTxAck = 11,
    QueryTx = 12,
    QueryTxAck = 13,
    CommitTx = 14,
    CommitTxAck = 15,
    AbortTx = 16,
    AbortTxAck = 17,
    TakeoverRequest = 18,
    TakeoverAck = 19,
    Status = 20,
    StatusAck = 21,
    Shutdown = 22,
    ShutdownAck = 23,
    StreamShard = 30,
    ShardAck = 31,
    Cleanup = 32,
    CleanupAck = 33,
    FsListVolumes = 34,
    FsListVolumesAck = 35,
    FsListDir = 36,
    FsListDirAck = 37,
    /// `FsHash` asks the payload to BLAKE3-hash a single file. Used for
    /// reconciling local-vs-remote content in "Safe" resume mode, where
    /// size equality isn't enough. Request body: `{"path":"/abs/..."}`;
    /// ACK body: `{"hash":"<hex>","size":N}`.
    FsHash = 38,
    FsHashAck = 39,
    /// Destructive FS ops. Payload enforces a writable-root allowlist
    /// (/data/**, /user/**, /mnt/ext*/**, /mnt/usb*/**) and rejects
    /// paths containing `..`. Success → empty ACK body; failure →
    /// FsError with a short diagnostic string.
    FsDelete = 40,
    FsDeleteAck = 41,
    FsMove = 42,
    FsMoveAck = 43,
    FsChmod = 44,
    FsChmodAck = 45,
    FsMkdir = 46,
    FsMkdirAck = 47,
    /// Bounded read of a file on the PS5. Used to pull small metadata
    /// blobs (param.json, icon0.png) out of a game folder so the Library
    /// can render covers + titles. Request body is JSON:
    /// `{"path":"/abs/...","offset":N,"limit":N}`. ACK body is the raw
    /// bytes (not JSON-wrapped — metadata is in the frame header). The
    /// payload caps `limit` at FS_READ_MAX_BYTES to keep the response
    /// bounded regardless of what the client asks for.
    FsRead = 48,
    FsReadAck = 49,
    /// Recursive copy. Both `from` and `to` must pass the writable-root
    /// allowlist. Behaves like `cp -r` — descends directories, preserves
    /// file modes, refuses if `to` already exists (payload responds with
    /// a descriptive error; UI is expected to pre-check or offer merge).
    /// Request body: `{"from":"/abs/...","to":"/abs/..."}`. Success →
    /// empty ACK body.
    FsCopy = 50,
    FsCopyAck = 51,
    /// Mount a disk image file on the PS5. Attaches the image to a
    /// memory-disk unit (/dev/md<N>) via MDIOCATTACH, then `nmount`s
    /// with the right fstype (`exfatfs` for .exfat, `ufs` for .ffpkg).
    /// Request body: `{"image_path":"/abs/...","mount_name":"foo"}`
    /// (mount_name optional — derived from basename without extension
    /// when absent). ACK body:
    ///   `{"mount_point":"/mnt/ps5upload/foo","dev_node":"/dev/md0","fstype":"exfatfs"}`
    /// Mounting .ffpfs / PFS is explicitly NOT supported — that format
    /// needs per-image crypto keys we don't have and never will.
    FsMount = 52,
    FsMountAck = 53,
    /// Reverse of FS_MOUNT: unmount a previously-mounted image and
    /// detach its MD unit. Request body: `{"mount_point":"/mnt/ps5upload/foo"}`.
    /// Success → empty ACK body. Best-effort: the payload forces the
    /// unmount if the bare unmount is busy, since an orphaned MD
    /// attachment leaks a kernel device until reboot.
    FsUnmount = 54,
    FsUnmountAck = 55,
    /// Register a title from a PS5 folder (game folder on /data or
    /// content inside a mounted `/mnt/ps5upload/<name>/`). The payload
    /// stages sce_sys into `/user/app/<title_id>/`, nullfs-binds the
    /// source at `/system_ex/app/<title_id>/`, and calls Sony's
    /// sceAppInstUtilAppInstallTitleDir so the title appears in XMB.
    /// Request body: `{"src_path":"/abs/..."}`.
    /// ACK body: `{"title_id":"...","title_name":"...","used_nullfs":true}`.
    AppRegister = 56,
    AppRegisterAck = 57,
    /// Reverse of AppRegister. Unmounts the nullfs at
    /// `/system_ex/app/<title_id>/`, removes tracking files, and
    /// (on firmware where Sony's API is exported) calls
    /// sceAppInstUtilAppUninstall to clear the XMB tile. Request
    /// body: `{"title_id":"..."}`.
    AppUnregister = 58,
    AppUnregisterAck = 59,
    /// Start an already-registered title via sceLncUtilLaunchApp.
    /// Title must exist in app.db (register first if needed).
    /// Request body: `{"title_id":"..."}`.
    AppLaunch = 60,
    AppLaunchAck = 61,
    /// List titles currently registered in Sony's app.db. Fallbacks:
    /// "sqlite_unavailable" if libSceSqlite can't be dlopened (rare;
    /// only on firmware where Sony moved the library).
    /// Request body: empty.
    /// ACK body: `{"apps":[{"title_id":"...","title_name":"...",
    ///             "src":"/mnt/ps5upload/foo","image_backed":true}, …]}`
    /// where `src` is our mount.lnk contents (empty if we did not
    /// install the title) and `image_backed` is true iff a
    /// `mount_img.lnk` lives alongside (meaning the source path
    /// lives inside a disk image we mounted — unmounting the image
    /// will break the title).
    AppListRegistered = 62,
    AppListRegisteredAck = 63,
    /// Hardware monitoring frames. Bodies are newline-separated
    /// "key=value" text (not JSON) so a simple parser on the client
    /// can handle all three.
    HwInfo = 64,
    HwInfoAck = 65,
    HwTemps = 66,
    HwTempsAck = 67,
    HwPower = 68,
    HwPowerAck = 69,
    /// Open the PS5's built-in web browser (NPXS20001) via
    /// sceSystemServiceLaunchApp. Fire-and-forget.
    AppLaunchBrowser = 70,
    AppLaunchBrowserAck = 71,
    /// Set the PS5 fan turbo threshold in °C. The payload opens
    /// `/dev/icc_fan` and sends `ioctl(0xC01C8F07, {0,0,0,0,0, T,
    /// 0,0,0,0})` — the canonical fan-threshold ioctl on PS5,
    /// hardware-validated on 9.x–11.x and expected-to-work on 12.x
    /// (the `/dev/icc_fan` device + ioctl code have been stable
    /// across every SDK-supported firmware so far). Request body is ASCII
    /// decimal digits of the threshold (e.g. `"65"`) for parser
    /// simplicity; the payload clamps to a safe range (45–80 °C)
    /// regardless of what the client sends. Persists until PS5
    /// reboot. ACK body empty on success.
    ///
    /// Temperature READING is *not* available via any safe
    /// userland path on our payload — see hw_info.c header for
    /// why `sceKernelGet*Temperature` requires running inside the
    /// Sony shellui process. Fan speed READ is a gap in the whole
    /// jailbreak ecosystem (no known ioctl returns RPM).
    HwSetFanThreshold = 72,
    HwSetFanThresholdAck = 73,
    /// Walk the kernel's `allproc` list and return a JSON blob of
    /// `{"ok":bool,"procs":[{"pid":N,"name":"..."}...]}`. Read-only —
    /// the payload never writes back to the kernel through this path.
    /// Uses the SDK's `kernel_copyout` primitive, so it fails cleanly
    /// (with `kernel_rw_unavailable`) if the payload was loaded into
    /// a context that doesn't supply kernel R/W.
    ProcList = 74,
    ProcListAck = 75,
    /// Long-running fs-op progress + cancel. Body: `{"op_id":<u64>}`
    /// where op_id is the trace_id the engine sent on the originating
    /// FS_COPY (or future FS_MOVE-via-copy) frame. The payload looks
    /// up the in-flight op in its g_fs_ops table and replies with
    /// either a status snapshot or a cancel-ack. Both run on a
    /// separate mgmt-port worker than the FS_COPY itself so the engine
    /// can interleave status polls with the FS_COPY's blocking wait
    /// for FS_COPY_ACK.
    FsOpStatus = 76,
    FsOpStatusAck = 77,
    FsOpCancel = 78,
    FsOpCancelAck = 79,
    /// "Console Storage" aggregate matching what PS5 Settings shows.
    /// Sums `/user effective + /system_data + /system_ex` from
    /// `statfs(2)`. ACK body is `key=value\n…` with total_bytes,
    /// free_bytes, used_bytes, reserved_bytes, plus the per-partition
    /// breakdown. Read-only. Distinct from FS_LIST_VOLUMES, which
    /// lists per-volume detail; HW_STORAGE is the single-line summary
    /// the Hardware tab surfaces.
    HwStorage = 80,
    HwStorageAck = 81,
    /// Install a `.pkg` file via Sony's BGFT service. Body is a JSON
    /// object: `{"url":"http://<pc-ip>:<port>/pkg-host/<sess>/file.pkg",
    /// "content_id":"EP0006-CUSA12345_00-...","size":<u64>,
    /// "title":"...","package_type":"PS4GD"}`. The payload calls
    /// `sceBgftServiceDownloadRegisterTask` then
    /// `sceBgftServiceIntDownloadStartTask` and returns the assigned
    /// task_id. ACK body: `{"task_id":<i32>,"err_code":<u32>}` where
    /// err_code is non-zero only if BGFT itself rejected the call
    /// (already installed, no space, etc.). The actual download +
    /// install happens asynchronously inside the PS5 firmware; status
    /// is polled via PKG_INSTALL_STATUS.
    PkgInstall = 82,
    PkgInstallAck = 83,
    /// Poll a BGFT install in progress. Body: `{"task_id":<i32>}`.
    /// ACK body: `{"phase":"download|install|done|error",
    /// "downloaded":<u64>,"total":<u64>,"err_code":<u32>}`.
    /// `phase=done` with `err_code=0` means BGFT reported success;
    /// at this point the title should appear in the PS5 library.
    /// `err_code != 0` carries Sony's BGFT status code (0x80990xxx
    /// family) — caller maps to a user-facing message.
    PkgInstallStatus = 84,
    PkgInstallStatusAck = 85,
    /// Graceful PS5 power control. Body is JSON:
    /// `{"action":"reboot|shutdown|standby|tick"}`.
    ///   - reboot    → sceSystemServiceRequestReboot
    ///   - shutdown  → sceSystemServiceRequestPowerOff
    ///   - standby   → sceSystemStateMgrEnterStandby (rest mode)
    ///   - tick      → sceSystemServicePowerTick (defer auto-sleep
    ///     by one tick; useful during long uploads)
    ///
    /// ACK body: `{"ok":true}` on success, `{"ok":false,"err":"..."}`
    /// on Sony API failure. The destructive actions are async — the
    /// PS5 starts the shutdown sequence and our payload may not get
    /// to send the ACK before the network drops. Caller must treat
    /// "no ACK + connection drop" as success for those actions.
    SystemControl = 86,
    SystemControlAck = 87,
    /// Extended power telemetry beyond HwPower. Returns cumulative
    /// operating seconds (sceKernelIccGetPowerOperatingTime), boot
    /// cycle count (sceKernelIccGetPowerNumberOfBootShutdown), and
    /// thermal alert flags (sceKernelIccGetThermalAlert). Useful for
    /// hardware-health dashboards. ACK body is `key=value\n…` text.
    /// Read-only; safe even on jailbreak loaders without elevated
    /// caps (ICC calls don't require kernel R/W).
    PowerTelemetry = 88,
    PowerTelemetryAck = 89,
    /// Enumerate user accounts on the console. Body empty. ACK body
    /// is JSON: `{"foreground":<uid>,"users":[{"id":N,"name":"..."}, …]}`.
    /// Resolved via sceUserServiceGetForegroundUser +
    /// sceUserServiceGetUserName. Read-only.
    UserList = 90,
    UserListAck = 91,
    /// List save data for one user (or all users when `user_id` is 0).
    /// Body: `{"user_id":N}`. ACK body: JSON with per-game saves
    /// `{"saves":[{"title_id":"...","user_id":N,"path":"...",
    /// "size":<u64>,"mtime":<i64>}, …]}`. Walks both
    /// `/user/home/<uid>/savedata_prospero/<title>` (PS5 native) and
    /// `/user/home/<uid>/savedata/<title>` (PS4 legacy).
    ListSaves = 92,
    ListSavesAck = 93,
    /// List screenshots stored on the console. Body empty. ACK body:
    /// JSON `{"items":[{"path":"...","size":N,"mtime":<i64>}, …]}`
    /// where path is the .jpg/.jpeg under
    /// `/user/av_contents/thumbnails/photo` (recursive, 5 levels deep).
    ListScreenshots = 94,
    ListScreenshotsAck = 95,
    /// Build / query / search a filesystem index for fast wildcard
    /// matching across the whole console. The index is in-memory on
    /// the payload side, rebuilt on demand.
    /// IndexStart body: `{"roots":["/user","/data",…]}`; ACK with
    /// `{"started":true}`.
    /// IndexStatus body empty; ACK with
    /// `{"phase":"idle|building|ready","files":N,"started_at":<i64>,
    /// "completed_at":<i64>}`.
    /// SearchIndex body: `{"query":"*.pkg","size_min":N,"size_max":N,
    /// "limit":N}`; ACK with `{"results":[{"path":"...","size":N}, …]}`.
    IndexStart = 96,
    IndexStartAck = 97,
    IndexStatus = 98,
    IndexStatusAck = 99,
    SearchIndex = 100,
    SearchIndexAck = 101,
    IndexCancel = 102,
    IndexCancelAck = 103,
    /// App lifecycle control. Body is JSON
    /// `{"action":"suspend|resume|kill|list","app_id":<u32>}`.
    ///   - suspend → sceApplicationSuspend(app_id)
    ///   - resume  → sceApplicationResume(app_id)
    ///   - kill    → sceApplicationKill(app_id)
    ///   - list    → sceApplicationGetProcs (app_id ignored), returns
    ///     `{"apps":[{"app_id":N,"title_id":"...","name":"..."}, …]}`
    AppLifecycle = 104,
    AppLifecycleAck = 105,
    /// Toast — push a styled JSON notification to the PS5 via
    /// sceNotificationSend (libSceNotification). Body: JSON
    /// `{"title":"...","subtitle":"...","icon":"...","action_url":"..."}`.
    /// Fire-and-forget; ACK body is `{"ok":true}`.
    ToastSend = 106,
    ToastSendAck = 107,
    /// Read a chunk of the kernel log (/dev/klog). Body:
    /// `{"max_bytes":N}` (max 64 KiB). ACK body is the raw kernel
    /// log text (newline-delimited). Each call drains and returns
    /// what's currently buffered; the renderer polls every ~1s for
    /// streaming. The kernel buffer is FIFO so missed reads aren't
    /// re-readable — for serious logging the user should also
    /// configure on-PS5 logging to disk.
    KlogRead = 108,
    KlogReadAck = 109,
    /// Enumerate network interfaces (sceNetGetIfList). ACK body:
    /// JSON `{"interfaces":[{"name":"eth0","mac":"aa:bb:..","ipv4":"...",
    /// "ipv6":"...","link_state":"up|down","mtu":N}, …]}`. Read-only.
    NetInterfaces = 110,
    NetInterfacesAck = 111,
    /// Peripheral control: BD drive power, USB port power, eject
    /// disc. Body: `{"action":"eject_disc|bd_power_off|bd_power_on|
    /// usb_port_off|usb_port_on","port":N}` (port for usb_*).
    /// Sony's ICC controls; safe even on payloads without kernel R/W.
    PeripheralControl = 112,
    PeripheralControlAck = 113,
    /// Enumerate loaded sprx modules in a process. Body:
    /// `{"pid":N}`. ACK body: JSON `{"modules":[{"name":"...",
    /// "base":<u64>,"size":<u64>}, …]}`. Uses sceKernelGetModuleList.
    /// Read-only.
    ProcModules = 114,
    ProcModulesAck = 115,
    /// Run a single shell command on the PS5 and capture output.
    /// Body: `{"cmd":"...","session_id":"...","cwd":"/abs/path","timeout_secs":N}`.
    /// ACK body: `{"exit_code":N,"stdout":"...","cwd":"/abs/path","session_id":"...","timed_out":bool}`.
    /// stdout capped at 256 KB.
    ShellExec = 116,
    ShellExecAck = 117,
    /// CRC32 hash of a file. Body: `{"path":"/abs/..."}`. ACK body:
    /// `{"crc32":<u32>,"size":<u64>}`. Cheap integrity check; for
    /// crypto-strength use FsHash (BLAKE3) instead.
    Crc32File = 118,
    Crc32FileAck = 119,
    /// Direct sqlite query of /system_data/priv/mms/app.db for the
    /// title_id ↔ app_id ↔ display name mapping. Body empty. ACK
    /// body: `{"apps":[{"title_id":"CUSAxxxxx","app_id":N,
    /// "name":"..."}, …]}`. Read-only.
    AppDbQuery = 120,
    AppDbQueryAck = 121,
    /// Network round-trip + throughput test. Body: `{"shard_count":N,
    /// "shard_size_bytes":N}`. Payload echoes each shard back without
    /// writing to disk. ACK body: `{"sent":N,"recv":N,"bytes":N,
    /// "elapsed_us":N,"latency_us_p50":N,"latency_us_p95":N}`.
    /// Useful for diagnosing slow uploads ("is the bottleneck the
    /// LAN or the PS5's disk?").
    NetSpeedTest = 122,
    NetSpeedTestAck = 123,
    /// Direct .pkg mount via sceFsMountGamePkg, bypassing BGFT.
    /// Body: `{"pkg_path":"/abs/...","mount_point":"/mnt/ps5upload/foo"}`.
    /// ACK body: `{"ok":bool,"code":N,"err":"..."}`. Faster than full
    /// install for testing patches; doesn't register the title in
    /// the home screen.
    PkgDirectMount = 124,
    PkgDirectMountAck = 125,
    /// Run sceFsUfsFsck on a UFS partition. Body:
    /// `{"device":"/dev/ufs0","repair":bool}`. ACK body:
    /// `{"ok":bool,"code":N}`. Read-only by default; the `repair`
    /// flag must be explicitly true (UI confirms before sending).
    UfsFsck = 126,
    UfsFsckAck = 127,
    /// Write a small file (≤256 KB) on the PS5 atomically.
    /// Body: `{"path":"/abs/...","bytes":"<base64>","mode":"create|overwrite"}`.
    /// `mode=create` fails if the file exists; `mode=overwrite` (default)
    /// replaces it via tmp+rename. Useful for patching small config
    /// files without going through the heavyweight transfer pipeline.
    /// ACK body: `{"ok":bool,"size":N}`.
    FsWriteBytes = 130,
    FsWriteBytesAck = 131,
    /// Mount a LWFS patch overlay via sceFsMountLwfs. Body:
    /// `{"patch_path":"/abs/...","mount_point":"/mnt/...","title_id":"..."}`.
    /// Patches a title's files without a full reinstall. ACK body
    /// mirrors PKG_DIRECT_MOUNT_ACK: `{"ok":bool,"code":N,
    /// "mount_point":"..."}`. Best-effort; sceFsMountLwfs's flag
    /// semantics are sparsely documented across firmware revisions.
    LwfsMount = 128,
    LwfsMountAck = 129,
    /// PS5 system clock get/set via sceSystemServiceGet/SetCurrentDateTime.
    /// All UTC.
    ///   TIME_GET  request body: empty.
    ///   TIME_GET_ACK body: `{"ok":bool,"err_code":N,"year":Y,
    ///                       "month":M,"day":D,"hour":h,"min":m,"sec":s}`.
    ///   TIME_SET  request body: `{"year":Y,"month":M,"day":D,
    ///                            "hour":h,"min":m,"sec":s}`.
    ///   TIME_SET_ACK body: `{"ok":bool,"err_code":N,
    ///                       "prior_unix":N,"new_unix":N}`.
    /// `prior_unix` + `new_unix` are -1 if the bookend get failed; the
    /// desktop uses them to detect "rc=0 but the clock didn't actually
    /// move" SDK-stub no-ops on firmwares where the symbol exists but
    /// the runtime SPRX doesn't implement the underlying syscall.
    /// The set call requires a ucred-elevated loader (kstuff or
    /// equivalent) — without it, SceShellCore's authid check rejects
    /// the IPC and the payload surfaces the Sony err_code.
    TimeGet = 132,
    TimeGetAck = 133,
    TimeSet = 134,
    TimeSetAck = 135,
    /// Read all PS5 Date & Time state in one round-trip: wall-clock
    /// (UTC), NTP-derived tick (cached, no fresh sync), timezone
    /// index + offset, DST policy + current flag, date/time format
    /// preference, auto-sync (NTP) flag, tzdata version, and the
    /// NTP-sync error counter. Body: empty. Ack body: JSON object
    /// with per-field availability flags so the desktop can grey
    /// out fields the payload couldn't read on this firmware.
    TimeStateGet = 136,
    TimeStateGetAck = 137,
    /// Write a subset of PS5 Date & Time state — timezone index,
    /// DST policy, date/time format, auto-sync flag. Each field is
    /// optional in the request JSON; only present fields are
    /// written. Ack body returns per-field rc + err_code so the
    /// desktop can report which writes took and which were rejected
    /// (e.g. write to set_auto succeeded but tz_index didn't).
    /// Writes go through sceRegMgrSetInt which needs ucred elevation
    /// — same envelope as TimeSet.
    TimeStateSet = 138,
    TimeStateSetAck = 139,
    /// ShadowMountPlus metadata self-healer control frame. The payload
    /// has a background worker (`smp_meta.c`) that copies missing
    /// icon0.png / pic*.png / param.json from `/user/app/<TID>/sce_sys`
    /// into `/user/appmeta/<TID>` to fix SMP's "blank home-screen tile"
    /// failure mode. The worker is off until the user opts in. Body is
    /// JSON: `{"action":"start|run_now|set_poll","interval"?: <int>}`.
    /// Sub-actions:
    ///
    ///   - `start` — first call launches the watcher (idempotent on
    ///     subsequent calls). The worker waits 60 s for kstuff/SMP to
    ///     settle before its first sweep, then sweeps every
    ///     `poll_seconds` (default 30).
    ///   - `run_now` — set the run-now flag so the next 1 s tick triggers
    ///     an immediate sweep instead of waiting for the next interval.
    ///   - `set_poll` — set the watcher's tick interval. Clamped to
    ///     [5, 600] seconds inside the payload; ACK echoes the clamped
    ///     value so the UI can reconcile slider position.
    ///
    /// Ack body: `{"ok":true,"poll_seconds":<int>}` on success;
    /// `{"ok":false,"err":"..."}` on failure (e.g. pthread_create EAGAIN).
    SmpMetaControl = 140,
    SmpMetaControlAck = 141,
    /// Snapshot of the SMP-meta worker's stats. Body: empty. Ack body:
    /// JSON with `running`, `poll_seconds`, `last_run_unix`,
    /// `games_scanned`, `icons_healed`, `pics_healed`, `json_healed`,
    /// `still_missing`, and `last_missing` (TITLE_ID of most recent
    /// unfixable game, or empty string when everything is healthy).
    /// Safe to call before SmpMetaControl/start — returns
    /// `running:false` with zeroed counters.
    SmpMetaStats = 142,
    SmpMetaStatsAck = 143,
    /// Recent kernel log lines (PS5-side `dmesg` equivalent). Body: empty.
    /// Ack body: raw text bytes from `sysctl kern.msgbuf` — the kernel
    /// circular buffer of recent printf/printk output. Used by the desktop
    /// to diagnose "why didn't the payload load?" / "why is the helper
    /// silent?" without making the user dig through ssh / ftp.
    SyslogTail = 144,
    SyslogTailAck = 145,
    /// One-way payload → client during the COMMIT_TX apply loop on
    /// multi-file uploads when the client opted in via
    /// `TX_FLAG_APPLY_PROGRESS_REQUESTED`. Body is a small JSON object
    /// (~80 bytes) carrying `{"files_applied":N,"total_files":M,
    /// "bytes_applied":B}`. Emitted every ~1 sec or every 1024 files
    /// applied (whichever first); always at least once at the start
    /// and once at the end of the apply loop. The client must keep
    /// reading frames after CommitTx until CommitTxAck arrives —
    /// progress frames interleave with the eventual ack.
    ///
    /// No ack — fire-and-forget. If a frame is dropped (TCP buffer
    /// drain stalls), the next one carries the latest counters, so
    /// the UI stays "eventually correct".
    ApplyProgress = 146,
    /// Profile — avatar image + offline-account (username) operations.
    ///   PROFILE_INFO         req: empty. Ack: `{"ok":bool,"uid":N,
    ///     "uid_hex":"0x..","username":"..","slots":[{"slot":N,"name":"..",
    ///     "type":"..","flags":N,"id":"0x..","activated":bool}]}` — the
    ///     foreground user + the offline-account name slots.
    ///   PROFILE_SET_USERNAME req: `{"slot":N,"name":".."}` → renames an
    ///     account-name slot via sceRegMgrSetStr. Ack:
    ///     `{"ok":bool,"slot":N,"name":"..","err_code":N}`.
    ///   PROFILE_ACTIVATE     req: `{"slot":N,"id"?:"0x.."}` → sets the
    ///     slot id (derived from the name if absent) + type "np" +
    ///     DEFAULT_FLAGS. Ack: `{"ok":bool,"slot":N,"id":"0x.."}`.
    ///   PROFILE_APPLY_AVATAR req: `{"uid":N}` → copies the host-staged
    ///     DDS + online.json files from `/data/ps5upload/profile/0x<UID>/`
    ///     into the live profile cache dir (privileged). The desktop does
    ///     the decode/resize/DXT5 encode and stages the files via
    ///     FS_WRITE_BYTES first. Ack: `{"ok":bool,"uid":N,"copied":N}`.
    ///   PROFILE_CLEAR_SLOT   req: `{"slot":N}` → zero id + flags. Ack:
    ///     `{"ok":bool,"slot":N}`.
    /// Registry + /system_data writes need ucred elevation (same envelope
    /// as the time/registry frames).
    ProfileInfo = 150,
    ProfileInfoAck = 151,
    ProfileSetUsername = 152,
    ProfileSetUsernameAck = 153,
    ProfileActivate = 154,
    ProfileActivateAck = 155,
    ProfileApplyAvatar = 156,
    ProfileApplyAvatarAck = 157,
    ProfileClearSlot = 158,
    ProfileClearSlotAck = 159,
    /// Rename a local console user (the active profile's display name) via
    /// sceUserServiceSetUserName. req `{"uid":N,"name":".."}` → ack
    /// `{"ok":bool,"uid":N,"name":".."}`. Distinct from PROFILE_SET_USERNAME
    /// (which renames an offline-account registry slot).
    ProfileSetLocalUsername = 160,
    ProfileSetLocalUsernameAck = 161,
    /// Process manager. List enumerates running processes via
    /// sysctl(KERN_PROC) — ack `{"processes":[{pid,name,title_id,app_id,
    /// memory_mib,threads,kind}]}` where kind ∈ app|payload|system. Kill
    /// sends SIGKILL to a pid: req `{"pid":N}` → ack `{"ok":bool,...}`.
    /// Read-only enumerate needs no elevation; kill runs as the payload's
    /// (elevated) ucred. Restart is the client composing Kill + AppLaunch.
    ProcessList = 162,
    ProcessListAck = 163,
    ProcessKill = 164,
    ProcessKillAck = 165,
    /// List gameplay video clips stored on the console. Body empty. ACK
    /// body: JSON `{"items":[{"path":"...","size":N,"mtime":<i64>}, …]}`,
    /// same shape as ListScreenshotsAck, where path is a `.webm`/`.mp4`
    /// under `/user/av_contents/video` (recursive, 5 levels deep).
    ListVideos = 166,
    ListVideosAck = 167,
}

impl FrameType {
    pub fn try_from_u16(v: u16) -> Result<Self, DecodeError> {
        match v {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Error),
            10 => Ok(Self::BeginTx),
            11 => Ok(Self::BeginTxAck),
            12 => Ok(Self::QueryTx),
            13 => Ok(Self::QueryTxAck),
            14 => Ok(Self::CommitTx),
            15 => Ok(Self::CommitTxAck),
            16 => Ok(Self::AbortTx),
            17 => Ok(Self::AbortTxAck),
            18 => Ok(Self::TakeoverRequest),
            19 => Ok(Self::TakeoverAck),
            20 => Ok(Self::Status),
            21 => Ok(Self::StatusAck),
            22 => Ok(Self::Shutdown),
            23 => Ok(Self::ShutdownAck),
            30 => Ok(Self::StreamShard),
            31 => Ok(Self::ShardAck),
            32 => Ok(Self::Cleanup),
            33 => Ok(Self::CleanupAck),
            34 => Ok(Self::FsListVolumes),
            35 => Ok(Self::FsListVolumesAck),
            36 => Ok(Self::FsListDir),
            37 => Ok(Self::FsListDirAck),
            38 => Ok(Self::FsHash),
            39 => Ok(Self::FsHashAck),
            40 => Ok(Self::FsDelete),
            41 => Ok(Self::FsDeleteAck),
            42 => Ok(Self::FsMove),
            43 => Ok(Self::FsMoveAck),
            44 => Ok(Self::FsChmod),
            45 => Ok(Self::FsChmodAck),
            46 => Ok(Self::FsMkdir),
            47 => Ok(Self::FsMkdirAck),
            48 => Ok(Self::FsRead),
            49 => Ok(Self::FsReadAck),
            50 => Ok(Self::FsCopy),
            51 => Ok(Self::FsCopyAck),
            52 => Ok(Self::FsMount),
            53 => Ok(Self::FsMountAck),
            54 => Ok(Self::FsUnmount),
            55 => Ok(Self::FsUnmountAck),
            56 => Ok(Self::AppRegister),
            57 => Ok(Self::AppRegisterAck),
            58 => Ok(Self::AppUnregister),
            59 => Ok(Self::AppUnregisterAck),
            60 => Ok(Self::AppLaunch),
            61 => Ok(Self::AppLaunchAck),
            62 => Ok(Self::AppListRegistered),
            63 => Ok(Self::AppListRegisteredAck),
            64 => Ok(Self::HwInfo),
            65 => Ok(Self::HwInfoAck),
            66 => Ok(Self::HwTemps),
            67 => Ok(Self::HwTempsAck),
            68 => Ok(Self::HwPower),
            69 => Ok(Self::HwPowerAck),
            70 => Ok(Self::AppLaunchBrowser),
            71 => Ok(Self::AppLaunchBrowserAck),
            72 => Ok(Self::HwSetFanThreshold),
            73 => Ok(Self::HwSetFanThresholdAck),
            74 => Ok(Self::ProcList),
            75 => Ok(Self::ProcListAck),
            76 => Ok(Self::FsOpStatus),
            77 => Ok(Self::FsOpStatusAck),
            78 => Ok(Self::FsOpCancel),
            79 => Ok(Self::FsOpCancelAck),
            80 => Ok(Self::HwStorage),
            81 => Ok(Self::HwStorageAck),
            82 => Ok(Self::PkgInstall),
            83 => Ok(Self::PkgInstallAck),
            84 => Ok(Self::PkgInstallStatus),
            85 => Ok(Self::PkgInstallStatusAck),
            86 => Ok(Self::SystemControl),
            87 => Ok(Self::SystemControlAck),
            88 => Ok(Self::PowerTelemetry),
            89 => Ok(Self::PowerTelemetryAck),
            90 => Ok(Self::UserList),
            91 => Ok(Self::UserListAck),
            92 => Ok(Self::ListSaves),
            93 => Ok(Self::ListSavesAck),
            94 => Ok(Self::ListScreenshots),
            95 => Ok(Self::ListScreenshotsAck),
            96 => Ok(Self::IndexStart),
            97 => Ok(Self::IndexStartAck),
            98 => Ok(Self::IndexStatus),
            99 => Ok(Self::IndexStatusAck),
            100 => Ok(Self::SearchIndex),
            101 => Ok(Self::SearchIndexAck),
            102 => Ok(Self::IndexCancel),
            103 => Ok(Self::IndexCancelAck),
            104 => Ok(Self::AppLifecycle),
            105 => Ok(Self::AppLifecycleAck),
            106 => Ok(Self::ToastSend),
            107 => Ok(Self::ToastSendAck),
            108 => Ok(Self::KlogRead),
            109 => Ok(Self::KlogReadAck),
            110 => Ok(Self::NetInterfaces),
            111 => Ok(Self::NetInterfacesAck),
            112 => Ok(Self::PeripheralControl),
            113 => Ok(Self::PeripheralControlAck),
            114 => Ok(Self::ProcModules),
            115 => Ok(Self::ProcModulesAck),
            116 => Ok(Self::ShellExec),
            117 => Ok(Self::ShellExecAck),
            118 => Ok(Self::Crc32File),
            119 => Ok(Self::Crc32FileAck),
            120 => Ok(Self::AppDbQuery),
            121 => Ok(Self::AppDbQueryAck),
            122 => Ok(Self::NetSpeedTest),
            123 => Ok(Self::NetSpeedTestAck),
            124 => Ok(Self::PkgDirectMount),
            125 => Ok(Self::PkgDirectMountAck),
            126 => Ok(Self::UfsFsck),
            127 => Ok(Self::UfsFsckAck),
            128 => Ok(Self::LwfsMount),
            129 => Ok(Self::LwfsMountAck),
            130 => Ok(Self::FsWriteBytes),
            131 => Ok(Self::FsWriteBytesAck),
            132 => Ok(Self::TimeGet),
            133 => Ok(Self::TimeGetAck),
            134 => Ok(Self::TimeSet),
            135 => Ok(Self::TimeSetAck),
            136 => Ok(Self::TimeStateGet),
            137 => Ok(Self::TimeStateGetAck),
            138 => Ok(Self::TimeStateSet),
            139 => Ok(Self::TimeStateSetAck),
            140 => Ok(Self::SmpMetaControl),
            141 => Ok(Self::SmpMetaControlAck),
            142 => Ok(Self::SmpMetaStats),
            143 => Ok(Self::SmpMetaStatsAck),
            144 => Ok(Self::SyslogTail),
            145 => Ok(Self::SyslogTailAck),
            146 => Ok(Self::ApplyProgress),
            150 => Ok(Self::ProfileInfo),
            151 => Ok(Self::ProfileInfoAck),
            152 => Ok(Self::ProfileSetUsername),
            153 => Ok(Self::ProfileSetUsernameAck),
            154 => Ok(Self::ProfileActivate),
            155 => Ok(Self::ProfileActivateAck),
            156 => Ok(Self::ProfileApplyAvatar),
            157 => Ok(Self::ProfileApplyAvatarAck),
            158 => Ok(Self::ProfileClearSlot),
            159 => Ok(Self::ProfileClearSlotAck),
            160 => Ok(Self::ProfileSetLocalUsername),
            161 => Ok(Self::ProfileSetLocalUsernameAck),
            162 => Ok(Self::ProcessList),
            163 => Ok(Self::ProcessListAck),
            164 => Ok(Self::ProcessKill),
            165 => Ok(Self::ProcessKillAck),
            166 => Ok(Self::ListVideos),
            167 => Ok(Self::ListVideosAck),
            _ => Err(DecodeError::UnknownFrameType(v)),
        }
    }
}

// ─── Frame header ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    pub magic: u32,
    pub version: u16,
    pub frame_type: u16,
    pub flags: u32,
    pub body_len: u64,
    pub trace_id: u64,
}

impl FrameHeader {
    pub fn new(frame_type: FrameType, flags: u32, body_len: u64, trace_id: u64) -> Self {
        Self {
            magic: FTX2_MAGIC,
            version: FTX2_VERSION_1,
            frame_type: frame_type as u16,
            flags,
            body_len,
            trace_id,
        }
    }

    pub fn encode(self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..6].copy_from_slice(&self.version.to_le_bytes());
        out[6..8].copy_from_slice(&self.frame_type.to_le_bytes());
        out[8..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..20].copy_from_slice(&self.body_len.to_le_bytes());
        out[20..28].copy_from_slice(&self.trace_id.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != FRAME_HEADER_LEN {
            return Err(DecodeError::InvalidHeaderLength {
                expected: FRAME_HEADER_LEN,
                actual: bytes.len(),
            });
        }

        let header = Self {
            magic: u32::from_le_bytes(bytes[0..4].try_into().expect("slice length checked")),
            version: u16::from_le_bytes(bytes[4..6].try_into().expect("slice length checked")),
            frame_type: u16::from_le_bytes(bytes[6..8].try_into().expect("slice length checked")),
            flags: u32::from_le_bytes(bytes[8..12].try_into().expect("slice length checked")),
            body_len: u64::from_le_bytes(bytes[12..20].try_into().expect("slice length checked")),
            trace_id: u64::from_le_bytes(bytes[20..28].try_into().expect("slice length checked")),
        };

        if header.magic != FTX2_MAGIC {
            return Err(DecodeError::BadMagic(header.magic));
        }
        if header.version != FTX2_VERSION_1 {
            return Err(DecodeError::UnsupportedVersion(header.version));
        }

        Ok(header)
    }

    /// Decoded frame type, or an error if the u16 is not a known variant.
    pub fn frame_type(&self) -> Result<FrameType, DecodeError> {
        FrameType::try_from_u16(self.frame_type)
    }
}

// ─── TxMeta ──────────────────────────────────────────────────────────────────
//
// Binary prefix carried in the body of BEGIN_TX, QUERY_TX, and ABORT_TX frames.
// Layout (little-endian, 24 bytes total) matches the C payload's ftx2_tx_meta_t:
//
//   tx_id:  [u8; 16]
//   kind:   u32
//   flags:  u32 — see TX_FLAG_* below. Bits unused pre-2.1.

/// BeginTx only: resume an already-interrupted tx_id instead of allocating
/// a fresh entry. When set, the payload looks up the tx_id in its journal;
/// if found in `interrupted` state, it preserves the existing `shards_received`
/// counter and reports it back via BeginTxAck's `last_acked_shard` field so
/// the client can skip past shards the server already has. If the tx_id is
/// unknown or not in resumable state, the payload treats the request as a
/// fresh BeginTx (resume flag becomes a no-op).
pub const TX_FLAG_RESUME: u32 = 0x1;

/// BeginTx only: client opts in to receive `ApplyProgress` frames from
/// the payload during the multi-file COMMIT_TX apply loop. Payloads
/// that don't recognise the flag ignore it and behave as before
/// (silent apply, single CommitTxAck at the end). Payloads that do
/// recognise it stream {"files_applied","total_files","bytes_applied"}
/// frames every ~1 sec or every 1024 files committed, in addition to
/// the final CommitTxAck. Engine clients use this to surface a live
/// "Finalized N of M files" counter to the user instead of an
/// indeterminate "Finalizing on PS5…" wait.
///
/// Opt-in so old clients don't choke on unknown frames mid-commit:
/// silent payloads accept the flag harmlessly, chatty payloads stay
/// silent for old clients.
pub const TX_FLAG_APPLY_PROGRESS_REQUESTED: u32 = 0x4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxMeta {
    pub tx_id: [u8; 16],
    pub kind: u32,
    pub flags: u32,
}

impl TxMeta {
    pub fn encode(self) -> [u8; TX_META_LEN] {
        let mut out = [0u8; TX_META_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..20].copy_from_slice(&self.kind.to_le_bytes());
        out[20..24].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < TX_META_LEN {
            return Err(DecodeError::InvalidTxMetaLength {
                expected: TX_META_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("slice length checked"),
            kind: u32::from_le_bytes(bytes[16..20].try_into().expect("slice length checked")),
            flags: u32::from_le_bytes(bytes[20..24].try_into().expect("slice length checked")),
        })
    }

    /// Return the extra body bytes that follow the metadata prefix.
    pub fn extra(bytes: &[u8]) -> &[u8] {
        if bytes.len() > TX_META_LEN {
            &bytes[TX_META_LEN..]
        } else {
            &[]
        }
    }
}

// ─── Core identifiers ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxId(pub [u8; 16]);

impl TxId {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobId(pub [u8; 16]);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestHash(pub [u8; 32]);

// ─── Semantic types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionKind {
    UploadTree,
    UploadFile,
    DownloadTree,
    DownloadFile,
    LocalCopy,
    LocalMove,
    ExtractArchive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyMode {
    DirectApply,
    SpooledApply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AckState {
    Spooled = 0,
    Applied = 1,
    DuplicateIgnored = 2,
}

impl AckState {
    pub fn try_from_u8(v: u8) -> Result<Self, DecodeError> {
        match v {
            0 => Ok(Self::Spooled),
            1 => Ok(Self::Applied),
            2 => Ok(Self::DuplicateIgnored),
            _ => Err(DecodeError::UnknownAckState(v)),
        }
    }
}

// ─── ShardHeader ─────────────────────────────────────────────────────────────
//
// Binary prefix at the start of every STREAM_SHARD body (64 bytes, LE):
//
//   tx_id:        [u8; 16]   offset  0
//   shard_seq:    u64        offset 16
//   shard_digest: [u8; 32]   offset 24  (BLAKE3-256)
//   record_count: u32        offset 56
//   flags:        u32        offset 60

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardHeader {
    pub tx_id: [u8; 16],
    pub shard_seq: u64,
    pub shard_digest: [u8; 32],
    pub record_count: u32,
    pub flags: u32,
}

impl ShardHeader {
    pub fn encode(&self) -> [u8; SHARD_HEADER_LEN] {
        let mut out = [0u8; SHARD_HEADER_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..24].copy_from_slice(&self.shard_seq.to_le_bytes());
        out[24..56].copy_from_slice(&self.shard_digest);
        out[56..60].copy_from_slice(&self.record_count.to_le_bytes());
        out[60..64].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SHARD_HEADER_LEN {
            return Err(DecodeError::InvalidShardHeaderLength {
                expected: SHARD_HEADER_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("checked"),
            shard_seq: u64::from_le_bytes(bytes[16..24].try_into().expect("checked")),
            shard_digest: bytes[24..56].try_into().expect("checked"),
            record_count: u32::from_le_bytes(bytes[56..60].try_into().expect("checked")),
            flags: u32::from_le_bytes(bytes[60..64].try_into().expect("checked")),
        })
    }
}

// ─── ShardAck ────────────────────────────────────────────────────────────────
//
// Binary body of SHARD_ACK frames (48 bytes, LE):
//
//   tx_id:                [u8; 16]  offset  0
//   shard_seq:            u64       offset 16
//   ack_state:            u8        offset 24
//   _pad:                 [u8; 7]   offset 25
//   bytes_committed_total: u64      offset 32
//   files_committed_total: u64      offset 40

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardAck {
    pub tx_id: [u8; 16],
    pub shard_seq: u64,
    pub ack_state: AckState,
    pub bytes_committed_total: u64,
    pub files_committed_total: u64,
}

impl ShardAck {
    pub fn encode(&self) -> [u8; SHARD_ACK_LEN] {
        let mut out = [0u8; SHARD_ACK_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..24].copy_from_slice(&self.shard_seq.to_le_bytes());
        out[24] = self.ack_state as u8;
        // out[25..32] stays zero (pad)
        out[32..40].copy_from_slice(&self.bytes_committed_total.to_le_bytes());
        out[40..48].copy_from_slice(&self.files_committed_total.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SHARD_ACK_LEN {
            return Err(DecodeError::InvalidShardAckLength {
                expected: SHARD_ACK_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("checked"),
            shard_seq: u64::from_le_bytes(bytes[16..24].try_into().expect("checked")),
            ack_state: AckState::try_from_u8(bytes[24])?,
            bytes_committed_total: u64::from_le_bytes(bytes[32..40].try_into().expect("checked")),
            files_committed_total: u64::from_le_bytes(bytes[40..48].try_into().expect("checked")),
        })
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("invalid header length: expected {expected}, got {actual}")]
    InvalidHeaderLength { expected: usize, actual: usize },
    #[error("invalid magic: {0:#010x}")]
    BadMagic(u32),
    #[error("unsupported version: {0}")]
    UnsupportedVersion(u16),
    #[error("unknown frame type: {0}")]
    UnknownFrameType(u16),
    #[error("unknown ack state: {0}")]
    UnknownAckState(u8),
    #[error("invalid tx_meta length: expected >= {expected}, got {actual}")]
    InvalidTxMetaLength { expected: usize, actual: usize },
    #[error("invalid shard header length: expected >= {expected}, got {actual}")]
    InvalidShardHeaderLength { expected: usize, actual: usize },
    #[error("invalid shard ack length: expected >= {expected}, got {actual}")]
    InvalidShardAckLength { expected: usize, actual: usize },
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_round_trip() {
        let header = FrameHeader::new(FrameType::Hello, 7, 1234, 55);
        let bytes = header.encode();
        let decoded = FrameHeader::decode(&bytes).expect("decode should succeed");
        assert_eq!(decoded.magic, FTX2_MAGIC);
        assert_eq!(decoded.version, FTX2_VERSION_1);
        assert_eq!(decoded.frame_type, FrameType::Hello as u16);
        assert_eq!(decoded.flags, 7);
        assert_eq!(decoded.body_len, 1234);
        assert_eq!(decoded.trace_id, 55);
    }

    #[test]
    fn frame_type_decoded_from_header() {
        let header = FrameHeader::new(FrameType::Status, 0, 0, 1);
        let bytes = header.encode();
        let decoded = FrameHeader::decode(&bytes).unwrap();
        assert_eq!(decoded.frame_type().unwrap(), FrameType::Status);
    }

    #[test]
    fn frame_type_all_variants_round_trip() {
        let variants = [
            FrameType::Hello,
            FrameType::HelloAck,
            FrameType::Error,
            FrameType::BeginTx,
            FrameType::BeginTxAck,
            FrameType::QueryTx,
            FrameType::QueryTxAck,
            FrameType::CommitTx,
            FrameType::CommitTxAck,
            FrameType::AbortTx,
            FrameType::AbortTxAck,
            FrameType::TakeoverRequest,
            FrameType::TakeoverAck,
            FrameType::Status,
            FrameType::StatusAck,
            FrameType::Shutdown,
            FrameType::ShutdownAck,
            FrameType::StreamShard,
            FrameType::ShardAck,
            FrameType::Cleanup,
            FrameType::CleanupAck,
            FrameType::FsListVolumes,
            FrameType::FsListVolumesAck,
            FrameType::FsListDir,
            FrameType::FsListDirAck,
            FrameType::FsHash,
            FrameType::FsHashAck,
            FrameType::FsDelete,
            FrameType::FsDeleteAck,
            FrameType::FsMove,
            FrameType::FsMoveAck,
            FrameType::FsChmod,
            FrameType::FsChmodAck,
            FrameType::FsMkdir,
            FrameType::FsMkdirAck,
            FrameType::FsRead,
            FrameType::FsReadAck,
            FrameType::FsCopy,
            FrameType::FsCopyAck,
            FrameType::FsMount,
            FrameType::FsMountAck,
            FrameType::FsUnmount,
            FrameType::FsUnmountAck,
            FrameType::AppRegister,
            FrameType::AppRegisterAck,
            FrameType::AppUnregister,
            FrameType::AppUnregisterAck,
            FrameType::AppLaunch,
            FrameType::AppLaunchAck,
            FrameType::AppListRegistered,
            FrameType::AppListRegisteredAck,
            FrameType::HwInfo,
            FrameType::HwInfoAck,
            FrameType::HwTemps,
            FrameType::HwTempsAck,
            FrameType::HwPower,
            FrameType::HwPowerAck,
            FrameType::AppLaunchBrowser,
            FrameType::AppLaunchBrowserAck,
            FrameType::HwSetFanThreshold,
            FrameType::HwSetFanThresholdAck,
            FrameType::ProcList,
            FrameType::ProcListAck,
            FrameType::FsOpStatus,
            FrameType::FsOpStatusAck,
            FrameType::FsOpCancel,
            FrameType::FsOpCancelAck,
            FrameType::HwStorage,
            FrameType::HwStorageAck,
            FrameType::PkgInstall,
            FrameType::PkgInstallAck,
            FrameType::PkgInstallStatus,
            FrameType::PkgInstallStatusAck,
            FrameType::SystemControl,
            FrameType::SystemControlAck,
            FrameType::PowerTelemetry,
            FrameType::PowerTelemetryAck,
            FrameType::UserList,
            FrameType::UserListAck,
            FrameType::ListSaves,
            FrameType::ListSavesAck,
            FrameType::ListScreenshots,
            FrameType::ListScreenshotsAck,
            FrameType::IndexStart,
            FrameType::IndexStartAck,
            FrameType::IndexStatus,
            FrameType::IndexStatusAck,
            FrameType::SearchIndex,
            FrameType::SearchIndexAck,
            FrameType::IndexCancel,
            FrameType::IndexCancelAck,
            FrameType::AppLifecycle,
            FrameType::AppLifecycleAck,
            FrameType::ToastSend,
            FrameType::ToastSendAck,
            FrameType::KlogRead,
            FrameType::KlogReadAck,
            FrameType::NetInterfaces,
            FrameType::NetInterfacesAck,
            FrameType::PeripheralControl,
            FrameType::PeripheralControlAck,
            FrameType::ProcModules,
            FrameType::ProcModulesAck,
            FrameType::ShellExec,
            FrameType::ShellExecAck,
            FrameType::Crc32File,
            FrameType::Crc32FileAck,
            FrameType::AppDbQuery,
            FrameType::AppDbQueryAck,
            FrameType::NetSpeedTest,
            FrameType::NetSpeedTestAck,
            FrameType::PkgDirectMount,
            FrameType::PkgDirectMountAck,
            FrameType::UfsFsck,
            FrameType::UfsFsckAck,
            FrameType::LwfsMount,
            FrameType::LwfsMountAck,
            FrameType::FsWriteBytes,
            FrameType::FsWriteBytesAck,
            FrameType::TimeGet,
            FrameType::TimeGetAck,
            FrameType::TimeSet,
            FrameType::TimeSetAck,
            FrameType::TimeStateGet,
            FrameType::TimeStateGetAck,
            FrameType::TimeStateSet,
            FrameType::TimeStateSetAck,
            FrameType::SmpMetaControl,
            FrameType::SmpMetaControlAck,
            FrameType::SmpMetaStats,
            FrameType::SmpMetaStatsAck,
            FrameType::SyslogTail,
            FrameType::SyslogTailAck,
            FrameType::ApplyProgress,
        ];
        for ft in variants {
            assert_eq!(FrameType::try_from_u16(ft as u16).unwrap(), ft);
        }
    }

    #[test]
    fn unknown_frame_type_is_error() {
        assert!(matches!(
            FrameType::try_from_u16(0xFFFF),
            Err(DecodeError::UnknownFrameType(0xFFFF))
        ));
    }

    #[test]
    fn bad_magic_is_error() {
        let mut bytes = FrameHeader::new(FrameType::Hello, 0, 0, 0).encode();
        bytes[0] = 0xDE;
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(DecodeError::BadMagic(_))
        ));
    }

    #[test]
    fn tx_meta_round_trip() {
        let meta = TxMeta {
            tx_id: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10,
            ],
            kind: 0x0000_0001,
            flags: 0x0000_0002,
        };
        let bytes = meta.encode();
        assert_eq!(bytes.len(), TX_META_LEN);
        let decoded = TxMeta::decode(&bytes).expect("decode should succeed");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn tx_meta_extra_bytes() {
        let meta = TxMeta {
            tx_id: [0u8; 16],
            kind: 1,
            flags: 0,
        };
        let mut buf = meta.encode().to_vec();
        buf.extend_from_slice(b"{\"hello\":true}");
        let decoded_meta = TxMeta::decode(&buf).unwrap();
        assert_eq!(decoded_meta, meta);
        assert_eq!(TxMeta::extra(&buf), b"{\"hello\":true}");
    }

    #[test]
    fn tx_meta_short_body_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            TxMeta::decode(&short),
            Err(DecodeError::InvalidTxMetaLength { .. })
        ));
    }

    #[test]
    fn tx_id_hex() {
        let id = TxId([
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xaa, 0xbb,
        ]);
        assert_eq!(id.to_hex(), "deadbeef00112233445566778899aabb");
    }

    #[test]
    fn header_length_mismatch_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            FrameHeader::decode(&short),
            Err(DecodeError::InvalidHeaderLength { .. })
        ));
    }

    #[test]
    fn shard_header_round_trip() {
        let hdr = ShardHeader {
            tx_id: [0x01; 16],
            shard_seq: 42,
            shard_digest: [0xab; 32],
            record_count: 7,
            flags: 0,
        };
        let bytes = hdr.encode();
        assert_eq!(bytes.len(), SHARD_HEADER_LEN);
        let decoded = ShardHeader::decode(&bytes).unwrap();
        assert_eq!(decoded, hdr);
    }

    #[test]
    fn shard_header_field_offsets() {
        let hdr = ShardHeader {
            tx_id: [0xAA; 16],
            shard_seq: u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            shard_digest: [0xBB; 32],
            record_count: 0x0000_000F,
            flags: 0x0000_0001,
        };
        let bytes = hdr.encode();
        // tx_id at [0..16]
        assert!(bytes[0..16].iter().all(|&b| b == 0xAA));
        // shard_seq at [16..24]
        assert_eq!(&bytes[16..24], &[1, 2, 3, 4, 5, 6, 7, 8]);
        // shard_digest at [24..56]
        assert!(bytes[24..56].iter().all(|&b| b == 0xBB));
        // record_count at [56..60]
        assert_eq!(u32::from_le_bytes(bytes[56..60].try_into().unwrap()), 15);
        // flags at [60..64]
        assert_eq!(u32::from_le_bytes(bytes[60..64].try_into().unwrap()), 1);
    }

    #[test]
    fn shard_header_too_short_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            ShardHeader::decode(&short),
            Err(DecodeError::InvalidShardHeaderLength { .. })
        ));
    }

    #[test]
    fn shard_ack_round_trip() {
        let ack = ShardAck {
            tx_id: [0x02; 16],
            shard_seq: 99,
            ack_state: AckState::Spooled,
            bytes_committed_total: 1_048_576,
            files_committed_total: 42,
        };
        let bytes = ack.encode();
        assert_eq!(bytes.len(), SHARD_ACK_LEN);
        let decoded = ShardAck::decode(&bytes).unwrap();
        assert_eq!(decoded, ack);
    }

    #[test]
    fn shard_ack_all_states() {
        for state in [
            AckState::Spooled,
            AckState::Applied,
            AckState::DuplicateIgnored,
        ] {
            let ack = ShardAck {
                tx_id: [0u8; 16],
                shard_seq: 1,
                ack_state: state,
                bytes_committed_total: 0,
                files_committed_total: 0,
            };
            let decoded = ShardAck::decode(&ack.encode()).unwrap();
            assert_eq!(decoded.ack_state, state);
        }
    }

    #[test]
    fn shard_ack_too_short_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            ShardAck::decode(&short),
            Err(DecodeError::InvalidShardAckLength { .. })
        ));
    }

    #[test]
    fn shard_ack_unknown_state_is_error() {
        let mut bytes = ShardAck {
            tx_id: [0u8; 16],
            shard_seq: 0,
            ack_state: AckState::Spooled,
            bytes_committed_total: 0,
            files_committed_total: 0,
        }
        .encode();
        bytes[24] = 0xFF; // unknown ack_state
        assert!(matches!(
            ShardAck::decode(&bytes),
            Err(DecodeError::UnknownAckState(0xFF))
        ));
    }

    /// Cross-check that the wire constants we use in Rust match the
    /// hard-coded values in the C payload (payload/src/runtime.c and
    /// payload/include/config.h). If any of these drift, the two sides
    /// will fail to speak FTX2 and the symptoms are usually mysterious
    /// "connection reset" or "bad magic" at runtime.
    ///
    /// When editing frame numbers, MAGIC, version, or the TX_FLAG_*
    /// bits, update BOTH sides and this test.
    #[test]
    fn payload_c_wire_constants_match() {
        // Magic and version (top of FrameHeader)
        assert_eq!(FTX2_MAGIC, 0x32585446); // "FTX2" little-endian
        assert_eq!(FTX2_VERSION_1, 1);

        // TX meta flags (must match FTX2_TX_FLAG_* in runtime.c)
        assert_eq!(TX_FLAG_RESUME, 0x1);
        assert_eq!(TX_FLAG_APPLY_PROGRESS_REQUESTED, 0x4);

        // Representative frame type values used by the C dispatcher
        // and the takeover / shutdown paths (see runtime.c defines).
        assert_eq!(FrameType::Hello as u16, 1);
        assert_eq!(FrameType::HelloAck as u16, 2);
        assert_eq!(FrameType::Error as u16, 3);
        assert_eq!(FrameType::BeginTx as u16, 10);
        assert_eq!(FrameType::TakeoverRequest as u16, 18);
        assert_eq!(FrameType::Shutdown as u16, 22);
        assert_eq!(FrameType::StreamShard as u16, 30);
        assert_eq!(FrameType::ShardAck as u16, 31);
        assert_eq!(FrameType::FsListDir as u16, 36);
        assert_eq!(FrameType::HwInfo as u16, 64);
        assert_eq!(FrameType::PkgInstall as u16, 82);
        assert_eq!(FrameType::ApplyProgress as u16, 146);
    }
}
