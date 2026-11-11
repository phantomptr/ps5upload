//! Tar archives and single `.zst` files, read on this computer for an upload that unpacks on
//! the way (the AVA1 tar source, `ps5upload-ava1/src/tar_source.rs`) and for Convert's unpack
//! (`archive_extract`).
//!
//! Formats: `.tar`, `.tar.zst`/`.tzst`, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, `.tar.bz2`/`.tbz2`/
//! `.tbz`, and a lone `.zst` (one file, named without `.zst`, as `zstd -d` and 7-Zip do). The
//! name says it is a tar; the first bytes say how it is compressed, so a `.tgz` that is really
//! zstd (or a plain tar) still opens, as GNU tar's own detection does.
//!
//! Decoders: zstd is C zstd through the `zstd` crate (the same build the engine already links
//! through ps5-dump-forge-zstd, about three times ruzstd's speed); gzip is `flate2`, xz is
//! `lzma-rust2`, bzip2 is `bzip2` on `libbz2-rs-sys`, all pure Rust.
//!
//! A tar has no index: its file list is the headers spread through the stream. A plain `.tar`
//! is listed by seeking from header to header (instant), and its files are byte ranges of the
//! archive, so the upload reads them like a folder. A compressed tar has to be decoded once to
//! list it ([`tar_layout_watched`], cancellable, with progress), and then once more as it is
//! sent; the listing is kept in memory ([`cached_layout`]) so the inspect, the plan and the
//! upload of one archive decode it for the list only once.
//!
//! Entries: regular files and directories are unpacked. Symbolic links, hard links, devices
//! and FIFOs are left out and counted (the console has no use for them). A GNU sparse file is
//! refused with what to do. Names get the zip rules (`sanitize_zip_entry`): no `..`, no NUL, no
//! backslash; a leading `/` or `./` is dropped (as GNU tar does). A path that appears twice
//! keeps its last copy, which is what extracting the tar leaves on disk.
//!
//! Memory: zstd's window is what a decoder must hold. `zstd --long=31` frames need 2 GiB, more
//! than a phone or a small Docker host should give one upload, so the window is capped
//! ([`zstd_max_window`]) and a frame over the cap fails before anything is allocated, naming
//! the size it needs and what to do. xz's dictionary gets the same cap.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::transfer::{sanitize_zip_entry, LiveNotes, LIVE_PHASE_LISTING};

/// How the bytes of the archive are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    None,
    Zstd,
    Gzip,
    Xz,
    Bzip2,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::None => "tar",
            Codec::Zstd => "zstd",
            Codec::Gzip => "gzip",
            Codec::Xz => "xz",
            Codec::Bzip2 => "bzip2",
        }
    }
}

/// What a file name says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// A tar, plain or compressed.
    Tar,
    /// One file compressed with zstd.
    Zst,
}

/// Endings that name a tar (the longest first is not needed: every one is checked).
pub const TAR_SUFFIXES: [&str; 10] = [
    ".tar", ".tar.zst", ".tzst", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tbz",
];

/// The kind a file name says, if any.
pub fn stream_kind(path: &Path) -> Option<StreamKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if TAR_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        Some(StreamKind::Tar)
    } else if name.ends_with(".zst") {
        Some(StreamKind::Zst)
    } else {
        None
    }
}

/// The name a lone `.zst` unpacks to: its own without `.zst` (`game.exfat.zst` → `game.exfat`).
pub fn zst_output_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = if name.to_ascii_lowercase().ends_with(".zst") {
        &name[..name.len() - 4]
    } else {
        &name[..]
    };
    match sanitize_zip_entry(stem) {
        Some(s) if !s.contains('/') => s,
        _ => "unpacked".to_string(),
    }
}

/// The codec the first bytes of a file show. A ustar header (magic at 257) wins over the
/// compressors' magics: a plain tar whose first file is named "BZh…" is still a tar.
pub fn sniff_codec(head: &[u8]) -> Codec {
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Codec::None;
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) || is_skippable_magic(head) {
        Codec::Zstd
    } else if head.starts_with(&[0x1F, 0x8B]) {
        Codec::Gzip
    } else if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        Codec::Xz
    } else if head.starts_with(b"BZh") {
        Codec::Bzip2
    } else {
        Codec::None
    }
}

fn is_skippable_magic(head: &[u8]) -> bool {
    head.len() >= 4 && u32::from_le_bytes([head[0], head[1], head[2], head[3]]) >> 4 == 0x0184_D2A5
}

// ─── Faults ───────────────────────────────────────────────────────────────────

/// Why an archive cannot be read, carried inside an `io::Error` (`InvalidData`) so it survives
/// the sender's error chain and the upload can give it a stable reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TarFault {
    /// The stream is damaged, truncated or fails a checksum.
    #[error("the archive is damaged: {0}")]
    Corrupt(String),
    /// A zstd window or xz dictionary over this computer's cap.
    #[error("{0}")]
    TooMuchMemory(String),
    /// An entry name that would escape the destination.
    #[error("the archive has an unsafe entry path: {0}")]
    UnsafePath(String),
    /// A feature this reader does not handle (a sparse file, two names that differ only in
    /// case, a path that is both a file and a folder).
    #[error("{0}")]
    Unsupported(String),
}

pub fn fault(f: TarFault) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, f)
}

fn corrupt(msg: impl Into<String>) -> io::Error {
    fault(TarFault::Corrupt(msg.into()))
}

/// The [`TarFault`] inside an error chain, if any.
pub fn tar_fault_of<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a TarFault> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if let Some(f) = c.downcast_ref::<TarFault>() {
            return Some(f);
        }
        if let Some(io) = c.downcast_ref::<io::Error>() {
            if let Some(f) = io.get_ref().and_then(|i| i.downcast_ref::<TarFault>()) {
                return Some(f);
            }
        }
        cur = c.source();
    }
    None
}

// ─── Memory cap ───────────────────────────────────────────────────────────────

const MIB: u64 = 1 << 20;
/// zstd's own default decode limit (`--memory=128MB`, window log 27): every archive made
/// without `--long` (and with `--long` at its default 27) fits.
pub const ZSTD_WINDOW_FLOOR: u64 = 128 * MIB;
/// The largest window `zstd --long` makes (window log 31).
pub const ZSTD_WINDOW_CEIL: u64 = 2048 * MIB;

