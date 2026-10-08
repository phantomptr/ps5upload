//! Reading PFS images (`.ffpfs`, and the outer image of a `.ffpfsc`) and the image inside a
//! `.ffpfsc`.
//!
//! Every length and pointer comes from the file, so nothing is trusted: counts are capped
//! before anything is allocated from them, each node's pointers must be the one layout the
//! writers make (validated, not assumed), names are decoded strictly, and a damaged or hostile
//! image is an error, never a panic or a silent skip. A compressed file is read through its
//! PFSC container one 64 KiB block at a time, with a bounded inflate. The console looks files
//! up through the flat path table, so it (and the collision resolver) must be exactly what the
//! walked tree encodes to.
//! ponytail: contiguous nodes only (`db[0]` plus a run, no indirect blocks): what MkPFS and this
//! crate write; anything else is refused as an unsupported layout.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;
use ps5upload_fpkg::exfat::{ExFat, ExFatSource};
use ps5upload_fpkg::source::{SourceFile, SourceTree};
use ps5upload_fpkg::ufs2_source::Ufs2Source;
use ps5upload_fpkg::{Error, PkgFile, ReadSeek, Result};

use crate::image::{
    DIRENT_DIR, DIRENT_DOT, DIRENT_DOTDOT, DIRENT_FILE, FLAG_COMPRESSED, INODE_LEN, MAX_DIR_BYTES,
    MODE_64BIT_INODES, MODE_CASE_INSENSITIVE, MODE_DIR, MODE_ENCRYPTED, MODE_FILE, MODE_SIGNED,
    PFS_MAGIC, PathEntry, fpt_hash, path_table,
};
use crate::wrap::{PFSC_HEADER_LEN, PFSC_MAGIC, PFSC_TABLE_AT};
use crate::{BLOCK, err, le16, le32, le64};

/// The most files and directories an image may list, as the vendored exFAT/UFS2 readers.
const MAX_ENTRIES: u64 = 1_000_000;
/// Inodes the table may declare: the entries plus the internal ones.
const MAX_INODES: u64 = MAX_ENTRIES + 16;
const MAX_DEPTH: u32 = 32;
const MAX_PATH_BYTES: u64 = 256 << 20;
/// Decoded PFSC blocks kept per file: a sequential read needs one, a directory walk a few.
const CACHE_BLOCKS: usize = 8;

/// The header's facts, for verification and inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PfsHeader {
    /// 1 (PS4) or 2 (PS5).
    pub version: u64,
    /// Mode bits; this reader takes only case-sensitivity (0x8).
    pub mode: u16,
    /// Bytes; a power of two, 4 KiB..64 KiB. The console misreads anything but 64 KiB.
    pub block_size: u32,
    pub inodes: u64,
    pub inode_blocks: u64,
    /// The image's length in blocks, as the header declares it.
    pub ndblock: u64,
    /// The bytes actually there.
    pub image_len: u64,
}

/// One inode, as much of it as the walk needs.
#[derive(Debug, Clone, Copy)]
struct Inode {
    mode: u16,
    flags: u32,
    size: u64,
    size_compressed: u64,
    blocks: u64,
    db0: i64,
    /// What every one of `db[1..12]` holds when they all agree and every `ib` is 0: -1 for a
    /// contiguous run from `db0`, 0 for the super-root.
    fill: Option<i32>,
}

/// Where a file's bytes are, by absolute offset.
#[derive(Debug, Clone, Copy)]
struct Extent {
    at: u64,
    stored: u64,
    /// Logical length.
    len: u64,
    compressed: bool,
}

/// A PFS image as a [`SourceTree`].
pub struct PfsSource {
    r: Box<dyn ReadSeek>,
    label: String,
    header: PfsHeader,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    index: HashMap<String, Extent>,
    /// The compressed file read last, with its decoded blocks.
    pfsc: Option<(String, Pfsc)>,
}

