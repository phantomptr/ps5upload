//! The upload adapters — the entry points the engine and the lab call. Blocking:
//! call them from `spawn_blocking` or a non-async thread (C15), exactly like the
//! calling them from inside an async task panics.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gen;
use anyhow::{anyhow, Result};
use ava1::manifest::{self, Entry, Manifest};
use ava1::send::{send_job, Progress, SendError, SendOptions};
use ava1::source::{LocalSource, Source};
use ava1::Ava1Error;
use ps5upload_core::transfer::{FileListEntry, TransferConfig, TransferResult};

use crate::pool::{pool, Pool};
use crate::progress::{bottleneck_name, Bridge};
use crate::source::{FsSource, ListSource};
use crate::zip_source::ZipSource;

/// The archive cannot be an AVA1 source (a path the manifest refuses, an unsupported
/// method, encryption, a damaged directory).
#[derive(Debug, thiserror::Error)]
#[error("zip is not usable as an AVA1 source: {0}")]
pub struct ZipUnsupported(pub String);

/// A failed `ZipSource::open`: a format problem (damaged directory, unsupported method,
/// encryption, an unsafe path) is `ZipUnsupported`, which the client treats as terminal; an I/O
/// error reading the file (a share that dropped, a permission) is `zip_read_error`, which stays
/// retryable like any other transient failure.
fn zip_open_error(e: io::Error) -> anyhow::Error {
    if e.kind() == io::ErrorKind::InvalidData {
        ZipUnsupported(e.to_string()).into()
    } else {
        UploadFailure {
            reason: "zip_read_error".into(),
            detail: format!("could not read the zip archive: {e}"),
        }
        .into()
    }
}

pub fn upload_zip_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    zip_path: &Path,
) -> Result<TransferResult> {
    let (manifest, source) = ZipSource::open(zip_path, &cfg.excludes).map_err(zip_open_error)?;
    upload_with_in(
        pool,
        &cfg.addr,
        job_id,
        manifest,
        Arc::new(source),
        SendOptions::upload(dest_root),
        cfg,
    )
    .map_err(|e| {
        // An entry whose bytes fail their CRC-32 is damaged input: terminal, typed.
        if e.chain().any(crate::zip_source::is_zip_corrupt) {
            UploadFailure {
                reason: "ava1_zip_corrupt".into(),
                detail: format!("the zip archive is corrupt: {e:#}"),
            }
            .into()
        } else {
            e
        }
    })
}

/// The archive cannot be an AVA1 source for a reason of its own (a header
/// feature this source does not handle): the engine fails the job with `7z_unsupported`.
#[derive(Debug, thiserror::Error)]
#[error("7z is not usable as an AVA1 source: {0}")]
pub struct SevenzUnsupported(pub String);

/// A job id that names this archive's contents: a changed archive (same listing and
/// sizes, different bytes) must not resume a journal written for the old one.
fn sevenz_job_id(job_id: [u8; 16], identity: &[u8; 32]) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(&job_id);
    h.update(identity);
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

fn sevenz_failure(e: &anyhow::Error) -> Option<UploadFailure> {
    use crate::seq::{fault_of, SevenzFault};
    let f = e.chain().find_map(|c| fault_of(c))?;
    let reason = match f {
        SevenzFault::Corrupt(_) => "ava1_7z_corrupt",
        SevenzFault::Encrypted(_) => "ava1_7z_encrypted",
        SevenzFault::UnsafePath(_) => "ava1_7z_unsafe_path",
        SevenzFault::Unsupported(_) | SevenzFault::Conflict(_) => "ava1_7z_unsupported",
        SevenzFault::UnsupportedLayout => "ava1_7z_unsupported_layout",
    };
    Some(UploadFailure {
        reason: reason.into(),
        detail: f.to_string(),
    })
}

/// Uploads a `.7z`. Resume restarts at the solid folder holding the earliest
/// unfinished file and discards what the console already has (no decoder checkpoints).
pub fn upload_7z_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    archive: &Path,
) -> Result<TransferResult> {
    let (manifest, source) = match crate::seq::SevenzSource::open(archive, &cfg.excludes) {
        Ok(v) => v,
        Err(e) => {
            let e = anyhow::Error::from(e);
            return Err(
                match e.chain().find_map(|c| crate::seq::fault_of(c)).cloned() {
                    Some(crate::seq::SevenzFault::Unsupported(why)) => {
                        SevenzUnsupported(why).into()
                    }
                    Some(_) => sevenz_failure(&e).expect("a fault").into(),
                    None => e.context(format!("open 7z {}", archive.display())),
                },
            );
        }
    };
    upload_7z_source_in(pool, cfg, job_id, dest_root, manifest, Arc::new(source))
}

/// `upload_7z_in` for an already opened archive (the caller keeps the `Arc` to read
/// the source's counters).
pub fn upload_7z_source_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    manifest: Manifest,
    source: Arc<crate::seq::SevenzSource>,
) -> Result<TransferResult> {
    // The wire job id names the archive's contents (see `sevenz_job_id`); the result
    // reports the caller's id so job bookkeeping keyed by it stays consistent.
    let wire_id = sevenz_job_id(job_id, &source.identity());
    let mut r = upload_with_seq_in(
        pool,
        &cfg.addr,
        wire_id,
        manifest,
        Arc::new(crate::seq::NoSource),
        Some(source),
        SendOptions::upload(dest_root),
        cfg,
    )
    .map_err(|e| match sevenz_failure(&e) {
        Some(f) => f.into(),
        None => e,
    })?;
    r.tx_id_hex = hex(&job_id);
    Ok(r)
}

pub fn upload_7z(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    archive: &Path,
) -> Result<TransferResult> {
    upload_7z_in(pool(), cfg, job_id, dest_root, archive)
}

pub fn upload_zip(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    zip_path: &Path,
) -> Result<TransferResult> {
    upload_zip_in(pool(), cfg, job_id, dest_root, zip_path)
}

/// Retained for the engine's error mapping; AVA1 no longer raises it. Duplicate paths,
/// case clashes, file/directory clashes and unsafe paths in a RAR are terminal
/// (`ava1_rar_unsupported`).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, thiserror::Error)]
#[error("rar is not usable as an AVA1 source: {0}")]
pub struct RarUnsupported(pub String);

/// A RAR upload over AVA1: the archive is decoded forward on one thread
/// ([`crate::rar_source::RarSource`]). `password` is held only in memory for this job;
/// it is never logged. Failures that retrying cannot fix are [`UploadFailure`]s with
/// the `ava1_rar_*` reasons in [`crate::rar_source::RarReason`].
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn upload_rar_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    archive: &Path,
    password: Option<&str>,
) -> Result<TransferResult> {
    use crate::rar_source::{rar_failure, RarOpenError, RarSource};
    let watch = ps5upload_core::transfer::RarLayoutWatch {
        cancel: cfg.cancel.as_deref(),
        live: cfg.progress_live.as_deref(),
    };
    let (manifest, source) = match RarSource::open_watched(archive, password, &cfg.excludes, watch)
    {
        Ok(v) => v,
        Err(RarOpenError::Cancelled) => return Err(anyhow!("transfer_cancelled")),
        Err(RarOpenError::Plan(f)) => return Err(rar_upload_failure(f.reason, f.message).into()),
        // A duplicate, a case clash, a file/dir clash or an unsafe path: terminal.
        Err(RarOpenError::Unsupported(m)) => {
            return Err(UploadFailure {
                reason: "ava1_rar_unsupported".into(),
                detail: m,
            }
            .into())
        }
    };
    // The headers do not give an unknown size, so the job's plan left those entries out: now
    // that they are measured, the bar counts them.
    if let Some(total) = &cfg.progress_total {
        let measured = manifest
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .fold(0u64, |sum, e| sum.saturating_add(e.size));
        total.store(measured, Ordering::Release);
    }
    let source = Arc::new(source);
    let mut opts = SendOptions::upload(dest_root);
    opts.seq = Some(source.clone());
    upload_with_in(pool, &cfg.addr, job_id, manifest, source, opts, cfg).map_err(|e| {
        match e.chain().find_map(|c| rar_failure(c)) {
            Some(f) => rar_upload_failure(f.reason, f.message.clone()).into(),
            None => e,
        }
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn rar_upload_failure(reason: crate::rar_source::RarReason, message: String) -> UploadFailure {
    use crate::rar_source::RarReason::*;
    let detail = match reason {
        PasswordRequired => {
            "the RAR is password protected and no password is available (a restarted engine \
             forgets it); enter the password again"
                .to_string()
        }
        PasswordWrong => "the RAR password is wrong".to_string(),
        Corrupt => format!("the RAR archive is corrupt: {message}"),
        MissingVolume | Reordered | Other => message,
    };
    UploadFailure {
        reason: reason.as_str().into(),
        detail,
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn upload_rar(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    archive: &Path,
    password: Option<&str>,
) -> Result<TransferResult> {
    upload_rar_in(pool(), cfg, job_id, dest_root, archive, password)
}

/// Why the console refused a transfer whose data it had already received (a
/// post-commit failure). The upload must never be retried: the destination is taken
/// and resuming would re-send every byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCommitKind {
    /// The destination already exists (`ERR_EXISTS`).
    Exists,
    /// The destination is on a different storage device than the staged files
    /// (`ERR_CROSS_DEVICE`).
    CrossDevice,
}

impl PostCommitKind {
    /// The machine-readable name Task 23's handler uses to build `error_reason`
    /// (`{"error": …, "detail": …}`). The `Display`s stay human sentences (A2).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exists => "ava1_commit_exists",
            Self::CrossDevice => "ava1_commit_cross_device",
        }
    }
}

impl std::fmt::Display for PostCommitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Exists => "the destination already exists",
            Self::CrossDevice => "the destination is on another storage device",
        })
    }
}

