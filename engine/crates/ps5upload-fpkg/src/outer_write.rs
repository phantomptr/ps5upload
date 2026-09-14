//! The outer PFS writer: the five-inode template the samples carry, encrypted.
//!
//! Layout, measured on both samples: `[pfs_image.dat blocks][naps block][superblock
//! (plaintext)][inode table][root dirents][flat-path table][uroot dirents]`. Every block
//! but the superblock is AES-128-XTS encrypted — data blocks with the block index as the
//! sector, metadata blocks with bit 47 set — and each block's plaintext SHA3-256 is what
//! `imagedigs.dat` carries (byte-reversed there).

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
const DIRECT_SLOTS: usize = 12;
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

/// Where every block of the outer image lives. Both writers — the in-memory one and the
/// streaming one — take their geometry from here, so they cannot drift apart.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub naps_block: u64,
    pub superblock_block: u64,
    pub table_block: u64,
    pub root_block: u64,
    pub flt_block: u64,
    pub uroot_block: u64,
    pub first_indirect_block: u64,
    pub indirect_blocks: u64,
    pub ndblock: u64,
}

/// The block order: the data, then the naps layout, the superblock, the inode table, the
/// root dirents, the flat-path table, the uroot dirents, and the indirect tables last.
pub fn layout(inner_blocks: u64) -> Result<Layout> {
    if inner_blocks == 0 || inner_blocks > max_inner_blocks() {
        return format_err(format!(
            "an inner image of {inner_blocks} blocks needs more indirect slots than a dinode has"
        ));
    }
    let indirect_needed = (inner_blocks as usize).saturating_sub(DIRECT_SLOTS);
    let indirect_blocks = indirect_needed.div_ceil(PER_INDIRECT) as u64;
    Ok(Layout {
        naps_block: inner_blocks,
        superblock_block: inner_blocks + 1,
        table_block: inner_blocks + 2,
        root_block: inner_blocks + 3,
        flt_block: inner_blocks + 4,
        uroot_block: inner_blocks + 5,
        first_indirect_block: inner_blocks + 6,
        indirect_blocks,
        ndblock: inner_blocks + 6 + indirect_blocks,
    })
}

/// The largest inner image this writer can describe: the twelve direct slots plus five
/// indirect ones, each holding a 36-byte `{digest, block}` record — roughly 570 MiB. A
/// larger source needs the dinode's double-indirect slot, which is still unverified
/// (writer plan, gate G1). Callers check this before building the image, not after.
pub fn max_inner_blocks() -> u64 {
    (DIRECT_SLOTS + PER_INDIRECT * INDIRECT_SLOTS) as u64
}

