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
//! Every file goes via `<name>.tmp` and all of them are renamed at the end, so a full or
//! yanked stick keeps what it had (`write_autoloader_folder`).

use std::path::PathBuf;
// `Path` is only used inside the macOS/Linux `enumerate_drives` +
// `statvfs_or_zero` paths. On Windows neither cfg block compiles, so
// an unconditional `Path` import trips `-D unused-imports`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::Path;

use ps5upload_core::payload_library::Library;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

/// Removable drive enumeration result.
#[derive(Serialize, Clone)]
pub struct UsbDrive {
    /// Mount path the user / OS sees: `/Volumes/MyStick`,
    /// `/media/alice/STICK`, `E:\\`. Suitable for `Path::new()`.
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

// Mobile (Android/iOS) fallback: there is no removable-drive mount
// enumeration to do here (the USB autoloader is a desktop workflow that
// writes an ELF onto a stick plugged into the computer). Returning an
// empty list keeps `usb_list_removable` resolving cleanly — the UI shows
// "no drives" — until/if a SAF-based flow is added. See the Android port
// feasibility doc (docs/android-port-feasibility.md, USB-autoloader row).
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn enumerate_drives() -> Vec<UsbDrive> {
    Vec::new()
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
        if !super::drive_checks::usable_mount(&p) {
            continue;
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
    use std::os::unix::fs::MetadataExt;
    let roots = [format!("/media/{user}"), format!("/run/media/{user}")];
    // Also check the legacy bare /media (some distros mount auto-USB
    // there without per-user subdirectories) and /mnt.
    let alt_roots = ["/media".to_string(), "/mnt".to_string()];
    for (root, is_alt) in roots
        .iter()
        .map(|r| (r, false))
        .chain(alt_roots.iter().map(|r| (r, true)))
    {
        // Device id of the root itself, so we can tell a real mountpoint
        // (different st_dev) from a plain subdirectory (same st_dev).
        let root_dev = std::fs::metadata(root).map(|m| m.dev()).ok();
        let read = match std::fs::read_dir(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for f in read.flatten() {
            let p = f.path();
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // `/media/$USER` in the bare root is the folder of mounts, not a drive; a stick
            // labelled like the user under `/media/$USER/` is one.
            if (is_alt && name == user) || name.starts_with('.') {
                continue;
            }
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_dir() {
                continue;
            }
            // For the bare /media and /mnt roots, only entries that are
            // actual mountpoints count — a mountpoint's st_dev differs
            // from its parent root's. This is the device check the old
            // comment always claimed but never performed; without it,
            // plain subdirs (other users' /media dirs, ordinary folders)
            // were surfaced as removable drives. /media/$USER and
            // /run/media/$USER are exempt: their children are the mounts.
            if is_alt {
                match (root_dev, meta.dev()) {
                    (Some(rd), d) if rd != d => {} // genuine mountpoint
                    _ => continue,
                }
            }
            let (free, total) = statvfs_or_zero(&p);
            // Skip suspicious entries with zero capacity — usually
            // empty mountpoint stubs left over by a previous mount.
            if total == 0 || !super::drive_checks::usable_mount(&p) {
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
    // Dedupe by path — /media/alice/STICK and /run/media/alice/STICK
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
    const DRIVE_FIXED: u32 = 3;
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

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
                let usb_disk = kind == DRIVE_FIXED
                    && super::drive_checks::fixed_disk_on_usb(&path_os.to_string_lossy());
                if kind == DRIVE_REMOVABLE || usb_disk {
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
                    // A locked or write-protected drive would only fail when written to.
                    if label_ok != 0 && fs_flags & FILE_READ_ONLY_VOLUME != 0 {
                        start = i + 1;
                        continue;
                    }
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
    /// Per-payload lines we skipped (`<id>: <english reason>`). Empty when everything succeeded.
    skipped: Vec<String>,
}

/// The first line of every `autoload.txt` we write; how an earlier one of ours is recognised.
const AUTOLOAD_HEADER: &str = "# Generated by ps5upload — do not edit manually.";

/// Locate our own bundled payload through the same hash-stamped
/// extraction the Connection screen uses, so the stick gets this
/// version's helper even when nothing has extracted it yet since an
/// app update (a bare `exists()` check handed out the previous one).
fn resolve_bundled_ps5upload(app: &AppHandle) -> Option<PathBuf> {
    match super::probes::find_bundled_payload(app) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[usb-autoloader] bundled payload: {e}");
            None
        }
    }
}

/// Sort selected payloads by their autoload priority so the generated `autoload.txt` boots them
/// in the right order regardless of UI selection sequence. A custom repo sorts after every curated
/// entry. ps5upload goes after them all (kstuff is up, SMP is up, then we boot to talk to the
/// user), unless the user explicitly disabled `include_ps5upload`. An id the library doesn't know
/// comes back in the second list, so it is reported and not silently dropped.
fn ordered_payloads(
    lib: &Library,
    selected_ids: &[String],
    include_ps5upload: bool,
) -> (Vec<(String, u8, u32)>, Vec<String>) {
    let mut rows = Vec::with_capacity(selected_ids.len() + 1);
    let mut unknown = Vec::new();
    for id in selected_ids {
        match lib.autoload_meta(id) {
            Some(m) => rows.push((m.id, m.priority, m.delay_ms)),
            None => unknown.push(id.clone()),
        }
    }
    rows.sort_by_key(|(_, prio, _)| *prio);
    if include_ps5upload {
        // One above the highest in the user's set, not 255, so a future catalogue entry can
        // still slot after us.
        let max_prio = rows.iter().map(|(_, p, _)| *p).max().unwrap_or(2);
        rows.push(("ps5upload".to_string(), max_prio.saturating_add(1), 0));
    }
    (rows, unknown)
}

/// One payload going onto the stick.
struct StickFile {
    src: PathBuf,
    /// Name under `ps5_autoloader/`.
    name: String,
    delay_ms: u32,
}

/// The `autoload.txt` for these files, in order.
fn autoload_text(files: &[StickFile]) -> String {
    let mut lines = vec![
        AUTOLOAD_HEADER.to_string(),
        "# Ordering follows catalogue autoload_priority; lower = earlier.".to_string(),
    ];
    for f in files {
        lines.push(f.name.clone());
        if f.delay_ms > 0 {
            // PLK autoload syntax: `!N` waits N milliseconds before continuing. Different forks
            // use slightly different syntaxes; this is the broadest-compatibility one.
            lines.push(format!("!{}", f.delay_ms));
        }
    }
    lines.join("\n") + "\n"
}

/// An ELF an earlier run of this wizard put on the stick: named in our own previous
/// `autoload.txt`, or under a name only we use. Anything else in the folder is the user's own
/// file and stays.
fn is_ours_from_an_earlier_run(name: &str, previous_autoload: Option<&str>, lib: &Library) -> bool {
    if let Some(text) = previous_autoload.filter(|t| t.starts_with(AUTOLOAD_HEADER)) {
        if text.lines().any(|l| l.trim() == name) {
            return true;
        }
    }
    let stem = name.strip_suffix(".elf").unwrap_or(name);
    stem == "ps5upload" || lib.autoload_meta(stem).is_some()
}

/// The files that need writing, in bytes.
fn total_size(files: &[StickFile]) -> Result<u64, String> {
    let mut total = 0u64;
    for f in files {
        let meta = std::fs::metadata(&f.src).map_err(|e| format!("{}: {e}", f.src.display()))?;
        total = total.saturating_add(meta.len());
    }
    Ok(total)
}

/// Write the folder as one step, as far as a stick allows. Every file is copied to a temporary
/// name first, so a full or failing stick leaves what was there before untouched; only then do
/// the names change, `autoload.txt` last. ELFs of an earlier run that this one doesn't name go
/// too. A mid-way failure used to leave new ELFs beside an old `autoload.txt` naming another set.
fn write_autoloader_folder(
    folder: &std::path::Path,
    files: &[StickFile],
    autoload_txt: &str,
    lib: &Library,
) -> Result<Vec<String>, String> {
    struct Tmps(Vec<PathBuf>);
    impl Drop for Tmps {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("usb_write_failed: create ps5_autoloader/: {e}"))?;
    let previous = std::fs::read_to_string(folder.join("autoload.txt")).ok();

    let mut tmps = Tmps(Vec::with_capacity(files.len() + 1));
    for f in files {
        let tmp = folder.join(format!("{}.tmp", f.name));
        tmps.0.push(tmp.clone());
        std::fs::copy(&f.src, &tmp).map_err(|e| {
            format!(
                "usb_write_failed: copy {} -> {}: {e}",
                f.src.display(),
                tmp.display()
            )
        })?;
    }
    let autoload_tmp = folder.join("autoload.txt.tmp");
    tmps.0.push(autoload_tmp.clone());
    std::fs::write(&autoload_tmp, autoload_txt.as_bytes())
        .map_err(|e| format!("usb_write_failed: write autoload.txt: {e}"))?;

    let mut written = Vec::with_capacity(files.len() + 1);
    for f in files {
        let (tmp, dst) = (folder.join(format!("{}.tmp", f.name)), folder.join(&f.name));
        super::replace_file(&tmp, &dst)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), dst.display()))?;
        written.push(format!("ps5_autoloader/{}", f.name));
    }
    super::replace_file(&autoload_tmp, &folder.join("autoload.txt"))
        .map_err(|e| format!("rename autoload.txt: {e}"))?;
    written.push("ps5_autoloader/autoload.txt".to_string());

    if let Ok(read) = std::fs::read_dir(folder) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".elf")
                && !files.iter().any(|f| f.name == name)
                && is_ours_from_an_earlier_run(&name, previous.as_deref(), lib)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(written)
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
    let drive_meta = std::fs::metadata(&drive)
        .map_err(|e| format!("usb_drive_gone: {}: {e}", req.drive_path))?;
    if !drive_meta.is_dir() {
        return Err(format!(
            "usb_drive_gone: {} is not a folder",
            req.drive_path
        ));
    }

    // Re-validate server-side that the target is actually one of the
    // removable drives we enumerated. The frontend passes back a value
    // from `usb_list_removable`, but this command must not trust that:
    // a malformed or replayed IPC call could otherwise point
    // `drive_path` at any writable directory (`/`, the user's home, a
    // system folder) and we'd happily create `ps5_autoloader/` there
    // and write ELF files into it. "exists + is a dir" is necessary
    // but nowhere near sufficient.
    let requested = req.drive_path.clone();
    let known = tokio::task::spawn_blocking(enumerate_drives)
        .await
        .map_err(|e| format!("drive enumeration failed: {e}"))?;
    let canon_req = std::fs::canonicalize(&drive).ok();
    let known_drive = known.iter().find(|d| {
        d.path == requested
            || (canon_req.is_some() && std::fs::canonicalize(&d.path).ok() == canon_req)
    });
    let Some(known_drive) = known_drive else {
        return Err(format!(
            "usb_drive_gone: {} is not a currently-mounted removable drive",
            requested
        ));
    };
    let free_bytes = known_drive.free_bytes;

    let lib = super::payloads::library(&app)?;
    let (ordered, unknown) = ordered_payloads(&lib, &req.payload_ids, req.include_ps5upload);
    let mut skipped: Vec<String> = unknown
        .iter()
        .map(|id| format!("{id}: unknown_payload"))
        .collect();
    let mut files: Vec<StickFile> = Vec::with_capacity(ordered.len());
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
            match lib.local_path(id) {
                Some(p) => p,
                None => {
                    skipped.push(format!(
                        "{id}: no local cache (download via Payloads tab first)"
                    ));
                    continue;
                }
            }
        };
        files.push(StickFile {
            src,
            name: format!("{id}.elf"),
            delay_ms: *delay_ms,
        });
    }

    // Before anything is written. Zero means the OS didn't say; the copy then reports a full stick.
    let needed = total_size(&files)?;
    if free_bytes > 0 && needed > free_bytes {
        return Err(format!(
            "usb_not_enough_space: The payloads need {needed} bytes and the drive has {free_bytes} free."
        ));
    }

    let autoload_txt = autoload_text(&files);
    let folder = drive.join("ps5_autoloader");
    let text = autoload_txt.clone();
    let written =
        tokio::task::spawn_blocking(move || write_autoloader_folder(&folder, &files, &text, &lib))
            .await
            .map_err(|e| format!("write task: {e}"))??;

    Ok(AutoloaderInstallResult {
        written,
        autoload_txt,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ps5upload-usb-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn library(t: &Temp) -> Library {
        Library::new(t.0.join("payloads"))
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn files_in(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn src_file(t: &Temp, name: &str, body: &[u8]) -> PathBuf {
        let dir = t.0.join("src");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn stick_file(src: PathBuf, name: &str) -> StickFile {
        StickFile {
            src,
            name: name.to_string(),
            delay_ms: 0,
        }
    }

    #[test]
    fn ordered_includes_ps5upload_last_when_requested() {
        let t = Temp::new("order-us");
        let (ord, _) = ordered_payloads(
            &library(&t),
            &ids(&["shadowmountplus", "kstuff-echostretch"]),
            true,
        );
        assert_eq!(ord[0].0, "kstuff-echostretch");
        assert_eq!(ord.last().unwrap().0, "ps5upload");
    }

    #[test]
    fn ordered_omits_ps5upload_when_not_requested() {
        let t = Temp::new("order-nous");
        let (ord, _) = ordered_payloads(
            &library(&t),
            &ids(&["shadowmountplus", "kstuff-echostretch"]),
            false,
        );
        assert_eq!(ord.len(), 2);
        assert!(ord.iter().all(|(id, _, _)| id != "ps5upload"));
    }

    #[test]
    fn ordered_sorts_by_priority_not_user_input() {
        let t = Temp::new("order-sort");
        let (ord, _) = ordered_payloads(
            &library(&t),
            &ids(&["shadowmountplus", "kstuff-echostretch"]),
            false,
        );
        assert_eq!(ord[0].0, "kstuff-echostretch");
        assert_eq!(ord[1].0, "shadowmountplus");
    }

    #[test]
    fn an_unknown_id_is_reported_not_dropped() {
        let t = Temp::new("order-unknown");
        let (ord, unknown) =
            ordered_payloads(&library(&t), &ids(&["nope", "kstuff-echostretch"]), false);
        assert_eq!(ord.len(), 1);
        assert_eq!(unknown, ["nope"]);
    }

    #[test]
    fn a_custom_repo_is_resolved_and_loads_after_the_curated_ones() {
        // A ticked custom repo was silently left off the stick: "wrote N files" without it.
        let t = Temp::new("order-custom");
        let lib = library(&t);
        let added = lib
            .add_custom_repo(&ps5upload_core::payload_library::AddRepo {
                host: "github.com".into(),
                owner: "me".into(),
                name: "thing".into(),
                ..Default::default()
            })
            .unwrap();
        let (ord, unknown) = ordered_payloads(&lib, &ids(&[&added.id, "kstuff-echostretch"]), true);
        assert!(unknown.is_empty());
        let got: Vec<&str> = ord.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(got, ["kstuff-echostretch", added.id.as_str(), "ps5upload"]);
        assert_eq!(ord[1].2, 200);
    }

    #[test]
    fn the_folder_gets_the_new_set_and_loses_the_old_one() {
        let t = Temp::new("write");
        let lib = library(&t);
        let folder = t.0.join("stick").join("ps5_autoloader");
        std::fs::create_dir_all(&folder).unwrap();
        // An earlier run's files, and one the user put there by hand.
        std::fs::write(
            folder.join("autoload.txt"),
            format!("{AUTOLOAD_HEADER}\nold-thing.elf\nkstuff-echostretch.elf\n"),
        )
        .unwrap();
        std::fs::write(folder.join("old-thing.elf"), b"old").unwrap();
        std::fs::write(folder.join("shadowmountplus.elf"), b"old").unwrap();
        std::fs::write(folder.join("kstuff-echostretch.elf"), b"old").unwrap();
        std::fs::write(folder.join("mine.elf"), b"user's").unwrap();

        let files = vec![stick_file(
            src_file(&t, "k.elf", b"new kstuff"),
            "kstuff-echostretch.elf",
        )];
        let text = autoload_text(&files);
        let written = write_autoloader_folder(&folder, &files, &text, &lib).unwrap();
        assert_eq!(
            written,
            [
                "ps5_autoloader/kstuff-echostretch.elf",
                "ps5_autoloader/autoload.txt"
            ]
        );
        assert_eq!(
            files_in(&folder),
            ["autoload.txt", "kstuff-echostretch.elf", "mine.elf"]
        );
        assert_eq!(
            std::fs::read(folder.join("kstuff-echostretch.elf")).unwrap(),
            b"new kstuff"
        );
        assert_eq!(std::fs::read(folder.join("mine.elf")).unwrap(), b"user's");
        assert_eq!(
            std::fs::read_to_string(folder.join("autoload.txt")).unwrap(),
            text
        );
    }

    #[test]
    fn a_failed_copy_changes_nothing_on_the_stick() {
        let t = Temp::new("fail");
        let lib = library(&t);
        let folder = t.0.join("stick").join("ps5_autoloader");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("autoload.txt"), "old list\n").unwrap();
        std::fs::write(folder.join("kstuff-echostretch.elf"), b"old").unwrap();

        let files = vec![
            stick_file(src_file(&t, "k.elf", b"new"), "kstuff-echostretch.elf"),
            // Stands in for a full stick on payload 2.
            stick_file(t.0.join("src").join("missing.elf"), "shadowmountplus.elf"),
        ];
        let e = write_autoloader_folder(&folder, &files, "new list\n", &lib).unwrap_err();
        assert!(e.starts_with("usb_write_failed: "), "{e}");
        assert_eq!(
            files_in(&folder),
            ["autoload.txt", "kstuff-echostretch.elf"]
        );
        assert_eq!(
            std::fs::read(folder.join("kstuff-echostretch.elf")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("autoload.txt")).unwrap(),
            "old list\n"
        );
    }

    #[test]
    fn autoload_lists_files_in_order_with_their_waits() {
        let t = Temp::new("text");
        let text = autoload_text(&[
            StickFile {
                src: t.0.join("a"),
                name: "a.elf".into(),
                delay_ms: 3000,
            },
            StickFile {
                src: t.0.join("b"),
                name: "b.elf".into(),
                delay_ms: 0,
            },
        ]);
        assert!(text.starts_with(AUTOLOAD_HEADER));
        assert!(text.ends_with("a.elf\n!3000\nb.elf\n"), "{text}");
    }
}
