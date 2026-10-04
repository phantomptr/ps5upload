//! The management transport seam.
//!
//! About thirty modules in this crate talk to the console's management service one
//! request at a time: connect, send one request, read one reply. They all go through
//! [`call`] now, so the wire underneath can change without touching them.
//!
//! * A [`Method`] names one management operation: its AVA1 method number and, until
//!   the FTX2 removal tasks, the legacy request and ack frame types.
//! * Request and reply bodies keep their legacy shape (JSON or `key=value` text, or raw
//!   bytes for `fs.read`). The AVA1 transport (`ps5upload-ava1::mgmt`) converts them
//!   to and from the typed bodies; callers never see the difference.
//! * `ps5upload-core` cannot depend on `ps5upload-ava1` (the dependency runs the other
//!   way), so the engine registers the transport once at start ([`set_transport`]).
//! * A registered transport is the only path: when it does not serve a console (nothing
//!   listening on the AVA1 port, or a helper too old to serve management) the call fails with
//!   [`helper_not_ava1`], never with a second protocol. The FTX2 path below runs only in a
//!   process that registered no transport at all (the lab and the benchmark harness, which
//!   keep it until the FTX2 baseline is recorded); the engine always registers one.
//!
//! Errors: a refusal by the payload is a [`MgmtError`] inside the `anyhow::Error`
//! (`payload rejected <LABEL>: <cause>`, the text callers already match on). Use
//! `err.downcast_ref::<MgmtError>()` to read the status and cause.

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::de::DeserializeOwned;

use crate::connection::Connection;

/// One management operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    /// The AVA1 method number (`METHOD_*` in `protocol/ava1/schema/ava1.toml`).
    pub id: u16,
    /// The legacy label callers put in `payload rejected <LABEL>: ...`.
    pub label: &'static str,
    /// FTX2 request and ack frames (removed with FTX2). `None` for a method FTX2 never had
    /// (`fs.stat`): a console on an FTX2-only helper cannot serve it.
    pub ftx2: Option<(FrameType, FrameType)>,
}

macro_rules! methods {
    ($(($name:ident, $id:expr, $label:expr, $req:ident, $ack:ident),)*) => {
        /// Every management method that has one AVA1 method number. Operations that
        /// run as `job.run` (hash, delete, crc32, fsck, backup snapshot/restore) have
        /// no entry; the job helpers cover them.
        pub mod m {
            use super::Method;
            use ftx2_proto::FrameType;
            $(pub const $name: Method = Method {
                id: $id,
                label: $label,
                ftx2: Some((FrameType::$req, FrameType::$ack)),
            };)*
            /// `fs.stat`: new with AVA1 (the legacy callers tested existence with a 1-byte
            /// `FsRead`). Request `{"path"}`, reply `{"kind","size","mtime","mode","dev"}`.
            pub const FS_STAT: Method = Method {
                id: 34,
                label: "FS_STAT",
                ftx2: None,
            };
            /// All of them, for drift tests.
            pub const ALL: &[Method] = &[$($name,)* FS_STAT];
        }
    };
}

