//! A `.ffpkg` game image as a source tree: the UFS2 reader, wrapped for the converter.

use std::path::Path;

use ps5upload_pkg::ufs2::{Inode, Ufs2Error, Ufs2Image, ROOT_INODE};

use crate::source::{dir_label, is_junk, SourceFile, SourceTree};
use crate::{format_err, ReadSeek, Result};

/// A directory tree deeper than this is refused, as the reader's own walker does.
const MAX_DEPTH: u32 = 32;
/// An image listing more files and directories than this is refused, as an exFAT one is: a
/// game package holds at most half a million files (see
/// [`crate::sdk_rules::LARGE_PACKAGE_LV2_FILE_LIMIT`]).
const MAX_ENTRIES: usize = 1_000_000;
/// The most path bytes a walk keeps (every file's, every empty directory's). A real game's are
/// a few tens of megabytes; deep trees of long names in a corrupt image would be gigabytes.
const MAX_PATH_BYTES: usize = 256 << 20;
/// How many special files an error lists by path; the rest are counted.
const MAX_SPECIAL_LISTED: usize = 50;

impl From<Ufs2Error> for crate::Error {
    fn from(e: Ufs2Error) -> Self {
        crate::Error::Format(e.to_string())
    }
}

/// A game image's files and the inodes they came from, in the same order.
pub struct Ufs2Source {
    label: String,
    image: Ufs2Image<Box<dyn ReadSeek>>,
    files: Vec<SourceFile>,
    inodes: Vec<Inode>,
    empty_dirs: Vec<String>,
}

impl Ufs2Source {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(Box::new(file), format!("ffpkg {}", path.display()))
    }

    /// A UFS2 image in any seekable bytes; `label` names it in errors and logs.
    pub fn from_reader(reader: Box<dyn ReadSeek>, label: String) -> Result<Self> {
        let mut image = Ufs2Image::from_reader(reader)
            .map_err(|e| crate::Error::Format(format!("{label}: {e}")))?;
        let mut files = Vec::new();
        let mut inodes = Vec::new();
        let mut empty_dirs = Vec::new();
        let root = image.read_inode(ROOT_INODE)?;
        let mut tree = Walk {
            files: &mut files,
            inodes: &mut inodes,
            empty: &mut empty_dirs,
            dirs: std::collections::HashSet::from([ROOT_INODE]),
            path_bytes: 0,
            listed: 0,
            special: Vec::new(),
            special_count: 0,
        };
        walk(&mut image, &root, "", 0, &mut tree)?;
        if tree.special_count > 0 {
            let more = tree.special_count - tree.special.len();
            return format_err(format!(
                "{label} holds {} special file(s) (symlinks, devices, FIFOs, sockets), which a \
                 package cannot hold:\n  {}{}",
                tree.special_count,
                tree.special.join("\n  "),
                if more > 0 {
                    format!("\n  ... and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        empty_dirs.sort();
        if files.is_empty() {
            return format_err(format!("{label} holds no files"));
        }
        let order: Vec<usize> = {
            let mut idx: Vec<usize> = (0..files.len()).collect();
            idx.sort_by(|a, b| files[*a].path.cmp(&files[*b].path));
            idx
        };
        let files = order.iter().map(|&i| files[i].clone()).collect();
        let inodes = order.iter().map(|&i| inodes[i].clone()).collect();
        Ok(Self {
            label,
            image,
            files,
            inodes,
            empty_dirs,
        })
    }

    fn at(&self, path: &str) -> Result<usize> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map_err(|_| crate::Error::Format(format!("{path} is not in this .ffpkg")))
    }
}

impl SourceTree for Ufs2Source {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// A game image keeps its empty directories as a folder does: a build from the image
    /// must lay out the same tree as a build from the folder it was made of.
    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let inode = self.inodes[i].clone();
        // The cap is the inode's own size, read off the image: a truncated or
        // oversized read is then impossible, and a hostile size is still bounded
        // by what the reader will allocate.
        Ok(self.image.read_file(&inode, inode.size)?)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let inode = self.inodes[i].clone();
        Ok(self.image.read_range(&inode, offset, len as u64)?)
    }

    fn describe(&self) -> String {
        format!(
            "{} (UFS2, {} KiB blocks, {} files)",
            self.label,
            self.image.superblock.block_size / 1024,
            self.files.len()
        )
    }
}

/// What a walk collects: files with their inodes, in the same order, and empty directories.
struct Walk<'a> {
    files: &'a mut Vec<SourceFile>,
    inodes: &'a mut Vec<Inode>,
    empty: &'a mut Vec<String>,
    /// Directory inodes already walked. UFS links a directory from one parent only, so a
    /// second link is a cycle (or a fan-out the depth limit alone would let grow
    /// exponentially) in a corrupt image.
    dirs: std::collections::HashSet<u64>,
    /// Bytes of the paths kept so far, against [`MAX_PATH_BYTES`].
    path_bytes: usize,
    /// Entries listed so far, every directory's (each is listed once), against
    /// [`MAX_ENTRIES`]: counted as they are listed, so the entries of directories still being
    /// walked further up count too.
    listed: usize,
    /// Entries that are neither files nor directories, as `path (kind)`: the first
    /// [`MAX_SPECIAL_LISTED`], refused together once the walk is done.
    special: Vec<String>,
    /// How many such entries there are in all.
    special_count: usize,
}

impl Walk<'_> {
    /// Count one more kept path against the walk's budgets.
    fn keep(&mut self, path: &str) -> Result<()> {
        self.path_bytes += path.len();
        if self.path_bytes > MAX_PATH_BYTES {
            return format_err("the image's paths are over 256 MiB");
        }
        Ok(())
    }
}

