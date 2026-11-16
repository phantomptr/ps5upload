//! A game archive (`.zip`, `.7z`, `.rar`, a multi-part RAR set from its first part) read in
//! place: its files listed, and each served in whatever order a caller asks, with nothing
//! unpacked to disk. Convert builds images and folders from it this way.
//!
//! How (the design PS5 Dump Forge's `ps5-dump-forge-archive::stream` proves): a session decodes
//! the archive front to back on its own thread and hands the asked-for entry's bytes over a
//! bounded queue, skipping the entries before it. A file the session has already gone past
//! starts a new session: from that file for a `.zip`, from its block for a `.7z` (blocks before
//! it are not decoded), from the start for a `.rar` (UnRAR walks forward only; a solid one
//! decodes everything before it again). What keeps a builder that reads in its own order off
//! that slow path:
//! - a short rest of an unfinished file is read through, and the session goes on;
//! - small files a solid stream (a `.7z` block, a `.rar`) decodes on its way to the asked-for
//!   one are kept in memory, up to [`KEEP_BUDGET`], until they are read;
//! - the first [`HEAD`] bytes of every file decoded from its start are kept, for the passes of
//!   header reads a build makes before it writes.
//!
//! Memory, beyond the decoder's own (a `.7z` block's dictionaries are refused past
//! [`DICT_LIMIT`]; a `.rar` decodes wherever the embedder's [`Extras`] decodes it, in the engine
//! the separate `ps5upload-unrar` program): [`QUEUE`] chunks of at most 64 KiB in flight
//! (4 MiB), [`KEEP_BUDGET`] of kept files, [`HEAD`] bytes per file, and the read the caller asked
//! for. On the PS5 (`ps5upload-convert`) that is at most 4 + 64 + 256 MiB: its [`DICT_LIMIT`] is
//! 256 MiB, and an LZMA/LZMA2 decoder holds its dictionary and a few KiB.
//!
//! `.rar` and zip entries compressed with zstd are not decoded here: the embedder installs
//! [`Extras`] for them ([`set_extras`]; the engine's `ps5upload-core` does). Without it both are
//! refused with a reason. UnRAR is never linked into a program that links this crate's users'
//! GPL code without its exception (LICENSES/UnRAR-exception.md).
//!
//! Archives are hostile input: every name is checked (no `..`, no absolute path, no empty or
//! over-long part, no file listed twice or used as a folder), links and special files are
//! refused, sizes are checked against what decodes (and every archive's own checksum), and no
//! allocation follows a size the archive declares without a bound. Passwords (RAR only) are
//! never logged, put in an error, or shown: [`Reader`] has no `Debug`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::names::{sanitize_7z_entry, sanitize_zip_entry, sevenz_check_layout};

/// Chunks in flight from a session to the reader (each at most [`CHUNK`]).
pub const QUEUE: usize = 64;
/// The most bytes one message carries.
const CHUNK: usize = 64 << 10;
/// The most of small files a solid stream decoded on its way that are kept until read.
pub const KEEP_BUDGET: u64 = 64 << 20;
/// The largest file kept that way.
pub const KEEP_ONE: u64 = 4 << 20;
/// The first bytes of every file kept once decoded (a header check reads 4 to 64 of them).
pub const HEAD: usize = 64;
/// The most of an unfinished file read through (and dropped) to go on to a later one, where a
/// new session would have to walk the whole archive again (a non-solid `.rar`).
const DRAIN_MAX: u64 = 64 << 20;
/// The same where a new session is cheap (a `.zip` seeks, a `.7z` starts at the file's block).
const DRAIN_CHEAP: u64 = 4 << 20;
/// The most memory a `.7z` block's coders may ask for together (LZMA/LZMA2/PPMd dictionaries).
/// 7-Zip's strongest preset uses 64 MiB; past this the archive is refused, not decoded. On the
/// PS5, whose converter keeps to a 512 MiB budget, 256 MiB.
pub const DICT_LIMIT: u64 = if cfg!(target_env = "ps5") {
    256 << 20
} else {
    1 << 30
};
/// Coders in one `.7z` block: 7-Zip writes at most ~7; a crafted chain would make the decoder
/// spin and overflow its stack.
const MAX_CODERS: usize = 16;
/// The most a `.7z` header (or an encoded header's declared size) may be.
const HEADER_LIMIT: u64 = 64 << 20;
/// The longest name of one file or folder in a path, in bytes (every filesystem's limit).
const NAME_MAX: usize = 255;

const ZIP_PASSWORD: &str = "this .zip is password-protected, which Convert can't open; re-pack it without a password, or as .rar";
const SEVENZ_PASSWORD: &str = "this .7z is password-protected, which Convert can't open; re-pack it without a password, or as .rar";

/// The archive kinds read in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    SevenZ,
    Rar,
}

impl Kind {
    /// The kind a file's name says: `.zip`, `.7z` or `.rar` (a multi-part RAR set is named by
    /// its first part). Tars and `.zst` files are not read in place.
    pub fn of(path: &Path) -> Option<Kind> {
        // By extension, as `ps5upload-core`'s `archive_kind` names them (no tar suffix ends in
        // one of these).
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "zip" => Some(Kind::Zip),
            "7z" => Some(Kind::SevenZ),
            "rar" => Some(Kind::Rar),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Zip => "zip",
            Kind::SevenZ => "7z",
            Kind::Rar => "rar",
        }
    }
}

/// One file of the archive: its checked path (`/`-separated, relative) and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
}

/// Where each file is, for a walk: what a session starts from.
enum Places {
    /// The zip index of each file.
    Zip(Vec<usize>),
    /// The parsed archive (read once), each file's block (`None`: no data), and the first file
    /// of each block, as an index into [`Listing::files`].
    SevenZ {
        archive: Arc<sevenz_rust2::Archive>,
        block: Vec<Option<usize>>,
        first: Vec<usize>,
    },
    Rar,
}

/// What an archive holds, in the order a walk meets it.
pub struct Listing {
    pub kind: Kind,
    /// Every file, in archive order.
    pub files: Vec<Entry>,
    /// The folders it lists (checked like the files).
    pub dirs: Vec<String>,
    /// A solid archive decodes every file before the one asked for.
    pub solid: bool,
    places: Places,
    /// Bytes of the files before each one (and the total last), for the cost of a walk.
    before: Vec<u64>,
}

impl Listing {
    /// Every file's bytes together.
    pub fn total(&self) -> u64 {
        self.before.last().copied().unwrap_or(0)
    }
}

/// The files and folders of the archive at `path`, read from its headers (a RAR file of unknown
/// size is decoded once to learn it). `password` is for a `.rar`; a protected `.zip` or `.7z`
/// is refused with the reason.
pub fn list(path: &Path, password: Option<&str>) -> Result<Listing> {
    let kind = Kind::of(path)
        .ok_or_else(|| anyhow!("{} is not a .zip, .7z or .rar archive", path.display()))?;
    let (files, dirs, solid, places) = match kind {
        Kind::Zip => zip_list(path)?,
        Kind::SevenZ => sevenz_list(path)?,
        Kind::Rar => rar_list(path, password)?,
    };
    validate(&files, &dirs)?;
    let mut before = Vec::with_capacity(files.len() + 1);
    let mut at = 0u64;
    for f in &files {
        before.push(at);
        at = at
            .checked_add(f.size)
            .ok_or_else(|| anyhow!("the archive lists more data than any disk holds"))?;
    }
    before.push(at);
    Ok(Listing {
        kind,
        files,
        dirs,
        solid,
        places,
        before,
    })
}

