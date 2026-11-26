//! The name a fake package gives its game (#937): `GET /api/pkg/name` reads it and says whether
//! it can be changed, `POST /api/pkg/name` changes it in the package file on this computer.
//!
//! The package is changed in place (`ps5upload_fpkg::pkg_name` plans a few KiB of writes that
//! keep every offset and the file's length), never copied: a 100 GB package is renamed in the
//! time it takes to read its container. What keeps the user's file safe is what an image edit
//! has (`image_edit_in_place::patch_checked`): the file is claimed (no build, upload or edit of
//! it meanwhile, and no install served from it), the bytes it overwrites go to an undo journal
//! beside it first, the result is read back from disk and checked, and anything short of that
//! puts the file back as it was. A journal a crash left is settled the next time the package is
//! opened here, by these routes or by an install.

use std::path::{Component, Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5_dump_forge_patch::{Data, Write};
use ps5upload_fpkg::pkg_name;
use serde::{Deserialize, Serialize};

use crate::image_edit_in_place::{self, Refusal};
use crate::json_err;
use crate::pkg_install::{PkgInstallState, PkgInstallStateHandle};

#[derive(Debug, Deserialize)]
pub(crate) struct NameQuery {
    /// The package, on the engine's computer.
    path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RenameRequest {
    /// The package, on the engine's computer.
    path: String,
    /// The new name: at most 127 bytes of UTF-8, no line breaks.
    name: String,
    /// Give every language this name. Otherwise only the entries that hold the package's
    /// current default name change, and a language with its own name keeps it.
    #[serde(default)]
    all_languages: bool,
}

/// What `GET /api/pkg/name` answers.
#[derive(Debug, Serialize)]
pub(crate) struct NameResponse {
    #[serde(flatten)]
    names: pkg_name::Names,
    /// The longest name accepted, in UTF-8 bytes.
    max_bytes: usize,
}

/// What `POST /api/pkg/name` answers.
#[derive(Debug, Serialize)]
pub(crate) struct RenameResponse {
    platform: pkg_name::Platform,
    /// The name it had.
    previous: String,
    /// The name it has now.
    name: String,
    /// How many of its name entries changed (0: it already had this name).
    changed: usize,
    /// Bytes written into the package.
    written_bytes: u64,
    /// Set when the change is in the package and checked, but the removal of its undo journal
    /// could not be confirmed saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

type Failure = (StatusCode, String);

fn bad(why: impl Into<String>) -> Failure {
    (StatusCode::BAD_REQUEST, why.into())
}

/// The package a request names, as a path on this computer: an absolute path (or one under the
/// home folder) to a `.pkg` or `.fpkg` file, with no `..` in it. A package on a server or a
/// console is refused: it is renamed where it is a file.
fn local_package(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("no package was named".into());
    }
    if crate::remote::path::is_remote(raw) || raw.starts_with("ps5://") {
        return Err(
            "a package's name is changed in the file itself, so the package has to be on this \
             computer: one on a server or on the PS5 can't be changed from here"
                .into(),
        );
    }
    let path = crate::fpkg_api::resolve_engine_path(raw);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("the package's path must not hold \"..\"".into());
    }
    let is_package = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pkg") || e.eq_ignore_ascii_case("fpkg"));
    if !is_package {
        return Err("only a .pkg or .fpkg file can be renamed".into());
    }
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() => Ok(path),
        Ok(_) => Err(format!("{} is not a file", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Refuses a package split into parts (`game.pkg`, `game.pkg.0`…): what covers its name is
/// spread over its first and last part.
fn whole(path: &Path) -> Result<(), String> {
    match ps5upload_pkg::parse_split_pkg(path) {
        Ok(split) if split.parts.len() > 1 => Err(format!(
            "this package is split into {} parts; a split package can't be renamed here",
            split.parts.len()
        )),
        _ => Ok(()),
    }
}

/// `GET /api/pkg/name?path=`: the names a package on this computer holds (one per language)
/// and whether they can be changed. `room` is how long a new name may be in this package, in
/// UTF-8 bytes (`default_name`: changing the default name only; `all_languages`). `refusal` is set when they can't: `code` is `retail` for an
/// official package, `patch` for a delta update, `no_name`, or `unsupported` with the reason.
pub(crate) async fn get_handler(Query(q): Query<NameQuery>) -> Response {
    let read = tokio::task::spawn_blocking(move || -> Result<NameResponse, Failure> {
        let path = local_package(&q.path).map_err(bad)?;
        // A rename a crash interrupted is undone before the package is read; one that runs
        // now is waited out by the caller.
        image_edit_in_place::before_open(&path).map_err(|why| (StatusCode::CONFLICT, why))?;
        whole(&path).map_err(bad)?;
        let names = pkg_name::read(&path).map_err(|e| bad(e.to_string()))?;
        Ok(NameResponse {
            names,
            max_bytes: pkg_name::MAX_NAME_BYTES,
        })
    })
    .await;
    match read {
        Ok(Ok(names)) => Json(names).into_response(),
        Ok(Err((code, why))) => json_err(code, why).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("reading the package's name failed: {e}"),
        )
        .into_response(),
    }
}

/// `POST /api/pkg/name` `{path, name, all_languages}`: gives a fake package on this computer
/// another name, in place. 409 while the package is in use (an install of it has not finished,
/// however long ago the PS5 last read from it; an upload or a build reads it, or another program
/// has it open), with what holds it and how to free it; 400 with the reason when it can't be done (an
/// official package, a name that does not fit). On any failure the file is as it was.
pub(crate) async fn set_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<RenameRequest>,
) -> Response {
    let done = tokio::task::spawn_blocking(move || rename(&state, &req)).await;
    match done {
        Ok(Ok(done)) => Json(done).into_response(),
        Ok(Err((code, why))) => json_err(code, why).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the rename failed: {e}"),
        )
        .into_response(),
    }
}

fn rename(state: &PkgInstallState, req: &RenameRequest) -> Result<RenameResponse, Failure> {
    let path = local_package(&req.path).map_err(bad)?;
    whole(&path).map_err(bad)?;
    let name = pkg_name::clean_name(&req.name).map_err(|e| bad(e.to_string()))?;
    let renamed = image_edit_in_place::patch_checked(
        &path,
        || {
            // The file is claimed by now. An install that starts after this sees the claim;
            // one already being served from it is seen here.
            if let Some(why) = crate::pkg_install::serving(state, &path) {
                return Err(Refusal::busy(why));
            }
            let plan = pkg_name::plan(&path, &name, req.all_languages)
                .map_err(|e| Refusal::new(e.to_string()))?;
            let writes = plan
                .patches
                .iter()
                .map(|p| Write {
                    offset: p.offset,
                    data: Data::Bytes(p.data.clone()),
                    live: true,
                })
                .collect();
            Ok((writes, plan))
        },
        |plan| pkg_name::check(&path, plan).map_err(|e| e.to_string()),
    );
    let (plan, warning) = renamed.map_err(|r| {
        let code = if r.busy {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        (code, r.reason)
    })?;
    if plan.changed > 0 {
        crate::engine_log::record(
            "info",
            format!(
                "package name changed in place: {} ({} entries, {} bytes written)",
                path.display(),
                plan.changed,
                plan.write_bytes()
            ),
        );
    }
    Ok(RenameResponse {
        platform: plan.platform,
        written_bytes: plan.write_bytes(),
        changed: plan.changed,
        previous: plan.previous,
        name: plan.name,
        warning,
    })
}

#[cfg(test)]
mod tests;
