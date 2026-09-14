//! The `\x7FCNT` metadata container writer: header, entries, digests and signature.
//!
//! Big-endian throughout, offsets relative to the container. The entry set, flags, header
//! constants and digest formulas are the ones measured on the three samples; the body is
//! laid out in the sample's order, which puts the entry table (`0x0100`, which *is* the
//! entry table) right after the general digests and the digest table after it.

use std::collections::HashMap;

use crate::cnt::ids;
use crate::crypto::{derive_pfs_key, sha3};
use crate::keys;
use crate::rsa;
use crate::{format_err, Result, BLOCK};

/// Where the body region starts.
const BODY_AT: usize = 0x2000;
/// Offset of the package digest (the container's self-seal).
const PACKAGE_DIGEST_AT: usize = 0xFE0;
/// Offset of the header signature.
const SIGNATURE_AT: usize = 0x1000;
/// The `set_digests` bits a debug package sets.
const GENERAL_DIGEST_SET: u32 = 0x10DE;
/// The general-digests payload's length.
const GENERAL_LEN: usize = 0x20 + 14 * 32;

/// `(id, flags1, name)` in entry-table order.
const ENTRIES: [(u32, u32, &str); 13] = [
    (ids::DIGESTS, 0x4000_0000, ""),
    (ids::ENTRY_KEYS, 0x6000_0000, ""),
    (ids::IMAGE_KEY, 0x6000_0000, ""),
    (ids::GENERAL_DIGESTS, 0x6000_0000, ""),
    (ids::METAS, 0x6000_0000, ""),
    (ids::ENTRY_NAMES, 0x4000_0000, ""),
    (ids::IMAGE_DIGESTS, 0x0800_0000, ""),
    (ids::PLAYGO_CHUNK, 0x0800_0000, "playgo-chunk.dat"),
    (ids::ICON0_PNG, 0x0800_0000, "icon0.png"),
    (ids::ICON0_DDS, 0x0800_0000, "icon0.dds"),
    (ids::PARAM_JSON, 0x0000_0000, "param.json"),
    (ids::PLAYGO_HASH_TABLE, 0x0800_0000, "playgo-hash-table.dat"),
    (ids::PLAYGO_FICM, 0x0800_0000, "playgo-ficm.dat"),
];

pub struct CntParams<'a> {
    pub content_id: &'a str,
    pub param_json: &'a [u8],
    pub icon_png: &'a [u8],
    pub icon_dds: &'a [u8],
    pub playgo_chunk: &'a [u8],
    pub playgo_hash_table: &'a [u8],
    pub playgo_ficm: &'a [u8],
    /// One digest per outer block, natural order (the container stores them reversed).
    pub imagedigs: &'a [[u8; 32]],
    pub game_digest: [u8; 32],
    /// The finalized-image header block (`CNT+0x460` is its SHA3).
    pub fih_block: &'a [u8],
    pub outer_size: u64,
    /// Absolute offset of this container (the SI segment follows it).
    pub cnt_offset: u64,
    /// The outer superblock seed (`CNT+0x4A0`).
    pub seed: [u8; 16],
    pub passcode: &'a str,
    /// `0x26` for an app, `0x21` for additional content; DLC also sets `drm_type` `0x10`.
    pub content_type: u32,
    pub drm_type: u32,
    pub content_flags: u32,
    /// The block-aligned inner image size (the promote size).
    pub inner_size: u64,
}

