//! `naps_pkg_layout.dat`: the map from the on-disk inner image to the mount.
//!
//! Layout (validated by parsing `webbrowser.pkg`'s 432-byte file exactly):
//!
//! * 16-byte header of two little-endian words —
//!   `{numFiles-1:24, compType:2, numKeys-1:2, numShuffle:4, numUBlocks:24}` and
//!   `{numOuterBlocks:24, numCblockInfo-2:24}`.
//! * Sections in fixed order with fixed strides: outer-block digests (8 B), shuffle
//!   patterns (8 B), `fidx` (6 B: 40-bit little-endian offset + a 1-byte type, the last
//!   entry the mount size with type `0x40`), `u2c` mapping (10 B: uint24 base plus seven
//!   per-ublock deltas), `cblockinfo` (9 B records, bit-packed).
//!
//! `cblockinfo` records are either run-base markers (`m_isRunBase` at bit 18, carrying the
//! AES-XTS tweak and key slot of an encrypted download run plus the base of its compressed
//! offset) or per-block records carrying the block's compressed offset, uncompressed
//! offset, first-sub-chunk compressed length, even/odd flags, the KDE predictor (`2` =
//! Kraken, `4` = stored) and a shuffle index.
//!
//! For v1 nothing is compressed, so the builder emits one stored record per 256 KiB ublock
//! over a linear map and no run-bases; the KDE predictor is 4 everywhere. Which record
//! values a console needs for a stored map is the least-measured part of this format
//! (open question in the writer plan): the round trip through [`reconstruct`] pins our own
//! consistency, and G3 decides acceptance.

use crate::{format_err, Result, BLOCK};

/// The `u2c` stride.
const U2C_LEN: usize = 10;
/// The `fidx` stride.
const FIDX_LEN: usize = 6;
/// One `cblockinfo` record's stride.
const CBLOCK_LEN: usize = 9;
/// One outer-block digest's stride (the sample's are all zero).
const OUTER_DIGEST_LEN: usize = 8;
/// A NAPS ublock: 256 KiB.
pub const UBLOCK: u64 = 0x40000;
/// The type byte the final `fidx` entry (the mount size) carries.
const FIDX_TYPE_MOUNT_END: u8 = 0x40;
/// The KDE predictor value that means "stored", the only one v1 emits.
const KDE_STORED: u8 = 4;

const MAX_FILE_LEN: u64 = 0x20000;

/// One `cblockinfo` record, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cblock {
    /// A run base: re-anchors the compressed cursor and names the encryption slot.
    RunBase {
        coffset_end_mod_256k: u32,
        tweak: u32,
        key_slot: u8,
        coffset_start_256k: u32,
    },
    /// A per-block record.
    Block {
        coffset_start_mod_256k: u32,
        uoffset_start: u32,
        clen_even_minus1: u32,
        even: u8,
        odd: u8,
        kde: u8,
        shuffle: u8,
    },
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub num_files: u32,
    pub compression_type: u8,
    pub num_keys: u32,
    pub num_shuffle: u32,
    pub num_ublocks: u32,
    pub num_outer_blocks: u32,
    pub outer_digests: Vec<[u8; 8]>,
    pub shuffle_patterns: Vec<[u8; 8]>,
    /// `(offset, type)` per entry; the last entry is the mount size with type `0x40`.
    pub fidx: Vec<(u64, u8)>,
    /// `(base, deltas)` per entry, each covering eight ublocks.
    pub u2c: Vec<(u32, [u8; 7])>,
    pub cblocks: Vec<Cblock>,
}

impl Layout {
    /// The number of `u2c` entries the block count implies: one group per eight ublocks,
    /// plus a trailing group (measured on the sample: 19 ublocks carry four entries).
    pub fn u2c_count(num_ublocks: u32) -> usize {
        num_ublocks.div_ceil(8) as usize + 1
    }

    /// The mount size the layout's final `fidx` entry carries.
    pub fn mount_size(&self) -> u64 {
        self.fidx.last().map(|(o, _)| *o).unwrap_or(0)
    }
}