/// One metadata block: its index, its plaintext digest, and its bytes.
pub type MetadataBlock = (u64, [u8; 32], Vec<u8>);

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
) -> Result<Vec<MetadataBlock>> {
    let inner_size = inner_blocks * BLOCK;
    let mut out: Vec<MetadataBlock> = Vec::new();
    fn push(out: &mut Vec<MetadataBlock>, index: u64, block: Vec<u8>) {
        let digest = sha3(&block);
        out.push((index, digest, block));
    }

    let mut naps_block = naps.to_vec();
    naps_block.resize(BLOCK as usize, 0);
    let naps_digest = sha3(&naps_block);
    push(&mut out, lay.naps_block, naps_block);

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
    sb[0x368] = 1;
    sb[0x370..0x380].copy_from_slice(&seed);
    let sb_slot = out.len();
    push(&mut out, lay.superblock_block, sb);

    // Indirect blocks: `{SHA3(plaintext), block}` records at the dinode's 36-byte stride,
    // covering the data blocks past the twelve direct slots (1820 blocks each). They are
    // laid out after the uroot dirents, so they are built here and pushed last.
    let mut indirect: Vec<(u64, [u8; 32])> = Vec::new();
    let mut indirect_blocks_extra: Vec<Vec<u8>> = Vec::new();
    for chunk in 0..lay.indirect_blocks {
        let mut block = vec![0u8; BLOCK as usize];
        for slot in 0..PER_INDIRECT {
            let index = DIRECT_SLOTS + chunk as usize * PER_INDIRECT + slot;
            if index >= inner_blocks as usize {
                break;
            }
            let at = slot * 36;
            block[at..at + 32].copy_from_slice(&data_digests[index]);
            block[at + 32..at + 36].copy_from_slice(&(index as u32).to_le_bytes());
        }
        indirect.push((lay.first_indirect_block + chunk, sha3(&block)));
        indirect_blocks_extra.push(block);
    }

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
            size_stored: inner_size,
            direct: data_digests
                .iter()
                .take(DIRECT_SLOTS)
                .enumerate()
                .map(|(i, d)| (i as u32, *d))
                .collect(),
            indirect,
            blocks: Some(inner_blocks as u32),
        },
        DinodeRecord {
            index: 4,
            mode: 0o100555,
            nlink: 1,
            flags: 0xD,
            size: naps.len() as u64,
            size_stored: naps.len() as u64,
            direct: vec![(lay.naps_block as u32, naps_digest)],
            indirect: Vec::new(),
            blocks: None,
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
    for (k, block) in indirect_blocks_extra.into_iter().enumerate() {
        push(&mut out, lay.first_indirect_block + k as u64, block);
    }

    // The inode-signature record inside the superblock, then its ICV.
    {
        let (_, _, sb) = &mut out[sb_slot];
        sb[0xB8..0xD8].copy_from_slice(&table_digest);
        sb[0xD8..0xE0].copy_from_slice(&lay.table_block.to_le_bytes());
        let mut zeroed = sb[..0x5A0].to_vec();
        zeroed[0x380..0x3A0].fill(0);
        let icv = sha3(&zeroed);
        sb[0x380..0x3A0].copy_from_slice(&icv);
    }
    // The superblock's digest is its ICV-bearing plaintext.
    out[sb_slot].1 = sha3(&out[sb_slot].2);

    if out.len() as u64 != lay.ndblock - inner_blocks {
        return format_err("outer layout block count is inconsistent");
    }
    Ok(out)
}

/// Build the encrypted outer image around a stored inner image.
///
/// `afids` are the outer table's per-file afid values — the uroot files' ordinals, which
/// the samples carry as 0 (`pfs_image.dat`) and 1 (`naps_pkg_layout.dat`).
pub fn write(
    inner: &[u8],
    naps: &[u8],
    seed: [u8; 16],
    content_id: &str,
    passcode: &str,
    time: (i64, u32),
) -> Result<OuterImage> {
    if !inner.len().is_multiple_of(BLOCK as usize) || inner.is_empty() {
        return format_err("the inner image must be a non-empty whole number of blocks");
    }
    if naps.len() > BLOCK as usize {
        return format_err("naps_pkg_layout.dat does not fit one block");
    }
    let inner_blocks = inner.len() as u64 / BLOCK;
    let lay = layout(inner_blocks)?;
    let data_digests: Vec<[u8; 32]> = inner
        .as_chunks::<{ BLOCK as usize }>()
        .0
        .iter()
        .map(|b| sha3(b))
        .collect();

    let ekpfs = derive_ekpfs(content_id, passcode);
    let xts = Xts::new(&derive_xts_keys(&ekpfs, &seed));
    let mut plaintext_digests = vec![[0u8; 32]; lay.ndblock as usize];
    let mut image = Vec::with_capacity(lay.ndblock as usize * BLOCK as usize);
    for (index, chunk) in inner.as_chunks::<{ BLOCK as usize }>().0.iter().enumerate() {
        let mut bytes = chunk.to_vec();
        xts.encrypt(index as u64, &mut bytes);
        plaintext_digests[index] = data_digests[index];
        image.extend_from_slice(&bytes);
    }
    for (index, digest, mut plaintext) in
        metadata_blocks(&lay, inner_blocks, naps, &data_digests, seed, time)?
    {
        if index != lay.superblock_block {
            let sector = if index < lay.superblock_block {
                index
            } else {
                SIGNED_SECTOR_FLAG | index
            };
            xts.encrypt(sector, &mut plaintext);
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

    /// An image past the twelve direct slots exercises the indirect tables.
    #[test]
    fn a_large_image_uses_indirect_blocks() {
        let inner = vec![0x5Au8; 20 * BLOCK as usize];
        let naps = vec![9u8; 432];
        let outer = write(
            &inner,
            &naps,
            [0x22; 16],
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
}