fn be32_into(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn be64_into(buf: &mut [u8], at: usize, value: u64) {
    buf[at..at + 8].copy_from_slice(&value.to_be_bytes());
}

struct Body {
    bytes: Vec<u8>,
    /// Entry id to `(offset, size)`.
    spans: HashMap<u32, (u32, u32)>,
}

impl Body {
    fn new() -> Self {
        Self {
            bytes: vec![0u8; BODY_AT],
            spans: HashMap::new(),
        }
    }

    fn add(&mut self, id: u32, data: &[u8]) {
        let at = self.bytes.len() as u32;
        self.bytes.extend_from_slice(data);
        self.spans.insert(id, (at, data.len() as u32));
    }

    fn span(&self, id: u32) -> (u32, u32) {
        self.spans[&id]
    }

    fn payload(&self, id: u32) -> &[u8] {
        let (at, size) = self.span(id);
        &self.bytes[at as usize..(at + size) as usize]
    }

    fn write_at(&mut self, at: u32, data: &[u8]) {
        self.bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
    }
}

/// The entry-keys slot: the seed digest, seven key digests and seven RSA wraps. Slot 0
/// wraps the raw passcode, the others the passcode-derived keys.
fn keys_entry(content_id: &str, passcode: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + 7 * 32 + 7 * 384);
    let mut cid = [0u8; 48];
    let n = content_id.len().min(48);
    cid[..n].copy_from_slice(&content_id.as_bytes()[..n]);
    out.extend_from_slice(&sha3(&cid));
    let keys: Vec<[u8; 32]> = (0..7)
        .map(|i| derive_pfs_key(content_id, passcode, i))
        .collect();
    for key in &keys {
        let mut digest = sha3(key);
        for (d, k) in digest.iter_mut().zip(key) {
            *d ^= k;
        }
        out.extend_from_slice(&digest);
    }
    for (i, key) in keys.iter().enumerate() {
        let modulus = &keys::PASSCODE_MODULI[i * 384..(i + 1) * 384];
        let message: &[u8] = if i == 0 { passcode.as_bytes() } else { key };
        out.extend_from_slice(&rsa::pkcs1_encrypt(modulus, message));
    }
    out
}

/// The image-key slot: 0x800 bytes of back-to-back RSA wraps of the EKPFS.
fn image_key_entry(ekpfs: &[u8; 32]) -> Vec<u8> {
    let mut out = vec![0u8; 0x800];
    let mut at = 0usize;
    while at < out.len() {
        let wrap = rsa::pkcs1_encrypt(&keys::MOUNT_IMAGE_MODULUS, ekpfs);
        let n = wrap.len().min(out.len() - at);
        out[at..at + n].copy_from_slice(&wrap[..n]);
        at += wrap.len();
    }
    out
}

/// The GeneralDigests slot: `0xD256`, type `0x0102`, the set mask, then fourteen slots.
fn general_digests(
    header_prefix: &[u8],
    mount_descriptor: &[u8],
    game_digest: &[u8; 32],
    entry_digest: &dyn Fn(u32) -> Option<[u8; 32]>,
) -> Vec<u8> {
    let mut out = vec![0u8; GENERAL_LEN];
    out[0x00..0x02].copy_from_slice(&0xD256u16.to_be_bytes());
    out[0x02..0x04].copy_from_slice(&0x0102u16.to_be_bytes());
    be32_into(&mut out, 0x1C, GENERAL_DIGEST_SET);
    let mut slot = |i: usize, digest: &[u8; 32]| {
        out[0x20 + i * 32..0x40 + i * 32].copy_from_slice(digest);
    };
    let mut pre = Vec::with_capacity(0x38 + 64);
    pre.extend_from_slice(&header_prefix[0x40..0x78]);
    pre.extend_from_slice(game_digest);
    pre.extend_from_slice(&[0u8; 32]);
    slot(0, &sha3(&pre));
    slot(1, game_digest);
    let mut pre = Vec::with_capacity(0xC0);
    pre.extend_from_slice(&header_prefix[0..0x40]);
    pre.extend_from_slice(mount_descriptor);
    slot(2, &sha3(&pre));
    if let (Some(png), Some(dds)) = (entry_digest(ids::ICON0_PNG), entry_digest(ids::ICON0_DDS)) {
        let mut pre = Vec::with_capacity(64);
        pre.extend_from_slice(&png);
        pre.extend_from_slice(&dds);
        slot(3, &sha3(&pre));
    }
    if let Some(param) = entry_digest(ids::PARAM_JSON) {
        slot(5, &param);
    }
    if let (Some(chunk), Some(hash), Some(ficm)) = (
        entry_digest(ids::PLAYGO_CHUNK),
        entry_digest(ids::PLAYGO_HASH_TABLE),
        entry_digest(ids::PLAYGO_FICM),
    ) {
        let mut pre = Vec::with_capacity(96);
        pre.extend_from_slice(&chunk);
        pre.extend_from_slice(&hash);
        pre.extend_from_slice(&ficm);
        slot(6, &sha3(&pre));
    }
    slot(11, game_digest);
    out
}

