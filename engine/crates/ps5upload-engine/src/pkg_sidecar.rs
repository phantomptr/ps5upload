//! Files the PS5 installer fetches beside a streamed package.
//!
//! While installing over HTTP the console may request `<content-id>.crc` from
//! the same directory as the package. That file is the package's
//! `config/<content-id>/playgo-chunk.crc`: one little-endian CRC-32C per 64 KiB
//! of the finalized image. Debug FPKGs carry it in the STORED ZIP (the "SI"
//! segment) at the very end of the package, so it can be served straight from
//! the package bytes. Measured on two debug samples: member length is exactly
//! `4 * (zip_start / 0x10000)`.
//!
//! Serving package bytes under the `.crc` name made every such install fail
//! with 0x80b211cd (#319).

use std::io;

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: u64 = 22;
/// EOCD plus the longest possible ZIP comment.
const EOCD_SEARCH: u64 = EOCD_LEN + 0xFFFF;
/// A real SI central directory is a few hundred bytes; refuse anything absurd.
const MAX_CENTRAL_DIR: u64 = 1 << 20;
const CRC_MEMBER_SUFFIX: &[u8] = b"/playgo-chunk.crc";

/// True when the console is asking for the PlayGo CRC sidecar rather than the
/// package itself.
pub fn is_crc_request(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".crc")
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Absolute `(offset, length)` of the STORED `config/*/playgo-chunk.crc`
/// member in the ZIP that ends the package, or `None` when the package has no
/// such ZIP (retail images, PS4 packages) or the ZIP is not the plain shape
/// debug FPKGs use. `read_at(offset, len)` must return exactly `len` bytes.
pub fn locate_playgo_crc<F>(total: u64, mut read_at: F) -> io::Result<Option<(u64, u64)>>
where
    F: FnMut(u64, u64) -> io::Result<Vec<u8>>,
{
    if total < EOCD_LEN {
        return Ok(None);
    }
    let tail_len = total.min(EOCD_SEARCH);
    let tail_start = total - tail_len;
    let tail = read_at(tail_start, tail_len)?;
    if tail.len() < EOCD_LEN as usize {
        return Ok(None);
    }
    let Some(rel) = (0..=tail.len() - EOCD_LEN as usize)
        .rev()
        .find(|&i| u32le(&tail, i) == EOCD_SIG)
    else {
        return Ok(None);
    };
    let entries = u16le(&tail, rel + 10) as usize;
    let cd_size = u64::from(u32le(&tail, rel + 12));
    let cd_off = u64::from(u32le(&tail, rel + 16));
    let eocd_abs = tail_start + rel as u64;
    if cd_size == 0 || cd_size > MAX_CENTRAL_DIR || cd_size + cd_off > eocd_abs {
        return Ok(None);
    }
    // ZIP offsets are relative to the start of the archive, which sits after
    // the package image.
    let cd_abs = eocd_abs - cd_size;
    let zip_start = cd_abs - cd_off;
    let cd = read_at(cd_abs, cd_size)?;

    let mut pos = 0usize;
    for _ in 0..entries {
        if pos + 46 > cd.len() || u32le(&cd, pos) != CENTRAL_SIG {
            return Ok(None);
        }
        let method = u16le(&cd, pos + 10);
        let compressed = u64::from(u32le(&cd, pos + 20));
        let size = u64::from(u32le(&cd, pos + 24));
        let name_len = u16le(&cd, pos + 28) as usize;
        let extra_len = u16le(&cd, pos + 30) as usize;
        let comment_len = u16le(&cd, pos + 32) as usize;
        let local_off = u64::from(u32le(&cd, pos + 42));
        let name_end = pos + 46 + name_len;
        if name_end > cd.len() {
            return Ok(None);
        }
        let name = &cd[pos + 46..name_end];
        pos = name_end + extra_len + comment_len;

        if !(name.starts_with(b"config/") && name.ends_with(CRC_MEMBER_SUFFIX)) {
            continue;
        }
        // Served verbatim, so it must be stored, not deflated.
        if method != 0 || compressed != size || size == 0 {
            return Ok(None);
        }
        let local_abs = zip_start + local_off;
        if local_abs + 30 > cd_abs {
            return Ok(None);
        }
        let local = read_at(local_abs, 30)?;
        if local.len() < 30 || u32le(&local, 0) != LOCAL_SIG {
            return Ok(None);
        }
        let data = local_abs + 30 + u64::from(u16le(&local, 26)) + u64::from(u16le(&local, 28));
        if data + size > cd_abs {
            return Ok(None);
        }
        return Ok(Some((data, size)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    const CID: &str = "UP0000-PPSA01234_00-TESTGAME00000000";

    /// A fake finalized package: `prefix_len` bytes of image, then the SI ZIP.
    fn fake_package(prefix_len: usize, crc: &[u8], method: zip::CompressionMethod) -> Vec<u8> {
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        zw.start_file("common/etc/playgo-chunk.dat", opts).unwrap();
        zw.write_all(&[0x11; 416]).unwrap();
        zw.start_file(format!("config/{CID}/playgo-chunk.crc"), opts)
            .unwrap();
        zw.write_all(crc).unwrap();
        let zip_bytes = zw.finish().unwrap().into_inner();
        let mut pkg = vec![0xAB; prefix_len];
        pkg.extend_from_slice(&zip_bytes);
        pkg
    }

    fn reader(bytes: &[u8]) -> impl FnMut(u64, u64) -> std::io::Result<Vec<u8>> + '_ {
        move |off, len| Ok(bytes[off as usize..(off + len) as usize].to_vec())
    }

    #[test]
    fn finds_stored_crc_member_after_image() {
        let crc: Vec<u8> = (0u8..68).collect();
        let pkg = fake_package(0x20000, &crc, zip::CompressionMethod::Stored);
        let (off, len) = locate_playgo_crc(pkg.len() as u64, reader(&pkg))
            .unwrap()
            .expect("member found");
        assert_eq!(len, 68);
        assert_eq!(&pkg[off as usize..(off + len) as usize], &crc[..]);
        assert!(off > 0x20000, "offset is absolute, past the image");
    }

    #[test]
    fn no_zip_means_none() {
        let pkg = vec![0u8; 0x30000];
        assert_eq!(
            locate_playgo_crc(pkg.len() as u64, reader(&pkg)).unwrap(),
            None
        );
    }

    #[test]
    fn tiny_file_means_none() {
        let pkg = vec![0u8; 8];
        assert_eq!(
            locate_playgo_crc(pkg.len() as u64, reader(&pkg)).unwrap(),
            None
        );
    }

    #[test]
    fn compressed_member_is_refused() {
        let crc = vec![0u8; 4096];
        let pkg = fake_package(0x10000, &crc, zip::CompressionMethod::Deflated);
        assert_eq!(
            locate_playgo_crc(pkg.len() as u64, reader(&pkg)).unwrap(),
            None
        );
    }

    #[test]
    fn crc_request_names() {
        assert!(is_crc_request("UP0177-PPSA17597_00-SONICXSHADOWGENE.crc"));
        assert!(is_crc_request("X.CRC"));
        assert!(!is_crc_request("UP0177-PPSA17597_00-SONICXSHADOWGENE.pkg"));
        assert!(!is_crc_request("file.pkg"));
    }

    /// Real debug FPKG. Offsets measured on 2026-09-13.
    #[test]
    fn real_debug_sample_when_present() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let got = locate_playgo_crc(bytes.len() as u64, reader(&bytes)).unwrap();
        assert_eq!(got, Some((0x1324d8, 76)));
    }
}
