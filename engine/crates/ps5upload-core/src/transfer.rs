//! Reusable blocking transfer logic (single-file and directory).
//!
//! One TCP connection is opened per transaction and reused across BEGIN_TX,
//! every STREAM_SHARD, and COMMIT_TX. This removes per-frame connect overhead
//! and keeps the TCP congestion window warm, which is the primary throughput
//! constraint on a LAN.

use crate::connection::Connection;
use crate::{hash_shard, Manifest, ManifestFile};
use anyhow::{bail, Context, Result};
pub use ftx2_proto::TX_FLAG_RESUME;
use ftx2_proto::{
    FrameType, ShardAck, ShardHeader, TxMeta, PACKED_RECORD_PREFIX_LEN, SHARD_FLAG_PACKED,
};
use std::collections::VecDeque;
use std::path::{Component, Path};

pub const DEFAULT_SHARD_SIZE: usize = 32 * 1024 * 1024; // 32 MiB
pub const DEFAULT_MAX_SHARD_RETRIES: u32 = 3;
/// Default maximum number of STREAM_SHARD frames outstanding (no ACK yet).
/// Pipelining past 1 is what hides per-shard RTT on small-file directories.
/// Set conservatively: 32 small shards (~128 KiB total at 4 KiB each) or a
/// few large shards (32 MiB) never come close to kernel socket buffer size.
pub const DEFAULT_INFLIGHT_SHARDS: usize = 32;
/// Default byte cap on shards outstanding at once.
///
/// 64 MiB — two full 32 MiB shards in flight, enough to overlap hash+send
/// with payload-side write. The 2026-04-17 audit suggested 4-8 MiB on the
/// basis that the PS5's 512 KiB receive-buffer cap makes larger host send
/// buffers bufferbloat-without-throughput. On our e1000 lab NIC that thesis
/// didn't survive measurement: 8 MiB and 32 MiB caps each produced ~2-4%
/// regressions on huge-file single-file throughput vs 64 MiB, with no tail
/// latency benefit visible in the sweep. 64 MiB is the empirically-best
/// default for this hardware class; tunable via `FTX2_INFLIGHT_BYTES` for
/// non-e1000 networks where the audit math may pay off.
pub const DEFAULT_INFLIGHT_BYTES: usize = 64 * 1024 * 1024;
/// Default pack shard target size — the cap on total packed body bytes per
/// STREAM_SHARD frame. 4 MiB trades payload-side pack-parse cost against
/// shard-count reduction.
pub const DEFAULT_PACK_SIZE: usize = 4 * 1024 * 1024;
/// Default per-file cap for packing. Files at or above this size get their
/// own non-packed shard, which on the payload side goes through the
/// double-buffered writer thread (overlaps recv + disk write). Packing that
/// regime would lose the overlap since the packed path is serialised per
/// record. 128 KiB tracks the observed PS5 `pthread_create` crossover cost
/// (~4–6 ms) vs small-file write time (~µs).
pub const DEFAULT_PACK_FILE_MAX: usize = 128 * 1024;

