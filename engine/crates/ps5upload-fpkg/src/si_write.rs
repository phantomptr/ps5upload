//! The install-metadata (SI) segment: the chunk CRCs, the NAPS metric records and the
//! trailing STORED ZIP. The PlayGo tables are [`crate::playgo`]'s.
//!
//! The header fields and the member order are the samples'. `naps_meta_18` is the AES-128-XTS TLV
//! metric blob whose key material is fixed across packages.

use crate::crypto::{sha3, XtsKeys};
use crate::keys;
use crate::naps::UBLOCK;
use crate::xts::Xts;
use crate::{format_err, Result, BLOCK};

/// The 64 KiB CRC reduction over the mount image, little-endian per block.
pub fn chunk_crc(mount_image: &[u8]) -> Vec<u8> {
    let blocks = mount_image.len().div_ceil(BLOCK as usize);
    let mut out = Vec::with_capacity(blocks * 4);
    for i in 0..blocks {
        let at = i * BLOCK as usize;
        let end = (at + BLOCK as usize).min(mount_image.len());
        out.extend_from_slice(&crate::crypto::crc32c(&mount_image[at..end]).to_le_bytes());
    }
    out
}

/// One 48-byte `naps_meta_300/301/302/308` record: `R` at `0x10` and `0x20`, kind id `0x3E9`
/// at `0x18`, the PFS block size at `0x28`, where `R = inner image size - 0x10000`.
pub fn naps_meta_300(inner_size: u64) -> Vec<u8> {
    let r = inner_size.saturating_sub(BLOCK);
    let mut d = vec![0u8; 48];
    d[0x10..0x18].copy_from_slice(&r.to_le_bytes());
    d[0x18..0x20].copy_from_slice(&0x3E9u64.to_le_bytes());
    d[0x20..0x28].copy_from_slice(&r.to_le_bytes());
    d[0x28..0x30].copy_from_slice(&BLOCK.to_le_bytes());
    d
}

/// One record of the `naps_meta_18` TLV stream: the tag bytes reversed, a version byte,
/// three zero bytes, a little-endian u64 length and the payload.
fn tlv(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&[tag[3], tag[2], tag[1], tag[0], 1, 0, 0, 0]);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);
}

/// One `naps_meta_18` block-map entry: `co, cs, ps, c0, c1, flag`, the owning file's afid index
/// (for app payload blocks) and the block's logical offset in the mount.
pub type Block = (u64, u32, u32, u32, u32, u32, Option<usize>, u64);

/// A compressed image's block map: each block where its compressed bytes are, with the even
/// and odd halves' stored lengths. The flags are the ones a Kraken package's own map carries:
/// `0x4005_0000` for a block that holds Kraken data, `0x4009_0000` for a file block stored raw,
/// `0x4011_0000` for the zeros before the metadata base.
pub fn kraken_blocks(image: &crate::kraken_image::KrakenImage) -> Vec<Block> {
    use crate::kraken_image::Owner;
    image
        .blocks
        .iter()
        .map(|b| {
            let lz = b.halves.iter().any(|h| h.1);
            let flag = match b.owner {
                _ if lz => 0x4005_0000,
                Owner::Gap => 0x4011_0000,
                Owner::Meta => 0x4005_0000,
                Owner::File(_) => 0x4009_0000,
            };
            let owner = match b.owner {
                Owner::File(afid) => Some(afid),
                _ => None,
            };
            (
                b.stored_at,
                b.stored_len() as u32,
                b.len,
                b.halves[0].0,
                b.halves.get(1).map_or(0, |h| h.0),
                flag,
                owner,
                b.logical,
            )
        })
        .collect()
}

/// The `naps_meta_18.dat` metric blob: a TLV stream over the inner block map, encrypted
/// with the fixed AES-128-XTS key set as a single data unit.
///
/// `files` are the inner files in afid order `(path, offset, size)`; `metadata_at` and
/// `metadata_len` locate the metadata region. Digest values the console's gate does not
/// read are still filled from the built image, so the blob is self-consistent.
///
/// What the metric blob needs from the inner image: a digest of the whole image and one
/// per content file. Both are computed while the image streams out, so the blob can be
/// built for a package too large to hold in memory.
pub struct InnerDigests {
    /// `SHA3-256` of each stored 64 KiB block of the image, in order — the `obdg` table.
    pub blocks: Vec<[u8; 32]>,
    pub files: Vec<[u8; 32]>,
}

