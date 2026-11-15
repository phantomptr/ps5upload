//! In-place editing of a Forge-built PFS image (`.ffpfs`): the read side. [`Volume::open`]
//! checks the header is the one layout the writer (and MkPFS) makes, reads every inode, walks
//! the super-root and every directory, and cross-checks the flat path table and collision
//! resolver against the walked tree, so a later edit starts from a consistent image.
//!
//! The inodes and the tree (names and paths) are held in memory whole, within
//! [`MEMORY_LIMIT`]; [`Volume::list`] and [`Volume::lookup`] read nothing more.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

use ps5_dump_forge_patch::{Data, Op, Write};

use crate::BLOCK;
use crate::image;
use crate::image::{
    DIRENT_DIR, DIRENT_DOT, DIRENT_DOTDOT, DIRENT_FILE, FLAG_INTERNAL, FLAG_READONLY, INODE_LEN,
    INODES_PER_BLOCK, MAX_DIR_BYTES, MAX_NAME, MODE_CASE_INSENSITIVE, MODE_DIR, MODE_FILE, MODE_RX,
    PFS_MAGIC, PFS_VERSION, PathEntry, dirent, dirent_len, fpt_hash, header_block, name_problem,
    path_table,
};

/// The inodes, the tree and the transient buffers of [`Volume::open`], at most.
// ponytail: 128 MiB holds roughly a million entries with short paths; past it, rebuild.
pub const MEMORY_LIMIT: u64 = 128 << 20;
/// As the reader: deeper trees are refused.
const MAX_DEPTH: u32 = 32;

fn other_tool(why: impl std::fmt::Display) -> Vec<String> {
    vec![format!(
        "written by another tool (PFS {why}): convert it first"
    )]
}

fn damaged(why: impl std::fmt::Display) -> Vec<String> {
    vec![format!("the image is damaged (PFS {why})")]
}

fn too_big(bytes: u64) -> Vec<String> {
    vec![format!(
        "its metadata is too big to edit in place ({} MiB); rebuild it instead",
        bytes.div_ceil(1 << 20)
    )]
}

fn read_at(image: &mut File, offset: u64, buf: &mut [u8]) -> Result<(), Fail> {
    image.seek(SeekFrom::Start(offset))?;
    Ok(image.read_exact(buf)?)
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Why [`Volume::open`] stopped: an I/O error, or findings.
#[derive(Debug)]
enum Fail {
    Io(io::Error),
    Findings(Vec<String>),
}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail::Io(e)
    }
}

impl From<Vec<String>> for Fail {
    fn from(f: Vec<String>) -> Self {
        Fail::Findings(f)
    }
}

/// Bytes charged against the limit; every allocation from image data is charged first.
#[derive(Debug)]
struct Budget {
    used: u64,
    limit: u64,
}

impl Budget {
    fn charge(&mut self, bytes: u64) -> Result<(), Vec<String>> {
        self.used = self.used.saturating_add(bytes);
        if self.used > self.limit {
            return Err(too_big(self.used));
        }
        Ok(())
    }

    fn release(&mut self, bytes: u64) {
        self.used = self.used.saturating_sub(bytes);
    }

    /// Room for `more` elements in `v`, its growth charged (by capacity) before it happens;
    /// capacity at least doubles, so pushes stay amortized.
    fn reserve<T>(&mut self, v: &mut Vec<T>, more: usize) -> Result<(), Vec<String>> {
        let free = v.capacity() - v.len();
        if free < more {
            let cap = v.len().saturating_add(more).max(2 * v.capacity()).max(4);
            self.charge(((cap - v.capacity()) as u64).saturating_mul(size_of::<T>() as u64))?;
            v.reserve_exact(cap - v.len());
        }
        Ok(())
    }

    /// Room for one more element in `v`.
    fn grow<T>(&mut self, v: &mut Vec<T>) -> Result<(), Vec<String>> {
        self.reserve(v, 1)
    }
}

/// What an entry is: Forge writes files and folders only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// One D32 inode as the writer lays it out: one contiguous run of `blocks` from `block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
    pub mode: u16,
    pub nlink: u16,
    pub flags: u32,
    /// Bytes (a directory's whole run; the writer stores one size, never compressed).
    pub size: u64,
    pub block: u64,
    pub blocks: u64,
}

/// Inode `ino`'s bytes: `None` for an unused (all-zero) slot. `fill` is what `db[1..12]` hold:
/// -1 for a run, 0 for the super-root. The run must end inside the image's `ndblock` blocks,
/// so every later read of it is in bounds.
fn parse_inode(ino: u32, d: &[u8], fill: i32, ndblock: u64) -> Result<Option<Inode>, Vec<String>> {
    if d.iter().all(|&b| b == 0) {
        return Ok(None);
    }
    let bad = || other_tool(format!("inode {ino}: not one contiguous run"));
    let field = |at| le32(d, at).ok_or_else(bad);
    let (size, second) = (le64(d, 8).ok_or_else(bad)?, le64(d, 16).ok_or_else(bad)?);
    let flags = field(4)?;
    if flags & crate::image::FLAG_COMPRESSED != 0 || size != second {
        return Err(other_tool(format!("inode {ino}: compressed")));
    }
    let blocks = u64::from(field(0x60)?);
    let db0 = field(0x64)? as i32;
    let fills = (1..12).all(|k| le32(d, 0x64 + k * 4) == Some(fill as u32));
    let ib_clear = (0..5).all(|k| le32(d, 0x94 + k * 4) == Some(0));
    if !fills || !ib_clear || db0 <= 0 || blocks != size.div_ceil(BLOCK).max(1) {
        return Err(bad());
    }
    if (db0 as u64).saturating_add(blocks) > ndblock {
        return Err(damaged(format!("inode {ino}: its run ends past the image")));
    }
    Ok(Some(Inode {
        mode: le16(d, 0).ok_or_else(bad)?,
        nlink: le16(d, 2).ok_or_else(bad)?,
        flags,
        size,
        block: db0 as u64,
        blocks,
    }))
}

/// One directory entry of the tree, with what its inode says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// The game path: `/`-separated, no leading `/`.
    pub path: String,
    pub ino: u32,
    pub kind: Kind,
    /// A file's length; a directory's entry bytes in use (`.` and `..` included).
    pub size: u64,
    /// The directory holding the entry, the entry's absolute byte offset in the image and its
    /// on-disk length.
    pub parent: u32,
    pub offset: u64,
    pub reclen: u32,
    /// A directory's entries in [`Volume`]'s node list, in disk order.
    children: Range<usize>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}

/// A Forge-built `.ffpfs`, read and checked.
#[derive(Debug)]
pub struct Volume {
    inode_blocks: u64,
    /// The one timestamp the writer stamps (header and every inode).
    time: i64,
    ndblock: u64,
    /// Indexed by inode number; `None` is an unused slot.
    inodes: Vec<Option<Inode>>,
    uroot: u32,
    fpt: u32,
    resolver: Option<u32>,
    /// `uroot`'s entry bytes in use, and its entries in `nodes`.
    root_size: u64,
    root: Range<usize>,
    nodes: Vec<Node>,
    held: u64,
    /// Nodes a plan moved into a folder (by its inode) from outside its range of `nodes`; a
    /// node moved out keeps its place there, its `parent` changed.
    adopted: BTreeMap<u32, Vec<usize>>,
}

/// The header's `(inodes, inode_blocks, ndblock)`, if block 0 (`h`, the whole block) is the
/// one the writer makes: version 2, case-insensitive, unsigned, 32-bit inodes, unencrypted,
/// 64 KiB blocks, the inode table at block 1 described by the header's own inode, an image as
/// long as the header says, and every other byte as the writer leaves it.
fn check_header(h: &[u8], file_len: u64) -> Result<(u64, u64, u64, i64), Vec<String>> {
    let get = |at| le64(h, at).ok_or_else(|| other_tool("header missing"));
    if get(8)? != PFS_MAGIC {
        return Err(other_tool("magic"));
    }
    let version = get(0)?;
    if version != PFS_VERSION {
        return Err(other_tool(format!("version {version}")));
    }
    let mode = le16(h, 0x1C).unwrap_or(0);
    if mode != MODE_CASE_INSENSITIVE {
        return Err(other_tool(format!(
            "mode {mode:#x}: signed, encrypted, 64-bit-inode or case-sensitive"
        )));
    }
    let block_size = le32(h, 0x20).unwrap_or(0);
    if u64::from(block_size) != BLOCK {
        return Err(other_tool(format!("block size {block_size}")));
    }
    if h.get(0x1A) != Some(&1) || get(0x28)? != 1 || le32(h, 0x368) != Some(1) {
        return Err(other_tool("header flags"));
    }
    let (inodes, ndblock, inode_blocks) = (get(0x30)?, get(0x38)?, get(0x40)?);
    if ndblock.checked_mul(BLOCK) != Some(file_len) {
        return Err(damaged(format!(
            "the header says {ndblock} blocks of 64 KiB, the file is {file_len} bytes"
        )));
    }
    // The super-root, the flat path table and uroot at least.
    if !(3..=u64::from(u32::MAX)).contains(&inodes)
        || inode_blocks != inodes.div_ceil(INODES_PER_BLOCK)
        || inode_blocks >= ndblock
    {
        return Err(damaged(format!(
            "{inodes} inodes in {inode_blocks} table blocks"
        )));
    }
    // Everything else, the inode table's own descriptor at 0x50 included (flags, pointers,
    // the S64 signatures), is what the writer puts there: the block compared whole, the one
    // timestamp it stamps taken from the descriptor.
    let time = get(0x50 + 0x18)? as i64;
    if h != header_block(inodes, ndblock, inode_blocks, time) {
        return Err(other_tool("header fields"));
    }
    Ok((inodes, inode_blocks, ndblock, time))
}

/// The inode table, one block at a time, the vector charged before it is allocated.
fn read_inodes(
    image: &mut File,
    count: u64,
    ndblock: u64,
    budget: &mut Budget,
) -> Result<Vec<Option<Inode>>, Fail> {
    let bytes = count.saturating_mul(size_of::<Option<Inode>>() as u64);
    budget.charge(bytes.saturating_add(BLOCK))?;
    let mut out = Vec::with_capacity(count as usize);
    let mut block = vec![0u8; BLOCK as usize];
    for b in 0..count.div_ceil(INODES_PER_BLOCK) {
        read_at(image, (1 + b) * BLOCK, &mut block)?;
        let here = INODES_PER_BLOCK.min(count - b * INODES_PER_BLOCK) as usize;
        for (i, d) in block
            .as_chunks::<INODE_LEN>()
            .0
            .iter()
            .take(here)
            .enumerate()
        {
            let ino = (b * INODES_PER_BLOCK) as u32 + i as u32;
            out.push(parse_inode(ino, d, if ino == 0 { 0 } else { -1 }, ndblock)?);
        }
    }
    budget.release(BLOCK);
    Ok(out)
}

