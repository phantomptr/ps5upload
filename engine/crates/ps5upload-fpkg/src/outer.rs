//! The outer PFS of a finalized image: decrypt, superblock, inodes, dirents.

use crate::cnt::Cnt;
use crate::crypto::{derive_ekpfs, derive_xts_keys, sha3};
use crate::fih::Fih;
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{format_err, i32le, le16, le32, le64, PkgFile, Result, BLOCK};

pub const DINODE_LEN: usize = 0x2C8;
/// First direct block signature (32-byte digest + u32 block = 36-byte stride).
const DIRECT_AT: usize = 0x64;
/// First indirect block signature; the 36-byte stride continues past the 12 direct slots.
/// The samples never use indirect blocks, so this offset is structural, not measured.
const INDIRECT_AT: usize = DIRECT_AT + 12 * 36;
const BLOCK_SIG_LEN: usize = 36;
/// Direct slots in a dinode.
pub const DIRECT_SLOTS: usize = 12;
/// `{SHA3-256(plaintext), block u32}` records per indirect block: 64 KiB / 36.
pub const PER_INDIRECT: usize = BLOCK as usize / 36;
const SUPERBLOCK_MAGIC: u64 = 20_130_315;
const ICV: std::ops::Range<usize> = 0x380..0x3A0;
const SIGNED_REGION: usize = 0x5A0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// A block of `pfs_image.dat`'s ciphertext in the native mode.
    Data,
    /// An outer metadata block's ciphertext in the native mode, transformed with bit 47 set.
    Signed,
    /// The superblock, which is never transformed in either mode.
    Superblock,
    /// A block of a plaintext package, stored as it is. The mode omits the keyed authentication
    /// tags, so there is no data/metadata distinction to report — the digest is the whole check.
    Plaintext,
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
    /// The seed slot's 16 bytes — a random seed in the native mode, and
    /// [`crate::PLAINTEXT_MARKER`] in the plaintext one, which is how the two are told apart.
    pub seed: [u8; 16],
    /// Decided by `seed`, not by a caller's flag: a package says how it is stored.
    pub mode: crate::ImageMode,
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

pub(crate) fn parse_superblock(index: u64, sb: &[u8]) -> Result<Superblock> {
    if le64(sb, 0) != 2 || le64(sb, 8) != SUPERBLOCK_MAGIC {
        return format_err("outer superblock version/magic mismatch");
    }
    let mut zeroed = sb[..SIGNED_REGION].to_vec();
    zeroed[ICV].fill(0);
    let mut seed = [0u8; 16];
    seed.copy_from_slice(&sb[0x370..0x380]);
    // The marker in the seed slot is what a plaintext package looks like from the inside, so the
    // package itself decides the reader's mode; there is no flag to get out of step with it.
    let mode = if seed == crate::PLAINTEXT_MARKER {
        crate::ImageMode::PlaintextNoAuth
    } else {
        crate::ImageMode::Native
    };
    let table = block_sig(sb, 0xB8);
    Ok(Superblock {
        index,
        dinode_count: le64(sb, 0x30),
        ndblock: le64(sb, 0x38),
        inode_table_block: table.block,
        inode_table_digest: table.digest,
        seed,
        mode,
        icv_ok: sha3(&zeroed) == sb[ICV],
    })
}

