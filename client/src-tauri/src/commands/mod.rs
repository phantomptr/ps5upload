//! Tauri command modules. All `#[tauri::command]` fns are re-exported from
//! this module root so `lib.rs` can register them via `generate_handler!`
//! without needing to know which file they live in.
//!
//! Layout:
//!   ps5_engine  — HTTP proxies to the ps5upload-engine sidecar
//!   persistence — JSON file-backed send-payload history
//!   app_info    — FAQ + changelog bundled-markdown loaders
//!   probes      — payload send/check/probe, port probes
//!   folder_inspect — sce_sys / param.json walk for local folders
//!   keep_awake  — platform sleep inhibitor spawn/kill
//!   user_config — ~/.ps5upload/settings.json read/write
//!   companions  — TCP probe of known scene-tool ports

pub mod app_info;
pub mod companions;
pub mod folder_inspect;
pub mod keep_awake;
pub mod persistence;
pub mod probes;
pub mod ps5_engine;
pub mod updates;
pub mod user_config;

pub(crate) fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::rename(from, to) {
            Ok(()) => Ok(()),
            Err(first_err) if to.exists() => {
                std::fs::remove_file(to)?;
                std::fs::rename(from, to).map_err(|retry_err| {
                    std::io::Error::new(
                        retry_err.kind(),
                        format!(
                            "replace retry failed after removing existing destination: {retry_err}; initial rename failed: {first_err}"
                        ),
                    )
                })
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

// Glob imports pull in both the command fn and the `#[tauri::command]`-
// generated `__cmd__<name>` helper macros need to find via `commands::*`.
// Explicit lists break `generate_handler!`.
pub use app_info::*;
pub use companions::*;
pub use folder_inspect::*;
pub use keep_awake::*;
pub use persistence::*;
pub use probes::*;
pub use ps5_engine::*;
pub use updates::*;
pub use user_config::*;
