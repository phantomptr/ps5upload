//! BPS binary patch application.
//!
//! Backporting a game to older firmware needs system libraries from a
//! newer firmware, patched to drop imports the older firmware does not
//! have. Those patches are distributed as BPS files (BackPork ships one
//! set per firmware range), and applying them has meant sending users to
//! a browser-based patcher. Doing it here lets a library be patched on
//! the way to the console.
//!
//! Format: <https://www.romhacking.net/documents/746/> — "BPS1", three
//! varint sizes, a metadata blob, a stream of copy/read actions, and
//! three CRC32s. The checksums are the point: a patch applied to the
//! wrong source file produces a plausible-looking but broken library,
//! and only the target checksum catches it.

use anyhow::{bail, Result};

const MAGIC: &[u8; 4] = b"BPS1";

/// Header fields, readable without applying the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BpsInfo {
    pub source_size: u64,
    pub target_size: u64,
    pub metadata: String,
    /// CRC32 the source file must have for this patch to apply.
    pub source_crc: u32,
    /// CRC32 the result is expected to have.
    pub target_crc: u32,
}

/// Read a BPS "number": 7 bits per byte, low group first, high bit marks
/// the last byte, and each continuation adds the next power so encodings
/// stay unique.
fn read_number(patch: &[u8], pos: &mut usize) -> Result<u64> {
    let mut data: u64 = 0;
    let mut shift: u64 = 1;
    loop {
        if *pos >= patch.len() {
            bail!("truncated patch: number runs past end");
        }
        let x = patch[*pos];
        *pos += 1;
        data = data
            .checked_add((x as u64 & 0x7f).wrapping_mul(shift))
            .ok_or_else(|| anyhow::anyhow!("patch number overflow"))?;
        if x & 0x80 != 0 {
            return Ok(data);
        }
        shift = shift
            .checked_shl(7)
            .ok_or_else(|| anyhow::anyhow!("patch number overflow"))?;
        data = data
            .checked_add(shift)
            .ok_or_else(|| anyhow::anyhow!("patch number overflow"))?;
    }
}

/// Relative offsets are stored with the sign in the low bit.
fn read_signed(patch: &[u8], pos: &mut usize) -> Result<i64> {
    let n = read_number(patch, pos)?;
    let mag = (n >> 1) as i64;
    Ok(if n & 1 != 0 { -mag } else { mag })
}

fn read_u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

/// Magic (4) + three minimal varints (3) + three CRC32s (12).
const MIN_PATCH_LEN: usize = 19;

struct Header {
    source_size: usize,
    target_size: usize,
    metadata: String,
    /// First byte of the action stream.
    body: usize,
    /// First byte of the 12-byte footer.
    footer: usize,
}

fn parse_header(patch: &[u8]) -> Result<Header> {
    if patch.len() < MIN_PATCH_LEN {
        bail!("too small to be a BPS patch ({} bytes)", patch.len());
    }
    if &patch[..4] != MAGIC {
        bail!("not a BPS patch: missing BPS1 magic");
    }
    let footer = patch.len() - 12;
    let mut pos = 4;
    let source_size = read_number(patch, &mut pos)? as usize;
    let target_size = read_number(patch, &mut pos)? as usize;
    let meta_len = read_number(patch, &mut pos)? as usize;
    if pos + meta_len > footer {
        bail!("truncated patch: metadata runs past the end");
    }
    let metadata = String::from_utf8_lossy(&patch[pos..pos + meta_len]).into_owned();
    Ok(Header {
        source_size,
        target_size,
        metadata,
        body: pos + meta_len,
        footer,
    })
}

/// Parse the header and footer without applying anything.
pub fn bps_info(patch: &[u8]) -> Result<BpsInfo> {
    let h = parse_header(patch)?;
    Ok(BpsInfo {
        source_size: h.source_size as u64,
        target_size: h.target_size as u64,
        metadata: h.metadata,
        source_crc: read_u32_le(patch, h.footer),
        target_crc: read_u32_le(patch, h.footer + 4),
    })
}