/// The largest zstd window (and xz dictionary) a decoder here may hold: an eighth of this
/// computer's memory (the container's limit when there is one), at least zstd's own default of
/// 128 MiB, at most 2 GiB (512 MiB on a phone). `PS5UPLOAD_ZSTD_MAX_WINDOW_MB` overrides it.
pub fn zstd_max_window() -> u64 {
    static CAP: OnceLock<u64> = OnceLock::new();
    *CAP.get_or_init(|| {
        select_max_window(
            std::env::var("PS5UPLOAD_ZSTD_MAX_WINDOW_MB")
                .ok()
                .as_deref(),
            usable_memory(),
            cfg!(any(target_os = "android", target_os = "ios")),
        )
    })
}

/// Rounded down to a power of two: zstd's limit is a window log, and the header check and
/// the decoder's `windowLogMax` must agree.
fn select_max_window(configured: Option<&str>, memory: Option<u64>, phone: bool) -> u64 {
    let pow2 = |v: u64| 1u64 << (63 - v.max(1).leading_zeros());
    if let Some(mb) = configured.and_then(|v| v.trim().parse::<u64>().ok()) {
        return pow2(mb.saturating_mul(MIB).clamp(MIB, ZSTD_WINDOW_CEIL));
    }
    let ceil = if phone { 512 * MIB } else { ZSTD_WINDOW_CEIL };
    pow2(memory.map_or(ZSTD_WINDOW_FLOOR, |m| {
        (m / 8).clamp(ZSTD_WINDOW_FLOOR, ceil.max(ZSTD_WINDOW_FLOOR))
    }))
}

/// This computer's memory, or the container's limit when that is lower (Linux cgroups).
fn usable_memory() -> Option<u64> {
    let total = crate::host_info::host_info().memory_bytes;
    match (total, cgroup_limit()) {
        (Some(t), Some(c)) => Some(t.min(c)),
        (t, c) => t.or(c),
    }
}

