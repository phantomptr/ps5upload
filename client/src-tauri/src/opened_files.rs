//! "Open with PS5Upload" for .pkg / .fpkg files on the desktop.
//!
//! A package reaches the app three ways: in the launch arguments (Windows and Linux, first
//! launch), in a second launch's arguments, which tauri-plugin-single-instance hands to the
//! running app, and as a macOS "open these files" event (`RunEvent::Opened`, both at launch
//! and while running). Each path is kept here until the page takes it with
//! `opened_files_take`, and `ps5upload-open-files` tells a page that is already listening.
//! The page subscribes first and then takes, so a path that arrives before it has loaded is
//! never lost. Nothing is installed from here: the page shows what arrived and asks first.

// Only the page's `opened_files_take` is reachable on a phone; it answers empty there.
#![cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Event the page listens for; the paths themselves come from `opened_files_take`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const EVENT: &str = "ps5upload-open-files";

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn pending() -> std::sync::MutexGuard<'static, Vec<String>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// True for a name the app installs from (the association covers these two only).
fn is_package(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pkg") || e.eq_ignore_ascii_case("fpkg"))
        .unwrap_or(false)
}

/// The package files named in a launch's arguments. `argv[0]` is the program; flags are
/// skipped; relative paths are taken from `cwd` (the second launch's own folder); a
/// `file://` URL (some Linux file managers) becomes its path. Only existing files count.
pub fn package_args(argv: &[String], cwd: &Path) -> Vec<String> {
    argv.iter()
        .skip(1)
        .filter_map(|arg| {
            if arg.starts_with('-') {
                return None;
            }
            let path = if arg.starts_with("file://") {
                tauri::Url::parse(arg).ok()?.to_file_path().ok()?
            } else {
                PathBuf::from(arg)
            };
            existing_package(path, cwd)
        })
        .collect()
}

/// The package files among the URLs macOS asked the app to open.
#[cfg(any(target_os = "macos", test))]
pub fn package_urls(urls: &[tauri::Url]) -> Vec<String> {
    urls.iter()
        .filter(|u| u.scheme() == "file")
        .filter_map(|u| u.to_file_path().ok())
        .filter_map(|p| existing_package(p, Path::new("/")))
        .collect()
}

fn existing_package(path: PathBuf, cwd: &Path) -> Option<String> {
    if !is_package(&path) {
        return None;
    }
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// Keeps `paths` for the page. True when there was anything new.
pub fn push(paths: Vec<String>) -> bool {
    let mut held = pending();
    let mut added = false;
    for p in paths {
        if !held.contains(&p) {
            held.push(p);
            added = true;
        }
    }
    added
}

/// Hands the page the paths that arrived since it last asked, once.
#[tauri::command]
pub fn opened_files_take() -> Vec<String> {
    std::mem::take(&mut *pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            crate::test_tmp::temp_path(format!("ps5upload-opened-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keeps_existing_packages_only() {
        let dir = scratch("args");
        let pkg = dir.join("Game.PKG");
        let fpkg = dir.join("game.fpkg");
        let other = dir.join("notes.txt");
        for p in [&pkg, &fpkg, &other] {
            std::fs::write(p, b"x").unwrap();
        }
        let argv = vec![
            "ps5upload".to_string(),
            "--flag".to_string(),
            pkg.to_string_lossy().into_owned(),
            other.to_string_lossy().into_owned(),
            dir.join("missing.pkg").to_string_lossy().into_owned(),
            "game.fpkg".to_string(),
        ];
        let got = package_args(&argv, &dir);
        assert_eq!(
            got,
            vec![
                pkg.to_string_lossy().into_owned(),
                fpkg.to_string_lossy().into_owned()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_urls_become_paths() {
        let dir = scratch("urls");
        let pkg = dir.join("a b.pkg");
        std::fs::write(&pkg, b"x").unwrap();
        let url = tauri::Url::from_file_path(&pkg).unwrap();
        let web = tauri::Url::parse("https://example.com/a.pkg").unwrap();
        assert_eq!(
            package_urls(&[url.clone(), web]),
            vec![pkg.to_string_lossy().into_owned()]
        );
        let argv = vec!["ps5upload".to_string(), url.to_string()];
        assert_eq!(
            package_args(&argv, Path::new("/")),
            vec![pkg.to_string_lossy().into_owned()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_drains_once_and_push_dedupes() {
        let _ = opened_files_take();
        assert!(push(vec!["/a.pkg".into(), "/a.pkg".into()]));
        assert!(!push(vec!["/a.pkg".into()]));
        assert_eq!(opened_files_take(), vec!["/a.pkg".to_string()]);
        assert!(opened_files_take().is_empty());
    }
}
