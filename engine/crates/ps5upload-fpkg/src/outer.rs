//! The outer PFS of a finalized image: decrypt, superblock, inodes, dirents.

use crate::cnt::Cnt;
use crate::crypto::{derive_ekpfs, derive_xts_keys, sha3};
use crate::fih::Fih;
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{format_err, i32le, le16, le32, le64, PkgFile, Result, BLOCK};

pub const DINODE_LEN: usize = 0x2C8;
const SUPERBLOCK_MAGIC: u64 = 20_130_315;
const ICV: std::ops::Range<usize> = 0x380..0x3A0;
const SIGNED_REGION: usize = 0x5A0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Data,
    Signed,
    Superblock,
}

pub struct BlockVerdict {
    pub index: u64,
    /// `None` when no decryption of the block matched its image digest.
    pub kind: Option<BlockKind>,
}

pub struct Superblock {
    pub index: u64,
    pub dinode_count: u64,
    pub ndblock: u64,
    pub inode_table_block: u32,
    pub inode_table_digest: [u8; 32],
    pub seed: [u8; 16],
    pub icv_ok: bool,
}

#[derive(Clone, Copy)]
pub struct DirectBlock {
    pub digest: [u8; 32],
    pub block: u32,
}

pub struct Dinode {
    pub mode: u16,
    pub nlink: u16,
    pub flags: u32,
    pub size: u64,
    pub size_compressed: u64,
    pub blocks: u32,
    pub direct: [DirectBlock; 12],
    pub indirect: [DirectBlock; 5],
}

pub struct Dirent {
    pub ino: u32,
    pub kind: i32,
    pub name: String,
}

pub struct OuterImage {
    pub superblock: Superblock,
    pub plaintext: Vec<Vec<u8>>,
    pub verdicts: Vec<BlockVerdict>,
}

fn block_sig(b: &[u8], at: usize) -> DirectBlock {
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&b[at..at + 32]);
    DirectBlock {
        digest,
        block: le32(b, at + 32),
    }
}

fn parse_superblock(index: u64, sb: &[u8]) -> Result<Superblock> {
    if le64(sb, 0) != 2 || le64(sb, 8) != SUPERBLOCK_MAGIC {
        return format_err("outer superblock version/magic mismatch");
    }
    let mut zeroed = sb[..SIGNED_REGION].to_vec();
    zeroed[ICV].fill(0);
    let mut seed = [0u8; 16];
    seed.copy_from_slice(&sb[0x370..0x380]);
    let table = block_sig(sb, 0xB8);
    Ok(Superblock {
        index,
        dinode_count: le64(sb, 0x30),
        ndblock: le64(sb, 0x38),
        inode_table_block: table.block,
        inode_table_digest: table.digest,
        seed,
        icv_ok: sha3(&zeroed) == sb[ICV],
    })
}

/// Decrypt every outer block, classifying each by which XTS sector makes it
/// hash to its `imagedigs` entry.
pub fn open(file: &mut PkgFile, fih: &Fih, cnt: &Cnt, passcode: &str) -> Result<OuterImage> {
    if !fih.pfs_size.is_multiple_of(BLOCK) {
        return format_err("outer image size is not a whole number of blocks");
    }
    let Some(digests) = cnt.image_digests() else {
        return format_err("CNT has no imagedigs entry");
    };
    let count = fih.pfs_size / BLOCK;
    if digests.len() as u64 != count {
        return format_err("imagedigs length does not match the outer image");
    }
    let mut raw = Vec::with_capacity(count as usize);
    for i in 0..count {
        raw.push(file.read_at(fih.pfs_offset + i * BLOCK, BLOCK as usize)?);
    }
    let Some(sb_index) = raw.iter().position(|b| sha3(b) == fih.game_digest) else {
        return format_err("no outer block matches the FIH game digest");
    };
    let superblock = parse_superblock(sb_index as u64, &raw[sb_index])?;
    let ekpfs = derive_ekpfs(&cnt.content_id, passcode);
    let xts = Xts::new(&derive_xts_keys(&ekpfs, &superblock.seed));

    let mut plaintext = Vec::with_capacity(raw.len());
    let mut verdicts = Vec::with_capacity(raw.len());
    for (i, block) in raw.into_iter().enumerate() {
        let index = i as u64;
        if i == sb_index {
            plaintext.push(block);
            verdicts.push(BlockVerdict {
                index,
                kind: Some(BlockKind::Superblock),
            });
            continue;
        }
        let mut found = None;
        for (kind, sector) in [
            (BlockKind::Data, index),
            (BlockKind::Signed, SIGNED_SECTOR_FLAG | index),
        ] {
            let mut pt = block.clone();
            xts.decrypt(sector, &mut pt);
            if sha3(&pt) == digests[i] {
                found = Some((kind, pt));
                break;
            }
        }
        match found {
            Some((kind, pt)) => {
                plaintext.push(pt);
                verdicts.push(BlockVerdict {
                    index,
                    kind: Some(kind),
                });
            }
            None => {
                plaintext.push(block);
                verdicts.push(BlockVerdict { index, kind: None });
            }
        }
    }
    Ok(OuterImage {
        superblock,
        plaintext,
        verdicts,
    })
}

