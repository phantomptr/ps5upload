//! Read-only UFS2 image reader.
//!
//! Implements just enough of FreeBSD's UFS2 (BSD Fast File System v2)
//! to enumerate directory trees and extract individual files from a
//! local image. PS5 game backups in `.ffpkg` format are UFS2
//! filesystems — letting users browse them locally before uploading
//! cuts the "send 50 GB to PS5 just to find out it's the wrong game"
//! pain.
//!
//! What's implemented:
//!   - Superblock parse + validate (UFS2 magic 0x19540119 only)
//!   - Inode read (256-byte UFS2 inode, by number)
//!   - Directory listing (variable-length entries, DIRBLKSIZ-aligned)
//!   - File read via direct + single/double/triple indirect blocks
//!
//! What's NOT implemented (not needed for inspect-only use):
//!   - Filesystem creation, modification, fsck, growfs
//!   - UFS1 (32-bit pointers) — PS5 backups are always UFS2
//!   - Soft updates / journaling state
//!   - Snapshots
//!   - Writing inodes or block bitmaps
//!
//! Reference: the FreeBSD UFS2 on-disk layout (`sys/ufs/ffs/fs.h`,
//! `sys/ufs/ufs/dinode.h`, `sys/ufs/ufs/dir.h`). All field offsets,
//! magic constants, and inode/dir structures come from those public
//! FreeBSD source headers and have been stable since FreeBSD 5.x.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;
use thiserror::Error;

/// UFS2 superblock magic. Verified against FreeBSD sys/ufs/ffs/fs.h.
pub const UFS2_MAGIC: u32 = 0x19540119;

/// Standard UFS2 superblock offset. UFS2 puts the superblock at 64 KiB
/// in to leave room for boot sector/other partition metadata. Older
/// UFS1 had it at 8 KiB. PS5's .ffpkg uses the UFS2 location.
pub const UFS2_SUPERBLOCK_OFFSET: u64 = 65536;

/// Number of direct block pointers in a UFS2 inode.
const NDADDR: usize = 12;
/// Number of indirect block pointer levels (single, double, triple).
const NIADDR: usize = 3;
/// Inode size for UFS2 (UFS1 was 128).
const INODE_SIZE: u64 = 256;
/// Directory block size — directory entries can't span this boundary.
/// Currently unused because we read the entire directory at once and
/// trust the embedded record-length field; if we ever stream large
/// directories we'd need to align reads to this boundary.
#[allow(dead_code)]
const DIRBLKSIZ: u64 = 512;
/// Root inode number. Always 2 in UFS — inode 0 is reserved, 1 is the
/// bad-block list (historic).
pub const ROOT_INODE: u64 = 2;

/// Inode mode bit field constants. Mirrors POSIX `mode_t` layout —
/// not platform-specific, defined by UFS spec.
const IFMT: u16 = 0xF000;
const IFDIR: u16 = 0x4000;
const IFREG: u16 = 0x8000;
const IFLNK: u16 = 0xA000;

/// Errors a UFS2 inspection can raise. Most are corruption indicators
/// — a healthy .ffpkg should never trigger them.
#[derive(Debug, Error)]
pub enum Ufs2Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "not a UFS2 image: magic at offset {offset} was 0x{found:08x}, expected 0x{UFS2_MAGIC:08x}"
    )]
    BadMagic { offset: u64, found: u32 },
    /// Superblock magic was correct but a structural field (block_size,
    /// fragment_size, cg_count, etc.) failed sanity checks. Distinct
    /// from BadMagic so the error message + UI surface "image is
    /// corrupt" rather than "this isn't a UFS2 image", which mislead
    /// users when a real UFS2 image had a corrupted structural field.
    #[error("UFS2 superblock has bad {field}: {value}")]
    BadSuperblock { field: &'static str, value: u64 },
    #[error("inode {inode} has unrecognized type bits 0x{mode:04x}")]
    BadInodeMode { inode: u64, mode: u16 },
    #[error("inode number {inode} is out of range (image has {max} inodes)")]
    InodeOutOfRange { inode: u64, max: u64 },
    #[error("directory tree exceeds max depth {max} — refusing to recurse (possible cycle or pathological nesting in the image)")]
    WalkTooDeep { max: u32 },
    #[error("inode {inode} would point at block {block} which is past the image's {total_blocks} blocks")]
    BlockOutOfRange {
        inode: u64,
        block: u64,
        total_blocks: u64,
    },
    #[error("directory entry at offset {offset} has invalid record length {rec_len}")]
    BadDirEntry { offset: u64, rec_len: u16 },
    #[error("file is too large to read: {size} bytes; cap is {cap} bytes")]
    FileTooLarge { size: u64, cap: u64 },
    #[error("path component not found: {component}")]
    NotFound { component: String },
}

