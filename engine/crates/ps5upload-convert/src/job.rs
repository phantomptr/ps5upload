//! One job, asked for and reported through files in a folder (on the PS5
//! `/data/ps5upload/convert`). The helper's extractor works the same way
//! (`payload/extractor/README.md`); no port is opened, so nothing but the helper can ask. The
//! helper's side is `payload/src/console_convert.c`.
//!
//! - `request.json`: written by the helper, read and deleted here at start.
//!   `{"id": "<the helper's job id>", "op": "inspect"|"build", "path": "<source>"}`, and for a
//!   build `"dest"` (the output's full path), `"format"` (`exfat`, `ffpkg`, `ffpfs`, `ffpfsc`,
//!   `folder`), `"zstd"` (bool), `"verify"` (`fast`, the default, or `full`), and optionally
//!   `"zstd_runtime"` (the path of ampr-emu-ng's `libSceAmpr.sprx` on the console, which zstd
//!   packs need) and `"zstd_level"` (1 to 22, 5 when absent); a `.ffpfsc` also `"inner"` (the
//!   image inside it: `exfat`, the default, `ffpkg` or `ffpfs`) and `"ffpfsc_level"` (`fast`,
//!   `balanced`, the default, or `smallest`), compressed on [`THREADS`] threads; a `.pkg`
//!   (`"format": "pkg"`, a debug package, written, never installed here) `"pkg_level"` (one of
//!   [`PKG_LEVELS`], `fast` when absent).
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
use ps5upload_fpkg::ffpfsc_block::Level;
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
pub const THREADS: usize = ps5upload_convert_core::WORKERS;

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
    /// With `format` `ffpfsc`: the image inside it, `exfat` (the default), `ffpkg` or `ffpfs`.
    #[serde(default)]
    inner: String,
    /// With `format` `ffpfsc`: `fast`, `balanced` (the default) or `smallest`.
    #[serde(default)]
    ffpfsc_level: String,
    /// With `format` `pkg`: `stored` (no compression), `fast` (the default) or `balanced`
    /// ([`PKG_LEVELS`]).
    #[serde(default)]
    pkg_level: String,
}

/// The Kraken levels a `.pkg` is made at on the console. `smallest` is left out: as slow as
/// `balanced` (hours for a large game on 3 CPUs) for no measured gain over it.
pub const PKG_LEVELS: [&str; 3] = ["stored", "fast", "balanced"];
/// A `.pkg`'s level when the request names none: PS5 Dump Forge's choice on the console.
const DEFAULT_PKG_LEVEL: ps5upload_fpkg::kraken::Level = ps5upload_fpkg::kraken::Level::Fast;
/// How often the `.pkg` being written is looked at for its growth (U5, [`build_pkg`]).
const PKG_SYNC_POLL: Duration = Duration::from_millis(100);

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
    /// A `.ffpfsc`: the name of the image inside it and how hard it is compressed.
    ffpfsc: Option<(String, Level)>,
    mode: VerifyMode,
    /// The emulator's bytes and the level, for zstd packs.
    zstd: Option<(Vec<u8>, u8)>,
    /// A `.pkg` (`image` is `None`) and its Kraken level.
    pkg: Option<ps5upload_fpkg::kraken::Level>,
    /// The source's content id, as its inspection read it.
    content_id: Option<String>,
    /// The `.zip` or `.7z` the inspection opened, read by the build rather than opened again.
    opened: Option<Box<dyn SourceTree>>,
}

fn bad(detail: String) -> Refusal {
    Refusal {
        cause: "convert_bad_request",
        detail,
    }
}