/// Parse a layout blob.
pub fn parse(blob: &[u8]) -> Result<Layout> {
    if blob.len() < 16 {
        return format_err("naps layout is shorter than its header");
    }
    let word0 = u64::from_le_bytes(blob[0..8].try_into().unwrap());
    let word1 = u64::from_le_bytes(blob[8..16].try_into().unwrap());
    let num_files = (word0 & 0xFF_FFFF) as u32 + 1;
    let compression_type = ((word0 >> 24) & 0x3) as u8;
    let num_keys = ((word0 >> 26) & 0x3) as u32 + 1;
    let num_shuffle = ((word0 >> 28) & 0xF) as u32;
    let num_ublocks = ((word0 >> 32) & 0xFF_FFFF) as u32;
    let num_outer_blocks = (word1 & 0xFF_FFFF) as u32;
    let num_cblock = ((word1 >> 24) & 0xFF_FFFF) as u32 + 2;

    let mut at = 16usize;
    let take = |at: &mut usize, len: usize, what: &str| -> Result<&[u8]> {
        let slice = blob
            .get(*at..*at + len)
            .ok_or_else(|| crate::Error::Format(format!("naps layout ends inside its {what}")))?;
        *at += len;
        Ok(slice)
    };

    let mut outer_digests = Vec::with_capacity(num_outer_blocks as usize);
    for _ in 0..num_outer_blocks {
        let raw = take(&mut at, OUTER_DIGEST_LEN, "outer digests")?;
        outer_digests.push(raw.try_into().unwrap());
    }
    let mut shuffle_patterns = Vec::with_capacity(num_shuffle as usize);
    for _ in 0..num_shuffle {
        let raw = take(&mut at, OUTER_DIGEST_LEN, "shuffle patterns")?;
        shuffle_patterns.push(raw.try_into().unwrap());
    }

    // The fidx count is package-dependent; the cblockinfo section is last, so what remains
    // before it (after the fixed strides) is the fidx section.
    let u2c_at =
        16 + num_outer_blocks as usize * OUTER_DIGEST_LEN + num_shuffle as usize * OUTER_DIGEST_LEN;
    let cblock_bytes = num_cblock as usize * CBLOCK_LEN;
    if blob.len() < u2c_at + cblock_bytes {
        return format_err("naps layout is shorter than its sections require");
    }
    let after_u2c = blob.len() - cblock_bytes;
    let fidx_bytes = after_u2c
        .checked_sub(u2c_at + Layout::u2c_count(num_ublocks) * U2C_LEN)
        .ok_or_else(|| {
            crate::Error::Format("naps layout has no room for its fidx section".into())
        })?;
    if fidx_bytes % FIDX_LEN != 0 {
        return format_err(format!(
            "naps fidx section is {fidx_bytes} bytes, not a multiple of 6"
        ));
    }

    let mut fidx = Vec::with_capacity(fidx_bytes / FIDX_LEN);
    for _ in 0..fidx_bytes / FIDX_LEN {
        let raw = take(&mut at, FIDX_LEN, "fidx")?;
        let offset = u64::from_le_bytes([raw[0], raw[1], raw[2], raw[3], raw[4], 0, 0, 0]);
        fidx.push((offset, raw[5]));
    }
    let mut u2c = Vec::with_capacity(Layout::u2c_count(num_ublocks));
    for _ in 0..Layout::u2c_count(num_ublocks) {
        let raw = take(&mut at, U2C_LEN, "u2c")?;
        let base = u32::from(raw[0]) | u32::from(raw[1]) << 8 | u32::from(raw[2]) << 16;
        let mut deltas = [0u8; 7];
        deltas.copy_from_slice(&raw[3..10]);
        u2c.push((base, deltas));
    }

    let mut cblocks = Vec::with_capacity(num_cblock as usize);
    for _ in 0..num_cblock {
        let raw = take(&mut at, CBLOCK_LEN, "cblockinfo")?;
        let mut lo = 0u64;
        for (i, b) in raw[..8].iter().enumerate() {
            lo |= u64::from(*b) << (8 * i);
        }
        let hi = u64::from(raw[8]);
        let coffset = (lo & 0x3_FFFF) as u32;
        if (lo >> 18) & 1 != 0 {
            cblocks.push(Cblock::RunBase {
                coffset_end_mod_256k: coffset,
                tweak: ((lo >> 19) & 0x0FFF_FFFF) as u32,
                key_slot: ((lo >> 47) & 0x3) as u8,
                coffset_start_256k: (((lo >> 49) & 0x7FFF) | ((hi & 0x1FF) << 15)) as u32,
            });
        } else {
            cblocks.push(Cblock::Block {
                coffset_start_mod_256k: coffset,
                uoffset_start: ((lo >> 19) & 0x3_FFFF) as u32,
                clen_even_minus1: ((lo >> 37) & 0x1_FFFF) as u32,
                even: ((lo >> 54) & 1) as u8,
                odd: ((lo >> 55) & 1) as u8,
                kde: ((lo >> 56) & 0x7) as u8,
                shuffle: ((lo >> 59) & 0xF) as u8,
            });
        }
    }

    Ok(Layout {
        num_files,
        compression_type,
        num_keys,
        num_shuffle,
        num_ublocks,
        num_outer_blocks,
        outer_digests,
        shuffle_patterns,
        fidx,
        u2c,
        cblocks,
    })
}

