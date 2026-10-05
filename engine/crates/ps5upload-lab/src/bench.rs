//! Benchmark corpora, corpus statistics and result records (Tasks 26–28).
//!
//! Every item here is `pub` (C14): Task 27's scenarios import the corpora and the
//! statistics, and measure AVA1 transfers on identical inputs.

use std::collections::HashSet;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The dedup class is `len < SMALL` — the corpus generators' `last_small` rule. One
/// boundary table shared with the histogram buckets (C4), so the two cannot drift.
pub const SMALL: u64 = 64 << 10;

/// Histogram buckets (C4's table). A size falls into the **first** bucket whose range
/// contains it, so exactly 16 MiB lands in bucket 3 even though bucket 4's lower bound
/// re-states 16 MiB.
const BUCKETS: [(u64, u64); 5] = [
    (0, 4096),
    (4097, SMALL - 1),
    (SMALL, 1 << 20),
    ((1 << 20) + 1, 16 << 20),
    (16 << 20, u64::MAX),
];

/// Compressible-fraction sample budget per file: 1 MiB (A4).
const SAMPLE: u64 = 1 << 20;

/// A4: files larger than the budget are sampled head+middle+tail — three chunks of
/// 340 KiB, not a 1 MiB prefix (game data often starts with incompressible tables and
/// goes repetitive later, or vice versa). 3 × 340 KiB = 1 020 KiB ≤ 1 MiB.
const SAMPLE_CHUNK: u64 = 340 << 10;

/// `bench-corpus ppsa01342`'s file count (C5: the command passes this; tests pass
/// hundreds, which at scale 1.0 keeps the corpus under ~100 MiB).
pub const PPSA_COUNT: u64 = 223_000;

/// ~40 files per synthetic-game directory (1–8 levels deep).
pub const PER_DIR: u64 = 40;

/// A1: `corpus_listing` drops this marker at the corpus root. A listing carries no
/// bytes, so a duplicate-by-content ratio is not measurable for such a corpus —
/// `stats()` then reports `duplicate_ratio` as `NaN` (which serde_json writes as
/// `null`), never as a false 0 that Tasks 27/28 would treat as data. The listing
/// format itself is not extended for v1 (known limitation, ledger).
pub const LISTING_MARKER: &str = ".ps5upload-lab-listing";

/// The frozen record schema version (A3). Every record carries `schema: 1` plus the
/// identity fields; changing the schema means bumping this and saying what changed,
/// never silently.
// FIXME-ish note, not a lint hack: SCHEMA is consumed by the record-writing commands
// (the follow-up calibrate arm and Task 27's runner) and by the tests; until then the
// bin build sees it unused.
#[allow(dead_code)]
pub const SCHEMA: u64 = 1;

/// One corpus's statistics. Serializes (T27/T28 record it); `duplicate_ratio` is `NaN`
/// — JSON `null` — for a corpus reproduced from a listing (A1).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    /// File counts per `BUCKETS` (labels: `histogram_labels()`).
    pub histogram: [u64; 5],
    /// Byte-weighted share of the sampled bytes whose deflate output is ≤ 90 % of the
    /// input (C10) — the number the deferred zstd work needs. `0.0` when nothing was
    /// sampled (an all-empty corpus; a zero-length file contributes nothing).
    pub compressible_fraction: f64,
    /// Files with `len < SMALL` whose whole-file BLAKE3 was seen before, over all such
    /// files (C3/C4). `NaN` (JSON `null`) for listing corpora (A1).
    pub duplicate_ratio: f64,
}

/// Human labels for the five histogram buckets, matching `BUCKETS`.
pub fn histogram_labels() -> [&'static str; 5] {
    [
        "≤ 4 KiB",
        "4–64 KiB",
        "64 KiB–1 MiB",
        "1–16 MiB",
        "> 16 MiB",
    ]
}

/// Deterministic incompressible bytes: a BLAKE3 XOF keyed by a spread seed.
///
/// The key spreads the seed explicitly — `seed.to_le_bytes()` cycled to 32 bytes, not
/// `[seed as u8; 32]` — so two different seeds can never collide even if a future
/// "optimisation" drops the extra `update` (C9). The seed travels in the file name
/// (`large-{gib}g.bin`), so a corpus file documents its own content.
pub fn write_random(p: &Path, len: u64, seed: u64) -> io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut key = [0u8; 32];
    for (k, b) in key.iter_mut().zip(seed.to_le_bytes().iter().cycle()) {
        *k = *b;
    }
    let mut x = blake3::Hasher::new_keyed(&key)
        .update(&seed.to_le_bytes())
        .finalize_xof();
    let mut f = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(p)?);
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        x.fill(&mut buf[..n]);
        f.write_all(&buf[..n])?;
        left -= n as u64;
    }
    f.flush()
}

/// Deterministic repetitive bytes: a 4 KiB pattern derived from the seed, so deflate
/// shrinks it hard — the corpus's compressible 40 %.
fn write_repetitive(p: &Path, len: u64, seed: u64) -> io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let pat: Vec<u8> = (0..4096u64).map(|i| ((i * 7 + seed) % 61) as u8).collect();
    let mut f = io::BufWriter::new(std::fs::File::create(p)?);
    let mut left = len;
    while left > 0 {
        let n = left.min(pat.len() as u64) as usize;
        f.write_all(&pat[..n])?;
        left -= n as u64;
    }
    f.flush()
}

/// SplitMix64 for shapes (sizes, paths, directory depths) — never content.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// 60 % incompressible, 40 % repetitive — content, not shape (the plan's mix).
fn write_one(p: &Path, len: u64, seed: u64, rng: &mut Rng) -> io::Result<()> {
    if rng.next() % 10 < 6 {
        write_random(p, len, seed)
    } else {
        write_repetitive(p, len, seed)
    }
}

/// A duplicate is a byte-for-byte copy of an earlier file. The copy path never
/// computes or receives a length (C6), so no refactor can turn a duplicate into an
/// empty file.
fn write_duplicate(p: &Path, src: &Path) -> io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::copy(src, p)?;
    Ok(())
}

/// One incompressible file, `large-{gib}g.bin`. The seed **is** the GiB count, so the
/// name documents the content (C9): the same command reproduces the same bytes, and
/// two sizes never collide.
pub fn corpus_large(dir: &Path, gib: u64) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    write_random(&dir.join(format!("large-{gib}g.bin")), gib << 30, gib)
}

/// N files of 1–64 KiB, 64 per directory (`d0000/f000000.dat` …). Every 50th file is a
/// byte-identical copy of the most recently written file (a ~2 % duplicate ratio, so
/// the ratio is measurable). `last_small` updates only for files actually written
/// (C6); a duplicate of a duplicate is still byte-identical by construction.
pub fn corpus_tiny(dir: &Path, n: u64) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut rng = Rng(11);
    let mut last_small: Option<PathBuf> = None;
    for i in 0..n {
        let p = dir.join(format!("d{:04}/f{i:06}.dat", i / 64));
        let src = if rng.next().is_multiple_of(50) {
            last_small.clone()
        } else {
            None
        };
        match src {
            Some(src) => write_duplicate(&p, &src)?,
            None => {
                write_one(&p, rng.range(1024, 64 * 1024), i, &mut rng)?;
                last_small = Some(p);
            }
        }
    }
    Ok(())
}

/// The synthetic PPSA01342 game-folder shape (C5: `count` parametrises the plan's
/// hard-coded 223 000 — the command passes `PPSA_COUNT`, tests pass a few hundred):
/// 70 % ≤ 4 KiB, 20 % 4–64 KiB, 9 % 64 KiB–1 MiB, 1 % 1–16 MiB, every size multiplied
/// by `scale` (1.0 ≈ 30 GB); directories 1–8 levels deep under `Image0`, at most
/// `PER_DIR` files per directory. Content: 60 % incompressible / 40 % repetitive,
/// and ~2 % of small files are byte-identical copies of an earlier small file.
pub fn corpus_ppsa01342(dir: &Path, scale: f64, count: u64) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut rng = Rng(1342);
    let mut last_small: Option<PathBuf> = None;
    let mut path_stack: Vec<String> = vec!["Image0".into()];
    for i in 0..count {
        if i % PER_DIR == 0 && i > 0 {
            // A fresh full path every PER_DIR files (C5's ≤ 40-files-per-directory
            // cadence): the plan's truncate-and-grow tree reuses shared parents
            // whenever the depth draw lands on an ancestor's level, so a directory
            // could accumulate several windows' worth of files. Fresh names at
            // every level keep the shape (Image0 root, 1–8 levels) and make every
            // directory hold at most PER_DIR files. A depth-1 draw after the first
            // window would reuse Image0 itself, so it goes one level deeper.
            let mut depth = rng.range(1, 8) as usize;
            if depth == 1 {
                depth = 2;
            }
            path_stack.clear();
            path_stack.push("Image0".into());
            for _ in 1..depth {
                path_stack.push(format!("dir{:05}", rng.next() % 100_000));
            }
        }
        let bucket = rng.next() % 100;
        let len = match bucket {
            0..=69 => rng.range(0, 4 << 10),
            70..=89 => rng.range(4 << 10, 64 << 10),
            90..=98 => rng.range(64 << 10, 1 << 20),
            _ => rng.range(1 << 20, 16 << 20),
        };
        let len = ((len as f64) * scale) as u64;
        let p = dir.join(path_stack.join("/")).join(format!("f{i:06}.bin"));
        let src = if len < SMALL && rng.next().is_multiple_of(50) {
            last_small.clone()
        } else {
            None
        };
        match src {
            Some(src) => write_duplicate(&p, &src)?,
            None => {
                write_one(&p, len, i, &mut rng)?;
                if len < SMALL {
                    last_small = Some(p);
                }
            }
        }
    }
    Ok(())
}

/// Reproduces a real game listing exactly: one `<size> <path>` line per file, sizes
/// multiplied by `scale`, synthetic bytes (the 60/40 mix). Rejects absolute paths and
/// `..`, a duplicate relative path, a path with a trailing `/`, and an empty path;
/// skips blank lines and `#` comments (C7). Fabricates no duplicates — a listing
/// carries no bytes, so the duplicate ratio is not measurable: the generator drops
/// `LISTING_MARKER` and `stats()` reports `duplicate_ratio` as `NaN`, never 0 (A1).
pub fn corpus_listing(dir: &Path, listing: &Path, scale: f64) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut rng = Rng(5);
    let mut seen: HashSet<String> = HashSet::new();
    let text = std::fs::read_to_string(listing)?;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue; // C7: blank lines and comments are skipped
        }
        let bad = |what: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {}: {what}", i + 1),
            )
        };
        let Some((size, path)) = trimmed.split_once(' ') else {
            return Err(bad(format!("expected `<size> <path>`, got {trimmed:?}")));
        };
        let size: u64 = size
            .trim()
            .parse()
            .map_err(|_| bad(format!("bad size {size:?}")))?;
        let path = path.trim();
        let rel = Path::new(path);
        if rel.as_os_str().is_empty() {
            return Err(bad("empty path".into()));
        }
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(bad(format!("path must be relative without `..`: {path}")));
        }
        if path.ends_with('/') {
            return Err(bad(format!("trailing `/`: {path}")));
        }
        if !seen.insert(path.to_string()) {
            return Err(bad(format!("duplicate path: {path}")));
        }
        write_one(
            &dir.join(rel),
            ((size as f64) * scale) as u64,
            i as u64,
            &mut rng,
        )?;
    }
    std::fs::write(dir.join(LISTING_MARKER), listing.display().to_string())?;
    Ok(())
}

/// A2: the CLI's generator guard. Refuses to write into a non-empty directory unless
/// `force` — a generator pointed at a real folder is a data-loss bug waiting to
/// happen. With `--force`, logs what is being overwritten. Never deletes the target
/// directory itself; the generators write into it. A missing directory is created.
pub fn ensure_writable_target(dir: &Path, force: bool) -> io::Result<()> {
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            let entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            if entries.is_empty() {
                return Ok(());
            }
            if !force {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} is not empty ({} entries); pass --force to write anyway",
                        dir.display(),
                        entries.len()
                    ),
                ));
            }
            eprintln!(
                "overwriting {} existing entries in {} (e.g. {})",
                entries.len(),
                dir.display(),
                entries[0].path().display()
            );
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::create_dir_all(dir),
        Err(e) => Err(e),
    }
}

/// Walks `dir` with an explicit stack and computes the statistics.
///
/// - C8: uses `symlink_metadata`, skips symlinks (counted, reported on stderr — a
///   real game folder may have them; the generators never create one), and reads
///   samples with `Read::take(len)` so a file that shrinks between `metadata` and
///   `open` cannot fail the whole run with `read_exact`'s error.
/// - C10: the compressible fraction is a **byte-weighted** sample, not a file count.
/// - A4: the sample is head+middle+tail (3 × 340 KiB) for files > 1 MiB, the whole
///   file otherwise.
/// - C3: the dedup digest reads the whole file — the 1 MiB budget applies only to the
///   compressible-fraction sample.
///
/// Single-threaded and that is fine (C13): a 223 000-file corpus reads ~15 GiB of
/// samples and deflates them — minutes, not seconds.
pub fn stats(dir: &Path) -> io::Result<Stats> {
    let mut st = Stats {
        files: 0,
        bytes: 0,
        histogram: [0; 5],
        compressible_fraction: 0.0,
        duplicate_ratio: 0.0,
    };
    let (mut sampled, mut compressible, mut small, mut dups, mut symlinks) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut from_listing = false;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let p = e.path();
            let m = std::fs::symlink_metadata(&p)?;
            let ft = m.file_type();
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if ft.is_symlink() {
                symlinks += 1;
                continue;
            }
            if p == dir.join(LISTING_MARKER) {
                from_listing = true; // A1: the ratio is not measurable for this corpus
                continue;
            }
            let len = m.len();
            st.files += 1;
            st.bytes += len;
            let bucket = BUCKETS
                .iter()
                .position(|(lo, hi)| len >= *lo && len <= *hi)
                .expect("BUCKETS covers every u64");
            st.histogram[bucket] += 1;

            let mut f = std::fs::File::open(&p)?;

            // Compressible-fraction sample: head+middle+tail, or the whole file.
            let mut sample = Vec::with_capacity(len.min(SAMPLE) as usize);
            if len <= SAMPLE {
                io::Read::take(&mut f, len).read_to_end(&mut sample)?;
            } else {
                let mid = len / 2 - SAMPLE_CHUNK / 2;
                for at in [0u64, mid, len - SAMPLE_CHUNK] {
                    f.seek(io::SeekFrom::Start(at))?;
                    io::Read::take(&mut f, SAMPLE_CHUNK).read_to_end(&mut sample)?;
                }
            }
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(&sample)?;
            let out = enc.finish()?;
            sampled += sample.len() as u64;
            if !sample.is_empty() && (out.len() as f64) <= sample.len() as f64 * 0.9 {
                compressible += sample.len() as u64;
            }

            // Dedup digest: the whole file, not the sample (C3).
            if len < SMALL {
                small += 1;
                f.seek(io::SeekFrom::Start(0))?;
                let mut full = Vec::with_capacity(len as usize);
                io::Read::take(&mut f, len).read_to_end(&mut full)?;
                if !seen.insert(*blake3::hash(&full).as_bytes()) {
                    dups += 1;
                }
            }
        }
    }
    st.compressible_fraction = if sampled == 0 {
        0.0
    } else {
        compressible as f64 / sampled as f64
    };
    st.duplicate_ratio = if from_listing {
        f64::NAN
    } else if small == 0 {
        0.0
    } else {
        dups as f64 / small as f64
    };
    if symlinks > 0 {
        eprintln!("stats: skipped {symlinks} symlink(s) in {}", dir.display());
    }
    Ok(st)
}