/// The console accepted and applied the transfer but refused to commit it (C13/A2).
/// The reason travels as the typed [`PostCommitKind`], never as a machine-shaped
/// Display — any wrapper or log formatter that rewrites the message must not destroy
/// the engine's `error_reason`. Not retryable: the destination is taken.
#[derive(Debug, thiserror::Error)]
#[error("the console refused to commit the transfer: {kind} ({detail})")]
pub struct PostCommitError {
    pub kind: PostCommitKind,
    /// The console's own message, when it sent one.
    pub detail: String,
}

/// A refusal or terminal connection failure with a stable reason for the UI.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{detail}")]
pub struct UploadFailure {
    pub reason: String,
    pub detail: String,
}

/// An [`UploadFailure`] that happened on one named console. A relay talks to two, so its
/// failure must say which one the user has to fix (the not-paired dialog opens for that
/// console); `console` is the address as the caller gave it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{failure}")]
pub struct ConsoleFailure {
    pub console: String,
    pub failure: UploadFailure,
}

impl ConsoleFailure {
    pub fn on(console: &str, failure: UploadFailure) -> Self {
        Self {
            console: console.to_string(),
            failure,
        }
    }
}

/// The failure when a console answered BUSY to every JobOpen the bound allowed.
pub(crate) fn busy_failure(tries: u32, message: &str) -> anyhow::Error {
    UploadFailure {
        reason: "ava1_busy".into(),
        detail: format!(
            "the console stayed busy for {tries} retries and could not take this job: {message}"
        ),
    }
    .into()
}

/// The failure when a console never answered a JobOpen the bound allowed.
pub(crate) fn open_timeout_failure(tries: u32, waited: Duration) -> anyhow::Error {
    UploadFailure {
        reason: "ava1_open_timeout".into(),
        detail: format!(
            "the console did not answer the job open ({waited:?} each, {tries} retries) and could not take this job"
        ),
    }
    .into()
}

pub(crate) fn refusal_reason(status: u16) -> String {
    match status {
        gen::ERR_NO_SPACE => "ava1_no_space".into(),
        gen::ERR_PATH => "ava1_not_allowed".into(),
        gen::ERR_EXISTS => "ava1_exists".into(),
        gen::ERR_CROSS_DEVICE => "ava1_cross_device".into(),
        gen::ERR_STALLED => "ava1_stalled".into(),
        _ => format!("ava1_refused_{status}"),
    }
}

pub(crate) fn terminal_connection_reason(error: &Ava1Error) -> Option<&'static str> {
    match error {
        Ava1Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            Some("ava1_unreachable")
        }
        Ava1Error::NotPaired => Some("ava1_not_paired"),
        // The console's own refusal of an unpaired peer (pairing window closed).
        Ava1Error::Refused { code, .. }
            if *code == gen::ERR_NOT_PAIRED || *code == gen::ERR_PAIRING_CLOSED =>
        {
            Some("ava1_not_paired")
        }
        Ava1Error::WrongPeer => Some("ava1_wrong_console"),
        _ => None,
    }
}

/// How many consecutive terminal connection failures end a job.
pub(crate) const TERMINAL_ATTEMPTS: u32 = 3;

/// The session-level retry policy every AVA1 job shares (uploads and the relay): a
/// console that refuses us, is not paired, has another key or has no listener is
/// given three tries and then reported with a stable reason; anything else is
/// transient and resets the count. Once a job has been connected, any disconnect is
/// assumed random: "no listener" is waited for until the console is back, bounded only by
/// the job's no-durable-progress limit ([`STALL_LIMIT`]).
#[derive(Default)]
pub(crate) struct SessionGate {
    terminal: u32,
    connected_once: bool,
}

impl SessionGate {
    /// No identity can never recover by retrying.
    pub(crate) fn identity(pool: &Pool) -> Result<(), UploadFailure> {
        if pool.has_identity() {
            return Ok(());
        }
        Err(UploadFailure {
            reason: "ava1_no_identity".into(),
            detail: "no AVA1 identity is available".into(),
        })
    }

    /// The failure of a job that made no durable progress for [`STALL_LIMIT`]: a console that
    /// DDP says is in rest mode (620) is `console_rest_mode`, so the client waits for the wake
    /// instead of spending a recovery attempt; anything else is the plain stall (`bare`).
    /// Blocking for up to about 800 ms (one DDP probe), once, at the end of the job.
    pub(crate) fn stalled(pool: &Pool, console: &str, bare: String) -> anyhow::Error {
        if pool.ddp_code(console) == Some(ps5upload_core::ddp::CODE_STANDBY) {
            let f = crate::console::console_rest_mode();
            pool.note_verdict(
                console,
                &f.reason,
                &f.detail,
                Some(ps5upload_core::ddp::Power::Rest),
            );
            return f.into();
        }
        anyhow::anyhow!(bare)
    }

    /// A session was opened.
    pub(crate) fn connected(&mut self) {
        self.terminal = 0;
        self.connected_once = true;
    }

    /// A session attempt failed: `Some` when the job must end now.
    pub(crate) fn failed(&mut self, e: &Ava1Error) -> Option<UploadFailure> {
        let Some(reason) = terminal_connection_reason(e) else {
            self.terminal = 0;
            return None;
        };
        if reason == "ava1_unreachable" && self.connected_once {
            return None;
        }
        self.terminal += 1;
        (self.terminal >= TERMINAL_ATTEMPTS).then(|| UploadFailure {
            reason: reason.into(),
            detail: e.to_string(),
        })
    }
}

pub(crate) fn refusal(status: u16, message: String) -> UploadFailure {
    // The protocol has one code for any write failure; EFBIG (a file past FAT32's 4 GiB) cannot
    // be fixed by trying again, so it is told apart by the console's words.
    let reason = if status == gen::ERR_IO && message.contains("File too large") {
        "ava1_file_too_large".into()
    } else if status == gen::ERR_PATH
        && (message.contains("is a symbolic link") || message.contains("is not a folder"))
    {
        // AVA1 1.0.7: a link (or a file) where a folder of the job goes, inside the destination.
        "ava1_path_link".into()
    } else {
        refusal_reason(status)
    };
    UploadFailure {
        reason,
        detail: format!("console refused the transfer ({status}): {message}"),
    }
}

