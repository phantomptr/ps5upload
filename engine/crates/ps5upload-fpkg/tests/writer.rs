//! Gate G2: a package this crate builds must verify with its own reader, and the inner
//! image must walk back to the exact source bytes.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::inner::MetaCodec;
use ps5upload_fpkg::{cnt, fih, inner, naps, outer, plan, source, verify, PkgFile};

const CONTENT_ID: &str = "UP0000-PPSA01234_00-TESTGAME00000000";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fpkg-writer-{}-{name}", std::process::id()));
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

/// A small app tree: a module, the `sce_sys` files the container carries, one file that
/// crosses a block boundary, one empty directory and one file that only exists in `uroot`.
fn write_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut put = |path: &str, data: Vec<u8>| {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &data).unwrap();
        files.push((path.to_string(), data));
    };
    put("eboot.bin", (0..4096u32).map(|i| (i % 251) as u8).collect());
    put(
        "data/small.bin",
        (0..100u32).map(|i| (i % 13) as u8).collect(),
    );
    put("data/large.bin", vec![0xAB; 600 * 1024]);
    put(
        "sce_sys/param.json",
        format!(
            "{{\"contentId\":\"{CONTENT_ID}\",\"contentVersion\":\"01.001.000\",\"titleId\":\"PPSA01234\"}}"
        )
        .into_bytes(),
    );
    put("sce_sys/icon0.png", vec![0x89; 2048]);
    put("sce_sys/icon0.dds", vec![0x44; 4096]);
    put(
        "sce_sys/about/right.sprx",
        vec![0x54, 0x14, 0xF5, 0xEE, 0, 0, 0, 0],
    );
    std::fs::create_dir_all(root.join("data/empty")).unwrap();
    files
}

/// The inner image as stored in the built package's outer PFS.
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