#[cfg(target_os = "linux")]
fn cgroup_limit() -> Option<u64> {
    for p in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        if let Ok(s) = std::fs::read_to_string(p) {
            // "max", or an unlimited v1 value near i64::MAX.
            if let Ok(n) = s.trim().parse::<u64>() {
                if n < (1 << 60) {
                    return Some(n);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn cgroup_limit() -> Option<u64> {
    None
}

fn human_mib(bytes: u64) -> String {
    if bytes >= 1 << 30 && bytes.is_multiple_of(1 << 30) {
        format!("{} GiB", bytes >> 30)
    } else {
        format!("{} MiB", bytes.div_ceil(MIB))
    }
}

fn window_too_big(needed: u64, allowed: u64) -> io::Error {
    fault(TarFault::TooMuchMemory(format!(
        "this archive was compressed with a {} zstd window (zstd --long), and this computer \
         allows {} for one. Unpack it on a computer with more memory, set \
         PS5UPLOAD_ZSTD_MAX_WINDOW_MB={} and restart the app (it then needs that much free \
         memory), or compress it again without --long",
        human_mib(needed),
        human_mib(allowed),
        needed.div_ceil(MIB)
    )))
}

fn dictionary_too_big(allowed: u64) -> io::Error {
    fault(TarFault::TooMuchMemory(format!(
        "this .xz needs a dictionary larger than the {} this computer allows for one. Unpack it \
         on a computer with more memory, set PS5UPLOAD_ZSTD_MAX_WINDOW_MB higher and restart \
         the app, or compress it again with a smaller dictionary (xz -9 or lower)",
        human_mib(allowed)
    )))
}

// ─── zstd ─────────────────────────────────────────────────────────────────────

/// What a frame's first bytes say.
#[derive(Debug, PartialEq, Eq)]
enum FrameHead {
    /// A zstd frame needing this window (its content size, for a single-segment frame).
    Zstd { window: u64 },
    /// A skippable frame (metadata): C zstd passes over it.
    Skippable,
    /// Not enough bytes yet to tell.
    Short,
    /// Neither magic number.
    Bad(u32),
}

/// Reads a frame header far enough to know its window (RFC 8878 §3.1.1.1).
fn frame_head(h: &[u8]) -> FrameHead {
    if h.len() < 4 {
        return FrameHead::Short;
    }
    let magic = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    if magic >> 4 == 0x0184_D2A5 {
        return FrameHead::Skippable;
    }
    if magic != 0xFD2F_B528 {
        return FrameHead::Bad(magic);
    }
    let Some(&fhd) = h.get(4) else {
        return FrameHead::Short;
    };
    if fhd & 0x20 == 0 {
        // Window_Descriptor: 2^(10 + exponent), plus eighths of that by the mantissa.
        let Some(&wd) = h.get(5) else {
            return FrameHead::Short;
        };
        let base = 1u64 << (10 + u32::from(wd >> 3));
        return FrameHead::Zstd {
            window: base + (base / 8) * u64::from(wd & 7),
        };
    }
    // Single segment: the window is the content size.
    let dict_len = [0usize, 1, 2, 4][usize::from(fhd & 3)];
    let fcs_len = [1usize, 2, 4, 8][usize::from(fhd >> 6)];
    let at = 5 + dict_len;
    let Some(bytes) = h.get(at..at + fcs_len) else {
        return FrameHead::Short;
    };
    let mut v = [0u8; 8];
    v[..fcs_len].copy_from_slice(bytes);
    let size = u64::from_le_bytes(v) + if fcs_len == 2 { 256 } else { 0 };
    FrameHead::Zstd { window: size }
}

/// The longest frame header (magic, descriptor, window, dictionary id, content size).
const FRAME_HEAD_MAX: usize = 18;

/// A zstd stream as a `Read`, decoded by C zstd (the `zstd` crate the engine already links):
/// every frame in turn (`pzstd` and `cat a.zst b.zst` make several), skippable frames passed
/// over, each frame's content checksum checked by zstd itself. Before a frame is handed to the
/// decoder its header is read here and a window over `max_window` is refused, so nothing is
/// allocated for it and the error names the size it needs; the decoder's own `windowLogMax`
/// is set from the same cap as a second guard.
pub struct ZstdReader<R: Read> {
    src: R,
    dec: zstd::stream::raw::Decoder<'static>,
    max_window: u64,
    /// Compressed input read from `src`, `start..end` not yet given to the decoder.
    buf: Box<[u8]>,
    start: usize,
    end: usize,
    eof: bool,
    /// The next input byte starts a frame.
    at_frame: bool,
    frames: u64,
}

impl<R: Read> ZstdReader<R> {
    pub fn new(src: R) -> Self {
        Self::with_max_window(src, zstd_max_window())
    }

    pub fn with_max_window(src: R, max_window: u64) -> Self {
        let mut dec = zstd::stream::raw::Decoder::new().expect("a zstd decoding context");
        // The cap as a window log (it is a power of two; zstd takes 10..=31 here).
        let log = (63 - max_window.max(1).leading_zeros()).clamp(10, 31);
        dec.set_parameter(zstd::stream::raw::DParameter::WindowLogMax(log))
            .expect("windowLogMax in range");
        Self {
            src,
            dec,
            max_window,
            buf: vec![0u8; 256 << 10].into_boxed_slice(),
            start: 0,
            end: 0,
            eof: false,
            at_frame: true,
            frames: 0,
        }
    }

    /// Reads more input unless at the end, keeping what is unread; true when it added some.
    fn refill(&mut self) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        loop {
            match self.src.read(&mut self.buf[self.end..]) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    self.end += n;
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// At a frame boundary: refuse a window over the cap before zstd sees it. False at the end.
    fn check_frame(&mut self) -> io::Result<bool> {
        loop {
            match frame_head(&self.buf[self.start..self.end]) {
                FrameHead::Short if self.end - self.start < FRAME_HEAD_MAX && self.refill()? => {
                    continue
                }
                FrameHead::Short if self.start == self.end => {
                    if self.frames == 0 {
                        return Err(corrupt("the zstd stream is empty"));
                    }
                    return Ok(false);
                }
                FrameHead::Short => {
                    return Err(corrupt("the zstd stream ends inside a frame header"))
                }
                FrameHead::Bad(m) => {
                    return Err(corrupt(format!(
                        "not a zstd frame (it starts with {m:#010x}){}",
                        if self.frames > 0 {
                            " after the last frame"
                        } else {
                            ""
                        }
                    )))
                }
                FrameHead::Zstd { window } if window > self.max_window => {
                    return Err(window_too_big(window, self.max_window))
                }
                FrameHead::Zstd { .. } | FrameHead::Skippable => {
                    self.at_frame = false;
                    self.frames += 1;
                    return Ok(true);
                }
            }
        }
    }
}

/// zstd's own error, as a fault. "Frame requires too much memory for decoding" is its
/// windowLogMax refusal (`ZSTD_error_frameParameter_windowTooLarge`); the header check above
/// catches that first, so this only names the cap.
fn zstd_error(e: io::Error, max_window: u64) -> io::Error {
    let msg = e.to_string();
    if msg.contains("Frame requires too much memory") {
        fault(TarFault::TooMuchMemory(format!(
            "this archive was compressed with a zstd window (zstd --long) larger than the {} \
             this computer allows for one. Unpack it on a computer with more memory, set \
             PS5UPLOAD_ZSTD_MAX_WINDOW_MB higher and restart the app, or compress it again \
             without --long",
            human_mib(max_window)
        )))
    } else {
        corrupt(format!("zstd: {msg}"))
    }
}

impl<R: Read> Read for ZstdReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        use zstd::stream::raw::{InBuffer, Operation, OutBuffer};
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.at_frame && !self.check_frame()? {
                return Ok(0);
            }
            if self.start == self.end && !self.refill()? {
                // The decoder may still hold output for the input it has had.
                let mut ob = OutBuffer::around(&mut *out);
                let hint = self
                    .dec
                    .run(&mut InBuffer::around(&[]), &mut ob)
                    .map_err(|e| zstd_error(e, self.max_window))?;
                let produced = ob.pos();
                if hint == 0 {
                    self.at_frame = true;
                }
                if produced > 0 {
                    return Ok(produced);
                }
                if hint == 0 {
                    continue;
                }
                return Err(corrupt(
                    "the zstd stream ends inside a frame: the archive is cut short (a download \
                     that did not finish?)",
                ));
            }
            let mut ib = InBuffer::around(&self.buf[self.start..self.end]);
            let mut ob = OutBuffer::around(&mut *out);
            let hint = self
                .dec
                .run(&mut ib, &mut ob)
                .map_err(|e| zstd_error(e, self.max_window))?;
            self.start += ib.pos();
            let produced = ob.pos();
            // 0: a frame is decoded and flushed; the next byte starts another.
            if hint == 0 {
                self.at_frame = true;
            }
            if produced > 0 {
                return Ok(produced);
            }
        }
    }
}

/// A lone `.zst`'s unpacked size from its frame headers, without decoding: each frame's
/// content size, its blocks walked by their headers. `None` when a frame does not say
/// (a stream compressed from a pipe), and the file must be decoded to learn it.
pub fn zst_content_size(path: &Path) -> io::Result<Option<u64>> {
    let mut f = BufReader::with_capacity(64 << 10, File::open(path)?);
    let mut total = 0u64;
    let mut frames = 0u64;
    loop {
        let mut magic = [0u8; 4];
        match f.read_exact(&mut magic) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && frames > 0 => {
                return Ok(Some(total))
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(corrupt("the .zst file is empty or cut short"))
            }
            Err(e) => return Err(e),
        }
        frames += 1;
        let m = u32::from_le_bytes(magic);
        if m >> 4 == 0x0184_D2A5 {
            let mut len = [0u8; 4];
            f.read_exact(&mut len).map_err(short)?;
            f.seek_relative(i64::from(u32::from_le_bytes(len)))?;
            continue;
        }
        if m != 0xFD2F_B528 {
            return Err(corrupt(format!(
                "not a zstd frame (it starts with {m:#010x})"
            )));
        }
        let mut d = [0u8; 1];
        f.read_exact(&mut d).map_err(short)?;
        let d = d[0];
        let fcs_flag = d >> 6;
        let single_segment = d & 0x20 != 0;
        let checksum = d & 0x04 != 0;
        let dict_len = [0usize, 1, 2, 4][(d & 0x03) as usize];
        let fcs_len = match fcs_flag {
            0 if single_segment => 1,
            0 => 0,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let skip = usize::from(!single_segment) + dict_len;
        let mut hdr = [0u8; 13];
        f.read_exact(&mut hdr[..skip + fcs_len]).map_err(short)?;
        if fcs_len == 0 {
            return Ok(None);
        }
        let mut v = [0u8; 8];
        v[..fcs_len].copy_from_slice(&hdr[skip..skip + fcs_len]);
        let mut size = u64::from_le_bytes(v);
        if fcs_len == 2 {
            size += 256;
        }
        total = total.saturating_add(size);
        loop {
            let mut bh = [0u8; 3];
            f.read_exact(&mut bh).map_err(short)?;
            let h = u32::from(bh[0]) | u32::from(bh[1]) << 8 | u32::from(bh[2]) << 16;
            let last = h & 1 != 0;
            let kind = (h >> 1) & 3;
            let size = h >> 3;
            let stored = match kind {
                1 => 1, // RLE: one byte, repeated
                3 => return Err(corrupt("a zstd block has the reserved type")),
                _ => size,
            };
            f.seek_relative(i64::from(stored))?;
            if last {
                break;
            }
        }
        if checksum {
            f.seek_relative(4)?;
        }
    }
}

/// A zip entry compressed with zstd (method 93), as written by 7-Zip ZS, WinZip and Python
/// 3.14: its raw bytes decoded with [`ZstdReader`], its size and CRC-32 checked at the end.
/// Errors are `InvalidData` (a damaged entry).
pub struct ZipZstdEntry<R: Read> {
    inner: ZstdReader<R>,
    crc: crc32fast::Hasher,
    want_crc: u32,
    want_size: u64,
    got: u64,
}

impl<R: Read> ZipZstdEntry<R> {
    pub fn new(raw: R, crc: u32, size: u64) -> Self {
        Self {
            inner: ZstdReader::new(raw),
            crc: crc32fast::Hasher::new(),
            want_crc: crc,
            want_size: size,
            got: 0,
        }
    }
}

/// The zip method number zstd uses.
pub const ZIP_METHOD_ZSTD: u16 = 93;

impl<R: Read> Read for ZipZstdEntry<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.got += n as u64;
        if self.got > self.want_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "zip entry decodes to more than its declared size",
            ));
        }
        self.crc.update(&buf[..n]);
        if n == 0 || (self.got == self.want_size && !buf.is_empty()) {
            if self.got != self.want_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zip entry ends before its declared size",
                ));
            }
            let got = self.crc.clone().finalize();
            if got != self.want_crc {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "zip entry CRC-32 mismatch (stored {:08x}, computed {got:08x})",
                        self.want_crc
                    ),
                ));
            }
        }
        Ok(n)
    }
}

