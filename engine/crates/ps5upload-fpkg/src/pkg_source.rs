//! A debug `.pkg` as a source tree: the game's files read back out of a package, block by block.
//!
//! A package is three images deep, and each is read only where a file's bytes are:
//!
//! - the finalized-image header (`\x7FFIH`) and its container (`\x7FCNT`), read whole — the
//!   container is small and carries the per-block digests every outer block is checked against;
//! - the outer PFS, whose 64 KiB blocks are decrypted on demand (the native mode) or used as they
//!   are stored (the plaintext mode), the way [`crate::verify::verify_streaming`] reads them —
//!   never [`crate::outer::open`], which holds the whole image;
//! - the inner image, `pfs_image.dat`, through its layout descriptor `naps_pkg_layout.dat`. Two
//!   layouts come out of our own builder and both are read here: the block layout
//!   ([`crate::kraken_image`], the default, whose 256 KiB blocks are Kraken-compressed or stored
//!   raw) and the older flat one (`kraken: false`), where every file sits raw at its own 64 KiB
//!   boundary and the metadata region is stored or zlib-compressed ([`crate::inner::MetaCodec`]).
//!
//! The inner metadata region (superblock, inodes, directories) is bounded by the file count, so it
//! is expanded whole to list the tree and then dropped. File bytes are never held: a read decodes
//! the blocks it touches, and a small cache keeps a sequential read from decoding one twice. A
//! 100 GB package unpacks in a few tens of megabytes this way.
//!
//! The artwork the container carries in the image's place (`icon0.png`, `pic0.png`, … — see
//! [`crate::cnt_write::CONTAINER_ONLY`]) is put back under `sce_sys/` from the container, so the
//! tree is the game folder the package was built from, not just the image's share of it.
//!
//! Only packages made with the debug key open: a retail package, a native-mode one built with
//! another passcode, a PS4 package and a layout this reader does not know are each refused with
//! the reason in plain words.

use std::collections::HashSet;
use std::path::Path;

use crate::cnt::{self, Cnt};
use crate::crypto::{derive_ekpfs, derive_xts_keys, sha3};
use crate::inner::{inode_offset, inode_table_len, MetaCodec, INODE_LEN};
use crate::kraken_image::{self, DescribedBlock};
use crate::outer::{self, Dinode, OuterImage};
use crate::source::{is_junk, SourceFile, SourceTree};
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{fih, le16, le32, le64, plan, Error, PkgFile, ReadSeek, Result, BLOCK};

/// Why a package with blocks the debug passcode does not open is refused.
pub const RETAIL_MESSAGE: &str = "this package's files are encrypted with a key this app does not have: it is a retail package, or a fake package made with a passcode other than the default. Only packages made with the default debug passcode can be unpacked";

/// Decrypted outer blocks kept (64 KiB each).
const OUTER_CACHE: usize = 16;
/// Decoded Kraken blocks kept (up to 256 KiB each).
const KRAKEN_CACHE: usize = 4;
/// The most a metadata region may claim. Minecraft's 37,552 inodes take about 6 MiB; a bound
/// keeps a damaged descriptor from asking for an allocation the size of its lie.
const MAX_META_BYTES: u64 = 2 << 30;
/// Directory nesting this reader follows.
const MAX_DEPTH: u32 = 64;
/// Inodes an inner superblock may claim (the same bound [`crate::inner::read`] uses).
const MAX_INODES: u64 = 4_000_000;

const DIRENT_FILE: i32 = plan::DIRENT_FILE as i32;
const DIRENT_DIR: i32 = plan::DIRENT_DIR as i32;

fn bad(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

/// A small most-recently-used cache: the last entry is the newest.
struct Lru {
    cap: usize,
    items: Vec<(u64, Vec<u8>)>,
}

impl Lru {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            items: Vec::with_capacity(cap),
        }
    }

    /// Moves `key` to the newest slot; true when it was there.
    fn touch(&mut self, key: u64) -> bool {
        match self.items.iter().position(|(k, _)| *k == key) {
            Some(at) => {
                let hit = self.items.remove(at);
                self.items.push(hit);
                true
            }
            None => false,
        }
    }

    fn insert(&mut self, key: u64, value: Vec<u8>) {
        if self.items.len() >= self.cap {
            self.items.remove(0);
        }
        self.items.push((key, value));
    }

    fn newest(&self) -> &[u8] {
        &self.items.last().expect("just inserted or touched").1
    }

    fn bytes(&self) -> usize {
        self.items.iter().map(|(_, v)| v.len()).sum()
    }
}

/// The outer PFS, read a block at a time and checked against the container's digests.
struct Outer {
    file: PkgFile,
    cnt: Cnt,
    pfs_offset: u64,
    count: u64,
    sb_index: u64,
    /// The native mode's keys; `None` for a plaintext package, whose blocks are stored as they are.
    xts: Option<Xts>,
    cache: Lru,
}

