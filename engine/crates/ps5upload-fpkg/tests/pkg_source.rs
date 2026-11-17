//! A package this crate builds reads back, through `pkg_source`, as the folder it was built from:
//! every layout the builder writes, names, sizes, bytes (whole and by range) and empty
//! directories. Packages it cannot read are refused with the reason in plain words.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::inner::MetaCodec;
use ps5upload_fpkg::kraken::Level;
use ps5upload_fpkg::pkg_source::{self, PkgSource};
use ps5upload_fpkg::source::{self, SourceTree};
use ps5upload_fpkg::{inner, plan, ImageMode};

const CONTENT_ID: &str = "UP0000-PPSA04690_00-PKGSOURCETEST000";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("fpkg-pkgsource-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Bytes that are neither all one value nor random: some runs compress, some do not.
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..len)
        .map(|i| {
            if (i / 4096) % 3 == 0 {
                (i % 7) as u8
            } else {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            }
        })
        .collect()
}

/// A small game: a module, the `sce_sys` files the container takes over, a 0-byte file, a file
/// over several 256 KiB blocks, nested directories and two empty ones (one nested).
fn write_game(root: &Path, big: usize) {
    let put = |path: &str, data: &[u8]| {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    };
    put("eboot.bin", &pattern(5000, 1));
    put(
        "sce_sys/param.json",
        format!(
            "{{\"contentId\":\"{CONTENT_ID}\",\"contentVersion\":\"01.001.000\",\"titleId\":\"PPSA04690\",\"applicationCategoryType\":0,\"attributePub\":0,\"applicationDrmType\":\"standard\"}}"
        )
        .as_bytes(),
    );
    put("sce_sys/icon0.png", &pattern(3000, 2));
    put("sce_sys/pic0.png", &pattern(7000, 3));
    put("sce_sys/trophy2/trophy00.ucp", &pattern(900, 4));
    put("data/zero.bin", &[]);
    put("data/big.bin", &pattern(big, 5));
    put("data/nested/deep/leaf.txt", b"a leaf three levels down\n");
    put("data/nested/odd.bin", &pattern(65_537, 6));
    std::fs::create_dir_all(root.join("data/empty")).unwrap();
    std::fs::create_dir_all(root.join("data/nested/hollow")).unwrap();
}

fn request(src: &Path, out: &Path) -> BuildRequest {
    let mut r = BuildRequest::new(src, out);
    r.time = Some((1_700_000_000, 0));
    r.seed = Some([0x5A; 16]);
    // Pinned against the environment's overrides, so each test builds exactly what it names.
    r.kraken = true;
    r.level = Level::Balanced;
    r.metadata_codec = MetaCodec::Stored;
    r.image_mode = ImageMode::PlaintextNoAuth;
    r.firmware = None;
    r
}