fn encode_cblock(c: &Cblock) -> [u8; CBLOCK_LEN] {
    let (lo, hi) = match c {
        Cblock::Block {
            coffset_start_mod_256k,
            uoffset_start,
            clen_even_minus1,
            even,
            odd,
            kde,
            shuffle,
        } => {
            let mut lo = u64::from(*coffset_start_mod_256k & 0x3_FFFF);
            lo |= u64::from(*uoffset_start & 0x3_FFFF) << 19;
            lo |= u64::from(*clen_even_minus1 & 0x1_FFFF) << 37;
            lo |= u64::from(*even & 1) << 54;
            lo |= u64::from(*odd & 1) << 55;
            lo |= u64::from(*kde & 0x7) << 56;
            lo |= u64::from(*shuffle & 0xF) << 59;
            (lo, 0u64)
        }
        Cblock::RunBase {
            coffset_end_mod_256k,
            tweak,
            key_slot,
            coffset_start_256k,
        } => {
            let mut lo = u64::from(*coffset_end_mod_256k & 0x3_FFFF);
            lo |= 1 << 18;
            lo |= u64::from(*tweak & 0x0FFF_FFFF) << 19;
            lo |= u64::from(*key_slot & 0x3) << 47;
            lo |= u64::from(*coffset_start_256k & 0x7FFF) << 49;
            let hi = u64::from((*coffset_start_256k >> 15) & 0x1FF);
            (lo, hi)
        }
    };
    let mut out = [0u8; CBLOCK_LEN];
    for (i, b) in out[..8].iter_mut().enumerate() {
        *b = (lo >> (8 * i)) as u8;
    }
    out[8] = hi as u8;
    out
}

