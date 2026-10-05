//! Host-side planning for uploads: the transfer configuration, the archive inspectors and
//! plan previews (`.zip`, `.7z`, `.rar`), the entry-name sanitizers, and the live notes a
//! running job reports.
//!
//! The bytes themselves go over AVA1 (`ps5upload-ava1`), whose adapters take a
//! [`TransferConfig`] and read archives through the decoders and walkers defined here.

use anyhow::{bail, Context, Result};
use std::path::Path;

// ─── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Where the source files are read from; `None` = this computer's disk.
    pub source_fs: Option<std::sync::Arc<dyn crate::source_fs::SourceFs>>,
    /// The console's address. A bare host is what the engine passes; AVA1 owns the port, and a
    /// stale `host:port` suffix is ignored.
    pub addr: String,
    /// Glob-ish patterns to exclude from folder walks. See `crate::excludes` for the pattern
    /// grammar. Empty = include everything; populated = skip matching files before they enter
    /// the manifest. The common case is passing `excludes::DEFAULT_EXCLUDES` to skip
    /// `.DS_Store`, `*.esbak`, `.git/**`, `Thumbs.db`, `desktop.ini`.
    pub excludes: Vec<String>,
    /// Optional cumulative-bytes progress counter the transfer adds to as bytes go out.
    /// Consumers (the engine's transfer handlers) poll it on a separate cadence to drive
    /// progress-bar updates without adding a lock to the hot send loop. `None` = no reporting.
    pub progress_bytes: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Optional per-file progress counter: one `fetch_add(1)` per source file as it is read.
    /// Lets the UI show a counter that climbs continuously through the read-many-small-files
    /// phase instead of jumping. `None` = no per-file tick.
    pub progress_files: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Optional counters of what the console has made durable: files and bytes. The engine's
    /// ticker reads both and shows a live "Finalized N of M files" counter, so the UI has
    /// motion through the end of a job. `None` = no such counters.
    pub progress_files_finalized: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub progress_bytes_finalized: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Bytes of the source hashed so far while a `verify` job reads every file up front
    /// (SPEC.md §11.4). The engine shows it as the job's "verify" stage.
    pub progress_verify: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// The live notes a running job reports beyond bytes and files (what limits it, the 7z/RAR
    /// skipping phase, files still settling). The engine puts them in the job snapshot.
    pub progress_live: Option<std::sync::Arc<LiveNotes>>,
    /// Optional outbound bandwidth cap, in bytes per second. `None` = unlimited. Useful when
    /// uploading from a connection that is also carrying video calls or game streaming and you
    /// want to leave headroom.
    pub bandwidth_cap_bps: Option<u64>,
    /// Optional cooperative cancel flag. When set and flipped to `true`, the transfer stops at
    /// its next checkpoint and returns a `transfer_cancelled` error instead of finishing.
    /// `None` = never cancel.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl TransferConfig {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            excludes: Vec::new(),
            progress_bytes: None,
            progress_files: None,
            progress_files_finalized: None,
            progress_bytes_finalized: None,
            progress_verify: None,
            progress_live: None,
            bandwidth_cap_bps: None,
            cancel: None,
            source_fs: None,
        }
    }

    /// The file system the source is read from.
    pub fn fs(&self) -> &dyn crate::source_fs::SourceFs {
        match &self.source_fs {
            Some(fs) => fs.as_ref(),
            None => &crate::source_fs::LocalFs,
        }
    }

    /// Convenience: enable the built-in default exclude list (dotfile /
    /// OS-junk / editor-backup filter). Mutates the config in place.
    pub fn with_default_excludes(mut self) -> Self {
        self.excludes = crate::excludes::DEFAULT_EXCLUDES
            .iter()
            .map(|s| s.to_string())
            .collect();
        self
    }
}

// ─── Result ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TransferResult {
    /// The job id, in hex.
    pub tx_id_hex: String,
    /// Files the job delivered.
    pub files_sent: u64,
    pub bytes_sent: u64,
    pub dest: String,
    /// JSON the adapters build from the job's final status: the skipped-file counts, the
    /// verify result, the transport's telemetry. Callers parse it as JSON.
    pub commit_ack_body: String,
}

/// True when this error is (or wraps) the `transfer_cancelled` sentinel raised when the
/// cancel flag is set.
fn is_cancel_err(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string() == "transfer_cancelled")
}

/// Returns true when an error from a transfer is network-drop-ish and
/// worth retrying via resume. Intentionally conservative: we only retry
/// on errors whose root cause is "the TCP stream broke mid-transfer,"
/// not on protocol errors like `direct_tx_corrupt` (the payload has
/// already aborted the tx — a retry can't help).
pub fn is_retryable_transfer_error(err: &anyhow::Error) -> bool {
    // A cancelled transfer is never retried. The user asked for it to stop —
    // resuming would put bytes back on the wire after the UI said "cancelled",
    // which is the exact symptom reported in 5.4.7. Today's cancel sentinel
    // carries no io::Error so it would fall through to `false` anyway; this
    // makes the guarantee explicit rather than incidental, so a future cancel
    // path that wraps an io::Error (a killed socket, say) can't reintroduce it.
    if is_cancel_err(err) {
        return false;
    }
    // Walk the chain looking for std::io::Error with a retryable kind.
    // Covers: mid-transfer TCP resets (ConnectionReset/ConnectionAborted/
    // BrokenPipe), server-side hang on shutdown (UnexpectedEof), wifi
    // drop on write (TimedOut), EINTR during signal delivery on macOS
    // (Interrupted), and the half-open state seen after a macOS
    // sleep/wake cycle (NotConnected).
    for cause in err.chain() {
        if let Some(ioerr) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                ioerr.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::NotConnected
            ) {
                return true;
            }
            // Backstop for transient *local* network-stack resource
            // exhaustion (Windows WSAENOBUFS 10055 under connection
            // churn): retry with backoff rather than aborting the whole
            // upload. These map to `ErrorKind::Other`, so they must be
            // matched by OS code, not kind.
            //
            // `if` rather than `return`: this used to return on the FIRST
            // io::Error in the chain, so a wrapper io::Error with a
            // non-retryable kind hid a retryable one underneath it and the
            // upload aborted instead of resuming. The loop is meant to walk
            // the whole chain.
            if crate::net::is_transient_local_resource_error(ioerr) {
                return true;
            }
        }
    }
    false
}

// ─── Explicit file-list transfer ──────────────────────────────────────────────

/// An entry in an explicit file-list transfer.
#[derive(Debug, Clone)]
pub struct FileListEntry {
    /// Absolute path to the local file.
    pub src: String,
    /// Destination path on PS5 storage (absolute).
    pub dest: String,
}

/// Live notes of a running AVA1 job, written by the AVA1 progress bridge and read by the
/// engine when it serves the job snapshot. All values are absolute (stored, never added).
#[derive(Debug, Default)]
pub struct LiveNotes {
    /// AVA1's bottleneck code (`ava1::gen::BN_*`; 0 = none).
    pub bottleneck: std::sync::atomic::AtomicU8,
    /// 0 = sending, `LIVE_PHASE_SKIPPING` = the decoder is discarding data the console has.
    pub phase: std::sync::atomic::AtomicU8,
    pub skip_done_bytes: std::sync::atomic::AtomicU64,
    pub skip_total_bytes: std::sync::atomic::AtomicU64,
    /// Files are still settling on the console after the job finished.
    pub settling: std::sync::atomic::AtomicBool,
    /// The files the console still reports unswept while `settling` (0 outside it), and the
    /// most it reported: the numerator and denominator of "Finishing on the console: N left".
    pub unswept: std::sync::atomic::AtomicU32,
    pub unswept_peak: std::sync::atomic::AtomicU32,
    /// The job's telemetry (where its time went, the console's own end-of-job line), as the
    /// transport last reported it. The engine writes it into the per-job record when the job
    /// ends (review 009 #4).
    pub telemetry: std::sync::Mutex<Option<serde_json::Value>>,
}

pub const LIVE_PHASE_SKIPPING: u8 = 1;

/// An entry's name, reading UTF-8 even when the zip doesn't say so.
///
/// The spec says a name without the UTF-8 flag (general-purpose bit 11) is
/// CP437, and the `zip` crate decodes it that way. But zips made by macOS,
/// Linux and most tools write UTF-8 without setting the flag, so a file
/// called `名前 é.txt` landed on the PS5 as `σÉìσëì ├⌐.txt`. Like Info-ZIP
/// and 7-Zip, use the raw bytes when they are valid UTF-8 and fall back to
/// the crate's CP437 decoding only when they are not.
pub(crate) fn zip_entry_name(raw: &[u8], decoded: &str) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => decoded.to_string(),
    }
}

/// Sanitize a zip entry name into a safe POSIX-relative path (forward
/// slashes for the PS5). Rejects traversal (`..`), NUL, backslash segments,
/// and absolute/empty paths — the same zip-slip defense as the client's
/// `save_archive::sanitize_entry`, returning a string instead of a host
/// `PathBuf`. Returns `None` for directory-only or unsafe names.
pub(crate) fn sanitize_zip_entry(name: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        if seg.contains('\\') || seg.contains('\0') {
            return None;
        }
        parts.push(seg);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Lightweight preview of a `.zip` for the Upload screen: how much it expands
/// to, how many files, and the game it contains (if it carries a
/// `sce_sys/param.json`). Reads only the central directory plus, at most, one
/// small `param.json` — never inflates the bulk of the archive.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZipInspect {
    /// Number of extractable file entries (directories excluded).
    pub file_count: u64,
    /// Sum of uncompressed sizes — what lands on the PS5.
    pub total_uncompressed: u64,
    /// Size of the `.zip` on disk — what the user is storing.
    pub compressed_size: u64,
    /// Game title from an embedded `sce_sys/param.json`, if any.
    pub title: Option<String>,
    /// Title ID, e.g. "PPSA00000".
    pub title_id: Option<String>,
    /// Content ID, e.g. "EP0000-PPSA00000_00-…".
    pub content_id: Option<String>,
    /// `applicationCategoryType` (0 = game).
    pub application_category_type: Option<i64>,
    /// The path *inside the zip* that contains `sce_sys/` (the game root),
    /// e.g. "MyGame" for `MyGame/sce_sys/param.json`, or "" if param.json is
    /// at the archive root. `None` when no game metadata was found.
    pub game_root: Option<String>,
}

