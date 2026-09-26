//! Normalise a dumped PS5 executable before it is packaged.
//!
//! Two defects appear in executables pulled off a console by some dumpers, and
//! Sony's Publishing Tools refuse a package containing either:
//!
//! - **the older signature**: the file starts with `4F 15 3D 1D` rather than the
//!   PS5 `54 14 F5 EE`. It is rewritten to the PS5 one.
//! - **a shifted version trailer**: the header's u64 at `0x10` marks where the
//!   appended `.sceversion` records begin, but in the dump they begin up to 15
//!   bytes earlier. Zero bytes are inserted so they start exactly at the marked
//!   offset again.
//!
//! These rules were learned from reading a third-party build kit (sdk-fpkg279);
//! this is an independent implementation of them. Whether the *console* needs
//! either repair — as opposed to only Sony's tool — has not been shown on
//! hardware: no dump available here has either defect. Repairs are therefore
//! applied only to files that match these exact conditions, and every repair is
//! reported. A file that matches neither is served byte for byte unchanged.
//!
//! Nothing is buffered: the repaired file is presented as a mapping over the
//! original, so a 300 MB `eboot.bin` is streamed like any other file.

/// The PS5 executable signature.
pub const PROSPERO_MAGIC: [u8; 4] = [0x54, 0x14, 0xF5, 0xEE];
/// The older signature some dumps carry.
pub const LEGACY_MAGIC: [u8; 4] = [0x4F, 0x15, 0x3D, 0x1D];

/// The furthest the version records are found before the header's marker.
const MAX_SHIFT: u64 = 0x0F;
/// Larger trailers are not version records; do not read them.
const MAX_TRAILER: u64 = 64 * 1024 * 1024;

/// How to present a repaired executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfRepair {
    pub rewrite_magic: bool,
    /// Where the zero padding goes: the first byte of the version records.
    pub insert_at: u64,
    /// Zero bytes inserted at `insert_at`.
    pub padding: u64,
}

impl SelfRepair {
    /// The repaired file's size.
    pub fn new_size(&self, original_size: u64) -> u64 {
        original_size + self.padding
    }

    /// One line for the build log.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.rewrite_magic {
            parts.push("older signature rewritten to the PS5 one".to_string());
        }
        if self.padding > 0 {
            parts.push(format!(
                "version records realigned (+{} bytes at {:#x})",
                self.padding, self.insert_at
            ));
        }
        parts.join(", ")
    }

    /// Bytes `[offset, offset + len)` of the repaired file, read from the
    /// original through `read_original(offset, len)`.
    pub fn read(
        &self,
        original_size: u64,
        offset: u64,
        len: usize,
        read_original: &mut dyn FnMut(u64, usize) -> crate::Result<Vec<u8>>,
    ) -> crate::Result<Vec<u8>> {
        let size = self.new_size(original_size);
        let end = offset.saturating_add(len as u64).min(size);
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        let mut pos = offset;
        let gap_end = self.insert_at + self.padding;
        while pos < end {
            if pos < self.insert_at {
                // Before the padding: the original at the same offset.
                let stop = end.min(self.insert_at);
                let mut chunk = read_original(pos, (stop - pos) as usize)?;
                if self.rewrite_magic && pos < 4 {
                    for (i, b) in chunk.iter_mut().enumerate() {
                        let at = pos as usize + i;
                        if at < 4 {
                            *b = PROSPERO_MAGIC[at];
                        }
                    }
                }
                pos += chunk.len() as u64;
                out.extend_from_slice(&chunk);
                if chunk.is_empty() {
                    break;
                }
            } else if pos < gap_end {
                let stop = end.min(gap_end);
                out.resize(out.len() + (stop - pos) as usize, 0);
                pos = stop;
            } else {
                // After the padding: the original, shifted back by it.
                let from = pos - self.padding;
                let chunk = read_original(from, (end - pos) as usize)?;
                pos += chunk.len() as u64;
                out.extend_from_slice(&chunk);
                if chunk.is_empty() {
                    break;
                }
            }
        }
        Ok(out)
    }
}