// ─── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// FTX2 server address (e.g. "192.168.137.2:9113")
    pub addr: String,
    /// Maximum bytes per shard
    pub shard_size: usize,
    /// Max retry attempts per shard on digest mismatch (non-pipelined path only).
    pub max_shard_retries: u32,
    /// Maximum number of shards sent without a matching ACK. Set to 1 to
    /// force strict send-wait-send lockstep (matches pre-pipelining behaviour).
    pub inflight_shards: usize,
    /// Maximum total bytes of shard payload sent without a matching ACK.
    /// Whichever of inflight_shards / inflight_bytes is reached first causes
    /// the sender to block until the oldest outstanding shard is ACKed.
    pub inflight_bytes: usize,
    /// Target packed-shard body size in bytes. Multiple files are coalesced
    /// into packed shards up to this cap. Set to 0 to disable packing entirely
    /// (one file = one shard, pre-B2 behaviour).
    pub pack_size: usize,
    /// Per-file cap for packing. Files at or above this size get their own
    /// non-packed shard so they benefit from the payload's double-buffered
    /// writer thread (packed records are written serially). Only files with
    /// `size < pack_file_max` are packing candidates.
    pub pack_file_max: usize,
    /// Glob-ish patterns to exclude from `transfer_dir` walks. See
    /// `crate::excludes` for the pattern grammar. Empty = include
    /// everything; populated = skip matching files before they enter the
    /// manifest. The common case is passing
    /// `excludes::DEFAULT_EXCLUDES` to skip `.DS_Store`, `*.esbak`,
    /// `.git/**`, `Thumbs.db`, `desktop.ini`.
    pub excludes: Vec<String>,
    /// Optional cumulative-bytes progress counter. When set, the transfer
    /// loop `fetch_add`s each shard's wire size into this counter right
    /// after it's queued. Consumers (the HTTP engine's transfer handlers)
    /// poll the counter on a separate cadence to drive progress-bar
    /// updates without adding a lock to the hot send loop. A `None` here
    /// keeps the single-shot legacy behavior (no progress reporting).
    pub progress_bytes: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl TransferConfig {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            shard_size: DEFAULT_SHARD_SIZE,
            max_shard_retries: DEFAULT_MAX_SHARD_RETRIES,
            inflight_shards: DEFAULT_INFLIGHT_SHARDS,
            inflight_bytes: DEFAULT_INFLIGHT_BYTES,
            pack_size: DEFAULT_PACK_SIZE,
            pack_file_max: DEFAULT_PACK_FILE_MAX,
            excludes: Vec::new(),
            progress_bytes: None,
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
    pub tx_id_hex: String,
    pub shards_sent: u64,
    pub bytes_sent: u64,
    pub dest: String,
    /// Raw JSON body of the final CommitTxAck. Callers that want detailed
    /// payload-side timing breakdown (`timing_us.{recv,write,verify,apply}`)
    /// can parse this as JSON.
    pub commit_ack_body: String,
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

fn bytes_to_hex(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ps5_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().replace('\\', "/")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn join_ps5_path(root: &str, rel: &Path) -> String {
    let rel = ps5_relative_path(rel);
    if rel.is_empty() {
        root.trim_end_matches('/').to_string()
    } else {
        format!("{}/{}", root.trim_end_matches('/'), rel)
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn ps5_dest_paths_use_forward_slashes() {
        assert_eq!(
            join_ps5_path("/data/game", Path::new(r"sce_sys\param.json")),
            "/data/game/sce_sys/param.json"
        );
        assert_eq!(
            join_ps5_path("/data/game/", Path::new("sce_sys/icon0.png")),
            "/data/game/sce_sys/icon0.png"
        );
    }
}

fn tx_meta_buf(tx_id: [u8; 16], kind: u32, extra: &[u8]) -> Vec<u8> {
    tx_meta_buf_flags(tx_id, kind, 0, extra)
}

/// Variant that lets the caller pass explicit BeginTx flags. The reconnect
/// wrapper uses this with `ftx2_proto::TX_FLAG_RESUME` to signal "this is
/// a resume, the tx_id already exists in your journal." All other call
/// sites go through `tx_meta_buf` with flags=0.
pub(crate) fn tx_meta_buf_flags(tx_id: [u8; 16], kind: u32, flags: u32, extra: &[u8]) -> Vec<u8> {
    let mut buf = TxMeta { tx_id, kind, flags }.encode().to_vec();
    buf.extend_from_slice(extra);
    buf
}

/// Returns true when an error from a transfer is network-drop-ish and
/// worth retrying via resume. Intentionally conservative: we only retry
/// on errors whose root cause is "the TCP stream broke mid-transfer,"
/// not on protocol errors like `direct_tx_corrupt` (the payload has
/// already aborted the tx — a retry can't help).
pub fn is_retryable_transfer_error(err: &anyhow::Error) -> bool {
    // Walk the chain looking for std::io::Error with a retryable kind.
    // Covers: mid-transfer TCP resets (ConnectionReset/ConnectionAborted/
    // BrokenPipe), server-side hang on shutdown (UnexpectedEof), wifi
    // drop on write (TimedOut), EINTR during signal delivery on macOS
    // (Interrupted), and the half-open state seen after a macOS
    // sleep/wake cycle (NotConnected).
    for cause in err.chain() {
        if let Some(ioerr) = cause.downcast_ref::<std::io::Error>() {
            return matches!(
                ioerr.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::NotConnected
            );
        }
    }
    false
}

/// Send a control frame on an existing connection and expect a specific ACK back.
fn send_and_expect(
    c: &mut Connection,
    ft_send: FrameType,
    body: &[u8],
    ft_expect: FrameType,
) -> Result<Vec<u8>> {
    c.send_frame(ft_send, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft != ft_expect {
        bail!(
            "{ft_send:?} rejected ({ft:?}): {}",
            String::from_utf8_lossy(&resp)
        );
    }
    Ok(resp)
}

/// Parse `last_acked_shard` out of a BeginTxAck body. Returns 0 for both
/// "fresh transfer, field absent" (old payload) and "resume but field
/// missing" (payload doesn't yet support TX_FLAG_RESUME) — safe
/// degradation since direct-write and spool-then-apply are both
/// idempotent on shard_seq.
///
/// When `is_resume` is true, also logs a warning if the field is missing
/// or the body isn't JSON — the caller set TX_FLAG_RESUME expecting the
/// new protocol, and a missing field is a compatibility signal worth
/// surfacing to the investigation log so the user learns their client
/// upgrade is ahead of their payload.
///
/// When non-zero, the client MUST skip streaming shards with
/// `shard_seq <= last_acked_shard` — the payload has already journalled
/// them from a prior (interrupted) connection. See `specs/ftx2-protocol.md`
/// "Resume flow (2.1+)".
fn parse_last_acked_shard(body: &[u8], is_resume: bool) -> u64 {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => match v.get("last_acked_shard").and_then(|s| s.as_u64()) {
            Some(n) => n,
            None => {
                if is_resume {
                    eprintln!(
                        "[resume] BeginTxAck lacks last_acked_shard field \
                         (payload predates 2.1 protocol); resending from shard 1 — \
                         correct but slower than true resume"
                    );
                }
                0
            }
        },
        Err(e) => {
            if is_resume {
                eprintln!(
                    "[resume] BeginTxAck body not parseable as JSON ({e}); \
                     falling back to full resend. Body: {}",
                    String::from_utf8_lossy(body)
                );
            }
            0
        }
    }
}

/// Send one shard on an existing connection, retrying on `shard_digest_mismatch`.
fn send_shard_on(
    c: &mut Connection,
    tx_id: [u8; 16],
    shard_seq: u64,
    total_shards: u64,
    data: &[u8],
    max_retries: u32,
) -> Result<ShardAck> {
    let digest = hash_shard(data);
    let hdr_bytes = ShardHeader {
        tx_id,
        shard_seq,
        shard_digest: digest,
        record_count: 1,
        flags: 0,
    }
    .encode();

    let max = max_retries.max(1);
    for attempt in 1..=max {
        c.send_frame_split(FrameType::StreamShard, &hdr_bytes, data)?;
        let (resp_hdr, resp_body) = c.recv_frame()?;
        let ft = resp_hdr.frame_type().unwrap_or(FrameType::Error);
        match ft {
            FrameType::ShardAck => {
                return ShardAck::decode(&resp_body).context("decode SHARD_ACK");
            }
            FrameType::Error => {
                let msg = String::from_utf8_lossy(&resp_body);
                if msg.contains("shard_digest_mismatch") && attempt < max {
                    eprintln!(
                        "shard {shard_seq}/{total_shards}: digest mismatch, retry {attempt}/{max}"
                    );
                    continue;
                }
                bail!("shard {shard_seq} error after {attempt} attempt(s): {msg}");
            }
            other => bail!("shard {shard_seq}: expected SHARD_ACK, got {other:?}"),
        }
    }
    bail!("shard {shard_seq}: exhausted {max} retries");
}

// ─── Pipelined shard sender ───────────────────────────────────────────────────

/// A bounded-window pipelined sender over an existing `Connection`.
///
/// Sending `N` shards with `send()` queues up to `max_inflight_shards` / bytes
/// on the wire without waiting for their SHARD_ACKs. When the window fills,
/// `send()` blocks on the oldest outstanding ACK before issuing the next
/// STREAM_SHARD. Call `drain()` before COMMIT_TX to flush all outstanding ACKs
/// — the payload must see every shard ACKed before CommitTx so its internal
/// `shards_received` count is consistent.
///
/// **Ordering.** The payload processes frames serially per connection
/// (single-threaded inner loop). ACKs therefore arrive in send order, and we
/// validate that — if the observed `shard_seq` doesn't match the expected
/// head of the queue, the sender returns an error rather than guessing.
///
/// **Error handling.** No per-shard retry. If any shard reports
/// `shard_digest_mismatch` or any other error frame, the whole transaction
/// fails: we've already sent shards beyond the failing one, and for direct
/// write mode those are not individually undoable. Callers who need retry
/// must use the legacy non-pipelined path (`send_shard_on`) and accept its
/// lockstep performance.
struct PipelinedSender<'a> {
    c: &'a mut Connection,
    /// (shard_seq, payload_bytes) for each shard awaiting an ACK, in send order.
    inflight: VecDeque<(u64, usize)>,
    inflight_bytes: usize,
    max_inflight_shards: usize,
    max_inflight_bytes: usize,
    tx_id: [u8; 16],
    total_shards: u64,
}

impl<'a> PipelinedSender<'a> {
    fn new(
        c: &'a mut Connection,
        cfg: &TransferConfig,
        tx_id: [u8; 16],
        total_shards: u64,
    ) -> Self {
        let max_inflight_shards = cfg.inflight_shards.max(1);
        let max_inflight_bytes = cfg.inflight_bytes.max(1);
        Self {
            c,
            inflight: VecDeque::with_capacity(max_inflight_shards),
            inflight_bytes: 0,
            max_inflight_shards,
            max_inflight_bytes,
            tx_id,
            total_shards,
        }
    }

    /// Send a single shard. Blocks on prior ACKs only as needed to stay under
    /// the configured window. First shard always sends regardless of byte cap
    /// (otherwise shard_size > cap would deadlock).
    fn send(&mut self, shard_seq: u64, data: &[u8]) -> Result<()> {
        self.send_with(shard_seq, data, 1, 0)
    }

    /// Send a shard with explicit flags / record_count. Used by the pack-shard
    /// path to set `SHARD_FLAG_PACKED` and the multi-record count. BLAKE3 digest
    /// is computed over `data` (the full shard body exactly as sent).
    fn send_with(
        &mut self,
        shard_seq: u64,
        data: &[u8],
        record_count: u32,
        flags: u32,
    ) -> Result<()> {
        while !self.inflight.is_empty()
            && (self.inflight.len() >= self.max_inflight_shards
                || self.inflight_bytes + data.len() > self.max_inflight_bytes)
        {
            self.await_one_ack()?;
        }
        let digest = hash_shard(data);
        let hdr_bytes = ShardHeader {
            tx_id: self.tx_id,
            shard_seq,
            shard_digest: digest,
            record_count,
            flags,
        }
        .encode();
        self.c
            .send_frame_split(FrameType::StreamShard, &hdr_bytes, data)?;
        self.inflight.push_back((shard_seq, data.len()));
        self.inflight_bytes += data.len();
        Ok(())
    }

    /// Receive and validate the ACK at the head of the queue.
    fn await_one_ack(&mut self) -> Result<ShardAck> {
        let (expected, bytes) = self
            .inflight
            .pop_front()
            .context("await_one_ack with empty queue")?;
        let (hdr, body) = self.c.recv_frame()?;
        self.inflight_bytes = self.inflight_bytes.saturating_sub(bytes);
        let ft = hdr.frame_type().unwrap_or(FrameType::Error);
        match ft {
            FrameType::ShardAck => {
                let ack = ShardAck::decode(&body).context("decode SHARD_ACK")?;
                if ack.shard_seq != expected {
                    bail!(
                        "SHARD_ACK out of order: expected shard {}/{}, got {}",
                        expected,
                        self.total_shards,
                        ack.shard_seq
                    );
                }
                Ok(ack)
            }
            FrameType::Error => bail!(
                "shard {}/{} error: {}",
                expected,
                self.total_shards,
                String::from_utf8_lossy(&body)
            ),
            other => bail!(
                "shard {}/{}: expected SHARD_ACK, got {:?}",
                expected,
                self.total_shards,
                other
            ),
        }
    }

    /// Drain all outstanding ACKs. MUST be called before CommitTx.
    fn drain(&mut self) -> Result<()> {
        while !self.inflight.is_empty() {
            self.await_one_ack()?;
        }
        Ok(())
    }
}

// ─── Standalone shard send (kept for `lab send-shard` convenience) ────────────

/// Open a fresh connection and send one shard. Used by the lab CLI's
/// `send-shard` command; production transfers use the connection-reusing
/// helpers below.
pub fn send_shard(
    addr: &str,
    tx_id: [u8; 16],
    shard_seq: u64,
    total_shards: u64,
    data: &[u8],
    max_retries: u32,
) -> Result<ShardAck> {
    let mut c = Connection::connect(addr)?;
    send_shard_on(&mut c, tx_id, shard_seq, total_shards, data, max_retries)
}

// ─── Single-file transfer ─────────────────────────────────────────────────────

/// Transfer a file (already read into memory) to `dest` on the PS5.
///
/// Single-attempt — aborts on any connection drop. For auto-resume on
/// network-flake, use `transfer_file_resumable` instead.
pub fn transfer_file(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    data: &[u8],
) -> Result<TransferResult> {
    transfer_file_with_flags(cfg, tx_id, dest, data, 0)
}

/// Transfer a file from disk without reading or mapping the whole file.
///
/// Peak host RAM is bounded by `cfg.shard_size` plus socket buffers. This is
/// the production path for large game images; the slice-based `transfer_file`
/// remains for tests/benchmarks and callers that already own the bytes.
pub fn transfer_file_path(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    src: &Path,
) -> Result<TransferResult> {
    transfer_file_path_with_flags(cfg, tx_id, dest, src, 0)
}

/// Transfer a file with explicit BeginTx flags. `flags=TX_FLAG_RESUME`
/// asks the payload to reuse an existing (interrupted) tx_id. Internal
/// helper — public consumers should use `transfer_file` (fresh) or
/// `transfer_file_resumable` (with retry loop).
fn transfer_file_with_flags(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    data: &[u8],
    flags: u32,
) -> Result<TransferResult> {
    let tx_id_hex = bytes_to_hex(&tx_id);
    let total_bytes = data.len() as u64;
    let total_shards = data.chunks(cfg.shard_size).count() as u64;

    let manifest_json = serde_json::to_vec(&Manifest {
        dest_root: dest.to_string(),
        file_count: 1,
        total_bytes,
        total_shards,
        files: vec![],
    })?;

    let mut c = Connection::connect(&cfg.addr)?;
    let begin_ack = send_and_expect(
        &mut c,
        FrameType::BeginTx,
        &tx_meta_buf_flags(tx_id, 1, flags, &manifest_json),
        FrameType::BeginTxAck,
    )?;
    // `last_acked_shard` is non-zero only when a prior (interrupted)
    // connection for this tx_id reached the payload journal. For fresh
    // transfers (flags=0) it's always 0 and the skip branch becomes
    // unreachable; for resume attempts (flags=TX_FLAG_RESUME) it's how
    // we know where to pick up.
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);

    // `shards_sent` counts only what this call actually transmitted — on a
    // resume, shards 1..=last_acked_shard are skipped because they're
    // already on the payload's disk from the interrupted prior attempt.
    // `bytes_sent` reflects the full plan (`total_bytes`) so benchmarks
    // and UI progress readouts don't have to special-case resume — the
    // payload ends up with `total_bytes` regardless of split across
    // attempts. Callers that need "wire bytes this call" can subtract
    // shards below `last_acked_shard` times `shard_size`.
    let mut shards_sent = 0u64;
    {
        let mut sender = PipelinedSender::new(&mut c, cfg, tx_id, total_shards);
        for (i, chunk) in data.chunks(cfg.shard_size).enumerate() {
            let shard_seq = i as u64 + 1;
            if shard_seq > last_acked_shard {
                sender.send(shard_seq, chunk)?;
                shards_sent += 1;
                if let Some(p) = &cfg.progress_bytes {
                    p.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        sender.drain()?;
    }

    let commit_ack = send_and_expect(
        &mut c,
        FrameType::CommitTx,
        &tx_meta_buf(tx_id, 0, b""),
        FrameType::CommitTxAck,
    )?;

    Ok(TransferResult {
        tx_id_hex,
        shards_sent,
        bytes_sent: total_bytes,
        dest: dest.to_string(),
        commit_ack_body: String::from_utf8_lossy(&commit_ack).into_owned(),
    })
}

/// Transfer a file with automatic resume-on-network-drop. The first
/// attempt is a fresh BeginTx; if it fails with a retryable IO error
/// (ConnectionReset / BrokenPipe / TimedOut / UnexpectedEof /
/// ConnectionAborted), the wrapper waits with exponential backoff
/// (500 ms → 1 s → 2 s → …) and retries with `TX_FLAG_RESUME` set,
/// re-using the same `tx_id` so the payload's journal can report
/// `last_acked_shard` and the client can skip past already-ACKed shards.
///
/// Non-retryable errors (protocol rejects, digest corruption, tx table
/// full, etc.) surface immediately. A zero `max_retries` means "no
/// retry" — equivalent to `transfer_file`.
///
/// The payload side of this must support `TX_FLAG_RESUME` (2.1+); older
/// payloads will treat the flag as a no-op and allocate a fresh entry,
/// which means the retry will double-send shards rather than resume.
/// That's still correct (the payload overwrites on direct-write, and
/// spool-then-apply dedups on shard_seq), but slower than the true
/// resume path.
pub fn transfer_file_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    data: &[u8],
    max_retries: u32,
) -> Result<TransferResult> {
    // Single-file uploads don't currently have a cross-session resume
    // flow (no persisted tx_id → no opportunity to re-supply one), so
    // initial_flags=0 is the only sensible value here. If that ever
    // changes, thread an initial_flags parameter through the way the
    // dir/file_list wrappers do.
    resumable_retry(max_retries, "transfer_file", 0, |flags| {
        transfer_file_with_flags(cfg, tx_id, dest, data, flags)
    })
}

fn transfer_file_path_with_flags(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    src: &Path,
    flags: u32,
) -> Result<TransferResult> {
    use std::io::{Read, Seek, SeekFrom};

    let tx_id_hex = bytes_to_hex(&tx_id);
    let meta = std::fs::metadata(src).with_context(|| format!("stat {}", src.display()))?;
    if !meta.is_file() {
        bail!("source is not a regular file: {}", src.display());
    }
    let total_bytes = meta.len();
    let total_shards = if total_bytes == 0 {
        0
    } else {
        total_bytes.div_ceil(cfg.shard_size as u64)
    };

    let manifest_json = serde_json::to_vec(&Manifest {
        dest_root: dest.to_string(),
        file_count: 1,
        total_bytes,
        total_shards,
        files: vec![],
    })?;

    let mut c = Connection::connect(&cfg.addr)?;
    let begin_ack = send_and_expect(
        &mut c,
        FrameType::BeginTx,
        &tx_meta_buf_flags(tx_id, 1, flags, &manifest_json),
        FrameType::BeginTxAck,
    )?;
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);

    let mut shards_sent = 0u64;
    {
        let mut file =
            std::fs::File::open(src).with_context(|| format!("open {}", src.display()))?;
        if last_acked_shard > 0 {
            file.seek(SeekFrom::Start(
                last_acked_shard.saturating_mul(cfg.shard_size as u64),
            ))
            .with_context(|| format!("seek {}", src.display()))?;
        }
        let mut sender = PipelinedSender::new(&mut c, cfg, tx_id, total_shards);
        let mut shard_seq = last_acked_shard + 1;
        // One shard-sized buffer reused across iterations. `send` writes to
        // the socket synchronously and the inflight queue stores only
        // `(shard_seq, len)`, so the buffer is free to reuse on the next
        // pass. Avoids 3,200 × 32 MiB allocate/free cycles on a 100 GiB
        // upload.
        let mut buf = vec![0u8; cfg.shard_size];
        while shard_seq <= total_shards {
            let remaining = total_bytes.saturating_sub((shard_seq - 1) * cfg.shard_size as u64);
            let len = std::cmp::min(cfg.shard_size as u64, remaining) as usize;
            let chunk = &mut buf[..len];
            file.read_exact(chunk)
                .with_context(|| format!("read {} shard {}", src.display(), shard_seq))?;
            sender.send(shard_seq, chunk)?;
            shards_sent += 1;
            if let Some(p) = &cfg.progress_bytes {
                p.fetch_add(len as u64, std::sync::atomic::Ordering::Relaxed);
            }
            shard_seq += 1;
        }
        sender.drain()?;
    }

    let commit_ack = send_and_expect(
        &mut c,
        FrameType::CommitTx,
        &tx_meta_buf(tx_id, 0, b""),
        FrameType::CommitTxAck,
    )?;

    Ok(TransferResult {
        tx_id_hex,
        shards_sent,
        bytes_sent: total_bytes,
        dest: dest.to_string(),
        commit_ack_body: String::from_utf8_lossy(&commit_ack).into_owned(),
    })
}

pub fn transfer_file_path_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    src: &Path,
    max_retries: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_file_path", 0, |flags| {
        transfer_file_path_with_flags(cfg, tx_id, dest, src, flags)
    })
}

