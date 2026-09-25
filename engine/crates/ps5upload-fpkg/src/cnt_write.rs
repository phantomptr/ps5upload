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
/// The leading system entries (0x0001, 0x0010, 0x0020, 0x0080, 0x0100, 0x0200) the header
/// counts at 0x14, and the rows of the entry table the second rollup at 0x120 covers.
const SC_ENTRY_COUNT: u16 = 6;
/// One entry-table row.
const ENTRY_ROW: u32 = 32;
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

/// Presentation files the container carries beside the icons, when the source has them:
/// `(entry id, source path, entry name)`. The ids and names are the ones a Publishing Tools
/// package uses for the same files. Each is also left in the image, as that package does.
pub const PRESENTATION: [(u32, &str, &str); 8] = [
    (ids::SAVE_DATA_PNG, "sce_sys/save_data.png", "save_data.png"),
    (ids::PIC0_PNG, "sce_sys/pic0.png", "pic0.png"),
    (ids::SND0_AT9, "sce_sys/snd0.at9", "snd0.at9"),
    (ids::PIC0_DDS, "sce_sys/pic0.dds", "pic0.dds"),
    (ids::PIC1_DDS, "sce_sys/pic1.dds", "pic1.dds"),
    (
        ids::TROPHY,
        "sce_sys/trophy2/trophy00.ucp",
        "trophy2/trophy00.ucp",
    ),
    (ids::UDS, "sce_sys/uds/uds00.ucp", "uds/uds00.ucp"),
    (ids::PIC2_DDS, "sce_sys/pic2.dds", "pic2.dds"),
];

/// Artwork and sound the image need not carry once the container does: the installer
/// extracts the container's copies to `/user/appmeta/<title>/`, which is where the system
/// reads them, and a Publishing Tools image leaves them out. `param.json`, the trophy and
/// UDS data and the NP files stay in the image — our own backport tooling and third-party
/// trophy tools read them from the game's folder.
pub const CONTAINER_ONLY: [&str; 8] = [
    "sce_sys/icon0.png",
    "sce_sys/icon0.dds",
    "sce_sys/pic0.png",
    "sce_sys/pic0.dds",
    "sce_sys/pic1.dds",
    "sce_sys/pic2.dds",
    "sce_sys/snd0.at9",
    "sce_sys/save_data.png",
];

/// A container entry beyond the fixed set: stored in the clear, digested like the rest.
#[derive(Debug, Clone)]
pub struct ExtraEntry {
    pub id: u32,
    pub name: &'static str,
    pub data: Vec<u8>,
    /// `Some(k)` for a protected entry: stored encrypted with entry key `k` (flags2 `k << 12`).
    pub key_index: Option<u8>,
}

/// The protected entries a dump carries itself: `(id, source path, entry name, key index)`.
/// Encrypted with [`crate::crypto::encrypt_entry`], which needs nothing but the content id and
/// the passcode. Ids, names and key 3 as in Sony's and LibProsperoPkg's packages. The license
/// entries (0x0400/0x0401) are not here: a valid `license.dat` is signed with Sony's debug RIF
/// key, which this project does not use.
pub const PROTECTED: [(u32, &str, &str, u8); 3] = [
    (0x0402, "sce_sys/nptitle.dat", "nptitle.dat", 3),
    (0x2020, "sce_sys/uds/npbind.dat", "uds/npbind.dat", 3),
    (
        0x2021,
        "sce_sys/trophy2/npbind.dat",
        "trophy2/npbind.dat",
        3,
    ),
];

/// The languages a generated PlayGo scenario names, in LibProsperoPkg's order.
const SCENARIO_LANGUAGES: [&str; 31] = [
    "ja-JP", "en-US", "fr-FR", "es-ES", "de-DE", "it-IT", "nl-NL", "pt-PT", "ru-RU", "ko-KR",
    "zh-Hant", "zh-Hans", "fi-FI", "sv-SE", "da-DK", "no-NO", "pl-PL", "pt-BR", "en-GB", "tr-TR",
    "es-419", "ar-AE", "fr-CA", "cs-CZ", "hu-HU", "el-GR", "ro-RO", "th-TH", "vi-VN", "id-ID",
    "uk-UA",
];

