//! The inner image (`pfs_image.dat`): data-first, every file stored raw.
//!
//! v1 stores everything, so the on-disk image *is* the logical mount: files sit at their
//! afid-order offsets, the block-info table sits in the padding block just after the data
//! (where the sample carries it too), and the metadata region starts at the 256 KiB-aligned
//! `meta_base` the finalized-image header records at `0x50` (in 4096-byte sectors). A package that compressed
//! anything would instead have to map the two spaces through `naps_pkg_layout.dat`.
//!
//! The metadata region itself is a small PFS: a superblock, a table of 0xA8-byte inodes
//! that carry one logical offset each, the super-root's four entries, both flat-path
//! tables, the afid table, then one dirent block per directory.

use crate::crypto::hmac_sha256;
use crate::flt;
use crate::keys;
use crate::plan::{self, Plan};
use crate::{format_err, le16, le32, le64, Result, BLOCK};

/// Bytes of one inner inode.
pub const INODE_LEN: usize = 0xA8;
/// The block-info table's entry count, as measured on the sample (31 template entries and
/// one derived value).
pub const BLOCK_INFO_ENTRIES: usize = 32;
const BLOCK_INFO_TEMPLATE: u32 = 0x00FC_FF27;
const BLOCK_INFO_VERSION: u32 = 0x0040_0003;
const BLOCK_INFO_BASE: u32 = 0x0027_373C;

/// Where one afid's bytes land, on disk and in the mount.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub on_disk_offset: u64,
    pub size: u64,
    pub logical_offset: u64,
}

pub struct InnerImage {
    /// The `pfs_image.dat` bytes, which are also the logical mount.
    pub image: Vec<u8>,
    pub meta_base: u64,
    pub ndblock: u64,
    pub block_info_offset: u64,
    /// Indexed like `plan.files`.
    pub placements: Vec<Placement>,
    /// Logical offsets in afid order — the naps `fidx` entries.
    pub afid_offsets: Vec<u64>,
}

/// The 96-byte `sce_sys/keystone` for a passcode.
///
/// Verified against `webbrowser.pkg`'s inner image, whose keystone is stored raw at
/// offset 0: its bytes are exactly this construction with the default passcode.
pub fn keystone(passcode: &str) -> [u8; 96] {
    let mut out = [0u8; 96];
    out[..8].copy_from_slice(b"keystone");
    out[8..10].copy_from_slice(&3u16.to_le_bytes());
    out[10] = 0x01;
    let fingerprint = hmac_sha256(&keys::KEYSTONE_HMAC_1, &[passcode.as_bytes()]);
    out[0x20..0x40].copy_from_slice(&fingerprint);
    let seal = hmac_sha256(&keys::KEYSTONE_HMAC_2, &[&out[..0x40]]);
    out[0x40..0x60].copy_from_slice(&seal);
    out
}

/// The 0x100-byte block-info table that sits between the data and the metadata.
///
/// The last entry encodes the uroot payload size: `swap24(0x27373C - 4*Σ mod 0x40000)`,
/// which reproduces the sample's stored value (`0x00646725`) exactly.
pub fn block_info_table(plan: &Plan) -> Vec<u8> {
    let uroot_sum: u64 = plan
        .files
        .iter()
        .filter(|f| !f.sce_sys)
        .map(|f| f.size)
        .sum();
    let sub = ((4 * uroot_sum) & 0xF_FFFF) as u32;
    let be = (BLOCK_INFO_BASE.wrapping_sub(sub)) & 0xFF_FFFF;
    let value = ((be & 0xFF) << 16) | (be & 0xFF00) | ((be >> 16) & 0xFF);

    let mut table = Vec::with_capacity(BLOCK_INFO_ENTRIES * 8);
    for i in 0..BLOCK_INFO_ENTRIES {
        let entry = if i + 1 == BLOCK_INFO_ENTRIES {
            value
        } else {
            BLOCK_INFO_TEMPLATE
        };
        table.extend_from_slice(&entry.to_le_bytes());
        table.extend_from_slice(&BLOCK_INFO_VERSION.to_le_bytes());
    }
    table
}

