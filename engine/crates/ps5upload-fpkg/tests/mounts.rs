//! The real game mounts: reading them is what the converter's image sources are for.
//!
//! Gated like the other sample tests — without the mount folder these skip with a note
//! instead of failing. `PS5UPLOAD_SAMPLE_MOUNTS` overrides the default location.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::inner::MetaCodec;
use ps5upload_fpkg::source;
use ps5upload_fpkg::{cnt, cnt_write, fih, inner, naps, outer, plan, PkgFile};

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn mount_dir() -> Option<PathBuf> {
    let dir = std::env::var("PS5UPLOAD_SAMPLE_MOUNTS")
        .unwrap_or_else(|_| "/Volumes/Storage/PS5/games/game_mounts".to_string());
    let path = PathBuf::from(dir);
    path.is_dir().then_some(path)
}

fn images(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        // `._NAME.exfat` is macOS resource fork, not an image.
        .filter(|p| {
            !p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with("._")
        })
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        })
        .collect();
    out.sort();
    out
}

/// Every real `.exfat` mount opens, walks, and gives back the bytes of its files.
#[test]
fn every_real_exfat_mount_walks_and_reads() {
    let Some(dir) = mount_dir() else {
        eprintln!("skip: no mount folder (set PS5UPLOAD_SAMPLE_MOUNTS)");
        return;
    };
    let images = images(&dir, "exfat");
    if images.is_empty() {
        eprintln!("skip: no .exfat images in {}", dir.display());
        return;
    }
    for image in &images {
        let mut tree = source::open(image).expect("the image opens as a source");
        let files = tree.files().to_vec();
        let name = image.file_name().unwrap().to_string_lossy().into_owned();

        // Real counts run from 33 (a 90 GB game in one blob) to several hundred.
        assert!(
            files.len() >= 20,
            "{name}: only {} files walked",
            files.len()
        );
        for want in ["eboot.bin", "sce_sys/param.json", "sce_sys/icon0.png"] {
            assert!(
                files.iter().any(|f| f.path == want),
                "{name}: {want} is missing"
            );
        }
        assert!(
            files.windows(2).all(|w| w[0].path < w[1].path),
            "{name}: the walk is not sorted and unique"
        );

        let param = tree.read("sce_sys/param.json").expect("param.json reads");
        let id = source::content_id(&param).expect("param.json has a content id");
        assert_eq!(id.len(), 36, "{name}: content id {id:?}");

        // Real bytes at real offsets: a module magic at 0 and a PNG header in sce_sys.
        let magic = tree.read_range("eboot.bin", 0, 4).expect("eboot head");
        assert!(
            [
                source::magic::RAW_ELF.as_slice(),
                source::magic::SELF_PS5.as_slice(),
                source::magic::SELF_PS4.as_slice(),
                source::magic::SIGNED_SELF.as_slice()
            ]
            .contains(&magic.as_slice()),
            "{name}: eboot.bin starts with {magic:02x?}"
        );
        let png = tree
            .read_range("sce_sys/icon0.png", 0, 8)
            .expect("icon head");
        assert_eq!(png, PNG_MAGIC, "{name}: icon0.png is not a PNG");

        // The walk accounts for the volume: its files fill most of the image.
        let total: u64 = files.iter().map(|f| f.size).sum();
        let image_size = std::fs::metadata(image).unwrap().len();
        assert!(
            total <= image_size,
            "{name}: {total} bytes of files in a {image_size} byte image"
        );
        assert!(
            total * 100 >= image_size * 90,
            "{name}: only {total} of {image_size} bytes are walked files ({} files)",
            files.len()
        );

        let dds = tree
            .read_range("sce_sys/icon0.dds", 0, 4)
            .expect("dds head");
        assert_eq!(dds, b"DDS ", "{name}: icon0.dds is not a DDS");

        // A ranged read takes different offset arithmetic than the whole-file read; the
        // tail of a file both times must agree.
        let full = tree.read("sce_sys/param.json").expect("param.json reads");
        let tail = tree
            .read_range("sce_sys/param.json", full.len() as u64 - 16, 16)
            .expect("tail read");
        assert_eq!(tail, full[full.len() - 16..], "{name}: tail mismatch");
        // And past the end it stops, rather than failing or reading a cluster.
        let past = tree
            .read_range("sce_sys/param.json", full.len() as u64 - 4, 64)
            .expect("past-the-end read");
        assert_eq!(past, full[full.len() - 4..], "{name}: past-the-end read");
    }
    eprintln!("read {} exfat mounts", images.len());
}