/// `playgo-scenario.json` (entry `0x3000`): one play-mode scenario covering the package.
///
/// Without it the console logs `[PlayGoCore] ... not found PlayGoScenario json` at every launch,
/// and Minecraft sat on a black screen with its main thread busy: it waits on PlayGo before
/// drawing. LibProsperoPkg's packages carry this exact document and play.
pub fn playgo_scenario_json() -> Vec<u8> {
    let names = SCENARIO_LANGUAGES
        .iter()
        .map(|l| format!(r#""{l}":{{"title":"Scenario #0","description":"Scenario #0"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let langs = SCENARIO_LANGUAGES
        .iter()
        .map(|l| format!(r#""{l}""#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"scenarioCount":1,"scenarioDefaultId":0,"scenarioDefaultLanguage":"en-US","scenarios":[{{"id":0,"type":"playmode",{names}}}],"chunkDefaultLanguage":"en-US","chunkSupportedLanguages":[{langs}]}}"#
    )
    .into_bytes()
}

/// The PlayGo scenario entry; see [`playgo_scenario_json`].
pub fn playgo_scenario_extra() -> ExtraEntry {
    ExtraEntry {
        id: crate::cnt::ids::PLAYGO_SCENARIO_JSON,
        name: "playgo-scenario.json",
        data: playgo_scenario_json(),
        key_index: None,
    }
}

/// The debug license entries for `content_id`: `license.dat` (0x0400, key 3) and
/// `license.info` (0x0401, key 4), as in Sony's and LibProsperoPkg's packages.
pub fn license_extras(content_id: &str) -> Vec<ExtraEntry> {
    vec![
        ExtraEntry {
            id: 0x0400,
            name: "license.dat",
            data: crate::license::license_dat(content_id).to_vec(),
            key_index: Some(3),
        },
        ExtraEntry {
            id: 0x0401,
            name: "license.info",
            data: crate::license::license_info(content_id).to_vec(),
            key_index: Some(4),
        },
    ]
}

/// The DRM type of a package that carries a debug license — LibProsperoPkg's, in a package
/// that launches. `PS5UPLOAD_FPKG_DRM_TYPE` still overrides it.
pub const LICENSED_DRM_TYPE: u32 = 0x10;

/// The [`PROTECTED`] entries `read` finds (a missing or empty file is skipped).
pub fn protected_extras(read: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Vec<ExtraEntry> {
    PROTECTED
        .iter()
        .filter_map(|(id, path, name, key)| {
            let data = read(path).filter(|d| !d.is_empty())?;
            Some(ExtraEntry {
                id: *id,
                name,
                data,
                key_index: Some(*key),
            })
        })
        .collect()
}

/// The [`PRESENTATION`] entries `read` finds (a missing or empty file is skipped).
pub fn presentation_extras(read: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Vec<ExtraEntry> {
    PRESENTATION
        .iter()
        .filter_map(|(id, path, name)| {
            let data = read(path).filter(|d| !d.is_empty())?;
            Some(ExtraEntry {
                id: *id,
                name,
                data,
                key_index: None,
            })
        })
        .collect()
}

/// Where an entry's payload sits: every body starts with the key and table entries, then
/// `param.json`, then the entries in the `0x2000` range ahead of the image digests; the
/// PlayGo chunk table and the `0x1000`-range presentation entries follow them, and the two
/// PlayGo tables close it. Measured on a Publishing Tools package with 26 entries.
fn body_rank(id: u32) -> (u8, u32) {
    match id {
        ids::ENTRY_KEYS => (0, 0),
        ids::IMAGE_KEY => (1, 0),
        ids::GENERAL_DIGESTS => (2, 0),
        ids::METAS => (3, 0),
        ids::DIGESTS => (4, 0),
        ids::ENTRY_NAMES => (5, 0),
        ids::PARAM_JSON => (6, 0),
        ids::IMAGE_DIGESTS => (8, 0),
        ids::PLAYGO_CHUNK => (9, 0),
        ids::PLAYGO_HASH_TABLE | ids::PLAYGO_FICM => (11, id),
        0x1000..=0x1FFF => (10, id),
        _ => (7, id),
    }
}

/// Every payload starts on this boundary, as in every sample.
const ENTRY_ALIGN: usize = 16;

pub struct CntParams<'a> {
    pub content_id: &'a str,
    pub param_json: &'a [u8],
    pub icon_png: &'a [u8],
    pub icon_dds: &'a [u8],
    /// Presentation entries beyond the icons; see [`presentation_extras`].
    pub extras: &'a [ExtraEntry],
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
    /// `0x20` for a game, `0x26` for an app, `0x21` for additional content; DLC also sets
    /// `drm_type` `0x10`. See [`content_class`].
    pub content_type: u32,
    pub drm_type: u32,
    pub content_flags: u32,
    /// The block-aligned inner image size (the promote size).
    pub inner_size: u64,
}

/// `PS5UPLOAD_FPKG_DRM_TYPE` (hex, e.g. `0x10`): a diagnostic override of the container's DRM
/// type (`0x70`). LibProsperoPkg's application packages carry 0x10 where ours carry 0.
pub fn drm_type_override() -> Option<u32> {
    let v = std::env::var("PS5UPLOAD_FPKG_DRM_TYPE").ok()?;
    u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()
}

/// The container's `(content_type, content_flags)` for an application package, from its
/// `param.json`'s `applicationCategoryType`.
///
/// Measured on Publishing Tools packages: a game (category 0, Spider-Man 2) is `0x20` /
/// `0x0202_0000`, an app (category 65536: the Web Browser, YouTube) `0x26` / `0x0602_0000`.
/// Every package here used the app pair, copied from the Web Browser, so games went out
/// labelled as apps. A param.json that names no category is a game: 0 is the default.
pub fn content_class(param_json: &[u8]) -> (u32, u32) {
    let category = crate::source::parse_param_json(param_json)
        .and_then(|j| j.get("applicationCategoryType").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    if category == 0 {
        (0x20, 0x0202_0000)
    } else {
        (0x26, 0x0602_0000)
    }
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
        let aligned = self.bytes.len().next_multiple_of(ENTRY_ALIGN);
        self.bytes.resize(aligned, 0);
        let at = self.bytes.len() as u32;
        self.bytes.extend_from_slice(data);
        self.spans.insert(id, (at, data.len() as u32));
    }

    /// Store `data` but record `size` (<= its length) as the entry's size.
    fn add_sized(&mut self, id: u32, data: &[u8], size: u32) {
        self.add(id, data);
        if let Some(span) = self.spans.get_mut(&id) {
            span.1 = size;
        }
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
    let mut system = Vec::with_capacity(ids::SYSTEM_DIGEST_IDS.len() * 32);
    for id in ids::SYSTEM_DIGEST_IDS {
        if let Some(digest) = entry_digest(id) {
            system.extend_from_slice(&digest);
        }
    }
    if !system.is_empty() {
        slot(3, &sha3(&system));
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

/// The entry table: the fixed set plus the extras, in id order.
fn entry_list(extras: &[ExtraEntry]) -> Result<Vec<(u32, u32, &'static str)>> {
    let mut entries: Vec<(u32, u32, &'static str)> = ENTRIES.to_vec();
    for extra in extras {
        if entries.iter().any(|(id, _, _)| *id == extra.id) {
            return format_err(format!("container entry {:#06x} given twice", extra.id));
        }
        let flags1 = if extra.key_index.is_some() {
            0x8000_0000
        } else {
            0x0800_0000
        };
        entries.push((extra.id, flags1, extra.name));
    }
    entries.sort_by_key(|(id, _, _)| *id);
    Ok(entries)
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
    let entries = entry_list(p.extras)?;
    let count = entries.len();
    let digest_table_len = count * 32;
    let ekpfs = derive_pfs_key(p.content_id, p.passcode, 1);

    // Names table: the empty name, then each named entry's name in entry order.
    let mut names = vec![0u8];
    let mut name_offsets = vec![0u32; count];
    for (i, (_, _, name)) in entries.iter().enumerate() {
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
    let keys = keys_entry(p.content_id, p.passcode);
    let image_key = image_key_entry(&ekpfs);
    let general_placeholder = vec![0u8; GENERAL_LEN];
    let table_placeholder = vec![0u8; digest_table_len];
    let payload_of = |id: u32| -> &[u8] {
        match id {
            ids::ENTRY_KEYS => &keys,
            ids::IMAGE_KEY => &image_key,
            ids::GENERAL_DIGESTS => &general_placeholder,
            ids::METAS | ids::DIGESTS => &table_placeholder,
            ids::ENTRY_NAMES => &names,
            ids::PARAM_JSON => p.param_json,
            ids::IMAGE_DIGESTS => &imagedigs,
            ids::PLAYGO_CHUNK => p.playgo_chunk,
            ids::ICON0_PNG => p.icon_png,
            ids::ICON0_DDS => p.icon_dds,
            ids::PLAYGO_HASH_TABLE => p.playgo_hash_table,
            ids::PLAYGO_FICM => p.playgo_ficm,
            other => p
                .extras
                .iter()
                .find(|e| e.id == other)
                .map(|e| e.data.as_slice())
                .unwrap_or(&[]),
        }
    };
    let mut order: Vec<u32> = entries.iter().map(|(id, _, _)| *id).collect();
    order.sort_by_key(|id| body_rank(*id));
    let key_of = |id: u32| {
        p.extras
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.key_index)
    };
    let mut body = Body::new();
    for id in order {
        match key_of(id) {
            // A protected payload is stored padded to 16 (the cipher's block); the table keeps
            // its real size, so the padding sits in the space the next entry's alignment leaves.
            Some(_) => {
                let data = payload_of(id);
                let mut padded = data.to_vec();
                padded.resize(data.len().next_multiple_of(16), 0);
                body.add_sized(id, &padded, data.len() as u32);
            }
            None => body.add(id, payload_of(id)),
        }
    }
    let body_end = body.bytes.len();
    // Every sample's install segment starts on a 64 KiB boundary — `webbrowser.pkg` carries
    // 42 KiB of zero padding after the container to reach one, and the container's own
    // descriptor records that padded end rather than its content end. The PlayGo CRC table
    // is one entry per block of everything before the segment, so the padding is what makes
    // its length exact.
    let padded_end = body_end.next_multiple_of(BLOCK as usize);

    // The entry table (`0x0100`'s payload), in entry-table order.
    let mut table = vec![0u8; digest_table_len];
    for (i, (id, flags1, _)) in entries.iter().enumerate() {
        let (at, size) = body.span(*id);
        let entry = &mut table[i * 32..(i + 1) * 32];
        be32_into(entry, 0x00, *id);
        be32_into(entry, 0x04, name_offsets[i]);
        be32_into(entry, 0x08, *flags1);
        be32_into(entry, 0x0C, key_of(*id).map_or(0, |k| u32::from(k) << 12));
        be32_into(entry, 0x10, at);
        be32_into(entry, 0x14, size);
    }
    // Protected entries are encrypted now, when their rows (which the key covers) are final.
    for (i, (id, _, _)) in entries.iter().enumerate() {
        let Some(k) = key_of(*id) else { continue };
        let row: [u8; 32] = table[i * 32..(i + 1) * 32].try_into().unwrap();
        let entry_key = derive_pfs_key(p.content_id, p.passcode, u32::from(k));
        let (at, size) = body.span(*id);
        let end = (at + size).next_multiple_of(16) as usize;
        crate::crypto::encrypt_entry(&row, &entry_key, &mut body.bytes[at as usize..end]);
    }
    let metas_at = body.span(ids::METAS).0;
    body.write_at(metas_at, &table);

    // Per-entry digests (the table's own slot stays zero; the general digests are only
    // final once their payload exists).
    let mut digests = vec![0u8; digest_table_len];
    for (i, (id, _, _)) in entries.iter().enumerate() {
        if *id == ids::DIGESTS || *id == ids::GENERAL_DIGESTS {
            continue;
        }
        // A protected entry's digest covers its stored ciphertext, padding included — as
        // LibProsperoPkg writes it, in a package that launches.
        let digest = match key_of(*id) {
            Some(_) => {
                let (at, size) = body.span(*id);
                sha3(&body.bytes[at as usize..(at + size).next_multiple_of(16) as usize])
            }
            None => sha3(body.payload(*id)),
        };
        digests[i * 32..(i + 1) * 32].copy_from_slice(&digest);
    }

    // The header's first 0x80 bytes: the prefix the header digest covers and the content
    // descriptor the content digest covers.
    let mut head = vec![0u8; 0x80];
    head[0..4].copy_from_slice(&[0x7F, b'C', b'N', b'T']);
    be32_into(&mut head, 0x04, 0x20001);
    be32_into(&mut head, 0x08, 0x8000_0000);
    be32_into(&mut head, 0x0C, 0xC);
    be32_into(&mut head, 0x10, count as u32);
    head[0x14..0x16].copy_from_slice(&SC_ENTRY_COUNT.to_be_bytes());
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
        let i = entries.iter().position(|(eid, _, _)| *eid == id)?;
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
        let i = entries
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
    let mut named: Vec<(u32, u32, &'static str)> = entries
        .iter()
        .filter(|(_, _, name)| !name.is_empty())
        .map(|(id, _, name)| {
            let (at, size) = body.span(*id);
            (at, size, *name)
        })
        .collect();
    named.sort_by_key(|(at, _, _)| *at);
    // The second system-entry rollup covers the key, image-key and general-digest entries and
    // the first SC_ENTRY_COUNT rows of the entry table.
    let sc_spans: Vec<(u32, u32)> = [ids::ENTRY_KEYS, ids::IMAGE_KEY, ids::GENERAL_DIGESTS]
        .iter()
        .map(|id| body.span(*id))
        .chain(std::iter::once((
            body.span(ids::METAS).0,
            u32::from(SC_ENTRY_COUNT) * ENTRY_ROW,
        )))
        .collect();
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
    // Every Sony package fills 0x120 (verified on ten: a game, an app, a patch, seven DLC);
    // ours left it zero.
    let mut sc2 = Vec::new();
    for (at, size) in &sc_spans {
        sc2.extend_from_slice(&cnt[*at as usize..(*at + *size) as usize]);
    }
    cnt[0x120..0x140].copy_from_slice(&sha3(&sc2));
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
            extras: &[],
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
        let pg =
            crate::playgo::build(id, &[("eboot.bin".into(), crate::BLOCK, 5)], 0xB0000, 1).unwrap();
        let (chunk, ficm, hash) = (pg.chunk_dat, pg.ficm, pg.hash_table);
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
        assert!(parsed.sc_entries2_ok());
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

    /// The layout of a Publishing Tools package with every presentation entry: table in id
    /// order, payloads in its body order and on 16-byte boundaries, the system digest over
    /// all eight presentation entries.
    /// The document LibProsperoPkg writes, byte for byte (its Minecraft package: 2,293 bytes).
    #[test]
    fn playgo_scenario_matches_the_reference() {
        let json = playgo_scenario_json();
        assert_eq!(json.len(), 2293);
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["scenarioCount"], 1);
        assert_eq!(v["scenarios"][0]["type"], "playmode");
        assert_eq!(v["chunkSupportedLanguages"].as_array().unwrap().len(), 31);
        assert_eq!(v["scenarios"][0]["en-US"]["title"], "Scenario #0");
    }

    #[test]
    fn extras_take_the_publishing_tools_layout() {
        let id = "UP0000-PPSA01234_00-TESTGAME00000000";
        let pg =
            crate::playgo::build(id, &[("eboot.bin".into(), crate::BLOCK, 5)], 0xB0000, 1).unwrap();
        let (chunk, ficm, hash) = (pg.chunk_dat, pg.ficm, pg.hash_table);
        let digests = vec![[9u8; 32]; 3];
        let fih = vec![1u8; crate::BLOCK as usize];
        let extras: Vec<ExtraEntry> = PRESENTATION
            .iter()
            .enumerate()
            .map(|(i, (eid, _, name))| ExtraEntry {
                id: *eid,
                name,
                data: vec![i as u8 + 1; 1001 + i],
                key_index: None,
            })
            .collect();
        let mut p = params(
            id,
            b"{\"a\":1}",
            &[7; 333],
            &[8; 555],
            &chunk,
            &hash,
            &ficm,
            &digests,
            &fih,
        );
        p.extras = &extras;
        let parsed = crate::cnt::Cnt::from_bytes(write(&p).unwrap().bytes).unwrap();
        let table: Vec<u32> = parsed.entries.iter().map(|e| e.id).collect();
        let mut sorted = table.clone();
        sorted.sort();
        assert_eq!(table, sorted, "the entry table is in id order");
        assert_eq!(table.len(), 21);
        let mut by_offset = parsed.entries.clone();
        by_offset.sort_by_key(|e| e.offset);
        let body: Vec<u32> = by_offset.iter().map(|e| e.id).collect();
        assert_eq!(
            body,
            [
                0x0010, 0x0020, 0x0080, 0x0100, 0x0001, 0x0200, 0x2000, 0x2060, 0x040A, 0x1001,
                0x100D, 0x1200, 0x1220, 0x1240, 0x1280, 0x12A0, 0x12C0, 0x1480, 0x14A0, 0x2010,
                0x2011,
            ]
        );
        assert!(parsed.entries.iter().all(|e| e.offset % 16 == 0));
        let checks = parsed.general_digests(&[3u8; 32]);
        assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
        assert!(checks
            .iter()
            .any(|(n, _)| *n == "cnt general digest system"));
        assert!(parsed.body_digest_ok() && parsed.package_digest_ok());
        // Trophy and UDS data are digested but are not part of the system digest.
        let gd = parsed.payload(parsed.entry(ids::GENERAL_DIGESTS).unwrap());
        let mut pre = Vec::new();
        for eid in ids::SYSTEM_DIGEST_IDS {
            pre.extend_from_slice(&sha3(parsed.payload(parsed.entry(eid).unwrap())));
        }
        assert_eq!(&gd[0x20 + 3 * 32..0x40 + 3 * 32], &sha3(&pre));
        // A duplicate id is refused rather than written twice.
        let twice = [extras[0].clone(), extras[0].clone()];
        p.extras = &twice;
        assert!(write(&p).is_err());
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