#[test]
fn gate_g2_a_built_package_verifies_and_round_trips() {
    let source_dir = TempDir::new("source");
    let output_dir = TempDir::new("out");
    let expected = write_tree(source_dir.path());

    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        seed: Some([0x42; 16]),
        // This gate asserts the container shape (a stored image shorter than the mount), so it
        // pins the codec the container belongs to. The default is `Stored`, whose round trip
        // `mounts.rs` covers.
        metadata_codec: MetaCodec::Zlib,
        ..BuildRequest::new(source_dir.path(), output_dir.path())
    };
    let mut phases = Vec::new();
    let report = build::build(&request, &mut |phase| phases.push(phase.to_string())).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    assert_eq!(report.content_id, CONTENT_ID);
    assert!(report.size > 600 * 1024);
    assert!(phases.contains(&"verifying".to_string()));
    assert!(report.path.extension().unwrap() == "pkg");
    assert!(!output_dir
        .path()
        .join(format!("{CONTENT_ID}.pkg.partial"))
        .exists());

    // The reader's own report must include the checks the writer was built against.
    for required in [
        "cnt package digest",
        "cnt header rollup digest",
        "cnt body digest",
        "cnt finalized-image digest",
        "cnt general digest playgo",
        "outer flat-path table hashes the uroot names",
        "si playgo-chunk.crc",
    ] {
        assert!(
            report
                .verify
                .checks
                .iter()
                .any(|c| c.name == required && c.ok),
            "missing check: {required}"
        );
    }

    // The plan's geometry is what the package carries — less the icons, which the container
    // carries instead of the image.
    let only_in_container = |path: &str| ps5upload_fpkg::cnt_write::CONTAINER_ONLY.contains(&path);
    let mut files = source::scan(source_dir.path()).unwrap();
    files.retain(|f| !only_in_container(&f.path));
    let built = plan::build(&files).unwrap();
    let mut pkg = PkgFile::open(&report.path).unwrap();
    let head = pkg.read_at(0, fih::HEADER_LEN).unwrap();
    let parsed = fih::parse(&head).unwrap();
    let fih_block = pkg.read_at(0, ps5upload_fpkg::BLOCK as usize).unwrap();
    assert_eq!(
        u32::from_le_bytes(fih_block[0x50..0x54].try_into().unwrap()) as u64,
        built.meta_base / ps5upload_fpkg::BLOCK,
        "the header must point at the inner metadata base in mount blocks, which the console \
         multiplies by the block size the header carries at 0x60"
    );
    assert_eq!(
        u32::from_le_bytes(fih_block[0x94..0x98].try_into().unwrap()),
        built.content_inodes
    );
    assert_eq!(
        u64::from_le_bytes(fih_block[0xA0..0xA8].try_into().unwrap()),
        built.ndblock * ps5upload_fpkg::BLOCK
    );

    // The inner image walks back to the source bytes, and the layout reconstructs the mount.
    let inner_image = outer_file(&report.path, "pfs_image.dat");
    assert_eq!(
        u32::from_le_bytes(fih_block[0x90..0x94].try_into().unwrap()) as u64,
        inner_image.len() as u64 / ps5upload_fpkg::BLOCK,
        "the header's 0x90 is the stored image's block count"
    );
    assert!(
        (inner_image.len() as u64) < built.ndblock * ps5upload_fpkg::BLOCK,
        "the fixture's metadata deflates, so the stored image must be shorter than the mount"
    );
    assert!(parsed.pfs_size >= inner_image.len() as u64);
    // What the mount reads at the metadata base is the container expanded, not the container.
    let mount_image = inner::logical_mount(
        &inner_image,
        built.meta_base,
        MetaCodec::Zlib,
        &built.placements(),
    )
    .unwrap();
    assert!(
        mount_image.len() as u64 >= built.ndblock * ps5upload_fpkg::BLOCK,
        "the mount reaches at least to the end the plan fixed"
    );
    let mount = inner::read(&mount_image, built.meta_base).unwrap();
    assert!(
        mount.flt_ok,
        "the inner flat-path table must hash every path"
    );
    let mut recovered: Vec<(String, Vec<u8>)> = mount
        .files
        .iter()
        .map(|f| {
            let at = f.offset as usize;
            (
                f.path.clone(),
                mount_image[at..at + f.size as usize].to_vec(),
            )
        })
        .collect();
    recovered.sort();
    let mut wanted: Vec<(String, Vec<u8>)> = expected
        .iter()
        .filter(|(p, _)| !only_in_container(p))
        .map(|(p, d)| (p.clone(), d.clone()))
        .chain([(
            "sce_sys/keystone".to_string(),
            inner::keystone(DEFAULT_PASSCODE).to_vec(),
        )])
        .collect();
    wanted.sort();
    assert_eq!(recovered, wanted);

    let layout = naps::parse(&outer_file(&report.path, "naps_pkg_layout.dat")).unwrap();
    assert_eq!(layout.mount_size(), built.ndblock * ps5upload_fpkg::BLOCK);
    assert_eq!(
        layout.compression_type, 1,
        "the layout must declare the codec its payloads use (1 = zlib)"
    );
    assert_eq!(layout.num_files as usize, built.afid_order.len() + 3);
    let faces: Vec<u64> = layout.fidx.iter().map(|(o, _)| *o).collect();
    assert_eq!(
        faces[faces.len() - 3..],
        [
            built.data_end,
            built.meta_base,
            built.ndblock * ps5upload_fpkg::BLOCK
        ]
    );
    // The layout's own reconstruction has to land on the same mount the container expands to.
    let rebuilt = naps::reconstruct(&inner_image, &layout, &built.placements()).unwrap();
    let data_end = built.data_end as usize;
    assert_eq!(
        &rebuilt[..data_end],
        &mount_image[..data_end],
        "the data region is stored where the mount reads it"
    );
    assert_eq!(
        &rebuilt[built.meta_base as usize..],
        &mount_image[built.meta_base as usize..],
        "the layout's metadata region is the container's expansion"
    );
}

#[test]
fn a_second_build_reports_warnings_but_still_verifies() {
    let source_dir = TempDir::new("warn-source");
    let output_dir = TempDir::new("warn-out");
    write_tree(source_dir.path());
    // Remove the icon pair: not fatal, but the readiness report says so.
    std::fs::remove_file(source_dir.path().join("sce_sys/icon0.dds")).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        seed: Some([7; 16]),
        ..BuildRequest::new(source_dir.path(), output_dir.path())
    };
    let report = build::build(&request, &mut |_| {}).unwrap();
    assert!(report.verify.ok());
    assert!(report.verify.checks.iter().all(|c| c.ok));
    // The icon entry is now empty; the container still verifies.
    let mut pkg = PkgFile::open(&report.path).unwrap();
    let head = pkg.read_at(0, fih::HEADER_LEN).unwrap();
    let parsed = fih::parse(&head).unwrap();
    let container = cnt::read(&mut pkg, parsed.cnt_offset).unwrap();
    assert_eq!(container.entry(cnt::ids::ICON0_DDS).unwrap().size, 0);
}

