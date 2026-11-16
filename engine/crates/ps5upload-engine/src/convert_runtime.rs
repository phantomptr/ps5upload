//! The zstd runtime on the console, for Convert on the PS5 (Minion's Convert page).
//!
//! A game written in zstd packs needs ampr-emu-ng's `libSceAmpr.sprx` inside it, and the console
//! downloads nothing: the converter there (engine/crates/ps5upload-convert) takes the file from
//! the console's own disk. So this engine puts it there: the release it pins
//! ([`crate::zstd_runtime::PINNED`]), from its cache or GitHub, checked against the digest it
//! ships with, written to
//!
//! `/data/ps5upload/convert/runtime/ampr-emu-ng-<version>/libSceAmpr.sprx`
//!
//! through a temporary name in that same folder, read back and hashed, then renamed in place
//! (one folder, one drive: never a rename across filesystems). The helper
//! (payload/src/console_convert.c) passes the newest such folder's file to the converter.
//!
//! It happens on its own the first time a console with a helper that has the converter answers
//! a status call (once per console per engine run; a failure is tried again after
//! [`RETRY_AFTER`]), in the background, never in the way of the status answer or of a helper
//! send, and also on request: `POST /api/ps5/convert-runtime {host}`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use crate::zstd_runtime::{PackRuntime, RuntimeChoice, PINNED};

/// Where the runtimes go on the console (payload/include/console_convert.h names the same).
pub(crate) const CONSOLE_DIR: &str = "/data/ps5upload/convert/runtime";
const FILE: &str = "libSceAmpr.sprx";
/// The temporary name in the same folder while it is written and checked: `<FILE>.<tag>.part`,
/// a tag of its own for each put, so two puts (this engine's two callers, or two engines) never
/// write one temporary file and rename the other's unchecked bytes into place.
fn part_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!(
        "{FILE}.{}-{}-{nanos}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
/// The first helper that carries the converter.
const FIRST_HELPER: &str = "12.5.0";
/// A failed automatic attempt is tried again on a status answer this long after it.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

/// What the console holds after a put.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Outcome {
    pub version: String,
    pub path: String,
    /// False: the same version was already there with the right digest; nothing was written.
    pub written: bool,
}

/// The few file calls a put makes, so the steps are tested without a console.
pub(crate) trait ConsoleFiles {
    /// The size of a file, or None when there is nothing at `path`.
    fn size(&self, path: &str) -> Result<Option<u64>, String>;
    fn read(&self, path: &str, size: u64) -> Result<Vec<u8>, String>;
    fn mkdir(&self, path: &str) -> Result<(), String>;
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String>;
    /// Within one folder, replacing what is at `to`.
    fn rename(&self, from: &str, to: &str) -> Result<(), String>;
    fn remove(&self, path: &str) -> Result<(), String>;
}

/// The console, through its helper.
struct Helper<'a>(&'a str);

