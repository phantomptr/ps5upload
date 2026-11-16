//! Seed corpora for the fuzz targets: small valid inputs per target, built with the engine's own
//! writers and its test fixtures, so the fuzzer starts past every magic number and checksum.
//!
//!   cargo run --release -p ps5upload-fuzz-seedgen -- corpus      (from engine/fuzz)
//!
//! Images are stored in the sparse encoding of `src/sparse.rs`. After ps5-dump-forge's
//! `fuzz/seedgen/src/main.rs`.

#[path = "../../src/sparse.rs"]
#[allow(dead_code)]
mod sparse;

#[path = "../../src/mem_tree.rs"]
#[allow(dead_code)]
mod mem_tree;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::kraken::{self, Half};
use ps5upload_fpkg::kraken_image::{self, KrakenImage, Owner, StoredBlock};
use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{ampr_index, ampr_pack, exfat_write, ffpfsc, source};

type Res<T> = Result<T, Box<dyn std::error::Error>>;
type KrakenSample = (Vec<u8>, Vec<u8>, Vec<Vec<u8>>);

const TIME: i64 = 1_700_000_000;
const PARAM_JSON: &str = r#"{"contentId":"UP0000-PPSA01234_00-FUZZSEED00000000","contentVersion":"01.001.000","titleId":"PPSA01234","applicationCategoryType":0,"attributePub":0,"applicationDrmType":"standard"}"#;