impl PfsSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(Box::new(file), path.display().to_string())
    }

    /// Parses the header and walks the whole tree; `label` prefixes every error.
    pub fn from_reader(mut r: Box<dyn ReadSeek>, label: String) -> Result<Self> {
        let walked = walk(&mut *r).map_err(|e| prefix(&label, e))?;
        Ok(Self {
            r,
            label,
            header: walked.header,
            files: walked.files,
            empty_dirs: walked.empty_dirs,
            index: walked.index,
            pfsc: None,
        })
    }

    pub fn header(&self) -> &PfsHeader {
        &self.header
    }

    /// The file at `path` as its own `Read + Seek`, decoded if it is compressed.
    fn into_reader(mut self, path: &str) -> Result<Window> {
        let e = self.extent(path)?;
        let pfsc = if e.compressed {
            Some(Pfsc::open(&mut *self.r, e)?)
        } else {
            None
        };
        Ok(Window {
            r: self.r,
            at: e.at,
            len: e.len,
            pos: 0,
            pfsc,
        })
    }

    fn extent(&self, path: &str) -> Result<Extent> {
        self.index
            .get(path)
            .copied()
            .ok_or_else(|| err(format!("{}: no file {path:?}", self.label)))
    }
}

impl SourceTree for PfsSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let len = self.extent(path)?.len;
        let len =
            usize::try_from(len).map_err(|_| err(format!("{path}: too large to read whole")))?;
        self.read_range(path, 0, len)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let e = self.extent(path)?;
        let n = e.len.saturating_sub(offset).min(len as u64) as usize;
        if n == 0 {
            return Ok(Vec::new());
        }
        if !e.compressed {
            let mut buf = vec![0u8; n];
            self.r.seek(SeekFrom::Start(e.at + offset))?;
            self.r.read_exact(&mut buf)?;
            return Ok(buf);
        }
        // The container is checked before its logical length sizes anything.
        if self.pfsc.as_ref().is_none_or(|(p, _)| p != path) {
            let pfsc = Pfsc::open(&mut *self.r, e).map_err(|x| prefix(&self.label, x))?;
            self.pfsc = Some((path.to_string(), pfsc));
        }
        let mut buf = vec![0u8; n];
        let Some((_, pfsc)) = self.pfsc.as_mut() else {
            return Ok(buf);
        };
        let mut done = 0;
        while done < n {
            let got = pfsc
                .read_at(&mut *self.r, offset + done as u64, &mut buf[done..])
                .map_err(|x| prefix(&self.label, x))?;
            done += got;
        }
        Ok(buf)
    }

    fn describe(&self) -> String {
        format!("PFS image {}", self.label)
    }
}

/// What the walk found.
struct Walked {
    header: PfsHeader,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    index: HashMap<String, Extent>,
}

/// The header, checked.
fn read_header(r: &mut dyn ReadSeek) -> Result<PfsHeader> {
    let image_len = r.seek(SeekFrom::End(0))?;
    let mut h = [0u8; 0x400];
    r.seek(SeekFrom::Start(0))?;
    if image_len < h.len() as u64 || r.read_exact(&mut h).is_err() {
        return Err(err("not a PFS image (too short)"));
    }
    let version = le64(&h, 0);
    if !(1..=2).contains(&version) || le64(&h, 8) != PFS_MAGIC {
        return Err(err("not a PFS image (version/magic)"));
    }
    let mode = le16(&h, 0x1C);
    for (bit, what) in [
        (MODE_SIGNED, "signed"),
        (MODE_64BIT_INODES, "64-bit-inode"),
        (MODE_ENCRYPTED, "encrypted"),
    ] {
        if mode & bit != 0 {
            return Err(err(format!("{what} PFS images are not supported")));
        }
    }
    let block_size = le32(&h, 0x20);
    if !block_size.is_power_of_two() || !(0x1000..=0x10000).contains(&block_size) {
        return Err(err(format!("unsupported PFS block size {block_size}")));
    }
    let inodes = le64(&h, 0x30);
    if inodes == 0 || inodes > MAX_INODES {
        return Err(err(format!(
            "the inode table declares {inodes} inodes (at most {MAX_INODES} are read)"
        )));
    }
    let inode_blocks = le64(&h, 0x40);
    let per_block = u64::from(block_size) / INODE_LEN as u64;
    let ndblock = le64(&h, 0x38);
    let limit = ndblock.min(image_len / u64::from(block_size));
    if inode_blocks < inodes.div_ceil(per_block) || inode_blocks >= limit {
        return Err(err("the inode table does not fit the image"));
    }
    Ok(PfsHeader {
        version,
        mode,
        block_size,
        inodes,
        inode_blocks,
        ndblock,
        image_len,
    })
}

