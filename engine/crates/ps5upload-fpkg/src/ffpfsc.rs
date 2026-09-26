//! `.ffpfsc`: a game image compressed for ShadowMountPlus.
//!
//! One file — normally an `.exfat` (or `.ffpkg`) game image — inside a minimal unsigned PFS,
//! stored as a zlib `PFSC` container. The console's kernel decodes the container itself as the
//! game reads, so the image mounts like any other and takes 40–60% less space. This is the
//! compressed format the scene has proven on consoles; a package install cannot use it (the
//! installer refuses zlib images), so it is the compressed route for mounted games.
//!
//! The layout follows MkPFS's single-file images (GPL-3, read for its format, written here
//! independently), which ShadowMountPlus mounts:
//!
//! ```text
//! block 0  PFS header (version 2, unsigned, case-insensitive)
//! block 1  inode table: super-root, flat_path_table, uroot, the file
//! block 2  super-root entries: flat_path_table, uroot
//! block 3  the flat path table
//! block 4  reserved, empty
//! block 5  uroot entries: ., .., the file
//! block 6… the file's bytes, contiguous: a PFSC container, or the raw file
//! ```
//!
//! The container: a 0x30-byte header, a table of `n + 1` absolute block offsets at `0x400`,
//! and the blocks from `0x10000`. Each 64 KiB block is a zlib stream when that saves at least
//! [`WrapOptions::min_block_gain`] percent and the raw block otherwise; a reader tells them
//! apart by the stored length alone, so a compressed block is always shorter than 64 KiB.
//! Blocks that barely compress stay raw on purpose: the console then reads them with no
//! inflate at all.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::{format_err, Result};

/// The PFS block size, and the container's logical block size.
pub const BLOCK: u64 = 0x10000;
const PFS_VERSION: u64 = 2;
const PFS_MAGIC: u64 = 20_130_315;
const MODE_CASE_INSENSITIVE: u16 = 0x8;

const PFSC_MAGIC: u32 = 0x4353_4650;
const PFSC_VERSION_WORD: u32 = 6;
const PFSC_HEADER_LEN: usize = 0x30;
const PFSC_TABLE_AT: u64 = 0x400;
const PFSC_DATA_AT: u64 = 0x10000;

const INODE_LEN: usize = 0xA8;
const INODE_DIR: u16 = 0x4000;
const INODE_FILE: u16 = 0x8000;
/// Read and execute for owner, group and others.
const INODE_RX: u16 = 0x001 | 0x004 | 0x008 | 0x020 | 0x040 | 0x100;
const FLAG_COMPRESSED: u32 = 0x1;
const FLAG_READONLY: u32 = 0x10;
const FLAG_INTERNAL: u32 = 0x2_0000;

const DIRENT_FILE: i32 = 2;
const DIRENT_DIR: i32 = 3;
const DIRENT_DOT: i32 = 4;
const DIRENT_DOTDOT: i32 = 5;

/// Where each piece sits, in blocks.
const SUPER_ROOT_BLOCK: i32 = 2;
const FPT_BLOCK: i32 = 3;
const UROOT_BLOCK: i32 = 5;
const FILE_BLOCK: u64 = 6;

/// Blocks in flight per worker: enough to keep every core fed, few enough that memory stays
/// flat however large the image.
const IN_FLIGHT_PER_WORKER: usize = 8;

#[derive(Debug, Clone)]
pub struct WrapOptions {
    /// zlib level, 1–9. The console's inflate cost barely depends on it. Measured on a real
    /// exFAT game image: level 6 lands within 0.2% of level 9's size and builds 30% faster, so
    /// it is the default.
    pub level: u32,
    /// Percent a block must shrink by to be stored compressed.
    pub min_block_gain: u8,
    /// Worker threads; 0 uses every core.
    pub threads: usize,
    /// The name the file carries inside the image; derived from the source when absent.
    pub inner_name: Option<String>,
    /// Build timestamp (seconds); the current time when absent.
    pub time: Option<i64>,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            level: 6,
            min_block_gain: 5,
            threads: 0,
            inner_name: None,
            time: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WrapReport {
    pub output: PathBuf,
    pub inner_name: String,
    pub raw_size: u64,
    /// The file's bytes as stored: the container, or the raw file when compression did not pay.
    pub stored_size: u64,
    pub image_size: u64,
    pub compressed: bool,
    pub blocks: u64,
    pub compressed_blocks: u64,
}

/// Progress and cancellation from an asynchronous caller.
#[derive(Default)]
pub struct Control<'a> {
    /// `(raw bytes processed, raw total)`.
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
    pub cancel: Option<&'a AtomicBool>,
}