// ─── Directory transfer ───────────────────────────────────────────────────────

/// Collect all regular files under `dir`, sorted by path.
pub fn collect_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&cur)
            .with_context(|| format!("readdir {}", cur.display()))?
            .collect::<std::result::Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Per-destination description to emit into the manifest. One entry per
/// logical file the payload will land on disk.
struct PlannedFile {
    dest_path: String,
    size: u64,
    shard_start: u64,
    shard_count: u64,
}

/// One packed record, as planned from the file walk. The source file is
/// opened and read lazily at send time, not at plan time, so a 129 GiB
/// directory plan is O(file_count × path_size) bytes of RAM, not O(total_bytes).
#[derive(Clone)]
struct PackRecord {
    dest_path: String,
    source: std::path::PathBuf,
    size: u64,
}

/// A planned shard — either a packed shard (many small records from many
/// source files) or a non-packed chunk (one source file, possibly a slice of
/// a larger file if we're splitting by `cfg.shard_size`). The actual body
/// bytes aren't materialised until just before send (`materialise_body`).
enum PlannedShard {
    /// `record_count == 1`, `flags == 0`, body is the file slice.
    NonPacked {
        shard_seq: u64,
        source: std::path::PathBuf,
        offset: u64,
        len: u64,
    },
    /// Zero-byte file — an empty non-packed shard so the payload still opens
    /// the destination and creates the file at COMMIT.
    Empty { shard_seq: u64 },
    /// `record_count == records.len()`, `flags == SHARD_FLAG_PACKED`.
    Packed {
        shard_seq: u64,
        records: Vec<PackRecord>,
    },
}