/// Read every outer block, classifying it by how it is stored.
///
/// A native package's blocks are tried against both XTS sectors and must hash to their `imagedigs`
/// entry; a plaintext package's are stored as they are, so the digest is checked directly. Which
/// one this is comes from the superblock's seed slot (see `Superblock::mode`), never from the
/// caller, so both kinds of package read with the same call.
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
    let plaintext_mode = superblock.mode == crate::ImageMode::PlaintextNoAuth;
    // Key derivation is native-only work, so a plaintext image never pays for it.
    let xts = (!plaintext_mode).then(|| {
        let ekpfs = derive_ekpfs(&cnt.content_id, passcode);
        Xts::new(&derive_xts_keys(&ekpfs, &superblock.seed))
    });

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
        if plaintext_mode {
            // Nothing to decrypt and no tag to classify by: the block must be its own digest.
            let kind = if sha3(&block) == digests[i] {
                Some(BlockKind::Plaintext)
            } else {
                None
            };
            plaintext.push(block);
            verdicts.push(BlockVerdict { index, kind });
            continue;
        }
        let mut found = None;
        for (kind, sector) in [
            (BlockKind::Data, index),
            (BlockKind::Signed, SIGNED_SECTOR_FLAG | index),
        ] {
            let mut pt = block.clone();
            xts.as_ref()
                .expect("a native image derives its keys")
                .decrypt(sector, &mut pt);
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

/// A dinode's direct blocks and indirect tables, checked against the records that point
/// at them, through a caller-supplied block reader. `data_blocks` is false when the
/// caller verifies the data separately (a streaming sweep) and only the metadata needs
/// checking here.
pub fn verify_dinode<F>(node: &Dinode, data_blocks: bool, mut read: F) -> Result<bool>
where
    F: FnMut(u64) -> Result<Vec<u8>>,
{
    if data_blocks {
        for d in node
            .direct
            .iter()
            .take((node.blocks as usize).min(DIRECT_SLOTS))
        {
            if sha3(&read(d.block as u64)?) != d.digest {
                return Ok(false);
            }
        }
    }
    for (level, table) in node.indirect.iter().enumerate() {
        if table.block == 0 {
            continue;
        }
        if !table_ok(&mut read, table.block as u64, level as u32, &table.digest)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn table_ok<F>(read: &mut F, block: u64, level: u32, digest: &[u8; 32]) -> Result<bool>
where
    F: FnMut(u64) -> Result<Vec<u8>>,
{
    let bytes = read(block)?;
    if sha3(&bytes) != *digest {
        return Ok(false);
    }
    if level == 0 {
        return Ok(true);
    }
    for slot in 0..PER_INDIRECT {
        let at = slot * BLOCK_SIG_LEN;
        let child = le32(&bytes, at + 32) as u64;
        if child == 0 {
            continue;
        }
        let mut record = [0u8; 32];
        record.copy_from_slice(&bytes[at..at + 32]);
        if !table_ok(read, child, level - 1, &record)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The outer inode table's records.
pub fn parse_dinodes(table: &[u8], count: u64) -> Vec<Dinode> {
    OuterImage::parse_dinodes(table, count)
}

impl OuterImage {
    pub fn dinodes(&self) -> Vec<Dinode> {
        let Some(table) = self
            .plaintext
            .get(self.superblock.inode_table_block as usize)
        else {
            return Vec::new();
        };
        Self::parse_dinodes(table, self.superblock.dinode_count)
    }

    /// The inode table's records, without an image around them.
    pub(crate) fn parse_dinodes(table: &[u8], count: u64) -> Vec<Dinode> {
        (0..count as usize)
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
                    direct: std::array::from_fn(|k| {
                        block_sig(table, o + DIRECT_AT + k * BLOCK_SIG_LEN)
                    }),
                    indirect: std::array::from_fn(|k| {
                        block_sig(table, o + INDIRECT_AT + k * BLOCK_SIG_LEN)
                    }),
                }
            })
            .collect()
    }

    /// A dinode's file bytes: the direct blocks, then the indirect block tables, bounded
    /// by the inode's size.
    /// Follows one indirect table: its records point at data blocks at level 0, or at
    /// the tables of the level below otherwise.
    fn walk_table(&self, block: u64, level: u32, size: u64, out: &mut Vec<u8>) {
        let Some(table) = self.plaintext.get(block as usize) else {
            return;
        };
        for slot in 0..PER_INDIRECT {
            if out.len() as u64 >= size {
                return;
            }
            let at = slot * BLOCK_SIG_LEN;
            let child = le32(table, at + 32) as u64;
            if child == 0 {
                continue;
            }
            if level == 0 {
                match self.plaintext.get(child as usize) {
                    Some(data) => out.extend_from_slice(data),
                    None => return,
                }
            } else {
                self.walk_table(child, level - 1, size, out);
            }
        }
    }

    /// Every indirect table's plaintext must be the block its parent (or the dinode)
    /// recorded a digest for. This is the check that makes the deeper levels trustworthy.
    pub fn indirect_ok(&self, node: &Dinode) -> bool {
        for (level, table) in node.indirect.iter().enumerate() {
            if table.block == 0 {
                continue;
            }
            if !self.table_ok(table.block as u64, level as u32, &table.digest) {
                return false;
            }
        }
        true
    }

    fn table_ok(&self, block: u64, level: u32, digest: &[u8; 32]) -> bool {
        let Some(bytes) = self.plaintext.get(block as usize) else {
            return false;
        };
        if sha3(bytes) != *digest {
            return false;
        }
        if level == 0 {
            return true;
        }
        for slot in 0..PER_INDIRECT {
            let at = slot * BLOCK_SIG_LEN;
            let child = le32(bytes, at + 32) as u64;
            if child == 0 {
                continue;
            }
            let mut record = [0u8; 32];
            record.copy_from_slice(&bytes[at..at + 32]);
            if !self.table_ok(child, level - 1, &record) {
                return false;
            }
        }
        true
    }

    pub fn file_data(&self, node: &Dinode) -> Vec<u8> {
        let mut out = Vec::new();
        for d in node
            .direct
            .iter()
            .take((node.blocks as usize).min(DIRECT_SLOTS))
        {
            match self.plaintext.get(d.block as usize) {
                Some(block) => out.extend_from_slice(block),
                None => return out,
            }
        }
        // The dinode's slots are indirect levels: slot 0's records point at data blocks,
        // slot 1's at tables like slot 0's, and so on. Unused slots are zeroed.
        for (level, table) in node.indirect.iter().enumerate() {
            if out.len() as u64 >= node.size || table.block == 0 {
                break;
            }
            self.walk_table(table.block as u64, level as u32, node.size, &mut out);
        }
        out.truncate(node.size as usize);
        out
    }

    /// Directory entries in a directory inode's first block.
    pub fn dirents(&self, dir: &Dinode) -> Vec<Dirent> {
        let Some(block) = self.plaintext.get(dir.direct[0].block as usize) else {
            return Vec::new();
        };
        Self::parse_dirents(block, dir.size)
    }

    /// A directory's entries, without an image around them.
    pub fn parse_dirents(block: &[u8], size: u64) -> Vec<Dirent> {
        let limit = (size as usize).min(block.len());
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

    /// The samples only use direct blocks, so the indirect offset is structural: it must
    /// continue the 36-byte stride after the twelve direct slots.
    #[test]
    fn indirect_blocks_follow_the_direct_stride() {
        let mut table = vec![0u8; crate::BLOCK as usize];
        table[0x60..0x64].copy_from_slice(&13u32.to_le_bytes());
        let direct0 = DIRECT_AT;
        table[direct0 + 32..direct0 + 36].copy_from_slice(&7u32.to_le_bytes());
        let indirect0 = DIRECT_AT + 12 * BLOCK_SIG_LEN;
        table[indirect0 + 32..indirect0 + 36].copy_from_slice(&9u32.to_le_bytes());
        let img = OuterImage {
            superblock: Superblock {
                index: 0,
                dinode_count: 1,
                ndblock: 1,
                inode_table_block: 0,
                inode_table_digest: [0; 32],
                seed: [0; 16],
                mode: crate::ImageMode::Native,
                icv_ok: true,
            },
            plaintext: vec![table],
            verdicts: vec![],
        };
        let nodes = img.dinodes();
        assert_eq!(nodes[0].direct[0].block, 7);
        assert_eq!(nodes[0].indirect[0].block, 9);
    }

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