/// Names a game folder can hold: none twice, none both a file and a folder, no part empty,
/// `.`/`..`, or longer than [`NAME_MAX`].
fn validate(files: &[Entry], dirs: &[String]) -> Result<()> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(files.len());
    let mut parents: HashSet<&str> = HashSet::new();
    let check = |p: &str| -> Result<()> {
        if p.is_empty() || p.starts_with('/') {
            bail!("the archive has an unsafe entry path: {p:?}");
        }
        for part in p.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains(['\0', '\\']) {
                bail!("the archive has an unsafe entry path: {p:?}");
            }
            if part.len() > NAME_MAX {
                bail!("the archive has a name longer than {NAME_MAX} bytes: {p:?}");
            }
        }
        Ok(())
    };
    for f in files {
        check(&f.path)?;
        if !seen.insert(&f.path) {
            bail!("the archive lists {} twice", f.path);
        }
        let mut at = f.path.as_str();
        while let Some((parent, _)) = at.rsplit_once('/') {
            parents.insert(parent);
            at = parent;
        }
    }
    for d in dirs {
        check(d)?;
    }
    for p in &parents {
        if seen.contains(p) {
            bail!("the archive has {p} as both a file and a folder");
        }
    }
    for d in dirs {
        if seen.contains(d.as_str()) {
            bail!("the archive has {d} as both a file and a folder");
        }
    }
    Ok(())
}

// ── what the embedder adds ─────────────────────────────────────────────────────────────────

/// Receives the files a `.rar` walk delivers (`ps5upload-core`'s `RarWalkSink`, method for
/// method).
pub trait RarSink {
    /// Called for every file in header order; false stops the walk.
    fn visit(&mut self, ordinal: u64, path: &str) -> bool;
    /// Whether to deliver the file at archive-order ordinal `ordinal` (false: skip it).
    fn want(&mut self, ordinal: u64, path: &str, size: u64) -> bool;
    fn begin(&mut self, path: &str) -> std::io::Result<()>;
    fn data(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn end(&mut self) -> std::io::Result<()>;
    /// `n` more bytes were decoded only to be discarded.
    fn skipped(&mut self, _n: u64) {}
}

/// Why a `.rar` walk ended early.
#[derive(Debug)]
pub enum RarFail {
    /// The halt flag was raised.
    Cancelled,
    /// The sink refused a call (it keeps its own reason).
    Sink,
    /// The archive (damaged, a wrong password, a missing volume: the message says).
    Failed(String),
}

/// What this crate leaves to the program it is in: `.rar` archives (UnRAR) and zip entries
/// compressed with zstd. The defaults refuse both with a reason; the engine's `ps5upload-core`
/// installs its own ([`set_extras`]).
pub trait Extras: Send + Sync {
    /// A `.rar`'s files in archive order, its folders, and whether it is solid.
    fn rar_list(
        &self,
        _path: &Path,
        _password: Option<&str>,
    ) -> Result<(Vec<Entry>, Vec<String>, bool)> {
        bail!("{RAR_NOT_HERE}")
    }

    /// One forward pass over a `.rar`, its files handed to `sink`; `halt` stops it.
    fn rar_walk(
        &self,
        _path: &Path,
        _password: Option<&str>,
        _sink: &mut dyn RarSink,
        _halt: &AtomicBool,
    ) -> std::result::Result<(), RarFail> {
        Err(RarFail::Failed(RAR_NOT_HERE.into()))
    }

    /// A zip entry's raw bytes (method 93, zstd) decoded, its size and CRC-32 checked.
    fn zip_zstd<'a>(
        &self,
        _raw: Box<dyn Read + 'a>,
        _crc: u32,
        _size: u64,
    ) -> Result<Box<dyn Read + 'a>> {
        bail!("{ZIP_ZSTD_NOT_HERE}")
    }
}

/// Why a `.rar` is refused where no [`Extras`] reads one.
pub const RAR_NOT_HERE: &str =
    "RAR archives can't be read here; unpack it on a computer, or re-pack it as .zip or .7z";
/// Why a zip entry compressed with zstd is refused where no [`Extras`] decodes one.
pub const ZIP_ZSTD_NOT_HERE: &str = "this .zip has files compressed with zstd, which can't be read here; unpack it on a computer, or re-pack it with deflate";

struct NoExtras;
impl Extras for NoExtras {}

static EXTRAS: std::sync::OnceLock<Box<dyn Extras>> = std::sync::OnceLock::new();

/// Installs what this program adds for `.rar` and zstd zip entries. The first call wins; later
/// ones (the same embedder again) change nothing.
pub fn set_extras(extras: Box<dyn Extras>) {
    let _ = EXTRAS.set(extras);
}

fn extras() -> &'static dyn Extras {
    EXTRAS.get().map_or(&NoExtras, |e| e.as_ref())
}

// ── zip ─────────────────────────────────────────────────────────────────────────────────────

fn open_zip(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    zip::ZipArchive::new(BufReader::new(f)).map_err(|e| anyhow!("read zip {}: {e}", path.display()))
}

fn zip_name(e: &zip::read::ZipFile<'_, BufReader<File>>) -> String {
    let decoded = e
        .name()
        .map(|n| n.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(e.name_raw()).into_owned());
    crate::names::zip_entry_name(e.name_raw(), &decoded)
}

type Listed = (Vec<Entry>, Vec<String>, bool, Places);

fn zip_list(path: &Path) -> Result<Listed> {
    let mut z = open_zip(path)?;
    let (mut files, mut dirs, mut index) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..z.len() {
        let e = z
            .by_index_raw(i)
            .map_err(|e| anyhow!("read zip entry {i}: {e}"))?;
        if e.encrypted() {
            bail!("{ZIP_PASSWORD}");
        }
        let name = zip_name(&e);
        let method = e.compression();
        if !matches!(
            method,
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        ) && method != zip::CompressionMethod::ZSTD
        {
            bail!("this .zip uses {method} compression, which Convert can't read; re-pack it with Deflate (the usual zip)");
        }
        if e.is_symlink() {
            bail!("the archive has a link ({name}), which a game folder can't hold");
        }
        let Some(rel) = sanitize_zip_entry(&name) else {
            if e.is_dir() && name.trim_matches('/').is_empty() {
                continue;
            }
            bail!("the archive has an unsafe entry path: {name:?}");
        };
        if name.starts_with('/') {
            bail!("the archive has an unsafe entry path: {name:?}");
        }
        if e.is_dir() {
            dirs.push(rel);
        } else {
            files.push(Entry {
                path: rel,
                size: e.size(),
            });
            index.push(i);
        }
    }
    Ok((files, dirs, false, Places::Zip(index)))
}

// ── 7z ──────────────────────────────────────────────────────────────────────────────────────

/// The memory a coder asks for, as sevenz-rust2 reads its properties.
fn dictionary(id: &[u8], props: &[u8]) -> Option<u64> {
    match id {
        // LZMA and PPMd: a little-endian u32 after the first byte.
        [0x03, 0x01, 0x01] | [0x03, 0x04, 0x01] => props
            .get(1..5)
            .and_then(|d| <[u8; 4]>::try_from(d).ok())
            .map(|d| u64::from(u32::from_le_bytes(d))),
        [0x21] => match *props.first()? {
            bits @ 0..40 => Some(u64::from(2 | (bits & 1)) << (bits / 2 + 11)),
            40 => Some(u64::from(u32::MAX)),
            _ => None,
        },
        _ => None,
    }
}

fn byte(b: &mut &[u8]) -> Option<u8> {
    let (&first, rest) = b.split_first()?;
    *b = rest;
    Some(first)
}

