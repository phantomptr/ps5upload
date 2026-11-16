//! One job, asked for and reported through files in a folder (on the PS5
//! `/data/ps5upload/convert`). The helper's extractor works the same way
//! (`payload/extractor/README.md`); no port is opened, so nothing but the helper can ask. The
//! helper's side is `payload/src/console_convert.c`.
//!
//! - `request.json`: written by the helper, read and deleted here at start.
//!   `{"id": "<the helper's job id>", "op": "inspect"|"build", "path": "<source>"}`, and for a
//!   build `"dest"` (the output's full path), `"format"` (`exfat`, `ffpkg`, `ffpfs`, `folder`),
//!   `"zstd"` (bool), `"verify"` (`fast`, the default, or `full`), and optionally
//!   `"zstd_runtime"` (the path of ampr-emu-ng's `libSceAmpr.sprx` on the console, which zstd
//!   packs need) and `"zstd_level"` (1 to 22, 5 when absent).
//! - `status.json`: rewritten here through a temporary file and a rename in the same folder, so
//!   the helper never reads half of one; at most every [`STATUS_EVERY`] while a job runs.
//!   `{"id", "pid", "op", "state": "running"|"done"|"failed", "stage"?, "bytes_done"?,
//!   "bytes_total"?, "cause"?, "detail"?, "result"?}`
//!   `stage` is the build's (`plan`, `pack`, `write`, `verify`); `cause` is a token the apps
//!   translate; `detail` is for the log; `result` is the op's answer.
//! - `cancel`: created by the helper to stop a job. A thread looks for it every
//!   [`CANCEL_EVERY`] and raises the job's cancel flag, which the build code checks at every
//!   checkpoint of its loops. One there when the build starts (its helper gave up waiting for
//!   this converter) stops it before it clears or writes anything.
//!
//! A build writes `<dest>.partial` (a file, or a folder for a folder output) beside `dest`, and,
//! for an image, may leave `<dest>.partial.resume` and `<dest>.partial.stamp` beside it: the
//! three names the helper removes after a crash. Only a build that read back gets renamed to
//! `dest`, in its own folder. No resume yet: a stopped build starts over.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ps5upload_convert_core::folder_build::{self, FolderOut};
use ps5upload_convert_core::image_build::{self, ImageFormat};
use ps5upload_convert_core::{safe_write, zstd_packs};
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::SourceTree;
use serde_json::json;

use crate::Refusal;

/// The most a request may be: a path and a few options. A larger file is not one of ours.
const MAX_REQUEST_BYTES: u64 = 64 << 10;
/// How often a running job rewrites its status. The helper polls every 200 ms.
const STATUS_EVERY: Duration = Duration::from_millis(250);
/// How often a running job looks for the cancel file.
const CANCEL_EVERY: Duration = Duration::from_millis(150);
/// The most of a failure's words kept in the status: the helper reads `detail` into 400 bytes
/// and drops a longer one.
const MAX_DETAIL_BYTES: usize = 300;
/// The largest emulator file read for zstd packs (ampr-emu-ng is well under 1 MiB).
const MAX_RUNTIME_BYTES: u64 = 16 << 20;
/// zstd's level when the request names none: the engine's default.
const DEFAULT_ZSTD_LEVEL: u8 = 5;
/// Compression threads: on the PS5 three of the payload's five CPUs, the other two left to the
/// console's own UI and a suspended game; elsewhere every core (0).
pub const THREADS: usize = if cfg!(target_env = "ps5") { 3 } else { 0 };

#[derive(Debug, serde::Deserialize)]
struct Request {
    id: String,
    op: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    dest: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    zstd: bool,
    #[serde(default)]
    verify: String,
    #[serde(default)]
    zstd_runtime: String,
    #[serde(default)]
    zstd_level: Option<u8>,
}

/// Writes `status` as `dir/status.json`. The temporary file is in `dir` too: a rename never
/// leaves the folder (a rename across filesystems panics the PS5's kernel). `durable`: the
/// file's bytes are synced before the rename, so a crash after it never leaves an empty or
/// half-written status under the real name. Asked for the first and final statuses and each
/// new stage; the progress rewrites in between (every [`STATUS_EVERY`], from the build's own
/// thread) skip it, since a sync can take seconds on a drive busy with the build's writes.
fn write_status(dir: &Path, status: &serde_json::Value, durable: bool) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!("status.json.{}.tmp", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(format!("{status}\n").as_bytes())?;
    if durable {
        // Best effort: a drive without fsync still gets the status.
        let _ = f.sync_all();
    }
    drop(f);
    safe_write::rename(&tmp, &dir.join("status.json"))
}

