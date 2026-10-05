//! One-time clean-up for an upgrade to 6.0 (AVA1). Nothing the user set is reset: settings,
//! queue, playlists, history, telemetry, cheats and backups stay where they are. Only state no
//! 6.x code reads is touched, and files that might still mean something to the user are moved
//! aside into `legacy-5x/`, never deleted.
//!
//! Host side (`run_host`, once per data dir): pre-5.x config files that neither 5.41 nor 6.0
//! reads move to `<data_dir>/legacy-5x/`. Console side (`clean_console`, once per console, on
//! the first answer from a 6.0 helper): the old transfer records and staging folders, which
//! the AVA1 helper never reads, are deleted.

use std::path::Path;

/// The marker directory: one file per finished step, so a step that failed half-way runs again.
const MARKERS: &str = "migrations";
const HOST_DONE: &str = "6.0.0-host";
const CONSOLES_DONE: &str = "6.0.0-consoles";

/// Read by no 5.41 or 6.0 code (pre-5.x builds wrote them). Moved, not deleted: they may hold
/// an old address or profile the user wants to look at.
const HOST_LEGACY_FILES: [&str; 5] = [
    "api_token",
    "ps5upload.ini",
    "ps5upload_profiles.ini",
    "ps5upload_profiles.json",
    "app-config.json",
];

/// The pre-6.0 helper's transfer records and staging area. The AVA1 helper keeps its state under
/// `/data/ps5upload/ava`; nothing reads these any more.
pub const CONSOLE_LEGACY_DIRS: [&str; 2] = ["/data/ps5upload/tx", "/data/ps5upload/spool"];

/// Moves the host's legacy files aside, once. Returns what was moved (for the log).
pub fn run_host(data_dir: &Path) -> Vec<String> {
    let markers = data_dir.join(MARKERS);
    if markers.join(HOST_DONE).exists() {
        return Vec::new();
    }
    let legacy = data_dir.join("legacy-5x");
    let mut moved = Vec::new();
    let mut ok = true;
    for name in HOST_LEGACY_FILES {
        let from = data_dir.join(name);
        if !from.exists() {
            continue;
        }
        // Same directory tree, same device: a rename never crosses a mount here.
        let to = legacy.join(name);
        match std::fs::create_dir_all(&legacy).and_then(|_| std::fs::rename(&from, &to)) {
            Ok(()) => moved.push(name.to_string()),
            Err(_) => ok = false,
        }
    }
    if ok && std::fs::create_dir_all(&markers).is_ok() {
        let _ = std::fs::write(markers.join(HOST_DONE), b"");
    }
    moved
}

/// Whether `console` (its host) still needs the console-side clean-up.
pub fn console_pending(data_dir: &Path, console: &str) -> bool {
    let done =
        std::fs::read_to_string(data_dir.join(MARKERS).join(CONSOLES_DONE)).unwrap_or_default();
    !done.lines().any(|l| l.trim() == console)
}

/// Records that `console` is cleaned.
pub fn mark_console_done(data_dir: &Path, console: &str) {
    let markers = data_dir.join(MARKERS);
    if std::fs::create_dir_all(&markers).is_err() {
        return;
    }
    let path = markers.join(CONSOLES_DONE);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == console) {
        return;
    }
    text.push_str(console);
    text.push('\n');
    let _ = std::fs::write(path, text);
}

/// A delete that failed only because the folder is not there counts as done.
pub fn is_not_found(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("errno_2") || e.contains("no such file") || e.contains("not found")
}

/// Deletes the pre-6.0 helper's folders on the console through `delete` (the engine passes the management
/// call). True when every folder is gone, so the console is marked and never asked again.
pub fn clean_console(delete: impl Fn(&str) -> Result<(), String>) -> bool {
    let mut all = true;
    for dir in CONSOLE_LEGACY_DIRS {
        match delete(dir) {
            Ok(()) => {}
            Err(e) if is_not_found(&e) => {}
            Err(_) => all = false,
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("migrate6-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn legacy_files_move_aside_and_settings_stay() {
        let d = temp("host");
        std::fs::write(d.join("ps5upload.ini"), b"old").unwrap();
        std::fs::write(d.join("api_token"), b"tok").unwrap();
        // Current state: never touched.
        std::fs::write(d.join("settings.json"), b"{\"keep\":1}").unwrap();
        std::fs::create_dir_all(d.join("ava")).unwrap();
        std::fs::write(d.join("ava/identity"), b"id").unwrap();

        let mut moved = run_host(&d);
        moved.sort();
        assert_eq!(moved, ["api_token", "ps5upload.ini"]);
        assert_eq!(
            std::fs::read(d.join("legacy-5x/ps5upload.ini")).unwrap(),
            b"old"
        );
        assert!(!d.join("ps5upload.ini").exists());
        assert_eq!(
            std::fs::read(d.join("settings.json")).unwrap(),
            b"{\"keep\":1}"
        );
        assert_eq!(std::fs::read(d.join("ava/identity")).unwrap(), b"id");

        // Once only: a file that appears later is left alone.
        std::fs::write(d.join("app-config.json"), b"new").unwrap();
        assert!(run_host(&d).is_empty());
        assert!(d.join("app-config.json").exists());
    }

    #[test]
    fn a_fresh_install_has_nothing_to_move_and_is_marked() {
        let d = temp("fresh");
        assert!(run_host(&d).is_empty());
        assert!(d.join(MARKERS).join(HOST_DONE).exists());
    }

    #[test]
    fn a_console_is_cleaned_once_and_a_missing_folder_counts_as_done() {
        let d = temp("console");
        assert!(console_pending(&d, "192.168.1.9"));
        let asked = Mutex::new(Vec::new());
        let done = clean_console(|p| {
            asked.lock().unwrap().push(p.to_string());
            if p.ends_with("/spool") {
                Err("fs_delete_errno_2".into())
            } else {
                Ok(())
            }
        });
        assert!(done);
        assert_eq!(*asked.lock().unwrap(), CONSOLE_LEGACY_DIRS);
        mark_console_done(&d, "192.168.1.9");
        mark_console_done(&d, "192.168.1.9");
        assert!(!console_pending(&d, "192.168.1.9"));
        assert!(console_pending(&d, "192.168.1.10"));
        let text = std::fs::read_to_string(d.join(MARKERS).join(CONSOLES_DONE)).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn a_failed_delete_leaves_the_console_pending() {
        assert!(!clean_console(|_| Err("connection refused".into())));
    }
}