/// Inspect a `.zip` without extracting it. Walks the central directory for
/// counts/sizes and, if it finds the shallowest `sce_sys/param.json`, parses
/// it in memory for game metadata.
pub fn inspect_zip(zip_path: &Path) -> Result<ZipInspect> {
    inspect_zip_with_progress(zip_path, |_| {})
}

/// `inspect_zip` variant that calls `on_progress(entries_seen)` every
/// 1,000 entries during the central-directory walk so streaming HTTP
/// handlers can emit watchdog-resetting heartbeats. The progress is
/// proportional to entry count, not bytes — the bytes side is bounded
/// by central-directory size which is small. Used by
/// `zip_inspect_handler`'s NDJSON streaming response.
pub fn inspect_zip_with_progress(
    zip_path: &Path,
    on_progress: impl FnMut(u64),
) -> Result<ZipInspect> {
    let compressed_size = std::fs::metadata(zip_path)
        .with_context(|| format!("stat zip {}", zip_path.display()))?
        .len();

    // Read the central directory via our own zero-seek parser
    // (`zip_cd`) instead of `zip::ZipArchive::new`. We get per-entry
    // (name, uncompressed_size, is_dir) for free, so total_uncompressed
    // is computed by direct summation instead of going through
    // `archive.decompressed_size()` — which returned `None` on any
    // archive with general-purpose bit 3 set (bsdtar / libarchive
    // streaming zippers; see the "Windows zip bit-3 trap" lesson). With
    // direct summation we surface the real extracted size in the
    // Upload card on those archives too, instead of the misleading
    // count-only display.
    let entries = crate::zip_cd::read_central_directory_with_progress(zip_path, on_progress)
        .with_context(|| format!("read zip central directory {}", zip_path.display()))?;

    let mut file_count = 0u64;
    let mut total_uncompressed = 0u64;
    // Find the shallowest "<root>/sce_sys/param.json" (fewest path
    // segments) so a wrapped dump (`MyGame/sce_sys/…`) and a root dump
    // (`sce_sys/…`) both resolve to the real game root. Keep the entry's
    // ORIGINAL name so we can look it up by-name for the single
    // content read below.
    let mut param_hit: Option<(usize, String, String)> = None; // (depth, original_name, game_root)
    for entry in &entries {
        // Directory entries (trailing '/') don't count as files —
        // matches the old behaviour. sanitize_zip_entry keeps "foo/"
        // as "foo", so we must filter dirs by name first.
        if entry.is_dir {
            continue;
        }
        let Some(rel) = sanitize_zip_entry(&entry.name) else {
            continue;
        };
        file_count += 1;
        total_uncompressed = total_uncompressed.saturating_add(entry.uncompressed_size);
        if let Some(root) = rel.strip_suffix("sce_sys/param.json") {
            let game_root = root.trim_end_matches('/').to_string();
            let depth = rel.split('/').count();
            if param_hit.as_ref().is_none_or(|(d, _, _)| depth < *d) {
                param_hit = Some((depth, entry.name.clone(), game_root));
            }
        }
    }
    drop(entries);

    let mut inspect = ZipInspect {
        file_count,
        total_uncompressed,
        compressed_size,
        title: None,
        title_id: None,
        content_id: None,
        application_category_type: None,
        game_root: None,
    };

    // Only open the zip-crate archive when we actually need to inflate
    // param.json — most game dumps have one, but skipping the open for
    // archives without one saves a redundant central-directory parse on
    // top of our own.
    if let Some((_, original_name, game_root)) = param_hit {
        use std::io::Read;
        let file = std::fs::File::open(zip_path)
            .with_context(|| format!("re-open zip for param.json {}", zip_path.display()))?;
        if let Ok(mut archive) = zip::ZipArchive::new(std::io::BufReader::new(file)) {
            // Cap the inflate: a real param.json is a few KiB, but
            // inspect_zip runs on user-supplied archives just to render
            // the Upload preview, so a crafted entry named
            // sce_sys/param.json that decompresses to gigabytes (zip
            // bomb) must not OOM the engine before any upload even
            // starts. `take` bounds the read.
            //
            // Everything here is non-fatal: a param.json that's itself
            // in an unsupported method, won't fit in 4 MiB, or doesn't
            // parse just falls through to the size/count-only preview —
            // the transfer path surfaces any real compression error.
            const MAX_PARAM_JSON: u64 = 4 * 1024 * 1024;
            if let Ok(zf) = archive.by_name(&original_name) {
                let mut bytes = Vec::new();
                if zf.take(MAX_PARAM_JSON).read_to_end(&mut bytes).is_ok() {
                    if let Ok(meta) = crate::game_meta::parse_param_json_bytes(&bytes) {
                        inspect.title = meta.title;
                        inspect.title_id = meta.title_id;
                        inspect.content_id = meta.content_id;
                        inspect.application_category_type = meta.application_category_type;
                        inspect.game_root = Some(game_root);
                    }
                }
            }
        }
    }

    Ok(inspect)
}

/// Engine-facing preview of what a zip transfer will send: total uncompressed
/// bytes (the progress-bar denominator) and a sorted `(rel_path, size)` list
/// (the UI file tree). Applies the same sanitize + excludes the transfer does,
/// reading only the central directory. Lets the HTTP engine render a zip job
/// without taking its own dependency on the `zip` crate.
///
/// Backed by `zip_cd::read_central_directory` rather than the `zip`
/// crate's `by_index_raw`: the crate API forces a per-entry seek
/// (`find_content`) to return metadata that already lives in memory.
/// On a 6.7 GB / 1,548-entry zip from cold-cache exFAT external the
/// old path took 17 s (one seek per entry); the new path reads the
/// EOCD + central directory in two bulk reads and parses linearly
/// (~ms). For huge dumps (100k+ tiny files) the difference is even
/// larger — the per-entry-seek path would have blown the Tauri client's
/// 60 s deadline (the reported "engine request failed" symptom).
pub fn zip_plan_preview(zip_path: &Path, excludes: &[String]) -> Result<(u64, Vec<(String, u64)>)> {
    zip_plan_preview_with_progress(zip_path, excludes, |_| {})
}

/// `zip_plan_preview` variant that forwards a per-N-entries callback so
/// a streaming HTTP handler can emit progress heartbeats while the
/// central directory is parsed. The walk itself is fast once the bulk
/// read lands; the callback exists purely so a client-side watchdog has
/// a continuous "engine still alive" signal during cold-cache reads of
/// the central directory itself.
pub fn zip_plan_preview_with_progress(
    zip_path: &Path,
    excludes: &[String],
    on_progress: impl FnMut(u64),
) -> Result<(u64, Vec<(String, u64)>)> {
    let entries = crate::zip_cd::read_central_directory_with_progress(zip_path, on_progress)
        .with_context(|| format!("read zip central directory {}", zip_path.display()))?;
    let mut files: Vec<(String, u64)> = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let Some(rel) = sanitize_zip_entry(&entry.name) else {
            continue;
        };
        if !excludes.is_empty() && crate::excludes::is_excluded_strings(Path::new(&rel), excludes) {
            continue;
        }
        files.push((rel, entry.uncompressed_size));
    }
    // Collapse duplicate paths keeping the last (the AVA1 zip
    // source's manifest rule) so the preview's total + file count match
    // exactly what the transfer sends — otherwise the progress bar's
    // denominator would exceed the bytes actually streamed.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut deduped: Vec<(String, u64)> = Vec::with_capacity(files.len());
    for f in files {
        if deduped.last().is_some_and(|(p, _)| *p == f.0) {
            deduped.pop();
        }
        deduped.push(f);
    }
    let total = deduped.iter().map(|(_, s)| *s).sum();
    Ok((total, deduped))
}

// ═══════════════════════════════════════════════════════════════════════════
// .7z archive support
//
// 7z's LZMA2 streams cannot be seeked, so an entry's bytes are read sequentially by the AVA1
// sequential source (`ps5upload-ava1/src/seq.rs`). Inspecting reads only the (tiny) header
// for counts and sizes, so it is instant even on a 124 GB archive; game metadata is not read
// (a 7z of an .exfat has no host-visible param.json), so `title` stays `None`.
// ═══════════════════════════════════════════════════════════════════════════

/// Normalise + validate a 7z entry path. Same zip-slip rules as
/// `sanitize_zip_entry`, except 7z archives created on Windows legitimately use
/// '\\' as the path separator (zip always uses '/'), so backslashes are
/// translated to forward slashes rather than rejected.
pub fn sanitize_7z_entry(name: &str) -> Option<String> {
    sanitize_zip_entry(&name.replace('\\', "/"))
}

/// The refusal for a 7z whose solid block has stream-less entries (directories, empty
/// files) between its streamed files. The `sevenz-rust2` per-block walk covers fewer
/// entries than such a block spans and would silently drop the last files, so the
/// archive is refused rather than partly uploaded. Shared by the AVA1 sources and the plan previews.
pub const SEVENZ_LAYOUT_UNSUPPORTED: &str =
    "this 7z archive's layout is not supported (a solid block has directories or empty files between its files); re-pack it with 7-Zip or extract it first";

