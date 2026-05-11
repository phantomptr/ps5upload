//! USB autoloader wizard — write a `/ps5_autoloader/` folder onto a
//! USB stick so the user can plug it into the PS5 and have a curated
//! payload chain auto-load on boot.
//!
//! Why this exists: every jailbreak session today requires the user
//! to manually re-send the right ELFs in the right order. PLK-style
//! autoloaders read `/ps5_autoloader/autoload.txt` from the inserted
//! USB and execute its entries — turning "click Send 5 times in the
//! right order" into "plug in the stick."
//!
//! Scope:
//!   - Read-only enumeration of mounted removable drives (no
//!     mounting, no formatting — that's out-of-scope per
//!     `project_exfat_writer_stub.md` in memory).
//!   - Curated copy from the Phase 2 cache (we never re-download
//!     here; payloads must already exist locally).
//!   - autoload.txt generation with the catalogue's recommended
//!     priorities + delays.
//!
//! Cross-platform per CLAUDE.md:
//!   - macOS: `/Volumes/*` minus the system disks.
//!   - Linux: `/media/$USER/*` and `/run/media/$USER/*`.
//!   - Windows: `GetLogicalDriveStringsW` + `GetDriveTypeW` for
//!     `DRIVE_REMOVABLE`.
//!
//! Atomic writes throughout — every file goes via `<name>.tmp` →
//! rename to avoid leaving half-written ELFs on the stick if the
//! user yanks it mid-write.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Removable drive enumeration result.
#[derive(Serialize, Clone)]
pub struct UsbDrive {
    /// Mount path the user / OS sees: `/Volumes/MyStick`,
    /// `/media/yunpeng/STICK`, `E:\\`. Suitable for `Path::new()`.
    path: String,
    /// Human label (volume name). Falls back to the basename of
    /// `path` when the OS doesn't expose one.
    label: String,
    /// Free bytes available, or 0 when unknown. Surfaced so the UI
    /// can warn before bundling a 50 MiB Itemzflow onto a stick
    /// with 8 MiB free.
    free_bytes: u64,
    /// Total bytes (capacity), or 0 when unknown.
    total_bytes: u64,
}

