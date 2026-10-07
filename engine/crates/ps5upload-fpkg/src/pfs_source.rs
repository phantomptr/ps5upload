//! A PFS game image as a source tree.
//!
//! This is the unsigned, unencrypted PFS that game images are packed in for mounting on a
//! console: a `.ffpfs` file, or the `pfs_image.dat` nested inside a `.ffpfsc` container. The
//! layout, checked against real images of each kind (tests/fixtures/pfs):
//!
//! - block 0: the header (version, magic, mode, block size, inode count, inode-table blocks);
//! - blocks 1..: the inode table, 0xA8-byte inodes with 32-bit block pointers;
//! - the next block: the super root, a directory naming `uroot`, the tree's root directory;
//! - every file and directory is stored in one run of blocks starting at its first pointer.
//!
//! A file may be stored as it is or as a zlib `PFSC` container of 64 KiB blocks (the same
//! container a `.ffpfsc` wraps its one file in). Signed, encrypted and 64-bit-inode images are
//! other formats and are refused by name.
//!
//! Most such images do not hold the game's files directly: they hold one exFAT image of the
//! game folder. So a PFS image holding a single `.exfat` (or `.ffpkg`) is opened as the tree
//! of that image ([`open`]).

use std::io::{Read, Seek, SeekFrom};

use flate2::read::ZlibDecoder;

use crate::source::{is_junk, SourceFile, SourceTree};
use crate::{format_err, ReadSeek, Result};

const PFS_MAGIC: u64 = 20_130_315;
const MODE_SIGNED: u16 = 0x1;
const MODE_64BIT_INODES: u16 = 0x2;
const MODE_ENCRYPTED: u16 = 0x4;

const INODE_LEN: usize = 0xA8;
const INODE_DIR: u16 = 0x4000;
const INODE_FILE: u16 = 0x8000;
const FLAG_COMPRESSED: u32 = 0x1;

const DIRENT_FILE: i32 = 2;
const DIRENT_DIR: i32 = 3;

const PFSC_MAGIC: u32 = 0x4353_4650;
const PFSC_BLOCK: u64 = 0x10000;
const PFSC_HEADER_LEN: usize = 0x30;

/// Bounds on what an image may claim, so a damaged or hostile one is an error and never an
/// allocation the size of its lie.
const MAX_INODES: u64 = 8_000_000;
const MAX_DIR_BYTES: u64 = 256 << 20;
const MAX_DEPTH: u32 = 64;

#[derive(Clone)]
struct Inode {
    mode: u16,
    flags: u32,
    size: u64,
    size_compressed: u64,
    first_block: u32,
}

impl Inode {
    fn compressed(&self) -> bool {
        self.flags & FLAG_COMPRESSED != 0
    }
    /// What the file takes in the image.
    fn stored(&self) -> u64 {
        if self.compressed() {
            self.size
        } else {
            self.size_compressed
        }
    }
    /// The file's own size.
    fn logical(&self) -> u64 {
        if self.compressed() {
            self.size_compressed
        } else {
            self.size
        }
    }
}

/// Where one file's bytes are.
struct Entry {
    base: u64,
    stored: u64,
    size: u64,
    compressed: bool,
    /// A compressed file's block offsets (from `base`), read on first use.
    offsets: Option<Vec<u64>>,
}

pub struct PfsSource {
    label: String,
    r: Box<dyn ReadSeek>,
    image_len: u64,
    block_size: u64,
    files: Vec<SourceFile>,
    entries: Vec<Entry>,
    empty_dirs: Vec<String>,
    /// Decoded blocks of compressed files, `(file, block)`, most recently used last: an image
    /// nested in a compressed file is read in many small pieces that land in the same block.
    cache: Vec<((usize, u64), Vec<u8>)>,
}

/// Decoded blocks kept (64 KiB each).
const CACHE_BLOCKS: usize = 16;

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Whether `r` starts with a PFS superblock: version 1 or 2, then the PFS magic. Leaves `r`
/// at the start.
pub(crate) fn is_pfs_image<R: Read + Seek>(r: &mut R) -> bool {
    let mut head = [0u8; 16];
    let found = r.seek(SeekFrom::Start(0)).is_ok()
        && r.read_exact(&mut head).is_ok()
        && matches!(le64(&head, 0), 1 | 2)
        && le64(&head, 8) == PFS_MAGIC;
    let _ = r.seek(SeekFrom::Start(0));
    found
}

