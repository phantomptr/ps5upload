//! Where an upload reads its source from. Local disk by default; the engine plugs in a saved
//! server (SMB, FTP, SFTP) so a folder on a NAS uploads to the console without a local copy.
//!
//! Only the plain file and folder uploads read through this. Archive uploads (zip, rar, 7z) read
//! their archive from local disk.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

pub struct SourceMeta {
    pub len: u64,
    pub is_dir: bool,
    pub is_file: bool,
}

pub trait SourceFs: Send + Sync + std::fmt::Debug {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>>;
    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta>;
    /// Direct children of `p` as (full path, is_dir).
    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>>;
}

/// This computer's disk.
#[derive(Debug, Default)]
pub struct LocalFs;

impl SourceFs for LocalFs {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>> {
        Ok(Box::new(std::fs::File::open(p)?))
    }
    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta> {
        let m = std::fs::metadata(p)?;
        Ok(SourceMeta {
            len: m.len(),
            is_dir: m.is_dir(),
            is_file: m.is_file(),
        })
    }
    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
        std::fs::read_dir(p)?
            .map(|e| {
                let e = e?;
                let path = e.path();
                let is_dir = path.is_dir();
                Ok((path, is_dir))
            })
            .collect()
    }
}
