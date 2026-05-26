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
/// Default resume-on-drop retries for whole-transfer wrappers (folder,
/// file-list, single-file). One fresh attempt + this many RESUME retries.
///
/// Sized to outlast the PAYLOAD's serial transfer-accept loop being briefly
/// unable to `accept()` a reconnect while it's still draining the dropped
/// connection (its `SO_RCVTIMEO` is 120s in the worst case of a true network
/// partition; in the common case — engine closes the socket, or the payload
/// finishes its current shard write — it frees in seconds). Folder + file-list
/// uploads previously used only 2 here while single-file used 5, so a single
/// transient blip killed a multi-hour folder upload that single-file shrugged
/// off. Paired with the raised backoff cap in `resumable_retry`. A genuinely
/// dead PS5 still fails fast: `ConnectionRefused` is non-retryable.
pub const DEFAULT_RESUME_RETRIES: u32 = 6;
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
    /// Optional per-file progress counter — `fetch_add(1)` once per file as
    /// it's pulled into a pack frame (packed shards) or as its first chunk
    /// is materialised (non-packed). Sized at one bump per *source file* so
    /// a 5-shard non-packed file counts as 1 and an 8 MiB packed shard with
    /// 200 records counts as 200. Lets the UI show a counter that climbs
    /// continuously inside the read-many-small-files phase instead of
    /// jumping per packed-shard ACK — the latter looked like
    /// "start → finished" on 46 k-file game folders. `None` keeps legacy
    /// callers' behaviour (no per-file tick).
    pub progress_files: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Optional outbound bandwidth cap, in bytes per second. `None` =
    /// unlimited (current behaviour). When set, the transfer loop
    /// sleeps after each shard's wire write to bring the running
    /// average below this cap. Useful when uploading from a connection
    /// that's also carrying video calls / game streaming and you want
    /// to leave headroom.
    ///
    /// Implementation: token-bucket-style sleep — track wall time +
    /// bytes written since the throttle started, sleep enough to
    /// bring the average down to the cap. Coarse-grained (sleeps
    /// happen between shards, not within a single shard's TCP write),
    /// so the actual rate may briefly spike above the cap by one
    /// shard's worth of bytes.
    pub bandwidth_cap_bps: Option<u64>,
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
            progress_files: None,
            bandwidth_cap_bps: None,
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
/// them from a prior (interrupted) connection. The wire-level resume
/// flow lives in the FrameType doc comments (`BeginTxAck` body bits).
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

/// Reject a payload-reported resume cursor that's past the end of the
/// current plan. A `last_acked_shard > total_shards` makes every
/// `seq <= last_acked_shard` skip fire, so the send loop transmits ZERO
/// shards and the caller proceeds straight to CommitTx — a "ghost
/// commit" that finalizes a transfer this attempt never sent. This
/// happens when a reused `tx_id` still references the payload's journal
/// from a different/larger prior upload. ALL transfer paths (single
/// file, dir, file-list, zip) share the same resume contract, so they
/// must share this guard — keeping it in one place stops the multi-file
/// paths from drifting out of sync with the single-file ones (they did:
/// the guard was originally only on the single-file paths).
fn guard_last_acked(last_acked_shard: u64, total_shards: u64) -> Result<()> {
    if total_shards > 0 && last_acked_shard > total_shards {
        bail!(
            "payload reported last_acked_shard={last_acked_shard} > total_shards={total_shards}; \
             refusing to commit a transfer that hasn't been transmitted"
        );
    }
    Ok(())
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
    /// Bandwidth throttle state. `None` = unlimited (no sleeps).
    /// `Some(BandwidthThrottle)` = sleep between shards to enforce
    /// the configured bytes-per-second ceiling.
    throttle: Option<BandwidthThrottle>,
}

/// Token-bucket throttle. `allowance` (in bytes) refills continuously at
/// `cap_bps`, capped at a one-second burst; each send spends `n` tokens and
/// sleeps off any deficit.
///
/// Why a bucket and not a cumulative bytes/elapsed average: the old version
/// compared total `bytes_sent` against `cap_bps * elapsed_since_start`. Once
/// real elapsed time ran ahead of the byte target — which any stall (slow
/// ACK, network hiccup, a long drain) guarantees — it concluded "behind
/// pace" forever and stopped sleeping, silently abandoning the cap for the
/// rest of the transfer. That's the opposite of what the cap is for (leave
/// headroom when the link is contended). The bucket can't bank more than one
/// second of unspent credit, so a stall grants at most a one-second burst
/// and pacing resumes immediately after.
struct BandwidthThrottle {
    cap_bps: f64,
    /// Available send budget in bytes. May go negative between the spend and
    /// the compensating sleep; carried forward so oversized shards still get
    /// fully paced across calls.
    allowance: f64,
    last_refill: std::time::Instant,
}

impl BandwidthThrottle {
    fn new(cap_bps: u64) -> Self {
        let cap = cap_bps.max(1) as f64;
        Self {
            cap_bps: cap,
            allowance: cap, // start with one second of burst headroom
            last_refill: std::time::Instant::now(),
        }
    }

    /// Pure pacing math: refill against `now`, spend `n`, and return how long
    /// the caller must sleep. Split out from the wall-clock sleep so it's
    /// unit-testable with synthetic timestamps.
    fn charge(&mut self, n: usize, now: std::time::Instant) -> std::time::Duration {
        let dt = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.last_refill = now;
        // Refill, capped at one second of burst — this clamp is the fix: a
        // long idle stretch can't accumulate unbounded credit.
        self.allowance = (self.allowance + dt * self.cap_bps).min(self.cap_bps);
        self.allowance -= n as f64;
        if self.allowance < 0.0 {
            // Sleep off the deficit, clamped to 1s so one huge shard against
            // a low cap is paced over several short sleeps. Credit the time
            // we'll sleep so the deficit isn't also charged on the next call.
            let deficit_secs = (-self.allowance / self.cap_bps).min(1.0);
            self.allowance += deficit_secs * self.cap_bps;
            std::time::Duration::from_secs_f64(deficit_secs)
        } else {
            std::time::Duration::ZERO
        }
    }