/// Every real `.ffpkg` (UFS2) mount opens, walks, and gives back the bytes of its files.
#[test]
fn every_real_ffpkg_mount_walks_and_reads() {
    let Some(dir) = mount_dir() else {
        eprintln!("skip: no mount folder (set PS5UPLOAD_SAMPLE_MOUNTS)");
        return;
    };
    let images = images(&dir, "ffpkg");
    if images.is_empty() {
        eprintln!("skip: no .ffpkg images in {}", dir.display());
        return;
    }
    for image in &images {
        let mut tree = source::open(image).expect("the image opens as a source");
        let files = tree.files().to_vec();
        let name = image.file_name().unwrap().to_string_lossy().into_owned();

        assert!(
            files.len() >= 20,
            "{name}: only {} files walked",
            files.len()
        );
        for want in ["eboot.bin", "sce_sys/param.json", "sce_sys/icon0.png"] {
            assert!(
                files.iter().any(|f| f.path == want),
                "{name}: {want} is missing"
            );
        }
        let param = tree.read("sce_sys/param.json").expect("param.json reads");
        let id = source::content_id(&param).expect("param.json has a content id");
        assert_eq!(id.len(), 36, "{name}: content id {id:?}");
        let magic = tree.read_range("eboot.bin", 0, 4).expect("eboot head");
        assert!(
            [
                source::magic::RAW_ELF.as_slice(),
                source::magic::SELF_PS5.as_slice(),
                source::magic::SELF_PS4.as_slice(),
                source::magic::SIGNED_SELF.as_slice()
            ]
            .contains(&magic.as_slice()),
            "{name}: eboot.bin starts with {magic:02x?}"
        );
        let png = tree
            .read_range("sce_sys/icon0.png", 0, 8)
            .expect("icon head");
        assert_eq!(png, PNG_MAGIC, "{name}: icon0.png is not a PNG");

        let total: u64 = files.iter().map(|f| f.size).sum();
        let image_size = std::fs::metadata(image).unwrap().len();
        assert!(
            total <= image_size,
            "{name}: {total} bytes of files in a {image_size} byte image"
        );
        // Real density is ~80% (inode tables, directory blocks and the free
        // fragments a dumped filesystem still carries); half is a floor that
        // still catches a walk that stops early.
        assert!(
            total * 2 >= image_size,
            "{name}: only {total} of {image_size} bytes are walked files ({} files)",
            files.len()
        );

        // The ranged read walks block pointers; the whole-file read walks the
        // block chain. On a file that uses indirect blocks they must agree at
        // every boundary — this is what proves the pointer arithmetic.
        let victim = files
            .iter()
            .filter(|f| f.path != "eboot.bin" && f.size > 2 << 20 && f.size < 96 << 20)
            .max_by_key(|f| f.size)
            .unwrap_or_else(|| panic!("{name}: no multi-megabyte file to range-test"));
        let whole = tree.read(&victim.path).expect("whole file");
        assert_eq!(
            whole.len() as u64,
            victim.size,
            "{name}: {} read as {} bytes",
            victim.path,
            whole.len()
        );
        for offset in [0u64, 1, 4095, 32 * 1024, victim.size / 3, victim.size - 16] {
            for len in [4usize, 4096] {
                let got = tree
                    .read_range(&victim.path, offset, len)
                    .expect("ranged read");
                let start = (offset as usize).min(whole.len());
                let want = &whole[start..(start + len).min(whole.len())];
                assert_eq!(got, want, "{name}: {} at {offset}+{len}", victim.path);
            }
        }
        eprintln!(
            "{name}: {} files, {:.1} GiB of {:.1} GiB, ranged-read proof on {} ({:.0} MiB)",
            files.len(),
            total as f64 / (1u64 << 30) as f64,
            image_size as f64 / (1u64 << 30) as f64,
            victim.path,
            victim.size as f64 / (1u64 << 20) as f64
        );
    }
}

/// The fixture: a 2 MiB exFAT volume macOS itself formatted, holding a minimal app tree.
/// `tests/fixtures/mini.exfat` also carries the `._*` AppleDouble sidecars a Mac copy
/// leaves behind, so every walk of it proves the junk filter.
const FIXTURE_ID: &str = "UP0000-PPSA99001_00-MINIFI8TURE00001";
const FIXTURE_FILES: &[(&str, u64)] = &[
    ("data/one.bin", 921_600),
    ("eboot.bin", 16_384),
    ("sce_sys/about/right.sprx", 2_048),
    ("sce_sys/icon0.dds", 4_100),
    ("sce_sys/icon0.png", 74),
    ("sce_sys/param.json", 197),
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini.exfat")
}

