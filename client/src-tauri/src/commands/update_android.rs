//! Android: download the update APK inside the app so it can be handed to the
//! system installer.
//!
//! The desktop flow (`updates::update_download`) saves into ~/Downloads and
//! reveals the folder; on Android that meant "open the APK URL in the browser",
//! then find the download, allow unknown apps, install: four to six steps. Here
//! the APK is streamed into the app's own cache dir (`<cacheDir>/updates`) with
//! progress events, checked (APK header, and the SHA-256 from latest.json when
//! the release publishes one), and the page passes the path to
//! `window.PS5UploadUpdater.install` (AppUpdater.kt, written by
//! scripts/release/android-postinit-patch.sh), which opens Android's own
//! "Update?" dialog through a PackageInstaller session.
//!
//! The command builds on every platform (its checks are tested on the host),
//! but only the Android page calls it.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::shell_error::{code_only, coded};

/// Event the page listens on for download progress.
pub const PROGRESS_EVENT: &str = "update-apk-progress";

/// Release APKs are ~40 MB; nothing legitimate comes near this.
const APK_MAX_BYTES: u64 = 300 * 1024 * 1024;

/// Manifest keys for the APKs this device can run, best first, from its ABI
/// list (`ro.product.cpu.abilist`, best first, e.g. `arm64-v8a,armeabi-v7a`).
/// `compiled` (the key of the running build) is the last resort, so an
/// unreadable ABI list still updates like before. A 32-bit copy on a 64-bit
/// phone moves to the 64-bit APK; a 32-bit-only phone never gets it.
#[cfg(any(target_os = "android", test))]
pub fn asset_keys_for_abis(abilist: &str, compiled: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for abi in abilist.split(',').map(str::trim) {
        let key = match abi {
            "arm64-v8a" => "android",
            "armeabi-v7a" => "android-armv7",
            _ => continue,
        };
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    if !keys.iter().any(|k| k == compiled) {
        keys.push(compiled.to_string());
    }
    keys
}

/// This device's ABI list, best first. Empty when it cannot be read.
#[cfg(target_os = "android")]
pub fn device_abilist() -> String {
    std::process::Command::new("getprop")
        .arg("ro.product.cpu.abilist")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// A SHA-256 from the manifest as 64 lowercase hex chars, or None when it is
/// absent or malformed (a malformed one is an error for the caller, not a skip).
fn normalize_sha256(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// A plain `<name>.apk` basename: no separators, no leading dot.
fn is_apk_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && name.len() > 4
        && name.to_ascii_lowercase().ends_with(".apk")
}

/// Where update APKs live: inside the app's cache dir, which is where
/// AppUpdater.kt accepts them from.
fn updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("no cache dir: {e}"))?
        .join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    Ok(dir)
}

/// `<name>.part`: what an interrupted download of `name` has so far.
fn part_name(name: &str) -> String {
    format!("{name}.part")
}

