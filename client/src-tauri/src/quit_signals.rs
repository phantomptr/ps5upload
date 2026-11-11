//! Linux: quit cleanly on SIGTERM, SIGINT and SIGHUP.
//!
//! Left to the default action, SIGTERM ended this process at once and its
//! WebKitWebProcess then crashed (Ubuntu showed a crash report every time),
//! and SIGINT did nothing when the app was started from a shell in the
//! background (the shell sets it to ignored). Handled here, each goes through
//! Tauri's normal exit — the same as Quit in the tray — so the webview shuts
//! down, the engine is stopped and the exit clean-up runs. A second signal
//! while that runs ends the process at once.

use tauri::{AppHandle, Runtime};
use tokio::signal::unix::{signal, SignalKind};

pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            quiet_line("[tauri] cannot handle quit signals");
            return;
        };
        let mut asked = false;
        loop {
            let name = tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
                _ = hup.recv() => "SIGHUP",
            };
            if asked {
                quiet_line(&format!("[tauri] {name} again: exiting now"));
                std::process::exit(1);
            }
            asked = true;
            quiet_line(&format!("[tauri] {name}: quitting"));
            app.exit(0);
        }
    });
}

/// SIGHUP is what arrives when the terminal that started the app closes, and stderr is that
/// terminal: `eprintln!` panics on it, and the task died before `app.exit` ran.
fn quiet_line(text: &str) {
    let _ = crate::crash_hook::stderr_line(text);
}

#[cfg(test)]
mod tests {
    #[test]
    fn nothing_here_prints_with_a_macro_that_panics_on_a_closed_stderr() {
        let source = include_str!("quit_signals.rs");
        assert!(!source.contains(concat!("eprint", "ln!(")));
        assert!(!source.contains(concat!("print", "ln!(")));
        // Writing to stderr through the helper never panics.
        super::quiet_line("[tauri] test line");
    }
}
