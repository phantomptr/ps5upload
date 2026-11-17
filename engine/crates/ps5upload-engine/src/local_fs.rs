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
            crate::log_warn!(
                "ignoring PS5UPLOAD_BROWSE_ROOTS entries that are \
                 not a directory the engine can see: {}",
                invalid.join(", ")
            );
        }
        let roots = parse_browse_roots(&raw);
        if !roots.is_empty() {
            return roots;
        }
        crate::log_warn!(
            "PS5UPLOAD_BROWSE_ROOTS named no usable directory; the \
             file browser will open at the home directory instead."
        );
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".to_string());
    // iOS: HOME is the app's container (Documents, Library, tmp); the place people put files is
    // Documents, which the Files app shows. Same root the app's own picker uses (local_fs.rs in
    // the desktop crate).
    if cfg!(target_os = "ios") {
        return vec![format!("{}/Documents", home.trim_end_matches('/'))];
    }
    // Home first, then every other disk this machine has mounted: a second internal disk, a USB
    // drive, a NAS share. Without them the in-app browser (web UI, Docker, a phone using this
    // engine) could only reach the home folder.
    let mut roots = vec![home];
    if !cfg!(target_os = "android") {
        for d in mounted_disks() {
            if !roots.contains(&d) {
                roots.push(d);
            }
        }
    }
    roots
}

/// How long one disk is given to show it can be read before it is left out. Every disk is tried
/// at once, so the whole list waits about this long at most, whatever hangs.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// The disks this machine has mounted, as browse roots: local disks that can be read (each tried
/// on its own thread, at once, for [`PROBE_TIMEOUT`]); network shares unprobed (reading a share
/// whose server is gone can hang for minutes, and listing it later says what is wrong).
fn mounted_disks() -> Vec<String> {
    let found: Vec<(String, bool)> = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/self/mountinfo")
            .map(|t| linux_disk_mounts_kinds(&t))
            .unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        macos_volumes().into_iter().map(|p| (p, false)).collect()
    } else if cfg!(windows) {
        // A drive letter can be a mapped network drive: every one is tried with the timeout.
        (b'C'..=b'Z')
            .map(|c| (format!("{}:\\", c as char), false))
            .collect()
    } else {
        Vec::new()
    };
    let readable = probe_readable(
        found
            .iter()
            .filter(|(_, network)| !network)
            .map(|(p, _)| p.clone())
            .collect(),
        PROBE_TIMEOUT,
    );
    found
        .into_iter()
        .filter(|(p, network)| *network || readable.contains(p))
        .map(|(p, _)| p)
        .collect()
}

/// The folders of `paths` that could be read within `timeout`, all tried at once on threads of
/// their own. One that has not answered by then is left out (its thread is left to finish).
fn probe_readable(paths: Vec<String>, timeout: std::time::Duration) -> Vec<String> {
    probe_with(paths, timeout, |p| std::fs::read_dir(p).is_ok())
}

fn probe_with(
    paths: Vec<String>,
    timeout: std::time::Duration,
    probe: impl Fn(&str) -> bool + Send + Sync + Clone + 'static,
) -> Vec<String> {
    let (tx, rx) = std::sync::mpsc::channel::<(usize, bool)>();
    let mut started = 0usize;
    for (i, p) in paths.iter().enumerate() {
        let (tx, p, probe) = (tx.clone(), p.clone(), probe.clone());
        let spawned = std::thread::Builder::new()
            .name("disk-probe".into())
            .spawn(move || {
                let _ = tx.send((i, probe(&p)));
            });
        if spawned.is_ok() {
            started += 1;
        }
    }
    drop(tx);
    let deadline = std::time::Instant::now() + timeout;
    let mut ok = vec![false; paths.len()];
    for _ in 0..started {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok((i, readable)) => ok[i] = readable,
            Err(_) => break,
        }
    }
    paths
        .into_iter()
        .zip(ok)
        .filter_map(|(p, ok)| ok.then_some(p))
        .collect()
}

/// File systems that hold people's files (disks, USB drives, network shares); anything else in
/// the mount table (proc, tmpfs, overlay, squashfs snaps, cgroups...) is the system's own.
const DISK_FS: &[&str] = &[
    "ext2",
    "ext3",
    "ext4",
    "btrfs",
    "xfs",
    "f2fs",
    "zfs",
    "jfs",
    "reiserfs",
    "vfat",
    "exfat",
    "ntfs",
    "ntfs3",
    "fuseblk",
    "hfsplus",
    "apfs",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "9p",
    "virtiofs",
    "fuse.sshfs",
    "fuse.rclone",
    "smbfs",
    "afpfs",
    "davfs",
    "fuse.davfs2",
];

/// The network file systems of [`DISK_FS`]: listed without being read first (a share whose
/// server is gone can hang a read for minutes).
const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "9p",
    "fuse.sshfs",
    "fuse.rclone",
    "smbfs",
    "afpfs",
    "davfs",
    "fuse.davfs2",
];

/// Mount points under these hold the system, not files to send.
const SYSTEM_MOUNTS: &[&str] = &[
    "/boot",
    "/efi",
    "/snap",
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/var/lib",
    "/var/snap",
    "/var/log",
    "/usr",
    "/etc",
    "/opt",
    "/nix",
    "/tmp",
    "/var/tmp",
];

