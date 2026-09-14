//! The install-metadata (SI) segment: the PlayGo helpers, the NAPS metric records and the
//! trailing STORED ZIP.
//!
//! The header fields and the member order are the samples'; the PlayGo files are generated
//! to the shapes the reference documents (their sizes match the samples: a 416-byte chunk
//! file, a 16 + n ficm, a `0x38 + 8n` hash table). `naps_meta_18` is the AES-128-XTS TLV
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

/// The 400-byte `playgo-chunk.dat` (`plgx`, version 0x1000) for the single-image /
/// single-chunk / single-scenario profile: one chunk, one mchunk covering `[0, CNT offset)`,
/// no labels. Transcribed from the three third-party packages, which are the samples whose
/// install actually transfers. Sony's own `webbrowser.pkg` uses a 416-byte variant that
/// splits the region at the inner metadata base into two mchunks; that split is unverified
/// through a transfer, so this profile follows the proven one.
pub fn playgo_chunk_dat(content_id: &str, mchunk_size: u64) -> Result<Vec<u8>> {
    if content_id.len() != 36 {
        return format_err(format!(
            "content id must be 36 characters, got {}",
            content_id.len()
        ));
    }
    let mut d = vec![0u8; 0x190];
    d[0x00..0x04].copy_from_slice(b"plgx");
    d[0x04..0x06].copy_from_slice(&0x1000u16.to_le_bytes());
    d[0x08..0x0A].copy_from_slice(&1u16.to_le_bytes());
    d[0x0A..0x0C].copy_from_slice(&1u16.to_le_bytes());
    d[0x0C..0x0E].copy_from_slice(&0u16.to_le_bytes());
    d[0x0E..0x10].copy_from_slice(&1u16.to_le_bytes());
    d[0x10..0x14].copy_from_slice(&(0x190u32).to_le_bytes());
    d[0x14..0x16].copy_from_slice(&0u16.to_le_bytes());
    d[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
    d[0x1E] = 0x85;
    // The mchunk count. It has to agree with the `mchunk_attrs` record's length below
    // (one `{offset, size}` pair = 0x10 bytes): a package that declares two mchunks while
    // carrying one leaves the console reading a phantom second pair out of the record that
    // follows, and it refuses to transfer — measured against the three scene packages, whose
    // one-mchunk profile says 1 here, and Sony's webbrowser, whose two-mchunk profile says 2.
    d[0x20] = 0x01;
    d[0x24] = 0x01;
    d[0x30] = 0x11;
    d[0x38..0x40].fill(0xFF);
    d[0x40..0x64].copy_from_slice(content_id.as_bytes());
    for (at, offset, size) in [
        (0xC0usize, 0x100u32, 0x20u32),
        (0xC8, 0x120, 0x04),
        (0xD0, 0x130, 0x01),
        (0xD8, 0x140, 0x10),
        (0xE0, 0x150, 0x20),
        (0xE8, 0x170, 0x02),
        (0xF0, 0x180, 0x01),
    ] {
        d[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        d[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
    }
    d[0x100] = 0x80;
    d[0x102] = 0x03;
    d[0x104..0x108].copy_from_slice(&1u32.to_le_bytes());
    d[0x108] = 0x11;
    d[0x110..0x118].copy_from_slice(&u64::MAX.to_le_bytes());
    // chunk_mchunks: chunk #0 is mchunk #0.
    d[0x120..0x124].copy_from_slice(&0u32.to_le_bytes());
    // mchunk_attrs: one {offset 0, size} entry spanning the whole mount image.
    d[0x148..0x150].copy_from_slice(&mchunk_size.to_le_bytes());
    // inner mchunk attrs, then the constant {1, 1} marker.
    d[0x150..0x158].copy_from_slice(&0x21u64.to_le_bytes());
    d[0x164..0x166].copy_from_slice(&1u16.to_le_bytes());
    d[0x166..0x168].copy_from_slice(&1u16.to_le_bytes());
    Ok(d)
}

/// `playgo-ficm.dat`: a 16-byte header plus a zero byte per file.
pub fn playgo_ficm(file_count: u32) -> Vec<u8> {
    let mut d = vec![0u8; 0x10 + file_count as usize];
    d[0x00..0x04].copy_from_slice(&1u32.to_le_bytes());
    d[0x08..0x0C].copy_from_slice(&0x10u32.to_le_bytes());
    d[0x0C..0x10].copy_from_slice(&file_count.to_le_bytes());
    d
}

/// The per-chunk constants `playgo-hash-table.dat` carries (content-independent).
const HASH_TABLE_ENTRIES: [[u8; 8]; 5] = [
    [0x8E, 0x54, 0xCB, 0x4D, 0x4A, 0xF6, 0x30, 0x0E],
    [0xF2, 0xBF, 0xF6, 0x27, 0xB9, 0x8F, 0x88, 0x53],
    [0xCB, 0xDC, 0xC6, 0x3E, 0xEC, 0xB3, 0xC4, 0xAE],
    [0x0B, 0xF4, 0xE9, 0xC5, 0xDA, 0xF8, 0xC9, 0xAE],
    [0x4C, 0xF7, 0x0C, 0x08, 0x17, 0x4D, 0xCB, 0xD3],
];

/// `playgo-hash-table.dat`: a `0x28` header, a 16-byte constant prefix and an 8-byte
/// constant per chunk, so `0x38 + 8n` bytes.
pub fn playgo_hash_table(chunk_count: u32) -> Vec<u8> {
    let mut d = vec![0u8; 0x38 + chunk_count as usize * 8];
    d[0x00..0x04].copy_from_slice(&1u32.to_le_bytes());
    d[0x04..0x08].copy_from_slice(&0x0800_0000u32.to_le_bytes());
    d[0x08..0x0C].copy_from_slice(&0x38u32.to_le_bytes());
    d[0x0C..0x10].copy_from_slice(&(chunk_count * 8).to_le_bytes());
    d[0x18..0x1C].copy_from_slice(&[0x7F, b'F', b'L', b'T']);
    d[0x24..0x28].copy_from_slice(&chunk_count.to_le_bytes());
    d[0x28..0x38].copy_from_slice(&crate::flt::HEADER_SEED);
    for i in 0..chunk_count as usize {
        let entry = HASH_TABLE_ENTRIES[i.min(HASH_TABLE_ENTRIES.len() - 1)];
        d[0x38 + i * 8..0x40 + i * 8].copy_from_slice(&entry);
    }
    d
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

/// One `naps_meta_18` block-map entry: `co, cs, ps, c0, c1, flag` and the owning file's
/// afid index (for app payload blocks).
type Block = (u64, u32, u32, u32, u32, u32, Option<usize>);

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
    /// Digests of an image already in memory, in afid order as `files` lists it.
    pub fn of_image(image: &[u8], files: &[(String, u64, u64)]) -> Self {
        let files = files
            .iter()
            .map(|(_, offset, size)| {
                let at = (*offset as usize).min(image.len());
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
    let inner_blocks = (inner_size / BLOCK) as u32;
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

    // The block map: one entry per file, then the data-region holes, then the metadata.
    // co, cs, ps, c0, c1, flag, owner (afid index for file blocks, `None` otherwise).
    let mut blocks: Vec<Block> = Vec::new();
    for (i, (_, offset, size)) in files.iter().enumerate() {
        let cs = (*size).min(u32::MAX as u64) as u32;
        blocks.push((*offset, cs, cs, cs, 0, 0x4009_0000, Some(i)));
    }
    let hole_start = data_end.div_ceil(UBLOCK) * UBLOCK;
    let mut at = hole_start;
    while at < meta_base {
        let ps = UBLOCK.min(meta_base - at) as u32;
        blocks.push((at, ps, ps, ps, 0, 0x4011_0000, None));
        at += UBLOCK;
    }
    let meta_len = inner_size.saturating_sub(meta_base);
    let mut at = meta_base;
    while at < inner_size {
        let ps = UBLOCK.min(inner_size - at) as u32;
        blocks.push((at, ps, ps, ps, 0, 0x4005_0000, None));
        at += UBLOCK;
    }

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
        for (co, cs, ps, c0, c1, flag, _) in &blocks {
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
        for (co, _, ps, _, _, flag, owner) in &blocks {
            let digest: [u8; 32] = if *flag == 0x4011_0000 {
                sha3(&zeros[..*ps as usize])
            } else if let Some(i) = owner {
                digests.files[*i]
            } else {
                let at = (*co).saturating_sub(meta_base) as usize;
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
    fn playgo_shapes_match_the_samples() {
        // The third-party profile: one chunk, one mchunk spanning the whole mount image,
        // and the container's own offset as its size (crimson: 0x80000 against a 0x80000
        // container).
        let chunk = playgo_chunk_dat("IV9999-WEBB00002_00-XXXXXXXXXXXXXXXX", 0xC0000).unwrap();
        assert_eq!(chunk.len(), 400);
        assert_eq!(&chunk[..4], b"plgx");
        assert_eq!(&chunk[0x40..0x64], b"IV9999-WEBB00002_00-XXXXXXXXXXXXXXXX");
        assert_eq!(
            u32::from_le_bytes(chunk[0x10..0x14].try_into().unwrap()),
            400
        );
        // One mchunk: {offset 0, size}, and chunk #0 references mchunk #0. The header's
        // mchunk count must match the single pair the attrs record carries.
        assert_eq!(u32::from_le_bytes(chunk[0x20..0x24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(chunk[0xDC..0xE0].try_into().unwrap()),
            0x10,
            "one 16-byte mchunk pair"
        );
        assert_eq!(
            u32::from_le_bytes(chunk[0x104..0x108].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_le_bytes(chunk[0x140..0x148].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(chunk[0x148..0x150].try_into().unwrap()),
            0xC0000
        );
        assert_eq!(
            u32::from_le_bytes(chunk[0x120..0x124].try_into().unwrap()),
            0
        );
        let ficm = playgo_ficm(10);
        assert_eq!(ficm.len(), 26);
        let table = playgo_hash_table(5);
        assert_eq!(table.len(), 0x60);
        assert_eq!(u32::from_le_bytes(table[0x24..0x28].try_into().unwrap()), 5);
    }

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