/// One entry as read from a directory's run.
struct Raw {
    ino: u32,
    kind: i32,
    name: String,
    /// Absolute byte offset in the image.
    at: u64,
    reclen: u32,
}

/// What [`read_dir`] returns: the entries, the bytes in use, and what they were charged.
struct Listing {
    raw: Vec<Raw>,
    used: u64,
    charged: u64,
}

/// The entries of directory run `d` (`what` names it for findings): every one as the writer
/// encodes it (its length exactly the padded `16 + name + 1`, a UTF-8 name), up to the first
/// all-zero header, and nothing but zeros after it.
fn read_dir(image: &mut File, d: &Inode, what: &str, budget: &mut Budget) -> Result<Listing, Fail> {
    if d.size > MAX_DIR_BYTES {
        return Err(other_tool(format!("{what}: over 64 MiB of entries")).into());
    }
    budget.charge(d.size)?;
    let mut buf = vec![0u8; d.size as usize];
    read_at(image, d.block * BLOCK, &mut buf)?;
    let out = parse_dir(&buf, d.block * BLOCK, what, budget);
    budget.release(d.size);
    Ok(out?)
}

fn parse_dir(
    buf: &[u8],
    base: u64,
    what: &str,
    budget: &mut Budget,
) -> Result<Listing, Vec<String>> {
    let mut l = Listing {
        raw: Vec::new(),
        used: 0,
        charged: 0,
    };
    let before = budget.used;
    let mut at = 0usize;
    while let Some(head) = buf.get(at..at + 16) {
        if head.iter().all(|&b| b == 0) {
            break;
        }
        let bad = || damaged(format!("{what}: entry at byte {at}"));
        let field = |i| le32(head, i).ok_or_else(bad);
        let (ino, kind, name_len, reclen) = (field(0)?, field(4)? as i32, field(8)?, field(12)?);
        let name_len = name_len as usize;
        if name_len == 0 || name_len > MAX_NAME || u64::from(reclen) != dirent_len(name_len) {
            return Err(bad());
        }
        let rec = buf.get(at..at + reclen as usize).ok_or_else(bad)?;
        let name = rec.get(16..16 + name_len).ok_or_else(bad)?;
        if rec
            .get(16 + name_len..)
            .is_none_or(|pad| pad.iter().any(|&b| b != 0))
        {
            return Err(bad());
        }
        budget.charge(name_len as u64)?;
        let name = String::from_utf8(name.to_vec()).map_err(|_| bad())?;
        budget.grow(&mut l.raw)?;
        l.raw.push(Raw {
            ino,
            kind,
            name,
            at: base + at as u64,
            reclen,
        });
        at += reclen as usize;
    }
    if buf
        .get(at..)
        .is_some_and(|rest| rest.iter().any(|&b| b != 0))
    {
        return Err(damaged(format!("{what}: bytes after the last entry")));
    }
    l.used = at as u64;
    l.charged = budget.used - before;
    Ok(l)
}

const DIR_MODE: u16 = MODE_DIR | MODE_RX;
const FILE_MODE: u16 = MODE_FILE | MODE_RX;
const INTERNAL: u32 = FLAG_INTERNAL | FLAG_READONLY;

/// Inode `ino`, which an entry names as `kind` with `flags` (`what` names it): in range, used,
/// of the writer's mode and flags; a directory's size its whole run, a file's link count 1.
fn expect(
    inodes: &[Option<Inode>],
    ino: u32,
    kind: Kind,
    flags: u32,
    what: &str,
) -> Result<Inode, Vec<String>> {
    let Some(Some(i)) = inodes.get(ino as usize) else {
        return Err(damaged(format!(
            "{what}: inode {ino} is out of range or unused"
        )));
    };
    let mode = if kind == Kind::Dir {
        DIR_MODE
    } else {
        FILE_MODE
    };
    if i.mode != mode || i.flags != flags {
        return Err(other_tool(format!(
            "{what}: inode {ino} mode {:#x} flags {:#x}",
            i.mode, i.flags
        )));
    }
    let shape = match kind {
        Kind::Dir => i.size == i.blocks * BLOCK,
        Kind::File => i.nlink == 1,
    };
    if !shape {
        return Err(other_tool(format!(
            "{what}: inode {ino} size or link count"
        )));
    }
    Ok(*i)
}

/// `/`-rooted, for findings and the path table.
/// `p` is a path under `q` (ASCII case folded).
fn under(p: &str, q: &str) -> bool {
    p.as_bytes().get(q.len()) == Some(&b'/')
        && p.as_bytes()
            .get(..q.len())
            .is_some_and(|h| h.eq_ignore_ascii_case(q.as_bytes()))
}

fn slashed(path: &str) -> String {
    joined(&["/", path])
}

/// `parts` concatenated into a string of exactly their length (no `format!`, whose capacity
/// may overshoot), so the budget's charge for a path is what it holds.
fn joined(parts: &[&str]) -> String {
    let mut s = String::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        s.push_str(p);
    }
    s
}

impl Volume {
    /// Reads and checks the header, the inode table, the super-root, every directory, the
    /// flat path table and the collision resolver. `Ok(Err(findings))` when the image can't be
    /// edited in place: another tool's layout, a damaged or inconsistent structure, or
    /// metadata past [`MEMORY_LIMIT`].
    pub fn open(image: &mut File) -> io::Result<Result<Volume, Vec<String>>> {
        Self::open_within(image, MEMORY_LIMIT)
    }

    fn open_within(image: &mut File, limit: u64) -> io::Result<Result<Volume, Vec<String>>> {
        match Self::load(image, limit) {
            Ok(v) => Ok(Ok(v)),
            Err(Fail::Findings(f)) => Ok(Err(f)),
            Err(Fail::Io(e)) => Err(e),
        }
    }

    fn load(image: &mut File, limit: u64) -> Result<Volume, Fail> {
        let file_len = image.metadata()?.len();
        if file_len < BLOCK {
            return Err(other_tool("header missing").into());
        }
        let mut budget = Budget { used: 0, limit };
        // The block read and the one it is compared with.
        budget.charge(2 * BLOCK)?;
        let mut h = vec![0u8; BLOCK as usize];
        read_at(image, 0, &mut h)?;
        let (count, inode_blocks, ndblock, time) = check_header(&h, file_len)?;
        drop(h);
        budget.release(2 * BLOCK);
        let inodes = read_inodes(image, count, ndblock, &mut budget)?;
        let mut v = Volume {
            inode_blocks,
            time,
            ndblock,
            inodes,
            uroot: 0,
            fpt: 1,
            resolver: None,
            root_size: 0,
            root: 0..0,
            nodes: Vec::new(),
            held: 0,
            adopted: BTreeMap::new(),
        };
        v.super_root(image, &mut budget)?;
        let mut seen = Vec::new();
        budget.charge(count)?;
        seen.resize(count as usize, false);
        for ino in [0, v.fpt, v.uroot].into_iter().chain(v.resolver) {
            if let Some(s) = seen.get_mut(ino as usize) {
                *s = true;
            }
        }
        let paths = v.walk(image, &mut budget, &mut seen)?;
        v.check_unused(&seen)?;
        budget.release(count);
        v.check_runs(&mut budget)?;
        v.check_path_table(image, &mut budget, paths)?;
        v.held = budget.used;
        Ok(v)
    }
}

impl Volume {
    /// The super-root (inode 0, one block) lists exactly what the writer puts there:
    /// `flat_path_table` (inode 1), `collision_resolver` (inode 2) when there is one, then
    /// `uroot` (the next inode).
    fn super_root(&mut self, image: &mut File, budget: &mut Budget) -> Result<(), Fail> {
        let what = "the super-root";
        let sr = expect(&self.inodes, 0, Kind::Dir, INTERNAL, what)?;
        if sr.size != BLOCK || sr.nlink != 1 {
            return Err(other_tool("the super-root's size or link count").into());
        }
        let l = read_dir(image, &sr, what, budget)?;
        let got: Vec<(u32, i32, &str)> = l
            .raw
            .iter()
            .map(|r| (r.ino, r.kind, r.name.as_str()))
            .collect();
        let fpt = (1, DIRENT_FILE, "flat_path_table");
        let resolver = (2, DIRENT_FILE, "collision_resolver");
        // The writer's two layouts, and the one a rename leaves when it makes the first
        // collision: uroot kept at 2, the resolver in a freed or spare inode past it.
        let (resolver, uroot) = match got[..] {
            [a, (2, DIRENT_DIR, "uroot")] if a == fpt => (None, 2),
            [a, b, (3, DIRENT_DIR, "uroot")] if a == fpt && b == resolver => (Some(2), 3),
            [
                a,
                (r, DIRENT_FILE, "collision_resolver"),
                (2, DIRENT_DIR, "uroot"),
            ] if a == fpt && r > 2 => (Some(r), 2),
            _ => return Err(other_tool("super-root entries").into()),
        };
        budget.release(l.charged);
        (self.resolver, self.uroot) = (resolver, uroot);
        for (ino, what) in [(1, "the flat path table")]
            .into_iter()
            .chain(self.resolver.map(|r| (r, "the collision resolver")))
        {
            expect(&self.inodes, ino, Kind::File, INTERNAL, what)?;
        }
        Ok(())
    }

    /// Every inode no entry names is an unused (all-zero) slot.
    fn check_unused(&self, seen: &[bool]) -> Result<(), Fail> {
        let stray = self
            .inodes
            .iter()
            .zip(seen)
            .position(|(i, s)| i.is_some() && !s);
        match stray {
            Some(ino) => Err(damaged(format!("inode {ino} is used by no entry")).into()),
            None => Ok(()),
        }
    }