impl Outer {
    /// The digest `imagedigs` records for block `index`, in natural byte order.
    fn digest(&self, index: u64) -> Option<[u8; 32]> {
        let e = self.cnt.entry(cnt::ids::IMAGE_DIGESTS)?;
        let at = usize::try_from(index).ok()?.checked_mul(32)?;
        let mut d: [u8; 32] = self.cnt.payload(e).get(at..at + 32)?.try_into().ok()?;
        d.reverse();
        Some(d)
    }

    /// Block `index`'s plaintext, or `None` when no reading of it matches its digest.
    fn decode(&mut self, index: u64) -> Result<Option<Vec<u8>>> {
        if index >= self.count {
            return Err(bad(format!(
                "the package points at outer block {index}, past its {} blocks",
                self.count
            )));
        }
        let raw = self
            .file
            .read_at(self.pfs_offset + index * BLOCK, BLOCK as usize)?;
        let want = self
            .digest(index)
            .ok_or_else(|| bad("the package's block digest table is shorter than its image"))?;
        if index == self.sb_index {
            // The superblock is never transformed; its check is the header's game digest.
            return Ok(Some(raw));
        }
        let Some(xts) = &self.xts else {
            // A plaintext package transforms nothing: the block must be its own digest.
            return Ok((sha3(&raw) == want).then_some(raw));
        };
        // Data blocks (before the superblock) carry their own index as the sector; metadata
        // blocks set bit 47. The other sector is tried too, as `outer::open` does, so a block on
        // the wrong side of that rule still reads.
        let first = if index < self.sb_index {
            index
        } else {
            SIGNED_SECTOR_FLAG | index
        };
        for sector in [first, first ^ SIGNED_SECTOR_FLAG] {
            let mut pt = raw.clone();
            xts.decrypt(sector, &mut pt);
            if sha3(&pt) == want {
                return Ok(Some(pt));
            }
        }
        Ok(None)
    }

    /// Block `index`'s plaintext, through the cache.
    fn block(&mut self, index: u64) -> Result<&[u8]> {
        if !self.cache.touch(index) {
            let Some(plain) = self.decode(index)? else {
                return Err(bad(format!(
                    "outer block {index} of the package is damaged: it does not match the digest \
                     the package records for it"
                )));
            };
            self.cache.insert(index, plain);
        }
        Ok(self.cache.newest())
    }

    /// Every data block a dinode's file occupies, in file order: its direct slots, then its
    /// indirect tables level by level (slot 0's records point at data blocks, slot 1's at tables
    /// like slot 0's, and so on).
    fn file_blocks(&mut self, node: &Dinode) -> Result<Vec<u32>> {
        let need = node.size.div_ceil(BLOCK);
        if need > self.count {
            return Err(bad("a file in the outer image is larger than the image"));
        }
        let mut out = Vec::with_capacity(need as usize);
        for d in node
            .direct
            .iter()
            .take((need as usize).min(outer::DIRECT_SLOTS))
        {
            out.push(d.block);
        }
        for (level, table) in node.indirect.iter().enumerate() {
            if out.len() as u64 >= need || table.block == 0 {
                break;
            }
            self.walk_table(u64::from(table.block), level as u32, need, &mut out)?;
        }
        if (out.len() as u64) < need {
            return Err(bad(
                "a file in the outer image has fewer blocks than its size needs",
            ));
        }
        Ok(out)
    }

    fn walk_table(&mut self, block: u64, level: u32, need: u64, out: &mut Vec<u32>) -> Result<()> {
        let table = self.block(block)?.to_vec();
        for slot in 0..outer::PER_INDIRECT {
            if out.len() as u64 >= need {
                return Ok(());
            }
            let child = le32(&table, slot * 36 + 32);
            if child == 0 {
                continue;
            }
            if level == 0 {
                out.push(child);
            } else {
                self.walk_table(u64::from(child), level - 1, need, out)?;
            }
        }
        Ok(())
    }
}

/// One file of the outer image (`pfs_image.dat` or `naps_pkg_layout.dat`), by its block list.
struct OuterFile {
    blocks: Vec<u32>,
    len: u64,
}

impl OuterFile {
    fn open(outer: &mut Outer, node: &Dinode) -> Result<Self> {
        Ok(Self {
            blocks: outer.file_blocks(node)?,
            len: node.size,
        })
    }

    /// Exactly `len` bytes at `at`.
    fn read(&self, outer: &mut Outer, at: u64, len: u64) -> Result<Vec<u8>> {
        let end = at
            .checked_add(len)
            .filter(|e| *e <= self.len)
            .ok_or_else(|| bad("a read runs past the end of the package's inner image"))?;
        let mut out = Vec::with_capacity(len as usize);
        let mut pos = at;
        while pos < end {
            let n = pos / BLOCK;
            let block = outer.block(u64::from(self.blocks[n as usize]))?;
            let from = (pos % BLOCK) as usize;
            let to = ((end - n * BLOCK).min(BLOCK)) as usize;
            out.extend_from_slice(&block[from..to]);
            pos = n * BLOCK + to as u64;
        }
        Ok(out)
    }
}

