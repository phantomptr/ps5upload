//! The outer PFS writer: the five-inode template the samples carry.
//!
//! Layout, measured on both samples: `[pfs_image.dat blocks][naps block][superblock
//! (plaintext)][inode table][root dirents][flat-path table][uroot dirents]`. Each block's
//! plaintext SHA3-256 is what `imagedigs.dat` carries (byte-reversed there), and the block
//! order never depends on the mode.
//!
//! In [`crate::ImageMode::Native`] every block but the superblock is AES-128-XTS encrypted —
//! data blocks with the block index as the sector, metadata blocks with bit 47 set. In
//! [`crate::ImageMode::PlaintextNoAuth`], the default, the blocks are stored as they are and the
//! seed slot carries [`crate::PLAINTEXT_MARKER`]; that is the shape the console mounts, because
//! its PPR read path serves a marked image without authenticating it.

use crate::crypto::{derive_ekpfs, derive_xts_keys, sha3};
use crate::flt;
use crate::plan;
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{format_err, Result, BLOCK};

/// The outer template's inode count.
pub const DINODES: usize = 5;
/// Bytes of one signed 32-bit dinode.
const DINODE_LEN: usize = 0x2C8;
/// First direct block signature (32-byte digest + u32 block = 36-byte stride).
const DIRECT_AT: usize = 0x64;
/// Direct slots in a dinode.
pub const DIRECT_SLOTS: usize = 12;
/// Indirect slots in a dinode.
const INDIRECT_SLOTS: usize = 5;
/// First indirect block signature; the 36-byte stride continues past the direct slots.
const INDIRECT_AT: usize = DIRECT_AT + DIRECT_SLOTS * 36;
/// `{SHA3-256(plaintext), block u32}` records per indirect block: 64 KiB / 36.
const PER_INDIRECT: usize = BLOCK as usize / 36;

pub struct OuterImage {
    /// The encrypted image, block-aligned.
    pub image: Vec<u8>,
    /// `SHA3-256(plaintext block)` in block order — `imagedigs.dat` before byte reversal.
    pub plaintext_digests: Vec<[u8; 32]>,
    /// The superblock's block index (the FIH records its absolute offset at `0x20`).
    pub superblock_block: u64,
    pub seed: [u8; 16],
}

/// One outer dinode's fixed fields.
struct DinodeRecord {
    index: usize,
    mode: u16,
    nlink: u16,
    flags: u32,
    /// Logical size.
    size: u64,
    /// Stored size.
    size_stored: u64,
    /// `(block index, SHA3-256(plaintext block))` per direct block.
    direct: Vec<(u32, [u8; 32])>,
    /// `(block index, SHA3-256(plaintext block))` per indirect block.
    indirect: Vec<(u64, [u8; 32])>,
    /// Total block count when it exceeds the direct slots.
    blocks: Option<u32>,
}

fn write_dinode(table: &mut [u8], rec: &DinodeRecord, time: (i64, u32)) {
    let o = rec.index * DINODE_LEN;
    let ino = &mut table[o..o + DINODE_LEN];
    ino[0..2].copy_from_slice(&rec.mode.to_le_bytes());
    ino[2..4].copy_from_slice(&rec.nlink.to_le_bytes());
    ino[4..8].copy_from_slice(&rec.flags.to_le_bytes());
    ino[8..16].copy_from_slice(&rec.size.to_le_bytes());
    ino[0x10..0x18].copy_from_slice(&rec.size_stored.to_le_bytes());
    for t in 0..4 {
        ino[0x18 + t * 8..0x20 + t * 8].copy_from_slice(&time.0.to_le_bytes());
    }
    for t in 0..4 {
        ino[0x38 + t * 4..0x3C + t * 4].copy_from_slice(&time.1.to_le_bytes());
    }
    let blocks = rec.blocks.unwrap_or(rec.direct.len() as u32);
    ino[0x60..0x64].copy_from_slice(&blocks.to_le_bytes());
    for (i, (block, digest)) in rec.direct.iter().enumerate() {
        let at = DIRECT_AT + i * 36;
        ino[at..at + 32].copy_from_slice(digest);
        ino[at + 32..at + 36].copy_from_slice(&(block).to_le_bytes());
    }
    for (i, (block, digest)) in rec.indirect.iter().enumerate() {
        let at = INDIRECT_AT + i * 36;
        ino[at..at + 32].copy_from_slice(digest);
        ino[at + 32..at + 36].copy_from_slice(&(*block as u32).to_le_bytes());
    }
}

