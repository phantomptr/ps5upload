use crate::message::{PendingUpdate, ReleaseInfo};
use std::path::{Path, PathBuf};

pub async fn fetch_latest_release(include_prerelease: bool) -> anyhow::Result<ReleaseInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/repos/phantomptr/ps5upload/releases")
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;

    let releases = response.json::<Vec<ReleaseInfo>>().await?;
    if include_prerelease {
        releases
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No releases found"))
    } else {
        releases
            .into_iter()
            .find(|r| !r.prerelease)
            .ok_or_else(|| anyhow::anyhow!("No stable releases found"))
    }
}

pub async fn fetch_release_by_tag(tag: &str) -> anyhow::Result<ReleaseInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "https://api.github.com/repos/phantomptr/ps5upload/releases/tags/{}",
            tag
        ))
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;
    let release = response.json::<ReleaseInfo>().await?;
    Ok(release)
}

pub async fn download_asset(url: &str, dest_path: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "ps5upload")
        .send()
        .await?
        .error_for_status()?;

    let bytes = response.bytes().await?;
    std::fs::write(dest_path, bytes)?;
    Ok(())
}

pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dest_dir)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest_dir.join(file.mangled_name());
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut outfile = std::fs::File::create(&outpath)?;
        std::io::copy(&mut file, &mut outfile)?;
    }
    Ok(())
}

fn find_app_bundle(root: &Path) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            if entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("app"))
                .unwrap_or(false)
            {
                return Some(entry.path().to_path_buf());
            }
        }
    }
    None
}

fn find_file_by_name(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.file_name() == name {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

pub fn build_pending_update(extract_dir: &Path) -> anyhow::Result<PendingUpdate> {
    let current_exe = std::env::current_exe()?;
    let mut bundle_root: Option<PathBuf> = None;
    let mut cursor = current_exe.as_path();
    while let Some(parent) = cursor.parent() {
        if cursor
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("app"))
            .unwrap_or(false)
        {
            bundle_root = Some(cursor.to_path_buf());
            break;
        }
        cursor = parent;
    }

    if let Some(bundle_root) = bundle_root {
        let replacement = find_app_bundle(extract_dir)
            .ok_or_else(|| anyhow::anyhow!("Update bundle not found"))?;
        let restart_path = current_exe.clone();
        return Ok(PendingUpdate {
            target_path: bundle_root,
            replacement_path: replacement,
            restart_path,
            is_dir: true,
        });
    }

    let exe_name = current_exe
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid executable name"))?;
    let replacement = find_file_by_name(extract_dir, exe_name)
        .ok_or_else(|| anyhow::anyhow!("Update binary not found"))?;
    Ok(PendingUpdate {
        target_path: current_exe.clone(),
        replacement_path: replacement,
        restart_path: current_exe,
        is_dir: false,
    })
}

pub fn spawn_update_helper(pending: &PendingUpdate) -> anyhow::Result<()> {
    let helper_dir = std::env::temp_dir();
    let pid = std::process::id();
    let target = pending.target_path.to_string_lossy().to_string();
    let replacement = pending.replacement_path.to_string_lossy().to_string();
    let restart = pending.restart_path.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        let script_path = helper_dir.join("ps5upload_update.cmd");
        let script = format!(
            "@echo off\r\n:wait\r\ntasklist /FI \"PID eq {}\" | find \"{}\" >nul\r\nif not errorlevel 1 (timeout /t 1 >nul & goto wait)\r\n",
            pid, pid
        );
        let script = if pending.is_dir {
            format!(
                "{}rmdir /S /Q \"{}\"\r\nmove /Y \"{}\" \"{}\"\r\nstart \"\" \"{}\"\r\n",
                script, target, replacement, target, restart
            )
        } else {
            format!(
                "{}del /F /Q \"{}\"\r\nmove /Y \"{}\" \"{}\"\r\nstart \"\" \"{}\"\r\n",
                script, target, replacement, target, restart
            )
        };
        std::fs::write(&script_path, script)?;
        std::process::Command::new("cmd")
            .args(["/C", script_path.to_string_lossy().as_ref()])
            .spawn()?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        let script_path = helper_dir.join("ps5upload_update.sh");
        let script = format!(
            "#!/bin/sh\nwhile kill -0 {} 2>/dev/null; do sleep 0.2; done\n",
            pid
        );
        let script = if pending.is_dir {
            format!(
                "{}rm -rf \"{}\"\nmv \"{}\" \"{}\"\n\"{}\" &\n",
                script, target, replacement, target, restart
            )
        } else {
            format!(
                "{}rm -f \"{}\"\nmv \"{}\" \"{}\"\nchmod +x \"{}\"\n\"{}\" &\n",
                script, target, replacement, target, restart, restart
            )
        };
        std::fs::write(&script_path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms)?;
        }
        std::process::Command::new("sh").arg(script_path).spawn()?;
        return Ok(());
    }
}

pub fn normalize_version(version: &str) -> String {
    version.trim_start_matches('v').trim().to_string()
}

pub fn is_newer_version(latest: &str, current: &str) -> Option<bool> {
    let latest_norm = normalize_version(latest);
    let current_norm = normalize_version(current);
    let latest_v = semver::Version::parse(&latest_norm).ok()?;
    let current_v = semver::Version::parse(&current_norm).ok()?;
    Some(latest_v > current_v)
}

pub fn current_asset_name() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let arch_name = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return Err(format!("Unsupported arch: {}", arch)),
    };

    let os_name = match os {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => return Err(format!("Unsupported OS: {}", os)),
    };

    Ok(format!("ps5upload-{}-{}.zip", os_name, arch_name))
}
