//! AMPR seekable LZ4 asset packs: a game's files stored as independent LZ4 blocks in a few
//! large volumes, which `ampr_emu` 0.4 and later (a backport's `fakelib/libSceAmpr.sprx`) serves
//! back to the title as the original files. The game image gets smaller; the packed originals
//! are not shipped.
//!
//! The format and the packing rules are drakmor's (drakmor/ampr_emu, GPL-3:
//! `tools/ampr_pack.py`, `tools/ampr_pack_format.py`, `docs/ASSET_PACKS.md`); this is a port.
//! Files in a pack set, next to `ampr_emu.index` at `/app0`:
//!
//! ```text
//! ampr_assets.index          AMPRPAK4 manifest: 128-byte header; a 48-byte file record per
//!                            AMPRIDX3 entry (file id = record + 1), packed or loose; 12-byte
//!                            chunk records (48-bit offset, 16-bit volume, 20-bit size − 1,
//!                            2-bit codec, 8 flag bits); 32-byte volume records; strings.
//!                            CRC-32 over the header and over the payload.
//! ampr_assets-NNN.pak        AMPRDAT3 volumes: 64-byte header, payload from the first I/O
//!                            page (64 KiB), size a whole number of pages.
//! ampr_assets.index.crc      AMPRCRC1: one CRC-32 per decoded chunk; offline checks only.
//! ampr_assets.index.runtime  AMPRCFG1: cache sizes and workers, when the profile sets them.
//! ```
//!
//! A chunk is one block of a file (16 KiB to 1 MiB, 64 KiB by default) stored as a raw LZ4
//! block, or raw when LZ4 does not save enough. Outside streaming files a chunk never straddles
//! an I/O page unless it is page-sized and page-aligned. Identical blocks are stored once per
//! lane. The build id, which every header carries, is a hash of the configuration and records,
//! so the same files and profile give the same set from this packer and from drakmor's.
//!
//! The runtime intercepts AMPR asset reads (and, in its PackedStdio build, ordinary read-only
//! file reads) but never `mmap`: executables and modules have to stay loose.

pub mod config;
pub mod format;
pub mod glob;
pub mod image;
pub mod lz4;
pub mod pack;
pub mod read;

pub use config::{default_profile, Config};
pub use pack::{build, Control, Output};
pub use read::Reader;

/// Does this `libSceAmpr.sprx` serve asset packs, and which version is it? Read from the
/// module's own strings: the manifest magic it checks and the version it logs at load
/// (`[AMPR_EMU] name=libSceAmpr version=0.4.2.1 (c) Drakmor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSupport {
    pub packs: bool,
    pub version: Option<String>,
}

pub fn runtime_support(module: &[u8]) -> RuntimeSupport {
    let has = |needle: &[u8]| module.windows(needle.len()).any(|w| w == needle);
    let packs = has(format::INDEX_MAGIC) && has(b"ampr_assets");
    let marker = b" (c) Drakmor";
    let version = module
        .windows(marker.len())
        .position(|w| w == marker)
        .and_then(|end| {
            let start = module[..end]
                .iter()
                .rposition(|&b| !(b.is_ascii_digit() || b == b'.'))
                .map_or(0, |p| p + 1);
            let v = std::str::from_utf8(&module[start..end]).ok()?;
            (!v.is_empty() && v.contains('.')).then(|| v.to_string())
        });
    RuntimeSupport { packs, version }
}

#[cfg(test)]
mod tests {
    use super::config::{Action, Layout, Rule};
    use super::format::*;
    use super::*;
    use crate::ampr_index::Entry;
    use crate::source::{SourceFile, SourceTree};
    use std::path::{Path, PathBuf};

    struct Mem {
        files: Vec<SourceFile>,
        data: std::collections::HashMap<String, Vec<u8>>,
    }

    impl Mem {
        fn new(items: &[(&str, Vec<u8>)]) -> Self {
            Self {
                files: items
                    .iter()
                    .map(|(p, d)| SourceFile {
                        path: p.to_string(),
                        size: d.len() as u64,
                    })
                    .collect(),
                data: items
                    .iter()
                    .map(|(p, d)| (p.to_string(), d.clone()))
                    .collect(),
            }
        }