/// `Err` with [`SEVENZ_LAYOUT_UNSUPPORTED`] when any stream-less entry sits inside a
/// block (before the block's last streamed file). Trailing and leading stream-less
/// entries are not in a block and are fine.
pub fn sevenz_check_layout(archive: &sevenz_rust2::Archive) -> Result<()> {
    let fbi = &archive.stream_map.file_block_index;
    let bad = archive
        .files
        .iter()
        .enumerate()
        .any(|(i, e)| !e.has_stream() && fbi.get(i).copied().flatten().is_some());
    if bad {
        bail!("{SEVENZ_LAYOUT_UNSUPPORTED}");
    }
    Ok(())
}

/// Pick the LZMA2 worker-thread count. Defaults to 1 — multi-threaded decode
/// is opt-in via `PS5UPLOAD_7Z_THREADS` because it trades a bounded streaming
/// footprint for one proportional to the whole archive.
///
/// Measured on a 2.5 GB corpus (sevenz-rust2 0.22.2 / lzma-rust2 0.20.1),
/// peak RSS for the decode alone:
///
/// | archive packed with | threads=1 | threads>1        |
/// |---------------------|-----------|------------------|
/// | `-mmt=8` (MT LZMA2) | 74 MiB    | 2.3 GiB, +24%    |
/// | `-mmt=1` (solid)    | 74 MiB    | 4.4 GiB, +0%     |
///
/// The reason is structural: `Lzma2ReaderMt` splits work on LZMA2
/// dictionary-reset chunks (control byte `>= 0xE0` or `== 0x01`) and buffers
/// each unit's *decompressed* output in a `Vec<u8>`. A solid stream contains a
/// single dict reset, so the one work unit is the entire archive — the reader
/// stops streaming and materialises everything in RAM. Halving a 205 GB game
/// dump's transfer time is worthless if the engine is OOM-killed first, and
/// Docker hosts and Android have the tightest budgets of anyone.
///
/// So: honour an explicit opt-in (clamped to a sane range), otherwise stay on
/// the single-threaded path that keeps peak RAM at one LZMA2 window.
fn select_sevenz_decode_threads(configured: Option<&str>) -> u32 {
    const HARD_MAX: usize = 16;

    configured
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, HARD_MAX))
        .unwrap_or(1) as u32
}

pub fn sevenz_decode_threads() -> u32 {
    select_sevenz_decode_threads(std::env::var("PS5UPLOAD_7Z_THREADS").ok().as_deref())
}

/// Reconstruct the exact order `ArchiveReader::for_each_entries` visits files:
/// every file that belongs to a block (ordered by block index, then file
/// index), followed by every block-less file (empties / directories). This
/// matches `BlockDecoder`'s `start..start+count` ascending walk plus the
/// trailing empty-file loop, using only public `Archive` fields so we never
/// touch crate internals. O(n log n), so a non-solid archive with one block
/// per file (n blocks) stays cheap.
fn sevenz_visit_order(archive: &sevenz_rust2::Archive) -> Vec<usize> {
    let fbi = &archive.stream_map.file_block_index;
    let mut order: Vec<usize> = (0..archive.files.len()).collect();
    order.sort_by_key(|&fi| match fbi.get(fi).copied().flatten() {
        // Block files first, grouped by block index then ascending file index.
        Some(block) => (0u8, block, fi),
        // Block-less files (empties, directories) last, ascending file index.
        None => (1u8, usize::MAX, fi),
    });
    order
}

/// Inspect a `.7z` without extracting it. Reads only the (tiny) header for
/// counts + sizes — instant even on a 124 GB archive. Game metadata is left
/// `None` (see the module note); the Upload card renders fine without it.
pub fn inspect_7z(archive_path: &Path) -> Result<ZipInspect> {
    inspect_7z_with_progress(archive_path, |_| {})
}

/// `inspect_7z` variant that calls `on_progress(files_seen)` so streaming HTTP
/// handlers can emit watchdog-resetting heartbeats while a many-file header is
/// walked.
pub fn inspect_7z_with_progress(
    archive_path: &Path,
    mut on_progress: impl FnMut(u64),
) -> Result<ZipInspect> {
    let compressed_size = std::fs::metadata(archive_path)
        .with_context(|| format!("stat 7z {}", archive_path.display()))?
        .len();
    let mut src = std::io::BufReader::new(
        std::fs::File::open(archive_path)
            .with_context(|| format!("open 7z {}", archive_path.display()))?,
    );
    let pw = sevenz_rust2::Password::from("");
    let archive = sevenz_rust2::Archive::read(&mut src, &pw)
        .map_err(|e| anyhow::anyhow!("read 7z header {}: {e}", archive_path.display()))?;

    let mut file_count = 0u64;
    let mut total_uncompressed = 0u64;
    for (i, e) in archive.files.iter().enumerate() {
        if e.is_directory() {
            continue;
        }
        file_count += 1;
        total_uncompressed += e.size();
        if i.is_multiple_of(1000) {
            on_progress(file_count);
        }
    }
    on_progress(file_count); // always emit a final tick

    Ok(ZipInspect {
        file_count,
        total_uncompressed,
        compressed_size,
        title: None,
        title_id: None,
        content_id: None,
        application_category_type: None,
        game_root: None,
    })
}

/// Metadata-only plan preview for the HTTP handler's synchronous pre-flight:
/// total file-data bytes + the sanitised dest paths (sorted) for the live file
/// tree. No decompression.
pub fn sevenz_plan_preview(
    archive_path: &Path,
    excludes: &[String],
) -> Result<(u64, Vec<(String, u64)>)> {
    sevenz_plan_preview_with_progress(archive_path, excludes, |_| {})
}

/// `sevenz_plan_preview` with a per-N-files progress callback.
pub fn sevenz_plan_preview_with_progress(
    archive_path: &Path,
    excludes: &[String],
    mut on_progress: impl FnMut(u64),
) -> Result<(u64, Vec<(String, u64)>)> {
    let mut src = std::io::BufReader::new(
        std::fs::File::open(archive_path)
            .with_context(|| format!("open 7z {}", archive_path.display()))?,
    );
    let pw = sevenz_rust2::Password::from("");
    let archive = sevenz_rust2::Archive::read(&mut src, &pw)
        .map_err(|e| anyhow::anyhow!("read 7z header {}: {e}", archive_path.display()))?;
    sevenz_check_layout(&archive)?;

    let mut total = 0u64;
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut seen = 0u64;
    for &fi in &sevenz_visit_order(&archive) {
        let e = &archive.files[fi];
        if e.is_directory() {
            continue;
        }
        let Some(rel) = sanitize_7z_entry(e.name()) else {
            bail!(
                "7z contains an unsafe or invalid entry path: {:?}",
                e.name()
            );
        };
        if !excludes.is_empty() && crate::excludes::is_excluded_strings(Path::new(&rel), excludes) {
            continue;
        }
        total += e.size();
        files.push((rel, e.size()));
        seen += 1;
        if seen.is_multiple_of(1000) {
            on_progress(seen);
        }
    }
    on_progress(seen);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((total, files))
}

// ═══════════════════════════════════════════════════════════════════════════
//  .rar support  (desktop-only)
//
//  Real RAR support needs the UnRAR C++ source (the `unrar` crate): there is no
//  production-grade pure-Rust RAR decoder, and only UnRAR covers RAR5 +
//  multi-volume + AES passwords. That's a C dependency, which would break the
//  Android pure-Rust cross-compile the zip/7z pins guard, so this whole module
//  is compiled out on Android (Android keeps zip/7z only).
//
//  This module inspects and walks an archive: `inspect_rar` and `rar_plan_preview`
//  for the Upload card, `rar_layout` and `rar_walk` for the AVA1 RAR source
//  (`ps5upload-ava1/src/rar_source.rs`), which decodes the archive forward on one
//  thread and streams each entry to the console as it is produced (no host staging
//  directory). Multi-volume sets are opened from the first volume (UnRAR pulls in
//  the siblings automatically); a password flows in but is never logged or
//  persisted.
//
//  REQUIRED UnRAR NOTICE (UnRAR license, paragraph 2 — reproduced verbatim, as
//  the license mandates it appear "in source code comments of resulting
//  package"):
//    UnRAR source code may be used in any software to handle RAR archives
//    without limitations free of charge, but cannot be used to develop RAR
//    (WinRAR) compatible archiver and to re-create RAR compression algorithm,
//    which is proprietary. Distribution of modified UnRAR source code in
//    separate form or as a part of other software is permitted, provided that
//    full text of this paragraph, starting from "UnRAR source code" words, is
//    included in license, or in documentation if license is not available, and
//    in source code comments of resulting package.
//  ps5upload uses UnRAR ONLY to extract; it never compresses RAR. GPLv3 §7
//  linking exception + the full UnRAR license: see LICENSES/UnRAR-exception.md
//  and LICENSES/UnRAR-license.txt.
// ═══════════════════════════════════════════════════════════════════════════
#[cfg(not(target_os = "android"))]
pub use rar_support::{inspect_rar, rar_plan_preview};
#[cfg(not(target_os = "android"))]
pub(crate) use rar_support::{rar_dirs, rar_plan_entries, spawn_rar_worker};
#[cfg(not(target_os = "android"))]
pub use rar_support::{rar_layout, rar_walk, RarFailKind, RarLayout, RarWalkError, RarWalkSink};

#[cfg(not(target_os = "android"))]
mod rar_support {
    use super::*;
    use unrar::error::{Code as RarCode, UnrarError};
    use unrar::{Archive, FileHeader, StreamSink};

    /// Map UnRAR errors to stable, UI-detectable strings for the two cases the
    /// UI must react to (prompt for / re-prompt the password); everything else
    /// passes through verbatim with context.
    fn map_rar_err(ctx: &str, e: UnrarError) -> anyhow::Error {
        match e.code {
            RarCode::MissingPassword => anyhow::anyhow!("rar_password_required"),
            RarCode::BadPassword => anyhow::anyhow!("rar_password_wrong"),
            _ => anyhow::anyhow!("{ctx}: {e}"),
        }
    }