fn main() -> Res<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: seedgen <corpus-dir>")?,
    );
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/ps5upload-fpkg/tests/fixtures");
    let work = std::env::temp_dir().join(format!("ps5upload-seedgen-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let result = run(&out, &fixtures, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

fn run(out: &Path, fixtures: &Path, work: &Path) -> Res<()> {
    let game = fixture(&work.join("game"))?;
    let cancel = AtomicBool::new(false);
    let mini = fixtures.join("mini.exfat");

    // exFAT: the checked-in fixture (4 KiB clusters, AppleDouble junk) and one from our writer.
    seed(out, "exfat", "mini", &sparse::encode(&fs::read(&mini)?))?;
    let exfat = work.join("game.exfat");
    {
        let mut tree = source::open(&game)?;
        exfat_write::build_exfat(&mut *tree, &exfat, None, &mut |_, _| {}, &|| false)?;
    }
    seed(out, "exfat", "built", &sparse::encode(&fs::read(&exfat)?))?;

    // UFS2 (.ffpkg), from the same tree.
    let ffpkg = work.join("game.ffpkg");
    {
        let mut tree = source::open(&game)?;
        let layout = ps5_dump_forge_ufs2::plan(&*tree, &Default::default(), &cancel)?;
        let mut file = fs::File::create(&ffpkg)?;
        ps5_dump_forge_ufs2::write(&mut *tree, &layout, &mut file, &cancel, &mut |_, _| {})?;
    }
    seed(out, "ufs2", "built", &sparse::encode(&fs::read(&ffpkg)?))?;

    // .ffpfs from the same tree, .ffpfsc around our exFAT and .ffpfs of it, and the fixtures.
    let ffpfs = work.join("game.ffpfs");
    {
        let mut tree = source::open(&game)?;
        let opts = ps5_dump_forge_pfs::Options { time: Some(TIME) };
        let layout = ps5_dump_forge_pfs::plan(&*tree, &opts, &cancel)?;
        let mut file = fs::File::create(&ffpfs)?;
        ps5_dump_forge_pfs::write(&mut *tree, &layout, &mut file, &cancel, &mut |_, _| {})?;
    }
    seed(out, "pfs", "built", &sparse::encode(&fs::read(&ffpfs)?))?;
    for (inner, image) in [("exfat", &exfat), ("ffpfs", &ffpfs)] {
        let len = fs::metadata(image)?.len();
        let wrapped = work.join(format!("game-{inner}.ffpfsc"));
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&wrapped)?;
        let opts = ps5_dump_forge_pfs::WrapOptions {
            threads: 1,
            time: Some(TIME),
            ..ps5_dump_forge_pfs::WrapOptions::at(Default::default())
        };
        let name = format!("PPSA01234.{inner}");
        ps5_dump_forge_pfs::wrap(&name, len, &mut file, &opts, &cancel, |s| {
            std::io::copy(&mut fs::File::open(image)?, s)?;
            Ok(())
        })?;
        drop(file);
        let bytes = sparse::encode(&fs::read(&wrapped)?);
        seed(out, "pfs", &format!("ffpfsc-{inner}"), &bytes)?;
        seed(out, "pfsc", &format!("built-{inner}"), &bytes)?;
    }
    for name in ["compressed.ffpfs", "raw.ffpfs", "nested.ffpfsc"] {
        let bytes = sparse::encode(&gunzip(&fixtures.join(format!("pfs/{name}.gz")))?);
        seed(out, "pfs", name, &bytes)?;
        if name.ends_with(".ffpfsc") {
            seed(out, "pfsc", name, &bytes)?;
        }
    }

    // .ffpfsc around the exFAT fixture with the older wrapper (zlib blocks and stored ones).
    let wrapped = work.join("mini.ffpfsc");
    let options = ffpfsc::WrapOptions {
        time: Some(TIME),
        threads: 1,
        ..Default::default()
    };
    ffpfsc::wrap(&mini, &wrapped, &options, &mut ffpfsc::Control::default())?;
    seed(out, "pfsc", "mini", &sparse::encode(&fs::read(&wrapped)?))?;

    // A debug package of the game folder, as the app builds them (Kraken, plaintext), and the
    // PS4 package fixtures.
    let pkg_dir = work.join("pkg");
    fs::create_dir_all(&pkg_dir)?;
    let mut request = BuildRequest::new(&game, &pkg_dir);
    request.time = Some((TIME, 0));
    request.seed = Some([0x5A; 16]);
    let report = build::build(&request, &mut |_| {})?;
    let pkg = sparse::encode(&fs::read(&report.path)?);
    for target in ["fih_cnt", "pkg_source", "pkg_meta"] {
        seed(out, target, "ps5", &pkg)?;
    }
    for name in ["synthetic", "licence"] {
        let pkg = sparse::encode(&gunzip(&fixtures.join(format!("ps4/{name}.pkg.gz")))?);
        for target in ["fih_cnt", "pkg_source", "pkg_meta"] {
            seed(out, target, &format!("ps4-{name}"), &pkg)?;
        }
    }

    // Kraken: a descriptor with real LZ, LZ-delta and raw halves, and the blocks themselves.
    let (blob, image, blocks) = kraken_sample()?;
    seed(
        out,
        "kraken_descriptor",
        "sample",
        &mem_tree::join(&[&blob, &image]),
    )?;
    for (i, data) in blocks.iter().enumerate() {
        let halves = kraken::encode_block(data);
        let kind = |h: &Half| match h {
            Half::Raw(_) => 0u8,
            Half::Lz(_) => 1,
            Half::LzDelta(_) => 2,
            Half::Entropy(_) => 3,
        };
        let mut input = vec![kind(&halves[0]) | halves.get(1).map_or(0, |h| kind(h) << 2)];
        input.extend_from_slice(&((data.len() - 1) as u32).to_le_bytes());
        input.extend_from_slice(&(halves[0].bytes().len() as u32).to_le_bytes());
        for h in &halves {
            input.extend_from_slice(h.bytes());
        }
        seed(out, "kraken_block", &format!("block{i}"), &input)?;
    }

    pack_seeds(out, fixtures, work)?;

    // param.json, a param.sfo cut from the PS4 fixture, and the game's own small files.
    seed(out, "param", "json", PARAM_JSON.as_bytes())?;
    let ps4 = gunzip(&fixtures.join("ps4/synthetic.pkg.gz"))?;
    if let Some(at) = ps4.windows(4).position(|w| w == b"\0PSF") {
        let end = (at + 4096).min(ps4.len());
        seed(out, "param", "sfo", &ps4[at..end])?;
    }
    let eboot = fs::read(game.join("eboot.bin"))?;
    seed(
        out,
        "game_checks",
        "game",
        &mem_tree::join(&[&eboot, PARAM_JSON.as_bytes(), b""]),
    )?;
    seed(
        out,
        "lz4_profile",
        "compress",
        &fs::read(fixtures.join("ampr_pack/compress.toml"))?,
    )?;
    seed(
        out,
        "lz4_profile",
        "store",
        &fs::read(fixtures.join("ampr_pack/store.toml"))?,
    )?;
    seed(
        out,
        "cheat",
        "json",
        br#" {"name":"Game","id":"PPSA01234","version":"01.000","process":"eboot.bin","mods":[{"name":"God","type":"checkbox","memory":[{"offset":"1A2B3C","on":"90909090","off":"0F84AABB"}]}]}"#,
    )?;
    seed(
        out,
        "cheat",
        "xml",
        b"\x01<?xml version=\"1.0\" encoding=\"utf-8\"?><Patch><TitleID><ID>PPSA01234</ID></TitleID><Metadata Title=\"Game\" Name=\"60 FPS\" AppVer=\"01.000\" AppElf=\"eboot.bin\"><PatchList><Line Type=\"bytes\" Address=\"0x1A2B3C\" Value=\"90909090\"/></PatchList></Metadata></Patch>",
    )?;
    Ok(())
}

fn gunzip(path: &Path) -> Res<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(path)?).read_to_end(&mut out)?;
    Ok(out)
}

