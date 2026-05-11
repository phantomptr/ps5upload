//! ShadowMount+ integration — read-only awareness.
//!
//! Detects whether ShadowMountPlus is installed/running on the
//! connected PS5, surfaces its config + state, lists its mounted
//! image dirs. Strictly read-only: we never write SMP's config,
//! never restart it through anything other than the same payload-send
//! flow the user could trigger themselves.
//!
//! Why integrate at all when SMP runs autonomously: the PS5-side
//! state (which images are mounted, which scan paths are configured,
//! what the auto-tuned kstuff delays look like) lives behind a
//! filesystem the user can only browse via FTP today. Lifting it
//! into the Library tab makes the tool stack behave like one product
//! instead of three.
//!
//! Detection signals (any one is sufficient):
//!   1. `/data/shadowmount/debug.log` exists (SMP writes this on
//!      first run). Strongest signal — the file is unique to SMP and
//!      survives across reboots.
//!   2. A process named `shadowmountplus` appears in PROC_LIST.
//!      Definitive proof it's currently running.
//!
//! All Sony API calls go through the engine's existing FTX2 RPCs
//! (FS_LIST_DIR, PROC_LIST, FS_READ); nothing here speaks the wire
//! directly — same separation of concerns as `commands/probes.rs`.

use std::time::Duration;

use serde::Serialize;

use ps5upload_core::fs_ops::{fs_read_with_timeout, list_dir_with_timeout, ListDirOptions};
use ps5upload_core::hw::proc_list;

/// Per-call deadline for any single FTX2 RPC. SMP's config is small,
/// the directory listings are small, the proc list is small. 5s is
/// generous and bounds the whole snapshot if any one call hangs.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard cap on file bytes we'll pull back to the desktop. SMP's
/// config files are typically <8 KiB; debug.log can be larger but
/// we only show the tail. 256 KiB matches the engine's game_meta
/// param.json limit — same "small text file" use case.
const READ_LIMIT_BYTES: u64 = 256 * 1024;

/// Canonical paths SMP writes/reads. Hardcoded because they're
/// burned into SMP's source (sm_config_mount.c) — not user-configurable.
const SMP_DATA_DIR: &str = "/data/shadowmount";
const SMP_CONFIG_PATH: &str = "/data/shadowmount/config.ini";
const SMP_AUTOTUNE_PATH: &str = "/data/shadowmount/autotune.ini";
const SMP_DEBUG_LOG_PATH: &str = "/data/shadowmount/debug.log";
const SMP_MOUNT_POINT: &str = "/mnt/shadowmnt";
/// Name SMP processes appear under in `ps`. Stable since the project's
/// inception (the binary is shadowmountplus.elf and the kernel-side
/// process inherits that basename).
const SMP_PROCESS_NAME_HINT: &str = "shadowmountplus";

/// One mounted-image row, surfaced in the Library tab's SMP panel.
#[derive(Serialize)]
pub struct SmpMountedImage {
    /// Mount point under /mnt/shadowmnt (hashed name SMP picks per
    /// image — `<image_name>_<crc32hex>`).
    mount_point: String,
    /// Best-effort image source filename, derived from the mount
    /// point name. SMP doesn't expose the original .ffpkg path
    /// through any read-only surface, so we strip the trailing
    /// "_<8-hex>" hash. Empty if the mount point doesn't match the
    /// expected pattern.
    derived_name: String,
}

/// Full SMP status snapshot. Every field is null/empty when SMP
/// isn't present — UI checks `installed` and `running` to decide
/// what to show.
#[derive(Serialize)]
pub struct SmpStatus {
    /// True when `/data/shadowmount/debug.log` exists. Implies SMP
    /// has been loaded at least once on this console.
    installed: bool,
    /// True when a process matching `shadowmountplus` is in PROC_LIST.
    /// Independent of `installed`: SMP can be installed but not
    /// currently running (e.g. user removed it from autoload).
    running: bool,
    /// Raw config.ini contents (truncated to READ_LIMIT_BYTES).
    /// Renderer parses + displays as a key/value table; we don't
    /// parse here so the renderer can show the raw file too.
    config_ini: Option<String>,
    /// Raw autotune.ini — kstuff per-game delays SMP learned from
    /// crash patterns + per-image sector-size overrides.
    autotune_ini: Option<String>,
    /// Tail of debug.log (last READ_LIMIT_BYTES). Useful for "why
    /// didn't my game mount?" troubleshooting without making the
    /// user FTP into the console.
    debug_log_tail: Option<String>,
    /// Images currently mounted under /mnt/shadowmnt/. Empty list
    /// when no mounts (or when SMP isn't running). UI shows count.
    mounted_images: Vec<SmpMountedImage>,
    /// Per-call error captures. Always present so the renderer can
    /// surface "config.ini exists but couldn't be read" without
    /// inferring from null fields.
    errors: Vec<String>,
}

/// One-shot status snapshot. Tauri command — invoked from the
/// Library tab's SMP panel mount + refresh button.
///
/// `addr` is the management-port address ("ip:9114"). Renderer is
/// expected to construct it via the existing `toMgmtAddr` helper.
///
/// All RPCs run on a `spawn_blocking` so we don't tie up the async
/// reactor; `fs_read` and friends are sync inside `ps5upload-core`.
#[tauri::command]
pub async fn smp_status(addr: String) -> Result<SmpStatus, String> {
    tokio::task::spawn_blocking(move || collect_status(&addr))
        .await
        .map_err(|e| format!("smp_status task: {e}"))?
}

