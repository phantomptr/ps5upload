//! The tree as PFS inodes, and the metadata that describes it: header, inode table, super-root,
//! flat path table, collision resolver and directories. Shared by the `.ffpfs` writer and the
//! `.ffpfsc` container, which is the same encoder over a one-file tree.
//!
//! Inode order, which is also data order: super-root, `flat_path_table`, the
//! `collision_resolver` when two paths share a hash, `uroot`, the other directories by
//! lower-cased path, then the files by lower-cased path. Every node is one contiguous run of
//! blocks from `db[0]`; an empty file or directory still takes one block.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{BufWriter, Write};
use std::sync::atomic::AtomicBool;

use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{Error, Result};

use crate::{BLOCK, check_cancel, err};

pub(crate) const PFS_VERSION: u64 = 2;
pub(crate) const PFS_MAGIC: u64 = 20_130_315;
/// Case-insensitive; unsigned, 32-bit inodes, not encrypted.
pub(crate) const MODE_CASE_INSENSITIVE: u16 = 0x8;
pub(crate) const MODE_SIGNED: u16 = 0x1;
pub(crate) const MODE_64BIT_INODES: u16 = 0x2;
pub(crate) const MODE_ENCRYPTED: u16 = 0x4;
/// The unsigned D32 inode.
pub(crate) const INODE_LEN: usize = 0xA8;
/// 390 per 64 KiB block; the 16-byte tail of each table block stays zero.
const INODES_PER_BLOCK: u64 = BLOCK / INODE_LEN as u64;
pub(crate) const MODE_DIR: u16 = 0x4000;
pub(crate) const MODE_FILE: u16 = 0x8000;
/// Read and execute for owner, group and others.
const MODE_RX: u16 = 0x001 | 0x004 | 0x008 | 0x020 | 0x040 | 0x100;
pub(crate) const FLAG_COMPRESSED: u32 = 0x1;
const FLAG_READONLY: u32 = 0x10;
const FLAG_INTERNAL: u32 = 0x2_0000;
pub(crate) const DIRENT_FILE: i32 = 2;
pub(crate) const DIRENT_DIR: i32 = 3;
pub(crate) const DIRENT_DOT: i32 = 4;
pub(crate) const DIRENT_DOTDOT: i32 = 5;
/// The FPT value bits: a directory's inode, and an offset into the collision resolver.
const FPT_DIR: u32 = 0x2000_0000;
const FPT_COLLISION: u32 = 0x8000_0000;
const MAX_NAME: usize = 255;
/// Larger directories are refused, here and by the reader.
pub(crate) const MAX_DIR_BYTES: u64 = 64 << 20;

/// A file or directory of the image.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub name: String,
    /// `/`-separated path; empty for `uroot`.
    pub path: String,
    pub dir: bool,
    /// A file's logical length; a directory's entry bytes.
    pub size: u64,
    /// A file's bytes as stored: its length, or its PFSC container's.
    pub stored: u64,
    pub compressed: bool,
    pub parent: usize,
    /// Subdirectories first, then files, each by lower-cased name.
    pub children: Vec<usize>,
    pub subdirs: u64,
    pub ino: u32,
    /// `db[0]` and the run length.
    pub block: u64,
    pub blocks: u64,
}

impl Node {
    fn new(name: &str, path: &str, dir: bool, size: u64, parent: usize) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            dir,
            size,
            stored: size,
            compressed: false,
            parent,
            children: Vec::new(),
            subdirs: 0,
            ino: 0,
            block: 0,
            blocks: 0,
        }
    }
}

/// The whole image, fixed before the first byte is written.
#[derive(Debug, Clone)]
pub(crate) struct Image {
    /// Node 0 is `uroot`.
    pub nodes: Vec<Node>,
    /// Nodes in inode and data order: `uroot`, directories, files.
    pub order: Vec<usize>,
    pub fpt: Vec<u8>,
    /// The collision resolver's bytes; `None` leaves its block reserved and empty.
    pub resolver: Option<Vec<u8>>,
    pub inodes: u64,
    pub inode_blocks: u64,
    pub fpt_block: u64,
    pub fpt_blocks: u64,
    pub resolver_blocks: u64,
    /// Image length in blocks (`ndblock`).
    pub ndblock: u64,
    pub time: i64,
}

impl Image {
    fn super_root_block(&self) -> u64 {
        1 + self.inode_blocks
    }

    fn special_inodes(&self) -> u32 {
        if self.resolver.is_some() { 3 } else { 2 }
    }

