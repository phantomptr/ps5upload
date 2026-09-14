//! Writing at game scale: the streaming writer must emit exactly what the in-memory
//! writer emits, and must refuse what it cannot yet cover.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::build::{self, BuildRequest};

const CONTENT_ID: &str = "UP0000-PPSA05555_00-SCALETEST0000000";
const SEED: [u8; 16] = [0x33; 16];
const TIME: (i64, u32) = (1_700_000_000, 0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fpkg-scale-{}-{name}", std::process::id()));
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

/// A tree that crosses block boundaries in both directions: a file smaller than a block,
/// one that spans several, and the `sce_sys` set the container carries.
fn write_tree(root: &Path) {
    let title_id = &CONTENT_ID[7..16];
    let put = |path: &str, data: Vec<u8>| {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &data).unwrap();
    };
    // A fake SELF, as a real converted folder would carry.
    let mut eboot: Vec<u8> = vec![0x54, 0x14, 0xF5, 0xEE];
    eboot.extend((0..3996u32).map(|i| (i % 241) as u8));
    put("eboot.bin", eboot);
    put(
        "data/one.bin",
        (0..600_000u32).map(|i| (i % 199) as u8).collect(),
    );
    put("data/two.bin", vec![0x5A; 5000]);
    put(
        "sce_sys/param.json",
        format!(
            "{{\"contentId\":\"{CONTENT_ID}\",\"contentVersion\":\"01.002.003\",\
             \"titleName\":\"Scale Test\",\"titleId\":\"{title_id}\",\
             \"requiredSystemSoftwareVersion\":\"0x1160000000000000\"}}"
        )
        .into_bytes(),
    );
    put("sce_sys/icon0.png", vec![0x89; 2048]);
    put("sce_sys/icon0.dds", vec![0x44; 4096]);
    put("sce_sys/about/right.sprx", vec![0x54, 0x14, 0xF5, 0xEE]);
}

fn request(source: &Path, out: &Path) -> BuildRequest {
    BuildRequest {
        time: Some(TIME),
        seed: Some(SEED),
        ..BuildRequest::new(source, out)
    }
}

/// The whole point of the streaming rewrite: for the same plan, seed and time, it must
/// produce the same file as the writer gate G2 verified. Byte for byte — the header,
/// every encrypted block, the container and the install metadata.
#[test]
fn the_streaming_writer_matches_the_in_memory_writer() {
    let source = TempDir::new("oracle-source");
    let streamed_dir = TempDir::new("oracle-streamed");
    let memory_dir = TempDir::new("oracle-memory");
    write_tree(source.path());

    let streamed = build::build(&request(source.path(), streamed_dir.path()), &mut |_| {}).unwrap();
    assert!(streamed.verify.ok(), "{}", streamed.verify);
    let in_memory =
        build::build_in_memory(&request(source.path(), memory_dir.path()), &mut |_| {}).unwrap();
    assert!(in_memory.verify.ok(), "{}", in_memory.verify);

    let a = std::fs::read(&streamed.path).unwrap();
    let b = std::fs::read(&in_memory.path).unwrap();
    assert_eq!(a.len(), b.len(), "the two writers disagree on the size");
    if a != b {
        let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap();
        panic!(
            "the two writers disagree at {at:#x} of {:#x}: {:#04x} against {:#04x}",
            a.len(),
            a[at],
            b[at]
        );
    }
    assert_eq!(streamed.content_id, CONTENT_ID);
    assert_eq!(in_memory.content_id, CONTENT_ID);
}

/// Byte progress is reported, and it climbs: the engine's progress bar depends on it.
#[test]
fn the_streaming_writer_reports_progress() {
    let source = TempDir::new("progress-source");
    let out = TempDir::new("progress-out");
    write_tree(source.path());
    let mut samples: Vec<(u64, u64)> = Vec::new();
    let mut control = build::BuildControl {
        bytes: Some(&mut |done, total| samples.push((done, total))),
        cancel: None,
    };
    let report = build::build_controlled(
        &request(source.path(), out.path()),
        &mut |_| {},
        &mut control,
    )
    .unwrap();
    assert!(report.verify.ok());
    assert!(!samples.is_empty());
    // One total throughout (the outer image), never exceeded, and the run ends complete.
    // Each phase sweeps it — writing, then verifying — so the sequence steps back
    // between phases, which is what the engine's per-phase progress expects.
    let total = samples[0].1;
    assert!(samples.iter().all(|(_, t)| *t == total), "{samples:?}");
    assert!(samples.iter().all(|(done, t)| done <= t), "{samples:?}");
    assert_eq!(*samples.last().unwrap(), (total, total), "{samples:?}");
}