    /// Every inode's run lies past the header and the inode table, inside the image, and no
    /// two runs share a block.
    fn check_runs(&self, budget: &mut Budget) -> Result<(), Fail> {
        let used = self.inodes.iter().flatten().count();
        let bytes = (used as u64 + 1) * size_of::<(u64, u64)>() as u64;
        budget.charge(bytes)?;
        let mut runs = Vec::with_capacity(used + 1);
        runs.push((0, 1 + self.inode_blocks));
        runs.extend(self.inodes.iter().flatten().map(|i| (i.block, i.blocks)));
        runs.sort_unstable();
        let mut end = 0u64;
        for &(block, blocks) in &runs {
            if block < end {
                return Err(damaged(format!("two runs share block {block}")).into());
            }
            end = block.saturating_add(blocks);
            if end > self.ndblock {
                return Err(damaged(format!("a run at block {block} ends past the image")).into());
            }
        }
        budget.release(bytes);
        Ok(())
    }
}

impl Volume {
    /// Walks `uroot`'s tree, every directory's entries pushed to `nodes` together (in disk
    /// order) and their range kept on the directory. Each directory starts with its `.` and
    /// `..`, names the writer accepts, no name twice (ASCII case folded, as mode 0x8 looks
    /// names up), its link count `2 + subdirectories` (`uroot`: 3 +); each inode is named once.
    /// Returns the path-table entries the tree encodes to.
    fn walk(
        &mut self,
        image: &mut File,
        budget: &mut Budget,
        seen: &mut [bool],
    ) -> Result<Vec<PathEntry>, Fail> {
        let mut paths: Vec<PathEntry> = Vec::new();
        // (directory inode, its parent's, its node (`None`: uroot), depth)
        let mut stack = vec![(self.uroot, self.uroot, None::<usize>, 0u32)];
        while let Some((dir, parent, node, depth)) = stack.pop() {
            // This directory's path and its `/`-rooted form, held while it is read.
            let held = node
                .and_then(|n| self.nodes.get(n))
                .map_or(0, |n| 2 * n.path.len() as u64 + 1);
            budget.charge(held)?;
            let path = match node {
                Some(n) => self
                    .nodes
                    .get(n)
                    .map_or(String::new(), |n| joined(&[&n.path])),
                None => String::new(),
            };
            let what = slashed(&path);
            let d = expect(&self.inodes, dir, Kind::Dir, FLAG_READONLY, &what)?;
            let Listing { raw, used, charged } = read_dir(image, &d, &what, budget)?;
            let mut raw = raw.into_iter();
            let dots = (raw.next(), raw.next());
            let ok = matches!(&dots, (Some(a), Some(b))
                if (a.ino, a.kind, a.name.as_str()) == (dir, DIRENT_DOT, ".")
                    && (b.ino, b.kind, b.name.as_str()) == (parent, DIRENT_DOTDOT, ".."));
            if !ok {
                return Err(other_tool(format!("{what}: its '.' and '..' entries")).into());
            }
            let first = self.nodes.len();
            let mut subdirs = 0u64;
            for r in raw {
                let kind = match r.kind {
                    DIRENT_DIR => Kind::Dir,
                    DIRENT_FILE => Kind::File,
                    k => return Err(other_tool(format!("{what}: entry type {k}")).into()),
                };
                if let Some(why) = name_problem(&r.name) {
                    return Err(other_tool(format!("{what}: {:?}: {why}", r.name)).into());
                }
                // The node's name and path, and the path entry's `/`-rooted path, before
                // they are made.
                let child_len = path.len() + usize::from(!path.is_empty()) + r.name.len();
                budget.charge((r.name.len() + 2 * child_len + 1) as u64)?;
                let child = if path.is_empty() {
                    joined(&[&r.name])
                } else {
                    joined(&[&path, "/", &r.name])
                };
                let cwhat = slashed(&child);
                // Anything past the exact length an allocator rounded up to, charged too.
                let over = (child.capacity() + cwhat.capacity()).saturating_sub(2 * child_len + 1);
                budget.charge(over as u64)?;
                match seen.get_mut(r.ino as usize) {
                    Some(s) if !*s => *s = true,
                    _ => {
                        let why =
                            format!("{cwhat}: inode {} is named twice or out of range", r.ino);
                        return Err(damaged(why).into());
                    }
                }
                let i = expect(&self.inodes, r.ino, kind, FLAG_READONLY, &cwhat)?;
                if kind == Kind::Dir {
                    subdirs += 1;
                    if depth >= MAX_DEPTH {
                        let why = format!("{cwhat}: nests deeper than {MAX_DEPTH} levels");
                        return Err(other_tool(why).into());
                    }
                }
                budget.grow(&mut self.nodes)?;
                budget.grow(&mut paths)?;
                paths.push(PathEntry {
                    hash: fpt_hash(&cwhat, true),
                    path: cwhat,
                    ino: r.ino,
                    dir: kind == Kind::Dir,
                });
                self.nodes.push(Node {
                    name: r.name,
                    path: child,
                    ino: r.ino,
                    kind,
                    size: i.size,
                    parent: dir,
                    offset: r.at,
                    reclen: r.reclen,
                    children: 0..0,
                });
            }
            budget.release(charged);
            let range = first..self.nodes.len();
            self.check_unique(&range, &what, budget)?;
            let base = if node.is_none() { 3 } else { 2 };
            if u64::from(d.nlink) != base + subdirs {
                return Err(other_tool(format!("{what}: link count {}", d.nlink)).into());
            }
            match node.and_then(|n| self.nodes.get_mut(n)) {
                Some(n) => (n.children, n.size) = (range.clone(), used),
                None => (self.root, self.root_size) = (range.clone(), used),
            }
            budget.reserve(&mut stack, subdirs as usize)?;
            for k in range {
                if let Some(n) = self.nodes.get(k).filter(|n| n.is_dir()) {
                    stack.push((n.ino, dir, Some(k), depth + 1));
                }
            }
            budget.release(held);
        }
        Ok(paths)
    }
}

impl Volume {
    /// No two entries of `range` (one directory) share a name, ASCII case folded.
    fn check_unique(
        &self,
        range: &Range<usize>,
        what: &str,
        budget: &mut Budget,
    ) -> Result<(), Fail> {
        let kids = self.nodes.get(range.clone()).unwrap_or_default();
        let bytes = (kids.len() * size_of::<&str>()) as u64;
        budget.charge(bytes)?;
        let mut names: Vec<&str> = kids.iter().map(|n| n.name.as_str()).collect();
        names.sort_unstable_by(|a, b| {
            let (a, b) = (a.bytes(), b.bytes());
            a.map(|c| c.to_ascii_lowercase())
                .cmp(b.map(|c| c.to_ascii_lowercase()))
        });
        if let Some(w) = names.windows(2).find(|w| w[0].eq_ignore_ascii_case(w[1])) {
            return Err(damaged(format!("{what}: lists {:?} and {:?}", w[0], w[1])).into());
        }
        budget.release(bytes);
        Ok(())
    }

    /// The flat path table and collision resolver are byte for byte what the writer encodes
    /// for the walked tree (the console looks files up through them). A resolver is there
    /// when two paths share a hash; one that lists nothing (size 0) is kept by an edit that
    /// removed the last shared hash, so inode numbers never move. Each is read only at the
    /// expected length. What `path_table` allocates (sort scratch, the table, the resolver's
    /// growth) and both reads are charged first.
    fn check_path_table(
        &self,
        image: &mut File,
        budget: &mut Budget,
        mut paths: Vec<PathEntry>,
    ) -> Result<(), Fail> {
        paths.sort_unstable_by_key(|e| (e.hash, e.ino));
        let mut resolver_len = 0u64;
        let mut i = 0;
        while let Some(first) = paths.get(i) {
            let group = paths
                .get(i..)
                .unwrap_or_default()
                .iter()
                .take_while(|e| e.hash == first.hash);
            let (n, bytes) = group.fold((0usize, 0u64), |(n, b), e| {
                (n + 1, b + dirent_len(e.path.len()))
            });
            if n > 1 {
                resolver_len += bytes + 0x18;
            }
            i += n;
        }
        let n = paths.len() as u64;
        let fpt_bound = n * 8;
        let bound = n * size_of::<PathEntry>() as u64 + fpt_bound + 2 * resolver_len + 64;
        budget.charge(bound)?;
        let mut problems = Vec::new();
        let (fpt, resolver) = path_table(paths, &mut problems);
        let mismatch = || damaged("the flat path table does not match the directory tree");
        if !problems.is_empty() {
            return Err(mismatch().into());
        }
        let tables = match (self.resolver, resolver) {
            (Some(ino), Some(want)) => vec![(self.fpt, fpt), (ino, want)],
            (Some(ino), None) => vec![(self.fpt, fpt), (ino, Vec::new())],
            (None, None) => vec![(self.fpt, fpt)],
            (None, Some(_)) => return Err(mismatch().into()),
        };
        for (ino, want) in tables {
            let Some(Some(i)) = self.inodes.get(ino as usize) else {
                return Err(mismatch().into());
            };
            if i.size != want.len() as u64 {
                return Err(mismatch().into());
            }
            budget.charge(want.len() as u64)?;
            let mut got = vec![0u8; want.len()];
            read_at(image, i.block * BLOCK, &mut got)?;
            if got != want {
                return Err(mismatch().into());
            }
            budget.release(want.len() as u64);
        }
        budget.release(bound);
        Ok(())
    }
}

