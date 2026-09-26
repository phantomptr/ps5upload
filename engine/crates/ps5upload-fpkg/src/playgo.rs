//! The PlayGo tables: `playgo-chunk.dat`, `playgo-hash-table.dat` and `playgo-ficm.dat`.
//!
//! Measured on a Publishing Tools package with 64 chunks (Spider-Man 2, built by the
//! sdk-fpkg279 kit) and on the single-chunk packages that install:
//!
//! - **Chunks are equal slices of the data.** The mount image from its start to the end of
//!   the game files is cut into `n` slices on 64 KiB boundaries, the first `units % n` of them
//!   one unit longer; chunk `k` owns slice `k`. The rest of the image up to the container
//!   (the file system's own metadata, which a mount needs first) is one more mchunk, owned by
//!   chunk 0.
//! - **The hash table is a flat-path table of the image's files.** One entry per file (no
//!   directories): its [`flt::hash_path`], sorted. The single-chunk writer used to carry five
//!   constants here, which turned out to be path hashes from the sample packages.
//! - **The file map follows the hash table.** One `u16` chunk id per entry, in the same
//!   sorted order: the chunk holding the start of that file's data.
//!
//! `playgo-chunk.dat` is `plgx` version `0x1000` in the layout LibOrbisPkg documents for the
//! PS4, with two PS5 differences: the mchunk count is the byte at `0x20`, and a chunk's
//! mchunk list is `u32` entries. Every table is 16-byte aligned.

use crate::flt;
use crate::{format_err, Result, BLOCK};

/// The chunk count a build uses unless told otherwise.
pub const DEFAULT_CHUNKS: u16 = 1;
/// The most chunks the format's scenario table and the SDK allow.
pub const MAX_CHUNKS: u16 = 255;

/// The three tables, and the map the install manifest repeats.
#[derive(Debug, Clone)]
pub struct PlayGo {
    pub chunk_dat: Vec<u8>,
    pub hash_table: Vec<u8>,
    pub ficm: Vec<u8>,
    /// Each chunk's mchunk ids, in chunk order.
    pub chunks: Vec<Vec<u32>>,
    /// Each mchunk's `(offset, size)` in the mount image.
    pub mchunks: Vec<(u64, u64)>,
}

impl PlayGo {
    /// Bytes in chunk `id`.
    pub fn chunk_size(&self, id: usize) -> u64 {
        self.chunks[id]
            .iter()
            .map(|&m| self.mchunks[m as usize].1)
            .sum()
    }
}

/// The tables for an image whose files are `files` (`(path, offset in the mount image,
/// size)`), whose container starts at `cnt_offset`, split into `chunk_count` chunks.
///
/// One chunk keeps the shape every installing single-chunk package has: one mchunk spanning
/// the whole image, and no labels. More chunks take the Publishing Tools shape above. The
/// count is lowered when the data has fewer 64 KiB units than chunks.
pub fn build(
    content_id: &str,
    files: &[(String, u64, u64)],
    cnt_offset: u64,
    chunk_count: u16,
) -> Result<PlayGo> {
    if content_id.len() != 36 {
        return format_err(format!(
            "content id must be 36 characters, got {}",
            content_id.len()
        ));
    }
    if chunk_count == 0 || chunk_count > MAX_CHUNKS {
        return format_err(format!(
            "PlayGo chunk count must be 1 through {MAX_CHUNKS}, not {chunk_count}"
        ));
    }
    let data_end = files
        .iter()
        .map(|(_, at, size)| at + size)
        .max()
        .unwrap_or(0)
        .next_multiple_of(BLOCK)
        .clamp(BLOCK, cnt_offset.max(BLOCK));
    let units = data_end / BLOCK;
    let n = u64::from(chunk_count).min(units).max(1);

    let (chunks, mchunks) = if n == 1 {
        (vec![vec![0u32]], vec![(0, cnt_offset)])
    } else {
        let mut mchunks = Vec::with_capacity(n as usize + 1);
        let mut at = 0u64;
        for k in 0..n {
            let len = units / n + u64::from(k < units % n);
            mchunks.push((at * BLOCK, len * BLOCK));
            at += len;
        }
        let mut chunks: Vec<Vec<u32>> = (0..n as u32).map(|k| vec![k]).collect();
        if cnt_offset > data_end {
            mchunks.push((data_end, cnt_offset - data_end));
            chunks[0].push(n as u32);
        }
        (chunks, mchunks)
    };

    // The file map, in the hash table's order.
    let mut entries: Vec<(u64, u16)> = files
        .iter()
        .map(|(path, at, _)| {
            let chunk = mchunks[..n as usize]
                .iter()
                .position(|(start, len)| *at < start + len)
                .unwrap_or(0) as u16;
            (flt::hash_path(path), if n == 1 { 0 } else { chunk })
        })
        .collect();
    entries.sort_by_key(|(hash, _)| *hash);

    Ok(PlayGo {
        chunk_dat: chunk_dat(content_id, &chunks, &mchunks, n > 1),
        hash_table: hash_table(&entries),
        ficm: ficm(&entries),
        chunks,
        mchunks,
    })
}

