//! Verifies the Rust corpus code against the developer's real library corpus.
//!
//! The corpus is gitignored — it is Sony binaries harvested from the user's own
//! games — so this cannot run in CI and no-ops when it is absent. It exists
//! because the synthetic fixtures in the unit tests were written from what the
//! Python collector found, and a port that agrees with its own fixtures but not
//! with real files would be worthless.

use std::path::PathBuf;

use ps5upload_core::fakelibs::{code_id, param_site, sdk_pair, sha256_hex};

fn corpus_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fakelibs")
        .canonicalize()
        .ok()?;
    root.join("manifest.json").exists().then_some(root)
}

#[test]
fn every_real_build_yields_an_sdk_pair() {
    let Some(root) = corpus_root() else {
        eprintln!("no local corpus; skipping");
        return;
    };
    let mut checked = 0;
    let mut missing = Vec::new();
    for entry in walk(&root.join("builds")) {
        let data = std::fs::read(&entry).unwrap();
        checked += 1;
        if param_site(&data).is_none() || sdk_pair(&data).is_none() {
            missing.push(entry);
        }
    }
    assert!(checked > 0, "corpus has no build files");
    // The ELF walk alone left 27 of 52 builds with no readable pair, which made
    // every SDK-based decision operate on absent data. With the magic-scan
    // fallback, every build reads.
    assert!(
        missing.is_empty(),
        "{} of {checked} builds have no SDK pair: {missing:?}",
        missing.len()
    );
}

#[test]
fn same_size_builds_of_one_library_are_stamp_variants() {
    let Some(root) = corpus_root() else {
        eprintln!("no local corpus; skipping");
        return;
    };
    // Measured with the Python collector: two libSceAmpr builds and two
    // libScePlayGo builds differ only in the 32-byte digest at 0x510 and the
    // 8-byte SDK pair, and are byte-identical once masked. Any pair of builds
    // of the same library with the same size must therefore share a code_id.
    let mut compared = 0;
    for lib_dir in walk_dirs(&root.join("builds")) {
        let files: Vec<PathBuf> = walk(&lib_dir);
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let a = std::fs::read(&files[i]).unwrap();
                let b = std::fs::read(&files[j]).unwrap();
                if a.len() != b.len() {
                    continue;
                }
                compared += 1;
                assert_ne!(sha256_hex(&a), sha256_hex(&b), "corpus stored a duplicate");
                assert_eq!(
                    code_id(&a),
                    code_id(&b),
                    "same-size builds of {} should differ only in their stamp: {:?} vs {:?}",
                    lib_dir.display(),
                    files[i].file_name(),
                    files[j].file_name(),
                );
            }
        }
    }
    assert!(compared > 0, "no same-size build pairs found to compare");
    eprintln!("verified {compared} same-size build pair(s) collapse to one code_id");
}

fn walk(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "sprx" || x == "prx") {
                out.push(p);
            }
        }
    }
    out
}

fn walk_dirs(dir: &PathBuf) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}
