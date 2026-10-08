//! The source tree as inodes: names checked, directories derived from the file paths,
//! everything ordered so the image comes out the same every time.

use std::collections::{HashMap, HashSet};

use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{Error, Result};
use unicode_normalization::{IsNormalized, is_nfc_quick};

/// FreeBSD's `UFS_LINK_MAX`: a directory's link count is 2 + its subdirectories.
const LINK_MAX: u64 = 65500;
/// `UFS_MAXNAMLEN`.
const MAXNAMLEN: usize = 255;
/// FreeBSD's `MAXPATHLEN` (with its NUL): a longer path cannot be opened on the console
/// anyway, and the bound keeps the per-directory path keys and the recursion below small.
const MAXPATHLEN: usize = 1023;
/// FreeBSD's `MAXDIRSIZE`: directory offsets are a signed 32-bit `doff_t`.
const MAXDIRSIZE: u64 = 0x7fff_ffff;

/// One inode to write. Nodes are kept in inode order: node i is inode `ROOTINO + i`.
#[derive(Debug, Clone)]
pub struct Node {
    /// The name in its parent directory ("" for the root).
    pub name: Box<str>,
    /// Node index of the parent (the root is its own parent).
    pub parent: u32,
    pub kind: Kind,
    /// Bytes: the file's length, or the directory's size (a multiple of DIRBLKSIZ).
    pub size: u64,
    /// Data index of slot 0 (see `bmap`); fixed by `plan`.
    pub start: u64,
    /// Data + indirect blocks the node owns.
    pub slots: u64,
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// Index into `tree.files()`.
    File { src: usize },
    /// Children (node indexes, sorted by name bytes), subdirectory count, depth from root.
    Dir {
        children: Vec<u32>,
        subdirs: u32,
        depth: u32,
    },
}

impl Node {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir { .. })
    }

    pub fn nlink(&self) -> u16 {
        match self.kind {
            Kind::File { .. } => 1,
            // Bounded by LINK_MAX in `build`.
            Kind::Dir { subdirs, .. } => (2 + subdirs) as u16,
        }
    }
}

/// Why a single path component cannot go into the image as is.
fn bad_component(c: &str) -> Option<&'static str> {
    if c.is_empty() {
        return Some("empty path component");
    }
    if c == "." || c == ".." {
        return Some("'.' or '..' component");
    }
    if c.len() > MAXNAMLEN {
        return Some("component longer than 255 bytes");
    }
    if c.contains('\0') {
        return Some("NUL in name");
    }
    if c.contains('\u{FFFD}') {
        // The scanners map undecodable bytes to U+FFFD; writing it would rename the file.
        return Some("U+FFFD in name (the original was not valid UTF-8?)");
    }
    if is_nfc_quick(c.chars()) != IsNormalized::Yes && !unicode_normalization::is_nfc(c) {
        return Some("name is not NFC (macOS NFD?)");
    }
    None
}

/// Builds the inode list. Collects every naming problem first and fails with all of them.
pub fn build(tree: &dyn SourceTree) -> Result<Vec<Node>> {
    let mut problems: Vec<String> = Vec::new();
    let check = |path: &str, problems: &mut Vec<String>| -> bool {
        if path.len() > MAXPATHLEN {
            problems.push(format!("{path:?}: path longer than {MAXPATHLEN} bytes"));
            return false;
        }
        let mut ok = true;
        for c in path.split('/') {
            if let Some(why) = bad_component(c) {
                problems.push(format!("{path:?}: {why}"));
                ok = false;
                break;
            }
        }
        ok
    };

    // Directory path -> node index; node 0 is the root.
    let mut nodes: Vec<Node> = vec![dir_node("", 0)];
    let mut dirs: HashMap<String, u32> = HashMap::from([(String::new(), 0)]);
    let mut files: HashSet<&str> = HashSet::new();

    for (src, f) in tree.files().iter().enumerate() {
        if !check(&f.path, &mut problems) {
            continue;
        }
        if !files.insert(&f.path) {
            problems.push(format!("{:?}: listed twice", f.path));
            continue;
        }
        let (parent_path, name) = split(&f.path);
        let Some(parent) = ensure_dir(parent_path, &mut nodes, &mut dirs) else {
            problems.push(format!("{:?}: overflows 2^32 inodes", f.path));
            continue;
        };
        let Ok(idx) = u32::try_from(nodes.len()) else {
            problems.push(format!("{:?}: overflows 2^32 inodes", f.path));
            continue;
        };
        nodes.push(Node {
            name: name.into(),
            parent,
            kind: Kind::File { src },
            size: f.size,
            start: 0,
            slots: 0,
        });
        if let Kind::Dir { children, .. } = &mut nodes[parent as usize].kind {
            children.push(idx);
        }
    }
    for d in tree.empty_dirs() {
        if check(d, &mut problems) && ensure_dir(d, &mut nodes, &mut dirs).is_none() {
            problems.push(format!("{d:?}: overflows 2^32 inodes"));
        }
    }
    // A path that is both a file and a directory ("a" and "a/b").
    for f in tree.files() {
        if dirs.contains_key(f.path.as_str()) {
            problems.push(format!("{:?}: is both a file and a directory", f.path));
        }
    }
    if !problems.is_empty() {
        problems.sort();
        problems.dedup();
        return Err(Error::Format(format!(
            "{} path(s) cannot be written to a UFS2 image (names are not renamed):\n  {}",
            problems.len(),
            problems.join("\n  ")
        )));
    }
    renumber(nodes)
}

