//! One-time clean-up of the app's own data folder for an upgrade to 6.0. The engine does the
//! same for `~/.ps5upload` (engine `migrate_6`). Nothing the user set is reset: the queue,
//! playlists, history and saved settings stay. Files no 5.41 or 6.0 code reads are moved
//! aside into `legacy-5x/`; only a cached helper binary nothing loads is deleted.

use std::path::Path;

const MARKER: &str = "migrations/6.0.0-app";

/// Written by pre-5.x desktop builds; read by no 5.41 or 6.0 code. Moved, not deleted.
const LEGACY_FILES: [&str; 2] = ["config.json", "profiles.json"];

/// Cached payload binaries no current code loads (rebuilt or re-extracted if ever needed).
const DEAD_CACHE: [&str; 2] = [
    "payload/ezremote-dpi.elf",
    "payload/ezremote-dpi.elf.gz.blake3",
];

/// Runs once per data folder; returns what it did, for the log.
pub fn run(data_dir: &Path) -> Vec<String> {
    let marker = data_dir.join(MARKER);
    if marker.exists() {
        return Vec::new();
    }
    let legacy = data_dir.join("legacy-5x");
    let mut did = Vec::new();
    let mut ok = true;
    for name in LEGACY_FILES {
        let from = data_dir.join(name);
        if !from.is_file() {
            continue;
        }
        // Same folder tree, same device: never a cross-mount rename.
        match std::fs::create_dir_all(&legacy)
            .and_then(|_| std::fs::rename(&from, legacy.join(name)))
        {
            Ok(()) => did.push(format!("moved {name}")),
            Err(_) => ok = false,
        }
    }
    for rel in DEAD_CACHE {
        let p = data_dir.join(rel);
        if p.is_file() {
            match std::fs::remove_file(&p) {
                Ok(()) => did.push(format!("deleted {rel}")),
                Err(_) => ok = false,
            }
        }
    }
    if ok {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, b"");
    }
    did
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("app-migrate6-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("payload")).unwrap();
        d
    }

    #[test]
    fn legacy_files_move_aside_dead_cache_goes_current_state_stays() {
        let d = temp("app");
        std::fs::write(d.join("config.json"), b"{\"address\":\"10.0.0.2\"}").unwrap();
        std::fs::write(d.join("payload/ezremote-dpi.elf"), b"elf").unwrap();
        // Current state: never touched.
        for keep in [
            "upload_queue.json",
            "payload_playlists.json",
            "send_payload_history.json",
        ] {
            std::fs::write(d.join(keep), b"[]").unwrap();
        }
        std::fs::write(d.join("payload/ps5upload.elf"), b"helper").unwrap();

        let did = run(&d);
        assert_eq!(
            did,
            ["moved config.json", "deleted payload/ezremote-dpi.elf"]
        );
        assert_eq!(
            std::fs::read(d.join("legacy-5x/config.json")).unwrap(),
            b"{\"address\":\"10.0.0.2\"}"
        );
        for keep in [
            "upload_queue.json",
            "payload_playlists.json",
            "send_payload_history.json",
        ] {
            assert!(d.join(keep).exists(), "{keep} must stay");
        }
        assert!(d.join("payload/ps5upload.elf").exists());

        // Once only.
        std::fs::write(d.join("profiles.json"), b"{}").unwrap();
        assert!(run(&d).is_empty());
        assert!(d.join("profiles.json").exists());
    }
}