/// How the inner image's blocks map onto the mount.
enum Layout {
    /// The block layout: every 256 KiB of the mount is one described block, compressed or raw.
    Blocks {
        blocks: Vec<DescribedBlock>,
        cache: Lru,
        /// True when at least one block is Kraken-compressed (for [`SourceTree::describe`]).
        compressed: bool,
    },
    /// The flat layout: each file raw at its own on-disk offset; nothing to decode.
    Flat { codec: MetaCodec },
}

/// Where one file's bytes come from.
enum Location {
    /// The inner mount: its logical offset (the block layout reads by it) and its on-disk
    /// offset in `pfs_image.dat` (the flat layout reads by that).
    Image { logical: u64, on_disk: u64 },
    /// A container entry, by id — the artwork the image leaves to the container.
    Container(u32),
}

/// A debug package's game files.
pub struct PkgSource {
    label: String,
    outer: Outer,
    image: OuterFile,
    layout: Layout,
    files: Vec<SourceFile>,
    locations: Vec<Location>,
    empty_dirs: Vec<String>,
    content_id: String,
}

/// The layout descriptor (`naps_pkg_layout.dat`) of the package at `path`, read through the
/// outer image block by block: a package of any size, without holding its image.
pub fn layout_descriptor(path: &Path, passcode: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let reader = PkgFile::from_reader(Box::new(file), len);
    PkgSource::open_outer(reader, passcode).map(|(_, _, naps, _)| naps)
}

/// The game files of the package at `path`, opened with `passcode` (the debug default is
/// [`crate::crypto::DEFAULT_PASSCODE`]).
pub fn open(path: &Path, passcode: &str) -> Result<PkgSource> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    PkgSource::from_reader(
        Box::new(file),
        len,
        passcode,
        format!("package {}", path.display()),
    )
}

impl PkgSource {
    /// A package in any seekable bytes of known length; `label` names it in errors and logs.
    pub fn from_reader(
        reader: Box<dyn ReadSeek>,
        len: u64,
        passcode: &str,
        label: String,
    ) -> Result<Self> {
        Self::open_inner(PkgFile::from_reader(reader, len), passcode, &label).map_err(|e| match e {
            Error::Format(msg) => Error::Format(format!("{label}: {msg}")),
            other => other,
        })
    }

