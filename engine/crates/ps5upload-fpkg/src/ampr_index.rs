//! `ampr_emu.index`: the file table AMPR emulation reads from the image root.
//!
//! A title built against libSceAmpr resolves its files through AMPR; on a backported console
//! that is `ampr_emu`, which looks every `/app0` path up in this index (and otherwise has to
//! build one itself at startup). The format is `AMPRIDX3`, as `ampr_emu` 0.3.1 and later read it
//! (drakmor/ampr_emu, which validates every slot's hash against its path before trusting the
//! table):
//!
//! ```text
//! header   "AMPRIDX3", u32 3, u32 record size (24), u64 records, u64 path bytes,
//!          u64 slot table offset, u32 slot size (16), u32 slot count          (0x30 bytes)
//! records  {u32 path offset, u32 path length, u64 size, i64 mtime}, sorted by key
//! paths    "/app0/…" strings, each NUL-terminated
//! slots    {u64 hash, u32 record index + 1, u32 flags}, 16-aligned; open addressing from
//!          hash & (count − 1), count the least power of two ≥ twice the records
//! ```
//!
//! The key is the path with `\` as `/` and ASCII letters lowered; the hash is 64-bit FNV-1a of
//! the key with the offset basis `1469598103934665603` (0 is stored as 1). That basis is the
//! standard FNV-64 one with its last digit dropped, and it is what the format uses: `ampr_emu`
//! computes it at runtime and rejects a table hashed any other way. Every one of the 263 slots
//! in Spider-Man 2's released index reproduces with it. Flag 1 marks a slot whose hash another
//! path shares.

const MAGIC: &[u8; 8] = b"AMPRIDX3";
const RECORD: usize = 24;
const SLOT: usize = 16;
const HEADER: usize = 0x30;
const DUPLICATE: u32 = 1;

/// The lookup key: `\` as `/`, ASCII `A`–`Z` lowered, everything else as it is.
fn key(path: &str) -> Vec<u8> {
    path.bytes()
        .map(|b| match b {
            b'\\' => b'/',
            b'A'..=b'Z' => b + 0x20,
            _ => b,
        })
        .collect()
}

/// The format's FNV offset basis (see the module notes): not the standard `0xcbf29ce484222325`.
const BASIS: u64 = 1_469_598_103_934_665_603;

