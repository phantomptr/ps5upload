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
    let put = |path: &str, data: Vec<u8>| {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &data).unwrap();
    };
    put("eboot.bin", (0..4000u32).map(|i| (i % 241) as u8).collect());
    put(
        "data/one.bin",
        (0..600_000u32).map(|i| (i % 199) as u8).collect(),
    );
    put("data/two.bin", vec![0x5A; 5000]);
    put(
        "sce_sys/param.json",
        format!("{{\"contentId\":\"{CONTENT_ID}\",\"contentVersion\":\"01.002.003\"}}")
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
    assert!(samples.windows(2).all(|w| w[0].0 <= w[1].0), "{samples:?}");
    assert_eq!(
        samples.last().unwrap().1,
        samples[0].1,
        "one total throughout"
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