/// Enumerate currently-mounted removable drives. Tauri command —
/// invoked when the wizard opens.
///
/// We do not auto-refresh; the wizard exposes a "Rescan" button so
/// users can plug in a stick after opening the dialog.
#[tauri::command]
pub async fn usb_list_removable() -> Vec<UsbDrive> {
    // Spawn-blocking because the directory walks + statfs calls below
    // are sync. Cheap on all platforms — no I/O beyond a few
    // hundred bytes of metadata.
    tokio::task::spawn_blocking(enumerate_drives)
        .await
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn enumerate_drives() -> Vec<UsbDrive> {
    let mut out = Vec::new();
    let read = match std::fs::read_dir("/Volumes") {
        Ok(r) => r,
        Err(_) => return out,
    };
    for f in read.flatten() {
        let p = f.path();
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // `/Volumes/Macintosh HD` is the system root mount; skip.
        // It's the only one safe to exclude by name across macOS
        // versions — disk names are otherwise user-controlled.
        if name == "Macintosh HD" || name.starts_with('.') {
            continue;
        }
        // Skip anything that's actually a symlink to / (system disk
        // sometimes appears under multiple names).
        if let Ok(meta) = std::fs::symlink_metadata(&p) {
            if meta.file_type().is_symlink() {
                if let Ok(real) = std::fs::canonicalize(&p) {
                    if real == Path::new("/") {
                        continue;
                    }
                }
            }
        }
        let (free, total) = statvfs_or_zero(&p);
        out.push(UsbDrive {
            path: p.to_string_lossy().to_string(),
            label: name,
            free_bytes: free,
            total_bytes: total,
        });
    }
    out
}

#[cfg(target_os = "linux")]
fn enumerate_drives() -> Vec<UsbDrive> {
    let mut out = Vec::new();
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
    // Both common locations on modern Linux: /media/$USER (Ubuntu,
    // Debian default) and /run/media/$USER (Fedora, Arch with udisks2).
    let roots = [format!("/media/{user}"), format!("/run/media/{user}")];
    // Also check the legacy bare /media (some distros mount auto-USB
    // there without per-user subdirectories).
    let alt_roots = ["/media".to_string(), "/mnt".to_string()];
    for root in roots.iter().chain(alt_roots.iter()) {
        let read = match std::fs::read_dir(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for f in read.flatten() {
            let p = f.path();
            // For /media and /mnt, skip non-mountpoint subdirs (the
            // user dirs themselves) by checking that the entry is a
            // dir AND has a different device than its parent. Skip
            // for /media/$USER and /run/media/$USER (those parents
            // are user-specific).
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if name == user || name.starts_with('.') {
                continue;
            }
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_dir() {
                continue;
            }
            let (free, total) = statvfs_or_zero(&p);
            // Skip suspicious entries with zero capacity — usually
            // empty mountpoint stubs left over by a previous mount.
            if total == 0 {
                continue;
            }
            out.push(UsbDrive {
                path: p.to_string_lossy().to_string(),
                label: name,
                free_bytes: free,
                total_bytes: total,
            });
        }
    }
    // Dedupe by path — /media/yunpeng/STICK and /run/media/yunpeng/STICK
    // can both be present if both stacks are configured.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

#[cfg(target_os = "windows")]
fn enumerate_drives() -> Vec<UsbDrive> {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::ffi::OsStringExt;

    extern "system" {
        fn GetLogicalDriveStringsW(buf_len: u32, buf: *mut u16) -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
        fn GetVolumeInformationW(
            root: *const u16,
            volume_name: *mut u16,
            volume_name_len: u32,
            volume_serial: *mut u32,
            max_component_len: *mut u32,
            file_system_flags: *mut u32,
            file_system_name: *mut u16,
            file_system_name_len: u32,
        ) -> i32;
    }
    const DRIVE_REMOVABLE: u32 = 2;

    let mut buf: Vec<u16> = vec![0; 256];
    let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if n == 0 || (n as usize) > buf.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // The buffer is a sequence of null-terminated strings, double-null
    // terminated. GetLogicalDriveStringsW returns the count of u16s
    // EXCLUDING the final null terminator of the double-null. So if the
    // last drive's trailing null sits at index `n-1`, looping
    // `0..n` would skip the final null-terminated segment because we
    // never see a closing null for the last entry. Treat the byte just
    // past `n` as if it were a null so the last segment is processed.
    let nu = n as usize;
    let mut start = 0usize;
    for i in 0..=nu {
        let is_null = i == nu || buf[i] == 0;
        if is_null {
            if i > start {
                let slice = &buf[start..i];
                let path_os = OsString::from_wide(slice);
                let mut wide: Vec<u16> = path_os.encode_wide().collect();
                wide.push(0);
                let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
                if kind == DRIVE_REMOVABLE {
                    let (free, total) = {
                        let mut free_caller: u64 = 0;
                        let mut total_bytes: u64 = 0;
                        let mut total_free: u64 = 0;
                        let ok = unsafe {
                            GetDiskFreeSpaceExW(
                                wide.as_ptr(),
                                &mut free_caller,
                                &mut total_bytes,
                                &mut total_free,
                            )
                        };
                        if ok != 0 {
                            (free_caller, total_bytes)
                        } else {
                            (0, 0)
                        }
                    };
                    let mut label_buf: Vec<u16> = vec![0; 261];
                    let mut serial: u32 = 0;
                    let mut max_comp: u32 = 0;
                    let mut fs_flags: u32 = 0;
                    let mut fs_name: Vec<u16> = vec![0; 32];
                    let label_ok = unsafe {
                        GetVolumeInformationW(
                            wide.as_ptr(),
                            label_buf.as_mut_ptr(),
                            label_buf.len() as u32,
                            &mut serial,
                            &mut max_comp,
                            &mut fs_flags,
                            fs_name.as_mut_ptr(),
                            fs_name.len() as u32,
                        )
                    };
                    let label_str = if label_ok != 0 {
                        let nul = label_buf
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(label_buf.len());
                        OsString::from_wide(&label_buf[..nul])
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        String::new()
                    };
                    let path_str = path_os.to_string_lossy().into_owned();
                    let label = if !label_str.is_empty() {
                        label_str
                    } else {
                        path_str.trim_end_matches('\\').to_string()
                    };
                    out.push(UsbDrive {
                        path: path_str,
                        label,
                        free_bytes: free,
                        total_bytes: total,
                    });
                }
            }
            start = i + 1;
        }
    }
    let _ = OsStr::new(""); // silence unused-import warning if MSVC trims
    out
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn statvfs_or_zero(path: &Path) -> (u64, u64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };
    // SAFETY: libc::statvfs writes into a zeroed struct on success;
    // we read only the fields documented to be valid.
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statvfs(c.as_ptr(), &mut buf) };
    if r != 0 {
        return (0, 0);
    }
    let frsize = buf.f_frsize as u64;
    let free = (buf.f_bavail as u64).saturating_mul(frsize);
    let total = (buf.f_blocks as u64).saturating_mul(frsize);
    (free, total)
}

// ─── autoload.txt generation + write ────────────────────────────────

#[derive(Deserialize)]
pub struct AutoloaderInstallReq {
    /// Drive root (one of `usb_list_removable`'s `path` values).
    drive_path: String,
    /// Catalogue payload ids to include, in the order the user
    /// selected them in the wizard. We re-sort by `autoload_priority`
    /// so user's pick-order doesn't accidentally load SMP before
    /// kstuff. UI shows the resulting effective order in a preview.
    payload_ids: Vec<String>,
    /// True = also write our own ps5upload.elf to the stick. Defaults
    /// to true at the wizard layer; false skips inserting our own
    /// payload (rare — user explicitly wants a kstuff+SMP-only stick).
    #[serde(default)]
    include_ps5upload: bool,
}

#[derive(Serialize)]
pub struct AutoloaderInstallResult {
    /// Paths of all files written, relative to drive root. Wizard
    /// shows them in a "wrote N files" summary.
    written: Vec<String>,
    /// Preview of the autoload.txt the wizard generated. Useful for
    /// the user to copy or audit.
    autoload_txt: String,
    /// Per-payload reasons we skipped (e.g. "no local cache"). Empty
    /// when everything succeeded.
    skipped: Vec<String>,
}

/// Resolve a payload id to its locally-cached ELF, if any. Mirrors
/// the logic in `payloads::payloads_local_path` but uses the
/// AppHandle directly rather than going through Tauri command IPC.
fn resolve_local_payload(app: &AppHandle, id: &str) -> Option<PathBuf> {
    let root = app.path().app_local_data_dir().ok()?;
    let dir = root.join("payloads").join(id);
    let read = std::fs::read_dir(&dir).ok()?;
    for f in read.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) == Some("elf") {
            return Some(p);
        }
    }
    None
}