impl PfsSource {
    /// A PFS image in any seekable bytes; `label` names it in errors and logs.
    pub fn from_reader(mut r: Box<dyn ReadSeek>, label: String) -> Result<Self> {
        let bad = |what: &str| crate::Error::Format(format!("{label}: {what}"));
        let image_len = r.seek(SeekFrom::End(0))?;
        let mut head = [0u8; 0x48];
        r.seek(SeekFrom::Start(0))?;
        r.read_exact(&mut head)
            .map_err(|_| bad("too short to be a PFS image"))?;
        if !matches!(le64(&head, 0), 1 | 2) || le64(&head, 8) != PFS_MAGIC {
            return Err(bad("not a PFS image"));
        }
        let mode = le16(&head, 0x1C);
        if mode & MODE_ENCRYPTED != 0 {
            return Err(bad("the PFS image is encrypted, which Convert cannot read"));
        }
        if mode & (MODE_SIGNED | MODE_64BIT_INODES) != 0 {
            return Err(bad(
                "the PFS image is signed or uses 64-bit inodes, which Convert cannot read (it reads the unsigned 32-bit kind)",
            ));
        }
        let block_size = u64::from(le32(&head, 0x20));
        let inode_count = le64(&head, 0x30);
        let inode_blocks = le64(&head, 0x40);
        if !block_size.is_power_of_two() || !(0x1000..=0x40_0000).contains(&block_size) {
            return Err(bad("the PFS image has an impossible block size"));
        }
        if inode_count == 0 || inode_count > MAX_INODES {
            return Err(bad("the PFS image has an impossible inode count"));
        }
        let per_block = block_size / INODE_LEN as u64;
        let table_end = inode_blocks
            .checked_add(2)
            .and_then(|n| n.checked_mul(block_size));
        if inode_blocks.saturating_mul(per_block) < inode_count
            || table_end.is_none_or(|e| e > image_len)
        {
            return Err(bad("the PFS image's inode table runs past the image"));
        }

        // The inode table, a block at a time (a block's tail is padding, not an inode).
        let mut inodes: Vec<Inode> = Vec::with_capacity(inode_count as usize);
        let mut block = vec![0u8; block_size as usize];
        'table: for b in 0..inode_blocks {
            r.seek(SeekFrom::Start((1 + b) * block_size))?;
            r.read_exact(&mut block)?;
            for i in 0..per_block as usize {
                if inodes.len() as u64 >= inode_count {
                    break 'table;
                }
                let n = &block[i * INODE_LEN..(i + 1) * INODE_LEN];
                inodes.push(Inode {
                    mode: le16(n, 0),
                    flags: le32(n, 4),
                    size: le64(n, 8),
                    size_compressed: le64(n, 0x10),
                    first_block: le32(n, 0x64),
                });
            }
        }

        let mut me = Self {
            label,
            r,
            image_len,
            block_size,
            files: Vec::new(),
            entries: Vec::new(),
            empty_dirs: Vec::new(),
            cache: Vec::new(),
        };

        // The super root is the block after the table; it names the real root, `uroot`.
        let mut sroot = vec![0u8; block_size as usize];
        me.r.seek(SeekFrom::Start((1 + inode_blocks) * block_size))?;
        me.r.read_exact(&mut sroot)?;
        let uroot = dirents(&sroot)
            .into_iter()
            .find(|d| d.name == "uroot")
            .map(|d| d.inode)
            .ok_or_else(|| me.bad("the PFS image has no root directory"))?;