/// Collect the snapshot. Pure sync function so unit tests can drive
/// it against a mock; today we only call it from `smp_status` which
/// supplies a real address.
fn collect_status(addr: &str) -> Result<SmpStatus, String> {
    let mut errors: Vec<String> = Vec::new();

    // Probe SMP's data dir. Existence ≈ "installed at least once."
    let data_dir_listing = list_dir_with_timeout(
        addr,
        SMP_DATA_DIR,
        ListDirOptions::default(),
        Some(RPC_TIMEOUT),
    );
    let installed = match &data_dir_listing {
        Ok(_) => true,
        Err(e) => {
            // ENOENT-equivalent: dir doesn't exist, which is
            // "not installed", not an error.
            let s = e.to_string();
            if s.contains("ENOENT") || s.contains("No such file") {
                false
            } else {
                errors.push(format!("list /data/shadowmount: {s}"));
                false
            }
        }
    };

    // PROC_LIST → look for shadowmountplus process. Match is
    // substring on lowercased name to handle "shadowmountplus.elf"
    // vs "shadowmountplus" depending on how the kernel records it.
    let running = match proc_list(addr) {
        Ok(list) => {
            let needle = SMP_PROCESS_NAME_HINT.to_ascii_lowercase();
            list.procs
                .iter()
                .any(|p| p.name.to_ascii_lowercase().contains(&needle))
        }
        Err(e) => {
            errors.push(format!("proc list: {e}"));
            false
        }
    };

    // Read config.ini + autotune.ini + debug.log tail. Each is
    // best-effort — if any fails we still return the others.
    let config_ini = read_text_file_or_record(addr, SMP_CONFIG_PATH, &mut errors);
    let autotune_ini = read_text_file_or_record(addr, SMP_AUTOTUNE_PATH, &mut errors);
    let debug_log_tail = read_text_file_or_record(addr, SMP_DEBUG_LOG_PATH, &mut errors);

    // List mounted images. SMP creates one subdir per mounted image
    // under /mnt/shadowmnt/. Empty list when nothing is mounted.
    let mounted_images = match list_dir_with_timeout(
        addr,
        SMP_MOUNT_POINT,
        ListDirOptions::default(),
        Some(RPC_TIMEOUT),
    ) {
        Ok(listing) => listing
            .entries
            .iter()
            .filter(|e| e.kind == "dir")
            .map(|e| SmpMountedImage {
                mount_point: format!("{SMP_MOUNT_POINT}/{}", e.name),
                derived_name: derive_image_name(&e.name),
            })
            .collect(),
        Err(e) => {
            // /mnt/shadowmnt may not exist if SMP has never mounted
            // anything; that's not an error worth surfacing.
            let s = e.to_string();
            if !(s.contains("ENOENT") || s.contains("No such file")) {
                errors.push(format!("list /mnt/shadowmnt: {s}"));
            }
            Vec::new()
        }
    };

    Ok(SmpStatus {
        installed,
        running,
        config_ini,
        autotune_ini,
        debug_log_tail,
        mounted_images,
        errors,
    })
}

/// Helper: try to read a small text file, recording errors but never
/// failing the whole call. None when the file doesn't exist; Some
/// when readable; None + errors entry when something else went wrong.
fn read_text_file_or_record(addr: &str, path: &str, errors: &mut Vec<String>) -> Option<String> {
    match fs_read_with_timeout(addr, path, 0, READ_LIMIT_BYTES, Some(RPC_TIMEOUT)) {
        Ok(bytes) => {
            // Files we read here are config files / log tails. UTF-8
            // is overwhelmingly the case; lossy decode is the right
            // fallback for the rare debug-log line with non-UTF-8
            // bytes (kernel printf output).
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(e) => {
            let s = e.to_string();
            if s.contains("ENOENT") || s.contains("No such file") {
                // Not an error — file just hasn't been written yet
                // (e.g. autotune.ini is created lazily on first
                // tuning event).
                None
            } else {
                errors.push(format!("read {path}: {s}"));
                None
            }
        }
    }
}

/// Strip SMP's CRC32 hash suffix from a mount-point dir name.
/// "Outlast2_a3c3fd8b" → "Outlast2".
///
/// SMP's hash suffix is exactly 8 lowercase hex chars (CRC32 in hex).
/// If the name doesn't match that pattern we return it unchanged —
/// future SMP versions might use a different scheme.
fn derive_image_name(dir_name: &str) -> String {
    let len = dir_name.len();
    if len < 10 {
        return dir_name.to_string();
    }
    let (head, tail) = dir_name.split_at(len - 9);
    if !tail.starts_with('_') {
        return dir_name.to_string();
    }
    if !tail[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return dir_name.to_string();
    }
    head.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_strips_hex_suffix() {
        assert_eq!(derive_image_name("Outlast2_a3c3fd8b"), "Outlast2");
        assert_eq!(derive_image_name("My Game_deadbeef"), "My Game");
    }

    #[test]
    fn derive_keeps_name_without_suffix() {
        assert_eq!(derive_image_name("plainname"), "plainname");
        assert_eq!(derive_image_name("foo_notmatching"), "foo_notmatching");
    }

    #[test]
    fn derive_handles_short_names() {
        assert_eq!(derive_image_name("a"), "a");
        assert_eq!(derive_image_name(""), "");
    }
}