/// Plan-time pack accumulator. Collects `PackRecord`s, emitting a complete
/// `PlannedShard::Packed` when the next record would exceed the pack target.
struct PackPlanner {
    records: Vec<PackRecord>,
    body_size: usize,
    shard_seq: u64,
    target: usize,
}

impl PackPlanner {
    fn new(target: usize) -> Self {
        Self {
            records: Vec::new(),
            body_size: 0,
            shard_seq: 0,
            target,
        }
    }

    fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn record_size(dest_path: &str, size: u64) -> usize {
        PACKED_RECORD_PREFIX_LEN + dest_path.len() + size as usize
    }

    fn would_exceed(&self, rec_size: usize) -> bool {
        !self.records.is_empty() && self.body_size + rec_size > self.target
    }

    fn start(&mut self, shard_seq: u64) {
        debug_assert!(self.is_empty());
        self.shard_seq = shard_seq;
        self.body_size = 0;
    }

    fn push(&mut self, rec: PackRecord) {
        self.body_size += Self::record_size(&rec.dest_path, rec.size);
        self.records.push(rec);
    }

    fn take(&mut self) -> PlannedShard {
        let records = std::mem::take(&mut self.records);
        let shard = PlannedShard::Packed {
            shard_seq: self.shard_seq,
            records,
        };
        self.body_size = 0;
        self.shard_seq = 0;
        shard
    }
}