    /// Same, but first check whether a volume of the set is simply absent.
    ///
    /// UnRAR reports a missing sibling as a generic open failure, which the
    /// UI turned into "select the FIRST part and keep every volume in one
    /// folder" — useless to someone who had done both. Checking the set
    /// ourselves lets us name the file that is actually missing.
    fn map_rar_open_err(path: &str, ctx: &str, e: UnrarError) -> anyhow::Error {
        if !matches!(e.code, RarCode::MissingPassword | RarCode::BadPassword) {
            let on_disk = |p: &str| std::path::Path::new(p).exists();
            if let Some(missing) = missing_volume(path, &on_disk) {
                return anyhow::anyhow!("rar_missing_volume: {missing}");
            }
        }
        map_rar_err(ctx, e)
    }

    /// Name the first missing volume of a multi-part set, if one is missing.
    ///
    /// UnRAR opens siblings itself and, when one is absent, fails with a
    /// generic open error. The UI then showed "select the FIRST part and
    /// make sure every volume is in the same folder" — advice a user who
    /// had already done both could not act on (reported with a screenshot
    /// showing exactly that). Naming the missing file turns it into
    /// something they can fix.
    ///
    /// Handles both schemes in the wild:
    ///   name.part1.rar / name.part2.rar …  (any digit width, 1-based)
    ///   name.rar / name.r00 / name.r01 …   (older split scheme)
    ///
    /// `exists` is injected so the scan is testable without a filesystem.
    pub(crate) fn missing_volume(path: &str, exists: &dyn Fn(&str) -> bool) -> Option<String> {
        let (dir, file) = match path.rfind(['/', '\\']) {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => ("", path),
        };
        let lower = file.to_ascii_lowercase();

        // .partN.rar — walk forward until a volume is absent, and only
        // report a gap if a LATER volume exists (otherwise we are simply
        // past the end of the set).
        if let Some(pos) = lower.rfind(".part") {
            let rest = &lower[pos + 5..];
            if let Some(dot) = rest.find(".rar") {
                let digits = &rest[..dot];
                if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                    let width = digits.len();
                    let start: u32 = digits.parse().ok()?;
                    let stem = &file[..pos];
                    let name_of = |n: u32| format!("{stem}.part{n:0width$}.rar", width = width);
                    let mut n = start;
                    loop {
                        n += 1;
                        let cand = name_of(n);
                        if exists(&format!("{dir}{cand}")) {
                            continue;
                        }
                        // Absent: a gap only matters if the set continues.
                        for ahead in 1..=3 {
                            if exists(&format!("{dir}{}", name_of(n + ahead))) {
                                return Some(cand);
                            }
                        }
                        return None;
                    }
                }
            }
        }