/// The inode table, read a block at a time.
fn read_inodes(r: &mut dyn ReadSeek, h: &PfsHeader) -> Result<Vec<Inode>> {
    let bs = u64::from(h.block_size);
    let per_block = bs / INODE_LEN as u64;
    let mut inodes = Vec::with_capacity(h.inodes as usize);
    let mut block = vec![0u8; bs as usize];
    for b in 0..h.inodes.div_ceil(per_block) {
        r.seek(SeekFrom::Start((1 + b) * bs))?;
        r.read_exact(&mut block)?;
        for i in 0..per_block.min(h.inodes - b * per_block) as usize {
            let d = &block[i * INODE_LEN..(i + 1) * INODE_LEN];
            let rest: Vec<i32> = (1..12).map(|k| le32(d, 0x64 + k * 4) as i32).collect();
            let ib_clear = (0..5).all(|k| le32(d, 0x94 + k * 4) == 0);
            let fill = rest.iter().all(|&p| p == rest[0]) && ib_clear;
            inodes.push(Inode {
                mode: le16(d, 0),
                flags: le32(d, 4),
                size: le64(d, 8),
                size_compressed: le64(d, 16),
                blocks: u64::from(le32(d, 0x60)),
                db0: i64::from(le32(d, 0x64) as i32),
                fill: fill.then_some(rest[0]),
            });
        }
    }
    Ok(inodes)
}

/// Where node `n`'s bytes are: `(offset, stored length, logical length, compressed)`.
/// Refuses any layout but one contiguous run inside the image.
fn extent(h: &PfsHeader, inodes: &[Inode], n: u32, what: &str) -> Result<Extent> {
    let bad = |why: &str| err(format!("{what} (inode {n}): {why}"));
    let ino = inodes
        .get(n as usize)
        .ok_or_else(|| bad("inode number out of range"))?;
    let compressed = ino.flags & FLAG_COMPRESSED != 0;
    let (stored, len) = if compressed {
        (ino.size, ino.size_compressed)
    } else {
        if ino.size != ino.size_compressed {
            return Err(bad("unsupported PFS layout (two sizes on a plain file)"));
        }
        (ino.size, ino.size)
    };
    if stored > i64::MAX as u64 || len > i64::MAX as u64 {
        return Err(bad("size out of range"));
    }
    let bs = u64::from(h.block_size);
    let limit = h.ndblock.min(h.image_len / bs);
    let in_image = ino.db0 > 0
        && (ino.db0 as u64)
            .checked_add(ino.blocks)
            .is_some_and(|e| e <= limit);
    let fill_ok = ino.fill == Some(-1) || (n == 0 && ino.fill == Some(0));
    if !fill_ok || ino.blocks != stored.div_ceil(bs).max(1) || !in_image {
        return Err(bad("unsupported PFS pointer layout"));
    }
    Ok(Extent {
        at: ino.db0 as u64 * bs,
        stored,
        len,
        compressed,
    })
}

/// One directory's entries, streamed from its run, checked: `(inode, type, name)`. More than
/// `budget` of them (plus `.` and `..`) is an error before they are all held.
fn dir_entries(
    r: &mut dyn ReadSeek,
    e: Extent,
    what: &str,
    budget: u64,
) -> Result<Vec<(u32, i32, String)>> {
    let bad = |why: String| err(format!("{what}: {why}"));
    if e.compressed {
        return Err(bad("a compressed directory is not supported".into()));
    }
    if e.stored > MAX_DIR_BYTES {
        return Err(bad(format!("{} bytes of entries, over 64 MiB", e.stored)));
    }
    r.seek(SeekFrom::Start(e.at))?;
    let mut r = BufReader::with_capacity(BLOCK as usize, (&mut *r).take(e.stored));
    let mut out = Vec::new();
    let mut at = 0u64;
    let mut head = [0u8; 16];
    while at + 16 <= e.stored {
        r.read_exact(&mut head)?;
        if head == [0; 16] {
            break;
        }
        let (ino, kind) = (le32(&head, 0), le32(&head, 4) as i32);
        let (name_len, len) = (le32(&head, 8) as i32, le32(&head, 12) as i32);
        let fits = name_len > 0
            && len % 8 == 0
            && i64::from(len) >= 16 + i64::from(name_len)
            && at + len as u64 <= e.stored;
        if !fits {
            return Err(bad(format!("damaged directory entry at byte {at}")));
        }
        let mut rest = vec![0u8; len as usize - 16];
        r.read_exact(&mut rest)?;
        let name = String::from_utf8(rest[..name_len as usize].to_vec())
            .map_err(|_| bad(format!("an entry name at byte {at} is not UTF-8")))?;
        if name.contains(['/', '\0']) {
            return Err(bad(format!("entry name {name:?} holds '/' or NUL")));
        }
        out.push((ino, kind, name));
        if out.len() as u64 > budget + 2 {
            return Err(err(format!(
                "the image lists more than {MAX_ENTRIES} files and directories"
            )));
        }
        at += len as u64;
    }
    Ok(out)
}