        let mut found: Vec<(SourceFile, Entry)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        me.walk(&inodes, uroot, "", 0, &mut seen, &mut found)?;
        if found.is_empty() {
            return Err(me.bad("holds no files"));
        }
        found.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        me.empty_dirs.sort();
        for (f, e) in found {
            me.files.push(f);
            me.entries.push(e);
        }
        Ok(me)
    }

    fn bad(&self, what: &str) -> crate::Error {
        crate::Error::Format(format!("{}: {what}", self.label))
    }

    /// `len` bytes of the image at `at`, refused when they run past it.
    fn bytes(&mut self, at: u64, len: u64) -> Result<Vec<u8>> {
        if at.checked_add(len).is_none_or(|e| e > self.image_len) {
            return Err(self.bad("the PFS image is shorter than what it holds"));
        }
        let mut out = vec![0u8; len as usize];
        self.r.seek(SeekFrom::Start(at))?;
        self.r.read_exact(&mut out)?;
        Ok(out)
    }

    fn walk(
        &mut self,
        inodes: &[Inode],
        dir: u32,
        prefix: &str,
        depth: u32,
        seen: &mut std::collections::HashSet<u32>,
        out: &mut Vec<(SourceFile, Entry)>,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(self.bad("the PFS image nests deeper than 64 levels"));
        }
        let Some(inode) = inodes.get(dir as usize).cloned() else {
            return Err(self.bad("a directory points outside the inode table"));
        };
        // A directory reached twice is a loop or a hard link; either way it is read once.
        if inode.mode & INODE_DIR == 0 || !seen.insert(dir) {
            return Ok(());
        }
        if inode.size > MAX_DIR_BYTES {
            return Err(self.bad("a directory in the PFS image is impossibly large"));
        }
        let payload = self.bytes(u64::from(inode.first_block) * self.block_size, inode.size)?;
        let mut children = 0usize;
        for d in dirents(&payload) {
            if d.name == "." || d.name == ".." || d.name.is_empty() || d.name.contains('/') {
                continue;
            }
            if is_junk(&d.name) {
                continue;
            }
            let Some(child) = inodes.get(d.inode as usize) else {
                continue;
            };
            let path = format!("{prefix}{}", d.name);
            if d.kind == DIRENT_DIR && child.mode & INODE_DIR != 0 {
                children += 1;
                self.walk(inodes, d.inode, &format!("{path}/"), depth + 1, seen, out)?;
            } else if d.kind == DIRENT_FILE && child.mode & INODE_FILE != 0 {
                children += 1;
                // Checked now, so a truncated image is refused before a build starts and
                // not part-way through one.
                let base = u64::from(child.first_block) * self.block_size;
                if child.stored() > 0
                    && base
                        .checked_add(child.stored())
                        .is_none_or(|end| end > self.image_len)
                {
                    return Err(self.bad("the PFS image is shorter than what it holds"));
                }
                out.push((
                    SourceFile {
                        path,
                        size: child.logical(),
                    },
                    Entry {
                        base: u64::from(child.first_block) * self.block_size,
                        stored: child.stored(),
                        size: child.logical(),
                        compressed: child.compressed(),
                        offsets: None,
                    },
                ));
            }
        }
        if children == 0 && !prefix.is_empty() {
            self.empty_dirs
                .push(prefix.trim_end_matches('/').to_string());
        }
        Ok(())
    }

    fn at(&self, path: &str) -> Result<usize> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map_err(|_| self.bad(&format!("{path} is not in this PFS image")))
    }

    /// A compressed file's block offsets, validated once and kept.
    fn offsets(&mut self, i: usize) -> Result<()> {
        if self.entries[i].offsets.is_some() {
            return Ok(());
        }
        let (base, stored, size) = {
            let e = &self.entries[i];
            (e.base, e.stored, e.size)
        };
        let head = self.bytes(base, PFSC_HEADER_LEN as u64)?;
        let blocks = le64(&head, 0x28) / PFSC_BLOCK;
        if le32(&head, 0) != PFSC_MAGIC
            || le64(&head, 0x10) != PFSC_BLOCK
            || blocks < size.div_ceil(PFSC_BLOCK)
            || blocks > stored
        {
            return Err(self.bad("a compressed file in the PFS image has a damaged header"));
        }
        let table = self.bytes(
            base.checked_add(le64(&head, 0x18))
                .ok_or_else(|| self.bad("a compressed file's block table is out of range"))?,
            (blocks + 1) * 8,
        )?;
        let offsets: Vec<u64> = table.chunks(8).map(|c| le64(c, 0)).collect();
        let ordered = offsets
            .windows(2)
            .all(|w| w[1] >= w[0] && w[1] - w[0] <= PFSC_BLOCK);
        if !ordered || offsets.last().is_none_or(|&end| end > stored) {
            return Err(self.bad("a compressed file's block offsets are damaged"));
        }
        self.entries[i].offsets = Some(offsets);
        Ok(())
    }

    fn range(&mut self, i: usize, offset: u64, len: u64) -> Result<Vec<u8>> {
        let (base, size, compressed) = {
            let e = &self.entries[i];
            (e.base, e.size, e.compressed)
        };
        let start = offset.min(size);
        let end = start.saturating_add(len).min(size);
        if start == end {
            return Ok(Vec::new());
        }
        if !compressed {
            return self.bytes(base + start, end - start);
        }
        self.offsets(i)?;
        let mut out = Vec::with_capacity((end - start) as usize);
        for n in start / PFSC_BLOCK..=(end - 1) / PFSC_BLOCK {
            let (from, to) = {
                let o = self.entries[i].offsets.as_ref().expect("just read");
                (o[n as usize], o[n as usize + 1])
            };
            if let Some(at) = self.cache.iter().position(|(k, _)| *k == (i, n)) {
                let hit = self.cache.remove(at);
                self.cache.push(hit);
            } else {
                let stored = self.bytes(base + from, to - from)?;
                let plain = if stored.len() as u64 == PFSC_BLOCK {
                    stored
                } else {
                    // Bounded: a hostile stream cannot inflate past one block.
                    let mut d = Vec::with_capacity(PFSC_BLOCK as usize);
                    ZlibDecoder::new(&stored[..])
                        .take(PFSC_BLOCK + 1)
                        .read_to_end(&mut d)?;
                    if d.len() as u64 != PFSC_BLOCK {
                        return Err(self.bad("a compressed block does not decode to 64 KiB"));
                    }
                    d
                };
                if self.cache.len() >= CACHE_BLOCKS {
                    self.cache.remove(0);
                }
                self.cache.push(((i, n), plain));
            }
            let plain = &self.cache.last().expect("just pushed").1;
            let block_at = n * PFSC_BLOCK;
            let a = start.max(block_at) - block_at;
            let b = end.min(block_at + PFSC_BLOCK) - block_at;
            out.extend_from_slice(&plain[a as usize..b as usize]);
        }
        Ok(out)
    }
}

