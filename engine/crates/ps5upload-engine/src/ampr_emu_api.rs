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
}