    /// The outer image opened down to its two files: the inner image and the layout descriptor
    /// (read whole; it is bounded by [`MAX_META_BYTES`]).
    fn open_outer(
        mut file: PkgFile,
        passcode: &str,
    ) -> Result<(Outer, OuterFile, Vec<u8>, String)> {
        let truncated = |what: &str| {
            bad(format!(
                "the package is incomplete (it is cut short {what}); copy or download it again"
            ))
        };
        let len = file.len();
        if len < 4 {
            return Err(bad("not a package: the file is nearly empty"));
        }
        let magic = file.read_at(0, 4)?;
        if magic == [0x7F, b'C', b'N', b'T'] {
            return Err(bad(
                "this is a PS4 package, which cannot be unpacked here; only PS5 packages made with the debug key can",
            ));
        }
        if magic != [0x7F, b'F', b'I', b'H'] {
            return Err(bad("not a PS5 package (no \\x7FFIH header)"));
        }
        if len < BLOCK {
            return Err(truncated("inside its header"));
        }
        let head = file.read_at(0, BLOCK as usize)?;
        let fih = fih::parse(&head)?;
        if !fih.is_debug() {
            return Err(bad(RETAIL_MESSAGE));
        }
        if !fih.pfs_size.is_multiple_of(BLOCK) || fih.pfs_size == 0 {
            return Err(bad(
                "the package's image is not a whole number of blocks, a layout this reader does not know",
            ));
        }
        if len < fih.pfs_offset + fih.pfs_size || len < fih.cnt_offset.saturating_add(0x1000) {
            return Err(truncated("before the end of its image"));
        }
        let cnt = cnt::read(&mut file, fih.cnt_offset).map_err(|e| match e {
            Error::Format(msg) if msg.contains("past end") => {
                truncated("inside its metadata container")
            }
            other => other,
        })?;
        let content_id = cnt.content_id.clone();
        let count = fih.pfs_size / BLOCK;
        match cnt.entry(cnt::ids::IMAGE_DIGESTS) {
            Some(e) if u64::from(e.size) == count * 32 => {}
            _ => {
                return Err(bad(
                    "the package has no block digest table for its image, a layout this reader does not know",
                ))
            }
        }

        // The superblock is where the header says, and its plaintext is the header's game digest.
        let sb_absolute = le64(&head, 0x20);
        let sb_index = sb_absolute.saturating_sub(BLOCK) / BLOCK;
        if sb_absolute < BLOCK || sb_index >= count {
            return Err(bad("the header's superblock offset is outside the image"));
        }
        let sb_block = file.read_at(fih.pfs_offset + sb_index * BLOCK, BLOCK as usize)?;
        if sha3(&sb_block) != fih.game_digest {
            return Err(bad(
                "the package's superblock does not match its header; the package is damaged",
            ));
        }
        let superblock = outer::parse_superblock(sb_index, &sb_block)?;
        let xts = (superblock.mode == crate::ImageMode::Native).then(|| {
            Xts::new(&derive_xts_keys(
                &derive_ekpfs(&cnt.content_id, passcode),
                &superblock.seed,
            ))
        });
        let native = xts.is_some();
        let mut outer = Outer {
            file,
            cnt,
            pfs_offset: fih.pfs_offset,
            count,
            sb_index,
            xts,
            cache: Lru::new(OUTER_CACHE),
        };

        // The inode table is the first block the passcode has to open. A native package whose
        // table does not decrypt to its digest was made with keys we do not have.
        let table_index = u64::from(superblock.inode_table_block);
        let table = match outer.decode(table_index)? {
            Some(t) if sha3(&t) == superblock.inode_table_digest => t,
            _ if native => return Err(bad(RETAIL_MESSAGE)),
            _ => return Err(bad(
                "the package's outer inode table does not match its digest; the package is damaged",
            )),
        };
        let nodes = outer::parse_dinodes(&table, superblock.dinode_count);
        let uroot = nodes
            .get(2)
            .ok_or_else(|| bad("the package's outer image has no root directory"))?;
        let uroot_bytes = outer.block(u64::from(uroot.direct[0].block))?.to_vec();
        let entries = OuterImage::parse_dirents(&uroot_bytes, uroot.size);
        let node_of = |name: &str| {
            entries
                .iter()
                .find(|d| d.name == name)
                .and_then(|d| nodes.get(d.ino as usize))
                .ok_or_else(|| {
                    bad(format!(
                        "the package's outer image has no {name}, a layout this reader does not know"
                    ))
                })
        };
        let image = OuterFile::open(&mut outer, node_of("pfs_image.dat")?)?;
        let naps_file = OuterFile::open(&mut outer, node_of("naps_pkg_layout.dat")?)?;
        // The descriptor is the console's own map of the image, bounded by the block count.
        if naps_file.len > MAX_META_BYTES {
            return Err(bad("the package's layout descriptor is impossibly large"));
        }
        let naps = naps_file.read(&mut outer, 0, naps_file.len)?;
        Ok((outer, image, naps, content_id))
    }

    fn open_inner(file: PkgFile, passcode: &str, label: &str) -> Result<Self> {
        let (outer, image, naps, content_id) = Self::open_outer(file, passcode)?;

        let mut me = Self {
            label: label.to_string(),
            outer,
            image,
            layout: Layout::Flat {
                codec: MetaCodec::Stored,
            },
            files: Vec::new(),
            locations: Vec::new(),
            empty_dirs: Vec::new(),
            content_id,
        };
        let (meta_base, meta, fidx) = me.read_layout(&naps)?;
        me.walk_metadata(meta_base, &meta, &fidx)?;
        me.restore_container_files();
        Ok(me)
    }

    /// Decide the layout from the descriptor and expand the metadata region. Returns the
    /// metadata base, the region's bytes and the descriptor's file starts (afid order).
    fn read_layout(&mut self, naps: &[u8]) -> Result<(u64, Vec<u8>, Vec<u64>)> {
        // The block layout first: its walk checks every record against the next one's key, so a
        // flat descriptor does not pass for one.
        let block_err = match kraken_image::describe(naps) {
            Ok(blocks) => return self.block_layout(naps, blocks),
            Err(e) => e,
        };
        let layout = match crate::naps::parse(naps) {
            Ok(l) => l,
            Err(flat_err) => {
                return Err(bad(format!(
                    "the package's inner image uses a layout this reader does not know \
                     (not the block layout: {block_err}; not the flat layout: {flat_err})"
                )))
            }
        };
        let n = layout.fidx.len();
        if n < 3 {
            return Err(bad(
                "the package's layout descriptor has no metadata base, a layout this reader does not know",
            ));
        }
        let meta_base = layout.fidx[n - 2].0;
        let mount = layout.mount_size();
        let codec = MetaCodec::from_compression_type(layout.compression_type)
            .map_err(|e| bad(format!("a layout this reader does not know: {e}")))?;
        let region = mount
            .checked_sub(meta_base)
            .filter(|r| *r > 0 && *r <= MAX_META_BYTES)
            .ok_or_else(|| {
                bad("the package's layout descriptor has an impossible metadata region")
            })?;
        let meta = match codec {
            MetaCodec::Stored => {
                if self.image.len != mount {
                    return Err(bad(format!(
                        "the package's inner image is {:#x} bytes where its stored layout needs \
                         {mount:#x}, a layout this reader does not know",
                        self.image.len
                    )));
                }
                self.image.read(&mut self.outer, meta_base, region)?
            }
            MetaCodec::Zlib => {
                let tail_len = self
                    .image
                    .len
                    .checked_sub(meta_base)
                    .filter(|t| *t <= MAX_META_BYTES)
                    .ok_or_else(|| bad("the package's inner image ends before its metadata"))?;
                let tail = self.image.read(&mut self.outer, meta_base, tail_len)?;
                let plain = crate::pfsc::parse(&tail)?.decompress()?;
                if plain.len() as u64 != region {
                    return Err(bad(
                        "the package's metadata region does not expand to the size its layout gives",
                    ));
                }
                plain
            }
        };
        self.layout = Layout::Flat { codec };
        let starts = layout.fidx[..n - 3].iter().map(|(o, _)| *o).collect();
        Ok((meta_base, meta, starts))
    }