/// One file of a PFS image as seekable bytes: the game image nested in it.
struct PfsFile {
    src: PfsSource,
    index: usize,
    len: u64,
    pos: u64,
}

impl Read for PfsFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let got = self
            .src
            .range(self.index, self.pos, buf.len() as u64)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        buf[..got.len()].copy_from_slice(&got);
        self.pos += got.len() as u64;
        Ok(got.len())
    }
}

impl Seek for PfsFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match target {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )),
        }
    }
}

/// The game tree of a PFS image: its own files, or, when all it holds is one game image
/// (the usual case), that image's files.
pub fn open(reader: Box<dyn ReadSeek>, label: String) -> Result<Box<dyn SourceTree>> {
    let src = PfsSource::from_reader(reader, label)?;
    let nested = match src.files() {
        [only] => Some(only.path.to_ascii_lowercase()),
        _ => None,
    };
    let Some(name) =
        nested.filter(|n| n.ends_with(".exfat") || n.ends_with(".ffpkg") || n.ends_with(".ufs2"))
    else {
        return Ok(Box::new(src));
    };
    let len = src.entries[0].size;
    let label = format!("{} ({})", src.label, src.files[0].path);
    let file: Box<dyn ReadSeek> = Box::new(PfsFile {
        src,
        index: 0,
        len,
        pos: 0,
    });
    if name.ends_with(".exfat") {
        crate::remote_source::exfat_tree(file, len, label)
    } else {
        Ok(Box::new(crate::ufs2_source::Ufs2Source::from_reader(
            file, label,
        )?))
    }
}

struct Dirent {
    inode: u32,
    kind: i32,
    name: String,
}

/// The entries of a directory's bytes: `inode, type, name length, entry size`, then the name.
/// Stops at the first entry that does not parse, as the console does.
fn dirents(blob: &[u8]) -> Vec<Dirent> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 16 <= blob.len() {
        let inode = le32(blob, at);
        let kind = le32(blob, at + 4) as i32;
        let name_len = le32(blob, at + 8) as i32;
        let size = le32(blob, at + 12) as i32;
        if inode == 0 && kind == 0 && name_len == 0 && size == 0 {
            break;
        }
        if size < 17 || size % 8 != 0 || name_len < 0 || name_len > size - 16 {
            break;
        }
        let (size, name_len) = (size as usize, name_len as usize);
        if at + size > blob.len() {
            break;
        }
        out.push(Dirent {
            inode,
            kind,
            name: String::from_utf8_lossy(&blob[at + 16..at + 16 + name_len]).into_owned(),
        });
        at += size;
    }
    out
}

impl SourceTree for PfsSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let size = self.entries[i].size;
        if usize::try_from(size).is_err() {
            return format_err(format!("{path} is too large to read whole"));
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
        format!(
            "{} (PFS, {} KiB blocks, {} files)",
            self.label,
            self.block_size / 1024,
            self.files.len()
        )
    }
}