fn dir_node(name: &str, parent: u32) -> Node {
    Node {
        name: name.into(),
        parent,
        kind: Kind::Dir {
            children: Vec::new(),
            subdirs: 0,
            depth: 0,
        },
        size: 0,
        start: 0,
        slots: 0,
    }
}

/// "a/b/c" -> ("a/b", "c"); "c" -> ("", "c").
fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Node index of directory `path`, creating it and its parents as needed.
fn ensure_dir(path: &str, nodes: &mut Vec<Node>, dirs: &mut HashMap<String, u32>) -> Option<u32> {
    if let Some(&i) = dirs.get(path) {
        return Some(i);
    }
    let (parent_path, name) = split(path);
    let parent = ensure_dir(parent_path, nodes, dirs)?;
    let idx = u32::try_from(nodes.len()).ok()?;
    nodes.push(dir_node(name, parent));
    if let Kind::Dir { children, .. } = &mut nodes[parent as usize].kind {
        children.push(idx);
    }
    dirs.insert(path.to_string(), idx);
    Some(idx)
}

/// Puts the nodes in depth-first pre-order with children sorted by name bytes, so the
/// inode numbers and the block layout depend only on the tree, never on scan order.
fn renumber(mut nodes: Vec<Node>) -> Result<Vec<Node>> {
    let mut order: Vec<u32> = Vec::with_capacity(nodes.len());
    let mut stack: Vec<u32> = vec![0];
    while let Some(i) = stack.pop() {
        order.push(i);
        let kids = match &mut nodes[i as usize].kind {
            Kind::Dir { children, .. } => std::mem::take(children),
            Kind::File { .. } => continue,
        };
        let mut kids = kids;
        // Names are unique per directory: files were de-duplicated, dirs come from a map,
        // and file/dir clashes were rejected.
        kids.sort_by(|a, b| {
            nodes[*a as usize]
                .name
                .as_bytes()
                .cmp(nodes[*b as usize].name.as_bytes())
        });
        stack.extend(kids.iter().rev());
        if let Kind::Dir { children, .. } = &mut nodes[i as usize].kind {
            *children = kids;
        }
    }
    let mut new_index = vec![0u32; nodes.len()];
    for (new, &old) in order.iter().enumerate() {
        new_index[old as usize] = new as u32;
    }
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for &old in &order {
        let mut n = std::mem::replace(&mut nodes[old as usize], dir_node("", 0));
        n.parent = new_index[n.parent as usize];
        if let Kind::Dir { children, .. } = &mut n.kind {
            for c in children.iter_mut() {
                *c = new_index[*c as usize];
            }
        }
        out.push(n);
    }
    // Depths and subdirectory counts; parents precede children in pre-order.
    let mut problems = Vec::new();
    for i in 0..out.len() {
        let depth = match out[out[i].parent as usize].kind {
            Kind::Dir { depth, .. } if i != 0 => depth + 1,
            _ => 0,
        };
        let subdirs = match &out[i].kind {
            Kind::Dir { children, .. } => children
                .iter()
                .filter(|&&c| out[c as usize].is_dir())
                .count() as u64,
            Kind::File { .. } => 0,
        };
        if 2 + subdirs > LINK_MAX {
            problems.push(format!(
                "a directory holds {subdirs} subdirectories; UFS allows {}",
                LINK_MAX - 2
            ));
        }
        if let Kind::Dir {
            subdirs: s,
            depth: d,
            ..
        } = &mut out[i].kind
        {
            *s = subdirs as u32;
            *d = depth;
        }
    }
    if let Some(p) = problems.into_iter().next() {
        return Err(Error::Format(p));
    }
    Ok(out)
}