    /// The block layout: the descriptor's own boundary table gives the metadata base, and the
    /// region is decoded from the blocks that tile it.
    fn block_layout(
        &mut self,
        naps: &[u8],
        blocks: Vec<DescribedBlock>,
    ) -> Result<(u64, Vec<u8>, Vec<u64>)> {
        let bounds = block_bounds(naps)?;
        let n = bounds.len();
        if n < 3 {
            return Err(bad(
                "the package's layout descriptor has no metadata base, a layout this reader does not know",
            ));
        }
        let (meta_base, mount) = (bounds[n - 2], bounds[n - 1]);
        let region = mount
            .checked_sub(meta_base)
            .filter(|r| *r > 0 && *r <= MAX_META_BYTES)
            .ok_or_else(|| {
                bad("the package's layout descriptor has an impossible metadata region")
            })?;
        if let Some(b) = blocks
            .iter()
            .find(|b| b.stored_at.saturating_add(b.stored_len) > self.image.len)
        {
            return Err(bad(format!(
                "the block at {:#x} is stored past the end of the package's inner image",
                b.logical
            )));
        }
        // Entropy-only halves (stored shorter, no LZ flag) are compressed too.
        let compressed = blocks
            .iter()
            .any(|b| b.even_lz || b.odd_lz || b.stored_len < b.len);
        self.layout = Layout::Blocks {
            blocks,
            cache: Lru::new(KRAKEN_CACHE),
            compressed,
        };
        let meta = self.mount_range(meta_base, region)?;
        Ok((meta_base, meta, bounds[..n - 3].to_vec()))
    }

    /// `len` bytes of the mount at `at`, through the block layout.
    fn mount_range(&mut self, at: u64, len: u64) -> Result<Vec<u8>> {
        let Layout::Blocks { blocks, cache, .. } = &mut self.layout else {
            return Err(bad("the flat layout is read by file, not by mount offset"));
        };
        let end = at.saturating_add(len);
        let mut out = Vec::with_capacity(len as usize);
        let mut i = blocks.partition_point(|b| b.logical + b.len <= at);
        let mut pos = at;
        while pos < end {
            let b = blocks
                .get(i)
                .filter(|b| b.logical <= pos)
                .ok_or_else(|| bad(format!("no block of the package covers {pos:#x}")))?;
            if !cache.touch(b.logical) {
                let stored = self
                    .image
                    .read(&mut self.outer, b.stored_at, b.stored_len)?;
                let local = DescribedBlock {
                    stored_at: 0,
                    ..b.clone()
                };
                let plain = kraken_image::decode_described(&stored, &local).map_err(|e| {
                    bad(format!(
                        "the block at {:#x} does not decode ({e}; {:#x} bytes stored as \
                         {:#x} + {:#x}, flags {:#04x}); the package is damaged or compressed \
                         in a way this reader does not know",
                        b.logical,
                        b.len,
                        b.even_len,
                        b.stored_len.saturating_sub(b.even_len),
                        b.mode_bits
                    ))
                })?;
                cache.insert(b.logical, plain);
            }
            let plain = cache.newest();
            let from = (pos - b.logical) as usize;
            let to = ((end - b.logical).min(b.len)) as usize;
            out.extend_from_slice(&plain[from..to]);
            pos = b.logical + to as u64;
            i += 1;
        }
        Ok(out)
    }

