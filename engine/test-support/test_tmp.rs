//! Test scratch files and folders that never outlive the test process.
//!
//! Tests here make their scratch space under the system temp folder with a readable name
//! (`<what>-<tag>-<pid>`), which helps when a run fails. Removing it at the end of the test body
//! misses every failing or panicking test, and many tests never removed it at all: one cleanup
//! found 32,911 leftovers (38 GB) in a developer's temp folder. Wrap the path in
//! [`remove_at_exit`] where it is made instead: the file or folder is deleted when the test
//! process exits, pass, fail or panic (nextest runs each test in a process of its own, so that is
//! right after the test). Statics that hold a per-process folder (a test-only data directory) are
//! covered the same way, which a `Drop` guard cannot do.
//!
//! `PS5UPLOAD_KEEP_TEST_TMP=1` keeps everything and prints what was kept, to inspect a failed run.
//!
//! One source file for every crate: each includes it with
//! `#[path = ".../engine/test-support/test_tmp.rs"] mod test_tmp;` in test code only, so no crate
//! gains a dependency. `scripts/check-test-tmp-leaks.sh` (and CI) fail when a run leaves anything.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

static PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();

unsafe extern "C" {
    // The C runtime's exit hook (every platform Rust tests run on has it). Rust's
    // `std::process::exit`, which the test harness calls, and a normal return from `main` both run
    // it; a test that panics is caught by the harness first, so it runs then too.
    fn atexit(cb: extern "C" fn()) -> std::os::raw::c_int;
}

extern "C" fn remove_registered() {
    let paths = std::mem::take(&mut *PATHS.lock().unwrap_or_else(|e| e.into_inner()));
    let keep = keep_requested();
    for p in paths {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if keep {
            eprintln!("PS5UPLOAD_KEEP_TEST_TMP: kept {}", p.display());
        } else if meta.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn keep_requested() -> bool {
    std::env::var_os("PS5UPLOAD_KEEP_TEST_TMP").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Removes `path` (a file or a folder with everything in it) when this test process exits, and
/// returns it unchanged. Registering a path that never gets made, or one already removed, is fine.
pub fn remove_at_exit(path: impl Into<PathBuf>) -> PathBuf {
    let path = path.into();
    // Never the temp folder itself or anything above it.
    assert!(
        path.is_absolute() && !std::env::temp_dir().starts_with(&path),
        "test_tmp: refusing to remove {}",
        path.display()
    );
    HOOK.call_once(|| {
        // SAFETY: `remove_registered` is a plain `extern "C" fn()` that lives for the whole
        // program; registering it once is all `atexit` needs.
        unsafe {
            atexit(remove_registered);
        }
    });
    PATHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(path.clone());
    path
}

/// `<temp>/<name>`, removed when the test process exits (see [`remove_at_exit`]). Nothing is
/// created; the caller makes the file or folder as before.
pub fn temp_path(name: impl AsRef<Path>) -> PathBuf {
    remove_at_exit(std::env::temp_dir().join(name))
}