/// Apply `patch` to `source`, returning the patched bytes.
///
/// All three checksums are enforced. The source check is the one that
/// matters in practice: these patches target one exact build of one
/// library, and applying one to a library from a different firmware
/// would otherwise yield a corrupt file that only fails at game launch.
pub fn bps_apply(patch: &[u8], source: &[u8]) -> Result<Vec<u8>> {
    let h = parse_header(patch)?;

    let want_patch_crc = read_u32_le(patch, h.footer + 8);
    let got_patch_crc = crc32(&patch[..patch.len() - 4]);
    if want_patch_crc != got_patch_crc {
        bail!("patch file is corrupt: checksum {got_patch_crc:08x} != {want_patch_crc:08x}");
    }

    if source.len() != h.source_size {
        bail!(
            "source is {} bytes but this patch expects {}",
            source.len(),
            h.source_size
        );
    }
    let want_source_crc = read_u32_le(patch, h.footer);
    let got_source_crc = crc32(source);
    if got_source_crc != want_source_crc {
        bail!(
            "source checksum {got_source_crc:08x} does not match the {want_source_crc:08x} \
             this patch was built for — wrong file, or a library from a different firmware"
        );
    }

    let mut out: Vec<u8> = Vec::with_capacity(h.target_size);
    let mut src_rel: i64 = 0;
    let mut tgt_rel: i64 = 0;
    let mut pos = h.body;

    while pos < h.footer {
        let packed = read_number(patch, &mut pos)?;
        let cmd = packed & 3;
        let len = (packed >> 2) as usize + 1;
        if out.len() + len > h.target_size {
            bail!("patch writes past its own declared target size");
        }
        match cmd {
            // SourceRead: same offset in the source as in the output.
            0 => {
                let at = out.len();
                if at + len > source.len() {
                    bail!("SourceRead runs past the end of the source");
                }
                out.extend_from_slice(&source[at..at + len]);
            }
            // TargetRead: literal bytes carried in the patch.
            1 => {
                if pos + len > h.footer {
                    bail!("TargetRead runs past the end of the patch");
                }
                out.extend_from_slice(&patch[pos..pos + len]);
                pos += len;
            }
            // SourceCopy: independently tracked cursor into the source.
            2 => {
                src_rel += read_signed(patch, &mut pos)?;
                if src_rel < 0 || (src_rel as usize).saturating_add(len) > source.len() {
                    bail!("SourceCopy addresses outside the source");
                }
                let s = src_rel as usize;
                out.extend_from_slice(&source[s..s + len]);
                src_rel += len as i64;
            }
            // TargetCopy: reads what has already been written, so it may
            // overlap the write cursor — that is how BPS encodes runs,
            // and it has to stay byte-at-a-time for that reason.
            _ => {
                tgt_rel += read_signed(patch, &mut pos)?;
                if tgt_rel < 0 {
                    bail!("TargetCopy addresses before the start of the output");
                }
                for _ in 0..len {
                    let t = tgt_rel as usize;
                    if t >= out.len() {
                        bail!("TargetCopy reads past what has been written so far");
                    }
                    let b = out[t];
                    out.push(b);
                    tgt_rel += 1;
                }
            }
        }
    }

    if out.len() != h.target_size {
        bail!(
            "patch produced {} bytes but declared {}",
            out.len(),
            h.target_size
        );
    }
    let want_target_crc = read_u32_le(patch, h.footer + 4);
    let got_target_crc = crc32(&out);
    if got_target_crc != want_target_crc {
        bail!("patched result checksum {got_target_crc:08x} != expected {want_target_crc:08x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BPS number encoder, mirroring read_number.
    fn enc(mut n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let x = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(0x80 | x);
                break;
            }
            out.push(x);
            n -= 1;
        }
        out
    }

    fn enc_signed(v: i64) -> Vec<u8> {
        let n = ((v.unsigned_abs()) << 1) | if v < 0 { 1 } else { 0 };
        enc(n)
    }

    /// Build a complete, checksum-correct patch from encoded actions.
    fn build(source: &[u8], target: &[u8], actions: Vec<u8>, metadata: &str) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(MAGIC);
        p.extend_from_slice(&enc(source.len() as u64));
        p.extend_from_slice(&enc(target.len() as u64));
        p.extend_from_slice(&enc(metadata.len() as u64));
        p.extend_from_slice(metadata.as_bytes());
        p.extend_from_slice(&actions);
        p.extend_from_slice(&crc32(source).to_le_bytes());
        p.extend_from_slice(&crc32(target).to_le_bytes());
        let so_far = crc32(&p);
        p.extend_from_slice(&so_far.to_le_bytes());
        p
    }

    fn action(cmd: u64, len: u64) -> Vec<u8> {
        enc(((len - 1) << 2) | cmd)
    }

    #[test]
    fn rejects_a_file_that_is_not_a_bps_patch() {
        assert!(bps_apply(b"not a patch at all............", b"").is_err());
        assert!(bps_apply(b"", b"").is_err());
    }

    /// TargetRead: the patch carries the bytes literally.
    #[test]
    fn applies_a_literal_target_read() {
        let source = b"".as_slice();
        let target = b"hello".as_slice();
        let mut a = action(1, 5);
        a.extend_from_slice(target);
        let patch = build(source, target, a, "");
        assert_eq!(bps_apply(&patch, source).unwrap(), target);
    }

    /// SourceRead: copy from the source at the current output position.
    #[test]
    fn applies_a_source_read() {
        let source = b"ABCDEFGH".as_slice();
        let target = source;
        let patch = build(source, target, action(0, 8), "");
        assert_eq!(bps_apply(&patch, source).unwrap(), target);
    }

    /// The common real-world shape: keep most of the file, replace a few
    /// bytes in the middle — which is exactly what an import patch does.
    #[test]
    fn applies_a_mixed_patch_that_edits_the_middle() {
        let source = b"AAAABBBBCCCC".as_slice();
        let target = b"AAAAXXXXCCCC".as_slice();
        let mut a = action(0, 4); // keep AAAA
        a.extend_from_slice(&action(1, 4)); // literal XXXX
        a.extend_from_slice(b"XXXX");
        a.extend_from_slice(&action(0, 4)); // keep CCCC
        let patch = build(source, target, a, "");
        assert_eq!(bps_apply(&patch, source).unwrap(), target);
    }

    /// SourceCopy reads from an independently tracked source cursor.
    #[test]
    fn applies_a_source_copy() {
        let source = b"0123456789".as_slice();
        let target = b"5678".as_slice();
        let mut a = action(2, 4);
        a.extend_from_slice(&enc_signed(5));
        let patch = build(source, target, a, "");
        assert_eq!(bps_apply(&patch, source).unwrap(), target);
    }

    /// TargetCopy reads from what has already been written — this is how
    /// BPS expresses run-length repetition.
    #[test]
    fn applies_a_target_copy() {
        let source = b"".as_slice();
        let target = b"abcabcabc".as_slice();
        let mut a = action(1, 3);
        a.extend_from_slice(b"abc");
        a.extend_from_slice(&action(3, 6));
        a.extend_from_slice(&enc_signed(0));
        let patch = build(source, target, a, "");
        assert_eq!(bps_apply(&patch, source).unwrap(), target);
    }

    /// Applying a patch to the wrong file is the mistake that produces a
    /// silently broken library, so the source checksum must stop it.
    #[test]
    fn refuses_a_source_whose_checksum_does_not_match() {
        let source = b"ABCDEFGH".as_slice();
        let patch = build(source, source, action(0, 8), "");
        let err = bps_apply(&patch, b"WRONGWRO").unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("source"),
            "error should name the source checksum, got: {err}"
        );
    }

    #[test]
    fn refuses_a_source_of_the_wrong_length() {
        let source = b"ABCDEFGH".as_slice();
        let patch = build(source, source, action(0, 8), "");
        assert!(bps_apply(&patch, b"SHORT").is_err());
    }

    /// A corrupted patch body must be caught rather than producing a
    /// plausible-looking output.
    #[test]
    fn refuses_a_patch_whose_own_checksum_is_wrong() {
        let source = b"ABCDEFGH".as_slice();
        let mut patch = build(source, source, action(0, 8), "");
        let n = patch.len();
        patch[n - 1] ^= 0xff;
        assert!(bps_apply(&patch, source).is_err());
    }

    #[test]
    fn reports_header_fields_without_applying() {
        let source = b"ABCDEFGH".as_slice();
        let target = b"ABCDEFGH".as_slice();
        let patch = build(source, target, action(0, 8), "note");
        let info = bps_info(&patch).unwrap();
        assert_eq!(info.source_size, 8);
        assert_eq!(info.target_size, 8);
        assert_eq!(info.metadata, "note");
        assert_eq!(info.source_crc, crc32(source));
    }

    #[test]
    fn round_trips_every_number_the_encoder_produces() {
        for n in [
            0u64,
            1,
            127,
            128,
            129,
            255,
            16383,
            16384,
            1 << 20,
            u32::MAX as u64,
        ] {
            let bytes = enc(n);
            let mut pos = 0;
            assert_eq!(read_number(&bytes, &mut pos).unwrap(), n, "n={n}");
            assert_eq!(pos, bytes.len());
        }
    }

    #[test]
    fn round_trips_signed_offsets() {
        for v in [
            0i64,
            1,
            -1,
            1000,
            -1000,
            i32::MAX as i64,
            -(i32::MAX as i64),
        ] {
            let bytes = enc_signed(v);
            let mut pos = 0;
            assert_eq!(read_signed(&bytes, &mut pos).unwrap(), v, "v={v}");
        }
    }

    #[test]
    fn refuses_a_truncated_patch() {
        let source = b"ABCDEFGH".as_slice();
        let patch = build(source, source, action(0, 8), "");
        assert!(bps_apply(&patch[..patch.len() - 6], source).is_err());
    }
}