    /// Walk the inner file system from `uroot`, the way the console does, collecting every file
    /// and every directory with nothing in it.
    fn walk_metadata(&mut self, meta_base: u64, meta: &[u8], starts: &[u64]) -> Result<()> {
        let region = Region {
            base: meta_base,
            bytes: meta,
        };
        let sb = region.at(meta_base, 0x400)?;
        if le64(sb, 0) != 2 || le64(sb, 8) != 20_130_315 {
            return Err(bad(
                "the package's inner superblock is not one this reader knows",
            ));
        }
        let inode_count = le64(sb, 0x30);
        if u64::from(le32(sb, 0x20)) != BLOCK || inode_count == 0 || inode_count > MAX_INODES {
            return Err(bad(format!(
                "the package's inner superblock is implausible ({inode_count} inodes, block size {:#x})",
                le32(sb, 0x20)
            )));
        }
        // The inode table's absolute block, which every package we build records; the block after
        // the superblock is where it sits when the field is empty.
        let table_at = match le64(sb, 0xD8) {
            0 => meta_base + BLOCK,
            block => block * BLOCK,
        };
        let table = region.at(table_at, inode_table_len(inode_count as usize))?;
        let walk = Walk {
            region: &region,
            table,
            count: inode_count as usize,
        };
        let mut found: Vec<Found> = Vec::new();
        let mut empty = Vec::new();
        let mut seen = HashSet::new();
        walk.dir(
            plan::FIRST_DIR_INODE as usize,
            "",
            0,
            &mut seen,
            &mut found,
            &mut empty,
        )?;
        if found.is_empty() {
            return Err(bad("the package holds no files"));
        }

        // Where each file's bytes sit in the stored image.
        let on_disk: Vec<u64> = match &self.layout {
            // The block layout stores the mount itself, block by block.
            Layout::Blocks { .. } => found.iter().map(|f| f.logical).collect(),
            // The flat layout gives each file a 64 KiB boundary of its own, in afid order — the
            // planner's rule (`plan::FILE_ALIGN`). The descriptor's boundary table carries each
            // afid's logical start, which is how this is checked to be that layout.
            Layout::Flat { .. } => {
                let mut by_afid: Vec<usize> = (0..found.len()).collect();
                by_afid.sort_by_key(|&i| found[i].afid);
                let dense = by_afid
                    .iter()
                    .enumerate()
                    .all(|(k, &i)| found[i].afid == k as i64);
                let agrees = starts.len() == found.len()
                    && by_afid
                        .iter()
                        .zip(starts)
                        .all(|(&i, &s)| found[i].logical == s);
                if !dense || !agrees {
                    return Err(bad(
                        "the package's file table does not match its layout descriptor, a layout \
                         this reader does not know",
                    ));
                }
                let mut at = vec![0u64; found.len()];
                let mut cursor = 0u64;
                for &i in &by_afid {
                    at[i] = cursor;
                    cursor = (cursor + found[i].size).next_multiple_of(plan::FILE_ALIGN);
                }
                at
            }
        };
        for (f, &disk) in found.iter().zip(&on_disk) {
            let fits = match &self.layout {
                Layout::Blocks { .. } => f
                    .logical
                    .checked_add(f.size)
                    .is_some_and(|e| e <= meta_base),
                Layout::Flat { .. } => disk.checked_add(f.size).is_some_and(|e| e <= meta_base),
            };
            if !fits {
                return Err(bad(format!(
                    "{} runs past the package's data region; the package is damaged",
                    f.path
                )));
            }
        }

        let mut paired: Vec<(SourceFile, Location)> = found
            .into_iter()
            .zip(on_disk)
            .map(|(f, disk)| {
                (
                    SourceFile {
                        path: f.path,
                        size: f.size,
                    },
                    Location::Image {
                        logical: f.logical,
                        on_disk: disk,
                    },
                )
            })
            .collect();
        paired.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        for (f, loc) in paired {
            // Map every afid before filtering: ignored files still occupy space in a flat
            // package. The visible tree follows the folder and image readers' junk rules.
            if f.path.split('/').any(is_junk) {
                continue;
            }
            self.files.push(f);
            self.locations.push(loc);
        }
        empty.sort();
        self.empty_dirs = empty;
        Ok(())
    }

    /// The artwork a build moves from the image into the container, put back where the game
    /// folder had it. Only what the image lacks: a package that kept its own copy wins.
    fn restore_container_files(&mut self) {
        let mut wanted: Vec<(u32, &str)> = vec![
            (cnt::ids::ICON0_PNG, "sce_sys/icon0.png"),
            (cnt::ids::ICON0_DDS, "sce_sys/icon0.dds"),
        ];
        for (id, path, _) in crate::cnt_write::PRESENTATION {
            if crate::cnt_write::CONTAINER_ONLY.contains(&path) {
                wanted.push((id, path));
            }
        }
        let mut added = false;
        for (id, path) in wanted {
            let Some(e) = self.outer.cnt.entry(id) else {
                continue;
            };
            if e.size == 0 || self.files.iter().any(|f| f.path == path) {
                continue;
            }
            self.files.push(SourceFile {
                path: path.to_string(),
                size: u64::from(e.size),
            });
            self.locations.push(Location::Container(id));
            added = true;
        }
        if added {
            let mut paired: Vec<(SourceFile, Location)> = std::mem::take(&mut self.files)
                .into_iter()
                .zip(std::mem::take(&mut self.locations))
                .collect();
            paired.sort_by(|a, b| a.0.path.cmp(&b.0.path));
            for (f, loc) in paired {
                self.files.push(f);
                self.locations.push(loc);
            }
            // A restored file fills a directory the image had left empty.
            let files = &self.files;
            self.empty_dirs.retain(|d| {
                let prefix = format!("{d}/");
                !files.iter().any(|f| f.path.starts_with(&prefix))
            });
        }
    }