/// Subset of the superblock we actually use for reading. The full
/// FreeBSD `struct fs` is ~1376 bytes with 60+ fields; ours is the
/// minimum needed to walk inodes and blocks.
#[derive(Debug, Clone, Serialize)]
pub struct Superblock {
    /// fs_bsize — block size in bytes (typically 32768 on PS5 .ffpkg).
    pub block_size: u32,
    /// fs_fsize — fragment size (typically 4096). All disk offsets are
    /// in fragments, but reads are always block-aligned.
    pub fragment_size: u32,
    /// fs_size — total fragments in filesystem.
    pub size_fragments: u64,
    /// fs_ncg — number of cylinder groups.
    pub cg_count: u32,
    /// fs_ipg — inodes per cylinder group.
    pub inodes_per_cg: u32,
    /// fs_fpg — fragments per cylinder group.
    pub fragments_per_cg: u32,
    /// fs_iblkno — fragment offset of the inode table within each CG.
    pub iblkno: u32,
    /// fs_volname — UTF-8 volume label (typically empty for PS5 .ffpkg).
    pub volume_name: String,
}

impl Superblock {
    /// Parse the superblock from a stream positioned anywhere — we
    /// seek to UFS2_SUPERBLOCK_OFFSET internally.
    pub fn read<R: Read + Seek>(r: &mut R) -> Result<Self, Ufs2Error> {
        r.seek(SeekFrom::Start(UFS2_SUPERBLOCK_OFFSET))?;
        let mut buf = vec![0u8; 1376];
        r.read_exact(&mut buf)?;
        // Magic lives at fs_magic, offset 1372 in struct fs (last
        // 4 bytes of a 1376-byte superblock). FreeBSD layout.
        let magic = read_u32(&buf, 1372);
        if magic != UFS2_MAGIC {
            return Err(Ufs2Error::BadMagic {
                offset: UFS2_SUPERBLOCK_OFFSET + 1372,
                found: magic,
            });
        }
        // Field offsets per FreeBSD sys/ufs/ffs/fs.h struct fs.
        // These are stable across FreeBSD versions — UFS2 layout has
        // not changed since FreeBSD 5.x.
        let fragment_size = read_u32(&buf, 56); // fs_fsize
        let block_size = read_u32(&buf, 48); // fs_bsize
        let cg_count = read_u32(&buf, 16); // fs_ncg
        let iblkno = read_u32(&buf, 20); // fs_iblkno
        let fragments_per_cg = read_u32(&buf, 36); // fs_fpg
        let inodes_per_cg = read_u32(&buf, 184); // fs_ipg
        let size_fragments = read_u64(&buf, 1080); // fs_size (UFS2 64-bit field)
        let volume_name = read_cstr(&buf, 680, 32);

        // Sanity-check sizes a hostile .ffpkg might set to 0 or
        // wildly large values. Without these:
        //   - block_size or inodes_per_cg = 0 → divide-by-zero panic
        //     in inode_offset/read_file
        //   - fragment_size huge → multiplication overflow / unbounded
        //     allocation in read_file's resize loop
        // Ranges chosen from PS5 .ffpkg samples (4 KiB frag, 32 KiB
        // block) with 4× headroom for non-standard images.
        const MAX_BLOCK_SIZE: u32 = 256 * 1024;
        const MAX_FRAG_SIZE: u32 = 256 * 1024;
        if fragment_size == 0 || fragment_size > MAX_FRAG_SIZE || !fragment_size.is_power_of_two() {
            return Err(Ufs2Error::BadSuperblock {
                field: "fragment_size",
                value: fragment_size as u64,
            });
        }
        if block_size == 0
            || block_size > MAX_BLOCK_SIZE
            || !block_size.is_power_of_two()
            || block_size < fragment_size
        {
            return Err(Ufs2Error::BadSuperblock {
                field: "block_size",
                value: block_size as u64,
            });
        }
        if inodes_per_cg == 0 {
            return Err(Ufs2Error::BadSuperblock {
                field: "inodes_per_cg",
                value: inodes_per_cg as u64,
            });
        }
        if fragments_per_cg == 0 {
            return Err(Ufs2Error::BadSuperblock {
                field: "fragments_per_cg",
                value: fragments_per_cg as u64,
            });
        }
        if cg_count == 0 {
            return Err(Ufs2Error::BadSuperblock {
                field: "cg_count",
                value: cg_count as u64,
            });
        }

        Ok(Superblock {
            block_size,
            fragment_size,
            size_fragments,
            cg_count,
            inodes_per_cg,
            fragments_per_cg,
            iblkno,
            volume_name,
        })
    }