/// What the container measured, for the install manifest that describes it elsewhere. Every
/// value here is one this writer just fixed; nothing is recomputed from the bytes later.
pub struct Facts {
    /// The container's padded length: where the SI segment begins.
    pub container_size: u64,
    /// The header region's end — the image-digests offset, as the samples record it.
    pub mandatory_size: u64,
    pub body_offset: u64,
    pub body_size: u64,
    pub body_digest: [u8; 32],
    pub package_digest: [u8; 32],
    /// `SHA3` of the finalized-image header block.
    pub fih_digest: [u8; 32],
    /// The general-digest slots the manifest repeats.
    pub content_digest: [u8; 32],
    pub header_digest: [u8; 32],
    pub system_digest: [u8; 32],
    pub param_digest: [u8; 32],
    /// The named entries' `(offset, size, name)`, relative to the container, in offset order.
    pub entries: Vec<(u32, u32, &'static str)>,
}

pub struct Container {
    pub bytes: Vec<u8>,
    pub facts: Facts,
}

/// Build the container. Its length ends where the SI segment begins.
pub fn write(p: &CntParams) -> Result<Container> {
    if p.content_id.len() != 36 || !p.content_id.is_ascii() {
        return format_err("content id must be 36 ASCII characters");
    }
    let count = ENTRIES.len();
    let digest_table_len = count * 32;
    let ekpfs = derive_pfs_key(p.content_id, p.passcode, 1);

    // Names table: the empty name, then each named entry's name in entry order.
    let mut names = vec![0u8];
    let mut name_offsets = vec![0u32; count];
    for (i, (_, _, name)) in ENTRIES.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        name_offsets[i] = names.len() as u32;
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }

    // The body, in the sample's order; the table, digest table and general digests are
    // placeholders until their inputs exist.
    let mut imagedigs = Vec::with_capacity(p.imagedigs.len() * 32);
    for digest in p.imagedigs {
        let mut reversed = *digest;
        reversed.reverse();
        imagedigs.extend_from_slice(&reversed);
    }
    let mut body = Body::new();
    body.add(ids::ENTRY_KEYS, &keys_entry(p.content_id, p.passcode));
    body.add(ids::IMAGE_KEY, &image_key_entry(&ekpfs));
    body.add(ids::GENERAL_DIGESTS, &vec![0u8; GENERAL_LEN]);
    body.add(ids::METAS, &vec![0u8; digest_table_len]);
    body.add(ids::DIGESTS, &vec![0u8; digest_table_len]);
    body.add(ids::ENTRY_NAMES, &names);
    body.add(ids::PARAM_JSON, p.param_json);
    body.add(ids::IMAGE_DIGESTS, &imagedigs);
    body.add(ids::PLAYGO_CHUNK, p.playgo_chunk);
    body.add(ids::ICON0_PNG, p.icon_png);
    body.add(ids::ICON0_DDS, p.icon_dds);
    body.add(ids::PLAYGO_HASH_TABLE, p.playgo_hash_table);
    body.add(ids::PLAYGO_FICM, p.playgo_ficm);
    let body_end = body.bytes.len();
    // Every sample's install segment starts on a 64 KiB boundary — `webbrowser.pkg` carries
    // 42 KiB of zero padding after the container to reach one, and the container's own
    // descriptor records that padded end rather than its content end. The PlayGo CRC table
    // is one entry per block of everything before the segment, so the padding is what makes
    // its length exact.
    let padded_end = body_end.next_multiple_of(BLOCK as usize);

    // The entry table (`0x0100`'s payload), in entry-table order.
    let mut table = vec![0u8; digest_table_len];
    for (i, (id, flags1, _)) in ENTRIES.iter().enumerate() {
        let (at, size) = body.span(*id);
        let entry = &mut table[i * 32..(i + 1) * 32];
        be32_into(entry, 0x00, *id);
        be32_into(entry, 0x04, name_offsets[i]);
        be32_into(entry, 0x08, *flags1);
        be32_into(entry, 0x0C, 0);
        be32_into(entry, 0x10, at);
        be32_into(entry, 0x14, size);
    }
    let metas_at = body.span(ids::METAS).0;
    body.write_at(metas_at, &table);