    /// The content id the package's container declares.
    pub fn content_id(&self) -> &str {
        &self.content_id
    }

    /// Bytes this reader holds for blocks it has read: both caches. Bounded by their sizes,
    /// whatever the package's — which is what lets a 100 GB package stream.
    pub fn cached_bytes(&self) -> usize {
        let blocks = match &self.layout {
            Layout::Blocks { cache, .. } => cache.bytes(),
            Layout::Flat { .. } => 0,
        };
        self.outer.cache.bytes() + blocks
    }

    fn at(&self, path: &str) -> Result<usize> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map_err(|_| bad(format!("{}: {path} is not in this package", self.label)))
    }

    fn range(&mut self, i: usize, offset: u64, len: u64) -> Result<Vec<u8>> {
        let size = self.files[i].size;
        let start = offset.min(size);
        let end = start.saturating_add(len).min(size);
        if start == end {
            return Ok(Vec::new());
        }
        match self.locations[i] {
            Location::Container(id) => {
                let e = self
                    .outer
                    .cnt
                    .entry(id)
                    .ok_or_else(|| bad("a container entry went missing"))?;
                Ok(self.outer.cnt.payload(e)[start as usize..end as usize].to_vec())
            }
            Location::Image { logical, on_disk } => match self.layout {
                Layout::Blocks { .. } => self.mount_range(logical + start, end - start),
                Layout::Flat { .. } => {
                    self.image
                        .read(&mut self.outer, on_disk + start, end - start)
                }
            },
        }
    }
}

/// The block layout's boundary table: each file's logical start in afid order, then the data
/// end, the metadata base and the mount size. Laid out as [`kraken_image::describe`] reads it.
fn block_bounds(blob: &[u8]) -> Result<Vec<u64>> {
    if blob.len() < 16 {
        return Err(bad("the package's layout descriptor is too short"));
    }
    let (w0, w1) = (le64(blob, 0), le64(blob, 8));
    let files = (w0 & 0xFF_FFFF) as usize + 1;
    let outer = (w1 & 0xFF_FFFF) as usize;
    let at = 16 + outer * 8;
    let table = blob
        .get(at..at + files * 6)
        .ok_or_else(|| bad("the package's layout descriptor ends inside its file table"))?;
    Ok(table
        .chunks(6)
        .map(|r| u64::from_le_bytes([r[0], r[1], r[2], r[3], r[4], 0, 0, 0]))
        .collect())
}

/// The metadata region, addressed by absolute mount offset.
struct Region<'a> {
    base: u64,
    bytes: &'a [u8],
}

impl Region<'_> {
    fn at(&self, offset: u64, len: u64) -> Result<&[u8]> {
        offset
            .checked_sub(self.base)
            .and_then(|o| {
                let o = usize::try_from(o).ok()?;
                self.bytes
                    .get(o..o.checked_add(usize::try_from(len).ok()?)?)
            })
            .ok_or_else(|| {
                bad(format!(
                    "the package's metadata points at {offset:#x}, outside its metadata region"
                ))
            })
    }
}

/// A file the walk found.
struct Found {
    path: String,
    size: u64,
    logical: u64,
    afid: i64,
}

struct Walk<'a> {
    region: &'a Region<'a>,
    table: &'a [u8],
    count: usize,
}

/// One inner inode's fields: mode, size, logical offset, afid.
struct InnerInode {
    mode: u16,
    size: u64,
    offset: u64,
    afid: i32,
}