    pub fn files(&self) -> impl Iterator<Item = &Node> {
        self.order
            .iter()
            .map(|&n| &self.nodes[n])
            .filter(|n| !n.dir)
    }
}

/// Why `name` cannot be one path component of a PFS image.
/// ponytail: ASCII only, as MkPFS (the FPT hash upper-cases ASCII, and it is the only layout
/// known to boot); widen after a console test with UTF-8 names.
pub(crate) fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty name");
    }
    if name == "." || name == ".." {
        return Some("'.' or '..' as a name");
    }
    if name.len() > MAX_NAME {
        return Some("name longer than 255 bytes");
    }
    if !name.is_ascii() {
        return Some("non-ASCII name (PFS images hold ASCII names only)");
    }
    if name.bytes().any(|b| b.is_ascii_control()) {
        return Some("control character in name");
    }
    // Components are split before they get here; only the wrapper's one name can hold one.
    if name.contains('/') {
        return Some("'/' in a name");
    }
    None
}

/// The tree's nodes, names checked: fails with every offending path listed.
pub(crate) fn nodes(tree: &dyn SourceTree, cancel: &AtomicBool) -> Result<Vec<Node>> {
    let mut problems = Vec::new();
    let mut nodes = vec![Node::new("", "", true, 0, 0)];
    let mut index = HashMap::new();
    let files = tree.files().iter().map(|f| (f.path.as_str(), Some(f.size)));
    let dirs = tree.empty_dirs().iter().map(|d| (d.as_str(), None));
    for (i, (path, size)) in files.chain(dirs).enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        add_path(&mut nodes, &mut index, path, size, &mut problems);
    }
    for d in 0..nodes.len() {
        if d % 4096 == 0 {
            check_cancel(cancel)?;
        }
        if nodes[d].dir {
            sort_and_check_collisions(&mut nodes, d, &mut problems);
        }
    }
    refuse(
        problems,
        "name problem(s) keep this tree out of a PFS image",
    )?;
    Ok(nodes)
}

/// Adds `path` (a file when `size` is set, else a directory) and its parent directories.
fn add_path(
    nodes: &mut Vec<Node>,
    index: &mut HashMap<String, usize>,
    path: &str,
    size: Option<u64>,
    problems: &mut Vec<String>,
) {
    let parts: Vec<&str> = path.split('/').collect();
    let mut parent = 0;
    let mut end = 0;
    for (i, part) in parts.iter().enumerate() {
        end += part.len() + usize::from(i > 0);
        let here = &path[..end];
        let dir = i + 1 < parts.len() || size.is_none();
        if let Some(&n) = index.get(here) {
            if nodes[n].dir && dir {
                parent = n;
                continue;
            }
            let what = if nodes[n].dir == dir {
                "listed twice"
            } else {
                "both a file and a directory"
            };
            problems.push(format!("{}: {what}", here.escape_debug()));
            return;
        }
        if let Some(why) = name_problem(part) {
            problems.push(format!("{}: {why}", here.escape_debug()));
        }
        let n = nodes.len();
        let bytes = size.filter(|_| !dir).unwrap_or(0);
        nodes.push(Node::new(part, here, dir, bytes, parent));
        nodes[parent].children.push(n);
        if dir {
            nodes[parent].subdirs += 1;
        }
        index.insert(here.to_string(), n);
        parent = n;
    }
}

/// Orders a directory's entries as MkPFS lists them (subdirectories, then files, each by
/// lower-cased name) and reports names that are one name to the case-insensitive image.
fn sort_and_check_collisions(nodes: &mut [Node], dir: usize, problems: &mut Vec<String>) {
    let mut kids = std::mem::take(&mut nodes[dir].children);
    kids.sort_by_cached_key(|&k| (!nodes[k].dir, nodes[k].name.to_ascii_lowercase()));
    let mut seen: HashMap<String, usize> = HashMap::new();
    for &k in &kids {
        match seen.entry(nodes[k].name.to_ascii_lowercase()) {
            Entry::Occupied(first) => problems.push(format!(
                "{}: same name as {} on the case-insensitive image",
                nodes[k].path.escape_debug(),
                nodes[*first.get()].path.escape_debug()
            )),
            Entry::Vacant(slot) => {
                slot.insert(k);
            }
        }
    }
    nodes[dir].children = kids;
}