#[test]
fn a_build_never_overwrites_an_existing_package() {
    let source_dir = TempDir::new("preserve-source");
    let output_dir = TempDir::new("preserve-out");
    write_tree(source_dir.path());
    let request = BuildRequest::new(source_dir.path(), output_dir.path());
    let first = build::build(&request, &mut |_| {}).unwrap();
    let original = std::fs::read(&first.path).unwrap();
    let error = build::build(&request, &mut |_| {}).err().unwrap();
    assert!(error.to_string().contains("output already exists"));
    assert_eq!(std::fs::read(&first.path).unwrap(), original);
}

#[test]
fn a_source_without_a_content_id_is_refused() {
    let source_dir = TempDir::new("bad-source");
    let output_dir = TempDir::new("bad-out");
    std::fs::create_dir_all(source_dir.path().join("sce_sys")).unwrap();
    std::fs::write(source_dir.path().join("eboot.bin"), [0u8; 16]).unwrap();
    std::fs::write(source_dir.path().join("sce_sys/param.json"), b"{}").unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        seed: Some([0; 16]),
        ..BuildRequest::new(source_dir.path(), output_dir.path())
    };
    let error = match build::build(&request, &mut |_| {}) {
        Ok(report) => panic!("expected a refusal, built {}", report.path.display()),
        Err(error) => error,
    };
    assert!(error.to_string().contains("content id"), "{error}");
    assert!(!output_dir.path().join("out.pkg.partial").exists());
    assert!(verify::verify_package(&output_dir.path().join("x.pkg"), DEFAULT_PASSCODE).is_err());
}

/// A Kraken package decodes back to its source: every block through the descriptor, the way
/// the console reads it, then the inner file system walked and each file compared.
#[test]
fn a_kraken_package_decodes_back_to_its_source() {
    use ps5upload_fpkg::kraken_image;
    let source_dir = TempDir::new("kraken-src");
    let out = TempDir::new("kraken-out");
    let expected = write_tree(source_dir.path());
    let mut request = BuildRequest::new(source_dir.path(), out.path());
    request.kraken = true;
    request.time = Some((1_700_000_000, 0));
    let report = build::build(&request, &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);

    let naps = outer_file(&report.path, "naps_pkg_layout.dat");
    let image = outer_file(&report.path, "pfs_image.dat");
    let blocks = kraken_image::describe(&naps).unwrap();
    let mount_size = blocks.last().map(|b| b.logical + b.len).unwrap();
    assert!((image.len() as u64) < mount_size, "the image is compressed");
    let mut mount = vec![0u8; mount_size as usize];
    for b in &blocks {
        let bytes = kraken_image::decode_described(&image, b).unwrap();
        mount[b.logical as usize..(b.logical + b.len) as usize].copy_from_slice(&bytes);
    }
    let files = source::scan(source_dir.path()).unwrap();
    let meta_base = plan::build(
        &files
            .into_iter()
            .filter(|f| !ps5upload_fpkg::cnt_write::CONTAINER_ONLY.contains(&f.path.as_str()))
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .meta_base;
    let walked = inner::read(&mount, meta_base).unwrap();
    assert!(walked.flt_ok);
    for (path, data) in &expected {
        if ps5upload_fpkg::cnt_write::CONTAINER_ONLY.contains(&path.as_str())
            || path == "sce_sys/param.json"
        {
            continue;
        }
        let f = walked
            .files
            .iter()
            .find(|f| &f.path == path)
            .unwrap_or_else(|| panic!("{path} missing from the mount"));
        assert_eq!(
            &mount[f.offset as usize..(f.offset + f.size) as usize],
            &data[..],
            "{path}"
        );
    }
}
