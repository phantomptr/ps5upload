//! Browse the engine's OWN local filesystem — the browser-mode counterpart
//! to the Tauri desktop app's native file/folder dialog.
//!
//! The Tauri app picks files with a native OS dialog, which returns a real
//! filesystem path on the SAME machine the app (and its bundled engine)
//! runs on. A browser client has no such dialog — there is no way for an
//! HTML `<input type="file">` to browse anything other than the browser's
//! own machine, which usually isn't where the engine runs (e.g. the engine
//! is in a Docker container, the browser is on another device entirely).
//!
//! So in browser mode the picker instead browses the *engine's* filesystem
//! directly — the same real-path browser built for Android's scoped-storage
//! problem (`client/src-tauri/src/commands/local_fs.rs`, `LocalPathPicker`
//! in the frontend), just pointed at these HTTP routes instead of Tauri
//! commands. A user who mounts files into the container (e.g. `docker run
//! -v /host/games:/data/games ...`) can browse to `/data/games` here and
//! transfer from there — `POST /api/transfer/file`/`/dir` already read an
//! arbitrary caller-supplied local path, so this listing route doesn't
//! expand what the engine can already be asked to read; it only makes that
//! existing capability browsable instead of requiring the exact path to be
//! known in advance.
//!
//! Ported from the non-Android branch of the Tauri command file above,
//! which has zero Tauri-specific APIs to begin with.

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Serialize)]
pub struct LocalEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// List a real local directory: directories first, then files, each
/// alphabetical (case-insensitive). Unreadable entries are skipped rather
/// than failing the whole listing.
pub fn list_dir(path: &str) -> Result<Vec<LocalEntry>> {
    let rd = std::fs::read_dir(path).with_context(|| format!("read_dir {path}"))?;
    let mut out: Vec<LocalEntry> = Vec::new();
    for ent in rd.flatten() {
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(LocalEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            path: ent.path().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: if md.is_file() { md.len() } else { 0 },
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

/// Entries of a comma-separated `PS5UPLOAD_BROWSE_ROOTS` that are usable as a
/// browse root: trimmed, blanks dropped, and a directory the engine can see.
fn parse_browse_roots(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && std::path::Path::new(s).is_dir())
        .map(str::to_string)
        .collect()
}

/// The `PS5UPLOAD_BROWSE_ROOTS` entries that are not blank and are not a
/// directory the engine can see. Dropping them silently would make a typo, or
/// a volume that was never actually mounted, look exactly like "the setting is
/// ignored", which is the trap `invalid_allow_ip_entries` already exists to
/// avoid.
fn invalid_browse_root_entries(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !std::path::Path::new(s).is_dir())
        .map(str::to_string)
        .collect()
}

/// Roots to seed the in-app browser with: `PS5UPLOAD_BROWSE_ROOTS` when it
/// names at least one directory, else the engine's own home dir (falling back
/// to "/"), which is what this always used to return. Mirrors the desktop
/// branch of the Tauri command; there's no Android-style removable-volume
/// enumeration to do here.
///
/// Home alone is the wrong default in a container: the image sets `HOME=/data`
/// for the engine's own state while the operator mounts packages elsewhere
/// (`engine/compose.yaml` says `/pkgs`), so the picker opened on the state
/// volume every time with no way to change it. Moving `HOME` is not the
/// answer, because `fpkg_api::default_output_dir` writes conversions under
/// `$HOME/Downloads/fpkgs` and a package mount is typically read-only. So the
/// browse root gets its own variable, like the other directory overrides.
pub fn storage_roots() -> Vec<String> {
    let raw = std::env::var("PS5UPLOAD_BROWSE_ROOTS").unwrap_or_default();
    if !raw.trim().is_empty() {
        let invalid = invalid_browse_root_entries(&raw);
        if !invalid.is_empty() {
            eprintln!(
                "[ps5upload-engine] WARNING: ignoring PS5UPLOAD_BROWSE_ROOTS entries that are \
                 not a directory the engine can see: {}",
                invalid.join(", ")
            );
        }
        let roots = parse_browse_roots(&raw);
        if !roots.is_empty() {
            return roots;
        }
        eprintln!(
            "[ps5upload-engine] WARNING: PS5UPLOAD_BROWSE_ROOTS named no usable directory; the \
             file browser will open at the home directory instead."
        );
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".to_string());
    vec![home]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_dir_sorts_dirs_first_then_files_case_insensitive() {
        let tmp = std::env::temp_dir().join(format!("ps5_engine_lf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("Zsub")).unwrap();
        std::fs::create_dir_all(tmp.join("alpha")).unwrap();
        std::fs::write(tmp.join("b.txt"), b"hello").unwrap();
        std::fs::write(tmp.join("A.bin"), b"xy").unwrap();

        let out = list_dir(tmp.to_str().unwrap()).unwrap();
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alpha, Zsub — case-insensitive), then files
        // (A.bin, b.txt — case-insensitive).
        assert_eq!(names, vec!["alpha", "Zsub", "A.bin", "b.txt"]);
        assert!(out[0].is_dir);
        assert_eq!(out[0].size, 0);
        let btxt = out.iter().find(|e| e.name == "b.txt").unwrap();
        assert!(!btxt.is_dir);
        assert_eq!(btxt.size, 5);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_dir_errors_on_missing_dir() {
        let r = list_dir("/no/such/ps5upload-test-dir-xyz");
        assert!(r.is_err());
    }

    #[test]
    fn storage_roots_returns_at_least_one_entry() {
        let roots = storage_roots();
        assert!(!roots.is_empty(), "expected at least the home dir / \"/\"");
    }

    // `storage_roots` reads a process-global env var, so tests that set it
    // must not run concurrently or one leaks into another.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn home_default() -> Vec<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/".to_string());
        vec![home]
    }

    fn tmp_dir(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("ps5_browse_{}_{tag}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn browse_roots_env_replaces_the_home_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp_dir("one");
        std::env::set_var("PS5UPLOAD_BROWSE_ROOTS", &dir);
        let roots = storage_roots();
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        assert_eq!(roots, vec![dir]);
    }

    #[test]
    fn browse_roots_unset_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        assert_eq!(storage_roots(), home_default());
    }

    #[test]
    fn browse_roots_blank_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PS5UPLOAD_BROWSE_ROOTS", "   ");
        let roots = storage_roots();
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        assert_eq!(roots, home_default());
    }

    #[test]
    fn browse_roots_takes_several_comma_separated_directories() {
        let a = tmp_dir("multi_a");
        let b = tmp_dir("multi_b");
        assert_eq!(parse_browse_roots(&format!(" {a} , {b} ")), vec![a, b]);
    }

    #[test]
    fn browse_roots_reports_entries_that_are_not_directories() {
        let good = tmp_dir("reported");
        let raw = format!("{good},,/no/such/ps5upload-browse-root");
        // A typo, or an unmounted volume, must not look like "the setting was
        // ignored". A blank entry is neither, so it stays unreported.
        assert_eq!(
            invalid_browse_root_entries(&raw),
            vec!["/no/such/ps5upload-browse-root".to_string()]
        );
        assert_eq!(parse_browse_roots(&raw), vec![good]);
    }
}