/// The inner image of a built package, read back out of its outer PFS.
fn outer_file(pkg: &Path, name: &str) -> Vec<u8> {
    let mut file = PkgFile::open(pkg).unwrap();
    let head = file.read_at(0, fih::HEADER_LEN).unwrap();
    let parsed = fih::parse(&head).unwrap();
    let container = cnt::read(&mut file, parsed.cnt_offset).unwrap();
    let image = outer::open(&mut file, &parsed, &container, DEFAULT_PASSCODE).unwrap();
    let nodes = image.dinodes();
    let uroot = nodes.get(2).unwrap();
    let ino = image
        .dirents(uroot)
        .into_iter()
        .find(|d| d.name == name)
        .unwrap()
        .ino as usize;
    image.file_data(&nodes[ino])
}

/// Gate G2 for image sources: a package built from the exFAT fixture verifies, and its
/// inner image holds exactly the fixture's tree.
#[test]
fn a_build_from_an_exfat_mount_verifies_and_round_trips() {
    let fixture = fixture();
    let out = std::env::temp_dir().join(format!("fpkg-mount-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();

    // The walk sees the fixture's six real files, sidecars excluded.
    let tree = source::open(&fixture).unwrap();
    let walked: Vec<(String, u64)> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let expected: Vec<(String, u64)> = FIXTURE_FILES
        .iter()
        .map(|(p, s)| (p.to_string(), *s))
        .collect();
    assert_eq!(walked, expected, "{}", tree.describe());

    // The flat layout, which this test reads back directly; the block layout that is now the
    // default has its own round-trip tests.
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        seed: Some([0x5A; 16]),
        kraken: false,
        ..BuildRequest::new(&fixture, &out)
    };
    let report = build::build(&request, &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    assert_eq!(report.content_id, FIXTURE_ID);
    assert!(report.verify.checks.iter().all(|c| c.ok));

    // The inner image walks back to the fixture's files, byte for byte, plus the
    // keystone the writer generates — less the icons, which the container carries instead.
    let in_image: Vec<source::SourceFile> = tree
        .files()
        .iter()
        .filter(|f| !cnt_write::CONTAINER_ONLY.contains(&f.path.as_str()))
        .cloned()
        .collect();
    let built = plan::build(&in_image).unwrap();
    let image = outer_file(&report.path, "pfs_image.dat");
    assert!(image.len() as u64 <= built.ndblock * ps5upload_fpkg::BLOCK);
    // The image is read back through the codec the build used — the default, whose metadata
    // region is stored verbatim rather than wrapped in a container.
    let mount_image = inner::logical_mount(
        &image,
        built.meta_base,
        MetaCodec::Stored,
        &built.placements(),
    )
    .unwrap();
    let mount = inner::read(&mount_image, built.meta_base).unwrap();
    assert!(mount.flt_ok);
    let mut recovered: Vec<(String, u64)> = mount
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    recovered.sort();
    let mut wanted: Vec<(String, u64)> = expected
        .iter()
        .filter(|(p, _)| !cnt_write::CONTAINER_ONLY.contains(&p.as_str()))
        .cloned()
        .collect();
    wanted.push(("sce_sys/keystone".to_string(), 96));
    wanted.sort();
    assert_eq!(recovered, wanted);

    // The icon's bytes come back out of the package's container: the PNG magic, read from
    // the package rather than the fixture.
    let mut file = PkgFile::open(&report.path).unwrap();
    let head = file.read_at(0, fih::HEADER_LEN).unwrap();
    let container = cnt::read(&mut file, fih::parse(&head).unwrap().cnt_offset).unwrap();
    let icon = container.entry(cnt::ids::ICON0_PNG).unwrap();
    assert_eq!(container.payload(icon)[..8], PNG_MAGIC);

    // And the layout reconstructs the mount it describes — the data region where the mount reads
    // it, and the metadata region out of the container the image stores. The gap between the two
    // is padding the image never stores, so it is only asserted to be empty.
    let layout = naps::parse(&outer_file(&report.path, "naps_pkg_layout.dat")).unwrap();
    let rebuilt = naps::reconstruct(&image, &layout, &built.placements()).unwrap();
    let data_end = built.data_end as usize;
    let meta_at = built.meta_base as usize;
    assert_eq!(&rebuilt[..data_end], &mount_image[..data_end]);
    assert!(rebuilt[data_end..meta_at].iter().all(|b| *b == 0));
    assert_eq!(&rebuilt[meta_at..], &mount_image[meta_at..]);
    std::fs::remove_dir_all(&out).ok();
}

/// A `.ffpfsc` of an image.
fn wrapped(image: &Path, name: &str) -> PathBuf {
    use ps5upload_fpkg::ffpfsc::{wrap, Control, WrapOptions};
    let dir = std::env::temp_dir().join(format!("fpkg-ffpfsc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(format!("{name}.ffpfsc"));
    wrap(
        image,
        &out,
        &WrapOptions::default(),
        &mut Control::default(),
    )
    .unwrap();
    out
}

/// Part 1 of the Convert-inputs spec: a `.ffpfsc` reads back as the image inside it —
/// every read, at any offset, across block boundaries and at the end.
#[test]
fn the_pfsc_reader_reads_the_inner_image_at_any_offset() {
    use std::io::{Read, Seek, SeekFrom};
    // The fixture compresses; noise is stored as it is (no block shrinks enough).
    let noise = std::env::temp_dir().join(format!("fpkg-noise-{}.bin", std::process::id()));
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let bytes: Vec<u8> = (0..0x10000 * 3 + 777)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(&noise, &bytes).unwrap();
    for (source, compress) in [(fixture(), true), (noise.clone(), false)] {
        let raw = std::fs::read(&source).unwrap();
        let img = wrapped(&source, &format!("reader-{compress}"));
        let r0 = ps5upload_fpkg::ffpfsc::verify(&img, None).unwrap();
        assert_eq!(r0.compressed, compress);
        let mut r = ps5upload_fpkg::pfsc_reader::PfscReader::open(&img).unwrap();
        assert_eq!(r.len(), raw.len() as u64);
        let block = 0x10000u64;
        let len = raw.len() as u64;
        for (at, n) in [
            (0, 16),
            (block - 3, 7),
            (block * 3 + 100, (block * 2) as usize),
            (len - 10, 10),
            (len - 5, 100), // short read at the end
            (12_345, 1),
        ] {
            r.seek(SeekFrom::Start(at)).unwrap();
            let mut got = vec![0u8; n];
            let mut filled = 0;
            loop {
                let k = r.read(&mut got[filled..]).unwrap();
                if k == 0 {
                    break;
                }
                filled += k;
            }
            let want = &raw[at as usize..(at as usize + n).min(raw.len())];
            assert_eq!(
                &got[..filled],
                want,
                "at {at:#x} len {n} compress {compress}"
            );
        }
        r.seek(SeekFrom::Start(len + 5)).unwrap();
        let mut b = [0u8; 4];
        assert_eq!(r.read(&mut b).unwrap(), 0);
    }
}

/// A `.ffpfsc` source is the tree of the image it holds.
#[test]
fn an_ffpfsc_opens_as_the_tree_of_its_image() {
    let img = wrapped(&fixture(), "tree");
    let mut plain = source::open(&fixture()).unwrap();
    let mut packed = source::open(&img).unwrap();
    assert_eq!(plain.files(), packed.files());
    for f in plain.files().to_vec() {
        assert_eq!(
            plain.read(&f.path).unwrap(),
            packed.read(&f.path).unwrap(),
            "{}",
            f.path
        );
    }
    assert!(packed.describe().contains("ffpfsc"));
}

/// A PFS image of one small game folder, unpacked from its gzipped fixture: `nested.ffpfsc`
/// (a container holding a PFS image that holds an exFAT image: what a tester's console held),
/// `compressed.ffpfs` (a PFS image holding a compressed exFAT image) and `raw.ffpfs` (a PFS
/// image holding the game's files themselves).
fn pfs_fixture(name: &str) -> PathBuf {
    use std::io::Read;
    let gz = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/pfs/{name}.gz"));
    let dir = std::env::temp_dir().join(format!("fpkg-pfs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(name);
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(gz).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    std::fs::write(&out, bytes).unwrap();
    out
}

/// The folder those images were packed from: path, size and FNV-1a 64 of every file.
const PFS_FIXTURE_FILES: &[(&str, u64, u64)] = &[
    ("data/deep/er/leaf.dat", 40, 0x33a7a995c1730cc5),
    ("data/noise.bin", 70000, 0x5112984a7b777a22),
    ("data/text.txt", 300000, 0xa0565bb22b3b00a5),
    ("data/zero.bin", 0, 0xcbf29ce484222325),
    ("eboot.bin", 10255, 0x3a5c1a4f2c9e831d),
    ("sce_sys/param.json", 312, 0xe1abce88c66970e7),
];

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Every kind of PFS game image opens as the game's own files, byte for byte: the
/// `.ffpfsc` that nests a PFS image that nests an exFAT image (a tester's Convert failed on one
/// with "neither exFAT nor UFS2"), the compressed `.ffpfs`, and the raw one that holds the
/// files themselves.
#[test]
fn a_pfs_image_opens_as_the_game_inside_it() {
    for (name, kind) in [
        ("nested.ffpfsc", "exfat"),
        ("compressed.ffpfs", "exfat"),
        ("raw.ffpfs", "PFS"),
    ] {
        let mut tree = source::open(&pfs_fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let listed: Vec<(&str, u64)> = tree
            .files()
            .iter()
            .map(|f| (f.path.as_str(), f.size))
            .collect();
        let expected: Vec<(&str, u64)> = PFS_FIXTURE_FILES.iter().map(|f| (f.0, f.1)).collect();
        assert_eq!(listed, expected, "{name}");
        for (path, size, hash) in PFS_FIXTURE_FILES {
            let whole = tree.read(path).unwrap();
            assert_eq!(whole.len() as u64, *size, "{name} {path}");
            assert_eq!(fnv1a(&whole), *hash, "{name} {path}");
            // A read across a 64 KiB block boundary, and one past the end.
            let from = 65_000usize.min(whole.len());
            let to = (from + 2_000).min(whole.len());
            assert_eq!(
                tree.read_range(path, from as u64, 2_000).unwrap(),
                &whole[from..to],
                "{name} {path}"
            );
            assert!(tree.read_range(path, *size + 10, 16).unwrap().is_empty());
        }
        assert!(
            tree.describe()
                .to_ascii_lowercase()
                .contains(&kind.to_ascii_lowercase()),
            "{name}: {}",
            tree.describe()
        );
    }
}

/// A PFS image that is damaged, or of a kind Convert does not read, is refused with its own
/// reason and never read past its end.
#[test]
fn a_damaged_or_unsupported_pfs_image_is_refused() {
    let good = std::fs::read(pfs_fixture("raw.ffpfs")).unwrap();
    let dir = std::env::temp_dir().join(format!("fpkg-pfs-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let refused = |name: &str, bytes: &[u8]| -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        source::open(&p).err().expect("refused").to_string()
    };
    // Signed (mode bit 0) and encrypted (bit 2) images are other formats.
    let mut signed = good.clone();
    signed[0x1C] |= 0x1;
    assert!(refused("signed.ffpfs", &signed).contains("signed"));
    let mut encrypted = good.clone();
    encrypted[0x1C] |= 0x4;
    assert!(refused("encrypted.ffpfs", &encrypted).contains("encrypted"));
    // Cut short: the inode table or the files run past the image.
    assert!(refused("short.ffpfs", &good[..0x10000]).contains("PFS"));
    assert!(refused("cut.ffpfs", &good[..good.len() / 2]).contains("shorter"));
    // An inode count no image could hold.
    let mut huge = good.clone();
    huge[0x30..0x38].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(refused("huge.ffpfs", &huge).contains("inode"));
    // Not a PFS image at all.
    assert!(refused("zero.ffpfs", &vec![0u8; 0x20000]).contains("not a PFS image"));
}

/// A file that only claims to be `.ffpfsc` is refused with a reason.
#[test]
fn a_bogus_ffpfsc_is_refused() {
    let dir = std::env::temp_dir().join(format!("fpkg-ffpfsc-bogus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.ffpfsc");
    std::fs::write(&p, vec![0u8; 8 * 0x10000]).unwrap();
    let err = source::open(&p).err().expect("refused").to_string();
    assert!(err.contains("PFS"), "{err}");
}

/// A package built from a `.ffpfsc` is byte for byte the package built from its image.
#[test]
fn a_build_from_an_ffpfsc_equals_the_build_from_its_image() {
    let packed = wrapped(&fixture(), "build");
    let build_of = |src: &Path, tag: &str| {
        let out =
            std::env::temp_dir().join(format!("fpkg-ffpfsc-build-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let request = BuildRequest {
            time: Some((1_700_000_000, 0)),
            seed: Some([0x5A; 16]),
            kraken: false,
            ..BuildRequest::new(src, &out)
        };
        let report = build::build(&request, &mut |_| {}).unwrap();
        assert!(report.verify.ok(), "{}", report.verify);
        std::fs::read(&report.path).unwrap()
    };
    let a = build_of(&fixture(), "exfat");
    let b = build_of(&packed, "ffpfsc");
    assert!(
        a == b,
        "the packages differ ({} vs {} bytes)",
        a.len(),
        b.len()
    );
}

/// A saved server, played by this computer's disk: every read goes through the trait, and
/// opens are counted so a test can see the reader is reused rather than reopened per block.
struct DiskAsServer {
    opens: std::sync::atomic::AtomicUsize,
}

impl ps5upload_fpkg::remote_source::RemoteFiles for DiskAsServer {
    fn open(&self, path: &str) -> std::io::Result<Box<dyn ps5upload_fpkg::ReadSeek>> {
        self.opens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Box::new(std::fs::File::open(path)?))
    }
    fn stat(&self, path: &str) -> std::io::Result<(u64, bool)> {
        let m = std::fs::metadata(path)?;
        Ok((m.len(), m.is_dir()))
    }
    fn list(&self, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>> {
        std::fs::read_dir(dir)?
            .map(|e| {
                let e = e?;
                let m = e.metadata()?;
                Ok((
                    e.file_name().to_string_lossy().into_owned(),
                    m.is_dir(),
                    m.len(),
                ))
            })
            .collect()
    }
    fn label(&self) -> String {
        "test server".to_string()
    }
}

fn server() -> std::sync::Arc<DiskAsServer> {
    std::sync::Arc::new(DiskAsServer {
        opens: std::sync::atomic::AtomicUsize::new(0),
    })
}

/// A folder on a server reads as the same tree as the folder here: files, empty directories
/// and bytes, with junk skipped.
#[test]
fn a_server_folder_is_the_same_tree_as_a_local_one() {
    let dir = std::env::temp_dir().join(format!("fpkg-remote-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("g/sce_sys")).unwrap();
    std::fs::create_dir_all(dir.join("g/data/shaders")).unwrap();
    std::fs::write(
        dir.join("g/sce_sys/param.json"),
        br#"{"titleId":"PPSA00001"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("g/eboot.bin"), vec![3u8; 300_000]).unwrap();
    std::fs::write(dir.join("g/.DS_Store"), b"junk").unwrap();
    let root = dir.join("g");
    let mut local = source::open(&root).unwrap();
    let srv = server();
    let mut remote =
        ps5upload_fpkg::remote_source::open_remote(srv.clone(), root.to_str().unwrap()).unwrap();
    assert_eq!(local.files(), remote.files());
    assert_eq!(local.empty_dirs(), remote.empty_dirs());
    for f in local.files().to_vec() {
        assert_eq!(local.read(&f.path).unwrap(), remote.read(&f.path).unwrap());
    }
    // Sequential ranges of one file share one open.
    let before = srv.opens.load(std::sync::atomic::Ordering::Relaxed);
    for i in 0..4u64 {
        let got = remote.read_range("eboot.bin", i * 65_536, 65_536).unwrap();
        assert_eq!(got.len(), 65_536);
    }
    assert_eq!(
        srv.opens.load(std::sync::atomic::Ordering::Relaxed),
        before + 1
    );
    assert!(remote.describe().contains("test server"));
}

/// Images on a server — .exfat and .ffpfsc — are the trees of the same images here.
#[test]
fn server_images_are_the_same_trees_as_local_ones() {
    let packed = wrapped(&fixture(), "remote");
    for image in [fixture(), packed] {
        let mut local = source::open(&image).unwrap();
        let mut remote =
            ps5upload_fpkg::remote_source::open_remote(server(), image.to_str().unwrap()).unwrap();
        assert_eq!(local.files(), remote.files(), "{}", image.display());
        for f in local.files().to_vec() {
            assert_eq!(local.read(&f.path).unwrap(), remote.read(&f.path).unwrap());
        }
    }
}

/// `source::open` hands a `remote://` path to the opener the engine registered.
#[test]
fn a_remote_path_goes_to_the_registered_opener() {
    ps5upload_fpkg::remote_source::register(Box::new(|p: &str| {
        let rest = p.strip_prefix("remote://disk").unwrap();
        Ok((
            server() as std::sync::Arc<dyn ps5upload_fpkg::remote_source::RemoteFiles>,
            rest.to_string(),
        ))
    }));
    let url = format!("remote://disk{}", fixture().display());
    let tree = source::open(Path::new(&url)).unwrap();
    assert_eq!(tree.files().len(), FIXTURE_FILES.len());
}
