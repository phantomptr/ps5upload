//! Parallel HTTP range proxy for installing a package straight from a link.
//!
//! Sony's installer pulls from a given URL over a single connection, so
//! pointing the console straight at an internet host leaves one TCP stream
//! against a distant, possibly rate-limited origin. Instead the engine fetches
//! the URL itself over many connections at once and re-serves those bytes to
//! the console from the pkg-host on the LAN — the same transport the Stream
//! install path already uses, so the console side is unchanged and already
//! hardware-verified.
//!
//! CORRECTION (2026-09-20): this comment used to claim the console "tops out
//! around 5-10 MB/s no matter how fast the line is". That is wrong and it
//! misled real work — a 79 GB install measured **105.7 MB/s sustained** to a
//! wired console on FW 5.10, and a link install's serve leg logged 73.3 MB/s.
//! The console is not the ceiling on a LAN. When a link install is slow, the
//! constraint is the origin fetch or the PC-to-console path, and the
//! `url-install origin fetch` / `pkg-host serve rate` log lines say which —
//! do not assume a console-side limit that does not exist.
//!
//! Nothing is staged on disk. The proxy keeps a short ring of recently
//! fetched windows in memory, so a 100 GB package needs neither PC disk space
//! nor twice the console's free space.
//!
//! ## Why windows instead of proxying each request 1:1
//!
//! Splitting each incoming console request across N origin connections would
//! be simpler, but BGFT's requests are far smaller than the window at which
//! parallelism pays off — a 256 KiB request cut eight ways is eight
//! latency-bound 32 KiB fetches, which is *slower* than one connection. So a
//! console request instead faults in an aligned window (32 MiB by default),
//! fetched as N contiguous pieces in parallel, and is served from it. Because
//! BGFT reads a package broadly front-to-back, the following requests hit the
//! same cached window and cost no origin traffic at all.

use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Origin connections opened per window fetch.
const DEFAULT_PARALLELISM: usize = 8;
/// Bytes faulted in per cache miss.
const DEFAULT_WINDOW_MB: u64 = 32;
/// Windows retained in memory. Peak proxy memory is roughly
/// `DEFAULT_WINDOW_MB * DEFAULT_CACHE_WINDOWS` (128 MiB at the defaults).
const DEFAULT_CACHE_WINDOWS: usize = 4;

/// How many windows ahead of the console to fetch in the background.
///
/// One is right when the origin is at least as fast as the console: the next
/// window lands while the current one is being served and the pipeline never
/// stalls. It is NOT enough when the origin is the slow leg — with a
/// high-latency or rate-limited host, one window of lead is consumed before
/// the next arrives and throughput collapses back to the origin's
/// per-connection rate.
///
/// Left at 1 by default because raising it costs real memory (each window is
/// `PS5UPLOAD_URL_WINDOW_MB`) and wastes origin traffic on a backward seek.
/// Raise it only with evidence — the `url-install origin fetch` and
/// `pkg-host serve rate` log lines say which leg is actually the constraint.
const DEFAULT_READAHEAD_WINDOWS: u64 = 1;
/// Never cut a window into pieces smaller than this — below it, per-request
/// latency dominates and more connections make the transfer slower.
const MIN_PIECE_BYTES: u64 = 1024 * 1024;
/// Consecutive *no-progress* attempts tolerated per piece. Only attempts that
/// deliver nothing count against this budget — an attempt that moved the
/// cursor forward resets it. A flaky origin that dribbles a piece out over
/// many short responses therefore still completes, which is exactly the
/// "install fails halfway" class we must survive on a multi-hour 100 GB
/// download.
const PIECE_STALL_ATTEMPTS: u32 = 4;
/// Absolute cap on attempts per piece, so an origin that answers every
/// request with a single byte cannot spin forever.
const PIECE_MAX_ATTEMPTS: u32 = 512;
/// Per-piece timeout. Generous: a piece is a few MiB and the origin may be a
/// slow mirror, but a wedged connection must not hang the console's fetch
/// (BGFT gives up long before an unbounded wait would return).
const PIECE_TIMEOUT: Duration = Duration::from_secs(120);

fn env_u64(key: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(|v| v.clamp(min, max))
        .unwrap_or(default)
}

/// What a probe of the origin established about the package.
#[derive(Debug, Clone)]
pub struct RemoteProbe {
    pub total_size: u64,
    /// Filename from the URL path, when it had one. Used only for display.
    pub filename: String,
}

/// A package on an HTTP(S) origin, readable by byte range.
pub struct RemoteSource {
    url: String,
    total_size: u64,
    agent: ureq::Agent,
    window_bytes: u64,
    parallelism: usize,
    max_windows: usize,
    /// LRU ring of `(window index, bytes)`, most recently used at the back.
    cache: Mutex<VecDeque<(u64, Arc<Vec<u8>>)>>,
    /// Windows to fetch ahead of demand; see DEFAULT_READAHEAD_WINDOWS.
    readahead: u64,
    /// Serialises cache misses. Without it two concurrent BGFT requests that
    /// miss the same window each fetch it, doubling origin traffic and peak
    /// memory. Window fetches are internally parallel, so serialising misses
    /// costs no throughput on the sequential access pattern BGFT actually has.
    fetch_lock: Mutex<()>,
    /// Window indices a readahead thread is currently fetching. Stops several
    /// console requests that land inside one window from each spawning their
    /// own thread for the same next window.
    prefetching: Mutex<HashSet<u64>>,
    /// Cumulative origin bytes fetched and nanoseconds spent fetching them.
    /// Kept so the install status can report the DOWNLOAD leg's speed next to
    /// the console leg's, instead of one blended number that hides which of
    /// the two is actually slow.
    origin_bytes: AtomicU64,
    origin_nanos: AtomicU64,
}

