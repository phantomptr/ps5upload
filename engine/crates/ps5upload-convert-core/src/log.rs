//! Where the build code's log lines go. This crate has no log of its own: the engine installs a
//! sink that writes into its log ring at its level settings, the console program one that
//! writes its log file. With none installed, info and above go to stderr.

use std::sync::OnceLock;

/// The sink: whether a line at a level in a category is wanted (asked before it is formatted),
/// and the line itself. Levels are the names PS5Upload's logs use: `error`, `warn`, `info`,
/// `debug`, `trace`.
pub struct Sink {
    pub enabled: fn(level: &'static str, category: &'static str) -> bool,
    pub record: fn(level: &'static str, category: &'static str, line: String),
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// The category of every line this crate writes.
pub const CONVERT: &str = "convert";
/// The category of [`record`]'s lines.
pub const GENERAL: &str = "general";

/// Installs the sink. The first call wins.
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

fn rank(level: &str) -> u8 {
    match level {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        _ => 4,
    }
}

/// Whether a line at `level` in `category` is wanted now.
pub fn enabled(level: &'static str, category: &'static str) -> bool {
    match SINK.get() {
        Some(s) => (s.enabled)(level, category),
        None => rank(level) <= rank("info"),
    }
}

/// A line in `category`, whatever the level settings say (the caller asked [`enabled`]).
pub fn record_cat(level: &'static str, category: &'static str, line: String) {
    match SINK.get() {
        Some(s) => (s.record)(level, category, line),
        None => {
            use std::io::Write;
            // Never `eprintln!`: it panics when stderr is gone, and a log line is not worth a job.
            let _ = writeln!(std::io::stderr(), "[{level}] {category}: {line}");
        }
    }
}

/// A line in the general category, always written (the engine's `engine_log::record`).
pub fn record(level: &'static str, line: String) {
    record_cat(level, GENERAL, line)
}

/// A line at a level in a category, formatted only when it is wanted:
/// `log_at!(Info, "convert", "packed {n} files")`.
#[macro_export]
macro_rules! log_at {
    ($lv:ident, $cat:expr, $($arg:tt)*) => {{
        let level = $crate::log::level_name!($lv);
        if $crate::log::enabled(level, $cat) {
            $crate::log::record_cat(level, $cat, format!($($arg)*))
        }
    }};
}

/// Info in the general category.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log_at!(Info, $crate::log::GENERAL, $($arg)*) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __level_name {
    (Error) => {
        "error"
    };
    (Warn) => {
        "warn"
    };
    (Info) => {
        "info"
    };
    (Debug) => {
        "debug"
    };
    (Trace) => {
        "trace"
    };
}
#[doc(hidden)]
pub use crate::__level_name as level_name;