impl Volume {
    /// The entries of the directory at game path `dir` (`""` is the root), `.` and `..` left
    /// out, in disk order.
    pub fn list(&self, dir: &str) -> io::Result<&[Node]> {
        let range = if dir.is_empty() {
            self.root.clone()
        } else {
            match self.lookup(dir)? {
                Some(n) if n.is_dir() => n.children.clone(),
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!("{dir}: not a directory"),
                    ));
                }
                None => return Err(io::Error::new(io::ErrorKind::NotFound, dir.to_string())),
            }
        };
        self.nodes
            .get(range)
            .ok_or_else(|| io::Error::other("internal error: a directory's entries"))
    }

    /// The node at game path `path` (`/`-separated), or `None` when it does not exist (or a
    /// parent is a file). Names compare with ASCII case folded, as the image's mode 0x8 (and
    /// the console) looks them up; [`Volume::open`] refused a folder with two such names.
    pub fn lookup(&self, path: &str) -> io::Result<Option<&Node>> {
        Ok(self.locate(path)?.and_then(|i| self.nodes.get(i)))
    }

    /// [`Self::lookup`]'s node as an index into `nodes`; a node a plan deleted is skipped.
    fn locate(&self, path: &str) -> io::Result<Option<usize>> {
        if path.split('/').any(str::is_empty) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad path {path:?}"),
            ));
        }
        let (mut dir, mut kids) = (self.uroot, self.root.clone());
        let mut found: Option<usize> = None;
        for part in path.split('/') {
            if found.is_some_and(|i| !self.nodes.get(i).is_some_and(Node::is_dir)) {
                return Ok(None);
            }
            let hit = self.kids(dir, kids).find(|&i| {
                self.nodes
                    .get(i)
                    .is_some_and(|n| n.name.eq_ignore_ascii_case(part))
            });
            let Some(i) = hit else {
                return Ok(None);
            };
            (dir, kids) = self
                .nodes
                .get(i)
                .map_or((0, 0..0), |n| (n.ino, n.children.clone()));
            found = Some(i);
        }
        Ok(found)
    }

    /// The live entries of folder `dir` (`range`: its entries in `nodes`) as indices: those
    /// of its range still in it, in disk order, then those a plan moved in.
    fn kids(&self, dir: u32, range: Range<usize>) -> impl Iterator<Item = usize> + '_ {
        let moved = self.adopted.get(&dir).map_or(&[][..], Vec::as_slice);
        range.chain(moved.iter().copied()).filter(move |&i| {
            self.nodes
                .get(i)
                .is_some_and(|n| n.parent == dir && self.live(n.ino))
        })
    }

    /// Inode `ino` is in use (a plan clears the inodes it deletes).
    fn live(&self, ino: u32) -> bool {
        matches!(self.inodes.get(ino as usize), Some(Some(_)))
    }

    /// Inode `ino`, `None` past the table or for an unused slot.
    pub fn inode(&self, ino: u32) -> Option<&Inode> {
        self.inodes.get(ino as usize)?.as_ref()
    }

    /// Every entry of the tree, directories' entries together, in walk order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Bytes the volume holds once open (inodes and tree), as charged to the budget.
    pub fn held(&self) -> u64 {
        self.held
    }
}

/// The finding for what a `.ffpfs` can't take in place: a new file, a bigger one, a folder.
pub const NO_ROOM: &str =
    "a .ffpfs keeps every file in one run: convert it to .exfat or .ffpkg to add files";

/// The finding for renames whose paths share a path-table hash the image has no room to
/// resolve: a resolver past its run, or a first one without a free inode or blocks.
pub const SHARED_HASH: &str = "the renamed paths share a path-table hash this .ffpfs has no room \
    to resolve: rebuild it instead";

fn internal() -> String {
    "internal error: the PFS edit plan lost track of a node".to_string()
}

/// Byte offset of inode `ino`'s slot in the image.
fn inode_offset(ino: u32) -> u64 {
    let ino = u64::from(ino);
    (1 + ino / INODES_PER_BLOCK) * BLOCK + (ino % INODES_PER_BLOCK) * INODE_LEN as u64
}

/// Plans the writes that apply `ops` to the image, in order (each op sees the ones before
/// it), reading it only. Every write is `live`: a Put overwrites an existing file's run in
/// place; a Delete rewrites its folder's entries, clears the inodes and shrinks the path
/// tables, and the blocks it leaves are just unreferenced (PFS keeps no free-space map); a
/// Rename rewrites the entries of the folders it touches (within their runs), a moved
/// folder's `..` and link counts, and rebuilds the path tables. An
/// op that does not apply is a finding; so is an image Forge can't edit in place
/// ([`Volume::open`]'s findings). The image never grows. The open and the plan together,
/// the returned writes included, stay within [`MEMORY_LIMIT`] (else the "too big" finding).
pub fn plan(image: &mut File, ops: &[Op]) -> Result<Vec<Write>, Vec<String>> {
    plan_within(image, ops, MEMORY_LIMIT)
}

/// [`plan`] within `limit` bytes: the open's budget carried on, every allocation of the plan
/// (copied data, sets, folder buffers, the rebuilt path tables, the writes) charged first.
fn plan_within(image: &mut File, ops: &[Op], limit: u64) -> Result<Vec<Write>, Vec<String>> {
    match Volume::open_within(image, limit) {
        Ok(Ok(v)) => {
            let mut budget = Budget {
                used: v.held,
                limit,
            };
            v.plan(ops, &mut budget)
        }
        Ok(Err(f)) => Err(f),
        Err(e) => Err(vec![e.to_string()]),
    }
}

/// What a set or map entry is charged: B-tree nodes at half fill, with room to spare.
const SET_ENTRY: u64 = 32;
/// An inode slot's three small writes' buffers (2, 16 and 4 bytes), with allocator rounding.
const INODE_HEAP: u64 = 64;

/// An op's outcome: `Err` stops the plan (the budget, or an internal error); `Ok(Err)` is
/// the op's finding.
type Step = Result<Result<(), String>, Vec<String>>;

fn lost() -> Vec<String> {
    vec![internal()]
}

/// `ino` added to `set`, its entry charged first.
fn mark(set: &mut BTreeSet<u32>, ino: u32, budget: &mut Budget) -> Result<(), Vec<String>> {
    if !set.contains(&ino) {
        budget.charge(SET_ENTRY)?;
        set.insert(ino);
    }
    Ok(())
}

/// What a plan changed so far, written out by [`Volume::writes`].
#[derive(Default)]
struct Changes {
    /// File data, in op order.
    data: Vec<Write>,
    /// Inodes whose slot is rewritten (cleared when gone).
    inodes: BTreeSet<u32>,
    /// Folders whose entries are rewritten.
    dirs: BTreeSet<u32>,
    /// Folders moved to another parent: their `..` entry is rewritten.
    moved: BTreeSet<u32>,
    /// Something was deleted: the path tables are rebuilt.
    tables: bool,
}

impl Volume {
    fn plan(mut self, ops: &[Op], budget: &mut Budget) -> Result<Vec<Write>, Vec<String>> {
        let mut c = Changes::default();
        let mut findings: Vec<String> = Vec::new();
        for op in ops {
            let done = match op {
                Op::Delete { path } => self.delete(&mut c, budget, path)?,
                Op::Put { path, len, data } => self.put(&mut c, budget, path, *len, data)?,
                Op::Mkdir { .. } => Err(NO_ROOM.to_string()),
                Op::Rename { from, to } => self.rename(&mut c, budget, from, to)?,
            };
            if let Err(f) = done {
                budget.charge(f.capacity() as u64)?;
                budget.grow(&mut findings)?;
                findings.push(f);
            }
        }
        if !findings.is_empty() {
            return Err(findings);
        }
        self.writes(c, budget)
    }

    /// The live node at `path`, as an index into `nodes`; `Err` is a bad path's finding.
    fn find(&self, path: &str) -> Result<Option<usize>, String> {
        self.locate(path).map_err(|_| format!("{path:?}: bad path"))
    }

    fn inode_mut(&mut self, ino: u32) -> Result<&mut Inode, Vec<String>> {
        self.inodes
            .get_mut(ino as usize)
            .and_then(Option::as_mut)
            .ok_or_else(lost)
    }

    /// Deletes the file or folder at `path`: its inode cleared, and for a folder every inode
    /// under it (their entries are left: nothing references them), its folder's entries and
    /// a folder's parent link count marked for rewriting, the path tables for rebuilding.
    fn delete(&mut self, c: &mut Changes, budget: &mut Budget, path: &str) -> Step {
        let at = match self.find(path) {
            Ok(Some(at)) => at,
            Ok(None) => return Ok(Err(format!("{path}: not found"))),
            Err(f) => return Ok(Err(f)),
        };
        let node = self.nodes.get(at).ok_or_else(lost)?;
        let (parent, is_dir) = (node.parent, node.is_dir());
        let mut stack = Vec::new();
        budget.grow(&mut stack)?;
        stack.push(at);
        while let Some(i) = stack.pop() {
            let n = self.nodes.get(i).ok_or_else(lost)?;
            let (ino, range) = (n.ino, n.children.clone());
            if n.is_dir() {
                budget.reserve(&mut stack, self.kids(ino, range.clone()).count())?;
                stack.extend(self.kids(ino, range));
            }
            if let Some(slot) = self.inodes.get_mut(ino as usize) {
                *slot = None;
            }
            mark(&mut c.inodes, ino, budget)?;
        }
        budget.release((stack.capacity() * size_of::<usize>()) as u64);
        drop(stack);
        if is_dir {
            let p = self.inode_mut(parent)?;
            p.nlink = p.nlink.checked_sub(1).ok_or_else(lost)?;
            mark(&mut c.inodes, parent, budget)?;
        }
        mark(&mut c.dirs, parent, budget)?;
        c.tables = true;
        Ok(Ok(()))
    }

    /// Renames or moves the file or folder at `from` to `to` (names ASCII case folded, as the
    /// image looks them up; a change of case alone is a rename). Every folder it touches is
    /// rewritten in the writer's entry order ([`Volume::writes`]); the target folder's run
    /// (a directory never grows) must hold the new name, else the "no room" finding. A moved folder's `..` and both parents' link counts follow, as the writer sets
    /// them; every path under it changes, for the path tables rebuilt at the end.
    fn rename(&mut self, c: &mut Changes, budget: &mut Budget, from: &str, to: &str) -> Step {
        if let Some(Err(f)) = [from, to]
            .into_iter()
            .map(|p| self.find(p))
            .find(Result::is_err)
        {
            return Ok(Err(f));
        }
        let Ok(Some(at)) = self.find(from) else {
            return Ok(Err(format!("{from}: not found")));
        };
        let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
        if let Some(why) = name_problem(name) {
            return Ok(Err(format!("{to}: {why}")));
        }
        let node = self.nodes.get(at).ok_or_else(lost)?;
        let (old_parent, is_dir) = (node.parent, node.is_dir());
        let old_len = dirent_len(node.name.len());
        if is_dir && under(to, from) {
            return Ok(Err(format!("{to}: a folder can't be moved into itself")));
        }
        match self.find(to) {
            Ok(Some(j)) if j == at && node.name == name => return Ok(Ok(())),
            Ok(Some(j)) if j != at => return Ok(Err(format!("{to}: already exists"))),
            _ => {}
        }
        let (dir, range, base) = if parent.is_empty() {
            (self.uroot, self.root.clone(), None)
        } else {
            match self.find(parent) {
                Ok(Some(p)) if self.nodes.get(p).is_some_and(Node::is_dir) => {
                    let p_node = self.nodes.get(p).ok_or_else(lost)?;
                    (p_node.ino, p_node.children.clone(), Some(p))
                }
                _ => return Ok(Err(format!("{to}: folder {parent} not found"))),
            }
        };
        let moved = dir != old_parent;
        // The folder's live entries, `.` and `..` included, against its run.
        let used = dirent_len(1)
            + dirent_len(2)
            + self
                .kids(dir, range.clone())
                .filter_map(|i| self.nodes.get(i))
                .map(|n| dirent_len(n.name.len()))
                .sum::<u64>();
        let need = used + dirent_len(name.len()) - if moved { 0 } else { old_len };
        let cap = self.inode(dir).ok_or_else(lost)?.size;
        if need > cap {
            let p = slashed(parent);
            return Ok(Err(format!(
                "no room in folder {p} for the new name: rebuild it instead"
            )));
        }
        let base_path = base
            .and_then(|p| self.nodes.get(p))
            .map(|p| p.path.as_str());
        budget.charge((base_path.map_or(0, |b| b.len() + 1) + name.len()) as u64)?;
        let new_path = base_path.map_or_else(|| joined(&[name]), |b| joined(&[b, "/", name]));
        if is_dir {
            let depth = |p: &str| p.split('/').count();
            let old_path = &self.nodes.get(at).ok_or_else(lost)?.path;
            let deepest = self
                .nodes
                .iter()
                .filter(|n| n.is_dir() && self.live(n.ino) && under(&n.path, old_path))
                .map(|n| depth(&n.path) - depth(old_path))
                .max()
                .unwrap_or(0);
            if depth(&new_path) + deepest > MAX_DEPTH as usize {
                return Ok(Err(format!("{to}: nests deeper than {MAX_DEPTH} levels")));
            }
        }
        if is_dir && moved && self.inode(dir).is_none_or(|d| d.nlink == u16::MAX) {
            return Ok(Err(format!("{to}: its folder holds too many folders")));
        }
        self.moved_to(c, budget, at, dir, range.contains(&at), new_path)?;
        Ok(Ok(()))
    }