    /// Total size in bytes (fragments × fragment size). Saturating
    /// math because both operands are u64 and a hostile image could
    /// otherwise wrap silently.
    pub fn total_bytes(&self) -> u64 {
        self.size_fragments
            .saturating_mul(self.fragment_size as u64)
    }

    /// Convert an inode number into a byte offset within the image.
    /// Uses saturating math throughout — a corrupt inodes_per_cg can't
    /// produce an in-range result anyway, and saturation gives us a
    /// loud "EOF on read" instead of a wrap-to-near-zero seek that
    /// could hand back garbage data.
    fn inode_offset(&self, inode: u64) -> u64 {
        let inodes_per_cg = self.inodes_per_cg as u64;
        // inodes_per_cg validated non-zero in Superblock::read.
        let cg_idx = inode / inodes_per_cg;
        let inode_in_cg = inode % inodes_per_cg;
        let cg_start = cg_idx
            .saturating_mul(self.fragments_per_cg as u64)
            .saturating_mul(self.fragment_size as u64);
        let inode_table =
            cg_start.saturating_add((self.iblkno as u64).saturating_mul(self.fragment_size as u64));
        inode_table.saturating_add(inode_in_cg.saturating_mul(INODE_SIZE))
    }

    /// Convert a fragment number to a byte offset.
    fn frag_offset(&self, frag: u64) -> u64 {
        frag.saturating_mul(self.fragment_size as u64)
    }
}

/// One inode's metadata — just what we need to walk files/dirs.
/// Real `struct ufs2_dinode` is 256 bytes with a lot more (uid, gid,
/// timestamps with nanosec resolution, extended attrs).
#[derive(Debug, Clone)]
pub struct Inode {
    /// Inode number (caller-supplied, not stored on disk).
    pub number: u64,
    /// File mode (POSIX mode bits — type + permissions).
    pub mode: u16,
    /// File size in bytes.
    pub size: u64,
    /// Direct block pointers (12 entries).
    pub direct: [u64; NDADDR],
    /// Indirect block pointers (single, double, triple).
    pub indirect: [u64; NIADDR],
    /// Modification time (seconds since epoch).
    pub mtime: i64,
}

impl Inode {
    pub fn is_dir(&self) -> bool {
        self.mode & IFMT == IFDIR
    }
    pub fn is_file(&self) -> bool {
        self.mode & IFMT == IFREG
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & IFMT == IFLNK
    }
    /// File-type one-letter tag for UI ("d" / "f" / "l" / "?").
    pub fn type_tag(&self) -> &'static str {
        match self.mode & IFMT {
            IFDIR => "d",
            IFREG => "f",
            IFLNK => "l",
            _ => "?",
        }
    }
}

/// One directory entry (inode + name).
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub inode: u64,
    pub name: String,
    /// File type from the directory entry (not the inode). UFS stores
    /// this redundantly so we don't have to read every child inode
    /// just to render a listing.
    pub kind: String,
}

/// Image handle. Wraps a seekable reader and the parsed superblock.
pub struct Ufs2Image<R: Read + Seek> {
    reader: R,
    pub superblock: Superblock,
}

impl Ufs2Image<File> {
    /// Open a `.ffpkg` (or any UFS2 image) by path. Cheap — only
    /// reads the superblock.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Ufs2Error> {
        let mut f = File::open(path)?;
        let sb = Superblock::read(&mut f)?;
        Ok(Self {
            reader: f,
            superblock: sb,
        })
    }
}