impl Walk<'_> {
    fn inode(&self, i: usize) -> Result<InnerInode> {
        let raw = (i < self.count)
            .then(|| {
                let o = inode_offset(i);
                self.table.get(o..o + INODE_LEN)
            })
            .flatten()
            .ok_or_else(|| bad(format!("inode {i} is outside the package's inode table")))?;
        Ok(InnerInode {
            mode: le16(raw, 0),
            size: le64(raw, 8),
            offset: le64(raw, 0x60),
            afid: crate::i32le(raw, 0x68),
        })
    }

    fn dir(
        &self,
        ino: usize,
        prefix: &str,
        depth: u32,
        seen: &mut HashSet<usize>,
        files: &mut Vec<Found>,
        empty: &mut Vec<String>,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(bad("the package's directories nest deeper than 64 levels"));
        }
        if !seen.insert(ino) {
            return Err(bad(format!(
                "the package's directories loop at inode {ino}"
            )));
        }
        let node = self.inode(ino)?;
        if node.mode & 0x4000 == 0 {
            return Err(bad(format!(
                "inode {ino} is listed as a directory but is not one"
            )));
        }
        let block = self.region.at(node.offset, node.size.min(MAX_META_BYTES))?;
        let mut children = 0usize;
        let mut o = 0usize;
        while o + 16 <= block.len() {
            let child = le32(block, o) as usize;
            let kind = crate::i32le(block, o + 4);
            let name_len = le32(block, o + 8) as usize;
            let ent_size = le32(block, o + 12) as usize;
            if ent_size == 0 || o + 16 + name_len > block.len() {
                break;
            }
            let name = String::from_utf8_lossy(&block[o + 16..o + 16 + name_len]).into_owned();
            o += ent_size;
            if name == "." || name == ".." {
                continue;
            }
            if name.is_empty() || name.contains('/') || name.contains('\0') {
                return Err(bad(format!(
                    "the package holds an entry named {name:?}, which no folder can hold"
                )));
            }
            let path = format!("{prefix}{name}");
            match kind {
                DIRENT_DIR => {
                    children += usize::from(!is_junk(&name));
                    self.dir(child, &format!("{path}/"), depth + 1, seen, files, empty)?;
                }
                DIRENT_FILE => {
                    children += usize::from(!is_junk(&name));
                    let f = self.inode(child)?;
                    if f.mode & 0x8000 == 0 {
                        return Err(bad(format!("{path} is listed as a file but is not one")));
                    }
                    files.push(Found {
                        path,
                        size: f.size,
                        logical: f.offset,
                        afid: i64::from(f.afid),
                    });
                }
                other => {
                    return Err(bad(format!(
                        "{path} has entry kind {other}, which this reader does not know"
                    )))
                }
            }
        }
        if children == 0 && !prefix.is_empty() && !prefix.split('/').any(is_junk) {
            empty.push(prefix.trim_end_matches('/').to_string());
        }
        Ok(())
    }
}

impl SourceTree for PkgSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let size = self.files[i].size;
        if usize::try_from(size).is_err() {
            return Err(bad(format!("{path} is too large to read whole")));
        }
        self.range(i, 0, size)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        self.range(i, offset, len as u64)
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        let layout = match &self.layout {
            Layout::Blocks {
                compressed: true, ..
            } => "Kraken block layout",
            Layout::Blocks { .. } => "stored block layout",
            Layout::Flat {
                codec: MetaCodec::Zlib,
            } => "flat layout, zlib metadata",
            Layout::Flat { .. } => "flat layout",
        };
        format!(
            "{} ({}, {layout}, {} files)",
            self.label,
            self.content_id,
            self.files.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_bytes(bytes: Vec<u8>) -> Result<PkgSource> {
        let len = bytes.len() as u64;
        PkgSource::from_reader(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            crate::crypto::DEFAULT_PASSCODE,
            "test".into(),
        )
    }

    fn message(r: Result<PkgSource>) -> String {
        match r {
            Ok(_) => panic!("must not open"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn a_ps4_package_is_named() {
        let mut b = vec![0u8; 0x1000];
        b[..4].copy_from_slice(b"\x7FCNT");
        assert!(message(open_bytes(b)).contains("PS4 package"));
    }

    #[test]
    fn something_else_is_not_a_package() {
        assert!(message(open_bytes(vec![0x55; 0x2000])).contains("not a PS5 package"));
        assert!(message(open_bytes(vec![1, 2])).contains("not a package"));
    }

    #[test]
    fn a_retail_header_is_refused_by_name() {
        let mut b = vec![0u8; BLOCK as usize];
        b[..4].copy_from_slice(b"\x7FFIH");
        b[5] = 0x80;
        assert!(message(open_bytes(b)).contains("encrypted with a key this app does not have"));
    }

    #[test]
    fn a_header_alone_is_truncated() {
        let mut b = vec![0u8; 0x200];
        b[..4].copy_from_slice(b"\x7FFIH");
        assert!(message(open_bytes(b)).contains("incomplete"));
    }

    #[test]
    fn the_lru_keeps_the_newest() {
        let mut c = Lru::new(2);
        c.insert(1, vec![1]);
        c.insert(2, vec![2]);
        assert!(c.touch(1));
        c.insert(3, vec![3]);
        assert!(!c.touch(2), "the oldest goes first");
        assert!(c.touch(1) && c.touch(3));
        assert_eq!(c.bytes(), 2);
    }

    #[test]
    fn the_source_is_send() {
        fn send<T: Send>() {}
        send::<PkgSource>();
    }
}