/// Build a layout for a stored inner image: one record per ublock, no runs, no shuffle,
/// no outer digests.
pub fn build(
    image_len: u64,
    ndblock: u64,
    afid_offsets: &[u64],
    data_end: u64,
    meta_base: u64,
) -> Result<Vec<u8>> {
    if image_len == 0 || ndblock == 0 || !meta_base.is_multiple_of(UBLOCK) {
        return format_err("naps needs a non-empty image with 256 KiB-aligned metadata");
    }
    let mount_size = ndblock * BLOCK;
    let num_ublocks = mount_size.div_ceil(UBLOCK) as u32;
    let num_outer_blocks = image_len.div_ceil(BLOCK) as u32;
    let num_files = afid_offsets.len() as u32 + 3;

    // fidx: the afid offsets, then the data end, the metadata base and the mount size.
    let mut fidx: Vec<(u64, u8)> = afid_offsets.iter().map(|o| (*o, 0u8)).collect();
    fidx.push((data_end, 0));
    fidx.push((meta_base, 0));
    fidx.push((mount_size, FIDX_TYPE_MOUNT_END));

    // cblockinfo: a zeroed prefix record (the sample's first record is all zeros and its
    // u2c mapping starts at index 1), then one stored record per ublock.
    let mut cblocks = vec![Cblock::Block {
        coffset_start_mod_256k: 0,
        uoffset_start: 0,
        clen_even_minus1: 0,
        even: 0,
        odd: 0,
        kde: 0,
        shuffle: 0,
    }];
    for k in 0..num_ublocks as u64 {
        let uoffset = k * UBLOCK;
        let len = UBLOCK.min(mount_size - uoffset);
        cblocks.push(Cblock::Block {
            coffset_start_mod_256k: (uoffset % UBLOCK) as u32,
            uoffset_start: (uoffset % UBLOCK) as u32,
            clen_even_minus1: (len.min(MAX_FILE_LEN) - 1) as u32,
            even: 0,
            odd: 0,
            kde: KDE_STORED,
            shuffle: 0,
        });
    }

    // u2c: ublock k maps to cblockinfo index 1 + k.
    let u2c: Vec<(u32, [u8; 7])> = (0..Layout::u2c_count(num_ublocks))
        .map(|g| {
            let base = 1 + (g as u32) * 8;
            let mut deltas = [0u8; 7];
            for (i, d) in deltas.iter_mut().enumerate() {
                *d = (i + 1) as u8;
            }
            (base, deltas)
        })
        .collect();

    let mut blob: Vec<u8> = Vec::with_capacity(
        16 + fidx.len() * FIDX_LEN + u2c.len() * U2C_LEN + cblocks.len() * CBLOCK_LEN,
    );
    // compType 2 (Kraken), one key, no shuffle patterns.
    let word0 = u64::from(num_files - 1) & 0xFF_FFFF
        | 2u64 << 24
        | (u64::from(num_ublocks) & 0xFF_FFFF) << 32;
    let word1 = u64::from(num_outer_blocks) & 0xFF_FFFF
        | (u64::from(cblocks.len() as u32 - 2) & 0xFF_FFFF) << 24;
    blob.extend_from_slice(&word0.to_le_bytes());
    blob.extend_from_slice(&word1.to_le_bytes());
    for _ in 0..num_outer_blocks {
        blob.extend_from_slice(&[0u8; OUTER_DIGEST_LEN]);
    }
    for (offset, kind) in &fidx {
        let bytes = offset.to_le_bytes();
        blob.extend_from_slice(&bytes[..5]);
        blob.push(*kind);
    }
    for (base, deltas) in &u2c {
        blob.extend_from_slice(&base.to_le_bytes()[..3]);
        blob.extend_from_slice(deltas);
    }
    for c in &cblocks {
        blob.extend_from_slice(&encode_cblock(c));
    }
    Ok(blob)
}

