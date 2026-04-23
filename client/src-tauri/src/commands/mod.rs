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

pub mod ps5_engine;
pub mod persistence;
pub mod app_info;
pub mod probes;
pub mod folder_inspect;
pub mod keep_awake;
pub mod user_config;
pub mod companions;
pub mod updates;

// Glob imports pull in both the command fn and the `#[tauri::command]`-
// generated `__cmd__<name>` helper macros need to find via `commands::*`.
// Explicit lists break `generate_handler!`.
pub use ps5_engine::*;
pub use persistence::*;
pub use app_info::*;
pub use probes::*;
pub use folder_inspect::*;
pub use keep_awake::*;
pub use user_config::*;
pub use companions::*;
pub use updates::*;
