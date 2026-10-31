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

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// Event the page listens on for download progress.
pub const PROGRESS_EVENT: &str = "update-apk-progress";

/// Release APKs are ~40 MB; nothing legitimate comes near this.
const APK_MAX_BYTES: u64 = 300 * 1024 * 1024;

/// Manifest keys for the APKs this device can run, best first, from its ABI
/// list (`ro.product.cpu.abilist`, best first, e.g. `arm64-v8a,armeabi-v7a`).
/// `compiled` (the key of the running build) is the last resort, so an
/// unreadable ABI list still updates like before. A 32-bit copy on a 64-bit
/// phone moves to the 64-bit APK; a 32-bit-only phone never gets it.
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {path:?}: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
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

/// Remove earlier downloads (older versions, interrupted `.part` files).
fn remove_others(dir: &std::path::Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_str() != Some(keep) {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
/// one; a mismatch deletes the file and fails.
#[tauri::command]
pub async fn update_download_apk(
    app: AppHandle,
    url: String,
    filename: String,
    sha256: Option<String>,
) -> Result<ApkDownload, String> {
    if !super::updates::is_allowed_download_url(&url) {
        return Err(format!("refusing to download from this address: {url}"));
    }
    if !is_apk_filename(&filename) {
        return Err(format!("not an APK name: {filename:?}"));
    }
    let expected = match sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(
            normalize_sha256(raw).ok_or_else(|| format!("bad SHA-256 in latest.json: {raw}"))?,
        ),
        None => None,
    };
    let dir = updates_dir(&app)?;
    remove_others(&dir, &filename);
    let target = dir.join(&filename);

    // Already downloaded and verified once: hand it over again (the person
    // cancelled Android's dialog and tapped again).
    if let (Some(want), Ok(meta)) = (&expected, std::fs::metadata(&target)) {
        if sha256_file(&target).ok().as_ref() == Some(want) {
            return Ok(ApkDownload {
                path: target.to_string_lossy().into_owned(),
                bytes: meta.len(),
                verified: true,
            });
        }
    }

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(60 * 15))
        .build()
        .map_err(|e| format!("download client init: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download HTTP {}: {url}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    if total > APK_MAX_BYTES {
        return Err(format!("download too large ({total} bytes)"));
    }

    let tmp = dir.join(format!("{filename}.part"));
    struct Cleanup(PathBuf, bool);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if self.1 {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
    let mut cleanup = Cleanup(tmp.clone(), true);

    use futures_util::StreamExt;
    use sha2::Digest;
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create {tmp:?}: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut received: u64 = 0;
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let mut last_emit = std::time::Instant::now();
    let _ = app.emit(PROGRESS_EVENT, Progress { received, total });
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download interrupted: {e}"))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > APK_MAX_BYTES {
            return Err("download exceeded the size limit — aborting".into());
        }
        if head.len() < 4 {
            let need = 4 - head.len();
            head.extend_from_slice(&chunk[..need.min(chunk.len())]);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|e| format!("write: {e}"))?;
        if last_emit.elapsed() >= std::time::Duration::from_millis(200) {
            last_emit = std::time::Instant::now();
            let _ = app.emit(PROGRESS_EVENT, Progress { received, total });
        }
    }
    let _ = app.emit(PROGRESS_EVENT, Progress { received, total });
    file.sync_all().map_err(|e| format!("fsync: {e}"))?;
    drop(file);

    if total > 0 && received != total {
        return Err(format!("download incomplete ({received} of {total} bytes)"));
    }
    // An APK is a zip: anything else is an error page or a wrong asset.
    if head != b"PK\x03\x04" {
        return Err("the download is not an APK".into());
    }
    let got = hex(&hasher.finalize());
    if let Some(want) = &expected {
        if &got != want {
            return Err(format!(
                "the download does not match the release checksum (got {got}, expected {want})"
            ));
        }
    }
    super::replace_file(&tmp, &target).map_err(|e| format!("rename: {e}"))?;
    cleanup.1 = false;
    Ok(ApkDownload {
        path: target.to_string_lossy().into_owned(),
        bytes: received,
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
    fn sha256_of_a_file() {
        let dir = std::env::temp_dir().join(format!("ps5u-apk-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.apk");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
