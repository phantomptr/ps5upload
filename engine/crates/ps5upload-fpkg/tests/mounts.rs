//! The real game mounts: reading them is what the converter's image sources are for.
//!
//! Gated like the other sample tests — without the mount folder these skip with a note
//! instead of failing. `PS5UPLOAD_SAMPLE_MOUNTS` overrides the default location.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::inner::MetaCodec;
use ps5upload_fpkg::source;
use ps5upload_fpkg::{cnt, fih, inner, naps, outer, plan, PkgFile};

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

    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        seed: Some([0x5A; 16]),
        ..BuildRequest::new(&fixture, &out)
    };
    let report = build::build(&request, &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    assert_eq!(report.content_id, FIXTURE_ID);
    assert!(report.verify.checks.iter().all(|c| c.ok));

    // The inner image walks back to the fixture's files, byte for byte, plus the
    // keystone the writer generates.
    let built = plan::build(tree.files()).unwrap();
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
    let mut wanted: Vec<(String, u64)> = expected.clone();
    wanted.push(("sce_sys/keystone".to_string(), 96));
    wanted.sort();
    assert_eq!(recovered, wanted);

    // A recovered file's bytes are the image's bytes: the PNG magic, read from the
    // package rather than the fixture.
    let png = mount
        .files
        .iter()
        .find(|f| f.path == "sce_sys/icon0.png")
        .unwrap();
    let at = png.offset as usize;
    assert_eq!(mount_image[at..at + 8], PNG_MAGIC);

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