/// A drive that cannot take the job, with the figures behind it when they are known. The typed
/// failure stays what callers downcast to; `downcast_ref::<SpaceRefusal>` finds the figures, so
/// the client can word them in the user's language.
fn no_room_failure(detail: String, refusal: Option<crate::space::SpaceRefusal>) -> anyhow::Error {
    let reason = refusal
        .as_ref()
        .map_or("preflight_insufficient_space", |r| r.reason());
    let failure = UploadFailure {
        reason: reason.into(),
        detail,
    };
    match refusal {
        Some(r) => anyhow::Error::from(failure).context(r),
        None => failure.into(),
    }
}

/// The console's own ENOSPC (its preallocation of a file, or a write) after the up-front check
/// admitted the job: the same refusal the check gives, with the same figures and what explains
/// them. The console keeps the journal and the `.ava-part`, so a retry resumes.
pub(crate) fn late_no_space(job: &[u8; 16], console_message: String) -> anyhow::Error {
    let (detail, refusal) = crate::space::late_no_space(job, &console_message);
    no_room_failure(detail, refusal)
}

impl PostCommitError {
    fn new(kind: PostCommitKind, message: Option<String>) -> Self {
        let detail = message
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| kind.as_str().to_string());
        Self { kind, detail }
    }
}

/// Readers for a remote source (SMB/FTP/SFTP): per-file round-trip latency through a
/// saved server means 8 sequential readers starve the lanes.
const REMOTE_SOURCE_READERS: usize = 16;

/// No durable progress for this long ends the job: the elapsed clock resets only when
/// `bytes_durable` grows, so a link that keeps reconnecting but never lands a byte
/// gives up.
pub(crate) const STALL_LIMIT: Duration = Duration::from_secs(600);

pub(crate) fn hex(b: &[u8; 16]) -> String {
    ava1::hex::encode(b)
}

/// SPEC.md §11.4: chooses the policy for "skip files the console already has".
/// When every file carries a real mtime the receiver compares size and mtime
/// (`skip-existing`, free). A source that reports no mtime (some NAS backends) cannot
/// use that, so the job runs under `verify` instead: this reads each file once to put
/// its root in the manifest, and the console skips a file whose size and hash match.
/// Costlier than an mtime compare, but correct, and the console only hashes files that
/// already exist. Returns the policy it set on `opts`.
pub fn apply_existing_policy(
    source: &dyn Source,
    manifest: &mut Manifest,
    opts: &mut SendOptions,
) -> io::Result<u8> {
    apply_existing_policy_with(source, manifest, opts, &Hashing::default())
}

/// What a long up-front hash reports to and obeys: a cancel flag (checked between files
/// and every MiB inside one) and a counter of source bytes hashed so far.
#[derive(Default)]
pub struct Hashing {
    pub cancel: Option<Arc<AtomicBool>>,
    pub done: Option<Arc<std::sync::atomic::AtomicU64>>,
}

impl Hashing {
    fn check(&self) -> io::Result<()> {
        match &self.cancel {
            Some(c) if c.load(Ordering::Relaxed) => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "transfer_cancelled",
            )),
            _ => Ok(()),
        }
    }
}

/// [`apply_existing_policy`] that reports and obeys `hashing` while it reads the source.
pub fn apply_existing_policy_with(
    source: &dyn Source,
    manifest: &mut Manifest,
    opts: &mut SendOptions,
    hashing: &Hashing,
) -> io::Result<u8> {
    let files = |m: &Manifest| {
        m.entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .count()
    };
    let missing = manifest
        .entries
        .iter()
        .any(|e| e.kind == gen::ENTRY_FILE && e.mtime == 0);
    if !missing || files(manifest) == 0 {
        opts.policy = gen::POLICY_SKIP_EXISTING;
        return Ok(opts.policy);
    }
    apply_verify_policy(source, manifest, opts, hashing)?;
    Ok(opts.policy)
}

/// The user's "skip files the console already has" choice, the engine's reconcile
/// modes: `Fast` compares size and mtime when the source has them (SPEC.md §11.4),
/// `Safe` always compares content (size plus BLAKE3 root).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMode {
    Fast,
    Safe,
}

impl SkipMode {
    /// The engine API's `"fast"` / `"safe"`.
    pub fn parse(s: &str) -> Option<SkipMode> {
        match s {
            "fast" => Some(SkipMode::Fast),
            "safe" => Some(SkipMode::Safe),
            _ => None,
        }
    }
}

/// Reads every file once, puts its BLAKE3 root in the manifest and selects `verify`.
/// Used by `Safe` mode, and by `Fast` mode when a file has no mtime. Stops with
/// `ErrorKind::Interrupted` ("transfer_cancelled") when `hashing.cancel` is set.
fn apply_verify_policy(
    source: &dyn Source,
    manifest: &mut Manifest,
    opts: &mut SendOptions,
    hashing: &Hashing,
) -> io::Result<()> {
    for e in manifest
        .entries
        .iter_mut()
        .filter(|e| e.kind == gen::ENTRY_FILE)
    {
        e.root = Some(hash_file(source, &e.path, e.size, hashing)?);
    }
    opts.policy = gen::POLICY_VERIFY;
    Ok(())
}

/// BLAKE3 of a source file (equal to the root the receiver computes, `ava1::verify`).
fn hash_file(source: &dyn Source, rel: &str, size: u64, hashing: &Hashing) -> io::Result<[u8; 32]> {
    hashing.check()?;
    let mut r = source.open(rel)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut off = 0u64;
    while off < size {
        hashing.check()?;
        let n = ava1::source::read_full_at(r.as_mut(), off, &mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{rel} is shorter than its listed size"),
            ));
        }
        h.update(&buf[..n]);
        off += n as u64;
        if let Some(d) = &hashing.done {
            d.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    Ok(*h.finalize().as_bytes())
}

fn source_for(cfg: &TransferConfig, root: &Path) -> Arc<dyn Source> {
    match &cfg.source_fs {
        Some(fs) => Arc::new(FsSource::new(fs.clone(), root.to_path_buf())),
        None => Arc::new(LocalSource::new(root.to_path_buf())),
    }
}

/// [`source_for`] a folder upload, with the config's renames applied, and its kept names read
/// back as the console's (`unescape_names`).
fn dir_source_for(cfg: &TransferConfig, root: &Path) -> Arc<dyn Source> {
    let mut source = source_for(cfg, root);
    if cfg.unescape_names {
        source = Arc::new(crate::source::KeptNamesSource::new(source));
    }
    if cfg.renames.is_empty() {
        return source;
    }
    Arc::new(crate::source::RenamedSource::new(source, &cfg.renames))
}

/// `gate`, also storing in `resumed` the bytes the receiver held before this job's own
/// sending: what it proves durable at an open (whole files in place plus durable ranges of
/// unfinished ones) less what this job has had acknowledged so far. The first open of a
/// resume gives it all; a later open can only raise it, when bytes an earlier run sent became
/// durable after that open (a resume right after a cancel).
fn record_resumed(
    gate: ava1::send::SpaceGate,
    progress: Arc<Progress>,
    resumed: Arc<AtomicU64>,
) -> ava1::send::SpaceGate {
    Arc::new(move |figures: &ava1::send::SpaceFigures| {
        let ours = progress.bytes_sent.load(Ordering::Relaxed);
        resumed.fetch_max(figures.durable_bytes.saturating_sub(ours), Ordering::AcqRel);
        gate(figures)
    })
}

/// One upload, retried across connection loss with the same `job_id`: the console's
/// journal resumes the job, and durable progress bounds the retries.
pub fn upload_with_in(
    pool: &Pool,
    console: &str,
    job_id: [u8; 16],
    manifest: Manifest,
    source: Arc<dyn Source>,
    opts: SendOptions,
    cfg: &TransferConfig,
) -> Result<TransferResult> {
    upload_with_seq_in(pool, console, job_id, manifest, source, None, opts, cfg)
}