fn read_request(dir: &Path) -> Result<Request, String> {
    let path = dir.join("request.json");
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("request.json: {e}"))?
        .len();
    if size > MAX_REQUEST_BYTES {
        return Err(format!("request.json is {size} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("request.json: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("request.json: {e}"))
}

/// `detail` cut to [`MAX_DETAIL_BYTES`] at a character boundary.
fn short(detail: &str) -> &str {
    let mut end = detail.len().min(MAX_DETAIL_BYTES);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    &detail[..end]
}

/// A running job's reports: rewrites the status at most every [`STATUS_EVERY`], and at once
/// when the stage changes.
struct Running<'a> {
    dir: &'a Path,
    base: serde_json::Value,
    last_status: Option<Instant>,
    stage: String,
}

impl Running<'_> {
    /// A status that cannot be written is not worth ending the job for: the next one may go
    /// through, and the final one is checked.
    fn progress(&mut self, stage: &str, done: u64, total: u64) {
        let new_stage = stage != self.stage;
        if !new_stage && self.last_status.is_some_and(|t| t.elapsed() < STATUS_EVERY) {
            return;
        }
        if new_stage {
            crate::log(format_args!("stage {stage}"));
            self.stage = stage.to_string();
        }
        self.last_status = Some(Instant::now());
        let mut s = self.base.clone();
        s["state"] = json!("running");
        s["stage"] = json!(stage);
        s["bytes_done"] = json!(done);
        s["bytes_total"] = json!(total);
        let _ = write_status(self.dir, &s, new_stage);
    }
}

/// Raises `flag` once `dir/cancel` appears, looking every [`CANCEL_EVERY`] until `done`.
fn watch_cancel(
    dir: &Path,
    flag: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let file = dir.join("cancel");
    std::thread::Builder::new()
        .name("ps5u-convert".into())
        .spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if file.exists() {
                    flag.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(CANCEL_EVERY);
            }
        })
        .expect("a thread to watch for the cancel file")
}

/// What a build is asked for, checked.
struct Plan {
    source: PathBuf,
    dest: PathBuf,
    /// `None`: a folder.
    image: Option<ImageFormat>,
    mode: VerifyMode,
    /// The emulator's bytes and the level, for zstd packs.
    zstd: Option<(Vec<u8>, u8)>,
}

fn bad(detail: String) -> Refusal {
    Refusal {
        cause: "convert_bad_request",
        detail,
    }
}

/// The request checked against the console: the source readable, the destination new and in a
/// folder that exists, the emulator there when zstd packs are asked for.
fn plan(request: &Request) -> Result<Plan, Refusal> {
    if request.dest.is_empty() {
        return Err(bad("a build needs a dest".into()));
    }
    let image = match request.format.as_str() {
        "folder" => None,
        "exfat" => Some(ImageFormat::Exfat),
        "ffpkg" => Some(ImageFormat::Ffpkg),
        "ffpfs" => Some(ImageFormat::Ffpfs),
        other => return Err(bad(format!("unknown format {other:?}"))),
    };
    let mode = match request.verify.as_str() {
        "" | "fast" => VerifyMode::from_full(false, image == Some(ImageFormat::Ffpkg)),
        "full" => VerifyMode::Full,
        other => return Err(bad(format!("unknown verify {other:?}"))),
    };
    let dest = PathBuf::from(&request.dest);
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let bad_dest = |detail: String| Refusal {
        cause: "convert_bad_dest",
        detail,
    };
    if !dest.is_absolute() || name.is_empty() || name.to_ascii_lowercase().ends_with(".partial") {
        return Err(bad_dest(format!("{} is not an output path", request.dest)));
    }
    if let Some(f) = image {
        let ext = dest
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if !ext.eq_ignore_ascii_case(f.extension()) {
            return Err(bad_dest(format!(
                "{} does not end in .{}",
                request.dest,
                f.extension()
            )));
        }
    }
    if dest.symlink_metadata().is_ok() {
        return Err(Refusal {
            cause: "convert_destination_exists",
            detail: format!("{} exists", request.dest),
        });
    }
    let source = PathBuf::from(&request.path);
    // What this program reads, and that it opens: the checks an inspection makes.
    crate::inspect(&source)?;
    if source == dest || dest.starts_with(&source) {
        return Err(bad_dest(format!(
            "{} is inside the source {}",
            request.dest, request.path
        )));
    }
    if reads_own_leftovers(&source, &dest) {
        // The build clears `<dest>.partial` (and its two side files) before it writes: a source
        // that is one of them, or inside one, would be deleted.
        return Err(bad_dest(format!(
            "the source {} is where a build of {} writes",
            request.path, request.dest
        )));
    }
    let zstd = if request.zstd {
        let level = request.zstd_level.unwrap_or(DEFAULT_ZSTD_LEVEL);
        if !(1..=22).contains(&level) {
            return Err(bad(format!("zstd level {level} is not 1 to 22")));
        }
        Some((read_runtime(&request.zstd_runtime)?, level))
    } else {
        None
    };
    Ok(Plan {
        source,
        dest,
        image,
        mode,
        zstd,
    })
}

