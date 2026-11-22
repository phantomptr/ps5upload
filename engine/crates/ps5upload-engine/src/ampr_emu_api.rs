//! Adding the AMPR emulator to a game that is already where it plays, without converting it.
//!
//! Some games that read through libSceAmpr and ship no emulator fail to start from an image or a
//! folder, and ShadowMount+ doesn't add one. Convert puts one in what it writes; this is for a
//! game that already exists:
//!
//! - a **folder**, here or on the PS5: the emulator goes in the folder, with an `ampr_emu.index`
//!   when it has none.
//! - an **image on the PS5**, `.ffpfsc` included: images are read-only, so the emulator goes in a
//!   ShadowMount+ backport folder, `<scan path>/backports/<TITLE_ID>`, laid over the game. A fresh
//!   one also takes the game's other fakelib modules, because the backport's folder is the one
//!   the game then loads from.
//!
//! The layout follows fthdgn-gh/Alembic's `attach_backport` (GPL-3.0-or-later). Tried on the Pro:
//! ShadowMount+ said "Backport applied" and LEGO Horizon Adventures reached its menu. An image on this computer gets it from Convert instead.

use std::path::Path;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use ps5upload_fpkg::ampr_index::{self, NAME as INDEX};
use ps5upload_fpkg::ampr_pack::image::emulator_needed_at;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

use crate::fpkg_api::resolve_engine_path;
use crate::json_err;

/// Where images are given a backport when no folder for the title exists yet.
const BACKPORTS_ROOT: &str = "/data/homebrew/backports";
const MODULE: &str = "libSceAmpr.sprx";

#[derive(Deserialize)]
pub(crate) struct AmprEmuReq {
    source: String,
    /// Which emulator: `ampr_emu` (the default) or `ampr-emu-ng`.
    #[serde(default)]
    build: Option<String>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct AmprEmuOutcome {
    /// `folder`: added to the game's own folder. `backport`: a backport folder for an image.
    kind: &'static str,
    /// Where the files went.
    folder: String,
    /// What was written, relative to `folder`.
    written: Vec<String>,
    /// Whether ShadowMount+ was asked to scan again (a console's image only).
    rescanned: bool,
}

/// The folder a title's backport goes in: an existing one beside the image or one folder up, as
/// ShadowMount+ prefers `<scan path>/backports/<TITLE_ID>`, else the default root.
fn backport_folder(image: &str, title_id: &str, exists: &dyn Fn(&str) -> bool) -> String {
    let mut dir = image.rsplit_once('/').map_or("", |(d, _)| d);
    for _ in 0..2 {
        if dir.is_empty() {
            break;
        }
        let candidate = format!("{dir}/backports/{title_id}");
        if exists(&candidate) {
            return candidate;
        }
        dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
    }
    format!("{BACKPORTS_ROOT}/{title_id}")
}

/// Left out of a backport's copy of the game's modules.
fn left_out(name: &str) -> bool {
    name.eq_ignore_ascii_case("libkernel.sprx")
        || name.eq_ignore_ascii_case(MODULE)
        || name.starts_with("._")
        || name.eq_ignore_ascii_case(".DS_Store")
        || name.eq_ignore_ascii_case("Thumbs.db")
}

/// What to write for the game `tree`, relative to the folder it goes in, emulator last: if a
/// write fails, a retry doesn't find the emulator and call the game done.
///
/// `at`: the emulator's path. `copy_modules`: the game's own modules from `own_dir` come along.
fn writes(
    tree: &mut dyn SourceTree,
    at: &str,
    runtime: &[u8],
    copy_modules: Option<(&str, &str)>,
    write_index: bool,
    mtime: i64,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    if let Some((own_dir, target)) = copy_modules {
        let prefix = format!("{own_dir}/");
        let modules: Vec<String> = tree
            .files()
            .iter()
            .filter_map(|f| {
                let name = f.path.strip_prefix(&prefix)?;
                (!name.contains('/') && !left_out(name)).then(|| name.to_string())
            })
            .collect();
        for name in modules {
            let bytes = tree
                .read(&format!("{own_dir}/{name}"))
                .map_err(|e| format!("{own_dir}/{name}: {e}"))?;
            out.push((format!("{target}/{name}"), bytes));
        }
    }
    if write_index {
        let mut files: Vec<SourceFile> = tree.files().to_vec();
        files.push(SourceFile {
            path: at.to_string(),
            size: runtime.len() as u64,
        });
        let index = ampr_index::build(&ampr_index::listing(&files), mtime)?;
        out.push((INDEX.to_string(), index));
    }
    out.push((at.to_string(), runtime.to_vec()));
    Ok(out)
}

fn has_index(tree: &dyn SourceTree) -> bool {
    tree.files()
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case(INDEX))
}