fn short(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        corrupt("the .zst file is cut short")
    } else {
        e
    }
}

// ─── The archive file, tapped ─────────────────────────────────────────────────

/// The archive file: counts what is read (the listing's progress) and keeps the first read
/// error, so a decoder that turns a failed read into "corrupt" is overruled by the real cause
/// (as the 7z source does, #446).
struct Tapped {
    file: File,
    read: Arc<AtomicU64>,
    failure: Arc<Mutex<Option<(io::ErrorKind, String)>>>,
}

impl Read for Tapped {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Ok(n) => {
                self.read.fetch_add(n as u64, Ordering::Relaxed);
                Ok(n)
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::Interrupted {
                    let mut f = self.failure.lock().unwrap_or_else(|p| p.into_inner());
                    f.get_or_insert_with(|| (e.kind(), e.to_string()));
                }
                Err(e)
            }
        }
    }
}

impl Seek for Tapped {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

/// An open archive: the decoded byte stream, how much of the file has been read, and the
/// first read error. A plain tar can seek (its listing jumps from header to header).
pub struct Opened {
    pub codec: Codec,
    inner: Inner,
    /// Where in the file the stream starts (a plain tar opened part way).
    base: u64,
    read: Arc<AtomicU64>,
    failure: Arc<Mutex<Option<(io::ErrorKind, String)>>>,
}

enum Inner {
    Plain(BufReader<Tapped>),
    Decoded(Box<dyn Read + Send>),
}

impl Opened {
    /// Compressed bytes read from the file so far.
    pub fn file_bytes_read(&self) -> u64 {
        self.read.load(Ordering::Relaxed)
    }

    /// A decode error, or the read error behind it when the file failed to read: a disk that
    /// stopped answering is not a damaged archive (a job retries the one, not the other).
    pub fn explain(&self, e: io::Error) -> io::Error {
        let seen = self
            .failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        match seen {
            Some((kind, why)) => io::Error::new(kind, format!("reading the archive failed: {why}")),
            None => classify(e),
        }
    }
}

impl Read for Opened {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            Inner::Plain(r) => r.read(buf),
            Inner::Decoded(r) => r.read(buf),
        }
    }
}

impl Seek for Opened {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match &mut self.inner {
            // Positions are the stream's, which starts at `base` in the file (the tar reader
            // keeps its own count from 0 and resets it to what a seek returns).
            Inner::Plain(r) => {
                let pos = match pos {
                    SeekFrom::Start(n) => SeekFrom::Start(self.base.saturating_add(n)),
                    other => other,
                };
                Ok(r.seek(pos)?.saturating_sub(self.base))
            }
            Inner::Decoded(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a compressed tar is read in order",
            )),
        }
    }
}

/// The tar reader over an opened archive: a plain tar seeks past what it does not read.
fn entries<'a, 'b>(
    ar: &'a mut tar::Archive<&'b mut Opened>,
    plain: bool,
) -> io::Result<tar::Entries<'a, &'b mut Opened>> {
    if plain {
        ar.entries_with_seek()
    } else {
        ar.entries()
    }
}

/// An error of the decoder or the tar reader as a fault: memory when a dictionary is over the
/// cap, otherwise damage. Only called where a failed read of the file itself has already been
/// ruled out ([`Opened::explain`] checks that first), so what is left is the archive's bytes:
/// a bad header checksum, a damaged stream, an end that comes too early.
fn classify(e: io::Error) -> io::Error {
    if e.get_ref()
        .and_then(|i| i.downcast_ref::<TarFault>())
        .is_some()
    {
        return e;
    }
    match e.kind() {
        io::ErrorKind::OutOfMemory => dictionary_too_big(zstd_max_window()),
        io::ErrorKind::UnexpectedEof => {
            corrupt("it ends early (a download that did not finish?): test it with 7-Zip or tar -t")
        }
        _ => corrupt(format!("{e}")),
    }
}

