//! Payload Library commands: thin callers of `ps5upload_core::payload_library`, over the app's
//! own cache folder. The engine's `/api/payloads` routes call the same code over the same folder
//! (see `library_dir` and `PS5UPLOAD_PAYLOADS_DIR`).
//!
//! ps5upload never downloads a payload: these read the catalogue, release information and the
//! local copies users got earlier. Users get a payload from its project page and send the file.

use std::path::PathBuf;

use ps5upload_core::payload_library::{
    Library, LibraryError, LocalInventoryEntry, PayloadInfo, ReleaseInfo, SourceInfo,
};
use tauri::{AppHandle, Manager};

/// The folder of the library: `payloads` under the app's local data folder, where it has always
/// been, so an existing cache keeps working.
pub(crate) fn library_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    Ok(root.join("payloads"))
}

pub(crate) fn library(app: &AppHandle) -> Result<Library, String> {
    Ok(Library::new(library_dir(app)?))
}

/// Run a library call off the async runtime: it blocks on the network and the disk.
async fn on_library<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(Library) -> Result<T, LibraryError> + Send + 'static,
) -> Result<T, String> {
    let lib = library(app)?;
    tokio::task::spawn_blocking(move || f(lib))
        .await
        .map_err(|e| format!("payload library task: {e}"))?
        .map_err(|e| e.ui_text())
}

/// The full catalogue: curated entries first, then the user's own repos.
#[tauri::command]
pub async fn payloads_catalog(app: AppHandle) -> Result<Vec<PayloadInfo>, String> {
    on_library(&app, |lib| Ok(lib.catalog())).await
}

/// Remove a user's repo and its cached files. False when the id is not one.
#[tauri::command]
pub async fn payloads_remove_custom_repo(app: AppHandle, id: String) -> Result<bool, String> {
    on_library(&app, move |lib| lib.remove_custom_repo(&id)).await
}

/// The latest release of a payload, cached for an hour. A failed refresh returns the cache with
/// `refresh_error` set.
#[tauri::command]
pub async fn payloads_release(
    app: AppHandle,
    id: String,
    force_refresh: Option<bool>,
) -> Result<ReleaseInfo, String> {
    on_library(&app, move |lib| {
        lib.release(&id, force_refresh.unwrap_or(false))
    })
    .await
}

/// All releases of a payload, newest first, so a user can pin a version or go back to one.
#[tauri::command]
pub async fn payloads_releases(
    app: AppHandle,
    id: String,
    force_refresh: Option<bool>,
) -> Result<Vec<ReleaseInfo>, String> {
    on_library(&app, move |lib| {
        lib.releases(&id, force_refresh.unwrap_or(false))
    })
    .await
}

/// All locally cached payload binaries.
#[tauri::command]
pub async fn payloads_local_inventory(app: AppHandle) -> Result<Vec<LocalInventoryEntry>, String> {
    on_library(&app, |lib| Ok(lib.local_inventory())).await
}

/// The cached binary of a payload, if any. An id that isn't a known payload is `None` before
/// anything touches the file system.
#[tauri::command]
pub async fn payloads_local_path(app: AppHandle, id: String) -> Result<Option<String>, String> {
    on_library(&app, move |lib| {
        Ok(lib
            .local_path(&id)
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
}

/// The payload lists the user added (#829), from the copies on disk.
#[tauri::command]
pub async fn payloads_sources(app: AppHandle) -> Result<Vec<SourceInfo>, String> {
    on_library(&app, |lib| Ok(lib.sources())).await
}

/// Remove a payload list and the local copies of its payloads.
#[tauri::command]
pub async fn payloads_remove_source(app: AppHandle, id: String) -> Result<bool, String> {
    on_library(&app, move |lib| lib.remove_source(&id)).await
}