/// An image past the first indirect slot's 1820 blocks (12 + 1820 blocks = 114 MiB) makes
/// the dinode use its second slot, whose table points at tables rather than at data. That
/// nesting is the one part of the format no local sample can confirm — there is no debug
/// package over 600 MB to check it against (writer plan, gate G1) — so this test pins the
/// streaming writer against the in-memory one, and hardware (gate G4) decides whether the
/// console agrees.
///
/// Ignored by default: it writes ~117 MiB. Run it with
/// `cargo test -p ps5upload-fpkg --release --test scale -- --ignored`.
#[test]
#[ignore = "writes ~117 MiB to the temp directory"]
fn a_source_past_the_first_indirect_slot_round_trips() {
    let source = TempDir::new("large-source");
    let out = TempDir::new("large-out");
    let out_memory = TempDir::new("large-memory");
    write_tree(source.path());
    // A sparse file just past the slot: 12 + 1820 blocks of 64 KiB, plus one.
    let size = (12 + 1820 + 1) * 0x10000;
    let big = source.path().join("data/big.bin");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(size).unwrap();
    drop(file);

    let report = build::build(&request(source.path(), out.path()), &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    assert!(report.size > size);
    // The second-slot metadata must come out the same either way.
    let memory =
        build::build_in_memory(&request(source.path(), out_memory.path()), &mut |_| {}).unwrap();
    assert!(memory.verify.ok(), "{}", memory.verify);
    assert_eq!(
        std::fs::read(&report.path).unwrap(),
        std::fs::read(&memory.path).unwrap(),
        "the two writers disagree on the second-slot layout"
    );
    // The container carries one digest per outer block, so its length tells the story.
    let image = std::fs::read(&report.path).unwrap();
    let cnt_offset = u64::from_le_bytes(image[0x58..0x60].try_into().unwrap()) as usize;
    let container = ps5upload_fpkg::cnt::read(
        &mut ps5upload_fpkg::PkgFile::open(&report.path).unwrap(),
        cnt_offset as u64,
    )
    .unwrap();
    let digests = container.entry(ps5upload_fpkg::cnt::ids::IMAGE_DIGESTS);
    assert!(digests.is_some(), "the container carries the block digests");
    // And the reader walks the inner image back out of the second slot.
    let files = ps5upload_fpkg::source::scan(source.path()).unwrap();
    let plan = ps5upload_fpkg::plan::build(&files).unwrap();
    let mut pkg = ps5upload_fpkg::PkgFile::open(&report.path).unwrap();
    let head = pkg.read_at(0, ps5upload_fpkg::fih::HEADER_LEN).unwrap();
    let fih = ps5upload_fpkg::fih::parse(&head).unwrap();
    let container = ps5upload_fpkg::cnt::read(&mut pkg, fih.cnt_offset).unwrap();
    let img = ps5upload_fpkg::outer::open(
        &mut pkg,
        &fih,
        &container,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
    )
    .unwrap();
    let nodes = img.dinodes();
    let inner_image = img.file_data(&nodes[3]);
    assert_eq!(
        inner_image.len() as u64,
        plan.ndblock * ps5upload_fpkg::BLOCK,
        "the reader must recover the whole inner image"
    );
    let inner = ps5upload_fpkg::inner::read(&inner_image, plan.meta_base).unwrap();
    assert!(inner.flt_ok);
    let mut recovered: Vec<(String, u64)> = inner
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    recovered.sort();
    let mut wanted: Vec<(String, u64)> = files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .chain([(
            "sce_sys/keystone".to_string(),
            ps5upload_fpkg::plan::KEYSTONE_LEN,
        )])
        .collect();
    wanted.sort();
    assert_eq!(recovered, wanted);
    eprintln!(
        "built {:.2} GiB with a {}-block image, walked back through the second indirect slot",
        report.size as f64 / (1u64 << 30) as f64,
        size / 0x10000
    );
}

/// A real title's tree is far past one block: Minecraft's mount has 37,565 inodes and a
/// single directory can hold thousands of entries. A structure that outgrows a block just
/// runs on into the next, so the inode table and a directory's dirents are read as the byte
/// range they claim, not as one block. Kept small enough to run with the default suite.
#[test]
fn a_tree_with_blocks_of_inodes_round_trips() {
    let source = TempDir::new("deep-source");
    let out = TempDir::new("deep-out");
    let out_memory = TempDir::new("deep-memory");
    write_tree(source.path());
    // One flat directory whose dirents alone fill more than a 64 KiB block, and whose files
    // push the inode table past one too.
    let many = source.path().join("data/many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..3000u32 {
        std::fs::write(
            many.join(format!("entry{i:05}.bin")),
            (0..64u32).map(|b| (b + i) as u8).collect::<Vec<u8>>(),
        )
        .unwrap();
    }

    let report = build::build(&request(source.path(), out.path()), &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    let memory =
        build::build_in_memory(&request(source.path(), out_memory.path()), &mut |_| {}).unwrap();
    assert!(memory.verify.ok(), "{}", memory.verify);

    let files = ps5upload_fpkg::source::scan(source.path()).unwrap();
    let plan = ps5upload_fpkg::plan::build(&files).unwrap();
    // The point of the test: this tree does not fit the region's first block, and the plan
    // says so with an explicit region rather than by failing.
    let inodes = plan.files.len() as u64 + plan.dirs.len() as u64;
    assert!(inodes > 3000, "the tree must carry {inodes} inodes");
    assert!(
        plan.metadata.blocks > 2,
        "a {inodes}-inode tree needs a multi-block metadata region, got {}",
        plan.metadata.blocks
    );
    assert!(
        plan.metadata.inode_table.1 > ps5upload_fpkg::BLOCK,
        "the inode table itself must outgrow a block: {} bytes",
        plan.metadata.inode_table.1
    );
    let flat = plan
        .dirs
        .iter()
        .find(|d| d.path == "data/many")
        .expect("the flat directory is in the plan");
    assert!(
        ps5upload_fpkg::plan::dirent_size("entry00000.bin") as u64
            * (flat.dirents.len() as u64 - 2)
            > ps5upload_fpkg::BLOCK,
        "the flat directory's dirents must outgrow a block"
    );

    assert_eq!(
        std::fs::read(&report.path).unwrap(),
        std::fs::read(&memory.path).unwrap(),
        "the two writers disagree on a multi-block metadata region"
    );

    // The reader walks the whole tree back out through the multi-block region.
    let mut pkg = ps5upload_fpkg::PkgFile::open(&report.path).unwrap();
    let head = pkg.read_at(0, ps5upload_fpkg::fih::HEADER_LEN).unwrap();
    let fih = ps5upload_fpkg::fih::parse(&head).unwrap();
    let container = ps5upload_fpkg::cnt::read(&mut pkg, fih.cnt_offset).unwrap();
    let img = ps5upload_fpkg::outer::open(
        &mut pkg,
        &fih,
        &container,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
    )
    .unwrap();
    let nodes = img.dinodes();
    let inner_image = img.file_data(&nodes[3]);
    let inner = ps5upload_fpkg::inner::read(&inner_image, plan.meta_base).unwrap();
    assert!(inner.flt_ok);
    let mut recovered: Vec<(String, u64)> = inner
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    recovered.sort();
    let mut wanted: Vec<(String, u64)> = files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .chain([(
            "sce_sys/keystone".to_string(),
            ps5upload_fpkg::plan::KEYSTONE_LEN,
        )])
        .collect();
    wanted.sort();
    assert_eq!(recovered.len(), wanted.len(), "every file must survive");
    assert_eq!(recovered, wanted);
    eprintln!(
        "{inodes} inodes in {} metadata blocks; the whole tree walked back",
        plan.metadata.blocks
    );
}

/// The free-space estimate must never be smaller than the package: the guard exists to
/// stop a build that cannot finish, and an underestimate defeats it.
#[test]
fn the_size_estimate_covers_the_package() {
    let source = TempDir::new("estimate-source");
    let out = TempDir::new("estimate-out");
    write_tree(source.path());
    let report = build::build(&request(source.path(), out.path()), &mut |_| {}).unwrap();

    let files = ps5upload_fpkg::source::scan(source.path()).unwrap();
    let plan = ps5upload_fpkg::plan::build(&files).unwrap();
    let estimate = build::estimate_size(&plan).unwrap();
    assert!(
        estimate >= report.size,
        "estimated {estimate} for a {} byte package",
        report.size
    );
    // And it is not wildly pessimistic either: a few megabytes of container and install
    // metadata on top of the image, which for a small package is most of the difference.
    assert!(
        estimate < report.size + 4 * 1024 * 1024,
        "estimated {estimate} for a {} byte package",
        report.size
    );
}

/// The firmware word real titles carry is BCD hex; 0x1160… is 11.60 and 0x0960… is 9.60.
#[test]
fn the_firmware_word_reads_as_a_version() {
    let json = |v: &str| serde_json::json!({ "requiredSystemSoftwareVersion": v });
    assert_eq!(
        ps5upload_fpkg::build::firmware_version(&json("0x1160000000000000")),
        Some("11.60".to_string())
    );
    assert_eq!(
        ps5upload_fpkg::build::firmware_version(&json("0x0960000000000000")),
        Some("09.60".to_string())
    );
    // A plain version is passed through, and so is a word with no version in it.
    assert_eq!(
        ps5upload_fpkg::build::firmware_version(&json("03.000.000")),
        Some("03.000.000".to_string())
    );
    assert_eq!(
        ps5upload_fpkg::build::firmware_version(&json("0x0000000000000000")),
        Some("0x0000000000000000".to_string())
    );
}

/// A source that says `"free"` (or `"upgradable"`) must still produce a package that says
/// `"standard"`: the console shows a lock and refuses to start a title that says otherwise.
/// The user's own `param.json` never changes — the package carries the rewritten bytes.
#[test]
fn a_non_standard_drm_reaches_the_package_as_standard() {
    let source = TempDir::new("drm-source");
    let out = TempDir::new("drm-out");
    write_tree(source.path());
    let param = source.path().join("sce_sys/param.json");
    let title_id = &CONTENT_ID[7..16];
    let original = format!(
        "{{\"contentId\":\"{CONTENT_ID}\",\"contentVersion\":\"01.002.003\",\
         \"applicationDrmType\":\"free\",\"titleName\":\"Scale Test\",\"titleId\":\"{title_id}\"}}"
    );
    std::fs::write(&param, &original).unwrap();

    let report = build::build(&request(source.path(), out.path()), &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    assert_eq!(
        std::fs::read_to_string(&param).unwrap(),
        original,
        "the source file must be left alone"
    );

    // Read the packaged copy back out of the built package's inner image.
    let files = ps5upload_fpkg::source::scan(source.path()).unwrap();
    let plan = ps5upload_fpkg::plan::build(&files).unwrap();
    let mut pkg = ps5upload_fpkg::PkgFile::open(&report.path).unwrap();
    let head = pkg.read_at(0, ps5upload_fpkg::BLOCK as usize).unwrap();
    let fih = ps5upload_fpkg::fih::parse(&head).unwrap();
    let cnt = ps5upload_fpkg::cnt::read(&mut pkg, fih.cnt_offset).unwrap();
    let img = ps5upload_fpkg::outer::open(
        &mut pkg,
        &fih,
        &cnt,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
    )
    .unwrap();
    let nodes = img.dinodes();
    let image = img.file_data(&nodes[3]);
    let mount = ps5upload_fpkg::inner::read(&image, plan.meta_base).unwrap();
    let entry = mount
        .files
        .iter()
        .find(|f| f.path == "sce_sys/param.json")
        .expect("param.json is in the image");
    let at = entry.offset as usize;
    let packaged = &image[at..at + entry.size as usize];
    let text = String::from_utf8_lossy(packaged);
    assert!(
        text.contains("\"applicationDrmType\":\"standard\""),
        "the packaged copy says {text}"
    );
    assert!(!text.contains("free"), "the packaged copy says {text}");
    assert_eq!(
        packaged.len(),
        original.len() + "standard".len() - "free".len(),
        "only the value's length changed"
    );
}

/// Cancelling removes the partial and says so — a 100 GB build must not leave a 100 GB
/// file behind.
#[test]
fn a_cancelled_build_leaves_nothing_behind() {
    let source = TempDir::new("cancel-source");
    let out = TempDir::new("cancel-out");
    write_tree(source.path());
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let mut control = build::BuildControl {
        bytes: None,
        cancel: Some(&cancel),
    };
    let error = match build::build_controlled(
        &request(source.path(), out.path()),
        &mut |_| {},
        &mut control,
    ) {
        Ok(report) => panic!("expected a cancellation, built {}", report.path.display()),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(!out.path().join(format!("{CONTENT_ID}.pkg")).exists());
    assert!(!out
        .path()
        .join(format!("{CONTENT_ID}.pkg.partial"))
        .exists());
}

/// What a caller sees before building: the source, its readiness, the cost and the room.
#[test]
fn inspect_reports_the_source_before_a_build() {
    let source = TempDir::new("inspect-source");
    let out = TempDir::new("inspect-out");
    write_tree(source.path());
    let inspection = build::inspect(source.path(), out.path()).unwrap();
    assert!(
        inspection.ok(),
        "{:?}",
        inspection.warnings().collect::<Vec<_>>()
    );
    assert_eq!(inspection.content_id.as_deref(), Some(CONTENT_ID));
    // The source's own files; the keystone is generated at plan time, not found here.
    assert_eq!(inspection.files, 7);
    assert!(inspection.bytes > 600_000);
    assert_eq!(inspection.title.as_deref(), Some("Scale Test"));
    // Real titles carry the firmware as a BCD hex word; 0x1160… is 11.60.
    assert_eq!(inspection.required_firmware.as_deref(), Some("11.60"));
    assert!(inspection.planned_size > inspection.bytes);
    assert!(inspection.source.starts_with("folder "));
    assert!(inspection.output_free.unwrap_or(u64::MAX) > inspection.planned_size);
    assert!(inspection.checks.iter().any(|c| c.name == "source" && c.ok));
}