/// The identity fields every record carries (A3): a machine tag (the host OS/arch, or
/// `PS5UPLOAD_BENCH_MACHINE` to disambiguate two identical hosts) and a start
/// timestamp (unix epoch seconds).
// Consumed via `record_envelope` (below); unused until the first record-writing
// command lands.
#[allow(dead_code)]
fn bench_machine() -> String {
    std::env::var("PS5UPLOAD_BENCH_MACHINE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

#[allow(dead_code)] // see bench_machine
fn started_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The frozen record envelope (A3, coordinator ruling 2026-10-02): every record starts
/// with `schema: 1` and the identity fields `machine`, `started_at`, `corpus`, `seed`,
/// `protocol`; the caller adds its kind-specific measured fields, with units in the
/// field names (`bytes`, `ms`, `files_per_s`). Tasks 27/28 consume the same file, so a
/// schema change means bumping `SCHEMA` and saying what changed — never silently.
///
/// Kind-specific fields, frozen now so the follow-up arms only append:
/// - `"calibrate"` (added with Task 26b): `console`, `dir`, `files`,
///   `size_bytes`, and `points: [{workers, files_per_s, create_ms, fsync_ms}]`
///   — the wire reports µs; the record converts to `ms`.
/// - `"bench"` (Task 27): the scenario record, same envelope.
// Consumed by the same commands as `record` (below).
#[allow(dead_code)]
pub fn record_envelope(
    kind: &str,
    corpus: Option<&str>,
    seed: Option<u64>,
    protocol: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema": SCHEMA,
        "kind": kind,
        "machine": bench_machine(),
        "started_at": started_at(),
        "corpus": corpus,
        "seed": seed,
        "protocol": protocol,
    })
}

/// Appends one JSON record as a single line to `out` (C2/C15: the path is the
/// caller's — Task 27's runner exposes `--out FILE`, and the lab's default is the lab
/// data dir's `bench-results.jsonl`; two concurrent runs must not overwrite each
/// other). The whole line goes out in one `write_all` on an `O_APPEND` handle, so a
/// concurrent run can never interleave a partial line.
// Consumed by the first record-writing command (the follow-up calibrate arm) and
// Task 27's runner; the tests exercise it today.
#[allow(dead_code)]
pub fn record(out: &Path, line: &serde_json::Value) -> io::Result<()> {
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)?;
    let mut bytes = line.to_string();
    bytes.push('\n');
    f.write_all(bytes.as_bytes())
}

// ════════════════════════════════════════════════════════════════════════════
// Task 27 — the scenario runner: AVA1 transfers on identical inputs.
// ════════════════════════════════════════════════════════════════════════════

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use ps5upload_core::download::DownloadKind;
use ps5upload_core::fs_ops::{list_dir, ListDirOptions};
use ps5upload_core::transfer::{TransferConfig, TransferResult};
use serde_json::{json, Map, Value};

/// `bench --help`.
pub const HELP: &str = "\
Usage: ps5upload-lab bench CONSOLE SCENARIO --proto ava1 --src PATH [options]

CONSOLE is a bare host (a trailing :port is ignored); the address is derived from the host:
  AVA1 HOST:9120 (AVA1_PORT overrides).

Scenarios:
  upload-file   --src local FILE        --dest console FILE path
  upload-dir    --src local DIR         --dest console DIR path
  download      --src console path      --dest local directory (default: a temp directory)
  copy          --src console path      --dest console path (same console)
  drop60        --src local file/dir    --dest console DIR   connections killed
                every --kill-every-s seconds through an in-process proxy;
                passes only with drops > 0 and resent <= drops x 64 MiB
  resume        --src local file/dir    --dest console path  payload stopped at 50 % durable
                and re-sent (needs --elf); passes only if no durable byte went twice
  relay         --src console path      --dest path on --to  console to console

Options:
  --proto ava1          required: the protocol under test (never read from the environment)
  --src PATH            required (see the scenarios)
  --dest PATH           console path (local directory for download). Recursive deletes happen
                        here between runs, so a console path must mirror the payload's cleanup
                        allowlist: /data or /mnt/ext<N> or /mnt/usb<N>, then
                        /ps5upload/tests/..., with a component equal to `bench` or starting
                        with `bench-` below tests (`bench-src` is reserved for staging sources
                        and is refused). A local download directory must be absent, empty or
                        bench-named.
  --warmup              run one extra, flagged (run_kind=warmup, run 0) first run; the summary
                        medians use warm runs only (cold ones if there are none)
  --runs N              repetitions, N >= 1 (default 1)
  --elf FILE            payload for `resume` (default: --elf, then PS5UPLOAD_ELF, then
                        payload/ps5upload.elf or ../payload/ps5upload.elf)
  --to CONSOLE2         second console for `relay`
  --out FILE            results file (default: <data dir>/bench-results.jsonl)
  --kill-every-s N      drop60 only: seconds between kills (default min(60, expected
                        seconds / 4) at ~100 MB/s, so a run sees at least ~3 kills; a run
                        with zero kills is reported as 'not exercised')

Every attempt has a deadline: 120 s + bytes / 5 MB/s + files / 50 s; expiry fails the run
(and stops the remaining runs, since the abandoned transfer may still be running).

Every run appends one JSON line to the results file; a summary per drive follows. Run from
the engine/ directory. Nothing prompts: a console that needs a pairing code is an error.";

pub const SCENARIOS: [&str; 7] = [
    "upload-file",
    "upload-dir",
    "download",
    "copy",
    "drop60",
    "resume",
    "relay",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Ava1,
}

impl Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            Proto::Ava1 => "ava1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    UploadFile,
    UploadDir,
    Download,
    Copy,
    Drop60,
    Resume,
    Relay,
}

impl Scenario {
    pub fn name(self) -> &'static str {
        SCENARIOS[self as usize]
    }
    fn parse(s: &str) -> Option<Scenario> {
        Some(match s {
            "upload-file" => Scenario::UploadFile,
            "upload-dir" => Scenario::UploadDir,
            "download" => Scenario::Download,
            "copy" => Scenario::Copy,
            "drop60" => Scenario::Drop60,
            "resume" => Scenario::Resume,
            "relay" => Scenario::Relay,
            _ => return None,
        })
    }
}

/// The parsed `bench` command line (C2: the console comes from `bench`'s own positional
/// list; the lab's global ADDR is never consulted).
#[derive(Debug, Clone)]
pub struct BenchArgs {
    pub host: String,
    /// `host:9120` (`AVA1_PORT` honoured) — AVA1.
    pub ava1: String,
    pub scenario: Scenario,
    pub proto: Proto,
    pub src: String,
    pub dest: Option<String>,
    pub runs: u32,
    pub elf: Option<PathBuf>,
    pub to: Option<String>,
    pub out: Option<PathBuf>,
    /// `None` = derive it from the corpus size (see `derive_kill_interval`).
    pub kill_every_s: Option<u64>,
    pub warmup: bool,
}

fn host_of(console: &str) -> String {
    console
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(console)
        .to_string()
}

impl BenchArgs {
    pub fn parse<S: AsRef<str>>(args: &[S]) -> anyhow::Result<BenchArgs> {
        let a: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
        let mut pos: Vec<&str> = Vec::new();
        let (mut proto, mut src, mut dest, mut elf, mut to, mut out) =
            (None, None, None, None, None, None);
        let (mut runs, mut kill_every_s, mut warmup) = (1u32, None::<u64>, false);
        let mut i = 0;
        while i < a.len() {
            let Some(flag) = a[i].strip_prefix("--") else {
                pos.push(a[i]);
                i += 1;
                continue;
            };
            if flag == "warmup" {
                warmup = true;
                i += 1;
                continue;
            }
            let v = *a
                .get(i + 1)
                .ok_or_else(|| anyhow!("--{flag} needs a value"))?;
            i += 2;
            match flag {
                "proto" => proto = Some(v),
                "src" => src = Some(v.to_string()),
                "dest" => dest = Some(v.to_string()),
                "elf" => elf = Some(PathBuf::from(v)),
                "to" => to = Some(v.to_string()),
                "out" => out = Some(PathBuf::from(v)),
                "runs" => {
                    runs = v
                        .parse()
                        .map_err(|_| anyhow!("--runs needs a whole number, got {v:?}"))?;
                    if runs == 0 {
                        bail!("--runs must be at least 1 (0 is an error, not \"once\")");
                    }
                }
                "kill-every-s" => {
                    kill_every_s = Some(
                        v.parse()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| anyhow!("--kill-every-s needs a positive number"))?,
                    );
                }
                _ => bail!("unknown option --{flag} (see `bench --help`)"),
            }
        }
        let [console, scenario] = pos[..] else {
            bail!("bench needs CONSOLE and SCENARIO (see `bench --help`)");
        };
        let scenario = Scenario::parse(scenario).ok_or_else(|| {
            anyhow!(
                "unknown scenario {scenario:?}; valid scenarios: {}",
                SCENARIOS.join(", ")
            )
        })?;
        let proto = match proto {
            Some("ava1") => Proto::Ava1,
            Some(p) => bail!("unknown protocol {p:?}; valid protocols: ava1"),
            None => bail!("--proto is required; valid protocols: ava1"),
        };
        let host = host_of(console);
        let parsed = BenchArgs {
            ava1: crate::ava1_cmds::ava1_addr(&host),
            host,
            scenario,
            proto,
            src: src.ok_or_else(|| anyhow!("--src is required"))?,
            dest,
            runs,
            elf,
            to,
            out,
            kill_every_s,
            warmup,
        };
        if parsed.scenario != Scenario::Download && parsed.dest.is_none() {
            bail!("--dest is required for {}", parsed.scenario.name());
        }
        if parsed.scenario == Scenario::Relay && parsed.to.is_none() {
            bail!("relay needs --to CONSOLE2");
        }
        if parsed.scenario == Scenario::Copy {
            check_copy_overlap(&parsed.src, parsed.dest.as_deref().unwrap_or(""))?;
        }
        Ok(parsed)
    }
}

/// A bench-owned name: `bench` or `bench-<anything>`.
fn bench_named(c: &str) -> bool {
    c == "bench" || c.starts_with("bench-")
}

/// A destination the bench may delete recursively between runs (C8). It mirrors the
/// payload's `cleanup_path_allowed` (payload/src/runtime.c): `/data`, `/mnt/ext<N>` or
/// `/mnt/usb<N>`, then `/ps5upload/tests/...`, no `.`/`..`. On top of that the bench
/// demands a `bench` / `bench-*` component *below* `tests` (never the shared sandbox
/// root itself) and reserves `bench-src*` for staged sources, which no run may delete.
pub fn check_console_dest(path: &str) -> anyhow::Result<()> {
    if !path.starts_with('/') || path.contains("//") || path.contains('\\') {
        bail!("{path:?}: the destination must be an absolute console path");
    }
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if comps.iter().any(|c| *c == ".." || *c == ".") {
        bail!("{path:?}: the destination must not contain . or ..");
    }
    let drive_len = match comps.as_slice() {
        ["data", ..] => 1,
        ["mnt", d, ..]
            if (d.starts_with("ext") || d.starts_with("usb"))
                && d.len() > 3
                && d[3..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            2
        }
        _ => bail!(
            "{path:?}: the payload only cleans /data, /mnt/ext<N> or /mnt/usb<N> (under \
             /ps5upload/tests/)"
        ),
    };
    let rest = &comps[drive_len..];
    if rest.len() < 3 || rest[0] != "ps5upload" || rest[1] != "tests" {
        bail!(
            "{path:?}: the payload only cleans <drive>/ps5upload/tests/..., with a bench \
             directory below tests (e.g. /data/ps5upload/tests/bench/tiny)"
        );
    }
    let below = &rest[2..];
    if below.iter().any(|c| c.starts_with("bench-src")) {
        bail!("{path:?}: bench-src is reserved for staged sources and is never a destination");
    }
    if !below.iter().any(|c| bench_named(c)) {
        bail!("{path:?}: a component below tests must be `bench` or start with `bench-`");
    }
    Ok(())
}

/// The local download directory: the run deletes `<dir>/<basename of --src>` between
/// runs, so the directory must be absent, empty, or bench-named, and the basename must
/// be a real name (never empty, `.` or `..`).
pub fn check_local_dest(dir: &Path, src: &str) -> anyhow::Result<()> {
    let base = basename(src);
    if base.is_empty() || base == "." || base == ".." {
        bail!("--src {src:?} has no usable name to download into");
    }
    if dir.as_os_str().is_empty() || dir.parent().is_none() {
        bail!(
            "{}: refusing a root or empty download directory",
            dir.display()
        );
    }
    if dir.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        bail!(
            "{}: the download directory must not contain . or ..",
            dir.display()
        );
    }
    // What gets deleted is `<real dir>/<base>`, so the *resolved* directory decides: a
    // `bench-*` ancestor or a symlink named `bench-*` that points elsewhere is not enough.
    let (resolved, exists) = match std::fs::canonicalize(dir) {
        Ok(c) => (c, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (dir.to_path_buf(), false),
        Err(e) => bail!("{}: {e}", dir.display()),
    };
    let named = resolved
        .file_name()
        .is_some_and(|n| bench_named(&n.to_string_lossy()));
    let empty_or_absent = !exists
        || std::fs::read_dir(&resolved)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(false);
    if !named && !empty_or_absent {
        bail!(
            "{} (resolves to {}): the download directory holds files and its final component \
             is not bench-named; use an absent, empty or bench-* directory (the bench deletes \
             <dir>/{base} between runs)",
            dir.display(),
            resolved.display()
        );
    }
    Ok(())
}