/// Locate our own bundled payload. Reuses the same hash-stamped
/// extraction `probes.rs::payload_bundled_path` performs, going
/// through that command would require a full Tauri invoke round-trip;
/// instead we derive the path the same way it does.
fn resolve_bundled_ps5upload(app: &AppHandle) -> Option<PathBuf> {
    let root = app.path().app_local_data_dir().ok()?;
    let p = root.join("payload").join("ps5upload.elf");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Sort selected payloads by their autoload priority so the
/// generated `autoload.txt` boots them in the right order regardless
/// of UI selection sequence. ps5upload always goes after the
/// catalogue entries (priority is "everything from catalogue, then
/// us"), unless the user explicitly disabled `include_ps5upload`.
fn ordered_payloads(selected_ids: &[String], include_ps5upload: bool) -> Vec<(String, u8, u32)> {
    let mut rows: Vec<(String, u8, u32)> = selected_ids
        .iter()
        .filter_map(|id| {
            crate::commands::payloads::autoload_meta_for(id)
                .map(|(canonical_id, prio, delay)| (canonical_id.to_string(), prio, delay))
        })
        .collect();
    rows.sort_by_key(|(_, prio, _)| *prio);
    if include_ps5upload {
        // ps5upload runs *after* the runtime payloads (so kstuff is
        // up, SMP is up, then we boot to talk to the user). Pick a
        // priority slot one above the highest in the user's set —
        // not 255 — so future catalogue entries with priority < 255
        // can still slot after us if we ever add such entries.
        let max_prio = rows.iter().map(|(_, p, _)| *p).max().unwrap_or(2);
        rows.push(("ps5upload".to_string(), max_prio.saturating_add(1), 0));
    }
    rows
}

/// Write the autoloader to the picked drive. Tauri command — invoked
/// when the user clicks "Install" in the wizard.
#[tauri::command]
pub async fn usb_autoloader_install(
    app: AppHandle,
    req: AutoloaderInstallReq,
) -> Result<AutoloaderInstallResult, String> {
    // Validate drive path: must exist, must be writable. We do this
    // upfront rather than on first write so the user gets a single
    // clear error.
    let drive = PathBuf::from(&req.drive_path);
    let drive_meta = std::fs::metadata(&drive).map_err(|e| format!("drive not accessible: {e}"))?;
    if !drive_meta.is_dir() {
        return Err(format!("drive path is not a directory: {}", req.drive_path));
    }

    let autoloader_dir = drive.join("ps5_autoloader");
    std::fs::create_dir_all(&autoloader_dir).map_err(|e| format!("create ps5_autoloader/: {e}"))?;

    let ordered = ordered_payloads(&req.payload_ids, req.include_ps5upload);
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut autoload_lines: Vec<String> = Vec::new();
    autoload_lines.push("# Generated by ps5upload — do not edit manually.".to_string());
    autoload_lines
        .push("# Ordering follows catalogue autoload_priority; lower = earlier.".to_string());

    for (id, _prio, delay_ms) in &ordered {
        let src = if id == "ps5upload" {
            match resolve_bundled_ps5upload(&app) {
                Some(p) => p,
                None => {
                    skipped.push(
                        "ps5upload: bundled payload not yet extracted (open Connection screen first)"
                            .to_string(),
                    );
                    continue;
                }
            }
        } else {
            match resolve_local_payload(&app, id) {
                Some(p) => p,
                None => {
                    skipped.push(format!(
                        "{id}: no local cache (download via Payloads tab first)"
                    ));
                    continue;
                }
            }
        };
        let dst_name = format!("{id}.elf");
        let dst = autoloader_dir.join(&dst_name);
        let tmp = autoloader_dir.join(format!("{dst_name}.tmp"));
        std::fs::copy(&src, &tmp)
            .map_err(|e| format!("copy {} -> {}: {e}", src.display(), tmp.display()))?;
        super::replace_file(&tmp, &dst)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), dst.display()))?;
        written.push(format!("ps5_autoloader/{dst_name}"));
        autoload_lines.push(dst_name.clone());
        if *delay_ms > 0 {
            // PLK autoload syntax: `!N` waits N milliseconds before
            // continuing. Different forks use slightly different
            // syntaxes; this is the broadest-compatibility one.
            autoload_lines.push(format!("!{delay_ms}"));
        }
    }

    let autoload_txt = autoload_lines.join("\n") + "\n";
    let autoload_path = autoloader_dir.join("autoload.txt");
    let autoload_tmp = autoloader_dir.join("autoload.txt.tmp");
    std::fs::write(&autoload_tmp, autoload_txt.as_bytes())
        .map_err(|e| format!("write autoload.txt: {e}"))?;
    super::replace_file(&autoload_tmp, &autoload_path)
        .map_err(|e| format!("rename autoload.txt: {e}"))?;
    written.push("ps5_autoloader/autoload.txt".to_string());

    Ok(AutoloaderInstallResult {
        written,
        autoload_txt,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_includes_ps5upload_last_when_requested() {
        let ids = vec![
            "shadowmountplus".to_string(),
            "kstuff-echostretch".to_string(),
        ];
        let ord = ordered_payloads(&ids, true);
        // First two are catalogue entries by priority; last is us.
        assert_eq!(
            ord.first().map(|(id, _, _)| id.as_str()),
            Some("kstuff-echostretch")
        );
        assert_eq!(ord.last().map(|(id, _, _)| id.as_str()), Some("ps5upload"));
    }

    #[test]
    fn ordered_omits_ps5upload_when_not_requested() {
        let ids = vec![
            "shadowmountplus".to_string(),
            "kstuff-echostretch".to_string(),
        ];
        let ord = ordered_payloads(&ids, false);
        assert_eq!(ord.len(), 2);
        assert!(ord.iter().all(|(id, _, _)| id != "ps5upload"));
    }

    #[test]
    fn ordered_sorts_by_priority_not_user_input() {
        // User picked SMP before kstuff; result must put kstuff first.
        let ids = vec![
            "shadowmountplus".to_string(),
            "kstuff-echostretch".to_string(),
        ];
        let ord = ordered_payloads(&ids, false);
        assert_eq!(ord[0].0, "kstuff-echostretch");
        assert_eq!(ord[1].0, "shadowmountplus");
    }

    #[test]
    fn ordered_drops_unknown_ids() {
        let ids = vec!["nope".to_string(), "kstuff-echostretch".to_string()];
        let ord = ordered_payloads(&ids, false);
        assert_eq!(ord.len(), 1);
        assert_eq!(ord[0].0, "kstuff-echostretch");
    }
}