/// Walks one directory; returns whether anything under it was kept.
fn walk(
    image: &mut Ufs2Image<Box<dyn ReadSeek>>,
    dir: &Inode,
    prefix: &str,
    depth: u32,
    tree: &mut Walk,
) -> Result<bool> {
    if depth > MAX_DEPTH {
        return format_err(format!("{prefix} nests deeper than {MAX_DEPTH} levels"));
    }
    let entries: Vec<_> = image
        .list_dir(dir)
        .map_err(|e| crate::Error::Format(format!("{}: {e}", dir_label(prefix))))?
        .into_iter()
        .filter(|e| !e.name.is_empty() && !is_junk(&e.name))
        .collect();
    tree.listed += entries.len();
    if tree.listed > MAX_ENTRIES {
        return format_err("the image holds more than a million files and directories");
    }
    // The children's inodes, in runs the image can hand over in one read each. A game
    // mount holds a quarter of a million files, and one syscall apiece took minutes.
    let mut inodes_by_entry: Vec<Option<Inode>> = vec![None; entries.len()];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].inode);
    let mut run: Vec<usize> = Vec::new();
    for &i in order.iter() {
        // One cylinder group, and no more than the reader takes in one call: entries far apart
        // in a group with a huge inode count would otherwise ask for one enormous read.
        let fits = run.first().is_none_or(|&first| {
            let per_cg = u64::from(image.superblock.inodes_per_cg);
            entries[first].inode / per_cg == entries[i].inode / per_cg
                && entries[i].inode - entries[first].inode < ps5upload_pkg::ufs2::MAX_INODE_RUN
        });
        if !fits {
            read_run(image, &entries, &run, prefix, &mut inodes_by_entry)?;
            run.clear();
        }
        run.push(i);
    }
    read_run(image, &entries, &run, prefix, &mut inodes_by_entry)?;

    let mut kept = false;
    for (i, entry) in entries.iter().enumerate() {
        let path = format!("{prefix}{}", entry.name);
        let Some(inode) = inodes_by_entry[i].take() else {
            return format_err(format!("{path}: its inode was not read"));
        };
        if inode.is_dir() {
            if !tree.dirs.insert(inode.number) {
                return format_err(format!(
                    "{path} links a directory a second time (inode {})",
                    inode.number
                ));
            }
            if !walk(image, &inode, &format!("{path}/"), depth + 1, tree)? {
                tree.keep(&path)?;
                tree.empty.push(path);
            }
            kept = true;
        } else if inode.is_file() {
            tree.keep(&path)?;
            tree.files.push(SourceFile {
                path,
                size: inode.size,
            });
            tree.inodes.push(inode);
            kept = true;
        } else {
            // Not dropped quietly: the package would lack a path the game may open.
            tree.special_count += 1;
            if tree.special.len() < MAX_SPECIAL_LISTED {
                tree.special
                    .push(format!("{path} ({})", special_kind(inode.mode)));
            }
        }
    }
    Ok(kept)
}