    // Per-entry digests (the table's own slot stays zero; the general digests are only
    // final once their payload exists).
    let mut digests = vec![0u8; digest_table_len];
    for (i, (id, _, _)) in ENTRIES.iter().enumerate() {
        if *id == ids::DIGESTS || *id == ids::GENERAL_DIGESTS {
            continue;
        }
        digests[i * 32..(i + 1) * 32].copy_from_slice(&sha3(body.payload(*id)));
    }

    // The header's first 0x80 bytes: the prefix the header digest covers and the content
    // descriptor the content digest covers.
    let mut head = vec![0u8; 0x80];
    head[0..4].copy_from_slice(&[0x7F, b'C', b'N', b'T']);
    be32_into(&mut head, 0x04, 0x20001);
    be32_into(&mut head, 0x08, 0x8000_0000);
    be32_into(&mut head, 0x0C, 0xC);
    be32_into(&mut head, 0x10, count as u32);
    head[0x14..0x16].copy_from_slice(&6u16.to_be_bytes());
    head[0x16..0x18].copy_from_slice(&(count as u16).to_be_bytes());
    let metas_span = body.span(ids::METAS);
    let names_at = body.span(ids::ENTRY_NAMES).0;
    be32_into(&mut head, 0x18, metas_span.0);
    let rollup_size = names_at - BODY_AT as u32;
    be32_into(&mut head, 0x1C, rollup_size);
    be64_into(&mut head, 0x20, BODY_AT as u64);
    // The samples measure this from the *padded* region end, not the content end:
    // `webbrowser.pkg`'s container holds 307,325 bytes of content in a 0x50000 region and
    // the field reads 0x4E000.
    be64_into(&mut head, 0x28, (padded_end - BODY_AT) as u64);
    be64_into(&mut head, 0x30, body.span(ids::IMAGE_DIGESTS).0 as u64);
    head[0x40..0x64].copy_from_slice(p.content_id.as_bytes());
    be32_into(&mut head, 0x70, p.drm_type);
    be32_into(&mut head, 0x74, p.content_type);
    be32_into(&mut head, 0x78, p.content_flags);

    let mut descriptor = vec![0u8; 0x80];
    be32_into(&mut descriptor, 0x00, 1);
    be32_into(&mut descriptor, 0x04, 1);
    be32_into(&mut descriptor, 0x08, 0xA000_0000);
    descriptor[0x0E..0x10].copy_from_slice(&0x030Cu16.to_be_bytes());
    be64_into(&mut descriptor, 0x10, 0x1_0000);
    be64_into(&mut descriptor, 0x18, p.outer_size);
    // The mount image ends where the SI segment begins — recorded at both `0x28` and `0x30`
    // in every sample.
    let si_offset = p.cnt_offset + padded_end as u64;
    be64_into(&mut descriptor, 0x28, si_offset);
    be64_into(&mut descriptor, 0x30, si_offset);
    be32_into(&mut descriptor, 0x38, 0x1_0000);
    let entry_digest = |id: u32| -> Option<[u8; 32]> {
        let i = ENTRIES.iter().position(|(eid, _, _)| *eid == id)?;
        Some(digests[i * 32..(i + 1) * 32].try_into().unwrap())
    };
    // The general digests hash the container's own header bytes, so write the header and
    // descriptor first and hash those.
    body.write_at(0, &head);
    body.write_at(0x400, &descriptor);
    // These two slots sit inside the header-digest preimage, so they land before it.
    body.bytes[0x440..0x460].copy_from_slice(&p.game_digest);
    body.bytes[0x460..0x480].copy_from_slice(&sha3(p.fih_block));
    let head_final = body.bytes[0..0x80].to_vec();
    let descriptor_final = body.bytes[0x400..0x480].to_vec();
    let general = general_digests(
        &head_final,
        &descriptor_final,
        &p.game_digest,
        &entry_digest,
    );
    let general_at = body.span(ids::GENERAL_DIGESTS).0;
    body.write_at(general_at, &general);
    {
        let i = ENTRIES
            .iter()
            .position(|(id, _, _)| *id == ids::GENERAL_DIGESTS)
            .unwrap();
        digests[i * 32..(i + 1) * 32].copy_from_slice(&sha3(&general));
    }
    let digest_table_at = body.span(ids::DIGESTS).0;
    body.write_at(digest_table_at, &digests);