methods! {
    (NODE_STATUS, 4, "STATUS", Status, StatusAck),
    (NODE_SHUTDOWN, 5, "SHUTDOWN", Shutdown, ShutdownAck),
    (NODE_CLEANUP, 6, "CLEANUP", Cleanup, CleanupAck),
    (LOG_KLOG, 7, "KLOG_READ", KlogRead, KlogReadAck),
    (LOG_SYSLOG, 8, "SYSLOG_TAIL", SyslogTail, SyslogTailAck),
    (NET_INTERFACES, 9, "NET_INTERFACES", NetInterfaces, NetInterfacesAck),
    (NET_REACH, 10, "NET_REACH", NetReach, NetReachAck),
    (NET_SPEEDTEST, 11, "NET_SPEED_TEST", NetSpeedTest, NetSpeedTestAck),
    (FS_VOLUMES, 32, "FS_LIST_VOLUMES", FsListVolumes, FsListVolumesAck),
    (FS_LIST, 33, "FS_LIST_DIR", FsListDir, FsListDirAck),
    (FS_MKDIR, 35, "FS_MKDIR", FsMkdir, FsMkdirAck),
    (FS_RENAME, 36, "FS_MOVE", FsMove, FsMoveAck),
    (FS_CHMOD, 37, "FS_CHMOD", FsChmod, FsChmodAck),
    (FS_READ, 38, "FS_READ", FsRead, FsReadAck),
    (FS_WRITE, 39, "FS_WRITE_BYTES", FsWriteBytes, FsWriteBytesAck),
    (FS_MOUNT, 40, "FS_MOUNT", FsMount, FsMountAck),
    (FS_UNMOUNT, 41, "FS_UNMOUNT", FsUnmount, FsUnmountAck),
    (FS_MOUNT_PKG, 42, "PKG_DIRECT_MOUNT", PkgDirectMount, PkgDirectMountAck),
    (FS_MOUNT_LWFS, 43, "LWFS_MOUNT", LwfsMount, LwfsMountAck),
    (APP_REGISTER, 48, "APP_REGISTER", AppRegister, AppRegisterAck),
    (APP_UNREGISTER, 49, "APP_UNREGISTER", AppUnregister, AppUnregisterAck),
    (APP_LAUNCH, 50, "APP_LAUNCH", AppLaunch, AppLaunchAck),
    (APP_LIST, 51, "APP_LIST_REGISTERED", AppListRegistered, AppListRegisteredAck),
    (APP_LAUNCH_BROWSER, 52, "APP_LAUNCH_BROWSER", AppLaunchBrowser, AppLaunchBrowserAck),
    (APP_LIFECYCLE, 53, "APP_LIFECYCLE", AppLifecycle, AppLifecycleAck),
    (APP_INFO_QUERY, 54, "APP_INFO_QUERY", AppInfoQuery, AppInfoQueryAck),
    (APP_INFO_SET, 55, "APP_INFO_SET", AppInfoSet, AppInfoSetAck),
    (APP_DB_QUERY, 56, "APP_DB_QUERY", AppDbQuery, AppDbQueryAck),
    (PROC_FOCUS, 57, "FOCUS_PROBE", FocusProbe, FocusProbeAck),
    (PROC_LIST, 58, "PROC_LIST", ProcList, ProcListAck),
    (PROC_PROCESS_LIST, 59, "PROCESS_LIST", ProcessList, ProcessListAck),
    (PROC_KILL, 60, "PROCESS_KILL", ProcessKill, ProcessKillAck),
    (PROC_MODULES, 61, "PROC_MODULES", ProcModules, ProcModulesAck),
    (SAVES_LIST, 64, "LIST_SAVES", ListSaves, ListSavesAck),
    (SHOTS_LIST, 65, "LIST_SCREENSHOTS", ListScreenshots, ListScreenshotsAck),
    (VIDEOS_LIST, 66, "LIST_VIDEOS", ListVideos, ListVideosAck),
    (INDEX_START, 67, "INDEX_START", IndexStart, IndexStartAck),
    (INDEX_STATUS, 68, "INDEX_STATUS", IndexStatus, IndexStatusAck),
    (INDEX_SEARCH, 69, "SEARCH_INDEX", SearchIndex, SearchIndexAck),
    (INDEX_CANCEL, 70, "INDEX_CANCEL", IndexCancel, IndexCancelAck),
    (HW_INFO, 72, "HW_INFO", HwInfo, HwInfoAck),
    (HW_TEMPS, 73, "HW_TEMPS", HwTemps, HwTempsAck),
    (HW_POWER, 74, "HW_POWER", HwPower, HwPowerAck),
    (HW_STORAGE, 75, "HW_STORAGE", HwStorage, HwStorageAck),
    (HW_FAN_THRESHOLD, 76, "HW_SET_FAN_THRESHOLD", HwSetFanThreshold, HwSetFanThresholdAck),
    (HW_FAN_CURVE_SET, 77, "HW_FAN_CURVE_SET", HwFanCurveSet, HwFanCurveSetAck),
    (HW_FAN_CURVE_GET, 78, "HW_FAN_CURVE_GET", HwFanCurveGet, HwFanCurveGetAck),
    (HW_DRIVE_SENSORS, 79, "HW_DRIVE_SENSORS", HwDriveSensors, HwDriveSensorsAck),
    (POWER_CONTROL, 80, "SYSTEM_CONTROL", SystemControl, SystemControlAck),
    (POWER_TELEMETRY, 81, "POWER_TELEMETRY", PowerTelemetry, PowerTelemetryAck),
    (TIME_GET, 82, "TIME_GET", TimeGet, TimeGetAck),
    (TIME_SET, 83, "TIME_SET", TimeSet, TimeSetAck),
    (TIME_STATE_GET, 84, "TIME_STATE_GET", TimeStateGet, TimeStateGetAck),
    (TIME_STATE_SET, 85, "TIME_STATE_SET", TimeStateSet, TimeStateSetAck),
    (PERIPH_CONTROL, 86, "PERIPHERAL_CONTROL", PeripheralControl, PeripheralControlAck),
    (SHELL_EXEC, 87, "SHELL_EXEC", ShellExec, ShellExecAck),
    (PROFILE_INFO, 88, "PROFILE_INFO", ProfileInfo, ProfileInfoAck),
    (PROFILE_SET_USERNAME, 89, "PROFILE_SET_USERNAME", ProfileSetUsername, ProfileSetUsernameAck),
    (PROFILE_ACTIVATE, 90, "PROFILE_ACTIVATE", ProfileActivate, ProfileActivateAck),
    (PROFILE_APPLY_AVATAR, 91, "PROFILE_APPLY_AVATAR", ProfileApplyAvatar, ProfileApplyAvatarAck),
    (PROFILE_CLEAR_SLOT, 92, "PROFILE_CLEAR_SLOT", ProfileClearSlot, ProfileClearSlotAck),
    (PROFILE_SET_LOCAL_USERNAME, 93, "PROFILE_SET_LOCAL_USERNAME", ProfileSetLocalUsername, ProfileSetLocalUsernameAck),
    (USER_LIST, 94, "USER_LIST", UserList, UserListAck),
    (USER_CREATE, 95, "USER_CREATE", UserCreate, UserCreateAck),
    (USER_DELETE, 96, "USER_DELETE", UserDelete, UserDeleteAck),
    (BACKUP_LIST, 98, "BACKUP_LIST", BackupList, BackupListAck),
    (BACKUP_DELETE, 100, "BACKUP_DELETE", BackupDelete, BackupDeleteAck),
    (CHEATS_LIST, 104, "CHEATS_LIST", CheatsList, CheatsListAck),
    (CHEATS_GET, 105, "CHEATS_GET", CheatsGet, CheatsGetAck),
    (CHEATS_TOGGLE, 106, "CHEATS_TOGGLE", CheatsToggle, CheatsToggleAck),
    (CHEATS_DELETE, 107, "CHEATS_DELETE", CheatsDelete, CheatsDeleteAck),
    (CHEATS_RELOAD, 108, "CHEATS_RELOAD", CheatsReload, CheatsReloadAck),
    (CHEATS_STATUS, 109, "CHEATS_STATUS", CheatsStatus, CheatsStatusAck),
    (CHEATS_ENGINE_SET, 110, "CHEATS_ENGINE_SET", CheatsEngineSet, CheatsEngineSetAck),
    (SMP_META_CONTROL, 112, "SMP_META_CONTROL", SmpMetaControl, SmpMetaControlAck),
    (SMP_META_STATS, 113, "SMP_META_STATS", SmpMetaStats, SmpMetaStatsAck),
    (SDK_SCAN, 114, "SDK_SCAN", SdkScan, SdkScanAck),
    (SDK_PATCH, 115, "SDK_PATCH", SdkPatch, SdkPatchAck),
    (SDK_RESTORE, 116, "SDK_RESTORE", SdkRestore, SdkRestoreAck),
    (TMDB_FETCH, 117, "TMDB_FETCH", TmdbFetch, TmdbFetchAck),
    (TMDB_STORE, 118, "TMDB_STORE", TmdbStore, TmdbStoreAck),
    (FTP_START, 119, "FTP_START", FtpStart, FtpStartAck),
    (FTP_STATUS, 120, "FTP_STATUS", FtpStatus, FtpStatusAck),
    (FWSPOOF_STATUS, 121, "FW_SPOOF_STATUS", FwSpoofStatus, FwSpoofStatusAck),
    (NOTIF_LIST, 122, "NOTIF_LIST", NotifList, NotifListAck),
    (NOTIF_SEND, 123, "NOTIF_SEND", NotifSend, NotifSendAck),
    (NOTIF_CLEAR, 124, "NOTIF_CLEAR", NotifClear, NotifClearAck),
    (TOAST_SEND, 125, "TOAST_SEND", ToastSend, ToastSendAck),
    (ACTIVITY_GET, 126, "ACTIVITY_GET", ActivityGet, ActivityGetAck),
    (ACTIVITY_DB_QUERY, 127, "ACTIVITY_DB_QUERY", ActivityDbQuery, ActivityDbQueryAck),
    (ACTIVITY_RESET, 128, "ACTIVITY_RESET", ActivityReset, ActivityResetAck),
    (RP_REQUEST, 136, "REMOTE_PLAY_REQUEST", RemotePlayRequest, RemotePlayStatus),
    (RP_STATUS, 137, "REMOTE_PLAY_STATUS", RemotePlayStatus, RemotePlayStatus),
    (RP_CANCEL, 138, "REMOTE_PLAY_CANCEL", RemotePlayCancel, RemotePlayCancelAck),
    (RP_READINESS, 139, "REMOTE_PLAY_READINESS", RemotePlayReadiness, RemotePlayReadiness),
    (RP_ENABLE, 140, "REMOTE_PLAY_ENABLE", RemotePlayEnable, RemotePlayEnable),
    (RP_DEVICES, 141, "REMOTE_PLAY_DEVICES", RemotePlayDevices, RemotePlayDevices),
}

