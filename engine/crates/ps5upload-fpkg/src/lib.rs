//! Read (and later write) PS5 debug FPKG packages.
//!
//! Written from the format as measured on real packages; see
//! docs/superpowers/specs/2026-09-13-fpkg-builder-design.md.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub mod build;
pub mod cnt;
pub mod cnt_write;
pub mod crypto;
pub mod exfat;
pub mod fih;
pub mod fih_write;
pub mod flt;
pub mod inner;
pub mod keys;
pub mod naps;
pub mod outer;
pub mod outer_write;
pub mod pfsc;
pub mod pfsimage;
pub mod plan;
pub mod playgo;
pub mod rsa;
pub mod sdk_rules;
pub mod self_repair;
pub mod si;
pub mod si_write;
pub mod source;
pub mod stream;
pub mod ufs2_source;
pub mod verify;
pub mod xts;

/// Every PFS and finalized-image block is 64 KiB.
pub const BLOCK: u64 = 0x10000;

/// The 16 bytes a plaintext package carries where a native one carries its random seed:
/// written at the outer superblock's seed slot (`0x370`) and at the container's (`0x4A0`).
///
/// It is a marker, not key material. The console mounts game data through `PfsMountGameData_PPR`,
/// the A53-served read path that drakmor's `ppr-patch` patches, and only an image *marked* this way
/// is served without authentication — a native image is verified against Sony's keys and fails for
/// anything not signed with them. Measured on a package the console mounts: those 16 bytes appear
/// in both slots, its outer blocks are stored in the clear, and its SHA3 digests and superblock ICV
/// are unchanged. `outer::open` reads the marker back, so the reader tells the two apart from the
/// package itself and needs no flag.
pub const PLAINTEXT_MARKER: [u8; 16] = *b"PPRPLAIN-NOAUTH!";

/// How the outer image's blocks are stored.
///
/// `PlaintextNoAuth` is the default because it is the only mode that mounts: the blocks are stored
/// in the clear and the seed slot carries [`PLAINTEXT_MARKER`]. `Native` AES-XTS encrypts every
/// block but the superblock with keys derived from a random seed — the shape Sony's own debug
/// packages have — and the console's A53 authenticates it, so a package built here cannot pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageMode {
    #[default]
    PlaintextNoAuth,
    Native,
}

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