/// True when `records` is exactly a run of well-formed `.sceversion` records:
/// each `00 00 <u16 payload> 08 <name ending ':'> <u64 version> <same u64>`.
pub fn is_version_records(records: &[u8]) -> bool {
    let mut at = 0usize;
    let mut count = 0usize;
    while at < records.len() {
        let rest = &records[at..];
        if rest.len() < 5 || rest[0] != 0 || rest[1] != 0 {
            return false;
        }
        let payload = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        let record = payload + 4;
        if payload < 18 || record > rest.len() || rest[4] != 8 {
            return false;
        }
        let name_len = payload - 17;
        let name = &rest[5..5 + name_len];
        let version = &rest[5 + name_len..5 + name_len + 16];
        if name.last() != Some(&b':')
            || name.iter().any(|&b| !(0x20..=0x7E).contains(&b))
            || version[..8] != version[8..]
        {
            return false;
        }
        at += record;
        count += 1;
    }
    count > 0
}

/// The repair `header` (the first 0x20 bytes) and the file's tail call for, or
/// `None` when the file is not a PS5 executable or needs nothing.
///
/// `read(offset, len)` reads the original; it is used only for the trailer.
pub fn plan(
    header: &[u8],
    size: u64,
    read: &mut dyn FnMut(u64, usize) -> crate::Result<Vec<u8>>,
) -> Option<SelfRepair> {
    if header.len() < 0x20 || size < 0x20 {
        return None;
    }
    let magic: [u8; 4] = header[..4].try_into().ok()?;
    let rewrite_magic = match magic {
        LEGACY_MAGIC => true,
        PROSPERO_MAGIC => false,
        _ => return None,
    };
    let only_magic = rewrite_magic.then_some(SelfRepair {
        rewrite_magic: true,
        insert_at: size,
        padding: 0,
    });
    let marker = u64::from_le_bytes(header[0x10..0x18].try_into().ok()?);
    if !(0x20..=size).contains(&marker) {
        return only_magic;
    }
    let start = marker.saturating_sub(MAX_SHIFT);
    let tail_len = size - start;
    if tail_len == 0 || tail_len > MAX_TRAILER {
        return only_magic;
    }
    let tail = read(start, tail_len as usize).ok()?;
    if tail.len() as u64 != tail_len {
        return only_magic;
    }
    let local = (marker - start) as usize;
    for shift in 0..=MAX_SHIFT.min(local as u64) as usize {
        if is_version_records(&tail[local - shift..]) {
            if shift == 0 && !rewrite_magic {
                return None;
            }
            return Some(SelfRepair {
                rewrite_magic,
                insert_at: marker - shift as u64,
                padding: shift as u64,
            });
        }
    }
    only_magic
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `.sceversion` record named `name` at `version`.
    fn record(name: &str, version: u64) -> Vec<u8> {
        let payload = (name.len() + 17) as u16;
        let mut r = vec![0, 0];
        r.extend_from_slice(&payload.to_le_bytes());
        r.push(8);
        r.extend_from_slice(name.as_bytes());
        r.extend_from_slice(&version.to_le_bytes());
        r.extend_from_slice(&version.to_le_bytes());
        r
    }

    /// A synthetic executable: 0x20 header (marker at 0x10), a body, and version
    /// records that start `shift` bytes before the marker.
    fn exe(magic: [u8; 4], body_len: usize, shift: usize) -> Vec<u8> {
        let records = [
            record("libc:", 0x0100_0000),
            record("libkernel:", 0x0200_0000),
        ]
        .concat();
        let mut f = magic.to_vec();
        f.resize(0x20, 0x11);
        f.resize(0x20 + body_len, 0x22);
        let records_at = f.len();
        f.extend_from_slice(&records);
        let marker = (records_at + shift) as u64;
        f[0x10..0x18].copy_from_slice(&marker.to_le_bytes());
        f
    }

    fn reader(bytes: &[u8]) -> impl FnMut(u64, usize) -> crate::Result<Vec<u8>> + '_ {
        move |o, l| {
            let s = (o as usize).min(bytes.len());
            Ok(bytes[s..(s + l).min(bytes.len())].to_vec())
        }
    }

    /// The repaired file, materialised naively, for comparison.
    fn repaired(original: &[u8], r: &SelfRepair) -> Vec<u8> {
        let mut out = original[..r.insert_at as usize].to_vec();
        if r.rewrite_magic {
            out[..4].copy_from_slice(&PROSPERO_MAGIC);
        }
        out.resize(out.len() + r.padding as usize, 0);
        out.extend_from_slice(&original[r.insert_at as usize..]);
        out
    }

    #[test]
    fn records_are_recognised_and_garbage_is_not() {
        assert!(is_version_records(&record("libc:", 7)));
        assert!(is_version_records(
            &[record("a:", 1), record("bb:", 2)].concat()
        ));
        assert!(!is_version_records(&[]));
        assert!(!is_version_records(b"not records at all"));
        // Name must end with ':' and versions must repeat.
        let mut bad = record("libc", 7);
        assert!(!is_version_records(&bad));
        bad = record("libc:", 7);
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(!is_version_records(&bad));
    }

    /// The common case, and every executable in the Minecraft dump: already
    /// correct, so nothing changes.
    #[test]
    fn a_correct_executable_is_left_alone() {
        let f = exe(PROSPERO_MAGIC, 100, 0);
        assert_eq!(plan(&f[..0x20], f.len() as u64, &mut reader(&f)), None);
    }

    /// A PS5 executable with no version records at the marker (several in the
    /// Minecraft dump) is not touched either.
    #[test]
    fn no_version_records_means_no_repair() {
        let mut f = PROSPERO_MAGIC.to_vec();
        f.resize(0x200, 0x33);
        f[0x10..0x18].copy_from_slice(&0x100u64.to_le_bytes());
        assert_eq!(plan(&f[..0x20], f.len() as u64, &mut reader(&f)), None);
    }

    #[test]
    fn the_older_signature_is_rewritten() {
        let f = exe(LEGACY_MAGIC, 100, 0);
        let r = plan(&f[..0x20], f.len() as u64, &mut reader(&f)).expect("repair");
        assert!(r.rewrite_magic);
        assert_eq!(r.padding, 0);
        let got = r.read(f.len() as u64, 0, 8, &mut reader(&f)).unwrap();
        assert_eq!(&got[..4], &PROSPERO_MAGIC);
        assert_eq!(&got[4..], &f[4..8]);
    }

    #[test]
    fn shifted_version_records_are_realigned_to_the_marker() {
        for shift in 1..=15 {
            let f = exe(PROSPERO_MAGIC, 100, shift);
            let r = plan(&f[..0x20], f.len() as u64, &mut reader(&f)).expect("repair");
            assert_eq!(r.padding, shift as u64, "shift {shift}");
            let fixed = repaired(&f, &r);
            let marker = u64::from_le_bytes(f[0x10..0x18].try_into().unwrap()) as usize;
            assert!(is_version_records(&fixed[marker..]), "shift {shift}");
            assert_eq!(fixed.len() as u64, r.new_size(f.len() as u64));
        }
    }

    #[test]
    fn a_non_executable_is_ignored() {
        let f = [0x7F, b'E', b'L', b'F', 0, 0, 0, 0].repeat(8);
        assert_eq!(plan(&f[..0x20], f.len() as u64, &mut reader(&f)), None);
    }

    /// The streamed view must equal the naively repaired file for every range,
    /// across the magic, the padding and the shifted tail.
    #[test]
    fn every_range_of_the_streamed_view_matches_the_repaired_file() {
        let f = exe(LEGACY_MAGIC, 300, 7);
        let r = plan(&f[..0x20], f.len() as u64, &mut reader(&f)).expect("repair");
        let want = repaired(&f, &r);
        let size = r.new_size(f.len() as u64) as usize;
        for start in (0..size).step_by(3) {
            for len in [1usize, 2, 5, 13, 64, 400] {
                let got = r
                    .read(f.len() as u64, start as u64, len, &mut reader(&f))
                    .unwrap();
                let end = (start + len).min(size);
                assert_eq!(got, want[start..end], "range {start}+{len}");
            }
        }
    }
}