/// The fields one 0xA8-byte inner inode carries.
struct InodeRecord {
    mode: u16,
    nlink: u16,
    flags: u32,
    size: u64,
    logical_offset: u64,
    /// afid for files, -1 for directories and tables.
    db1: i32,
    /// Parent inode, -1 for the super-root's own children.
    db2: i32,
    /// Byte offset of this node's dirent in the parent, -1 when it has none.
    db3: i32,
}

fn write_inode(table: &mut [u8], index: usize, rec: &InodeRecord, time: (i64, u32)) {
    let o = index * INODE_LEN;
    let ino = &mut table[o..o + INODE_LEN];
    ino[0..2].copy_from_slice(&rec.mode.to_le_bytes());
    ino[2..4].copy_from_slice(&rec.nlink.to_le_bytes());
    ino[4..8].copy_from_slice(&rec.flags.to_le_bytes());
    ino[8..16].copy_from_slice(&rec.size.to_le_bytes());
    ino[0x10..0x18].copy_from_slice(&rec.size.to_le_bytes());
    for t in 0..4 {
        ino[0x18 + t * 8..0x20 + t * 8].copy_from_slice(&time.0.to_le_bytes());
    }
    for t in 0..4 {
        ino[0x38 + t * 4..0x3C + t * 4].copy_from_slice(&time.1.to_le_bytes());
    }
    ino[0x60..0x68].copy_from_slice(&rec.logical_offset.to_le_bytes());
    ino[0x68..0x6C].copy_from_slice(&rec.db1.to_le_bytes());
    ino[0x6C..0x70].copy_from_slice(&rec.db2.to_le_bytes());
    ino[0x70..0x74].copy_from_slice(&rec.db3.to_le_bytes());
}