/// Materialise a shard's wire body. Reads from disk lazily — for a packed
/// shard, opens each source file in turn and copies its bytes into the
/// output buffer. Total host RAM for one shard = shard body size, bounded
/// by `pack_size` (for packed) or `shard_size` (for non-packed).
fn materialise_body(ps: &PlannedShard) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    match ps {
        PlannedShard::Empty { .. } => Ok(Vec::new()),
        PlannedShard::NonPacked {
            source,
            offset,
            len,
            ..
        } => {
            let mut f = std::fs::File::open(source)
                .with_context(|| format!("open {}", source.display()))?;
            if *offset > 0 {
                f.seek(SeekFrom::Start(*offset))
                    .with_context(|| format!("seek {} to {}", source.display(), offset))?;
            }
            let mut buf = vec![0u8; *len as usize];
            f.read_exact(&mut buf)
                .with_context(|| format!("read {} at {}+{}", source.display(), offset, len))?;
            Ok(buf)
        }
        PlannedShard::Packed { records, .. } => {
            let total: usize = records
                .iter()
                .map(|r| PackPlanner::record_size(&r.dest_path, r.size))
                .sum();
            let mut buf = Vec::with_capacity(total);
            for r in records {
                let p = r.dest_path.as_bytes();
                buf.extend_from_slice(&(p.len() as u32).to_le_bytes());
                buf.extend_from_slice(&(r.size as u32).to_le_bytes());
                buf.extend_from_slice(p);
                let mut f = std::fs::File::open(&r.source)
                    .with_context(|| format!("open pack record {}", r.source.display()))?;
                let start = buf.len();
                buf.resize(start + r.size as usize, 0);
                f.read_exact(&mut buf[start..])
                    .with_context(|| format!("read pack record {}", r.source.display()))?;
            }
            Ok(buf)
        }
    }
}