    /// Node `at` named and placed at `new_path` in folder `dir` (`own`: `at` is in `dir`'s
    /// range of `nodes`), every path under it changed, both folders' entries and the path
    /// tables marked; a folder moved to another parent gets its `..` marked and both link
    /// counts changed.
    fn moved_to(
        &mut self,
        c: &mut Changes,
        budget: &mut Budget,
        at: usize,
        dir: u32,
        own: bool,
        new_path: String,
    ) -> Result<(), Vec<String>> {
        let cut = new_path.rfind('/').map_or(0, |i| i + 1);
        let name = new_path.get(cut..).ok_or_else(lost)?;
        // The node's own copies of its name and path; the old ones go.
        budget.charge((name.len() + new_path.len()) as u64)?;
        let name = joined(&[name]);
        let node = self.nodes.get_mut(at).ok_or_else(lost)?;
        let (ino, old_parent, is_dir) = (node.ino, node.parent, node.is_dir());
        budget.release(node.name.len() as u64);
        node.name = name;
        node.parent = dir;
        let old_path = std::mem::replace(&mut node.path, joined(&[&new_path]));
        if is_dir {
            let inodes = &self.inodes;
            for n in &mut self.nodes {
                if matches!(inodes.get(n.ino as usize), Some(Some(_))) && under(&n.path, &old_path)
                {
                    let rest = n.path.get(old_path.len()..).ok_or_else(lost)?;
                    budget.charge((new_path.len() + rest.len()) as u64)?;
                    let path = joined(&[&new_path, rest]);
                    budget.release(n.path.len() as u64);
                    n.path = path;
                }
            }
        }
        if dir != old_parent {
            if let Some(v) = self.adopted.get_mut(&old_parent) {
                v.retain(|&i| i != at);
            }
            if !own {
                if !self.adopted.contains_key(&dir) {
                    budget.charge(SET_ENTRY)?;
                }
                let v = self.adopted.entry(dir).or_default();
                budget.grow(v)?;
                v.push(at);
            }
            if is_dir {
                let p = self.inode_mut(old_parent)?;
                p.nlink = p.nlink.checked_sub(1).ok_or_else(lost)?;
                let d = self.inode_mut(dir)?;
                d.nlink = d.nlink.checked_add(1).ok_or_else(lost)?;
                mark(&mut c.inodes, old_parent, budget)?;
                mark(&mut c.inodes, dir, budget)?;
                mark(&mut c.moved, ino, budget)?;
            }
        }
        mark(&mut c.dirs, old_parent, budget)?;
        mark(&mut c.dirs, dir, budget)?;
        c.tables = true;
        budget.release((old_path.len() + new_path.len()) as u64);
        Ok(())
    }

    /// Replaces the file at `path` with `len` bytes of `data`, in the run it holds: refused
    /// ([`NO_ROOM`]) for a new path or one longer than the run. The tail of its new last
    /// block is zeroed; blocks past it are left unreferenced. `Data::Bytes` is copied into
    /// the plan's write, charged first.
    fn put(
        &mut self,
        c: &mut Changes,
        budget: &mut Budget,
        path: &str,
        len: u64,
        data: &Data,
    ) -> Step {
        if data.len() != len {
            return Ok(Err(format!(
                "{path}: {len} bytes, but its data holds {}",
                data.len()
            )));
        }
        let at = match self.find(path) {
            Ok(Some(at)) => at,
            Ok(None) => return Ok(Err(NO_ROOM.to_string())),
            Err(f) => return Ok(Err(f)),
        };
        let node = self.nodes.get(at).ok_or_else(lost)?;
        if node.is_dir() {
            return Ok(Err(format!("{path} is a folder")));
        }
        let ino = node.ino;
        if len > self.inode_mut(ino)?.blocks.saturating_mul(BLOCK) {
            return Ok(Err(NO_ROOM.to_string()));
        }
        budget.reserve(&mut c.data, 2)?;
        if let Data::Bytes(b) = data {
            budget.charge(b.len() as u64)?;
        }
        mark(&mut c.inodes, ino, budget)?;
        let i = self.inode_mut(ino)?;
        let (start, blocks) = (i.block * BLOCK, len.div_ceil(BLOCK).max(1));
        (i.size, i.blocks) = (len, blocks);
        if len > 0 {
            c.data.push(live(start, data.clone()));
        }
        if blocks * BLOCK > len {
            c.data
                .push(live(start + len, Data::Zero(blocks * BLOCK - len)));
        }
        if let Some(n) = self.nodes.get_mut(at) {
            n.size = len;
        }
        Ok(Ok(()))
    }
}

fn live(offset: u64, data: Data) -> Write {
    Write {
        offset,
        data,
        live: true,
    }
}