fn put16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// `playgo-chunk.dat`. `labels` names the chunks `Chunk #k` and the scenario `Scenario #0`,
/// as the Publishing Tools package does; the single-chunk shape leaves both empty.
pub fn chunk_dat(
    content_id: &str,
    chunks: &[Vec<u32>],
    mchunks: &[(u64, u64)],
    labels: bool,
) -> Vec<u8> {
    let n = chunks.len();
    // Chunk labels, and each chunk's offset into them.
    let mut chunk_labels = Vec::new();
    let mut label_at = Vec::with_capacity(n);
    for k in 0..n {
        label_at.push(chunk_labels.len() as u32);
        if labels {
            chunk_labels.extend_from_slice(format!("Chunk #{k}").as_bytes());
            chunk_labels.push(0);
        }
    }
    if !labels {
        chunk_labels.push(0);
        label_at.fill(0);
    }
    let scenario_label: &[u8] = if labels { b"Scenario #0\0" } else { b"\0" };
    let mchunk_list: Vec<u32> = chunks.iter().flatten().copied().collect();

    // The tables, each on a 16-byte boundary, in the order the header lists them.
    let sizes = [
        n * 0x20,              // chunk attrs
        mchunk_list.len() * 4, // chunk mchunks
        chunk_labels.len(),    // chunk labels
        mchunks.len() * 0x10,  // mchunk attrs
        0x20,                  // scenario attrs
        n * 2,                 // scenario chunks
        scenario_label.len(),  // scenario labels
    ];
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut at = 0x100usize;
    for size in sizes {
        offsets.push(at);
        at = (at + size).next_multiple_of(16);
    }
    let total = at;

    let mut d = vec![0u8; total];
    d[0x00..0x04].copy_from_slice(b"plgx");
    put16(&mut d, 0x04, 0x1000);
    put16(&mut d, 0x08, 1); // images
    put16(&mut d, 0x0A, n as u16);
    put16(&mut d, 0x0E, 1); // scenarios
    put32(&mut d, 0x10, total as u32);
    put16(&mut d, 0x16, 1);
    d[0x1E] = 0x85;
    // The mchunk count. It has to agree with the mchunk table's length: a package that
    // declares two while carrying one leaves the console reading a phantom pair out of the
    // record that follows, and it refuses to transfer.
    put16(&mut d, 0x20, mchunks.len() as u16);
    d[0x24] = 0x01;
    d[0x30] = 0x11;
    d[0x38..0x40].fill(0xFF);
    d[0x40..0x40 + content_id.len()].copy_from_slice(content_id.as_bytes());
    for (i, (offset, size)) in offsets.iter().zip(sizes).enumerate() {
        put32(&mut d, 0xC0 + i * 8, *offset as u32);
        put32(&mut d, 0xC4 + i * 8, size as u32);
    }

    let mut list_at = 0u32;
    for (k, owned) in chunks.iter().enumerate() {
        let r = offsets[0] + k * 0x20;
        d[r] = 0x80;
        d[r + 2] = 0x03;
        put32(&mut d, r + 4, owned.len() as u32);
        d[r + 8] = 0x11;
        put64(&mut d, r + 0x10, u64::MAX);
        put32(&mut d, r + 0x18, list_at);
        put32(&mut d, r + 0x1C, label_at[k]);
        list_at += owned.len() as u32 * 4;
    }
    for (i, m) in mchunk_list.iter().enumerate() {
        put32(&mut d, offsets[1] + i * 4, *m);
    }
    d[offsets[2]..offsets[2] + chunk_labels.len()].copy_from_slice(&chunk_labels);
    for (i, (offset, size)) in mchunks.iter().enumerate() {
        put64(&mut d, offsets[3] + i * 0x10, *offset);
        put64(&mut d, offsets[3] + i * 0x10 + 8, *size);
    }
    // One scenario: every chunk, the first one initial.
    let s = offsets[4];
    d[s] = 0x21;
    put16(&mut d, s + 0x14, 1);
    put16(&mut d, s + 0x16, n as u16);
    for k in 0..n {
        put16(&mut d, offsets[5] + k * 2, k as u16);
    }
    d[offsets[6]..offsets[6] + scenario_label.len()].copy_from_slice(scenario_label);
    d
}

