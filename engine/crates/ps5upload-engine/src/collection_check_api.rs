//! `/api/collection/check`: checks a copy in the Collection end to end, in the background.
//!
//! What "checked" means depends on what the copy is, and the result says which it was:
//! - `package`: a PS5 debug package Convert can open is verified block by block against the
//!   digests it carries (the same check Convert runs on what it builds). What its problems add
//!   up to is in `damage`: data that is not what the package says (how many blocks of how
//!   many), a file cut short, or only the package's own header information not matching.
//! - `archive`: every entry of a `.zip` / `.7z` / `.rar` is decoded and its checksum checked.
//! - `files`: a game folder or image has every file read back through its filesystem.
//! - `bytes`: anything else (a retail or PS4 package, a copy on a server) is read from the first
//!   byte to the last, which finds a copy that sits on failing storage, but not one whose bytes
//!   are wrong yet readable. A package is also held against its own header: one shorter than
//!   the header says is cut short, and `damage` says so (`truncated`). A split package is one
//!   package: every part is read, in order, and a part that is missing is a problem named by
//!   that part.
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
    /// For a failed check of a package, what the check looks at: `data` (the game's data
    /// blocks), `structure` (the image's own tables), `metadata` (what the package says about
    /// itself: header, digests, install manifest) or `truncated` (the file ends early).
    /// Absent for any other problem.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
}

/// What a package's problems add up to, for a screen to say in plain words. Counts that don't
/// belong to the `kind` are 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Damage {
    /// - `data`: `blocks_bad` of the `blocks_total` data blocks (`block_bytes` each) are not
    ///   the bytes the package's own digests say they are. The usual result of a copy or
    ///   download that was interrupted or went wrong.
    /// - `truncated`: the file ends early. It is `bytes_have` long and the package needs at
    ///   least `bytes_need`; both are 0 when only the very end of the file is missing.
    /// - `structure`: `checks_failed` checks of the image's own tables fail though no data
    ///   block was found bad.
    /// - `metadata`: only `checks_failed` checks of what the package says about itself fail;
    ///   every block of the game's data is right. Seen on packages edited after they were made.
    pub kind: String,
    pub blocks_bad: u64,
    pub blocks_total: u64,
    pub block_bytes: u64,
    pub bytes_have: u64,
    pub bytes_need: u64,
    pub checks_failed: u64,
}