impl ConsoleFiles for Helper<'_> {
    fn size(&self, path: &str) -> Result<Option<u64>, String> {
        match ps5upload_core::fs_ops::fs_stat(self.0, path) {
            Ok(st) if st.kind == "file" => Ok(Some(st.size)),
            Ok(st) => Err(format!("{path} is a {}, not a file", st.kind)),
            Err(e) => {
                let e = format!("{e:#}");
                if ps5upload_core::fs_ops::is_not_found(&e) {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
    fn read(&self, path: &str, size: u64) -> Result<Vec<u8>, String> {
        ps5upload_core::fs_ops::fs_read_whole(self.0, path, size).map_err(|e| format!("{e:#}"))
    }
    fn mkdir(&self, path: &str) -> Result<(), String> {
        ps5upload_core::fs_ops::fs_mkdir(self.0, path).map_err(|e| format!("{e:#}"))
    }
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        ps5upload_core::diagnostics::fs_write_bytes_large(self.0, path, bytes, false)
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }
    fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        ps5upload_core::fs_ops::fs_move_with_timeout(self.0, from, to, true, None)
            .map_err(|e| format!("{e:#}"))
    }
    fn remove(&self, path: &str) -> Result<(), String> {
        ps5upload_core::fs_ops::fs_delete(self.0, path).map_err(|e| format!("{e:#}"))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The folder a version goes in.
pub(crate) fn version_dir(version: &str) -> String {
    format!("{CONSOLE_DIR}/ampr-emu-ng-{version}")
}

/// Whether `path` holds exactly the file whose digest is `sha` (and `len` bytes).
fn holds(fs: &dyn ConsoleFiles, path: &str, len: u64, sha: &str) -> Result<bool, String> {
    match fs.size(path)? {
        Some(size) if size == len => Ok(sha256_hex(&fs.read(path, size)?) == sha),
        _ => Ok(false),
    }
}

/// Puts `bytes` (ampr-emu-ng `version`, already checked against its digest) on the console.
pub(crate) fn put(fs: &dyn ConsoleFiles, version: &str, bytes: &[u8]) -> Result<Outcome, String> {
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!("{version:?} is not a version"));
    }
    let dir = version_dir(version);
    let path = format!("{dir}/{FILE}");
    let part = format!("{dir}/{}", part_name());
    let sha = sha256_hex(bytes);
    let len = bytes.len() as u64;
    if holds(fs, &path, len, &sha)? {
        return Ok(Outcome {
            version: version.to_string(),
            path,
            written: false,
        });
    }
    fs.mkdir(&dir)
        .map_err(|e| format!("creating {dir} on the console: {e}"))?;
    let written = (|| {
        fs.write(&part, bytes)
            .map_err(|e| format!("writing {part} on the console: {e}"))?;
        if !holds(fs, &part, len, &sha)? {
            return Err(format!(
                "{part} read back from the console does not match what was written"
            ));
        }
        fs.rename(&part, &path)
            .map_err(|e| format!("renaming {part} on the console: {e}"))
    })();
    if let Err(e) = written {
        if let Err(why) = fs.remove(&part) {
            crate::log_warn!("zstd runtime: {part} was left on the console: {why}");
        }
        return Err(e);
    }
    // What is in place now is checked too: another put (another engine) may have renamed its
    // own file over it in between. A file there that is not this one is not left for the
    // helper to hand to a converter.
    if !holds(fs, &path, len, &sha)? {
        if let Err(why) = fs.remove(&path) {
            crate::log_warn!("zstd runtime: {path} does not match and could not be removed: {why}");
        }
        return Err(format!(
            "{path} on the console does not match what was put there"
        ));
    }
    Ok(Outcome {
        version: version.to_string(),
        path,
        written: true,
    })
}

/// The pinned runtime, from this machine's cache or GitHub, digest checked.
fn pinned_runtime() -> Result<PackRuntime, String> {
    crate::zstd_runtime::pack_runtime(RuntimeChoice::Pinned)
}

/// Runs `f` while holding `host`'s put lock: the Settings button (`put_handler`) and the
/// automatic put (`after_status`) of one console never write it at once. Blocking: only ever
/// called on a blocking thread (`spawn_blocking`), never held across an `.await`.
fn with_host_lock<T>(host: &str, f: impl FnOnce() -> T) -> T {
    static LOCKS: std::sync::OnceLock<Mutex<HashMap<String, std::sync::Arc<Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let lock = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(host.to_string())
        .or_default()
        .clone();
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// Puts the pinned runtime on the console at `host` (blocking; one put per console at a time).
pub(crate) fn ensure(host: &str) -> Result<Outcome, String> {
    with_host_lock(host, || ensure_unlocked(host))
}

fn ensure_unlocked(host: &str) -> Result<Outcome, String> {
    let rt = pinned_runtime()?;
    debug_assert_eq!(rt.version, PINNED.version);
    let out = put(&Helper(host), &rt.version, &rt.bytes)?;
    if out.written {
        crate::log_info!(
            "zstd runtime: put ampr-emu-ng {} on {host} ({})",
            out.version,
            out.path
        );
    }
    Ok(out)
}

/// Whether a helper of `version` carries the converter. A helper of this engine's own version
/// does too (a development build is not numbered as a release yet).
pub(crate) fn helper_converts(version: &str) -> bool {
    use ps5upload_fpkg::ampr_pack::ng::older;
    let v = version.trim();
    !v.is_empty() && (!older(v, FIRST_HELPER) || v == env!("CARGO_PKG_VERSION"))
}

#[derive(Default)]
struct Seen {
    done: bool,
    running: bool,
    last_try: Option<Instant>,
}

fn seen() -> &'static Mutex<HashMap<String, Seen>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, Seen>>> = std::sync::OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// Whether an automatic put should start now for a console in state `s`.
fn due(s: &Seen, now: Instant) -> bool {
    !s.done
        && !s.running
        && s.last_try
            .is_none_or(|t| now.duration_since(t) >= RETRY_AFTER)
}

/// A console answered a status call with `status` (its helper's `node.status`): when its helper
/// carries the converter and the runtime has not been put there in this engine run, put it, in
/// the background. Never fails or delays the caller; a failure is logged and tried again later.
pub(crate) fn after_status(host: String, status: &serde_json::Value) {
    let version = status.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if host.is_empty() || !helper_converts(version) {
        return;
    }
    // A transfer or an install has the management port: next time.
    if crate::helper_stop::is_busy(&host) || crate::helper_mirror::engine_busy() {
        return;
    }
    {
        let mut map = seen().lock().unwrap_or_else(|e| e.into_inner());
        let s = map.entry(host.clone()).or_default();
        if !due(s, Instant::now()) {
            return;
        }
        s.running = true;
        s.last_try = Some(Instant::now());
    }
    tokio::task::spawn_blocking(move || {
        let r = ensure(&host);
        if let Err(e) = &r {
            crate::log_warn!(
                "zstd runtime: could not put it on {host} (tried again later; zstd packs from Convert on the PS5 need it): {e}"
            );
        }
        let mut map = seen().lock().unwrap_or_else(|e| e.into_inner());
        let s = map.entry(host).or_default();
        s.running = false;
        s.done = r.is_ok();
    });
}

#[derive(Deserialize)]
pub(crate) struct ConvertRuntimeRequest {
    #[serde(alias = "console", alias = "addr")]
    host: String,
}

/// POST /api/ps5/convert-runtime {host} — put the zstd runtime Convert on the PS5 needs (the
/// ampr-emu-ng release this app pins, digest checked) on the console, under
/// /data/ps5upload/convert/runtime/. Answers {version, path, written}; written is false when the
/// same version was already there. Needs the helper up.
pub(crate) async fn put_handler(Json(req): Json<ConvertRuntimeRequest>) -> impl IntoResponse {
    let host = crate::console_addr(&crate::consoles_api::resolve(req.host.trim()));
    if host.is_empty() {
        return crate::json_err(StatusCode::BAD_REQUEST, "host is required").into_response();
    }
    let for_seen = host.clone();
    let r = tokio::task::spawn_blocking(move || ensure(&host)).await;
    match r {
        Ok(Ok(out)) => {
            if let Some(s) = seen()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&for_seen)
            {
                s.done = true;
            }
            (StatusCode::OK, Json(out)).into_response()
        }
        Ok(Err(e)) => crate::json_err(StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => {
            crate::json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A console's files in memory, with ways to make each step fail.
    #[derive(Default)]
    struct Fake {
        files: RefCell<BTreeMap<String, Vec<u8>>>,
        dirs: RefCell<Vec<String>>,
        writes: RefCell<u32>,
        /// What a write actually stores (a flaky drive), when set.
        corrupt: bool,
        fail_rename: bool,
        /// What lands at the rename's target instead (another put renamed its file there).
        rename_lands: Option<Vec<u8>>,
        /// Every temporary name written.
        parts: RefCell<Vec<String>>,
    }

    impl ConsoleFiles for Fake {
        fn size(&self, path: &str) -> Result<Option<u64>, String> {
            Ok(self.files.borrow().get(path).map(|b| b.len() as u64))
        }
        fn read(&self, path: &str, size: u64) -> Result<Vec<u8>, String> {
            let f = self.files.borrow();
            let b = f.get(path).ok_or("ENOENT")?;
            Ok(b[..size as usize].to_vec())
        }
        fn mkdir(&self, path: &str) -> Result<(), String> {
            self.dirs.borrow_mut().push(path.to_string());
            Ok(())
        }
        fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
            let dir = &path[..path.rfind('/').unwrap()];
            assert!(self.dirs.borrow().iter().any(|d| d == dir), "{dir} made");
            *self.writes.borrow_mut() += 1;
            self.parts.borrow_mut().push(path.to_string());
            let mut b = bytes.to_vec();
            if self.corrupt {
                b[0] ^= 1;
            }
            self.files.borrow_mut().insert(path.to_string(), b);
            Ok(())
        }
        fn rename(&self, from: &str, to: &str) -> Result<(), String> {
            // Never across folders.
            assert_eq!(
                from.rsplit_once('/').unwrap().0,
                to.rsplit_once('/').unwrap().0
            );
            if self.fail_rename {
                return Err("EIO".into());
            }
            let b = self.files.borrow_mut().remove(from).ok_or("ENOENT")?;
            let b = self.rename_lands.clone().unwrap_or(b);
            self.files.borrow_mut().insert(to.to_string(), b);
            Ok(())
        }
        fn remove(&self, path: &str) -> Result<(), String> {
            self.files.borrow_mut().remove(path);
            Ok(())
        }
    }

    const FINAL: &str = "/data/ps5upload/convert/runtime/ampr-emu-ng-0.4.1/libSceAmpr.sprx";

    #[test]
    fn it_is_written_through_a_temporary_name_and_renamed_in_its_folder() {
        let fs = Fake::default();
        let out = put(&fs, "0.4.1", b"EMULATOR").unwrap();
        assert_eq!(
            out,
            Outcome {
                version: "0.4.1".into(),
                path: FINAL.into(),
                written: true
            }
        );
        let files = fs.files.borrow();
        assert_eq!(files.keys().collect::<Vec<_>>(), vec![FINAL]);
        assert_eq!(files[FINAL], b"EMULATOR");
    }

    #[test]
    fn the_same_version_already_there_is_left_alone() {
        let fs = Fake::default();
        fs.files
            .borrow_mut()
            .insert(FINAL.into(), b"EMULATOR".to_vec());
        let out = put(&fs, "0.4.1", b"EMULATOR").unwrap();
        assert!(!out.written);
        assert_eq!(*fs.writes.borrow(), 0);
    }

    #[test]
    fn a_damaged_copy_is_replaced() {
        let fs = Fake::default();
        fs.files
            .borrow_mut()
            .insert(FINAL.into(), b"EMULATOS".to_vec());
        assert!(put(&fs, "0.4.1", b"EMULATOR").unwrap().written);
        assert_eq!(fs.files.borrow()[FINAL], b"EMULATOR");
        // A short one too.
        fs.files.borrow_mut().insert(FINAL.into(), b"EMU".to_vec());
        assert!(put(&fs, "0.4.1", b"EMULATOR").unwrap().written);
    }

    #[test]
    fn a_copy_that_reads_back_wrong_is_never_put_in_place() {
        let fs = Fake {
            corrupt: true,
            ..Default::default()
        };
        let e = put(&fs, "0.4.1", b"EMULATOR").unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        assert!(
            fs.files.borrow().is_empty(),
            "the temporary file is removed"
        );
    }

    #[test]
    fn a_failed_rename_leaves_nothing_behind() {
        let fs = Fake {
            fail_rename: true,
            ..Default::default()
        };
        assert!(put(&fs, "0.4.1", b"EMULATOR").is_err());
        assert!(fs.files.borrow().is_empty());
    }

    #[test]
    fn each_put_writes_a_temporary_name_of_its_own_in_the_folder() {
        let fs = Fake::default();
        put(&fs, "0.4.1", b"EMULATOS").unwrap();
        put(&fs, "0.4.1", b"EMULATOR").unwrap();
        let parts = fs.parts.borrow();
        assert_eq!(parts.len(), 2);
        assert_ne!(parts[0], parts[1]);
        for p in parts.iter() {
            let (dir, name) = p.rsplit_once('/').unwrap();
            assert_eq!(dir, version_dir("0.4.1"));
            // Never the name the helper looks for.
            assert!(
                name.starts_with("libSceAmpr.sprx.") && name.ends_with(".part"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_final_file_that_is_not_this_one_after_the_rename_is_removed() {
        let fs = Fake {
            rename_lands: Some(b"EMULATOS".to_vec()),
            ..Default::default()
        };
        let e = put(&fs, "0.4.1", b"EMULATOR").unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        assert!(fs.files.borrow().is_empty(), "{:?}", fs.files.borrow());
    }

    #[test]
    fn puts_to_one_console_take_turns() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        let inside = Arc::new(AtomicBool::new(false));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (inside, overlaps) = (Arc::clone(&inside), Arc::clone(&overlaps));
                std::thread::spawn(move || {
                    with_host_lock("192.0.2.7", || {
                        if inside.swap(true, Ordering::SeqCst) {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(30));
                        inside.store(false, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(overlaps.load(Ordering::SeqCst), 0);
        // Another console is not held up by this one's lock.
        let held = with_host_lock("192.0.2.7", || with_host_lock("192.0.2.8", || 1));
        assert_eq!(held, 1);
    }

    #[test]
    fn a_version_is_a_folder_name_and_nothing_else() {
        let fs = Fake::default();
        for bad in ["", "../x", "0.4/1", "0 4"] {
            assert!(put(&fs, bad, b"E").is_err(), "{bad:?}");
        }
        assert_eq!(*fs.writes.borrow(), 0);
    }

    #[test]
    fn only_a_helper_with_the_converter_gets_it() {
        assert!(helper_converts("12.5.0"));
        assert!(helper_converts("12.10.1"));
        assert!(helper_converts("13.0.0"));
        assert!(!helper_converts("12.4.10"));
        assert!(!helper_converts(""));
        assert!(helper_converts(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn an_automatic_put_is_once_per_console_and_a_failure_waits() {
        let now = Instant::now();
        assert!(due(&Seen::default(), now));
        let done = Seen {
            done: true,
            ..Default::default()
        };
        assert!(!due(&done, now));
        let running = Seen {
            running: true,
            ..Default::default()
        };
        assert!(!due(&running, now));
        let failed = Seen {
            last_try: Some(now),
            ..Default::default()
        };
        assert!(!due(&failed, now + Duration::from_secs(60)));
        assert!(due(&failed, now + RETRY_AFTER));
    }

    #[test]
    fn the_folder_is_the_one_the_helper_looks_in() {
        let h = include_str!("../../../../payload/include/console_convert.h");
        assert!(h.contains(&format!("\"{CONSOLE_DIR}\"")), "{CONSOLE_DIR}");
        assert!(version_dir("0.4.1").ends_with("/ampr-emu-ng-0.4.1"));
    }
}