fn seed(out: &Path, target: &str, name: &str, bytes: &[u8]) -> Res<()> {
    let dir = out.join(target);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(name), bytes)?;
    println!("{target}/{name}: {} bytes", bytes.len());
    Ok(())
}

/// A tiny game: nested dirs, an empty dir and file, a directory with many entries, and a
/// 1 MiB file (UFS2 indirect blocks) that is zeros apart from its ends, so it costs nothing in
/// the sparse seed.
fn fixture(dir: &Path) -> Res<PathBuf> {
    fs::create_dir_all(dir.join("sce_sys"))?;
    fs::create_dir_all(dir.join("data/levels/a"))?;
    fs::create_dir_all(dir.join("data/empty"))?;
    fs::create_dir_all(dir.join("data/many"))?;
    fs::write(
        dir.join("eboot.bin"),
        [0x4F, 0x15, 0x3D, 0x1D, 0, 1, 1, 0x12],
    )?;
    fs::write(dir.join("sce_sys/param.json"), PARAM_JSON)?;
    fs::write(dir.join("data/levels/a/small.txt"), b"hello\n")?;
    fs::write(dir.join("data/zero.bin"), b"")?;
    for i in 0..40 {
        fs::write(dir.join(format!("data/many/f{i:02}.txt")), format!("{i}\n"))?;
    }
    let mut big = vec![0u8; 1 << 20];
    big[..16].copy_from_slice(b"first block head");
    let n = big.len();
    big[n - 16..].copy_from_slice(b"last block tail!");
    fs::write(dir.join("data/levels/big.bin"), big)?;
    Ok(dir.to_path_buf())
}