/// `upload_with_in` for a forward-only source (`seq`, SPEC.md section 17): one decode
/// thread replaces the random readers and `source` is never read.
#[allow(clippy::too_many_arguments)]
pub fn upload_with_seq_in(
    pool: &Pool,
    console: &str,
    job_id: [u8; 16],
    manifest: Manifest,
    source: Arc<dyn Source>,
    seq: Option<Arc<dyn ava1::seq::SeqSource>>,
    opts: SendOptions,
    cfg: &TransferConfig,
) -> Result<TransferResult> {
    let manifest_files = manifest
        .entries
        .iter()
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .count() as u64;
    ps5upload_core::clog!(
        Debug,
        "transfer",
        "ava1 send {} to {console}:{}: {} entries, {manifest_files} files, {} bytes, {} reader(s), sequential source {}",
        hex(&job_id),
        opts.root,
        manifest.entries.len(),
        manifest.entries.iter().map(|e| e.size).sum::<u64>(),
        opts.readers,
        seq.is_some() || opts.seq.is_some()
    );
    let job_started = Instant::now();
    let manifest = Arc::new(manifest);
    let progress = Arc::new(Progress::default());
    let cancel = cfg
        .cancel
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    // Sender outboards (engine restart without re-reading); removed on success.
    let persist = pool.ava_dir().join("send").join(hex(&job_id));
    let _live = pool.live_job(&job_id); // the journal sweep leaves a running job alone
    let dest = opts.root.clone();
    // What this job promised the drive is released however it ends (design 015/02).
    let _promise = crate::space::Reservation::new(job_id);
    // What the console already held when the job first opened (a resume): read from the
    // figures the sender hands the space check before any byte goes out.
    let resumed = Arc::new(AtomicU64::new(0));
    let space_gate = record_resumed(
        crate::space::gate(pool.room_probe(), console.to_string(), dest.clone(), job_id),
        progress.clone(),
        resumed.clone(),
    );
    let outcome = crate::block_on(async {
        SessionGate::identity(pool)?;
        let _bridge = Bridge::start_resumed(progress.clone(), cfg, resumed.clone());
        let mut backoff = Duration::from_millis(250);
        let mut gate = SessionGate::default();
        let mut busy = 0u32;
        let (mut last_at, mut last_durable) = (Instant::now(), 0u64);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("transfer_cancelled"));
            }
            let durable = progress.bytes_durable.load(Ordering::Relaxed);
            if durable > last_durable {
                (last_at, last_durable) = (Instant::now(), durable);
            } else if last_at.elapsed() > STALL_LIMIT {
                return Err(SessionGate::stalled(
                    pool,
                    console,
                    format!("no durable progress for {STALL_LIMIT:?}; giving up"),
                ));
            }
            let session = match pool.session(console).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate.failed(&e) {
                        return Err(failure.into());
                    }
                    wait(&mut backoff, &e.to_string()).await;
                    continue;
                }
            };
            gate.connected();
            ps5upload_core::clog!(
                Debug,
                "transfer",
                "ava1 send {}: session up, {} bytes durable so far",
                hex(&job_id),
                progress.bytes_durable.load(Ordering::Relaxed)
            );
            progress.telemetry.lock().unwrap().peer_key = Some(session.peer_key());
            let mut link = session.job(job_id);
            let o = SendOptions {
                kind: opts.kind,
                policy: opts.policy,
                flags: opts.flags,
                root: opts.root.clone(),
                // Must stay gen::LARGE_CUTOFF (only tests may change it, C18).
                cutoff: opts.cutoff,
                readers: if cfg.source_fs.is_some() {
                    REMOTE_SOURCE_READERS
                } else {
                    opts.readers
                },
                persist: Some(persist.clone()),
                progress: progress.clone(),
                // The shared flag, not a copy (C18): flipping cfg.cancel ends the job.
                cancel: cancel.clone(),
                bandwidth_cap: cfg.bandwidth_cap_bps,
                // 7z passes its source as `seq`; RAR sets it on `opts`.
                seq: seq.clone().or_else(|| opts.seq.clone()),
                settle_max: None,
                open_ack_timeout: pool.open_ack_timeout(),
                space_gate: Some(space_gate.clone()),
                receiver_silent_timeout: None,
            };
            let (started, durable_before) = (
                Instant::now(),
                progress.bytes_durable.load(Ordering::Relaxed),
            );
            match send_job(&mut link, manifest.clone(), source.clone(), o).await {
                Ok(r) if r.status == gen::STATUS_OK => {
                    let _ = std::fs::remove_dir_all(&persist);
                    let skipped_files = progress.skipped_files.load(Ordering::Relaxed);
                    let skipped_bytes = progress.skipped_bytes.load(Ordering::Relaxed);
                    let mut body = serde_json::json!({
                        "protocol": "ava1",
                        "files": r.files,
                        "bytes": r.bytes,
                        "resent": r.resent,
                        "max_lanes": r.max_lanes,
                        "bottleneck": bottleneck_name(r.bottleneck),
                        "sequential": r.sequential,
                        // What the receiver already had when the job first opened
                        // (SPEC §11.4 skip policies), and the files actually sent.
                        "skipped_files": skipped_files,
                        "skipped_bytes": skipped_bytes,
                        "files_sent": manifest_files.saturating_sub(skipped_files),
                        // Every byte the console held when the job first opened, partly
                        // written files included (a resumed single file has no skipped files).
                        "resumed_bytes": resumed.load(Ordering::Acquire).max(skipped_bytes),
                    });
                    // The bytes are durable, but the console did not confirm its files settled in place
                    // (SPEC.md §15.7): the job's snapshot says so instead of a clean success.
                    if let Some(w) = &r.message {
                        body["warning"] = serde_json::Value::String(w.clone());
                    }
                    return Ok(TransferResult {
                        tx_id_hex: hex(&job_id),
                        // The field name predates AVA1; it counts files (C19).
                        files_sent: u64::from(r.files),
                        bytes_sent: progress.bytes_sent.load(Ordering::Relaxed),
                        dest,
                        commit_ack_body: body.to_string(),
                    });
                }
                Ok(r) if r.status == gen::ERR_EXISTS => {
                    return Err(PostCommitError::new(PostCommitKind::Exists, r.message).into());
                }
                Ok(r) if r.status == gen::ERR_CROSS_DEVICE => {
                    return Err(PostCommitError::new(PostCommitKind::CrossDevice, r.message).into());
                }
                Ok(r) if r.status == gen::ERR_NO_SPACE => {
                    return Err(late_no_space(&job_id, r.message.unwrap_or_default()));
                }
                Ok(r) => return Err(refusal(r.status, r.message.unwrap_or_default()).into()),
                Err(SendError::Disconnected(why)) => {
                    let durable = progress.bytes_durable.load(Ordering::Relaxed);
                    // Whatever ended the session (a dead control link, a lane refused because the
                    // console already dropped the session, a replayed join), a fresh handshake is
                    // the answer; the job and its journal stay, so the resume continues.
                    pool.forget_if(console, &session).await;
                    if rearm(&mut backoff, started, durable_before, durable) {
                        busy = 0;
                    }
                    wait(&mut backoff, &format!("{why} ({durable} bytes durable)")).await;
                }
                // BUSY on the JobOpen is the console saying "not now" (it is finishing this job's files, or
                // has no room): the same bounded backoff as a lost session, cancel honoured at the loop top.
                Err(SendError::Refused { status, message }) if status == gen::ERR_BUSY => {
                    busy += 1;
                    if busy > pool.busy_tries() {
                        return Err(busy_failure(pool.busy_tries(), &message));
                    }
                    wait(&mut backoff, &format!("the console is busy: {message}")).await;
                }
                // The console never answered the JobOpen (the open was lost behind a job it was closing):
                // retried like BUSY within the same bound, then a typed failure.
                Err(SendError::OpenTimeout(t)) => {
                    busy += 1;
                    if busy > pool.busy_tries() {
                        return Err(open_timeout_failure(pool.busy_tries(), t));
                    }
                    wait(
                        &mut backoff,
                        &format!("the console did not answer the open in {t:?}"),
                    )
                    .await;
                }
                Err(SendError::Refused { status, message }) if status == gen::ERR_EXISTS => {
                    return Err(PostCommitError::new(PostCommitKind::Exists, Some(message)).into());
                }
                Err(SendError::Refused { status, message }) if status == gen::ERR_CROSS_DEVICE => {
                    return Err(
                        PostCommitError::new(PostCommitKind::CrossDevice, Some(message)).into(),
                    );
                }
                Err(SendError::Refused { status, message }) if status == gen::ERR_NO_SPACE => {
                    return Err(late_no_space(&job_id, message));
                }
                Err(SendError::Refused { status, message }) => {
                    return Err(refusal(status, message).into());
                }
                Err(SendError::Cancelled) => return Err(anyhow!("transfer_cancelled")),
                // The rest of the job does not fit the drive: said once, up front, with the
                // numbers. The console keeps what it has, so freeing room and retrying resumes.
                Err(SendError::NoRoom(detail)) => {
                    return Err(no_room_failure(detail, crate::space::take_refusal(&job_id)));
                }
                // The source, not the connection: no helper re-send helps. A damaged archive
                // keeps its own wording (InvalidData), see `seq.rs`.
                Err(SendError::Source(e)) if e.kind() != std::io::ErrorKind::InvalidData => {
                    return Err(UploadFailure {
                        reason: "source_read_failed".into(),
                        detail: format!("reading the source: {e}"),
                    }
                    .into());
                }
                Err(e) => return Err(anyhow!(e)),
            }
        }
    });
    match &outcome {
        Ok(r) => ps5upload_core::clog!(
            Debug,
            "transfer",
            "ava1 send {}: done in {} ms: {}",
            hex(&job_id),
            job_started.elapsed().as_millis(),
            r.commit_ack_body
        ),
        Err(e) => ps5upload_core::clog!(
            Debug,
            "transfer",
            "ava1 send {}: ended after {} ms: {e:#}",
            hex(&job_id),
            job_started.elapsed().as_millis()
        ),
    }
    outcome
}

