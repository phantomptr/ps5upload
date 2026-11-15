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
    // `mcp` bridges an MCP client's stdio to the running engine, and runs no engine of its own.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        let args = std::env::args().skip(2).collect();
        std::process::exit(ps5upload_engine::run_mcp_bridge(args));
    }
    // `inspect` and `convert` are the Convert screen for scripts: they run the conversion in this
    // process and exit, with no engine started and none needed.
    if let Some(command @ ("inspect" | "convert")) = std::env::args().nth(1).as_deref() {
        let args = std::env::args().skip(2).collect();
        std::process::exit(ps5upload_engine::run_convert_cli(command, args).await);
    }
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--help" | "-h" | "help")
    ) {
        // Not `println!`: it panics when stdout is closed (`| head`).
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{}", ps5upload_engine::CLI_USAGE);
        return;
    }
    ps5upload_engine::run_cli().await;
}