fn take<'a>(b: &mut &'a [u8], n: u64) -> Option<&'a [u8]> {
    let n = usize::try_from(n).ok().filter(|&n| n <= b.len())?;
    let (head, rest) = b.split_at(n);
    *b = rest;
    Some(head)
}

/// 7z's variable-length number: leading one bits in the first byte count the extra bytes.
fn number(b: &mut &[u8]) -> Option<u64> {
    let first = byte(b)?;
    let mut value = 0u64;
    for i in 0..8 {
        let mask = 0x80u8 >> i;
        if first & mask == 0 {
            return Some(value | (u64::from(first & (mask - 1)) << (8 * i)));
        }
        value |= u64::from(byte(b)?) << (8 * i);
    }
    Some(value)
}

/// An encoded header's StreamsInfo (after its 0x17): `Some(true)` when an unpack size or a
/// coder's dictionary is over [`HEADER_LIMIT`], `None` for a structure sevenz-rust2 judges.
/// sevenz-rust2 decodes an encoded header to its declared size with no limit, so a small
/// crafted `.7z` could otherwise expand to gigabytes before any check. (From PS5 Dump Forge.)
fn encoded_over(mut b: &[u8]) -> Option<bool> {
    let b = &mut b;
    let mut nid = byte(b)?;
    if nid == 0x06 {
        number(b)?;
        let count = number(b)?;
        nid = byte(b)?;
        if nid == 0x09 {
            for _ in 0..count {
                number(b)?;
            }
            nid = byte(b)?;
        }
        if nid == 0x0a {
            let defined = if byte(b)? != 0 {
                count
            } else {
                let bits = take(b, count.div_ceil(8))?;
                (0..count)
                    .filter(|i| {
                        bits.get((i / 8) as usize)
                            .is_some_and(|x| x & (0x80 >> (i % 8)) != 0)
                    })
                    .count() as u64
            };
            take(b, defined.checked_mul(4)?)?;
            nid = byte(b)?;
        }
        if nid != 0 {
            return None;
        }
        nid = byte(b)?;
    }
    if nid != 0x07 || byte(b)? != 0x0b {
        return None;
    }
    let blocks = number(b)?;
    if byte(b)? != 0 {
        return None;
    }
    let mut outs = Vec::new();
    for _ in 0..blocks {
        let (mut ins, mut out) = (0u64, 0u64);
        let coders = number(b)?;
        if coders > MAX_CODERS as u64 {
            return Some(true);
        }
        for _ in 0..coders {
            let flags = byte(b)?;
            let id = take(b, u64::from(flags & 0x0f))?;
            if flags & 0x10 == 0 {
                ins = ins.checked_add(1)?;
                out = out.checked_add(1)?;
            } else {
                ins = ins.checked_add(number(b)?)?;
                out = out.checked_add(number(b)?)?;
            }
            if flags & 0x20 != 0 {
                let n = number(b)?;
                let props = take(b, n)?;
                if dictionary(id, props).is_some_and(|d| d > HEADER_LIMIT) {
                    return Some(true);
                }
            }
            if flags & 0x80 != 0 {
                return None;
            }
        }
        let pairs = out.checked_sub(1)?;
        for _ in 0..pairs {
            number(b)?;
            number(b)?;
        }
        let packed = ins.checked_sub(pairs)?;
        if packed > 1 {
            for _ in 0..packed {
                number(b)?;
            }
        }
        if outs.len() >= 1 << 16 {
            return Some(true);
        }
        outs.push(out);
    }
    if byte(b)? != 0x0c {
        return None;
    }
    for out in outs {
        for _ in 0..out {
            if number(b)? > HEADER_LIMIT {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// The start header and an encoded header, checked before sevenz-rust2 reads the archive: a
/// truncated archive (a download cut short) says so, and an oversized header is refused.
fn check_7z_header(source: &mut (impl Read + Seek), len: u64) -> Result<()> {
    let mut h = [0u8; 32];
    if len < 32 {
        bail!("this .7z is truncated (only {len} bytes)");
    }
    source.seek(SeekFrom::Start(0))?;
    source.read_exact(&mut h)?;
    if h[..6] != [b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c] {
        bail!("this is not a .7z archive");
    }
    if h[8..].iter().all(|&b| b == 0) {
        bail!("this .7z is incomplete: it was not finished being written or downloaded");
    }
    let field = |at: usize| -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&h[at..at + 8]);
        u64::from_le_bytes(b)
    };
    let crc = u32::from_le_bytes([h[8], h[9], h[10], h[11]]);
    if crc32fast::hash(&h[12..]) != crc {
        bail!("this .7z is damaged: its start header does not match its checksum");
    }
    let (offset, size) = (field(12), field(20));
    let end = offset
        .checked_add(32)
        .and_then(|n| n.checked_add(size))
        .filter(|&n| n <= len);
    let Some(end) = end else {
        bail!("this .7z is truncated: it ends before its header (a download cut short?)");
    };
    if size > HEADER_LIMIT {
        bail!("this .7z's header is too large to read");
    }
    let mut header = vec![0u8; size as usize];
    source.seek(SeekFrom::Start(end - size))?;
    source.read_exact(&mut header)?;
    match header.split_first() {
        Some((0x17, rest)) if encoded_over(rest) == Some(true) => {
            bail!("this .7z's header is too large to read")
        }
        _ => Ok(()),
    }
}

fn seven_err(path: &Path, e: sevenz_rust2::Error) -> anyhow::Error {
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    if matches!(
        e,
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_)
    ) || lower.contains("password")
        || lower.contains("aes")
    {
        anyhow!("{SEVENZ_PASSWORD}")
    } else {
        anyhow!("read 7z {}: {msg}", path.display())
    }
}

/// The parsed archive, checked: every block's coders known and within [`MAX_CODERS`] and
/// [`DICT_LIMIT`], nothing encrypted, the layout the walk can follow.
fn sevenz_open(path: &Path) -> Result<sevenz_rust2::Archive> {
    use sevenz_rust2::EncoderMethod as M;
    let mut src =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let len = src.get_ref().metadata()?.len();
    check_7z_header(&mut src, len)?;
    src.seek(SeekFrom::Start(0))?;
    let archive = sevenz_rust2::Archive::read(&mut src, &sevenz_rust2::Password::from(""))
        .map_err(|e| seven_err(path, e))?;
    sevenz_check_layout(&archive)?;
    for block in &archive.blocks {
        if block.coders.len() > MAX_CODERS {
            bail!(
                "this .7z chains {} methods in one block, which Convert can't read",
                block.coders.len()
            );
        }
        let mut memory = 0u64;
        for coder in &block.coders {
            let id = coder.encoder_method_id();
            if id == M::ID_AES256_SHA256 {
                bail!("{SEVENZ_PASSWORD}");
            }
            memory = memory.saturating_add(dictionary(id, coder.properties()).unwrap_or(0));
        }
        if memory > DICT_LIMIT {
            bail!(
                "this .7z needs a {} MiB decompression dictionary (over {} MiB); re-pack it with a smaller one",
                memory.div_ceil(1 << 20),
                DICT_LIMIT >> 20
            );
        }
    }
    Ok(archive)
}

fn sevenz_list(path: &Path) -> Result<Listed> {
    let archive = sevenz_open(path)?;
    let fbi = &archive.stream_map.file_block_index;
    // A walk's order: the files of each block in turn, then those with no data.
    let mut order: Vec<usize> = (0..archive.files.len()).collect();
    order.sort_by_key(|&i| match fbi.get(i).copied().flatten() {
        Some(b) => (0u8, b, i),
        None => (1u8, usize::MAX, i),
    });
    let (mut files, mut dirs, mut block) = (Vec::new(), Vec::new(), Vec::new());
    let mut first = vec![usize::MAX; archive.blocks.len()];
    for i in order {
        let e = &archive.files[i];
        if e.is_anti_item || e.windows_attributes & 0x400 != 0 {
            bail!(
                "the archive has a link or a deletion marker ({}), which a game folder can't hold",
                e.name
            );
        }
        if e.has_windows_attributes {
            let mode = e.windows_attributes >> 16;
            let kind = mode & 0o170000;
            if kind != 0 && kind != if e.is_directory { 0o040000 } else { 0o100000 } {
                bail!(
                    "the archive has a link or special file ({}), which a game folder can't hold",
                    e.name
                );
            }
        }
        let Some(rel) = sanitize_7z_entry(&e.name) else {
            if e.is_directory && e.name.replace('\\', "/").trim_matches('/').is_empty() {
                continue;
            }
            bail!("the archive has an unsafe entry path: {:?}", e.name);
        };
        if e.name.starts_with(['/', '\\']) {
            bail!("the archive has an unsafe entry path: {:?}", e.name);
        }
        if e.is_directory {
            dirs.push(rel);
            continue;
        }
        let b = fbi.get(i).copied().flatten().filter(|_| e.has_stream);
        if let Some(b) = b {
            if let Some(f) = first.get_mut(b) {
                if *f == usize::MAX {
                    *f = files.len();
                }
            }
        }
        files.push(Entry {
            path: rel,
            size: if e.has_stream { e.size } else { 0 },
        });
        block.push(b);
    }
    let solid = archive.is_solid;
    Ok((
        files,
        dirs,
        solid,
        Places::SevenZ {
            archive: Arc::new(archive),
            block,
            first,
        },
    ))
}

// ── rar ─────────────────────────────────────────────────────────────────────────────────────

fn rar_list(path: &Path, password: Option<&str>) -> Result<Listed> {
    let (files, dirs, solid) = extras().rar_list(path, password)?;
    Ok((files, dirs, solid, Places::Rar))
}

// ── reading ─────────────────────────────────────────────────────────────────────────────────

/// Why a read failed: stopped by the caller, or the archive (damaged, changed, a wrong
/// password: the message says).
#[derive(Debug)]
pub enum ReadError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Cancelled => f.write_str("cancelled"),
            ReadError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ReadError {}

fn failed(m: impl Into<String>) -> ReadError {
    ReadError::Failed(m.into())
}

/// Whether the caller wants reads stopped.
pub type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

/// Called on the reading thread when a session starts; what it returns runs on the session's
/// thread first and then once per chunk (the engine enters the job's lane and follows its
/// priority switch there).
pub type Lane = Arc<dyn Fn() -> Box<dyn FnMut() + Send> + Send + Sync>;

/// Small files decoded on the way and kept until read, and the files read to their end.
#[derive(Default)]
struct Kept {
    files: HashMap<usize, Vec<u8>>,
    bytes: u64,
    done: HashSet<usize>,
}

/// Reads the files of one archive, a session at a time. No `Debug`: it holds the password.
pub struct Reader {
    path: PathBuf,
    password: Option<String>,
    listing: Arc<Listing>,
    index: HashMap<String, usize>,
    stop: Stop,
    lane: Option<Lane>,
    session: Option<Session>,
    heads: HashMap<usize, Vec<u8>>,
    kept: Arc<Mutex<Kept>>,
    sessions: u64,
}

enum Msg {
    Chunk(Vec<u8>),
    End,
    /// The walk reached the archive's end without the asked-for file.
    Missing,
    Failed(String),
}

struct Session {
    target: Option<mpsc::SyncSender<usize>>,
    data: Option<mpsc::Receiver<Msg>>,
    halt: Arc<AtomicBool>,
    /// The file being served, its size, the bytes of it handed out, and whether its end (and
    /// the archive's own check of it) has come.
    current: usize,
    size: u64,
    at: u64,
    ended: bool,
    head: Vec<u8>,
    pending: Vec<u8>,
    pending_at: usize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Both queues closed: a walk waiting on either stops, then lets go of the archive (and
        // of the RAR helper) before the next session opens it.
        self.halt.store(true, Ordering::Relaxed);
        self.target = None;
        self.data = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Reader {
    /// `listing` is [`list`]'s for the same archive and password; `stop` ends a read that
    /// waits on the archive (a long skip through solid data) with [`ReadError::Cancelled`].
    pub fn new(path: &Path, password: Option<&str>, listing: Listing, stop: Stop) -> Reader {
        let index = (listing.files.iter().enumerate())
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        Reader {
            path: path.to_path_buf(),
            password: password.map(str::to_owned),
            listing: Arc::new(listing),
            index,
            stop,
            lane: None,
            session: None,
            heads: HashMap::new(),
            kept: Arc::default(),
            sessions: 0,
        }
    }

    /// See [`Lane`].
    pub fn with_lane(mut self, lane: Lane) -> Reader {
        self.lane = Some(lane);
        self
    }

    pub fn listing(&self) -> &Listing {
        &self.listing
    }

    /// Sessions started so far (each a walk from the archive's start, a file or a block).
    pub fn sessions(&self) -> u64 {
        self.sessions
    }

    /// The bytes kept now (small files decoded on the way).
    pub fn kept_bytes(&self) -> u64 {
        self.kept.lock().unwrap_or_else(|e| e.into_inner()).bytes
    }

    /// Whether a session (a walk and its decoder) is open now.
    pub fn reading(&self) -> bool {
        self.session.is_some()
    }

    /// Ends the session (its walk, the decoder and its dictionary) and lets go of the files kept
    /// on the way and the heads served: before the archive is opened again, so two decoders are
    /// never held at once. A read after it starts a new session.
    pub fn release(&mut self) {
        // Joins the walk's thread: nothing else touches what it kept after this.
        self.session = None;
        *self.kept.lock().unwrap_or_else(|e| e.into_inner()) = Kept::default();
        self.heads = HashMap::new();
    }

    /// Up to `len` bytes of `path` from `offset`; fewer only at the file's end.
    pub fn read_at(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, ReadError> {
        let Some(&idx) = self.index.get(path) else {
            return Err(failed(format!("{path} is not in the archive")));
        };
        if (self.stop)() {
            self.session = None;
            return Err(ReadError::Cancelled);
        }
        let size = self.listing.files[idx].size;
        let end = offset.saturating_add(len as u64).min(size);
        if offset >= end {
            return Ok(Vec::new());
        }
        if let Some(bytes) = self.take_kept(idx, offset, end) {
            return Ok(bytes);
        }
        if let Some(h) = self.heads.get(&idx) {
            if end <= h.len() as u64 {
                return Ok(h[offset as usize..end as usize].to_vec());
            }
        }
        let r = self.try_read(idx, offset, (end - offset) as usize);
        if r.is_err() {
            self.session = None;
        }
        r
    }

    /// A kept file's bytes; read to its end, it is let go.
    fn take_kept(&mut self, idx: usize, offset: u64, end: u64) -> Option<Vec<u8>> {
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = kept.files.get(&idx)?;
        let out = bytes.get(offset as usize..end as usize)?.to_vec();
        if end >= bytes.len() as u64 {
            if let Some(gone) = kept.files.remove(&idx) {
                kept.bytes = kept.bytes.saturating_sub(gone.len() as u64);
            }
            kept.done.insert(idx);
        }
        Some(out)
    }

    fn try_read(&mut self, idx: usize, offset: u64, len: usize) -> Result<Vec<u8>, ReadError> {
        let path = self.listing.files[idx].path.clone();
        let mut fresh = false;
        loop {
            if !self.moved_to(idx, offset)? {
                if fresh {
                    return Err(failed(format!(
                        "{path} is not in the archive where its listing says"
                    )));
                }
                self.start(idx)?;
                fresh = true;
                continue;
            }
            let Some(s) = self.session.as_mut() else {
                return Err(failed("the archive reader stopped"));
            };
            let mut out = Vec::new();
            out.try_reserve_exact(len)
                .map_err(|_| failed(format!("not enough memory to read {len} bytes of {path}")))?;
            match s.fill(&mut out, len, &mut self.heads, &self.stop)? {
                true => {
                    // At the file's end, its end comes first: the archive's check of it fails
                    // the read that completes it, not a later one nobody makes.
                    if s.at >= s.size && !s.ended {
                        let mut more = Vec::new();
                        if !s.fill(&mut more, 1, &mut self.heads, &self.stop)? || !s.ended {
                            return Err(failed(format!("{path} is longer than the archive lists")));
                        }
                    }
                    if out.len() < len {
                        return Err(failed(format!("{path} is shorter than the archive lists")));
                    }
                    if s.ended {
                        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
                        kept.done.insert(idx);
                    }
                    return Ok(out);
                }
                // A walk that missed it: once more, from where a new session starts.
                false if !fresh => {
                    self.start(idx)?;
                    fresh = true;
                }
                false => return Err(failed(format!("{path} is not in the archive"))),
            }
        }
    }

    /// Bytes the walk decodes past the current file's end before it reaches `to` in the same
    /// block of a `.7z`: the rest of that block.
    fn rest_of_block(&self, current: usize) -> u64 {
        let Places::SevenZ { block, .. } = &self.listing.places else {
            return 0;
        };
        let b = block.get(current).copied().flatten();
        let mut end = current + 1;
        while end < block.len() && block[end] == b && b.is_some() {
            end += 1;
        }
        self.listing.before[end].saturating_sub(self.listing.before[current + 1])
    }

    /// Whether the session should read on to `to` (rather than a new session start there),
    /// `rest` bytes of the current file left.
    fn go_on(&self, current: usize, to: usize, rest: u64) -> bool {
        match &self.listing.places {
            // A new session seeks straight to it.
            Places::Zip(_) => rest <= DRAIN_CHEAP,
            Places::SevenZ { block, .. } => {
                let same = block.get(current) == block.get(to) && block.get(to).is_some();
                // In the same block everything before it decodes either way; in another, a
                // new session starts at that block.
                same || rest.saturating_add(self.rest_of_block(current)) <= DRAIN_CHEAP
            }
            // A solid stream decodes everything before it either way; a new session walks
            // every header again.
            Places::Rar => self.listing.solid || rest <= DRAIN_MAX,
        }
    }

    /// Whether the session now serves `idx` from `offset`; false when a new one must.
    fn moved_to(&mut self, idx: usize, offset: u64) -> Result<bool, ReadError> {
        let Some(s) = self.session.as_ref() else {
            return Ok(false);
        };
        let (current, rest, ended) = (s.current, s.size.saturating_sub(s.at), s.ended);
        if current == idx {
            let Some(s) = self.session.as_mut() else {
                return Ok(false);
            };
            if s.at > offset || (s.ended && offset >= s.at) {
                return Ok(false);
            }
            // Ahead in this file: read up to it.
            let mut skip = offset - s.at;
            while skip > 0 && !s.ended {
                let step = skip.min(1 << 20) as usize;
                let mut gone = Vec::with_capacity(step);
                if !s.fill(&mut gone, step, &mut self.heads, &self.stop)? {
                    return Ok(false);
                }
                if gone.is_empty() {
                    break;
                }
                skip -= gone.len() as u64;
            }
            return Ok(skip == 0);
        }
        // A file the walk went past (or one before where this session started).
        if idx < current {
            return Ok(false);
        }
        if !self.go_on(current, idx, if ended { 0 } else { rest }) {
            return Ok(false);
        }
        let Some(s) = self.session.as_mut() else {
            return Ok(false);
        };
        while !s.ended {
            let left = s.size.saturating_sub(s.at);
            let step = left.clamp(1, 1 << 20) as usize;
            let mut gone = Vec::with_capacity(step);
            if !s.fill(&mut gone, step, &mut self.heads, &self.stop)? {
                return Ok(false);
            }
            if s.at > s.size {
                return Err(failed(format!(
                    "{} is longer than the archive lists",
                    self.listing.files[s.current].path
                )));
            }
        }
        if !s.target.as_ref().is_some_and(|t| t.send(idx).is_ok()) {
            return Ok(false);
        }
        s.current = idx;
        s.size = self.listing.files[idx].size;
        s.at = 0;
        s.ended = false;
        s.head.clear();
        self.moved_to(idx, offset)
    }

    /// A new session, serving `idx` first.
    fn start(&mut self, idx: usize) -> Result<(), ReadError> {
        self.session = None;
        self.sessions += 1;
        let halt = Arc::new(AtomicBool::new(false));
        let (target_tx, target_rx) = mpsc::sync_channel::<usize>(1);
        let (data_tx, data_rx) = mpsc::sync_channel::<Msg>(QUEUE);
        let mut tick: Box<dyn FnMut() + Send> = match &self.lane {
            Some(lane) => lane(),
            None => Box::new(|| {}),
        };
        let (path, password, listing, kept) = (
            self.path.clone(),
            self.password.clone(),
            self.listing.clone(),
            self.kept.clone(),
        );
        let feed_halt = halt.clone();
        let thread = std::thread::Builder::new()
            .name("archive-read".into())
            .spawn(move || {
                tick();
                let mut feed = Feed {
                    targets: target_rx,
                    data: data_tx,
                    target: Some(idx),
                    halt: feed_halt,
                    listing: listing.clone(),
                    kept,
                    keeping: None,
                    serving: false,
                    may_keep: !matches!(listing.places, Places::Zip(_)),
                    tick,
                };
                let walked = walk(&path, password.as_deref(), &listing, idx, &mut feed);
                // A file being kept when the walk ended gives its reservation back.
                feed.release();
                let last = match walked {
                    Err(Walk::Fail(e)) => Msg::Failed(e),
                    Err(Walk::Stop) => return,
                    Ok(()) if feed.target.is_some() => Msg::Missing,
                    Ok(()) => return,
                };
                let _ = feed.data.send(last);
            })
            .map_err(|e| failed(format!("could not start the archive reader: {e}")))?;
        self.session = Some(Session {
            target: Some(target_tx),
            data: Some(data_rx),
            halt,
            current: idx,
            size: self.listing.files[idx].size,
            at: 0,
            ended: false,
            head: Vec::new(),
            pending: Vec::new(),
            pending_at: 0,
            thread: Some(thread),
        });
        Ok(())
    }
}

impl Session {
    /// Appends up to `len` bytes of the current file to `out`, fewer at its end, keeping its
    /// head in `heads` once known. False: the walk went to the archive's end without the file.
    fn fill(
        &mut self,
        out: &mut Vec<u8>,
        len: usize,
        heads: &mut HashMap<usize, Vec<u8>>,
        stop: &Stop,
    ) -> Result<bool, ReadError> {
        while out.len() < len && !self.ended {
            if stop() {
                return Err(ReadError::Cancelled);
            }
            if self.pending_at == self.pending.len() {
                let Some(data) = self.data.as_ref() else {
                    return Err(failed("the archive reader stopped"));
                };
                match data.recv_timeout(Duration::from_millis(100)) {
                    Ok(Msg::Chunk(b)) => {
                        self.pending = b;
                        self.pending_at = 0;
                    }
                    Ok(Msg::End) => {
                        self.ended = true;
                        // A short file whole, now that the archive has checked it.
                        if self.head.len() as u64 == self.size && self.size <= HEAD as u64 {
                            heads.insert(self.current, self.head.clone());
                        }
                    }
                    Ok(Msg::Missing) => return Ok(false),
                    Ok(Msg::Failed(e)) => return Err(ReadError::Failed(e)),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    // A walk that ended at the archive's end before it took this file's
                    // request: as missing (a new session looks again). Part way into the
                    // file, the walk is broken.
                    Err(mpsc::RecvTimeoutError::Disconnected) if self.at == 0 => {
                        return Ok(false);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(failed("the archive reader stopped"));
                    }
                }
                continue;
            }
            let bytes = &self.pending[self.pending_at..];
            let n = (len - out.len()).min(bytes.len());
            out.extend_from_slice(&bytes[..n]);
            if self.head.len() < HEAD && self.at == self.head.len() as u64 {
                let k = n.min(HEAD - self.head.len());
                self.head.extend_from_slice(&bytes[..k]);
                // Bytes short of the file's end need no check of it to be served again.
                if self.size > HEAD as u64 {
                    heads.insert(self.current, self.head.clone());
                }
            }
            self.pending_at += n;
            self.at += n as u64;
        }
        Ok(true)
    }
}

// ── the walk (the session's thread) ─────────────────────────────────────────────────────────

/// Why a walk ended early.
enum Walk {
    /// The reader went away (a new session, a stop): nothing to report.
    Stop,
    Fail(String),
}

/// What the walk does with one file.
#[derive(PartialEq, Eq)]
enum Take {
    Serve,
    Keep,
    Skip,
}

/// The session's side: serves each asked-for file, keeps small ones decoded on the way, skips
/// the rest.
struct Feed {
    targets: mpsc::Receiver<usize>,
    data: mpsc::SyncSender<Msg>,
    target: Option<usize>,
    halt: Arc<AtomicBool>,
    listing: Arc<Listing>,
    kept: Arc<Mutex<Kept>>,
    /// A file being kept: its index and bytes so far (its size is reserved in the budget).
    keeping: Option<(usize, Vec<u8>)>,
    serving: bool,
    /// Whether passing a small file is worth decoding it to keep (not for a `.zip`, whose new
    /// sessions seek).
    may_keep: bool,
    tick: Box<dyn FnMut() + Send>,
}

impl Feed {
    fn halted(&self) -> bool {
        self.halt.load(Ordering::Relaxed)
    }

    /// The file asked for, waiting for the reader to ask; `None` once the reader is gone.
    fn target(&mut self) -> Option<usize> {
        if self.target.is_none() && !self.halted() {
            self.target = self.targets.recv().ok();
        }
        self.target
    }

    /// Whether a block holding files `from..to` holds the one asked for (a `.7z` block that
    /// doesn't is not decoded).
    fn block_wanted(&mut self, from: usize, to: usize) -> Result<bool, Walk> {
        let t = self.target().ok_or(Walk::Stop)?;
        Ok((from..to).contains(&t))
    }

    fn begin(&mut self, idx: usize) -> Result<Take, Walk> {
        let t = self.target().ok_or(Walk::Stop)?;
        if t == idx {
            self.serving = true;
            return Ok(Take::Serve);
        }
        let size = self.listing.files.get(idx).map_or(0, |f| f.size);
        if self.may_keep && size > 0 && size <= KEEP_ONE && t > idx {
            let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            let fits = kept.bytes.saturating_add(size) <= KEEP_BUDGET;
            if fits && !kept.done.contains(&idx) && !kept.files.contains_key(&idx) {
                kept.bytes += size;
                drop(kept);
                let mut bytes = Vec::new();
                if bytes.try_reserve_exact(size as usize).is_ok() {
                    self.keeping = Some((idx, bytes));
                    return Ok(Take::Keep);
                }
                self.release_size(size);
            }
        }
        Ok(Take::Skip)
    }

    fn release_size(&self, size: u64) {
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        kept.bytes = kept.bytes.saturating_sub(size);
    }

    /// A file being kept is given up (its reservation freed).
    fn release(&mut self) {
        if let Some((idx, _)) = self.keeping.take() {
            let size = self.listing.files.get(idx).map_or(0, |f| f.size);
            self.release_size(size);
        }
    }

    fn chunk(&mut self, b: &[u8]) -> Result<(), Walk> {
        (self.tick)();
        if self.halted() {
            return Err(Walk::Stop);
        }
        if let Some((_, bytes)) = &mut self.keeping {
            bytes.extend_from_slice(b);
            return Ok(());
        }
        if self.serving {
            for piece in b.chunks(CHUNK) {
                self.data
                    .send(Msg::Chunk(piece.to_vec()))
                    .map_err(|_| Walk::Stop)?;
            }
        }
        Ok(())
    }

    /// The file `idx` has ended, its size and the archive's check passed.
    fn end(&mut self, idx: usize) -> Result<(), Walk> {
        if let Some((k, bytes)) = self.keeping.take() {
            if k == idx {
                let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
                kept.files.insert(idx, bytes);
            } else {
                let size = self.listing.files.get(k).map_or(0, |f| f.size);
                self.release_size(size);
            }
            return Ok(());
        }
        if self.serving {
            self.serving = false;
            self.target = None;
            self.data.send(Msg::End).map_err(|_| Walk::Stop)?;
        }
        Ok(())
    }
}

/// One file's bytes from `reader` through `feed`: all of them (a `.7z` block must be read
/// through), checked against the listed size.
fn pump(feed: &mut Feed, idx: usize, take: &Take, reader: &mut dyn Read) -> Result<(), Walk> {
    let size = feed.listing.files.get(idx).map_or(0, |f| f.size);
    let path = feed
        .listing
        .files
        .get(idx)
        .map_or("", |f| f.path.as_str())
        .to_string();
    let mut buf = vec![0u8; CHUNK];
    let mut got = 0u64;
    loop {
        if feed.halted() {
            return Err(Walk::Stop);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Walk::Fail(format!("{path}: {e}"))),
        };
        got = got.saturating_add(n as u64);
        if got > size {
            return Err(Walk::Fail(format!(
                "{path} is longer than the archive lists"
            )));
        }
        if *take != Take::Skip {
            feed.chunk(&buf[..n])?;
        } else {
            (feed.tick)();
        }
    }
    if got != size {
        return Err(Walk::Fail(format!(
            "{path} is {got} bytes, not the {size} the archive lists"
        )));
    }
    if *take != Take::Skip {
        feed.end(idx)?;
    }
    Ok(())
}

/// One walk from where a session for file `first` starts.
fn walk(
    path: &Path,
    password: Option<&str>,
    listing: &Arc<Listing>,
    first: usize,
    feed: &mut Feed,
) -> Result<(), Walk> {
    match &listing.places {
        Places::Zip(index) => zip_walk(path, index, first, feed),
        Places::SevenZ {
            archive,
            block,
            first: firsts,
        } => sevenz_walk(path, archive, block, firsts, first, feed),
        Places::Rar => rar_walk(path, password, feed),
    }
}

fn zip_walk(path: &Path, index: &[usize], first: usize, feed: &mut Feed) -> Result<(), Walk> {
    let fail = |e: anyhow::Error| Walk::Fail(format!("{e:#}"));
    let mut z = open_zip(path).map_err(fail)?;
    for (idx, &zi) in index.iter().enumerate().skip(first) {
        let take = feed.begin(idx)?;
        if take == Take::Skip {
            continue;
        }
        let size = feed.listing.files[idx].size;
        // zstd entries (method 93): the zip crate is built without its zstd, so their raw
        // bytes are decoded here, size and CRC checked as `archive_extract` does.
        let zstd = {
            let raw = z
                .by_index_raw(zi)
                .map_err(|e| Walk::Fail(format!("read zip entry: {e}")))?;
            (raw.compression() == zip::CompressionMethod::ZSTD).then(|| raw.crc32())
        };
        if let Some(crc) = zstd {
            let raw = z
                .by_index_raw(zi)
                .map_err(|e| Walk::Fail(format!("read zip entry: {e}")))?;
            let mut r = extras()
                .zip_zstd(Box::new(raw), crc, size)
                .map_err(|e| Walk::Fail(format!("{e:#}")))?;
            pump(feed, idx, &take, &mut r)?;
        } else {
            // The zip crate checks the CRC-32 at the entry's end.
            let mut e = z
                .by_index(zi)
                .map_err(|e| Walk::Fail(format!("read zip entry: {e}")))?;
            pump(feed, idx, &take, &mut e)?;
        }
    }
    Ok(())
}

fn sevenz_walk(
    path: &Path,
    archive: &sevenz_rust2::Archive,
    block: &[Option<usize>],
    firsts: &[usize],
    first: usize,
    feed: &mut Feed,
) -> Result<(), Walk> {
    let Some(start) = block.get(first).copied().flatten() else {
        return Ok(());
    };
    let mut source = BufReader::new(
        File::open(path).map_err(|e| Walk::Fail(format!("open {}: {e}", path.display())))?,
    );
    let pw = sevenz_rust2::Password::from("");
    let threads = crate::names::sevenz_decode_threads();
    for b in start..archive.blocks.len() {
        let Some(&from) = firsts.get(b) else {
            break;
        };
        if from == usize::MAX {
            continue;
        }
        let decoder = sevenz_rust2::BlockDecoder::new(threads, b, archive, &pw, &mut source);
        let count = decoder.entry_count();
        if !feed.block_wanted(from, from + count)? {
            continue;
        }
        let mut failure: Option<Walk> = None;
        let mut k = 0usize;
        let result = decoder.for_each_entries(&mut |_e, reader| {
            let idx = from + k;
            k += 1;
            let step = (|| -> Result<(), Walk> {
                let take = feed.begin(idx)?;
                pump(feed, idx, &take, reader)
            })();
            match step {
                Ok(()) => Ok(true),
                Err(e) => {
                    failure = Some(e);
                    Ok(false)
                }
            }
        });
        if let Some(e) = failure {
            feed.release();
            return Err(e);
        }
        result.map_err(|e| Walk::Fail(format!("{:#}", seven_err(path, e))))?;
        if k != count {
            return Err(Walk::Fail(format!(
                "{}: a block of the .7z decoded {k} of its {count} files",
                path.display()
            )));
        }
    }
    Ok(())
}

fn rar_walk(path: &Path, password: Option<&str>, feed: &mut Feed) -> Result<(), Walk> {
    struct Sink<'a> {
        feed: &'a mut Feed,
        idx: usize,
        take: Take,
        got: u64,
        failure: Option<Walk>,
    }
    impl Sink<'_> {
        fn keep<T>(&mut self, r: Result<T, Walk>) -> std::io::Result<T> {
            r.map_err(|e| {
                self.failure = Some(e);
                std::io::Error::other("stopped")
            })
        }
    }
    impl RarSink for Sink<'_> {
        fn visit(&mut self, ordinal: u64, path: &str) -> bool {
            // The walk must meet the files in the order they were listed.
            usize::try_from(ordinal)
                .ok()
                .and_then(|i| self.feed.listing.files.get(i))
                .is_some_and(|f| f.path == path)
        }
        fn want(&mut self, ordinal: u64, _path: &str, _size: u64) -> bool {
            let idx = ordinal as usize;
            self.idx = idx;
            self.got = 0;
            match self.feed.begin(idx) {
                Ok(t) => {
                    self.take = t;
                    self.take != Take::Skip
                }
                Err(e) => {
                    self.failure = Some(e);
                    // Delivered, so the sink's own error stops the walk at once.
                    self.take = Take::Skip;
                    true
                }
            }
        }
        fn begin(&mut self, _path: &str) -> std::io::Result<()> {
            if self.failure.is_some() {
                return Err(std::io::Error::other("stopped"));
            }
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.got = self.got.saturating_add(bytes.len() as u64);
            let size = self.feed.listing.files.get(self.idx).map_or(0, |f| f.size);
            if self.got > size {
                let path = self.feed.listing.files[self.idx].path.clone();
                return self.keep(Err(Walk::Fail(format!(
                    "{path} is longer than the archive lists"
                ))));
            }
            let r = self.feed.chunk(bytes);
            self.keep(r)
        }
        fn end(&mut self) -> std::io::Result<()> {
            let size = self.feed.listing.files.get(self.idx).map_or(0, |f| f.size);
            if self.got != size {
                let path = self.feed.listing.files[self.idx].path.clone();
                let got = self.got;
                return self.keep(Err(Walk::Fail(format!(
                    "{path} is {got} bytes, not the {size} the archive lists"
                ))));
            }
            let r = self.feed.end(self.idx);
            self.keep(r)
        }
        fn skipped(&mut self, _n: u64) {
            (self.feed.tick)();
        }
    }
    let halt = feed.halt.clone();
    let mut sink = Sink {
        feed,
        idx: 0,
        take: Take::Skip,
        got: 0,
        failure: None,
    };
    let r = extras().rar_walk(path, password, &mut sink, &halt);
    let failure = sink.failure.take();
    sink.feed.release();
    if let Some(f) = failure {
        return Err(f);
    }
    match r {
        Ok(()) => Ok(()),
        Err(RarFail::Cancelled) => Err(Walk::Stop),
        Err(RarFail::Sink) => Err(Walk::Stop),
        Err(RarFail::Failed(message)) => Err(Walk::Fail(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ps5u-archive-stream-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn data(i: usize, size: usize) -> Vec<u8> {
        (0..size)
            .map(|j| (i * 31 + j * 7 + j / 251) as u8)
            .collect()
    }

    fn sample() -> Vec<(String, Vec<u8>)> {
        (0..40)
            .map(|i| {
                let size = [10, 64, 65, 1000, 70_000, 300_000][i % 6] + i;
                (format!("g/d{}/f{i:02}.bin", i % 3), data(i, size))
            })
            .collect()
    }

    fn zip_of(path: &Path, files: &[(String, Vec<u8>)], method: zip::CompressionMethod) {
        let mut zw = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        for (p, d) in files {
            zw.start_file(p.as_str(), opts).unwrap();
            zw.write_all(d).unwrap();
        }
        zw.finish().unwrap();
    }

    fn reader(path: &Path) -> Reader {
        let listing = list(path, None).unwrap();
        Reader::new(path, None, listing, Arc::new(|| false))
    }

    /// Every file in any order gives its bytes, in pieces at odd offsets too.
    fn read_all_orders(r: &mut Reader, files: &[(String, Vec<u8>)]) {
        for (p, d) in files {
            assert_eq!(r.read_at(p, 0, d.len() + 5).unwrap(), *d, "{p}");
        }
        for (p, d) in files.iter().rev() {
            let at = d.len() / 3;
            let n = (d.len() - at).min(7000);
            assert_eq!(r.read_at(p, at as u64, n).unwrap(), d[at..at + n], "{p}");
            assert_eq!(r.read_at(p, d.len() as u64, 9).unwrap(), Vec::<u8>::new());
        }
    }

    #[test]
    fn zip_files_read_in_any_order() {
        let d = dir("zip");
        let files = sample();
        let path = d.join("game.zip");
        zip_of(&path, &files, zip::CompressionMethod::Deflated);
        let mut r = reader(&path);
        assert_eq!(r.listing().files.len(), files.len());
        read_all_orders(&mut r, &files);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pass of head reads in archive order runs in one session; heads then come from what was
    /// kept.
    #[test]
    fn head_reads_in_order_take_one_session() {
        let d = dir("heads");
        let files = sample();
        let path = d.join("game.zip");
        zip_of(&path, &files, zip::CompressionMethod::Stored);
        let mut r = reader(&path);
        for (p, data) in &files {
            assert_eq!(r.read_at(p, 0, 32).unwrap(), data[..32.min(data.len())]);
        }
        assert_eq!(r.sessions(), 1);
        for (p, data) in files.iter().rev() {
            assert_eq!(r.read_at(p, 0, 16).unwrap(), data[..16.min(data.len())]);
        }
        assert_eq!(r.sessions(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn sevenz_of(path: &Path, files: &[(String, Vec<u8>)], solid: bool) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let mut w = ArchiveWriter::create(path).unwrap();
        if solid {
            let entries: Vec<ArchiveEntry> = files
                .iter()
                .map(|(p, _)| ArchiveEntry::new_file(p))
                .collect();
            let readers: Vec<SourceReader<std::io::Cursor<Vec<u8>>>> = files
                .iter()
                .map(|(_, d)| SourceReader::new(std::io::Cursor::new(d.clone())))
                .collect();
            w.push_archive_entries(entries, readers).unwrap();
        } else {
            for (p, d) in files {
                w.push_archive_entry(ArchiveEntry::new_file(p), Some(d.as_slice()))
                    .unwrap();
            }
        }
        w.finish().unwrap();
    }

    #[test]
    fn sevenz_solid_and_not_read_in_any_order() {
        for solid in [false, true] {
            let d = dir(if solid { "7z-solid" } else { "7z" });
            let files = sample();
            let path = d.join("game.7z");
            sevenz_of(&path, &files, solid);
            let mut r = reader(&path);
            assert_eq!(r.listing().solid, solid, "solid {solid}");
            read_all_orders(&mut r, &files);
            if solid {
                // Small files decoded on the way to the last one are kept: reading them
                // afterwards, backwards, takes no new session, and lets them go.
                let mut r = reader(&path);
                let (last, last_data) = files.last().unwrap();
                assert_eq!(r.read_at(last, 0, last_data.len()).unwrap(), *last_data);
                assert_eq!(r.sessions(), 1);
                assert!(r.kept_bytes() > 0 && r.kept_bytes() <= KEEP_BUDGET);
                for (p, data) in files.iter().rev().skip(1) {
                    assert_eq!(r.read_at(p, 0, data.len()).unwrap(), *data, "{p}");
                }
                assert_eq!(r.sessions(), 1);
                assert_eq!(r.kept_bytes(), 0);
            }
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Release ends the session (its walk and decoder) and lets go of what it kept; a read after
    /// it starts a new session and still gives the file's bytes.
    #[test]
    fn release_drops_the_session_and_what_it_kept() {
        let d = dir("7z-release");
        let files = sample();
        let path = d.join("game.7z");
        sevenz_of(&path, &files, true);
        let mut r = reader(&path);
        let (last, last_data) = files.last().unwrap();
        assert_eq!(r.read_at(last, 0, last_data.len()).unwrap(), *last_data);
        assert!(r.reading());
        assert!(r.kept_bytes() > 0);
        r.release();
        assert!(!r.reading());
        assert_eq!(r.kept_bytes(), 0);
        let (first, first_data) = &files[0];
        assert_eq!(r.read_at(first, 0, first_data.len()).unwrap(), *first_data);
        assert_eq!(r.sessions(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_truncated_archive_says_so() {
        let d = dir("truncated");
        let files = sample();
        let z = d.join("game.zip");
        zip_of(&z, &files, zip::CompressionMethod::Deflated);
        let bytes = std::fs::read(&z).unwrap();
        std::fs::write(&z, &bytes[..bytes.len() / 2]).unwrap();
        assert!(list(&z, None).is_err());
        let s = d.join("game.7z");
        sevenz_of(&s, &files, true);
        let bytes = std::fs::read(&s).unwrap();
        std::fs::write(&s, &bytes[..bytes.len() / 2]).unwrap();
        let e = list(&s, None).err().unwrap().to_string();
        assert!(e.contains("truncated"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A damaged file's bytes fail its read (the archive's own CRC), not a later one.
    #[test]
    fn damaged_data_fails_the_read() {
        let d = dir("damaged");
        let files = vec![("g/a.bin".to_string(), vec![b'A'; 9000])];
        let z = d.join("game.zip");
        zip_of(&z, &files, zip::CompressionMethod::Stored);
        let mut bytes = std::fs::read(&z).unwrap();
        let at = bytes.windows(64).position(|w| w == [b'A'; 64]).unwrap();
        bytes[at + 100] = b'X';
        std::fs::write(&z, bytes).unwrap();
        let mut r = reader(&z);
        assert!(r.read_at("g/a.bin", 0, 9000).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unsafe_names_are_refused() {
        let ok = |p: &str| Entry {
            path: p.into(),
            size: 1,
        };
        assert!(validate(&[ok("a/b")], &[]).is_ok());
        assert!(validate(&[ok("a"), ok("a")], &[]).is_err());
        assert!(validate(&[ok("a"), ok("a/b")], &[]).is_err());
        assert!(validate(&[ok("a")], &["a".into()]).is_err());
        assert!(validate(&[ok(&"x".repeat(256))], &[]).is_err());
        for bad in ["", "/abs", "a/../b", "a//b", "a/./b"] {
            assert!(validate(&[ok(bad)], &[]).is_err(), "{bad}");
        }
        // A zip whose entry climbs out is refused at listing.
        let d = dir("unsafe");
        let z = d.join("evil.zip");
        zip_of(
            &z,
            &[("../evil".into(), vec![1])],
            zip::CompressionMethod::Stored,
        );
        assert!(list(&z, None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stop_ends_a_read() {
        let d = dir("stop");
        let files = sample();
        let path = d.join("game.zip");
        zip_of(&path, &files, zip::CompressionMethod::Deflated);
        let listing = list(&path, None).unwrap();
        let mut r = Reader::new(&path, None, listing, Arc::new(|| true));
        assert!(matches!(
            r.read_at(&files[4].0, 0, 10),
            Err(ReadError::Cancelled)
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn oversized_7z_headers_are_refused() {
        let encoded = |dict: u8, size: &[u8]| {
            let h = [
                0x06, 0, 1, 0x09, 0, 0, 0x07, 0x0b, 1, 0, 1, 0x21, 0x21, 1, dict, 0x0c,
            ];
            [&h[..], size, &[0, 0]].concat()
        };
        assert_eq!(encoded_over(&encoded(16, &[0xf1, 0, 0, 0, 0])), Some(true));
        assert_eq!(encoded_over(&encoded(40, &[100])), Some(true));
        assert_eq!(encoded_over(&encoded(16, &[100])), Some(false));
        assert_eq!(dictionary(&[0x21], &[36]), Some(1 << 30));
    }
}
