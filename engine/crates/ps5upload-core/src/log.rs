//! Pluggable log sink for ps5upload-core.
//!
//! Core is a library crate, so it can't know anything about where logs
//! should land (stderr for the CLI, an in-memory ring for the engine
//! sidecar, a Tauri event for the desktop client). Each consumer
//! installs a sink via `set_sink` at startup; anything core emits via
//! the `core_log!` macro goes through it.
//!
//! Default sink is `eprintln!` so tests and standalone usage still
//! produce visible output without any setup.

use std::sync::OnceLock;

type Sink = Box<dyn Fn(&str) + Send + Sync + 'static>;

fn sink_cell() -> &'static OnceLock<Sink> {
    static CELL: OnceLock<Sink> = OnceLock::new();
    &CELL
}

/// Install a log sink. First call wins; subsequent calls are ignored —
/// callers that need to replace the sink should use a forwarding sink
/// of their own.
pub fn set_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = sink_cell().set(Box::new(f));
}

/// Emit a log line through the installed sink, or `eprintln!` if none
/// has been set yet. Safe to call before/without `set_sink`.
pub fn log(msg: &str) {
    match sink_cell().get() {
        Some(f) => f(msg),
        None => eprintln!("[core] {msg}"),
    }
}

/// `format!`-style shortcut. Use this in core modules that want to
/// surface diagnostic info to whatever frontend has attached.
#[macro_export]
macro_rules! core_log {
    ($($arg:tt)*) => { $crate::log::log(&format!($($arg)*)) };
}