impl Volume {
    /// The writes for `c`: file data in op order, then each changed folder's entries (`.`
    /// and `..` kept, the live ones packed after them in the writer's order: folders, then
    /// files, each by lower-cased name; zeros up to the old end), a moved folder's `..`, the
    /// path tables, then each changed inode slot.
    fn writes(mut self, c: Changes, budget: &mut Budget) -> Result<Vec<Write>, Vec<String>> {
        let mut out = c.data;
        budget.charge(c.dirs.len() as u64 * SET_ENTRY)?;
        let dirs: BTreeMap<u32, usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_dir() && c.dirs.contains(&n.ino))
            .map(|(i, n)| (n.ino, i))
            .collect();
        let dots = dirent_len(1) + dirent_len(2);
        for &d in &c.dirs {
            let Some(Some(dir)) = self.inodes.get(d as usize) else {
                continue; // deleted by a later op
            };
            let (kids, used) = if d == self.uroot {
                (self.root.clone(), self.root_size)
            } else {
                let n = dirs
                    .get(&d)
                    .and_then(|&i| self.nodes.get(i))
                    .ok_or_else(lost)?;
                (n.children.clone(), n.size)
            };
            // The entries now, against the folder's run (renames checked it), then zeros up
            // to the old end.
            let old = used.checked_sub(dots).ok_or_else(lost)?;
            let new: u64 = self
                .kids(d, kids.clone())
                .filter_map(|i| self.nodes.get(i))
                .map(|n| dirent_len(n.name.len()))
                .sum();
            if dots + new > dir.size {
                return Err(lost());
            }
            let len = old.max(new);
            if len == 0 {
                continue;
            }
            budget.grow(&mut out)?;
            budget.charge(len)?;
            // In the writer's order: folders, then files, each by lower-cased name.
            let count = self.kids(d, kids.clone()).count();
            let order_bytes = (count * size_of::<usize>()) as u64;
            budget.charge(order_bytes)?;
            let mut order = Vec::with_capacity(count);
            order.extend(self.kids(d, kids).filter_map(|i| self.nodes.get(i)));
            order.sort_by(|a, b| {
                let fold = |c: u8| c.to_ascii_lowercase();
                let (x, y) = (a.name.bytes().map(fold), b.name.bytes().map(fold));
                (!a.is_dir()).cmp(&!b.is_dir()).then_with(|| x.cmp(y))
            });
            let mut bytes = Vec::with_capacity(len as usize);
            for n in order {
                let kind = if n.is_dir() { DIRENT_DIR } else { DIRENT_FILE };
                bytes.extend_from_slice(&dirent(n.ino, kind, &n.name));
            }
            budget.release(order_bytes);
            bytes.resize(len as usize, 0);
            out.push(live(dir.block * BLOCK + dots, Data::Bytes(bytes)));
        }
        // A moved folder's `..`: its new parent's inode, the entry's first field.
        budget.reserve(&mut out, c.moved.len())?;
        budget.charge(c.moved.len() as u64 * INODE_HEAP)?;
        for &m in &c.moved {
            let Some(Some(dir)) = self.inodes.get(m as usize) else {
                continue; // deleted by a later op
            };
            let n = self.nodes.iter().find(|n| n.ino == m).ok_or_else(lost)?;
            let at = dir.block * BLOCK + dirent_len(1);
            out.push(live(at, Data::Bytes(n.parent.to_le_bytes().to_vec())));
        }
        budget.release(c.dirs.len() as u64 * SET_ENTRY);
        drop(dirs);
        let mut inodes = c.inodes;
        if c.tables {
            self.tables(&mut out, &mut inodes, budget)?;
        }
        budget.reserve(&mut out, inodes.len().saturating_mul(3))?;
        budget.charge(inodes.len() as u64 * INODE_HEAP)?;
        for ino in inodes {
            let at = inode_offset(ino);
            match self.inodes.get(ino as usize) {
                Some(Some(i)) => {
                    let blocks = u32::try_from(i.blocks).map_err(|_| lost())?;
                    let mut size = Vec::with_capacity(16);
                    size.extend(i.size.to_le_bytes());
                    size.extend(i.size.to_le_bytes());
                    out.push(live(at + 2, Data::Bytes(i.nlink.to_le_bytes().to_vec())));
                    out.push(live(at + 8, Data::Bytes(size)));
                    out.push(live(at + 0x60, Data::Bytes(blocks.to_le_bytes().to_vec())));
                }
                Some(None) => out.push(live(at, Data::Zero(INODE_LEN as u64))),
                None => return Err(lost()),
            }
        }
        Ok(out)
    }

    /// The flat path table and collision resolver of the live tree, re-hashed and re-sorted
    /// as the writer builds them, written over the old ones, zeros up to their old length.
    /// The FPT never grows (one entry per live node); the resolver may, within its run, else
    /// the [`SHARED_HASH`] finding; the first collision makes one ([`Self::make_resolver`]). A
    /// resolver left with no collision stays, empty (size 0, one block): uroot keeps its
    /// inode number and [`Volume::open`] accepts it. Charged before building: the entries,
    /// their paths, the sort's scratch, the FPT at its old length and the resolver twice at
    /// its exact length; what the tables hold stays charged.
    fn tables(
        &mut self,
        out: &mut Vec<Write>,
        inodes: &mut BTreeSet<u32>,
        budget: &mut Budget,
    ) -> Result<(), Vec<String>> {
        let (n, path_bytes) = self
            .nodes
            .iter()
            .filter(|n| self.live(n.ino))
            .fold((0u64, 0u64), |(k, b), n| {
                (k + 1, b + n.path.len() as u64 + 1)
            });
        let old = |ino: Option<u32>| ino.and_then(|i| self.inode(i)).map_or(0, |i| i.size);
        let fpt_old = old(Some(self.fpt));
        let bound = 2 * n * size_of::<PathEntry>() as u64 + path_bytes + fpt_old.max(n * 8) + 64;
        budget.charge(bound)?;
        let mut entries = Vec::with_capacity(n as usize);
        for node in self.nodes.iter().filter(|n| self.live(n.ino)) {
            let path = slashed(&node.path);
            entries.push(PathEntry {
                hash: fpt_hash(&path, true),
                path,
                ino: node.ino,
                dir: node.is_dir(),
            });
        }
        // The resolver's exact length from the hash groups, checked against its run (a
        // rename can make or lengthen a group) and charged twice (it grows by pushes).
        entries.sort_unstable_by_key(|e| (e.hash, e.ino));
        let (mut resolver_len, mut i) = (0u64, 0);
        while let Some(h) = entries.get(i).map(|e| e.hash) {
            let group = entries.get(i..).unwrap_or_default();
            let group = group.iter().take_while(|e| e.hash == h);
            let (k, bytes) = group.fold((0usize, 0u64), |(k, b), e| {
                (k + 1, b + dirent_len(e.path.len()))
            });
            if k > 1 {
                resolver_len += bytes + 0x18;
            }
            i += k;
        }
        if self.resolver.is_none() && resolver_len > 0 {
            self.make_resolver(out, resolver_len, budget)?;
        }
        let room = self
            .resolver
            .and_then(|r| self.inode(r))
            .map_or(0, |r| r.blocks * BLOCK);
        if resolver_len > room {
            return Err(vec![SHARED_HASH.to_string()]);
        }
        budget.charge(2 * resolver_len)?;
        let mut problems = Vec::new();
        let (fpt, resolver) = path_table(entries, &mut problems);
        budget.release(bound + 2 * resolver_len);
        budget.charge((fpt.capacity() + resolver.as_ref().map_or(0, Vec::capacity)) as u64)?;
        if !problems.is_empty() {
            return Err(problems);
        }
        let resolver = match (self.resolver, resolver) {
            (Some(r), got) => Some((r, got.unwrap_or_default())),
            (None, None) => None,
            // ponytail: the resolver would need a super-root entry and every inode renumbered.
            (None, Some(_)) => return Err(vec![SHARED_HASH.to_string()]),
        };
        budget.reserve(out, 4)?;
        for (ino, table) in [(self.fpt, fpt)].into_iter().chain(resolver) {
            mark(inodes, ino, budget)?;
            let i = self.inode_mut(ino)?;
            let (new, old) = (table.len() as u64, i.size);
            // A table grows only within its run (a rename can lengthen the resolver).
            if new.div_ceil(BLOCK).max(1) > i.blocks {
                return Err(vec![SHARED_HASH.to_string()]);
            }
            let start = i.block * BLOCK;
            (i.size, i.blocks) = (new, new.div_ceil(BLOCK).max(1));
            if new > 0 {
                out.push(live(start, Data::Bytes(table)));
            }
            if old > new {
                out.push(live(start + new, Data::Zero(old - new)));
            }
        }
        Ok(())
    }
}

impl Volume {
    /// The first collision's resolver, made as the writer makes it (an internal file of
    /// `len` bytes, zeros to the end of its last block) in the lowest freed inode past uroot,
    /// else a spare slot of the inode table's last block (the header's count raised), and in
    /// the first unreferenced blocks that hold it (the writer reserves one after the FPT);
    /// the super-root lists it between the FPT and uroot, which keeps inode 2. Without an
    /// inode or blocks for it, the [`SHARED_HASH`] finding. Its bytes come from the caller.
    fn make_resolver(
        &mut self,
        out: &mut Vec<Write>,
        len: u64,
        budget: &mut Budget,
    ) -> Result<(), Vec<String>> {
        let refuse = || vec![SHARED_HASH.to_string()];
        let need = len.div_ceil(BLOCK).max(1);
        let count = self.inodes.len() as u64;
        let free = self
            .inodes
            .iter()
            .enumerate()
            .skip(3)
            .find(|(_, i)| i.is_none());
        let (ino, spare) = match free {
            Some((k, _)) => (u32::try_from(k).map_err(|_| refuse())?, false),
            None if count < self.inode_blocks * INODES_PER_BLOCK => {
                (u32::try_from(count).map_err(|_| refuse())?, true)
            }
            None => return Err(refuse()),
        };
        // Every run in use (the header and inode table first), for the first gap of `need`.
        let used = self.inodes.iter().flatten().count();
        let runs_bytes = (used as u64 + 1) * 16;
        budget.charge(runs_bytes)?;
        let mut runs: Vec<(u64, u64)> = Vec::with_capacity(used + 1);
        runs.push((0, 1 + self.inode_blocks));
        runs.extend(self.inodes.iter().flatten().map(|i| (i.block, i.blocks)));
        runs.sort_unstable();
        let mut next = 0u64;
        for &(start, blocks) in &runs {
            if start >= next.saturating_add(need) {
                break;
            }
            next = next.max(start.saturating_add(blocks));
        }
        drop(runs);
        budget.release(runs_bytes);
        if next.saturating_add(need) > self.ndblock {
            return Err(refuse());
        }
        let at = next;
        budget.reserve(out, 4)?;
        budget.charge(BLOCK + 2 * INODE_HEAP + 128)?;
        if spare {
            budget.grow(&mut self.inodes)?;
            self.inodes.push(None);
            let h = header_block(count + 1, self.ndblock, self.inode_blocks, self.time);
            out.push(live(0, Data::Bytes(h)));
        }
        // A node a plan deleted that held this inode must not look live again.
        for n in self.nodes.iter_mut().filter(|n| n.ino == ino) {
            n.ino = u32::MAX;
        }
        let slot = self.inodes.get_mut(ino as usize).ok_or_else(lost)?;
        *slot = Some(Inode {
            mode: FILE_MODE,
            nlink: 1,
            flags: INTERNAL,
            size: len,
            block: at,
            blocks: need,
        });
        let i = image::inode(FILE_MODE, 1, INTERNAL, len, len, need, at, -1, self.time);
        out.push(live(inode_offset(ino), Data::Bytes(i.to_vec())));
        out.push(live(at * BLOCK + len, Data::Zero(need * BLOCK - len)));
        let sr = self.inode(0).ok_or_else(lost)?.block * BLOCK;
        let mut entries = dirent(self.fpt, DIRENT_FILE, "flat_path_table");
        entries.extend(dirent(ino, DIRENT_FILE, "collision_resolver"));
        entries.extend(dirent(self.uroot, DIRENT_DIR, "uroot"));
        out.push(live(sr, Data::Bytes(entries)));
        self.resolver = Some(ino);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write as _};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use ps5upload_fpkg::source::{SourceFile, SourceTree};

    use super::*;
    use crate::{Options, PfsSource, plan, write};

    const B: usize = BLOCK as usize;

    /// Files (path, length) and empty folders, held in memory; bytes follow the path.
    struct Mem(Vec<SourceFile>, Vec<String>);