/// The name a source file takes inside the image: its title id with its own extension when the
/// name carries one (`PPSA01234.exfat`), otherwise its name cleaned to at most 15 safe characters.
pub fn inner_name_for(source: &Path) -> String {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.find('.') {
        Some(at) => (&name[..at], name[at..].to_ascii_lowercase()),
        None => (name.as_str(), String::new()),
    };
    if let Some(id) = title_id_in(stem) {
        return format!("{id}{ext}");
    }
    let mut out = String::new();
    let mut pending: Option<char> = None;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            if let Some(sep) = pending.take() {
                if !out.is_empty() {
                    out.push(sep);
                }
            }
            out.push(c);
        } else if c == ' ' {
            pending = Some('_');
        } else if c == '_' || c == '-' {
            pending = Some(c);
        }
    }
    out.truncate(15);
    if out.is_empty() {
        out = "IMAGE".to_string();
    }
    format!("{out}{ext}")
}

/// A `XXXX00000` title id standing on its own in `s`.
fn title_id_in(s: &str) -> Option<String> {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(8)).find_map(|i| {
        let w = &b[i..i + 9];
        let edge = |j: Option<&u8>| j.is_none_or(|c| !c.is_ascii_alphanumeric());
        (w[..4].iter().all(u8::is_ascii_uppercase)
            && w[4..].iter().all(u8::is_ascii_digit)
            && edge(i.checked_sub(1).and_then(|p| b.get(p)))
            && edge(b.get(i + 9)))
        .then(|| String::from_utf8_lossy(w).into_owned())
    })
}

/// The flat-path-table hash: `h = 31h + c` over the upper-cased path.
fn fpt_hash(path: &str) -> u32 {
    path.bytes().fold(0u32, |h, c| {
        h.wrapping_mul(31)
            .wrapping_add(u32::from(c.to_ascii_uppercase()))
    })
}

fn dirent(inode: u32, kind: i32, name: &str) -> Vec<u8> {
    let len = (name.len() + 17).next_multiple_of(8);
    let mut d = Vec::with_capacity(len);
    d.extend_from_slice(&inode.to_le_bytes());
    d.extend_from_slice(&kind.to_le_bytes());
    d.extend_from_slice(&(name.len() as i32).to_le_bytes());
    d.extend_from_slice(&(len as i32).to_le_bytes());
    d.extend_from_slice(name.as_bytes());
    d.resize(len, 0);
    d
}

struct Inode {
    mode: u16,
    nlink: u16,
    flags: u32,
    size: u64,
    size_compressed: u64,
    blocks: u32,
    db: [i32; 12],
}

impl Inode {
    fn bytes(&self, time: i64) -> [u8; INODE_LEN] {
        let mut d = [0u8; INODE_LEN];
        d[0..2].copy_from_slice(&self.mode.to_le_bytes());
        d[2..4].copy_from_slice(&self.nlink.to_le_bytes());
        d[4..8].copy_from_slice(&self.flags.to_le_bytes());
        d[8..16].copy_from_slice(&self.size.to_le_bytes());
        d[16..24].copy_from_slice(&self.size_compressed.to_le_bytes());
        for i in 0..4 {
            d[24 + i * 8..32 + i * 8].copy_from_slice(&time.to_le_bytes());
        }
        // 0x38..0x60: nanoseconds, uid, gid and two reserved words, all zero.
        d[0x60..0x64].copy_from_slice(&self.blocks.to_le_bytes());
        for (i, b) in self.db.iter().enumerate() {
            d[0x64 + i * 4..0x68 + i * 4].copy_from_slice(&b.to_le_bytes());
        }
        // 0x94..0xA8: the five indirect slots, unused.
        d
    }
}