impl<R: Read + Seek> Ufs2Image<R> {
    /// Read inode `n`. Cheap — single seek + 256-byte read.
    pub fn read_inode(&mut self, n: u64) -> Result<Inode, Ufs2Error> {
        // Bound the (attacker-controlled) inode number before computing an
        // offset. Directory-entry inode numbers come straight off a
        // potentially-hostile image; an out-of-range number would otherwise
        // map (via saturating offset math) to an in-file offset belonging to
        // unrelated data and be reinterpreted as an inode (type confusion).
        // Inode 0 is reserved/invalid in UFS. Max = cg_count * inodes_per_cg.
        let max_inode =
            (self.superblock.cg_count as u64).saturating_mul(self.superblock.inodes_per_cg as u64);
        if n == 0 || n >= max_inode {
            return Err(Ufs2Error::InodeOutOfRange {
                inode: n,
                max: max_inode,
            });
        }
        let off = self.superblock.inode_offset(n);
        self.reader.seek(SeekFrom::Start(off))?;
        let mut buf = [0u8; INODE_SIZE as usize];
        self.reader.read_exact(&mut buf)?;
        // Layout per FreeBSD sys/ufs/ufs/dinode.h struct ufs2_dinode:
        //   0-1   di_mode (u16)
        //   2-3   di_nlink (u16)
        //   4-7   di_uid (u32)
        //   8-11  di_gid (u32)
        //   12-15 di_blksize (u32)
        //   16-23 di_size (u64)
        //   24-31 di_blocks (u64)
        //   32-39 di_atime (i64)
        //   40-47 di_mtime (i64)
        //   48-55 di_ctime (i64)
        //   56-63 di_birthtime (i64)
        //   ...
        //   112-207 di_db[12] (u64 each) = direct blocks
        //   208-231 di_ib[3] (u64 each) = indirect blocks
        let mode = read_u16(&buf, 0);
        let size = read_u64(&buf, 16);
        let mtime = read_i64(&buf, 40);
        let mut direct = [0u64; NDADDR];
        for (i, slot) in direct.iter_mut().enumerate() {
            *slot = read_u64(&buf, 112 + i * 8);
        }
        let mut indirect = [0u64; NIADDR];
        for (i, slot) in indirect.iter_mut().enumerate() {
            *slot = read_u64(&buf, 208 + i * 8);
        }
        Ok(Inode {
            number: n,
            mode,
            size,
            direct,
            indirect,
            mtime,
        })
    }

