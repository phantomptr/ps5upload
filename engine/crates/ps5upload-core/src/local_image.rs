//! Open a game image on this computer so its files can be edited.
//!
//! See `docs/superpowers/specs/2026-08-20-local-image-editing-design.md`.
//!
//! We deliberately do not implement exFAT. Every desktop OS already ships
//! a driver for it, far better tested than anything we would write, so
//! the job is to attach the image as a block device and let the OS mount
//! it. The user then edits files with the tools they already know, and
//! every byte written goes through the OS driver rather than through us.
//!
//! This module therefore only ever attaches and detaches. It never
//! formats, resizes, or writes to an image itself.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedImage {
    /// Absolute path of the image file.
    pub image: String,
    /// OS device the image is attached to (`/dev/disk4`, `/dev/loop0`).
    pub device: String,
    /// Where the OS mounted it, when it did. Empty when the image
    /// attached but no filesystem was mounted -- which is a real state,
    /// not a failure: an unformatted or unsupported image attaches fine
    /// and mounts nothing.
    #[serde(default)]
    pub mount_point: String,
}

/// Why this platform cannot open images locally, or None if it can.
///
/// Windows is the awkward one: `Mount-DiskImage` handles VHD and ISO but
/// not raw images, so it needs a third-party driver. Saying so up front
/// beats offering a button that fails at the point of use.
pub fn unsupported_reason() -> Option<&'static str> {
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        None
    } else if cfg!(target_os = "windows") {
        Some(
            "Opening an image on Windows needs a third-party driver \
             (Windows can only mount VHD and ISO images by itself). \
             Not supported yet.",
        )
    } else {
        Some("Opening an image locally is not supported on this platform.")
    }
}

// ── Output parsing (pure, and the part most likely to be subtly wrong) ──

/// Device name from `hdiutil attach` output.
///
/// hdiutil prints one line per partition, whitespace-aligned:
///
/// ```text
/// /dev/disk4              GUID_partition_scheme
/// /dev/disk4s1            Microsoft Basic Data         /Volumes/PS5GAME
/// ```
///
/// The first line's first field is the whole-disk device, which is what
/// must be passed to `hdiutil detach`. Detaching a partition instead
/// leaves the disk attached.
pub fn parse_hdiutil_device(output: &str) -> Option<String> {
    for line in output.lines() {
        let first = line.split_whitespace().next()?;
        if first.starts_with("/dev/") {
            return Some(first.to_string());
        }
    }
    None
}

/// Mount point from `hdiutil attach` output, if the OS mounted anything.
///
/// The mount path is the last field on a partition line and may contain
/// spaces ("/Volumes/PS5 GAME"), so it cannot be taken with
/// `split_whitespace`. hdiutil separates columns with tabs.
pub fn parse_hdiutil_mount_point(output: &str) -> Option<String> {
    for line in output.lines() {
        let last = line.rsplit('\t').next().unwrap_or("").trim();
        if last.starts_with('/') && !last.starts_with("/dev/") {
            return Some(last.to_string());
        }
    }
    None
}

/// Device from `udisksctl loop-setup -f <image>`, which prints:
/// `Mapped file image.exfat as /dev/loop0.`
pub fn parse_udisks_loop_device(output: &str) -> Option<String> {
    let idx = output.find(" as /dev/")?;
    let rest = &output[idx + 4..];
    let end = rest
        .find(|c: char| c == '.' || c.is_whitespace())
        .unwrap_or(rest.len());
    let dev = rest[..end].trim();
    if dev.starts_with("/dev/") && dev.len() > 5 {
        Some(dev.to_string())
    } else {
        None
    }
}