/// Everything before the file's bytes: header, inode table, directories and path table.
fn metadata(inner: &str, stored: u64, raw: u64, compressed: bool, time: i64) -> Vec<u8> {
    let file_blocks = stored.div_ceil(BLOCK).max(1);
    let ndblock = FILE_BLOCK + file_blocks;
    let mut out = vec![0u8; (FILE_BLOCK * BLOCK) as usize];

    // Block 0: the header. The inode-table descriptor at 0x50 is a small inode of its own.
    let h = &mut out[..BLOCK as usize];
    h[0x00..0x08].copy_from_slice(&PFS_VERSION.to_le_bytes());
    h[0x08..0x10].copy_from_slice(&PFS_MAGIC.to_le_bytes());
    h[0x1A] = 1;
    h[0x1C..0x1E].copy_from_slice(&MODE_CASE_INSENSITIVE.to_le_bytes());
    h[0x20..0x24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    h[0x28..0x30].copy_from_slice(&1u64.to_le_bytes()); // leading blocks
    h[0x30..0x38].copy_from_slice(&4u64.to_le_bytes()); // inodes
    h[0x38..0x40].copy_from_slice(&ndblock.to_le_bytes());
    h[0x40..0x48].copy_from_slice(&1u64.to_le_bytes()); // inode-table blocks
    let t = 0x50;
    h[t + 2..t + 4].copy_from_slice(&1u16.to_le_bytes());
    h[t + 4..t + 8].copy_from_slice(&FLAG_READONLY.to_le_bytes());
    h[t + 8..t + 16].copy_from_slice(&BLOCK.to_le_bytes());
    h[t + 16..t + 24].copy_from_slice(&BLOCK.to_le_bytes());
    for i in 0..4 {
        h[t + 0x18 + i * 8..t + 0x20 + i * 8].copy_from_slice(&time.to_le_bytes());
    }
    h[t + 0x60..t + 0x64].copy_from_slice(&1u32.to_le_bytes());
    h[t + 0x68 + 32..t + 0x68 + 40].copy_from_slice(&1u64.to_le_bytes()); // table at block 1
    h[0x368..0x36C].copy_from_slice(&1u32.to_le_bytes()); // unsigned

    // The directories and the path table.
    let file_path = format!("/{inner}");
    let mut fpt = Vec::new();
    fpt.extend_from_slice(&fpt_hash(&file_path).to_le_bytes());
    fpt.extend_from_slice(&3u32.to_le_bytes());
    let mut super_root = dirent(1, DIRENT_FILE, "flat_path_table");
    super_root.extend(dirent(2, DIRENT_DIR, "uroot"));
    let mut uroot = dirent(2, DIRENT_DOT, ".");
    uroot.extend(dirent(2, DIRENT_DOTDOT, ".."));
    uroot.extend(dirent(3, DIRENT_FILE, inner));

    let with = |first: i32, fill: i32| {
        let mut db = [fill; 12];
        db[0] = first;
        db
    };
    let inodes = [
        Inode {
            mode: INODE_DIR | INODE_RX,
            nlink: 1,
            flags: FLAG_INTERNAL | FLAG_READONLY,
            size: BLOCK,
            size_compressed: BLOCK,
            blocks: 1,
            db: with(SUPER_ROOT_BLOCK, 0),
        },
        Inode {
            mode: INODE_FILE | INODE_RX,
            nlink: 1,
            flags: FLAG_INTERNAL | FLAG_READONLY,
            size: fpt.len() as u64,
            size_compressed: fpt.len() as u64,
            blocks: 1,
            db: with(FPT_BLOCK, -1),
        },
        Inode {
            mode: INODE_DIR | INODE_RX,
            nlink: 3,
            flags: FLAG_READONLY,
            size: BLOCK,
            size_compressed: BLOCK,
            blocks: 1,
            db: with(UROOT_BLOCK, -1),
        },
        Inode {
            mode: INODE_FILE | INODE_RX,
            nlink: 1,
            flags: FLAG_READONLY | if compressed { FLAG_COMPRESSED } else { 0 },
            // A compressed file records its stored length as its size and its logical length
            // in the second field.
            size: stored,
            size_compressed: if compressed { raw } else { stored },
            blocks: file_blocks as u32,
            db: with(FILE_BLOCK as i32, -1),
        },
    ];
    for (i, inode) in inodes.iter().enumerate() {
        let at = BLOCK as usize + i * INODE_LEN;
        out[at..at + INODE_LEN].copy_from_slice(&inode.bytes(time));
    }
    let put = |out: &mut Vec<u8>, block: i32, data: &[u8]| {
        let at = block as usize * BLOCK as usize;
        out[at..at + data.len()].copy_from_slice(data);
    };
    put(&mut out, SUPER_ROOT_BLOCK, &super_root);
    put(&mut out, FPT_BLOCK, &fpt);
    put(&mut out, UROOT_BLOCK, &uroot);
    out
}

/// Size of the container's header area: 64 KiB, grown by whole blocks when the offset table
/// outgrows the room between `0x400` and `0x10000`.
fn pfsc_header_len(blocks: u64) -> u64 {
    let table = (blocks + 1) * 8;
    let room = PFSC_DATA_AT - PFSC_TABLE_AT;
    PFSC_DATA_AT + table.saturating_sub(room).div_ceil(BLOCK) * BLOCK
}

fn pfsc_header(blocks: u64, data_at: u64) -> [u8; PFSC_HEADER_LEN] {
    let mut h = [0u8; PFSC_HEADER_LEN];
    h[0x00..0x04].copy_from_slice(&PFSC_MAGIC.to_le_bytes());
    h[0x08..0x0C].copy_from_slice(&PFSC_VERSION_WORD.to_le_bytes());
    h[0x0C..0x10].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    h[0x10..0x18].copy_from_slice(&BLOCK.to_le_bytes());
    h[0x18..0x20].copy_from_slice(&PFSC_TABLE_AT.to_le_bytes());
    h[0x20..0x28].copy_from_slice(&data_at.to_le_bytes());
    h[0x28..0x30].copy_from_slice(&(blocks * BLOCK).to_le_bytes());
    h
}

/// One logical block as it is stored: compressed when that saves `min_gain` percent.
fn encode_block(raw: &[u8], level: u32, min_gain: u8) -> Vec<u8> {
    let mut padded = raw.to_vec();
    padded.resize(BLOCK as usize, 0);
    let mut enc = ZlibEncoder::new(Vec::with_capacity(BLOCK as usize), Compression::new(level));
    // Writing into a Vec cannot fail.
    enc.write_all(&padded).expect("zlib into memory");
    let z = enc.finish().expect("zlib into memory");
    let limit = BLOCK as usize * (100 - min_gain as usize) / 100;
    if z.len() < BLOCK as usize && z.len() <= limit {
        z
    } else {
        padded
    }
}

/// Run `work` over what `produce` yields on `threads` workers, handing each result to `sink` in
/// the order it was produced. Reading, the workers and writing all overlap, and the queues are
/// bounded, so a 100 GB image streams through in constant memory. The first error from any
/// stage stops the rest.
pub(crate) fn pipeline<T: Send, U: Send>(
    threads: usize,
    mut produce: impl FnMut() -> Result<Option<T>> + Send,
    work: impl Fn(T) -> Result<U> + Sync,
    mut sink: impl FnMut(U) -> Result<()>,
) -> Result<()> {
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};
    let depth = threads * IN_FLIGHT_PER_WORKER;
    std::thread::scope(|s| {
        let (tx_in, rx_in) = sync_channel::<(u64, T)>(depth);
        let (tx_out, rx_out) = sync_channel::<(u64, Result<U>)>(depth);
        let rx_in = Arc::new(Mutex::new(rx_in));
        let work = &work;
        for _ in 0..threads {
            let rx_in = Arc::clone(&rx_in);
            let tx_out = tx_out.clone();
            s.spawn(move || loop {
                // Holding the lock only to take the next item keeps the workers independent.
                let next = rx_in.lock().map(|rx| rx.recv());
                match next {
                    Ok(Ok((i, item))) => {
                        if tx_out.send((i, work(item))).is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            });
        }
        drop(rx_in);
        let tx_err = tx_out.clone();
        drop(tx_out);
        s.spawn(move || {
            let mut i = 0u64;
            loop {
                match produce() {
                    Ok(Some(item)) => {
                        if tx_in.send((i, item)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx_err.send((i, Err(e)));
                        return;
                    }
                }
                i += 1;
            }
        });
        let mut pending = std::collections::BTreeMap::new();
        let mut next = 0u64;
        for (i, result) in rx_out {
            pending.insert(i, result);
            while let Some(result) = pending.remove(&next) {
                sink(result?)?;
                next += 1;
            }
        }
        Ok(())
    })
}

/// Wrap `source` into a `.ffpfsc` at `output`. Written as `<output>.partial` and renamed only
/// once the finished image has been read back and matched against the source.
pub fn wrap(
    source: &Path,
    output: &Path,
    options: &WrapOptions,
    control: &mut Control,
) -> Result<WrapReport> {
    if !(1..=9).contains(&options.level) {
        return format_err(format!(
            "zlib level must be 1 through 9, not {}",
            options.level
        ));
    }
    if options.min_block_gain > 90 {
        return format_err("the minimum block gain must be at most 90%");
    }
    let raw_size = std::fs::metadata(source)?.len();
    if raw_size == 0 {
        return format_err(format!("{} is empty", source.display()));
    }
    if output.exists() {
        return format_err(format!("{} already exists", output.display()));
    }
    let inner = options
        .inner_name
        .clone()
        .unwrap_or_else(|| inner_name_for(source));
    if inner.is_empty() || !inner.is_ascii() || inner.contains('/') {
        return format_err(format!("{inner:?} cannot name a file inside the image"));
    }
    let time = options.time.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });
    let threads = match options.threads {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    };
    let partial = PathBuf::from(format!("{}.partial", output.display()));
    let result = write_image(
        source, &partial, &inner, raw_size, time, threads, options, control,
    )
    .and_then(|(report, crcs)| {
        // Decoded and matched against the checksums taken as the source was read, so the
        // check costs no second pass over the source — on a slow disk, a third of the time.
        check(&partial, Expect::Crcs(&crcs))?;
        std::fs::rename(&partial, output)?;
        Ok(WrapReport {
            output: output.to_path_buf(),
            ..report
        })
    });
    if result.is_err() {
        std::fs::remove_file(&partial).ok();
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn write_image(
    source: &Path,
    out_path: &Path,
    inner: &str,
    raw_size: u64,
    time: i64,
    threads: usize,
    options: &WrapOptions,
    control: &mut Control,
) -> Result<(WrapReport, Vec<u32>)> {
    let mut src = File::open(source)?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out_path)?;
    let base = FILE_BLOCK * BLOCK;
    let blocks = raw_size.div_ceil(BLOCK);
    let data_at = pfsc_header_len(blocks);

    // The blocks: read, compressed on every core, and written in order, all at once.
    let mut offsets = Vec::with_capacity(blocks as usize + 1);
    offsets.push(data_at);
    let mut compressed_blocks = 0u64;
    let mut crcs = Vec::with_capacity(blocks as usize);
    out.seek(SeekFrom::Start(base + data_at))?;
    let mut writer = std::io::BufWriter::with_capacity(8 << 20, &mut out);
    let mut read = 0u64;
    let mut done = 0u64;
    let (level, gain) = (options.level, options.min_block_gain);
    let cancel = control.cancel;
    let progress = &mut control.progress;
    pipeline(
        threads,
        || {
            if read >= raw_size {
                return Ok(None);
            }
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return format_err("cancelled");
            }
            let n = (raw_size - read).min(BLOCK) as usize;
            let mut block = vec![0u8; n];
            src.read_exact(&mut block)?;
            read += n as u64;
            Ok(Some(block))
        },
        |block| {
            Ok((
                block.len() as u64,
                crc32fast::hash(&block),
                encode_block(&block, level, gain),
            ))
        },
        |(raw_len, crc, stored)| {
            writer.write_all(&stored)?;
            crcs.push(crc);
            if stored.len() < BLOCK as usize {
                compressed_blocks += 1;
            }
            offsets.push(offsets.last().unwrap() + stored.len() as u64);
            done += raw_len;
            if let Some(p) = progress.as_deref_mut() {
                if done.is_multiple_of(64 * BLOCK) || done == raw_size {
                    p(done, raw_size);
                }
            }
            Ok(())
        },
    )?;
    writer.flush()?;
    drop(writer);
    let mut src = File::open(source)?;
    let container_len = *offsets.last().unwrap();

    // Keep the container only when it is actually smaller; otherwise store the file as it is.
    let compressed = compressed_blocks > 0 && container_len < raw_size;
    let stored = if compressed {
        let mut head = vec![0u8; data_at as usize];
        head[..PFSC_HEADER_LEN].copy_from_slice(&pfsc_header(blocks, data_at));
        for (i, o) in offsets.iter().enumerate() {
            let at = PFSC_TABLE_AT as usize + i * 8;
            head[at..at + 8].copy_from_slice(&o.to_le_bytes());
        }
        out.seek(SeekFrom::Start(base))?;
        out.write_all(&head)?;
        container_len
    } else {
        out.seek(SeekFrom::Start(base))?;
        src.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut (&mut src).take(raw_size), &mut out)?;
        raw_size
    };
    let image_size = base + stored.div_ceil(BLOCK).max(1) * BLOCK;
    out.set_len(image_size)?;
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&metadata(inner, stored, raw_size, compressed, time))?;
    out.sync_all()?;
    Ok((
        WrapReport {
            output: out_path.to_path_buf(),
            inner_name: inner.to_string(),
            raw_size,
            stored_size: stored,
            image_size,
            compressed,
            blocks,
            compressed_blocks,
        },
        crcs,
    ))
}