    /// List entries in a directory inode.
    pub fn list_dir(&mut self, dir: &Inode) -> Result<Vec<DirEntry>, Ufs2Error> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let bytes = self.read_file(dir, 1024 * 1024)?; // 1 MiB cap on dir size — huge by UFS standards
        let mut out: Vec<DirEntry> = Vec::new();
        let mut off: usize = 0;
        while off + 8 <= bytes.len() {
            let inode = read_u32(&bytes, off) as u64;
            let rec_len = read_u16(&bytes, off + 4);
            let kind_byte = bytes[off + 6];
            let name_len = bytes[off + 7] as usize;
            if rec_len == 0 || (rec_len as usize) < 8 || off + rec_len as usize > bytes.len() {
                return Err(Ufs2Error::BadDirEntry {
                    offset: off as u64,
                    rec_len,
                });
            }
            if inode != 0 && name_len > 0 && off + 8 + name_len <= bytes.len() {
                let name = String::from_utf8_lossy(&bytes[off + 8..off + 8 + name_len])
                    .trim_end_matches('\0')
                    .to_string();
                let kind = dirent_kind(kind_byte);
                if name != "." && name != ".." {
                    out.push(DirEntry { inode, name, kind });
                }
            }
            off += rec_len as usize;
        }
        Ok(out)
    }

    /// Read a file's bytes. `cap` bounds the response so a corrupt
    /// inode reporting a huge size can't OOM us.
    ///
    /// `cap` is also used as the OUTPUT-byte budget passed into the
    /// indirect-block walker, so a hostile image full of "hole"
    /// pointers (which `resize()` honors) can't grow `out` past the
    /// declared size. Without that, an inode claiming `size = 1 KiB`
    /// but with millions of indirect pointers would OOM despite
    /// passing the initial size check.
    pub fn read_file(&mut self, inode: &Inode, cap: u64) -> Result<Vec<u8>, Ufs2Error> {
        if inode.size > cap {
            return Err(Ufs2Error::FileTooLarge {
                size: inode.size,
                cap,
            });
        }
        // 32-bit-host safety: `Vec::with_capacity(inode.size as usize)`
        // silently truncates on Windows x86. usize::try_from converts
        // the runtime-checked failure to our typed error.
        let size_usize = usize::try_from(inode.size).map_err(|_| Ufs2Error::FileTooLarge {
            size: inode.size,
            cap: usize::MAX as u64,
        })?;
        // Do NOT pre-allocate the full declared size. `inode.size` is read
        // straight off (untrusted) disk, so a few-KB image can claim a
        // multi-GiB inode and force that allocation here before a single
        // block is read (a decompression/parse bomb). The read loop below
        // grows `out` only as real blocks/holes are consumed — bounded by
        // `remaining` (≤ cap) — so hint a small ceiling and let it grow.
        const PREALLOC_CEILING: usize = 8 * 1024 * 1024;
        let mut out = Vec::with_capacity(size_usize.min(PREALLOC_CEILING));
        let mut remaining = inode.size;
        let bsize = self.superblock.block_size as u64;

        // Direct blocks first.
        for i in 0..NDADDR {
            if remaining == 0 {
                break;
            }
            let frag = inode.direct[i];
            let take = remaining.min(bsize);
            self.read_block_or_hole(inode.number, frag, take, &mut out)?;
            remaining -= take;
        }
        // Indirect block chain. Each indirect block holds
        // (block_size / 8) u64 pointers.
        let ptrs_per_block = bsize / 8;
        // Visited-set for cycle detection across all indirect levels.
        // A crafted image can have a triple-indirect block point
        // back to itself (or any ancestor); without this set the
        // walker recurses unboundedly until stack overflow. Capacity
        // hint keeps the rehash count down on large files.
        let mut visited: HashSet<u64> = HashSet::with_capacity(64);
        if remaining > 0 && inode.indirect[0] != 0 {
            self.read_indirect(
                inode.number,
                inode.indirect[0],
                1,
                ptrs_per_block,
                &mut remaining,
                &mut out,
                &mut visited,
            )?;
        }
        if remaining > 0 && inode.indirect[1] != 0 {
            self.read_indirect(
                inode.number,
                inode.indirect[1],
                2,
                ptrs_per_block,
                &mut remaining,
                &mut out,
                &mut visited,
            )?;
        }
        if remaining > 0 && inode.indirect[2] != 0 {
            self.read_indirect(
                inode.number,
                inode.indirect[2],
                3,
                ptrs_per_block,
                &mut remaining,
                &mut out,
                &mut visited,
            )?;
        }
        Ok(out)
    }

    // 8 args: this is a tight recursive block-walker — `inode_num` is
    // carried purely so a corrupt pointer can be reported as
    // BlockOutOfRange, and the rest (depth, remaining, out, visited)
    // are all genuine per-recursion walk state. Bundling them into a
    // struct would obscure more than it clarifies.
    #[allow(clippy::too_many_arguments)]
    fn read_indirect(
        &mut self,
        inode_num: u64,
        block: u64,
        depth: u8,
        ptrs_per_block: u64,
        remaining: &mut u64,
        out: &mut Vec<u8>,
        visited: &mut HashSet<u64>,
    ) -> Result<(), Ufs2Error> {
        if *remaining == 0 || block == 0 {
            return Ok(());
        }
        // An indirect block pointer past the end of the image is
        // corruption — surface it as BlockOutOfRange rather than a
        // generic EOF Io error from the read_exact below.
        if block >= self.superblock.size_fragments {
            return Err(Ufs2Error::BlockOutOfRange {
                inode: inode_num,
                block,
                total_blocks: self.superblock.size_fragments,
            });
        }
        if !visited.insert(block) {
            // Cycle — same indirect block visited twice. Stop the
            // walk; the file won't be fully reconstructed but the
            // engine won't blow the stack either.
            return Ok(());
        }
        let bsize = self.superblock.block_size as u64;
        let off = self.superblock.frag_offset(block);
        self.reader.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; bsize as usize];
        self.reader.read_exact(&mut buf)?;
        for i in 0..ptrs_per_block as usize {
            if *remaining == 0 {
                break;
            }
            let ptr = read_u64(&buf, i * 8);
            if depth == 1 {
                let take = (*remaining).min(bsize);
                self.read_block_or_hole(inode_num, ptr, take, out)?;
                *remaining -= take;
            } else {
                self.read_indirect(
                    inode_num,
                    ptr,
                    depth - 1,
                    ptrs_per_block,
                    remaining,
                    out,
                    visited,
                )?;
            }
        }
        Ok(())
    }

    fn read_block_or_hole(
        &mut self,
        inode_num: u64,
        block: u64,
        len: u64,
        out: &mut Vec<u8>,
    ) -> Result<(), Ufs2Error> {
        if block == 0 {
            // Sparse hole — UFS represents holes as null pointers.
            out.resize(out.len() + len as usize, 0u8);
            return Ok(());
        }
        // A corrupt inode can point at a fragment past the end of the
        // image. Catch it with a precise BlockOutOfRange instead of
        // letting frag_offset + seek + read_exact surface it as a
        // generic "unexpected EOF" Io error — the descriptive variant
        // tells the user the .ffpkg is structurally corrupt, not
        // merely truncated.
        if block >= self.superblock.size_fragments {
            return Err(Ufs2Error::BlockOutOfRange {
                inode: inode_num,
                block,
                total_blocks: self.superblock.size_fragments,
            });
        }
        let off = self.superblock.frag_offset(block);
        self.reader.seek(SeekFrom::Start(off))?;
        let start = out.len();
        out.resize(start + len as usize, 0);
        self.reader.read_exact(&mut out[start..])?;
        Ok(())
    }

    /// Resolve a path of components ("sce_sys/param.sfo") starting
    /// from the root inode. Returns the terminal inode + its
    /// directory-entry kind; useful when the caller wants to confirm
    /// the path is actually a regular file before calling read_file.
    ///
    /// Component count cap: a hostile path with thousands of components
    /// (each one re-listing the same directory) could otherwise pin
    /// the engine sidecar single-thread for minutes per inspect call.
    /// 256 covers any legitimate game asset path with headroom.
    pub fn resolve_path(&mut self, components: &[&str]) -> Result<Inode, Ufs2Error> {
        const MAX_PATH_COMPONENTS: usize = 256;
        if components.len() > MAX_PATH_COMPONENTS {
            return Err(Ufs2Error::NotFound {
                component: format!(
                    "path has {} components; max {}",
                    components.len(),
                    MAX_PATH_COMPONENTS
                ),
            });
        }
        let mut current = self.read_inode(ROOT_INODE)?;
        for comp in components {
            if !current.is_dir() {
                return Err(Ufs2Error::NotFound {
                    component: (*comp).to_string(),
                });
            }
            let entries = self.list_dir(&current)?;
            let entry =
                entries
                    .iter()
                    .find(|e| e.name == *comp)
                    .ok_or_else(|| Ufs2Error::NotFound {
                        component: (*comp).to_string(),
                    })?;
            current = self.read_inode(entry.inode)?;
        }
        Ok(current)
    }
}

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}
fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}
fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}
fn read_i64(buf: &[u8], off: usize) -> i64 {
    read_u64(buf, off) as i64
}
fn read_cstr(buf: &[u8], off: usize, max: usize) -> String {
    let end = (off..off + max).find(|i| buf[*i] == 0).unwrap_or(off + max);
    String::from_utf8_lossy(&buf[off..end]).into_owned()
}