/// A management call the payload refused.
///
/// `Display` is `payload rejected <label>: <cause>`, byte for byte what the FTX2 path
/// produced for an `Error` frame. `status` is the AVA1 `ERR_*` code, 0 on the FTX2 path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MgmtError {
    pub label: String,
    pub status: u16,
    pub cause: String,
}

impl fmt::Display for MgmtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "payload rejected {}: {}", self.label, self.cause)
    }
}

impl std::error::Error for MgmtError {}

/// `error_reason` / error token for a console with no AVA1 listener or an older helper.
pub const HELPER_NOT_AVA1: &str = "helper_not_ava1";
/// The text that goes with [`HELPER_NOT_AVA1`].
pub const HELPER_NOT_AVA1_MESSAGE: &str = "The PS5 helper is not running or is an old version. Desktop app: send it from the Connection screen, or click Update helper. Web UI: start the ps5upload payload on the console with your payload loader, or click Update helper.";
/// `error_reason` / error token for a console that has not accepted this app.
pub const NOT_PAIRED: &str = "not_paired";
/// The text that goes with [`NOT_PAIRED`].
pub const NOT_PAIRED_MESSAGE: &str =
    "This PS5 has not accepted this app yet. Pair it from the Connection screen.";

/// The refusal for a console that has no AVA1 listener (or a helper too old to serve this):
/// `payload rejected <label>: helper_not_ava1: <message>`. The client keys on the token.
pub fn helper_not_ava1(label: &str) -> anyhow::Error {
    MgmtError {
        label: label.to_string(),
        status: 0,
        cause: format!("{HELPER_NOT_AVA1}: {HELPER_NOT_AVA1_MESSAGE}"),
    }
    .into()
}