impl Damage {
    fn of(damage: ps5upload_fpkg::verify::Damage) -> Self {
        use ps5upload_fpkg::verify::Damage as D;
        let kind = |k: &str| Damage {
            kind: k.into(),
            ..Damage::default()
        };
        match damage {
            D::Truncated { have, need } => Damage {
                bytes_have: have,
                bytes_need: need,
                ..kind("truncated")
            },
            D::Data { bad, total } => Damage {
                blocks_bad: bad,
                blocks_total: total,
                block_bytes: ps5upload_fpkg::BLOCK,
                ..kind("data")
            },
            D::Structure { failed } => Damage {
                checks_failed: failed,
                ..kind("structure")
            },
            D::Metadata { failed } => Damage {
                checks_failed: failed,
                ..kind("metadata")
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckStatus {
    pub path: String,
    /// `queued` (waiting its turn), `running`, `ok`, `problems`, `failed` (could not be
    /// checked at all: `error` says why) or `cancelled`.
    pub state: String,
    /// `package`, `archive`, `files` or `bytes`: see the module docs.
    pub method: String,
    /// Of every pass together: a package's full check reads it twice (its blocks, then the
    /// PlayGo chunk CRCs), and both count here, so `bytes_done` reaches `bytes_total` only at
    /// the end (#951).
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// For `method` `package` while it runs: `blocks` (every block against its digest) or
    /// `playgo` (the PlayGo chunk CRCs, a second read of the package). Absent otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub phase: String,
    pub files_done: u64,
    pub files_total: u64,
    pub problems: Vec<Problem>,
    pub problems_total: u64,
    /// With `state` `problems`, for a package: what the problems add up to. Always there for
    /// `method` `package`; for `bytes` only when the file is shorter than its header says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damage: Option<Damage>,
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
        self.problem_of("", file, error);
    }
    /// [`Self::problem`] of a `kind` ([`Problem::kind`]).
    fn problem_of(&self, kind: &str, file: &str, error: impl Into<String>) {
        self.update(|s| {
            s.problems_total += 1;
            if s.problems.len() < MAX_PROBLEMS {
                s.problems.push(Problem {
                    file: file.to_string(),
                    error: error.into(),
                    kind: kind.to_string(),
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
        // A split package's later parts come from the index with it, never from the request.
        let copy = lib
            .games
            .values()
            .flat_map(|g| g.locations.iter())
            .find(|l| &l.absolute_path == p)
            .map(|l| (l.kind.clone(), l.parts.clone()));
        match copy {
            Some((kind, parts)) => wanted.push(Wanted {
                path: p.clone(),
                kind,
                parts,
            }),
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

/// A copy to check: where it is, its Collection type, and its later files when it is a split
/// package.
struct Wanted {
    path: String,
    kind: String,
    parts: Vec<String>,
}

/// Queues a check of each copy not already being checked.
fn start(wanted: Vec<Wanted>, handle: tokio::runtime::Handle) {
    let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
    for Wanted { path, kind, parts } in wanted {
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
            let outcome = run(&job, Path::new(&path), &kind, &parts);
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

/// Checks the copy at `path` (of Collection type `kind`; `parts` are a split package's later
/// files, in order). Problems found are recorded on the job; `Err` is a copy that could not be
/// checked at all.
fn run(job: &Job, path: &Path, kind: &str, parts: &[String]) -> Result<(), String> {
    let remote = ps5upload_fpkg::remote_source::is_remote(path);
    match kind {
        "pkg" if !parts.is_empty() => split_package(job, path, parts),
        "pkg" => {
            if !remote && package(job, path)? {
                return Ok(());
            }
            package_header(job, path, None);
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
    let mut phase = |p: ps5upload_fpkg::verify::Phase| job.update(|s| s.phase = p.as_str().into());
    let verified = ps5upload_fpkg::verify::verify_streaming_phased(
        path,
        ps5upload_fpkg::crypto::DEFAULT_PASSCODE,
        &job.cancel,
        &mut progress,
        &mut phase,
    );
    // A pass names a running check only.
    job.update(|s| s.phase.clear());
    match verified {
        // A retail package got as far as a report: its blocks can't be read with a debug
        // key, so "every block fails" would be about the key, not the file.
        Ok(report) if !report.is_debug() => {
            job.update(|s| {
                s.bytes_done = 0;
                s.bytes_total = 0;
            });
            Ok(false)
        }
        Ok(report) => {
            for c in report.checks.iter().filter(|c| !c.ok) {
                let what = if c.detail.is_empty() {
                    c.name.clone()
                } else {
                    format!("{} ({})", c.name, c.detail)
                };
                job.problem_of(c.kind.as_str(), "", format!("failed: {what}"));
            }
            let damage = report.damage().map(Damage::of);
            job.update(|s| s.damage = damage);
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
/// file is cut short. That is asked of the sizes the header declares, not of whether the header
/// reads: a PS4 package's header sits at the start and reads perfectly well from a file that
/// stops half way. `whole` is the length of a split package, all its parts together.
fn package_header(job: &Job, path: &Path, whole: Option<u64>) {
    if let Some((have, need)) = crate::collection::identify::cut_short(path, whole) {
        job.problem_of(
            "truncated",
            "",
            format!("the file ends early: {have} bytes, at least {need} needed"),
        );
        job.update(|s| {
            s.damage = Some(Damage {
                kind: "truncated".into(),
                bytes_have: have,
                bytes_need: need,
                ..Damage::default()
            })
        });
        return;
    }
    let Ok((mut f, size)) = open(path) else {
        return;
    };
    if let Err(e) = ps5upload_pkg::parse_pkg_from(&mut f, size, path) {
        job.problem("", format!("the package header can't be read: {e}"));
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

/// A file's bytes and its length: on this computer, or on a saved server.
fn open(path: &Path) -> Result<(Box<dyn ps5upload_fpkg::ReadSeek>, u64), String> {
    if ps5upload_fpkg::remote_source::is_remote(path) {
        return ps5upload_fpkg::remote_source::open_file(path).map_err(|e| format!("{e}"));
    }
    let f = std::fs::File::open(path).map_err(|e| format!("{e}"))?;
    let n = f.metadata().map_err(|e| format!("{e}"))?.len();
    Ok((Box::new(f), n))
}

/// Reads `f` (`size` bytes, named `file` in a problem) from its first byte to its last, adding
/// to the job's bytes done after the `before` bytes read ahead of it. `Ok(false)`: it could not
/// be read to its end, and the problem says where.
fn read_through(
    job: &Job,
    f: &mut dyn Read,
    size: u64,
    file: &str,
    before: u64,
) -> Result<bool, String> {
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
                job.problem(file, format!("can't be read at byte {done}: {e}"));
                return Ok(false);
            }
        };
        done += n as u64;
        job.update(|s| s.bytes_done = before + done);
    }
    if done < size {
        job.problem(file, format!("ends early: {done} of {size} bytes"));
        return Ok(false);
    }
    Ok(true)
}

/// Every byte of a single file, start to end.
fn bytes(job: &Job, path: &Path) -> Result<(), String> {
    job.update(|s| s.method = "bytes".into());
    let (mut f, size) = open(path)?;
    job.update(|s| s.bytes_total = size);
    read_through(job, &mut f, size, "", 0).map(|_| ())
}

/// A path's last name, as a problem shows it.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The parts a split set's own numbering says should be there and are not: `Game_0.pkg` and
/// `Game_2.pkg` miss `Game_1.pkg`; `Game.pkg` and `Game.pkg.1` miss `Game.pkg.0`. Parts missing
/// after the last one there leave no gap in the names: the package's declared length finds those.
fn missing_from_run(lead: &Path, parts: &[String]) -> Vec<String> {
    use crate::collection::split::{named_part, suffix_part};
    let lead = name_of(lead);
    let names: Vec<String> = parts.iter().map(|p| name_of(Path::new(p))).collect();
    let suffixed: Option<Vec<u32>> = names
        .iter()
        .map(|n| suffix_part(n).map(|(_, k)| k))
        .collect();
    // The numbers a later part may have (from `first`), the ones there, and how a name is made:
    // `{lead}.{k}`, or `{stem}_{k}.pkg`.
    let (first, have, stem) = match suffixed {
        Some(have) => (0, have, None),
        None => {
            let Some((stem, _)) = named_part(&lead) else {
                return Vec::new();
            };
            let Some(have) = names
                .iter()
                .map(|n| named_part(n).map(|(_, k)| k))
                .collect::<Option<Vec<u32>>>()
            else {
                return Vec::new();
            };
            (1, have, Some(stem.to_string()))
        }
    };
    let last = have.iter().copied().max().unwrap_or(0);
    // A set has dozens of parts at the very most: a name with a wild number is not walked to.
    if last.saturating_sub(first) > 4096 {
        return Vec::new();
    }
    (first..last)
        .filter(|k| !have.contains(k))
        .map(|k| match &stem {
            Some(stem) => format!("{stem}_{k}.pkg"),
            None => format!("{lead}.{k}"),
        })
        .collect()
}

/// A split package: its parts are one package, so every one is read, in order, and the package's
/// own header is held against all of them together. A part that is not there is a problem named
/// by that part; so is one that cannot be opened or read to its end.
fn split_package(job: &Job, lead: &Path, parts: &[String]) -> Result<(), String> {
    job.update(|s| s.method = "bytes".into());
    let all: Vec<&Path> = std::iter::once(lead)
        .chain(parts.iter().map(Path::new))
        .collect();
    // Each part's length first: the total is what the header is held against and what the
    // progress counts to. The first part is the package: without it nothing can be checked.
    let mut sizes: Vec<Option<u64>> = Vec::with_capacity(all.len());
    for (i, part) in all.iter().enumerate() {
        match open(part) {
            Ok((_, n)) => sizes.push(Some(n)),
            Err(e) if i == 0 => return Err(e),
            Err(e) => {
                let gone = !ps5upload_fpkg::remote_source::is_remote(part) && !part.exists();
                job.problem(
                    &name_of(part),
                    if gone {
                        "this part of the split package is missing".to_string()
                    } else {
                        format!("this part of the split package can't be opened: {e}")
                    },
                );
                sizes.push(None);
            }
        }
        if job.cancelled() {
            return Err("cancelled".into());
        }
    }
    for name in missing_from_run(lead, parts) {
        job.problem(&name, "this part of the split package is missing");
    }
    let total: u64 = sizes.iter().flatten().sum();
    job.update(|s| {
        s.files_total = all.len() as u64;
        s.bytes_total = total;
    });
    package_header(job, lead, Some(total));
    let mut before = 0u64;
    for (part, size) in all.iter().zip(&sizes) {
        let Some(size) = *size else {
            continue;
        };
        match open(part) {
            Ok((mut f, _)) => {
                read_through(job, &mut f, size, &name_of(part), before)?;
            }
            Err(e) => job.problem(&name_of(part), format!("can't be opened: {e}")),
        }
        // A part given up on still counts as gone through.
        before += size;
        job.update(|s| {
            s.files_done += 1;
            s.bytes_done = before;
        });
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
            let d = crate::test_tmp::temp_path(format!("ps5u-check-{tag}-{}", std::process::id()));
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
        run(&j, t.path(), "folder", &[]).unwrap();
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
        run(&j, t.path(), "folder", &[]).unwrap();
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
        run(&j, &good, "zip", &[]).unwrap();
        assert_eq!(j.status.lock().unwrap().problems_total, 0);

        // Flip a byte of the stored data: the CRC no longer matches.
        let mut bytes = std::fs::read(&good).unwrap();
        let at = bytes.windows(4).position(|w| w == [1, 1, 1, 1]).unwrap() + 100;
        bytes[at] ^= 0xFF;
        let bad = t.path().join("bad.zip");
        std::fs::write(&bad, bytes).unwrap();
        let j = job();
        run(&j, &bad, "zip", &[]).unwrap();
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

    /// A small debug package, as Convert builds one.
    fn debug_package(dir: &Path) -> std::path::PathBuf {
        let source = dir.join("game");
        std::fs::create_dir_all(source.join("sce_sys")).unwrap();
        std::fs::create_dir_all(source.join("data")).unwrap();
        std::fs::write(source.join("eboot.bin"), [7u8; 4096]).unwrap();
        std::fs::write(source.join("data/big.bin"), vec![0x5Au8; 3 << 20]).unwrap();
        std::fs::write(
            source.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.000.000","titleId":"PPSA01234"}"#,
        )
        .unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut request = ps5upload_fpkg::build::BuildRequest::new(&source, &out);
        request.time = Some((1_700_000_000, 0));
        // Stored, so a byte of the game's data sits in the file as it is.
        request.level = ps5upload_fpkg::kraken::Level::Stored;
        ps5upload_fpkg::build::build(&request, &mut |_| {})
            .unwrap()
            .path
    }

    /// The check of the package at `path`, finished.
    fn checked(path: &Path) -> CheckStatus {
        let j = job();
        run(&j, path, "pkg", &[]).unwrap();
        let s = j.status.lock().unwrap().clone();
        s
    }

    /// #951: the PlayGo CRC pass counts in the check's total (a 200 GB package showed 100%
    /// through a whole second read), and a finished check names no pass.
    #[test]
    fn a_package_check_counts_both_passes_and_ends_with_no_phase() {
        let t = Scratch::new("phases");
        let pkg = debug_package(t.path());
        let mut f = ps5upload_fpkg::PkgFile::open(&pkg).unwrap();
        let zip_start = ps5upload_fpkg::si::read(&mut f)
            .unwrap()
            .expect("an SI ZIP")
            .zip_start;
        drop(f);
        let s = checked(&pkg);
        assert_eq!(s.method, "package");
        assert_eq!(s.problems_total, 0, "{:?}", s.problems);
        assert_eq!(s.bytes_done, s.bytes_total);
        // Both reads: the image's blocks and everything before the SI ZIP again.
        assert!(
            s.bytes_total > zip_start,
            "{} vs {zip_start}",
            s.bytes_total
        );
        assert!(s.phase.is_empty());
        assert!(!serde_json::to_string(&s).unwrap().contains("\"phase\""));
    }

    /// The three things a package check can tell apart, each with what the screen needs to
    /// say it in plain words: damaged data (how many blocks of how many), a file cut short
    /// (how long it is and has to be), and only the package's own information not matching.
    #[test]
    fn a_package_check_says_what_kind_of_damage_it_found() {
        use std::io::{Seek, SeekFrom, Write};
        let t = Scratch::new("damage");
        let good = debug_package(t.path());
        let whole = std::fs::read(&good).unwrap();
        let ok = checked(&good);
        assert_eq!(ok.method, "package");
        assert_eq!(ok.problems_total, 0, "{:?}", ok.problems);
        assert_eq!(ok.damage, None);

        let mut file = ps5upload_fpkg::PkgFile::open(&good).unwrap();
        let head = file.read_at(0, ps5upload_fpkg::fih::HEADER_LEN).unwrap();
        let fih = ps5upload_fpkg::fih::parse(&head).unwrap();
        drop(file);
        let block = ps5upload_fpkg::BLOCK;
        let blocks = fih.pfs_size / block;

        // Two blocks of the image overwritten, as a faulty copy leaves them.
        let bad = t.path().join("data.pkg");
        std::fs::write(&bad, &whole).unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&bad).unwrap();
        for index in [3u64, 9] {
            f.seek(SeekFrom::Start(fih.pfs_offset + index * block + 100))
                .unwrap();
            f.write_all(&[0xEE; 64]).unwrap();
        }
        drop(f);
        let s = checked(&bad);
        assert_eq!(s.method, "package");
        assert!(s.problems_total >= 1, "{:?}", s.problems);
        assert_eq!(
            s.damage,
            Some(Damage {
                kind: "data".into(),
                blocks_bad: 2,
                blocks_total: blocks,
                block_bytes: 65536,
                ..Damage::default()
            })
        );
        let data: Vec<&Problem> = s.problems.iter().filter(|p| p.kind == "data").collect();
        assert_eq!(data.len(), 1, "{:?}", s.problems);
        assert!(
            data[0].error.contains(&format!("2 of {blocks} failed")),
            "{}",
            data[0].error
        );
        // The technical lines stay as they were, for a bug report.
        assert!(s.problems.iter().all(|p| p.error.starts_with("failed: ")));

        // Cut off inside the image: the file ends before the package does.
        let short = t.path().join("short.pkg");
        let cut = (fih.pfs_offset + fih.pfs_size / 2) as usize;
        std::fs::write(&short, &whole[..cut]).unwrap();
        let s = checked(&short);
        assert_eq!(s.method, "package");
        assert_eq!(s.problems_total, 1, "{:?}", s.problems);
        assert_eq!(s.problems[0].kind, "truncated");
        let damage = s.damage.unwrap();
        assert_eq!(damage.kind, "truncated");
        assert_eq!(damage.bytes_have, cut as u64);
        assert!(damage.bytes_need > cut as u64 && damage.bytes_need <= whole.len() as u64);

        // One byte of the container's own digests changed: nothing of the game's data is wrong.
        let meta = t.path().join("meta.pkg");
        std::fs::write(&meta, &whole).unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&meta).unwrap();
        f.seek(SeekFrom::Start(fih.cnt_offset + 0x100)).unwrap();
        let was = whole[fih.cnt_offset as usize + 0x100];
        f.write_all(&[was ^ 0xFF]).unwrap();
        drop(f);
        let s = checked(&meta);
        assert_eq!(s.method, "package");
        let damage = s.damage.clone().unwrap();
        assert_eq!(damage.kind, "metadata", "{:?}", s.problems);
        assert_eq!(damage.checks_failed, s.problems_total);
        assert_eq!(damage.blocks_bad, 0);
        assert!(s.problems.iter().all(|p| p.kind == "metadata"));

        // On the wire: the old fields as they were, the new ones beside them.
        let v = serde_json::to_value(checked(&bad)).unwrap();
        assert_eq!(v["damage"]["kind"], "data");
        assert_eq!(v["damage"]["blocks_bad"], 2);
        assert_eq!(v["problems"][0]["file"], "");
        assert!(v["problems"][0]["error"].is_string());
        let clean = serde_json::to_value(ok).unwrap();
        assert!(clean.get("damage").is_none());
    }

    #[test]
    fn a_package_that_is_not_a_debug_package_is_read_byte_by_byte() {
        let t = Scratch::new("retail");
        let f = t.path().join("retail.pkg");
        std::fs::write(&f, vec![0u8; 70_000]).unwrap();
        let j = job();
        run(&j, &f, "pkg", &[]).unwrap();
        let s = j.status.lock().unwrap();
        assert_eq!(s.method, "bytes");
        assert_eq!(s.bytes_done, 70_000);
    }

    /// A PS4 package (`\\x7FCNT`) whose header declares `total` bytes, in a file of `have`.
    fn ps4_package(path: &Path, total: u64, have: usize) {
        let mut pkg = vec![0u8; have];
        pkg[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        pkg[0x74..0x78].copy_from_slice(&0x1Au32.to_be_bytes());
        pkg[0x430..0x438].copy_from_slice(&total.to_be_bytes());
        std::fs::write(path, pkg).unwrap();
    }

    /// The audit's case: a PS4 package cut short reads without an error from its first byte to
    /// its last, so only its own header says it is not all there. The check says it is cut
    /// short, in the words the screen has for that, and a whole one is fine.
    #[test]
    fn a_package_shorter_than_its_header_says_is_reported_cut_short() {
        let t = Scratch::new("ps4-short");
        let short = t.path().join("short.pkg");
        ps4_package(&short, 0x9000, 0x3000);
        let s = checked(&short);
        assert_eq!(s.method, "bytes");
        assert_eq!(s.bytes_done, 0x3000, "it is still read to its end");
        assert_eq!(s.problems_total, 1, "{:?}", s.problems);
        assert_eq!(s.problems[0].kind, "truncated");
        assert_eq!(
            s.damage,
            Some(Damage {
                kind: "truncated".into(),
                bytes_have: 0x3000,
                bytes_need: 0x9000,
                ..Damage::default()
            })
        );

        let whole = t.path().join("whole.pkg");
        ps4_package(&whole, 0x9000, 0x9000);
        let s = checked(&whole);
        assert_eq!(s.problems_total, 0, "{:?}", s.problems);
        assert_eq!(s.damage, None);
    }

    /// The check of a split package: its first file and its later parts.
    fn checked_set(lead: &Path, parts: &[&Path]) -> CheckStatus {
        let parts: Vec<String> = parts.iter().map(|p| p.display().to_string()).collect();
        let j = job();
        run(&j, lead, "pkg", &parts).unwrap();
        let s = j.status.lock().unwrap().clone();
        s
    }

    /// The audit's case: only the first file of a split package was read. Every part is read,
    /// in order, as the one package they make; a part that is gone is named.
    #[test]
    fn every_part_of_a_split_package_is_checked_as_one_package() {
        let t = Scratch::new("split");
        let (p0, p1, p2) = (
            t.path().join("Game_0.pkg"),
            t.path().join("Game_1.pkg"),
            t.path().join("Game_2.pkg"),
        );
        // 0x9000 bytes declared, kept as three files of 0x3000.
        ps4_package(&p0, 0x9000, 0x3000);
        std::fs::write(&p1, vec![1u8; 0x3000]).unwrap();
        std::fs::write(&p2, vec![2u8; 0x3000]).unwrap();
        let s = checked_set(&p0, &[&p1, &p2]);
        assert_eq!(s.method, "bytes");
        assert_eq!(s.problems_total, 0, "{:?}", s.problems);
        assert_eq!((s.bytes_done, s.bytes_total), (0x9000, 0x9000));
        assert_eq!((s.files_done, s.files_total), (3, 3));
        assert_eq!(s.damage, None);

        // The last part cut short: the first file alone would have read as fine.
        std::fs::write(&p2, vec![2u8; 0x1000]).unwrap();
        let s = checked_set(&p0, &[&p1, &p2]);
        assert_eq!(s.bytes_done, 0x7000);
        let damage = s.damage.expect("the set is shorter than the package says");
        assert_eq!(
            (damage.kind.as_str(), damage.bytes_have, damage.bytes_need),
            ("truncated", 0x7000, 0x9000)
        );

        // A part the index knows that is not there any more.
        std::fs::remove_file(&p1).unwrap();
        let s = checked_set(&p0, &[&p1, &p2]);
        let gone: Vec<&Problem> = s
            .problems
            .iter()
            .filter(|p| p.file == "Game_1.pkg")
            .collect();
        assert_eq!(gone.len(), 1, "{:?}", s.problems);
        assert_eq!(gone[0].error, "this part of the split package is missing");
        assert_eq!((s.files_done, s.files_total), (2, 3));
        assert_eq!(
            s.bytes_done, 0x4000,
            "the parts that are there are still read"
        );

        // A part missing from the run of names (the index holds parts 0 and 2 only).
        let s = checked_set(&p0, &[&p2]);
        assert!(
            s.problems
                .iter()
                .any(|p| p.file == "Game_1.pkg" && p.error.contains("is missing")),
            "{:?}",
            s.problems
        );
    }

    #[test]
    fn the_numbering_of_a_set_says_which_parts_are_missing() {
        let named = |parts: &[&str]| {
            let parts: Vec<String> = parts.iter().map(|p| format!("/lib/{p}")).collect();
            missing_from_run(Path::new("/lib/Game_0.pkg"), &parts)
        };
        assert!(named(&["Game_1.pkg", "Game_2.pkg"]).is_empty());
        assert_eq!(
            named(&["Game_2.pkg", "Game_4.pkg"]),
            ["Game_1.pkg", "Game_3.pkg"]
        );
        let suffixed = |parts: &[&str]| {
            let parts: Vec<String> = parts.iter().map(|p| format!("/lib/{p}")).collect();
            missing_from_run(Path::new("/lib/Game.pkg"), &parts)
        };
        assert!(suffixed(&["Game.pkg.0", "Game.pkg.1"]).is_empty());
        assert_eq!(suffixed(&["Game.pkg.1", "Game.pkg.2"]), ["Game.pkg.0"]);
        // A name with a wild number is not walked to.
        assert!(named(&["Game_4000000000.pkg"]).is_empty());
    }
}