/// Map UFS directory-entry type byte to a stable string for the
/// renderer. Values per FreeBSD sys/ufs/ufs/dir.h DT_*.
fn dirent_kind(b: u8) -> String {
    match b {
        4 => "dir",
        8 => "file",
        10 => "link",
        _ => "other",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic minimal UFS2 image for unit testing the parser.
    /// Real .ffpkg files are too big for a unit test; we hand-craft
    /// the bytes for a 1-block "empty" filesystem just enough that
    /// Superblock::read returns Ok.
    #[test]
    fn rejects_bad_magic() {
        use std::io::Cursor;
        let buf = vec![0u8; UFS2_SUPERBLOCK_OFFSET as usize + 1376];
        let mut c = Cursor::new(buf);
        let err = Superblock::read(&mut c).unwrap_err();
        assert!(matches!(err, Ufs2Error::BadMagic { .. }));
    }

    #[test]
    fn dirent_kind_maps() {
        assert_eq!(dirent_kind(4), "dir");
        assert_eq!(dirent_kind(8), "file");
        assert_eq!(dirent_kind(10), "link");
        assert_eq!(dirent_kind(99), "other");
    }

    #[test]
    fn inode_type_tags() {
        let i = Inode {
            number: 2,
            mode: IFDIR,
            size: 0,
            direct: [0; NDADDR],
            indirect: [0; NIADDR],
            mtime: 0,
        };
        assert!(i.is_dir());
        assert_eq!(i.type_tag(), "d");
    }
}