fn walk(r: &mut dyn ReadSeek) -> Result<Walked> {
    let header = read_header(r)?;
    let inodes = read_inodes(r, &header)?;
    let is_dir = |n: u32| {
        inodes
            .get(n as usize)
            .is_some_and(|i| i.mode & MODE_DIR != 0)
    };
    let is_file = |n: u32| {
        inodes
            .get(n as usize)
            .is_some_and(|i| i.mode & MODE_FILE != 0)
    };

    let super_root = extent(&header, &inodes, 0, "the super-root")?;
    if !is_dir(0) {
        return Err(err("the super-root is not a directory"));
    }
    let specials = dir_entries(r, super_root, "the super-root", 16)?;
    let named = |kind: i32, name: &str| -> Vec<u32> {
        specials
            .iter()
            .filter(|(_, k, n)| *k == kind && n == name)
            .map(|(ino, ..)| *ino)
            .collect()
    };
    let [uroot] = named(DIRENT_DIR, "uroot")[..] else {
        return Err(err("the super-root must list exactly one uroot"));
    };
    let fpt = match named(DIRENT_FILE, "flat_path_table")[..] {
        [ino] => ino,
        [] => return Err(err(format!("the image has no flat path table ({CONSOLE})"))),
        _ => return Err(err("the super-root lists two flat path tables")),
    };
    let resolver = match named(DIRENT_FILE, "collision_resolver")[..] {
        [] => None,
        [ino] => Some(ino),
        _ => return Err(err("the super-root lists two collision resolvers")),
    };
    let case_insensitive = header.mode & MODE_CASE_INSENSITIVE != 0;
    let mut paths = Vec::new();

    let mut files = Vec::new();
    let mut empty_dirs = Vec::new();
    let mut index = HashMap::new();
    let mut seen_dirs = HashSet::from([uroot]);
    let (mut entries, mut path_bytes) = (0u64, 0u64);
    // (inode, parent inode, path, depth); uroot is its own parent.
    let mut stack = vec![(uroot, uroot, String::new(), 0u32)];
    while let Some((dir, parent, path, depth)) = stack.pop() {
        let what = if path.is_empty() {
            "/".to_string()
        } else {
            format!("/{path}")
        };
        if !is_dir(dir) {
            return Err(err(format!("{what} (inode {dir}) is not a directory")));
        }
        let ext = extent(&header, &inodes, dir, &what)?;
        let list = dir_entries(r, ext, &what, MAX_ENTRIES - entries)?;
        let (mut dot, mut dotdot, mut kids) = (0, 0, 0);
        let mut names = HashSet::new();
        for (ino, kind, name) in list {
            let bad = |why: &str| err(format!("{what}: entry {name:?} {why}"));
            match kind {
                DIRENT_DOT if name == "." && ino == dir => dot += 1,
                DIRENT_DOTDOT if name == ".." && ino == parent => dotdot += 1,
                DIRENT_DOT | DIRENT_DOTDOT => return Err(bad("is a wrong '.' or '..' entry")),
                DIRENT_DIR | DIRENT_FILE => {
                    if name == "." || name == ".." {
                        return Err(bad("is not a '.' or '..' entry"));
                    }
                    if !names.insert(name.clone()) {
                        return Err(bad("is listed twice"));
                    }
                    entries += 1;
                    if entries > MAX_ENTRIES {
                        return Err(err(format!(
                            "the image lists more than {MAX_ENTRIES} files and directories"
                        )));
                    }
                    let child = if path.is_empty() {
                        name
                    } else {
                        format!("{path}/{name}")
                    };
                    path_bytes += child.len() as u64;
                    if path_bytes > MAX_PATH_BYTES {
                        return Err(err("the image's paths are over 256 MiB"));
                    }
                    kids += 1;
                    let slashed = format!("/{child}");
                    paths.push(PathEntry {
                        hash: fpt_hash(&slashed, case_insensitive),
                        path: slashed,
                        ino,
                        dir: kind == DIRENT_DIR,
                    });
                    if kind == DIRENT_DIR {
                        if !is_dir(ino) {
                            return Err(err(format!(
                                "/{child}: listed as a directory, inode {ino} is not one"
                            )));
                        }
                        if !seen_dirs.insert(ino) {
                            return Err(err(format!(
                                "/{child}: directory inode {ino} is reached twice (a cycle?)"
                            )));
                        }
                        if depth >= MAX_DEPTH {
                            return Err(err(format!(
                                "/{child}: nests deeper than {MAX_DEPTH} levels"
                            )));
                        }
                        stack.push((ino, dir, child, depth + 1));
                    } else {
                        if !is_file(ino) {
                            return Err(err(format!(
                                "/{child}: listed as a file, inode {ino} is not one"
                            )));
                        }
                        let e = extent(&header, &inodes, ino, &format!("/{child}"))?;
                        files.push(SourceFile {
                            path: child.clone(),
                            size: e.len,
                        });
                        index.insert(child, e);
                    }
                }
                _ => return Err(bad(&format!("has unknown type {kind}"))),
            }
        }
        if (dot, dotdot) != (1, 1) {
            return Err(err(format!(
                "{what}: needs exactly one '.' and one '..' entry"
            )));
        }
        if kids == 0 && !path.is_empty() {
            empty_dirs.push(path);
        }
    }
    check_path_table(r, &header, &inodes, paths, fpt, resolver)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    empty_dirs.sort();
    Ok(Walked {
        header,
        files,
        empty_dirs,
        index,
    })
}