/// The text of the error a method FTX2 never had (`fs.stat`) gets on a console whose helper
/// does not serve AVA1 management.
const NEEDS_AVA1: &str =
    "needs a helper that speaks AVA1 management; this console runs an older one";

/// True when `e` says the console's helper cannot serve this method at all (an FTX2-only helper
/// asked for a method FTX2 never had), as opposed to the payload refusing the call.
pub fn is_unsupported(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MgmtError>().is_none() && format!("{e:#}").contains(NEEDS_AVA1)
}

/// The default per-call deadline (the FTX2 socket default).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How a registered transport serves management calls.
pub trait MgmtTransport: Send + Sync {
    /// Runs one call. `Ok(None)` means this transport does not serve `addr` (for example the
    /// console runs an older helper): the caller fails with [`helper_not_ava1`]. A refusal by the
    /// payload is an `Err` holding a [`MgmtError`].
    fn call(
        &self,
        addr: &str,
        method: Method,
        label: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>>;

    /// Runs one long operation as a `job.run` job and waits for it (polling its status) up
    /// to `call.deadline`. Same `Ok(None)` and error rules as [`call`](Self::call).
    fn run_job(
        &self,
        _addr: &str,
        _op: JobOp,
        _label: &str,
        _body: &[u8],
        _call: &JobCall<'_>,
    ) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Progress of the operation started under `op_id` by [`run_job`](Self::run_job):
    /// `Ok(None)` = this transport does not serve `addr`; `Ok(Some(None))` = served, but no
    /// such operation is running (finished, or never started).
    fn job_progress(&self, _addr: &str, _op_id: u64) -> Result<Option<Option<JobProgress>>> {
        Ok(None)
    }

    /// Asks the operation under `op_id` to stop: `Ok(Some(found))`, or `Ok(None)` when this
    /// transport does not serve `addr`.
    fn job_cancel(&self, _addr: &str, _op_id: u64) -> Result<Option<bool>> {
        Ok(None)
    }
}

/// A long management operation: one `job.run` op over AVA1, one request frame over FTX2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobOp {
    /// The AVA1 `JOB_OP_*` number.
    pub id: u8,
    /// The legacy label callers put in `payload rejected <LABEL>: ...`.
    pub label: &'static str,
    /// What `fs_op_status` reports as the operation's kind (`fs_delete`, ...).
    pub kind: &'static str,
    /// FTX2 request and ack frames (removed with FTX2).
    pub ftx2: (FrameType, FrameType),
}

macro_rules! job_ops {
    ($(($name:ident, $id:expr, $label:expr, $kind:expr, $req:ident, $ack:ident),)*) => {
        /// The operations that run as jobs (`JOB_OP_*` in `protocol/ava1/schema/ava1.toml`).
        pub mod ops {
            use super::JobOp;
            use ftx2_proto::FrameType;
            $(pub const $name: JobOp = JobOp {
                id: $id,
                label: $label,
                kind: $kind,
                ftx2: (FrameType::$req, FrameType::$ack),
            };)*
            /// All of them, for drift tests.
            pub const ALL: &[JobOp] = &[$($name,)*];
        }
    };
}

job_ops! {
    (DELETE, 1, "FS_DELETE", "fs_delete", FsDelete, FsDeleteAck),
    (CHMOD_R, 2, "FS_CHMOD", "fs_chmod", FsChmod, FsChmodAck),
    (HASH, 3, "FS_HASH", "fs_hash", FsHash, FsHashAck),
    (CRC32, 4, "CRC32_FILE", "crc32_file", Crc32File, Crc32FileAck),
    (FSCK, 5, "UFS_FSCK", "ufs_fsck", UfsFsck, UfsFsckAck),
    (BACKUP_SNAPSHOT, 6, "BACKUP_SNAPSHOT", "backup_snapshot", BackupSnapshot, BackupSnapshotAck),
    (BACKUP_RESTORE, 7, "BACKUP_RESTORE", "backup_restore", BackupRestore, BackupRestoreAck),
    (CLEANUP, 8, "CLEANUP", "cleanup", Cleanup, CleanupAck),
    (SDK_SCAN, 9, "SDK_SCAN", "sdk_scan", SdkScan, SdkScanAck),
}