fn build(req: &BuildRequest) -> PathBuf {
    let report = build::build(req, &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    report.path
}

/// The package's tree is the source's: the same files (plus the keystone the build generates),
/// the same bytes — whole, by odd ranges and read in odd-sized pieces — and the same empty
/// directories. `param.json` is the one file the build rewrites, so it is compared by content id.
fn assert_matches_source(tree: &mut dyn SourceTree, src: &Path) {
    let (mut want, want_empty) = source::scan_tree(src).unwrap();
    want.push(source::SourceFile {
        path: plan::KEYSTONE.to_string(),
        size: plan::KEYSTONE_LEN,
    });
    want.sort_by(|a, b| a.path.cmp(&b.path));
    let got: Vec<_> = tree.files().to_vec();
    let names = |v: &[source::SourceFile]| v.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
    assert_eq!(names(&got), names(&want), "file names");
    assert_eq!(tree.empty_dirs(), &want_empty[..], "empty directories");

    for f in &got {
        let data = tree.read(&f.path).unwrap();
        assert_eq!(data.len() as u64, f.size, "{} size", f.path);
        if f.path == "sce_sys/param.json" {
            assert_eq!(source::content_id(&data).as_deref(), Some(CONTENT_ID));
            continue;
        }
        let expected = if f.path == plan::KEYSTONE {
            inner::keystone(DEFAULT_PASSCODE).to_vec()
        } else {
            std::fs::read(src.join(&f.path)).unwrap()
        };
        assert_eq!(data.len(), expected.len(), "{} size", f.path);
        assert!(data == expected, "{} bytes", f.path);

        // Odd ranges: a byte in, across 64 KiB and 256 KiB boundaries, and past the end.
        let n = expected.len() as u64;
        for (off, len) in [
            (1u64, 3usize),
            (n.saturating_sub(5), 11),
            (65_535, 70_001),
            (262_141, 7),
            (n, 10),
            (n + 100, 10),
        ] {
            let got = tree.read_range(&f.path, off, len).unwrap();
            let a = (off as usize).min(expected.len());
            let b = (a + len).min(expected.len());
            assert!(got == expected[a..b], "{} range {off}+{len}", f.path);
        }
        let mut pieced = Vec::new();
        while (pieced.len() as u64) < n {
            pieced.extend(
                tree.read_range(&f.path, pieced.len() as u64, 100_003)
                    .unwrap(),
            );
        }
        assert!(pieced == expected, "{} read in pieces", f.path);
    }
}

fn round_trip(name: &str, adjust: impl FnOnce(&mut BuildRequest)) -> String {
    let src = TempDir::new(&format!("{name}-src"));
    let out = TempDir::new(&format!("{name}-out"));
    write_game(src.path(), 700 * 1024);
    let mut req = request(src.path(), out.path());
    adjust(&mut req);
    let pkg = build(&req);
    let mut tree = source::open(&pkg).unwrap();
    assert_matches_source(tree.as_mut(), src.path());
    tree.describe()
}

#[test]
fn a_kraken_package_at_each_level_reads_back_as_its_folder() {
    for level in [Level::Fast, Level::Balanced, Level::Smallest] {
        let described = round_trip(&format!("kraken-{level:?}"), |r| r.level = level);
        assert!(described.contains("Kraken block layout"), "{described}");
    }
}

#[test]
fn a_stored_block_package_reads_back_as_its_folder() {
    let described = round_trip("blocks-stored", |r| r.level = Level::Stored);
    assert!(described.contains("stored block layout"), "{described}");
}

#[test]
fn a_flat_package_reads_back_with_either_metadata_codec() {
    let described = round_trip("flat-stored", |r| r.kraken = false);
    assert!(described.contains("flat layout"), "{described}");
    let described = round_trip("flat-zlib", |r| {
        r.kraken = false;
        r.metadata_codec = MetaCodec::Zlib;
    });
    assert!(described.contains("zlib metadata"), "{described}");
}

#[test]
fn a_native_package_with_the_debug_passcode_reads_back() {
    round_trip("native-blocks", |r| r.image_mode = ImageMode::Native);
    round_trip("native-flat", |r| {
        r.image_mode = ImageMode::Native;
        r.kraken = false;
    });
}

/// A native package made with any other passcode cannot be opened with the debug one, which is
/// what a retail package looks like from here.
#[test]
fn a_package_made_with_another_passcode_is_refused_as_retail() {
    let src = TempDir::new("retail-src");
    let out = TempDir::new("retail-out");
    write_game(src.path(), 300 * 1024);
    let mut req = request(src.path(), out.path());
    req.image_mode = ImageMode::Native;
    req.passcode = "0123456789ABCDEF0123456789ABCDEF".into();
    let pkg = build(&req);
    let Err(e) = source::open(&pkg) else {
        panic!("a package made with another passcode must not open");
    };
    assert!(e.to_string().contains(pkg_source::RETAIL_MESSAGE), "{e}");
    // With its own passcode it reads.
    let tree = pkg_source::open(&pkg, &req.passcode).unwrap();
    assert!(!tree.files().is_empty());
}

#[test]
fn a_truncated_package_is_refused() {
    let src = TempDir::new("trunc-src");
    let out = TempDir::new("trunc-out");
    write_game(src.path(), 300 * 1024);
    let pkg = build(&request(src.path(), out.path()));
    let len = std::fs::metadata(&pkg).unwrap().len();
    for keep in [len / 2, len * 3 / 4, 0x10000 + 0x100, 0x80] {
        let cut = out.path().join(format!("cut-{keep}.pkg"));
        let mut bytes = std::fs::read(&pkg).unwrap();
        bytes.truncate(keep as usize);
        std::fs::write(&cut, bytes).unwrap();
        let Err(e) = source::open(&cut) else {
            panic!("a package cut to {keep} bytes must not open");
        };
        assert!(e.to_string().contains("incomplete"), "{keep}: {e}");
    }
}

/// A package whose image bytes were damaged is refused when the damaged block is read, not
/// handed out as if it were the game.
#[test]
fn a_damaged_block_is_an_error_not_bad_bytes() {
    let src = TempDir::new("damage-src");
    let out = TempDir::new("damage-out");
    write_game(src.path(), 300 * 1024);
    let mut req = request(src.path(), out.path());
    req.kraken = false;
    let pkg = build(&req);
    let mut bytes = std::fs::read(&pkg).unwrap();
    // The first outer block holds the start of the inner image (the keystone, then sce_sys).
    bytes[0x10000 + 0x10] ^= 0xFF;
    let bad = out.path().join("bad.pkg");
    std::fs::write(&bad, bytes).unwrap();
    let opened = source::open(&bad);
    let err = match opened {
        Err(e) => e.to_string(),
        Ok(mut tree) => tree.read(plan::KEYSTONE).unwrap_err().to_string(),
    };
    assert!(err.contains("damaged"), "{err}");
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// `pkg` as a patch package (`\x7FLIH`) that carries every outer block but `omit`: a 64 KiB
/// header, the FIH block, the carried blocks in order, the container. From PS5 Dump Forge's
/// `crates/ps5-dump-forge-fpkg/tests/patch.rs`.
fn patch_of(pkg: &[u8], omit: &[usize]) -> Vec<u8> {
    const B: usize = 0x10000;
    let (pfs_offset, pfs_size, cnt_offset) = (
        le64(pkg, 0x10) as usize,
        le64(pkg, 0x18) as usize,
        le64(pkg, 0x58) as usize,
    );
    // The container runs to the image or to the package's end, whichever comes first after it.
    let cnt_end = if cnt_offset < pfs_offset {
        pfs_offset
    } else {
        pkg.len()
    };
    let mut out = vec![0u8; B];
    out.extend_from_slice(&pkg[..B]);
    for i in (0..pfs_size / B).filter(|i| !omit.contains(i)) {
        out.extend_from_slice(&pkg[pfs_offset + i * B..pfs_offset + (i + 1) * B]);
    }
    let cnt_at = out.len();
    out.extend_from_slice(&pkg[cnt_offset..cnt_end]);
    let len = out.len() as u64;
    out[..4].copy_from_slice(b"\x7FLIH");
    for (at, v) in [
        (0x18, len),
        (0x20, B as u64),
        (0x28, B as u64),
        (0x30, cnt_at as u64),
        (0x38, (cnt_end - cnt_offset) as u64),
    ] {
        out[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// A package re-wrapped as a patch package that lacks one outer block, for every block in
/// turn and every layout the builder writes. Whichever block is missing, the patch either opens
/// as the files it carries whole, each reading back byte for byte, with every other file
/// accounted for as left to the base game, or is refused as not holding its own file table.
/// It never opens with bad bytes and never fails any other way.
#[test]
fn a_patch_package_reads_as_the_files_it_carries_whole() {
    const BIG: &str = "data/big.bin";
    for (name, kraken, level) in [
        ("patch-kraken", true, Level::Fast),
        ("patch-stored", true, Level::Stored),
        ("patch-flat", false, Level::Fast),
    ] {
        let src = TempDir::new(&format!("{name}-src"));
        let out = TempDir::new(&format!("{name}-out"));
        write_game(src.path(), 700 * 1024);
        let mut req = request(src.path(), out.path());
        req.kraken = kraken;
        req.level = level;
        let pkg_path = build(&req);
        let pkg = std::fs::read(&pkg_path).unwrap();
        let mut whole = pkg_source::open(&pkg_path, DEFAULT_PASSCODE).unwrap();
        assert!(whole.patch().is_none() && whole.notes().is_empty());
        let outer_blocks = le64(&pkg, 0x18) as usize / 0x10000;
        let path = out.path().join("update.pkg");

        // Every block carried: the patch is the package.
        std::fs::write(&path, patch_of(&pkg, &[])).unwrap();
        let all = pkg_source::open(&path, DEFAULT_PASSCODE).unwrap();
        assert_eq!(all.files(), whole.files(), "{name}");
        assert_eq!(all.empty_dirs(), whole.empty_dirs(), "{name}");
        let info = all.patch().unwrap();
        assert!(info.partial.is_empty() && info.in_base.is_empty(), "{name}");
        assert_eq!(info.outer_blocks, outer_blocks as u64, "{name}");

        let (mut opened, mut refused, mut without_big) = (0, 0, 0);
        for omit in 0..outer_blocks {
            std::fs::write(&path, patch_of(&pkg, &[omit])).unwrap();
            let mut patch = match pkg_source::open(&path, DEFAULT_PASSCODE) {
                Ok(p) => p,
                Err(e) => {
                    let e = e.to_string();
                    assert!(
                        e.contains("does not hold the blocks that list its files"),
                        "{name}, block {omit}: {e}"
                    );
                    refused += 1;
                    continue;
                }
            };
            opened += 1;
            let info = patch.patch().unwrap().clone();
            assert_eq!(info.outer_blocks, outer_blocks as u64);
            // Not always one fewer: a block left out that is byte for byte another one the
            // patch stores (padding) is served by that one.
            assert!(info.carried_blocks <= info.outer_blocks);
            let listed: Vec<String> = patch.files().iter().map(|f| f.path.clone()).collect();
            assert!(
                listed.iter().any(|p| p == "sce_sys/icon0.png"),
                "{name}, block {omit}: the container's files are always carried: {listed:?}"
            );
            for p in &listed {
                assert!(
                    patch.read(p).unwrap() == whole.read(p).unwrap(),
                    "{name}, block {omit}: {p}"
                );
            }
            // Every file of the package that is not listed is named as left out: by itself, or
            // by a folder above it that only the base game lists.
            for f in whole.files() {
                let accounted = listed.contains(&f.path)
                    || info.partial.contains(&f.path)
                    || info.in_base.iter().any(|b| {
                        *b == f.path
                            || b == "the top folder"
                            || f.path.starts_with(&format!("{b}/"))
                    });
                assert!(accounted, "{name}, block {omit}: {} vanished", f.path);
            }
            let left = info.partial.len() + info.in_base.len();
            if left > 0 {
                let notes = patch.notes().join("\n");
                assert!(notes.contains("patch package"), "{notes}");
                assert!(
                    notes.contains(&format!(
                        "{} files the patch holds only part of",
                        info.partial.len()
                    )),
                    "{notes}"
                );
                assert!(
                    patch.describe().contains("patch package"),
                    "{}",
                    patch.describe()
                );
            }
            if info.partial.iter().any(|p| p == BIG) {
                without_big += 1;
                assert!(!listed.iter().any(|p| p == BIG));
            }
        }
        // The big file spans several blocks, and some block is one an open cannot do without.
        assert!(
            without_big > 1,
            "{name}: {without_big} patches lacked part of {BIG}"
        );
        assert!(
            opened > 0 && refused > 0,
            "{name}: {opened} opened, {refused} refused"
        );
    }
}

/// The patch's blocks are placed by the digests the package records for them, which are of
/// the blocks as the game reads them. A native (encrypted) package's stored blocks match
/// none, and that is said rather than guessed around.
#[test]
fn a_patch_of_an_encrypted_package_is_refused() {
    let src = TempDir::new("patch-native-src");
    let out = TempDir::new("patch-native-out");
    write_game(src.path(), 300 * 1024);
    let mut req = request(src.path(), out.path());
    req.image_mode = ImageMode::Native;
    let pkg = std::fs::read(build(&req)).unwrap();
    let path = out.path().join("update.pkg");
    std::fs::write(&path, patch_of(&pkg, &[3])).unwrap();
    let Err(e) = source::open(&path) else {
        panic!("a patch of an encrypted package must not open");
    };
    assert!(e.to_string().contains("match no block"), "{e}");
}

/// The extension is matched in any case, and Convert's check step accepts a package.
#[test]
fn a_package_is_a_source_convert_can_check() {
    let src = TempDir::new("inspect-src");
    let out = TempDir::new("inspect-out");
    write_game(src.path(), 300 * 1024);
    let pkg = build(&request(src.path(), out.path()));
    let upper = out.path().join("GAME.PKG");
    std::fs::rename(&pkg, &upper).unwrap();
    let inspection = build::inspect(&upper, out.path()).unwrap();
    assert_eq!(inspection.content_id.as_deref(), Some(CONTENT_ID));
    assert_eq!(
        inspection.files,
        source::open(&upper).unwrap().files().len()
    );
}

/// A package becomes an image (exFAT here) and another package through the existing writers,
/// and both hold the same tree.
#[test]
fn a_package_becomes_an_image_and_another_package() {
    let src = TempDir::new("convert-src");
    let out = TempDir::new("convert-out");
    write_game(src.path(), 300 * 1024);
    let pkg = build(&request(src.path(), out.path()));
    let mut tree = source::open(&pkg).unwrap();

    let image = out.path().join("game.exfat");
    ps5upload_fpkg::exfat_write::build_exfat(tree.as_mut(), &image, None, &mut |_, _| {}, &|| {
        false
    })
    .unwrap();
    let mut from_image = source::open(&image).unwrap();
    assert_eq!(from_image.files(), tree.files());
    assert_eq!(from_image.empty_dirs(), tree.empty_dirs());
    for f in tree.files().to_vec() {
        assert!(from_image.read(&f.path).unwrap() == tree.read(&f.path).unwrap());
    }

    let again = TempDir::new("convert-again");
    let mut req = request(&pkg, again.path());
    req.file_name = Some("again".into());
    let second = build(&req);
    let mut rebuilt = source::open(&second).unwrap();
    assert_eq!(rebuilt.files(), tree.files());
    assert_eq!(rebuilt.empty_dirs(), tree.empty_dirs());
    for f in tree.files().to_vec() {
        assert!(rebuilt.read(&f.path).unwrap() == tree.read(&f.path).unwrap());
    }
}

/// A package made elsewhere can contain OS junk that our folder scanner would omit.
/// Keep the raw afid layout while hiding that junk, including directories it leaves empty.
#[test]
fn packages_with_os_junk_convert_with_the_same_visible_tree() {
    use ps5upload_fpkg::stream::{self, KrakenSpool, Progress, StreamRequest};
    use std::sync::atomic::AtomicBool;

    let data = [
        ("eboot.bin", pattern(5000, 1)),
        ("data/big.bin", pattern(300_000, 2)),
        ("only-junk/.DS_Store", pattern(127, 3)),
        ("System Volume Information/tracking.bin", pattern(4000, 4)),
        ("data/.Trashes/old.bin", pattern(6000, 5)),
        ("._eboot.bin", pattern(7000, 6)),
        ("data/zero.bin", Vec::new()),
    ];
    let files: Vec<_> = data
        .iter()
        .map(|(path, bytes)| source::SourceFile {
            path: (*path).into(),
            size: bytes.len() as u64,
        })
        .collect();
    let empty: Vec<String> = [
        "System Volume Information/K4W-21-25",
        "data/nested/sYsTeM vOlUmE iNfOrMaTiOn/empty",
        "fakelib",
        "sce_sys/trophy2",
        "sce_sys/uds",
    ]
    .map(String::from)
    .into();
    let want_empty = [
        "data/nested",
        "fakelib",
        "only-junk",
        "sce_sys/trophy2",
        "sce_sys/uds",
    ];

    for kraken in [false, true] {
        let out = TempDir::new(&format!("junk-{kraken}"));
        let pkg = out.path().join("junk.pkg");
        // Bypass FolderSource's filter so the fixture really stores the ignored entries.
        let plan = plan::build_tree(&files, &empty, kraken).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&pkg)
            .unwrap();
        let request = StreamRequest {
            plan: &plan,
            passcode: DEFAULT_PASSCODE,
            seed: ps5upload_fpkg::PLAINTEXT_MARKER,
            image_mode: ImageMode::PlaintextNoAuth,
            time: (1_700_000_000, 0),
            content_id: CONTENT_ID,
            content_version: 0x0100_0100,
            param_json: Vec::new(),
            icon_png: Vec::new(),
            icon_dds: Vec::new(),
            extras: Vec::new(),
            playgo_chunks: 1,
            kraken_spool: kraken.then_some(KrakenSpool::InPlace),
            level: Level::Fast,
            metadata_codec: MetaCodec::Stored,
        };
        stream::write_package(
            &mut file,
            &request,
            &mut |path, offset, len| {
                let bytes = &data.iter().find(|(p, _)| *p == path).unwrap().1;
                let start = (offset as usize).min(bytes.len());
                let end = start.saturating_add(len).min(bytes.len());
                Ok(bytes[start..end].to_vec())
            },
            &mut Progress {
                phase: &mut |_| {},
                bytes: &mut |_, _| {},
                stage: &mut |_| {},
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        drop(file);

        let mut tree = source::open(&pkg).unwrap();
        assert_eq!(tree.empty_dirs(), want_empty, "kraken={kraken}");
        let names: Vec<_> = tree.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "data/big.bin",
                "data/zero.bin",
                "eboot.bin",
                "sce_sys/keystone"
            ],
            "kraken={kraken}"
        );
        // Ignored files still occupy physical space in a flat package. Check the bytes
        // after them, not just the filtered names, so their afids cannot shift offsets.
        for (path, bytes) in data.iter().take(2).chain(data.iter().skip(6)) {
            assert_eq!(&tree.read(path).unwrap(), bytes, "{path} kraken={kraken}");
        }
        let image = out.path().join("game.exfat");
        ps5upload_fpkg::exfat_write::build_exfat(
            tree.as_mut(),
            &image,
            None,
            &mut |_, _| {},
            &|| false,
        )
        .unwrap();
        let mut converted = source::open(&image).unwrap();
        assert_eq!(converted.files(), tree.files(), "kraken={kraken}");
        assert_eq!(converted.empty_dirs(), tree.empty_dirs(), "kraken={kraken}");
        for f in tree.files().to_vec() {
            assert_eq!(
                converted.read(&f.path).unwrap(),
                tree.read(&f.path).unwrap()
            );
        }
    }
}

/// A large file streams: reading all of it in pieces holds a few blocks, never the file.
#[test]
fn a_large_file_streams_in_bounded_memory() {
    const BIG: usize = 40 << 20;
    for (name, kraken) in [("stream-flat", false), ("stream-blocks", true)] {
        let src = TempDir::new(&format!("{name}-src"));
        let out = TempDir::new(&format!("{name}-out"));
        write_game(src.path(), BIG);
        let mut req = request(src.path(), out.path());
        req.kraken = kraken;
        req.level = Level::Fast;
        let pkg = build(&req);
        let mut tree = pkg_source::open(&pkg, DEFAULT_PASSCODE).unwrap();
        let want = std::fs::read(src.path().join("data/big.bin")).unwrap();
        let mut at = 0usize;
        let mut peak = 0usize;
        while at < want.len() {
            let piece = tree.read_range("data/big.bin", at as u64, 1 << 20).unwrap();
            assert!(piece == want[at..at + piece.len()], "{name} at {at}");
            at += piece.len();
            peak = peak.max(tree.cached_bytes());
        }
        assert!(
            peak <= 2 << 20,
            "{name}: {peak} bytes cached for a {BIG}-byte file"
        );
        let _: &PkgSource = &tree;
    }
}

/// The image browser (#479) opens a debug package as a read-only tree: its folders list with
/// their totals, and one folder of it reads back as a tree of its own.
#[test]
fn a_package_browses_read_only() {
    use ps5upload_fpkg::browse::{self, Filesystem, Index, Kind, Subtree};
    let src = TempDir::new("browse-src");
    let out = TempDir::new("browse-out");
    write_game(src.path(), 300 * 1024);
    let pkg = build(&request(src.path(), out.path()));
    let opened = browse::open(&pkg).unwrap();
    assert_eq!(opened.filesystem, Filesystem::Pkg);
    assert!(!opened.filesystem.editable() && !opened.compressed);
    let index = Index::new(&*opened.tree);
    let nested: Vec<(String, bool)> = index
        .list("data/nested")
        .unwrap()
        .into_iter()
        .map(|e| (e.name, e.is_dir))
        .collect();
    assert_eq!(
        nested,
        [
            ("deep".to_string(), true),
            ("hollow".to_string(), true),
            ("odd.bin".to_string(), false)
        ]
    );
    assert_eq!(index.kind("data/nested/odd.bin"), Some(Kind::File(65_537)));
    let mut sub = Subtree::new(opened.tree, "data/nested").unwrap();
    assert_eq!(
        sub.read("deep/leaf.txt").unwrap(),
        std::fs::read(src.path().join("data/nested/deep/leaf.txt")).unwrap()
    );
    assert_eq!(sub.empty_dirs(), ["hollow"]);
}