/// The first wait after a connection that was doing real work.
pub(crate) const BACKOFF_FLOOR: Duration = Duration::from_millis(250);

/// A connection that stayed up this long was a working one, not a flapping one.
const WORKED_FOR: Duration = Duration::from_secs(1);

/// Re-arms the backoff after an attempt that ran: it lasted [`WORKED_FOR`] or made bytes durable.
/// The ladder exists for a console that is down. A link that was up and then dropped (a random
/// disconnect, a Wi-Fi roam, a periodic kill) is a console that is back: the next attempt
/// follows at once. Without this the ladder only ever grew, and a drop period near its 5 s cap
/// kept every attempt short of the first durable byte (hardware run 2026-10-04: drop60).
///
/// Returns whether the attempt worked. The caller then also forgets the BUSY / unanswered
/// opens counted so far: each reconnect of a long job may find the console still settling
/// the last attempt's files, and those answers are a bounded wait per reconnect, not a
/// budget the whole job spends.
pub(crate) fn rearm(
    backoff: &mut Duration,
    started: Instant,
    durable_before: u64,
    durable: u64,
) -> bool {
    let worked = durable > durable_before || started.elapsed() >= WORKED_FOR;
    if worked {
        *backoff = BACKOFF_FLOOR;
    }
    worked
}

/// Jittered doubling backoff, 250 ms → 5 s. Logs without ever panicking on a closed
/// stderr (an engine under a dead parent). The jitter is drawn from the clock, so two
/// retry loops (or a retry loop and a periodic drop) cannot stay in lock step.
pub(crate) async fn wait(backoff: &mut Duration, why: &str) {
    let spread = (backoff.as_millis() as u64 / 4).max(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let jitter = Duration::from_millis((u64::from(nanos) ^ u64::from(std::process::id())) % spread);
    let sleep = *backoff + jitter;
    let _ = writeln!(std::io::stderr(), "ava1: reconnecting in {sleep:?}: {why}");
    ps5upload_core::events::emit(
        ps5upload_core::events::Cat::Connection,
        ps5upload_core::events::Level::Warn,
        "reconnecting",
        None,
        format!("reconnecting in {sleep:?}: {why}"),
    );
    tokio::time::sleep(sleep).await;
    *backoff = (*backoff * 2).min(Duration::from_secs(5));
}

pub fn upload_with(
    console: &str,
    job_id: [u8; 16],
    manifest: Manifest,
    source: Arc<dyn Source>,
    opts: SendOptions,
    cfg: &TransferConfig,
) -> Result<TransferResult> {
    upload_with_in(pool(), console, job_id, manifest, source, opts, cfg)
}

pub fn upload_file_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest: &str,
    src: &Path,
) -> Result<TransferResult> {
    // C11: `dest` is the full destination path (parent directory + file name) —
    // `JF_SINGLE_FILE` writes `<dest>.ava-part` and renames it to `<dest>` on the
    // console, exactly the contract at the call sites. Do not "fix" it into a
    // root/name split.
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source has no parent directory"))?;
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("source name is not UTF-8"))?;
    let source = source_for(cfg, parent);
    let manifest = manifest::single(source.as_ref(), name)?;
    let mut opts = SendOptions::upload(dest);
    opts.flags = gen::JF_SINGLE_FILE;
    upload_with_in(pool, &cfg.addr, job_id, manifest, source, opts, cfg)
}

pub fn upload_dir_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
) -> Result<TransferResult> {
    let source = dir_source_for(cfg, src_dir);
    let excludes = cfg.excludes.clone();
    // The same matcher the engine uses, so excludes behave identically.
    let manifest = manifest::walk(source.as_ref(), &|p: &str| {
        ps5upload_core::excludes::is_excluded_strings(Path::new(p), &excludes)
    })?;
    upload_with_in(
        pool,
        &cfg.addr,
        job_id,
        manifest,
        source,
        SendOptions::upload(dest_root),
        cfg,
    )
}

/// A folder upload that skips what the console already has (the engine's "resume"
/// strategy). Local and remote sources alike; see [`apply_existing_policy`].
pub fn upload_dir_skip_existing_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
    mode: SkipMode,
) -> Result<TransferResult> {
    let source = dir_source_for(cfg, src_dir);
    let excludes = cfg.excludes.clone();
    let mut manifest = manifest::walk(source.as_ref(), &|p: &str| {
        ps5upload_core::excludes::is_excluded_strings(Path::new(p), &excludes)
    })?;
    let mut opts = SendOptions::upload(dest_root);
    let hashing = Hashing {
        cancel: cfg.cancel.clone(),
        done: cfg.progress_verify.clone(),
    };
    let hashed = match mode {
        SkipMode::Fast => {
            apply_existing_policy_with(source.as_ref(), &mut manifest, &mut opts, &hashing)
                .map(|_| ())
        }
        SkipMode::Safe => apply_verify_policy(source.as_ref(), &mut manifest, &mut opts, &hashing),
    };
    match hashed {
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            return Err(anyhow!("transfer_cancelled"))
        }
        other => other?,
    }
    upload_with_in(pool, &cfg.addr, job_id, manifest, source, opts, cfg)
}

pub fn upload_dir_skip_existing(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
    mode: SkipMode,
) -> Result<TransferResult> {
    upload_dir_skip_existing_in(pool(), cfg, job_id, dest_root, src_dir, mode)
}

/// The path within an AVA1 job's destination root. A relative list
/// destination is relative to that root; absolute destinations must really be
/// below it, with a path-component boundary.
fn relative_list_path(dest_root: &str, dest: &str) -> Result<String> {
    let root = if dest_root == "/" {
        "/"
    } else {
        dest_root.trim_end_matches('/')
    };
    let rel = if dest.starts_with('/') {
        Path::new(dest)
            .strip_prefix(Path::new(root))
            .map_err(|_| anyhow!("{dest} is not under {root}"))?
    } else {
        Path::new(dest)
    };
    let rel = rel
        .to_str()
        .ok_or_else(|| anyhow!("destination is not UTF-8"))?;
    manifest::check_path(rel)?;
    Ok(rel.to_owned())
}

/// Where one file-list destination goes: below the list's root (relative to it), or in another
/// directory (an absolute destination outside the root), which needs a job of its own.
enum Placed {
    In(String),
    Out { dir: String, name: String },
}

