//! `/api/collection/check`: checks a copy in the Collection end to end, in the background.
//!
//! What "checked" means depends on what the copy is, and the result says which it was:
//! - `package`: a PS5 debug package Convert can open is verified block by block against the
//!   digests it carries (the same check Convert runs on what it builds).
//! - `archive`: every entry of a `.zip` / `.7z` / `.rar` is decoded and its checksum checked.
//! - `files`: a game folder or image has every file read back through its filesystem.
//! - `bytes`: anything else (a retail or PS4 package, a copy on a server) is read from the first
//!   byte to the last, which finds a copy that is cut short or sits on failing storage, but not
//!   one whose bytes are wrong yet readable.
//!
//! One check reads at a time: two at once on the same drive would only slow both. Only paths
//! that are copies in the Collection can be checked.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::{http::StatusCode, response::Response, Json};
use serde::{Deserialize, Serialize};

use crate::collection_api::{err, library};

/// At most this many problems are kept for one copy; `problems_total` counts them all.
const MAX_PROBLEMS: usize = 50;
/// Finished checks kept for the screen; the oldest go first.
const MAX_KEPT: usize = 200;
const CHUNK: usize = 8 << 20;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Problem {
    /// The file inside the copy, or empty for the copy as a whole.
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckStatus {
    pub path: String,
    /// `queued` (waiting its turn), `running`, `ok`, `problems`, `failed` (could not be
    /// checked at all: `error` says why) or `cancelled`.
    pub state: String,
    /// `package`, `archive`, `files` or `bytes`: see the module docs.
    pub method: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub problems: Vec<Problem>,
    pub problems_total: u64,
    pub error: Option<String>,
    pub started_ms: u64,
    pub finished_ms: u64,
}

impl CheckStatus {
    fn active(&self) -> bool {
        self.state == "queued" || self.state == "running"
    }
}

struct Job {
    status: Mutex<CheckStatus>,
    cancel: AtomicBool,
}

impl Job {
    fn update(&self, f: impl FnOnce(&mut CheckStatus)) {
        f(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
    }
    fn problem(&self, file: &str, error: impl Into<String>) {
        self.update(|s| {
            s.problems_total += 1;
            if s.problems.len() < MAX_PROBLEMS {
                s.problems.push(Problem {
                    file: file.to_string(),
                    error: error.into(),
                });
            }
        });
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static J: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One check reads at a time; the others wait here, `queued`.
fn turn() -> &'static Mutex<()> {
    static T: OnceLock<Mutex<()>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Serialize)]
struct Checks {
    checks: Vec<CheckStatus>,
}

fn all_checks() -> Json<Checks> {
    let map = jobs().lock().unwrap_or_else(|e| e.into_inner());
    let mut checks: Vec<CheckStatus> = map
        .values()
        .map(|j| j.status.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .collect();
    checks.sort_by_key(|c| c.started_ms);
    Json(Checks { checks })
}

/// GET /api/collection/check: every check this engine has run or is running.
pub async fn get_checks() -> Response {
    axum::response::IntoResponse::into_response(all_checks())
}

#[derive(Deserialize)]
pub struct CheckReq {
    paths: Vec<String>,
}

/// POST /api/collection/check `{paths}`: checks these copies, one after another. A copy whose
/// check is already queued or running is left to it.
pub async fn post_check(Json(req): Json<CheckReq>) -> Response {
    let Some(lib) = library() else {
        return err(StatusCode::BAD_REQUEST, "scan the Collection first");
    };
    let mut wanted = Vec::new();
    for p in &req.paths {
        let kind = lib
            .games
            .values()
            .flat_map(|g| g.locations.iter())
            .find(|l| &l.absolute_path == p)
            .map(|l| l.kind.clone());
        match kind {
            Some(k) => wanted.push((p.clone(), k)),
            None => {
                return err(
                    StatusCode::NOT_FOUND,
                    format!("{p} is not a copy in the Collection"),
                )
            }
        }
    }
    start(wanted, tokio::runtime::Handle::current());
    get_checks().await
}

/// Queues a check of each `(path, kind)` not already being checked.
fn start(wanted: Vec<(String, String)>, handle: tokio::runtime::Handle) {
    let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
    for (path, kind) in wanted {
        if map
            .get(&path)
            .is_some_and(|j| j.status.lock().unwrap_or_else(|e| e.into_inner()).active())
        {
            continue;
        }
        let job = Arc::new(Job {
            status: Mutex::new(CheckStatus {
                path: path.clone(),
                state: "queued".into(),
                started_ms: now_ms(),
                ..CheckStatus::default()
            }),
            cancel: AtomicBool::new(false),
        });
        map.insert(path.clone(), job.clone());
        let handle = handle.clone();
        std::thread::spawn(move || {
            // A copy on a server is read through the engine's runtime.
            let _rt = handle.enter();
            let _turn = turn().lock().unwrap_or_else(|e| e.into_inner());
            if job.cancelled() {
                job.update(|s| {
                    s.state = "cancelled".into();
                    s.finished_ms = now_ms();
                });
                return;
            }
            job.update(|s| s.state = "running".into());
            let outcome = run(&job, Path::new(&path), &kind);
            job.update(|s| {
                s.finished_ms = now_ms();
                s.state = match outcome {
                    Err(e) if job.cancel.load(Ordering::Relaxed) || e == "cancelled" => {
                        "cancelled".into()
                    }
                    Err(e) => {
                        s.error = Some(e);
                        "failed".into()
                    }
                    Ok(()) if s.problems_total > 0 => "problems".into(),
                    Ok(()) => "ok".into(),
                };
            });
        });
    }
    prune(&mut map);
}

/// Drops the oldest finished checks past [`MAX_KEPT`].
fn prune(map: &mut HashMap<String, Arc<Job>>) {
    if map.len() <= MAX_KEPT {
        return;
    }
    let mut done: Vec<(u64, String)> = map
        .iter()
        .filter_map(|(k, j)| {
            let s = j.status.lock().unwrap_or_else(|e| e.into_inner());
            (!s.active()).then(|| (s.finished_ms, k.clone()))
        })
        .collect();
    done.sort();
    for (_, k) in done.into_iter().take(map.len() - MAX_KEPT) {
        map.remove(&k);
    }
}

#[derive(Deserialize)]
pub struct CancelReq {
    /// One copy's check; every queued and running check when absent.
    path: Option<String>,
}

/// POST /api/collection/check/cancel `{path?}`
pub async fn post_check_cancel(Json(req): Json<CancelReq>) -> Response {
    {
        let map = jobs().lock().unwrap_or_else(|e| e.into_inner());
        for (k, j) in map.iter() {
            if req.path.as_deref().is_none_or(|p| p == k) {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    get_checks().await
}

/// Checks the copy at `path` (of Collection type `kind`). Problems found are recorded on the
/// job; `Err` is a copy that could not be checked at all.
fn run(job: &Job, path: &Path, kind: &str) -> Result<(), String> {
    let remote = ps5upload_fpkg::remote_source::is_remote(path);
    match kind {
        "pkg" => {
            if !remote && package(job, path)? {
                return Ok(());
            }
            package_header(job, path);
            bytes(job, path)
        }
        "zip" | "7z" | "rar" if !remote => archive(job, path),
        "folder" | "mount.exfat" | "mount.ffpkg" | "mount.ffpfs" | "mount.ffpfsc" => {
            files(job, path)
        }
        _ => bytes(job, path),
    }
}

/// A PS5 debug package, verified against its own digests. `Ok(false)`: not a package this can
/// open (retail, PS4, PS3), so the caller reads it byte by byte instead.
fn package(job: &Job, path: &Path) -> Result<bool, String> {
    job.update(|s| s.method = "package".into());
    let mut progress = |done: u64, total: u64| {
        job.update(|s| {
            s.bytes_done = done;
            s.bytes_total = total;
        });
    };
    match ps5upload_fpkg::verify::verify_streaming_cancellable(
        path,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
        &job.cancel,
        &mut progress,
    ) {
        Ok(report) => {
            for c in report.checks.iter().filter(|c| !c.ok) {
                let what = if c.detail.is_empty() {
                    c.name.clone()
                } else {
                    format!("{} ({})", c.name, c.detail)
                };
                job.problem("", format!("failed: {what}"));
            }
            Ok(true)
        }
        Err(_) if job.cancelled() => Err("cancelled".into()),
        Err(_) => {
            job.update(|s| {
                s.bytes_done = 0;
                s.bytes_total = 0;
            });
            Ok(false)
        }
    }
}

/// The package header, for a package read byte by byte: one that says it is longer than the
/// file is cut short.
fn package_header(job: &Job, path: &Path) {
    let opened = if ps5upload_fpkg::remote_source::is_remote(path) {
        ps5upload_fpkg::remote_source::open_file(path).ok()
    } else {
        std::fs::File::open(path).ok().and_then(|f| {
            let n = f.metadata().ok()?.len();
            Some((Box::new(f) as Box<dyn ps5upload_fpkg::ReadSeek>, n))
        })
    };
    let Some((mut f, size)) = opened else {
        return;
    };
    match ps5upload_pkg::parse_pkg_from(&mut f, size, path) {
        Ok(h) => {
            for w in h
                .warnings
                .iter()
                .filter(|w| w.contains("truncat") || w.contains("shorter"))
            {
                job.problem("", w.clone());
            }
        }
        Err(e) => job.problem("", format!("the package header can't be read: {e}")),
    }
}

fn archive(job: &Job, path: &Path) -> Result<(), String> {
    job.update(|s| s.method = "archive".into());
    let mut progress = |done: u64, total: u64| {
        job.update(|s| {
            s.bytes_done = done;
            s.bytes_total = total;
        });
    };
    match ps5upload_core::archive_extract::test(path, None, &mut progress, &job.cancel) {
        Ok(()) => Ok(()),
        Err(_) if job.cancelled() => Err("cancelled".into()),
        Err(e) => {
            let msg = format!("{e:#}");
            // Can't be opened at all (password, unsupported layout): nothing was checked.
            if job.status.lock().map(|s| s.bytes_done == 0).unwrap_or(true)
                && (msg.contains("password") || msg.contains("not supported"))
            {
                return Err(msg);
            }
            job.problem("", msg);
            Ok(())
        }
    }
}

/// A game folder or image: every file read back through its filesystem, and the two files a
/// game cannot start without looked for.
fn files(job: &Job, path: &Path) -> Result<(), String> {
    job.update(|s| s.method = "files".into());
    // An image on this computer an in-place edit was interrupted in is settled before it is
    // read: a part-edited image would read as damaged.
    if !ps5upload_fpkg::remote_source::is_remote(path) && path.is_file() {
        crate::image_edit_in_place::before_open(path)?;
    }
    let mut tree = ps5upload_fpkg::source::open(path).map_err(|e| format!("{e}"))?;
    let list = tree.files().to_vec();
    let total: u64 = list.iter().map(|f| f.size).sum();
    job.update(|s| {
        s.files_total = list.len() as u64;
        s.bytes_total = total;
    });
    let has = |suffix: &str| {
        list.iter().any(|f| {
            let p = f.path.to_ascii_lowercase();
            p == suffix || p.ends_with(&format!("/{suffix}"))
        })
    };
    if !has("sce_sys/param.json") && !has("sce_sys/param.sfo") {
        job.problem(
            "sce_sys",
            "no param.json or param.sfo: the console can't read this game",
        );
    }
    if !has("eboot.bin") {
        job.problem("eboot.bin", "missing: the game can't start without it");
    }
    let mut done = 0u64;
    for f in &list {
        let mut at = 0u64;
        while at < f.size {
            if job.cancelled() {
                return Err("cancelled".into());
            }
            let want = (f.size - at).min(CHUNK as u64) as usize;
            match tree.read_range(&f.path, at, want) {
                Ok(b) if b.len() == want => {}
                Ok(b) => {
                    job.problem(
                        &f.path,
                        format!("ends early: {} of {} bytes", at + b.len() as u64, f.size),
                    );
                    break;
                }
                Err(e) => {
                    job.problem(&f.path, format!("can't be read at byte {at}: {e}"));
                    break;
                }
            }
            at += want as u64;
            done += want as u64;
            job.update(|s| s.bytes_done = done);
        }
        // A file given up on still counts as gone through.
        done += f.size - at.min(f.size);
        job.update(|s| {
            s.files_done += 1;
            s.bytes_done = done;
        });
    }
    Ok(())
}

/// Every byte of a single file, start to end.
fn bytes(job: &Job, path: &Path) -> Result<(), String> {
    job.update(|s| s.method = "bytes".into());
    let (mut f, size): (Box<dyn ps5upload_fpkg::ReadSeek>, u64) =
        if ps5upload_fpkg::remote_source::is_remote(path) {
            ps5upload_fpkg::remote_source::open_file(path).map_err(|e| format!("{e}"))?
        } else {
            let f = std::fs::File::open(path).map_err(|e| format!("{e}"))?;
            let n = f.metadata().map_err(|e| format!("{e}"))?.len();
            (Box::new(f), n)
        };
    job.update(|s| s.bytes_total = size);
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        if job.cancelled() {
            return Err("cancelled".into());
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                job.problem("", format!("can't be read at byte {done}: {e}"));
                return Ok(());
            }
        };
        done += n as u64;
        job.update(|s| s.bytes_done = done);
    }
    if done < size {
        job.problem("", format!("ends early: {done} of {size} bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch folder for one test, removed when it ends.
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("ps5u-check-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Scratch(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn job() -> Job {
        Job {
            status: Mutex::new(CheckStatus::default()),
            cancel: AtomicBool::new(false),
        }
    }

    fn game_folder(dir: &Path) {
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(dir.join("eboot.bin"), vec![7u8; 3 * CHUNK + 5]).unwrap();
    }

    #[test]
    fn a_whole_game_folder_reads_back_clean() {
        let t = Scratch::new("clean");
        game_folder(t.path());
        let j = job();
        run(&j, t.path(), "folder").unwrap();
        let s = j.status.lock().unwrap();
        assert_eq!(s.method, "files");
        assert_eq!(s.problems_total, 0, "{:?}", s.problems);
        assert_eq!(s.files_done, 2);
        assert_eq!(s.bytes_done, s.bytes_total);
    }

    #[test]
    fn a_folder_without_eboot_or_param_says_so() {
        let t = Scratch::new("missing");
        std::fs::write(t.path().join("data.bin"), b"x").unwrap();
        let j = job();
        run(&j, t.path(), "folder").unwrap();
        let s = j.status.lock().unwrap();
        assert_eq!(s.problems_total, 2, "{:?}", s.problems);
    }

    #[test]
    fn a_damaged_zip_entry_is_a_problem_and_a_good_one_is_not() {
        use std::io::Write;
        let t = Scratch::new("zip");
        let good = t.path().join("game.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&good).unwrap());
            z.start_file::<_, ()>(
                "PPSA00001/eboot.bin",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            z.write_all(&vec![1u8; 4096]).unwrap();
            z.finish().unwrap();
        }
        let j = job();
        run(&j, &good, "zip").unwrap();
        assert_eq!(j.status.lock().unwrap().problems_total, 0);

        // Flip a byte of the stored data: the CRC no longer matches.
        let mut bytes = std::fs::read(&good).unwrap();
        let at = bytes.windows(4).position(|w| w == [1, 1, 1, 1]).unwrap() + 100;
        bytes[at] ^= 0xFF;
        let bad = t.path().join("bad.zip");
        std::fs::write(&bad, bytes).unwrap();
        let j = job();
        run(&j, &bad, "zip").unwrap();
        let s = j.status.lock().unwrap();
        assert_eq!(s.method, "archive");
        assert_eq!(s.problems_total, 1, "{:?}", s.problems);
    }

    #[test]
    fn a_file_is_read_to_the_end_and_cancel_stops_it() {
        let t = Scratch::new("bytes");
        let f = t.path().join("game.pkg");
        std::fs::write(&f, vec![0u8; CHUNK + 1]).unwrap();
        let j = job();
        bytes(&j, &f).unwrap();
        assert_eq!(j.status.lock().unwrap().bytes_done, CHUNK as u64 + 1);

        let j = job();
        j.cancel.store(true, Ordering::Relaxed);
        assert_eq!(bytes(&j, &f).unwrap_err(), "cancelled");
    }

    #[test]
    fn a_package_that_is_not_a_debug_package_is_read_byte_by_byte() {
        let t = Scratch::new("retail");
        let f = t.path().join("retail.pkg");
        std::fs::write(&f, vec![0u8; 70_000]).unwrap();
        let j = job();
        run(&j, &f, "pkg").unwrap();
        let s = j.status.lock().unwrap();
        assert_eq!(s.method, "bytes");
        assert_eq!(s.bytes_done, 70_000);
    }
}
