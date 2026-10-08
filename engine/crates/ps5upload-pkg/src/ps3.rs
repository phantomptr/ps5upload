//! PS3 NPDRM packages (`\x7FPKG`, type 1): content ID, title, version, category and region,
//! read from the header and the PARAM.SFO inside.
//!
//! Ported from PS Game Library's `pspkg.py`, whose parsing derives from PKG Viewer (MIT). The
//! data stream is encrypted: a debug package with a SHA-1 keystream over its QA digest, a retail
//! one with AES-128 in counter mode under the NPDRM package key, published in PKG Viewer and
//! every PS3 package tool. Only the item table, item names and PARAM.SFO are decrypted, never
//! game data.

use std::io::{Read, Seek, SeekFrom};

use crate::kind::{classify_category, region_of_content_id, region_of_ps3_title_id, Kind};
use crate::{sfo_params, SfoValue};

/// `\x7FPKG`, the PS3 and PSP package magic.
pub const PS3_MAGIC: [u8; 4] = [0x7F, b'P', b'K', b'G'];
const PS3_KEY: [u8; 16] = [
    0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28, 0xb8, 0xf8,
];

/// What a PS3 package says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ps3Package {
    pub content_id: String,
    pub title_id: String,
    pub title: String,
    pub version: String,
    pub category: String,
    pub region: String,
    pub kind: Kind,
    pub kind_confident: bool,
    pub kind_reason: String,
    /// The file is at least as long as the header declares.
    pub complete: bool,
    pub retail: bool,
}

struct Header {
    retail: bool,
    item_count: u32,
    total_size: u64,
    data_off: u64,
    data_size: u64,
    content_id: String,
    qa_digest: [u8; 16],
    riv: [u8; 16],
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

fn header(h: &[u8; 128]) -> Result<Header, String> {
    if h[..4] != PS3_MAGIC {
        return Err("not a PS3 package".into());
    }
    let rev = be16(h, 4);
    let typ = be16(h, 6);
    if typ != 1 {
        return Err(format!("not PS3 NPDRM (type {typ:#x})"));
    }
    let cid = &h[0x30..0x60];
    let end = cid.iter().position(|&b| b == 0).unwrap_or(cid.len());
    Ok(Header {
        retail: rev == 0x8000,
        item_count: be32(h, 0x14),
        total_size: be64(h, 0x18),
        data_off: be64(h, 0x20),
        data_size: be64(h, 0x28),
        content_id: String::from_utf8_lossy(&cid[..end]).into_owned(),
        qa_digest: h[0x60..0x70].try_into().unwrap(),
        riv: h[0x70..0x80].try_into().unwrap(),
    })
}

/// The keystream block for 16-byte block `index` of the data stream.
fn keystream(h: &Header, index: u64) -> [u8; 16] {
    if h.retail {
        use aes::cipher::{BlockCipherEncrypt, KeyInit};
        let ctr = u128::from_be_bytes(h.riv).wrapping_add(u128::from(index));
        let mut blk = aes::Block::from(ctr.to_be_bytes());
        aes::Aes128::new(&PS3_KEY.into()).encrypt_block(&mut blk);
        blk.into()
    } else {
        use sha1::{Digest, Sha1};
        let qa = h.qa_digest;
        let mut buf = [0u8; 64];
        buf[0..8].copy_from_slice(&qa[..8]);
        buf[8..16].copy_from_slice(&qa[..8]);
        buf[16..24].copy_from_slice(&qa[8..16]);
        buf[24..32].copy_from_slice(&qa[8..16]);
        buf[56..64].copy_from_slice(&index.to_be_bytes());
        let d = Sha1::digest(buf);
        d[..16].try_into().unwrap()
    }
}

/// `size` decrypted bytes at `pos` in the data stream (relative to `data_off`).
fn decrypt<R: Read + Seek>(r: &mut R, h: &Header, pos: u64, size: u64) -> Option<Vec<u8>> {
    if size == 0 || size > 16 << 20 {
        return None;
    }
    let start = pos & !0xF;
    let pre = (pos - start) as usize;
    let blocks = (pre as u64 + size).div_ceil(16);
    r.seek(SeekFrom::Start(h.data_off.checked_add(start)?))
        .ok()?;
    let mut enc = vec![0u8; (blocks * 16) as usize];
    r.read_exact(&mut enc).ok()?;
    for (i, chunk) in enc.chunks_mut(16).enumerate() {
        let ks = keystream(h, start / 16 + i as u64);
        for (b, k) in chunk.iter_mut().zip(ks) {
            *b ^= k;
        }
    }
    Some(enc[pre..pre + size as usize].to_vec())
}

fn text(params: &[(String, SfoValue)], key: &str) -> String {
    params
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| match v {
            SfoValue::Text(s) => Some(s.trim_end_matches('\0').trim().to_string()),
            SfoValue::Int(_) => None,
        })
        .unwrap_or_default()
}