        fn entries(&self) -> Vec<Entry> {
            let index = crate::ampr_index::build(
                &self
                    .files
                    .iter()
                    .map(|f| (f.path.clone(), f.size))
                    .collect::<Vec<_>>(),
                1_700_000_000,
            )
            .unwrap();
            crate::ampr_index::parse(&index).unwrap()
        }
    }

    impl SourceTree for Mem {
        fn files(&self) -> &[SourceFile] {
            &self.files
        }
        fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
            self.data
                .get(path)
                .cloned()
                .ok_or_else(|| crate::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "memory".into()
        }
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "ampr-pack-{}-{name}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn text(n: usize) -> Vec<u8> {
        (0..n).map(|i| b"the quick brown fox "[i % 20]).collect()
    }

    fn pack(tree: &mut Mem, config: &Config, dir: &Path) -> Output {
        let entries = tree.entries();
        let out = build(&entries, tree, config, dir, &mut Control::default()).unwrap();
        out.write_small_files(dir).unwrap();
        out
    }

    /// Reads every packed file back through the reader and compares it with the source.
    fn round_trip(tree: &mut Mem, out: &Output, dir: &Path) {
        let mut folder = crate::source::FolderSource::open(dir).unwrap();
        let reader = Reader::open(&mut folder, &out.index_name).unwrap();
        assert!(reader.has_crcs());
        for p in &out.packed {
            let i = reader.find(p).unwrap();
            let got = reader.read_file(i, &mut folder, "").unwrap();
            assert_eq!(got, tree.read(p).unwrap(), "{p}");
        }
    }

    fn compress_all(block_shift: u8) -> Config {
        let mut c = Config {
            workers: 3,
            ..Config::default()
        };
        let mut r = c.default_rule();
        r.action = Action::Compress;
        r.block_shift = block_shift;
        r.layout = Layout::Mixed;
        c.rules.push(r);
        c
    }

    #[test]
    fn a_pack_set_round_trips_and_validates() {
        let dir = Dir::new("rt");
        let mut tree = Mem::new(&[
            ("d/text.bin", text(300_000)),
            ("d/noise.bin", noise(200_000, 7)),
            ("d/empty.bin", Vec::new()),
            ("d/tiny.bin", b"hello".to_vec()),
            ("eboot.bin", noise(5000, 3)),
        ]);
        let mut config = compress_all(16);
        config.rules.push(Rule {
            action: Action::Loose,
            include: vec!["eboot.bin".into()],
            ..config.default_rule()
        });
        let out = pack(&mut tree, &config, &dir.0);
        let m = Manifest::parse(&out.manifest).unwrap();
        assert_eq!(m.files.len(), 5);
        assert_eq!(out.loose, ["eboot.bin"]);
        assert_eq!(out.packed.len(), 4);
        // A 0-byte file is packed with no chunks.
        let empty = (0..5)
            .find(|&i| m.path(i).unwrap() == "/app0/d/empty.bin")
            .unwrap();
        let e = &m.files[empty];
        assert!(e.packed() && e.chunk_count == 0 && e.logical_size == 0);
        // Text compresses; noise falls back to raw.
        assert!(out.stats.chunks_lz4 > 0 && out.stats.chunks_raw > 0);
        let noise_at = (0..5)
            .find(|&i| m.path(i).unwrap() == "/app0/d/noise.bin")
            .unwrap();
        let n = &m.files[noise_at];
        for k in 0..n.chunk_count {
            assert_eq!(m.chunks[(n.first_chunk + k) as usize].codec, CODEC_RAW);
        }
        round_trip(&mut tree, &out, &dir.0);
    }