/// Opens `path` decoded, from the start, or (a plain tar only) from byte `offset`.
pub fn open_decoded(path: &Path, offset: u64) -> io::Result<Opened> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 512];
    let n = read_up_to(&mut file, &mut head)?;
    let codec = sniff_codec(&head[..n]);
    if offset > 0 && codec != Codec::None {
        return Err(io::Error::other("only a plain tar can be opened part way"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let read = Arc::new(AtomicU64::new(offset));
    let failure = Arc::new(Mutex::new(None));
    let tapped = Tapped {
        file,
        read: read.clone(),
        failure: failure.clone(),
    };
    let src = BufReader::with_capacity(256 << 10, tapped);
    let decoded: Box<dyn Read + Send> = match codec {
        Codec::None => {
            return Ok(Opened {
                codec,
                inner: Inner::Plain(src),
                base: offset,
                read,
                failure,
            })
        }
        Codec::Zstd => Box::new(ZstdReader::new(src)),
        Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(src)),
        Codec::Xz => {
            let kb = u32::try_from(zstd_max_window() / 1024).unwrap_or(u32::MAX);
            Box::new(lzma_rust2::XzReader::new_mem_limit(src, true, kb))
        }
        Codec::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(src)),
    };
    // Buffered for the tar reader's 512-byte header reads.
    Ok(Opened {
        codec,
        inner: Inner::Decoded(Box::new(BufReader::with_capacity(256 << 10, decoded))),
        base: 0,
        read,
        failure,
    })
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

// ─── Layout ───────────────────────────────────────────────────────────────────

/// One regular file of the archive, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarFile {
    /// Sanitised, '/'-separated, relative.
    pub path: String,
    pub size: u64,
    /// Unix seconds; 0 when the header has none.
    pub mtime: u64,
    pub mode: u32,
    /// Where the entry's first header starts (its long-name and PAX headers included), in
    /// the decoded stream: for a plain tar, in the file. A walk can start here.
    pub header_offset: u64,
    /// Where its bytes start, likewise.
    pub data_offset: u64,
    /// False when a later entry of the same path replaces this one.
    pub kept: bool,
}

/// What a tar holds, from one pass over its headers.
#[derive(Debug, Clone)]
pub struct TarLayout {
    pub codec: Codec,
    /// A lone `.zst`: `files` is its one file.
    pub single: bool,
    pub files: Vec<TarFile>,
    pub dirs: Vec<String>,
    /// Symbolic and hard links left out.
    pub links: u64,
    /// Devices and FIFOs left out.
    pub special: u64,
    /// Size of the archive file.
    pub archive_size: u64,
    /// Names the archive's contents (size, first and last bytes, the listing): a changed
    /// archive with the same name does not resume a job written for the old one.
    pub identity: [u8; 32],
    /// The shallowest `sce_sys/param.json`: the folder that holds `sce_sys` and its bytes.
    pub param_json: Option<(String, Vec<u8>)>,
}

impl TarLayout {
    /// Files that land (the last copy of each path).
    pub fn kept(&self) -> impl Iterator<Item = &TarFile> {
        self.files.iter().filter(|f| f.kept)
    }

    pub fn unpacked_total(&self) -> u64 {
        self.kept().fold(0u64, |s, f| s.saturating_add(f.size))
    }

    /// True when every byte must be decoded in order (anything but a plain tar).
    pub fn sequential(&self) -> bool {
        self.codec != Codec::None
    }
}

/// What may stop [`tar_layout_watched`] and what it reports.
#[derive(Default, Clone, Copy)]
pub struct TarWatch<'a> {
    pub cancel: Option<&'a AtomicBool>,
    /// Gets phase "listing" with the file bytes read of the archive's size.
    pub live: Option<&'a LiveNotes>,
    /// Files listed so far, every 1000 (an inspect's heartbeat).
    pub on_files: Option<&'a dyn Fn(u64)>,
}

/// What [`tar_layout_watched`] fails with when `cancel` is raised.
pub const TAR_LISTING_CANCELLED: &str = "transfer_cancelled";

/// The archive's layout, listing it when the cache does not hold it.
pub fn tar_layout(path: &Path) -> io::Result<Arc<TarLayout>> {
    tar_layout_watched(path, TarWatch::default())
}

pub fn tar_layout_watched(path: &Path, watch: TarWatch<'_>) -> io::Result<Arc<TarLayout>> {
    let key = cache_key(path)?;
    if let Some(l) = cache_get(&key) {
        return Ok(l);
    }
    let layout = Arc::new(list(path, watch)?);
    cache_put(key, layout.clone());
    Ok(layout)
}

/// The layout the cache holds for `path` as it is now, without listing.
pub fn cached_layout(path: &Path) -> Option<Arc<TarLayout>> {
    cache_get(&cache_key(path).ok()?)
}

#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    len: u64,
    mtime: Option<std::time::SystemTime>,
}

const CACHE_MAX: usize = 4;

/// Recent listings, oldest first.
type Cache = Mutex<Vec<(CacheKey, Arc<TarLayout>)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

fn cache_key(path: &Path) -> io::Result<CacheKey> {
    let m = std::fs::metadata(path)?;
    Ok(CacheKey {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()),
        len: m.len(),
        mtime: m.modified().ok(),
    })
}

fn cache_get(key: &CacheKey) -> Option<Arc<TarLayout>> {
    let c = cache().lock().unwrap_or_else(|p| p.into_inner());
    c.iter().find(|(k, _)| k == key).map(|(_, l)| l.clone())
}

fn cache_put(key: CacheKey, layout: Arc<TarLayout>) {
    let mut c = cache().lock().unwrap_or_else(|p| p.into_inner());
    c.retain(|(k, _)| k.path != key.path);
    c.push((key, layout));
    if c.len() > CACHE_MAX {
        c.remove(0);
    }
}

/// A param.json bigger than this is not read (a real one is a few KiB).
const MAX_PARAM_JSON: u64 = 4 << 20;