/// How one long operation is run and followed.
pub struct JobCall<'a> {
    /// The caller's operation id (the engine's `op_id`): the key `/api/ps5/fs/op-status` and
    /// `op-cancel` use, and the low 8 bytes of the AVA1 job id. 0 = nobody will ask.
    pub op_id: u64,
    /// What the operation works on, shown by `fs_op_status` (a path).
    pub subject: &'a str,
    /// The whole wait: the caller's `io_timeout` (the old socket deadline) is now this.
    pub deadline: Duration,
}

/// A running operation's progress, from the console.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub kind: String,
    pub subject: String,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub cancel_requested: bool,
}

static TRANSPORT: RwLock<Option<Arc<dyn MgmtTransport>>> = RwLock::new(None);

thread_local! {
    static SCOPED: RefCell<Option<Arc<dyn MgmtTransport>>> = const { RefCell::new(None) };
}

/// Registers the process's transport (once, at engine start). A second call replaces it.
pub fn set_transport(t: Arc<dyn MgmtTransport>) {
    *TRANSPORT.write().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

/// Test seam: routes this thread's calls to `t` until the guard drops. Other threads and
/// the registered transport are unaffected, so tests need no global lock.
pub fn scoped_transport(t: Arc<dyn MgmtTransport>) -> ScopedTransport {
    let prev = SCOPED.with(|s| s.borrow_mut().replace(t));
    ScopedTransport { prev }
}

pub struct ScopedTransport {
    prev: Option<Arc<dyn MgmtTransport>>,
}

impl Drop for ScopedTransport {
    fn drop(&mut self) {
        let prev = self.prev.take();
        SCOPED.with(|s| *s.borrow_mut() = prev);
    }
}

fn current() -> Option<Arc<dyn MgmtTransport>> {
    SCOPED
        .with(|s| s.borrow().clone())
        .or_else(|| TRANSPORT.read().unwrap_or_else(|e| e.into_inner()).clone())
}

/// One management call with the method's own label and the default deadline.
pub fn call(addr: &str, method: Method, body: &[u8]) -> Result<Vec<u8>> {
    call_with(addr, method, method.label, body, None)
}

/// Like [`call`] with a caller-chosen label (`FS_LIST_DIR(/data)`), the text that follows
/// `payload rejected ` in an error.
pub fn call_as(addr: &str, method: Method, label: &str, body: &[u8]) -> Result<Vec<u8>> {
    call_with(addr, method, label, body, None)
}

/// The general form: `timeout` `None` is [`DEFAULT_TIMEOUT`].
pub fn call_with(
    addr: &str,
    method: Method,
    label: &str,
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<Vec<u8>> {
    if let Some(t) = current() {
        if let Some(reply) = t.call(
            addr,
            method,
            label,
            body,
            timeout.unwrap_or(DEFAULT_TIMEOUT),
        )? {
            return Ok(reply);
        }
        return Err(helper_not_ava1(label));
    }
    // No transport registered (the lab and the benchmark harness).
    ftx2_call(addr, method, label, body, timeout)
}

/// A call whose request is a JSON value and whose reply is decoded as `T`.
pub fn call_json<T: DeserializeOwned>(
    addr: &str,
    method: Method,
    label: &str,
    body: &serde_json::Value,
) -> Result<T> {
    let body = serde_json::to_vec(body).with_context(|| format!("serialize {label} body"))?;
    let resp = call_as(addr, method, label, &body)?;
    serde_json::from_slice(&resp).with_context(|| format!("decode {label} reply as JSON"))
}

/// For handlers that used to answer a failure as a successful frame with
/// `{"ok":false,"err":"<token>"}` (SPEC section 7.3, "Legacy failure bodies"). The AVA1 payload
/// answers an error status with the token as the cause; this turns that refusal back into
/// the body the caller already parses. A handler whose failure body carries data the caller
/// reads (`net.reach`: `errno`, `timed_out`, `ms`; the mounts: `code`, `mount_point`) sends the
/// whole body as the cause: a cause that is a JSON object is returned as it is. Transport failures stay errors, and so does an FTX2
/// `Error` frame (status 0): those were errors before too.
pub fn call_legacy_ok(addr: &str, method: Method, label: &str, body: &[u8]) -> Result<Vec<u8>> {
    legacy_ok(call_as(addr, method, label, body))
}

/// [`call_legacy_ok`]'s conversion, separately testable.
pub fn legacy_ok(r: Result<Vec<u8>>) -> Result<Vec<u8>> {
    match r {
        Err(e) => match e.downcast_ref::<MgmtError>() {
            // The payload keeps the whole `{"ok":false,...}` body as the cause when it fits one: that is the
            // legacy body, byte for byte. A bare token (an ERROR frame, or a body too long for a cause) is
            // wrapped as the old `{"ok":false,"err":token}`.
            Some(m) if m.status != 0 => {
                if serde_json::from_str::<serde_json::Value>(&m.cause).is_ok_and(|v| v.is_object())
                {
                    Ok(m.cause.clone().into_bytes())
                } else {
                    Ok(serde_json::to_vec(
                        &serde_json::json!({ "ok": false, "err": m.cause }),
                    )?)
                }
            }
            _ => Err(e),
        },
        ok => ok,
    }
}

/// For the handlers whose `{"ok":false,...}` body carries data the caller reads (the Sony return
/// code of `app.lifecycle`, the errno and reason of `proc.kill`, the `error` text of `app.info_*`).
/// The AVA1 payload answers such a failure with an error status whose cause is the whole body
/// (`mgmt_call_text_keep`); this returns that body, so the caller parses the same bytes an FTX2
/// payload sent. A cause that is not a JSON object (a bare token, or a body too long for a cause
/// and cut to its token) and every transport failure stay errors.
pub fn call_legacy_body(addr: &str, method: Method, label: &str, body: &[u8]) -> Result<Vec<u8>> {
    legacy_body(call_as(addr, method, label, body))
}

/// [`call_legacy_body`] with a caller-chosen deadline.
pub fn call_legacy_body_with(
    addr: &str,
    method: Method,
    label: &str,
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<Vec<u8>> {
    legacy_body(call_with(addr, method, label, body, timeout))
}

/// [`call_legacy_body`]'s conversion, separately testable.
pub fn legacy_body(r: Result<Vec<u8>>) -> Result<Vec<u8>> {
    match r {
        Err(e) => match e.downcast_ref::<MgmtError>() {
            Some(m)
                if m.status != 0
                    && serde_json::from_str::<serde_json::Value>(&m.cause)
                        .is_ok_and(|v| v.is_object()) =>
            {
                Ok(m.cause.clone().into_bytes())
            }
            _ => Err(e),
        },
        ok => ok,
    }
}

/// For a method whose handler answers a failure as a successful frame with a JSON body that carries
/// data the caller reads (`{"ok":false,"err_code":N}`, `{"ok":false,"port":P,...}`): the body back, as
/// FTX2 gave it. The same conversion as [`call_legacy_body`]; a plain-token refusal (an FTX2 `Error`
/// frame such as `rp_enable_no_user`) stays an `Err` with the `payload rejected <LABEL>: <cause>` text.
pub fn call_keep(addr: &str, method: Method, label: &str, body: &[u8]) -> Result<Vec<u8>> {
    legacy_body(call_as(addr, method, label, body))
}

/// [`call_keep`]'s conversion ([`legacy_body`]), under the name the Task 7 modules use.
pub fn keep_body(r: Result<Vec<u8>>) -> Result<Vec<u8>> {
    legacy_body(r)
}

/// Runs one long operation and returns its result body (the legacy handler's reply: the same
/// JSON the FTX2 frame answered). Over AVA1 it is a job the console runs while this call polls
/// it, so a long delete or checksum no longer holds a socket for an hour; over FTX2 it is the
/// one frame it always was, with `call.op_id` as the trace id.
///
/// A refusal or a failed job is a [`MgmtError`] (`payload rejected <label>: <cause>`); a
/// cancelled one has the status `ERR_CANCELLED` and the handler's own cancel token as cause.
pub fn run_op(
    addr: &str,
    op: JobOp,
    label: &str,
    body: &[u8],
    call: &JobCall<'_>,
) -> Result<Vec<u8>> {
    if let Some(t) = current() {
        if let Some(reply) = t.run_job(addr, op, label, body, call)? {
            return Ok(reply);
        }
        return Err(helper_not_ava1(label));
    }
    // No transport registered (the lab and the benchmark harness).
    ftx2_run_op(addr, op, label, body, call)
}

/// Progress of an operation started by [`run_op`], for `/api/ps5/fs/op-status`.
/// `Ok(None)`: no transport registered (the lab asks FTX2). `Ok(Some(None))`: nothing running
/// under `op_id`. A transport that does not serve the console is [`helper_not_ava1`].
pub fn op_progress(addr: &str, op_id: u64) -> Result<Option<Option<JobProgress>>> {
    match current() {
        Some(t) => t
            .job_progress(addr, op_id)?
            .map(Some)
            .ok_or_else(|| helper_not_ava1("JOB_STATUS")),
        None => Ok(None),
    }
}

/// Asks an operation started by [`run_op`] to stop. `Ok(None)`: no transport registered.
pub fn op_cancel(addr: &str, op_id: u64) -> Result<Option<bool>> {
    match current() {
        Some(t) => t
            .job_cancel(addr, op_id)?
            .map(Some)
            .ok_or_else(|| helper_not_ava1("JOB_CANCEL")),
        None => Ok(None),
    }
}

/// The FTX2 form of [`run_op`]: connect, one request frame carrying `op_id` as its trace id,
/// one reply frame, read under the whole deadline.
fn ftx2_run_op(
    addr: &str,
    op: JobOp,
    label: &str,
    body: &[u8],
    call: &JobCall<'_>,
) -> Result<Vec<u8>> {
    let (req, ack) = op.ftx2;
    let mut c = Connection::connect(addr)?;
    c.set_io_timeout(call.deadline)
        .with_context(|| format!("applying {label} I/O timeout"))?;
    c.send_frame_with_trace(req, body, call.op_id)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        return Err(MgmtError {
            label: label.to_string(),
            status: 0,
            cause: String::from_utf8_lossy(&resp).into_owned(),
        }
        .into());
    }
    if ft != ack {
        bail!("expected {ack:?}, got {ft:?}");
    }
    Ok(resp)
}

/// Today's FTX2 path, unchanged: connect, one request frame, one reply frame.
fn ftx2_call(
    addr: &str,
    method: Method,
    label: &str,
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<Vec<u8>> {
    let Some((req, ack)) = method.ftx2 else {
        bail!("{label} {NEEDS_AVA1}");
    };
    let mut c = Connection::connect(addr)?;
    if let Some(t) = timeout {
        c.set_io_timeout(t)
            .with_context(|| format!("applying {label} I/O timeout"))?;
    }
    c.send_frame(req, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        return Err(MgmtError {
            label: label.to_string(),
            status: 0,
            cause: String::from_utf8_lossy(&resp).into_owned(),
        }
        .into());
    }
    if ft != ack {
        bail!("expected {ack:?}, got {ft:?}");
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    type Seen = Vec<(String, u16, String, Vec<u8>, Duration)>;
    type Script = Box<dyn Fn(Method, &[u8]) -> Result<Option<Vec<u8>>> + Send + Sync>;

    /// Records calls and answers from a script.
    struct Fake {
        seen: Mutex<Seen>,
        reply: Script,
    }

    impl MgmtTransport for Fake {
        fn call(
            &self,
            addr: &str,
            method: Method,
            label: &str,
            body: &[u8],
            timeout: Duration,
        ) -> Result<Option<Vec<u8>>> {
            self.seen.lock().unwrap().push((
                addr.into(),
                method.id,
                label.into(),
                body.to_vec(),
                timeout,
            ));
            (self.reply)(method, body)
        }
    }

    fn fake(
        f: impl Fn(Method, &[u8]) -> Result<Option<Vec<u8>>> + Send + Sync + 'static,
    ) -> Arc<Fake> {
        Arc::new(Fake {
            seen: Mutex::default(),
            reply: Box::new(f),
        })
    }

    #[test]
    fn method_ids_and_frames_are_unique() {
        let mut ids = HashSet::new();
        let mut reqs = HashSet::new();
        for x in m::ALL {
            assert!(ids.insert(x.id), "duplicate id {} ({})", x.id, x.label);
            if let Some((req, _)) = x.ftx2 {
                assert!(
                    reqs.insert(req as u16),
                    "duplicate frame {req:?} ({})",
                    x.label
                );
            }
        }
        assert!(m::ALL.len() >= 95);
    }

    #[test]
    fn a_registered_transport_receives_the_call_and_its_reply_is_returned() {
        let t = fake(|_, body| Ok(Some(body.iter().rev().copied().collect())));
        let _g = scoped_transport(t.clone());
        let r = call("10.0.0.1:9114", m::HW_INFO, b"abc").unwrap();
        assert_eq!(r, b"cba");
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen[0].0, "10.0.0.1:9114");
        assert_eq!(seen[0].1, 72);
        assert_eq!(seen[0].2, "HW_INFO");
        assert_eq!(seen[0].4, DEFAULT_TIMEOUT);
    }

    /// A registered transport that does not serve the console (nothing on the AVA1 port, an
    /// older helper) is a `helper_not_ava1` error, never the retired protocol's frame: the
    /// address below would only ever be dialled by that frame (a connect error would show).
    #[test]
    fn a_transport_that_does_not_serve_the_console_is_helper_not_ava1() {
        let _g = scoped_transport(fake(|_, _| Ok(None)));
        let e = call("127.0.0.1:1", m::HW_INFO, b"").unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected HW_INFO: helper_not_ava1: The PS5 helper is not running or is an old version. Desktop app: send it from the Connection screen, or click Update helper. Web UI: start the ps5upload payload on the console with your payload loader, or click Update helper."
        );
        assert!(e.downcast_ref::<MgmtError>().is_some());

        let call_ctx = JobCall {
            op_id: 1,
            subject: "/x",
            deadline: Duration::from_secs(1),
        };
        let e = run_op("127.0.0.1:1", ops::DELETE, "FS_DELETE", b"{}", &call_ctx).unwrap_err();
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
        let e = op_progress("127.0.0.1:1", 1).unwrap_err();
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
        let e = op_cancel("127.0.0.1:1", 1).unwrap_err();
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
    }

    #[test]
    fn call_as_and_call_with_carry_the_label_and_the_deadline() {
        let t = fake(|_, _| Ok(Some(vec![])));
        let _g = scoped_transport(t.clone());
        call_as("a:1", m::FS_LIST, "FS_LIST_DIR(/data)", b"").unwrap();
        call_with("a:1", m::FS_LIST, "X", b"", Some(Duration::from_secs(3))).unwrap();
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen[0].2, "FS_LIST_DIR(/data)");
        assert_eq!(seen[1].4, Duration::from_secs(3));
    }

    #[test]
    fn a_refusal_keeps_its_legacy_text_and_status() {
        let t = fake(|_, _| {
            Err(MgmtError {
                label: "FS_MOVE".into(),
                status: 16,
                cause: "fs_move_cross_mount".into(),
            }
            .into())
        });
        let _g = scoped_transport(t);
        let e = call("a:1", m::FS_RENAME, b"{}").unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected FS_MOVE: fs_move_cross_mount"
        );
        assert_eq!(e.downcast_ref::<MgmtError>().unwrap().status, 16);
    }

    #[test]
    fn call_json_decodes_the_reply() {
        let t = fake(|_, _| Ok(Some(br#"{"n":7}"#.to_vec())));
        let _g = scoped_transport(t);
        #[derive(serde::Deserialize)]
        struct R {
            n: u32,
        }
        let r: R = call_json("a:1", m::HW_INFO, "HW_INFO", &serde_json::json!({})).unwrap();
        assert_eq!(r.n, 7);
    }

    #[test]
    fn legacy_ok_turns_a_refusal_into_the_old_failure_body_but_not_a_transport_error() {
        let refused: Result<Vec<u8>> = Err(MgmtError {
            label: "FS_WRITE_BYTES".into(),
            status: 14,
            cause: "exists".into(),
        }
        .into());
        let body = legacy_ok(refused).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["err"], "exists");
        // a failure body that carries data comes back as it was sent
        let kept: Result<Vec<u8>> = Err(MgmtError {
            label: "NET_REACH".into(),
            status: 7,
            cause: r#"{"ok":false,"timed_out":true,"errno":0,"ms":3000}"#.into(),
        }
        .into());
        let v: serde_json::Value = serde_json::from_slice(&legacy_ok(kept).unwrap()).unwrap();
        assert_eq!(
            (v["timed_out"].as_bool(), v["ms"].as_u64()),
            (Some(true), Some(3000))
        );
        assert!(legacy_ok(Err(anyhow::anyhow!("network: reset"))).is_err());
        assert_eq!(legacy_ok(Ok(b"x".to_vec())).unwrap(), b"x");
    }

    #[test]
    fn legacy_body_returns_a_failure_body_kept_whole_but_not_a_bare_token_or_a_transport_error() {
        let refused = |cause: &str, status: u16| -> Result<Vec<u8>> {
            Err(MgmtError {
                label: "PROCESS_KILL".into(),
                status,
                cause: cause.into(),
            }
            .into())
        };
        let body =
            br#"{"ok":false,"pid":9,"err":"kill_failed","errno":3,"reason":"No such process"}"#;
        let kept = legacy_body(refused(std::str::from_utf8(body).unwrap(), 7)).unwrap();
        assert_eq!(kept, body, "the legacy bytes, not a rebuilt body");
        // a bare token (an ERROR frame's cause, or a too-long body cut to its token) stays an error
        let e = legacy_body(refused("kill_failed", 7)).unwrap_err();
        assert_eq!(e.to_string(), "payload rejected PROCESS_KILL: kill_failed");
        // an FTX2 Error frame (status 0) stays an error even when its text happens to be JSON
        assert!(legacy_body(refused("{\"a\":1}", 0)).is_err());
        assert!(legacy_body(Err(anyhow::anyhow!("network: reset"))).is_err());
        assert_eq!(legacy_body(Ok(b"x".to_vec())).unwrap(), b"x");
    }

    #[test]
    fn keep_body_returns_a_json_cause_as_the_body_and_keeps_token_refusals_as_errors() {
        let json = r#"{"ok":false,"err_code":3758104577}"#;
        let kept: Result<Vec<u8>> = Err(MgmtError {
            label: "TIME_SET".into(),
            status: 255,
            cause: json.into(),
        }
        .into());
        assert_eq!(keep_body(kept).unwrap(), json.as_bytes());
        let token: Result<Vec<u8>> = Err(MgmtError {
            label: "REMOTE_PLAY_ENABLE".into(),
            status: 5,
            cause: "rp_enable_no_user".into(),
        }
        .into());
        let e = keep_body(token).unwrap_err();
        assert_eq!(
            e.to_string(),
            "payload rejected REMOTE_PLAY_ENABLE: rp_enable_no_user"
        );
        // an FTX2 Error frame (status 0) whose text happens to be JSON is still an error
        let ftx2: Result<Vec<u8>> = Err(MgmtError {
            label: "X".into(),
            status: 0,
            cause: json.into(),
        }
        .into());
        assert!(keep_body(ftx2).is_err());
        assert_eq!(keep_body(Ok(b"x".to_vec())).unwrap(), b"x");
    }

    #[test]
    fn the_scoped_transport_is_per_thread_and_restores_the_previous_one() {
        let a = fake(|_, _| Ok(Some(b"a".to_vec())));
        let b = fake(|_, _| Ok(Some(b"b".to_vec())));
        let _ga = scoped_transport(a);
        {
            let _gb = scoped_transport(b);
            assert_eq!(call("x:1", m::HW_INFO, b"").unwrap(), b"b");
        }
        assert_eq!(call("x:1", m::HW_INFO, b"").unwrap(), b"a");
        std::thread::spawn(|| {
            assert!(SCOPED.with(|s| s.borrow().is_none()));
        })
        .join()
        .unwrap();
    }
}