/// Mount point from `udisksctl mount -b <dev>`, which prints:
/// `Mounted /dev/loop0 at /media/user/PS5GAME.`
pub fn parse_udisks_mount_point(output: &str) -> Option<String> {
    let idx = output.find(" at /")?;
    let rest = output[idx + 4..].trim_end();
    let rest = rest.strip_suffix('.').unwrap_or(rest);
    if rest.starts_with('/') && rest.len() > 1 {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Does this look like a raw disk image we can attach?
///
/// Conservative on purpose: attaching an arbitrary file is at best a
/// confusing no-op, and this is a user-facing action on a file they
/// picked.
pub fn looks_like_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".exfat")
        || lower.ends_with(".img")
        || lower.ends_with(".image")
        || lower.ends_with(".raw")
}

// ── Operations ──────────────────────────────────────────────────────

use std::process::Command;
use std::sync::Mutex;

/// Images this process attached, so nothing is left behind silently.
/// A forgotten loop device is how somebody later gets "resource busy"
/// on a file they no longer associate with us.
fn attached() -> &'static Mutex<Vec<AttachedImage>> {
    static A: std::sync::OnceLock<Mutex<Vec<AttachedImage>>> = std::sync::OnceLock::new();
    A.get_or_init(|| Mutex::new(Vec::new()))
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Attach an image so the OS mounts it for editing.
///
/// On macOS this needs no elevated privileges: `hdiutil attach` mounts
/// exFAT in Finder as the user. On Linux we prefer `udisksctl`, which
/// is also unprivileged; `losetup` would need root.
pub fn attach(path: &str) -> Result<AttachedImage> {
    if let Some(why) = unsupported_reason() {
        bail!("{why}");
    }
    let abs = std::fs::canonicalize(path).with_context(|| format!("no such image: {path}"))?;
    let abs = abs.to_string_lossy().into_owned();

    if !std::path::Path::new(&abs).is_file() {
        bail!("not a file: {abs}");
    }
    if let Ok(list) = attached().lock() {
        if let Some(existing) = list.iter().find(|a| a.image == abs) {
            // Attaching twice would produce a second device for the same
            // file; hand back the one already open instead.
            return Ok(existing.clone());
        }
    }

    let info = if cfg!(target_os = "macos") {
        let out = run(
            "hdiutil",
            &["attach", "-imagekey", "diskimage-class=CRawDiskImage", &abs],
        )?;
        let device = parse_hdiutil_device(&out)
            .ok_or_else(|| anyhow::anyhow!("could not read the device from hdiutil"))?;
        AttachedImage {
            image: abs.clone(),
            device,
            mount_point: parse_hdiutil_mount_point(&out).unwrap_or_default(),
        }
    } else {
        let out = run("udisksctl", &["loop-setup", "-f", &abs])?;
        let device = parse_udisks_loop_device(&out)
            .ok_or_else(|| anyhow::anyhow!("could not read the loop device from udisksctl"))?;
        // Mounting can legitimately fail (no filesystem, unsupported
        // type). The image is still attached, so report it with an
        // empty mount point rather than leaving a stray loop device.
        let mount_point = run("udisksctl", &["mount", "-b", &device])
            .ok()
            .and_then(|m| parse_udisks_mount_point(&m))
            .unwrap_or_default();
        AttachedImage {
            image: abs.clone(),
            device,
            mount_point,
        }
    };

    if let Ok(mut list) = attached().lock() {
        list.push(info.clone());
    }
    Ok(info)
}

/// Detach an image. Idempotent: a device the user already ejected in
/// Finder is not an error, it is the desired end state.
pub fn detach(device: &str) -> Result<()> {
    if let Some(why) = unsupported_reason() {
        bail!("{why}");
    }
    if !device.starts_with("/dev/") {
        bail!("refusing to detach a device that is not under /dev: {device}");
    }

    let result = if cfg!(target_os = "macos") {
        run("hdiutil", &["detach", device]).map(|_| ())
    } else {
        // Unmount first; an unmounted device is fine to delete.
        let _ = run("udisksctl", &["unmount", "-b", device]);
        run("udisksctl", &["loop-delete", "-b", device]).map(|_| ())
    };

    // Forget it either way: if the detach failed because it was already
    // gone, holding a stale entry helps nobody.
    if let Ok(mut list) = attached().lock() {
        list.retain(|a| a.device != device);
    }
    result
}