/// Fails with every problem listed, if there is any.
fn refuse(mut problems: Vec<String>, what: &str) -> Result<()> {
    if problems.is_empty() {
        return Ok(());
    }
    problems.sort();
    problems.dedup();
    Err(err(format!(
        "{} {what}:\n  {}",
        problems.len(),
        problems.join("\n  ")
    )))
}

/// The flat-path-table hash: `h = 31h + c` over the `/`-rooted path, upper-cased on a
/// case-insensitive image (MkPFS `fpt_hash`).
/// ponytail: bytes, ASCII upper-casing; MkPFS folds Unicode code points, so a non-ASCII name
/// hashes differently there (the writer refuses such names).
pub(crate) fn fpt_hash(path: &str, case_insensitive: bool) -> u32 {
    path.bytes().fold(0u32, |h, c| {
        let c = if case_insensitive {
            c.to_ascii_uppercase()
        } else {
            c
        };
        h.wrapping_mul(31).wrapping_add(u32::from(c))
    })
}

/// One path of the flat path table: the `/`-rooted path, its hash, inode and kind.
pub(crate) struct PathEntry {
    pub path: String,
    pub hash: u32,
    pub ino: u32,
    pub dir: bool,
}

/// The flat path table and, when two paths share a hash, the collision resolver (MkPFS
/// `make_fpt_and_collision_blob`). One `(hash, value)` per hash in hash order; the value is
/// the inode (`FPT_DIR` on a directory) or, for a shared hash, the offset of its group in the
/// resolver: the group's paths as directory entries by inode (directories before files, as
/// MkPFS numbers them), then 0x18 zero bytes. Shared by the writer and the reader's check.
pub(crate) fn path_table(
    mut entries: Vec<PathEntry>,
    problems: &mut Vec<String>,
) -> (Vec<u8>, Option<Vec<u8>>) {
    entries.sort_by_key(|e| (e.hash, e.ino));
    let collision = entries.windows(2).any(|w| w[0].hash == w[1].hash);
    let mut fpt = Vec::with_capacity(entries.len() * 8);
    let mut resolver = collision.then(Vec::new);
    let mut i = 0;
    while i < entries.len() {
        let h = entries[i].hash;
        let group = entries[i..].iter().take_while(|e| e.hash == h).count();
        let value = if let Some(blob) = resolver.as_mut().filter(|_| group > 1) {
            let at = blob.len() as u64;
            if at >= u64::from(FPT_COLLISION) {
                problems.push("the collision resolver is over 2 GiB".to_string());
            }
            for e in &entries[i..i + group] {
                let kind = if e.dir { DIRENT_DIR } else { DIRENT_FILE };
                blob.extend(dirent(e.ino, kind, &e.path));
            }
            blob.extend([0u8; 0x18]);
            FPT_COLLISION | at as u32
        } else {
            let e = &entries[i];
            e.ino | if e.dir { FPT_DIR } else { 0 }
        };
        fpt.extend_from_slice(&h.to_le_bytes());
        fpt.extend_from_slice(&value.to_le_bytes());
        i += group;
    }
    (fpt, resolver)
}

/// A directory entry's on-disk length.
fn dirent_len(name_len: usize) -> u64 {
    (name_len as u64 + 17).next_multiple_of(8)
}

fn dirent(ino: u32, kind: i32, name: &str) -> Vec<u8> {
    let len = dirent_len(name.len()) as usize;
    let mut d = Vec::with_capacity(len);
    d.extend_from_slice(&ino.to_le_bytes());
    d.extend_from_slice(&kind.to_le_bytes());
    d.extend_from_slice(&(name.len() as i32).to_le_bytes());
    d.extend_from_slice(&(len as i32).to_le_bytes());
    d.extend_from_slice(name.as_bytes());
    d.resize(len, 0);
    d
}

