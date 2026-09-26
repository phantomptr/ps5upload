//! Check a disk image out of ShadowMount+ so it can be edited in place.
//!
//! # Why this exists
//!
//! ShadowMount+ mounts every image it finds in a scan root **read-only**
//! (`mount_read_only=1` is its default), so files inside `/mnt/shadowmnt/...`
//! cannot be changed. The obvious workaround — unmount SMP's mount and
//! re-mount the image read-write ourselves — does not work: SMP keeps an
//! internal slot table and sweeps it every `scan_interval_seconds` (15 by
//! default). When it finds a slot whose mount has gone it logs
//!
//! ```text
//! [IMG][UNKNOWN] mount lost, retrying: /data/homebrew/X.exfat -> /mnt/shadowmnt/X_ca51a0d7
//! ```
//!
//! and immediately re-attaches the image to a fresh LVD unit. Within 15
//! seconds you would have two attachments of one image file, one of them
//! writable — a corruption path, not a race worth tuning.
//!
//! SMP has no control API. The one thing that reliably makes it let go is
//! taking the source out of every scan root, which is what this module does.
//!
//! # The sequence (verified on hardware, FW 5.10)
//!
//! Check out:
//!   1. resolve a staging dir on the image's **own volume** and `mkdir` it
//!   2. `rename` the image into it — instant even for 150 GB, and the
//!      same-volume requirement is not cosmetic (see [`staging_dir_for`])
//!   3. wait for SMP's sweep to notice the source is gone and unmount it
//!   4. `mount` the image read-write at the caller's chosen mount point
//!
//! Check in:
//!   5. `unmount` — this is what flushes the edits into the image file
//!   6. `rename` the image back to where it came from
//!   7. SMP's next sweep re-mounts and re-registers it, edits included
//!
//! # Crash safety
//!
//! Between steps 2 and 6 the image sits somewhere SMP cannot see it, which
//! means the user's game has silently disappeared from their console. If the
//! app dies, the console reboots, or the user just closes the window, nothing
//! would put it back. So the checkout is journalled **on the console** at
//! [`CHECKOUT_STATE_PATH`] before the move happens, and [`read_state`] lets
//! the app offer to finish an interrupted edit on the next connect. The
//! journal lives on the PS5 rather than the desktop deliberately: the
//! inconsistency is on the console, and the user may well come back to it
//! from a different machine.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::diagnostics::fs_write_bytes;
use crate::fs_ops::{fs_mkdir, fs_mount, fs_move, fs_read, fs_unmount, MountResult};
use crate::volumes::{list_volumes, VolumeList};

/// Directory holding the checkout journal. Fixed, on internal storage:
/// recovery has to find it with no other state, and `/data` is the one volume
/// that is always present. Note this is deliberately NOT the staging dir —
/// that follows the image's own volume (see [`staging_dir_for`]), so for an
/// image on `/mnt/usb0` the two are on different drives and BOTH have to be
/// created.
pub const CHECKOUT_STATE_DIR: &str = "/data/ps5upload/editing";

/// Where the checkout journal lives on the console.
pub const CHECKOUT_STATE_PATH: &str = "/data/ps5upload/editing/checkout.json";

/// Directory (relative to a volume root) images are staged into while checked
/// out. Deliberately NOT under `homebrew/`, which is what SMP scans.
const STAGING_SUBDIR: &str = "ps5upload/editing";

/// SMP's scan roots when `config.ini` names none of its own. Derived from the
/// `<volume>/homebrew` convention its logs show on both test consoles.
const DEFAULT_SCAN_SUBDIR: &str = "homebrew";

/// How long to wait for SMP to notice a source has left its scan roots and
/// drop the mount. Its sweep is every 15 s by default and the config caps
/// `scan_interval_seconds` at 3600, but a user who has set a very long
/// interval should get a clear timeout rather than an unbounded hang. 90 s
/// covers the default sweep several times over; measured release on FW 5.10
/// was under 20 s.
const SMP_RELEASE_TIMEOUT_SECS: u64 = 90;