/// What [`verify`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub inner_name: String,
    pub raw_size: u64,
    pub stored_size: u64,
    pub compressed: bool,
}

/// What each decoded block is checked against.
enum Expect<'a> {
    Nothing,
    Source(File),
    /// A CRC-32 per 64 KiB block of the source, taken as it was read.
    Crcs(&'a [u32]),
}

/// A block's expected content, handed from the reader to a worker.
enum Want {
    Nothing,
    Bytes(Vec<u8>),
    Crc(u32),
}

impl Want {
    fn check(&self, plain: &[u8]) -> Result<()> {
        let ok = match self {
            Want::Nothing => true,
            Want::Bytes(b) => plain == &b[..],
            Want::Crc(c) => crc32fast::hash(plain) == *c,
        };
        if ok {
            Ok(())
        } else {
            format_err("a decoded block differs from the source")
        }
    }
}

/// Read an image back: the header, the inode and directory it names, and every block of the
/// file decoded. With `source`, the decoded bytes must equal it exactly.
pub fn verify(image: &Path, source: Option<&Path>) -> Result<Inspection> {
    let expect = match source {
        Some(p) => Expect::Source(File::open(p)?),
        None => Expect::Nothing,
    };
    check(image, expect)
}

fn check(image: &Path, mut expect: Expect) -> Result<Inspection> {
    let mut f = File::open(image)?;
    let mut head = vec![0u8; (FILE_BLOCK * BLOCK) as usize];
    f.read_exact(&mut head)?;
    let le64 = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let le32 = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    if le64(&head, 0) != PFS_VERSION || le64(&head, 8) != PFS_MAGIC {
        return format_err("not a PFS image (version/magic)");
    }
    let node = BLOCK as usize + 3 * INODE_LEN;
    let flags = le32(&head, node + 4);
    let stored = le64(&head, node + 8);
    let second = le64(&head, node + 16);
    let first_block = le32(&head, node + 0x64) as u64;
    let compressed = flags & FLAG_COMPRESSED != 0;
    let raw_size = if compressed { second } else { stored };
    // The file named in uroot: the third entry.
    let uroot = &head[UROOT_BLOCK as usize * BLOCK as usize..];
    let mut at = 0usize;
    let mut name = String::new();
    for _ in 0..3 {
        let len = le32(uroot, at + 8) as usize;
        let step = le32(uroot, at + 12) as usize;
        name = String::from_utf8_lossy(&uroot[at + 16..at + 16 + len]).into_owned();
        at += step;
    }
    let base = first_block * BLOCK;
    if let Expect::Crcs(c) = &expect {
        if c.len() as u64 != raw_size.div_ceil(BLOCK) {
            return format_err("the checksum list does not cover the file");
        }
    }
    let has_expectation = !matches!(expect, Expect::Nothing);
    // The next block's expected content, in order.
    let mut index = 0usize;
    let mut next_want = |n: usize| -> Result<Want> {
        let w = match &mut expect {
            Expect::Nothing => Want::Nothing,
            Expect::Source(src) => {
                let mut e = vec![0u8; n];
                src.read_exact(&mut e)?;
                Want::Bytes(e)
            }
            Expect::Crcs(c) => Want::Crc(c[index]),
        };
        index += 1;
        Ok(w)
    };
    if !compressed && has_expectation {
        // Stored as it is: compare in place.
        f.seek(SeekFrom::Start(base))?;
        let mut got = vec![0u8; BLOCK as usize];
        let mut left = raw_size;
        while left > 0 {
            let n = left.min(BLOCK) as usize;
            f.read_exact(&mut got[..n])?;
            next_want(n)?.check(&got[..n])?;
            left -= n as u64;
        }
    }
    if !compressed {
        return Ok(Inspection {
            inner_name: name,
            raw_size,
            stored_size: stored,
            compressed,
        });
    }
    f.seek(SeekFrom::Start(base))?;
    let mut ph = [0u8; PFSC_HEADER_LEN];
    f.read_exact(&mut ph)?;
    if le32(&ph, 0) != PFSC_MAGIC || le64(&ph, 0x10) != BLOCK {
        return format_err("the file is flagged compressed but holds no PFSC container");
    }
    let blocks = le64(&ph, 0x28) / BLOCK;
    if blocks != raw_size.div_ceil(BLOCK) {
        return format_err("the container's length disagrees with the file size");
    }
    let mut table = vec![0u8; ((blocks + 1) * 8) as usize];
    f.seek(SeekFrom::Start(base + le64(&ph, 0x18)))?;
    f.read_exact(&mut table)?;
    let offsets: Vec<u64> = table.chunks(8).map(|c| le64(c, 0)).collect();
    if *offsets.last().unwrap() != stored {
        return format_err("the container's last offset is not the stored size");
    }
    // Every block decoded on every core, each checked against the source as it comes.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut pairs = offsets.windows(2);
    let mut left = raw_size;
    f.seek(SeekFrom::Start(base + offsets[0]))?;
    let mut f = std::io::BufReader::with_capacity(8 << 20, f);
    pipeline(
        threads,
        || {
            let Some(pair) = pairs.next() else {
                return Ok(None);
            };
            let len = pair[1]
                .checked_sub(pair[0])
                .filter(|l| *l <= BLOCK)
                .ok_or_else(|| crate::Error::Format("a block offset runs backwards".into()))?;
            let mut stored_block = vec![0u8; len as usize];
            f.read_exact(&mut stored_block)?;
            let n = left.min(BLOCK) as usize;
            left -= n as u64;
            Ok(Some((stored_block, n, next_want(n)?)))
        },
        |(stored_block, n, expected)| {
            let plain = if stored_block.len() == BLOCK as usize {
                stored_block
            } else {
                let mut d = Vec::with_capacity(BLOCK as usize);
                ZlibDecoder::new(&stored_block[..]).read_to_end(&mut d)?;
                if d.len() != BLOCK as usize {
                    return format_err("a compressed block does not decode to 64 KiB");
                }
                d
            };
            expected.check(&plain[..n])
        },
        |()| Ok(()),
    )?;
    Ok(Inspection {
        inner_name: name,
        raw_size,
        stored_size: stored,
        compressed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ffpfsc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn inner_names_follow_the_title_id() {
        assert_eq!(
            inner_name_for(Path::new("/x/PPSA01234.exfat")),
            "PPSA01234.exfat"
        );
        assert_eq!(
            inner_name_for(Path::new("My Game (PPSA03016) v1.EXFAT")),
            "PPSA03016.exfat"
        );
        assert_eq!(
            inner_name_for(Path::new("a game-image.ffpkg")),
            "a_game-image.ffpkg"
        );
        assert_eq!(
            inner_name_for(Path::new("xPPSA012345.bin")),
            "xPPSA012345.bin"
        );
    }

    #[test]
    fn a_compressible_image_round_trips_and_shrinks() {
        let d = temp("comp");
        let src = d.join("PPSA00001.exfat");
        // Mixed content: text-like blocks, zero runs, and a random-ish tail that stays raw.
        let mut data = Vec::new();
        for i in 0..400_000u32 {
            data.extend_from_slice(format!("line {i} of the game data\n").as_bytes());
        }
        data.extend(std::iter::repeat_n(0u8, 300_000));
        let mut x = 0x1234_5678u32;
        data.extend((0..200_000).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        }));
        std::fs::write(&src, &data).unwrap();
        let out = d.join("PPSA00001.ffpfsc");
        let r = wrap(&src, &out, &WrapOptions::default(), &mut Control::default()).unwrap();
        assert!(r.compressed);
        assert!(r.stored_size < data.len() as u64 / 3, "{r:?}");
        assert!(r.compressed_blocks < r.blocks, "the random tail stays raw");
        let i = verify(&out, Some(&src)).unwrap();
        assert_eq!(i.inner_name, "PPSA00001.exfat");
        assert_eq!(i.raw_size, data.len() as u64);
        assert!(!d.join("PPSA00001.ffpfsc.partial").exists());
    }

    #[test]
    fn an_incompressible_image_is_stored_as_it_is() {
        let d = temp("raw");
        let src = d.join("noise.bin");
        let mut x = 0x9E37_79B9u32;
        let data: Vec<u8> = (0..300_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        std::fs::write(&src, &data).unwrap();
        let out = d.join("noise.ffpfsc");
        let r = wrap(&src, &out, &WrapOptions::default(), &mut Control::default()).unwrap();
        assert!(!r.compressed);
        assert_eq!(r.stored_size, data.len() as u64);
        verify(&out, Some(&src)).unwrap();
    }

    #[test]
    fn the_layout_matches_the_single_file_image() {
        let d = temp("layout");
        let src = d.join("PPSA00002.exfat");
        std::fs::write(&src, vec![7u8; 3 * BLOCK as usize + 5]).unwrap();
        let out = d.join("o.ffpfsc");
        let opts = WrapOptions {
            time: Some(1_700_000_000),
            ..WrapOptions::default()
        };
        let r = wrap(&src, &out, &opts, &mut Control::default()).unwrap();
        let img = std::fs::read(&out).unwrap();
        assert_eq!(img.len() as u64, r.image_size);
        assert_eq!(
            &img[0x1C..0x1E],
            &0x8u16.to_le_bytes(),
            "case-insensitive, unsigned"
        );
        assert_eq!(&img[0x368..0x36C], &1u32.to_le_bytes());
        let nd = u64::from_le_bytes(img[0x38..0x40].try_into().unwrap());
        assert_eq!(nd * BLOCK, r.image_size);
        // The file inode: compressed, stored size first, logical size second, block 6.
        let node = BLOCK as usize + 3 * INODE_LEN;
        assert_eq!(
            &img[node + 4..node + 8],
            &(FLAG_READONLY | FLAG_COMPRESSED).to_le_bytes()
        );
        assert_eq!(&img[node + 16..node + 24], &(3 * BLOCK + 5).to_le_bytes());
        assert_eq!(&img[node + 0x64..node + 0x68], &6i32.to_le_bytes());
        assert_eq!(&img[node + 0x68..node + 0x6C], &(-1i32).to_le_bytes());
        // The path table names the file by its hash.
        let fpt = 3 * BLOCK as usize;
        assert_eq!(
            &img[fpt..fpt + 4],
            &fpt_hash("/PPSA00002.exfat").to_le_bytes()
        );
        assert_eq!(&img[fpt + 4..fpt + 8], &3u32.to_le_bytes());
        // The container header.
        let c = 6 * BLOCK as usize;
        assert_eq!(&img[c..c + 4], b"PFSC");
        assert_eq!(&img[c + 8..c + 12], &6u32.to_le_bytes());
        assert_eq!(&img[c + 0x28..c + 0x30], &(4 * BLOCK).to_le_bytes());
    }

    #[test]
    fn a_corrupted_block_is_caught() {
        let d = temp("corrupt");
        let src = d.join("PPSA00003.exfat");
        let data: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let out = d.join("c.ffpfsc");
        wrap(&src, &out, &WrapOptions::default(), &mut Control::default()).unwrap();
        let mut img = std::fs::read(&out).unwrap();
        // Flip a byte inside the first stored block.
        let at = (FILE_BLOCK * BLOCK + PFSC_DATA_AT) as usize + 40;
        img[at] ^= 0xFF;
        std::fs::write(&out, &img).unwrap();
        assert!(verify(&out, Some(&src)).is_err());
    }

    #[test]
    fn a_large_table_grows_the_header_area() {
        assert_eq!(pfsc_header_len(1), 0x10000);
        let room_blocks = (PFSC_DATA_AT - PFSC_TABLE_AT) / 8 - 1;
        assert_eq!(pfsc_header_len(room_blocks), 0x10000);
        assert_eq!(pfsc_header_len(room_blocks + 1), 0x20000);
    }

    #[test]
    fn existing_output_and_bad_options_are_refused() {
        let d = temp("refuse");
        let src = d.join("a.exfat");
        std::fs::write(&src, b"x").unwrap();
        let out = d.join("a.ffpfsc");
        std::fs::write(&out, b"").unwrap();
        assert!(wrap(&src, &out, &WrapOptions::default(), &mut Control::default()).is_err());
        let bad = WrapOptions {
            level: 0,
            ..WrapOptions::default()
        };
        assert!(wrap(&src, &d.join("b.ffpfsc"), &bad, &mut Control::default()).is_err());
    }
}