/// What a non-file, non-directory inode is, by its mode's type bits.
fn special_kind(mode: u16) -> &'static str {
    match mode & 0xF000 {
        0x1000 => "FIFO",
        0x2000 => "character device",
        0x6000 => "block device",
        0xA000 => "symlink",
        0xC000 => "socket",
        0xE000 => "whiteout",
        0 => "unallocated inode",
        _ => "unknown type",
    }
}

/// Reads one run of entries' inodes — a contiguous, same-cylinder-group stretch — in a
/// single call. An inode the image cannot produce is an error: skipping the entry would
/// leave its file out of the package.
fn read_run(
    image: &mut Ufs2Image<Box<dyn ReadSeek>>,
    entries: &[ps5upload_pkg::ufs2::DirEntry],
    run: &[usize],
    prefix: &str,
    out: &mut [Option<Inode>],
) -> Result<()> {
    let Some(&first) = run.first() else {
        return Ok(());
    };
    let start = entries[first].inode;
    let count = run
        .last()
        .map(|&i| entries[i].inode - start + 1)
        .unwrap_or(0);
    let batch = image.read_inodes(start, count).map_err(|e| {
        let others = match run.len() - 1 {
            0 => String::new(),
            n => format!(" and {n} other entries"),
        };
        crate::Error::Format(format!(
            "cannot read the inode of {prefix}{}{others}: {e}",
            entries[first].name
        ))
    })?;
    for &i in run {
        let at = (entries[i].inode - start) as usize;
        out[i] = batch.get(at).cloned();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: usize = 4096;
    const IBLKNO: usize = 20;

    /// A one-cylinder-group UFS2 image built in memory: 4 KiB blocks and fragments, the inode
    /// table at fragment 20, data allocated after it.
    pub(crate) struct Image {
        pub bytes: Vec<u8>,
        next: usize,
    }

    impl Image {
        pub fn new(inodes_per_cg: u32, frags: usize) -> Self {
            let mut bytes = vec![0u8; frags * FS];
            let sb = 65536;
            let mut put32 = |at: usize, v: u32| {
                bytes[sb + at..sb + at + 4].copy_from_slice(&v.to_le_bytes());
            };
            put32(16, IBLKNO as u32);
            put32(44, 1);
            put32(48, FS as u32);
            put32(52, FS as u32);
            put32(184, inodes_per_cg);
            put32(188, frags as u32);
            put32(1372, ps5upload_pkg::ufs2::UFS2_MAGIC);
            bytes[sb + 1080..sb + 1088].copy_from_slice(&(frags as u64).to_le_bytes());
            let table = (inodes_per_cg as usize * 256).div_ceil(FS);
            Self {
                bytes,
                next: IBLKNO + table,
            }
        }

        /// Blocks holding `data`, with a single-indirect block past the twelfth.
        fn store(&mut self, data: &[u8]) -> ([u64; 12], u64) {
            let mut blocks = Vec::new();
            for chunk in data.chunks(FS) {
                let at = self.next * FS;
                self.bytes[at..at + chunk.len()].copy_from_slice(chunk);
                blocks.push(self.next as u64);
                self.next += 1;
            }
            let mut direct = [0u64; 12];
            for (slot, b) in direct.iter_mut().zip(&blocks) {
                *slot = *b;
            }
            let mut indirect = 0;
            if blocks.len() > 12 {
                assert!(
                    blocks.len() - 12 <= FS / 8,
                    "the fixture has one indirect block"
                );
                indirect = self.next as u64;
                let at = self.next * FS;
                for (i, b) in blocks[12..].iter().enumerate() {
                    self.bytes[at + i * 8..at + i * 8 + 8].copy_from_slice(&b.to_le_bytes());
                }
                self.next += 1;
            }
            (direct, indirect)
        }

        fn inode(&mut self, n: u64, mode: u16, size: u64, direct: [u64; 12], indirect: u64) {
            let at = IBLKNO * FS + n as usize * 256;
            let raw = &mut self.bytes[at..at + 256];
            raw[0..2].copy_from_slice(&mode.to_le_bytes());
            raw[16..24].copy_from_slice(&size.to_le_bytes());
            for (i, b) in direct.iter().enumerate() {
                raw[112 + i * 8..120 + i * 8].copy_from_slice(&b.to_le_bytes());
            }
            raw[208..216].copy_from_slice(&indirect.to_le_bytes());
        }

        pub fn file(&mut self, n: u64, data: &[u8]) {
            let (direct, indirect) = self.store(data);
            self.inode(n, 0x81A4, data.len() as u64, direct, indirect);
        }

        /// A directory: `.`, `..`, then `entries` as `(name, inode)`, the last record running
        /// to the end of its block as UFS lays them out.
        pub fn dir(&mut self, n: u64, parent: u64, entries: &[(&str, u64)]) {
            let mut data = Vec::new();
            let mut last = 0;
            let all = [(".", n), ("..", parent)];
            for (name, ino) in all.iter().copied().chain(entries.iter().copied()) {
                let reclen = (8 + name.len() + 1).next_multiple_of(4);
                if data.len() / FS != (data.len() + reclen - 1) / FS {
                    // A record never crosses a block: the previous one takes up the slack.
                    let pad = data.len().next_multiple_of(FS) - data.len();
                    let grown = u16::from_le_bytes([data[last + 4], data[last + 5]]) as usize + pad;
                    data[last + 4..last + 6].copy_from_slice(&(grown as u16).to_le_bytes());
                    data.resize(data.len() + pad, 0);
                }
                last = data.len();
                data.extend_from_slice(&(ino as u32).to_le_bytes());
                data.extend_from_slice(&(reclen as u16).to_le_bytes());
                data.push(if name.starts_with('.') && name.len() <= 2 {
                    4
                } else {
                    8
                });
                data.push(name.len() as u8);
                data.extend_from_slice(name.as_bytes());
                data.resize(last + reclen, 0);
            }
            let pad = data.len().next_multiple_of(FS) - data.len();
            let grown = u16::from_le_bytes([data[last + 4], data[last + 5]]) as usize + pad;
            data[last + 4..last + 6].copy_from_slice(&(grown as u16).to_le_bytes());
            data.resize(data.len() + pad, 0);
            let (direct, indirect) = self.store(&data);
            self.inode(n, 0x41ED, data.len() as u64, direct, indirect);
        }

        pub fn open(self) -> Result<Ufs2Source> {
            Ufs2Source::from_reader(Box::new(std::io::Cursor::new(self.bytes)), "test".into())
        }
    }

    #[test]
    fn empty_directories_are_kept() {
        let mut img = Image::new(64, 64);
        img.dir(
            ROOT_INODE,
            ROOT_INODE,
            &[("eboot.bin", 3), ("empty", 4), ("junk", 5), ("nest", 7)],
        );
        img.file(3, b"eboot");
        img.dir(4, ROOT_INODE, &[]);
        img.dir(5, ROOT_INODE, &[(".DS_Store", 6)]);
        img.file(6, b"junk");
        img.dir(7, ROOT_INODE, &[("deeper", 8)]);
        img.dir(8, 7, &[]);
        let mut source = img.open().unwrap();
        let files: Vec<&str> = source.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["eboot.bin"]);
        // A directory holding only junk is empty; one holding an empty directory is not.
        assert_eq!(source.empty_dirs(), ["empty", "junk", "nest/deeper"]);
        assert_eq!(source.read("eboot.bin").unwrap(), b"eboot");
    }

    /// Thousands of long names in one directory: more than 1 MiB of entries, all read.
    #[test]
    fn a_directory_with_thousands_of_long_names_walks() {
        let names: Vec<String> = (0..5000).map(|i| format!("{i:0200}")).collect();
        let entries: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 3)).collect();
        let mut img = Image::new(64, 400);
        img.dir(ROOT_INODE, ROOT_INODE, &[("data", 4)]);
        img.dir(4, ROOT_INODE, &entries);
        img.file(3, b"same bytes");
        let source = img.open().unwrap();
        assert_eq!(source.files().len(), 5000);
        assert_eq!(source.files()[0].path, format!("data/{}", names[0]));
    }

    /// A directory linked from two places — here back to the root — is a corrupt image.
    #[test]
    fn a_directory_cycle_is_refused() {
        let mut img = Image::new(64, 64);
        img.dir(ROOT_INODE, ROOT_INODE, &[("eboot.bin", 3), ("loop", 4)]);
        img.file(3, b"eboot");
        img.dir(4, ROOT_INODE, &[("again", ROOT_INODE)]);
        let Err(e) = img.open() else {
            panic!("a cycle must not walk");
        };
        assert!(e.to_string().contains("a second time"), "{e}");
    }

    /// A '/' or a NUL in a name is refused with the directory it is in, not walked as a
    /// subfolder or cut short.
    #[test]
    fn a_bad_name_is_refused_with_its_directory() {
        for (name, want) in [
            (
                "a/b",
                "directory data: directory inode 4 holds a name that contains a '/' (bytes 612f62)",
            ),
            (
                "a\0b",
                "directory data: directory inode 4 holds a name that contains a NUL (bytes 610062)",
            ),
        ] {
            let mut img = Image::new(64, 64);
            img.dir(ROOT_INODE, ROOT_INODE, &[("eboot.bin", 3), ("data", 4)]);
            img.file(3, b"eboot");
            img.dir(4, ROOT_INODE, &[(name, 3)]);
            let Err(e) = img.open() else {
                panic!("{name:?} must not walk");
            };
            assert!(e.to_string().contains(want), "{e}");
        }
    }

    /// Symlinks, devices, FIFOs, sockets and entries at a free inode are refused together,
    /// by path, after the walk, not left out of the package.
    #[test]
    fn special_files_are_refused_by_path() {
        let mut img = Image::new(64, 64);
        img.dir(
            ROOT_INODE,
            ROOT_INODE,
            &[
                ("eboot.bin", 3),
                ("link", 4),
                ("sub", 5),
                ("pipe", 6),
                ("free", 9),
            ],
        );
        img.file(3, b"eboot");
        img.inode(4, 0xA1FF, 0, [0; 12], 0);
        img.dir(5, ROOT_INODE, &[("tty", 7), ("disk", 8), ("sock", 10)]);
        img.inode(6, 0x11A4, 0, [0; 12], 0);
        img.inode(7, 0x21A4, 0, [0; 12], 0);
        img.inode(8, 0x61A4, 0, [0; 12], 0);
        img.inode(10, 0xC1ED, 0, [0; 12], 0);
        let Err(e) = img.open() else {
            panic!("special files must not walk");
        };
        let e = e.to_string();
        assert!(e.contains("holds 6 special file(s)"), "{e}");
        for want in [
            "link (symlink)",
            "pipe (FIFO)",
            "free (unallocated inode)",
            "sub/tty (character device)",
            "sub/disk (block device)",
            "sub/sock (socket)",
        ] {
            assert!(e.contains(want), "{want}: {e}");
        }
        assert!(!e.contains("eboot.bin"), "{e}");
    }

    /// The list stops at [`MAX_SPECIAL_LISTED`] paths and counts the rest.
    #[test]
    fn a_long_list_of_special_files_is_bounded() {
        let names: Vec<String> = (0..MAX_SPECIAL_LISTED + 10)
            .map(|i| format!("l{i:03}"))
            .collect();
        let mut entries: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 4)).collect();
        entries.push(("eboot.bin", 3));
        let mut img = Image::new(64, 64);
        img.dir(ROOT_INODE, ROOT_INODE, &entries);
        img.file(3, b"eboot");
        img.inode(4, 0xA1FF, 0, [0; 12], 0);
        let Err(e) = img.open() else {
            panic!("symlinks must not walk");
        };
        let e = e.to_string();
        assert!(e.contains("holds 60 special file(s)"), "{e}");
        assert_eq!(e.matches("(symlink)").count(), MAX_SPECIAL_LISTED, "{e}");
        assert!(e.ends_with("... and 10 more"), "{e}");
    }

    /// An entry whose inode the image cannot produce fails the walk, by path, rather than
    /// vanishing with the rest of its run.
    #[test]
    fn an_unreadable_inode_is_an_error() {
        // Out of range: the image has 64 inodes.
        let mut img = Image::new(64, 64);
        img.dir(ROOT_INODE, ROOT_INODE, &[("eboot.bin", 3), ("data", 4)]);
        img.file(3, b"eboot");
        img.dir(4, ROOT_INODE, &[("lost", 64)]);
        let Err(e) = img.open() else {
            panic!("an out-of-range inode must not walk");
        };
        let e = e.to_string();
        assert!(e.contains("cannot read the inode of data/lost: "), "{e}");
        assert!(e.contains("out of range"), "{e}");

        // Past the end of the image, in one run with a good inode: neither is dropped.
        let mut img = Image::new(64, 64);
        img.dir(ROOT_INODE, ROOT_INODE, &[("eboot.bin", 3), ("far", 8000)]);
        img.file(3, b"eboot");
        // 16,384 inodes in a 1,100-fragment group: inode 8000 lies past the 256 KiB image.
        let sb = 65536;
        img.bytes[sb + 184..sb + 188].copy_from_slice(&16_384u32.to_le_bytes());
        img.bytes[sb + 188..sb + 192].copy_from_slice(&1100u32.to_le_bytes());
        let Err(e) = img.open() else {
            panic!("a truncated inode table must not walk");
        };
        let e = e.to_string();
        assert!(
            e.contains("cannot read the inode of eboot.bin and 1 other entries: io:"),
            "{e}"
        );
    }

    /// Children far apart in a group with many inodes are read in more than one run, and none
    /// is dropped.
    #[test]
    fn children_far_apart_are_all_read() {
        let far = ps5upload_pkg::ufs2::MAX_INODE_RUN + 100;
        // 16,384 inodes: a 4 MiB table, with data after it.
        let mut img = Image::new(16_384, 1100);
        img.dir(ROOT_INODE, ROOT_INODE, &[("a.bin", 3), ("b.bin", far)]);
        img.file(3, b"near");
        img.file(far, b"far away");
        let mut source = img.open().unwrap();
        let files: Vec<&str> = source.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["a.bin", "b.bin"]);
        assert_eq!(source.read("b.bin").unwrap(), b"far away");
    }
}