/// The request checked against the console: the source readable, the destination new and in a
/// folder that exists, the emulator there when zstd packs are asked for. `cancel` ends an
/// inspection waiting on an archive.
fn plan(request: &Request, cancel: &Arc<AtomicBool>) -> Result<Plan, Refusal> {
    if request.dest.is_empty() {
        return Err(bad("a build needs a dest".into()));
    }
    let mut pkg = None;
    let (image, compress) = match request.format.as_str() {
        "folder" => (None, false),
        "pkg" => {
            let level = match request.pkg_level.as_str() {
                "" => DEFAULT_PKG_LEVEL,
                l if PKG_LEVELS.contains(&l) => l.parse().map_err(bad)?,
                other => return Err(bad(format!("unknown .pkg level {other:?}"))),
            };
            if request.zstd {
                return Err(bad(
                    "zstd packs go into an image or a folder, not a .pkg".into()
                ));
            }
            pkg = Some(level);
            (None, false)
        }
        "exfat" => (Some(ImageFormat::Exfat), false),
        "ffpkg" => (Some(ImageFormat::Ffpkg), false),
        "ffpfs" => (Some(ImageFormat::Ffpfs), false),
        "ffpfsc" => {
            let inner = match request.inner.as_str() {
                "" | "exfat" => ImageFormat::Exfat,
                "ffpkg" => ImageFormat::Ffpkg,
                "ffpfs" => ImageFormat::Ffpfs,
                other => return Err(bad(format!("unknown image {other:?} inside a .ffpfsc"))),
            };
            (Some(inner), true)
        }
        other => return Err(bad(format!("unknown format {other:?}"))),
    };
    let level = if compress {
        Level::parse(Some(&request.ffpfsc_level)).map_err(bad)?
    } else {
        Level::default()
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
    if pkg.is_some() && !name.ends_with(".pkg") {
        // The package builder names its output `<stem>.pkg`, in this spelling.
        return Err(bad_dest(format!("{} does not end in .pkg", request.dest)));
    }
    let mut ffpfsc = None;
    if let Some(f) = image {
        let want = if compress { "ffpfsc" } else { f.extension() };
        let ext = dest
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if !ext.eq_ignore_ascii_case(want) {
            return Err(bad_dest(format!(
                "{} does not end in .{want}",
                request.dest
            )));
        }
        if compress {
            // The names ShadowMountPlus mounts: the container's at most 63 bytes, the image
            // inside it named after it with its own filesystem's extension.
            let (outer, inner) = image_build::output_names(&dest, f, true);
            if !outer.eq_ignore_ascii_case(name) {
                return Err(bad_dest(format!(
                    "{name}: a .ffpfsc's name is at most {} bytes, without \" * / : < > ? \\ |",
                    image_build::PFS_NAME_MAX
                )));
            }
            ffpfsc = inner.map(|inner| (inner, level));
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
    let (inspection, opened) = inspect_stoppable(&source, cancel)?;
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
        ffpfsc,
        mode,
        zstd,
        pkg,
        content_id: inspection.content_id,
        opened,
    })
}

/// [`crate::inspect_keeping`] ended by `cancel` (an archive's param file may be behind a large
/// solid block): a stopped inspection is `convert_cancelled`, not an unreadable source.
fn inspect_stoppable(
    path: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<(crate::Inspection, Option<Box<dyn SourceTree>>), Refusal> {
    let flag = Arc::clone(cancel);
    let stop: ps5upload_archive::stream::Stop = Arc::new(move || flag.load(Ordering::Relaxed));
    crate::inspect_keeping(path, stop).map_err(|refusal| {
        if cancel.load(Ordering::Relaxed) {
            Refusal {
                cause: "convert_cancelled",
                detail: refusal.detail,
            }
        } else {
            refusal
        }
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
/// eboot fake-signed; for packing, any packs it has decoded. A `.zip` or `.7z` is read in place
/// (`ps5upload_convert_core::archive_tree`), a read waiting on it ended by `cancel`; `opened`,
/// when given, is that archive already opened (by the plan's inspection).
fn open_source(
    path: &Path,
    packing: bool,
    cancel: &Arc<AtomicBool>,
    opened: Option<Box<dyn SourceTree>>,
) -> Result<Box<dyn SourceTree>, String> {
    let (tree, renamed, signing) = if let Some(tree) = opened {
        ps5upload_fpkg::module_case::wrap(tree)
    } else if is_archive_file(path) {
        let flag = Arc::clone(cancel);
        let stop: ps5upload_archive::stream::Stop = Arc::new(move || flag.load(Ordering::Relaxed));
        let tree = crate::open_archive(path, stop).map_err(|r| r.detail)?;
        ps5upload_fpkg::module_case::wrap(tree)
    } else {
        ps5upload_fpkg::module_case::open_with_update(path, None).map_err(|e| e.to_string())?
    };
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

/// Whether `path` is a `.zip` or `.7z` file, read in place (a `.rar` never gets this far: the
/// inspection in [`plan`] refuses it).
fn is_archive_file(path: &Path) -> bool {
    path.is_file()
        && matches!(
            ps5upload_archive::stream::Kind::of(path),
            Some(ps5upload_archive::stream::Kind::Zip | ps5upload_archive::stream::Kind::SevenZ)
        )
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
    } else if lower.contains("holds files of at most") || lower.contains("file too large") {
        // The second: EFBIG, a `.ffpfsc` (its size known only once written) past FAT32's 4 GiB.
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
    cancel: &Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<serde_json::Value, Refusal> {
    let mut plan = plan(request, cancel)?;
    let unreadable = |e: String| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", plan.source.display()),
    };
    if is_archive_file(&plan.source) && plan.pkg.is_some() {
        // The package builder opens its source by path, as a folder or an image.
        return Err(Refusal {
            cause: "convert_archive_needs_unpack",
            detail: format!(
                "{}: a .pkg is made from an unpacked game; unpack the archive first",
                plan.source.display()
            ),
        });
    }
    if is_archive_file(&plan.source) && plan.image.is_none() && plan.zstd.is_some() {
        // A folder in zstd packs reads its source again after packing (as the engine refuses it
        // for an archive): once more through a whole solid archive.
        return Err(Refusal {
            cause: "convert_archive_needs_unpack",
            detail: format!(
                "{}: a folder with zstd packs is made from an unpacked game; unpack the archive \
                 first, or make an image",
                plan.source.display()
            ),
        });
    }
    let opened = plan.opened.take();
    let mut tree = open_source(&plan.source, false, cancel, opened).map_err(unreadable)?;
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
    // A `.ffpfsc` is smaller by what compression saves, known only once written; a drive that
    // fills up ends the build with `convert_no_space`.
    let compressed = plan
        .pkg
        .is_some_and(|l| l != ps5upload_fpkg::kraken::Level::Stored);
    let need = if plan.zstd.is_some() || plan.ffpfsc.is_some() || compressed {
        0
    } else {
        data
    };
    let one_file = plan.image.is_some() || plan.pkg.is_some();
    if let Some(why) = probe.refusal(need, one_file, folder_build::SPARE) {
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
    if let Some((inner, level)) = &plan.ffpfsc {
        crate::log(format_args!(
            ".ffpfsc holding {inner}, {} compression on {} threads",
            level.name(),
            THREADS
        ));
    }
    let started = Instant::now();
    if let Some(level) = plan.pkg {
        let files = tree.files().len() as u64;
        // The package builder opens the source itself.
        drop(tree);
        return build_pkg(&plan, level, files, cancel, stage).map(|(bytes, detail)| {
            crate::log(format_args!(
                "built {} in {:.1} s: {detail}",
                plan.dest.display(),
                started.elapsed().as_secs_f64()
            ));
            json!({
                "dest": plan.dest,
                "format": request.format,
                "files": files,
                "bytes": bytes,
                "detail": detail,
            })
        });
    }
    let chunk = image_build::DEFAULT_VERIFY_CHUNK;
    let source = plan.source.clone();
    let packing = plan.zstd.is_some();
    let flag = Arc::clone(cancel);
    let reopen = move || open_source(&source, packing, &flag, None);
    let built = match (plan.image, &plan.zstd) {
        (Some(format), zstd) => {
            // A PS4 game's image needs the param.json ShadowMount+ reads.
            let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *tree)
                .map_err(|e| unreadable(format!("param.sfo: {e}")))?;
            let tree: &mut dyn SourceTree = match ps4.as_mut() {
                Some(p) => p,
                None => &mut *tree,
            };
            // A `.ffpfsc`: the image inside it and its compression level.
            let (inner, compression) = match &plan.ffpfsc {
                Some((name, level)) => (Some(name.as_str()), *level),
                None => (None, Level::default()),
            };
            match zstd {
                Some((runtime, level)) => zstd_packs::build_at(
                    format,
                    inner,
                    compression,
                    *level,
                    None,
                    runtime,
                    tree,
                    &plan.dest,
                    plan.mode,
                    chunk,
                    cancel,
                    stage,
                    THREADS,
                ),
                None => {
                    // A libSceAmpr game gets a fresh ampr_emu.index, as on a computer (#449).
                    let now = zstd_packs::now_secs();
                    match image_build::AmprIndexed::wrap_at(&mut *tree, now) {
                        Ok(Some(mut indexed)) => image_build::build_at(
                            format,
                            inner,
                            compression,
                            &mut indexed,
                            &plan.dest,
                            plan.mode,
                            chunk,
                            cancel,
                            stage,
                        ),
                        Ok(None) => image_build::build_at(
                            format,
                            inner,
                            compression,
                            tree,
                            &plan.dest,
                            plan.mode,
                            chunk,
                            cancel,
                            stage,
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

/// The stem the package builder is given for `dest`: the name without one `.pkg`, so the
/// builder's `<stem>.pkg` and `<stem>.pkg.partial` are `dest` and `partial_of(dest)` — the names
/// the sync, the claim, the cleanup and the helper use (`g.pkg.pkg` stays `g.pkg.pkg`).
fn pkg_stem(dest: &Path) -> String {
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name.strip_suffix(".pkg").unwrap_or(name).to_string()
}

/// Builds the plan's `.pkg` with `ps5upload-fpkg` (a debug package, the engine's builder; not
/// installed here): `<dest>.partial` beside `dest`, read back, renamed. Its stages are the
/// builder's (`check` reads the game's headers, `compress` is Kraken on the converter's
/// threads, `write`, `verify`). U5: the builder writes through its own file, so a thread syncs
/// the `.partial` each time it grew by `SYNC_EVERY` ([`safe_write::GrowthSync`]); a failed
/// sync stops the build. The size and a line for the result, or why it failed.
fn build_pkg(
    plan: &Plan,
    level: ps5upload_fpkg::kraken::Level,
    files: u64,
    cancel: &Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<(u64, String), Refusal> {
    use ps5upload_fpkg::build::{BuildControl, BuildRequest, Stage};
    let dir = plan.dest.parent().unwrap_or(Path::new("/"));
    let mut request = BuildRequest::new(&plan.source, dir);
    request.file_name = Some(pkg_stem(&plan.dest));
    // As the inspection reads it: a dump's param.json may start with a BOM and end in NULs.
    request.content_id = plan.content_id.clone();
    request.kraken = true;
    request.level = level;
    crate::log(format_args!(
        ".pkg at Kraken level {level:?} on {} threads ({files} files)",
        ps5upload_fpkg::workshop::workshop_threads()
    ));
    let sync = safe_write::GrowthSync::start(
        folder_build::partial_of(&plan.dest),
        safe_write::SYNC_EVERY,
        PKG_SYNC_POLL,
        Arc::clone(cancel),
    );
    let now = std::cell::Cell::new(Stage::Check);
    let stage = std::cell::RefCell::new(stage);
    let mut on_stage = |s: Stage| {
        now.set(s);
        (stage.borrow_mut())(s.id(), 0, 0);
    };
    let mut on_bytes = |done: u64, total: u64| (stage.borrow_mut())(now.get().id(), done, total);
    let mut control = BuildControl {
        cancel: Some(cancel),
        sample: match plan.mode {
            VerifyMode::Full => None,
            VerifyMode::Fast { seed, .. } => Some(seed),
        },
        ..BuildControl::default()
    };
    control.stage = Some(&mut on_stage);
    control.bytes = Some(&mut on_bytes);
    let built = ps5upload_fpkg::build::build_controlled(
        &request,
        &mut |line| crate::log(format_args!("pkg: {line}")),
        &mut control,
    );
    let synced = sync.finish();
    let fail = |detail: String| {
        remove_leftovers(&plan.dest);
        failure(detail, cancel.load(Ordering::Relaxed), plan)
    };
    match (built, synced) {
        (Ok(report), Ok(syncs)) => {
            if report.path != plan.dest {
                return Err(fail(format!(
                    "the package was written as {}, not {}",
                    report.path.display(),
                    plan.dest.display()
                )));
            }
            Ok((
                report.size,
                format!(
                    "pkg, Kraken {level:?}, {} bytes, content id {}, {syncs} syncs while written, \
                     verified: {}",
                    report.size,
                    report.content_id,
                    if report.verify.ok() { "ok" } else { "failed" }
                ),
            ))
        }
        // A sync that failed raised the cancel flag; its error is the cause, not "cancelled".
        (_, Err(e)) => Err(fail(format!("the drive did not save the package: {e}"))),
        (Err(e), Ok(_)) => Err(fail(e.to_string())),
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
            // Under the cancel watcher, as a build: a `.7z`'s param file may be behind a large
            // solid block, and Stop ends the wait.
            let cancel = Arc::new(AtomicBool::new(dir.join("cancel").exists()));
            let finished = Arc::new(AtomicBool::new(false));
            let watcher = watch_cancel(dir, Arc::clone(&cancel), Arc::clone(&finished));
            let inspected = inspect_stoppable(Path::new(&request.path), &cancel);
            finished.store(true, Ordering::Relaxed);
            let _ = watcher.join();
            match inspected.map(|(inspection, _)| inspection) {
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

    /// Inspecting a `.7z` (its param file may be behind a large solid block) is ended by Stop:
    /// `convert_cancelled`, as a build is; the same archive inspects when not stopped.
    #[test]
    fn an_archive_inspect_is_ended_by_stop() {
        let (tmp, game, dir) = setup("jobinspectstop");
        let archive = tmp.0.join("s.7z");
        crate::testing::sevenz_of(&game, &archive, true);
        std::fs::write(dir.join("cancel"), b"").unwrap();
        ask(&dir, json!({"id": "i", "op": "inspect", "path": archive}));
        assert_eq!(run(&dir).unwrap(), 1);
        let s = status(&dir);
        assert_eq!(s["cause"], "convert_cancelled", "{s}");
        std::fs::remove_file(dir.join("cancel")).unwrap();
        ask(&dir, json!({"id": "i", "op": "inspect", "path": archive}));
        assert_eq!(run(&dir).unwrap(), 0, "{}", status(&dir));
        assert_eq!(status(&dir)["result"]["format"], "7z");
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
        let ffpfsc = tmp.0.join("out.ffpfsc");
        assert_eq!(
            build(json!({"format": "ffpfsc", "dest": ffpfsc, "inner": "iso"})),
            "convert_bad_request"
        );
        assert_eq!(
            build(json!({"format": "ffpfsc", "dest": ffpfsc, "ffpfsc_level": "max"})),
            "convert_bad_request"
        );
        // Named for what it is, and short enough for ShadowMountPlus to mount.
        assert_eq!(build(json!({"format": "ffpfsc"})), "convert_bad_dest");
        assert_eq!(
            build(
                json!({"format": "ffpfsc", "dest": tmp.0.join(format!("{}.ffpfsc", "g".repeat(60)))})
            ),
            "convert_bad_dest"
        );
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

    /// A `.ffpfsc` holding each image, at each level: done, and it reads back as the game (the
    /// console's reader, through `inspect`).
    #[test]
    fn a_build_writes_a_ffpfsc_holding_each_image() {
        let (tmp, game, dir) = setup("jobffpfsc");
        // Some data deflate shrinks, some it can't.
        std::fs::write(game.join("data/text.bin"), "a line of text ".repeat(20_000)).unwrap();
        let noise: Vec<u8> = (0u32..300_000)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(game.join("data/noise.bin"), noise).unwrap();
        let want = crate::inspect(&game).unwrap();
        for (inner, level) in [
            ("", ""),
            ("ffpkg", "fast"),
            ("ffpfs", "smallest"),
            ("exfat", "balanced"),
        ] {
            let dest = tmp.0.join(format!("c{inner}{level}.ffpfsc"));
            ask(
                &dir,
                json!({"id": "f", "op": "build", "path": game, "dest": dest, "format": "ffpfsc",
                    "inner": inner, "ffpfsc_level": level, "verify": "full"}),
            );
            let mut stages: Vec<String> = Vec::new();
            let code = run_with(&dir, &mut |s| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            })
            .unwrap();
            let s = status(&dir);
            assert_eq!(code, 0, "{inner} {level}: {s}");
            assert_eq!(s["result"]["format"], "ffpfsc");
            assert!(stages.contains(&"compress".to_string()), "{stages:?}");
            let i = crate::inspect(&dest).unwrap();
            assert_eq!(i.format, "ffpfsc");
            assert_eq!(i.title.as_deref(), Some("Test Game"), "{inner}");
            assert_eq!((i.files, i.data_bytes), (want.files, want.data_bytes));
            // Compressed: smaller than the game it holds.
            assert!(i.size < want.data_bytes, "{inner} {level}: {}", i.size);
            assert!(!tmp
                .0
                .join(format!("c{inner}{level}.ffpfsc.partial"))
                .exists());
        }
    }

    /// A game read straight from a `.zip` and a `.7z` (solid and not) into an image and a
    /// folder: done, and the output holds the game; zstd packs in a folder from an archive are
    /// refused (unpack it first).
    #[test]
    fn a_build_reads_the_game_in_an_archive() {
        let (tmp, game, dir) = setup("jobarchive");
        let want = crate::inspect(&game).unwrap();
        for (archive, solid) in [("g.zip", false), ("g.7z", false), ("s.7z", true)] {
            let path = tmp.0.join(archive);
            if archive.ends_with(".zip") {
                crate::testing::zip_of(&game, &path);
            } else {
                crate::testing::sevenz_of(&game, &path, solid);
            }
            for (format, ext) in [("ffpkg", ".ffpkg"), ("folder", "")] {
                let dest = tmp
                    .0
                    .join(format!("out-{}{ext}", archive.replace('.', "-")));
                ask(
                    &dir,
                    json!({"id": "a", "op": "build", "path": path, "dest": dest,
                        "format": format, "verify": "full"}),
                );
                let code = run_with(&dir, &mut |_| {}).unwrap();
                assert_eq!(code, 0, "{archive} -> {format}: {}", status(&dir));
                let i = crate::inspect(&dest).unwrap();
                assert_eq!(i.title.as_deref(), Some("Test Game"), "{archive} {format}");
                assert_eq!((i.files, i.data_bytes), (want.files, want.data_bytes));
                assert_eq!(i.empty_dirs, want.empty_dirs, "{archive} {format}");
            }
        }
        let emu = tmp.0.join("libSceAmpr.sprx");
        std::fs::write(&emu, b"EMULATOR").unwrap();
        ask(
            &dir,
            json!({"id": "z", "op": "build", "path": tmp.0.join("g.zip"),
                "dest": tmp.0.join("zf"), "format": "folder", "zstd": true,
                "zstd_runtime": emu}),
        );
        assert_eq!(run_with(&dir, &mut |_| {}).unwrap(), 1);
        assert_eq!(status(&dir)["cause"], "convert_archive_needs_unpack");
        assert!(!tmp.0.join("zf").exists());
    }

    /// A `.pkg` at each level the console offers: done, through the builder's stages, and the
    /// package reads back as the game; what a `.pkg` is not made from or with is refused.
    #[test]
    fn a_build_writes_a_pkg_at_each_level() {
        let (tmp, game, dir) = setup("jobpkg");
        let want = crate::inspect(&game).unwrap();
        for level in ["stored", "fast", "balanced", ""] {
            let dest = tmp.0.join(format!("p{level}.pkg"));
            ask(
                &dir,
                json!({"id": "p", "op": "build", "path": game, "dest": dest, "format": "pkg",
                    "pkg_level": level, "verify": if level == "fast" { "fast" } else { "full" }}),
            );
            let mut stages: Vec<String> = Vec::new();
            let code = run_with(&dir, &mut |s| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            })
            .unwrap();
            let s = status(&dir);
            assert_eq!(code, 0, "{level}: {s}");
            assert_eq!(s["result"]["format"], "pkg");
            for st in ["compress", "write", "verify"] {
                assert!(stages.contains(&st.to_string()), "{level}: {stages:?}");
            }
            let mut pkg = ps5upload_fpkg::source::open(&dest).unwrap();
            assert!(
                pkg.files().iter().any(|f| f.path == "data/level.bin"),
                "{level}"
            );
            assert_eq!(
                ps5upload_fpkg::source::read_small(pkg.as_mut(), "data/level.bin", 1 << 20)
                    .unwrap(),
                vec![7u8; 100_000]
            );
            assert!(want.files <= pkg.files().len() as u64);
            assert!(!tmp.0.join(format!("p{level}.pkg.partial")).exists());
        }
        let refused = |extra: serde_json::Value| {
            let mut r = json!({"id": "r", "op": "build", "path": game,
                "dest": tmp.0.join("r.pkg"), "format": "pkg"});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            ask(&dir, r);
            assert_eq!(run_with(&dir, &mut |_| {}).unwrap(), 1);
            status(&dir)["cause"].clone()
        };
        assert_eq!(
            refused(json!({"pkg_level": "smallest"})),
            "convert_bad_request"
        );
        assert_eq!(refused(json!({"zstd": true})), "convert_bad_request");
        assert_eq!(
            refused(json!({"dest": tmp.0.join("r.PKG")})),
            "convert_bad_dest"
        );
        let zip = tmp.0.join("g.zip");
        crate::testing::zip_of(&game, &zip);
        assert_eq!(
            refused(json!({"path": zip})),
            "convert_archive_needs_unpack"
        );
        assert!(!tmp.0.join("r.pkg").exists());
    }

    /// A `.pkg` named `g.pkg.pkg`: the builder writes that name (one `.pkg` taken off for its
    /// stem, not every one), so the sync, claim and cleanup watch the `.partial` it writes.
    #[test]
    fn a_pkg_named_with_two_pkg_extensions_keeps_its_name() {
        for name in ["g.pkg", "g.pkg.pkg", "a.pkg.pkg.pkg"] {
            let dest = Path::new("/data/out").join(name);
            let stem = pkg_stem(&dest);
            assert_eq!(Path::new("/data/out").join(format!("{stem}.pkg")), dest);
            assert_eq!(
                Path::new("/data/out").join(format!("{stem}.pkg.partial")),
                folder_build::partial_of(&dest)
            );
        }
        let (tmp, game, dir) = setup("jobpkgpkg");
        let dest = tmp.0.join("g.pkg.pkg");
        ask(
            &dir,
            json!({"id": "p", "op": "build", "path": game, "dest": dest, "format": "pkg",
                "pkg_level": "stored", "verify": "full"}),
        );
        let code = run_with(&dir, &mut |_| {}).unwrap();
        assert_eq!(code, 0, "{}", status(&dir));
        assert!(dest.is_file());
        assert!(!tmp.0.join("g.pkg").exists());
        assert!(!tmp.0.join("g.pkg.pkg.partial").exists());
    }

    /// Stop while a `.pkg` is read back: `convert_cancelled`, and nothing is left.
    #[test]
    fn a_stop_during_a_pkg_verify_is_cancelled() {
        let (tmp, game, _dir) = setup("jobpkgstop");
        let dest = tmp.0.join("s.pkg");
        let request: Request = serde_json::from_value(json!({"id": "p", "op": "build",
            "path": game, "dest": dest, "format": "pkg", "pkg_level": "stored",
            "verify": "full"}))
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let refusal = build(&request, &cancel, &mut |stage, _, _| {
            if stage == "verify" {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(refusal.cause, "convert_cancelled", "{refusal}");
        assert!(!dest.exists());
        assert!(!tmp.0.join("s.pkg.partial").exists());
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
        for (format, name) in [
            ("exfat", "z.exfat"),
            ("ffpfsc", "z.ffpfsc"),
            ("folder", "z"),
        ] {
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
        for (format, name) in [
            ("exfat", "c.exfat"),
            ("ffpfsc", "c.ffpfsc"),
            ("pkg", "c.pkg"),
            ("folder", "c"),
        ] {
            let dest = tmp.0.join(name);
            let _ = std::fs::remove_file(dir.join("cancel"));
            ask(
                &dir,
                json!({"id": "c", "op": "build", "path": game, "dest": dest, "format": format}),
            );
            let cancel = dir.join("cancel");
            let code = run_with(&dir, &mut |s| {
                if (s == "write" || s == "compress") && !cancel.exists() {
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
            ffpfsc: None,
            mode: VerifyMode::Full,
            zstd: None,
            pkg: None,
            content_id: None,
            opened: None,
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