fn place_list_path(dest_root: &str, dest: &str) -> Result<Placed> {
    let root = if dest_root == "/" {
        "/"
    } else {
        dest_root.trim_end_matches('/')
    };
    // Only a path that is not below the root at all is "elsewhere". One below the root that
    // fails the path rules (`..`, an empty or odd component) is refused, never rerouted.
    if !dest.starts_with('/') || Path::new(dest).strip_prefix(Path::new(root)).is_ok() {
        return relative_list_path(root, dest).map(Placed::In);
    }
    let p = Path::new(dest);
    let (Some(dir), Some(name)) = (
        p.parent().and_then(|d| d.to_str()),
        p.file_name().and_then(|n| n.to_str()),
    ) else {
        return Err(anyhow!("{dest} is not a file path"));
    };
    manifest::check_path(name)?;
    // Every component of the directory the job will be rooted at is checked too.
    if dir != "/" {
        manifest::check_path(dir.trim_start_matches('/'))
            .map_err(|e| anyhow!("{dest}: the directory is refused: {e}"))?;
    }
    Ok(Placed::Out {
        dir: dir.to_string(),
        name: name.to_string(),
    })
}

/// One AVA1 job is one manifest under one root. A file list may name destinations in several
/// directories, so it is split: the files under `dest_root` first, then one job per other
/// destination directory (sorted, so a resume sees the same jobs). Each entry is
/// `(job root, [(path relative to that root, source)])`.
type ListGroup = (String, Vec<(String, PathBuf)>);

fn split_list(dest_root: &str, entries: &[FileListEntry]) -> Result<Vec<ListGroup>> {
    let root = if dest_root == "/" {
        "/"
    } else {
        dest_root.trim_end_matches('/')
    };
    let mut inside: Vec<(String, PathBuf)> = Vec::new();
    let mut outside: std::collections::BTreeMap<String, Vec<(String, PathBuf)>> =
        Default::default();
    for e in entries {
        match place_list_path(root, &e.dest)? {
            Placed::In(rel) => inside.push((rel, e.src.clone().into())),
            Placed::Out { dir, name } => outside
                .entry(dir)
                .or_default()
                .push((name, e.src.clone().into())),
        }
    }
    let mut groups = Vec::new();
    if !inside.is_empty() || outside.is_empty() {
        groups.push((root.to_string(), inside));
    }
    groups.extend(outside);
    Ok(groups)
}

/// The destination of a group's first file, for an error that says which path failed.
fn first_dest(root: &str, files: &[(String, PathBuf)]) -> String {
    let rel = files.iter().map(|f| f.0.as_str()).min().unwrap_or("");
    format!("{}/{rel}", root.trim_end_matches('/'))
}

/// Names the failing path while keeping the error's type (a typed failure keeps its reason).
fn named(e: anyhow::Error, path: &str) -> anyhow::Error {
    if e.to_string().contains("transfer_cancelled") {
        return e;
    }
    if let Some(f) = e.downcast_ref::<UploadFailure>() {
        return UploadFailure {
            reason: f.reason.clone(),
            detail: format!("{path}: {}", f.detail),
        }
        .into();
    }
    if let Some(p) = e.downcast_ref::<PostCommitError>() {
        return PostCommitError {
            kind: p.kind,
            detail: format!("{path}: {}", p.detail),
        }
        .into();
    }
    e.context(path.to_string())
}

/// The job id of group `k`: group 0 keeps the caller's id, the others derive from it, so a
/// resume of the whole list finds each job's journal again.
fn group_job_id(base: [u8; 16], k: usize) -> [u8; 16] {
    let mut id = base;
    for (b, x) in id[8..].iter_mut().zip((k as u64).to_le_bytes()) {
        *b ^= x;
    }
    id
}

/// Counters of the jobs already finished, added to the running job's own.
#[derive(Default, Clone, Copy)]
struct Done {
    bytes: u64,
    files: u64,
    files_finalized: u64,
    bytes_finalized: u64,
    bytes_resumed: u64,
}

/// Runs one job of a list with private counters and mirrors `done + private` into the caller's
/// absolute counters while it runs, so the engine's ticker sees one progressing transfer.
fn run_aggregated<T>(
    cfg: &TransferConfig,
    done: &mut Done,
    f: impl FnOnce(&TransferConfig) -> Result<T>,
) -> Result<T> {
    use std::sync::atomic::AtomicU64;
    type C = Option<Arc<AtomicU64>>;
    let fresh = |c: &C| c.as_ref().map(|_| Arc::new(AtomicU64::new(0)));
    let mut inner = cfg.clone();
    inner.progress_bytes = fresh(&cfg.progress_bytes);
    inner.progress_files = fresh(&cfg.progress_files);
    inner.progress_files_finalized = fresh(&cfg.progress_files_finalized);
    inner.progress_bytes_finalized = fresh(&cfg.progress_bytes_finalized);
    inner.progress_bytes_resumed = fresh(&cfg.progress_bytes_resumed);
    let pairs = |d: Done| -> Vec<(C, C, u64)> {
        vec![
            // First: `progress_bytes` includes it, so it must never lag behind them.
            (
                cfg.progress_bytes_resumed.clone(),
                inner.progress_bytes_resumed.clone(),
                d.bytes_resumed,
            ),
            (
                cfg.progress_bytes.clone(),
                inner.progress_bytes.clone(),
                d.bytes,
            ),
            (
                cfg.progress_files.clone(),
                inner.progress_files.clone(),
                d.files,
            ),
            (
                cfg.progress_files_finalized.clone(),
                inner.progress_files_finalized.clone(),
                d.files_finalized,
            ),
            (
                cfg.progress_bytes_finalized.clone(),
                inner.progress_bytes_finalized.clone(),
                d.bytes_finalized,
            ),
        ]
    };
    let mirror = |d: Done| {
        for (real, mine, base) in pairs(d) {
            if let (Some(real), Some(mine)) = (real, mine) {
                real.store(base + mine.load(Ordering::Relaxed), Ordering::Relaxed);
            }
        }
    };
    /// Stops and joins the mirroring thread when dropped, so a panicking job (which unwinds past
    /// this function) cannot leave it running.
    struct Ticker {
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Ticker {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
    let base = *done;
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = Ticker {
        stop: stop.clone(),
        handle: Some({
            let pairs = pairs(base);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for (real, mine, base) in &pairs {
                        if let (Some(real), Some(mine)) = (real, mine) {
                            real.store(base + mine.load(Ordering::Relaxed), Ordering::Relaxed);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            })
        }),
    };
    let r = f(&inner);
    drop(ticker);
    mirror(base);
    let get = |c: &C| c.as_ref().map_or(0, |a| a.load(Ordering::Relaxed));
    done.bytes += get(&inner.progress_bytes);
    done.files += get(&inner.progress_files);
    done.files_finalized += get(&inner.progress_files_finalized);
    done.bytes_finalized += get(&inner.progress_bytes_finalized);
    done.bytes_resumed += get(&inner.progress_bytes_resumed);
    r
}

/// Folds the per-job results of a split list into one.
fn merge_results(parts: Vec<TransferResult>) -> TransferResult {
    let jobs = parts.len();
    let mut it = parts.into_iter();
    let mut out = it.next().expect("at least one job");
    let mut ack: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&out.commit_ack_body).unwrap_or_default();
    for p in it {
        out.files_sent += p.files_sent;
        out.bytes_sent += p.bytes_sent;
        let other: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&p.commit_ack_body).unwrap_or_default();
        for k in [
            "files",
            "bytes",
            "resent",
            "skipped_files",
            "skipped_bytes",
            "files_sent",
        ] {
            let sum = ack.get(k).and_then(|v| v.as_u64()).unwrap_or(0)
                + other.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            ack.insert(k.into(), sum.into());
        }
        let lanes = ack
            .get("max_lanes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .max(other.get("max_lanes").and_then(|v| v.as_u64()).unwrap_or(0));
        ack.insert("max_lanes".into(), lanes.into());
    }
    ack.insert("jobs".into(), jobs.into());
    out.commit_ack_body = serde_json::Value::Object(ack).to_string();
    out
}

/// A file-list upload. Destinations below `dest_root` form one job; each other directory a
/// destination names (an absolute path outside the root) is a job of its own, run in sequence
/// under this one call: progress aggregates, a cancel stops the rest, and a failure names the
/// first failing path.
pub fn upload_list_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    entries: &[FileListEntry],
) -> Result<TransferResult> {
    let mut groups = split_list(dest_root, entries)?;
    if groups.len() == 1 {
        let (root, files) = groups.remove(0);
        let first = first_dest(&root, &files);
        return upload_list_group_in(pool, cfg, job_id, &root, files).map_err(|e| named(e, &first));
    }
    let cancel = cfg.cancel.clone();
    let mut done = Done::default();
    let mut results = Vec::new();
    for (k, (root, files)) in groups.into_iter().enumerate() {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(anyhow!("transfer_cancelled"));
        }
        let first = first_dest(&root, &files);
        let id = group_job_id(job_id, k);
        let r = run_aggregated(cfg, &mut done, |c| {
            upload_list_group_in(pool, c, id, &root, files)
        })
        .map_err(|e| named(e, &first))?;
        results.push(r);
    }
    Ok(merge_results(results))
}