fn planned_shard_seq(ps: &PlannedShard) -> u64 {
    match ps {
        PlannedShard::Empty { shard_seq }
        | PlannedShard::NonPacked { shard_seq, .. }
        | PlannedShard::Packed { shard_seq, .. } => *shard_seq,
    }
}

fn planned_shard_meta(ps: &PlannedShard) -> (u32, u32) {
    match ps {
        // Empty and NonPacked both carry "1 record, no flags" — packed
        // flag is only set when multiple small files were coalesced.
        PlannedShard::Empty { .. } | PlannedShard::NonPacked { .. } => (1, 0),
        PlannedShard::Packed { records, .. } => (records.len() as u32, SHARD_FLAG_PACKED),
    }
}

/// Transfer every file under `src_dir` to `dest_root` on the PS5.
///
/// Sharding strategy:
///   - Files smaller than `cfg.pack_file_max` are coalesced into packed shards
///     whose body carries `[u32 path_len, u32 data_len, path, data]` per
///     record, up to `cfg.pack_size` total body bytes.
///   - Files ≥ that threshold are sent as their own non-packed shard(s), split
///     into `cfg.shard_size` chunks if needed. The payload's double-buffered
///     writer thread handles these so recv and disk-write overlap.
///   - Both paths share the same pipelined ACK window.
///
/// **Streaming.** Files are *not* read into RAM up front. A planning pass
/// walks the directory using `metadata` only, builds a `PlannedShard` list
/// and manifest, and sends the manifest in BEGIN_TX. During the send phase,
/// each shard body is materialised from disk just before it goes on the wire.
/// Peak host RAM ≈ `inflight_bytes` (default 64 MiB) + one shard being built,
/// independent of the total transfer size — so a 129 GiB directory works.
pub fn transfer_dir(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
) -> Result<TransferResult> {
    transfer_dir_with_flags(cfg, tx_id, dest_root, src_dir, 0)
}

/// Directory transfer with explicit BeginTx flags. `flags = TX_FLAG_RESUME`
/// asks the payload to adopt an existing (interrupted) tx entry with the
/// same tx_id, skipping shards it already acked. Used by
/// `transfer_dir_resumable`; direct callers should use `transfer_dir`
/// (fresh) or the resumable wrapper.
pub fn transfer_dir_with_flags(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
    flags: u32,
) -> Result<TransferResult> {
    let tx_id_hex = bytes_to_hex(&tx_id);
    let all_files = collect_files(src_dir)?;
    // Apply cfg.excludes before the manifest is built. Default is empty
    // (match legacy behavior). Callers set excludes explicitly via
    // `cfg.excludes = ...` or ergonomically via
    // `cfg.with_default_excludes()`.
    let local_files: Vec<_> = if cfg.excludes.is_empty() {
        all_files
    } else {
        let pat_refs: Vec<&str> = cfg.excludes.iter().map(String::as_str).collect();
        all_files
            .into_iter()
            .filter(|p| !crate::excludes::is_excluded(p, &pat_refs))
            .collect()
    };
    if local_files.is_empty() {
        bail!(
            "source directory is empty or fully excluded: {}",
            src_dir.display()
        );
    }

    // ── Planning pass ── metadata-only, builds the manifest and the plan
    //    of how every shard will be assembled at send time.
    let mut planned_files: Vec<PlannedFile> = Vec::with_capacity(local_files.len());
    let mut planned_shards: Vec<PlannedShard> = Vec::new();
    let mut total_bytes = 0u64;
    let mut next_seq: u64 = 1;
    let pack_enabled = cfg.pack_size > 0;
    let pack_threshold = if pack_enabled { cfg.pack_file_max } else { 0 };
    let mut packer = PackPlanner::new(cfg.pack_size.max(4096));

    for lf in &local_files {
        let meta = std::fs::metadata(lf).with_context(|| format!("stat {}", lf.display()))?;
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let rel = lf.strip_prefix(src_dir).unwrap_or(lf.as_path());
        let dest_path = join_ps5_path(dest_root, rel);
        total_bytes += size;

        if pack_enabled && (size as usize) < pack_threshold {
            let rec_size = PackPlanner::record_size(&dest_path, size);
            if packer.would_exceed(rec_size) {
                planned_shards.push(packer.take());
            }
            if packer.is_empty() {
                packer.start(next_seq);
                next_seq += 1;
            }
            packer.push(PackRecord {
                dest_path: dest_path.clone(),
                source: lf.clone(),
                size,
            });
            planned_files.push(PlannedFile {
                dest_path,
                size,
                shard_start: packer.shard_seq,
                shard_count: 1,
            });
        } else {
            if !packer.is_empty() {
                planned_shards.push(packer.take());
            }
            let shard_start = next_seq;
            let mut shard_count = 0u64;
            if size == 0 {
                planned_shards.push(PlannedShard::Empty {
                    shard_seq: next_seq,
                });
                next_seq += 1;
                shard_count = 1;
            } else {
                let mut offset = 0u64;
                while offset < size {
                    let chunk_len = std::cmp::min(cfg.shard_size as u64, size - offset);
                    planned_shards.push(PlannedShard::NonPacked {
                        shard_seq: next_seq,
                        source: lf.clone(),
                        offset,
                        len: chunk_len,
                    });
                    next_seq += 1;
                    shard_count += 1;
                    offset += chunk_len;
                }
            }
            planned_files.push(PlannedFile {
                dest_path,
                size,
                shard_start,
                shard_count,
            });
        }
    }
    if !packer.is_empty() {
        planned_shards.push(packer.take());
    }

    let total_shards = next_seq - 1;
    let file_count = planned_files.len() as u64;

    let manifest_files: Vec<ManifestFile> = planned_files
        .into_iter()
        .map(|p| ManifestFile {
            path: p.dest_path,
            size: p.size,
            shard_start: p.shard_start,
            shard_count: p.shard_count,
        })
        .collect();

    let manifest_json = serde_json::to_vec(&Manifest {
        dest_root: dest_root.to_string(),
        file_count,
        total_bytes,
        total_shards,
        files: manifest_files,
    })?;

    let mut c = Connection::connect(&cfg.addr)?;
    let begin_ack = send_and_expect(
        &mut c,
        FrameType::BeginTx,
        &tx_meta_buf_flags(tx_id, 2, flags, &manifest_json),
        FrameType::BeginTxAck,
    )?;
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);

    // ── Send pass ── materialise each shard body just before it goes out.
    // Skip shards that were already journalled by a prior (interrupted)
    // connection — their bytes are on the payload's disk, don't re-send.
    // `shards_sent` counts only what this call transmitted.
    let mut shards_sent = 0u64;
    {
        let mut sender = PipelinedSender::new(&mut c, cfg, tx_id, total_shards);
        for ps in &planned_shards {
            let seq = planned_shard_seq(ps);
            if seq <= last_acked_shard {
                continue;
            }
            let body = materialise_body(ps)?;
            let (record_count, flags) = planned_shard_meta(ps);
            shards_sent += 1;
            let wire_len = body.len() as u64;
            sender.send_with(seq, &body, record_count, flags)?;
            if let Some(p) = &cfg.progress_bytes {
                p.fetch_add(wire_len, std::sync::atomic::Ordering::Relaxed);
            }
        }
        sender.drain()?;
    }

    let commit_ack = send_and_expect(
        &mut c,
        FrameType::CommitTx,
        &tx_meta_buf(tx_id, 0, b""),
        FrameType::CommitTxAck,
    )?;

    // `bytes_sent` reflects the full plan, not this call's wire bytes.
    // Rationale: for packed multi-file shards, wire bytes include pack
    // prefix overhead which isn't a user-meaningful number. The plan
    // total is what benchmarks + UI progress bars actually care about.
    Ok(TransferResult {
        tx_id_hex,
        shards_sent,
        bytes_sent: total_bytes,
        dest: dest_root.to_string(),
        commit_ack_body: String::from_utf8_lossy(&commit_ack).into_owned(),
    })
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

