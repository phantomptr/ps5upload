//! Make a PS4 package small enough to keep as a test fixture: every 4 KiB the reader never
//! reads (padding, digests, unused blocks) is zeroed, so the file gzips to little, and the copy
//! is checked to read back the same files as the original.
//!
//! `cargo run -p ps5upload-fpkg --example ps4_fixture_trim -- <in.pkg> <out.pkg>`

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use ps5upload_fpkg::ps4_pkg::Ps4Package;
use ps5upload_fpkg::source::SourceTree;

const SECTOR: u64 = 0x1000;

/// A file that notes every sector a read touches.
struct Recording {
    inner: std::io::Cursor<Vec<u8>>,
    seen: Arc<Mutex<BTreeSet<u64>>>,
}

impl Read for Recording {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let at = self.inner.position();
        let n = self.inner.read(buf)?;
        if n > 0 {
            let mut seen = self.seen.lock().unwrap();
            for s in at / SECTOR..=(at + n as u64 - 1) / SECTOR {
                seen.insert(s);
            }
        }
        Ok(n)
    }
}

impl Seek for Recording {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

/// Every file of the package, or the container alone when it has no image.
fn read_all(bytes: Vec<u8>, seen: Arc<Mutex<BTreeSet<u64>>>) -> Vec<(String, Vec<u8>)> {
    let len = bytes.len() as u64;
    let reader = Recording {
        inner: std::io::Cursor::new(bytes),
        seen,
    };
    match Ps4Package::open(
        Box::new(reader),
        len,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
        "fixture".into(),
    ) {
        Ok(mut p) => {
            let paths: Vec<String> = p.files().iter().map(|f| f.path.clone()).collect();
            paths
                .into_iter()
                .map(|path| {
                    let data = p.read(&path).expect("read");
                    (path, data)
                })
                .collect()
        }
        Err(e) => {
            eprintln!("no image to read ({e}); keeping what the container reads");
            Vec::new()
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: ps4_fixture_trim <in.pkg> <out.pkg>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&src).expect("read the package");
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let want = read_all(bytes.clone(), seen.clone());
    // The container is always read, image or not: the header region and its entries.
    let _ = ps5upload_fpkg::ps4_pkg::peek(
        Box::new(Recording {
            inner: std::io::Cursor::new(bytes.clone()),
            seen: seen.clone(),
        }),
        bytes.len() as u64,
    );
    let seen = seen.lock().unwrap().clone();
    let mut trimmed = vec![0u8; bytes.len()];
    for s in &seen {
        let from = (s * SECTOR) as usize;
        let to = (from + SECTOR as usize).min(bytes.len());
        trimmed[from..to].copy_from_slice(&bytes[from..to]);
    }
    let got = read_all(trimmed.clone(), Arc::default());
    assert_eq!(want, got, "the trimmed copy reads back differently");
    std::fs::write(&out, &trimmed).expect("write the copy");
    println!(
        "{} files; kept {} of {} sectors",
        want.len(),
        seen.len(),
        bytes.len().div_ceil(SECTOR as usize)
    );
}