        // name.rar + name.r00, r01 … — same idea on the older scheme.
        if lower.ends_with(".rar") {
            let stem = &file[..file.len() - 4];
            let name_of = |n: u32| format!("{stem}.r{n:02}");
            if exists(&format!("{dir}{}", name_of(0))) {
                let mut n = 0u32;
                loop {
                    n += 1;
                    let cand = name_of(n);
                    if exists(&format!("{dir}{cand}")) {
                        continue;
                    }
                    for ahead in 1..=3 {
                        if exists(&format!("{dir}{}", name_of(n + ahead))) {
                            return Some(cand);
                        }
                    }
                    return None;
                }
            }
        }
        None
    }

    /// Sanitise a RAR entry path with the same zip-slip rules as 7z (RAR, like
    /// 7z, can use '\\' separators on Windows-created archives).
    fn sanitize_rar_entry(name: &Path) -> Option<String> {
        sanitize_7z_entry(&name.to_string_lossy())
    }

    /// Open the archive's file list (with or without a password) as an iterator
    /// of headers. The returned `OpenArchive` owns its handle, so the borrowed
    /// `path` / `password` only need to live across this call.
    /// Multi-volume note: upstream `unrar 0.5.8` reads ~8 KB out of a much
    /// smaller buffer on every volume transition (a ~7.7 KB out-of-bounds
    /// heap read), which aborts debug builds on any multi-part archive.
    /// The crate is vendored at `third_party/unrar` with that fixed, wired
    /// up through `[patch.crates-io]` — so this is safe to call on a
    /// multi-part set. If you ever bump or un-vendor `unrar`, check the
    /// `UCM_CHANGEVOLUMEW` arm first: see `third_party/unrar/README.md`.
    fn list_headers(
        path: &str,
        password: Option<&str>,
    ) -> std::result::Result<
        impl Iterator<Item = std::result::Result<FileHeader, UnrarError>>,
        UnrarError,
    > {
        match password {
            Some(pw) => Archive::with_password(path, pw).open_for_listing(),
            None => Archive::new(path).open_for_listing(),
        }
    }

    /// Inspect a `.rar` (counts + uncompressed bytes) without extracting. Opens
    /// the first volume; UnRAR spans the rest of the set. `compressed_size` is
    /// the first volume's size only (a rough hint); `total_uncompressed` is the
    /// meaningful figure.
    pub fn inspect_rar(archive_path: &Path, password: Option<&str>) -> Result<ZipInspect> {
        let path_str = archive_path.to_string_lossy().into_owned();
        let compressed_size = std::fs::metadata(archive_path)
            .with_context(|| format!("stat rar {}", archive_path.display()))?
            .len();
        let mut file_count = 0u64;
        let mut total_uncompressed = 0u64;
        // Shallowest "<root>/sce_sys/param.json" wins, so a dump wrapped in an
        // extra folder (`[SITE]-PPSA12345/PPSA12345-app/sce_sys/…`) and a bare
        // one (`sce_sys/…`) both resolve to the real game root. Same rule as
        // the zip path.
        let mut param_hit: Option<(usize, String)> = None; // (depth, game_root)
        for entry in list_headers(&path_str, password)
            .map_err(|e| map_rar_open_err(&path_str, "open rar", e))?
        {
            let e = entry.map_err(|e| map_rar_err("read rar header", e))?;
            if e.is_directory() {
                continue;
            }
            file_count += 1;
            total_uncompressed += e.unpacked_size;
            if let Some(rel) = sanitize_rar_entry(&e.filename) {
                param_hit = better_param_hit(param_hit, &rel);
            }
        }

        let mut inspect = ZipInspect {
            file_count,
            total_uncompressed,
            compressed_size,
            title: None,
            title_id: None,
            content_id: None,
            application_category_type: None,
            game_root: None,
        };

        // Pull the title out of param.json. Without this a .rar upload showed
        // no game name and landed in a folder named after the archive —
        // `/data/homebrew/[DLPSGAME.COM]- 01.021 PPSA23226` in one real
        // report — while the identical .zip resolved a clean title.
        //
        // Entirely best-effort: any failure leaves the size/count-only
        // preview, exactly as before. This runs on user-supplied archives
        // just to render the Upload card, so it must never be able to fail
        // an upload that would otherwise work.
        if let Some((_, game_root)) = param_hit {
            if let Some(meta) = read_param_json(archive_path, password, &game_root) {
                inspect.title = meta.title;
                inspect.title_id = meta.title_id;
                inspect.content_id = meta.content_id;
                inspect.application_category_type = meta.application_category_type;
                inspect.game_root = Some(game_root);
            }
        }

        Ok(inspect)
    }

    /// Fold one entry path into the running "best param.json" choice.
    ///
    /// Shallowest wins, so a dump wrapped in an extra folder
    /// (`[SITE]-PPSA12345/PPSA12345-app/sce_sys/param.json`) and a bare one
    /// (`sce_sys/param.json`) both resolve to the real game root, and a
    /// nested DLC or update folder deeper in the tree cannot outrank the
    /// base game.
    pub(crate) fn better_param_hit(
        current: Option<(usize, String)>,
        rel: &str,
    ) -> Option<(usize, String)> {
        let Some(root) = rel.strip_suffix("sce_sys/param.json") else {
            return current;
        };
        // Guard against a file merely *ending* in that text, e.g.
        // "notsce_sys/param.json" — the boundary must be a real path
        // separator or the very start of the path.
        if !(root.is_empty() || root.ends_with('/')) {
            return current;
        }
        let depth = rel.split('/').count();
        match &current {
            Some((d, _)) if *d <= depth => current,
            _ => Some((depth, root.trim_end_matches('/').to_string())),
        }
    }

    /// Extract just `<game_root>/sce_sys/param.json` and parse it.
    ///
    /// Stops at the entry rather than walking the whole set: on a nine-volume
    /// archive there is no reason to cross eight volume boundaries to read a
    /// few KB. Returns `None` on anything unexpected — callers treat metadata
    /// as a nicety, never a precondition.
    fn read_param_json(
        archive_path: &Path,
        password: Option<&str>,
        game_root: &str,
    ) -> Option<crate::game_meta::FolderInspectResult> {
        // A real param.json is a few KB. Refuse a huge one rather than write
        // it to disk: this runs on untrusted archives before any upload.
        const MAX_PARAM_JSON: u64 = 4 * 1024 * 1024;

        let want = if game_root.is_empty() {
            "sce_sys/param.json".to_string()
        } else {
            format!("{game_root}/sce_sys/param.json")
        };

        let tmp = std::env::temp_dir().join(format!(".ps5upload-param-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).ok()?;

        let result = (|| -> Option<crate::game_meta::FolderInspectResult> {
            let path_str = archive_path.to_string_lossy().into_owned();
            let mut open = match password {
                Some(pw) => Archive::with_password(&path_str, pw).open_for_processing(),
                None => Archive::new(&path_str).open_for_processing(),
            }
            .ok()?;

            loop {
                let header = open.read_header().ok()??;
                let name = header.entry().filename.clone();
                let matches = sanitize_rar_entry(&name).as_deref() == Some(want.as_str());
                if !matches || header.entry().unpacked_size > MAX_PARAM_JSON {
                    open = header.skip().ok()?;
                    continue;
                }
                let dest = tmp.join("param.json");
                let _ = header.extract_to(&dest).ok()?;
                let bytes = std::fs::read(&dest).ok()?;
                return crate::game_meta::parse_param_json_bytes(&bytes).ok();
            }
        })();

        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    /// Metadata-only plan preview: total bytes + sanitised dest paths (sorted)
    /// for the live file tree. No extraction.
    /// Entries in **archive order** — the order a processing walk produces.
    ///
    /// Keep this distinct from `rar_plan_preview`, which sorts. The streaming
    /// upload builds its manifest from this: the decoder is forward-only, so
    /// the manifest must list entries in the order the decoder produces
    /// them. Feeding it sorted names transposed two adjacent pairs in a real
    /// 181-file archive — `precisionarrow` vs `precision_precisionplus`,
    /// where `_` sorts before a letter.
    /// The archive's directory entries, sanitised: an empty one (a game may
    /// look for it) has no file to create it.
    pub(crate) fn rar_dirs(archive_path: &Path, password: Option<&str>) -> Result<Vec<String>> {
        let path_str = archive_path.to_string_lossy().into_owned();
        let mut out = Vec::new();
        for entry in list_headers(&path_str, password)
            .map_err(|e| map_rar_open_err(&path_str, "open rar", e))?
        {
            let entry = entry.map_err(|e| map_rar_err("read rar header", e))?;
            if entry.is_directory() {
                if let Some(rel) = sanitize_rar_entry(&entry.filename) {
                    out.push(rel);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn rar_plan_entries(
        archive_path: &Path,
        password: Option<&str>,
        excludes: &[String],
    ) -> Result<(u64, Vec<(String, u64)>)> {
        let path_str = archive_path.to_string_lossy().into_owned();
        let mut total = 0u64;
        let mut files: Vec<(String, u64)> = Vec::new();
        for entry in list_headers(&path_str, password)
            .map_err(|e| map_rar_open_err(&path_str, "open rar", e))?
        {
            let e = entry.map_err(|e| map_rar_err("read rar header", e))?;
            if e.is_directory() {
                continue;
            }
            let Some(rel) = sanitize_rar_entry(&e.filename) else {
                bail!(
                    "rar contains an unsafe or invalid entry path: {:?}",
                    e.filename
                );
            };
            if !excludes.is_empty()
                && crate::excludes::is_excluded_strings(Path::new(&rel), excludes)
            {
                continue;
            }
            if !rar_size_unknown(e.unpacked_size) {
                total += e.unpacked_size;
            }
            files.push((rel, e.unpacked_size));
        }
        Ok((total, files))
    }

    /// Entries sorted by path, for showing a human a file list.
    ///
    /// Do NOT use this to build a transfer manifest — see `rar_plan_entries`.
    pub fn rar_plan_preview(
        archive_path: &Path,
        password: Option<&str>,
        excludes: &[String],
    ) -> Result<(u64, Vec<(String, u64)>)> {
        let (total, mut files) = rar_plan_entries(archive_path, password, excludes)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok((total, files))
    }

    /// Walk the archive on a worker thread, pushing entry-framed messages.
    ///
    /// Runs on its own thread because UnRAR pushes bytes at us while the sender
    /// pulls; the bounded channel between them is the backpressure, so
    /// peak memory is a few chunks rather than a whole entry.
    pub(crate) fn spawn_rar_worker(
        archive_path: &Path,
        password: Option<&str>,
        excludes: Vec<String>,
    ) -> (
        std::sync::mpsc::Receiver<crate::rar_stream::StreamMsg>,
        std::thread::JoinHandle<()>,
    ) {
        use crate::rar_stream::StreamMsg;

        let path_str = archive_path.to_string_lossy().into_owned();
        let password = password.map(str::to_string);
        // 4 chunks is enough to keep the sender fed without letting the worker
        // run far ahead of the network.
        let (tx, rx) = std::sync::mpsc::sync_channel::<StreamMsg>(4);

        let handle = std::thread::spawn(move || {
            let fail = |tx: &std::sync::mpsc::SyncSender<StreamMsg>, msg: String| {
                let _ = tx.send(StreamMsg::Failed(msg));
            };

            let opened = match password.as_deref() {
                Some(pw) => Archive::with_password(&path_str, pw).open_for_processing(),
                None => Archive::new(&path_str).open_for_processing(),
            };
            let mut open = match opened {
                Ok(o) => o,
                Err(e) => {
                    fail(
                        &tx,
                        format!("{:#}", map_rar_open_err(&path_str, "open rar", e)),
                    );
                    return;
                }
            };

            loop {
                let header = match open.read_header() {
                    Ok(Some(h)) => h,
                    Ok(None) => {
                        let _ = tx.send(StreamMsg::Finished);
                        return;
                    }
                    Err(e) => {
                        fail(&tx, format!("{:#}", map_rar_err("read rar header", e)));
                        return;
                    }
                };

                let name = header.entry().filename.clone();
                let sanitised = sanitize_rar_entry(&name);
                let skip_this = header.entry().is_directory()
                    || match &sanitised {
                        None => true,
                        Some(rel) => {
                            !excludes.is_empty()
                                && crate::excludes::is_excluded_strings(Path::new(rel), &excludes)
                        }
                    };

                if skip_this {
                    match header.skip() {
                        Ok(next) => {
                            open = next;
                            continue;
                        }
                        Err(e) => {
                            fail(&tx, format!("{:#}", map_rar_err("skip rar entry", e)));
                            return;
                        }
                    }
                }

                let Some(rel) = sanitised else {
                    fail(&tx, format!("rar contains an unsafe entry path: {name:?}"));
                    return;
                };
                if tx.send(StreamMsg::Entry(rel)).is_err() {
                    return; // consumer gone (cancel or error) — unwind quietly
                }

                let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<Box<[u8]>>(4);
                // Forward this entry's chunks on a helper thread: UnRAR's walk
                // blocks inside read_to_sink, so something else has to move
                // bytes onto the framed channel or the two capacities deadlock.
                let fwd_tx = tx.clone();
                let fwd = std::thread::spawn(move || {
                    for c in chunk_rx {
                        if fwd_tx.send(StreamMsg::Chunk(c)).is_err() {
                            return false;
                        }
                    }
                    true
                });

                // UnRAR reports a wrong password on a content-encrypted entry
                // as a CRC error; with a password given, that is what it means.
                let encrypted = header.entry().is_encrypted();
                let sink = StreamSink::new(chunk_tx);
                let result = header.read_to_sink(sink);

                match result {
                    Ok((sink, next)) => {
                        // Drop the sink BEFORE joining: it still owns the
                        // chunk sender, and the forwarder's `for c in
                        // chunk_rx` only ends when every sender is gone.
                        // Joining first deadlocks.
                        let disconnected = sink.disconnected();
                        drop(sink);
                        let alive = fwd.join().unwrap_or(false);
                        if disconnected || !alive {
                            return; // consumer went away
                        }
                        if tx.send(StreamMsg::EntryEnd).is_err() {
                            return;
                        }
                        open = next;
                    }
                    Err(e) => {
                        // The sink was consumed (and its sender dropped)
                        // inside the failed call, so the forwarder can finish.
                        let _ = fwd.join();
                        if encrypted && password.is_some() && e.code == RarCode::BadData {
                            fail(&tx, "rar_password_wrong".to_string());
                        } else {
                            fail(&tx, format!("{:#}", map_rar_err("extract rar entry", e)));
                        }
                        return;
                    }
                }
            }
        });

        (rx, handle)
    }

    /// What an archive holds, in archive order, for the AVA1 source.
    #[derive(Debug, Clone)]
    pub struct RarLayout {
        /// Extractable files (sanitised path, unpacked size), archive order.
        pub files: Vec<(String, u64)>,
        /// Each file's own last-modified time, Unix seconds (0 = none), parallel to
        /// `files`. UnRAR hands it over as a packed DOS time in the host's local
        /// zone and 2 s resolution; [`dos_local_to_unix`] undoes that.
        pub mtimes: Vec<u64>,
        /// Directory entries, sanitised.
        pub dirs: Vec<String>,
        /// A solid archive cannot skip an entry without decoding it.
        pub solid: bool,
    }

    /// One header pass: files, dirs and the solid flag. Never decodes.
    pub fn rar_layout(
        archive_path: &Path,
        password: Option<&str>,
        excludes: &[String],
    ) -> Result<RarLayout> {
        let path_str = archive_path.to_string_lossy().into_owned();
        let opened = match password {
            Some(pw) => Archive::with_password(&path_str, pw).open_for_listing(),
            None => Archive::new(&path_str).open_for_listing(),
        }
        .map_err(|e| map_rar_open_err(&path_str, "open rar", e))?;
        let solid = opened.is_solid();
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        let mut mtimes: Vec<u64> = Vec::new();
        for entry in opened {
            let e = entry.map_err(|e| map_rar_err("read rar header", e))?;
            let Some(rel) = sanitize_rar_entry(&e.filename) else {
                bail!(
                    "rar contains an unsafe or invalid entry path: {:?}",
                    e.filename
                );
            };
            if e.is_directory() {
                dirs.push(rel);
                continue;
            }
            if !excludes.is_empty()
                && crate::excludes::is_excluded_strings(Path::new(&rel), excludes)
            {
                continue;
            }
            files.push((rel, e.unpacked_size));
            mtimes.push(dos_local_to_unix(e.file_time));
        }
        if files.iter().any(|(_, s)| rar_size_unknown(*s)) {
            measure_unknown_sizes(&path_str, password, solid, &mut files)?;
        }
        Ok(RarLayout {
            files,
            mtimes,
            dirs,
            solid,
        })
    }

    /// UnRAR's packed DOS time (the host's local zone) as Unix seconds; 0 when absent.
    pub fn dos_local_to_unix(t: u32) -> u64 {
        if t == 0 {
            return 0;
        }
        let f = |sh: u32, m: u32| ((t >> sh) & m) as i64;
        let (y, mo, d) = (1980 + f(25, 0x7f), f(21, 0xf), f(16, 0x1f));
        let (h, mi, s) = (f(11, 0x1f), f(5, 0x3f), f(0, 0x1f) * 2);
        #[cfg(unix)]
        {
            // SAFETY: `tm` is fully initialised below and mktime only reads/normalises it.
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            tm.tm_year = (y - 1900) as _;
            tm.tm_mon = (mo.max(1) - 1) as _;
            tm.tm_mday = d.max(1) as _;
            tm.tm_hour = h as _;
            tm.tm_min = mi as _;
            tm.tm_sec = s as _;
            tm.tm_isdst = -1;
            let t = u64::try_from(unsafe { libc::mktime(&mut tm) }).unwrap_or(0);
            // An entry with no time at all comes back from UnRAR as a garbage DOS value
            // (the header's raw time is not exposed by the crate), so a stamp in the
            // future is read as "none". A real archive dated tomorrow loses its mtime.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            if t > now + 86_400 {
                0
            } else {
                t
            }
        }
        #[cfg(not(unix))]
        {
            // No libc zone lookup here: read the stamp as UTC (off by the host's offset).
            let _ = (y, mo, d, h, mi, s);
            0
        }
    }

    /// UnRAR reports a header whose unpacked size is unknown (a RAR5 flag, written
    /// by streaming archivers) as a huge sentinel (`INT64NDF`, about 2^63), never as
    /// a real size.
    pub(crate) fn rar_size_unknown(size: u64) -> bool {
        size >= 1 << 62
    }

    /// Learn the real size of every unknown-size entry by decoding it once (the
    /// stream's own end and CRC define it). Decodes the whole archive when it is
    /// solid; a non-solid archive only decodes the unknown entries.
    fn measure_unknown_sizes(
        path_str: &str,
        password: Option<&str>,
        solid: bool,
        files: &mut [(String, u64)],
    ) -> Result<()> {
        let mut open = match password {
            Some(pw) => Archive::with_password(path_str, pw).open_for_processing(),
            None => Archive::new(path_str).open_for_processing(),
        }
        .map_err(|e| map_rar_open_err(path_str, "open rar", e))?;
        {
            // A long silent "planning" phase must be explainable (review L7). Ignores a
            // closed stderr rather than panicking like eprintln!.
            use std::io::Write as _;
            let n = files.iter().filter(|(_, s)| rar_size_unknown(*s)).count();
            let _ = writeln!(
                std::io::stderr(),
                "rar plan: {n} entries have an unknown size; {} to measure them",
                if solid {
                    "decoding the whole solid archive"
                } else {
                    "decoding just those entries"
                }
            );
        }
        let mut idx = 0usize; // next file in header order (excluded files included)
                              // `files` holds only non-excluded files, so match by path.
        let wanted: std::collections::HashMap<String, usize> = files
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| rar_size_unknown(*s))
            .map(|(i, (p, _))| (p.clone(), i))
            .collect();
        let mut left = wanted.len();
        while left > 0 {
            let Some(header) = open
                .read_header()
                .map_err(|e| map_rar_err("read rar header", e))?
            else {
                break;
            };
            let target = sanitize_rar_entry(&header.entry().filename)
                .filter(|_| !header.entry().is_directory())
                .and_then(|r| wanted.get(&r).copied());
            let encrypted = header.entry().is_encrypted();
            match target {
                Some(i) => {
                    let mut n = 0u64;
                    open = header
                        .read_to_fn(&mut |b: &[u8]| {
                            n += b.len() as u64;
                            true
                        })
                        .map_err(|e| {
                            map_rar_err("measure rar entry", e)
                                .context(format!("encrypted={}", encrypted && password.is_some()))
                        })?;
                    files[i].1 = n;
                    left -= 1;
                }
                None if solid && !header.entry().is_directory() => {
                    open = header
                        .read_to_fn(&mut |_: &[u8]| true)
                        .map_err(|e| map_rar_err("measure rar entry", e))?;
                }
                None => {
                    open = header
                        .skip()
                        .map_err(|e| map_rar_err("skip rar entry", e))?;
                }
            }
            idx += 1;
        }
        let _ = idx;
        Ok(())
    }

    /// Why a RAR could not be read, in terms a caller can act on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RarFailKind {
        PasswordRequired,
        PasswordWrong,
        /// Damaged data: a bad CRC, a truncated stream, a broken header.
        Corrupt,
        MissingVolume,
        Other,
    }

    #[derive(Debug)]
    pub enum RarWalkError {
        /// `cancel` was raised.
        Cancelled,
        /// The sink refused a call; its error, unchanged.
        Sink(std::io::Error),
        Failed {
            kind: RarFailKind,
            message: String,
        },
    }

    /// Receives the entries a [`rar_walk`] delivers.
    pub trait RarWalkSink {
        /// Called for every file entry in header order, including those before
        /// `start`; false stops the walk (the caller reports why).
        fn visit(&mut self, ordinal: u64, path: &str) -> bool;
        /// Whether to deliver the entry at archive-order file ordinal `ordinal`
        /// (false: skip it; a non-solid archive does not decode it).
        fn want(&mut self, ordinal: u64, path: &str, size: u64) -> bool;
        fn begin(&mut self, path: &str) -> std::io::Result<()>;
        fn data(&mut self, bytes: &[u8]) -> std::io::Result<()>;
        fn end(&mut self) -> std::io::Result<()>;
    }

    fn classify(e: &UnrarError, encrypted_with_password: bool) -> RarFailKind {
        match e.code {
            RarCode::MissingPassword => RarFailKind::PasswordRequired,
            RarCode::BadPassword => RarFailKind::PasswordWrong,
            // UnRAR reports a wrong password on a content-encrypted entry as a CRC
            // error (BadData) when the archive carries no password-check value;
            // with one it answers BadPassword instead, which is matched above and
            // is exact. So this arm is a heuristic only for archives without a
            // check value (old RAR4-style or stripped RAR5): a genuinely corrupt
            // encrypted entry there is indistinguishable from a wrong password
            // (the header CRC cannot tell them apart without the key). The message
            // `rar_password_wrong` is the safer prompt: the user retries the
            // password, and a truly damaged archive fails again, typed.
            RarCode::BadData if encrypted_with_password => RarFailKind::PasswordWrong,
            RarCode::BadData
            | RarCode::BadArchive
            | RarCode::UnknownFormat
            | RarCode::ERead
            | RarCode::EReference => RarFailKind::Corrupt,
            _ => RarFailKind::Other,
        }
    }

    fn walk_fail(path: &str, ctx: &str, e: UnrarError, enc_pw: bool) -> RarWalkError {
        let mut kind = classify(&e, enc_pw);
        let message = if kind == RarFailKind::Other {
            // A missing volume surfaces as a generic open failure.
            let on_disk = |p: &str| std::path::Path::new(p).exists();
            if let Some(missing) = missing_volume(path, &on_disk) {
                kind = RarFailKind::MissingVolume;
                format!("rar_missing_volume: {missing}")
            } else {
                format!("{ctx}: {e}")
            }
        } else {
            match kind {
                RarFailKind::PasswordRequired => "rar_password_required".to_string(),
                RarFailKind::PasswordWrong => "rar_password_wrong".to_string(),
                _ => format!("{ctx}: {e}"),
            }
        };
        RarWalkError::Failed { kind, message }
    }

    /// One forward pass over the archive on the calling thread (no worker, no
    /// channel): the sink is called as UnRAR produces data, and `cancel` is polled
    /// on every piece UnRAR hands over (at most a few MiB of output apart) and
    /// between entries. Entries with ordinal below `start`, and entries the sink
    /// does not `want`, are skipped: a non-solid archive seeks past them, a solid
    /// one decodes and discards them (it must, to stay aligned) but still polls
    /// `cancel`, so a stop never waits out a long skip.
    ///
    /// The password is used only to open the archive; it is never logged or put in
    /// an error message.
    pub fn rar_walk(
        archive_path: &Path,
        password: Option<&str>,
        excludes: &[String],
        start: u64,
        sink: &mut dyn RarWalkSink,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), RarWalkError> {
        use std::sync::atomic::Ordering;
        let path_str = archive_path.to_string_lossy().into_owned();
        let cancelled = || cancel.load(Ordering::Relaxed);
        if cancelled() {
            return Err(RarWalkError::Cancelled);
        }
        let opened = match password {
            Some(pw) => Archive::with_password(&path_str, pw).open_for_processing(),
            None => Archive::new(&path_str).open_for_processing(),
        };
        let mut open = opened.map_err(|e| walk_fail(&path_str, "open rar", e, false))?;
        let solid = open.is_solid();
        let mut ordinal = 0u64;
        loop {
            if cancelled() {
                return Err(RarWalkError::Cancelled);
            }
            let header = match open.read_header() {
                Ok(Some(h)) => h,
                Ok(None) => return Ok(()),
                Err(e) => return Err(walk_fail(&path_str, "read rar header", e, false)),
            };
            let name = header.entry().filename.clone();
            let declared = header.entry().unpacked_size;
            // An unknown-size entry is read to its end; the sender checks the
            // delivered length against the size `rar_layout` measured.
            let size_known = !rar_size_unknown(declared);
            let size = if size_known { declared } else { u64::MAX };
            let is_dir = header.entry().is_directory();
            let encrypted = header.entry().is_encrypted();
            let Some(rel) = sanitize_rar_entry(&name) else {
                return Err(RarWalkError::Failed {
                    kind: RarFailKind::Other,
                    message: format!("rar contains an unsafe entry path: {name:?}"),
                });
            };
            let excluded = !excludes.is_empty()
                && crate::excludes::is_excluded_strings(Path::new(&rel), excludes);
            if is_dir || (excluded && !solid) {
                open = header
                    .skip()
                    .map_err(|e| walk_fail(&path_str, "skip rar entry", e, false))?;
                continue;
            }
            if excluded {
                // Solid: the entry must be decoded to keep the stream aligned, and
                // RAR_SKIP never calls back, so stop could not be polled. Decode it
                // through a discarding callback that does.
                let r = header.read_to_fn(&mut |_: &[u8]| !cancelled());
                if cancelled() {
                    return Err(RarWalkError::Cancelled);
                }
                open = r.map_err(|e| {
                    walk_fail(
                        &path_str,
                        "skip rar entry",
                        e,
                        encrypted && password.is_some(),
                    )
                })?;
                continue;
            }
            let this = ordinal;
            ordinal += 1;
            if !sink.visit(this, &rel) {
                return Err(RarWalkError::Failed {
                    kind: RarFailKind::Other,
                    message: format!("rar entry {rel:?} is out of the listed order"),
                });
            }
            let deliver = this >= start && sink.want(this, &rel, declared);
            if !deliver && !solid {
                open = header
                    .skip()
                    .map_err(|e| walk_fail(&path_str, "skip rar entry", e, false))?;
                continue;
            }
            if deliver {
                sink.begin(&rel).map_err(RarWalkError::Sink)?;
            }
            let mut got = 0u64;
            let mut sink_err: Option<std::io::Error> = None;
            let mut overrun = false;
            let result = header.read_to_fn(&mut |bytes: &[u8]| {
                if cancelled() {
                    return false;
                }
                got += bytes.len() as u64;
                if got > size {
                    overrun = true;
                    return false;
                }
                if deliver {
                    if let Err(e) = sink.data(bytes) {
                        sink_err = Some(e);
                        return false;
                    }
                }
                true
            });
            if let Some(e) = sink_err {
                return Err(RarWalkError::Sink(e));
            }
            if cancelled() {
                return Err(RarWalkError::Cancelled);
            }
            if overrun {
                return Err(RarWalkError::Failed {
                    kind: RarFailKind::Corrupt,
                    message: format!(
                        "rar entry {rel:?} produced more bytes than its header declared"
                    ),
                });
            }
            open = result.map_err(|e| {
                walk_fail(
                    &path_str,
                    "extract rar entry",
                    e,
                    encrypted && password.is_some(),
                )
            })?;
            if size_known && got != size {
                return Err(RarWalkError::Failed {
                    kind: RarFailKind::Corrupt,
                    message: format!(
                        "rar entry {rel:?} produced {got} bytes but its header declared {size}"
                    ),
                });
            }
            if deliver {
                sink.end().map_err(RarWalkError::Sink)?;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn fixture(name: &str) -> std::path::PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/rar")
                .join(name)
        }

        // crypted.rar: content-encrypted (names listable without a password),
        // password "unrar", first entry ".gitignore" = "target\nCargo.lock\n".
        #[test]
        fn content_encrypted_lists_names_without_password() {
            let (_total, files) = rar_plan_preview(&fixture("crypted.rar"), None, &[]).unwrap();
            assert!(
                files.iter().any(|(p, _)| p == ".gitignore"),
                "names should be listable without a password: {files:?}"
            );
        }

        /// Ported from the staged extractor: it asserted the decompressed
        /// bytes exactly. That is worth keeping, so it now drives the
        /// streaming bridge directly — no mock server, no network, just
        /// "does UnRAR hand us the right bytes through the channel".
        /// The manifest must be built in ARCHIVE order, never the sorted
        /// preview order.
        ///
        /// This is a regression test for a real failure: streaming used
        /// `rar_plan_preview`, which sorts, and two adjacent files in a
        /// 181-entry archive transposed because `_` sorts before a letter
        /// (`precision_precisionplus` vs `precisionarrow`). Every shipped
        /// fixture holds a single entry, where sorting is a no-op, so no
        /// fixture test could see it. Asserting the two functions are
        /// *different functions* is the part CI can actually hold onto.
        #[test]
        fn the_streaming_plan_is_archive_order_not_sorted() {
            let a = fixture("crypted.rar");
            let (t1, archive_order) = rar_plan_entries(&a, Some("unrar"), &[]).unwrap();
            let (t2, sorted) = rar_plan_preview(&a, Some("unrar"), &[]).unwrap();
            // Same content either way...
            assert_eq!(t1, t2);
            let mut a_sorted = archive_order.clone();
            a_sorted.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(a_sorted, sorted, "the two must agree once sorted");
            // ...and the preview is sorted by construction.
            let mut check = sorted.clone();
            check.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(check, sorted, "rar_plan_preview must stay sorted");
        }

        #[test]
        fn streaming_decompresses_content_exactly() {
            use crate::rar_stream::{next_entry, EntryReader};
            use std::io::Read;

            let (rx, worker) = spawn_rar_worker(&fixture("crypted.rar"), Some("unrar"), vec![]);
            let name = next_entry(&rx).unwrap().expect("an entry");
            assert_eq!(name, ".gitignore");
            let mut out = Vec::new();
            EntryReader::new(&rx).read_to_end(&mut out).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), "target\nCargo.lock\n");
            drop(rx);
            let _ = worker.join();
        }

        #[test]
        fn content_encrypted_stream_needs_password() {
            use crate::rar_stream::{next_entry, EntryReader};
            use std::io::Read;

            // Content-encrypted: the name lists fine, so the entry is
            // announced and the failure only surfaces when the data is read.
            let (rx, worker) = spawn_rar_worker(&fixture("crypted.rar"), None, vec![]);
            let _ = next_entry(&rx).unwrap().expect("an entry");
            let mut out = Vec::new();
            let err = EntryReader::new(&rx).read_to_end(&mut out).unwrap_err();
            assert!(
                err.to_string().contains("rar_password_required"),
                "got: {err}"
            );
            drop(rx);
            let _ = worker.join();
        }

        // comment-hpw-password.rar: HEADER-encrypted (names need the password),
        // password "password".
        #[test]
        fn header_encrypted_list_needs_password() {
            let err = inspect_rar(&fixture("comment-hpw-password.rar"), None).unwrap_err();
            assert!(
                err.to_string().contains("rar_password_required"),
                "got: {err}"
            );
        }

        #[test]
        fn header_encrypted_inspect_with_password() {
            let ins = inspect_rar(&fixture("comment-hpw-password.rar"), Some("password")).unwrap();
            assert!(ins.file_count >= 1);
            assert!(ins.total_uncompressed > 0);
        }

        #[test]
        fn wrong_password_is_reported() {
            use crate::rar_stream::next_entry;

            // HEADER-encrypted, so UnRAR really can tell a wrong password
            // from corruption here — unlike the content-encrypted fixture,
            // where it reports a CRC error instead.
            let (rx, worker) = spawn_rar_worker(
                &fixture("comment-hpw-password.rar"),
                Some("definitely-wrong"),
                vec![],
            );
            let err = next_entry(&rx).unwrap_err();
            assert!(err.to_string().contains("rar_password_wrong"), "got: {err}");
            drop(rx);
            let _ = worker.join();
        }

        // Excludes drop matching entries from the plan.
        #[test]
        fn excludes_apply_to_plan() {
            let (_t, all) = rar_plan_preview(&fixture("crypted.rar"), None, &[]).unwrap();
            let (_t2, filtered) =
                rar_plan_preview(&fixture("crypted.rar"), None, &[".gitignore".to_string()])
                    .unwrap();
            assert!(all.iter().any(|(p, _)| p == ".gitignore"));
            assert!(filtered.iter().all(|(p, _)| p != ".gitignore"));
        }
    }
}

#[cfg(test)]
mod sevenz_thread_tests {
    use super::select_sevenz_decode_threads;

    /// Multi-threaded decode buffers whole dict-reset units in RAM, so an
    /// unset/unparseable value must never silently opt a 205 GB transfer into
    /// it. Absence means single-threaded, not "guess from the CPU count".
    #[test]
    fn defaults_to_single_threaded_streaming_decode() {
        assert_eq!(select_sevenz_decode_threads(None), 1);
        assert_eq!(select_sevenz_decode_threads(Some("")), 1);
        assert_eq!(select_sevenz_decode_threads(Some("invalid")), 1);
    }

    #[test]
    fn explicit_override_is_validated_and_hard_capped() {
        assert_eq!(select_sevenz_decode_threads(Some("2")), 2);
        assert_eq!(select_sevenz_decode_threads(Some("0")), 1);
        assert_eq!(select_sevenz_decode_threads(Some("999")), 16);
    }
}

#[cfg(test)]
mod zip_sanitize_tests {
    use super::sanitize_zip_entry;

    #[test]
    fn rejects_traversal_and_unsafe() {
        assert_eq!(sanitize_zip_entry("../etc/passwd"), None);
        assert_eq!(sanitize_zip_entry("a/../../b"), None);
        assert_eq!(sanitize_zip_entry("a/b\0c"), None);
        assert_eq!(sanitize_zip_entry("a\\b"), None); // backslash segment
        assert_eq!(sanitize_zip_entry(""), None);
        // Note: directory entries are filtered out upstream via is_dir(), so
        // sanitize never has to reject a trailing-slash name — "dir/" simply
        // drops the empty segment and yields "dir".
        assert_eq!(sanitize_zip_entry("dir/").as_deref(), Some("dir"));
    }

    #[test]
    fn normalises_safe_paths_to_forward_slashes() {
        assert_eq!(
            sanitize_zip_entry("CUSA03474/sce_sys/icon0.png").as_deref(),
            Some("CUSA03474/sce_sys/icon0.png")
        );
        // Redundant separators and "." segments are collapsed.
        assert_eq!(
            sanitize_zip_entry("a//./b/c.txt").as_deref(),
            Some("a/b/c.txt")
        );
        assert_eq!(
            sanitize_zip_entry("eboot.bin").as_deref(),
            Some("eboot.bin")
        );
    }
}

#[cfg(test)]
mod retry_classification_tests {
    use super::*;

    fn ioerr(kind: std::io::ErrorKind) -> anyhow::Error {
        anyhow::Error::from(std::io::Error::new(kind, "test"))
    }

    #[test]
    fn never_retries_a_cancelled_transfer() {
        // Cancel means stop. A retry here would put bytes back on the wire
        // after the UI reported the upload cancelled — the 5.4.7 symptom.
        let bare = anyhow::anyhow!("transfer_cancelled");
        assert!(!is_retryable_transfer_error(&bare));

        // Wrapped in the context the streaming paths add on the way out.
        let wrapped = bare.context("transfer_rar attempt 0");
        assert!(!is_retryable_transfer_error(&wrapped));

        // And it must win even when a retryable io::Error rides along — a
        // cancelled transfer whose socket also died is still cancelled.
        let with_io = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer went away",
        ))
        .context("transfer_cancelled");
        assert!(!is_retryable_transfer_error(&with_io));
    }

    #[test]
    fn retries_network_drop_kinds() {
        for k in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::NotConnected,
        ] {
            assert!(
                is_retryable_transfer_error(&ioerr(k)),
                "expected retry for {k:?}"
            );
        }
    }

    #[test]
    fn does_not_retry_terminal_kinds() {
        for k in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::AlreadyExists,
            std::io::ErrorKind::WriteZero,
        ] {
            assert!(
                !is_retryable_transfer_error(&ioerr(k)),
                "expected NO retry for {k:?}"
            );
        }
    }

    #[test]
    fn walks_past_a_non_retryable_io_error_to_a_retryable_one() {
        // A chain can carry more than one io::Error — anyhow context is any
        // Display type, io::Error included. Classification used to stop at the
        // first one it found and judge the whole chain on that, so a
        // terminal-looking outer error hid the retryable cause underneath and
        // aborted an upload a resume would have recovered.
        let err = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "peer stopped reading",
        ))
        .context(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "outer",
        ))
        .context("write frame split");
        assert!(is_retryable_transfer_error(&err));
    }

    #[test]
    fn unwraps_through_anyhow_context() {
        // The transfer-loop wraps every IO error with .with_context(),
        // so the retry classifier must walk anyhow's cause chain to
        // find the underlying io::Error. Without this walk every error
        // is "not retryable" and resume never kicks in.
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "wifi");
        let wrapped: anyhow::Error = anyhow::Error::from(inner)
            .context("write_all_parts")
            .context("send chunk 7");
        assert!(is_retryable_transfer_error(&wrapped));
    }

    #[test]
    fn protocol_errors_do_not_retry() {
        // A bare anyhow::anyhow! string error has no io::Error in its
        // chain — these are protocol-level rejections like "tx_id
        // mismatch" or "unknown frame type", which the payload has
        // already aborted. Retrying can't help.
        let e: anyhow::Error = anyhow::anyhow!("direct_tx_corrupt");
        assert!(!is_retryable_transfer_error(&e));
    }
}