/// Why the path table matters, for its errors.
const CONSOLE: &str = "the console looks files up through it";

/// The flat path table and collision resolver must be byte for byte what the writer (and
/// MkPFS) encode for the walked tree; the resolver exists iff two paths share a hash. Each is
/// read only when its length is the expected one, so the expected length bounds the read.
fn check_path_table(
    r: &mut dyn ReadSeek,
    h: &PfsHeader,
    inodes: &[Inode],
    paths: Vec<PathEntry>,
    fpt: u32,
    resolver: Option<u32>,
) -> Result<()> {
    let mut problems = Vec::new();
    let (want_fpt, want_resolver) = path_table(paths, &mut problems);
    let mismatch = || {
        err(format!(
            "the flat path table does not match the directory tree ({CONSOLE})"
        ))
    };
    if !problems.is_empty() || want_resolver.is_some() != resolver.is_some() {
        return Err(mismatch());
    }
    let resolver = resolver.zip(want_resolver);
    let nodes = [(fpt, want_fpt, "the flat path table")]
        .into_iter()
        .chain(resolver.map(|(n, want)| (n, want, "the collision resolver")));
    for (n, want, what) in nodes {
        let e = extent(h, inodes, n, what)?;
        let is_file = inodes
            .get(n as usize)
            .is_some_and(|i| i.mode & MODE_FILE != 0);
        if !is_file || e.compressed || e.stored != want.len() as u64 {
            return Err(mismatch());
        }
        let mut got = vec![0u8; want.len()];
        r.seek(SeekFrom::Start(e.at))?;
        r.read_exact(&mut got)?;
        if got != want {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// A PFSC container inside a file's extent. Its offset table is read lazily, the two entries
/// a block needs at a time, and checked as it is read; decoded blocks are cached.
struct Pfsc {
    /// The container's start, its stored length and the logical length.
    at: u64,
    stored: u64,
    len: u64,
    blocks: u64,
    data_at: u64,
    /// Decoded blocks, most recently used last.
    cache: Vec<(u64, Vec<u8>)>,
}

impl Pfsc {
    /// Checks the header and both ends of the table against the extent.
    fn open(r: &mut dyn ReadSeek, e: Extent) -> Result<Self> {
        let bad = |why: &str| err(format!("damaged PFSC container: {why}"));
        if e.stored < PFSC_HEADER_LEN as u64 {
            return Err(bad("shorter than its header"));
        }
        let mut h = [0u8; PFSC_HEADER_LEN];
        r.seek(SeekFrom::Start(e.at))?;
        r.read_exact(&mut h)?;
        let blocks = e.len.div_ceil(BLOCK);
        if le32(&h, 0) != PFSC_MAGIC || le32(&h, 0x0C) != BLOCK as u32 || le64(&h, 0x10) != BLOCK {
            return Err(bad("no PFSC header with 64 KiB blocks"));
        }
        if blocks.checked_mul(BLOCK) != Some(le64(&h, 0x28)) || le64(&h, 0x18) != PFSC_TABLE_AT {
            return Err(bad("its length or table position disagrees with the file"));
        }
        let data_at = le64(&h, 0x20);
        let table_end = blocks
            .checked_add(1)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(PFSC_TABLE_AT));
        if table_end.is_none_or(|t| t > data_at) || data_at > e.stored {
            return Err(bad("its block table does not fit"));
        }
        let p = Self {
            at: e.at,
            stored: e.stored,
            len: e.len,
            blocks,
            data_at,
            cache: Vec::new(),
        };
        if p.entry(r, 0)? != data_at || p.entry(r, blocks)? != e.stored {
            return Err(bad("its block table does not span the container"));
        }
        Ok(p)
    }

    fn entry(&self, r: &mut dyn ReadSeek, i: u64) -> Result<u64> {
        let mut b = [0u8; 8];
        r.seek(SeekFrom::Start(self.at + PFSC_TABLE_AT + i * 8))?;
        r.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Block `n`'s stored span, checked.
    fn span(&self, r: &mut dyn ReadSeek, n: u64) -> Result<(u64, u64)> {
        let mut b = [0u8; 16];
        r.seek(SeekFrom::Start(self.at + PFSC_TABLE_AT + n * 8))?;
        r.read_exact(&mut b)?;
        let (start, end) = (le64(&b, 0), le64(&b, 8));
        if start < self.data_at || end <= start || end - start > BLOCK || end > self.stored {
            return Err(err(format!(
                "damaged PFSC container: block {n} has a bad span"
            )));
        }
        Ok((start, end))
    }

    /// Every span, read in one pass: the count of compressed blocks.
    fn compressed_blocks(&self, r: &mut dyn ReadSeek) -> Result<u64> {
        r.seek(SeekFrom::Start(self.at + PFSC_TABLE_AT))?;
        let mut t = BufReader::with_capacity(BLOCK as usize, &mut *r);
        let mut b = [0u8; 8];
        t.read_exact(&mut b)?;
        let mut prev = u64::from_le_bytes(b);
        let mut n = 0;
        for i in 0..self.blocks {
            t.read_exact(&mut b)?;
            let next = u64::from_le_bytes(b);
            if next <= prev || next - prev > BLOCK || next > self.stored {
                return Err(err(format!(
                    "damaged PFSC container: block {i} has a bad span"
                )));
            }
            n += u64::from(next - prev < BLOCK);
            prev = next;
        }
        Ok(n)
    }

    /// Decoded block `n`: a 64 KiB span is raw, a shorter one inflates to exactly 64 KiB.
    fn block(&mut self, r: &mut dyn ReadSeek, n: u64) -> Result<&[u8]> {
        if let Some(i) = self.cache.iter().position(|(b, _)| *b == n) {
            let hit = self.cache.remove(i);
            self.cache.push(hit);
        } else {
            let (start, end) = self.span(r, n)?;
            let mut stored = vec![0u8; (end - start) as usize];
            r.seek(SeekFrom::Start(self.at + start))?;
            r.read_exact(&mut stored)?;
            let plain = if stored.len() == BLOCK as usize {
                stored
            } else {
                let mut d = Vec::with_capacity(BLOCK as usize);
                let inflated = ZlibDecoder::new(&stored[..])
                    .take(BLOCK + 1)
                    .read_to_end(&mut d);
                if inflated.is_err() || d.len() != BLOCK as usize {
                    return Err(err(format!(
                        "damaged PFSC container: block {n} does not inflate to 64 KiB"
                    )));
                }
                d
            };
            if self.cache.len() >= CACHE_BLOCKS {
                self.cache.remove(0);
            }
            self.cache.push((n, plain));
        }
        Ok(self.cache.last().map_or(&[][..], |(_, b)| &b[..]))
    }

    /// Up to `buf.len()` logical bytes at `pos`, within one block.
    fn read_at(&mut self, r: &mut dyn ReadSeek, pos: u64, buf: &mut [u8]) -> Result<usize> {
        if pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let (n, off) = (pos / BLOCK, (pos % BLOCK) as usize);
        let want = (buf.len() as u64)
            .min(self.len - pos)
            .min(BLOCK - off as u64) as usize;
        let block = self.block(r, n)?;
        buf[..want].copy_from_slice(&block[off..off + want]);
        Ok(want)
    }
}

/// One file of an image as its own bytes: a window into the image, decoded if compressed.
struct Window {
    r: Box<dyn ReadSeek>,
    at: u64,
    len: u64,
    pos: u64,
    pfsc: Option<Pfsc>,
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let got = match &mut self.pfsc {
            Some(p) => p
                .read_at(&mut *self.r, self.pos, buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
            None => {
                let n = (buf.len() as u64).min(self.len - self.pos) as usize;
                self.r.seek(SeekFrom::Start(self.at + self.pos))?;
                self.r.read(&mut buf[..n])?
            }
        };
        self.pos += got as u64;
        Ok(got)
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let p = target.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )
        })?;
        self.pos = p;
        Ok(p)
    }
}

