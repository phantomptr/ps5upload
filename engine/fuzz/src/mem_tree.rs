//! A game tree held in memory, and the input shapes of the targets that fuzz several files at
//! once (shared with the seed generator through `#[path]`). After ps5-dump-forge's
//! `fuzz/src/lz4_tree.rs`.

use std::collections::BTreeMap;

use ps5upload_fpkg::source::{SourceFile, SourceTree};
use ps5upload_fpkg::{Error, Result};

pub struct Mem {
    files: Vec<SourceFile>,
    data: BTreeMap<String, Vec<u8>>,
}

impl Mem {
    pub fn new(entries: impl IntoIterator<Item = (String, Vec<u8>)>) -> Self {
        let data: BTreeMap<String, Vec<u8>> = entries.into_iter().collect();
        let files = data
            .iter()
            .map(|(p, d)| SourceFile {
                path: p.clone(),
                size: d.len() as u64,
            })
            .collect();
        Self { files, data }
    }
}

impl SourceTree for Mem {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        self.data
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Format(format!("no {path}")))
    }
    fn describe(&self) -> String {
        "memory".into()
    }
}

/// The loose file the pack seeds list beside their packed ones.
pub const LOOSE_EBOOT: (&str, u8, usize) = ("eboot.bin", 7, 100);

/// The files of an LZ4 pack set, for [`pack_tree`]: manifest, `.crc` sidecar, volume 0.
pub const LZ4_NAMES: [&str; 3] = [
    "ampr_assets.index",
    "ampr_assets.index.crc",
    "ampr_assets-000.pak",
];
/// The files of a zstd pack set: index, volume 1 (there is no sidecar), volume 0.
pub const ZSTD_NAMES: [&str; 3] = ["ampr_zstd.index", "ampr_zstd-001.pak", "ampr_zstd-000.pak"];

/// Split one input into `n` parts: `[len u32 LE][bytes]` for each of the first `n - 1`, the
/// rest is the last. A length past the end takes what is left.
pub fn split(data: &[u8], n: usize) -> Vec<&[u8]> {
    let mut rest = data;
    let mut out = Vec::with_capacity(n);
    for _ in 1..n {
        let len = rest
            .get(..4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        rest = rest.get(4..).unwrap_or(&[]);
        let (part, tail) = rest.split_at(len.min(rest.len()));
        out.push(part);
        rest = tail;
    }
    out.push(rest);
    out
}

/// The inverse of [`split`], for seeds.
pub fn join(parts: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    let (last, head) = parts.split_last().expect("at least one part");
    for p in head {
        v.extend((p.len() as u32).to_le_bytes());
        v.extend_from_slice(p);
    }
    v.extend_from_slice(last);
    v
}

/// The tree of a pack-set input: `names` are the manifest, its sidecar and the first volume, in
/// that order, each present when its part is not empty; plus the loose `eboot.bin`.
pub fn pack_tree(data: &[u8], names: [&str; 3]) -> Mem {
    let (name, byte, len) = LOOSE_EBOOT;
    let mut entries = vec![(name.to_string(), vec![byte; len])];
    for (name, part) in names.iter().zip(split(data, 3)) {
        if !part.is_empty() {
            entries.push((name.to_string(), part.to_vec()));
        }
    }
    Mem::new(entries)
}