    // Header. Every span and payload digest is captured before the buffer moves.
    let imagedigs_span = body.span(ids::IMAGE_DIGESTS);
    let image_key_span = body.span(ids::IMAGE_KEY);
    let image_key_digest = sha3(body.payload(ids::IMAGE_KEY));
    let imagedigs_digest = sha3(body.payload(ids::IMAGE_DIGESTS));
    // The manifest lists the entries the name table names, in offset order — the same set
    // the samples show.
    let mut named: Vec<(u32, u32, &'static str)> = ENTRIES
        .iter()
        .filter(|(_, _, name)| !name.is_empty())
        .map(|(id, _, name)| {
            let (at, size) = body.span(*id);
            (at, size, *name)
        })
        .collect();
    named.sort_by_key(|(at, _, _)| *at);
    let mut cnt = body.bytes;
    // Pad before the tail digests: the body digest covers the padded region, measured on the
    // sample (the region end matches its stored value, its content end does not).
    cnt.resize(padded_end, 0);
    // The container's own size, the quantity Sony's package records here (webbrowser: 327680,
    // exactly its CNT region). This held the inner image's size, which is a different quantity
    // and matches no working package; the scene packages leave the field zero.
    be32_into(&mut cnt, 0x7C, padded_end as u32);
    be32_into(&mut cnt, 0x80, 0x2024_0508);
    be32_into(&mut cnt, 0x84, 0x090F_BFC1);
    cnt[0x200..0x224].copy_from_slice(&head[0x40..0x64]);
    cnt[0x4A0..0x4B0].copy_from_slice(&p.seed);
    // The container's own absolute offset and its region size. Every sample fills both;
    // ours left them zeroed, which is what a reader uses to bound the container.
    be64_into(&mut cnt, 0x4B0, p.cnt_offset);
    be64_into(&mut cnt, 0x4B8, padded_end as u64);
    be32_into(&mut cnt, 0x510, image_key_span.0);
    be32_into(&mut cnt, 0x514, image_key_span.1);
    be32_into(&mut cnt, 0x518, imagedigs_span.0);
    be32_into(&mut cnt, 0x51C, imagedigs_span.1);
    cnt[0x520..0x540].copy_from_slice(&image_key_digest);
    cnt[0x540..0x560].copy_from_slice(&imagedigs_digest);

    // The digests over container regions.
    let rollup = sha3(&cnt[BODY_AT..BODY_AT + rollup_size as usize]);
    cnt[0x100..0x120].copy_from_slice(&rollup);
    cnt[0x140..0x160].copy_from_slice(&sha3(&digests));
    let body_digest = sha3(&cnt[BODY_AT..padded_end]);
    cnt[0x160..0x180].copy_from_slice(&body_digest);
    let package_digest = sha3(&cnt[..PACKAGE_DIGEST_AT]);
    cnt[PACKAGE_DIGEST_AT..SIGNATURE_AT].copy_from_slice(&package_digest);
    let signature = rsa::pkcs1_encrypt(&keys::METADATA_MODULUS, &sha3(&cnt[..SIGNATURE_AT]));
    cnt[SIGNATURE_AT..SIGNATURE_AT + signature.len()].copy_from_slice(&signature);

    let slot = |i: usize| -> [u8; 32] { general[0x20 + i * 32..0x40 + i * 32].try_into().unwrap() };
    let facts = Facts {
        container_size: padded_end as u64,
        mandatory_size: imagedigs_span.0 as u64,
        body_offset: BODY_AT as u64,
        body_size: (padded_end - BODY_AT) as u64,
        body_digest,
        package_digest,
        fih_digest: sha3(p.fih_block),
        content_digest: slot(0),
        header_digest: slot(2),
        system_digest: slot(3),
        param_digest: slot(5),
        entries: named,
    };
    Ok(Container { bytes: cnt, facts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::DEFAULT_PASSCODE;

    // A test fixture: every field of the parameters with defaults for the fixed ones.
    #[allow(clippy::too_many_arguments)]
    fn params<'a>(
        content_id: &'a str,
        param: &'a [u8],
        png: &'a [u8],
        dds: &'a [u8],
        chunk: &'a [u8],
        hash: &'a [u8],
        ficm: &'a [u8],
        digests: &'a [[u8; 32]],
        fih: &'a [u8],
    ) -> CntParams<'a> {
        CntParams {
            content_id,
            param_json: param,
            icon_png: png,
            icon_dds: dds,
            playgo_chunk: chunk,
            playgo_hash_table: hash,
            playgo_ficm: ficm,
            imagedigs: digests,
            game_digest: [3u8; 32],
            fih_block: fih,
            outer_size: 0xB0000,
            cnt_offset: 0xC0000,
            seed: [5u8; 16],
            passcode: DEFAULT_PASSCODE,
            content_type: 0x26,
            drm_type: 0,
            content_flags: 0x0602_0000,
            inner_size: 0x50000,
        }
    }

    #[test]
    fn container_passes_its_own_checks() {
        let id = "UP0000-PPSA01234_00-TESTGAME00000000";
        let param = br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000"}"#;
        let png = vec![0x89u8; 1000];
        let dds = vec![0x44u8; 2000];
        let chunk = crate::si_write::playgo_chunk_dat(id, 0xB0000).unwrap();
        let ficm = crate::si_write::playgo_ficm(10);
        let hash = crate::si_write::playgo_hash_table(5);
        let digests = vec![[9u8; 32]; 11];
        let fih = vec![1u8; crate::BLOCK as usize];
        let cnt = write(&params(
            id, param, &png, &dds, &chunk, &hash, &ficm, &digests, &fih,
        ))
        .unwrap()
        .bytes;
        let parsed = crate::cnt::Cnt::from_bytes(cnt).unwrap();
        assert_eq!(parsed.content_id, id);
        assert_eq!(parsed.entries.len(), 13);
        assert!(parsed.package_digest_ok());
        assert!(parsed.digest_table_digest_ok());
        assert!(parsed.header_rollup_ok());
        assert!(parsed.body_digest_ok());
        assert!(parsed.descriptor_ok());
        assert!(parsed.fih_digest_ok(&fih));
        assert_eq!(
            parsed
                .entry_digests()
                .iter()
                .filter(|(_, v)| *v == crate::cnt::EntryDigest::Mismatch)
                .count(),
            0
        );
        assert_eq!(
            parsed.image_digests().unwrap(),
            digests,
            "imagedigs must come back in natural order"
        );
        let checks = parsed.general_digests(&[3u8; 32]);
        assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
        // The name table resolves every named entry.
        let names = parsed.entry(ids::ENTRY_NAMES).unwrap();
        let table = parsed.payload(names);
        for (i, (id, _, name)) in ENTRIES.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            let entry = parsed.entry(*id).unwrap();
            let at = entry.name_off as usize;
            let end = table[at..].iter().position(|b| *b == 0).unwrap() + at;
            assert_eq!(&table[at..end], name.as_bytes(), "entry {i}");
        }
    }

    #[test]
    fn signature_is_a_public_key_encryption_of_the_header_digest() {
        let id = "UP0000-PPSA01234_00-TESTGAME00000000";
        let digests = vec![[1u8; 32]; 2];
        let fih = vec![0u8; crate::BLOCK as usize];
        let cnt = write(&params(
            id,
            b"{}",
            &[1],
            &[2],
            &[3; 416],
            &[4; 96],
            &[5; 26],
            &digests,
            &fih,
        ))
        .unwrap()
        .bytes;
        let signature = &cnt[SIGNATURE_AT..SIGNATURE_AT + 384];
        // Applying the public exponent recovers the padded block only for a private-key
        // operation; here it must at least not be the zero block and must round-trip
        // through the writer's own inputs.
        assert_ne!(signature, &[0u8; 384]);
        let mut header = cnt[..SIGNATURE_AT].to_vec();
        header[..0x1000].copy_from_slice(&cnt[..0x1000]);
        let expected =
            crate::rsa::pkcs1_encrypt(&keys::METADATA_MODULUS, &sha3(&cnt[..SIGNATURE_AT]));
        assert_eq!(signature, expected.as_slice());
    }
}