#[cfg(test)]
mod rar_volume_tests {
    use super::rar_support::missing_volume;
    use std::collections::HashSet;

    fn fs(files: &[&str]) -> impl Fn(&str) -> bool {
        let set: HashSet<String> = files.iter().map(|s| s.to_string()).collect();
        move |p: &str| set.contains(p)
    }

    /// The reported case: every volume present, first part selected. The
    /// old message told the user to do what they had already done, so a
    /// complete set must report nothing and let UnRAR speak.
    #[test]
    fn complete_set_reports_nothing() {
        let have = fs(&[
            "/g/game.part1.rar",
            "/g/game.part2.rar",
            "/g/game.part3.rar",
        ]);
        assert_eq!(missing_volume("/g/game.part1.rar", &have), None);
    }

    #[test]
    fn names_the_volume_that_is_actually_missing() {
        let have = fs(&["/g/game.part1.rar", "/g/game.part3.rar"]);
        assert_eq!(
            missing_volume("/g/game.part1.rar", &have).as_deref(),
            Some("game.part2.rar")
        );
    }

    /// Zero-padded widths must round-trip, or the name we print is wrong.
    #[test]
    fn preserves_the_padding_width() {
        let have = fs(&["/g/game.part01.rar", "/g/game.part03.rar"]);
        assert_eq!(
            missing_volume("/g/game.part01.rar", &have).as_deref(),
            Some("game.part02.rar")
        );
        let have3 = fs(&["/g/g.part001.rar", "/g/g.part003.rar"]);
        assert_eq!(
            missing_volume("/g/g.part001.rar", &have3).as_deref(),
            Some("g.part002.rar")
        );
    }