/// copy: the destination may not contain the source nor sit inside it.
pub fn check_copy_overlap(src: &str, dest: &str) -> anyhow::Result<()> {
    let (s, d) = (src.trim_end_matches('/'), dest.trim_end_matches('/'));
    let inside = |a: &str, b: &str| a == b || a.starts_with(&format!("{b}/"));
    if inside(d, s) || inside(s, d) {
        bail!("copy: --dest {dest:?} and --src {src:?} overlap");
    }
    Ok(())
}

/// `/mnt/usb0/x` → `/mnt/usb0`, `/data/x` → `/data`: the unit a calibration describes.
pub fn drive_of(path: &str) -> String {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    match comps.as_slice() {
        ["mnt", d, ..] => format!("/mnt/{d}"),
        [first, ..] => format!("/{first}"),
        [] => "/".into(),
    }
}

/// One result line (the plan's shape; every key always present).
#[derive(Debug, Clone)]
pub struct BenchRun {
    pub console: String,
    pub scenario: String,
    pub proto: String,
    pub run: u32,
    pub files: u64,
    pub bytes: u64,
    pub seconds: f64,
    pub resent: u64,
    pub max_lanes: u64,
    pub bottleneck: String,
    pub sequential: bool,
    pub ok: bool,
    pub error: Option<String>,
}

impl BenchRun {
    pub fn json(&self) -> Value {
        let date = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // A run that took no time has no rate; 0 keeps the JSON finite (no NaN/inf/null).
        let rate = |n: u64| {
            if self.seconds > 0.0 && self.seconds.is_finite() {
                n as f64 / self.seconds
            } else {
                0.0
            }
        };
        json!({
            "kind": "bench", "date": date, "console": self.console, "scenario": self.scenario,
            "proto": self.proto, "run": self.run, "files": self.files, "bytes": self.bytes,
            "seconds": self.seconds,
            "mb_s": rate(self.bytes) / 1e6,
            "files_s": rate(self.files),
            "resent": self.resent, "max_lanes": self.max_lanes, "bottleneck": self.bottleneck,
            "sequential": self.sequential, "ok": self.ok, "error": self.error,
        })
    }
}

/// C12: a drop60 run passes only if the fault fired and cost at most one 64 MiB credit
/// window of resent bytes per drop.
pub fn drop60_verdict(resent: u64, drops: u64) -> Result<(), String> {
    if drops == 0 {
        return Err(NOT_EXERCISED.into());
    }
    let allowed = drops.saturating_mul(64 << 20);
    if resent > allowed {
        return Err(format!(
            "resent {resent} bytes over {drops} drop(s); at most {allowed} (64 MiB per drop) allowed"
        ));
    }
    Ok(())
}

/// Throughput assumed when sizing the drop60 kill interval (the PS5 plateau is ~100 MB/s).
const ASSUMED_MB_S: u64 = 100;

/// drop60 kill interval: the explicit flag, else min(60, expected seconds / 4) at
/// `ASSUMED_MB_S` (at least 1 s), so a run long enough to matter sees at least three kills.
pub fn derive_kill_interval(explicit: Option<u64>, bytes: u64) -> u64 {
    explicit.unwrap_or_else(|| {
        let expected_s = bytes / (ASSUMED_MB_S * 1_000_000);
        (expected_s / 4).clamp(1, 60)
    })
}

/// The hint printed with a drop60 run in which the fault never fired.
pub const NOT_EXERCISED: &str =
    "not exercised: no connection was dropped before the transfer finished \
     (use a larger --src or a shorter --kill-every-s)";

/// Waits until every `host:port` accepts a TCP connection (the restarted helper's
/// listeners come up some seconds after `send-elf` returns) or `deadline` passes. Each
/// address must answer on two consecutive probes so a listener that is still being set up
/// is not mistaken for ready. Returns the time waited, or which addresses stayed silent.
pub fn wait_for_ports(
    addrs: &[String],
    deadline: Duration,
    poll: Duration,
) -> Result<Duration, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    let start = Instant::now();
    let probe = |a: &str| -> bool {
        a.to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .is_some_and(|sa| TcpStream::connect_timeout(&sa, Duration::from_secs(2)).is_ok())
    };
    let mut streak = vec![0u32; addrs.len()];
    loop {
        for (i, a) in addrs.iter().enumerate() {
            streak[i] = if probe(a) { streak[i] + 1 } else { 0 };
        }
        if streak.iter().all(|n| *n >= 2) {
            return Ok(start.elapsed());
        }
        if start.elapsed() >= deadline {
            let silent: Vec<&str> = addrs
                .iter()
                .zip(&streak)
                .filter(|(_, n)| **n < 2)
                .map(|(a, _)| a.as_str())
                .collect();
            return Err(format!(
                "{} not answering after {} s",
                silent.join(", "),
                deadline.as_secs()
            ));
        }
        std::thread::sleep(poll);
    }
}

/// How long the helper gets to come back after a restart or before a cleanup.
const PORTS_DEADLINE: Duration = Duration::from_secs(60);

/// C14: the middle value, or the mean of the two middle values of an even count; `None`
/// for no values (printed as `n/a`).
pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    Some(if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    })
}