    /// Record `n` bytes just sent and sleep if we're ahead of pace.
    /// Called after each successful shard send.
    fn account_and_pace(&mut self, n: usize) {
        let sleep_for = self.charge(n, std::time::Instant::now());
        if !sleep_for.is_zero() {
            std::thread::sleep(sleep_for);
        }
    }
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
        let throttle = cfg.bandwidth_cap_bps.map(BandwidthThrottle::new);
        Self {
            c,
            inflight: VecDeque::with_capacity(max_inflight_shards),
            inflight_bytes: 0,
            max_inflight_shards,
            max_inflight_bytes,
            tx_id,
            total_shards,
            throttle,
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
        // Pace AFTER the frame is on the wire — gives a tighter bound
        // than sleeping before the send (where outstanding inflight
        // bytes from earlier shards would skew the math).
        if let Some(t) = self.throttle.as_mut() {
            t.account_and_pace(data.len());
        }
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

// ─── Abort ────────────────────────────────────────────────────────────────────

/// Result of an `abort_transaction` call. Mostly informational —
/// callers usually treat the function as fire-and-forget once they
/// see it didn't error.
#[derive(Debug, Clone)]
pub struct AbortResult {
    /// Raw payload of the ABORT_TX_ACK frame (a small JSON object —
    /// `{"aborted":true,"tx_id":"…","active_transactions":N}` on
    /// success). Returned as bytes so the caller decides whether to
    /// parse, log, or discard.
    pub ack_body: Vec<u8>,
}

/// Send `ABORT_TX` for the given tx_id and wait for the ACK.
///
/// Opens a fresh connection — typically called on the mgmt port
/// (`:9114`) while a transfer is in flight on the transfer port
/// (`:9113`). The payload's runtime acquires the tx entry
/// exclusively, marks the entry `aborted`, releases its resources,
/// and ACKs. Any subsequent `STREAM_SHARD` for the same tx on the
/// transfer connection will fail with `tx_not_found`.
///
/// Returns `Err(tx_not_found)` if the tx_id isn't known to the
/// payload — typically because it never reached BEGIN_TX, the
/// payload was restarted, or the abort raced a successful COMMIT_TX.
///
/// Wiring this to a user-facing "Cancel" button requires verifying on
/// PS5 hardware that the abort actually preempts a long-running
/// shard write (the exclusive-acquire serialises with shard handling,
/// but the wall-clock latency between request and effect depends on
/// shard size + NVMe write speed). The mock-server test exercises the
/// frame round-trip; end-to-end "cancel mid-upload" needs a real
/// console.
pub fn abort_transaction(addr: &str, tx_id: [u8; 16]) -> Result<AbortResult> {
    use anyhow::Context;
    use ftx2_proto::FrameType;
    // Hex tx_id is the lever for log diagnosis — without it the
    // user sees "read frame header: ..." with no clue which
    // transaction was being aborted on which host. Format once
    // and reuse on every error branch.
    let tx_hex: String = tx_id.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    let body = tx_meta_buf(tx_id, 0, b"");
    let mut c = Connection::connect(addr)
        .with_context(|| format!("ABORT_TX connect to {addr} for tx_id={tx_hex}"))?;
    c.send_frame(FrameType::AbortTx, &body)
        .with_context(|| format!("ABORT_TX send for tx_id={tx_hex}"))?;
    let (hdr, body) = c
        .recv_frame()
        .with_context(|| format!("ABORT_TX recv ACK for tx_id={tx_hex}"))?;
    match hdr.frame_type() {
        Ok(FrameType::AbortTxAck) => Ok(AbortResult { ack_body: body }),
        Ok(FrameType::Error) => {
            let msg = String::from_utf8_lossy(&body).into_owned();
            anyhow::bail!("ABORT_TX error for tx_id={tx_hex}: {msg}");
        }
        Ok(other) => {
            anyhow::bail!("unexpected response to ABORT_TX (tx_id={tx_hex}): {other:?}");
        }
        Err(e) => {
            anyhow::bail!(
                "ABORT_TX response had undecodable frame_type ({}) for tx_id={tx_hex}: {e:?}",
                hdr.frame_type
            );
        }
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
    // A 0-byte file still needs ONE (empty) shard: the payload's direct
    // writer only creates the `.ps5up2-tmp` file when it receives a shard,
    // so with zero shards COMMIT's temp→final rename fails with
    // `direct_rename_failed` (confirmed on hardware). The dir path handles
    // this via PlannedShard::Empty; mirror it here.
    let total_shards = if data.is_empty() {
        1
    } else {
        data.chunks(cfg.shard_size).count() as u64
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
    // `last_acked_shard` is non-zero only when a prior (interrupted)
    // connection for this tx_id reached the payload journal. For fresh
    // transfers (flags=0) it's always 0 and the skip branch becomes
    // unreachable; for resume attempts (flags=TX_FLAG_RESUME) it's how
    // we know where to pick up.
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);
    // Defensive: shared guard (see guard_last_acked). A bogus
    // last_acked_shard > total_shards would otherwise ghost-commit here.
    guard_last_acked(last_acked_shard, total_shards)?;

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
        if data.is_empty() {
            // Single empty shard so the payload materialises the 0-byte file.
            if last_acked_shard < 1 {
                sender.send(1, &[])?;
                shards_sent += 1;
            }
        } else {
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
/// Slice-based counterpart to `transfer_file_path_resumable`. Same
/// `initial_flags` contract — see that function's doc comment.
pub fn transfer_file_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    data: &[u8],
    max_retries: u32,
    initial_flags: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_file", initial_flags, |flags| {
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
    // A 0-byte file needs ONE empty shard (not zero) so the payload's direct
    // writer creates the temp file that COMMIT renames into place — otherwise
    // commit fails with `direct_rename_failed` (confirmed on hardware). With
    // total_shards=1 the size-driven send loop below reads a 0-length chunk
    // and emits the empty shard naturally.
    let total_shards = if total_bytes == 0 {
        1
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
    // Defensive: a payload reporting last_acked_shard >= total_shards
    // would otherwise cause the engine to skip the entire send loop
    // and immediately COMMIT — a "ghost commit" that finalises a tx
    // with no actual bytes from this attempt. That's only valid if
    // the prior attempt truly completed all shards; otherwise it's a
    // payload bookkeeping bug we should surface, not silently
    // collude with. (Specifically allow `==` since a fully-acked
    // prior attempt that lost only the COMMIT_TX round-trip is a
    // legitimate "all shards in journal, just need to commit" case.)
    guard_last_acked(last_acked_shard, total_shards)?;

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

/// `transfer_file_path` with automatic resume-on-network-drop.
///
/// `initial_flags` controls the very first BEGIN_TX: pass `0` for a
/// fresh upload (random or newly-minted tx_id), or `TX_FLAG_RESUME`
/// when handing in a tx_id that the payload is expected to already
/// know about (user-initiated resume after a prior failure). Retries
/// always use `TX_FLAG_RESUME`. Mirrors `transfer_dir_resumable`'s
/// contract.
///
/// The payload's `TX_FLAG_RESUME` is a no-op when the tx_id is unknown
/// (falls through to fresh-allocate), so it's safe to always pass
/// `TX_FLAG_RESUME` even on the first attempt — the flag is effectively
/// "adopt the existing entry if you have one." That's the pattern the
/// folder uploader uses; the file uploader follows it now.
pub fn transfer_file_path_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest: &str,
    src: &Path,
    max_retries: u32,
    initial_flags: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_file_path", initial_flags, |flags| {
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
fn materialise_body(
    ps: &PlannedShard,
    progress_files: Option<&std::sync::atomic::AtomicU64>,
) -> Result<Vec<u8>> {
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
            // Count one file-start per non-packed FIRST chunk (offset==0).
            // Multi-shard files chunk into multiple PlannedShards but the
            // user-meaningful unit is the source file, so only the first
            // chunk bumps the per-file counter. Subsequent chunks advance
            // progress_bytes (file is in flight) without re-bumping files.
            if *offset == 0 {
                if let Some(pf) = progress_files {
                    pf.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
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
                // Packed-shard record header is two LE u32s (path_len, size).
                // Both fields are u32 by wire spec; the planner only routes
                // small files (PACK_FILE_MAX = 128 KiB) into the packed
                // path, but assert the bound at the cast site so a future
                // change to PACK_FILE_MAX can't silently truncate >4 GiB
                // sizes / paths into a corrupted record stream.
                let path_len = u32::try_from(p.len())
                    .with_context(|| format!("pack record path too long: {}", r.dest_path))?;
                let rec_size = u32::try_from(r.size).with_context(|| {
                    format!(
                        "pack record size {} exceeds u32 (file: {})",
                        r.size, r.dest_path
                    )
                })?;
                buf.extend_from_slice(&path_len.to_le_bytes());
                buf.extend_from_slice(&rec_size.to_le_bytes());
                buf.extend_from_slice(p);
                let mut f = std::fs::File::open(&r.source)
                    .with_context(|| format!("open pack record {}", r.source.display()))?;
                let start = buf.len();
                buf.resize(start + r.size as usize, 0);
                f.read_exact(&mut buf[start..])
                    .with_context(|| format!("read pack record {}", r.source.display()))?;
                // Per-record file-progress bump. Bumping HERE (inside the
                // per-record loop) is what makes the UI's file counter
                // climb smoothly during the pack-frame build for many-
                // tiny-files folders, instead of jumping by ~200 every
                // time a packed shard finally goes out. Bytes still
                // advance per-shard via progress_bytes (the wire-truth
                // counter); this is the user-perceived "we're working"
                // signal.
                if let Some(pf) = progress_files {
                    pf.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
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

/// File-data bytes a shard carries, EXCLUDING packed-record framing.
/// The live-progress denominator is the sum of uncompressed file sizes
/// (`walk_plan` / file-size sum), so the numerator must count file bytes
/// too — a packed shard's wire body adds `[u32 path_len][u32 size][path]`
/// per record, and counting that (body.len()) pushed the progress bar
/// past 100% on directories of many small files.
fn planned_shard_data_len(ps: &PlannedShard) -> u64 {
    match ps {
        PlannedShard::Empty { .. } => 0,
        PlannedShard::NonPacked { len, .. } => *len,
        PlannedShard::Packed { records, .. } => records.iter().map(|r| r.size).sum(),
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
    // Same ghost-commit guard as the single-file paths (the multi-file
    // paths previously lacked it). See guard_last_acked.
    guard_last_acked(last_acked_shard, total_shards)?;

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
            let body = materialise_body(ps, cfg.progress_files.as_deref())?;
            let (record_count, flags) = planned_shard_meta(ps);
            shards_sent += 1;
            sender.send_with(seq, &body, record_count, flags)?;
            if let Some(p) = &cfg.progress_bytes {
                // File-data bytes only — packed shards carry per-record
                // framing that isn't part of the plan total (the progress
                // denominator), so counting body.len() pushed the bar past
                // 100% on dirs of many small files.
                p.fetch_add(
                    planned_shard_data_len(ps),
                    std::sync::atomic::Ordering::Relaxed,
                );
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
    // Same ghost-commit guard as the single-file paths. See guard_last_acked.
    guard_last_acked(last_acked_shard, total_shards)?;

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
            let body = materialise_body(ps, cfg.progress_files.as_deref())?;
            let (record_count, flags) = planned_shard_meta(ps);
            shards_sent += 1;
            sender.send_with(seq, &body, record_count, flags)?;
            if let Some(p) = &cfg.progress_bytes {
                // File-data bytes only — packed shards carry per-record
                // framing that isn't part of the plan total (the progress
                // denominator), so counting body.len() pushed the bar past
                // 100% on dirs of many small files.
                p.fetch_add(
                    planned_shard_data_len(ps),
                    std::sync::atomic::Ordering::Relaxed,
                );
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
                // 500ms → 1s → 2s → 4s → 8s → 16s (capped). The 16s cap (was
                // 8s) widens the total retry window so it can outlast the
                // payload's serial accept loop briefly being unable to take a
                // reconnect while it drains the dropped connection.
                let backoff_ms = 500u64.saturating_mul(1u64 << attempt.min(5));
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

// ════════════════════════════════════════════════════════════════════════════
// Zip-archive transfer — stream-decompress a .zip straight into the FTX2 pipe
// ════════════════════════════════════════════════════════════════════════════
//
// The goal (feature request: "compress game dumps … extract them directly to
// the console"): let users keep a game as a single `.zip` on the PC and upload
// it so the files land *already extracted* on the PS5 — no temp copy of the
// whole game, and no payload changes (the console receives raw files exactly as
// it does for a folder upload).
//
// ── Why this is just `transfer_dir` with a different byte source ─────────────
//
// `transfer_dir` plans shards from `(file, offset, len)` and materialises each
// shard lazily by `seek + read`. A DEFLATE-compressed zip entry is NOT
// seekable, so we can't seek to an arbitrary offset of the *decompressed*
// stream on demand. The fix is the same one SimpleZipDrive uses: inflate a
// whole entry once into a seekable cache (RAM for small entries, a temp file
// for large ones), then serve the shard's byte range out of that cache.
//
// ── Why a single-entry cache suffices (Strategy C) ──────────────────────────
//
// Shards are planned and sent in entry order, and all of an entry's shards are
// contiguous. So the cache only ever needs to hold the *current* entry: when a
// shard for a different entry arrives, we evict and inflate the new one. This
// bounds host memory/temp to one entry's uncompressed size and makes resume
// trivial — a resumed mid-entry shard just re-inflates that entry locally
// (cheap, no re-download) and reads the resumed range. Tiny entries that get
// coalesced into a packed shard are inflated whole on the spot (they're below
// `pack_file_max`, i.e. 128 KiB) and never touch the single-entry cache.

/// Entries whose *uncompressed* size is at or above this inflate to a temp
/// file; smaller entries inflate into a RAM buffer. 512 MiB matches
/// SimpleZipDrive's default per-file RAM threshold. Only one entry is ever
/// cached at a time, so this is the peak extra RAM the zip path can use.
pub const DEFAULT_ZIP_ENTRY_RAM_THRESHOLD: u64 = 512 * 1024 * 1024;

/// Distinct cache-file id per `ZipMaterialiser`, so two concurrent zip
/// transfers in the same process can't collide on `pid-index` temp names.
static ZIP_MATERIALISER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sanitize a zip entry name into a safe POSIX-relative path (forward
/// slashes for the PS5). Rejects traversal (`..`), NUL, backslash segments,
/// and absolute/empty paths — the same zip-slip defense as the client's
/// `save_archive::sanitize_entry`, returning a string instead of a host
/// `PathBuf`. Returns `None` for directory-only or unsafe names.
fn sanitize_zip_entry(name: &str) -> Option<String> {
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

/// One file entry inside the source zip, resolved from the central directory.
/// `entry_index` indexes back into `ZipArchive::by_index`.
struct ZipPlanFile {
    dest_path: String,
    entry_index: usize,
    size: u64,
}

/// One packed record sourced from a zip entry (parallels `PackRecord`, which
/// is file-backed).
struct ZipPackRecord {
    dest_path: String,
    entry_index: usize,
    size: u64,
}

/// A planned zip shard. Parallels `PlannedShard` but sources bytes from zip
/// entries via a `ZipMaterialiser` instead of from files on disk.
enum ZipShard {
    Empty {
        shard_seq: u64,
    },
    /// A slice `[offset, offset+len)` of one entry's *uncompressed* bytes.
    /// `entry_size` is the entry's full uncompressed length (from the central
    /// directory) — needed so the materialiser sizes its cache and validates
    /// the inflate against the true size, not just this shard's slice.
    NonPacked {
        shard_seq: u64,
        entry_index: usize,
        entry_size: u64,
        offset: u64,
        len: u64,
    },
    /// Many small entries coalesced into one packed shard body.
    Packed {
        shard_seq: u64,
        records: Vec<ZipPackRecord>,
    },
}

fn zip_shard_seq(zs: &ZipShard) -> u64 {
    match zs {
        ZipShard::Empty { shard_seq }
        | ZipShard::NonPacked { shard_seq, .. }
        | ZipShard::Packed { shard_seq, .. } => *shard_seq,
    }
}

fn zip_shard_meta(zs: &ZipShard) -> (u32, u32) {
    match zs {
        ZipShard::Empty { .. } | ZipShard::NonPacked { .. } => (1, 0),
        ZipShard::Packed { records, .. } => (records.len() as u32, SHARD_FLAG_PACKED),
    }
}

/// File-data bytes a zip shard carries, excluding packed-record framing.
/// See `planned_shard_data_len` for why the progress numerator needs this.
fn zip_shard_data_len(zs: &ZipShard) -> u64 {
    match zs {
        ZipShard::Empty { .. } => 0,
        ZipShard::NonPacked { len, .. } => *len,
        ZipShard::Packed { records, .. } => records.iter().map(|r| r.size).sum(),
    }
}

/// The currently-inflated entry, kept seekable so shard byte-ranges can be
/// served without re-decompressing.
enum ZipEntryCache {
    Mem {
        index: usize,
        data: Vec<u8>,
    },
    Tmp {
        index: usize,
        file: std::fs::File,
        path: std::path::PathBuf,
    },
}

impl ZipEntryCache {
    fn index(&self) -> usize {
        match self {
            ZipEntryCache::Mem { index, .. } | ZipEntryCache::Tmp { index, .. } => *index,
        }
    }
}

/// Owns the open archive and the single-entry inflate cache. Drops/evicts the
/// temp file on drop so an aborted transfer doesn't leak cache files.
struct ZipMaterialiser {
    archive: zip::ZipArchive<std::io::BufReader<std::fs::File>>,
    ram_threshold: u64,
    tmp_dir: std::path::PathBuf,
    id: u64,
    cache: Option<ZipEntryCache>,
}

impl ZipMaterialiser {
    fn new(
        archive: zip::ZipArchive<std::io::BufReader<std::fs::File>>,
        ram_threshold: u64,
        tmp_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            archive,
            ram_threshold,
            tmp_dir,
            id: ZIP_MATERIALISER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            cache: None,
        }
    }

    /// Drop the cached entry, deleting the temp file if there was one.
    fn evict(&mut self) {
        if let Some(ZipEntryCache::Tmp { path, .. }) = self.cache.take() {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Turn a `by_index` failure into an actionable message. The common
    /// real case is an entry compressed with a method we don't build
    /// support for (Deflate64 / BZip2 / LZMA / Zstd / AES). The `zip`
    /// crate surfaces a terse `UnsupportedArchive`, which on the engine's
    /// HTTP 400 the user saw as the baffling "read zip entry 0"
    /// (Constantine-HD: "deflate64 and lzma … always error"). Rewrite it
    /// to name the offending entry + method and how to fix it. Non-
    /// compression failures (I/O, corruption) pass through with the
    /// original `open zip entry N` context.
    fn map_open_error(&mut self, index: usize, e: zip::result::ZipError) -> anyhow::Error {
        if !matches!(e, zip::result::ZipError::UnsupportedArchive(_)) {
            return anyhow::Error::new(e).context(format!("open zip entry {index}"));
        }
        let name = self
            .archive
            .name_for_index(index)
            .unwrap_or("<unknown>")
            .to_string();
        // by_index_raw skips decoder construction, so it succeeds where
        // by_index failed — letting us name the actual method. One seek,
        // only on this already-failing path.
        let method = self
            .archive
            .by_index_raw(index)
            .ok()
            .map(|zf| format!("{:?}", zf.compression()))
            .unwrap_or_else(|| "an unsupported method".to_string());
        anyhow::anyhow!(
            "zip entry \"{name}\" uses compression method {method}, which ps5upload \
             can't decompress. Only STORE and standard DEFLATE zips are supported — \
             re-create the archive with standard Deflate (e.g. `zip -r`, 7-Zip's \
             \"Deflate\" method, or Windows \"Send to → Compressed (zipped) folder\"). \
             Deflate64, LZMA, BZip2, Zstd and AES-encrypted zips won't work."
        )
    }

    /// Ensure `index` is the cached entry, inflating it whole if not. The
    /// inflated length is checked against the central-directory size we
    /// planned with — a mismatch means a corrupt/lying zip and fails loudly
    /// rather than silently truncating the file delivered to the PS5.
    fn ensure_entry(&mut self, index: usize, expected: u64) -> Result<()> {
        if self.cache.as_ref().map(ZipEntryCache::index) == Some(index) {
            return Ok(());
        }
        self.evict();
        use std::io::Read;
        // The by_index Result borrows self.archive AND ZipFile has a Drop
        // impl, so the matched temporary keeps the borrow live across the
        // whole match — meaning no arm can call self.map_open_error (a
        // second &mut self borrow). So the Err arm only stashes the OWNED
        // error; we build the message after the match statement, once the
        // temporary (and its borrow) is gone. The Ok arm does the whole
        // inflate in-place — self.cache / tmp_dir / id are disjoint fields
        // from self.archive, so they're usable while it's borrowed.
        let mut open_err: Option<zip::result::ZipError> = None;
        match self.archive.by_index(index) {
            Ok(mut zf) => {
                if expected < self.ram_threshold {
                    let mut data = Vec::with_capacity(expected as usize);
                    zf.read_to_end(&mut data)
                        .with_context(|| format!("inflate zip entry {index} to memory"))?;
                    if data.len() as u64 != expected {
                        bail!(
                            "zip entry {index} inflated to {} bytes, central directory said {expected}",
                            data.len()
                        );
                    }
                    self.cache = Some(ZipEntryCache::Mem { index, data });
                } else {
                    let path = self.tmp_dir.join(format!(
                        "ps5upload-zipcache-{}-{}-{index}.tmp",
                        std::process::id(),
                        self.id
                    ));
                    let mut wf = std::fs::File::create(&path)
                        .with_context(|| format!("create zip cache file {}", path.display()))?;
                    let written = std::io::copy(&mut zf, &mut wf).with_context(|| {
                        format!("inflate zip entry {index} to {}", path.display())
                    })?;
                    drop(zf);
                    wf.sync_all().ok();
                    drop(wf);
                    if written != expected {
                        let _ = std::fs::remove_file(&path);
                        bail!("zip entry {index} inflated to {written} bytes, central directory said {expected}");
                    }
                    let file = std::fs::File::open(&path)
                        .with_context(|| format!("reopen zip cache file {}", path.display()))?;
                    self.cache = Some(ZipEntryCache::Tmp { index, file, path });
                }
            }
            Err(e) => open_err = Some(e),
        }
        if let Some(e) = open_err {
            return Err(self.map_open_error(index, e));
        }
        Ok(())
    }

    /// Serve `[offset, offset+len)` of entry `index` (uncompressed) from the
    /// cache, inflating the entry first if needed. `entry_size` is the entry's
    /// full uncompressed length, used to size the cache and bounds-check.
    fn read_range(
        &mut self,
        index: usize,
        entry_size: u64,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>> {
        // checked_add: `entry_size` originates from the (untrusted) zip
        // central directory, so guard the bound itself against u64 wrap —
        // not just the slice math below.
        if offset.checked_add(len).is_none_or(|end| end > entry_size) {
            bail!("zip shard range {offset}+{len} exceeds entry {index} size {entry_size}");
        }
        self.ensure_entry(index, entry_size)?;
        use std::io::{Read, Seek, SeekFrom};
        match self.cache.as_mut().expect("ensure_entry populated cache") {
            ZipEntryCache::Mem { data, .. } => {
                let start = offset as usize;
                let end = start
                    .checked_add(len as usize)
                    .context("zip shard range overflow")?;
                let slice = data
                    .get(start..end)
                    .context("zip shard range out of bounds (mem cache)")?;
                Ok(slice.to_vec())
            }
            ZipEntryCache::Tmp { file, .. } => {
                file.seek(SeekFrom::Start(offset))
                    .context("seek zip cache file")?;
                let mut buf = vec![0u8; len as usize];
                file.read_exact(&mut buf).context("read zip cache file")?;
                Ok(buf)
            }
        }
    }

    /// Inflate an entire small entry without disturbing the single-entry
    /// cache. Used for packed records, which are below `pack_file_max`.
    fn read_whole_small(&mut self, index: usize, expected: u64) -> Result<Vec<u8>> {
        use std::io::Read;
        // Same borrow dance as ensure_entry: the Err arm only stashes the
        // owned error; map_open_error runs after the match, once the
        // by_index temporary's archive borrow is gone.
        let mut out: Option<Vec<u8>> = None;
        let mut open_err: Option<zip::result::ZipError> = None;
        match self.archive.by_index(index) {
            Ok(mut zf) => {
                let mut data = Vec::with_capacity(expected as usize);
                zf.read_to_end(&mut data)
                    .with_context(|| format!("inflate packed zip entry {index}"))?;
                if data.len() as u64 != expected {
                    bail!(
                        "packed zip entry {index} inflated to {} bytes, central directory said {expected}",
                        data.len()
                    );
                }
                out = Some(data);
            }
            Err(e) => open_err = Some(e),
        }
        if let Some(e) = open_err {
            return Err(self.map_open_error(index, e));
        }
        Ok(out.expect("Ok arm populates out when there was no open error"))
    }

    /// Materialise one shard's wire body — the zip analogue of
    /// `materialise_body`. Packed bodies use the same
    /// `[u32 path_len][u32 size][path][data]` record layout the payload's
    /// pack parser expects.
    fn body(&mut self, zs: &ZipShard) -> Result<Vec<u8>> {
        match zs {
            ZipShard::Empty { .. } => Ok(Vec::new()),
            ZipShard::NonPacked {
                entry_index,
                entry_size,
                offset,
                len,
                ..
            } => self.read_range(*entry_index, *entry_size, *offset, *len),
            ZipShard::Packed { records, .. } => {
                // Don't pin a large cached entry while emitting tiny records.
                self.evict();
                let mut buf = Vec::new();
                for r in records {
                    let data = self.read_whole_small(r.entry_index, r.size)?;
                    let p = r.dest_path.as_bytes();
                    let path_len = u32::try_from(p.len())
                        .with_context(|| format!("packed zip path too long: {}", r.dest_path))?;
                    let rec_size = u32::try_from(r.size).with_context(|| {
                        format!("packed zip size {} exceeds u32: {}", r.size, r.dest_path)
                    })?;
                    buf.extend_from_slice(&path_len.to_le_bytes());
                    buf.extend_from_slice(&rec_size.to_le_bytes());
                    buf.extend_from_slice(p);
                    buf.extend_from_slice(&data);
                }
                Ok(buf)
            }
        }
    }
}

impl Drop for ZipMaterialiser {
    fn drop(&mut self) {
        self.evict();
    }
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
    let compressed_size = std::fs::metadata(zip_path)
        .with_context(|| format!("stat zip {}", zip_path.display()))?
        .len();
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("open zip {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("read zip central directory {}", zip_path.display()))?;

    // Metadata-only pass — NO per-entry seeks. The previous version
    // called by_index(i) for EVERY entry; each call seeks to and reads
    // that entry's local header (zip's find_data_start). On a multi-GB
    // game dump over an HDD array that's tens of thousands of random
    // seeks: it pinned the disk at 100% and outran the client's request
    // timeout, and because inspect runs in spawn_blocking the orphaned
    // task kept seeking "until the program closed" (Constantine-HD
    // report). decompressed_size() + file_names() read only the
    // in-memory central directory that ZipArchive::new already parsed.
    //
    // It also means inspect no longer hard-fails on a deflate64/lzma
    // archive — by_index used to build a decoder and return
    // UnsupportedArchive, which surfaced as the baffling HTTP 400 "read
    // zip entry 0". The clear "unsupported compression" message now
    // comes from the transfer path (ZipMaterialiser::map_open_error),
    // where decompression actually happens and the user is committed.
    //
    // total_uncompressed via decompressed_size() reads each entry's
    // central-directory size from memory (no seeks; dirs are 0). Caveat:
    // zip 2.4.2's decompressed_size() returns None if ANY entry sets the
    // data-descriptor bit (general-purpose flag 3) — common with
    // bsdtar/libarchive/streaming zippers (see the Windows-zip-bit3
    // lesson) — even though the central directory still holds the real
    // size. We map None to 0; the Upload card then suppresses the
    // "extracted" figure and shows count-only rather than a
    // contradictory "0 B". We accept that over reintroducing the
    // per-entry local-header seeks (by_index_raw) this rewrite removed to
    // kill the 100%-disk inspect thrash. The actual transfer total +
    // progress denominator come from zip_plan_preview, which is correct
    // regardless. (decompressed_size() can also include a rare zip-slip
    // entry sanitize would drop — negligible for a preview number.)
    let total_uncompressed = archive
        .decompressed_size()
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(0);

    let mut file_count = 0u64;
    // Find the shallowest "<root>/sce_sys/param.json" (fewest path
    // segments) so a wrapped dump (`MyGame/sce_sys/…`) and a root dump
    // (`sce_sys/…`) both resolve to the real game root. Keep the entry's
    // ORIGINAL name so we can look its index back up for the single
    // content read below.
    let mut param_hit: Option<(usize, String, String)> = None; // (depth, original_name, game_root)
    for name in archive.file_names() {
        // Directory entries (trailing '/') don't count as files —
        // matches the old `is_dir()` skip. sanitize_zip_entry keeps
        // "foo/" as "foo", so we must filter dirs by name first.
        if name.ends_with('/') {
            continue;
        }
        let Some(rel) = sanitize_zip_entry(name) else {
            continue;
        };
        file_count += 1;
        if let Some(root) = rel.strip_suffix("sce_sys/param.json") {
            let game_root = root.trim_end_matches('/').to_string();
            let depth = rel.split('/').count();
            if param_hit.as_ref().is_none_or(|(d, _, _)| depth < *d) {
                param_hit = Some((depth, name.to_string(), game_root));
            }
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

    if let Some((_, original_name, game_root)) = param_hit {
        if let Some(idx) = archive.index_for_name(&original_name) {
            use std::io::Read;
            // Cap the inflate: a real param.json is a few KiB, but
            // inspect_zip runs on user-supplied archives just to render
            // the Upload preview, so a crafted entry named
            // sce_sys/param.json that decompresses to gigabytes (zip
            // bomb) must not OOM the engine before any upload even
            // starts. `take` bounds the read. This is the ONLY by_index
            // (one seek + small inflate) left in inspect.
            //
            // Everything here is non-fatal: a param.json that's itself
            // in an unsupported method, won't fit in 4 MiB, or doesn't
            // parse just falls through to the size/count-only preview —
            // the transfer path surfaces any real compression error.
            const MAX_PARAM_JSON: u64 = 4 * 1024 * 1024;
            if let Ok(zf) = archive.by_index(idx) {
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
pub fn zip_plan_preview(zip_path: &Path, excludes: &[String]) -> Result<(u64, Vec<(String, u64)>)> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("open zip {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("read zip central directory {}", zip_path.display()))?;
    let mut files: Vec<(String, u64)> = Vec::new();
    for i in 0..archive.len() {
        let (name, size, is_dir) = {
            // by_index_raw, not by_index: planning only needs central-
            // directory metadata (name/size/is_dir), and the raw variant
            // skips building a decoder, so an unsupported-compression
            // entry doesn't fail planning here — the ZipMaterialiser
            // raises the clear "unsupported compression" error at send
            // time instead (one place, one message).
            let e = archive
                .by_index_raw(i)
                .with_context(|| format!("read zip entry {i}"))?;
            (e.name().to_string(), e.size(), e.is_dir())
        };
        if is_dir {
            continue;
        }
        let Some(rel) = sanitize_zip_entry(&name) else {
            continue;
        };
        if !excludes.is_empty() && crate::excludes::is_excluded_strings(Path::new(&rel), excludes) {
            continue;
        }
        files.push((rel, size));
    }
    // Collapse duplicate paths keeping the last (same rule as
    // transfer_zip_with_opts) so the preview's total + file count match
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

/// Transfer a `.zip`'s contents to `dest_root` on the PS5, decompressing on
/// the host so files land already extracted. Default RAM threshold, fresh
/// transaction. See `transfer_zip_with_opts` for the full contract.
pub fn transfer_zip(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    zip_path: &Path,
) -> Result<TransferResult> {
    transfer_zip_with_opts(
        cfg,
        tx_id,
        dest_root,
        zip_path,
        DEFAULT_ZIP_ENTRY_RAM_THRESHOLD,
        0,
    )
}

/// Full-control zip transfer. `ram_threshold` is the per-entry RAM/temp-spill
/// cutoff (see `DEFAULT_ZIP_ENTRY_RAM_THRESHOLD`); `flags` is the BEGIN_TX
/// flag word (`TX_FLAG_RESUME` to adopt an interrupted tx of the same
/// `tx_id`). Mirrors `transfer_dir_with_flags`: a metadata-only planning pass
/// (central directory) builds the manifest + shard plan, then the send pass
/// materialises each shard's bytes just before it goes out — here by inflating
/// one entry at a time into a seekable cache.
pub fn transfer_zip_with_opts(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    zip_path: &Path,
    ram_threshold: u64,
    flags: u32,
) -> Result<TransferResult> {
    let tx_id_hex = bytes_to_hex(&tx_id);

    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("open zip {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("read zip central directory {}", zip_path.display()))?;

    // ── Planning pass ── central-directory only, no inflation. Enumerate file
    //    entries, sanitize names (zip-slip), apply excludes, sort for a stable
    //    manifest, then split/pack into shards exactly like `transfer_dir`.
    let mut plan_files: Vec<ZipPlanFile> = Vec::new();
    for i in 0..archive.len() {
        let (name, size, is_dir) = {
            // by_index_raw, not by_index: planning only needs central-
            // directory metadata, and the raw variant skips building a
            // decoder — so it doesn't seek per entry the way by_index does
            // (the 100%-disk inspect thrash, here on the transfer's own
            // planning pass) and doesn't hard-fail on an unsupported-
            // compression entry. The clear "unsupported compression"
            // error is raised at send time by ZipMaterialiser, one place.
            let e = archive
                .by_index_raw(i)
                .with_context(|| format!("read zip entry {i}"))?;
            (e.name().to_string(), e.size(), e.is_dir())
        };
        if is_dir {
            continue;
        }
        let Some(rel) = sanitize_zip_entry(&name) else {
            bail!("zip contains an unsafe or invalid entry path: {name:?}");
        };
        if !cfg.excludes.is_empty()
            && crate::excludes::is_excluded_strings(Path::new(&rel), &cfg.excludes)
        {
            continue;
        }
        let dest_path = join_ps5_path(dest_root, Path::new(&rel));
        plan_files.push(ZipPlanFile {
            dest_path,
            entry_index: i,
            size,
        });
    }
    if plan_files.is_empty() {
        bail!(
            "zip has no extractable files (after exclusions): {}",
            zip_path.display()
        );
    }
    // Stable sort by dest_path → duplicates land adjacent. Two distinct zip
    // entries can map to one dest_path (ZIP permits duplicate names, and
    // sanitize collapses `a/./b`, `a//b`, `a/b`). Collapse them keeping the
    // *last* (the copy that would win on disk anyway, since the payload
    // writes shards in manifest order) — otherwise we'd send the bytes
    // twice and report an inflated file_count. Stable sort preserves the
    // original relative order within a run, so "last" is deterministic.
    plan_files.sort_by(|a, b| a.dest_path.cmp(&b.dest_path));
    let before_dedup = plan_files.len();
    {
        let mut deduped: Vec<ZipPlanFile> = Vec::with_capacity(plan_files.len());
        for pf in plan_files.drain(..) {
            if deduped.last().is_some_and(|p| p.dest_path == pf.dest_path) {
                deduped.pop();
            }
            deduped.push(pf);
        }
        plan_files = deduped;
    }
    if plan_files.len() < before_dedup {
        eprintln!(
            "[zip] {} duplicate destination path(s) in archive collapsed (last-writer-wins)",
            before_dedup - plan_files.len()
        );
    }

    let mut planned_files: Vec<ManifestFile> = Vec::with_capacity(plan_files.len());
    let mut planned_shards: Vec<ZipShard> = Vec::new();
    let mut total_bytes = 0u64;
    let mut next_seq: u64 = 1;
    let pack_enabled = cfg.pack_size > 0;
    let pack_threshold = if pack_enabled { cfg.pack_file_max } else { 0 };
    let pack_target = cfg.pack_size.max(4096);
    // Inline packer — `PackPlanner` is file-source-specific, so the zip path
    // keeps its own small accumulator with the identical coalescing rule.
    let mut pack_records: Vec<ZipPackRecord> = Vec::new();
    let mut pack_body: usize = 0;
    let mut pack_seq: u64 = 0;

    for pf in &plan_files {
        total_bytes += pf.size;
        if pack_enabled && (pf.size as usize) < pack_threshold {
            let rec_size = PACKED_RECORD_PREFIX_LEN + pf.dest_path.len() + pf.size as usize;
            if !pack_records.is_empty() && pack_body + rec_size > pack_target {
                planned_shards.push(ZipShard::Packed {
                    shard_seq: pack_seq,
                    records: std::mem::take(&mut pack_records),
                });
                pack_body = 0;
            }
            if pack_records.is_empty() {
                pack_seq = next_seq;
                next_seq += 1;
            }
            pack_body += rec_size;
            pack_records.push(ZipPackRecord {
                dest_path: pf.dest_path.clone(),
                entry_index: pf.entry_index,
                size: pf.size,
            });
            planned_files.push(ManifestFile {
                path: pf.dest_path.clone(),
                size: pf.size,
                shard_start: pack_seq,
                shard_count: 1,
            });
        } else {
            if !pack_records.is_empty() {
                planned_shards.push(ZipShard::Packed {
                    shard_seq: pack_seq,
                    records: std::mem::take(&mut pack_records),
                });
                pack_body = 0;
            }
            let shard_start = next_seq;
            let mut shard_count = 0u64;
            if pf.size == 0 {
                planned_shards.push(ZipShard::Empty {
                    shard_seq: next_seq,
                });
                next_seq += 1;
                shard_count = 1;
            } else {
                let mut offset = 0u64;
                while offset < pf.size {
                    let chunk_len = std::cmp::min(cfg.shard_size as u64, pf.size - offset);
                    planned_shards.push(ZipShard::NonPacked {
                        shard_seq: next_seq,
                        entry_index: pf.entry_index,
                        entry_size: pf.size,
                        offset,
                        len: chunk_len,
                    });
                    next_seq += 1;
                    shard_count += 1;
                    offset += chunk_len;
                }
            }
            planned_files.push(ManifestFile {
                path: pf.dest_path.clone(),
                size: pf.size,
                shard_start,
                shard_count,
            });
        }
    }
    if !pack_records.is_empty() {
        planned_shards.push(ZipShard::Packed {
            shard_seq: pack_seq,
            records: std::mem::take(&mut pack_records),
        });
    }

    let total_shards = next_seq - 1;
    let file_count = planned_files.len() as u64;

    let manifest_json = serde_json::to_vec(&Manifest {
        dest_root: dest_root.to_string(),
        file_count,
        total_bytes,
        total_shards,
        files: planned_files,
    })?;

    let mut c = Connection::connect(&cfg.addr)?;
    let begin_ack = send_and_expect(
        &mut c,
        FrameType::BeginTx,
        &tx_meta_buf_flags(tx_id, 2, flags, &manifest_json),
        FrameType::BeginTxAck,
    )?;
    let last_acked_shard = parse_last_acked_shard(&begin_ack, flags & TX_FLAG_RESUME != 0);
    // Same ghost-commit guard as the single-file paths. See guard_last_acked.
    guard_last_acked(last_acked_shard, total_shards)?;

    // ── Send pass ── inflate one entry at a time into a seekable cache and
    //    serve each shard's range. Skipped (already-acked) shards on resume
    //    don't force re-sends; the cache re-inflates locally when first
    //    touched.
    let mut mat = ZipMaterialiser::new(archive, ram_threshold, std::env::temp_dir());
    let mut shards_sent = 0u64;
    {
        let mut sender = PipelinedSender::new(&mut c, cfg, tx_id, total_shards);
        for zs in &planned_shards {
            let seq = zip_shard_seq(zs);
            if seq <= last_acked_shard {
                continue;
            }
            let body = mat.body(zs)?;
            let (record_count, sflags) = zip_shard_meta(zs);
            shards_sent += 1;
            sender.send_with(seq, &body, record_count, sflags)?;
            if let Some(p) = &cfg.progress_bytes {
                // File-data bytes only (see transfer_dir) — packed-shard
                // framing isn't part of the plan total.
                p.fetch_add(zip_shard_data_len(zs), std::sync::atomic::Ordering::Relaxed);
            }
        }
        sender.drain()?;
    }
    drop(mat); // evict any temp cache file before COMMIT

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

/// `transfer_zip` with automatic resume-on-network-drop. See
/// `transfer_dir_resumable` for the `initial_flags` contract.
pub fn transfer_zip_resumable(
    cfg: &TransferConfig,
    tx_id: [u8; 16],
    dest_root: &str,
    zip_path: &Path,
    ram_threshold: u64,
    max_retries: u32,
    initial_flags: u32,
) -> Result<TransferResult> {
    resumable_retry(max_retries, "transfer_zip", initial_flags, |flags| {
        transfer_zip_with_opts(cfg, tx_id, dest_root, zip_path, ram_threshold, flags)
    })
}

#[cfg(test)]
mod bandwidth_throttle_tests {
    use super::BandwidthThrottle;
    use std::time::{Duration, Instant};

    #[test]
    fn stall_does_not_disable_pacing() {
        // Regression for the cumulative-average bug: a stall used to bank
        // unlimited credit and silently stop pacing for the rest of the tx.
        let mut t = BandwidthThrottle::new(1000); // 1000 B/s
        let t0 = Instant::now();
        assert!(t.charge(1000, t0).is_zero()); // spend initial 1s burst
                                               // 100-second stall, then resume sending.
        let after = t0 + Duration::from_secs(100);
        // The stall grants at most one second of burst, not 100.
        assert!(t.charge(1000, after).is_zero());
        // With the burst spent and no further time elapsed, the next at-cap
        // send must pace (~1s) — proving the cap wasn't abandoned.
        let sleep = t.charge(1000, after);
        assert!(
            sleep >= Duration::from_millis(900),
            "expected ~1s pacing after stall, got {sleep:?}"
        );
    }

    #[test]
    fn steady_send_at_cap_does_not_sleep() {
        let mut t = BandwidthThrottle::new(1000);
        let mut now = Instant::now();
        assert!(t.charge(1000, now).is_zero()); // burst
        for _ in 0..5 {
            now += Duration::from_secs(1);
            assert!(
                t.charge(1000, now).is_zero(),
                "1000 B/s under a 1000 B/s cap should not sleep"
            );
        }
    }

    #[test]
    fn oversized_send_paces_over_multiple_calls() {
        let mut t = BandwidthThrottle::new(1000);
        let now = Instant::now();
        // 3000 B against a 1000 B/s cap: burst covers 1000, deficit paced
        // out in 1s-clamped slices across calls.
        assert_eq!(t.charge(3000, now), Duration::from_secs(1));
        let second = t.charge(0, now);
        assert!(
            second >= Duration::from_millis(900) && second <= Duration::from_secs(1),
            "remaining deficit should pace ~1s, got {second:?}"
        );
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
mod materialise_body_tests {
    //! Tests for the shard-body encoder. The packed-shard wire format
    //! is a sequence of `[u32 path_len LE][u32 size LE][path bytes][file
    //! bytes]` records — the payload's reverse parser depends on each
    //! header field being exactly 4 bytes in the documented order. A
    //! regression here corrupts every multi-file upload silently, so we
    //! pin the encoding rather than trusting the comments.
    use super::*;
    use std::io::Write;

    fn unique_tempdir(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ps5upload_transfer_test_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn empty_shard_materialises_to_zero_bytes() {
        // `PlannedShard::Empty` exists so the payload still opens and
        // creates the destination on COMMIT for a 0-byte source file
        // (e.g. `.gitkeep`). The body must be empty — non-empty would
        // confuse the payload's record parser.
        let s = PlannedShard::Empty { shard_seq: 7 };
        let body = materialise_body(&s, None).expect("empty shard");
        assert_eq!(body.len(), 0);
    }

    #[test]
    fn non_packed_shard_reads_file_slice() {
        // Verify the seek+read for the NonPacked path: writing a known
        // byte pattern and reading back a slice must return exactly
        // the slice — not too-short, not too-long, not the whole file.
        let dir = unique_tempdir("nonpacked");
        let p = dir.join("blob.bin");
        let mut f = std::fs::File::create(&p).unwrap();
        let payload: Vec<u8> = (0u8..200u8).collect();
        f.write_all(&payload).unwrap();
        drop(f);

        let s = PlannedShard::NonPacked {
            shard_seq: 0,
            source: p,
            offset: 50,
            len: 32,
        };
        let body = materialise_body(&s, None).expect("non-packed shard");
        assert_eq!(body, &payload[50..82]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn packed_shard_emits_documented_record_layout() {
        // Two records, distinct paths and contents. The body must be:
        //   [path_len_LE u32][size_LE u32][path bytes][file bytes]
        // for each record, concatenated with no padding. This is the
        // contract the payload's reverse parser depends on.
        let dir = unique_tempdir("packed");
        let p1 = dir.join("a.bin");
        let p2 = dir.join("b.bin");
        std::fs::write(&p1, b"hello").unwrap();
        std::fs::write(&p2, b"world!!").unwrap();

        let records = vec![
            PackRecord {
                dest_path: "sce_sys/icon0.png".to_string(),
                source: p1,
                size: 5,
            },
            PackRecord {
                dest_path: "eboot.bin".to_string(),
                source: p2,
                size: 7,
            },
        ];
        let s = PlannedShard::Packed {
            shard_seq: 0,
            records,
        };
        let body = materialise_body(&s, None).expect("packed shard");

        let mut expected: Vec<u8> = Vec::new();
        // record 1: path_len=17, size=5, "sce_sys/icon0.png", "hello"
        expected.extend_from_slice(&17u32.to_le_bytes());
        expected.extend_from_slice(&5u32.to_le_bytes());
        expected.extend_from_slice(b"sce_sys/icon0.png");
        expected.extend_from_slice(b"hello");
        // record 2: path_len=9, size=7, "eboot.bin", "world!!"
        expected.extend_from_slice(&9u32.to_le_bytes());
        expected.extend_from_slice(&7u32.to_le_bytes());
        expected.extend_from_slice(b"eboot.bin");
        expected.extend_from_slice(b"world!!");

        assert_eq!(body, expected);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn packed_record_size_overflow_is_rejected() {
        // The planner only sends files < PACK_FILE_MAX (128 KiB) into
        // the packed path, so the `u32::try_from` guard is theoretical
        // today — but a future bump to PACK_FILE_MAX must NOT silently
        // truncate >4 GiB sizes into a corrupted record stream. Here we
        // construct a PackRecord whose declared size exceeds u32::MAX
        // and assert the encoder errors out before opening the file.
        let dir = unique_tempdir("oversize");
        let p = dir.join("dummy.bin");
        // The file is small — the encoder must fail at the size cast,
        // before reaching the read_exact. If it ever reads the file
        // first (then casts), this test would fail with a different
        // error message and signal that the bounds check was bypassed.
        std::fs::write(&p, b"short").unwrap();

        let s = PlannedShard::Packed {
            shard_seq: 0,
            records: vec![PackRecord {
                dest_path: "huge.bin".to_string(),
                source: p,
                size: u64::from(u32::MAX) + 1,
            }],
        };
        let err = materialise_body(&s, None).expect_err("should reject oversize");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("exceeds u32") || msg.contains("pack record size"),
            "unexpected error: {msg}"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod retry_classification_tests {
    //! Tests for `is_retryable_transfer_error`. The retry budget is
    //! cheap (3 attempts, exp backoff) but a misclassification either
    //! way is expensive: retrying a permission error wastes user time
    //! with no chance of success, and NOT retrying a real network drop
    //! turns a recoverable hiccup into a failed upload.
    use super::*;

    fn ioerr(kind: std::io::ErrorKind) -> anyhow::Error {
        anyhow::Error::from(std::io::Error::new(kind, "test"))
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
    fn unwraps_through_anyhow_context() {
        // The transfer-loop wraps every IO error with .with_context(),
        // so the retry classifier must walk anyhow's cause chain to
        // find the underlying io::Error. Without this walk every error
        // is "not retryable" and resume never kicks in.
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "wifi");
        let wrapped: anyhow::Error = anyhow::Error::from(inner)
            .context("write_all_parts")
            .context("send shard 7");
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