/// One job of a file list: `files` are `(path relative to root, source)`.
fn upload_list_group_in(
    pool: &Pool,
    cfg: &TransferConfig,
    job_id: [u8; 16],
    root: &str,
    mut files: Vec<(String, PathBuf)>,
) -> Result<TransferResult> {
    // Exactly manifest::walk's order (depth-first preorder: component comparison).
    files.sort_by(|a, b| a.0.split('/').cmp(b.0.split('/')));
    let mut all: Vec<Entry> = Vec::new();
    let mut dirs = std::collections::BTreeSet::new();
    for (rel, _) in &files {
        let mut acc = String::new();
        let parts: Vec<&str> = rel.split('/').collect();
        for c in parts[..parts.len() - 1].iter() {
            acc = if acc.is_empty() {
                (*c).to_string()
            } else {
                format!("{acc}/{c}")
            };
            dirs.insert(acc.clone());
        }
    }
    for d in dirs {
        all.push(Entry {
            kind: gen::ENTRY_DIR,
            mode: 0o755,
            size: 0,
            mtime: 0,
            path: d,
            root: None,
        });
    }
    let source: Arc<dyn Source> = Arc::new(ListSource::new(files.clone()));
    for (rel, _) in files {
        let st = source.stat(&rel)?;
        all.push(Entry {
            kind: gen::ENTRY_FILE,
            mode: st.mode,
            size: st.size,
            mtime: st.mtime,
            path: rel,
            root: None,
        });
    }
    all.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
    for e in &all {
        manifest::check_path(&e.path)?;
    }
    upload_with_in(
        pool,
        &cfg.addr,
        job_id,
        Manifest { entries: all },
        source,
        SendOptions::upload(root),
        cfg,
    )
}

pub fn upload_file(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest: &str,
    src: &Path,
) -> Result<TransferResult> {
    upload_file_in(pool(), cfg, job_id, dest, src)
}

pub fn upload_dir(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
) -> Result<TransferResult> {
    upload_dir_in(pool(), cfg, job_id, dest_root, src_dir)
}

pub fn upload_list(
    cfg: &TransferConfig,
    job_id: [u8; 16],
    dest_root: &str,
    entries: &[FileListEntry],
) -> Result<TransferResult> {
    upload_list_in(pool(), cfg, job_id, dest_root, entries)
}

#[cfg(test)]
mod list_destination_tests {
    use super::{group_job_id, relative_list_path, split_list};
    use ps5upload_core::transfer::FileListEntry;

    #[test]
    fn relative_destination_stays_under_the_requested_root() {
        assert_eq!(
            relative_list_path("/data/games", "Title/file.bin").unwrap(),
            "Title/file.bin"
        );
    }

    #[test]
    fn an_absolute_destination_uses_path_components() {
        assert_eq!(
            relative_list_path("/data/games", "/data/games/Title/file.bin").unwrap(),
            "Title/file.bin"
        );
        assert!(relative_list_path("/data/games", "/data/gamesX/file.bin").is_err());
    }

    #[test]
    fn a_destination_outside_the_root_is_rejected() {
        assert!(relative_list_path("/data/games", "/data/other/file.bin").is_err());
        assert!(relative_list_path("/data/games", "../other/file.bin").is_err());
    }

    fn e(src: &str, dest: &str) -> FileListEntry {
        FileListEntry {
            src: src.into(),
            dest: dest.into(),
        }
    }

    #[test]
    fn a_list_splits_into_the_root_then_one_job_per_other_directory() {
        let groups = split_list(
            "/data/games/",
            &[
                e("a", "Title/a"),
                e("b", "/data/other/b"),
                e("c", "/data/games/Title/c"),
                e("d", "/data/other/d"),
                e("f", "/data/third/x/f"),
            ],
        )
        .unwrap();
        let roots: Vec<&str> = groups.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(roots, ["/data/games", "/data/other", "/data/third/x"]);
        let names = |i: usize| groups[i].1.iter().map(|f| f.0.as_str()).collect::<Vec<_>>();
        assert_eq!(names(0), ["Title/a", "Title/c"]);
        assert_eq!(names(1), ["b", "d"]);
        assert_eq!(names(2), ["f"]);
    }

    #[test]
    fn a_list_wholly_outside_the_root_has_no_empty_first_job() {
        let groups = split_list("/data/games", &[e("b", "/data/other/b")]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "/data/other");
    }

    #[test]
    fn a_bad_path_under_the_root_is_refused_not_rerouted_as_elsewhere() {
        // Below the root, but `..` escapes it again: the path error, not a job rooted at
        // "/data/games/..".
        let err = split_list("/data/games", &[e("b", "/data/games/../etc/b")]).unwrap_err();
        assert!(!format!("{err:#}").is_empty());
    }

    #[test]
    fn every_component_of_an_outside_directory_is_checked() {
        for bad in [
            "/data/./x/b",
            "/data//x/b",
            "/data/x/../b",
            "/data/\u{0}x/b",
        ] {
            assert!(
                split_list("/data/games", &[e("b", bad)]).is_err(),
                "{bad:?} must be refused"
            );
        }
        assert!(split_list("/data/games", &[e("b", "/data/ok dir/x/b")]).is_ok());
    }

    #[test]
    fn a_hostile_destination_is_still_refused() {
        assert!(split_list("/data/games", &[e("b", "/data/../etc/b")]).is_err());
        assert!(split_list("/data/games", &[e("b", "../other/b")]).is_err());
    }