/// Serialize dirents in on-disk order (shared with the outer writer).
pub(crate) fn dirents_bytes(dirents: &[(String, u32, i8)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, inode, kind) in dirents {
        let size = plan::dirent_size(name) as usize;
        out.extend_from_slice(&inode.to_le_bytes());
        out.extend_from_slice(&(*kind as i32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.resize(out.len() + (size - 16 - name.len()), 0);
    }
    out
}

/// Copy a structure's bytes into the region at its planned offset. The region is block-aligned
/// as a whole; a structure may run past a block boundary, which is what big titles do.
fn place(region: &mut [u8], span: (u64, u64), meta_base: u64, bytes: &[u8]) {
    let at = (span.0 - meta_base) as usize;
    region[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Build the metadata region as one byte image, placing every structure at the offset the
/// planner fixed. Structures are block-aligned at their start and may span blocks.
fn metadata_region(plan: &Plan, build_time: (i64, u32)) -> Result<Vec<u8>> {
    let inode_count = 4 + plan.dirs.len() + plan.files.len();
    let mut region = vec![0u8; plan.metadata.blocks as usize * BLOCK as usize];

    // Block 0: superblock.
    let mut sb = vec![0u8; BLOCK as usize];
    sb[0x00..0x08].copy_from_slice(&2i64.to_le_bytes());
    sb[0x08..0x10].copy_from_slice(&20_130_315i64.to_le_bytes());
    sb[0x1A] = 1;
    sb[0x1C..0x1E].copy_from_slice(&0x18u16.to_le_bytes());
    sb[0x20..0x24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    sb[0x28..0x30].copy_from_slice(&1i64.to_le_bytes());
    sb[0x30..0x38].copy_from_slice(&(inode_count as i64).to_le_bytes());
    sb[0x38..0x40].copy_from_slice(&(plan.ndblock as i64).to_le_bytes());
    sb[0x40..0x48].copy_from_slice(&1i64.to_le_bytes());
    for (at, value) in [
        (0x50usize, BLOCK as u32),
        (0x54, 0x10),
        (0x58, BLOCK as u32),
        (0x60, BLOCK as u32),
    ] {
        sb[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for t in 0..4 {
        sb[0x68 + t * 8..0x70 + t * 8].copy_from_slice(&build_time.0.to_le_bytes());
    }
    for t in 0..4 {
        sb[0x88 + t * 4..0x8C + t * 4].copy_from_slice(&build_time.1.to_le_bytes());
    }
    sb[0xB0..0xB8].copy_from_slice(&1i64.to_le_bytes());
    sb[0xD8..0xE0].copy_from_slice(&0x89i64.to_le_bytes());
    sb[0x368] = 1;
    region[..BLOCK as usize].copy_from_slice(&sb);

    // Block 1: the inode table. Node order: super-root, the three tables, directories
    // pre-order (uroot first), then files in inode order.
    let flt_inode = flt::write(&plan.flt_inode);
    let flt_apr = flt::write(&plan.flt_apr);
    let mut afid_bytes = Vec::with_capacity(plan.afid_to_ino.len() * 4);
    for v in &plan.afid_to_ino {
        afid_bytes.extend_from_slice(&v.to_le_bytes());
    }
    let (table_at, table_bytes) = plan.metadata.inode_table;
    let table_off = (table_at - plan.meta_base) as usize;
    let table = &mut region[table_off..table_off + table_bytes as usize];
    write_inode(
        table,
        plan::SUPER_ROOT_INODE as usize,
        &InodeRecord {
            mode: plan::MODE_DIR_UROOT,
            nlink: 1,
            flags: plan::FLAGS_TABLE,
            size: plan.metadata.super_root.1,
            logical_offset: plan.metadata.super_root.0,
            db1: -1,
            db2: -1,
            db3: -1,
        },
        build_time,
    );
    for (inode, size, offset) in [
        (
            plan::INODE_FLT_INODE,
            plan.metadata.flt.1,
            plan.metadata.flt.0,
        ),
        (
            plan::APR_FLT_INODE,
            plan.metadata.flt_apr.1,
            plan.metadata.flt_apr.0,
        ),
        (
            plan::AFID_TABLE_INODE,
            plan.metadata.afid.1,
            plan.metadata.afid.0,
        ),
    ] {
        write_inode(
            table,
            inode as usize,
            &InodeRecord {
                mode: plan::MODE_FILE,
                nlink: 1,
                flags: plan::FLAGS_TABLE,
                size,
                logical_offset: offset,
                db1: -1,
                db2: -1,
                db3: -1,
            },
            build_time,
        );
    }
    for (i, d) in plan.dirs.iter().enumerate() {
        let is_root = i == 0;
        write_inode(
            table,
            4 + i,
            &InodeRecord {
                mode: if is_root {
                    plan::MODE_DIR_UROOT
                } else {
                    plan::MODE_DIR
                },
                nlink: d.nlink,
                flags: if is_root {
                    plan::FLAGS_DATA
                } else {
                    plan::FLAGS_TABLE
                },
                size: plan.metadata.dirs[i].1,
                logical_offset: plan.metadata.dirs[i].0,
                db1: -1,
                db2: d.parent_inode,
                db3: d.dirent_offset,
            },
            build_time,
        );
    }
    for (i, f) in plan.files.iter().enumerate() {
        write_inode(
            table,
            4 + plan.dirs.len() + i,
            &InodeRecord {
                mode: f.mode(),
                nlink: 1,
                flags: f.inode_flags(),
                size: f.size,
                logical_offset: f.logical_offset,
                db1: f.afid as i32,
                db2: f.parent_inode as i32,
                db3: f.dirent_offset,
            },
            build_time,
        );
    }
    place(
        &mut region,
        plan.metadata.super_root,
        plan.meta_base,
        &dirents_bytes(&plan::super_root_dirents()),
    );
    place(&mut region, plan.metadata.flt, plan.meta_base, &flt_inode);
    place(&mut region, plan.metadata.flt_apr, plan.meta_base, &flt_apr);
    place(&mut region, plan.metadata.afid, plan.meta_base, &afid_bytes);
    for (i, d) in plan.dirs.iter().enumerate() {
        place(
            &mut region,
            plan.metadata.dirs[i],
            plan.meta_base,
            &dirents_bytes(&d.dirents),
        );
    }
    Ok(region)
}

/// Assemble the inner image: `[payloads][block-info table][metadata]` on disk and the
/// matching logical mount.
pub fn write(
    plan: &Plan,
    passcode: &str,
    read: &mut dyn FnMut(&str) -> Result<Vec<u8>>,
    build_time: (i64, u32),
) -> Result<InnerImage> {
    // Payloads by inode index (only the keystone is generated).
    let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(plan.files.len());
    for f in &plan.files {
        let data = if f.generated && f.path == plan::KEYSTONE {
            keystone(passcode).to_vec()
        } else {
            read(&f.path)?
        };
        if data.len() as u64 != f.size {
            return format_err(format!(
                "{} is {} bytes on disk but the plan fixed {}",
                f.path,
                data.len(),
                f.size
            ));
        }
        payloads.push(data);
    }

    // One buffer: files at their logical offsets, the table in the padding block after
    // them, the metadata at `meta_base`.
    let mut image = vec![0u8; (plan.ndblock * BLOCK) as usize];
    let mut placements = vec![
        Placement {
            on_disk_offset: 0,
            size: 0,
            logical_offset: 0,
        };
        plan.files.len()
    ];
    let mut afid_offsets = vec![0u64; plan.afid_order.len()];
    for (afid, &fi) in plan.afid_order.iter().enumerate() {
        let f = &plan.files[fi];
        let at = f.logical_offset as usize;
        let end = at + payloads[fi].len();
        if end > image.len() {
            return format_err(format!("{} runs past the mount", f.path));
        }
        image[at..end].copy_from_slice(&payloads[fi]);
        placements[fi] = Placement {
            on_disk_offset: f.logical_offset,
            size: payloads[fi].len() as u64,
            logical_offset: f.logical_offset,
        };
        afid_offsets[afid] = f.logical_offset;
    }

    // The block-info table, in the first padding block after the data region.
    let block_info_offset = plan.data_end.div_ceil(BLOCK) * BLOCK;
    let table = block_info_table(plan);
    let at = block_info_offset as usize;
    if at + table.len() > plan.meta_base as usize {
        return format_err("no room for the block-info table before the metadata");
    }
    image[at..at + table.len()].copy_from_slice(&table);

    // The metadata region.
    let region = metadata_region(plan, build_time)?;
    if region.len() as u64 != plan.metadata_blocks * BLOCK {
        return format_err(format!(
            "metadata region is {} bytes but the plan fixed {} blocks",
            region.len(),
            plan.metadata_blocks
        ));
    }
    let meta_at = plan.meta_base as usize;
    if meta_at + region.len() > image.len() {
        return format_err("the metadata region runs past the mount");
    }
    image[meta_at..meta_at + region.len()].copy_from_slice(&region);

    Ok(InnerImage {
        image,
        meta_base: plan.meta_base,
        ndblock: plan.ndblock,
        block_info_offset,
        placements,
        afid_offsets,
    })
}

/// A ranged read of a source file, which is how a block is filled without holding one.
pub type RangeRead<'r> = &'r mut dyn FnMut(&str, u64, usize) -> Result<Vec<u8>>;

/// One file's span of the inner image, in placement order.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub start: u64,
    pub end: u64,
    /// Index into `plan.files`.
    pub file: usize,
}

/// The inner image as blocks, read on demand.
///
/// A block is filled from the file bytes that land in it (a block can hold the tail of one
/// file and the head of the next), the block-info table, or the metadata region — the two
/// latter are held whole, because both are bounded by the file count rather than the
/// package size. Nothing here is proportional to the image, so a 100 GB game streams.
pub struct BlockSource<'a> {
    plan: &'a Plan,
    spans: Vec<Span>,
    table: Vec<u8>,
    table_block: u64,
    meta: Vec<Vec<u8>>,
    meta_block: u64,
    keystone: Vec<u8>,
    buf: Vec<u8>,
}

impl<'a> BlockSource<'a> {
    pub fn new(plan: &'a Plan, passcode: &str, build_time: (i64, u32)) -> Result<Self> {
        let mut spans: Vec<Span> = plan
            .afid_order
            .iter()
            .map(|&fi| {
                let f = &plan.files[fi];
                Span {
                    start: f.logical_offset,
                    end: f.logical_offset + f.size,
                    file: fi,
                }
            })
            .collect();
        spans.sort_by_key(|s| s.start);
        let meta = metadata_region(plan, build_time)?;
        if meta.len() as u64 != plan.metadata_blocks * BLOCK {
            return format_err(format!(
                "metadata region is {} bytes but the plan fixed {} blocks",
                meta.len(),
                plan.metadata_blocks
            ));
        }
        Ok(Self {
            plan,
            spans,
            table: block_info_table(plan),
            table_block: plan.data_end.div_ceil(BLOCK),
            meta: meta.chunks(BLOCK as usize).map(<[u8]>::to_vec).collect(),
            meta_block: plan.meta_base / BLOCK,
            keystone: keystone(passcode).to_vec(),
            buf: vec![0u8; BLOCK as usize],
        })
    }

    pub fn ndblock(&self) -> u64 {
        self.plan.ndblock
    }

    /// The files that contribute bytes to block `index`, in placement order — at most two
    /// (a block can hold the tail of one file and the head of the next).
    pub fn block_spans(&self, index: u64) -> &[Span] {
        let lo = index * BLOCK;
        let hi = lo + BLOCK;
        let from = self.spans.partition_point(|s| s.end <= lo);
        let to = self.spans.partition_point(|s| s.start < hi);
        &self.spans[from..to.max(from)]
    }

    /// The metadata region's bytes — bounded by the file count, not the image size.
    pub fn metadata_region(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.meta.len() * BLOCK as usize);
        for block in &self.meta {
            out.extend_from_slice(block);
        }
        out
    }

    /// Block `index` of the image. The buffer is reused between calls, and `read` fetches
    /// a byte range of a source file.
    pub fn block(&mut self, index: u64, read: RangeRead<'_>) -> Result<&[u8]> {
        if index >= self.ndblock() {
            return format_err(format!(
                "block {index} is past the image's {} blocks",
                self.ndblock()
            ));
        }
        self.buf.fill(0);
        if index >= self.meta_block {
            let at = (index - self.meta_block) as usize;
            return match self.meta.get(at) {
                Some(block) => Ok(block),
                None => format_err(format!("the metadata region has no block {index}")),
            };
        }
        if index == self.table_block {
            let len = self.table.len().min(BLOCK as usize);
            self.buf[..len].copy_from_slice(&self.table[..len]);
            return Ok(&self.buf);
        }
        let lo = index * BLOCK;
        let hi = ((index + 1) * BLOCK).min(self.plan.data_end);
        if lo >= hi {
            return Ok(&self.buf); // padding between the data and the metadata
        }
        let mut at = self.spans.partition_point(|s| s.end <= lo);
        while at < self.spans.len() {
            let span = &self.spans[at];
            if span.start >= hi {
                break;
            }
            let from = span.start.max(lo);
            let to = span.end.min(hi);
            at += 1;
            if to <= from {
                continue;
            }
            let want = (to - from) as usize;
            let offset = from - span.start;
            let f = &self.plan.files[span.file];
            let bytes = if f.generated && f.path == plan::KEYSTONE {
                let start = offset as usize;
                self.keystone
                    .get(start..start + want)
                    .ok_or_else(|| {
                        crate::Error::Format(format!("{} is shorter than the plan fixed", f.path))
                    })?
                    .to_vec()
            } else {
                let bytes = read(&f.path, offset, want)?;
                if bytes.len() != want {
                    return format_err(format!(
                        "{} gave {} bytes at {offset} where the plan fixed {want}",
                        f.path,
                        bytes.len()
                    ));
                }
                bytes
            };
            let at_in_block = (from - lo) as usize;
            self.buf[at_in_block..at_in_block + want].copy_from_slice(&bytes);
        }
        Ok(&self.buf)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InnerFile {
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug)]
pub struct InnerMount {
    pub files: Vec<InnerFile>,
    pub inode_count: usize,
    /// Every file's path hashes into the inode flat-path table.
    pub flt_ok: bool,
}

/// Walk a reconstructed mount the way the console does: superblock, inodes, dirents.
pub fn read(mount: &[u8], meta_base: u64) -> Result<InnerMount> {
    let base = meta_base as usize;
    let sb = mount
        .get(base..base + 0x400)
        .ok_or_else(|| crate::Error::Format("mount is shorter than its metadata base".into()))?;
    if le64(sb, 0) != 2 || le64(sb, 8) != 20_130_315 {
        return format_err("inner superblock version/magic mismatch");
    }
    let block_size = le32(sb, 0x20) as usize;
    let inode_count = le64(sb, 0x30) as usize;
    if block_size != BLOCK as usize || inode_count == 0 || inode_count > 4_000_000 {
        return format_err(format!(
            "implausible inner superblock (block size {block_size:#x}, {inode_count} inodes)"
        ));
    }
    // The table is flat from one block after the superblock and spans as many blocks as the
    // inode count needs — a real title's does.
    let table = mount
        .get(base + block_size..base + block_size + inode_count * INODE_LEN)
        .ok_or_else(|| crate::Error::Format("mount has no inode table".into()))?;

    let inode = |i: usize| -> Option<(u16, u64, u64, i32)> {
        let o = i * INODE_LEN;
        let raw = table.get(o..o + INODE_LEN)?;
        Some((
            le16(raw, 0),
            le64(raw, 8),
            le64(raw, 0x60),
            crate::i32le(raw, 0x6C),
        ))
    };

    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    walk(
        mount,
        &inode,
        plan::FIRST_DIR_INODE as usize,
        "uroot",
        &mut files,
        &mut seen,
    )?;

    // The inode flat-path table must hash every file's path to its inode.
    let mut flt_ok = true;
    if let Some(flt_bytes) = read_table(mount, &inode, plan::INODE_FLT_INODE as usize) {
        let count = flt_bytes
            .get(0x2C..0x30)
            .map(|b| le32(b, 0) as usize)
            .unwrap_or(0);
        for f in &files {
            let want = flt::hash_path(&f.path);
            let found = (0..count).any(|i| {
                let at = 0x40 + i * 16;
                flt_bytes
                    .get(at..at + 16)
                    .is_some_and(|rec| u64::from_le_bytes(rec[..8].try_into().unwrap()) == want)
            });
            flt_ok &= found;
        }
    } else {
        flt_ok = false;
    }

    Ok(InnerMount {
        files,
        inode_count,
        flt_ok,
    })
}

/// The bytes of a metadata table (a file whose data is a metadata block).
fn read_table(
    mount: &[u8],
    inode: &dyn Fn(usize) -> Option<(u16, u64, u64, i32)>,
    i: usize,
) -> Option<Vec<u8>> {
    let (_, size, offset, _) = inode(i)?;
    let at = offset as usize;
    mount.get(at..at + size as usize).map(|s| s.to_vec())
}

#[allow(clippy::too_many_arguments)]
fn walk(
    mount: &[u8],
    inode: &dyn Fn(usize) -> Option<(u16, u64, u64, i32)>,
    dir_inode: usize,
    prefix: &str,
    files: &mut Vec<InnerFile>,
    seen: &mut std::collections::HashSet<usize>,
) -> Result<()> {
    if !seen.insert(dir_inode) {
        return format_err(format!("directory loop at inode {dir_inode}"));
    }
    let Some((mode, size, offset, _)) = inode(dir_inode) else {
        return format_err(format!("inode {dir_inode} is out of range"));
    };
    if mode & 0x4000 == 0 {
        return format_err(format!("inode {dir_inode} is not a directory"));
    }
    let at = offset as usize;
    let block = mount
        .get(at..(at + size as usize).min(mount.len()))
        .ok_or_else(|| crate::Error::Format(format!("directory {dir_inode} is out of range")))?;

    let mut entries = Vec::new();
    let mut o = 0usize;
    while o + 16 <= block.len() {
        let ino = le32(block, o) as usize;
        let kind = crate::i32le(block, o + 4);
        let name_len = le32(block, o + 8) as usize;
        let ent_size = le32(block, o + 12) as usize;
        if ent_size == 0 || o + 16 + name_len > block.len() {
            break;
        }
        let name = String::from_utf8_lossy(&block[o + 16..o + 16 + name_len]).into_owned();
        entries.push((ino, kind, name));
        o += ent_size;
    }

    for (ino, kind, name) in entries {
        if name == "." || name == ".." {
            continue;
        }
        let child = if prefix == "uroot" {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if kind == plan::DIRENT_DIR as i32 {
            walk(mount, inode, ino, &child, files, seen)?;
        } else if kind == plan::DIRENT_FILE as i32 {
            let Some((_, size, offset, _)) = inode(ino) else {
                return format_err(format!("inode {ino} is out of range"));
            };
            files.push(InnerFile {
                path: child,
                offset,
                size,
            });
        } else {
            return format_err(format!("unexpected dirent kind {kind} for {child}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceFile;

    fn plan_for(paths: &[(&str, u64)]) -> Plan {
        let files: Vec<SourceFile> = paths
            .iter()
            .map(|(p, s)| SourceFile {
                path: (*p).to_string(),
                size: *s,
            })
            .collect();
        plan::build(&files).unwrap()
    }

    /// The block source is the streaming replacement for `write`: for the same plan it
    /// must emit the same image, block for block.
    #[test]
    fn the_block_source_reproduces_the_in_memory_image() {
        let plan = plan_for(&[
            ("eboot.bin", 4096),
            ("data/one.bin", 200_000),
            ("data/two.bin", 70_000),
            ("sce_sys/param.json", 197),
            ("sce_sys/keystone", 96),
        ]);
        let time = (1_700_000_000u64 as i64, 0u32);
        let payloads: std::collections::HashMap<&str, Vec<u8>> = plan
            .files
            .iter()
            .map(|f| {
                let data: Vec<u8> = (0..f.size).map(|i| (i % 251) as u8).collect();
                (f.path.as_str(), data)
            })
            .collect();
        let mut read_all =
            |path: &str| -> Result<Vec<u8>> { Ok(payloads.get(path).cloned().unwrap_or_default()) };
        let whole = write(&plan, crate::crypto::DEFAULT_PASSCODE, &mut read_all, time).unwrap();

        let mut source = BlockSource::new(&plan, crate::crypto::DEFAULT_PASSCODE, time).unwrap();
        let mut image = Vec::with_capacity((plan.ndblock * BLOCK) as usize);
        for index in 0..plan.ndblock {
            let block = source
                .block(index, &mut |path, offset, len| {
                    let data = payloads.get(path).cloned().unwrap_or_default();
                    let at = (offset as usize).min(data.len());
                    let end = (at + len).min(data.len());
                    Ok(data[at..end].to_vec())
                })
                .unwrap()
                .to_vec();
            image.extend_from_slice(&block);
        }
        assert_eq!(image.len(), whole.image.len());
        assert!(image == whole.image, "the block source and write disagree");
    }

    /// `webbrowser.pkg` stores its keystone raw at the inner image's offset 0, so those
    /// 96 bytes are a measured vector for this construction.
    #[test]
    fn keystone_matches_the_sample() {
        let expected = concat!(
            "6b657973746f6e65030001000000000000000000000000000000000000000000",
            "9320f0d8df5b1d9539508da1ce060d9d37e0f4afa9957689f65da2c23575ac7c",
            "dd7e2650989fcc8aab24aebbb979addc3d20bca6a7a8abb3be4b160d0209640c",
        );
        let hex: String = keystone(crate::crypto::DEFAULT_PASSCODE)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, expected);
    }

    /// The block-info table's variable entry reproduces the sample's stored value for the
    /// sample's uroot payload sum (29686).
    #[test]
    fn block_info_entry_matches_the_sample() {
        let plan = plan_for(&[
            ("sce_sys/keystone", 96),
            ("sce_sys/about/right.sprx", 12752),
            ("00.dat", 10),
            ("m.dat", 14942),
            ("z.dat", 14734),
        ]);
        assert_eq!(
            plan.files
                .iter()
                .filter(|f| !f.sce_sys)
                .map(|f| f.size)
                .sum::<u64>(),
            29686
        );
        let table = block_info_table(&plan);
        assert_eq!(table.len(), BLOCK_INFO_ENTRIES * 8);
        let last = u32::from_le_bytes(table[30 * 8..30 * 8 + 4].try_into().unwrap());
        assert_eq!(last, 0x00FC_FF27);
        let derived = u32::from_le_bytes(table[31 * 8..31 * 8 + 4].try_into().unwrap());
        assert_eq!(derived, 0x0064_6725, "the sample's stored variable entry");
        assert_eq!(
            u32::from_le_bytes(table[31 * 8 + 4..31 * 8 + 8].try_into().unwrap()),
            BLOCK_INFO_VERSION
        );
    }

    #[test]
    fn round_trips_a_tree() {
        let plan = plan_for(&[
            ("eboot.bin", 100),
            ("sce_sys/param.json", 40),
            ("data/big.bin", 3 * BLOCK + 17),
            ("data/small.bin", 5),
        ]);
        let payload = |path: &str| -> Result<Vec<u8>> {
            let n = match path {
                "eboot.bin" => 100,
                "sce_sys/param.json" => 40,
                "data/big.bin" => 3 * BLOCK + 17,
                "data/small.bin" => 5,
                other => return format_err(format!("unexpected read of {other}")),
            };
            Ok((0..n).map(|i| (i % 251) as u8).collect())
        };
        let mut read = payload;
        let inner = write(
            &plan,
            crate::crypto::DEFAULT_PASSCODE,
            &mut read,
            (1_700_000_000, 0),
        )
        .unwrap();
        assert_eq!(inner.image.len(), (plan.ndblock * BLOCK) as usize);
        assert_eq!(inner.block_info_offset % BLOCK, 0);

        let mounted = read_mount(&inner).unwrap();
        assert!(mounted.flt_ok, "flat-path table must hash every path");
        let mut paths: Vec<&str> = mounted.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "data/big.bin",
                "data/small.bin",
                "eboot.bin",
                "sce_sys/keystone",
                "sce_sys/param.json",
            ]
        );
        for f in &mounted.files {
            let at = f.offset as usize;
            let bytes = &inner.image[at..at + f.size as usize];
            if f.path == "sce_sys/keystone" {
                assert_eq!(bytes, keystone(crate::crypto::DEFAULT_PASSCODE));
            } else {
                let want: Vec<u8> = (0..f.size).map(|i| (i % 251) as u8).collect();
                assert_eq!(bytes, want.as_slice(), "{}", f.path);
            }
        }
    }

    fn read_mount(inner: &InnerImage) -> Result<InnerMount> {
        read(&inner.image, inner.meta_base)
    }
}