fn list(path: &Path, watch: TarWatch<'_>) -> io::Result<TarLayout> {
    let archive_size = std::fs::metadata(path)?.len();
    if stream_kind(path) == Some(StreamKind::Zst) {
        return list_single(path, archive_size, watch);
    }
    // Set back to "sending" when the listing ends, however it ends.
    struct Phase<'a>(Option<&'a LiveNotes>);
    impl Drop for Phase<'_> {
        fn drop(&mut self) {
            if let Some(l) = self.0 {
                l.phase.store(0, Ordering::Relaxed);
                l.skip_done_bytes.store(0, Ordering::Relaxed);
                l.skip_total_bytes.store(0, Ordering::Relaxed);
            }
        }
    }
    let mut opened = open_decoded(path, 0)?;
    let codec = opened.codec;
    let _phase = Phase(watch.live.filter(|_| codec != Codec::None));
    if let (Some(l), true) = (watch.live, codec != Codec::None) {
        l.skip_total_bytes.store(archive_size, Ordering::Relaxed);
        l.skip_done_bytes.store(0, Ordering::Relaxed);
        l.phase.store(LIVE_PHASE_LISTING, Ordering::Relaxed);
    }
    let read = opened.read.clone();
    let cancelled = || watch.cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let report = |n: u64| {
        if let Some(l) = watch.live {
            l.skip_done_bytes
                .store(read.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        if let (Some(f), true) = (watch.on_files, n.is_multiple_of(1000)) {
            f(n);
        }
    };

    let mut files: Vec<TarFile> = Vec::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let (mut links, mut special) = (0u64, 0u64);
    let mut param: Option<(usize, String, Vec<u8>)> = None;
    let result = (|| -> io::Result<()> {
        let mut ar = tar::Archive::new(&mut opened);
        let mut entries = entries(&mut ar, codec == Codec::None).map_err(classify)?;
        let mut next_header = 0u64;
        let mut buf = vec![0u8; 256 << 10];
        loop {
            if cancelled() {
                return Err(io::Error::other(TAR_LISTING_CANCELLED));
            }
            let Some(entry) = entries.next() else { break };
            let mut entry = entry.map_err(classify)?;
            // An entry's headers start where the one before it ends: its data, padded to 512.
            let header_offset = next_header;
            next_header = align512(entry.raw_file_position().saturating_add(entry.size()));
            let what = Kind::of(&entry)?;
            let raw = entry.path_bytes().into_owned();
            let name = String::from_utf8(raw)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            match what {
                Kind::Skip => {}
                Kind::Link => links += 1,
                Kind::Special => special += 1,
                Kind::Dir => {
                    // `./` (the archive's own root) carries nothing; an unsafe folder name is
                    // ignored, its files are refused by name.
                    if let Some(rel) = sanitize_zip_entry(&name) {
                        dirs.insert(rel);
                    }
                }
                Kind::File => {
                    let Some(rel) = sanitize_zip_entry(&name) else {
                        return Err(fault(TarFault::UnsafePath(format!("{name:?}"))));
                    };
                    let size = entry.size();
                    let mtime = entry.header().mtime().unwrap_or(0);
                    let mode = match entry.header().mode().unwrap_or(0) & 0o777 {
                        0 => 0o644,
                        m => m,
                    };
                    let data_offset = entry.raw_file_position();
                    let cut_short = || {
                        corrupt(format!(
                            "{rel} ends before its {size} bytes: the archive is cut short (a \
                             download that did not finish?)"
                        ))
                    };
                    // A plain tar's listing seeks past the data: the file must hold all of it.
                    if codec == Codec::None && data_offset.saturating_add(size) > archive_size {
                        return Err(cut_short());
                    }
                    // The shallowest sce_sys/param.json names the game; read it as it passes.
                    let wants_param = rel.ends_with("sce_sys/param.json")
                        && (rel == "sce_sys/param.json" || rel.ends_with("/sce_sys/param.json"))
                        && size <= MAX_PARAM_JSON
                        && param
                            .as_ref()
                            .is_none_or(|(d, _, _)| rel.split('/').count() < *d);
                    if wants_param {
                        let mut bytes = Vec::with_capacity(size as usize);
                        (&mut entry)
                            .take(size)
                            .read_to_end(&mut bytes)
                            .map_err(classify)?;
                        if bytes.len() as u64 != size {
                            return Err(cut_short());
                        }
                        let root = rel
                            .strip_suffix("sce_sys/param.json")
                            .unwrap_or("")
                            .trim_end_matches('/')
                            .to_string();
                        param = Some((rel.split('/').count(), root, bytes));
                    } else if codec != Codec::None {
                        // Read past the data here, not in the tar reader, so a long file still
                        // polls the cancel and moves the progress.
                        let mut got = 0u64;
                        loop {
                            if cancelled() {
                                return Err(io::Error::other(TAR_LISTING_CANCELLED));
                            }
                            let n = entry.read(&mut buf).map_err(classify)?;
                            if n == 0 {
                                break;
                            }
                            got += n as u64;
                            report(1);
                        }
                        if got != size {
                            return Err(cut_short());
                        }
                    }
                    files.push(TarFile {
                        path: rel,
                        size,
                        mtime,
                        mode,
                        header_offset,
                        data_offset,
                        kept: true,
                    });
                    report(files.len() as u64);
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        if e.to_string() == TAR_LISTING_CANCELLED {
            return Err(e);
        }
        return Err(opened.explain(e));
    }
    if let Some(f) = watch.on_files {
        f(files.len() as u64);
    }
    finish_layout(
        path,
        codec,
        false,
        files,
        dirs,
        links,
        special,
        archive_size,
        param.map(|(_, r, b)| (r, b)),
    )
}

fn list_single(path: &Path, archive_size: u64, watch: TarWatch<'_>) -> io::Result<TarLayout> {
    let mut head = [0u8; 4];
    let n = read_up_to(&mut File::open(path)?, &mut head)?;
    if sniff_codec(&head[..n]) != Codec::Zstd {
        return Err(corrupt("the file is not zstd-compressed"));
    }
    let size = match zst_content_size(path)? {
        Some(s) => s,
        None => {
            // A frame without its size: decode once to learn it.
            let mut opened = open_decoded(path, 0)?;
            let mut buf = vec![0u8; 256 << 10];
            let mut total = 0u64;
            if let Some(l) = watch.live {
                l.skip_total_bytes.store(archive_size, Ordering::Relaxed);
                l.phase.store(LIVE_PHASE_LISTING, Ordering::Relaxed);
            }
            let r = (|| loop {
                if watch.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return Err(io::Error::other(TAR_LISTING_CANCELLED));
                }
                match opened.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => total += n as u64,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
                if let Some(l) = watch.live {
                    l.skip_done_bytes
                        .store(opened.file_bytes_read(), Ordering::Relaxed);
                }
            })();
            if let Some(l) = watch.live {
                l.phase.store(0, Ordering::Relaxed);
                l.skip_done_bytes.store(0, Ordering::Relaxed);
                l.skip_total_bytes.store(0, Ordering::Relaxed);
            }
            match r {
                Ok(()) => total,
                Err(e) if e.to_string() == TAR_LISTING_CANCELLED => return Err(e),
                Err(e) => return Err(opened.explain(e)),
            }
        }
    };
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let file = TarFile {
        path: zst_output_name(path),
        size,
        mtime,
        mode: 0o644,
        header_offset: 0,
        data_offset: 0,
        kept: true,
    };
    finish_layout(
        path,
        Codec::Zstd,
        true,
        vec![file],
        BTreeSet::new(),
        0,
        0,
        archive_size,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_layout(
    path: &Path,
    codec: Codec,
    single: bool,
    mut files: Vec<TarFile>,
    mut dirs: BTreeSet<String>,
    links: u64,
    special: u64,
    archive_size: u64,
    param_json: Option<(String, Vec<u8>)>,
) -> io::Result<TarLayout> {
    // The last copy of a path is the one extracting the tar leaves on disk.
    let mut last: HashMap<&str, usize> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        last.insert(f.path.as_str(), i);
    }
    let keep: Vec<bool> = (0..files.len())
        .map(|i| last.get(files[i].path.as_str()) == Some(&i))
        .collect();
    drop(last);
    for (f, k) in files.iter_mut().zip(keep) {
        f.kept = k;
    }
    // Console drives may fold case: two paths that differ only in case would land on one.
    let mut folded: HashMap<String, &str> = HashMap::new();
    for f in files.iter().filter(|f| f.kept) {
        if let Some(other) = folded.insert(f.path.to_lowercase(), &f.path) {
            return Err(fault(TarFault::Unsupported(format!(
                "{other} and {} differ only in case, and the console's drive may not tell them \
                 apart; rename one and pack the archive again",
                f.path
            ))));
        }
    }
    drop(folded);
    for f in files.iter().filter(|f| f.kept) {
        let parts: Vec<&str> = f.path.split('/').collect();
        let mut acc = String::new();
        for c in &parts[..parts.len() - 1] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(c);
            dirs.insert(acc.clone());
        }
    }
    if files.iter().any(|f| f.kept && dirs.contains(&f.path)) {
        return Err(fault(TarFault::Unsupported(
            "a path in the archive is both a file and a folder".into(),
        )));
    }
    let identity = identity(path, archive_size, &files)?;
    Ok(TarLayout {
        codec,
        single,
        files,
        dirs: dirs.into_iter().collect(),
        links,
        special,
        archive_size,
        identity,
        param_json,
    })
}

fn identity(path: &Path, len: u64, files: &[TarFile]) -> io::Result<[u8; 32]> {
    const EDGE: u64 = 64 << 10;
    let mut h = blake3::Hasher::new();
    h.update(&len.to_le_bytes());
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; EDGE as usize];
    let n = read_up_to(&mut f, &mut buf)?;
    h.update(&buf[..n]);
    if len > EDGE {
        f.seek(SeekFrom::Start(len - EDGE))?;
        let n = read_up_to(&mut f, &mut buf)?;
        h.update(&buf[..n]);
    }
    for t in files {
        h.update(t.path.as_bytes());
        h.update(&[0]);
        h.update(&t.size.to_le_bytes());
        h.update(&t.mtime.to_le_bytes());
    }
    Ok(*h.finalize().as_bytes())
}

fn align512(n: u64) -> u64 {
    n.div_ceil(512).saturating_mul(512)
}

/// What an entry is to this reader.
enum Kind {
    File,
    Dir,
    Link,
    Special,
    /// Carries nothing to unpack (a PAX global header, a volume label).
    Skip,
}

impl Kind {
    fn of<R: Read>(e: &tar::Entry<'_, R>) -> io::Result<Kind> {
        use tar::EntryType as T;
        let t = e.header().entry_type();
        Ok(match t {
            T::Regular | T::Continuous => Kind::File,
            T::Directory => Kind::Dir,
            T::Symlink | T::Link => Kind::Link,
            T::Char | T::Block | T::Fifo => Kind::Special,
            T::XGlobalHeader => Kind::Skip,
            T::GNUSparse => {
                return Err(fault(TarFault::Unsupported(format!(
                    "{} is a sparse file, which this app does not unpack; create the archive \
                     again without --sparse (-S)",
                    String::from_utf8_lossy(&e.path_bytes())
                ))))
            }
            T::GNULongName | T::GNULongLink | T::XHeader => Kind::Skip,
            other => {
                match other.as_byte() {
                    // GNU's dump directory is a directory with a listing as its data.
                    b'D' => Kind::Dir,
                    b'V' => Kind::Skip,
                    b'M' => return Err(fault(TarFault::Unsupported(
                        "this tar is one volume of a multi-volume set (tar -M); join the volumes \
                         into one archive first"
                            .into(),
                    ))),
                    // POSIX: an unknown type is read as a regular file.
                    _ => Kind::File,
                }
            }
        })
    }
}

// ─── Walk ─────────────────────────────────────────────────────────────────────

/// Receives a walk's files.
pub trait TarVisit {
    /// Whether to deliver `files[index]`. A skipped file is read past (a plain tar seeks).
    fn want(&mut self, index: usize, file: &TarFile) -> bool;
    fn begin(&mut self, file: &TarFile) -> io::Result<()>;
    fn data(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn end(&mut self) -> io::Result<()>;
    /// Bytes decoded only to be thrown away (a skipped file of a compressed tar).
    fn skipped(&mut self, _n: u64) {}
}

/// What [`tar_walk`] fails with when `cancel` is raised.
pub const TAR_WALK_CANCELLED: &str = "the tar decode was stopped";

/// One forward pass over the archive from `layout.files[start]` (a plain tar seeks there; any
/// other starts at the beginning, `start` must then be 0). Each file is checked against the
/// layout (path and size): an archive that changed since it was listed fails, never delivers
/// bytes under another name. `cancel` is polled every buffer.
pub fn tar_walk(
    path: &Path,
    layout: &TarLayout,
    start: usize,
    v: &mut dyn TarVisit,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let stopped = || io::Error::new(io::ErrorKind::Interrupted, TAR_WALK_CANCELLED);
    let offset = if layout.sequential() || start == 0 {
        0
    } else {
        match layout.files.get(start) {
            Some(f) => f.header_offset,
            None => return Ok(()),
        }
    };
    let mut opened = open_decoded(path, offset)?;
    if opened.codec != layout.codec {
        return Err(corrupt("the archive changed since it was listed"));
    }
    let mut buf = vec![0u8; 256 << 10];
    if layout.single {
        let Some(f) = layout.files.first() else {
            return Ok(());
        };
        if !v.want(0, f) {
            return Ok(());
        }
        let r = (|| -> io::Result<()> {
            v.begin(f)?;
            let mut total = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(stopped());
                }
                let n = match opened.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Tagged::decode(e)),
                };
                total += n as u64;
                if total > f.size {
                    return Err(corrupt("the file decodes to more than its frames say"));
                }
                v.data(&buf[..n])?;
            }
            if total != f.size {
                return Err(corrupt(format!(
                    "the file decoded to {total} of the {} bytes its frames say",
                    f.size
                )));
            }
            v.end()
        })();
        return r.map_err(|e| Tagged::finish(e, &opened));
    }
    let seq = layout.sequential();
    let r = (|| -> io::Result<()> {
        let mut ar = tar::Archive::new(&mut opened);
        let mut entries = entries(&mut ar, !seq).map_err(Tagged::decode)?;
        let mut i = start;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(stopped());
            }
            let Some(entry) = entries.next() else { break };
            let mut entry = entry.map_err(Tagged::decode)?;
            if !matches!(Kind::of(&entry)?, Kind::File) {
                continue;
            }
            let Some(f) = layout.files.get(i) else {
                return Err(corrupt(
                    "the archive holds more files than when it was listed",
                ));
            };
            let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            if sanitize_zip_entry(&name).as_deref() != Some(f.path.as_str())
                || entry.size() != f.size
            {
                return Err(corrupt(format!(
                    "the archive changed since it was listed ({name:?} where {:?} was)",
                    f.path
                )));
            }
            let index = i;
            i += 1;
            let want = v.want(index, f);
            if !want && !seq {
                continue; // the tar reader seeks past it
            }
            if want {
                v.begin(f)?;
            }
            let mut total = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(stopped());
                }
                let n = match entry.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Tagged::decode(e)),
                };
                total += n as u64;
                if want {
                    v.data(&buf[..n])?;
                } else {
                    v.skipped(n as u64);
                }
            }
            if total != f.size {
                return Err(corrupt(format!(
                    "{} ends after {total} of its {} bytes: the archive is cut short (a download \
                     that did not finish?)",
                    f.path, f.size
                )));
            }
            if want {
                v.end()?;
            }
        }
        if i != layout.files.len() {
            return Err(corrupt(format!(
                "the archive ended after {i} of its {} files: it is cut short (a download that \
                 did not finish?)",
                layout.files.len()
            )));
        }
        Ok(())
    })();
    r.map_err(|e| Tagged::finish(e, &opened))
}

