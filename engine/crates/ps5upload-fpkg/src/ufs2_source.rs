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
    for entry in image.list_dir(dir)? {
        if entry.name.is_empty() || is_junk(&entry.name) {
            continue;
        }
        let inode = match image.read_inode(entry.inode) {
            Ok(inode) => inode,
            // A directory entry pointing at an unusable inode is the image's
            // problem, not the caller's: skip it rather than fail the walk.
            Err(_) => continue,
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