/// Blocks shaped like `kraken_image`'s own descriptor test (a two-block file, a short one, the
/// gap and the metadata block), but with real encoded halves, laid out and walked back.
/// Returns (descriptor, stored image, logical bytes of each block).
fn kraken_sample() -> Res<KrakenSample> {
    let text: Vec<u8> = (0..0x60000u32)
        .flat_map(|i| format!("line {} of the quick brown fox\n", i % 977).into_bytes())
        .collect();
    let mut noise = 0x9E37_79B9u32;
    let mut noise = move || {
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        noise as u8
    };
    let file0: Vec<u8> = text[..0x41234].to_vec();
    let file1: Vec<u8> = (0..0x1000).map(|_| noise()).collect();
    let gap = vec![0u8; 0x3ddcc];
    let meta: Vec<u8> = text[0x1000..0x41000]
        .iter()
        .map(|b| b.wrapping_add(1))
        .collect();
    let parts: [(u64, &[u8], Owner); 5] = [
        (0, &file0[..0x40000], Owner::File(0)),
        (0x40000, &file0[0x40000..], Owner::File(0)),
        (0x41234, &file1, Owner::File(1)),
        (0x42234, &gap, Owner::Gap),
        (0x80000, &meta, Owner::Meta),
    ];
    let mut image = Vec::new();
    let mut blocks = Vec::new();
    for (logical, data, owner) in parts.iter().cloned() {
        let halves = kraken::encode_block(data);
        blocks.push(StoredBlock {
            logical,
            len: data.len() as u32,
            stored_at: image.len() as u64,
            halves: halves
                .iter()
                .map(|h| (h.bytes().len() as u32, h.is_lz()))
                .collect(),
            delta: [0, 1].map(|i| halves.get(i).is_some_and(Half::is_delta)),
            owner,
        });
        for h in &halves {
            image.extend_from_slice(h.bytes());
        }
    }
    let end = image.len() as u64;
    let img = KrakenImage {
        blocks,
        image_len: end.next_multiple_of(ps5upload_fpkg::BLOCK),
        file_digests: Vec::new(),
        file_stored_at: Vec::new(),
        mount_size: 0xc0000,
        data_end: 0x42234,
        meta_base: 0x80000,
    };
    let blob = kraken_image::layout(&img, &[0, 0x41234])?;
    // The seed must decode back to what went in.
    let want: Vec<Vec<u8>> = parts.iter().map(|p| p.1.to_vec()).collect();
    for (b, data) in kraken_image::describe(&blob)?.iter().zip(&want) {
        if &kraken_image::decode_described(&image, b)? != data {
            return Err(
                format!("kraken seed block at {:#x} does not round-trip", b.logical).into(),
            );
        }
    }
    Ok((blob, image, want))
}

/// Output of the zstd packer, kept in memory.
#[derive(Default)]
struct MemOut(std::collections::BTreeMap<String, Vec<u8>>);

impl ps5_dump_forge_zstd::writer::PackOutput for MemOut {
    type W = std::io::Cursor<Vec<u8>>;
    fn create(&mut self, _: &str) -> ps5upload_fpkg::Result<Self::W> {
        Ok(Default::default())
    }
    fn reopen(&mut self, name: &str) -> ps5upload_fpkg::Result<Self::W> {
        Ok(std::io::Cursor::new(
            self.0.remove(name).unwrap_or_default(),
        ))
    }
    fn finish(&mut self, name: &str, w: Self::W) -> ps5upload_fpkg::Result<()> {
        self.0.insert(name.to_string(), w.into_inner());
        Ok(())
    }
}