/// The container side of a `.ffpfsc`, for checks and inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfpfscInfo {
    /// The nested image's name (`PPSA01234.exfat`), which picks its filesystem.
    pub inner_name: String,
    /// The nested image's length.
    pub raw_size: u64,
    /// Its bytes as stored: the PFSC container, or the image itself when not compressed.
    pub stored_size: u64,
    pub compressed: bool,
    /// PFSC blocks (64 KiB of the nested image each); 0 when not compressed.
    pub blocks: u64,
    /// Of those, the ones stored compressed.
    pub compressed_blocks: u64,
    /// The outer PFS image.
    pub outer: PfsHeader,
}

/// Whether SMP would mount `name` as the image inside a `.ffpfsc`.
fn is_nested_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".exfat", ".ffpkg", ".ufs2", ".ffpfs"]
        .iter()
        .any(|ext| lower.ends_with(ext))
        || lower == "pfs_image.dat"
}

/// Opens the image inside a `.ffpfsc`: the outer PFS must hold exactly one nested image in
/// its root; that image's filesystem is picked by its name (exFAT, UFS2 or PFS) and read
/// through the vendored readers or [`PfsSource`].
pub fn open_ffpfsc(
    reader: Box<dyn ReadSeek>,
    label: &str,
) -> Result<(Box<dyn SourceTree>, FfpfscInfo)> {
    let outer = PfsSource::from_reader(reader, label.to_string())?;
    let nested: Vec<&SourceFile> = outer
        .files
        .iter()
        .filter(|f| !f.path.contains('/') && is_nested_image(&f.path))
        .collect();
    let [inner] = nested[..] else {
        let mut root: Vec<&str> = outer
            .files
            .iter()
            .filter(|f| !f.path.contains('/'))
            .map(|f| f.path.as_str())
            .collect();
        root.extend(outer.empty_dirs.iter().map(String::as_str));
        return Err(err(format!(
            "{label}: needs exactly one nested image (.exfat, .ffpkg, .ffpfs) in its root; it holds [{}]",
            root.join(", ")
        )));
    };
    let name = inner.path.clone();
    let e = outer.extent(&name)?;
    let header = outer.header.clone();
    let mut window = outer.into_reader(&name).map_err(|x| prefix(label, x))?;
    let compressed_blocks = match &window.pfsc {
        Some(p) => p
            .compressed_blocks(&mut *window.r)
            .map_err(|x| prefix(label, x))?,
        None => 0,
    };
    let info = FfpfscInfo {
        inner_name: name.clone(),
        raw_size: e.len,
        stored_size: e.stored,
        compressed: e.compressed,
        blocks: if e.compressed {
            e.len.div_ceil(BLOCK)
        } else {
            0
        },
        compressed_blocks,
        outer: header,
    };
    let inner_label = format!("{label} ({name})");
    let lower = name.to_ascii_lowercase();
    let tree: Box<dyn SourceTree> = if lower.ends_with(".exfat") {
        let volume = ExFat::from_file(PkgFile::from_reader(Box::new(window), e.len), &inner_label)?;
        Box::new(ExFatSource::from_volume(volume, inner_label)?)
    } else if lower.ends_with(".ffpkg") || lower.ends_with(".ufs2") {
        Box::new(Ufs2Source::from_reader(Box::new(window), inner_label)?)
    } else {
        Box::new(PfsSource::from_reader(Box::new(window), inner_label)?)
    };
    Ok((tree, info))
}

fn prefix(label: &str, e: Error) -> Error {
    match e {
        Error::Format(m) => Error::Format(format!("{label}: {m}")),
        Error::Io(io) => Error::Format(format!("{label}: {io}")),
        other => other,
    }
}
