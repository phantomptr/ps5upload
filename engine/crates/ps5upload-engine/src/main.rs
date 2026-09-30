// Release-mode on Windows: no console window. The engine is a daemon
// spawned as a child of the Tauri desktop exe, which pipes the
// engine's stdout/stderr back via `pipe_tagged`. Without this
// attribute Windows allocates a fresh console for the engine every
// time the desktop app starts it, flashing a terminal window next to
// the UI. Debug builds keep the default (console) subsystem so
// `cargo run -p ps5upload-engine` still shows log output in the
// terminal for local diagnostics.
//
// The engine's actual implementation lives in the library crate
// (`lib.rs`) so it can ALSO be linked in-process by the Tauri mobile
// build (Android/iOS have no sidecar-binary spawn model). This binary
// is just the desktop CLI entry point.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[tokio::main]
async fn main() {
    // `--healthcheck` probes a running engine and exits; the container images
    // have no shell or HTTP client to do it with (see `healthcheck`).
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        std::process::exit(ps5upload_engine::healthcheck());
    }
    ps5upload_engine::run_cli().await;
}
