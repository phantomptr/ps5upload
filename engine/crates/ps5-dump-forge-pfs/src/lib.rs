//! PFS images for ShadowMountPlus: the `.ffpfs` writer, the streaming `.ffpfsc` container, and
//! readers for both.
//!
//! Frozen interface (ps5-dump-forge-core depends on it): `plan` validates names and sizes the image
//! without reading file data; `write` streams the files into `out` in offset order; `wrap` turns
//! any inner image writer into a `.ffpfsc` as it writes; [`PfsSource`] and [`open_ffpfsc`] read
//! them back.
//!
//! The layout is MkPFS 1.1.0's unsigned PS5 image (PSBrew, GPL-3.0, read for its format and
//! written here independently): what `mkpfs pack folder --raw --no-compress --version PS5
//! --inode-bits 32 --block-size 65536` makes, byte for byte for the same tree and time. The one
//! difference: empty directories are kept (MkPFS drops them). By block:
//!
//! | Blocks (64 KiB) | What |
//! |---|---|
//! | 0 | header: version 2, case-insensitive, unsigned, 32-bit inodes, unencrypted |
//! | 1.. | inode table, 390 D32 inodes to a block |
//! | then | super-root entries (`flat_path_table`, `collision_resolver` if any, `uroot`) |
//! | then | flat path table; collision resolver, or one reserved empty block |
//! | then | `uroot`, the other directories, the files: one contiguous run each |
//!
//! Files are stored uncompressed: MkPFS's per-file compression passes its own verify, but the
//! console misreads it. A `.ffpfsc` is the same encoder over a one-file tree whose file is a
//! zlib PFSC container of the inner image (see [`wrap`]).

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{Error, Result};

mod image;
mod read;
mod wrap;

pub use read::{FfpfscInfo, PfsHeader, PfsSource, open_ffpfsc};
pub use wrap::{Stream, WrapOptions, WrapReport, container_size_max, wrap};

/// The PFS block size this crate writes, and the PFSC block size of every container.
pub const BLOCK: u64 = 0x10000;
/// Read size per `read_range`: bounded memory however large the file.
const CHUNK: u64 = 8 << 20;

/// Knobs a caller may set. `Default` is the SMP-recommended layout.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Every inode's timestamp (seconds since 1970); `None` is now. Tests pin it.
    pub time: Option<i64>,
}

/// Everything `write` needs, fixed before the first file byte is read.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Final image size in bytes (a multiple of 64 KiB).
    pub image_size: u64,
    pub files: u64,
    /// Directories below the root, empty ones included.
    pub dirs: u64,
    /// File-data bytes `write` copies: the `total` its progress reports against.
    pub data_bytes: u64,
    img: image::Image,
    /// Hash of the tree's file list and empty dirs as planned; `write` refuses another.
    source: u64,
}

/// What was written.
#[derive(Debug, Clone)]
pub struct Report {
    pub image_size: u64,
    pub files: u64,
    /// Directories below the root.
    pub dirs: u64,
}

/// Validate names and lay the image out. Fails with every offending path listed.
/// Checks `cancel` between steps.
pub fn plan(tree: &dyn SourceTree, opts: &Options, cancel: &AtomicBool) -> Result<Layout> {
    check_cancel(cancel)?;
    let nodes = image::nodes(tree, cancel)?;
    let img = image::lay_out(nodes, opts.time.unwrap_or_else(now), cancel)?;
    let files = img.files().count() as u64;
    let data_bytes = img.files().map(|n| n.size).sum();
    Ok(Layout {
        image_size: img.ndblock * BLOCK,
        files,
        dirs: img.order.len() as u64 - 1 - files,
        data_bytes,
        source: fingerprint(tree),
        img,
    })
}

/// Write the image described by `layout` into `out` (empty, positioned at 0; the caller
/// fsyncs and renames it). Every byte is written in order, so `out` may be a [`Stream`].
/// `progress(done, total)` counts file-data bytes.
pub fn write<W: Write + Seek>(
    tree: &mut dyn SourceTree,
    layout: &Layout,
    out: &mut W,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    check_cancel(cancel)?;
    // The plan holds the tree's paths and sizes; a different tree would write wrong data.
    if fingerprint(tree) != layout.source {
        return Err(err(
            "the source changed between planning and writing the PFS image",
        ));
    }
    if out.seek(SeekFrom::End(0))? != 0 {
        return Err(err("the PFS output is not empty"));
    }
    let img = &layout.img;
    let mut sink = image::Sink::new(&mut *out, cancel);
    image::write_meta(&mut sink, img)?;
    let mut done = 0u64;
    progress(0, layout.data_bytes);
    for node in img.files() {
        check_cancel(cancel)?;
        sink.expect(node.block * BLOCK)?;
        let mut at = 0u64;
        while at < node.size {
            check_cancel(cancel)?;
            let want = (node.size - at).min(CHUNK);
            let buf = tree.read_range(&node.path, at, want as usize)?;
            // Sources may return short reads; none may return nothing or too much.
            if buf.is_empty() || buf.len() as u64 > want {
                return Err(err(format!(
                    "{}: read {} bytes at {at} of its planned {} (did it change?)",
                    node.path,
                    buf.len(),
                    node.size
                )));
            }
            sink.write(&buf)?;
            at += buf.len() as u64;
            done += buf.len() as u64;
            progress(done, layout.data_bytes);
        }
        sink.zeros_to((node.block + node.blocks) * BLOCK)?;
    }
    sink.expect(layout.image_size)?;
    progress(done, layout.data_bytes);
    // A cancel raised by the last progress call still wins over finishing.
    check_cancel(cancel)?;
    sink.w.flush()?;
    Ok(Report {
        image_size: layout.image_size,
        files: layout.files,
        dirs: layout.dirs,
    })
}

/// FNV-1a over every planned path and size, so `write` can tell a renamed, reordered or
/// resized source from the one `plan` saw.
fn fingerprint(tree: &dyn SourceTree) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for f in tree.files() {
        eat(f.path.as_bytes());
        eat(&[0]);
        eat(&f.size.to_le_bytes());
    }
    eat(&[1]);
    for d in tree.empty_dirs() {
        eat(d.as_bytes());
        eat(&[0]);
    }
    h
}

/// Seconds since 1970, for `Options::time` left unset.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn err(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

/// Little-endian fields of fixed-size buffers; every caller's offset is in range.
fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or([0; 4]))
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8]))
}