/// Numbers the inodes, sizes the directories, builds the path table and places every run.
/// D32 limits (32-bit pointers and counts, 16-bit link counts) are checked here and listed
/// like name problems.
pub(crate) fn lay_out(mut nodes: Vec<Node>, time: i64, cancel: &AtomicBool) -> Result<Image> {
    check_cancel(cancel)?;
    let mut problems = Vec::new();
    let lower = |n: &Node| n.path.to_ascii_lowercase();
    let mut dirs: Vec<usize> = (1..nodes.len()).filter(|&n| nodes[n].dir).collect();
    let mut files: Vec<usize> = (1..nodes.len()).filter(|&n| !nodes[n].dir).collect();
    dirs.sort_by_cached_key(|&n| lower(&nodes[n]));
    files.sort_by_cached_key(|&n| lower(&nodes[n]));
    check_cancel(cancel)?;

    // The path table's hashes, first: a shared one adds the resolver's inode.
    let hashes: Vec<u32> = dirs
        .iter()
        .chain(&files)
        .map(|&n| fpt_hash(&format!("/{}", nodes[n].path), true))
        .collect();
    let collision = {
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        sorted.windows(2).any(|w| w[0] == w[1])
    };

    let first = if collision { 3u64 } else { 2 };
    let mut order = Vec::with_capacity(1 + dirs.len() + files.len());
    order.push(0);
    order.extend(&dirs);
    order.extend(&files);
    let inodes = first + order.len() as u64;
    if inodes > u64::from(u32::MAX) {
        problems.push(format!(
            "{inodes} inodes: over the 2^32 a PFS image numbers"
        ));
    }
    for (i, &n) in order.iter().enumerate() {
        nodes[n].ino = (first + i as u64) as u32;
    }

    for &n in &order {
        let node = &nodes[n];
        if node.dir {
            let entries: u64 = node
                .children
                .iter()
                .map(|&c| dirent_len(nodes[c].name.len()))
                .sum::<u64>()
                + 2 * dirent_len(1);
            let base = if n == 0 { 3 } else { 2 };
            if base + node.subdirs > u64::from(u16::MAX) {
                problems.push(format!(
                    "/{}: {} subdirectories, over the 65535 links a PFS directory counts",
                    node.path.escape_debug(),
                    node.subdirs
                ));
            }
            if entries > MAX_DIR_BYTES {
                problems.push(format!(
                    "/{}: {entries} bytes of directory entries, over 64 MiB",
                    node.path.escape_debug()
                ));
            }
            nodes[n].size = entries;
            nodes[n].stored = entries;
        } else if node.size > i64::MAX as u64 || node.stored > i64::MAX as u64 {
            problems.push(format!("{}: too large", node.path.escape_debug()));
        }
    }

    let entries = dirs
        .iter()
        .chain(&files)
        .zip(hashes)
        .map(|(&n, hash)| PathEntry {
            path: format!("/{}", nodes[n].path),
            hash,
            ino: nodes[n].ino,
            dir: nodes[n].dir,
        })
        .collect();
    let (fpt, resolver) = path_table(entries, &mut problems);
    check_cancel(cancel)?;

    // Blocks: header, inode table, super-root, FPT, resolver (or one reserved block), then
    // every node's run in inode order.
    let runs = |bytes: u64| bytes.div_ceil(BLOCK).max(1);
    let inode_blocks = inodes.div_ceil(INODES_PER_BLOCK);
    let fpt_blocks = runs(fpt.len() as u64);
    let resolver_blocks = resolver.as_ref().map_or(1, |r| runs(r.len() as u64));
    let mut next = 1 + inode_blocks + 1 + fpt_blocks + resolver_blocks;
    for &n in &order {
        let blocks = runs(nodes[n].stored);
        nodes[n].block = next;
        nodes[n].blocks = blocks;
        next = next.saturating_add(blocks);
    }
    if next > i32::MAX as u64 {
        problems.push(format!(
            "the image needs {next} blocks of 64 KiB, over the 2^31 - 1 a PFS block pointer holds"
        ));
    }
    refuse(problems, "limit(s) keep this tree out of a PFS image")?;
    Ok(Image {
        nodes,
        order,
        fpt,
        resolver,
        inodes,
        inode_blocks,
        fpt_block: 1 + inode_blocks + 1,
        fpt_blocks,
        resolver_blocks,
        ndblock: next,
        time,
    })
}

/// The one-file tree a `.ffpfsc` container is: `uroot` holding `name`, `stored` bytes on disk
/// for `raw` logical ones.
pub(crate) fn single(name: &str, raw: u64, stored: u64, time: i64) -> Result<Image> {
    if let Some(why) = name_problem(name) {
        return Err(err(format!("{name:?} cannot name the image inside: {why}")));
    }
    let mut root = Node::new("", "", true, 0, 0);
    root.children.push(1);
    let mut file = Node::new(name, name, false, raw, 0);
    file.stored = stored;
    file.compressed = true;
    lay_out(vec![root, file], time, &AtomicBool::new(false))
}