/// 64-bit FNV-1a of a path's key, from the format's basis.
pub fn hash(path: &str) -> u64 {
    let h = key(path).iter().fold(BASIS, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    h.max(1)
}

/// The index for `files`, as `(path inside the image, size)`; every entry gets `mtime`.
/// Returns `None` when two paths differ only in case, which the index cannot tell apart.
pub fn build(files: &[(String, u64)], mtime: i64) -> Option<Vec<u8>> {
    let mut rows: Vec<(Vec<u8>, String, u64)> = files
        .iter()
        .map(|(p, size)| {
            let path = format!("/app0/{}", p.trim_start_matches('/'));
            (key(&path), path, *size)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if rows.windows(2).any(|w| w[0].0 == w[1].0) {
        return None;
    }
    let mut records = Vec::with_capacity(rows.len() * RECORD);
    let mut paths = Vec::new();
    for (_, path, size) in &rows {
        records.extend_from_slice(&(paths.len() as u32).to_le_bytes());
        records.extend_from_slice(&(path.len() as u32).to_le_bytes());
        records.extend_from_slice(&size.to_le_bytes());
        records.extend_from_slice(&mtime.to_le_bytes());
        paths.extend_from_slice(path.as_bytes());
        paths.push(0);
    }
    let count = (rows.len() * 2).next_power_of_two().max(2);
    let mask = count - 1;
    let mut slots = vec![(0u64, 0u32, 0u32); count];
    for (i, (_, path, _)) in rows.iter().enumerate() {
        let h = hash(path);
        let mut at = h as usize & mask;
        let mut shared = false;
        while slots[at].1 != 0 {
            if slots[at].0 == h {
                slots[at].2 |= DUPLICATE;
                shared = true;
            }
            at = (at + 1) & mask;
        }
        slots[at] = (h, i as u32 + 1, if shared { DUPLICATE } else { 0 });
    }
    let slots_at = (HEADER + records.len() + paths.len()).next_multiple_of(SLOT);
    let mut out = Vec::with_capacity(slots_at + count * SLOT);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(RECORD as u32).to_le_bytes());
    out.extend_from_slice(&(rows.len() as u64).to_le_bytes());
    out.extend_from_slice(&(paths.len() as u64).to_le_bytes());
    out.extend_from_slice(&(slots_at as u64).to_le_bytes());
    out.extend_from_slice(&(SLOT as u32).to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&records);
    out.extend_from_slice(&paths);
    out.resize(slots_at, 0);
    for (h, index, flags) in slots {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(d: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
    }
    fn u64_at(d: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
    }

    /// Slots from Spider-Man 2's released `ampr_emu.index`, which `ampr_emu` accepts.
    #[test]
    fn the_hash_reproduces_a_released_index() {
        assert_eq!(hash("/app0/toc"), 0x3489_25d2_02b6_4400);
        assert_eq!(hash("/app0/d/materialgraph"), 0xedd2_eedc_1022_d002);
        assert_eq!(hash("/app0/commandline.txt"), 0x7fcf_babf_58b3_482b);
        assert_eq!(
            hash("/app0/sce_sys/trophy2/trophy00.ucp"),
            0xf487_434e_de06_e42f
        );
        assert_eq!(hash(""), BASIS);
        assert_eq!(hash("a"), 0x44bd8ad473cd9906);
        assert_eq!(hash("/APP0/Toc"), hash("/app0/toc"));
        assert_eq!(hash("\\app0\\toc"), hash("/app0/toc"));
    }

    /// Every record is found from its own hash by the runtime's probe, the table is at least
    /// twice the records, and the layout fields agree with the bytes.
    #[test]
    fn every_record_is_reachable_through_its_slot() {
        let files: Vec<(String, u64)> = (0..300)
            .map(|i| (format!("d/Part{i:03}.bin"), i as u64 * 7))
            .chain([
                ("eboot.bin".to_string(), 99),
                ("sce_sys/param.json".to_string(), 5),
            ])
            .collect();
        let d = build(&files, 1_777_000_000).unwrap();
        assert_eq!(&d[..8], MAGIC);
        let n = u64_at(&d, 0x10) as usize;
        let path_bytes = u64_at(&d, 0x18) as usize;
        let slots_at = u64_at(&d, 0x20) as usize;
        let count = u32_at(&d, 0x2C) as usize;
        assert_eq!(n, files.len());
        assert_eq!(count, 1024);
        assert_eq!(slots_at % 16, 0);
        assert_eq!(d.len(), slots_at + count * 16);
        let paths_at = HEADER + n * RECORD;
        assert!(paths_at + path_bytes <= slots_at);
        let path_of = |i: usize| {
            let r = HEADER + i * RECORD;
            let (off, len) = (u32_at(&d, r) as usize, u32_at(&d, r + 4) as usize);
            assert_eq!(d[paths_at + off + len], 0);
            std::str::from_utf8(&d[paths_at + off..paths_at + off + len]).unwrap()
        };
        let mut previous = Vec::new();
        for i in 0..n {
            let p = path_of(i);
            assert!(p.starts_with("/app0/"));
            assert!(key(p) > previous, "records sorted by key");
            previous = key(p);
            let h = hash(p);
            let mut at = h as usize & (count - 1);
            loop {
                let s = slots_at + at * 16;
                assert_ne!(u32_at(&d, s + 8), 0, "{p} not reachable");
                if u64_at(&d, s) == h && u32_at(&d, s + 8) as usize == i + 1 {
                    break;
                }
                at = (at + 1) & (count - 1);
            }
        }
        let size_of_eboot = (0..n)
            .find(|&i| path_of(i) == "/app0/eboot.bin")
            .map(|i| u64_at(&d, HEADER + i * RECORD + 8));
        assert_eq!(size_of_eboot, Some(99));
    }

    #[test]
    fn paths_differing_only_in_case_are_refused() {
        let files = vec![("A.bin".to_string(), 1), ("a.bin".to_string(), 2)];
        assert!(build(&files, 0).is_none());
    }
}
