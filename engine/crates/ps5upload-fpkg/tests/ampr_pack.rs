//! The AMPR pack port against drakmor's own tool: `tests/fixtures/ampr_pack` holds what
//! `ampr_pack.py` (drakmor/ampr_emu) and `build_ampr_index.py` wrote for a small tree, with
//! the profiles they were given. The tree itself is regenerated here from the same formulas.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::format::{Manifest, RuntimeSettings};
use ps5upload_fpkg::ampr_pack::{self, Config, Control, Reader};
use ps5upload_fpkg::source::FolderSource;

const MTIME: i64 = 1_700_000_000;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ampr_pack")
}

fn xorshift(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// The tree the fixtures were packed from, every file at mtime 1700000000.
fn source_files() -> Vec<(&'static str, Vec<u8>)> {
    let text: Vec<u8> = (0..2000)
        .flat_map(|i| format!("line {i} of the fixture\n").into_bytes())
        .collect();
    let mut dup: Vec<u8> = (0..512).flat_map(|_| 0..=255u8).collect();
    dup.extend_from_slice(b"tail");
    let mut eboot = b"\x7fELF".to_vec();
    eboot.resize(104, 0);
    vec![
        ("eboot.bin", eboot),
        ("d/text.bin", text),
        ("d/zeros.bin", vec![0; 70000]),
        ("d/empty.bin", Vec::new()),
        ("d/dup.bin", dup),
        (
            "d/hot/a.cfg",
            (0..800)
                .flat_map(|i| format!("key{i}=value{}\n", i % 7).into_bytes())
                .collect(),
        ),
        ("d/noise.bin", xorshift(8000, 0x1234_5678)),
        (
            "movies/m.bk2",
            (0..100_000u64).map(|i| ((i * 7919) % 251) as u8).collect(),
        ),
    ]
}

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("fpkg-ampr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_source(root: &Path) {
    for (rel, data) in source_files() {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
}

fn profile(name: &str) -> Config {
    Config::from_toml(
        &std::fs::read_to_string(fixture().join(format!("{name}.toml"))).unwrap(),
        None,
    )
    .unwrap()
}

/// Our `ampr_emu.index` for the tree is byte-identical to `build_ampr_index.py`'s.
#[test]
fn our_index_matches_drakmors() {
    let theirs = std::fs::read(fixture().join("ampr_emu.index")).unwrap();
    let entries: Vec<ampr_index::Entry> = source_files()
        .into_iter()
        .map(|(p, d)| ampr_index::Entry {
            path: p.to_string(),
            size: d.len() as u64,
            mtime: MTIME,
        })
        .collect();
    assert_eq!(ampr_index::build_entries(&entries).unwrap(), theirs);
}

/// Our reader opens drakmor's pack set, checks its headers and CRCs, and decodes every packed
/// file to the source bytes; the loose file is recorded loose.
#[test]
fn our_reader_decodes_drakmors_packs() {
    let files = source_files();
    for set in ["compress", "store"] {
        let dir = fixture().join(set);
        let mut tree = FolderSource::open(&dir).unwrap();
        let reader = Reader::open(&mut tree, "ampr_assets.index").unwrap();
        assert!(reader.has_crcs(), "{set}");
        let m = &reader.manifest;
        assert_eq!(m.files.len(), files.len(), "{set}");
        for (rel, data) in &files {
            let i = reader.find(rel).unwrap_or_else(|| panic!("{set}: {rel}"));
            if *rel == "eboot.bin" {
                assert!(!m.files[i].packed(), "{set}: eboot.bin stays loose");
                continue;
            }
            assert_eq!(
                &reader.read_file(i, &mut tree, "").unwrap(),
                data,
                "{set}: {rel}"
            );
        }
    }
    let rt = std::fs::read(fixture().join("compress/ampr_assets.index.runtime")).unwrap();
    let m = Manifest::parse(&std::fs::read(fixture().join("compress/ampr_assets.index")).unwrap())
        .unwrap();
    let settings = RuntimeSettings::parse(&rt, &m.build_id).unwrap();
    assert_eq!(settings.workers, 2);
    assert_eq!(settings.decoded_cache_bytes, 64 << 20);
}

fn pack(name: &str, out: &Path) -> ampr_pack::Output {
    let src = Temp::new(&format!("{name}-src"));
    write_source(&src.0);
    let entries =
        ampr_index::parse(&std::fs::read(fixture().join("ampr_emu.index")).unwrap()).unwrap();
    let mut tree = FolderSource::open(&src.0).unwrap();
    let output = ampr_pack::build(
        &entries,
        &mut tree,
        &profile(name),
        out,
        &mut Control::default(),
    )
    .unwrap();
    output.write_small_files(out).unwrap();
    output
}

/// With every block stored raw, nothing depends on the LZ4 encoder: our pack set is drakmor's,
/// byte for byte — manifest, CRC sidecar and every volume.
#[test]
fn an_all_store_profile_gives_drakmors_bytes() {
    let out = Temp::new("store-out");
    pack("store", &out.0);
    let theirs = fixture().join("store");
    let mut names: Vec<String> = std::fs::read_dir(&theirs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut ours: Vec<String> = std::fs::read_dir(&out.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ours.sort();
    assert_eq!(ours, names);
    for name in names {
        assert!(
            std::fs::read(theirs.join(&name)).unwrap() == std::fs::read(out.0.join(&name)).unwrap(),
            "{name} differs from drakmor's"
        );
    }
}

/// With LZ4 the stored bytes come from a different encoder, but the plan does not: the same
/// files packed, with the same flags, block sizes, chunk counts and ranges, codecs, volumes.
#[test]
fn a_compress_profile_plans_what_drakmor_plans() {
    let out = Temp::new("compress-out");
    pack("compress", &out.0);
    let read = |dir: &Path| {
        Manifest::parse(&std::fs::read(dir.join("ampr_assets.index")).unwrap()).unwrap()
    };
    let theirs = read(&fixture().join("compress"));
    let ours = read(&out.0);
    assert_eq!(ours.files.len(), theirs.files.len());
    for (i, (a, b)) in ours.files.iter().zip(&theirs.files).enumerate() {
        assert_eq!(ours.path(i).unwrap(), theirs.path(i).unwrap());
        assert_eq!(a, b, "file record {}", ours.path(i).unwrap());
    }
    assert_eq!(ours.chunks.len(), theirs.chunks.len());
    for (a, b) in ours.chunks.iter().zip(&theirs.chunks) {
        assert_eq!((a.codec, a.flags, a.pack_id), (b.codec, b.flags, b.pack_id));
    }
    assert_eq!(ours.packs.len(), theirs.packs.len());
    for i in 0..ours.packs.len() {
        assert_eq!(ours.pack_name(i).unwrap(), theirs.pack_name(i).unwrap());
    }
    // And it reads back.
    let mut tree = FolderSource::open(&out.0).unwrap();
    let reader = Reader::open(&mut tree, "ampr_assets.index").unwrap();
    for (rel, data) in source_files().iter().skip(1) {
        let i = reader.find(rel).unwrap();
        assert_eq!(&reader.read_file(i, &mut tree, "").unwrap(), data, "{rel}");
    }
}