/// One D32 inode. `db[1..12]` take `fill` (-1, or 0 for the super-root); `ib` stays 0.
#[allow(clippy::too_many_arguments)]
fn inode(
    mode: u16,
    nlink: u16,
    flags: u32,
    size: u64,
    size_compressed: u64,
    blocks: u64,
    db0: u64,
    fill: i32,
    time: i64,
) -> [u8; INODE_LEN] {
    let mut d = [0u8; INODE_LEN];
    d[0..2].copy_from_slice(&mode.to_le_bytes());
    d[2..4].copy_from_slice(&nlink.to_le_bytes());
    d[4..8].copy_from_slice(&flags.to_le_bytes());
    d[8..16].copy_from_slice(&size.to_le_bytes());
    d[16..24].copy_from_slice(&size_compressed.to_le_bytes());
    for i in 0..4 {
        d[24 + i * 8..32 + i * 8].copy_from_slice(&time.to_le_bytes());
    }
    // 0x38..0x60: nanoseconds, uid, gid and two reserved words, all zero. Every value
    // below was range-checked by `lay_out`.
    d[0x60..0x64].copy_from_slice(&(blocks as u32).to_le_bytes());
    d[0x64..0x68].copy_from_slice(&(db0 as i32).to_le_bytes());
    for i in 1..12 {
        d[0x64 + i * 4..0x68 + i * 4].copy_from_slice(&fill.to_le_bytes());
    }
    d
}

fn node_inode(img: &Image, node: &Node, root: bool) -> [u8; INODE_LEN] {
    if node.dir {
        let nlink = (if root { 3 } else { 2 }) + node.subdirs;
        let size = node.blocks * BLOCK;
        let mode = MODE_DIR | MODE_RX;
        inode(
            mode,
            nlink as u16,
            FLAG_READONLY,
            size,
            size,
            node.blocks,
            node.block,
            -1,
            img.time,
        )
    } else {
        let (flags, second) = if node.compressed {
            (FLAG_READONLY | FLAG_COMPRESSED, node.size)
        } else {
            (FLAG_READONLY, node.stored)
        };
        let mode = MODE_FILE | MODE_RX;
        inode(
            mode,
            1,
            flags,
            node.stored,
            second,
            node.blocks,
            node.block,
            -1,
            img.time,
        )
    }
}