    /// Small chunks share a page but never cross into the next; full-size raw chunks start on a
    /// page. Every chunk is flagged accordingly and the manifest validator agrees.
    #[test]
    fn chunks_are_placed_page_safely() {
        let dir = Dir::new("page");
        let mut items = Vec::new();
        for i in 0..40 {
            // Compressible small files of assorted sizes, so packed chunks land mid-page.
            let mut d = text(3000 + i * 997);
            d.extend(noise(1500 + i * 311, i as u32));
            items.push((format!("d/f{i:02}.bin"), d));
        }
        items.push(("d/raw.bin".into(), noise(3 * 65536 + 100, 99)));
        let borrowed: Vec<(&str, Vec<u8>)> =
            items.iter().map(|(p, d)| (p.as_str(), d.clone())).collect();
        let mut tree = Mem::new(&borrowed);
        let out = pack(&mut tree, &compress_all(16), &dir.0);
        let m = Manifest::parse(&out.manifest).unwrap();
        let page = 65536u64;
        let mut shared_page = false;
        for c in &m.chunks {
            let size = u64::from(c.stored_size);
            assert_eq!(c.offset % 64, 0);
            if size < page {
                assert_eq!(c.offset / page, (c.offset + size - 1) / page, "{c:?}");
                assert_ne!(c.flags & CHUNK_PAGE_CONTAINED, 0);
                shared_page |= c.offset % page != 0;
            } else {
                assert_eq!(c.offset % page, 0, "{c:?}");
                assert_ne!(c.flags & CHUNK_PAGE_ALIGNED, 0);
            }
        }
        assert!(shared_page, "small chunks pack densely within pages");
        // Volume sizes are whole pages and the payload starts on the first page boundary.
        for p in &m.packs {
            assert_eq!(p.file_size % page, 0);
            assert_eq!(p.file_size - p.payload_bytes, page);
        }
        round_trip(&mut tree, &out, &dir.0);
    }

    /// Volumes get ids in the order they are first written, and a lane rolls over to a new
    /// volume at its size cap; the assignment balances bytes across lanes, largest first.
    #[test]
    fn pack_ids_follow_first_write_order() {
        let dir = Dir::new("lanes");
        let mut tree = Mem::new(&[
            ("a/big.bin", noise(600_000, 1)),
            ("b/mid.bin", noise(300_000, 2)),
            ("c/small.bin", noise(200_000, 3)),
            ("d/small2.bin", noise(150_000, 4)),
        ]);
        let mut c = compress_all(16);
        let mut g = config::Group::new("assets");
        g.pack_count = 2;
        g.max_pack_size = 512 << 10;
        c.groups.insert("assets".into(), g);
        c.rules[0].group = "assets".into();
        c.pack_pattern = "ampr_assets-{group}-lane{lane:02d}-vol{volume:02d}-{id:03d}.pak".into();
        let out = pack(&mut tree, &c, &dir.0);
        let m = Manifest::parse(&out.manifest).unwrap();
        // a/big (largest) takes lane 0, b/mid lane 1, then c/small and d/small2 lane 1 (lighter).
        // a/big is first in file order, so lane 0's first volume is pack 0.
        let names: Vec<String> = (0..m.packs.len())
            .map(|i| m.pack_name(i).unwrap().to_string())
            .collect();
        assert_eq!(names[0], "ampr_assets-assets-lane00-vol00-000.pak");
        assert!(
            names.iter().any(|n| n.contains("lane00-vol01")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("lane01-vol00")),
            "{names:?}"
        );
        let mut first_use = Vec::new();
        for ch in &m.chunks {
            if !first_use.contains(&ch.pack_id) {
                first_use.push(ch.pack_id);
            }
        }
        let mut sorted = first_use.clone();
        sorted.sort();
        assert_eq!(first_use, sorted, "pack ids in first-write order");
        for p in &m.packs {
            assert!(p.file_size <= 512 << 10);
        }
        round_trip(&mut tree, &out, &dir.0);
    }

    #[test]
    fn identical_blocks_are_stored_once() {
        let dir = Dir::new("dedupe");
        let block = text(65536);
        let mut twice = block.clone();
        twice.extend_from_slice(&block);
        let mut tree = Mem::new(&[("d/a.bin", block.clone()), ("d/b.bin", twice)]);
        let out = pack(&mut tree, &compress_all(16), &dir.0);
        let m = Manifest::parse(&out.manifest).unwrap();
        assert_eq!(out.stats.chunks, 3);
        assert_eq!(out.stats.chunks_shared, 2);
        assert!(
            m.chunks
                .iter()
                .filter(|c| c.flags & CHUNK_SHARED != 0)
                .count()
                == 2
        );
        round_trip(&mut tree, &out, &dir.0);
    }