impl InnerDigests {
    /// Digests of an image already in memory, in afid order as `files` lists it. A file's digest
    /// is over the bytes the image holds for it, which sit at its physical offset — the logical
    /// one names a position in the mount, not in the image.
    pub fn of_image(image: &[u8], files: &[(u64, u64, u64)]) -> Self {
        let files = files
            .iter()
            .map(|(_, on_disk, size)| {
                let at = (*on_disk as usize).min(image.len());
                let end = (at + *size as usize).min(image.len());
                sha3(&image[at..end])
            })
            .collect();
        Self {
            blocks: image.chunks(BLOCK as usize).map(sha3).collect(),
            files,
        }
    }
}

pub fn naps_meta_18(
    inner_size: u64,
    digests: &InnerDigests,
    meta: &[u8],
    files: &[(String, u64, u64)],
    data_end: u64,
    meta_base: u64,
    game_digest: &[u8; 32],
) -> Result<Vec<u8>> {
    // The block map: one entry per file, then the data-region holes, then the metadata.
    // co, cs, ps, c0, c1, flag, owner (afid index for file blocks, `None` otherwise).
    let mut blocks: Vec<Block> = Vec::new();
    for (i, (_, offset, size)) in files.iter().enumerate() {
        let cs = (*size).min(u32::MAX as u64) as u32;
        blocks.push((*offset, cs, cs, cs, 0, 0x4009_0000, Some(i), *offset));
    }
    let hole_start = data_end.div_ceil(UBLOCK) * UBLOCK;
    let mut at = hole_start;
    while at < meta_base {
        let ps = UBLOCK.min(meta_base - at) as u32;
        blocks.push((at, ps, ps, ps, 0, 0x4011_0000, None, at));
        at += UBLOCK;
    }
    let mut at = meta_base;
    while at < inner_size {
        let ps = UBLOCK.min(inner_size - at) as u32;
        blocks.push((at, ps, ps, ps, 0, 0x4005_0000, None, at));
        at += UBLOCK;
    }
    naps_meta_18_blocks(
        inner_size,
        inner_size,
        digests,
        meta,
        files,
        &blocks,
        meta_base,
        game_digest,
    )
}