/// Per-piece evidence for one ranged GET, gathered so a slow link install can
/// be diagnosed from a bug report instead of guessed at.
///
/// The field that matters most is not here but derived from these across a
/// window: `sum(took) / window_elapsed`. With N pieces fetched concurrently
/// that ratio approaches N; if it sits near 1 the pieces are effectively
/// SERIALISED, which looks identical from the outside but wants the opposite
/// fix (more connections, not more bandwidth).
#[derive(Debug, Default, Clone, Copy)]
struct PieceStat {
    bytes: u64,
    took: Duration,
    /// Time to first byte of the FIRST attempt — the origin's answer latency,
    /// as distinct from how fast it then sends.
    first_ttfb: Option<Duration>,
    /// Total GETs issued, including resumes after a short read.
    attempts: u32,
    /// Worst consecutive run of zero-progress attempts.
    stalls: u32,
    /// Times the origin closed before sending the whole range.
    short_reads: u32,
    /// Milliseconds spent asleep in retry backoff, previously invisible.
    backoff_ms: u64,
}

impl PieceStat {
    fn record_ttfb(&mut self, ttfb: Duration) {
        if self.first_ttfb.is_none() {
            self.first_ttfb = Some(ttfb);
        }
    }
}

/// `parallelism` is the number of connections a single window fetch opens at
/// once, and the pool must be sized to keep all of them.
///
/// ureq defaults to `max_idle_connections_per_host: 3`. We open 8. So five of
/// every eight connections were dropped after each window and rebuilt for the
/// next one: a fresh TCP and TLS handshake, then TCP slow-start from zero, to
/// move only 4 MiB before being thrown away again. On a long path that is
/// most of the transfer spent ramping up and none of it at speed, which is
/// the shape of the 1.9 MB/s reports — a browser or download manager holds
/// its connections open and never pays this.
fn build_agent(parallelism: usize) -> ureq::Agent {
    let keep = parallelism.max(1);
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(PIECE_TIMEOUT))
        .max_idle_connections_per_host(keep)
        .max_idle_connections(keep.saturating_mul(2).max(10))
        .build();
    ureq::Agent::new_with_config(config)
}

/// `bytes 0-0/12345` → `12345`. A `*` total (unknown length) is rejected: the
/// console must be told a real `Content-Range` total or the install cannot be
/// sized.
fn total_from_content_range(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse::<u64>().ok().filter(|t| *t > 0)
}

fn filename_from_url(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Deliberately omits the URL: an install link can carry a signed query token
/// and this type is reachable from `InstallSession`'s derived `Debug`, which
/// the engine logs. Only the shape of the transfer is printed.
impl std::fmt::Debug for RemoteSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSource")
            .field("total_size", &self.total_size)
            .field("window_bytes", &self.window_bytes)
            .field("parallelism", &self.parallelism)
            .field("max_windows", &self.max_windows)
            .finish_non_exhaustive()
    }
}

impl RemoteSource {
    /// Establish the package's size and that the origin honours byte ranges.
    ///
    /// Probes with `Range: bytes=0-0` rather than `HEAD`: plenty of file hosts
    /// and CDNs answer `HEAD` with a wrong length, a redirect to an HTML
    /// landing page, or a 405, and a one-byte GET proves the exact thing we
    /// depend on — that ranged reads work and report a total.
    pub fn probe(url: &str) -> Result<RemoteProbe, String> {
        // A single one-byte GET; one pooled connection is all it can use.
        let agent = build_agent(1);
        let resp = agent
            .get(url)
            .header("User-Agent", "ps5upload")
            .header("Range", "bytes=0-0")
            .call()
            .map_err(|e| format!("could not reach the package URL: {e}"))?;

        let status = resp.status().as_u16();
        if status != 206 {
            return Err(format!(
                "the server answered {status} for a byte-range request; \
                 installing from a link needs a host that supports ranges \
                 (HTTP 206). Direct-download links work; share/preview pages \
                 usually do not."
            ));
        }
        let content_range = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| "the server sent no Content-Range for a range request".to_string())?;
        let total_size = total_from_content_range(content_range).ok_or_else(|| {
            format!("the server sent an unusable Content-Range ({content_range})")
        })?;