/// One indirect table: where it lives and what its 1820 records point at.
#[derive(Debug, Clone)]
pub struct IndirectTable {
    pub block: u64,
    /// `None` for a table whose records point at blocks of the thing it covers — the inner
    /// image's data, or the layout descriptor's own blocks — starting at `first_data`;
    /// otherwise the child tables' block indices, in record order.
    pub children: Option<Vec<u64>>,
    /// The first block this table covers: an absolute data-block index for the inner image,
    /// a descriptor-relative one for the layout descriptor.
    pub first_data: u64,
    /// True for the layout descriptor's own blocks rather than the inner image's, whose
    /// records take their digests from the descriptor.
    pub naps: bool,
}

/// The dinode's indirect tables, level by level.
///
/// Slot 0 of a dinode is a table of data records (the samples corroborate this); slot 1 is
/// a table whose records point at tables like slot 0's, slot 2 one level deeper again, and
/// so on — the same nesting UFS uses, with this format's 36-byte `{digest, block}` records.
/// Slot *k* covers `1820^(k+1)` data blocks, so two slots already cover 200 GB.
///
/// **Unverified**: no debug package over 570 MiB exists to check the deeper levels against
/// (writer plan, gate G1). The reader here implements the same nesting, so a round trip
/// proves self-consistency, not the console's agreement — gate G4 decides that.
#[derive(Debug, Clone, Default)]
pub struct IndirectLayout {
    /// Tables in build order: every level's tables before their parent's.
    pub tables: Vec<IndirectTable>,
    /// The dinode's slots, in order: the root table of each level.
    pub slots: Vec<u64>,
}

/// Where every block of the outer image lives. Both writers — the in-memory one and the
/// streaming one — take their geometry from here, so they cannot drift apart.
#[derive(Debug, Clone)]
pub struct Layout {
    pub naps_block: u64,
    /// How many blocks `naps_pkg_layout.dat` occupies. One for a small tree; it grows with the
    /// file count, because the descriptor carries a run per file — past twelve of them its own
    /// indirect tables (`naps_indirect`) cover the rest.
    pub naps_blocks: u64,
    pub superblock_block: u64,
    pub table_block: u64,
    pub root_block: u64,
    pub flt_block: u64,
    pub uroot_block: u64,
    pub indirect: IndirectLayout,
    /// The descriptor's dinode addressing, empty until it outgrows its direct slots.
    pub naps_indirect: IndirectLayout,
    pub ndblock: u64,
}

/// Builds one table covering `data` data blocks at `level` (0 = data records), allocating
/// block indices from `next`. Children are pushed before their parent, so a table's
/// children are already hashed when it is built.
fn build_table(
    level: u32,
    first_data: u64,
    data: u64,
    next: &mut u64,
    out: &mut Vec<IndirectTable>,
    naps: bool,
) -> u64 {
    if level == 0 {
        let block = *next;
        *next += 1;
        out.push(IndirectTable {
            block,
            children: None,
            first_data,
            naps,
        });
        return block;
    }
    let per_child = PER_INDIRECT.saturating_pow(level) as u64;
    let mut children = Vec::new();
    let mut at = first_data;
    while at < first_data + data {
        let take = per_child.min(first_data + data - at);
        children.push(build_table(level - 1, at, take, next, out, naps));
        at += take;
    }
    let block = *next;
    *next += 1;
    out.push(IndirectTable {
        block,
        children: Some(children),
        first_data,
        naps,
    });
    block
}

/// The tables the dinode needs to cover `inner_blocks`, laid out from `first_table`.
pub fn indirect_layout(inner_blocks: u64, first_table: u64) -> Result<IndirectLayout> {
    cover(inner_blocks, first_table, false)
}

/// The tables the layout descriptor's own blocks need. Empty while its twelve direct slots
/// are enough, so a descriptor that fitted before lays out exactly as it did.
pub fn naps_indirect_layout(naps_blocks: u64, first_table: u64) -> Result<IndirectLayout> {
    if naps_blocks <= DIRECT_SLOTS as u64 {
        return Ok(IndirectLayout::default());
    }
    cover(naps_blocks, first_table, true)
}