/// [`naps_meta_18`] over an explicit block map. `stored_size` is the image as stored (the
/// header's block count follows it) and `mount_size` the mount it expands to.
#[allow(clippy::too_many_arguments)]
pub fn naps_meta_18_blocks(
    stored_size: u64,
    mount_size: u64,
    digests: &InnerDigests,
    meta: &[u8],
    files: &[(String, u64, u64)],
    blocks: &[Block],
    meta_base: u64,
    game_digest: &[u8; 32],
) -> Result<Vec<u8>> {
    let inner_size = mount_size;
    let inner_blocks = stored_size.div_ceil(BLOCK) as u32;
    let mut out: Vec<u8> = Vec::with_capacity(4096);

    // phdr: {1, 0x30, innerBlocks, UBLOCK, 1, 0x10000}. The fourth word is the U-block
    // size, not the mount size — both samples carry 0x40000 there.
    {
        let mut p = Vec::with_capacity(0x18);
        for value in [1u32, 0x30, inner_blocks, UBLOCK as u32, 1, BLOCK as u32] {
            p.extend_from_slice(&value.to_le_bytes());
        }
        tlv(&mut out, b"phdr", &p);
    }

    let meta_len = inner_size.saturating_sub(meta_base);

    // file: one 0x18 entry per content file plus the metadata pseudo-file.
    {
        let mut body = Vec::with_capacity((files.len() + 1) * 0x18);
        for (i, (_, _, size)) in files.iter().enumerate() {
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(&(i as u32).to_le_bytes());
            body.extend_from_slice(&1u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&(if i == 0 { 0u32 } else { 1 }).to_le_bytes());
        }
        body.extend_from_slice(&meta_len.to_le_bytes());
        body.extend_from_slice(&(files.len() as u32).to_le_bytes());
        body.extend_from_slice(&3u32.to_le_bytes());
        body.extend_from_slice(&0x3E9u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        tlv(&mut out, b"file", &body);
    }

    // ibcl: one class byte per block — 0x01 for an app payload block, 0x0F otherwise.
    let ibcl: Vec<u8> = blocks
        .iter()
        .map(|b| {
            if matches!(b.6, Some(i) if i > 0) {
                0x01
            } else {
                0x0F
            }
        })
        .collect();
    tlv(&mut out, b"ibcl", &ibcl);

    // i2ob / i2op: the offset projections per block.
    {
        let mut i2ob = Vec::with_capacity(blocks.len() * 0x28);
        let mut i2op = Vec::with_capacity(blocks.len() * 0x10);
        for (co, cs, ps, c0, c1, flag, _, _) in blocks {
            i2ob.extend_from_slice(&co.to_le_bytes());
            i2ob.extend_from_slice(&cs.to_le_bytes());
            i2ob.extend_from_slice(&ps.to_le_bytes());
            i2ob.extend_from_slice(&c0.to_le_bytes());
            i2ob.extend_from_slice(&c1.to_le_bytes());
            i2ob.extend_from_slice(&((co >> 16) as u32).to_le_bytes());
            i2ob.extend_from_slice(&0u32.to_le_bytes());
            i2ob.extend_from_slice(&1u32.to_le_bytes());
            i2ob.extend_from_slice(&flag.to_le_bytes());
            i2op.extend_from_slice(&co.to_le_bytes());
            i2op.extend_from_slice(&(co >> 16).to_le_bytes());
        }
        tlv(&mut out, b"i2ob", &i2ob);
        tlv(&mut out, b"i2op", &i2op);
    }

    // ihsh: a digest per block over its plaintext span.
    {
        let mut body = Vec::with_capacity(blocks.len() * 0x30);
        // A hole is a zero-filled span; a content file's digest is the one taken while
        // the image was written; a metadata span is hashed from the resident region.
        let zeros = vec![0u8; UBLOCK as usize];
        for (_, _, ps, _, _, flag, owner, logical) in blocks {
            let digest: [u8; 32] = if *flag == 0x4011_0000 {
                sha3(&zeros[..*ps as usize])
            } else if let Some(i) = owner {
                digests.files[*i]
            } else {
                let at = (*logical).saturating_sub(meta_base) as usize;
                let end = at.saturating_add(*ps as usize);
                match meta.get(at..end) {
                    Some(span) => sha3(span),
                    None => sha3(&zeros[..*ps as usize]),
                }
            };
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&ps.to_le_bytes());
            body.extend_from_slice(&digest);
            let tail = if *flag & 0x0040_0000 != 0 {
                0x3E9u64
            } else {
                0
            };
            body.extend_from_slice(&tail.to_le_bytes());
        }
        tlv(&mut out, b"ihsh", &body);
    }

    // rhsh: the superblock digest, then zeros.
    {
        let mut body = vec![0u8; 0xB0];
        body[..32].copy_from_slice(game_digest);
        tlv(&mut out, b"rhsh", &body);
    }

    // fstr: NUL-terminated paths, then the metadata pseudo-file's name.
    {
        let mut body = Vec::new();
        for (path, _, _) in files {
            body.extend_from_slice(path.as_bytes());
            body.push(0);
        }
        body.extend_from_slice(b"*PFSmetadata\0");
        tlv(&mut out, b"fstr", &body);
    }

    // twek: the fixed marker.
    {
        let mut p = vec![0u8; 0x14];
        p[0x04] = 4;
        tlv(&mut out, b"twek", &p);
    }

    // obdg: one `SHA3-256` per stored 64 KiB block of the image, in order. Measured on
    // webbrowser.pkg, whose `obdg[0]` is exactly the digest of its stored image's first
    // block and whose table runs to one entry per stored block — as it does in every sample
    // (5, 3, 1 and 15736 entries). We wrote a single whole-image digest padded with zeros.
    let obdg: Vec<u8> = digests.blocks.iter().flatten().copied().collect();
    tlv(&mut out, b"obdg", &obdg);

    // The four descriptor records, then the 16-byte pad.
    let descriptor = naps_meta_300(inner_size);
    for tag in [b"pgpl", b"pgil", b"pgpi", b"pgpu"] {
        tlv(&mut out, tag, &descriptor);
    }
    let pad = (16 - out.len() % 16) % 16;
    tlv(&mut out, b"zero", &vec![0u8; pad]);
    if !out.len().is_multiple_of(16) {
        return format_err("naps_meta_18 plaintext is not a multiple of 16 bytes");
    }

    let xts = Xts::new(&XtsKeys {
        tweak: keys::NAPS_META_18_TWEAK_KEY,
        data: keys::NAPS_META_18_DATA_KEY,
    });
    // One data unit numbered by the fixed tweak constant, not unit 0: a sample's blob
    // decrypts at this sector number and reads as garbage at 0.
    xts.encrypt(keys::NAPS_META_18_TWEAK_SECTOR, &mut out);
    Ok(out)
}