/// Whether a build of `dest` would clear or write over `source`: `source` is one of the names a
/// build of `dest` writes and clears (`<dest>.partial`, `.partial.resume`, `.partial.stamp`) or
/// is inside one, or one of those names is inside `source`. Compared without case: on an exFAT
/// drive `g.partial` and `G.partial` are one folder, and refusing one build too many is safe.
fn reads_own_leftovers(source: &Path, dest: &Path) -> bool {
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_ascii_lowercase());
    let source = lower(source);
    let partial = folder_build::partial_of(dest);
    let resume = image_build::checkpoint_path(&partial);
    let mut stamp = partial.clone().into_os_string();
    stamp.push(".stamp");
    [partial, resume, PathBuf::from(stamp)]
        .iter()
        .map(|p| lower(p))
        .any(|own| source.starts_with(&own) || own.starts_with(&source))
}

/// The emulator zstd packs carry, from the file the app put on the console. Never fetched here:
/// the converter does not go online.
fn read_runtime(path: &str) -> Result<Vec<u8>, Refusal> {
    let missing = |detail: String| Refusal {
        cause: "convert_no_zstd_runtime",
        detail,
    };
    if path.is_empty() {
        return Err(missing(
            "zstd packs need ampr-emu-ng on the console; the request names none".into(),
        ));
    }
    let size = std::fs::metadata(path)
        .map_err(|e| missing(format!("{path}: {e}")))?
        .len();
    if size == 0 || size > MAX_RUNTIME_BYTES {
        return Err(missing(format!("{path} is {size} bytes: not an emulator")));
    }
    std::fs::read(path).map_err(|e| missing(format!("{path}: {e}")))
}

/// The source opened as the engine opens it: module names in the eboot's spelling, an unsigned
/// eboot fake-signed; for packing, any packs it has decoded.
fn open_source(path: &Path, packing: bool) -> Result<Box<dyn SourceTree>, String> {
    let (tree, renamed, signing) =
        ps5upload_fpkg::module_case::open_with_update(path, None).map_err(|e| e.to_string())?;
    if !renamed.is_empty() {
        crate::log(format_args!(
            "renamed to the eboot's spelling: {}",
            ps5upload_fpkg::module_case::describe(&renamed)
        ));
    }
    if let Some(line) = signing.describe() {
        crate::log(format_args!("{line}"));
    }
    if packing {
        zstd_packs::readable_for_packing(tree)
    } else {
        Ok(tree)
    }
}

/// Removes what a stopped or failed build of `dest` left: the names the helper also cleans up.
/// A `.partial` folder only when it carries the build's marker.
fn remove_leftovers(dest: &Path) {
    let partial = folder_build::partial_of(dest);
    if partial.is_dir() {
        if folder_build::is_our_partial_dir(&partial) {
            let _ = std::fs::remove_dir_all(&partial);
        }
    } else if partial.is_file() {
        let _ = safe_write::remove_file(&partial);
    }
    image_build::remove_checkpoint(&partial);
}

/// Why a build failed, as a cause token: stopped, a drive gone, no room, else a plain failure.
fn failure(detail: String, cancelled: bool, plan: &Plan) -> Refusal {
    let lower = detail.to_ascii_lowercase();
    let cause = if cancelled || detail == "cancelled" {
        "convert_cancelled"
    } else if !plan.source.exists() || plan.dest.parent().is_some_and(|d| !d.is_dir()) {
        // U7: a pulled drive is named before the raw error.
        "convert_drive_gone"
    } else if lower.contains("holds files of at most") {
        // The build's own check once the image or pack volume sizes are known
        // (`safe_write::too_big_for`): with zstd packs the size isn't known before.
        "convert_too_big_for_drive"
    } else if lower.contains("no space")
        || lower.contains("storage full")
        || lower.contains("disk full")
    {
        "convert_no_space"
    } else {
        "convert_build_failed"
    };
    Refusal { cause, detail }
}