/// The dinode's indirect levels over `blocks` blocks of one kind, from `first_table`.
fn cover(blocks: u64, first_table: u64, naps: bool) -> Result<IndirectLayout> {
    let mut next = first_table;
    let mut out = Vec::new();
    let mut slots = Vec::new();
    let mut remaining = blocks.saturating_sub(DIRECT_SLOTS as u64);
    let mut first_data = DIRECT_SLOTS as u64;
    let mut level = 0u32;
    while remaining > 0 {
        if slots.len() >= INDIRECT_SLOTS {
            let what = if naps {
                "a layout descriptor"
            } else {
                "an inner image"
            };
            return format_err(format!(
                "{what} of {blocks} blocks needs more indirect levels than a dinode has"
            ));
        }
        let span = (PER_INDIRECT as u64).saturating_pow(level + 1);
        let take = span.min(remaining);
        slots.push(build_table(
            level, first_data, take, &mut next, &mut out, naps,
        ));
        first_data += take;
        remaining -= take;
        level += 1;
    }
    Ok(IndirectLayout { tables: out, slots })
}

/// The block order: the data, then the naps layout, the superblock, the inode table, the
/// root dirents, the flat-path table, the uroot dirents, and the indirect tables last.
pub fn layout(inner_blocks: u64, naps_len: u64) -> Result<Layout> {
    if inner_blocks == 0 || inner_blocks > max_inner_blocks() {
        return format_err(format!(
            "an inner image of {inner_blocks} blocks is past what a dinode can describe"
        ));
    }
    // The layout descriptor runs on into as many blocks as its bytes need. Past the twelve a
    // dinode points at directly it gets its own indirect tables, the same addressing the inner
    // image uses, so its size is described rather than capped — a descriptor written short is
    // one the console rejects at mount.
    let naps_blocks = naps_len.div_ceil(BLOCK).max(1);
    if naps_blocks > max_inner_blocks() {
        return format_err(format!(
            "the outer layout descriptor needs {naps_blocks} blocks ({naps_len} bytes), past what a dinode addresses"
        ));
    }
    let after_naps = inner_blocks + naps_blocks;
    let indirect = indirect_layout(inner_blocks, after_naps + 5)?;
    let naps_indirect =
        naps_indirect_layout(naps_blocks, after_naps + 5 + indirect.tables.len() as u64)?;
    Ok(Layout {
        naps_block: inner_blocks,
        naps_blocks,
        superblock_block: after_naps,
        table_block: after_naps + 1,
        root_block: after_naps + 2,
        flt_block: after_naps + 3,
        uroot_block: after_naps + 4,
        ndblock: after_naps + 5 + indirect.tables.len() as u64 + naps_indirect.tables.len() as u64,
        indirect,
        naps_indirect,
    })
}

/// The largest inner image the dinode can describe: the twelve direct slots plus every
/// indirect level's span. The record's block field is 32 bits, so a quarter of a terabyte
/// of blocks is the format's own ceiling.
pub fn max_inner_blocks() -> u64 {
    let mut cover = DIRECT_SLOTS as u64;
    for level in 0..INDIRECT_SLOTS as u32 {
        cover = cover.saturating_add((PER_INDIRECT as u64).saturating_pow(level + 1));
    }
    cover.min(u32::MAX as u64)
}

/// One metadata block: its index, its plaintext digest, and its bytes.
pub type MetadataBlock = (u64, [u8; 32], Vec<u8>);

/// The superblock's ICV: the digest over its first `0x5A0` bytes with its own ICV field
/// zeroed. Both writers embed it, and the install manifest reports it.
pub fn superblock_icv(superblock: &[u8]) -> [u8; 32] {
    let mut zeroed = superblock[..0x5A0].to_vec();
    zeroed[0x380..0x3A0].fill(0);
    sha3(&zeroed)
}