/// Everything this process currently has attached.
pub fn status() -> Vec<AttachedImage> {
    attached().lock().map(|l| l.clone()).unwrap_or_default()
}

/// Detach everything we attached. Called on shutdown so a session does
/// not leave devices behind.
pub fn detach_all() {
    let all = status();
    for a in all {
        let _ = detach(&a.device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDIUTIL: &str = "/dev/disk4          \tGUID_partition_scheme          \t\n\
                           /dev/disk4s1        \tMicrosoft Basic Data           \t/Volumes/PS5GAME\n";

    #[test]
    fn hdiutil_device_is_the_whole_disk_not_the_partition() {
        // Detaching the partition would leave the disk attached, which
        // is how a stale loop device gets left behind.
        assert_eq!(parse_hdiutil_device(HDIUTIL).as_deref(), Some("/dev/disk4"));
    }

    #[test]
    fn hdiutil_mount_point_is_read_from_the_partition_line() {
        assert_eq!(
            parse_hdiutil_mount_point(HDIUTIL).as_deref(),
            Some("/Volumes/PS5GAME")
        );
    }

    #[test]
    fn hdiutil_mount_point_keeps_spaces_in_the_volume_name() {
        // "/Volumes/PS5 GAME" is legal and splitting on whitespace would
        // silently truncate it to "/Volumes/PS5".
        let out = "/dev/disk4      \tGUID_partition_scheme\t\n\
                   /dev/disk4s1    \tMicrosoft Basic Data \t/Volumes/PS5 GAME\n";
        assert_eq!(
            parse_hdiutil_mount_point(out).as_deref(),
            Some("/Volumes/PS5 GAME")
        );
    }

    #[test]
    fn hdiutil_unmounted_image_reports_no_mount_point() {
        // Attached but nothing mounted is a real state, not a failure.
        let out = "/dev/disk4          \tGUID_partition_scheme          \t\n";
        assert_eq!(parse_hdiutil_device(out).as_deref(), Some("/dev/disk4"));
        assert_eq!(parse_hdiutil_mount_point(out), None);
    }

    #[test]
    fn udisks_loop_device_is_parsed() {
        assert_eq!(
            parse_udisks_loop_device("Mapped file game.exfat as /dev/loop0.\n").as_deref(),
            Some("/dev/loop0")
        );
    }

    #[test]
    fn udisks_loop_device_handles_a_filename_containing_as() {
        // "as /dev/" is the anchor, so a filename with " as " in it must
        // not derail the parse.
        assert_eq!(
            parse_udisks_loop_device("Mapped file my as game.img as /dev/loop7.\n").as_deref(),
            Some("/dev/loop7")
        );
    }

    #[test]
    fn udisks_mount_point_is_parsed_without_the_trailing_period() {
        assert_eq!(
            parse_udisks_mount_point("Mounted /dev/loop0 at /media/me/PS5GAME.\n").as_deref(),
            Some("/media/me/PS5GAME")
        );
    }

    #[test]
    fn garbage_output_yields_none_rather_than_a_wrong_device() {
        // A wrong device name here would be passed to detach, which is
        // the one place a mistake could affect an unrelated disk.
        for bad in ["", "error: no such file", "Mapped file x as nothing"] {
            assert_eq!(parse_udisks_loop_device(bad), None, "{bad}");
            assert_eq!(parse_hdiutil_device(bad), None, "{bad}");
        }
    }

    #[test]
    fn image_extensions_are_recognised_case_insensitively() {
        for ok in ["a.exfat", "B.IMG", "c.Image", "d.raw"] {
            assert!(looks_like_image(ok), "{ok}");
        }
        for no in ["game.pkg", "notes.txt", "archive.zip", "noext"] {
            assert!(!looks_like_image(no), "{no}");
        }
    }

    #[test]
    fn platform_support_is_stated_not_guessed() {
        let reason = unsupported_reason();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(reason.is_none());
        } else {
            assert!(reason.is_some(), "unsupported platforms must say why");
        }
    }
}