/// Builds the request's output (see the module docs). `stage(id, done, total)` hears the build's
/// progress; `cancel` stops it.
fn build(
    request: &Request,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<serde_json::Value, Refusal> {
    let plan = plan(request)?;
    let unreadable = |e: String| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", plan.source.display()),
    };
    let mut tree = open_source(&plan.source, false).map_err(unreadable)?;
    if let Some(why) =
        ps5upload_convert_core::refusal::ps4_refusal(&plan.source, &mut *tree, plan.image.is_some())
    {
        return Err(Refusal {
            cause: "convert_unsupported_source",
            detail: why,
        });
    }
    if plan.zstd.is_some() {
        // A game zstd packs can't be made of (a PS4 game, no eboot.bin), or packs that don't
        // decode.
        tree = zstd_packs::readable_for_packing(tree).map_err(|detail| Refusal {
            cause: "convert_not_packable",
            detail,
        })?;
    }
    if tree.files().is_empty() {
        return Err(unreadable("no files in it".into()));
    }
    let data = crate::data_bytes(tree.files()).map_err(unreadable)?;
    // U1: the destination measured once, before the first byte.
    let dir = plan.dest.parent().unwrap_or(Path::new("/"));
    let probe = safe_write::probe(dir).map_err(|e| Refusal {
        cause: "convert_bad_dest",
        detail: format!("{}: {e}", dir.display()),
    })?;
    crate::log(format_args!(
        "destination {}: {} free={:?} read_only={}",
        dir.display(),
        probe.fs_type,
        probe.free,
        probe.read_only
    ));
    // An image is at least as large as the game in it; packs may well be smaller, so only the
    // drive's own limits are checked for them here. Their size is known once they are measured:
    // the build checks the image and each pack volume against the drive's file size limit then,
    // before writing (`safe_write::too_big_for`, `convert_too_big_for_drive` in `failure`).
    let need = if plan.zstd.is_some() { 0 } else { data };
    if let Some(why) = probe.refusal(need, plan.image.is_some(), folder_build::SPARE) {
        let cause = if probe.read_only {
            "convert_dest_read_only"
        } else if why.contains("at most") {
            "convert_too_big_for_drive"
        } else {
            "convert_no_space"
        };
        return Err(Refusal {
            cause,
            detail: format!("{}: {why}", dir.display()),
        });
    }
    // Stopped before the first write (a converter that started late, after its helper gave up on
    // it and left the cancel file): nothing is cleared or written.
    if cancel.load(Ordering::Relaxed) {
        return Err(failure("cancelled".into(), true, &plan));
    }
    // Ours alone from here: a leftover of an earlier run under this name is cleared.
    let partial = folder_build::partial_of(&plan.dest);
    let Some(_writing) = ps5upload_convert_core::writing::Writing::claim(partial.clone()) else {
        return Err(bad("this output is already being built".into()));
    };
    // A folder under that name that no build of ours left is someone's: kept, and refused.
    if let Some(detail) = ps5upload_convert_core::writing::partial_in_the_way(&partial) {
        return Err(Refusal {
            cause: "convert_partial_in_the_way",
            detail,
        });
    }
    remove_leftovers(&plan.dest);
    crate::log(format_args!(
        "building {} files, {data} bytes -> {} ({:?}, zstd {:?})",
        tree.files().len(),
        plan.dest.display(),
        plan.mode,
        plan.zstd.as_ref().map(|z| z.1)
    ));
    let started = Instant::now();
    let chunk = image_build::DEFAULT_VERIFY_CHUNK;
    let source = plan.source.clone();
    let packing = plan.zstd.is_some();
    let reopen = move || open_source(&source, packing);
    let built = match (plan.image, &plan.zstd) {
        (Some(format), zstd) => {
            // A PS4 game's image needs the param.json ShadowMount+ reads.
            let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *tree)
                .map_err(|e| unreadable(format!("param.sfo: {e}")))?;
            let tree: &mut dyn SourceTree = match ps4.as_mut() {
                Some(p) => p,
                None => &mut *tree,
            };
            match zstd {
                Some((runtime, level)) => zstd_packs::build(
                    format, *level, None, runtime, tree, &plan.dest, plan.mode, chunk, cancel,
                    stage, THREADS,
                ),
                None => {
                    // A libSceAmpr game gets a fresh ampr_emu.index, as on a computer (#449).
                    let now = zstd_packs::now_secs();
                    match image_build::AmprIndexed::wrap_at(&mut *tree, now) {
                        Ok(Some(mut indexed)) => image_build::build(
                            format,
                            None,
                            &mut indexed,
                            &plan.dest,
                            plan.mode,
                            chunk,
                            cancel,
                            stage,
                        ),
                        Ok(None) => image_build::build(
                            format, None, tree, &plan.dest, plan.mode, chunk, cancel, stage,
                        ),
                        Err(e) => Err(format!("ampr_emu.index: {e}")),
                    }
                }
            }
            .map(|b| (b.files, b.image_bytes, b.detail))
        }
        (None, Some((runtime, level))) => zstd_packs::build_folder(
            *level, None, runtime, &mut *tree, &reopen, &plan.dest, plan.mode, cancel, stage,
            THREADS,
        )
        .map(|b| (b.files, b.bytes, b.detail)),
        (None, None) => folder_build::build_to(
            &mut *tree,
            &reopen,
            &FolderOut::Local(&plan.dest),
            plan.mode,
            cancel,
            stage,
        )
        .map(|b| (b.files, b.bytes, b.detail)),
    };
    match built {
        Ok((files, bytes, detail)) => {
            crate::log(format_args!(
                "built {} in {:.1} s: {detail}",
                plan.dest.display(),
                started.elapsed().as_secs_f64()
            ));
            Ok(json!({
                "dest": plan.dest,
                "format": request.format,
                "files": files,
                "bytes": bytes,
                "detail": detail,
            }))
        }
        Err(detail) => {
            remove_leftovers(&plan.dest);
            Err(failure(detail, cancel.load(Ordering::Relaxed), &plan))
        }
    }
}