/// Block 0: the header, with the inode table described by a small inode of its own at 0x50.
fn header(img: &Image) -> Vec<u8> {
    let mut h = vec![0u8; BLOCK as usize];
    h[0x00..0x08].copy_from_slice(&PFS_VERSION.to_le_bytes());
    h[0x08..0x10].copy_from_slice(&PFS_MAGIC.to_le_bytes());
    h[0x1A] = 1;
    h[0x1C..0x1E].copy_from_slice(&MODE_CASE_INSENSITIVE.to_le_bytes());
    h[0x20..0x24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    h[0x28..0x30].copy_from_slice(&1u64.to_le_bytes()); // nblock
    h[0x30..0x38].copy_from_slice(&img.inodes.to_le_bytes());
    h[0x38..0x40].copy_from_slice(&img.ndblock.to_le_bytes());
    h[0x40..0x48].copy_from_slice(&img.inode_blocks.to_le_bytes());
    let t = 0x50;
    let table = img.inode_blocks * BLOCK;
    h[t + 2..t + 4].copy_from_slice(&1u16.to_le_bytes());
    h[t + 4..t + 8].copy_from_slice(&FLAG_READONLY.to_le_bytes());
    h[t + 8..t + 16].copy_from_slice(&table.to_le_bytes());
    h[t + 16..t + 24].copy_from_slice(&table.to_le_bytes());
    for i in 0..4 {
        h[t + 0x18 + i * 8..t + 0x20 + i * 8].copy_from_slice(&img.time.to_le_bytes());
    }
    h[t + 0x60..t + 0x64].copy_from_slice(&(img.inode_blocks as u32).to_le_bytes());
    // The S64 layout: 12 direct entries of a 32-byte signature and an 8-byte block; the
    // table starts at block 1.
    h[t + 0x68 + 32..t + 0x68 + 40].copy_from_slice(&1u64.to_le_bytes());
    h[0x368..0x36C].copy_from_slice(&1u32.to_le_bytes()); // unsigned
    h
}

/// Writes everything before the first file's data: header, inode table, super-root, path
/// table, resolver, and every directory.
pub(crate) fn write_meta<W: Write>(sink: &mut Sink<'_, W>, img: &Image) -> Result<()> {
    sink.expect(0)?;
    sink.write(&header(img))?;

    // The inode table, 390 to a block.
    let special = img.special_inodes();
    let internal = FLAG_INTERNAL | FLAG_READONLY;
    let (file, dir) = (MODE_FILE | MODE_RX, MODE_DIR | MODE_RX);
    let fpt_len = img.fpt.len() as u64;
    let mut table = vec![
        inode(
            dir,
            1,
            internal,
            BLOCK,
            BLOCK,
            1,
            img.super_root_block(),
            0,
            img.time,
        ),
        inode(
            file,
            1,
            internal,
            fpt_len,
            fpt_len,
            img.fpt_blocks,
            img.fpt_block,
            -1,
            img.time,
        ),
    ];
    if let Some(r) = &img.resolver {
        let len = r.len() as u64;
        let at = img.fpt_block + img.fpt_blocks;
        table.push(inode(
            file,
            1,
            internal,
            len,
            len,
            img.resolver_blocks,
            at,
            -1,
            img.time,
        ));
    }
    let entries = table.into_iter().chain(
        img.order
            .iter()
            .map(|&n| node_inode(img, &img.nodes[n], n == 0)),
    );
    for (i, bytes) in entries.enumerate() {
        let i = i as u64;
        let at = (1 + i / INODES_PER_BLOCK) * BLOCK + (i % INODES_PER_BLOCK) * INODE_LEN as u64;
        sink.zeros_to(at)?;
        sink.write(&bytes)?;
    }
    sink.zeros_to(img.super_root_block() * BLOCK)?;

    sink.write(&dirent(1, DIRENT_FILE, "flat_path_table"))?;
    if img.resolver.is_some() {
        sink.write(&dirent(2, DIRENT_FILE, "collision_resolver"))?;
    }
    sink.write(&dirent(special, DIRENT_DIR, "uroot"))?;
    sink.zeros_to(img.fpt_block * BLOCK)?;
    sink.write(&img.fpt)?;
    let resolver_at = img.fpt_block + img.fpt_blocks;
    sink.zeros_to(resolver_at * BLOCK)?;
    if let Some(r) = &img.resolver {
        sink.write(r)?;
    }
    sink.zeros_to((resolver_at + img.resolver_blocks) * BLOCK)?;

    for &n in img.order.iter().filter(|&&n| img.nodes[n].dir) {
        let node = &img.nodes[n];
        sink.expect(node.block * BLOCK)?;
        let parent = img.nodes[node.parent].ino;
        sink.write(&dirent(node.ino, DIRENT_DOT, "."))?;
        sink.write(&dirent(parent, DIRENT_DOTDOT, ".."))?;
        for &c in &node.children {
            let child = &img.nodes[c];
            let kind = if child.dir { DIRENT_DIR } else { DIRENT_FILE };
            sink.write(&dirent(child.ino, kind, &child.name))?;
        }
        sink.zeros_to((node.block + node.blocks) * BLOCK)?;
    }
    Ok(())
}

/// The output with its position, so every region can assert it starts where the plan put it.
/// It checks `cancel` every `CHUNK` bytes, so long runs stay cancellable.
pub(crate) struct Sink<'a, W: Write> {
    pub w: BufWriter<W>,
    pub pos: u64,
    unchecked: u64,
    cancel: &'a AtomicBool,
}

impl<'a, W: Write> Sink<'a, W> {
    pub fn new(w: W, cancel: &'a AtomicBool) -> Self {
        Self {
            w: BufWriter::with_capacity(1 << 20, w),
            pos: 0,
            unchecked: 0,
            cancel,
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        self.unchecked += bytes.len() as u64;
        if self.unchecked >= crate::CHUNK {
            self.unchecked = 0;
            check_cancel(self.cancel)?;
        }
        Ok(())
    }

    pub fn zeros_to(&mut self, end: u64) -> Result<()> {
        if end < self.pos {
            return Err(err(format!(
                "internal error: PFS writer at {} but the next region starts at {end}",
                self.pos
            )));
        }
        static ZEROS: [u8; BLOCK as usize] = [0; BLOCK as usize];
        while self.pos < end {
            let n = (end - self.pos).min(BLOCK) as usize;
            self.write(&ZEROS[..n])?;
        }
        Ok(())
    }

    pub fn expect(&self, at: u64) -> Result<()> {
        if self.pos != at {
            return Err(Error::Format(format!(
                "internal error: PFS writer at {} but the plan says {at}",
                self.pos
            )));
        }
        Ok(())
    }
}