/// `DIRSIZ`: an entry's header (8 bytes) plus its name and a NUL, rounded up to 4.
pub fn dirsiz(namelen: usize) -> usize {
    (8 + namelen + 1 + 3) & !3
}

pub const DIRBLKSIZ: usize = 512;
pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;

/// The directory's entries as (inode, type, name): ".", "..", then the children.
fn entries<'a>(nodes: &'a [Node], i: usize) -> impl Iterator<Item = (u32, u8, &'a [u8])> + 'a {
    let n = &nodes[i];
    let kids: &[u32] = match &n.kind {
        Kind::Dir { children, .. } => children,
        Kind::File { .. } => &[],
    };
    let ino = |j: usize| crate::ROOTINO as u32 + j as u32;
    [
        (ino(i), DT_DIR, &b"."[..]),
        (ino(n.parent as usize), DT_DIR, &b".."[..]),
    ]
    .into_iter()
    .chain(kids.iter().map(move |&c| {
        let child = &nodes[c as usize];
        let t = if child.is_dir() { DT_DIR } else { DT_REG };
        (ino(c as usize), t, child.name.as_bytes())
    }))
}

/// Directory size: entries packed into DIRBLKSIZ chunks, none straddling a chunk. Errors
/// past `MAXDIRSIZE`. ponytail: `dir_bytes` builds a directory whole in memory (~64 bytes
/// an entry, so a 100K-entry directory is ~6 MB); the cap bounds it at 2 GiB.
pub fn dir_size(nodes: &[Node], i: usize) -> Result<u64> {
    let size = packed_size(nodes, i);
    if size > MAXDIRSIZE {
        return Err(Error::Format(format!(
            "directory {:?} needs {size} bytes of entries; UFS allows {MAXDIRSIZE}",
            nodes[i].name
        )));
    }
    Ok(size)
}

fn packed_size(nodes: &[Node], i: usize) -> u64 {
    let mut chunks = 1u64;
    let mut used = 0usize;
    for (_, _, name) in entries(nodes, i) {
        let need = dirsiz(name.len());
        if used + need > DIRBLKSIZ {
            chunks += 1;
            used = 0;
        }
        used += need;
    }
    chunks * DIRBLKSIZ as u64
}

/// The directory's bytes (exactly `dir_size` long). The last entry of each chunk takes
/// the chunk's leftover space in its record length, as the kernel and newfs do.
pub fn dir_bytes(nodes: &[Node], i: usize) -> Vec<u8> {
    let mut out = vec![0u8; nodes[i].size as usize];
    let mut chunk = 0usize;
    let mut used = 0usize;
    let mut last: Option<usize> = None; // offset of the previous entry in this chunk
    for (ino, t, name) in entries(nodes, i) {
        let need = dirsiz(name.len());
        if used + need > DIRBLKSIZ {
            if let Some(at) = last {
                let reclen = (chunk + DIRBLKSIZ - at) as u16;
                out[at + 4..at + 6].copy_from_slice(&reclen.to_le_bytes());
            }
            chunk += DIRBLKSIZ;
            used = 0;
        }
        let at = chunk + used;
        out[at..at + 4].copy_from_slice(&ino.to_le_bytes());
        out[at + 4..at + 6].copy_from_slice(&(need as u16).to_le_bytes());
        out[at + 6] = t;
        out[at + 7] = name.len() as u8;
        out[at + 8..at + 8 + name.len()].copy_from_slice(name);
        used += need;
        last = Some(at);
    }
    if let Some(at) = last {
        let reclen = (chunk + DIRBLKSIZ - at) as u16;
        out[at + 4..at + 6].copy_from_slice(&reclen.to_le_bytes());
    }
    out
}