/// Transfer an explicit list of `(local_src, ps5_dest)` pairs in a single
/// FTX2 transaction. This is used when the caller already has the file list
/// (e.g. from `app/server.js` `uploadFiles`) and wants one atomic transaction.
///
/// `dest_root` is stored in the manifest for informational purposes only;
/// each file's `dest` is its absolute PS5 path.
pub fn transfer_file_list(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    entries: &[FileListEntry],
) -> Result<TransferResult> {
    transfer_file_list_with_flags(cfg, tx_id, dest_root, entries, 0)
}

/// Like `transfer_file_list` but with explicit BeginTx flags.
/// `flags = TX_FLAG_RESUME` asks the payload to adopt an existing
/// (interrupted) tx entry with the same tx_id, skipping shards it
/// already acked. Used by the engine's retry loop so a transient
/// payload hiccup doesn't force the caller to resend shards that
/// already landed.
pub fn transfer_file_list_with_flags(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    entries: &[FileListEntry],
    flags: u32,
) -> Result<TransferResult> {
    if entries.is_empty() {
        bail!("file list is empty");
    }

    let tx_id_hex = bytes_to_hex(&tx_id);

    // Planning pass (metadata only — no file contents read yet).
    let mut planned_files: Vec<PlannedFile> = Vec::with_capacity(entries.len());
    let mut planned_shards: Vec<PlannedShard> = Vec::new();
    let mut total_bytes = 0u64;
    let mut next_seq: u64 = 1;
    let pack_enabled = cfg.pack_size > 0;
    let pack_threshold = if pack_enabled { cfg.pack_file_max } else { 0 };
    let mut packer = PackPlanner::new(cfg.pack_size.max(4096));

    for entry in entries {
        let meta = std::fs::metadata(&entry.src).with_context(|| format!("stat {}", entry.src))?;
        let size = meta.len();
        let resolved_dest = if entry.dest.starts_with('/') {
            entry.dest.clone()
        } else {
            let root = dest_root.trim_end_matches('/');
            format!("{}/{}", root, entry.dest)
        };
        total_bytes += size;

        if pack_enabled && (size as usize) < pack_threshold {
            let rec_size = PackPlanner::record_size(&resolved_dest, size);
            if packer.would_exceed(rec_size) {
                planned_shards.push(packer.take());
            }
            if packer.is_empty() {
                packer.start(next_seq);
                next_seq += 1;
            }
            packer.push(PackRecord {
                dest_path: resolved_dest.clone(),
                source: std::path::PathBuf::from(&entry.src),
                size,
            });
            planned_files.push(PlannedFile {
                dest_path: resolved_dest,
                size,
                shard_start: packer.shard_seq,
                shard_count: 1,
            });
        } else {
            if !packer.is_empty() {
                planned_shards.push(packer.take());
            }
            let shard_start = next_seq;
            let mut shard_count = 0u64;
            if size == 0 {
                planned_shards.push(PlannedShard::Empty {
                    shard_seq: next_seq,
                });
                next_seq += 1;
                shard_count = 1;
            } else {
                let mut offset = 0u64;
                while offset < size {
                    let chunk_len = std::cmp::min(cfg.shard_size as u64, size - offset);
                    planned_shards.push(PlannedShard::NonPacked {
                        shard_seq: next_seq,
                        source: std::path::PathBuf::from(&entry.src),
                        offset,
                        len: chunk_len,
                    });
                    next_seq += 1;
                    shard_count += 1;
                    offset += chunk_len;
                }
            }
            planned_files.push(PlannedFile {
                dest_path: resolved_dest,
                size,
                shard_start,
                shard_count,
            });
        }
    }
    if !packer.is_empty() {
        planned_shards.push(packer.take());
    }

    let total_shards = next_seq - 1;
    let file_count = entries.len() as u64;

    let manifest_files: Vec<ManifestFile> = planned_files
        .into_iter()
        .map(|p| ManifestFile {
            path: p.dest_path,
            size: p.size,
            shard_start: p.shard_start,
            shard_count: p.shard_count,
        })
        .collect();

    let manifest_json = serde_json::to_vec(&Manifest {
        dest_root: dest_root.to_string(),
        file_count,
        total_bytes,
        total_shards,
        files: manifest_files,
    })?;

    let mut c = Connection::connect(&cfg.addr)?;
    let begin_ack = send_and_expect(
        &mut c,
        FrameType::BeginTx,
        &tx_meta_buf_flags(tx_id, 2, flags, &manifest_json),
        FrameType::BeginTxAck,
    )?;
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);

    // `shards_sent` reflects this call's transmission; skipped shards
    // (from a resumed prior attempt) are excluded.
    let mut shards_sent = 0u64;
    {
        let mut sender = PipelinedSender::new(&mut c, cfg, tx_id, total_shards);
        for ps in &planned_shards {
            let seq = planned_shard_seq(ps);
            if seq <= last_acked_shard {
                continue;
            }
            let body = materialise_body(ps)?;
            let (record_count, flags) = planned_shard_meta(ps);
            shards_sent += 1;
            let wire_len = body.len() as u64;
            sender.send_with(seq, &body, record_count, flags)?;
            if let Some(p) = &cfg.progress_bytes {
                p.fetch_add(wire_len, std::sync::atomic::Ordering::Relaxed);
            }
        }
        sender.drain()?;
    }

    let commit_ack = send_and_expect(
        &mut c,
        FrameType::CommitTx,
        &tx_meta_buf(tx_id, 0, b""),
        FrameType::CommitTxAck,
    )?;

    Ok(TransferResult {
        tx_id_hex,
        shards_sent,
        bytes_sent: total_bytes,
        dest: dest_root.to_string(),
        commit_ack_body: String::from_utf8_lossy(&commit_ack).into_owned(),
    })
}