    #[test]
    fn a_panicking_job_does_not_leak_the_progress_ticker() {
        use ps5upload_core::transfer::TransferConfig;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let real = Arc::new(AtomicU64::new(0));
        let mut cfg = TransferConfig::new("c");
        cfg.progress_bytes = Some(real.clone());
        let mut done = super::Done::default();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = super::run_aggregated(&cfg, &mut done, |c| -> anyhow::Result<()> {
                c.progress_bytes
                    .as_ref()
                    .unwrap()
                    .store(7, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(80));
                panic!("the job panicked");
            });
        }));
        assert!(r.is_err());
        assert_eq!(
            real.load(Ordering::Relaxed),
            7,
            "the mirror ran while the job did"
        );
        // The ticker must have stopped with the panic: it would overwrite this sentinel.
        real.store(12_345, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(
            real.load(Ordering::Relaxed),
            12_345,
            "a leaked ticker kept mirroring"
        );
    }

    #[test]
    fn a_zip_that_cannot_be_read_is_retryable_but_a_bad_one_is_unsupported() {
        let d = std::env::temp_dir().join(format!("p5a-zip-open-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let cfg = ps5upload_core::transfer::TransferConfig::new("c");
        let pool = crate::pool::Pool::unavailable();
        // Missing file: an I/O error, typed zip_read_error (retryable; the message carries
        // the OS text the client's fatal-message rules also look at).
        let e = super::upload_zip_in(&pool, &cfg, [1; 16], "r", &d.join("nope.zip")).unwrap_err();
        let f = e.downcast_ref::<super::UploadFailure>().expect("typed");
        assert_eq!(f.reason, "zip_read_error");
        assert!(e.downcast_ref::<super::ZipUnsupported>().is_none());
        // Not a zip at all: a format error, unsupported (terminal).
        std::fs::write(d.join("junk.zip"), b"this is not a zip file").unwrap();
        let e = super::upload_zip_in(&pool, &cfg, [1; 16], "r", &d.join("junk.zip")).unwrap_err();
        assert!(e.downcast_ref::<super::ZipUnsupported>().is_some(), "{e:#}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn group_job_ids_are_stable_and_distinct() {
        let base = [7u8; 16];
        assert_eq!(group_job_id(base, 0), base);
        assert_ne!(group_job_id(base, 1), group_job_id(base, 2));
        assert_eq!(group_job_id(base, 1), group_job_id(base, 1));
    }
}

#[cfg(test)]
mod failure_reason_tests {
    use super::{refusal, refusal_reason, terminal_connection_reason};
    use crate::gen;
    use ava1::Ava1Error;
    use std::io;

    #[test]
    fn connection_refusal_and_pairing_errors_have_distinct_terminal_reasons() {
        assert_eq!(
            terminal_connection_reason(&Ava1Error::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused
            ))),
            Some("ava1_unreachable")
        );
        assert_eq!(
            terminal_connection_reason(&Ava1Error::NotPaired),
            Some("ava1_not_paired")
        );
        assert_eq!(
            terminal_connection_reason(&Ava1Error::WrongPeer),
            Some("ava1_wrong_console")
        );
        assert_eq!(terminal_connection_reason(&Ava1Error::Timeout), None);
    }

    #[test]
    fn refusals_keep_their_machine_reason() {
        assert_eq!(refusal_reason(gen::ERR_NO_SPACE), "ava1_no_space");
        assert_eq!(refusal_reason(gen::ERR_PATH), "ava1_not_allowed");
        assert_eq!(
            refusal(gen::ERR_PATH, "Game/sce_sys is a symbolic link".into()).reason,
            "ava1_path_link"
        );
        assert_eq!(
            refusal(gen::ERR_PATH, "outside the allowed roots".into()).reason,
            "ava1_not_allowed"
        );
        assert_eq!(refusal_reason(gen::ERR_EXISTS), "ava1_exists");
        assert_eq!(refusal_reason(gen::ERR_CROSS_DEVICE), "ava1_cross_device");
        assert_eq!(refusal_reason(gen::ERR_STALLED), "ava1_stalled");
        assert_eq!(refusal_reason(65535), "ava1_refused_65535");
        // Every typed refusal (upload, download, relay, copy) goes through `refusal`.
        let f = refusal(gen::ERR_STALLED, "no data".into());
        assert_eq!(f.reason, "ava1_stalled");
        // One code for every write failure; a file too big for the drive is not retried.
        let f = refusal(gen::ERR_IO, "write failed: File too large (a.bin)".into());
        assert_eq!(f.reason, "ava1_file_too_large");
        let f = refusal(gen::ERR_IO, "write failed: Input/output error".into());
        assert_eq!(f.reason, format!("ava1_refused_{}", gen::ERR_IO));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused() -> Ava1Error {
        Ava1Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
    }
    fn other() -> Ava1Error {
        Ava1Error::Io(std::io::Error::other("handshake reset"))
    }

    #[test]
    fn a_connection_that_worked_rearms_the_backoff_and_a_flapping_one_does_not() {
        let long_ago = Instant::now() - Duration::from_secs(3);
        let mut b = Duration::from_secs(5);
        rearm(&mut b, Instant::now(), 10, 10);
        assert_eq!(b, Duration::from_secs(5), "short, no bytes: keep climbing");
        rearm(&mut b, Instant::now(), 10, 11);
        assert_eq!(
            b, BACKOFF_FLOOR,
            "durable bytes landed: the console is back"
        );
        b = Duration::from_secs(5);
        rearm(&mut b, long_ago, 10, 10);
        assert_eq!(b, BACKOFF_FLOOR, "it stayed up: a random drop, not a flap");
    }

    #[test]
    fn the_third_consecutive_refusal_ends_the_job() {
        let mut g = SessionGate::default();
        assert!(g.failed(&refused()).is_none());
        assert!(g.failed(&refused()).is_none());
        let f = g.failed(&refused()).expect("third attempt is terminal");
        assert_eq!(f.reason, "ava1_unreachable");
    }

    #[test]
    fn a_transient_error_resets_the_count() {
        let mut g = SessionGate::default();
        for _ in 0..2 {
            assert!(g.failed(&refused()).is_none());
        }
        assert!(g.failed(&other()).is_none());
        for _ in 0..2 {
            assert!(g.failed(&refused()).is_none(), "the count restarted");
        }
        assert!(g.failed(&refused()).is_some());
    }

    #[test]
    fn a_connected_session_resets_the_count() {
        let mut g = SessionGate::default();
        for _ in 0..2 {
            assert!(g.failed(&refused()).is_none());
        }
        g.connected();
        assert!(g.failed(&refused()).is_none());
    }

    #[test]
    fn a_helper_restart_mid_job_is_waited_for_not_ended_on_the_third_refusal() {
        // Hardware run 2026-10-04: the bench killed and relaunched the helper mid-upload; its
        // port refused for a few seconds and the job ended after three refusals (~2 s).
        let mut g = SessionGate::default();
        g.connected();
        for _ in 0..20 {
            assert!(
                g.failed(&refused()).is_none(),
                "a restarting helper is waited for"
            );
        }
    }

    #[test]
    fn pairing_failures_after_a_connection_still_end_the_job_on_the_third_try() {
        let mut g = SessionGate::default();
        g.connected();
        assert!(g.failed(&Ava1Error::NotPaired).is_none());
        assert!(g.failed(&Ava1Error::NotPaired).is_none());
        assert_eq!(
            g.failed(&Ava1Error::NotPaired).unwrap().reason,
            "ava1_not_paired"
        );
    }

    #[test]
    fn pairing_and_identity_failures_have_their_own_reasons() {
        for (e, reason) in [
            (Ava1Error::NotPaired, "ava1_not_paired"),
            (Ava1Error::WrongPeer, "ava1_wrong_console"),
            (
                Ava1Error::Refused {
                    code: gen::ERR_PAIRING_CLOSED,
                    message: "closed".into(),
                },
                "ava1_not_paired",
            ),
        ] {
            let mut g = SessionGate::default();
            assert!(g.failed(&e).is_none());
            assert!(g.failed(&e).is_none());
            assert_eq!(g.failed(&e).unwrap().reason, reason);
        }
    }

    #[test]
    fn a_stall_on_a_console_in_rest_mode_is_console_rest_mode() {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = asked.clone();
        let pool = Pool::unavailable().with_power_probe(move |h| {
            seen.lock().unwrap().push(h.to_string());
            Some(620)
        });
        let e = SessionGate::stalled(&pool, "10.0.0.2", "no durable progress".into());
        let f = e.downcast_ref::<UploadFailure>().expect("a typed failure");
        assert_eq!(f.reason, "console_rest_mode");
        assert_eq!(asked.lock().unwrap().as_slice(), ["10.0.0.2"]);
        assert_eq!(
            pool.recent_power("10.0.0.2"),
            Some(ps5upload_core::ddp::Power::Rest)
        );
    }

    #[test]
    fn a_stall_on_an_awake_or_silent_console_stays_the_bare_stall() {
        for code in [Some(200), None] {
            let pool = Pool::unavailable().with_power_probe(move |_| code);
            let e = SessionGate::stalled(&pool, "10.0.0.2", "no durable progress".into());
            assert!(e.downcast_ref::<UploadFailure>().is_none(), "{code:?}");
            assert_eq!(e.to_string(), "no durable progress");
        }
    }

    #[test]
    fn no_identity_is_terminal_immediately() {
        let d = std::env::temp_dir().join(format!("gate-noid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("identity")).unwrap();
        let pool = Pool::new(d.clone());
        assert_eq!(
            SessionGate::identity(&pool).unwrap_err().reason,
            "ava1_no_identity"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