/// A STORED ZIP over the given members, in order, with a fixed DOS timestamp.
pub fn zip(members: &[(String, Vec<u8>)], time: (i64, u32)) -> Vec<u8> {
    let (date, clock) = dos_time(time.0);
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in members {
        let offset = out.len() as u32;
        let crc = crate::crypto::crc32(data);
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // STORED
        out.extend_from_slice(&clock.to_le_bytes());
        out.extend_from_slice(&date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // no extra field
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        directory.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes()); // version made by
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&clock.to_le_bytes());
        directory.extend_from_slice(&date.to_le_bytes());
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes()); // extra
        directory.extend_from_slice(&0u16.to_le_bytes()); // comment
        directory.extend_from_slice(&0u16.to_le_bytes()); // disk
        directory.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        directory.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_at = out.len() as u32;
    let directory_len = directory.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&directory_len.to_le_bytes());
    out.extend_from_slice(&directory_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn dos_time(unix_seconds: i64) -> (u16, u16) {
    // Days since the Unix epoch to a civil date, then DOS's 1980-based stamps.
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let date = (((year - 1980).clamp(0, 127) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let clock = ((secs / 3600) as u16) << 11
        | (((secs % 3600) / 60) as u16) << 5
        | ((secs % 60) / 2) as u16;
    (date, clock)
}

/// Howard Hinnant's civil-from-days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_300_carries_the_inner_geometry() {
        let m = naps_meta_300(5 * BLOCK);
        assert_eq!(m.len(), 48);
        assert_eq!(
            u64::from_le_bytes(m[0x10..0x18].try_into().unwrap()),
            4 * BLOCK
        );
        assert_eq!(u64::from_le_bytes(m[0x18..0x20].try_into().unwrap()), 0x3E9);
        assert_eq!(u64::from_le_bytes(m[0x28..0x30].try_into().unwrap()), BLOCK);
    }

    #[test]
    fn chunk_crc_is_four_bytes_per_block() {
        let image = vec![0u8; 3 * BLOCK as usize + 5];
        let crc = chunk_crc(&image);
        assert_eq!(crc.len(), 4 * 4);
    }

    #[test]
    fn zip_round_trips_through_a_member_scan() {
        let members = vec![
            ("a.txt".to_string(), b"hello".to_vec()),
            ("dir/b.bin".to_string(), vec![7u8; 100]),
        ];
        let bytes = zip(&members, (1_700_000_000, 0));
        assert_eq!(&bytes[..4], &0x0403_4B50u32.to_le_bytes());
        // The member checksum must be the ZIP format's CRC-32, not the Castagnoli one the
        // PlayGo table uses — a standard unzip rejects the latter.
        assert_eq!(
            u32::from_le_bytes(bytes[14..18].try_into().unwrap()),
            crate::crypto::crc32(b"hello")
        );
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], &0x0605_4B50u32.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes(bytes[eocd + 10..eocd + 12].try_into().unwrap()),
            2
        );
    }
}