/// Tells a decoder's error (explained against the file's read errors) from the visitor's own
/// (passed through untouched) inside one closure.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Tagged(io::Error);

impl Tagged {
    fn decode(e: io::Error) -> io::Error {
        io::Error::new(e.kind(), Tagged(e))
    }

    fn finish(e: io::Error, opened: &Opened) -> io::Error {
        if e.get_ref().is_some_and(|i| i.is::<Tagged>()) {
            let inner = e
                .into_inner()
                .expect("checked")
                .downcast::<Tagged>()
                .expect("checked");
            opened.explain(inner.0)
        } else {
            e
        }
    }
}

// ─── Preview and inspect ──────────────────────────────────────────────────────

/// Kept files after `excludes`, sorted, with their total: the job's plan and the UI tree.
pub fn tar_plan(layout: &TarLayout, excludes: &[String]) -> (u64, Vec<(String, u64)>) {
    let mut files: Vec<(String, u64)> = layout
        .kept()
        .filter(|f| {
            excludes.is_empty()
                || !crate::excludes::is_excluded_strings(Path::new(&f.path), excludes)
        })
        .map(|f| (f.path.clone(), f.size))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let total = files.iter().fold(0u64, |s, (_, n)| s.saturating_add(*n));
    (total, files)
}

/// The Upload card's preview of a tar or `.zst`: counts, sizes, the entry names and the game
/// (from the shallowest `sce_sys/param.json`). A compressed tar is decoded to list it (once:
/// the listing is cached for the upload); `on_progress(files_seen)` beats while it runs.
pub fn inspect_tar_with_progress(
    path: &Path,
    on_progress: &dyn Fn(u64),
) -> anyhow::Result<crate::transfer::ZipInspect> {
    use anyhow::Context;
    let layout = tar_layout_watched(
        path,
        TarWatch {
            on_files: Some(on_progress),
            ..TarWatch::default()
        },
    )
    .map_err(anyhow::Error::from)
    .with_context(|| format!("read {}", path.display()))?;
    Ok(inspect_from_layout(&layout))
}