/// The unencrypted metadata after the header (RPCS3's `PKGMetaData`): content type, package
/// flags, and the software revision, whose last two bytes are the app version in BCD.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Meta {
    content_type: Option<u32>,
    flags: Option<u32>,
    /// `02.10` as `0x0210`.
    app_version: Option<u16>,
}

/// `PKG_FLAG_PATCH` in RPCS3's `unpkg.h`: the package is a game update.
const FLAG_PATCH: u32 = 0x10;
const CONTENT_GAME_DATA: u32 = 0x04;
const CONTENT_GAME_EXEC: u32 = 0x05;

fn read_meta<R: Read + Seek>(r: &mut R, raw: &[u8; 128]) -> Meta {
    let (off, count) = (u64::from(be32(raw, 8)), be32(raw, 12));
    let mut m = Meta::default();
    if off < 0x80 || count == 0 || count > 64 {
        return m;
    }
    let mut buf = vec![0u8; 2048];
    if r.seek(SeekFrom::Start(off)).is_err() {
        return m;
    }
    let n = r.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    let mut at = 0usize;
    for _ in 0..count {
        if at + 8 > buf.len() {
            break;
        }
        let (id, size) = (be32(&buf, at), be32(&buf, at + 4) as usize);
        let d = at + 8;
        if d + size > buf.len() {
            break;
        }
        match (id, size) {
            (0x2, 4) => m.content_type = Some(be32(&buf, d)),
            (0x3, 4) => m.flags = Some(be32(&buf, d)),
            (0x8, 8) => m.app_version = Some(be16(&buf, d + 6)),
            _ => {}
        }
        at = d + size;
    }
    m
}

/// `0x0210` → `02.10`, when it is a BCD version at all.
fn bcd_version(v: u16) -> Option<String> {
    let s = format!("{v:04x}");
    (v != 0 && s.bytes().all(|b| b.is_ascii_digit())).then(|| format!("{}.{}", &s[..2], &s[2..]))
}

/// Base, update or DLC. The package's own flags decide first (an update of a PSN game keeps
/// the game's CATEGORY, HG, so the SFO alone calls it a game); the SFO category is the fallback.
fn ps3_kind(meta: &Meta, category: &str) -> (Kind, bool, String) {
    if let Some(f) = meta.flags {
        if f & FLAG_PATCH != 0 {
            return (
                Kind::Patch,
                true,
                format!("package flags {f:#x} carry the patch bit ({FLAG_PATCH:#x})"),
            );
        }
        match meta.content_type {
            Some(CONTENT_GAME_EXEC) => {
                return (
                    Kind::Base,
                    true,
                    format!("content type {CONTENT_GAME_EXEC:#04x} (game), no patch bit"),
                )
            }
            Some(CONTENT_GAME_DATA) => {
                return (
                    Kind::Dlc,
                    false,
                    format!(
                        "content type {CONTENT_GAME_DATA:#04x} (game data) without the patch bit: add-on data"
                    ),
                )
            }
            _ => {}
        }
    }
    let (kind, confident, reason) = classify_category(category);
    (
        kind,
        confident,
        if confident {
            format!("param.sfo {reason}")
        } else {
            reason
        },
    )
}