/// LZ4 and zstd packs of one tiny tree (a compressible file, a noise file, the loose
/// `eboot.bin`), the LZ4 fixtures drakmor's own tool wrote, and the path index.
fn pack_seeds(out: &Path, fixtures: &Path, work: &Path) -> Res<()> {
    let text: Vec<u8> = (0..300_000)
        .map(|i| b"ps5upload packs assets "[i % 23])
        .collect();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let noise: Vec<u8> = (0..70_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let (loose, byte, len) = mem_tree::LOOSE_EBOOT;
    let entries = [
        ("data/a.bin", text),
        ("data/b.bin", noise),
        (loose, vec![byte; len]),
    ];
    let tree = || mem_tree::Mem::new(entries.iter().map(|(p, d)| (p.to_string(), d.clone())));
    let sizes: Vec<(String, u64)> = entries
        .iter()
        .map(|(p, d)| (p.to_string(), d.len() as u64))
        .collect();

    // The path index, and an LZ4 pack set that packs both data files.
    let index = ampr_index::build(&sizes, TIME)?;
    seed(out, "lz4_index", "three", &index)?;
    let listed = ampr_index::parse(&index).ok_or("the path index does not parse")?;
    let profile = "[pack]\nblock_size = \"64K\"\n\n[[rule]]\ninclude = [\"data/**\"]\n";
    let config = ampr_pack::Config::from_toml(profile, None)?;
    let pack_dir = work.join("lz4");
    fs::create_dir_all(&pack_dir)?;
    let packed = ampr_pack::build(
        &listed,
        &mut tree(),
        &config,
        &pack_dir,
        &mut ampr_pack::Control::default(),
    )?;
    if packed.packed.is_empty() {
        return Err("the LZ4 seed profile packed nothing".into());
    }
    let volume = fs::read(&packed.volumes[0].1)?;
    seed(
        out,
        "lz4_manifest",
        "built-crc",
        &mem_tree::join(&[&packed.manifest, &packed.crc]),
    )?;
    seed(
        out,
        "lz4_manifest",
        "built-volume",
        &mem_tree::join(&[&packed.manifest, &volume[..volume.len().min(64)]]),
    )?;
    for (name, crc) in [("built", packed.crc.as_slice()), ("built-nocrc", &[])] {
        let input = mem_tree::join(&[&packed.manifest, crc, &volume]);
        // The seed must open and read back whole.
        match ampr_pack::image::Lz4Unpacked::open(Box::new(mem_tree::pack_tree(
            &input,
            mem_tree::LZ4_NAMES,
        )))? {
            ampr_pack::image::Unpacking::Decoded(mut u) => {
                for (path, data) in &entries {
                    if &u.read(path)? != data {
                        return Err(format!("LZ4 seed {name}: {path} does not round-trip").into());
                    }
                }
            }
            ampr_pack::image::Unpacking::NotPacked(_) => {
                return Err(format!("LZ4 seed {name} does not read as packed").into())
            }
        }
        seed(out, "lz4_unpack", name, &input)?;
    }
    let dir = fixtures.join("ampr_pack/compress");
    let read = |name: &str| fs::read(dir.join(name));
    let manifest = read("ampr_assets.index")?;
    seed(
        out,
        "lz4_manifest",
        "fixture-runtime",
        &mem_tree::join(&[&manifest, &read("ampr_assets.index.runtime")?]),
    )?;
    seed(
        out,
        "lz4_unpack",
        "fixture",
        &mem_tree::join(&[
            &manifest,
            &read("ampr_assets.index.crc")?,
            &read("ampr_assets-000.pak")?,
        ]),
    )?;

    // The zstd pack set of the same two files.
    let mut src = tree();
    let files: Vec<_> = src
        .files()
        .iter()
        .filter(|f| f.path != loose)
        .cloned()
        .collect();
    let mut zout = MemOut::default();
    let zindex = ps5_dump_forge_zstd::writer::pack(&mut src, &files, 3, &mut zout)?;
    let zindex = ps5_dump_forge_zstd::encode_index(&zindex)?;
    let volume = zout
        .0
        .get(&ps5_dump_forge_zstd::volume_name(0))
        .cloned()
        .ok_or("the zstd packer wrote no volume")?;
    seed(out, "zstd_index", "index", &zindex)?;
    seed(out, "zstd_index", "volume-header", &volume[..64])?;
    let input = mem_tree::join(&[&zindex, &[], &volume]);
    let mut u = ps5_dump_forge_zstd::reader::unpack(Box::new(mem_tree::pack_tree(
        &input,
        mem_tree::ZSTD_NAMES,
    )))?;
    for (path, data) in &entries {
        if &u.read(path)? != data {
            return Err(format!("zstd seed: {path} does not round-trip").into());
        }
    }
    seed(out, "zstd_unpack", "built", &input)?;
    Ok(())
}