/// The metadata that follows the data: naps, superblock, inode table, root dirents,
/// flat-path table, uroot dirents and the indirect tables — in block order, each with its
/// plaintext digest. Everything here derives from the data blocks' digests, so the
/// streaming writer and the in-memory one emit the same bytes by construction.
pub fn metadata_blocks(
    lay: &Layout,
    inner_blocks: u64,
    naps: &[u8],
    data_digests: &[[u8; 32]],
    seed: [u8; 16],
    time: (i64, u32),
    mount_size: Option<u64>,
) -> Result<Vec<MetadataBlock>> {
    let inner_size = inner_blocks * BLOCK;
    let mut out: Vec<MetadataBlock> = Vec::new();
    fn push(out: &mut Vec<MetadataBlock>, index: u64, block: Vec<u8>) {
        let digest = sha3(&block);
        out.push((index, digest, block));
    }

    let mut naps_digests: Vec<(u32, [u8; 32])> = Vec::new();
    for (i, chunk) in naps.chunks(BLOCK as usize).enumerate() {
        let mut block = chunk.to_vec();
        block.resize(BLOCK as usize, 0);
        naps_digests.push(((lay.naps_block + i as u64) as u32, sha3(&block)));
        push(&mut out, lay.naps_block + i as u64, block);
    }

    // Superblock: the template the samples share, minus the values that vary. Its digest
    // is the game digest the header and the container carry, so it is filled in last.
    let mut sb = vec![0u8; BLOCK as usize];
    sb[0x00..0x08].copy_from_slice(&2i64.to_le_bytes());
    sb[0x08..0x10].copy_from_slice(&20_130_315i64.to_le_bytes());
    sb[0x1A] = 1; // ReadOnly
    sb[0x1C..0x1E].copy_from_slice(&0xDu16.to_le_bytes()); // Signed | Encrypted | 0x8
    sb[0x20..0x24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    sb[0x28..0x30].copy_from_slice(&1i64.to_le_bytes());
    sb[0x30..0x38].copy_from_slice(&(DINODES as i64).to_le_bytes());
    sb[0x38..0x40].copy_from_slice(&(lay.ndblock as i64).to_le_bytes());
    sb[0x40..0x48].copy_from_slice(&1i64.to_le_bytes());
    sb[0x52..0x54].copy_from_slice(&1u16.to_le_bytes()); // the inode-signature record's nlink
    sb[0x58..0x60].copy_from_slice(&(BLOCK as i64).to_le_bytes());
    sb[0x60..0x68].copy_from_slice(&(BLOCK as i64).to_le_bytes());
    for t in 0..4 {
        sb[0x68 + t * 8..0x70 + t * 8].copy_from_slice(&time.0.to_le_bytes());
    }
    for t in 0..4 {
        sb[0x88 + t * 4..0x8C + t * 4].copy_from_slice(&time.1.to_le_bytes());
    }
    sb[0xB0..0xB8].copy_from_slice(&1i64.to_le_bytes());
    // Measured on webbrowser's superblock: this flag is a 32-bit 1 at 0x36C, with 0x368
    // left zero. We wrote a single byte at 0x368, which the mount reads as a different field.
    sb[0x36C..0x370].copy_from_slice(&1u32.to_le_bytes());
    sb[0x370..0x380].copy_from_slice(&seed);
    let sb_slot = out.len();
    push(&mut out, lay.superblock_block, sb);

    // Indirect tables: `{SHA3(plaintext), block}` records at the dinode's 36-byte
    // stride. A data table's records point at data blocks past the twelve direct slots;
    // a parent's records point at the tables of the level below it. They are laid out
    // after the uroot dirents, children before parents so a parent can digest them.
    let mut records_per_table: std::collections::HashMap<u64, [u8; 32]> =
        std::collections::HashMap::new();
    let mut slots: Vec<(u64, [u8; 32])> = Vec::new();
    let mut naps_slots: Vec<(u64, [u8; 32])> = Vec::new();
    let mut table_blocks: Vec<(u64, Vec<u8>)> = Vec::new();
    for table in lay.indirect.tables.iter().chain(&lay.naps_indirect.tables) {
        let mut block = vec![0u8; BLOCK as usize];
        match &table.children {
            None => {
                for slot in 0..PER_INDIRECT {
                    let at = slot * 36;
                    // A data table covers the inner image's blocks by their absolute index;
                    // the descriptor's own tables cover it by a descriptor-relative one, and
                    // their records name the descriptor's blocks, which live at `naps_block`.
                    let (index, end, block_index) = if table.naps {
                        let k = table.first_data + slot as u64;
                        (k, lay.naps_blocks, lay.naps_block + k)
                    } else {
                        let i = table.first_data + slot as u64;
                        (i, inner_blocks, i)
                    };
                    if index >= end {
                        break;
                    }
                    let digest = if table.naps {
                        naps_digests[index as usize].1
                    } else {
                        data_digests[index as usize]
                    };
                    block[at..at + 32].copy_from_slice(&digest);
                    block[at + 32..at + 36].copy_from_slice(&(block_index as u32).to_le_bytes());
                }
            }
            Some(children) => {
                for (slot, child) in children.iter().enumerate() {
                    let at = slot * 36;
                    let digest = records_per_table.get(child).ok_or_else(|| {
                        crate::Error::Format("an indirect table is missing its child".to_string())
                    })?;
                    block[at..at + 32].copy_from_slice(digest);
                    block[at + 32..at + 36].copy_from_slice(&(*child as u32).to_le_bytes());
                }
            }
        }
        let digest = sha3(&block);
        records_per_table.insert(table.block, digest);
        if lay.indirect.slots.contains(&table.block) {
            slots.push((table.block, digest));
        }
        if lay.naps_indirect.slots.contains(&table.block) {
            naps_slots.push((table.block, digest));
        }
        table_blocks.push((table.block, block));
    }
    slots.sort_by_key(|(block, _)| *block);
    naps_slots.sort_by_key(|(block, _)| *block);

    let mut flt_entries: Vec<(u64, u64)> = Vec::new();
    for (i, name) in ["pfs_image.dat", "naps_pkg_layout.dat"].iter().enumerate() {
        flt_entries.push((
            flt::hash_path(name),
            flt::pack_inode_entry(3 + i as u32, false, false, i as u32),
        ));
    }
    let flt_bytes = flt::write(&flt_entries);
    let root_dirents = vec![
        ("inode_flat_path_table".to_string(), 1u32, plan::DIRENT_FILE),
        ("uroot".to_string(), 2u32, plan::DIRENT_DIR),
    ];
    let root_bytes = padded(crate::inner::dirents_bytes(&root_dirents))?;
    let uroot_dirents = vec![
        (".".to_string(), 2u32, plan::DIRENT_DOT),
        ("..".to_string(), 2u32, plan::DIRENT_DOTDOT),
        ("pfs_image.dat".to_string(), 3u32, plan::DIRENT_FILE),
        ("naps_pkg_layout.dat".to_string(), 4u32, plan::DIRENT_FILE),
    ];
    let uroot_bytes = padded(crate::inner::dirents_bytes(&uroot_dirents))?;

    let mut table = vec![0u8; BLOCK as usize];
    let mut flt_block_bytes = flt_bytes.clone();
    flt_block_bytes.resize(BLOCK as usize, 0);
    let records = [
        DinodeRecord {
            index: 0,
            mode: 0o40555,
            nlink: 1,
            flags: 0x2000C,
            size: BLOCK,
            size_stored: BLOCK,
            direct: vec![(lay.root_block as u32, sha3(&root_bytes))],
            blocks: None,
            indirect: Vec::new(),
        },
        DinodeRecord {
            index: 1,
            mode: 0o100555,
            nlink: 1,
            flags: 0x2000C,
            size: flt_bytes.len() as u64,
            size_stored: flt_bytes.len() as u64,
            direct: vec![(lay.flt_block as u32, sha3(&flt_block_bytes))],
            blocks: None,
            indirect: Vec::new(),
        },
        DinodeRecord {
            index: 2,
            mode: 0o40555,
            nlink: 3,
            flags: 0xC,
            size: BLOCK,
            size_stored: BLOCK,
            direct: vec![(lay.uroot_block as u32, sha3(&uroot_bytes))],
            blocks: None,
            indirect: Vec::new(),
        },
        DinodeRecord {
            index: 3,
            mode: 0o100555,
            nlink: 1,
            flags: 0xD,
            size: inner_size,
            // A compressed image records the mount it expands to here, as Sony's packages do
            // (Spider-Man 2: 108 GB stored, 272 GB here); a stored image is its own mount.
            size_stored: mount_size.unwrap_or(inner_size),
            direct: data_digests
                .iter()
                .take(DIRECT_SLOTS)
                .enumerate()
                .map(|(i, d)| (i as u32, *d))
                .collect(),
            indirect: slots,
            blocks: Some(inner_blocks as u32),
        },
        DinodeRecord {
            index: 4,
            mode: 0o100555,
            nlink: 1,
            flags: 0xD,
            size: naps.len() as u64,
            size_stored: naps.len() as u64,
            direct: naps_digests
                .iter()
                .take(DIRECT_SLOTS)
                .map(|(block, digest)| (*block, *digest))
                .collect(),
            indirect: naps_slots,
            blocks: Some(lay.naps_blocks as u32),
        },
    ];
    for rec in &records {
        write_dinode(&mut table, rec, time);
    }
    let table_digest = sha3(&table);
    push(&mut out, lay.table_block, table);
    push(&mut out, lay.root_block, root_bytes);
    push(&mut out, lay.flt_block, padded(flt_bytes)?);
    push(&mut out, lay.uroot_block, uroot_bytes);
    for (index, block) in table_blocks {
        push(&mut out, index, block);
    }

    // The inode-signature record inside the superblock, then its ICV.
    {
        let (_, _, sb) = &mut out[sb_slot];
        sb[0xB8..0xD8].copy_from_slice(&table_digest);
        sb[0xD8..0xE0].copy_from_slice(&lay.table_block.to_le_bytes());
        let icv = superblock_icv(sb);
        sb[0x380..0x3A0].copy_from_slice(&icv);
    }
    // The superblock's digest is its ICV-bearing plaintext.
    out[sb_slot].1 = sha3(&out[sb_slot].2);

    if out.len() as u64 != lay.ndblock - inner_blocks {
        return format_err("outer layout block count is inconsistent");
    }
    Ok(out)
}

/// Build the outer image around a stored inner image.
///
/// `afids` are the outer table's per-file afid values — the uroot files' ordinals, which
/// the samples carry as 0 (`pfs_image.dat`) and 1 (`naps_pkg_layout.dat`). `seed` is the
/// superblock's seed slot: a random seed in `ImageMode::Native`, [`crate::PLAINTEXT_MARKER`]
/// in the plaintext mode, whose blocks are stored as they are.
pub fn write(
    inner: &[u8],
    naps: &[u8],
    seed: [u8; 16],
    mode: crate::ImageMode,
    content_id: &str,
    passcode: &str,
    time: (i64, u32),
) -> Result<OuterImage> {
    if !inner.len().is_multiple_of(BLOCK as usize) || inner.is_empty() {
        return format_err("the inner image must be a non-empty whole number of blocks");
    }
    let inner_blocks = inner.len() as u64 / BLOCK;
    let lay = layout(inner_blocks, naps.len() as u64)?;
    let data_digests: Vec<[u8; 32]> = inner
        .as_chunks::<{ BLOCK as usize }>()
        .0
        .iter()
        .map(|b| sha3(b))
        .collect();

    // The mode decides one thing: whether every block but the superblock is XTS-transformed.
    // A plaintext image never is, so its keys are not even derived.
    let xts = match mode {
        crate::ImageMode::Native => Some(Xts::new(&derive_xts_keys(
            &derive_ekpfs(content_id, passcode),
            &seed,
        ))),
        crate::ImageMode::PlaintextNoAuth => None,
    };
    let mut plaintext_digests = vec![[0u8; 32]; lay.ndblock as usize];
    let mut image = Vec::with_capacity(lay.ndblock as usize * BLOCK as usize);
    for (index, chunk) in inner.as_chunks::<{ BLOCK as usize }>().0.iter().enumerate() {
        let mut bytes = chunk.to_vec();
        if let Some(xts) = &xts {
            xts.encrypt(index as u64, &mut bytes);
        }
        plaintext_digests[index] = data_digests[index];
        image.extend_from_slice(&bytes);
    }
    for (index, digest, mut plaintext) in
        metadata_blocks(&lay, inner_blocks, naps, &data_digests, seed, time, None)?
    {
        if index != lay.superblock_block {
            if let Some(xts) = &xts {
                let sector = if index < lay.superblock_block {
                    index
                } else {
                    SIGNED_SECTOR_FLAG | index
                };
                xts.encrypt(sector, &mut plaintext);
            }
        }
        plaintext_digests[index as usize] = digest;
        image.extend_from_slice(&plaintext);
    }

    Ok(OuterImage {
        image,
        plaintext_digests,
        superblock_block: lay.superblock_block,
        seed,
    })
}

fn padded(mut bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() > BLOCK as usize {
        return format_err("an outer metadata block overflows");
    }
    bytes.resize(BLOCK as usize, 0);
    Ok(bytes)
}

/// The superblock's absolute offset inside the finalized image, for the FIH.
pub fn superblock_absolute(outer: &OuterImage) -> u64 {
    BLOCK + outer.superblock_block * BLOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::DEFAULT_PASSCODE;

    /// The single-indirect ceiling is 12 + 1820 blocks (570 MiB); one block past it the
    /// dinode must use its second slot, and the second level must cover the rest.
    #[test]
    fn the_layout_escalates_a_level_past_570_mib() {
        let single = DIRECT_SLOTS as u64 + PER_INDIRECT as u64;
        let small = indirect_layout(single, 100).unwrap();
        assert_eq!(small.slots.len(), 1);
        assert_eq!(small.tables.len(), 1);

        // One block past the ceiling lands in the second slot: one data table, one child
        // of the second level (it covers a single block), and that level's root.
        let large = indirect_layout(single + 1, 100).unwrap();
        assert_eq!(large.slots.len(), 2, "a second slot");
        assert_eq!(large.tables.len(), 3, "one data table, one child, one root");
        assert_eq!(large.slots, [100, 102]);

        // Two slots are worth ~200 GB, which is past anything the format itself allows.
        assert!(max_inner_blocks() >= (200u64 << 30) / BLOCK);
        // Their exact capacity: the direct slots, the first level, then the second.
        let two_levels = DIRECT_SLOTS as u64 + PER_INDIRECT as u64 + (PER_INDIRECT as u64).pow(2);
        let big = indirect_layout(two_levels, 100).unwrap();
        assert_eq!(
            big.slots.len(),
            2,
            "the second slot's last block fits exactly"
        );
        assert_eq!(big.tables.len(), 1 + PER_INDIRECT + 1);
    }

    /// The descriptor's own addressing appears only once it outgrows its direct slots, so a
    /// descriptor that fitted before lays out byte for byte as it did.
    #[test]
    fn the_descriptor_addresses_itself_only_when_it_must() {
        assert!(
            naps_indirect_layout(DIRECT_SLOTS as u64, 100)
                .unwrap()
                .tables
                .is_empty(),
            "twelve blocks still fit the direct slots"
        );
        let over = naps_indirect_layout(DIRECT_SLOTS as u64 + 1, 100).unwrap();
        assert_eq!(over.slots, [100], "one table covers the thirteenth block");
        assert_eq!(over.tables.len(), 1);
        // A whole level's worth fits one table; one block more needs a second level.
        let two = naps_indirect_layout(DIRECT_SLOTS as u64 + PER_INDIRECT as u64 + 1, 100).unwrap();
        assert_eq!(two.slots.len(), 2, "a second slot");
        assert_eq!(two.tables.len(), 3, "two tables below, one root");

        // And the block order holds: the descriptor's tables land after the data's.
        let lay = layout(20, (DIRECT_SLOTS as u64 + 1) * BLOCK).unwrap();
        assert_eq!(lay.indirect.tables.len(), 1);
        assert_eq!(lay.naps_indirect.tables.len(), 1);
        assert_eq!(lay.naps_indirect.tables[0].block, lay.ndblock - 1);
        assert_eq!(lay.naps_indirect.tables[0].first_data, DIRECT_SLOTS as u64);
        assert!(lay.naps_indirect.tables[0].naps);
    }

    /// An image past the twelve direct slots exercises the indirect tables.
    #[test]
    fn a_large_image_uses_indirect_blocks() {
        let inner = vec![0x5Au8; 20 * BLOCK as usize];
        let naps = vec![9u8; 432];
        let outer = write(
            &inner,
            &naps,
            [0x22; 16],
            crate::ImageMode::Native,
            "UP0000-PPSA01234_00-TESTGAME00000000",
            DEFAULT_PASSCODE,
            (1_700_000_000, 0),
        )
        .unwrap();
        assert_eq!(outer.image.len() as u64, 27 * BLOCK);
        let cnt = crate::cnt::test_support::minimal_cnt(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &outer.plaintext_digests,
        );
        let path = std::env::temp_dir().join(format!("outer-big-{}.bin", std::process::id()));
        std::fs::write(&path, &outer.image).unwrap();
        let mut file = crate::PkgFile::open(&path).unwrap();
        let fih = crate::fih::Fih {
            signed_byte: 0,
            format_version: 3,
            pfs_offset: 0,
            pfs_size: outer.image.len() as u64,
            game_digest: outer.plaintext_digests[outer.superblock_block as usize],
            cnt_offset: outer.image.len() as u64,
        };
        let parsed = crate::cnt::Cnt::from_bytes(cnt).unwrap();
        let img = crate::outer::open(&mut file, &fih, &parsed, DEFAULT_PASSCODE).unwrap();
        std::fs::remove_file(&path).ok();
        let nodes = img.dinodes();
        let data = img.file_data(&nodes[3]);
        assert_eq!(data, inner, "the indirect tables must recover every block");
        assert_eq!(nodes[3].blocks, 20);
        assert_eq!(nodes[3].indirect[0].block, 26);
    }

    #[test]
    fn writes_and_reads_back_the_template() {
        let inner = vec![7u8; 3 * BLOCK as usize];
        let naps = vec![9u8; 432];
        let outer = write(
            &inner,
            &naps,
            [0x11; 16],
            crate::ImageMode::Native,
            "UP0000-PPSA01234_00-TESTGAME00000000",
            DEFAULT_PASSCODE,
            (1_700_000_000, 0),
        )
        .unwrap();
        assert_eq!(outer.image.len() as u64, 9 * BLOCK);
        assert_eq!(outer.superblock_block, 4);
        assert_eq!(outer.plaintext_digests.len(), 9);
        assert_eq!(superblock_absolute(&outer), 5 * BLOCK);

        // Round-trip through the reader.
        let path = std::env::temp_dir().join(format!("outer-write-{}.bin", std::process::id()));
        let cnt = crate::cnt::test_support::minimal_cnt(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &outer.plaintext_digests,
        );
        std::fs::write(&path, &outer.image).unwrap();
        let mut file = crate::PkgFile::open(&path).unwrap();
        let fih = crate::fih::Fih {
            signed_byte: 0,
            format_version: 3,
            pfs_offset: 0,
            pfs_size: outer.image.len() as u64,
            game_digest: outer.plaintext_digests[4],
            cnt_offset: outer.image.len() as u64,
        };
        let cnt_struct = crate::cnt::Cnt::from_bytes(cnt).unwrap();
        let img = crate::outer::open(&mut file, &fih, &cnt_struct, DEFAULT_PASSCODE).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(img.superblock.icv_ok);
        assert_eq!(img.superblock.dinode_count, 5);
        assert_eq!(img.superblock.ndblock, 9);
        assert_eq!(img.superblock.seed, [0x11; 16]);
        let nodes = img.dinodes();
        assert_eq!(nodes.len(), 5);
        assert_eq!(
            img.dirents(&nodes[2])
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>(),
            vec![".", "..", "pfs_image.dat", "naps_pkg_layout.dat"]
        );
        assert_eq!(
            img.dirents(&nodes[0])
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>(),
            vec!["inode_flat_path_table", "uroot"]
        );
        let data = {
            let mut out = Vec::new();
            for d in nodes[3].direct.iter().take(nodes[3].blocks as usize) {
                out.extend_from_slice(&img.plaintext[d.block as usize]);
            }
            out.truncate(nodes[3].size as usize);
            out
        };
        assert_eq!(data, inner);
    }

    /// The default mode, and the one the console mounts: the blocks are stored as they are and
    /// the seed slot carries the marker, which is what the reader goes by.
    #[test]
    fn a_plaintext_image_is_stored_as_it_is_and_marked() {
        let inner: Vec<u8> = (0..3 * BLOCK as usize).map(|i| (i % 251) as u8).collect();
        let naps = vec![9u8; 432];
        let outer = write(
            &inner,
            &naps,
            crate::PLAINTEXT_MARKER,
            crate::ImageMode::PlaintextNoAuth,
            "UP0000-PPSA01234_00-TESTGAME00000000",
            DEFAULT_PASSCODE,
            (1_700_000_000, 0),
        )
        .unwrap();

        assert_eq!(
            &outer.image[..BLOCK as usize],
            &inner[..BLOCK as usize],
            "a plaintext image's first block is its own bytes, not ciphertext"
        );
        let sb_at = outer.superblock_block as usize * BLOCK as usize;
        assert_eq!(
            &outer.image[sb_at + 0x370..sb_at + 0x380],
            &crate::PLAINTEXT_MARKER,
            "the seed slot carries the marker the console serves plaintext for"
        );

        let path = std::env::temp_dir().join(format!("outer-plain-{}.bin", std::process::id()));
        let cnt = crate::cnt::test_support::minimal_cnt(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &outer.plaintext_digests,
        );
        std::fs::write(&path, &outer.image).unwrap();
        let mut file = crate::PkgFile::open(&path).unwrap();
        let fih = crate::fih::Fih {
            signed_byte: 0,
            format_version: 3,
            pfs_offset: 0,
            pfs_size: outer.image.len() as u64,
            game_digest: outer.plaintext_digests[outer.superblock_block as usize],
            cnt_offset: outer.image.len() as u64,
        };
        let parsed = crate::cnt::Cnt::from_bytes(cnt).unwrap();
        let img = crate::outer::open(&mut file, &fih, &parsed, DEFAULT_PASSCODE).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(img.superblock.mode, crate::ImageMode::PlaintextNoAuth);
        assert!(
            img.verdicts.iter().all(|v| v.kind.is_some()),
            "every stored block must still be its own imagedigs entry"
        );
        let nodes = img.dinodes();
        assert_eq!(img.file_data(&nodes[3]), inner);
    }
}