/// Reads a PS3 package. `size` is the file's length (for "complete").
pub fn parse_ps3<R: Read + Seek>(r: &mut R, size: u64) -> Result<Ps3Package, String> {
    let mut raw = [0u8; 128];
    r.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    r.read_exact(&mut raw)
        .map_err(|_| "too small for a PS3 package".to_string())?;
    let h = header(&raw)?;
    let meta = read_meta(r, &raw);
    let mut params: Vec<(String, SfoValue)> = Vec::new();
    let n = u64::from(h.item_count);
    if n > 0 && n < 1_000_000 && h.data_off > 0 && h.data_size > 0 {
        if let Some(table) = decrypt(r, &h, 0, n * 32) {
            for i in 0..n as usize {
                let e = &table[i * 32..i * 32 + 32];
                let (name_off, name_size) = (u64::from(be32(e, 0)), u64::from(be32(e, 4)));
                let (file_off, file_size) = (be64(e, 8), be64(e, 16));
                let ds = h.data_size;
                if name_size == 0
                    || name_size > ds
                    || name_off + name_size > ds
                    || file_off.saturating_add(file_size) > ds
                {
                    continue;
                }
                let Some(name) = decrypt(r, &h, name_off, name_size) else {
                    continue;
                };
                let name = String::from_utf8_lossy(&name)
                    .trim_end_matches('\0')
                    .to_string();
                if name.to_ascii_uppercase().ends_with("PARAM.SFO")
                    && file_size > 0
                    && file_size < 1_000_000
                {
                    if let Some(sfo) = decrypt(r, &h, file_off, file_size) {
                        if sfo.starts_with(b"\0PSF") {
                            params = sfo_params(&sfo).unwrap_or_default();
                        }
                    }
                    break;
                }
            }
        }
    }
    let title_id = {
        let t = text(&params, "TITLE_ID");
        if t.is_empty() {
            crate::kind::title_id_of_content_id(&h.content_id)
        } else {
            t
        }
    };
    let region = if h.content_id.contains('-') {
        region_of_content_id(&h.content_id)
    } else {
        region_of_ps3_title_id(&title_id)
    };
    let category = text(&params, "CATEGORY");
    let (kind, kind_confident, kind_reason) = ps3_kind(&meta, &category);
    // The app version: the header's (an update's own version), else the SFO's APP_VER, else
    // its VERSION (which is the PARAM.SFO format's, 01.00 on every update).
    let version = {
        let v = meta.app_version.and_then(bcd_version).unwrap_or_else(|| {
            let a = text(&params, "APP_VER");
            if a.is_empty() {
                text(&params, "VERSION")
            } else {
                a
            }
        });
        crate::kind::normalize_version(&v)
    };
    Ok(Ps3Package {
        title: text(&params, "TITLE"),
        content_id: h.content_id,
        title_id,
        version,
        category,
        region,
        kind,
        kind_confident,
        kind_reason,
        complete: h.total_size == 0 || size >= h.total_size,
        retail: h.retail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A tiny debug package: header, a one-item table, the name and a PARAM.SFO, all encrypted
    /// with the debug keystream the reader has to undo.
    fn build_debug_pkg(sfo: &[u8]) -> Vec<u8> {
        build_debug_pkg_meta(sfo, None)
    }

    /// With header metadata `(content_type, flags, app_version)`, at 0x80 as real packages
    /// carry it after the header.
    fn build_debug_pkg_meta(sfo: &[u8], meta: Option<(u32, u32, u16)>) -> Vec<u8> {
        let data_off = 0xC0u64;
        let name = b"PARAM.SFO\0\0\0\0\0\0\0";
        let table_len = 32u64;
        let name_off = table_len;
        let file_off = name_off + name.len() as u64;
        let mut plain = vec![0u8; table_len as usize];
        plain[0..4].copy_from_slice(&(name_off as u32).to_be_bytes());
        plain[4..8].copy_from_slice(&(9u32).to_be_bytes());
        plain[8..16].copy_from_slice(&file_off.to_be_bytes());
        plain[16..24].copy_from_slice(&(sfo.len() as u64).to_be_bytes());
        plain.extend_from_slice(name);
        plain.extend_from_slice(sfo);
        while !plain.len().is_multiple_of(16) {
            plain.push(0);
        }
        let mut hdr = [0u8; 128];
        hdr[..4].copy_from_slice(&PS3_MAGIC);
        hdr[4..6].copy_from_slice(&0x0000u16.to_be_bytes()); // debug
        hdr[6..8].copy_from_slice(&1u16.to_be_bytes());
        hdr[0x14..0x18].copy_from_slice(&1u32.to_be_bytes());
        let total = data_off + plain.len() as u64;
        hdr[0x18..0x20].copy_from_slice(&total.to_be_bytes());
        hdr[0x20..0x28].copy_from_slice(&data_off.to_be_bytes());
        hdr[0x28..0x30].copy_from_slice(&(plain.len() as u64).to_be_bytes());
        let cid = b"UP0001-NPUB30001_00-TESTGAME00000000";
        hdr[0x30..0x30 + cid.len()].copy_from_slice(cid);
        hdr[0x60..0x70].copy_from_slice(&[7u8; 16]);
        let h = header(&hdr).unwrap();
        for (i, chunk) in plain.chunks_mut(16).enumerate() {
            for (b, k) in chunk.iter_mut().zip(keystream(&h, i as u64)) {
                *b ^= k;
            }
        }
        let mut out = hdr.to_vec();
        out.resize(data_off as usize, 0);
        if let Some((ct, flags, app)) = meta {
            let mut m = Vec::new();
            for (id, v) in [(2u32, ct), (3, flags)] {
                m.extend_from_slice(&id.to_be_bytes());
                m.extend_from_slice(&4u32.to_be_bytes());
                m.extend_from_slice(&v.to_be_bytes());
            }
            m.extend_from_slice(&8u32.to_be_bytes());
            m.extend_from_slice(&8u32.to_be_bytes());
            m.extend_from_slice(&[0x81, 0x02, 0x50, 0x00, 0x01, 0x00]);
            m.extend_from_slice(&app.to_be_bytes());
            out[0x80..0x80 + m.len()].copy_from_slice(&m);
            out[8..12].copy_from_slice(&0x80u32.to_be_bytes());
            out[12..16].copy_from_slice(&3u32.to_be_bytes());
        }
        out.extend_from_slice(&plain);
        out
    }

    /// Measured on Sony's own update packages for flOw (NPUA80001 2.10) and LittleBigPlanet 2
    /// (NPUA80662 1.33): flags 0x5e, content type 0x05, CATEGORY HG, VERSION 01.00.
    #[test]
    fn an_update_of_a_psn_game_is_an_update_at_its_app_version() {
        let pkg = build_debug_pkg_meta(
            &sfo(&[("CATEGORY", "HG"), ("TITLE", "flOw"), ("VERSION", "01.00")]),
            Some((0x05, 0x5e, 0x0210)),
        );
        let size = pkg.len() as u64;
        let p = parse_ps3(&mut Cursor::new(pkg), size).unwrap();
        assert_eq!((p.kind, p.kind_confident), (Kind::Patch, true));
        assert_eq!(p.version, "2.10");
        assert!(p.kind_reason.contains("patch bit"), "{}", p.kind_reason);
    }

    #[test]
    fn a_game_without_the_patch_bit_is_the_game_and_game_data_is_an_add_on() {
        let game = build_debug_pkg_meta(&sfo(&[("CATEGORY", "HG")]), Some((0x05, 0x4e, 0x0100)));
        let n = game.len() as u64;
        let p = parse_ps3(&mut Cursor::new(game), n).unwrap();
        assert_eq!((p.kind, p.kind_confident), (Kind::Base, true));
        assert_eq!(p.version, "1.00");
        let data = build_debug_pkg_meta(&sfo(&[("CATEGORY", "GD")]), Some((0x04, 0x0e, 0)));
        let n = data.len() as u64;
        let p = parse_ps3(&mut Cursor::new(data), n).unwrap();
        assert_eq!((p.kind, p.kind_confident), (Kind::Dlc, false));
    }

    fn sfo(entries: &[(&str, &str)]) -> Vec<u8> {
        // Minimal PSF: header, index entries, key table, data table (UTF-8 strings).
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (k, v) in entries {
            let ko = keys.len() as u16;
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
            let mut val = v.as_bytes().to_vec();
            val.push(0);
            while !val.len().is_multiple_of(4) {
                val.push(0);
            }
            let doff = data.len() as u32;
            index.extend_from_slice(&ko.to_le_bytes());
            index.extend_from_slice(&0x0204u16.to_le_bytes());
            index.extend_from_slice(&((v.len() + 1) as u32).to_le_bytes());
            index.extend_from_slice(&(val.len() as u32).to_le_bytes());
            index.extend_from_slice(&doff.to_le_bytes());
            data.extend_from_slice(&val);
        }
        while !keys.len().is_multiple_of(4) {
            keys.push(0);
        }
        let key_off = 0x14 + index.len() as u32;
        let data_off = key_off + keys.len() as u32;
        let mut out = b"\0PSF".to_vec();
        out.extend_from_slice(&0x0101u32.to_le_bytes());
        out.extend_from_slice(&key_off.to_le_bytes());
        out.extend_from_slice(&data_off.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&keys);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn a_debug_package_reads_its_param_sfo() {
        let pkg = build_debug_pkg(&sfo(&[
            ("CATEGORY", "GD"),
            ("TITLE", "Test Game"),
            ("TITLE_ID", "NPUB30001"),
            ("VERSION", "01.00"),
        ]));
        let size = pkg.len() as u64;
        let p = parse_ps3(&mut Cursor::new(pkg), size).unwrap();
        assert_eq!(p.title, "Test Game");
        assert_eq!(p.title_id, "NPUB30001");
        assert_eq!(p.content_id, "UP0001-NPUB30001_00-TESTGAME00000000");
        assert_eq!(p.version, "1.00");
        assert_eq!(p.region, "Americas");
        assert_eq!((p.kind, p.kind_confident), (Kind::Base, true));
        assert_eq!(p.kind_reason, "param.sfo CATEGORY=GD");
        assert!(p.complete && !p.retail);
    }

    #[test]
    fn a_truncated_package_is_incomplete_and_still_named() {
        let mut pkg = build_debug_pkg(&sfo(&[("CATEGORY", "AC"), ("TITLE", "DLC")]));
        let size = pkg.len() as u64 - 4;
        pkg.truncate(size as usize);
        let p = parse_ps3(&mut Cursor::new(pkg), size).unwrap();
        assert!(!p.complete);
        assert_eq!(
            p.title_id, "NPUB30001",
            "from the content ID when the SFO cannot say"
        );
    }

    #[test]
    fn other_package_types_are_refused() {
        let mut hdr = vec![0u8; 128];
        hdr[..4].copy_from_slice(&PS3_MAGIC);
        hdr[6..8].copy_from_slice(&2u16.to_be_bytes());
        assert!(parse_ps3(&mut Cursor::new(hdr), 128)
            .unwrap_err()
            .contains("type 0x2"));
    }
}