/// Remove earlier downloads (other versions, finished or interrupted). `keep` stays, and what an
/// interrupted download of it has so far, which the next try carries on from.
fn remove_others(dir: &std::path::Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let part = part_name(keep);
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_str();
        if name != Some(keep) && name != Some(part.as_str()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// An APK is a zip: anything else is an error page or a wrong asset.
fn looks_like_apk(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"PK\x03\x04"
}

#[derive(Serialize, Clone)]
struct Progress {
    received: u64,
    /// 0 when the server did not say.
    total: u64,
}

#[derive(Serialize)]
pub struct ApkDownload {
    /// Absolute path of the verified APK, for `PS5UploadUpdater.install`.
    pub path: String,
    pub bytes: u64,
    /// The SHA-256 from latest.json matched (false: the release has none).
    pub verified: bool,
}

/// Download the update APK into the app's cache, with progress events, and
/// check it. `sha256` is the release's hash for this APK when latest.json has
/// one; a mismatch deletes the file and fails. A download that was cut off
/// carries on where it stopped (`update_fetch`, which is the desktop's path too).
#[tauri::command]
pub async fn update_download_apk(
    app: AppHandle,
    url: String,
    filename: String,
    sha256: Option<String>,
) -> Result<ApkDownload, String> {
    if !super::updates::is_allowed_download_url(&url) {
        return Err(coded(
            "update_unsafe_url",
            serde_json::json!({ "url": url }),
        ));
    }
    if !is_apk_filename(&filename) {
        return Err(coded(
            "update_bad_name",
            serde_json::json!({ "name": filename }),
        ));
    }
    let expected = match sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(
            normalize_sha256(raw)
                .ok_or_else(|| coded("update_bad_checksum", serde_json::json!({ "value": raw })))?,
        ),
        None => None,
    };
    let active = super::update_fetch::ActiveDownload::begin()?;
    let dir = updates_dir(&app)?;
    remove_others(&dir, &filename);
    let target = dir.join(&filename);

    // Already downloaded and verified once: hand it over again (the person
    // cancelled Android's dialog and tapped again).
    if let (Some(want), Ok(meta)) = (&expected, std::fs::metadata(&target)) {
        let t = target.clone();
        let actual = tokio::task::spawn_blocking(move || super::update_fetch::sha256_file(&t))
            .await
            .ok()
            .and_then(Result::ok);
        if actual.as_ref() == Some(want) {
            return Ok(ApkDownload {
                path: target.to_string_lossy().into_owned(),
                bytes: meta.len(),
                verified: true,
            });
        }
    }

    let client = super::update_fetch::client()?;
    let part = dir.join(part_name(&filename));
    let progress_app = app.clone();
    let mut report = move |received: u64, total: u64| {
        let _ = progress_app.emit(PROGRESS_EVENT, Progress { received, total });
    };
    let done = super::update_fetch::download_verified(
        &client,
        &url,
        &part,
        APK_MAX_BYTES,
        expected.as_deref(),
        active.cancel_signal(),
        &mut report,
    )
    .await?;

    let to = target.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        if !looks_like_apk(&part) {
            let _ = std::fs::remove_file(&part);
            return Err(code_only("update_not_apk"));
        }
        super::replace_file(&part, &to).map_err(|e| {
            coded(
                "update_disk",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })
    })
    .await
    .map_err(|e| {
        coded(
            "update_disk",
            serde_json::json!({ "detail": e.to_string() }),
        )
    })??;
    Ok(ApkDownload {
        path: target.to_string_lossy().into_owned(),
        bytes: done.bytes,
        verified: expected.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_64_bit_phone_gets_the_arm64_apk_first() {
        assert_eq!(
            asset_keys_for_abis("arm64-v8a,armeabi-v7a,armeabi", "android"),
            vec!["android", "android-armv7"]
        );
        // A 32-bit copy on a 64-bit phone moves to arm64.
        assert_eq!(
            asset_keys_for_abis("arm64-v8a,armeabi-v7a", "android-armv7"),
            vec!["android", "android-armv7"]
        );
    }

    #[test]
    fn a_32_bit_phone_never_gets_the_arm64_apk() {
        assert_eq!(
            asset_keys_for_abis("armeabi-v7a,armeabi", "android-armv7"),
            vec!["android-armv7"]
        );
    }

    #[test]
    fn unknown_abis_fall_back_to_the_running_build() {
        assert_eq!(asset_keys_for_abis("", "android"), vec!["android"]);
        assert_eq!(
            asset_keys_for_abis("x86_64,x86", "android"),
            vec!["android"]
        );
        assert_eq!(
            asset_keys_for_abis(" x86_64 , arm64-v8a ", "android"),
            vec!["android"]
        );
    }

    #[test]
    fn sha256_must_be_64_hex_chars() {
        let good = "A".repeat(64);
        assert_eq!(normalize_sha256(&good), Some("a".repeat(64)));
        assert_eq!(
            normalize_sha256(&format!(" {} ", "0f".repeat(32))),
            Some("0f".repeat(32))
        );
        assert_eq!(normalize_sha256(&"a".repeat(63)), None);
        assert_eq!(normalize_sha256(&"g".repeat(64)), None);
        assert_eq!(normalize_sha256(""), None);
    }

    #[test]
    fn only_plain_apk_names_are_accepted() {
        assert!(is_apk_filename("PS5Upload-android.apk"));
        assert!(is_apk_filename("PS5Upload-10.2.0-android-armv7.APK"));
        assert!(!is_apk_filename(".apk"));
        assert!(!is_apk_filename(".hidden.apk"));
        assert!(!is_apk_filename("../x.apk"));
        assert!(!is_apk_filename("a\\b.apk"));
        assert!(!is_apk_filename("PS5Upload.zip"));
        assert!(!is_apk_filename(""));
    }

    #[test]
    fn a_new_download_keeps_only_its_own_file_and_part() {
        let dir = std::env::temp_dir().join(format!("ps5u-apk-others-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["new.apk", "new.apk.part", "old.apk", "old.apk.part"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        remove_others(&dir, "new.apk");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["new.apk", "new.apk.part"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_zip_counts_as_an_apk() {
        let dir = std::env::temp_dir().join(format!("ps5u-apk-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.apk");
        std::fs::write(&p, b"PK\x03\x04rest").unwrap();
        assert!(looks_like_apk(&p));
        std::fs::write(&p, b"<html>").unwrap();
        assert!(!looks_like_apk(&p));
        std::fs::write(&p, b"PK").unwrap();
        assert!(!looks_like_apk(&p), "shorter than the signature");
        assert!(!looks_like_apk(&dir.join("missing.apk")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