/// Rebuild the mount from a stored inner image and its layout, the way the console's
/// reader does: walk the u2c mapping and copy each stored block.
///
/// Only stored blocks are supported (v1 writes nothing else); a Kraken block is an error.
pub fn reconstruct(image: &[u8], layout: &Layout) -> Result<Vec<u8>> {
    let mount_size = layout.mount_size();
    if mount_size == 0 || mount_size > 1 << 40 {
        return format_err(format!("implausible mount size {mount_size:#x}"));
    }
    let mut mount = vec![0u8; mount_size as usize];

    // ublock -> cblockinfo index, through the u2c groups.
    let mut starts = Vec::with_capacity(layout.num_ublocks as usize);
    for (base, deltas) in &layout.u2c {
        starts.push(*base as usize);
        for d in deltas {
            starts.push(*base as usize + usize::from(*d));
        }
    }

    for k in 0..layout.num_ublocks as u64 {
        let uoffset = k * UBLOCK;
        let len = UBLOCK.min(mount_size - uoffset);
        let index = *starts.get(k as usize).ok_or_else(|| {
            crate::Error::Format("naps u2c mapping does not cover every ublock".into())
        })?;
        let record = layout.cblocks.get(index).ok_or_else(|| {
            crate::Error::Format(format!("naps cblockinfo index {index} is out of range"))
        })?;
        match record {
            Cblock::Block {
                coffset_start_mod_256k,
                kde,
                ..
            } => {
                if *kde == 2 {
                    return format_err("naps block is Kraken-compressed; v1 stores everything");
                }
                // v1's map is linear: a block's stored offset is its mount offset.
                if *coffset_start_mod_256k != (uoffset % UBLOCK) as u32 {
                    return format_err(format!("naps block {k} is not a linear stored mapping"));
                }
                let at = uoffset as usize;
                let src = image.get(at..at + len as usize).ok_or_else(|| {
                    crate::Error::Format(format!("naps block {k} reads past the stored image"))
                })?;
                mount[at..at + len as usize].copy_from_slice(src);
            }
            Cblock::RunBase { .. } => {
                return format_err("naps run bases are not part of a stored v1 layout");
            }
        }
    }
    Ok(mount)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull one file out of a package's outer PFS, through the reader's own decryption.
    fn outer_file(pkg: &[u8], name: &str) -> Option<Vec<u8>> {
        let path = std::env::temp_dir().join(format!("naps-sample-{}.pkg", std::process::id()));
        std::fs::write(&path, pkg).ok()?;
        let mut file = crate::PkgFile::open(&path).ok()?;
        let head = file.read_at(0, crate::fih::HEADER_LEN).ok()?;
        let fih = crate::fih::parse(&head).ok()?;
        let cnt = crate::cnt::read(&mut file, fih.cnt_offset).ok()?;
        let img =
            crate::outer::open(&mut file, &fih, &cnt, crate::crypto::DEFAULT_PASSCODE).ok()?;
        std::fs::remove_file(&path).ok();

        let nodes = img.dinodes();
        let uroot = nodes.get(2)?;
        let ino = img.dirents(uroot).into_iter().find(|d| d.name == name)?.ino as usize;
        let node = nodes.get(ino)?;
        let mut out = Vec::new();
        for d in node.direct.iter().take(node.blocks.min(12) as usize) {
            out.extend_from_slice(img.plaintext.get(d.block as usize)?);
        }
        out.truncate(node.size as usize);
        Some(out)
    }

    /// The real sample's layout: 432 bytes, five outer blocks, nineteen ublocks, eight fidx
    /// entries, four u2c entries and thirty-two cblockinfo records.
    #[test]
    fn parses_the_samples_layout() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("webbrowser.pkg");
        let Ok(pkg) = std::fs::read(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let blob = outer_file(&pkg, "naps_pkg_layout.dat").unwrap();
        assert_eq!(blob.len(), 432);
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.num_files, 8);
        assert_eq!(layout.compression_type, 2);
        assert_eq!(layout.num_keys, 1);
        assert_eq!(layout.num_shuffle, 0);
        assert_eq!(layout.num_ublocks, 19);
        assert_eq!(layout.num_outer_blocks, 5);
        assert_eq!(layout.cblocks.len(), 32);
        assert_eq!(layout.u2c.len(), 4);
        let offsets: Vec<u64> = layout.fidx.iter().map(|(o, _)| *o).collect();
        assert_eq!(
            offsets,
            vec![0, 0x60, 0x3230, 0x323a, 0x6c98, 0xa626, 0x400000, 0x4a0000]
        );
        assert_eq!(layout.fidx.last().unwrap().1, 0x40);
        assert_eq!(layout.mount_size(), 0x4a0000);
    }

    #[test]
    fn builds_and_reconstructs_a_stored_image() {
        let ndblock = 40u64;
        let mount_size = ndblock * BLOCK;
        let image: Vec<u8> = (0..mount_size).map(|i| (i % 253) as u8).collect();
        let blob = build(mount_size, ndblock, &[0, 96, 12848], 0xa626, 0x80000).unwrap();
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.num_files, 6);
        assert_eq!(layout.num_ublocks, 10);
        assert_eq!(layout.num_outer_blocks, 40);
        assert_eq!(layout.mount_size(), mount_size);
        let rebuilt = reconstruct(&image, &layout).unwrap();
        assert_eq!(rebuilt, image);
    }
}