impl OuterImage {
    pub fn dinodes(&self) -> Vec<Dinode> {
        let Some(table) = self
            .plaintext
            .get(self.superblock.inode_table_block as usize)
        else {
            return Vec::new();
        };
        (0..self.superblock.dinode_count as usize)
            .take_while(|j| (j + 1) * DINODE_LEN <= table.len())
            .map(|j| {
                let o = j * DINODE_LEN;
                Dinode {
                    mode: le16(table, o),
                    nlink: le16(table, o + 2),
                    flags: le32(table, o + 4),
                    size: le64(table, o + 8),
                    size_compressed: le64(table, o + 0x10),
                    blocks: le32(table, o + 0x60),
                    direct: std::array::from_fn(|k| block_sig(table, o + 0x64 + k * 36)),
                    indirect: std::array::from_fn(|k| block_sig(table, o + 0x1F4 + k * 36)),
                }
            })
            .collect()
    }

    /// Directory entries in a directory inode's first block.
    pub fn dirents(&self, dir: &Dinode) -> Vec<Dirent> {
        let Some(block) = self.plaintext.get(dir.direct[0].block as usize) else {
            return Vec::new();
        };
        let limit = (dir.size as usize).min(block.len());
        let mut out = Vec::new();
        let mut o = 0usize;
        while o + 16 <= limit {
            let name_len = le32(block, o + 8) as usize;
            let ent_size = le32(block, o + 12) as usize;
            if ent_size == 0 || o + 16 + name_len > limit {
                break;
            }
            out.push(Dirent {
                ino: le32(block, o),
                kind: i32le(block, o + 4),
                name: String::from_utf8_lossy(&block[o + 16..o + 16 + name_len]).into_owned(),
            });
            o += ent_size;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cnt, crypto::DEFAULT_PASSCODE, fih};

    #[test]
    fn dlc_sample_outer_tree() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg");
        let Ok(mut f) = PkgFile::open(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let head = f.read_at(0, fih::HEADER_LEN).unwrap();
        let fih = fih::parse(&head).unwrap();
        let cnt = cnt::read(&mut f, fih.cnt_offset).unwrap();
        let img = open(&mut f, &fih, &cnt, DEFAULT_PASSCODE).unwrap();

        assert_eq!(img.superblock.index, 2);
        assert!(img.superblock.icv_ok);
        assert_eq!(img.superblock.dinode_count, 5);
        let kinds: Vec<_> = img.verdicts.iter().map(|v| v.kind).collect();
        assert_eq!(
            kinds,
            [
                Some(BlockKind::Data),
                Some(BlockKind::Signed),
                Some(BlockKind::Superblock),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
            ]
        );
        assert_eq!(
            sha3(&img.plaintext[img.superblock.inode_table_block as usize]),
            img.superblock.inode_table_digest
        );

        let nodes = img.dinodes();
        assert_eq!(nodes.len(), 5);
        for n in &nodes {
            for d in n.direct.iter().take(n.blocks as usize) {
                assert_eq!(sha3(&img.plaintext[d.block as usize]), d.digest);
            }
        }
        let names = |ino: usize| -> Vec<String> {
            img.dirents(&nodes[ino])
                .into_iter()
                .map(|d| d.name)
                .collect()
        };
        assert_eq!(names(0), ["inode_flat_path_table", "uroot"]);
        assert_eq!(
            names(2),
            [".", "..", "pfs_image.dat", "naps_pkg_layout.dat"]
        );
    }
}