// ─── Resumable wrappers ──────────────────────────────────────────────────────
//
// These mirror `transfer_file_resumable` for the dir and file-list paths:
// retries set `TX_FLAG_RESUME` and reuse the tx_id so the payload's
// journal can report `last_acked_shard`. The first attempt's flags are
// controlled by `initial_flags`:
//
//   - `initial_flags = 0` — the common "fresh upload" case. A random or
//     caller-minted tx_id with no prior state on the payload. The first
//     BEGIN_TX takes the payload's fresh-allocation branch.
//   - `initial_flags = TX_FLAG_RESUME` — the "user-initiated resume"
//     case. The caller has a tx_id they believe the payload's journal
//     still carries (e.g., our client's cross-session resume flow
//     persists the tx_id and re-supplies it on the next upload attempt).
//     Sending `TX_FLAG_RESUME` on the very first BEGIN_TX signals
//     "adopt the existing entry, preserve partial data" instead of
//     falling into the payload's restart-in-place branch (which would
//     destroy the on-disk tmp + reset shards_received).
//
// Backoff between attempts is exponential (500 ms → 1 s → 2 s → 4 s
// capped). Non-retryable errors short-circuit the loop.

/// Shared retry-loop helper used by the resumable wrappers.
fn resumable_retry<F>(
    max_retries: u32,
    label: &str,
    initial_flags: u32,
    mut attempt_fn: F,
) -> Result<TransferResult>
where
    F: FnMut(u32) -> Result<TransferResult>,
{
    let mut attempt: u32 = 0;
    let mut prior_failures: Vec<String> = Vec::new();
    loop {
        // Attempt 0 honors the caller's requested initial_flags; all
        // retries unconditionally set TX_FLAG_RESUME (the payload's
        // journal, if it has anything, must carry last_acked_shard).
        let flags = if attempt == 0 {
            initial_flags
        } else {
            TX_FLAG_RESUME
        };
        match attempt_fn(flags) {
            Ok(r) => {
                if !prior_failures.is_empty() {
                    eprintln!(
                        "[resume] {label} succeeded on attempt {} after prior: {}",
                        attempt,
                        prior_failures.join(" | ")
                    );
                }
                return Ok(r);
            }
            Err(e) => {
                if attempt >= max_retries || !is_retryable_transfer_error(&e) {
                    if !prior_failures.is_empty() {
                        return Err(e.context(format!(
                            "{label} gave up after {attempt} retries. Prior: {}",
                            prior_failures.join(" | ")
                        )));
                    }
                    return Err(e);
                }
                let backoff_ms = 500u64.saturating_mul(1u64 << attempt.min(4));
                eprintln!("[resume] {label} attempt {attempt} failed ({e:#}); retrying in {backoff_ms} ms");
                prior_failures.push(format!("attempt {attempt}: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                attempt += 1;
            }
        }
    }
}

/// `transfer_dir` with automatic resume-on-network-drop.
///
/// `initial_flags` controls the very first BEGIN_TX: pass `0` for a
/// fresh upload (random or newly-minted tx_id), or `TX_FLAG_RESUME`
/// when you're handing in a tx_id that the payload is expected to
/// already know about (user-initiated resume). Retries always use
/// `TX_FLAG_RESUME`. See the retry-contract block above `resumable_retry`.
pub fn transfer_dir_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    src_dir: &Path,
    max_retries: u32,
    initial_flags: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_dir", initial_flags, |flags| {
        transfer_dir_with_flags(cfg, tx_id, dest_root, src_dir, flags)
    })
}

/// `transfer_file_list` with automatic resume-on-network-drop.
/// See `transfer_dir_resumable` for the `initial_flags` contract.
pub fn transfer_file_list_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    entries: &[FileListEntry],
    max_retries: u32,
    initial_flags: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_file_list", initial_flags, |flags| {
        transfer_file_list_with_flags(cfg, tx_id, dest_root, entries, flags)
    })
}