    impl SourceTree for Mem {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn empty_dirs(&self) -> &[String] {
            &self.1
        }
        fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            self.read_range(path, 0, usize::MAX)
        }
        fn read_range(
            &mut self,
            path: &str,
            at: u64,
            len: usize,
        ) -> ps5upload_fpkg::Result<Vec<u8>> {
            let size = self.0.iter().find(|f| f.path == path).map_or(0, |f| f.size);
            let n = (size.saturating_sub(at) as usize).min(len);
            Ok((0..n as u64)
                .map(|i| (at + i) as u8 ^ path.len() as u8)
                .collect())
        }
        fn describe(&self) -> String {
            "memory".into()
        }
    }

    fn build(files: &[(&str, u64)], empty: &[&str]) -> Vec<u8> {
        let files = files.iter().map(|&(p, size)| SourceFile {
            path: p.into(),
            size,
        });
        let mut tree = Mem(
            files.collect(),
            empty.iter().map(|d| d.to_string()).collect(),
        );
        let cancel = AtomicBool::new(false);
        let opts = Options {
            time: Some(1_700_000_000),
        };
        let layout = plan(&tree, &opts, &cancel).unwrap();
        let mut out = Cursor::new(Vec::new());
        write(&mut tree, &layout, &mut out, &cancel, &mut |_, _| {}).unwrap();
        out.into_inner()
    }

    /// `img` as a temporary file, opened within `limit`.
    fn open_limit(img: &[u8], limit: u64) -> io::Result<Result<Volume, Vec<String>>> {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("forge-pfs-edit-{}-{n}.ffpfs", std::process::id()));
        File::create(&path)?.write_all(img)?;
        let mut f = File::open(&path)?;
        let v = Volume::open_within(&mut f, limit);
        std::fs::remove_file(&path)?;
        v
    }

    fn open(img: &[u8]) -> Result<Volume, Vec<String>> {
        open_limit(img, MEMORY_LIMIT).unwrap()
    }

    fn findings(img: &[u8]) -> String {
        open(img).expect_err("refused").join("\n")
    }

    fn inode_at(n: usize) -> usize {
        (1 + n / 390) * B + (n % 390) * INODE_LEN
    }

    const GAME: &[(&str, u64)] = &[
        ("eboot.bin", 70_000),
        ("sce_sys/param.json", 1),
        ("sce_sys/icon0.png", 65_536),
        ("Data/Big.Pak", 200_000),
        ("Data/sub/Empty.bin", 0),
        ("Data/sub/deeper/x", 5),
    ];

    type Change<'a> = Box<dyn Fn(&mut Vec<u8>) + 'a>;

    fn put32(img: &mut [u8], at: usize, v: u32) {
        img[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn ino_of(img: &[u8], path: &str) -> usize {
        open(img).unwrap().lookup(path).unwrap().unwrap().ino as usize
    }

    #[test]
    fn another_tools_layout_is_refused() {
        let img = build(GAME, &[]);
        let eboot = ino_of(&img, "eboot.bin");
        let cases: Vec<(&str, Change)> = vec![
            ("signed", Box::new(|i| i[0x1C] |= 0x1)),
            ("encrypted", Box::new(|i| i[0x1C] |= 0x4)),
            ("case-sensitive", Box::new(|i| i[0x1C] = 0)),
            ("PS4 version", Box::new(|i| i[0] = 1)),
            ("magic", Box::new(|i| i[8] ^= 1)),
            ("32 KiB blocks", Box::new(|i| put32(i, 0x20, 0x8000))),
            (
                "compressed file",
                Box::new(move |i| i[inode_at(eboot) + 4] |= 1),
            ),
            (
                "second run",
                Box::new(move |i| put32(i, inode_at(eboot) + 0x68, 40)),
            ),
            (
                "indirect block",
                Box::new(move |i| put32(i, inode_at(eboot) + 0x94, 40)),
            ),
            ("file mode", Box::new(move |i| i[inode_at(eboot)] ^= 0x2)),
            ("compressed table", Box::new(|i| i[0x54] |= 0x1)),
            ("table mode", Box::new(|i| i[0x51] = 0x80)),
            (
                "second table pointer",
                Box::new(|i| i[0x50 + 0x68 + 40 + 32] = 2),
            ),
            ("table signature", Box::new(|i| i[0x50 + 0x68] = 0xAA)),
            (
                "table indirect",
                Box::new(|i| i[0x50 + 0x68 + 12 * 40 + 32] = 3),
            ),
            ("a second stamp", Box::new(|i| i[0x50 + 0x20] ^= 1)),
            ("header tail", Box::new(|i| i[B - 1] = 1)),
        ];
        for (what, change) in cases {
            let mut bad = img.clone();
            change(&mut bad);
            let f = findings(&bad);
            assert!(
                f.starts_with("written by another tool (PFS ") && f.ends_with("convert it first"),
                "{what}: {f}"
            );
        }
    }

    #[test]
    fn damaged_structures_are_findings() {
        let img = build(GAME, &[]);
        let v = open(&img).unwrap();
        let node = |p: &str| v.lookup(p).unwrap().unwrap().clone();
        let (eboot, param, icon) = (
            node("eboot.bin"),
            node("sce_sys/param.json"),
            node("sce_sys/icon0.png"),
        );
        let fpt_at = v.inode(v.fpt).unwrap().block as usize * B;
        let cases: Vec<(&str, &str, Change)> = vec![
            (
                "short",
                "the header says",
                Box::new(|i| i.truncate(i.len() - B)),
            ),
            (
                "path table",
                "does not match",
                Box::new(move |i| i[fpt_at] ^= 1),
            ),
            (
                "entry length",
                "entry at byte",
                Box::new(|i| put32(i, eboot.offset as usize + 12, 9)),
            ),
            (
                "inode twice",
                "named twice",
                Box::new(|i| put32(i, eboot.offset as usize, param.ino)),
            ),
            (
                "unused inode",
                "unused",
                Box::new(|i| i[inode_at(param.ino as usize)..][..INODE_LEN].fill(0)),
            ),
            (
                "overlap",
                "share block",
                Box::new(|i| {
                    let db0 = i[inode_at(eboot.ino as usize) + 0x64..][..4].to_vec();
                    i[inode_at(icon.ino as usize) + 0x64..][..4].copy_from_slice(&db0);
                }),
            ),
            (
                "past the end",
                "past the image",
                Box::new(|i| put32(i, inode_at(icon.ino as usize) + 0x64, 1 << 20)),
            ),
            (
                "stray inode",
                "used by no entry",
                Box::new(|i| {
                    let count = u64::from_le_bytes(i[0x30..0x38].try_into().unwrap()) as usize;
                    let src = i[inode_at(param.ino as usize)..][..INODE_LEN].to_vec();
                    i[inode_at(count)..][..INODE_LEN].copy_from_slice(&src);
                    i[0x30..0x38].copy_from_slice(&(count as u64 + 1).to_le_bytes());
                }),
            ),
            (
                "tail bytes",
                "after the last entry",
                Box::new(|i| {
                    let root = i[inode_at(2) + 0x64..][..4].to_vec();
                    let at = u32::from_le_bytes(root.try_into().unwrap()) as usize * B;
                    i[at + B - 1] = 1;
                }),
            ),
        ];
        for (what, want, change) in cases {
            let mut bad = img.clone();
            change(&mut bad);
            let f = findings(&bad);
            assert!(f.contains(want), "{what}: {f}");
        }
    }

    /// Heap bytes this thread holds, and the most it held since the last [`peak_of`].
    mod counting {
        use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
        use std::cell::Cell;

        thread_local! {
            static NOW: Cell<i64> = const { Cell::new(0) };
            static PEAK: Cell<i64> = const { Cell::new(0) };
        }

        fn add(d: i64) {
            let _ = NOW.try_with(|n| {
                let v = n.get() + d;
                n.set(v);
                let _ = PEAK.try_with(|p| p.set(p.get().max(v)));
            });
        }

        pub struct Counting;

        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, l: AllocLayout) -> *mut u8 {
                let p = unsafe { System.alloc(l) };
                if !p.is_null() {
                    add(l.size() as i64);
                }
                p
            }
            unsafe fn alloc_zeroed(&self, l: AllocLayout) -> *mut u8 {
                let p = unsafe { System.alloc_zeroed(l) };
                if !p.is_null() {
                    add(l.size() as i64);
                }
                p
            }
            unsafe fn dealloc(&self, p: *mut u8, l: AllocLayout) {
                unsafe { System.dealloc(p, l) };
                add(-(l.size() as i64));
            }
            unsafe fn realloc(&self, p: *mut u8, l: AllocLayout, new: usize) -> *mut u8 {
                let q = unsafe { System.realloc(p, l, new) };
                if !q.is_null() {
                    add(new as i64 - l.size() as i64);
                }
                q
            }
        }

        /// `f`'s result and the most heap it held at once on this thread, in bytes.
        pub fn peak_of<R>(f: impl FnOnce() -> R) -> (R, u64) {
            let base = NOW.with(|n| n.get());
            PEAK.with(|p| p.set(base));
            let r = f();
            (r, (PEAK.with(|p| p.get()) - base).max(0) as u64)
        }
    }

    #[global_allocator]
    static ALLOC: counting::Counting = counting::Counting;

    /// The heap `Volume::open_within` really uses stays within its limit at every limit, so the
    /// budget is charged before each allocation, not after: a large folder (long names, so
    /// names, paths and the directory buffer dominate) and a tree with a collision.
    #[test]
    fn the_budget_bounds_the_real_heap() {
        // Findings, the file handle, the walk's first stack slot: not charged.
        const SLACK: u64 = 16 << 10;
        let long: Vec<String> = (0..1500)
            .map(|k| format!("dir/{k:04}{}", "n".repeat(200)))
            .collect();
        // Short names under a long parent path (three 250-byte folders): a path built with
        // more capacity than its length would add up past the charge here.
        let parent = ["p", "q", "r"].map(|c| c.repeat(250)).join("/");
        let deep: Vec<String> = (0..3000).map(|k| format!("{parent}/{k:04}")).collect();
        let mut files: Vec<(&str, u64)> =
            long.iter().chain(&deep).map(|p| (p.as_str(), 1)).collect();
        files.extend([("A_", 3), ("B@", 4), ("eboot.bin", 5)]);
        let img = build(&files, &["empty"]);
        let path =
            std::env::temp_dir().join(format!("forge-pfs-peak-{}.ffpfs", std::process::id()));
        std::fs::write(&path, &img).unwrap();
        let mut f = File::open(&path).unwrap();
        let (v, top) = counting::peak_of(|| Volume::open(&mut f).unwrap().unwrap());
        assert!(v.held() > 1 << 20 && top >= v.held(), "{} {top}", v.held());
        drop(v);
        // Every 4 KiB up to the peak, so each step of the open is cut off somewhere.
        let limits = (0..top.div_ceil(4 << 10)).map(|k| k * (4 << 10));
        for limit in limits.chain([top - 1, top, top + 1, MEMORY_LIMIT]) {
            let (r, peak) =
                counting::peak_of(|| Volume::open_within(&mut f, limit).unwrap().map(|_| ()));
            assert!(
                peak <= limit + SLACK,
                "limit {limit}: {peak} bytes held ({r:?})"
            );
            if let Err(f) = r {
                assert!(
                    f.len() == 1 && f[0].contains("too big to edit in place"),
                    "{f:?}"
                );
            }
        }
        std::fs::remove_file(&path).unwrap();
    }

    /// `super::plan` over `img` as a temporary file.
    fn planned(img: &[u8], ops: &[Op]) -> Result<Vec<Write>, Vec<String>> {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("forge-pfs-plan-{}-{n}.ffpfs", std::process::id()));
        File::create(&path).unwrap().write_all(img).unwrap();
        let mut f = File::open(&path).unwrap();
        let out = super::plan(&mut f, ops);
        std::fs::remove_file(&path).unwrap();
        out
    }

    /// `img` with `ops` applied: every write live and inside the image, applied in order.
    fn edited(img: &[u8], ops: &[Op]) -> Vec<u8> {
        let mut img = img.to_vec();
        for w in planned(&img, ops).unwrap() {
            assert!(w.live);
            let (at, len) = (w.offset as usize, w.data.len() as usize);
            let dst = &mut img[at..at + len];
            match w.data {
                Data::Bytes(b) => dst.copy_from_slice(&b),
                Data::Zero(_) => dst.fill(0),
                Data::Source { .. } => unreachable!(),
            }
        }
        img
    }

    /// Every file (path, bytes) and empty folder, as the reader reads them.
    fn read_back(img: &[u8]) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
        let mut src =
            PfsSource::from_reader(Box::new(Cursor::new(img.to_vec())), "t".into()).unwrap();
        let paths: Vec<String> = src.files().iter().map(|f| f.path.clone()).collect();
        let files = paths
            .into_iter()
            .map(|p| {
                let b = src.read(&p).unwrap();
                (p, b)
            })
            .collect();
        (files, src.empty_dirs().to_vec())
    }

    /// What `build` puts in file `path` of `len` bytes.
    fn made(path: &str, len: u64) -> Vec<u8> {
        (0..len).map(|i| i as u8 ^ path.len() as u8).collect()
    }

    fn put(path: &str, data: Vec<u8>) -> Op {
        Op::Put {
            path: path.into(),
            len: data.len() as u64,
            data: Data::Bytes(data),
        }
    }

    fn del(path: &str) -> Op {
        Op::Delete { path: path.into() }
    }

    fn ren(from: &str, to: &str) -> Op {
        Op::Rename {
            from: from.into(),
            to: to.into(),
        }
    }

    /// `want` (path, bytes), sorted, against what the reader reads back from `img`, which
    /// [`Volume::open`] must accept.
    fn holds(img: &[u8], want: &[(&str, Vec<u8>)], empty: &[&str]) {
        open(img).unwrap();
        let mut want: Vec<(String, Vec<u8>)> = want
            .iter()
            .map(|(p, b)| (p.to_string(), b.clone()))
            .collect();
        want.sort();
        let (mut files, mut dirs) = read_back(img);
        files.sort();
        dirs.sort();
        assert_eq!(files, want);
        assert_eq!(dirs, empty);
    }

    /// The files `build(GAME, ..)` holds, with `changed` replacing (or, `None`, dropping) some.
    fn game_with(changed: &[(&str, Option<Vec<u8>>)]) -> Vec<(String, Vec<u8>)> {
        let mut want: Vec<(String, Vec<u8>)> = GAME
            .iter()
            .filter_map(|&(p, len)| match changed.iter().find(|c| c.0 == p) {
                Some((_, Some(b))) => Some((p.to_string(), b.clone())),
                Some((_, None)) => None,
                None => Some((p.to_string(), made(p, len))),
            })
            .collect();
        want.sort();
        want
    }

    #[test]
    fn put_rewrites_a_file_in_its_run() {
        let img = build(GAME, &["Empty Dir"]);
        let x: Vec<u8> = (0..65_536u32).map(|i| (i * 7) as u8 | 1).collect();
        let big: Vec<u8> = (0..70_001u32).map(|i| (i % 251) as u8 | 1).collect();
        // Same block count, grown within its block, shrunk by blocks, emptied, and the same
        // file twice (the last wins); paths case-folded.
        let ops = [
            put("DATA/sub/deeper/X", vec![9; 3]),
            put("Data/sub/deeper/x", x.clone()),
            put("data/big.pak", big.clone()),
            put("eboot.bin", Vec::new()),
            put("Data/sub/Empty.bin", vec![5; 10]),
        ];
        let out = edited(&img, &ops);
        let v = open(&out).unwrap();
        let (files, empty) = read_back(&out);
        let want = game_with(&[
            ("Data/sub/deeper/x", Some(x)),
            ("Data/Big.Pak", Some(big)),
            ("eboot.bin", Some(Vec::new())),
            ("Data/sub/Empty.bin", Some(vec![5; 10])),
        ]);
        assert_eq!(files, want);
        assert_eq!(empty, ["Empty Dir"]);
        // Shrunk to two blocks, the tail of the second zeroed; an empty file keeps one block.
        let p = v
            .inode(v.lookup("Data/Big.Pak").unwrap().unwrap().ino)
            .unwrap();
        assert_eq!((p.size, p.blocks), (70_001, 2));
        let end = (p.block + 2) as usize * B;
        assert!(
            out[(p.block as usize * B) + 70_001..end]
                .iter()
                .all(|&b| b == 0)
        );
        let e = v
            .inode(v.lookup("eboot.bin").unwrap().unwrap().ino)
            .unwrap();
        assert_eq!((e.size, e.blocks), (0, 1));
        assert!(out[e.block as usize * B..][..B].iter().all(|&b| b == 0));
        // Every directory entry is as it was.
        for n in v.nodes() {
            let rec = n.offset as usize..(n.offset + u64::from(n.reclen)) as usize;
            assert_eq!(out[rec.clone()], img[rec]);
        }
    }

    #[test]
    fn delete_removes_files_and_folder_trees() {
        let img = build(GAME, &["Empty Dir", "Data/void"]);
        let ops = [
            del("sce_sys/ICON0.png"),
            del("data/sub"),
            del("Empty Dir"),
            put("Data/Big.Pak", vec![1; 4]),
        ];
        let out = edited(&img, &ops);
        let v = open(&out).unwrap();
        let (files, empty) = read_back(&out);
        let gone = [
            ("sce_sys/icon0.png", None),
            ("Data/sub/Empty.bin", None),
            ("Data/sub/deeper/x", None),
            ("Data/Big.Pak", Some(vec![1; 4])),
        ];
        assert_eq!(files, game_with(&gone));
        assert_eq!(empty, ["Data/void"]);
        assert_eq!(v.lookup("data/sub/deeper").unwrap(), None);
        // Every inode of the tree but those left is clear.
        let used = v.inodes.iter().filter(|i| i.is_some()).count();
        assert_eq!(
            used,
            3 + v.nodes().len(),
            "super-root, FPT, uroot + the tree"
        );
        let names: Vec<_> = v
            .list("")
            .unwrap()
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names.len(), 3);

        // Deleting everything leaves an empty root and an empty path table.
        let all = ["eboot.bin", "sce_sys", "Data", "Empty Dir"].map(del);
        let out = edited(&img, &all);
        let v = open(&out).unwrap();
        assert!(v.list("").unwrap().is_empty());
        assert_eq!(v.inode(v.fpt).unwrap().size, 0);
        assert_eq!(read_back(&out), (Vec::new(), Vec::new()));
    }

    #[test]
    fn what_a_ffpfs_cant_take_is_refused() {
        let img = build(GAME, &["Empty Dir"]);
        let ops = [
            put("new.bin", vec![1]),
            put("Data/new/deeper.bin", vec![1]),
            put("sce_sys/param.json", vec![0; B + 1]),
            put("Data/Big.Pak", vec![0; 4 * B + 1]),
            Op::Mkdir {
                path: "Data/more".into(),
            },
            Op::Mkdir {
                path: "Empty Dir".into(),
            },
            put("Data/sub", vec![1]),
            put("eboot.bin/x", vec![1]),
            del("nothing"),
            del("/eboot.bin"),
            Op::Put {
                path: "eboot.bin".into(),
                len: 9,
                data: Data::Zero(8),
            },
            ren("eboot.bin", "SCE_SYS"),
        ];
        let f = planned(&img, &ops).unwrap_err();
        let want = [
            NO_ROOM,
            NO_ROOM,
            NO_ROOM,
            NO_ROOM,
            NO_ROOM,
            NO_ROOM,
            "Data/sub is a folder",
            NO_ROOM,
            "nothing: not found",
            "\"/eboot.bin\": bad path",
            "eboot.bin: 9 bytes, but its data holds 8",
            "SCE_SYS: already exists",
        ];
        assert_eq!(f, want);
        // Whatever fits is still planned exactly: a full run, and an image another tool made.
        let full = edited(&img, &[put("Data/Big.Pak", vec![3; 4 * B])]);
        assert_eq!(read_back(&full).0[0].1, vec![3; 4 * B]);
        let mut signed = img.clone();
        signed[0x1C] |= 0x1;
        assert!(planned(&signed, &[del("eboot.bin")]).unwrap_err()[0].contains("another tool"));
    }

    #[test]
    fn a_move_between_folders_relinks_them() {
        let img = build(GAME, &["Empty"]);
        let v = open(&img).unwrap();
        let ino = |v: &Volume, p: &str| v.lookup(p).unwrap().unwrap().ino;
        let ops = [
            ren("Data/sub", "sce_sys/sub"),
            ren("eboot.bin", "Data/eboot.bin"),
            ren("Data/Big.Pak", "Big.Pak"),
            ren("Big.Pak", "Data/Big2.Pak"),
            ren("Empty", "sce_sys/sub/deeper/Empty"),
            put("sce_sys/sub/deeper/x", b"12345".to_vec()),
        ];
        let out = edited(&img, &ops);
        let b = |p: &str| made(p, GAME.iter().find(|g| g.0 == p).unwrap().1);
        let want = [
            ("Data/eboot.bin", b("eboot.bin")),
            ("sce_sys/param.json", b("sce_sys/param.json")),
            ("sce_sys/icon0.png", b("sce_sys/icon0.png")),
            ("Data/Big2.Pak", b("Data/Big.Pak")),
            ("sce_sys/sub/Empty.bin", vec![]),
            ("sce_sys/sub/deeper/x", b"12345".to_vec()),
        ];
        holds(&out, &want, &["sce_sys/sub/deeper/Empty"]);
        // Inode numbers and link counts as the writer sets them (open checks `..` too).
        let w = open(&out).unwrap();
        assert_eq!(ino(&w, "sce_sys/sub"), ino(&v, "Data/sub"));
        assert_eq!(ino(&w, "Data/Big2.Pak"), ino(&v, "Data/Big.Pak"));
        let nlink = |v: &Volume, p: &str| v.inode(ino(v, p)).unwrap().nlink;
        assert_eq!((nlink(&v, "Data"), nlink(&w, "Data")), (3, 2));
        assert_eq!((nlink(&v, "sce_sys"), nlink(&w, "sce_sys")), (2, 3));
        assert_eq!(w.inode(w.uroot).unwrap().nlink, 3 + 2);
        // A moved folder deleted, and one moved and back, in the same plan.
        let out = edited(
            &img,
            &[
                ren("Data/sub", "sub"),
                del("sub/deeper"),
                ren("sub", "Data/sub"),
            ],
        );
        let want = &game_with(&[("Data/sub/deeper/x", None)]);
        let want: Vec<(&str, Vec<u8>)> =
            want.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
        holds(&out, &want, &["Empty"]);
    }
}