/// The emulator's path, or why the game takes none.
fn needed(tree: &mut dyn SourceTree) -> Result<String, String> {
    if !ps5upload_fpkg::source::imports_ampr(tree, "eboot.bin") {
        return Err(
            "this game does not read its files through AMPR, so it needs no emulator".into(),
        );
    }
    emulator_needed_at(tree).ok_or_else(|| "this game already has an AMPR emulator".to_string())
}

fn title_id(tree: &mut dyn SourceTree) -> Result<String, String> {
    let param = tree
        .read("sce_sys/param.json")
        .map_err(|e| format!("sce_sys/param.json: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_slice(&param).map_err(|e| format!("sce_sys/param.json: {e}"))?;
    v.get("titleId")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| "sce_sys/param.json has no titleId".to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn to_console(
    addr: &str,
    folder: &str,
    files: &[(String, Vec<u8>)],
) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for (rel, bytes) in files {
        let path = format!("{folder}/{rel}");
        ps5upload_core::fs_ops::fs_mkdir(addr, parent_of(&path))
            .map_err(|e| format!("{}: {e:#}", parent_of(&path)))?;
        ps5upload_core::diagnostics::fs_write_bytes_large(addr, &path, bytes, false)
            .map_err(|e| format!("{path}: {e:#}"))?;
        written.push(rel.clone());
    }
    Ok(written)
}

/// An image on the console: a backport folder, and ShadowMount+ asked to look again.
fn attach_console_image(
    addr: &str,
    image: &Path,
    build: Option<&str>,
) -> Result<AmprEmuOutcome, String> {
    let (_, path) = crate::convert_source::console_target(&image.to_string_lossy())?;
    let mut tree = ps5upload_fpkg::source::open(image).map_err(|e| e.to_string())?;
    let module = needed(tree.as_mut())?;
    let id = title_id(tree.as_mut())?;
    let exists = |p: &str| ps5upload_core::fs_ops::fs_exists(addr, p).unwrap_or(false);
    let folder = backport_folder(&path, &id, &exists);
    let own_dir = parent_of(&module).to_string();
    // A backport with its own modules keeps them; the emulator joins them.
    let existing = ["fakelib2", "fakelib"]
        .into_iter()
        .find(|d| exists(&format!("{folder}/{d}")));
    let target = existing.unwrap_or(&own_dir);
    let at = format!("{target}/{MODULE}");
    if exists(&format!("{folder}/{at}")) {
        return Err(format!("{folder} already has an AMPR emulator in {target}"));
    }
    let build =
        crate::ampr_trace::emulator_for_packs(crate::remove_backport::packed(tree.as_mut()), build);
    let runtime = crate::ampr_trace::emulator(build)?;
    let write_index = !has_index(tree.as_ref()) && !exists(&format!("{folder}/{INDEX}"));
    let copy = existing.is_none().then_some((own_dir.as_str(), target));
    let files = writes(tree.as_mut(), &at, &runtime, copy, write_index, now())?;
    let written = to_console(addr, &folder, &files)?;
    let rescanned =
        ps5upload_core::smp_api::SmpApi::probe(addr).is_some_and(|(api, _)| api.rescan().is_ok());
    Ok(AmprEmuOutcome {
        kind: "backport",
        folder,
        written,
        rescanned,
    })
}

/// Writes files in the folder the game is in.
type Put = dyn Fn(&[(String, Vec<u8>)]) -> Result<Vec<String>, String>;

/// A folder on the console, or on this computer: in place.
fn attach_folder(
    source: &Path,
    put: &Put,
    folder: String,
    build: Option<&str>,
) -> Result<AmprEmuOutcome, String> {
    let mut tree = ps5upload_fpkg::source::open(source).map_err(|e| e.to_string())?;
    let module = needed(tree.as_mut())?;
    // A game in packs gets the emulator that reads them, whichever was asked for (#828).
    let build =
        crate::ampr_trace::emulator_for_packs(crate::remove_backport::packed(tree.as_mut()), build);
    let runtime = crate::ampr_trace::emulator(build)?;
    let write_index = !has_index(tree.as_ref());
    let files = writes(tree.as_mut(), &module, &runtime, None, write_index, now())?;
    Ok(AmprEmuOutcome {
        kind: "folder",
        folder,
        written: put(&files)?,
        rescanned: false,
    })
}

fn attach(source: &str, build: Option<&str>) -> Result<AmprEmuOutcome, String> {
    let path = resolve_engine_path(source);
    let image = crate::image_build::has_image_extension(&path);
    if source.starts_with("ps5://") {
        let (host, console_path) = crate::convert_source::console_target(source)?;
        if ps5upload_core::volumes::in_image_mount_root(&console_path) {
            return Err(
                "this folder is inside a mounted image, which is read-only: pick the image".into(),
            );
        }
        let addr = crate::console_addr(&host);
        if image {
            return attach_console_image(&addr, &path, build);
        }
        let put_addr = addr.clone();
        let base = console_path.clone();
        return attach_folder(
            &path,
            &move |files| to_console(&put_addr, &base, files),
            console_path,
            build,
        );
    }
    if ps5upload_fpkg::remote_source::is_remote(&path) {
        return Err(
            "this game is on a saved server: copy it to this computer or the PS5 first".into(),
        );
    }
    if image {
        return Err(
            "this image is on this computer: convert it again with Add the AMPR emulator on".into(),
        );
    }
    let base = path.clone();
    attach_folder(
        &path,
        &move |files| {
            let mut written = Vec::new();
            for (rel, bytes) in files {
                let to = base.join(rel);
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                std::fs::write(&to, bytes).map_err(|e| format!("{}: {e}", to.display()))?;
                written.push(rel.clone());
            }
            Ok(written)
        },
        path.to_string_lossy().into_owned(),
        build,
    )
}

/// POST /api/fpkg/ampr-emu `{source}`: the emulator added to a game where it stands.
pub(crate) async fn ampr_emu_handler(Json(req): Json<AmprEmuReq>) -> impl IntoResponse {
    let source = req.source.trim().to_string();
    let build = req.build;
    let done = tokio::task::spawn_blocking(move || attach(&source, build.as_deref()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match done {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct ZstdRuntimeReq {
    source: String,
}

/// What updating a zstd-packed game's runtime did.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct ZstdRuntimeOutcome {
    /// `folder`: replaced in the game's own folder. `backport`: a backport folder for an image.
    kind: &'static str,
    folder: String,
    written: Vec<String>,
    /// What the game had: `ampr-emu-ng 0.3.2`, `ampr_emu 0.4.2.1`, `unknown`, `none`.
    from: String,
    /// What it has now: `ampr-emu-ng 0.4.0`.
    to: String,
    /// It already had exactly this one: nothing was written.
    already: bool,
    rescanned: bool,
    /// The runtime Kraken Mount puts over every game it mounts, when it runs here with
    /// `inject_ampr` on: the game starts with that file, not the one written to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    overridden_by: Option<String>,
}

fn runtime_label(r: Option<&ps5upload_fpkg::ampr_pack::ng::ZstdRuntime>) -> String {
    match r {
        None => "none".into(),
        Some(r) if r.kind == "missing" => "none".into(),
        Some(r) => match &r.version {
            Some(v) => format!("{} {v}", r.kind),
            None => r.kind.to_string(),
        },
    }
}

/// The plan for one game: where the runtime goes, relative to the folder written to.
/// `existing`: what that place holds now (`None`: nothing). Returns the files to write, empty when
/// the place already holds `runtime`.
fn zstd_writes(
    tree: &mut dyn SourceTree,
    at: &str,
    runtime: &[u8],
    existing: Option<&[u8]>,
    copy_modules: Option<(&str, &str)>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if existing == Some(runtime) {
        return Ok(Vec::new());
    }
    // A packed game's ampr_emu.index lists its packed files too, so it is never rebuilt here.
    writes(tree, at, runtime, copy_modules, false, now())
}

/// A file on the console, `None` when there is none (an emulator is well under 64 MiB).
fn console_file(addr: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    // Only "not there" is None: a console that didn't answer is an error, not a missing file.
    let st = match ps5upload_core::fs_ops::fs_stat(addr, path) {
        Ok(st) => st,
        Err(e) if ps5upload_core::fs_ops::is_not_found(&format!("{e:#}")) => return Ok(None),
        Err(e) => return Err(format!("{path}: {e:#}")),
    };
    if st.kind != "file" || st.size > 64 << 20 {
        return Ok(None);
    }
    ps5upload_core::fs_ops::fs_read_whole(addr, path, st.size)
        .map(Some)
        .map_err(|e| format!("{path}: {e:#}"))
}

fn update_zstd_runtime(source: &str) -> Result<ZstdRuntimeOutcome, String> {
    use ps5upload_fpkg::ampr_pack::ng;
    let path = resolve_engine_path(source);
    let image = crate::image_build::has_image_extension(&path);
    if !source.starts_with("ps5://") && ps5upload_fpkg::remote_source::is_remote(&path) {
        return Err(
            "this game is on a saved server: copy it to this computer or the PS5 first".into(),
        );
    }
    if image && !source.starts_with("ps5://") {
        return Err(
            "this image is on this computer: convert it again with zstd packs, which puts the \
             newest runtime in"
                .into(),
        );
    }
    let mut tree = ps5upload_fpkg::source::open(&path).map_err(|e| e.to_string())?;
    let current = ng::zstd_runtime(tree.as_mut())
        .ok_or_else(|| "this game isn't in zstd packs (no ampr_zstd.index)".to_string())?;
    let from = runtime_label(Some(&current));
    let r = crate::zstd_runtime::pack_runtime(crate::zstd_runtime::RuntimeChoice::Latest)?;
    let to = format!("{} {}", r.name, r.version);
    crate::log_info!(
        "zstd runtime update for {source}: {from} -> {}",
        r.describe()
    );
    let module = current
        .module
        .clone()
        .unwrap_or_else(|| ps5upload_fpkg::ampr_pack::image::RUNTIME_MODULE.to_string());

    if source.starts_with("ps5://") {
        let (host, console_path) = crate::convert_source::console_target(source)?;
        if !image && ps5upload_core::volumes::in_image_mount_root(&console_path) {
            return Err(
                "this folder is inside a mounted image, which is read-only: pick the image".into(),
            );
        }
        let addr = crate::console_addr(&host);
        let kraken = ps5upload_core::kraken::KrakenApi::probe(&addr).is_some();
        if image && kraken {
            // Kraken Mount mounts packages and folders, never images, and applies backports to
            // folder games only (quer3q/kraken-mount 0.2.0 src/scan.rs:175-197, src/main.rs:64-69).
            return Err(
                "Kraken Mount is running on this PS5, and it does not mount game images: a \
                 runtime put beside this image would not be used"
                    .into(),
            );
        }
        let kraken_override = if kraken {
            kraken_override(&addr)?
        } else {
            None
        };
        if image {
            // Images are read-only: a ShadowMount+ backport, which it loads ahead of the
            // game's own fakelib (backport fakelib2, backport fakelib, then the game's).
            let id = title_id(tree.as_mut())?;
            let exists = |p: &str| ps5upload_core::fs_ops::fs_exists(&addr, p).unwrap_or(false);
            let folder = backport_folder(&console_path, &id, &exists);
            let existing_dir = ["fakelib2", "fakelib"]
                .into_iter()
                .find(|d| exists(&format!("{folder}/{d}")));
            let own_dir = parent_of(&module).to_string();
            let target = existing_dir.unwrap_or(&own_dir).to_string();
            let at = format!("{target}/{MODULE}");
            let full = format!("{folder}/{at}");
            let have = console_file(&addr, &full)?;
            // No backport runtime yet and the image's own is already this one: nothing to do (a
            // backport would only copy the game's modules for the same runtime).
            if have.is_none() && tree.read(&module).ok().as_deref() == Some(r.bytes.as_slice()) {
                return Ok(ZstdRuntimeOutcome {
                    kind: "backport",
                    folder,
                    written: Vec::new(),
                    from,
                    to,
                    already: true,
                    rescanned: false,
                    overridden_by: None,
                });
            }
            let copy = existing_dir
                .is_none()
                .then_some((own_dir.as_str(), target.as_str()));
            let files = zstd_writes(tree.as_mut(), &at, &r.bytes, have.as_deref(), copy)?;
            let already = files.is_empty();
            let written = to_console(&addr, &folder, &files)?;
            let rescanned = !already
                && ps5upload_core::smp_api::SmpApi::probe(&addr)
                    .is_some_and(|(api, _)| api.rescan().is_ok());
            return Ok(ZstdRuntimeOutcome {
                kind: "backport",
                folder,
                written,
                from,
                to,
                already,
                rescanned,
                overridden_by: None,
            });
        }
        let have = console_file(&addr, &format!("{console_path}/{module}"))?;
        let files = zstd_writes(tree.as_mut(), &module, &r.bytes, have.as_deref(), None)?;
        let already = files.is_empty();
        let written = to_console(&addr, &console_path, &files)?;
        return Ok(ZstdRuntimeOutcome {
            kind: "folder",
            folder: console_path,
            written,
            from,
            to,
            already,
            rescanned: false,
            overridden_by: kraken_override,
        });
    }

    let have = std::fs::read(path.join(&module)).ok();
    let files = zstd_writes(tree.as_mut(), &module, &r.bytes, have.as_deref(), None)?;
    let already = files.is_empty();
    let mut written = Vec::new();
    for (rel, bytes) in &files {
        let to = path.join(rel);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&to, bytes).map_err(|e| format!("{}: {e}", to.display()))?;
        written.push(rel.clone());
    }
    Ok(ZstdRuntimeOutcome {
        kind: "folder",
        folder: path.to_string_lossy().into_owned(),
        written,
        from,
        to,
        already,
        rescanned: false,
        overridden_by: None,
    })
}

/// POST /api/fpkg/zstd-runtime `{source}`: a game in zstd packs gets the newest ampr-emu-ng
/// (checked against GitHub's digest; the tested release when it can't be). A folder, here or on
/// the PS5, in place; an image on the PS5 through a ShadowMount+ backport.
pub(crate) async fn zstd_runtime_handler(Json(req): Json<ZstdRuntimeReq>) -> impl IntoResponse {
    let source = req.source.trim().to_string();
    let done = tokio::task::spawn_blocking(move || update_zstd_runtime(&source))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match done {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// ShadowMount+'s emulator updates, as its config.ini sets them: whether they are on (they are
/// unless switched off) and the folder they come from.
fn smp_emulator_updates(ini: &str) -> (bool, String) {
    let mut enabled = true;
    let mut folder = "/data/shadowmount/emus".to_string();
    let mut section = String::new();
    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        // Version 2 keeps them under [emulators]; version 1 had them flat.
        let (is_switch, is_path) = match section.as_str() {
            "emulators" => (key == "update_enabled", key == "path"),
            "" | "general" | "config" => (key == "update_emulators", key == "emulators_path"),
            _ => (false, false),
        };
        if is_switch {
            match value.to_ascii_lowercase().as_str() {
                "off" | "0" | "false" | "no" => enabled = false,
                "on" | "1" | "true" | "yes" => enabled = true,
                _ => {}
            }
        } else if is_path && value.starts_with('/') {
            folder = value.trim_end_matches('/').to_string();
        }
    }
    (enabled, folder)
}

/// What ShadowMount+ does to a zstd-packed game's runtime on this console.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct SmpEmulatorCheck {
    /// ShadowMount+'s emulator updates are on.
    updates_on: bool,
    /// The `libSceAmpr.sprx` it puts into every game in its place, when there is one.
    path: Option<String>,
    /// What that file is (as a game's runtime: `ampr-emu-ng`, `ampr_emu`, `unknown`).
    kind: Option<&'static str>,
    version: Option<String>,
    /// Games in zstd packs get that file instead of their own, and it won't start them, or may
    /// hang them (drakmor's ampr_emu, or ampr-emu-ng older than 0.3.4).
    breaks_zstd: bool,
    /// Whose folder this is: `shadowmount` (its emulator updates) or `kraken` (Kraken Mount's
    /// `ampr/` folder, copied over every game's fakelib while `inject_ampr` is on).
    mounter: &'static str,
}

/// Kraken Mount's runtime swap, from its `config.toml` (`None`: it has none) and the file in its
/// `ampr/` folder (`None`: no file). With `inject_ampr` on (the default) it copies every file of
/// `/data/krakenmount/ampr/` over each game's fakelib at launch (quer3q/kraken-mount 0.2.0
/// src/cache.rs:531-571), so that file is the runtime every game it mounts runs with, whatever the
/// game carries itself.
fn kraken_emulator_check(config: Option<&str>, file: Option<&[u8]>) -> SmpEmulatorCheck {
    let settings = ps5upload_core::kraken::parse_config(config.unwrap_or(""));
    let updates_on = settings.inject_ampr;
    let Some(bytes) = file else {
        return SmpEmulatorCheck {
            updates_on,
            path: None,
            kind: None,
            version: None,
            breaks_zstd: false,
            mounter: "kraken",
        };
    };
    let (kind, version, outdated) = ps5upload_fpkg::ampr_pack::ng::classify(bytes);
    SmpEmulatorCheck {
        updates_on,
        path: Some(ps5upload_core::kraken::AMPR_FILE.to_string()),
        kind: Some(kind),
        version,
        breaks_zstd: updates_on && outdated,
        mounter: "kraken",
    }
}

/// The file Kraken Mount puts over every game's runtime on this console, when it does (its
/// `inject_ampr` on and a `libSceAmpr.sprx` in its `ampr/` folder).
fn kraken_override(addr: &str) -> Result<Option<String>, String> {
    let config = console_file(addr, ps5upload_core::kraken::CONFIG_PATH)?
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    let file = console_file(addr, ps5upload_core::kraken::AMPR_FILE)?;
    let check = kraken_emulator_check(config.as_deref(), file.as_deref());
    Ok(check.path.filter(|_| check.updates_on))
}

fn smp_emulator_check(addr: &str) -> Result<SmpEmulatorCheck, String> {
    // Kraken Mount's folder when Kraken Mount runs: its API answering is the proof, and then
    // ShadowMount+ does not (see ps5upload_core::kraken).
    if ps5upload_core::kraken::KrakenApi::probe(addr).is_some() {
        let config = console_file(addr, ps5upload_core::kraken::CONFIG_PATH)?
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let file = console_file(addr, ps5upload_core::kraken::AMPR_FILE)?;
        return Ok(kraken_emulator_check(config.as_deref(), file.as_deref()));
    }
    let ini = console_file(addr, "/data/shadowmount/config.ini")?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let (updates_on, folder) = smp_emulator_updates(&ini);
    let path = format!("{folder}/{MODULE}");
    let Some(bytes) = console_file(addr, &path)? else {
        return Ok(SmpEmulatorCheck {
            updates_on,
            path: None,
            kind: None,
            version: None,
            breaks_zstd: false,
            mounter: "shadowmount",
        });
    };
    let (kind, version, outdated) = ps5upload_fpkg::ampr_pack::ng::classify(&bytes);
    Ok(SmpEmulatorCheck {
        updates_on,
        path: Some(path),
        kind: Some(kind),
        version,
        breaks_zstd: updates_on && outdated,
        mounter: "shadowmount",
    })
}

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    addr: String,
}

/// GET /api/ps5/smp-emulator?addr=: whether ShadowMount+ swaps zstd-packed games' runtime for
/// one in its emulator folder that won't run them.
pub(crate) async fn smp_emulator_handler(
    axum::extract::Query(q): axum::extract::Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = crate::console_addr(q.addr.trim());
    let done = tokio::task::spawn_blocking(move || smp_emulator_check(&addr))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match done {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Files(Vec<SourceFile>);
    impl SourceTree for Files {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            Ok(
                if path == "eboot.bin" && self.0.iter().any(|f| f.path == path) {
                    b"..libSceAmpr.prx..".to_vec()
                } else {
                    path.as_bytes().to_vec()
                },
            )
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    fn game(paths: &[&str]) -> Files {
        Files(
            paths
                .iter()
                .map(|p| SourceFile {
                    path: (*p).to_string(),
                    size: 18,
                })
                .collect(),
        )
    }

    fn names(w: &[(String, Vec<u8>)]) -> Vec<&str> {
        w.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn a_backport_goes_where_shadowmount_looks_first() {
        let id = "PPSA00000";
        let none = |_: &str| false;
        assert_eq!(
            backport_folder("/data/homebrew/a.ffpfsc", id, &none),
            "/data/homebrew/backports/PPSA00000"
        );
        assert_eq!(
            backport_folder("/mnt/ext0/games/a.ffpfsc", id, &none),
            "/data/homebrew/backports/PPSA00000",
            "no folder yet: the default root"
        );
        let beside = |p: &str| p == "/mnt/ext0/games/backports/PPSA00000";
        assert_eq!(
            backport_folder("/mnt/ext0/games/a.ffpfsc", id, &beside),
            "/mnt/ext0/games/backports/PPSA00000"
        );
        let above = |p: &str| p == "/mnt/ext0/backports/PPSA00000";
        assert_eq!(
            backport_folder("/mnt/ext0/games/a.ffpfsc", id, &above),
            "/mnt/ext0/backports/PPSA00000"
        );
    }

    #[test]
    fn a_fresh_backport_brings_the_games_modules_and_ends_with_the_emulator() {
        let mut g = game(&[
            "eboot.bin",
            "fakelib/libA.sprx",
            "fakelib/libkernel.sprx",
            "fakelib/._libA.sprx",
            "fakelib/sub/libB.sprx",
        ]);
        let w = writes(
            &mut g,
            "fakelib/libSceAmpr.sprx",
            b"EMU",
            Some(("fakelib", "fakelib")),
            true,
            7,
        )
        .unwrap();
        assert_eq!(
            names(&w),
            ["fakelib/libA.sprx", INDEX, "fakelib/libSceAmpr.sprx"]
        );
        assert_eq!(w[0].1, b"fakelib/libA.sprx");
        assert_eq!(w[2].1, b"EMU");
    }

    #[test]
    fn a_folder_gets_only_what_it_lacks() {
        let mut g = game(&["eboot.bin", INDEX]);
        let w = writes(&mut g, "fakelib/libSceAmpr.sprx", b"E", None, false, 7).unwrap();
        assert_eq!(names(&w), ["fakelib/libSceAmpr.sprx"]);
    }

    #[test]
    fn a_game_that_cannot_take_one_says_why() {
        let mut has = game(&["eboot.bin", "fakelib/libSceAmpr.sprx"]);
        assert!(needed(&mut has).unwrap_err().contains("already"));
        let mut none = game(&["other.bin"]);
        assert!(needed(&mut none).unwrap_err().contains("needs no emulator"));
    }

    #[test]
    fn shadowmounts_emulator_updates_are_on_unless_switched_off() {
        assert_eq!(
            smp_emulator_updates(""),
            (true, "/data/shadowmount/emus".to_string())
        );
        let v2 = "[config]\nversion = 2\n[emulators]\n# update_enabled = off\nupdate_enabled = off\npath = /data/my/emus/\n";
        assert_eq!(
            smp_emulator_updates(v2),
            (false, "/data/my/emus".to_string())
        );
        let v1 = "update_emulators=0\nemulators_path=/data/e\n";
        assert_eq!(smp_emulator_updates(v1), (false, "/data/e".to_string()));
        // Another section's path is not the emulators'.
        let smb = "[smb:NAS]\npath = /x\n";
        assert_eq!(
            smp_emulator_updates(smb),
            (true, "/data/shadowmount/emus".to_string())
        );
    }

    #[test]
    fn kraken_mounts_runtime_folder_is_what_its_games_run_with() {
        // No file in its folder: nothing is swapped in.
        let none = kraken_emulator_check(None, None);
        assert!(none.updates_on && none.path.is_none() && !none.breaks_zstd);
        assert_eq!(none.mounter, "kraken");
        // A file nobody knows by its digest is never called outdated.
        let own = kraken_emulator_check(None, Some(b"not a known runtime"));
        assert_eq!(own.path.as_deref(), Some(ps5upload_core::kraken::AMPR_FILE));
        assert_eq!(own.kind, Some("unknown"));
        assert!(!own.breaks_zstd);
        // Switched off in its config.toml: the games keep their own runtime.
        let off = kraken_emulator_check(Some("inject_ampr = false\n"), Some(b"x"));
        assert!(!off.updates_on && !off.breaks_zstd);
    }

    #[test]
    fn a_runtime_already_in_place_is_not_written_again() {
        let mut g = game(&["eboot.bin", "fakelib/libSceAmpr.sprx", "fakelib/libc.prx"]);
        let none =
            zstd_writes(&mut g, "fakelib/libSceAmpr.sprx", b"NG", Some(b"NG"), None).unwrap();
        assert!(none.is_empty());
        let one =
            zstd_writes(&mut g, "fakelib/libSceAmpr.sprx", b"NG", Some(b"OLD"), None).unwrap();
        assert_eq!(
            one,
            vec![("fakelib/libSceAmpr.sprx".to_string(), b"NG".to_vec())]
        );
        // A fresh backport takes the game's other modules, the runtime last, and no index.
        let bp = zstd_writes(
            &mut g,
            "fakelib/libSceAmpr.sprx",
            b"NG",
            None,
            Some(("fakelib", "fakelib")),
        )
        .unwrap();
        let names: Vec<&str> = bp.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["fakelib/libc.prx", "fakelib/libSceAmpr.sprx"]);
    }
}