/// The disk mount points in a Linux `mountinfo` table, in table order, without `/` (home is
/// already the first root) and without the system's own mounts. `/run/media/<user>/<disk>` (where
/// desktops mount USB drives) is kept although `/run` is not.
#[cfg(test)]
fn linux_disk_mounts(mountinfo: &str) -> Vec<String> {
    linux_disk_mounts_kinds(mountinfo)
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

/// [`linux_disk_mounts`] with whether each is a network share ([`NETWORK_FS`]).
fn linux_disk_mounts_kinds(mountinfo: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for line in mountinfo.lines() {
        // "<id> <parent> <maj:min> <root> <mount point> <options> [optional...] - <fstype> <source> <super options>"
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fstype = right.split_whitespace().next().unwrap_or("");
        let Some(point) = left.split_whitespace().nth(4) else {
            continue;
        };
        let point = unescape_mount(point);
        if !DISK_FS.contains(&fstype) || point == "/" {
            continue;
        }
        let system = SYSTEM_MOUNTS
            .iter()
            .any(|s| point == *s || point.starts_with(&format!("{s}/")));
        if system && !point.starts_with("/run/media/") {
            continue;
        }
        if !out.iter().any(|(p, _)| *p == point) {
            out.push((point, NETWORK_FS.contains(&fstype)));
        }
    }
    out
}

/// `mountinfo` writes a space, tab, newline and backslash in a path as `\040`, `\011`, `\012`
/// and `\134`.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The disks under `/Volumes`, less the link back to the start-up disk (`/Volumes/Macintosh HD`
/// points at `/`).
fn macos_volumes() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir("/Volumes") else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| {
            e.file_type()
                .map(|t| t.is_dir() && !t.is_symlink())
                .unwrap_or(false)
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
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
    fn linux_mount_table_gives_the_disks_not_the_system() {
        let table = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
23 22 0:5 / /proc rw - proc proc rw
24 22 259:1 / /boot/efi rw - vfat /dev/nvme0n1p1 rw
25 22 8:1 / /mnt/data rw - ext4 /dev/sda1 rw
26 22 8:17 / /run/media/alice/MY\\040STICK rw - exfat /dev/sdb1 rw
27 22 0:40 / /run/user/1000 rw - tmpfs tmpfs rw
28 22 7:3 / /snap/core/123 ro - squashfs /dev/loop3 ro
29 22 0:50 / /srv/nas rw - cifs //nas/share rw
30 22 0:51 / /var/lib/docker/overlay2/x/merged rw - overlay overlay rw
31 22 8:1 / /mnt/data rw - ext4 /dev/sda1 rw
32 22 0:52 / /home/alice/games rw - btrfs /dev/sdc1 rw
";
        assert_eq!(
            linux_disk_mounts(table),
            vec![
                "/mnt/data".to_string(),
                "/run/media/alice/MY STICK".to_string(),
                "/srv/nas".to_string(),
                "/home/alice/games".to_string(),
            ]
        );
    }

    #[test]
    fn network_shares_are_told_apart() {
        let table = "\
25 22 8:1 / /mnt/data rw - ext4 /dev/sda1 rw
29 22 0:50 / /srv/nas rw - cifs //nas/share rw
30 22 0:51 / /mnt/nfs rw - nfs4 nas:/x rw
31 22 0:52 / /home/a/remote rw - fuse.sshfs a@b:/ rw
";
        assert_eq!(
            linux_disk_mounts_kinds(table),
            vec![
                ("/mnt/data".to_string(), false),
                ("/srv/nas".to_string(), true),
                ("/mnt/nfs".to_string(), true),
                ("/home/a/remote".to_string(), true),
            ]
        );
    }

    #[test]
    fn a_disk_that_hangs_is_left_out_without_holding_the_others() {
        let t0 = std::time::Instant::now();
        let got = probe_with(
            vec![
                "fast".into(),
                "hangs".into(),
                "unreadable".into(),
                "also".into(),
            ],
            std::time::Duration::from_millis(200),
            |p| {
                if p == "hangs" {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
                p != "unreadable"
            },
        );
        assert_eq!(got, vec!["fast".to_string(), "also".to_string()]);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn mount_paths_are_unescaped() {
        assert_eq!(unescape_mount("/a\\040b\\134c"), "/a b\\c");
        assert_eq!(unescape_mount("/plain"), "/plain");
        assert_eq!(unescape_mount("/end\\04"), "/end\\04");
    }

    #[test]
    fn browse_roots_unset_starts_at_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        let roots = storage_roots();
        assert_eq!(
            roots.first(),
            home_default().first(),
            "home is the first root"
        );
        let unique: std::collections::HashSet<&String> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len(), "each root once");
    }

    #[test]
    fn browse_roots_blank_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PS5UPLOAD_BROWSE_ROOTS", "   ");
        let roots = storage_roots();
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        assert_eq!(roots.first(), home_default().first());
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

    #[test]
    fn browse_roots_warns_through_the_engine_log_not_stderr() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let good = tmp_dir("logged");
        let bad = format!("/no/such/ps5upload-logged-root-{}", std::process::id());
        std::env::set_var("PS5UPLOAD_BROWSE_ROOTS", format!("{good},{bad}"));
        let roots = storage_roots();
        std::env::remove_var("PS5UPLOAD_BROWSE_ROOTS");
        assert_eq!(roots, vec![good]);
        assert!(
            crate::engine_log::tail_since(0)
                .iter()
                .any(|e| e.level == "warn" && e.msg.contains(&bad)),
            "the dropped entry must be named in the engine log"
        );
    }
}