    /// A store-only rule never compresses, and a streaming rule lays chunks densely.
    #[test]
    fn store_and_streaming_rules_are_honoured() {
        let dir = Dir::new("store");
        let mut tree = Mem::new(&[("m/movie.bin", text(400_000)), ("d/a.bin", text(1000))]);
        let mut c = compress_all(16);
        c.rules.push(Rule {
            action: Action::Store,
            include: vec!["m/**".into()],
            block_shift: 18,
            layout: Layout::Streaming,
            ..c.default_rule()
        });
        let out = pack(&mut tree, &c, &dir.0);
        let m = Manifest::parse(&out.manifest).unwrap();
        let i = (0..2)
            .find(|&i| m.path(i).unwrap() == "/app0/m/movie.bin")
            .unwrap();
        let f = &m.files[i];
        assert_ne!(f.flags & FILE_STORE_ONLY, 0);
        assert_ne!(f.flags & FILE_STREAMING, 0);
        assert_eq!(f.chunk_count, 2);
        for k in 0..f.chunk_count {
            let ch = m.chunks[(f.first_chunk + k) as usize];
            assert_eq!(ch.codec, CODEC_RAW);
            assert_ne!(ch.flags & CHUNK_STREAMING, 0);
        }
        round_trip(&mut tree, &out, &dir.0);
    }

    /// A large file LZ4 cannot shrink is left loose, unless the rule forces it in.
    #[test]
    fn a_large_incompressible_file_stays_loose() {
        let dir = Dir::new("auto");
        let mut tree = Mem::new(&[
            ("d/archive.bin", noise(3 << 20, 5)),
            ("d/t.bin", text(5000)),
        ]);
        let mut c = compress_all(16);
        c.auto_loose_min_file_size = 1 << 20;
        let out = pack(&mut tree, &c, &dir.0);
        assert_eq!(out.loose, ["d/archive.bin"]);
        assert_eq!(out.stats.files_auto_loose, 1);
        assert!(out.warnings[0].starts_with("auto-loose: d/archive.bin"));

        let dir2 = Dir::new("auto2");
        c.rules[0].force_pack = true;
        let out = pack(&mut tree, &c, &dir2.0);
        assert!(out.loose.is_empty());
        round_trip(&mut tree, &out, &dir2.0);
    }

    #[test]
    fn a_size_mismatch_with_the_index_is_refused_and_leaves_nothing() {
        let dir = Dir::new("mismatch");
        let mut tree = Mem::new(&[("d/a.bin", text(100_000))]);
        let mut entries = tree.entries();
        entries[0].size += 1;
        let e = build(
            &entries,
            &mut tree,
            &compress_all(16),
            &dir.0,
            &mut Control::default(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("the index says"), "{e}");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[test]
    fn a_damaged_volume_or_manifest_is_caught() {
        let dir = Dir::new("damage");
        let mut tree = Mem::new(&[("d/a.bin", text(200_000))]);
        let out = pack(&mut tree, &compress_all(16), &dir.0);
        let mut bad = out.manifest.clone();
        let n = bad.len();
        bad[n - 3] ^= 0xFF;
        assert!(Manifest::parse(&bad).is_err());
        // Flip a byte inside the first chunk: its CRC catches it.
        let (_, path) = &out.volumes[0];
        let mut v = std::fs::read(path).unwrap();
        v[65536 + 10] ^= 0xFF;
        std::fs::write(path, v).unwrap();
        let mut folder = crate::source::FolderSource::open(&dir.0).unwrap();
        let reader = Reader::open(&mut folder, "ampr_assets.index").unwrap();
        assert!(reader.read_file(0, &mut folder, "").is_err());
    }

    #[test]
    fn the_runtime_module_is_recognised_by_its_strings() {
        let mut module = vec![0u8; 4096];
        module[100..108].copy_from_slice(b"AMPRPAK4");
        module[200..217].copy_from_slice(b"ampr_assets.index");
        let v = b"\x000.4.2.1 (c) Drakmor\0";
        module[300..300 + v.len()].copy_from_slice(v);
        let s = runtime_support(&module);
        assert!(s.packs);
        assert_eq!(s.version.as_deref(), Some("0.4.2.1"));
        let old = runtime_support(b"\x000.3.1 (c) Drakmor\0 AMPRIDX3");
        assert!(!old.packs);
        assert_eq!(old.version.as_deref(), Some("0.3.1"));
    }
}
