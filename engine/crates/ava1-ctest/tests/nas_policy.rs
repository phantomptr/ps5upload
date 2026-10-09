//! SPEC.md §11.4 for sources that go through the engine's `SourceFs` (a NAS): a backend
//! that reports mtimes round-trips them into the manifest and uses skip-existing; one that
//! does not falls back to verify (size plus root) and still skips identical files. The
//! receiver is the real C one.
#![cfg(unix)]
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ava1::gen;
use ava1::manifest;
use ava1::send::SendOptions;
use ava1_ctest::*;
use ps5upload_ava1::source::FsSource;
use ps5upload_ava1::upload::apply_existing_policy;
use ps5upload_core::source_fs::{ReadSeek, SourceFs, SourceMeta};

const MTIME: u64 = 1_650_000_000;

/// A one-level in-memory tree; `with_mtime` decides whether the backend can report times.
#[derive(Debug)]
struct Nas {
    files: BTreeMap<String, Vec<u8>>,
    with_mtime: bool,
}

impl SourceFs for Nas {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>> {
        let b = self
            .files
            .get(p.to_str().unwrap())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        Ok(Box::new(Cursor::new(b.clone())))
    }
    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta> {
        match self.files.get(p.to_str().unwrap()) {
            Some(b) => Ok(SourceMeta {
                len: b.len() as u64,
                is_dir: false,
                is_file: true,
            }),
            None => Ok(SourceMeta {
                len: 0,
                is_dir: true,
                is_file: false,
            }),
        }
    }
    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
        let _ = p;
        Ok(self
            .files
            .keys()
            .map(|k| (PathBuf::from(k), false))
            .collect())
    }
    fn mtime(&self, p: &Path) -> Option<u64> {
        (self.with_mtime && self.files.contains_key(p.to_str().unwrap())).then_some(MTIME)
    }
}

fn nas(with_mtime: bool) -> Nas {
    let mut files = BTreeMap::new();
    files.insert("/src/a".to_string(), b"AAAA".to_vec());
    files.insert("/src/b".to_string(), b"BBBB".to_vec());
    Nas { files, with_mtime }
}

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-nas-{tag}-{}", std::process::id()))
}

fn manifest_of(with_mtime: bool) -> (Arc<FsSource>, manifest::Manifest, SendOptions) {
    let src = Arc::new(FsSource::new(Arc::new(nas(with_mtime)), "/src".into()));
    let mut m = manifest::walk(src.as_ref(), &|_| false).unwrap();
    let mut o = SendOptions::upload("/dest");
    apply_existing_policy(src.as_ref(), &mut m, &mut o).unwrap();
    (src, m, o)
}

/// A destination holding `a` identical to the source and `b` of the same size but other
/// bytes, both stamped with `MTIME` as an earlier upload (which applies the mtime) would.
fn dest(t: &Path) -> PathBuf {
    let d = t.join("dest");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a"), b"AAAA").unwrap();
    std::fs::write(d.join("b"), b"XXXX").unwrap();
    for n in ["a", "b"] {
        std::fs::File::options()
            .write(true)
            .open(d.join(n))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(MTIME))
            .unwrap();
    }
    d
}

#[test]
fn a_source_with_mtimes_round_trips_them_and_uses_skip_existing() {
    let (_, m, o) = manifest_of(true);
    assert!(
        m.entries.iter().all(|e| e.mtime == MTIME),
        "{:?}",
        m.entries
    );
    assert_eq!(o.policy, gen::POLICY_SKIP_EXISTING);
    assert!(
        m.entries.iter().all(|e| e.root.is_none()),
        "no hashing needed"
    );

    let t = tmp("with");
    let r = CRecv::open(&t.join("jobs"), &dest(&t), 0, o.policy, 0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    // size + mtime match for both, as skip-existing defines (b differs only in content).
    assert!(ev.contains("done=0+2"), "{ev}");
}

#[test]
fn a_source_without_mtimes_falls_back_to_verify_and_still_skips_identical_files() {
    let (_, m, o) = manifest_of(false);
    assert!(m.entries.iter().all(|e| e.mtime == 0));
    assert_eq!(o.policy, gen::POLICY_VERIFY);
    assert_eq!(m.entries[0].root, Some(*blake3::hash(b"AAAA").as_bytes()));
    assert_eq!(m.entries[1].root, Some(*blake3::hash(b"BBBB").as_bytes()));

    let t = tmp("without");
    let r = CRecv::open(&t.join("jobs"), &dest(&t), 0, o.policy, 0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    // only `a` hashes equal; `b` has the right size but other bytes and is re-sent.
    assert!(ev.contains("done=0+1,") && !ev.contains("done=0+2"), "{ev}");
}