/// `playgo-hash-table.dat`: a `\x7FFLT` header and the sorted path hashes.
fn hash_table(entries: &[(u64, u16)]) -> Vec<u8> {
    let n = entries.len();
    let mut d = vec![0u8; 0x38 + n * 8];
    put32(&mut d, 0x00, 1);
    put32(&mut d, 0x04, 0x0800_0000);
    put32(&mut d, 0x08, 0x38);
    put32(&mut d, 0x0C, (n * 8) as u32);
    d[0x18..0x1C].copy_from_slice(&[0x7F, b'F', b'L', b'T']);
    put32(&mut d, 0x24, n as u32);
    d[0x28..0x38].copy_from_slice(&flt::HEADER_SEED);
    for (i, (hash, _)) in entries.iter().enumerate() {
        put64(&mut d, 0x38 + i * 8, *hash);
    }
    d
}

/// `playgo-ficm.dat`: a 16-byte header and a `u16` chunk id per hash-table entry.
fn ficm(entries: &[(u64, u16)]) -> Vec<u8> {
    let mut d = vec![0u8; 0x10 + entries.len() * 2];
    put32(&mut d, 0x00, 1);
    put32(&mut d, 0x08, 0x10);
    put32(&mut d, 0x0C, (entries.len() * 2) as u32);
    for (i, (_, chunk)) in entries.iter().enumerate() {
        put16(&mut d, 0x10 + i * 2, *chunk);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "UP0000-PPSA01234_00-TESTGAME00000000";

    fn files() -> Vec<(String, u64, u64)> {
        vec![
            ("eboot.bin".into(), BLOCK, 5 * BLOCK),
            ("data/a.bin".into(), 7 * BLOCK, 30 * BLOCK),
            ("data/b.bin".into(), 40 * BLOCK, 60 * BLOCK),
            ("sce_sys/keystone".into(), 101 * BLOCK, 96),
        ]
    }

    /// One chunk is byte for byte the shape the installing packages carry: 400 bytes, one
    /// mchunk over the whole image, empty labels.
    #[test]
    fn one_chunk_keeps_the_installing_shape() {
        let p = build(ID, &files(), 0x20_0000, 1).unwrap();
        let d = &p.chunk_dat;
        assert_eq!(d.len(), 0x190);
        assert_eq!(&d[0..4], b"plgx");
        assert_eq!(d[0x20], 1);
        let table: Vec<(u32, u32)> = (0..7)
            .map(|i| {
                let at = 0xC0 + i * 8;
                (
                    u32::from_le_bytes(d[at..at + 4].try_into().unwrap()),
                    u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()),
                )
            })
            .collect();
        assert_eq!(
            table,
            [
                (0x100, 0x20),
                (0x120, 4),
                (0x130, 1),
                (0x140, 0x10),
                (0x150, 0x20),
                (0x170, 2),
                (0x180, 1)
            ]
        );
        assert_eq!(&d[0x148..0x150], &0x20_0000u64.to_le_bytes());
        assert_eq!(p.mchunks, [(0, 0x20_0000)]);
        assert!(p.ficm[0x10..].iter().all(|b| *b == 0));
    }

    #[test]
    fn many_chunks_slice_the_data_and_chunk_zero_takes_the_tail() {
        let cnt = 120 * BLOCK;
        let p = build(ID, &files(), cnt, 4).unwrap();
        // Data ends at block 102 (keystone's tail rounded up): 102 units over 4 chunks.
        let sizes: Vec<u64> = p.mchunks.iter().map(|m| m.1 / BLOCK).collect();
        assert_eq!(sizes, [26, 26, 25, 25, 18]);
        assert_eq!(p.chunks[0], [0, 4]);
        assert_eq!(p.chunks[3], [3]);
        let covered: u64 = p.mchunks.iter().map(|m| m.1).sum();
        assert_eq!(
            covered, cnt,
            "the mchunks tile the image up to the container"
        );
        // Each file's chunk is where its data starts, listed in hash order.
        let mut want: Vec<(u64, u16)> = files()
            .iter()
            .map(|(path, at, _)| {
                let unit = at / BLOCK;
                let chunk = match unit {
                    0..=25 => 0,
                    26..=51 => 1,
                    52..=76 => 2,
                    _ => 3,
                };
                (flt::hash_path(path), chunk)
            })
            .collect();
        want.sort();
        for (i, (hash, chunk)) in want.iter().enumerate() {
            let at = 0x38 + i * 8;
            assert_eq!(&p.hash_table[at..at + 8], &hash.to_le_bytes());
            let at = 0x10 + i * 2;
            assert_eq!(&p.ficm[at..at + 2], &chunk.to_le_bytes());
        }
        assert_eq!(p.chunk_size(0), 26 * BLOCK + 18 * BLOCK);
    }

    #[test]
    fn a_small_image_gets_fewer_chunks() {
        let p = build(ID, &[("eboot.bin".into(), 0, 3 * BLOCK)], 10 * BLOCK, 100).unwrap();
        assert_eq!(p.chunks.len(), 3);
        assert!(build(ID, &files(), BLOCK, 0).is_err());
        assert!(build(ID, &files(), BLOCK, 256).is_err());
    }

    #[test]
    fn one_chunk_matches_the_installing_samples() {
        // The third-party profile: one chunk, one mchunk spanning the whole mount image,
        // and the container's own offset as its size (crimson: 0x80000 against a 0x80000
        // container).
        let chunk = build(
            "IV9999-WEBB00002_00-XXXXXXXXXXXXXXXX",
            &[("eboot.bin".into(), BLOCK, 5)],
            0xC0000,
            1,
        )
        .unwrap()
        .chunk_dat;
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
    }

    /// The Publishing Tools package's own `playgo-chunk.dat`, regenerated from its content id
    /// and its data span alone. Skips when the sample is absent.
    #[test]
    fn reproduces_a_publishing_tools_chunk_table() {
        let Ok(dir) = std::env::var("PS5UPLOAD_PLAYGO_SAMPLE") else {
            eprintln!(
                "skip: set PS5UPLOAD_PLAYGO_SAMPLE to a folder with a 64-chunk playgo-chunk.dat"
            );
            return;
        };
        let want = std::fs::read(std::path::Path::new(&dir).join("playgo-chunk.dat")).unwrap();
        let id = std::str::from_utf8(&want[0x40..0x64]).unwrap();
        // Recover the data span and container offset from the sample's own mchunk table.
        let n = u16::from_le_bytes(want[0x0A..0x0C].try_into().unwrap()) as usize;
        let at = u32::from_le_bytes(want[0xD8..0xDC].try_into().unwrap()) as usize;
        let span = |i: usize| {
            let r = at + i * 16;
            (
                u64::from_le_bytes(want[r..r + 8].try_into().unwrap()),
                u64::from_le_bytes(want[r + 8..r + 16].try_into().unwrap()),
            )
        };
        let (tail_at, tail_len) = span(n);
        let files = vec![("data".to_string(), 0u64, tail_at)];
        let got = build(id, &files, tail_at + tail_len, n as u16).unwrap();
        assert_eq!(got.chunk_dat, want);
    }
}
