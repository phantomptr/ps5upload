//! A `.ffpkg` game image as a source tree: the UFS2 reader, wrapped for the converter.

use std::path::{Path, PathBuf};

use ps5upload_pkg::ufs2::{Inode, Ufs2Error, Ufs2Image, ROOT_INODE};

use crate::source::{is_junk, SourceFile, SourceTree};
use crate::{format_err, Result};

/// A directory tree deeper than this is refused, as the reader's own walker does.
const MAX_DEPTH: u32 = 32;

impl From<Ufs2Error> for crate::Error {
    fn from(e: Ufs2Error) -> Self {
        crate::Error::Format(e.to_string())
    }
}

/// A game image's files and the inodes they came from, in the same order.
pub struct Ufs2Source {
    path: PathBuf,
    image: Ufs2Image<std::fs::File>,
    files: Vec<SourceFile>,
    inodes: Vec<Inode>,
}

impl Ufs2Source {
    pub fn open(path: &Path) -> Result<Self> {
        let mut image = Ufs2Image::open(path)
            .map_err(|e| crate::Error::Format(format!("{}: {e}", path.display())))?;
        let mut files = Vec::new();
        let mut inodes = Vec::new();
        let root = image.read_inode(ROOT_INODE)?;
        walk(&mut image, &root, "", 0, &mut files, &mut inodes)?;
        if files.is_empty() {
            return format_err(format!("{} holds no files", path.display()));
        }
        let order: Vec<usize> = {
            let mut idx: Vec<usize> = (0..files.len()).collect();
            idx.sort_by(|a, b| files[*a].path.cmp(&files[*b].path));
            idx
        };
        let files = order.iter().map(|&i| files[i].clone()).collect();
        let inodes = order.iter().map(|&i| inodes[i].clone()).collect();
        Ok(Self {
            path: path.to_path_buf(),
            image,
            files,
            inodes,
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
            "ffpkg {} (UFS2, {} KiB blocks, {} files)",
            self.path.display(),
            self.image.superblock.block_size / 1024,
            self.files.len()
        )
    }
}

fn walk(
    image: &mut Ufs2Image<std::fs::File>,
    dir: &Inode,
    prefix: &str,
    depth: u32,
    files: &mut Vec<SourceFile>,
    inodes: &mut Vec<Inode>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return format_err(format!("{prefix} nests deeper than {MAX_DEPTH} levels"));
    }
    let entries: Vec<_> = image
        .list_dir(dir)?
        .into_iter()
        .filter(|e| !e.name.is_empty() && !is_junk(&e.name))
        .collect();
    // The children's inodes, in runs the image can hand over in one read each. A game
    // mount holds a quarter of a million files, and one syscall apiece took minutes.
    let mut inodes_by_entry: Vec<Option<Inode>> = vec![None; entries.len()];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].inode);
    let mut run: Vec<usize> = Vec::new();
    for &i in order.iter() {
        let fits = run.first().is_none_or(|&first| {
            let per_cg = u64::from(image.superblock.inodes_per_cg);
            entries[first].inode / per_cg == entries[i].inode / per_cg
        });
        if !fits {
            read_run(image, &entries, &run, &mut inodes_by_entry);
            run.clear();
        }
        run.push(i);
    }
    read_run(image, &entries, &run, &mut inodes_by_entry);

    for (i, entry) in entries.iter().enumerate() {
        // A directory entry pointing at an unusable inode is the image's problem, not the
        // caller's: skip it rather than fail the walk.
        let Some(inode) = inodes_by_entry[i].take() else {
            continue;
        };
        let path = format!("{prefix}{}", entry.name);
        if inode.is_dir() {
            walk(image, &inode, &format!("{path}/"), depth + 1, files, inodes)?;
        } else if inode.is_file() {
            files.push(SourceFile {
                path,
                size: inode.size,
            });
            inodes.push(inode);
        }
    }
    Ok(())
}

/// Reads one run of entries' inodes — a contiguous, same-cylinder-group stretch — in a
/// single call, ignoring entries whose inode the image cannot produce.
fn read_run(
    image: &mut Ufs2Image<std::fs::File>,
    entries: &[ps5upload_pkg::ufs2::DirEntry],
    run: &[usize],
    out: &mut [Option<Inode>],
) {
    let Some(&first) = run.first() else {
        return;
    };
    let start = entries[first].inode;
    let count = run
        .last()
        .map(|&i| entries[i].inode - start + 1)
        .unwrap_or(0);
    let Ok(batch) = image.read_inodes(start, count) else {
        return;
    };
    for &i in run {
        let at = (entries[i].inode - start) as usize;
        if let Some(inode) = batch.get(at) {
            out[i] = Some(inode.clone());
        }
    }
}