/// How often to re-probe SMP while waiting for it to let go.
const SMP_RELEASE_POLL_SECS: u64 = 3;

/// One image checked out for editing. At most one is active at a time —
/// a second concurrent checkout would need a second journal and buys nothing,
/// since editing is a foreground activity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckoutState {
    /// Where the image lives *right now* (inside the staging dir).
    pub staged_path: String,
    /// Where it must be put back for SMP to pick it up again.
    pub original_path: String,
    /// Where it is mounted read-write for editing.
    pub mount_point: String,
    /// Best-effort title id, for the UI. Empty when unknown.
    #[serde(default)]
    pub title_id: String,
    /// Unix ms when the checkout started, so the UI can say how long a
    /// recovered session has been open.
    pub started_at_ms: u64,
}

/// Parse SMP's configured scan roots out of `config.ini`.
///
/// `scanpath=` is repeatable, and SMP's own comment is explicit that "if at
/// least one scanpath=... is present, only these paths are used" — so a single
/// custom root REPLACES the built-in list rather than adding to it. Returns
/// None when the file names no roots, which means SMP is using its defaults.
pub fn parse_scan_paths(config_ini: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for line in config_ini.lines() {
        let line = line.trim();
        // Commented-out lines are the template's documentation, not config.
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("scanpath") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            out.push(value.trim_end_matches('/').to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// True when `path` sits inside (or is) one of `roots`.
pub fn is_under_any(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        let root = root.trim_end_matches('/');
        if root.is_empty() {
            return false;
        }
        path == root || path.starts_with(&format!("{root}/"))
    })
}

/// Effective scan roots for a console: SMP's configured list, or the
/// `<volume>/homebrew` default for every volume it could see.
pub fn effective_scan_roots(config_ini: Option<&str>, volumes: &VolumeList) -> Vec<String> {
    if let Some(configured) = config_ini.and_then(parse_scan_paths) {
        return configured;
    }
    volumes
        .volumes
        .iter()
        .filter(|v| !v.path.is_empty())
        .map(|v| format!("{}/{DEFAULT_SCAN_SUBDIR}", v.path.trim_end_matches('/')))
        .collect()
}

/// Pick the staging directory for `image_path`.
///
/// This MUST land on the same volume as the image. The move is a `rename(2)`,
/// and renaming across mounts on this firmware is not merely slow — it panics
/// the kernel (the payload guards on `st_dev` for exactly this reason). Using
/// the image's own volume root makes same-device structural rather than
/// something we hope holds.
pub fn staging_dir_for(image_path: &str, volumes: &VolumeList) -> Result<String> {
    let volume = volumes.find_for_path(image_path).with_context(|| {
        format!("no PS5 volume covers {image_path}; refusing to stage an edit off-volume")
    })?;
    Ok(format!(
        "{}/{STAGING_SUBDIR}",
        volume.path.trim_end_matches('/')
    ))
}

/// Read the checkout journal, if one exists. `Ok(None)` means "nothing is
/// checked out" — including the common case where the file has never been
/// written. A malformed journal is an error, not a None: silently ignoring it
/// would strand an image in staging with no way back.
pub fn read_state(addr: &str) -> Result<Option<CheckoutState>> {
    let bytes = match fs_read(addr, CHECKOUT_STATE_PATH, 0, 64 * 1024) {
        Ok(b) => b,
        Err(e) => {
            if is_not_found(&e.to_string()) {
                return Ok(None);
            }
            return Err(e).context("read the edit-session journal");
        }
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let state: CheckoutState = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the edit-session journal at {CHECKOUT_STATE_PATH}"))?;
    Ok(Some(state))
}

fn write_state(addr: &str, state: &CheckoutState) -> Result<()> {
    let body = serde_json::to_vec_pretty(state)?;
    fs_write_bytes(addr, CHECKOUT_STATE_PATH, &body, false)
        .context("journal the edit session on the console")?;
    Ok(())
}

fn clear_state(addr: &str) -> Result<()> {
    // Truncate rather than delete: an empty file reads back as "nothing
    // checked out" and leaves the path in place, so the next checkout doesn't
    // have to care whether the directory survived.
    fs_write_bytes(addr, CHECKOUT_STATE_PATH, b"", false)
        .context("clear the edit-session journal")?;
    Ok(())
}

/// The payload and the io error chain both encode "no such file" differently;
/// mirror the same tolerance `smp.rs` applies to its probes.
fn is_not_found(message: &str) -> bool {
    if message.contains("ENOENT") || message.contains("No such file") {
        return true;
    }
    if message.contains("fs_read_stat_failed") {
        return true;
    }
    message.split("_errno_").skip(1).any(|rest| {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits == "2"
    })
}

/// Is `image_path` currently mounted by ShadowMount+?
///
/// `Ok(None)` is a POSITIVE statement — we asked ShadowMount+ and it is not
/// holding this image. A failed probe returns `Err`, never `Ok(None)`:
/// treating "couldn't tell" as "released" would let us mount the image
/// read-write while SMP still has its own attachment open, which is the exact
/// double-attach this whole module exists to avoid.
fn smp_mount_for(addr: &str, image_path: &str) -> Result<Option<String>> {
    let basename = image_basename(image_path);
    let status = crate::smp::collect_status(addr)
        .context("ask ShadowMount+ what it currently has mounted")?;
    // collect_status degrades rather than failing: a listing error lands in
    // `errors` and leaves `mounted_images` empty, which would read as "nothing
    // mounted". Only trust an empty list when the probe reported no problems.
    if !status.errors.is_empty() {
        bail!(
            "could not read ShadowMount+'s mount list ({}); refusing to assume it has \
             released the image",
            status.errors.join("; ")
        );
    }
    Ok(status
        .mounted_images
        .into_iter()
        .find(|m| m.derived_name == basename)
        .map(|m| m.mount_point))
}

/// `/data/homebrew/PPSA09016.exfat` → `PPSA09016`. SMP names its mount points
/// `<this>_<crc32>`, which is the only link it keeps between a mount and the
/// image it came from.
pub fn image_basename(path: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    match leaf.rfind('.') {
        Some(i) if i > 0 => leaf[..i].to_string(),
        _ => leaf.to_string(),
    }
}

/// Wait until SMP no longer has the image mounted.
///
/// `probe` returns `Ok(None)` for "confirmed released", `Ok(Some(mount))` for
/// "still mounted", and `Err` for "couldn't tell". Split out from the RPC so
/// the loop's decisions — especially the one that must NOT treat a failed
/// probe as success — are testable without a console.
fn await_release<F>(
    mut probe: F,
    timeout: std::time::Duration,
    poll: std::time::Duration,
    mut sleep: impl FnMut(std::time::Duration),
    now: impl Fn() -> std::time::Instant,
) -> Result<()>
where
    F: FnMut() -> Result<Option<String>>,
{
    let deadline = now() + timeout;
    loop {
        // A probe failure is "unknown", not "released" — keep waiting and let
        // the deadline surface it, rather than racing SMP on a guess.
        let blocker = match probe() {
            Ok(None) => return Ok(()),
            Ok(Some(mount_point)) => mount_point,
            Err(e) => format!("(could not tell — {e:#})"),
        };
        if now() >= deadline {
            bail!(
                "ShadowMount+ still has this image mounted at {blocker} {}s after it was \
                 moved out of its scan folders. It may be configured with a very long scan \
                 interval, or the game may still be running. The image has been left in \
                 staging — finish the edit session to put it back.",
                timeout.as_secs()
            );
        }
        sleep(poll);
    }
}

/// Wait until SMP no longer has `image_path` mounted.
fn wait_for_smp_release(addr: &str, image_path: &str) -> Result<()> {
    await_release(
        || smp_mount_for(addr, image_path),
        std::time::Duration::from_secs(SMP_RELEASE_TIMEOUT_SECS),
        std::time::Duration::from_secs(SMP_RELEASE_POLL_SECS),
        std::thread::sleep,
        std::time::Instant::now,
    )
}

/// Check `image_path` out of ShadowMount+ and mount it read-write at
/// `mount_point`.
///
/// `title_id` is carried through to the journal for the UI only; pass an empty
/// string when it isn't known.
pub fn begin(
    addr: &str,
    image_path: &str,
    mount_point: &str,
    title_id: &str,
) -> Result<(CheckoutState, MountResult)> {
    if let Some(existing) = read_state(addr)? {
        bail!(
            "{} is already checked out for editing (mounted at {}). Finish that edit session \
             before starting another.",
            existing.original_path,
            existing.mount_point
        );
    }

    let volumes = list_volumes(addr).context("list PS5 volumes to place the staging folder")?;
    let staging_dir = staging_dir_for(image_path, &volumes)?;
    let staged_path = format!("{staging_dir}/{}", leaf_of(image_path));

    // Refuse to stage into a folder SMP scans — it would just re-adopt the
    // image from its new home and we'd be back to fighting it.
    let config_ini = crate::smp::collect_status(addr)
        .ok()
        .and_then(|s| s.config_ini);
    let scan_roots = effective_scan_roots(config_ini.as_deref(), &volumes);
    if is_under_any(&staged_path, &scan_roots) {
        bail!(
            "the staging folder {staging_dir} is inside a ShadowMount+ scan root; \
             it would re-adopt the image from there. Remove that scanpath from \
             /data/shadowmount/config.ini, or edit this image from a different volume."
        );
    }
    // Same reasoning in the other direction: mounting inside a scan root gives
    // SMP a second copy of the game to find.
    if is_under_any(mount_point, &scan_roots) {
        bail!(
            "{mount_point} is inside a ShadowMount+ scan root. Pick a mount point outside \
             {}, or ShadowMount+ will try to mount and register the edit session too.",
            scan_roots.join(", ")
        );
    }

    fs_mkdir(addr, &staging_dir).with_context(|| format!("create staging folder {staging_dir}"))?;
    // The journal lives on /data regardless of which volume the image is on,
    // so its directory is a SEPARATE mkdir. Skipping it worked only for images
    // that happened to live on /data (where staging_dir is the same folder);
    // for an image on /mnt/usb0 or /mnt/ext1 the journal write failed with
    // `FS_WRITE_BYTES failed: open_failed` and no edit session could start.
    fs_mkdir(addr, CHECKOUT_STATE_DIR)
        .with_context(|| format!("create the edit-session journal folder {CHECKOUT_STATE_DIR}"))?;

    let state = CheckoutState {
        staged_path: staged_path.clone(),
        original_path: image_path.to_string(),
        mount_point: mount_point.to_string(),
        title_id: title_id.to_string(),
        started_at_ms: now_ms(),
    };
    // Journal BEFORE the move. If we die between here and the move, recovery
    // finds a journal whose staged_path doesn't exist yet — `finish` treats
    // that as "already back where it belongs" and just clears the journal.
    write_state(addr, &state)?;

    fs_move(addr, image_path, &staged_path).with_context(|| {
        format!("move {image_path} out of ShadowMount+'s scan folders into {staging_dir}")
    })?;

    // From here on a failure must not leave the image stranded, so unwind the
    // move before returning.
    wait_for_smp_release(addr, &staged_path)?;
    match fs_mount(addr, &staged_path, None, Some(mount_point), false) {
        Ok(mount) => Ok((state, mount)),
        Err(e) => {
            // Put it straight back — a failed mount should not cost the user
            // their game. Best-effort: if this also fails the journal is still
            // on the console and recovery can retry.
            let _ = fs_move(addr, &staged_path, image_path);
            let _ = clear_state(addr);
            Err(e).context("mount the checked-out image read-write")
        }
    }
}

/// Finish an edit session: unmount, put the image back, and let SMP re-adopt
/// it. Safe to call for a session recovered from the journal after a crash.
pub fn finish(addr: &str) -> Result<CheckoutState> {
    let state = read_state(addr)?.context("no image is checked out for editing on this console")?;

    // Unmount is what flushes the edits into the image file, so a failure here
    // is fatal to the whole operation — moving the image back with writes
    // still in flight is how you corrupt it.
    if let Err(e) = fs_unmount(addr, &state.mount_point) {
        let msg = e.to_string();
        if msg.contains("fs_unmount_not_our_mount") {
            // Already unmounted (e.g. the console rebooted mid-session). The
            // image is intact on disk; carry on and put it back.
        } else if msg.contains("fs_unmount_busy") {
            return Err(e).context(
                "the edited image is still in use — close anything reading it on the PS5 \
                 (including a running game) and finish the edit session again",
            );
        } else {
            return Err(e).context("unmount the edited image before putting it back");
        }
    }

    // The journal is written BEFORE the image is moved, so a crash in that
    // narrow window leaves a journal describing a move that never happened.
    // Recovering from that must not fail: if the image is already back where
    // it belongs, the session is simply over.
    if !path_exists(addr, &state.staged_path) && path_exists(addr, &state.original_path) {
        clear_state(addr)?;
        return Ok(state);
    }

    fs_move(addr, &state.staged_path, &state.original_path).with_context(|| {
        format!(
            "put {} back so ShadowMount+ can pick it up again",
            state.original_path
        )
    })?;
    clear_state(addr)?;
    Ok(state)
}

/// Best-effort existence probe. A zero-byte read either succeeds (the file is
/// there) or reports ENOENT; anything else — a busy port, a timeout — is
/// deliberately reported as "exists" so a flaky probe never talks `finish`
/// out of attempting the move that puts the user's game back.
fn path_exists(addr: &str, path: &str) -> bool {
    match fs_read(addr, path, 0, 1) {
        Ok(_) => true,
        Err(e) => !is_not_found(&e.to_string()),
    }
}

fn leaf_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::Volume;

    fn vol(path: &str) -> Volume {
        Volume {
            path: path.to_string(),
            mount_from: String::new(),
            fs_type: "exfatfs".into(),
            total_bytes: 0,
            free_bytes: 0,
            writable: true,
            is_placeholder: false,
            source_image: String::new(),
            safety_reserve_bytes: 0,
            allocatable_bytes: 0,
        }
    }

    fn volumes(paths: &[&str]) -> VolumeList {
        VolumeList {
            volumes: paths.iter().map(|p| vol(p)).collect(),
        }
    }

    #[test]
    fn scan_paths_ignore_the_commented_template() {
        // Every line in SMP's shipped config.ini is commented out. Treating
        // those as configured roots would make us think `/data/homebre` (the
        // truncated example) was a real scan root.
        let ini = "# scanpath=/data/homebrew\n# scanpath=/mnt/usb0/games\n";
        assert_eq!(parse_scan_paths(ini), None);
    }

    #[test]
    fn scan_paths_read_repeated_entries() {
        let ini = "debug=1\nscanpath=/data/homebrew\nscanpath = /mnt/usb0/games/\n";
        assert_eq!(
            parse_scan_paths(ini),
            Some(vec![
                "/data/homebrew".to_string(),
                "/mnt/usb0/games".to_string(),
            ])
        );
    }

    #[test]
    fn effective_roots_fall_back_to_the_per_volume_default() {
        let vols = volumes(&["/data", "/mnt/ext1", "/mnt/usb0"]);
        assert_eq!(
            effective_scan_roots(Some("# scanpath=/x\n"), &vols),
            vec![
                "/data/homebrew".to_string(),
                "/mnt/ext1/homebrew".to_string(),
                "/mnt/usb0/homebrew".to_string(),
            ]
        );
    }

    #[test]
    fn configured_roots_replace_the_defaults() {
        // SMP's own comment: "If at least one scanpath=... is present, only
        // these paths are used." Adding to the defaults instead would make us
        // refuse staging folders that are actually fine.
        let vols = volumes(&["/data", "/mnt/usb0"]);
        assert_eq!(
            effective_scan_roots(Some("scanpath=/mnt/usb0/games\n"), &vols),
            vec!["/mnt/usb0/games".to_string()]
        );
    }

    #[test]
    fn under_any_matches_only_whole_components() {
        let roots = vec!["/data/homebrew".to_string()];
        assert!(is_under_any("/data/homebrew", &roots));
        assert!(is_under_any("/data/homebrew/X.exfat", &roots));
        // A sibling whose name merely starts with the root must not match, or
        // we'd refuse a perfectly good staging folder.
        assert!(!is_under_any("/data/homebrew-backup/X.exfat", &roots));
        assert!(!is_under_any("/data/ps5upload/editing/X.exfat", &roots));
    }

    #[test]
    fn staging_lands_on_the_images_own_volume() {
        // Cross-volume staging would make the move a cross-device rename,
        // which panics the kernel on this firmware.
        let vols = volumes(&["/data", "/mnt/usb0"]);
        assert_eq!(
            staging_dir_for("/mnt/usb0/homebrew/PPSA09016.exfat", &vols).unwrap(),
            "/mnt/usb0/ps5upload/editing"
        );
        assert_eq!(
            staging_dir_for("/data/homebrew/PPSA09016.exfat", &vols).unwrap(),
            "/data/ps5upload/editing"
        );
    }

    #[test]
    fn the_journal_folder_is_not_assumed_to_be_the_staging_folder() {
        // These coincide only for images on /data. An image on an external
        // drive stages onto THAT drive while the journal stays on /data, so
        // `begin` has to create both — assuming one implied the other meant
        // every edit of a USB/ext-hosted image died on
        // `FS_WRITE_BYTES failed: open_failed`.
        let vols = volumes(&["/data", "/mnt/usb0", "/mnt/ext1"]);
        assert!(CHECKOUT_STATE_PATH.starts_with(CHECKOUT_STATE_DIR));

        for (image, expected_staging) in [
            ("/mnt/usb0/homebrew/X.exfat", "/mnt/usb0/ps5upload/editing"),
            ("/mnt/ext1/homebrew/X.exfat", "/mnt/ext1/ps5upload/editing"),
        ] {
            let staging = staging_dir_for(image, &vols).unwrap();
            assert_eq!(staging, expected_staging);
            assert_ne!(
                staging, CHECKOUT_STATE_DIR,
                "an off-/data image must not be assumed to share the journal folder",
            );
        }

        // …and for a /data image they DO coincide, which is why this went
        // unnoticed: the only image tested happened to live there.
        assert_eq!(
            staging_dir_for("/data/homebrew/X.exfat", &vols).unwrap(),
            CHECKOUT_STATE_DIR,
        );
    }

    #[test]
    fn staging_refuses_a_path_no_volume_covers() {
        let vols = volumes(&["/data"]);
        assert!(staging_dir_for("/mnt/usb0/homebrew/X.exfat", &vols).is_err());
    }

    #[test]
    fn basename_strips_only_the_image_extension() {
        assert_eq!(
            image_basename("/data/homebrew/PPSA09016.exfat"),
            "PPSA09016"
        );
        assert_eq!(
            image_basename("/data/homebrew/PPSA01285.ffpkg"),
            "PPSA01285"
        );
        assert_eq!(image_basename("/data/homebrew/v1.2.exfat"), "v1.2");
        assert_eq!(image_basename("noslash.exfat"), "noslash");
    }

    /// Drive `await_release` with a scripted probe and a virtual clock, so the
    /// loop's behaviour is checked without a console or real sleeping.
    fn run_await(script: Vec<Result<Option<String>>>) -> (Result<()>, usize) {
        use std::cell::Cell;
        use std::rc::Rc;
        // The last scripted result repeats once the script runs out, so a
        // timeout test keeps seeing the condition it is timing out on.
        let mut it = script.into_iter().peekable();
        let mut last = "no probe scripted".to_string();
        let calls = Rc::new(Cell::new(0usize));
        let c = calls.clone();
        // Virtual time: every simulated sleep advances the clock by the poll
        // interval, so a probe that never says "released" hits the deadline in
        // a bounded number of iterations instead of blocking the test.
        let elapsed = Rc::new(Cell::new(std::time::Duration::ZERO));
        let e_sleep = elapsed.clone();
        let e_now = elapsed.clone();
        let base = std::time::Instant::now();
        let out = await_release(
            move || {
                c.set(c.get() + 1);
                match it.next() {
                    Some(Ok(v)) => {
                        last = v.clone().unwrap_or_default();
                        Ok(v)
                    }
                    Some(Err(e)) => {
                        last = e.to_string();
                        Err(e)
                    }
                    None => Err(anyhow::anyhow!("{last}")),
                }
            },
            std::time::Duration::from_secs(9),
            std::time::Duration::from_secs(3),
            move |d| e_sleep.set(e_sleep.get() + d),
            move || base + e_now.get(),
        );
        (out, calls.get())
    }

    #[test]
    fn release_returns_as_soon_as_smp_confirms_it_let_go() {
        let (out, calls) = run_await(vec![Ok(None)]);
        assert!(out.is_ok());
        assert_eq!(calls, 1);
    }

    #[test]
    fn release_keeps_waiting_while_smp_still_holds_the_mount() {
        let (out, calls) = run_await(vec![
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(None),
        ]);
        assert!(out.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_failed_probe_is_never_mistaken_for_a_release() {
        // This is the safety-critical case: if "couldn't ask ShadowMount+"
        // read as "ShadowMount+ let go", we would mount the image read-write
        // while SMP still had its own attachment open — two writers on one
        // image file.
        let (out, calls) = run_await(vec![
            Err(anyhow::anyhow!("connection reset")),
            Err(anyhow::anyhow!("connection reset")),
            Ok(None),
        ]);
        assert!(out.is_ok(), "should recover once the probe works again");
        assert_eq!(calls, 3);
    }

    #[test]
    fn release_times_out_rather_than_waiting_forever() {
        // More entries than the 9s/3s budget allows, so every iteration sees
        // a genuine "still mounted" rather than the script running dry.
        let (out, _) = run_await(vec![
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
            Ok(Some("/mnt/shadowmnt/X_deadbeef".into())),
        ]);
        let err = out.unwrap_err().to_string();
        assert!(err.contains("still has this image mounted"), "{err}");
        assert!(err.contains("X_deadbeef"), "{err}");
        // The user needs to know the image is recoverable, not just that it failed.
        assert!(err.contains("finish the edit session"), "{err}");
    }

    #[test]
    fn a_probe_that_never_recovers_times_out_with_the_probe_error() {
        let (out, _) = run_await(vec![Err(anyhow::anyhow!("mgmt port refused"))]);
        let err = out.unwrap_err().to_string();
        assert!(err.contains("could not tell"), "{err}");
    }

    #[test]
    fn not_found_probe_semantics_favour_attempting_the_move() {
        // `path_exists` can't be unit-tested without a console, but the
        // predicate it leans on can: everything that is NOT a definite
        // "missing" must read as present, so a flaky probe leaves `finish`
        // attempting the move rather than skipping it and stranding the image.
        assert!(is_not_found("fs_read_open_errno_2"));
        assert!(!is_not_found("timed out"));
        assert!(!is_not_found("Connection refused"));
    }

    #[test]
    fn journal_round_trips_through_json() {
        let state = CheckoutState {
            staged_path: "/data/ps5upload/editing/X.exfat".into(),
            original_path: "/data/homebrew/X.exfat".into(),
            mount_point: "/data/ps5upload/mnt/x".into(),
            title_id: "PPSA09016".into(),
            started_at_ms: 1_787_000_000_000,
        };
        let encoded = serde_json::to_vec(&state).unwrap();
        let decoded: CheckoutState = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(state, decoded);
    }

    #[test]
    fn not_found_detection_covers_every_encoding() {
        assert!(is_not_found("ENOENT"));
        assert!(is_not_found("No such file or directory (os error 2)"));
        assert!(is_not_found(
            "payload rejected FS_READ(/data/ps5upload/editing/checkout.json): fs_read_open_errno_2"
        ));
        assert!(is_not_found("fs_read_stat_failed"));
        // A permission error must NOT read as "nothing checked out" — that
        // would hide a live edit session and let a second one start.
        assert!(!is_not_found("fs_read_open_errno_13"));
        assert!(!is_not_found("connection reset"));
    }
}
