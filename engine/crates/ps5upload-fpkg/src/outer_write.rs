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
    ino[0x60..0x64].copy_from_slice(&(rec.direct.len() as u32).to_le_bytes());
    for (i, (block, digest)) in rec.direct.iter().enumerate() {
        let at = DIRECT_AT + i * 36;
        ino[at..at + 32].copy_from_slice(digest);
        ino[at + 32..at + 36].copy_from_slice(&block.to_le_bytes());
    }
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
    if inner_blocks > 12 {
        return format_err(
            "inner images past 12 data blocks need the indirect-block layout (gate G1)",
        );
    }
    let naps_block = inner_blocks;
    let superblock_block = inner_blocks + 1;
    let table_block = inner_blocks + 2;
    let root_block = inner_blocks + 3;
    let flt_block = inner_blocks + 4;
    let uroot_block = inner_blocks + 5;
    let ndblock = inner_blocks + 6;

    let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(ndblock as usize);
    for i in 0..inner_blocks {
        let at = (i * BLOCK) as usize;
        blocks.push(inner[at..at + BLOCK as usize].to_vec());
    }
    let mut naps_block_bytes = naps.to_vec();
    naps_block_bytes.resize(BLOCK as usize, 0);
    blocks.push(naps_block_bytes);

    // Superblock: the template the samples share, minus the values that vary.
    let mut sb = vec![0u8; BLOCK as usize];
    sb[0x00..0x08].copy_from_slice(&2i64.to_le_bytes());
    sb[0x08..0x10].copy_from_slice(&20_130_315i64.to_le_bytes());
    sb[0x1A] = 1; // ReadOnly
    sb[0x1C..0x1E].copy_from_slice(&0xDu16.to_le_bytes()); // Signed | Encrypted | 0x8
    sb[0x20..0x24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    sb[0x28..0x30].copy_from_slice(&1i64.to_le_bytes());
    sb[0x30..0x38].copy_from_slice(&(DINODES as i64).to_le_bytes());
    sb[0x38..0x40].copy_from_slice(&(ndblock as i64).to_le_bytes());
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
    blocks.push(sb); // placeholder; the ICV is filled once the table's digest is known

    // Inode table. ino 0 root dir, 1 flat-path table, 2 uroot, 3 pfs_image.dat, 4 naps.
    let inner_digests: Vec<[u8; 32]> = blocks[..inner_blocks as usize]
        .iter()
        .map(|b| sha3(b))
        .collect();
    let naps_digest = sha3(&blocks[naps_block as usize]);
    let mut flt_entries: Vec<(u64, u64)> = Vec::new();
    for (i, name) in ["pfs_image.dat", "naps_pkg_layout.dat"].iter().enumerate() {
        flt_entries.push((
            flt::hash_path(name),
            flt::pack_inode_entry(3 + i as u32, false, false, i as u32),
        ));
    }
    let flt_bytes = flt::write(&flt_entries);

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
            direct: vec![(root_block as u32, [0u8; 32])],
        },
        DinodeRecord {
            index: 1,
            mode: 0o100555,
            nlink: 1,
            flags: 0x2000C,
            size: flt_bytes.len() as u64,
            size_stored: flt_bytes.len() as u64,
            direct: vec![(flt_block as u32, sha3(&flt_block_bytes))],
        },
        DinodeRecord {
            index: 2,
            mode: 0o40555,
            nlink: 3,
            flags: 0xC,
            size: BLOCK,
            size_stored: BLOCK,
            direct: vec![(uroot_block as u32, [0u8; 32])],
        },
        DinodeRecord {
            index: 3,
            mode: 0o100555,
            nlink: 1,
            flags: 0xD,
            size: inner.len() as u64,
            size_stored: inner.len() as u64,
            direct: inner_digests
                .iter()
                .enumerate()
                .map(|(i, d)| (i as u32, *d))
                .collect(),
        },
        DinodeRecord {
            index: 4,
            mode: 0o100555,
            nlink: 1,
            flags: 0xD,
            size: naps.len() as u64,
            size_stored: naps.len() as u64,
            direct: vec![(naps_block as u32, naps_digest)],
        },
    ];
    for rec in &records {
        write_dinode(&mut table, rec, time);
    }
    let table_digest = sha3(&table);
    blocks.push(table);

    // Root dirents (inode 0): the flat-path table and uroot, no dot entries.
    let root_dirents = vec![
        ("inode_flat_path_table".to_string(), 1u32, plan::DIRENT_FILE),
        ("uroot".to_string(), 2u32, plan::DIRENT_DIR),
    ];
    blocks.push(padded(crate::inner::dirents_bytes(&root_dirents))?);

    // The flat-path table file.
    blocks.push(padded(flt_bytes.clone())?);

    // uroot dirents (inode 2).
    let uroot_dirents = vec![
        (".".to_string(), 2u32, plan::DIRENT_DOT),
        ("..".to_string(), 2u32, plan::DIRENT_DOTDOT),
        ("pfs_image.dat".to_string(), 3u32, plan::DIRENT_FILE),
        ("naps_pkg_layout.dat".to_string(), 4u32, plan::DIRENT_FILE),
    ];
    blocks.push(padded(crate::inner::dirents_bytes(&uroot_dirents))?);
    if blocks.len() as u64 != ndblock {
        return format_err("outer layout block count is inconsistent");
    }

    // The inode-signature record inside the superblock, then its ICV.
    {
        let sb = &mut blocks[superblock_block as usize];
        sb[0xB8..0xD8].copy_from_slice(&table_digest);
        sb[0xD8..0xE0].copy_from_slice(&table_block.to_le_bytes());
        let mut zeroed = sb[..0x5A0].to_vec();
        zeroed[0x380..0x3A0].fill(0);
        let icv = sha3(&zeroed);
        sb[0x380..0x3A0].copy_from_slice(&icv);
    }

    // Digests, encryption, assembly.
    let plaintext_digests: Vec<[u8; 32]> = blocks.iter().map(|b| sha3(b)).collect();
    let ekpfs = derive_ekpfs(content_id, passcode);
    let xts = Xts::new(&derive_xts_keys(&ekpfs, &seed));
    let mut image = Vec::with_capacity(blocks.len() * BLOCK as usize);
    for (i, block) in blocks.into_iter().enumerate() {
        let index = i as u64;
        let mut bytes = block;
        if index != superblock_block {
            let sector = if index < superblock_block {
                index
            } else {
                SIGNED_SECTOR_FLAG | index
            };
            xts.encrypt(sector, &mut bytes);
        }
        image.extend_from_slice(&bytes);
    }

    Ok(OuterImage {
        image,
        plaintext_digests,
        superblock_block,
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