        Ok(RemoteProbe {
            total_size,
            filename: filename_from_url(url),
        })
    }

    pub fn new(url: String, total_size: u64) -> Self {
        let window_bytes =
            env_u64("PS5UPLOAD_URL_WINDOW_MB", DEFAULT_WINDOW_MB, 1, 512) * 1024 * 1024;
        let parallelism =
            env_u64("PS5UPLOAD_URL_THREADS", DEFAULT_PARALLELISM as u64, 1, 32) as usize;
        let readahead = env_u64("PS5UPLOAD_URL_READAHEAD", DEFAULT_READAHEAD_WINDOWS, 1, 8);
        let max_windows = env_u64(
            "PS5UPLOAD_URL_CACHE_WINDOWS",
            DEFAULT_CACHE_WINDOWS as u64,
            1,
            64,
        ) as usize;
        Self {
            url,
            total_size,
            agent: build_agent(parallelism),
            window_bytes,
            parallelism,
            readahead,
            max_windows,
            cache: Mutex::new(VecDeque::new()),
            fetch_lock: Mutex::new(()),
            prefetching: Mutex::new(HashSet::new()),
            origin_bytes: AtomicU64::new(0),
            origin_nanos: AtomicU64::new(0),
        }
    }

    /// Read `[start, end]` inclusive. Blocking: call from `spawn_blocking`.
    pub fn read_range(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        if self.total_size == 0 || start >= self.total_size || end < start {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "range {start}-{end} is outside the remote package (size {})",
                    self.total_size
                ),
            ));
        }
        let end = end.min(self.total_size - 1);
        let want = usize::try_from(end - start + 1).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "range too large for usize",
            )
        })?;

        let mut out = Vec::with_capacity(want);
        let mut cursor = start;
        while cursor <= end {
            let idx = cursor / self.window_bytes;
            let window = self.window(idx)?;
            let w_start = idx * self.window_bytes;
            let offset = usize::try_from(cursor - w_start).unwrap_or(usize::MAX);
            if offset >= window.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("remote window {idx} is shorter than expected"),
                ));
            }
            let take = ((end - cursor + 1) as usize).min(window.len() - offset);
            out.extend_from_slice(&window[offset..offset + take]);
            cursor += take as u64;
        }
        Ok(out)
    }

    fn cache_get(&self, idx: u64) -> Option<Arc<Vec<u8>>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let pos = cache.iter().position(|(i, _)| *i == idx)?;
        // Touch: move to the back so the ring evicts genuinely cold windows.
        let entry = cache.remove(pos)?;
        let bytes = entry.1.clone();
        cache.push_back(entry);
        Some(bytes)
    }

    fn window(&self, idx: u64) -> std::io::Result<Arc<Vec<u8>>> {
        if let Some(bytes) = self.cache_get(idx) {
            return Ok(bytes);
        }
        let _miss = self.fetch_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Another request may have faulted this window in while we queued.
        if let Some(bytes) = self.cache_get(idx) {
            return Ok(bytes);
        }
        let bytes = Arc::new(self.fetch_window(idx)?);
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.push_back((idx, bytes.clone()));
            while cache.len() > self.max_windows {
                cache.pop_front();
            }
        }
        Ok(bytes)
    }

    /// Start fetching the window after `offset` in the background, unless it is
    /// already cached or already in flight.
    ///
    /// Why this exists: `window()` is demand-driven, so without readahead the
    /// console blocks at every window boundary while a whole window is fetched,
    /// then drains it from RAM in a fraction of that time. The origin therefore
    /// never runs faster than the console consumes — reported from the field as
    /// a 1-3 MB/s install on a line that pulls 20-50 MB/s to disk, unchanged by
    /// any amount of rearranging WiFi and LAN, because none of that touches the
    /// stall. The existing parallelism only hides latency *within* a window; it
    /// never overlaps fetching with serving. One window of readahead does.
    ///
    /// Deliberately direction-agnostic. BGFT is not strictly sequential, but a
    /// window wasted after a backward seek is absorbed by the LRU ring, and
    /// tracking direction would cost more complexity than the occasional waste.
    ///
    /// An associated function rather than a method because it must clone the
    /// `Arc` for the thread, and `&Arc<Self>` is not a stable receiver type.
    pub fn prefetch_after(this: &Arc<Self>, offset: u64) {
        if this.total_size == 0 || this.window_bytes == 0 {
            return;
        }
        let here = offset / this.window_bytes;
        let last = (this.total_size - 1) / this.window_bytes;
        for step in 1..=this.readahead {
            let next = here + step;
            if next > last {
                return;
            }
            if this.cache_get(next).is_some() {
                continue;
            }
            {
                let mut inflight = this.prefetching.lock().unwrap_or_else(|e| e.into_inner());
                // `insert` is false when this window is already queued.
                if !inflight.insert(next) {
                    continue;
                }
            }
            let me = Arc::clone(this);
            std::thread::spawn(move || {
                // Errors are deliberately dropped: a failed readahead costs
                // nothing because the demand path refetches the window and
                // surfaces any real error to the caller then.
                let _ = me.window(next);
                let mut inflight = me.prefetching.lock().unwrap_or_else(|e| e.into_inner());
                inflight.remove(&next);
            });
        }
    }

    /// Fetch one aligned window as `parallelism` contiguous pieces at once.
    fn fetch_window(&self, idx: u64) -> std::io::Result<Vec<u8>> {
        let w_start = idx * self.window_bytes;
        if w_start >= self.total_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("remote window {idx} starts past the end of the package"),
            ));
        }
        let w_len = self.window_bytes.min(self.total_size - w_start);
        let pieces = self.piece_ranges(w_start, w_len);
        // Time the origin leg. Without this there is NO way — from logs,
        // status or anything else — to tell whether a slow link install is
        // slow because the origin is slow or because the console is pulling
        // slowly, and the two want opposite fixes. A user reporting "my
        // browser downloads this at 30 MB/s but the install runs at 1 MB/s"
        // could not be answered without guessing; now the log says which leg
        // is the constraint.
        let fetch_started = std::time::Instant::now();

        let mut buf = vec![0u8; w_len as usize];
        // Hand each worker a disjoint &mut slice of the output so the pieces
        // land in order with no reassembly step and no per-piece allocation.
        let mut slices: Vec<&mut [u8]> = Vec::with_capacity(pieces.len());
        let mut rest = buf.as_mut_slice();
        for (_, len) in &pieces {
            let (head, tail) = rest.split_at_mut(*len as usize);
            slices.push(head);
            rest = tail;
        }

        let mut stats: Vec<PieceStat> = Vec::with_capacity(pieces.len());
        let errors: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = pieces
                .iter()
                .zip(slices)
                .map(|((p_start, p_len), dst)| {
                    let (p_start, p_len) = (*p_start, *p_len);
                    scope.spawn(move || self.fetch_piece(p_start, p_len, dst))
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| match h.join() {
                    Ok(Ok(stat)) => {
                        stats.push(stat);
                        None
                    }
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some("a download worker panicked".to_string()),
                })
                .collect()
        });

        if let Some(first) = errors.first() {
            return Err(std::io::Error::other(format!(
                "remote fetch failed at offset {w_start}: {first}"
            )));
        }

        let took = fetch_started.elapsed();
        self.origin_bytes.fetch_add(w_len, Ordering::Relaxed);
        self.origin_nanos
            .fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        let mbps = if took.as_secs_f64() > 0.0 {
            (w_len as f64) / took.as_secs_f64() / 1_000_000.0
        } else {
            0.0
        };
        // Concurrency evidence. Each piece runs on its own thread, so if they
        // truly overlap the sum of their durations is ~N x the window's own
        // elapsed time. A ratio near 1 means they are serialising — the same
        // slow window, but caused by too few connections rather than too
        // little bandwidth, and fixed differently.
        let piece_ms_total: u64 = stats.iter().map(|p| p.took.as_millis() as u64).sum();
        let overlap = if took.as_millis() > 0 {
            piece_ms_total as f64 / took.as_millis() as f64
        } else {
            0.0
        };
        let slowest = stats.iter().map(|p| p.took.as_millis()).max().unwrap_or(0);
        let fastest = stats.iter().map(|p| p.took.as_millis()).min().unwrap_or(0);
        let ttfb_max = stats
            .iter()
            .filter_map(|p| p.first_ttfb)
            .map(|d| d.as_millis())
            .max()
            .unwrap_or(0);
        // Throughput of a SINGLE connection. If every piece lands near the
        // same figure while the window total stays low, the origin is capping
        // per connection and the answer is more connections; if this is
        // already high, the cap is the line itself and more connections will
        // not help. This is the figure that tells those two apart.
        let per_conn: Vec<u64> = stats
            .iter()
            .map(|p| {
                let ms = p.took.as_millis() as u64;
                p.bytes.checked_div(ms).unwrap_or(0)
            })
            .collect();
        let per_conn_min = per_conn.iter().copied().min().unwrap_or(0);
        let per_conn_max = per_conn.iter().copied().max().unwrap_or(0);
        let attempts: u32 = stats.iter().map(|p| p.attempts).sum();
        let stalls: u32 = stats.iter().map(|p| p.stalls).sum();
        let short_reads: u32 = stats.iter().map(|p| p.short_reads).sum();
        let backoff_ms: u64 = stats.iter().map(|p| p.backoff_ms).sum();
        crate::log_info!(
            "url-install origin fetch: window={} bytes={} pieces={} took_ms={} rate={:.1} MB/s overlap={:.1}x piece_ms={}..{} per_conn_kBps={}..{} ttfb_max_ms={} attempts={} stalls={} short_reads={} backoff_ms={}",
            idx,
            w_len,
            pieces.len(),
            took.as_millis(),
            mbps,
            overlap,
            fastest,
            slowest,
            per_conn_min,
            per_conn_max,
            ttfb_max,
            attempts,
            stalls,
            short_reads,
            backoff_ms,
        );
        Ok(buf)
    }

    /// Average origin throughput in bytes/sec across every window fetched so
    /// far, or `None` before the first fetch completes. This is the DOWNLOAD
    /// leg only — the console leg is measured separately by the pkg-host — so
    /// the UI can name which side is slow rather than showing one blended
    /// figure that explains nothing.
    pub fn origin_rate_bps(&self) -> Option<u64> {
        let bytes = self.origin_bytes.load(Ordering::Relaxed);
        let nanos = self.origin_nanos.load(Ordering::Relaxed);
        if bytes == 0 || nanos == 0 {
            return None;
        }
        Some(((bytes as u128 * 1_000_000_000u128) / nanos as u128) as u64)
    }

    /// Split `[start, start+len)` into contiguous `(start, len)` pieces, at
    /// most `parallelism` of them and none smaller than `MIN_PIECE_BYTES`.
    fn piece_ranges(&self, start: u64, len: u64) -> Vec<(u64, u64)> {
        let by_min = (len / MIN_PIECE_BYTES).max(1);
        let n = (self.parallelism as u64).min(by_min).max(1);
        let base = len / n;
        let mut out = Vec::with_capacity(n as usize);
        let mut off = 0u64;
        for i in 0..n {
            // The last piece absorbs the remainder, so the pieces always sum
            // to exactly `len`.
            let this = if i == n - 1 { len - off } else { base };
            out.push((start + off, this));
            off += this;
        }
        out
    }

    /// Fetch exactly `len` bytes at `start` into `dst`, retrying transient
    /// failures. Resumes mid-piece: a connection that dies after 3 of 4 MiB
    /// re-requests only the missing tail, and any forward progress refreshes
    /// the stall budget so a slow, lossy origin still finishes.
    fn fetch_piece(&self, start: u64, len: u64, dst: &mut [u8]) -> Result<PieceStat, String> {
        let piece_started = std::time::Instant::now();
        let mut stat = PieceStat {
            bytes: len,
            ..PieceStat::default()
        };
        let mut filled = 0u64;
        let mut stalls = 0u32;
        let mut attempts = 0u32;
        let mut last_err = String::from("no attempt was made");
        while filled < len {
            if stalls >= PIECE_STALL_ATTEMPTS {
                return Err(format!(
                    "gave up after {stalls} attempts with no progress \
                     at {filled} of {len} bytes ({last_err})"
                ));
            }
            if attempts >= PIECE_MAX_ATTEMPTS {
                return Err(format!(
                    "gave up after {attempts} attempts, stuck at {filled} of \
                     {len} bytes ({last_err})"
                ));
            }
            if stalls > 0 {
                let backoff = Duration::from_millis(250 * u64::from(stalls));
                stat.backoff_ms += backoff.as_millis() as u64;
                std::thread::sleep(backoff);
            }
            attempts += 1;
            let from = start + filled;
            let to = start + len - 1;
            match self.read_into(from, to, &mut dst[filled as usize..]) {
                Ok((0, ttfb)) => {
                    stat.record_ttfb(ttfb);
                    stalls += 1;
                    last_err = format!("origin sent no bytes at offset {from}");
                }
                Ok((n, ttfb)) => {
                    stat.record_ttfb(ttfb);
                    filled += n;
                    // Progress: the connection died early but the cursor
                    // moved, so this does not count as a stall.
                    if n < len - (filled - n) {
                        stat.short_reads += 1;
                    }
                    stalls = 0;
                    last_err = format!("origin closed after {filled} of {len} bytes");
                }
                Err(e) => {
                    stalls += 1;
                    last_err = e;
                }
            }
            stat.stalls = stat.stalls.max(stalls);
        }
        stat.attempts = attempts;
        stat.took = piece_started.elapsed();
        Ok(stat)
    }

    /// One ranged GET. Returns how many bytes actually landed in `dst` (which
    /// may be short if the origin closed early — the caller resumes) and the
    /// time to first byte. TTFB is split out because it separates an origin
    /// that is slow to ANSWER from one that is slow to SEND: the first points
    /// at per-request throttling or cold storage, the second at bandwidth.
    fn read_into(&self, start: u64, end: u64, dst: &mut [u8]) -> Result<(u64, Duration), String> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .get(&self.url)
            .header("User-Agent", "ps5upload")
            .header("Range", &format!("bytes={start}-{end}"))
            .call()
            .map_err(|e| format!("{e}"))?;
        let ttfb = started.elapsed();
        let status = resp.status().as_u16();
        if status != 206 {
            // A 200 here means the origin ignored the Range and is about to
            // send the whole package down one connection. Treat it as fatal
            // rather than silently writing the wrong bytes at this offset.
            return Err(format!("expected 206 for a range request, got {status}"));
        }
        let mut reader = resp.into_body().into_reader();
        let mut filled = 0usize;
        while filled < dst.len() {
            match reader.read(&mut dst[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if filled == 0 {
                        return Err(format!("{e}"));
                    }
                    // Partial progress is still progress; let the retry loop
                    // ask for the tail instead of discarding what arrived.
                    break;
                }
            }
        }
        // Read once more to observe EOF. ureq only returns a connection to
        // the pool when its body reader reaches the end; stopping the instant
        // `dst` is full leaves the response looking half-read, so every piece
        // got a brand-new TCP+TLS connection and a fresh TCP slow-start. With
        // a Content-Length response this returns 0 immediately.
        if filled == dst.len() {
            let mut scratch = [0u8; 1];
            let _ = reader.read(&mut scratch);
        }
        Ok((filled as u64, ttfb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_from_content_range_reads_the_total_and_rejects_unknown_lengths() {
        assert_eq!(total_from_content_range("bytes 0-0/12345"), Some(12345));
        assert_eq!(total_from_content_range("bytes 5-9/10"), Some(10));
        assert_eq!(total_from_content_range("bytes 0-0/*"), None);
        assert_eq!(total_from_content_range("bytes 0-0/0"), None);
        assert_eq!(total_from_content_range("nonsense"), None);
    }

    #[test]
    fn filename_from_url_drops_query_and_fragment() {
        assert_eq!(
            filename_from_url("https://h.example/a/b/game.pkg?token=x"),
            "game.pkg"
        );
        assert_eq!(
            filename_from_url("https://h.example/game.pkg#f"),
            "game.pkg"
        );
        assert_eq!(filename_from_url("https://h.example/"), "");
    }

    fn source(total: u64) -> RemoteSource {
        RemoteSource::new("https://h.example/game.pkg".to_string(), total)
    }

    #[test]
    fn piece_ranges_are_contiguous_and_cover_the_window_exactly() {
        let s = source(1024 * 1024 * 1024);
        for len in [
            1u64,
            MIN_PIECE_BYTES - 1,
            MIN_PIECE_BYTES,
            MIN_PIECE_BYTES * 3 + 7,
            32 * 1024 * 1024,
        ] {
            let pieces = s.piece_ranges(4096, len);
            assert!(!pieces.is_empty(), "len {len} produced no pieces");
            assert!(pieces.len() <= s.parallelism);
            assert_eq!(pieces[0].0, 4096, "len {len} must start at the window");
            let summed: u64 = pieces.iter().map(|(_, l)| *l).sum();
            assert_eq!(summed, len, "pieces must sum to the window length");
            for w in pieces.windows(2) {
                assert_eq!(
                    w[0].0 + w[0].1,
                    w[1].0,
                    "pieces must be contiguous for len {len}"
                );
            }
            assert!(
                pieces.iter().all(|(_, l)| *l > 0),
                "no empty piece for len {len}"
            );
        }
    }

    #[test]
    fn small_windows_are_not_split_into_latency_bound_pieces() {
        let s = source(64 * 1024 * 1024);
        assert_eq!(s.piece_ranges(0, 256 * 1024).len(), 1);
        assert_eq!(s.piece_ranges(0, MIN_PIECE_BYTES * 2).len(), 2);
    }

    #[test]
    fn read_range_rejects_ranges_outside_the_package() {
        let s = source(100);
        assert!(s.read_range(100, 120).is_err());
        assert!(s.read_range(50, 10).is_err());
        assert!(source(0).read_range(0, 0).is_err());
    }
}

/// End-to-end tests against a real (tiny) HTTP range server on loopback.
///
/// The unit tests above cover the arithmetic, but the property that actually
/// matters — that N parallel workers write their pieces at the right offsets —
/// can only be shown by fetching real bytes and comparing them to the source.
/// A transposition bug here would silently corrupt an install.
#[cfg(test)]
pub(crate) mod origin_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic, position-dependent body: every byte encodes its own
    /// offset, so a misplaced piece is guaranteed to mismatch.
    pub(crate) fn body(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i * 31 + 7) as u8).collect()
    }

    pub(crate) struct Origin {
        pub(crate) addr: std::net::SocketAddr,
        requests: Arc<AtomicUsize>,
        /// Answer the first N range requests with a truncated body to prove
        /// the resume path, then behave normally.
        _shutdown: Arc<AtomicUsize>,
    }

    /// `truncate_first`: how many initial range responses stop halfway,
    /// simulating an origin that drops connections partway through.
    pub(crate) fn spawn_origin(
        data: Vec<u8>,
        truncate_first: usize,
        ignore_ranges: bool,
    ) -> Origin {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let data = Arc::new(data);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { break };
                let data = data.clone();
                let counter = counter.clone();
                std::thread::spawn(move || {
                    let _ = serve_one(stream, &data, &counter, truncate_first, ignore_ranges);
                });
            }
        });
        Origin {
            addr,
            requests,
            _shutdown: shutdown,
        }
    }

    fn serve_one(
        mut stream: TcpStream,
        data: &[u8],
        counter: &AtomicUsize,
        truncate_first: usize,
        ignore_ranges: bool,
    ) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut range: Option<(usize, usize)> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                let v = v.trim();
                if let Some((a, b)) = v.split_once('-') {
                    let start: usize = a.trim().parse().unwrap_or(0);
                    let end: usize = b
                        .trim()
                        .parse()
                        .unwrap_or_else(|_| data.len().saturating_sub(1));
                    range = Some((start, end.min(data.len().saturating_sub(1))));
                }
            }
        }

        let n = counter.fetch_add(1, Ordering::SeqCst);
        match range {
            Some((start, end)) if !ignore_ranges => {
                let full = &data[start..=end];
                // Early responses send only half the body and close, which is
                // exactly the mid-piece death `fetch_piece` must resume from.
                let send = if n < truncate_first {
                    &full[..full.len() / 2]
                } else {
                    full
                };
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\n\
                     Content-Type: application/octet-stream\r\n\
                     Accept-Ranges: bytes\r\n\
                     Content-Range: bytes {start}-{end}/{}\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    data.len(),
                    full.len(),
                )?;
                stream.write_all(send)?;
            }
            _ => {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                )?;
                stream.write_all(data)?;
            }
        }
        stream.flush()
    }

    fn source_for(origin: &Origin, total: u64, window_mb: u64, threads: usize) -> RemoteSource {
        let mut s = RemoteSource::new(format!("http://{}/game.pkg", origin.addr), total);
        s.window_bytes = window_mb * 1024 * 1024;
        s.parallelism = threads;
        s
    }

    /// Poll for a condition rather than sleeping a fixed amount: the readahead
    /// runs on its own thread, and a fixed sleep is the classic flaky-CI bug.
    fn wait_until(deadline: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cond()
    }

    /// Are the pieces of one window ACTUALLY fetched concurrently?
    ///
    /// This is not a style question. A user's link install sat at 1.9 MB/s
    /// while the same URL pulled 40-50 MB/s in a multi-connection downloader
    /// on the same machine and line. Every piece shares one `ureq::Agent`, and
    /// if that Agent serialises them then "8 parallel pieces" is one
    /// connection wearing a disguise — which looks exactly like a slow origin
    /// from the outside and is the difference between 1.9 MB/s and 15.
    ///
    /// The origin here holds every response open for a beat and records the
    /// high-water mark of simultaneously open requests. With N pieces truly in
    /// flight that mark is N; if the Agent serialises, it is 1.
    #[test]
    fn window_pieces_are_fetched_concurrently_not_serialised() {
        let threads = 8usize;
        let window_mb = 8u64;
        let total = window_mb * 1024 * 1024;
        let hold = Duration::from_millis(120);

        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let data = Arc::new(body(total as usize));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        {
            let (live, peak, data) = (live.clone(), peak.clone(), data.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(stream) = conn else { break };
                    let (live, peak, data) = (live.clone(), peak.clone(), data.clone());
                    std::thread::spawn(move || {
                        let n = live.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        // Hold the request open so genuine overlap is visible.
                        std::thread::sleep(hold);
                        let _ = serve_one(stream, &data, &Arc::new(AtomicUsize::new(0)), 0, false);
                        live.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        }

        let mut src = RemoteSource::new(format!("http://{addr}/game.pkg"), total);
        src.window_bytes = window_mb * 1024 * 1024;
        src.parallelism = threads;

        let started = std::time::Instant::now();
        let got = src.read_range(0, total - 1).expect("window fetch");
        let elapsed = started.elapsed();
        assert_eq!(got.len(), total as usize, "short window");

        let seen = peak.load(Ordering::SeqCst);
        assert_eq!(
            seen, threads,
            "expected {threads} concurrent origin requests, saw {seen} — the \
             shared ureq::Agent is serialising the pieces, so the window's \
             throughput is one connection's, not {threads}"
        );
        // Serialised would be threads * hold; concurrent is ~one hold.
        assert!(
            elapsed < hold * (threads as u32) / 2,
            "window took {elapsed:?}; concurrent pieces should finish in about \
             one {hold:?} hold, serialised ones in {threads} of them"
        );
    }

    /// A keep-alive origin: serves any number of sequential ranged GETs on
    /// one socket, exactly as a real HTTP/1.1 server does. The shared
    /// `serve_one` helper answers a single request and hangs up, which would
    /// make a connection-reuse test measure the stub instead of ureq.
    fn spawn_keepalive_origin(data: Vec<u8>, accepted: Arc<AtomicUsize>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let data = Arc::new(data);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { break };
                accepted.fetch_add(1, Ordering::SeqCst);
                let data = data.clone();
                std::thread::spawn(move || {
                    let Ok(peer) = stream.try_clone() else { return };
                    let mut reader = BufReader::new(peer);
                    // One iteration per request on this socket.
                    loop {
                        let mut range: Option<(usize, usize)> = None;
                        let mut saw_request = false;
                        loop {
                            let mut line = String::new();
                            match reader.read_line(&mut line) {
                                Ok(0) => return,
                                Ok(_) => {}
                                Err(_) => return,
                            }
                            if line == "\r\n" || line == "\n" {
                                break;
                            }
                            saw_request = true;
                            if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=")
                            {
                                if let Some((a, b)) = v.trim().split_once('-') {
                                    let st: usize = a.trim().parse().unwrap_or(0);
                                    let en: usize =
                                        b.trim().parse().unwrap_or(data.len().saturating_sub(1));
                                    range = Some((st, en.min(data.len() - 1)));
                                }
                            }
                        }
                        if !saw_request {
                            return;
                        }
                        let (st, en) = range.unwrap_or((0, data.len() - 1));
                        let body = &data[st..=en];
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\n\
                             Content-Range: bytes {}-{}/{}\r\n\
                             Content-Length: {}\r\n\
                             Connection: keep-alive\r\n\r\n",
                            st,
                            en,
                            data.len(),
                            body.len(),
                        );
                        if stream.write_all(head.as_bytes()).is_err()
                            || stream.write_all(body).is_err()
                            || stream.flush().is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    /// Connections must survive from one window to the next.
    ///
    /// ureq pools only 3 idle connections per host by default while a window
    /// fetch opens `parallelism` of them, so five of every eight were being
    /// closed and rebuilt each window — a TCP and TLS handshake plus a fresh
    /// TCP slow-start to move 4 MiB, over and over. This counts the distinct
    /// sockets the origin accepts across several sequential windows: with the
    /// pool sized to the parallelism it stays at `parallelism`, and without
    /// it climbs with every window.
    #[test]
    fn connections_are_reused_across_windows() {
        let threads = 4usize;
        // 4 MiB window with a 1 MiB minimum piece = 4 pieces, so each window
        // really does open `threads` connections at once.
        let window_mb = 4u64;
        let windows = 4u64;
        let total = window_mb * 1024 * 1024 * windows;

        let accepted = Arc::new(AtomicUsize::new(0));
        let addr = spawn_keepalive_origin(body(total as usize), accepted.clone());

        let mut src = RemoteSource::new(format!("http://{addr}/game.pkg"), total);
        src.window_bytes = window_mb * 1024 * 1024;
        src.parallelism = threads;
        // Defeat the window cache so every window is a real origin fetch.
        src.max_windows = 1;

        for w in 0..windows {
            let start = w * src.window_bytes;
            let end = start + src.window_bytes - 1;
            src.read_range(start, end).expect("window fetch");
        }

        let sockets = accepted.load(Ordering::SeqCst);
        assert!(
            sockets <= threads,
            "origin accepted {sockets} sockets for {windows} windows of \
             {threads} pieces; with the pool sized to the parallelism the \
             same {threads} connections should have served them all"
        );
    }

    #[test]
    fn a_read_faults_in_the_next_window_in_the_background() {
        // Three windows of 1 MiB. Reading inside window 0 must pull window 1
        // without anyone asking for it — that overlap is the whole point: it is
        // what stops the origin running only as fast as the console consumes.
        let total = 3 * 1024 * 1024usize;
        let data = body(total);
        let origin = spawn_origin(data, 0, false);
        let s = Arc::new(source_for(&origin, total as u64, 1, 2));

        let first = s.read_range(0, 1023).expect("first read");
        assert_eq!(first, &s.read_range(0, 1023).expect("cached reread")[..]);
        assert!(s.cache_get(1).is_none(), "window 1 must not be cached yet");

        RemoteSource::prefetch_after(&s, 1023);
        assert!(
            wait_until(std::time::Duration::from_secs(10), || s
                .cache_get(1)
                .is_some()),
            "readahead never cached window 1"
        );

        // And it cached the RIGHT bytes, not merely something.
        let w1 = s
            .read_range(1024 * 1024, 1024 * 1024 + 511)
            .expect("window 1");
        assert_eq!(w1, &body(total)[1024 * 1024..1024 * 1024 + 512]);
    }

    #[test]
    fn readahead_past_the_last_window_is_a_no_op() {
        let total = 2048usize;
        let origin = spawn_origin(body(total), 0, false);
        let s = Arc::new(source_for(&origin, total as u64, 1, 2));
        // One 1 MiB window covers the whole package, so there is no next window.
        RemoteSource::prefetch_after(&s, total as u64 - 1);
        assert!(
            !wait_until(std::time::Duration::from_millis(300), || s
                .cache_get(1)
                .is_some()),
            "nothing may be fetched past the end of the package"
        );
    }

    #[test]
    fn overlapping_readaheads_fetch_the_window_once() {
        // Several console requests landing inside one window must not each
        // spawn a fetch for the same next window.
        let total = 4 * 1024 * 1024usize;
        let origin = spawn_origin(body(total), 0, false);
        let s = Arc::new(source_for(&origin, total as u64, 1, 1));

        s.read_range(0, 1023).expect("first read");
        let before = origin.requests.load(Ordering::SeqCst);

        for off in [0u64, 100, 4096, 65_536] {
            RemoteSource::prefetch_after(&s, off);
        }
        assert!(
            wait_until(std::time::Duration::from_secs(10), || s
                .cache_get(1)
                .is_some()),
            "readahead never cached window 1"
        );
        // Give any late-starting duplicate a chance to appear before counting,
        // so the assertion below fails loudly rather than racing past one.
        std::thread::sleep(std::time::Duration::from_millis(300));

        let after = origin.requests.load(Ordering::SeqCst);
        // parallelism is 1, so one window costs exactly one origin request.
        assert_eq!(
            after - before,
            1,
            "four overlapping readaheads fetched window 1 more than once"
        );
    }

    #[test]
    fn probe_reports_the_total_size_from_content_range() {
        let data = body(5000);
        let origin = spawn_origin(data, 0, false);
        let probe =
            RemoteSource::probe(&format!("http://{}/game.pkg", origin.addr)).expect("probe ok");
        assert_eq!(probe.total_size, 5000);
        assert_eq!(probe.filename, "game.pkg");
    }

    #[test]
    fn probe_rejects_an_origin_that_ignores_ranges() {
        let origin = spawn_origin(body(5000), 0, true);
        let err = RemoteSource::probe(&format!("http://{}/game.pkg", origin.addr))
            .expect_err("a 200 origin must be rejected");
        assert!(err.contains("206"), "unhelpful message: {err}");
    }

    #[test]
    fn parallel_pieces_reassemble_to_exactly_the_source_bytes() {
        // 8 MiB across 4 workers with a 2 MiB window: several windows, each
        // split into pieces, so both axes of the reassembly are exercised.
        let total = 8 * 1024 * 1024usize;
        let data = body(total);
        let origin = spawn_origin(data.clone(), 0, false);
        let mut s = source_for(&origin, total as u64, 2, 4);
        s.window_bytes = 2 * 1024 * 1024;

        // Whole file in one call.
        let all = s.read_range(0, total as u64 - 1).expect("read all");
        assert_eq!(all.len(), total);
        assert_eq!(all, data, "reassembled bytes differ from the source");
    }

    #[test]
    fn unaligned_reads_across_window_boundaries_return_the_right_slice() {
        let total = 6 * 1024 * 1024usize;
        let data = body(total);
        let origin = spawn_origin(data.clone(), 0, false);
        let s = source_for(&origin, total as u64, 2, 4);

        for (start, len) in [
            (0u64, 1u64),
            (1, 3),
            (2 * 1024 * 1024 - 5, 10), // straddles a window edge
            (2 * 1024 * 1024, 4096),   // exactly on a window edge
            (1_500_000, 3_000_000),    // spans three windows
            (total as u64 - 1, 1),     // final byte
        ] {
            let end = start + len - 1;
            let got = s.read_range(start, end).unwrap_or_else(|e| {
                panic!("read {start}..={end} failed: {e}");
            });
            assert_eq!(
                got,
                &data[start as usize..=end as usize],
                "wrong bytes for {start}..={end}"
            );
        }
    }

    #[test]
    fn a_window_is_fetched_once_and_then_served_from_cache() {
        let total = 2 * 1024 * 1024usize;
        let data = body(total);
        let origin = spawn_origin(data.clone(), 0, false);
        let s = source_for(&origin, total as u64, 2, 1);

        let first = s.read_range(0, 1023).expect("first read");
        let after_first = origin.requests.load(Ordering::SeqCst);
        assert!(after_first >= 1, "the first read must hit the origin");

        // Four more reads inside the same window must cost no origin traffic.
        for start in [2048u64, 4096, 8192, 100_000] {
            let got = s.read_range(start, start + 511).expect("cached read");
            assert_eq!(got, &data[start as usize..start as usize + 512]);
        }
        assert_eq!(
            origin.requests.load(Ordering::SeqCst),
            after_first,
            "cached reads must not re-fetch the window"
        );
        assert_eq!(first, &data[..1024]);
    }

    #[test]
    fn a_connection_that_dies_mid_piece_is_resumed_not_failed() {
        let total = 4 * 1024 * 1024usize;
        let data = body(total);
        // Every one of the first 8 range responses is truncated to half.
        let origin = spawn_origin(data.clone(), 8, false);
        let s = source_for(&origin, total as u64, 2, 2);

        let got = s
            .read_range(0, total as u64 - 1)
            .expect("truncated responses must be resumed, not fatal");
        assert_eq!(got, data, "resumed bytes must match the source exactly");
    }
}