/// C13: the protocol an upload's commit acknowledgement shows. AVA1 says
/// `"protocol":"ava1"`; anything else is reported as unrecognised.
pub fn observe_upload(tx_id_hex: &str, commit_ack_body: &str) -> String {
    let body: Option<Value> = serde_json::from_str(commit_ack_body).ok();
    let protocol = body
        .as_ref()
        .and_then(|b| b.get("protocol"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match protocol {
        "ava1" => "ava1".into(),
        other => format!("unrecognised(protocol={other:?}, tx_id={tx_id_hex:?})"),
    }
}

pub fn verify_protocol(asked: Proto, observed: &str) -> Result<(), String> {
    if observed == asked.as_str() {
        Ok(())
    } else {
        Err(format!(
            "protocol mismatch: asked {}, saw {observed}",
            asked.as_str()
        ))
    }
}

// ─── Results, ceilings, summary ─────────────────────────────────────────────────

/// One recorded run plus what the summary needs.
#[derive(Debug, Clone)]
pub struct Row {
    pub run: BenchRun,
    pub drive: String,
    pub verified: bool,
    /// `warmup`, `cold` (the first measured run of an invocation without --warmup) or `warm`.
    pub run_kind: String,
    /// The drive's calibrated create ceiling (files/s), when a calibrate record exists.
    pub ceiling: Option<u64>,
    pub extra: Map<String, Value>,
}

impl Row {
    fn json(&self, started_at: u64, corpus: &str) -> Value {
        let mut v = record_envelope("bench", Some(corpus), None, &self.run.proto);
        let o = v.as_object_mut().expect("envelope is an object");
        if let Value::Object(run) = self.run.json() {
            o.extend(run);
        }
        o.insert("started_at".into(), started_at.into());
        o.insert("drive".into(), self.drive.clone().into());
        o.insert("verified".into(), self.verified.into());
        o.insert("run_kind".into(), self.run_kind.clone().into());
        o.insert("ceiling_files_s".into(), self.ceiling.into());
        o.extend(self.extra.clone());
        v
    }
}

/// The newest calibrate record for this console's drive: (max files/s over its worker
/// points, the directory it measured). Read from the results file before the runs add to
/// it. A calibration is 4 KiB files, so it bounds tiny-file create rates, nothing else.
pub fn calibrated_ceiling(results: &Path, host: &str, drive: &str) -> Option<(u64, String)> {
    let text = std::fs::read_to_string(results).ok()?;
    let mut best: Option<(u64, u64, String)> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["kind"] != "calibrate" {
            continue;
        }
        let (Some(console), Some(dir)) = (v["console"].as_str(), v["dir"].as_str()) else {
            continue;
        };
        if host_of(console) != host && console != host {
            continue;
        }
        if drive_of(dir) != drive {
            continue;
        }
        let top = v["points"]
            .as_array()
            .map(|p| {
                p.iter()
                    .filter_map(|x| x["files_per_s"].as_u64())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let at = v["started_at"].as_u64().unwrap_or(0);
        if top > 0 && best.as_ref().is_none_or(|b| at >= b.0) {
            best = Some((at, top, dir.to_string()));
        }
    }
    best.map(|(_, top, dir)| (top, dir))
}

fn fmt1(v: Option<f64>) -> String {
    v.map_or("n/a".into(), |x| format!("{x:.1}"))
}

/// The end-of-run table (C14): per scenario and drive, per protocol — successful runs,
/// median MB/s and files/s, the drive's calibrated ceiling beside them — then the
/// pass/fail counts and the results path (C15).
pub fn summary(rows: &[Row], out: &Path) -> String {
    let mut groups: BTreeMap<(String, String), Vec<&Row>> = BTreeMap::new();
    for r in rows {
        groups
            .entry((r.run.scenario.clone(), r.drive.clone()))
            .or_default()
            .push(r);
    }
    let mut s = String::new();
    for ((scenario, drive), rs) in &groups {
        let ceiling = rs.iter().find_map(|r| r.ceiling);
        s += &format!("\n{scenario} on {drive}");
        match ceiling {
            Some(c) => s += &format!("  (calibrated create ceiling: {c} files/s, 4 KiB files)\n"),
            None => s += "  (no calibrate record for this drive)\n",
        }
        s += "  proto  ok/runs   median MB/s   median files/s   files/s vs ceiling\n";
        for proto in ["ava1"] {
            // Warm-up runs are flagged and never counted in the table; medians use warm
            // runs, falling back to the cold first run when no warm run exists.
            let mine: Vec<&&Row> = rs
                .iter()
                .filter(|r| r.run.proto == proto && r.run_kind != "warmup")
                .collect();
            if mine.is_empty() {
                continue;
            }
            let any_warm = mine.iter().any(|r| r.run_kind == "warm");
            let kind = if any_warm { "warm" } else { "cold" };
            let good: Vec<&&Row> = mine
                .iter()
                .copied()
                .filter(|r| r.run.ok && r.run_kind == kind)
                .collect();
            let col = |f: fn(&BenchRun) -> f64| {
                median(&good.iter().map(|r| f(&r.run)).collect::<Vec<_>>())
            };
            let mb = col(|r| r.json()["mb_s"].as_f64().unwrap_or(0.0));
            let fs = col(|r| r.json()["files_s"].as_f64().unwrap_or(0.0));
            let pct = match (fs, ceiling) {
                (Some(f), Some(c)) if c > 0 => format!("{:.0} %", f / c as f64 * 100.0),
                _ => "n/a".into(),
            };
            s += &format!(
                "  {proto:<5}  {:>2}/{:<4}    {:>11}   {:>14}   {pct:>18}  [{kind} runs]\n",
                good.len(),
                mine.len(),
                fmt1(mb),
                fmt1(fs)
            );
        }
    }
    let ok = rows.iter().filter(|r| r.run.ok).count();
    s += &format!(
        "\n{} run(s): {ok} passed, {} failed\nresults: {}\n",
        rows.len(),
        rows.len() - ok,
        std::path::absolute(out)
            .unwrap_or_else(|_| out.to_path_buf())
            .display()
    );
    s
}

// ─── The runner ─────────────────────────────────────────────────────────────────

/// Which AVA1 pool the AVA1 calls use: the process's own (the engine's pool — same
/// identity and pins) or an injected one (the lab's proxy override; tests, C11).
#[derive(Clone)]
pub enum PoolRef {
    Global,
    Owned(Arc<ps5upload_ava1::Pool>),
}

impl PoolRef {
    fn get(&self) -> &ps5upload_ava1::Pool {
        match self {
            PoolRef::Global => ps5upload_ava1::pool(),
            PoolRef::Owned(p) => p,
        }
    }
}

/// Everything environmental, so tests can run the same scenarios against a loopback
/// host: no console cleanup, no idle wait.
pub struct Env {
    pub pool: PoolRef,
    pub ava_dir: PathBuf,
    pub idle: Duration,
    pub clean: bool,
    pub out: PathBuf,
}

impl Env {
    /// C11: the AVA1 address is overridden through a per-pool address (`Pool::with_addr`)
    /// only when `AVA1_PORT` moves it off the default; otherwise the engine's own pool.
    pub fn production(args: &BenchArgs) -> Env {
        let ava_dir = crate::ava1_cmds::ava_dir();
        let default = ps5upload_ava1::pool::ava1_addr(&args.host);
        let pool = if args.ava1 == default {
            PoolRef::Global
        } else {
            PoolRef::Owned(Arc::new(
                ps5upload_ava1::Pool::new(ava_dir.clone()).with_addr(args.ava1.clone()),
            ))
        };
        Env {
            pool,
            ava_dir,
            idle: Duration::from_secs(5),
            clean: true,
            out: args
                .out
                .clone()
                .unwrap_or_else(crate::ava1_cmds::bench_results_default),
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a blocking bench task panicked")
}

/// What a finished attempt measured. `seconds` times the transfer call only.
struct Measured {
    seconds: f64,
    resent: u64,
    max_lanes: u64,
    bottleneck: String,
    sequential: bool,
    /// The protocol the evidence shows (C13).
    observed: String,
    verified_by: &'static str,
    /// An error the scenario's own pass rule found in an otherwise finished run.
    verdict: Option<String>,
    extra: Map<String, Value>,
}

impl Measured {
    fn new(seconds: f64, observed: String, verified_by: &'static str) -> Measured {
        Measured {
            seconds,
            resent: 0,
            max_lanes: 0,
            bottleneck: "n/a".into(),
            sequential: false,
            observed,
            verified_by,
            verdict: None,
            extra: Map::new(),
        }
    }
}

/// A failed attempt: how long it ran, and why.
type Attempt = Result<Measured, (f64, String)>;

struct ElfInfo {
    path: PathBuf,
    size: u64,
    mtime: u64,
    hash12: String,
    stamped: Arc<Vec<u8>>,
}

struct Prep {
    files: u64,
    bytes: u64,
    src_is_dir: bool,
    kind: Option<DownloadKind>,
    corpus: String,
    drive: String,
    ceiling: Option<(u64, String)>,
    elf: Option<ElfInfo>,
    setup_ms: Option<u64>,
}

fn walk_local(root: &Path) -> std::io::Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let p = e?.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.is_dir() {
                stack.push(p);
            } else if m.is_file() {
                let rel = p
                    .strip_prefix(root)
                    .expect("walked below the root")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, p, m.len()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// What a console path holds: the size of each file under it.
struct ConsolePlan {
    sizes: Vec<u64>,
}

/// One directory, every page of it. The payload clamps a listing to 256 entries, so a
/// directory with more children is read in pages: a short page that was not buffer-truncated
/// is the end, an exact multiple of 256 asks once more.
fn list_dir_all(console: &str, dir: &str) -> anyhow::Result<Vec<ps5upload_core::fs_ops::DirEntry>> {
    const PAGE: u64 = 256;
    let mut all = Vec::new();
    let mut offset = 0u64;
    loop {
        let l = list_dir(
            console,
            dir,
            ListDirOptions {
                offset,
                limit: PAGE,
            },
        )
        .with_context(|| format!("list {dir} (offset {offset})"))?;
        let n = l.entries.len() as u64;
        all.extend(l.entries);
        offset += n;
        if n == 0 || (!l.truncated && n < PAGE) {
            return Ok(all);
        }
    }
}

fn enumerate_console(console: &str, src: &str, kind: DownloadKind) -> anyhow::Result<ConsolePlan> {
    fn walk(console: &str, dir: &str, depth: u32, out: &mut Vec<u64>) -> anyhow::Result<()> {
        if depth > 64 {
            bail!("{dir}: nested deeper than 64 levels");
        }
        for e in list_dir_all(console, dir)? {
            let child = format!("{}/{}", dir.trim_end_matches('/'), e.name);
            match e.kind.as_str() {
                "dir" => walk(console, &child, depth + 1, out)?,
                "file" => out.push(e.size),
                _ => {}
            }
        }
        Ok(())
    }
    let mut sizes = Vec::new();
    match kind {
        DownloadKind::File => {
            let name = basename(src);
            let parent = match src.trim_end_matches('/').rsplit_once('/') {
                Some((p, _)) if !p.is_empty() => p.to_string(),
                _ => "/".to_string(),
            };
            let e = list_dir_all(console, &parent)?
                .into_iter()
                .find(|e| e.name == name)
                .ok_or_else(|| anyhow!("source file not found: {src}"))?;
            if e.kind != "file" {
                bail!("{src} is not a regular file (kind={})", e.kind);
            }
            sizes.push(e.size);
        }
        DownloadKind::Folder => walk(console, src, 0, &mut sizes)?,
    }
    Ok(ConsolePlan { sizes })
}

/// A console source: a file first, else a folder (the listing error of the wrong guess
/// is discarded; both failing reports both).
fn console_plan(console: &str, src: &str) -> anyhow::Result<(DownloadKind, ConsolePlan)> {
    match enumerate_console(console, src, DownloadKind::File) {
        Ok(p) => Ok((DownloadKind::File, p)),
        Err(file_err) => enumerate_console(console, src, DownloadKind::Folder)
            .map(|p| (DownloadKind::Folder, p))
            .map_err(|e| anyhow!("{src}: not a readable file ({file_err:#}) or folder ({e:#})")),
    }
}

fn basename(p: &str) -> String {
    p.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(p)
        .to_string()
}

/// `--elf`, then `PS5UPLOAD_ELF`, then the repo's built payload (A4). The lab normally
/// runs from `engine/`, so the parent directory is searched too.
fn resolve_elf(flag: Option<&Path>) -> anyhow::Result<PathBuf> {
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Some(f) = flag {
        tried.push(f.to_path_buf());
    } else {
        if let Some(e) = std::env::var_os("PS5UPLOAD_ELF").filter(|v| !v.is_empty()) {
            tried.push(e.into());
        }
        tried.push("payload/ps5upload.elf".into());
        tried.push("../payload/ps5upload.elf".into());
    }
    for t in &tried {
        if t.is_file() {
            return Ok(t.clone());
        }
        if flag.is_some() {
            break;
        }
    }
    bail!(
        "no payload ELF for `resume`; tried: {} (pass --elf FILE)",
        tried
            .iter()
            .map(|t| t.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Reads the ELF, stamps a copy with the pool identity and a fresh launch token (so the
/// restarted console trusts us without a pairing code), and describes it (A4).
fn load_elf(path: &Path) -> anyhow::Result<ElfInfo> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let meta = std::fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let hash12 = ava1::hex::encode(&blake3::hash(&raw).as_bytes()[..6]);
    let key = crate::ava1_cmds::identity()?.public();
    let tokens = ava1::launch::LaunchTokens::at(&crate::ava1_cmds::ava_dir().join("launch_tokens"));
    let mut stamped = raw;
    ava1::trust::stamp_helper(&mut stamped, Some(&key), || tokens.issue().ok())
        .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    Ok(ElfInfo {
        path: path.to_path_buf(),
        size: meta.len(),
        mtime,
        hash12,
        stamped: Arc::new(stamped),
    })
}

/// The progress counters the runs read. Excludes stay empty, like the engine's default.
struct Counters4 {
    bytes: Arc<AtomicU64>,
    bytes_finalized: Arc<AtomicU64>,
}

fn make_cfg(args: &BenchArgs) -> (TransferConfig, Counters4) {
    let mut cfg = TransferConfig::new(args.host.clone());
    let c = Counters4 {
        bytes: Arc::new(AtomicU64::new(0)),
        bytes_finalized: Arc::new(AtomicU64::new(0)),
    };
    cfg.progress_bytes = Some(c.bytes.clone());
    cfg.progress_files = Some(Arc::new(AtomicU64::new(0)));
    cfg.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    cfg.progress_bytes_finalized = Some(c.bytes_finalized.clone());
    cfg.excludes = Vec::new();
    (cfg, c)
}

fn new_id() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

fn new_op_id() -> u64 {
    (u64::from_le_bytes(new_id()[..8].try_into().unwrap()) >> 1).max(1)
}

/// The upload itself, on a blocking thread, through the adapter the engine uses.
fn spawn_upload(
    pool: PoolRef,
    args: &BenchArgs,
    prep: &Prep,
    cfg: TransferConfig,
    id: [u8; 16],
) -> tokio::task::JoinHandle<(f64, anyhow::Result<TransferResult>)> {
    let (dest, src) = (
        args.dest.clone().expect("validated"),
        PathBuf::from(&args.src),
    );
    let is_dir = prep.src_is_dir;
    tokio::task::spawn_blocking(move || {
        let t = Instant::now();
        let r = if is_dir {
            ps5upload_ava1::upload::upload_dir_in(pool.get(), &cfg, id, &dest, &src)
        } else {
            ps5upload_ava1::upload::upload_file_in(pool.get(), &cfg, id, &dest, &src)
        };
        (t.elapsed().as_secs_f64(), r)
    })
}

fn measured_from_transfer(tr: &TransferResult, seconds: f64) -> Measured {
    let mut m = Measured::new(
        seconds,
        observe_upload(&tr.tx_id_hex, &tr.commit_ack_body),
        "commit_ack",
    );
    if let Ok(b) = serde_json::from_str::<Value>(&tr.commit_ack_body) {
        m.resent = b["resent"].as_u64().unwrap_or(0);
        m.max_lanes = b["max_lanes"].as_u64().unwrap_or(0);
        m.bottleneck = b["bottleneck"].as_str().unwrap_or("n/a").to_string();
        m.sequential = b["sequential"].as_bool().unwrap_or(false);
    }
    m.extra.insert("bytes_sent".into(), tr.bytes_sent.into());
    m
}

async fn run_upload(env: &Env, args: &BenchArgs, prep: &Prep, cancel: &Arc<AtomicBool>) -> Attempt {
    let (mut cfg, _c) = make_cfg(args);
    cfg.cancel = Some(cancel.clone());
    let (secs, r) = spawn_upload(env.pool.clone(), args, prep, cfg, new_id())
        .await
        .map_err(|e| (0.0, format!("upload task: {e}")))?;
    match r {
        Ok(tr) => Ok(measured_from_transfer(&tr, secs)),
        Err(e) => Err((secs, format!("{e:#}"))),
    }
}

/// drop60 (C12): the upload goes through the killing proxy; `drops` is how many times it
/// really killed live connections during this run.
async fn run_drop60(
    env: &Env,
    args: &BenchArgs,
    prep: &Prep,
    cancel: &Arc<AtomicBool>,
    proxy: &ava1_chaos::ChaosProxy,
) -> Attempt {
    let before = proxy.kills();
    let mut m = run_upload(env, args, prep, cancel).await?;
    let drops = proxy.kills() - before;
    m.extra.insert("drops".into(), drops.into());
    if drops == 0 {
        m.extra.insert("not_exercised".into(), true.into());
        m.verdict = Some(NOT_EXERCISED.into());
    } else {
        m.verdict = drop60_verdict(m.resent, drops).err();
    }
    Ok(m)
}

/// resume: stop the payload at 50 % durable, re-send the stamped ELF (C16), and let the
/// adapter's reconnect loop carry on. The trigger reads the durable byte counter.
async fn run_resume(env: &Env, args: &BenchArgs, prep: &Prep, cancel: &Arc<AtomicBool>) -> Attempt {
    let elf = prep.elf.as_ref().expect("resume loads its ELF first");
    let (mut cfg, c) = make_cfg(args);
    cfg.cancel = Some(cancel.clone());
    let counter = c.bytes_finalized.clone();
    let size = prep.bytes;
    let target = (size / 2).max(1);
    let handle = spawn_upload(env.pool.clone(), args, prep, cfg, new_id());
    let mut fired: Option<(u64, f64)> = None;
    let mut fault_error: Option<String> = None;
    while !handle.is_finished() {
        let at = counter.load(Ordering::Relaxed);
        if at >= target {
            let t0 = Instant::now();
            let host = args.host.clone();
            let _ = blocking(move || {
                ps5upload_core::payload_lifecycle::shutdown_running_payload(&host)
            })
            .await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            let (host, bytes) = (args.host.clone(), elf.stamped.clone());
            let sent = blocking(move || {
                ps5upload_core::payload_lifecycle::send_elf_to_loader(
                    &host,
                    9021,
                    &bytes,
                    ps5upload_core::payload_lifecycle::LoaderImage::Ps5Upload,
                )
            })
            .await;
            if let Err(e) = sent {
                cancel.store(true, Ordering::Relaxed);
                fault_error = Some(format!("re-sending the payload failed: {e}"));
            }
            // The adapter reconnects through its own session logic (it waits for :9120).
            fired = Some((at, t0.elapsed().as_secs_f64()));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (secs, r) = handle
        .await
        .map_err(|e| (0.0, format!("upload task: {e}")))?;
    let tr = match r {
        Ok(tr) => tr,
        Err(e) => {
            let why = fault_error.map_or(String::new(), |f| format!("{f}; "));
            return Err((secs, format!("{why}{e:#}")));
        }
    };
    let mut m = measured_from_transfer(&tr, secs);
    let sent = tr.bytes_sent;
    m.extra.insert("sent_bytes".into(), sent.into());
    m.extra.insert("payload_size".into(), size.into());
    match fired {
        None => {
            m.verdict = Some(
                "the transfer finished before reaching 50 %: the fault never fired \
                 (use a larger --src)"
                    .into(),
            )
        }
        Some((at, restart_s)) => {
            m.extra.insert("fault_at_bytes".into(), at.into());
            m.extra.insert("restart_s".into(), restart_s.into());
            // The pass rule: no durable byte went twice (one credit window of slack).
            if sent > size + (64 << 20) {
                m.verdict = Some(format!(
                    "sent {sent} bytes for {size}: more than one 64 MiB window was re-sent"
                ));
            }
        }
    }
    Ok(m)
}

async fn session_evidence(env: &Env, console: &str) -> String {
    match env.pool.get().session(console).await {
        Ok(s) if s.peer_caps() & ava1::gen::CAP_DATA_PLANE != 0 => "ava1".into(),
        Ok(s) => format!("ava1-session-without-data-plane(caps={:#x})", s.peer_caps()),
        Err(e) => format!("no-ava1-session({e})"),
    }
}

async fn run_download(
    env: &Env,
    args: &BenchArgs,
    prep: &Prep,
    dir: &Path,
    cancel: &Arc<AtomicBool>,
) -> Attempt {
    let kind = prep.kind.expect("download resolves its kind first");
    let (dest, src) = (dir.to_path_buf(), args.src.clone());
    let (pool, console) = (env.pool.clone(), args.host.clone());
    let cancel = cancel.clone();
    let (secs, r) = blocking(move || {
        let t = Instant::now();
        let r = ps5upload_ava1::download::to_local_in(
            pool.get(),
            &console,
            &src,
            kind,
            &dest,
            false,
            new_id(),
            &ps5upload_ava1::download::Counters::default(),
            Some(cancel),
        );
        (t.elapsed().as_secs_f64(), r)
    })
    .await;
    match r {
        Ok(n) => {
            let observed = session_evidence(env, &args.host).await;
            let mut m = Measured::new(secs, observed, "session+call-path");
            m.extra.insert("bytes_reported".into(), n.into());
            if n != prep.bytes {
                m.verdict = Some(format!(
                    "the download reported {n} bytes, the source holds {}",
                    prep.bytes
                ));
            }
            Ok(m)
        }
        Err(e) => Err((secs, format!("{e:#}"))),
    }
}

async fn run_copy(env: &Env, args: &BenchArgs) -> Attempt {
    let (src, dest, op) = (args.src.clone(), args.dest.clone().unwrap(), new_op_id());
    let (pool, console) = (env.pool.clone(), args.host.clone());
    let (secs, r) = blocking(move || {
        let t = Instant::now();
        // Overwrite off: the destination was cleaned, so it never decides the run.
        let r = ps5upload_ava1::copy::console_copy_in(
            pool.get(),
            &console,
            &src,
            &dest,
            op,
            false,
            false,
        );
        (t.elapsed().as_secs_f64(), r)
    })
    .await;
    match r {
        Ok(()) => {
            let observed = session_evidence(env, &args.host).await;
            let mut m = Measured::new(secs, observed, "session+call-path");
            m.extra.insert("overwrite".into(), false.into());
            Ok(m)
        }
        Err(e) => Err((secs, format!("{e:#}"))),
    }
}

/// relay: AVA1 streams console to console.
async fn run_relay(env: &Env, args: &BenchArgs, cancel: &Arc<AtomicBool>) -> Attempt {
    let to = host_of(args.to.as_deref().expect("validated"));
    let dest = args.dest.clone().unwrap();
    let src = args.src.clone();
    let (pool, from) = (env.pool.clone(), args.host.clone());
    let progress = Arc::new(ava1::send::Progress::default());
    let cancel = cancel.clone();
    let to_session = to.clone();
    let (secs, r) = blocking(move || {
        let t = Instant::now();
        let r = ps5upload_ava1::relay::ps5_to_ps5_between(
            pool.get(),
            &from,
            &src,
            pool.get(),
            &to_session,
            &dest,
            new_id(),
            progress,
            cancel,
        );
        (t.elapsed().as_secs_f64(), r)
    })
    .await;
    match r {
        Ok(rep) => {
            let mut observed = session_evidence(env, &args.host).await;
            if observed == "ava1" {
                observed = session_evidence(env, &to).await;
            }
            let mut m = Measured::new(secs, observed, "session+call-path");
            m.resent = rep.resent;
            m.max_lanes = u64::from(rep.max_lanes);
            m.bottleneck = ps5upload_ava1::progress::bottleneck_name(rep.bottleneck).into();
            m.sequential = rep.sequential;
            Ok(m)
        }
        Err(e) => Err((secs, format!("{e:#}"))),
    }
}

/// Deletes the destination between runs (C8). Missing is fine; anything else fails the
/// run rather than measuring over leftovers.
fn clean_console(host: &str, path: &str) -> Result<(), String> {
    // A helper restarted by the previous run (resume) may still be coming up.
    wait_for_ports(
        &[crate::ava1_cmds::ava1_addr(host)],
        PORTS_DEADLINE,
        Duration::from_millis(500),
    )
    .map_err(|e| format!("cleaning {path}: AVA1 port: {e}"))?;
    match ps5upload_core::cleanup::cleanup_path(host, path) {
        Ok(_) => Ok(()),
        Err(e) => {
            let m = format!("{e:#}");
            let l = m.to_ascii_lowercase();
            if ["not found", "no such", "enoent", "does not exist"]
                .iter()
                .any(|k| l.contains(k))
            {
                Ok(())
            } else {
                Err(format!("cleaning {path} failed: {m}"))
            }
        }
    }
}

async fn clean_between(env: &Env, args: &BenchArgs, local: &Path) -> Result<(), String> {
    if !env.clean {
        return Ok(());
    }
    match args.scenario {
        Scenario::Download => {
            let target = local.join(basename(&args.src));
            let r = if target.is_dir() {
                std::fs::remove_dir_all(&target)
            } else if target.exists() {
                std::fs::remove_file(&target)
            } else {
                Ok(())
            };
            r.map_err(|e| format!("cleaning {}: {e}", target.display()))
        }
        Scenario::Relay => {
            let (host, dest) = (
                host_of(args.to.as_deref().unwrap_or("")),
                args.dest.clone().unwrap(),
            );
            blocking(move || clean_console(&host, &dest)).await
        }
        _ => {
            let (host, dest) = (args.host.clone(), args.dest.clone().unwrap());
            blocking(move || clean_console(&host, &dest)).await
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An unattended AVA1 session before any run: a console that wants a pairing code is an
/// error here, never a prompt.
async fn ava1_preflight(env: &Env, console: &str) -> anyhow::Result<u64> {
    let pool = env.pool.get();
    pool.forget(console).await;
    let t = Instant::now();
    let s = pool.session(console).await.map_err(|e| match e {
        ava1::Ava1Error::NotPaired => anyhow!(
            "{console}: AVA1 needs a pairing code on the console, and bench never prompts. \
             Send a payload stamped with this machine's key (`ps5upload-lab ava1-stamp`, then \
             `send-elf`) so the console trusts it without a code"
        ),
        other => anyhow!("{console}: no AVA1 session: {other}"),
    })?;
    if s.peer_caps() & ava1::gen::CAP_DATA_PLANE == 0 {
        bail!("{console}: the console's helper does not advertise the AVA1 data plane");
    }
    Ok(t.elapsed().as_millis() as u64)
}

/// Gathers everything the runs need before the first (untimed) call.
async fn prepare(env: &Env, args: &BenchArgs) -> anyhow::Result<Prep> {
    let mut prep = Prep {
        files: 0,
        bytes: 0,
        src_is_dir: false,
        kind: None,
        corpus: basename(&args.src),
        drive: String::new(),
        ceiling: None,
        elf: None,
        setup_ms: None,
    };
    let (ceil_host, drive) = match args.scenario {
        Scenario::Download => (args.host.clone(), drive_of(&args.src)),
        Scenario::Relay => (
            host_of(args.to.as_deref().unwrap_or("")),
            drive_of(args.dest.as_deref().unwrap_or("/")),
        ),
        _ => (
            args.host.clone(),
            drive_of(args.dest.as_deref().unwrap_or("/")),
        ),
    };
    prep.drive = drive.clone();
    prep.ceiling = calibrated_ceiling(&env.out, &ceil_host, &drive);
    match args.scenario {
        Scenario::UploadFile | Scenario::UploadDir | Scenario::Drop60 | Scenario::Resume => {
            let src = PathBuf::from(&args.src);
            let meta = std::fs::metadata(&src)
                .with_context(|| format!("--src {} is not readable", src.display()))?;
            prep.src_is_dir = meta.is_dir();
            match (args.scenario, prep.src_is_dir) {
                (Scenario::UploadFile, true) => bail!("upload-file needs a file, not a directory"),
                (Scenario::UploadDir, false) => bail!("upload-dir needs a directory, not a file"),
                _ => {}
            }
            let s2 = src.clone();
            let set = if prep.src_is_dir {
                blocking(move || walk_local(&s2)).await?
            } else {
                vec![(basename(&args.src), src, meta.len())]
            };
            prep.files = set.len() as u64;
            prep.bytes = set.iter().map(|x| x.2).sum();
            if prep.files == 0 {
                bail!("--src holds no files");
            }
        }
        Scenario::Download | Scenario::Copy | Scenario::Relay => {
            let (host, src) = (args.host.clone(), args.src.clone());
            let (kind, plan) = blocking(move || console_plan(&host, &src)).await?;
            prep.kind = Some(kind);
            prep.files = plan.sizes.len() as u64;
            prep.bytes = plan.sizes.iter().sum();
            if prep.files == 0 {
                bail!("--src holds no files");
            }
        }
    }
    if args.scenario == Scenario::Resume {
        let path = resolve_elf(args.elf.as_deref())?;
        let p2 = path.clone();
        let elf = blocking(move || load_elf(&p2)).await?;
        println!(
            "payload: {} ({} bytes, mtime {}, blake3 {}…) — stamped copy is sent, the file is untouched",
            elf.path.display(),
            elf.size,
            elf.mtime,
            elf.hash12
        );
        prep.elf = Some(elf);
    }
    prep.setup_ms = Some(ava1_preflight(env, &args.host).await?);
    if args.scenario == Scenario::Relay {
        ava1_preflight(env, &host_of(args.to.as_deref().unwrap())).await?;
    }
    Ok(prep)
}

pub async fn run_bench(args: &BenchArgs) -> anyhow::Result<Vec<BenchRun>> {
    let env = Env::production(args);
    run_bench_in(&env, args).await
}

/// The scenario loop. A failed run is data too: it is recorded and the loop goes on.
pub async fn run_bench_in(env: &Env, args: &BenchArgs) -> anyhow::Result<Vec<BenchRun>> {
    if env.clean {
        if let Some(d) = args
            .dest
            .as_deref()
            .filter(|_| args.scenario != Scenario::Download)
        {
            check_console_dest(d)?;
        }
    }
    if env.clean && args.scenario == Scenario::Download {
        let dir = args.dest.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("bench-download-{}", std::process::id()))
                .to_string_lossy()
                .into_owned()
        });
        check_local_dest(Path::new(&dir), &args.src)?;
    }
    if args.scenario == Scenario::Relay && !matches!(env.pool, PoolRef::Global) {
        bail!("relay cannot run with AVA1_PORT moved: one address cannot serve two consoles");
    }
    let mut env_owned: Option<Env> = None;
    let mut proxy: Option<ava1_chaos::ChaosProxy> = None;
    if args.scenario == Scenario::Drop60 {
        let upstream = tokio::net::lookup_host(&args.ava1)
            .await
            .with_context(|| format!("resolving {}", args.ava1))?
            .next()
            .ok_or_else(|| anyhow!("{} resolves to nothing", args.ava1))?;
        let src_bytes = {
            let s = PathBuf::from(&args.src);
            blocking(move || match std::fs::metadata(&s) {
                Ok(m) if m.is_dir() => walk_local(&s)
                    .map(|v| v.iter().map(|x| x.2).sum())
                    .unwrap_or(0),
                Ok(m) => m.len(),
                Err(_) => 0,
            })
            .await
        };
        let kill_every_s = derive_kill_interval(args.kill_every_s, src_bytes);
        let p = ava1_chaos::ChaosProxy::start(
            upstream,
            ava1_chaos::ChaosConfig {
                kill_every: Some(Duration::from_secs(kill_every_s)),
                ..Default::default()
            },
        )
        .await?;
        println!(
            "drop60: killing every connection each {kill_every_s} s{} through {} -> {upstream}",
            if args.kill_every_s.is_some() {
                ""
            } else {
                " (derived from the size)"
            },
            p.addr
        );
        env_owned = Some(Env {
            pool: PoolRef::Owned(Arc::new(
                ps5upload_ava1::Pool::new(env.ava_dir.clone()).with_addr(p.addr.to_string()),
            )),
            ava_dir: env.ava_dir.clone(),
            idle: env.idle,
            clean: env.clean,
            out: env.out.clone(),
        });
        proxy = Some(p);
    }
    let env = env_owned.as_ref().unwrap_or(env);

    let prep = prepare(env, args).await?;
    let local_dir = args
        .dest
        .as_deref()
        .filter(|_| args.scenario == Scenario::Download);
    let local_dir = PathBuf::from(local_dir.map(String::from).unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!("bench-download-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }));
    println!(
        "bench {} {} on {} ({} files, {} bytes), {} run(s), results -> {}",
        args.scenario.name(),
        args.proto.as_str(),
        args.host,
        prep.files,
        prep.bytes,
        args.runs,
        env.out.display()
    );

    let mut rows: Vec<Row> = Vec::new();
    let limit = attempt_timeout(prep.bytes, prep.files);
    let first = if args.warmup { 0 } else { 1 };
    let mut abandoned = false;
    for run_no in first..=args.runs {
        let kind = match (run_no, args.warmup) {
            (0, _) => "warmup",
            (1, false) => "cold",
            _ => "warm",
        };
        let cleaned = clean_between(env, args, &local_dir).await;
        if env.clean {
            tokio::time::sleep(env.idle).await;
        }
        let started = unix_now();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut timed_out = false;
        let attempt: Attempt = match cleaned {
            Err(e) => Err((0.0, e)),
            Ok(()) => {
                let work = async {
                    match args.scenario {
                        Scenario::UploadFile | Scenario::UploadDir => {
                            run_upload(env, args, &prep, &cancel).await
                        }
                        Scenario::Drop60 => {
                            let p = proxy.as_ref().expect("started above");
                            run_drop60(env, args, &prep, &cancel, p).await
                        }
                        Scenario::Resume => run_resume(env, args, &prep, &cancel).await,
                        Scenario::Download => {
                            run_download(env, args, &prep, &local_dir, &cancel).await
                        }
                        Scenario::Copy => run_copy(env, args).await,
                        Scenario::Relay => run_relay(env, args, &cancel).await,
                    }
                };
                match tokio::time::timeout(limit, work).await {
                    Ok(a) => a,
                    Err(_) => {
                        cancel.store(true, Ordering::Relaxed);
                        timed_out = true;
                        Err((
                            limit.as_secs_f64(),
                            format!(
                                "timed out after {} s (120 s + bytes / 5 MB/s + files / 50 s)",
                                limit.as_secs()
                            ),
                        ))
                    }
                }
            }
        };
        let landed = if attempt.is_ok() {
            landing(env, args, &prep, &local_dir).await
        } else {
            None
        };
        let row = finish(args, &prep, run_no, kind, attempt, landed);
        record(&env.out, &row.json(started, &prep.corpus))?;
        println!(
            "run {run_no}/{} [{kind}]: {} {:.2}s {:.1} MB/s {:.1} files/s{}",
            args.runs,
            if row.run.ok {
                "ok"
            } else if row.extra.contains_key("not_exercised") {
                "NOT EXERCISED"
            } else {
                "FAILED"
            },
            row.run.seconds,
            row.run.json()["mb_s"].as_f64().unwrap_or(0.0),
            row.run.json()["files_s"].as_f64().unwrap_or(0.0),
            row.run
                .error
                .as_deref()
                .map_or(String::new(), |e| format!(" — {e}"))
        );
        rows.push(row);
        if timed_out {
            abandoned = true;
            eprintln!(
                "stopping: the timed-out transfer may still be running on a blocking thread, \
                 and further runs would measure on top of it"
            );
            break;
        }
    }
    // Leave the console tidy; the run records are already written.
    if abandoned {
        eprintln!(
            "final cleanup skipped: the abandoned transfer may still be running and writing to \
             the destination; clean it by hand once it has stopped"
        );
    } else if let Err(e) = clean_between(env, args, &local_dir).await {
        eprintln!("final cleanup: {e}");
    }
    print!("{}", summary(&rows, &env.out));
    Ok(rows.into_iter().map(|r| r.run).collect())
}

/// Per-attempt deadline: 120 s + bytes at 5 MB/s + files at 50/s (the brief's floor and
/// rate, plus a files term so a 200 000-file tree is not killed at its create ceiling).
pub fn attempt_timeout(bytes: u64, files: u64) -> Duration {
    Duration::from_secs(120 + bytes / 5_000_000 + files / 50)
}

/// What actually landed: the console destination listed over management, or the local
/// tree for download. `None` when there is no console to ask (tests).
async fn landing(
    env: &Env,
    args: &BenchArgs,
    prep: &Prep,
    local_dir: &Path,
) -> Option<Result<(u64, u64), String>> {
    if !env.clean {
        return None;
    }
    if args.scenario == Scenario::Download {
        let target = local_dir.join(basename(&args.src));
        return Some(
            blocking(move || {
                let meta = std::fs::symlink_metadata(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
                if meta.is_file() {
                    return Ok((1, meta.len()));
                }
                let set = walk_local(&target).map_err(|e| format!("{}: {e}", target.display()))?;
                Ok((set.len() as u64, set.iter().map(|x| x.2).sum()))
            })
            .await,
        );
    }
    let (host, dest) = match args.scenario {
        Scenario::Relay => (
            host_of(args.to.as_deref().unwrap_or("")),
            args.dest.clone().unwrap_or_default(),
        ),
        _ => (args.host.clone(), args.dest.clone().unwrap_or_default()),
    };
    let kind = match args.scenario {
        Scenario::Copy | Scenario::Relay => prep.kind.unwrap_or(DownloadKind::Folder),
        _ if prep.src_is_dir => DownloadKind::Folder,
        _ => DownloadKind::File,
    };
    Some(
        blocking(move || {
            enumerate_console(&host, &dest, kind)
                .map(|p| (p.sizes.len() as u64, p.sizes.iter().sum()))
                .map_err(|e| format!("listing {dest}: {e:#}"))
        })
        .await,
    )
}

fn finish(
    args: &BenchArgs,
    prep: &Prep,
    run_no: u32,
    run_kind: &str,
    attempt: Attempt,
    landed: Option<Result<(u64, u64), String>>,
) -> Row {
    let base = |ok: bool, seconds: f64, error: Option<String>| BenchRun {
        console: args.host.clone(),
        scenario: args.scenario.name().into(),
        proto: args.proto.as_str().into(),
        run: run_no,
        files: if ok { prep.files } else { 0 },
        bytes: if ok { prep.bytes } else { 0 },
        seconds,
        resent: 0,
        max_lanes: 0,
        bottleneck: "n/a".into(),
        sequential: false,
        ok,
        error,
    };
    let mut extra = Map::new();
    extra.insert("config".into(), "engine-default".into());
    extra.insert("bytes_planned".into(), prep.bytes.into());
    extra.insert("files_planned".into(), prep.files.into());
    if let Some((c, dir)) = &prep.ceiling {
        extra.insert("ceiling_dir".into(), dir.clone().into());
        extra.insert("ceiling_files_s_calibrated".into(), (*c).into());
    }
    if let Some(ms) = prep.setup_ms.filter(|_| run_kind != "warm") {
        extra.insert("session_setup_ms".into(), ms.into());
    }
    if let Some(e) = &prep.elf {
        extra.insert("elf_path".into(), e.path.display().to_string().into());
        extra.insert("elf_size".into(), e.size.into());
        extra.insert("elf_mtime".into(), e.mtime.into());
        extra.insert("elf_blake3_12".into(), e.hash12.clone().into());
    }
    let ceiling = prep.ceiling.as_ref().map(|c| c.0);
    match attempt {
        Err((seconds, why)) => {
            extra.insert("observed".into(), Value::Null);
            Row {
                run: base(false, seconds, Some(why)),
                drive: prep.drive.clone(),
                verified: false,
                run_kind: run_kind.into(),
                ceiling,
                extra,
            }
        }
        Ok(m) => {
            let verification = verify_protocol(args.proto, &m.observed);
            // The landing check: files and bytes on the destination must equal the plan.
            let landing_error = match &landed {
                Some(Ok((f, b))) => {
                    extra.insert("landed_files".into(), (*f).into());
                    extra.insert("landed_bytes".into(), (*b).into());
                    (*f != prep.files || *b != prep.bytes).then(|| {
                        format!(
                            "landed {f} files / {b} bytes, expected {} / {}",
                            prep.files, prep.bytes
                        )
                    })
                }
                Some(Err(e)) => Some(format!("could not verify what landed: {e}")),
                None => {
                    extra.insert("landed_checked".into(), false.into());
                    None
                }
            };
            extra.insert("durable".into(), true.into());
            let error = verification
                .clone()
                .err()
                .or_else(|| m.verdict.clone())
                .or(landing_error);
            let mut run = base(error.is_none(), m.seconds, error);
            run.resent = m.resent;
            run.max_lanes = m.max_lanes;
            run.bottleneck = m.bottleneck.clone();
            run.sequential = m.sequential;
            extra.extend(m.extra);
            extra.insert("observed".into(), m.observed.into());
            extra.insert("verified_by".into(), m.verified_by.into());
            Row {
                run,
                drive: prep.drive.clone(),
                verified: verification.is_ok(),
                run_kind: run_kind.into(),
                ceiling,
                extra,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C12: every test builds its corpus under the system temp dir with a
    /// process-specific component — not the repo, not `$HOME` — and removes it on
    /// drop. Tests never call `corpus_large` or `corpus_ppsa01342(…, 223_000)`.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> TempDir {
            let d =
                std::env::temp_dir().join(format!("ps5upload-lab-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_tiny_corpus_has_the_requested_shape() {
        let d = TempDir::new("bench-tiny");
        corpus_tiny(d.path(), 500).unwrap();
        let st = stats(d.path()).unwrap();
        assert_eq!(st.files, 500);
        assert!(
            st.bytes >= 500 * 1024 && st.bytes <= 500 * 64 * 1024,
            "bytes {}",
            st.bytes
        );
        assert!(
            st.duplicate_ratio > 0.0 && st.duplicate_ratio < 0.1,
            "duplicate_ratio {}",
            st.duplicate_ratio
        );
    }

    #[test]
    fn a_listing_is_reproduced_exactly() {
        let d = TempDir::new("bench-list");
        let bad = |text: &str, dir: &Path| {
            let l = d.path().join("listing.txt");
            std::fs::write(&l, text).unwrap();
            let out = dir.join("out");
            corpus_listing(&out, &l, 1.0)
        };
        // C7: an absolute path, `..`, a trailing `/` and a duplicate path are all
        // rejected; a blank line and a `#` comment are skipped.
        assert!(bad("5 /etc/passwd\n", &d.path().join("b1")).is_err());
        assert!(bad("5 ../escape\n", &d.path().join("b2")).is_err());
        assert!(bad("5 a/dir/\n", &d.path().join("b3")).is_err());
        assert!(bad("5 a/once\n5 a/once\n", &d.path().join("b4")).is_err());
        assert!(bad("5 a/\n", &d.path().join("b5")).is_err());

        let out = d.path().join("out");
        let l = d.path().join("listing.txt");
        std::fs::write(
            &l,
            "10 a/b/c.bin\n0 a/empty\n70000 sce_sys/param.json\n\n# a comment\n",
        )
        .unwrap();
        corpus_listing(&out, &l, 1.0).unwrap();
        assert_eq!(std::fs::metadata(out.join("a/b/c.bin")).unwrap().len(), 10);
        assert_eq!(std::fs::metadata(out.join("a/empty")).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(out.join("sce_sys/param.json"))
                .unwrap()
                .len(),
            70000
        );
        // A1: the marker makes the duplicate ratio undefined, never a false 0, and
        // the marker itself is not counted as a corpus file.
        let st = stats(&out).unwrap();
        assert_eq!(st.files, 3);
        assert!(st.duplicate_ratio.is_nan(), "{:?}", st.duplicate_ratio);
    }

    #[test]
    fn compressible_fraction_tells_random_from_repetitive() {
        let d = TempDir::new("bench-cmp");
        std::fs::write(d.path().join("z"), vec![0u8; 1 << 20]).unwrap();
        write_random(&d.path().join("r"), 1 << 20, 1).unwrap();
        let st = stats(d.path()).unwrap();
        assert!(
            (st.compressible_fraction - 0.5).abs() < 0.01,
            "{}",
            st.compressible_fraction
        );
        // C10's explicit empty rule: a zero-length file contributes nothing, and an
        // all-empty corpus has fraction 0.0 (the plan's `0 <= 0` was accidentally
        // true).
        let e = TempDir::new("bench-cmp-empty");
        std::fs::File::create(e.path().join("a")).unwrap();
        std::fs::File::create(e.path().join("b")).unwrap();
        let st = stats(e.path()).unwrap();
        assert_eq!(st.compressible_fraction, 0.0);
    }

    #[test]
    fn stats_counts_duplicates_by_content() {
        let d = TempDir::new("bench-dup");
        // Two byte-identical files and one that shares the pair's first 64 bytes but
        // differs after — a prefix-based digest would miscount it (C3: the digest
        // covers the whole file).
        let mut a = vec![0x5au8; 1000];
        a[..64].copy_from_slice(&[0x11u8; 64]);
        let mut b = a.clone();
        b[900] ^= 0xff;
        std::fs::write(d.path().join("a1"), &a).unwrap();
        std::fs::write(d.path().join("a2"), &a).unwrap();
        std::fs::write(d.path().join("b"), &b).unwrap();
        let st = stats(d.path()).unwrap();
        assert_eq!(st.files, 3);
        assert_eq!(st.duplicate_ratio, 1.0 / 3.0);
    }

    #[test]
    fn the_bucket_table_and_the_dedup_class_agree() {
        let d = TempDir::new("bench-buckets");
        // The five pinned boundary sizes (C4), plus a 65536-byte twin: if the dedup
        // class wrongly used `<= SMALL`, the twin pair would lift the ratio off 0.
        let twin = vec![0x42u8; SMALL as usize];
        let mut i = 0u64;
        let mut put = |len: u64| {
            let p = d.path().join(format!("f{i}.bin"));
            i += 1;
            write_random(&p, len, i).unwrap(); // distinct content per size
            if len == SMALL {
                std::fs::write(&p, &twin).unwrap(); // the twin pair shares bytes
            }
        };
        put(4096);
        put(4097);
        put(65535);
        put(65536);
        put(65536); // the twin
        put(65537);
        let st = stats(d.path()).unwrap();
        // C4: 4096 → bucket 0; 4097 and 65535 → bucket 1; 65536 (both) and 65537 →
        // bucket 2. And the dedup class (`len < SMALL`) contains only the first
        // three, all distinct → ratio exactly 0.
        assert_eq!(st.histogram, [1, 2, 3, 0, 0]);
        assert_eq!(st.duplicate_ratio, 0.0);
    }

    #[test]
    fn the_ppsa_shape_matches_its_histogram() {
        // C5, coordinator ruling 2026-10-02: scale 1.0 so the pinned proportions are
        // real, exercising the actual writer path; 700 files keeps the corpus at
        // ≈ 98.6 MiB expected (70 % × 2 KiB + 20 % × 34 KiB + 9 % × 544 KiB + 1 % ×
        // 8.5 MiB per file) — at or under ~100 MiB, cheap for CI. At 700 files the
        // ≤ 4 KiB share has σ ≈ 1.7 points and the > 1 MiB share σ ≈ 0.4 points, so
        // the ± 10-point tolerances are far from the flake edge.
        // The seed is fixed (Rng(1342)), so the corpus is identical every run:
        // measured 92.3 MiB, histogram [491, 139, 61, 9, 0] → shares 70.14 % /
        // 1.29 % (2026-10-02), both well inside the pinned tolerances.
        let d = TempDir::new("bench-ppsa");
        corpus_ppsa01342(d.path(), 1.0, 700).unwrap();
        let st = stats(d.path()).unwrap();
        let small_share = st.histogram[0] as f64 / st.files as f64;
        let large_share = (st.histogram[3] + st.histogram[4]) as f64 / st.files as f64;
        assert!(
            (small_share - 0.70).abs() < 0.10,
            "≤4 KiB share {small_share}, histogram {:?}",
            st.histogram
        );
        assert!(
            (large_share - 0.01).abs() < 0.10,
            ">1 MiB share {large_share}, histogram {:?}",
            st.histogram
        );
        // Directory cadence and depth (C5): ≤ 40 files per directory, at most 8 levels.
        let mut stack = vec![d.path().to_path_buf()];
        let mut max_depth = 0usize;
        while let Some(dir) = stack.pop() {
            max_depth = max_depth.max(
                dir.strip_prefix(d.path())
                    .map(|r| r.components().count())
                    .unwrap_or(0),
            );
            let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
            let files = entries
                .iter()
                .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
                .count();
            assert!(
                files <= PER_DIR as usize,
                "{} files in {}",
                files,
                dir.display()
            );
            for e in entries {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        assert!(max_depth <= 8, "depth {max_depth}");
    }

    #[test]
    fn a_duplicate_in_the_tiny_corpus_is_byte_identical() {
        let d = TempDir::new("bench-tiny-dup");
        corpus_tiny(d.path(), 1000).unwrap();
        let st = stats(d.path()).unwrap();
        assert!(st.duplicate_ratio > 0.0, "the corpus must hold duplicates");
        // Verify the property the ratio claims: some reported-duplicate pair is
        // byte-identical on disk. Group the dedup class by whole-file digest (the
        // same rule stats() uses) and compare a colliding pair.
        let mut by_digest: std::collections::HashMap<[u8; 32], Vec<PathBuf>> =
            std::collections::HashMap::new();
        for e in walk_files(d.path()) {
            let len = std::fs::metadata(&e).unwrap().len();
            if len < SMALL {
                by_digest
                    .entry(*blake3::hash(&std::fs::read(&e).unwrap()).as_bytes())
                    .or_default()
                    .push(e);
            }
        }
        let (_, pair) = by_digest
            .iter()
            .find(|(_, v)| v.len() > 1)
            .expect("stats reported duplicates, so a pair exists");
        assert_eq!(
            std::fs::read(&pair[0]).unwrap(),
            std::fs::read(&pair[1]).unwrap()
        );
    }

    fn walk_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(e.path());
                }
            }
        }
        out
    }

    #[test]
    fn listing_scale_multiplies_sizes() {
        let d = TempDir::new("bench-scale");
        let l = d.path().join("listing.txt");
        std::fs::write(&l, "100 f.bin\n").unwrap();
        let half = d.path().join("half");
        corpus_listing(&half, &l, 0.5).unwrap();
        assert_eq!(std::fs::metadata(half.join("f.bin")).unwrap().len(), 50);
        let double = d.path().join("double");
        corpus_listing(&double, &l, 2.0).unwrap();
        assert_eq!(std::fs::metadata(double.join("f.bin")).unwrap().len(), 200);
    }

    #[test]
    fn record_appends_one_line_per_call() {
        let d = TempDir::new("bench-record");
        // The path is the caller's (C15): recording into a fresh path creates it, and
        // repeated calls append one JSON line each.
        let out = d.path().join("nested").join("results.jsonl");
        record(
            &out,
            &serde_json::json!({"schema": SCHEMA, "kind": "bench"}),
        )
        .unwrap();
        record(
            &out,
            &serde_json::json!({"schema": SCHEMA, "kind": "bench", "run": 2}),
        )
        .unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["schema"], SCHEMA);
        }
        assert_eq!(
            lines[1].trim_end(),
            r#"{"kind":"bench","run":2,"schema":1}"#
        );
    }

    #[test]
    fn record_envelope_freezes_the_schema() {
        let v = record_envelope("bench", Some("tiny"), Some(11), "ava1");
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["kind"], "bench");
        assert_eq!(v["corpus"], "tiny");
        assert_eq!(v["seed"], 11);
        assert_eq!(v["protocol"], "ava1");
        assert!(v["machine"].as_str().is_some_and(|m| !m.is_empty()));
        assert!(v["started_at"].as_u64().is_some());
        // A1: an undefined ratio serializes as JSON null, never 0.
        let s = Stats {
            files: 1,
            bytes: 1,
            histogram: [0; 5],
            compressible_fraction: 0.0,
            duplicate_ratio: f64::NAN,
        };
        assert!(serde_json::to_value(&s).unwrap()["duplicate_ratio"].is_null());
    }

    #[test]
    fn a_generator_refuses_a_nonempty_target_without_force() {
        let d = TempDir::new("bench-force");
        let target = d.path().join("corpus");
        // A missing directory is created.
        ensure_writable_target(&target, false).unwrap();
        assert!(target.is_dir());
        // Empty is fine; non-empty is refused without --force (A2) …
        ensure_writable_target(&target, false).unwrap();
        std::fs::write(target.join("keep.me"), b"precious").unwrap();
        assert!(ensure_writable_target(&target, false).is_err());
        // … and allowed with it, without deleting anything already there.
        ensure_writable_target(&target, true).unwrap();
        assert_eq!(std::fs::read(target.join("keep.me")).unwrap(), b"precious");
        assert!(target.is_dir());
    }

    #[test]
    fn stats_skips_symlinks() {
        // C8: a real game folder may hold symlinks; the walk must not follow them
        // (or fail) — they are skipped and reported.
        let d = TempDir::new("bench-symlink");
        std::fs::write(d.path().join("real"), vec![7u8; 100]).unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("link")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", d.path().join("dangling")).unwrap();
        let st = stats(d.path()).unwrap();
        assert_eq!(st.files, 1);
        assert_eq!(st.bytes, 100);
    }

    #[test]
    fn write_random_is_deterministic_per_seed() {
        // Benchmarks must be reproducible: the same seed yields the same bytes, a
        // different seed does not (C9's spread key makes that explicit).
        let d = TempDir::new("bench-seed");
        write_random(&d.path().join("a"), 100_000, 42).unwrap();
        write_random(&d.path().join("b"), 100_000, 42).unwrap();
        write_random(&d.path().join("c"), 100_000, 43).unwrap();
        assert_eq!(
            std::fs::read(d.path().join("a")).unwrap(),
            std::fs::read(d.path().join("b")).unwrap()
        );
        assert_ne!(
            std::fs::read(d.path().join("a")).unwrap(),
            std::fs::read(d.path().join("c")).unwrap()
        );
    }

    #[test]
    fn the_sample_takes_head_middle_and_tail() {
        // A4: for files > 1 MiB the compressible-fraction sample is head+middle+tail
        // (3 × 340 KiB), not a 1 MiB prefix. A 4 MiB file that is random for its
        // first MiB and zeros after: the head+middle+tail sample is 340 KiB random
        // (head) + 680 KiB zeros (middle and tail), which deflates to ~34 % of its
        // size, so the whole file counts compressible — a prefix sampler would read
        // only the random MiB and report 0. The scratch file lives in a second temp
        // dir so the walked corpus holds exactly one file.
        let d = TempDir::new("bench-hmt");
        let s = TempDir::new("bench-hmt-scratch");
        write_random(&s.path().join("head"), 1 << 20, 5).unwrap();
        let f = d.path().join("f.bin");
        let mut out = std::fs::File::create(&f).unwrap();
        out.write_all(&std::fs::read(s.path().join("head")).unwrap())
            .unwrap();
        out.write_all(&vec![0u8; 3 << 20]).unwrap();
        let st = stats(d.path()).unwrap();
        assert!(
            st.compressible_fraction > 0.9,
            "{}",
            st.compressible_fraction
        );
    }

    // ─── Task 27 ────────────────────────────────────────────────────────────────

    fn run(seconds: f64) -> BenchRun {
        BenchRun {
            console: "1.2.3.4".into(),
            scenario: "upload-dir".into(),
            proto: "ava1".into(),
            run: 1,
            files: 10,
            bytes: 1 << 20,
            seconds,
            resent: 0,
            max_lanes: 3,
            bottleneck: "network".into(),
            sequential: false,
            ok: true,
            error: None,
        }
    }

    #[test]
    fn a_result_line_has_every_field() {
        let r = BenchRun {
            console: "1.2.3.4".into(),
            scenario: "upload-dir".into(),
            proto: "ava1".into(),
            run: 1,
            files: 10,
            bytes: 1 << 20,
            seconds: 0.5,
            resent: 0,
            max_lanes: 3,
            bottleneck: "network".into(),
            sequential: false,
            ok: true,
            error: None,
        };
        let v = r.json();
        for k in [
            "date",
            "console",
            "scenario",
            "proto",
            "run",
            "files",
            "bytes",
            "seconds",
            "mb_s",
            "files_s",
            "resent",
            "max_lanes",
            "bottleneck",
            "sequential",
            "ok",
        ] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert!((v["mb_s"].as_f64().unwrap() - 2.097152).abs() < 1e-6);
    }

    #[test]
    fn a_zero_second_run_is_finite() {
        let v = run(0.0).json();
        assert!(v["mb_s"].as_f64().unwrap().is_finite());
        assert!(v["files_s"].as_f64().unwrap().is_finite());
        let text = v.to_string();
        assert!(!text.contains("NaN") && !text.contains("inf"), "{text}");
        assert!(v["mb_s"].is_number() && v["files_s"].is_number());
    }

    #[test]
    fn bench_args_parse_the_console_and_derive_three_ports() {
        let a = BenchArgs::parse(&[
            "10.0.0.5",
            "upload-dir",
            "--proto",
            "ava1",
            "--src",
            "/x",
            "--dest",
            "/y",
            "--runs",
            "3",
        ])
        .unwrap();
        assert_eq!(a.host, "10.0.0.5");
        assert!(a.ava1.starts_with("10.0.0.5:"), "{}", a.ava1);
        assert_eq!(a.scenario, Scenario::UploadDir);
        assert_eq!(a.proto, Proto::Ava1);
        assert_eq!(
            (a.runs, a.src.as_str(), a.dest.as_deref()),
            (3, "/x", Some("/y"))
        );
        // A stale `:port` on the console (the retired transfer and management ports) is dropped.
        for typed in ["10.0.0.5:9113", "10.0.0.5:9114"] {
            let b = BenchArgs::parse(&[
                typed, "copy", "--proto", "ava1", "--src", "/a", "--dest", "/b",
            ])
            .unwrap();
            assert_eq!(b.host, "10.0.0.5", "{typed}");
        }
        let b = BenchArgs::parse(&[
            "10.0.0.5", "copy", "--proto", "ava1", "--src", "/a", "--dest", "/b",
        ])
        .unwrap();
        assert_eq!(b.runs, 1, "--runs defaults to 1");
    }

    #[test]
    fn an_unknown_scenario_or_proto_is_an_error_listing_the_valid_values() {
        let base = |sc: &str, proto: &str, runs: &str| {
            BenchArgs::parse(&[
                "h", sc, "--proto", proto, "--src", "/x", "--dest", "/y", "--runs", runs,
            ])
        };
        let e = base("sideways", "ava1", "1").unwrap_err().to_string();
        for s in SCENARIOS {
            assert!(e.contains(s), "{e}");
        }
        let e = base("copy", "ftp", "1").unwrap_err().to_string();
        assert!(e.contains("ava1"), "{e}");
        let e = base("copy", "ftx2", "1").unwrap_err().to_string();
        assert!(e.contains("unknown protocol"), "{e}");
        let e = base("copy", "ava1", "0").unwrap_err().to_string();
        assert!(e.contains("at least 1"), "{e}");
        assert!(base("copy", "ava1", "x").is_err());
        assert!(BenchArgs::parse(&[
            "h", "relay", "--proto", "ava1", "--src", "/x", "--dest", "/y"
        ])
        .is_err());
    }

    #[test]
    fn the_kill_interval_gives_at_least_three_kills_and_honours_the_flag() {
        // 4 GiB at ~100 MB/s is ~42 s: 10 s kills.
        assert_eq!(derive_kill_interval(None, 4 << 30), 10);
        // A huge transfer is capped at the old 60 s default.
        assert_eq!(derive_kill_interval(None, 100 << 30), 60);
        // A tiny one still gets a 1 s interval, never 0.
        assert_eq!(derive_kill_interval(None, 10_000), 1);
        assert_eq!(derive_kill_interval(None, 0), 1);
        // The explicit flag always wins.
        assert_eq!(derive_kill_interval(Some(60), 4 << 30), 60);
        assert_eq!(derive_kill_interval(Some(3), 100 << 30), 3);
        let a = BenchArgs::parse(&[
            "h", "drop60", "--proto", "ava1", "--src", "/x", "--dest", "/y",
        ])
        .unwrap();
        assert_eq!(a.kill_every_s, None);
        let a = BenchArgs::parse(&[
            "h",
            "drop60",
            "--proto",
            "ava1",
            "--src",
            "/x",
            "--dest",
            "/y",
            "--kill-every-s",
            "7",
        ])
        .unwrap();
        assert_eq!(a.kill_every_s, Some(7));
    }

    #[test]
    fn zero_drops_is_not_exercised_not_a_protocol_failure() {
        let e = drop60_verdict(0, 0).unwrap_err();
        assert!(e.starts_with("not exercised"), "{e}");
        assert!(
            e.contains("larger --src") && e.contains("--kill-every-s"),
            "{e}"
        );
    }

    fn free_addr() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    }

    #[test]
    fn wait_for_ports_waits_for_listeners_that_appear_late() {
        let (a, b) = (free_addr(), free_addr());
        let (a2, b2) = (a.clone(), b.clone());
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            let la = std::net::TcpListener::bind(&a2).unwrap();
            std::thread::sleep(Duration::from_millis(400));
            let lb = std::net::TcpListener::bind(&b2).unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop((la, lb));
        });
        let waited =
            wait_for_ports(&[a, b], Duration::from_secs(10), Duration::from_millis(50)).unwrap();
        assert!(waited >= Duration::from_millis(700), "{waited:?}");
        t.join().unwrap();
    }

    #[test]
    fn wait_for_ports_gives_up_at_the_deadline_naming_the_silent_port() {
        let up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let up_addr = up.local_addr().unwrap().to_string();
        let down = free_addr();
        let t = Instant::now();
        let e = wait_for_ports(
            &[up_addr.clone(), down.clone()],
            Duration::from_millis(500),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(e.contains(&down) && !e.contains(&up_addr), "{e}");
    }

    #[test]
    fn drop60_refuses_to_pass_without_drops() {
        assert_eq!(drop60_verdict(0, 0), Err(NOT_EXERCISED.to_string()));
        assert!(drop60_verdict(64 << 20, 1).is_ok());
        assert!(drop60_verdict((64 << 20) + 1, 1).is_err());
        assert!(drop60_verdict(128 << 20, 2).is_ok());
    }

    #[test]
    fn the_median_is_the_middle_or_the_mean_of_the_two_middle() {
        assert_eq!(median(&[1.0]), Some(1.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[]), None);
        assert_eq!(fmt1(median(&[])), "n/a");
    }

    #[test]
    fn the_protocol_evidence_must_match_what_was_asked() {
        assert_eq!(
            observe_upload("ab", r#"{"protocol":"ava1","files":1}"#),
            "ava1"
        );
        assert!(observe_upload("ab", r#"{"ok":true}"#).starts_with("unrecognised"));
        assert!(observe_upload("", r#"{"ok":true}"#).starts_with("unrecognised"));
        assert!(observe_upload("ab", r#"{"protocol":"zzz"}"#).starts_with("unrecognised"));
        assert!(verify_protocol(Proto::Ava1, "ava1").is_ok());
        let e = verify_protocol(Proto::Ava1, "other").unwrap_err();
        assert_eq!(e, "protocol mismatch: asked ava1, saw other");
    }

    #[test]
    fn the_delete_guard_mirrors_the_payloads_cleanup_allowlist() {
        for ok in [
            "/data/ps5upload/tests/bench",
            "/data/ps5upload/tests/bench/tiny",
            "/mnt/usb0/ps5upload/tests/bench-run/x",
            "/mnt/ext1/ps5upload/tests/foo/bench/y",
            "/mnt/usb12/ps5upload/tests/bench-1",
        ] {
            check_console_dest(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "/",
            "/data",
            "/data/bench",
            "/mnt/usb0/bench-run",
            "/mnt",
            "/mnt/usb0",
            "/mnt/usb/ps5upload/tests/bench",
            "/mnt/original_m2/ps5upload/tests/bench",
            "/user/ps5upload/tests/bench",
            "/data/ps5upload",
            "/data/ps5upload/tests",
            "/data/ps5upload/tests/tiny",
            "/data/ps5upload/tests/benchmark",
            "/data/ps5upload/tests/bench/../x",
            "/data/ps5upload/tests/bench/./x",
            "data/ps5upload/tests/bench",
            "/data//ps5upload/tests/bench",
            "/data/ps5upload/tests/bench-src/tiny",
            "/data/ps5upload/tests/bench/bench-src",
            "/data/ps5upload/runtime/bench",
        ] {
            assert!(check_console_dest(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_local_download_dir_must_be_absent_empty_or_bench_named() {
        let d = TempDir::new("bench-localdest");
        let full = d.path().join("plain");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("keep.txt"), b"x").unwrap();
        assert!(check_local_dest(&full, "/data/ps5upload/tests/bench-src/tiny").is_err());
        assert!(check_local_dest(&d.path().join("absent"), "/a/tiny").is_ok());
        let empty = d.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(check_local_dest(&empty, "/a/tiny").is_ok());
        let named = d.path().join("bench-dl");
        std::fs::create_dir_all(&named).unwrap();
        std::fs::write(named.join("old"), b"x").unwrap();
        assert!(check_local_dest(&named, "/a/tiny").is_ok());
        for src in ["/", "", "/a/..", "/a/."] {
            assert!(check_local_dest(&named, src).is_err(), "{src:?}");
        }
        // A bench-named ancestor does not make `..` safe.
        let up = named.join("..");
        assert!(check_local_dest(&up, "/a/tiny").is_err());
        assert!(check_local_dest(Path::new("bench-x/../y"), "/a/tiny").is_err());
        assert!(check_local_dest(Path::new("./bench-x"), "/a/tiny").is_err());
        // A `bench-*` ancestor over a plain directory: the final component decides.
        let inner = named.join("plain");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("keep"), b"x").unwrap();
        assert!(check_local_dest(&inner, "/a/tiny").is_err());
        #[cfg(unix)]
        {
            // A symlink named bench-link that points at a directory with real files.
            let outside = d.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("precious"), b"x").unwrap();
            let link = d.path().join("bench-link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(check_local_dest(&link, "/a/tiny").is_err());
        }
        assert!(check_local_dest(Path::new("/"), "/a/tiny").is_err());
        assert!(check_local_dest(Path::new(""), "/a/tiny").is_err());
    }

    #[test]
    fn copy_refuses_overlapping_paths_either_way() {
        for (a, b) in [
            ("/d/x", "/d/x"),
            ("/d/x", "/d/x/y"),
            ("/d/x/y", "/d/x"),
            ("/d/x/", "/d/x"),
        ] {
            assert!(check_copy_overlap(a, b).is_err(), "{a} {b}");
        }
        assert!(check_copy_overlap("/d/x", "/d/xy").is_ok());
        assert!(check_copy_overlap("/d/x", "/e/x").is_ok());
    }

    #[test]
    fn the_attempt_deadline_is_120s_plus_bytes_and_files() {
        assert_eq!(attempt_timeout(0, 0), Duration::from_secs(120));
        assert_eq!(attempt_timeout(5_000_000_000, 0), Duration::from_secs(1120));
        assert_eq!(attempt_timeout(0, 5000), Duration::from_secs(220));
    }

    /// Every command line the report hands Task 28, for both consoles and all three
    /// drives. The test parses each one (console after `bench`), applies the destination
    /// guard, and — with BENCH_DUMP_LINES=FILE — writes them out so the report is
    /// generated from exactly what is tested.
    fn task28_lines() -> (Vec<String>, Vec<String>) {
        let consoles = ["192.168.86.100", "192.168.86.99"];
        let drives = ["/data", "/mnt/usb0", "/mnt/ext1"];
        let (mut stage, mut bench) = (Vec::new(), Vec::new());
        for c in consoles {
            let other = if c == consoles[0] {
                consoles[1]
            } else {
                consoles[0]
            };
            for d in drives {
                let t = format!("{d}/ps5upload/tests");
                stage.push(format!(
                    "{c} transfer-dir $(tx) {t}/bench-src/tiny /tmp/b/tiny"
                ));
                {
                    let proto = "ava1";
                    let b = |sc: &str, rest: String| {
                        format!("bench {c} {sc} --proto {proto} {rest} --runs 3 --warmup --out $O")
                    };
                    bench.push(b(
                        "upload-file",
                        format!("--src /tmp/b/large/large-4g.bin --dest {t}/bench/large.bin"),
                    ));
                    bench.push(b(
                        "upload-dir",
                        format!("--src /tmp/b/tiny --dest {t}/bench/tiny"),
                    ));
                    bench.push(b(
                        "download",
                        format!("--src {t}/bench-src/tiny --dest /tmp/b/bench-dl"),
                    ));
                    bench.push(b(
                        "copy",
                        format!("--src {t}/bench-src/tiny --dest {t}/bench/tiny-copy"),
                    ));
                    bench.push(b("resume", format!("--src /tmp/b/large/large-4g.bin --dest {t}/bench/resume.bin --elf ../payload/ps5upload.elf")));
                    bench.push(b(
                        "relay",
                        format!("--src {t}/bench-src/tiny --dest {t}/bench/relayed --to {other}"),
                    ));
                }
                if d == "/data" {
                    // The 223 000-file corpus: one drive, one run, no warm-up.
                    {
                        let proto = "ava1";
                        bench.push(format!(
                            "bench {c} upload-dir --proto {proto} --src /tmp/b/ppsa --dest {t}/bench/ppsa --runs 1 --out $O"
                        ));
                    }
                }
                bench.push(format!(
                    "bench {c} drop60 --proto ava1 --src /tmp/b/large/large-4g.bin --dest {t}/bench/drop.bin --runs 1 --kill-every-s 10 --out $O"
                ));
            }
        }
        (stage, bench)
    }

    #[test]
    fn every_command_line_in_the_report_parses_and_passes_the_guard() {
        let (stage, bench) = task28_lines();
        for l in &bench {
            let words: Vec<&str> = l.split_whitespace().collect();
            assert_eq!(words[0], "bench", "{l}");
            let a = BenchArgs::parse(&words[1..]).unwrap_or_else(|e| panic!("{l}: {e}"));
            if a.scenario == Scenario::Download {
                check_local_dest(Path::new(a.dest.as_deref().unwrap()), &a.src).unwrap();
            } else {
                check_console_dest(a.dest.as_deref().unwrap())
                    .unwrap_or_else(|e| panic!("{l}: {e}"));
            }
            if l.contains("/tmp/b/ppsa") {
                assert!(
                    !a.warmup && a.runs == 1 && a.dest.as_deref().unwrap().starts_with("/data/"),
                    "ppsa runs on /data only, once, without warm-up: {l}"
                );
            } else {
                assert_eq!(a.warmup, a.scenario != Scenario::Drop60, "{l}");
            }
        }
        for l in &stage {
            // Staging is a plain AVA1 upload (transfer-dir), never a bench command, with a
            // fresh job id from the report's `tx` helper.
            let w: Vec<&str> = l.split_whitespace().collect();
            assert_eq!(w.len(), 5, "{l}");
            assert!(
                !w[0].contains(':') && w[1] == "transfer-dir" && w[2] == "$(tx)",
                "{l}"
            );
            assert!(w[4].starts_with("/tmp/b/tiny"), "{l}");
            let dest = w[3];
            assert!(
                check_console_dest(dest).is_err(),
                "{dest} must not be bench-deletable"
            );
        }
        if let Ok(f) = std::env::var("BENCH_DUMP_LINES") {
            let mut out = String::new();
            for l in &stage {
                out += &format!("S {l}\n");
            }
            for l in &bench {
                out += &format!("B {l}\n");
            }
            std::fs::write(f, out).unwrap();
        }
    }

    #[test]
    fn warmup_runs_are_flagged_and_medians_use_warm_runs() {
        let k = |kind: &str, secs: f64| {
            let mut r = row("ava1", true, secs, None);
            r.run_kind = kind.into();
            r
        };
        let rows = vec![k("warmup", 100.0), k("warm", 1.0), k("warm", 1.0)];
        let s = summary(&rows, Path::new("r.jsonl"));
        assert!(s.contains("[warm runs]"), "{s}");
        assert!(
            s.contains(" 2/2 "),
            "the warm-up is not counted in the table: {s}"
        );
        let cold = summary(&[k("cold", 2.0)], Path::new("r.jsonl"));
        assert!(cold.contains("[cold runs]"), "{cold}");
        let a = BenchArgs::parse(&[
            "h", "copy", "--proto", "ava1", "--src", "/a", "--dest", "/b", "--warmup",
        ])
        .unwrap();
        assert!(a.warmup);
    }

    #[test]
    fn a_landing_mismatch_fails_the_run_and_is_recorded() {
        let args = BenchArgs::parse(&[
            "h",
            "upload-dir",
            "--proto",
            "ava1",
            "--src",
            "/x",
            "--dest",
            "/y",
        ])
        .unwrap();
        let prep = Prep {
            files: 10,
            bytes: 1000,
            src_is_dir: true,
            kind: None,
            corpus: "x".into(),
            drive: "/data".into(),
            ceiling: None,
            elf: None,
            setup_ms: None,
        };
        let m = || Measured::new(1.0, "ava1".into(), "commit_ack");
        let good = finish(&args, &prep, 1, "cold", Ok(m()), Some(Ok((10, 1000))));
        assert!(good.run.ok, "{:?}", good.run.error);
        assert_eq!(good.extra["landed_files"], 10);
        assert_eq!(good.extra["durable"], true);
        let short = finish(&args, &prep, 1, "cold", Ok(m()), Some(Ok((9, 1000))));
        assert!(!short.run.ok);
        assert!(short
            .run
            .error
            .as_deref()
            .unwrap()
            .contains("landed 9 files"));
        let unlisted = finish(&args, &prep, 1, "cold", Ok(m()), Some(Err("boom".into())));
        assert!(!unlisted.run.ok);
    }

    #[test]
    fn the_drive_is_the_mount_or_the_top_directory() {
        assert_eq!(drive_of("/mnt/usb0/bench/x"), "/mnt/usb0");
        assert_eq!(drive_of("/mnt/ext1"), "/mnt/ext1");
        assert_eq!(drive_of("/data/bench"), "/data");
        assert_eq!(drive_of("/user/home"), "/user");
    }

    #[test]
    fn the_ceiling_is_the_newest_calibration_of_this_consoles_drive() {
        let d = TempDir::new("bench-ceiling");
        let f = d.path().join("r.jsonl");
        let cal = |console: &str, dir: &str, at: u64, tops: &[u64]| {
            let mut v = record_envelope("calibrate", None, None, "ava1");
            let o = v.as_object_mut().unwrap();
            o.insert("console".into(), console.into());
            o.insert("dir".into(), dir.into());
            o.insert("started_at".into(), at.into());
            o.insert(
                "points".into(),
                tops.iter()
                    .map(|t| json!({"files_per_s": t}))
                    .collect::<Vec<_>>()
                    .into(),
            );
            v
        };
        record(&f, &cal("1.1.1.1", "/data/cal", 1, &[188, 298])).unwrap();
        record(&f, &cal("1.1.1.1", "/data/cal2", 2, &[200, 310, 300])).unwrap();
        record(&f, &cal("1.1.1.1", "/mnt/usb0/cal", 3, &[575])).unwrap();
        record(&f, &cal("2.2.2.2", "/data/cal", 4, &[999])).unwrap();
        assert_eq!(
            calibrated_ceiling(&f, "1.1.1.1", "/data"),
            Some((310, "/data/cal2".into()))
        );
        assert_eq!(
            calibrated_ceiling(&f, "1.1.1.1", "/mnt/usb0").map(|c| c.0),
            Some(575)
        );
        assert_eq!(calibrated_ceiling(&f, "1.1.1.1", "/mnt/ext1"), None);
        assert_eq!(
            calibrated_ceiling(&d.path().join("none"), "1.1.1.1", "/data"),
            None
        );
    }

    fn row(proto: &str, ok: bool, seconds: f64, ceiling: Option<u64>) -> Row {
        let mut r = run(seconds);
        r.proto = proto.into();
        r.ok = ok;
        Row {
            run: r,
            drive: "/data".into(),
            verified: ok,
            run_kind: "warm".into(),
            ceiling,
            extra: Map::new(),
        }
    }

    #[test]
    fn the_summary_compares_per_drive_next_to_the_ceiling() {
        let rows = vec![
            row("ava1", true, 0.5, Some(300)),
            row("ava1", true, 1.5, Some(300)),
            row("ava1", false, 9.0, Some(300)),
        ];
        let s = summary(&rows, Path::new("results.jsonl"));
        assert!(s.contains("upload-dir on /data"), "{s}");
        assert!(s.contains("300 files/s"), "{s}");
        assert!(
            s.contains("2/3"),
            "failed runs are counted, not averaged: {s}"
        );
        assert!(!s.contains("ftx2"), "{s}");
        assert!(
            s.contains("2 passed, 1 failed") || s.contains("3 passed, 1 failed"),
            "{s}"
        );
        assert!(s.contains("results:") && s.contains("results.jsonl"), "{s}");
    }

    /// A whole AVA1 upload scenario against a loopback host: records one verified line
    /// per run, the file lands, and the line carries the drive.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_ava1_upload_dir_scenario_runs_end_to_end_on_loopback() {
        use ava1::host::FolderHost;
        use ava1::keys::Identity;
        use ava1::peers::PeerStore;
        use ava1::server::{self, ServerCtx};
        use ava1::session::RpcReply;
        use ava1::wire::Message;
        let d = TempDir::new("bench-e2e");
        let ava = d.path().join("ava");
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        let mut peers = PeerStore::in_memory();
        peers.add(me.public(), "bench").unwrap();
        let rpc: ava1::server::RpcHandler = Box::new(|method, _| {
            if method == ava1::gen::METHOD_NODE_INFO {
                let info = ava1::gen::NodeInfo {
                    version: "t".into(),
                    platform: "rust".into(),
                    name: "h".into(),
                    firmware: None,
                };
                RpcReply {
                    status: ava1::gen::STATUS_OK,
                    body: info.to_bytes().unwrap(),
                }
            } else {
                RpcReply {
                    status: ava1::gen::ERR_UNKNOWN_METHOD,
                    body: Vec::new(),
                }
            }
        });
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc).with_jobs(
            Arc::new(FolderHost {
                root: d.path().join("share"),
                jobs_dir: d.path().join("jobs"),
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));

        let src = d.path().join("src");
        corpus_tiny(&src, 40).unwrap();
        let want = stats(&src).unwrap();
        let out = d.path().join("out.jsonl");
        let env = Env {
            pool: PoolRef::Owned(Arc::new(
                ps5upload_ava1::Pool::new(ava.clone()).with_addr(addr),
            )),
            ava_dir: ava,
            idle: Duration::ZERO,
            clean: false,
            out: out.clone(),
        };
        let args = BenchArgs::parse(&[
            "127.0.0.1",
            "upload-dir",
            "--proto",
            "ava1",
            "--src",
            src.to_str().unwrap(),
            "--dest",
            "in",
            "--runs",
            "2",
        ])
        .unwrap();
        let runs = tokio::time::timeout(Duration::from_secs(60), run_bench_in(&env, &args))
            .await
            .expect("bench timed out")
            .unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|r| r.ok), "{runs:?}");
        assert_eq!((runs[0].files, runs[0].bytes), (want.files, want.bytes));
        let lines: Vec<Value> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["verified"], true);
        assert_eq!(lines[0]["observed"], "ava1");
        assert_eq!(lines[0]["drive"], "/in");
        assert_eq!(lines[0]["kind"], "bench");
        assert_eq!(lines[0]["schema"], 1);
        assert!(lines[0]["session_setup_ms"].is_number());
        assert!(
            lines[1].get("session_setup_ms").is_none(),
            "setup is recorded once"
        );
        assert!(d.path().join("share/in").is_dir());
    }

    /// C10: `kills()` counts kills that took down a live connection, and only those.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_chaos_proxy_counts_only_kills_that_hit_something() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((c, _)) = l.accept().await else { break };
                tokio::spawn(async move {
                    let _keep = c;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let p = ava1_chaos::ChaosProxy::start(up, Default::default())
            .await
            .unwrap();
        p.kill_all();
        assert_eq!(p.kills(), 0, "nothing was open");
        let _c = tokio::net::TcpStream::connect(p.addr).await.unwrap();
        for _ in 0..100 {
            if p.connections() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(p.connections(), 1);
        p.kill_all();
        assert_eq!(p.kills(), 1);
    }
}