    /// A single-volume archive is not an incomplete set.
    #[test]
    fn single_volume_is_not_a_gap() {
        let have = fs(&["/g/game.rar"]);
        assert_eq!(missing_volume("/g/game.rar", &have), None);
    }

    /// Older split scheme: name.rar + name.r00, r01 …
    #[test]
    fn handles_the_legacy_r_nn_scheme() {
        let have = fs(&["/g/game.rar", "/g/game.r00", "/g/game.r02"]);
        assert_eq!(
            missing_volume("/g/game.rar", &have).as_deref(),
            Some("game.r01")
        );
        let complete = fs(&["/g/game.rar", "/g/game.r00", "/g/game.r01"]);
        assert_eq!(missing_volume("/g/game.rar", &complete), None);
    }

    /// Windows paths and no-directory paths must not break the split.
    #[test]
    fn handles_windows_and_bare_paths() {
        let have = fs(&["C:\\dumps\\g.part1.rar", "C:\\dumps\\g.part3.rar"]);
        assert_eq!(
            missing_volume("C:\\dumps\\g.part1.rar", &have).as_deref(),
            Some("g.part2.rar")
        );
        let bare = fs(&["g.part1.rar", "g.part3.rar"]);
        assert_eq!(
            missing_volume("g.part1.rar", &bare).as_deref(),
            Some("g.part2.rar")
        );
    }

