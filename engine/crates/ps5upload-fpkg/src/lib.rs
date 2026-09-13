//! Read (and later write) PS5 debug FPKG packages.
//!
//! Written from the format as measured on real packages; see
//! docs/superpowers/specs/2026-09-13-fpkg-builder-design.md.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub mod cnt;
pub mod crypto;
pub mod fih;
pub mod flt;
pub mod inner;
pub mod keys;
pub mod outer;
pub mod plan;
pub mod rsa;
pub mod si;
pub mod source;
pub mod verify;
pub mod xts;

/// Every PFS and finalized-image block is 64 KiB.
pub const BLOCK: u64 = 0x10000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

pub(crate) fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn i32le(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub(crate) fn format_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}

/// A package on disk, read by offset.
pub struct PkgFile {
    file: std::fs::File,
    len: u64,
}

impl PkgFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exactly `len` bytes at `off`, or an error if the file is shorter.
    pub fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>> {
        let end = off.checked_add(len as u64);
        if end.is_none_or(|e| e > self.len) {
            return format_err(format!(
                "read of {len} bytes at {off:#x} past end {:#x}",
                self.len
            ));
        }
        self.file.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}