pub fn inspect_from_layout(layout: &TarLayout) -> crate::transfer::ZipInspect {
    let mut names = Vec::new();
    let mut complete = true;
    let mut count = 0u64;
    for f in layout.kept() {
        count += 1;
        if names.len() < crate::transfer::ENTRY_NAMES_MAX {
            names.push(f.path.clone());
        } else {
            complete = false;
        }
    }
    let mut inspect = crate::transfer::ZipInspect {
        file_count: count,
        total_uncompressed: layout.unpacked_total(),
        compressed_size: layout.archive_size,
        title: None,
        title_id: None,
        content_id: None,
        application_category_type: None,
        game_root: None,
        entry_names: Some(names),
        entry_names_complete: complete,
    };
    if let Some((root, bytes)) = &layout.param_json {
        if let Ok(meta) = crate::game_meta::parse_param_json_bytes(bytes) {
            inspect.title = meta.title;
            inspect.title_id = meta.title_id;
            inspect.content_id = meta.content_id;
            inspect.application_category_type = meta.application_category_type;
            inspect.game_root = Some(root.clone());
        }
    }
    inspect
}

/// Per-path order of the kept files (the manifest maps to these by path).
pub fn kept_index(layout: &TarLayout) -> BTreeMap<&str, usize> {
    layout
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kept)
        .map(|(i, f)| (f.path.as_str(), i))
        .collect()
}

#[cfg(test)]
mod tests;