    /// A gap at the very end is the end of the set, not a hole.
    #[test]
    fn trailing_absence_is_the_end_of_the_set() {
        let have = fs(&["/g/game.part1.rar", "/g/game.part2.rar"]);
        assert_eq!(missing_volume("/g/game.part1.rar", &have), None);
    }
}

#[cfg(test)]
mod rar_param_root_tests {
    use super::rar_support::better_param_hit;

    /// Fold a whole archive listing, in order, the way inspect_rar does.
    fn pick(entries: &[&str]) -> Option<String> {
        let mut hit: Option<(usize, String)> = None;
        for e in entries {
            hit = better_param_hit(hit, e);
        }
        hit.map(|(_, root)| root)
    }

    #[test]
    fn a_bare_dump_has_an_empty_root() {
        assert_eq!(pick(&["sce_sys/param.json"]), Some(String::new()));
    }

    #[test]
    fn a_wrapped_dump_resolves_to_the_wrapper() {
        // The real shape from a scene release: an outer site-named folder,
        // then the app folder.
        assert_eq!(
            pick(&[
                "[SITE]-PPSA13428/PPSA13428-app/eboot.bin",
                "[SITE]-PPSA13428/PPSA13428-app/sce_sys/param.json",
            ]),
            Some("[SITE]-PPSA13428/PPSA13428-app".to_string())
        );
    }

    #[test]
    fn the_shallowest_wins_regardless_of_listing_order() {
        let deep_first = pick(&["Game/patch/sce_sys/param.json", "Game/sce_sys/param.json"]);
        let shallow_first = pick(&["Game/sce_sys/param.json", "Game/patch/sce_sys/param.json"]);
        assert_eq!(deep_first, Some("Game".to_string()));
        assert_eq!(
            deep_first, shallow_first,
            "the answer must not depend on archive order"
        );
    }

    #[test]
    fn a_dlc_or_update_folder_cannot_outrank_the_base_game() {
        assert_eq!(
            pick(&[
                "Game/sce_sys/param.json",
                "Game/dlc0/sce_sys/param.json",
                "Game/dlc1/sce_sys/param.json",
            ]),
            Some("Game".to_string())
        );
    }

    #[test]
    fn an_archive_without_param_json_has_no_root() {
        assert_eq!(pick(&["Game/eboot.bin", "Game/sce_sys/icon0.png"]), None);
    }

    #[test]
    fn a_path_merely_ending_in_the_text_is_not_a_match() {
        // "notsce_sys" ends with "sce_sys" as a substring; only a real path
        // boundary counts, or every such folder would claim to be a root.
        assert_eq!(pick(&["Game/notsce_sys/param.json"]), None);
    }

    #[test]
    fn param_json_elsewhere_is_ignored() {
        assert_eq!(pick(&["Game/sce_sys/param.sfo", "Game/param.json"]), None);
    }

    #[test]
    fn ties_keep_the_first_seen() {
        // Two roots at the same depth: stable choice, no flapping.
        assert_eq!(
            pick(&["A/sce_sys/param.json", "B/sce_sys/param.json"]),
            Some("A".to_string())
        );
    }
}

#[cfg(test)]
mod zip_entry_name_tests {
    use super::zip_entry_name;

    #[test]
    fn utf8_bytes_without_the_flag_stay_utf8() {
        let raw = "unicode 名前 é.txt".as_bytes();
        // what the crate's CP437 decoding produced for these bytes
        assert_eq!(
            zip_entry_name(raw, "unicode σÉìσëì ├⌐.txt"),
            "unicode 名前 é.txt"
        );
    }

    #[test]
    fn non_utf8_bytes_keep_the_cp437_decoding() {
        // 0x82 is "é" in CP437 and not valid UTF-8 on its own.
        assert_eq!(zip_entry_name(b"caf\x82.txt", "café.txt"), "café.txt");
    }
}