/// Runs a build with its cancel watcher and status reports. `hook` (tests) hears each stage
/// report after the status does.
fn run_build(
    request: &Request,
    dir: &Path,
    base: &serde_json::Value,
    hook: &mut dyn FnMut(&str),
) -> Result<serde_json::Value, Refusal> {
    // A cancel file already there (left by a helper that stopped waiting for this converter) is
    // seen before anything else: the watcher's first look could come after the build's first write.
    let cancel = Arc::new(AtomicBool::new(dir.join("cancel").exists()));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = watch_cancel(dir, Arc::clone(&cancel), Arc::clone(&done));
    let mut running = Running {
        dir,
        base: base.clone(),
        last_status: None,
        stage: String::new(),
    };
    let result = build(request, &cancel, &mut |stage, d, t| {
        running.progress(stage, d, t);
        hook(stage);
    });
    done.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    result
}

/// Runs the request in `dir` and leaves its outcome in `dir/status.json`. Returns the process's
/// exit code: 0 done, 1 failed. A failure to write the status itself is the only error.
pub fn run(dir: &Path) -> std::io::Result<i32> {
    run_with(dir, &mut |_| {})
}

fn run_with(dir: &Path, hook: &mut dyn FnMut(&str)) -> std::io::Result<i32> {
    let pid = std::process::id();
    let request = read_request(dir);
    // Read once: a request left behind would run again at the next start.
    let _ = std::fs::remove_file(dir.join("request.json"));
    let request = match request {
        Ok(r) => r,
        Err(detail) => {
            crate::log(format_args!("bad request: {detail}"));
            let status = json!({"id": "", "pid": pid, "op": "", "state": "failed",
                "cause": "convert_bad_request", "detail": short(&detail)});
            write_status(dir, &status, true)?;
            return Ok(1);
        }
    };
    let base = json!({"id": request.id, "pid": pid, "op": request.op});
    let status = |state: &str| {
        let mut s = base.clone();
        s["state"] = json!(state);
        s
    };
    let fail = |refusal: Refusal| {
        crate::log(format_args!("{} failed: {refusal}", request.op));
        let mut s = status("failed");
        s["cause"] = json!(refusal.cause);
        s["detail"] = json!(short(&refusal.detail));
        write_status(dir, &s, true).map(|()| 1)
    };
    let done = |result: serde_json::Value| {
        let mut s = status("done");
        s["result"] = result;
        write_status(dir, &s, true).map(|()| 0)
    };
    // The helper waits for the first status to know the program started and learn its pid.
    write_status(dir, &status("running"), true)?;
    match request.op.as_str() {
        "inspect" => {
            crate::log(format_args!("inspect {}", request.path));
            match crate::inspect(Path::new(&request.path)) {
                Ok(inspection) => done(json!(inspection)),
                Err(refusal) => fail(refusal),
            }
        }
        "build" => {
            crate::log(format_args!(
                "build {} -> {} ({}, zstd {}, verify {:?})",
                request.path, request.dest, request.format, request.zstd, request.verify
            ));
            match run_build(&request, dir, &base, hook) {
                Ok(result) => done(result),
                Err(refusal) => fail(refusal),
            }
        }
        other => fail(Refusal {
            cause: "convert_bad_request",
            detail: format!("unknown op {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{game_folder, TempDir};

    fn status(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join("status.json")).unwrap()).unwrap()
    }

    /// The names in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    /// A job folder and a game beside it.
    fn setup(tag: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new(tag);
        let game = game_folder(&tmp.0);
        let dir = tmp.0.join("convert");
        std::fs::create_dir_all(&dir).unwrap();
        (tmp, game, dir)
    }

    fn ask(dir: &Path, request: serde_json::Value) {
        std::fs::write(dir.join("request.json"), request.to_string()).unwrap();
    }

    #[test]
    fn an_inspect_request_ends_in_a_done_status_with_the_result() {
        let (_tmp, game, dir) = setup("job");
        ask(&dir, json!({"id": "j1", "op": "inspect", "path": game}));

        assert_eq!(run(&dir).unwrap(), 0);
        let s = status(&dir);
        assert_eq!(
            (s["id"].as_str(), s["state"].as_str()),
            (Some("j1"), Some("done"))
        );
        assert_eq!(s["pid"], std::process::id());
        let result: crate::Inspection = serde_json::from_value(s["result"].clone()).unwrap();
        assert_eq!(result, crate::inspect(&game).unwrap());
        // The request is gone and no temporary status is left behind.
        assert_eq!(names(&dir), ["status.json"]);
    }

    #[test]
    fn failures_carry_a_cause() {
        let tmp = TempDir::new("jobfail");
        let game = game_folder(&tmp.0);
        let run_with = |request: Option<&str>| {
            if let Some(r) = request {
                std::fs::write(tmp.0.join("request.json"), r).unwrap();
            }
            assert_eq!(run(&tmp.0).unwrap(), 1);
            assert!(!tmp.0.join("request.json").exists());
            status(&tmp.0)
        };
        assert_eq!(run_with(None)["cause"], "convert_bad_request");
        assert_eq!(run_with(Some("{not json"))["cause"], "convert_bad_request");
        assert_eq!(
            run_with(Some(&"x".repeat(MAX_REQUEST_BYTES as usize + 1)))["cause"],
            "convert_bad_request"
        );
        assert_eq!(
            run_with(Some(r#"{"id":"a","op":"build"}"#))["cause"],
            "convert_bad_request"
        );
        assert_eq!(
            run_with(Some(r#"{"id":"a","op":"mount"}"#))["cause"],
            "convert_bad_request"
        );
        let s = run_with(Some(
            r#"{"id":"b","op":"inspect","path":"/nonexistent/game.exfat"}"#,
        ));
        assert_eq!(
            (s["id"].as_str(), s["cause"].as_str()),
            (Some("b"), Some("convert_source_unreadable"))
        );
        let build = |extra: serde_json::Value| {
            let mut r = json!({"id": "c", "op": "build", "path": game,
                "dest": tmp.0.join("out.exfat"), "format": "exfat"});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            run_with(Some(&r.to_string()))["cause"].clone()
        };
        assert_eq!(build(json!({"format": "iso"})), "convert_bad_request");
        assert_eq!(build(json!({"verify": "some"})), "convert_bad_request");
        assert_eq!(build(json!({"dest": "relative.exfat"})), "convert_bad_dest");
        assert_eq!(
            build(json!({"dest": tmp.0.join("out.ffpkg")})),
            "convert_bad_dest"
        );
        assert_eq!(
            build(json!({"dest": game.join("inside.exfat")})),
            "convert_bad_dest"
        );
        assert_eq!(
            build(json!({"dest": tmp.0.join("no-such-dir/out.exfat")})),
            "convert_bad_dest"
        );
        std::fs::write(tmp.0.join("taken.exfat"), b"mine").unwrap();
        assert_eq!(
            build(json!({"dest": tmp.0.join("taken.exfat")})),
            "convert_destination_exists"
        );
        assert_eq!(std::fs::read(tmp.0.join("taken.exfat")).unwrap(), b"mine");
        // A folder named like the output's `.partial` that no build of ours left: kept, refused.
        for (dest, format) in [("kept", "folder"), ("kept.exfat", "exfat")] {
            let theirs = tmp.0.join(format!("{dest}.partial"));
            std::fs::create_dir_all(&theirs).unwrap();
            std::fs::write(theirs.join("save.dat"), b"keep").unwrap();
            assert_eq!(
                build(json!({"dest": tmp.0.join(dest), "format": format})),
                "convert_partial_in_the_way"
            );
            assert_eq!(std::fs::read(theirs.join("save.dat")).unwrap(), b"keep");
            assert!(!tmp.0.join(dest).exists());
        }
        // zstd packs without ampr-emu-ng on the console: refused, nothing downloaded.
        assert_eq!(build(json!({"zstd": true})), "convert_no_zstd_runtime");
        assert_eq!(
            build(json!({"zstd": true, "zstd_runtime": tmp.0.join("missing.sprx")})),
            "convert_no_zstd_runtime"
        );
        std::fs::write(tmp.0.join("emu.sprx"), b"EMULATOR").unwrap();
        assert_eq!(
            build(json!({"zstd": true, "zstd_runtime": tmp.0.join("emu.sprx"), "zstd_level": 30})),
            "convert_bad_request"
        );
        assert!(!tmp.0.join("out.exfat").exists());
        assert!(!tmp.0.join("out.exfat.partial").exists());
    }

    /// Every output format, built through the job folder as the helper asks for it: done, the
    /// output in place and inspecting as the game, nothing else left beside it.
    #[test]
    fn a_build_writes_every_format_and_reads_back() {
        let (tmp, game, dir) = setup("jobbuild");
        let out = tmp.0.join("out");
        std::fs::create_dir_all(&out).unwrap();
        for (format, name, verify) in [
            ("exfat", "g.exfat", "fast"),
            ("ffpkg", "g.ffpkg", "full"),
            ("ffpfs", "g.ffpfs", ""),
            ("folder", "g", "full"),
        ] {
            let dest = out.join(name);
            ask(
                &dir,
                json!({"id": format, "op": "build", "path": game, "dest": dest,
                    "format": format, "zstd": false, "verify": verify}),
            );
            let mut stages: Vec<String> = Vec::new();
            let code = run_with(&dir, &mut |s| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            })
            .unwrap();
            let s = status(&dir);
            assert_eq!(code, 0, "{format}: {s}");
            assert_eq!(s["state"], "done");
            assert_eq!(s["result"]["format"], format);
            assert_eq!(s["result"]["files"], 3);
            assert!(stages.contains(&"write".to_string()), "{stages:?}");
            assert_eq!(stages.last().map(String::as_str), Some("verify"));
            let i = crate::inspect(&dest).unwrap();
            assert_eq!(i.title.as_deref(), Some("Test Game"), "{format}");
            assert_eq!(i.data_bytes, crate::inspect(&game).unwrap().data_bytes);
            assert_eq!(names(&dir), ["status.json"], "{format}");
        }
        assert_eq!(names(&out), ["g", "g.exfat", "g.ffpfs", "g.ffpkg"]);
    }

    /// zstd packs with the emulator the request names: an image and a folder, each reading
    /// back through its packs.
    #[test]
    fn a_build_with_zstd_packs_uses_the_given_emulator() {
        let (tmp, game, dir) = setup("jobzstd");
        // Enough data to pack.
        std::fs::write(game.join("data/table.bin"), "asset 7 ".repeat(40_000)).unwrap();
        let emu = tmp.0.join("libSceAmpr.sprx");
        std::fs::write(&emu, b"EMULATOR").unwrap();
        for (format, name) in [("exfat", "z.exfat"), ("folder", "z")] {
            let dest = tmp.0.join(name);
            ask(
                &dir,
                json!({"id": "z", "op": "build", "path": game, "dest": dest, "format": format,
                    "zstd": true, "zstd_runtime": emu, "zstd_level": 3}),
            );
            let mut stages: Vec<String> = Vec::new();
            let code = run_with(&dir, &mut |s| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            })
            .unwrap();
            assert_eq!(code, 0, "{format}: {}", status(&dir));
            assert_eq!(
                stages.first().map(String::as_str),
                Some("pack"),
                "{stages:?}"
            );
            assert_eq!(
                crate::inspect(&dest).unwrap().packed.as_deref(),
                Some("zstd")
            );
        }
    }

    /// The helper's Stop mid-build: the cancel file is seen, the build stops, and neither the
    /// output nor any `.partial` is left.
    #[test]
    fn a_cancel_mid_build_leaves_nothing() {
        let (tmp, game, dir) = setup("jobcancel");
        // Large enough that the write stage reports more than once.
        std::fs::write(game.join("data/big.bin"), vec![3u8; 24 << 20]).unwrap();
        for (format, name) in [("exfat", "c.exfat"), ("folder", "c")] {
            let dest = tmp.0.join(name);
            let _ = std::fs::remove_file(dir.join("cancel"));
            ask(
                &dir,
                json!({"id": "c", "op": "build", "path": game, "dest": dest, "format": format}),
            );
            let cancel = dir.join("cancel");
            let code = run_with(&dir, &mut |s| {
                if s == "write" && !cancel.exists() {
                    std::fs::write(&cancel, b"").unwrap();
                    // Long enough for the watcher to see it.
                    std::thread::sleep(CANCEL_EVERY * 3);
                }
            })
            .unwrap();
            let s = status(&dir);
            assert_eq!(code, 1, "{format}: {s}");
            assert_eq!(s["cause"], "convert_cancelled", "{format}");
            assert!(!dest.exists(), "{format}");
            let left: Vec<String> = names(&tmp.0)
                .into_iter()
                .filter(|n| n.contains(".partial"))
                .collect();
            assert!(left.is_empty(), "{format}: {left:?}");
        }
    }

    /// A source named like the build's own leftovers (`<dest>.partial`, its side files, or a
    /// folder inside one) is refused before anything is cleared: the source stays.
    #[test]
    fn a_source_where_the_build_writes_is_refused_and_kept() {
        let tmp = TempDir::new("jobpartialsrc");
        let game = game_folder(&tmp.0);
        let as_partial = tmp.0.join("G.partial");
        std::fs::rename(&game, &as_partial).unwrap();
        let build = |path: &Path, dest: &Path, format: &str| {
            std::fs::write(
                tmp.0.join("request.json"),
                json!({"id": "p", "op": "build", "path": path, "dest": dest, "format": format})
                    .to_string(),
            )
            .unwrap();
            assert_eq!(run(&tmp.0).unwrap(), 1);
            status(&tmp.0)["cause"].clone()
        };
        assert_eq!(
            build(&as_partial, &tmp.0.join("G"), "folder"),
            "convert_bad_dest"
        );
        // Any case: one folder on an exFAT drive.
        assert_eq!(
            build(&as_partial, &tmp.0.join("g"), "folder"),
            "convert_bad_dest"
        );
        assert!(as_partial.join("sce_sys/param.json").is_file());

        // A source folder inside `<dest>.partial`.
        let outer = tmp.0.join("H.partial");
        std::fs::create_dir_all(&outer).unwrap();
        let inner = outer.join("PPSA01234");
        std::fs::rename(&as_partial, &inner).unwrap();
        assert_eq!(
            build(&inner, &tmp.0.join("H"), "folder"),
            "convert_bad_dest"
        );
        assert!(inner.join("sce_sys/param.json").is_file());

        let p = Path::new;
        assert!(reads_own_leftovers(p("/d/G.partial"), p("/d/G")));
        assert!(reads_own_leftovers(p("/d/G.partial/x"), p("/d/G")));
        assert!(reads_own_leftovers(
            p("/d/G.exfat.partial.resume"),
            p("/d/G.exfat")
        ));
        assert!(reads_own_leftovers(
            p("/d/G.exfat.partial.stamp"),
            p("/d/G.exfat")
        ));
        assert!(reads_own_leftovers(p("/d"), p("/d/G")));
        assert!(!reads_own_leftovers(p("/d/G.partial2"), p("/d/G")));
        assert!(!reads_own_leftovers(p("/d/Game"), p("/e/G")));
    }

    /// A converter that starts after its helper gave up on it finds the cancel file the helper
    /// left: it stops before its first write, and an earlier run's leftover is not cleared.
    #[test]
    fn a_cancel_there_at_start_stops_before_any_write() {
        let (tmp, game, dir) = setup("jobcancelfirst");
        let dest = tmp.0.join("late.exfat");
        let leftover = tmp.0.join("late.exfat.partial");
        std::fs::write(&leftover, b"an earlier run's").unwrap();
        std::fs::write(dir.join("cancel"), b"").unwrap();
        ask(
            &dir,
            json!({"id": "l", "op": "build", "path": game, "dest": dest, "format": "exfat"}),
        );
        let mut stages = Vec::new();
        let code = run_with(&dir, &mut |s| stages.push(s.to_string())).unwrap();
        let s = status(&dir);
        assert_eq!(code, 1, "{s}");
        assert_eq!(s["cause"], "convert_cancelled");
        assert!(stages.is_empty(), "{stages:?}");
        assert!(!dest.exists());
        assert_eq!(std::fs::read(&leftover).unwrap(), b"an earlier run's");
    }

    /// The build's own file size check (made once zstd packs are measured) has its cause.
    #[test]
    fn a_file_too_big_for_the_drive_has_its_cause() {
        let plan = Plan {
            source: std::env::temp_dir(),
            dest: std::env::temp_dir().join("g.exfat"),
            image: Some(ImageFormat::Exfat),
            mode: VerifyMode::Full,
            zstd: None,
        };
        let fat = safe_write::Probe {
            fs_type: "msdosfs".into(),
            free: None,
            read_only: false,
            max_file: Some((4u64 << 30) - 1),
        };
        let why = fat.file_refusal(5 << 30, "pack volume").unwrap();
        assert_eq!(
            failure(why, false, &plan).cause,
            "convert_too_big_for_drive"
        );
        assert_eq!(
            failure("short read".into(), false, &plan).cause,
            "convert_build_failed"
        );
    }

    #[test]
    fn a_long_detail_is_cut_at_a_character() {
        let long = "é".repeat(400);
        let cut = short(&long);
        assert!(cut.len() <= MAX_DETAIL_BYTES && cut.len() >= MAX_DETAIL_BYTES - 1);
        assert_eq!(short("short"), "short");
    }
}
