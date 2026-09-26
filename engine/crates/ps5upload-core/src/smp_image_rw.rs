//! ShadowMount+ read-write edit sessions via `image_rw=`.
//!
//! Replaces the rename-out-of-the-scan-root dance in [`crate::smp_checkout`]
//! for the case where all we need is to WRITE INTO an image SMP already has
//! mounted (backporting: patch `eboot.bin`'s SDK pair, drop `fakelib/`).
//!
//! SMP's own config supports this directly:
//!
//! ```ini
//! image_rw=<image_filename>   # per-image, repeatable, matched on basename
//! ```
//!
//! Verified on hardware (Pro, FW 9.60, SMP v1.6beta16): SMP logs
//! `[CFG] Image mode override: /data/homebrew/PPSA30528.exfat -> rw`, and both
//! `fs/mkdir` and an FTP write into `/mnt/shadowmnt/<...>` then succeed. The
//! image never leaves its scan root, so there is no 20-second release race and
//! nothing to recover if the host dies mid-edit — only the config to undo.
//!
//! The rule is applied when an image is MOUNTED, so two things follow, both
//! measured rather than assumed:
//!
//!  1. `config.ini` is live-watched — SMP has a config watcher and reloads within
//!     seconds of a write (`[CFG] runtime config reloaded`, plus a console
//!     notification). No restart, and in particular no dependency on the SMP ELF
//!     existing on disk: `/data/pldmgr/payloads/ShadowMountPlus/*.elf` is an
//!     artifact of one autoloader setup, and users who load SMP from a webkit
//!     page have no copy at all.
//!  2. A reload does NOT re-mount images that are already mounted. To apply the
//!     rule we make the source blip: rename it WITHIN ITS OWN DIRECTORY, wait for
//!     `[IMG][LVD] Source removed, unmounting`, then rename back. SMP re-mounts
//!     it fresh and logs `[CFG] Image mode override: <path> -> rw`.
//!
//! The blip is what makes this safer than the old checkout: the image never
//! leaves its directory, never crosses a volume (see the cross-device rename
//! kernel panic), and is renamed for only ~20 s. A crash mid-session leaves an
//! obviously-named `<image>.ps5upload-editing` sitting next to its siblings,
//! recoverable by hand, rather than a game stranded outside every scan root.
//!
//! We only ever touch lines inside our own marker block, so a user's hand-written
//! `image_rw=` / `scanpath=` lines survive an edit session untouched.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::diagnostics::fs_write_bytes;
use crate::fs_ops::{fs_chmod, fs_mkdir, fs_move, fs_read};

const CONFIG_PATH: &str = "/data/shadowmount/config.ini";
const SESSION_DIR: &str = "/data/ps5upload/editing";
const SESSION_PATH: &str = "/data/ps5upload/editing/image-rw.json";
const REMOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const REMOUNT_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// Opening line of the block this module owns in `config.ini`. Everything
/// between this and [`MARKER_END`] is ours to rewrite or delete; everything
/// outside it belongs to the user and is preserved verbatim.
pub const MARKER_BEGIN: &str =
    "# --- ps5upload edit session (temporary; remove to restore read-only) ---";
pub const MARKER_END: &str = "# --- end ps5upload edit session ---";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageRwSession {
    pub title_id: String,
    pub image_path: String,
    pub temporary_path: String,
    pub mount_point: String,
    pub restore_root_mode: Option<String>,
    pub started_at_ms: u64,
}

/// Rewrite `config_ini` so exactly `images` (image basenames, e.g.
/// "PPSA26344.exfat") are mounted read-write, leaving every user-authored line
/// untouched. An empty slice removes the block entirely.
pub fn with_rw_images(config_ini: &str, images: &[String]) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in config_ini.lines() {
        if line.trim() == MARKER_BEGIN {
            skipping = true;
            // The blank line we write before the marker belongs to the block,
            // not to the user's file. Without this, add/remove cycles leave a
            // blank line behind each time and the config drifts from what the
            // user actually wrote.
            if out.ends_with("\n\n") {
                out.pop();
            }
            continue;
        }
        if skipping {
            if line.trim() == MARKER_END {
                skipping = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !images.is_empty() {
        out.push('\n');
        out.push_str(MARKER_BEGIN);
        out.push('\n');
        for image in images {
            out.push_str(&format!("image_rw={image}\n"));
        }
        out.push_str(MARKER_END);
        out.push('\n');
    }
    out
}

/// What a stale edit session (an `image_rw=` block still in `config.ini` from a
/// run that never finished) should cause us to do at startup.
#[derive(Debug, PartialEq, Eq)]
pub enum StaleSessionAction {
    /// Nothing to do — no marker block present.
    None,
    /// Strip the block and remount the affected image read-only now.
    RevertNow,
    /// Leave it, and tell the user. Remounting the affected image would kill a
    /// running game.
    WarnOnly,
}

/// Decide what to do when we find our marker block left over from a previous
/// run. `game_running` is true when any image-backed title currently has a
/// process.
///
/// The trade-off: leaving an image mounted read-write is a silent corruption
/// risk — Sony's own code can write to a 165 GB exfat image and nothing looks
/// wrong until it does. But reverting remounts the affected image and would
/// kill a game in progress. There is also a
/// third consideration: a user who quit mid-backport may well be about to
/// resume it, and reverting costs them two more SMP restarts.
pub fn stale_session_action(has_marker_block: bool, game_running: bool) -> StaleSessionAction {
    match (has_marker_block, game_running) {
        (false, _) => StaleSessionAction::None,
        (true, true) => StaleSessionAction::WarnOnly,
        (true, false) => StaleSessionAction::RevertNow,
    }
}

pub fn read_state(addr: &str) -> Result<Option<ImageRwSession>> {
    let bytes = match fs_read(addr, SESSION_PATH, 0, 256 * 1024) {
        Ok(bytes) => bytes,
        Err(e) if is_not_found(&e.to_string()) => return Ok(None),
        Err(e) => return Err(e).context("read the ShadowMount+ image edit journal"),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parse {SESSION_PATH}"))
}

fn write_state(addr: &str, state: &ImageRwSession) -> Result<()> {
    fs_mkdir(addr, SESSION_DIR).context("create image edit journal directory")?;
    fs_write_bytes(
        addr,
        SESSION_PATH,
        &serde_json::to_vec_pretty(state)?,
        false,
    )
    .context("write image edit journal")?;
    Ok(())
}

fn clear_state(addr: &str) -> Result<()> {
    fs_write_bytes(addr, SESSION_PATH, b"", false).context("clear image edit journal")?;
    Ok(())
}

fn read_text(addr: &str, path: &str) -> Result<String> {
    let bytes = fs_read(addr, path, 0, 256 * 1024).with_context(|| format!("read {path}"))?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_string())
}

fn image_name(path: &str) -> Result<&str> {
    let name = path.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        bail!("invalid ShadowMount+ image path: {path}");
    }
    Ok(name)
}

fn title_id_ok(title_id: &str) -> bool {
    let b = title_id.as_bytes();
    b.len() == 9 && matches!(&b[..4], b"PPSA" | b"CUSA") && b[4..].iter().all(u8::is_ascii_digit)
}

fn mount_for_image(addr: &str, image_path: &str) -> Result<Option<String>> {
    let wanted = crate::smp_checkout::image_basename(image_path);
    let status = crate::smp::collect_status(addr)?;
    if !status.errors.is_empty() {
        bail!(
            "could not inspect ShadowMount+ mounts: {}",
            status.errors.join("; ")
        );
    }
    Ok(status
        .mounted_images
        .into_iter()
        .find(|m| m.derived_name == wanted)
        .map(|m| m.mount_point))
}

fn wait_for_mount(addr: &str, image_path: &str, present: bool) -> Result<Option<String>> {
    let deadline = std::time::Instant::now() + REMOUNT_TIMEOUT;
    loop {
        match mount_for_image(addr, image_path) {
            Ok(found) if found.is_some() == present => return Ok(found),
            Ok(_) | Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(REMOUNT_POLL);
            }
            Ok(_) => bail!(
                "ShadowMount+ did not {} {} within {} seconds",
                if present { "remount" } else { "release" },
                image_path,
                REMOUNT_TIMEOUT.as_secs()
            ),
            Err(e) => return Err(e).context("wait for ShadowMount+ remount"),
        }
    }
}

fn path_exists(addr: &str, path: &str) -> bool {
    match fs_read(addr, path, 0, 1) {
        Ok(_) => true,
        Err(e) => !is_not_found(&e.to_string()),
    }
}

fn is_not_found(message: &str) -> bool {
    message.contains("ENOENT")
        || message.contains("No such file")
        || message.split("_errno_").skip(1).any(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                == "2"
        })
        || message.contains("fs_read_stat_failed")
}

fn blip(addr: &str, state: &ImageRwSession) -> Result<String> {
    fs_move(addr, &state.image_path, &state.temporary_path)
        .context("temporarily rename the image so ShadowMount+ releases it")?;
    let released = wait_for_mount(addr, &state.image_path, false);
    // Always put the image name back, even if the release probe failed. The
    // on-console journal remains until the rest of the sequence is confirmed.
    let moved_back = fs_move(addr, &state.temporary_path, &state.image_path)
        .context("restore the image name after the remount blip");
    released?;
    moved_back?;
    wait_for_mount(addr, &state.image_path, true)?
        .context("ShadowMount+ reported no mount after the image returned")
}

pub fn begin(addr: &str, title_id: &str) -> Result<ImageRwSession> {
    if !title_id_ok(title_id) {
        bail!("invalid title id: {title_id}");
    }
    if let Some(existing) = read_state(addr)? {
        bail!(
            "{} already has an image edit session open for {}",
            existing.title_id,
            existing.image_path
        );
    }

    let link = format!("/user/app/{title_id}/mount_img.lnk");
    let image_path = read_text(addr, &link)?.trim().to_string();
    if !image_path.starts_with('/') {
        bail!("{link} does not contain an absolute image path");
    }
    let name = image_name(&image_path)?.to_string();
    let mount_point = mount_for_image(addr, &image_path)?
        .with_context(|| format!("ShadowMount+ has not mounted {image_path}"))?;
    let temporary_path = format!("{image_path}.ps5upload-editing");
    if path_exists(addr, &temporary_path) {
        bail!("temporary image path already exists: {temporary_path}");
    }
    let restore_root_mode = image_path
        .to_ascii_lowercase()
        .ends_with(".ffpkg")
        .then(|| "0555".to_string());
    let state = ImageRwSession {
        title_id: title_id.to_string(),
        image_path,
        temporary_path,
        mount_point,
        restore_root_mode,
        started_at_ms: now_ms(),
    };
    let config = read_text(addr, CONFIG_PATH)?;
    if config.contains(MARKER_BEGIN) {
        bail!("a stale ps5upload image_rw block already exists in {CONFIG_PATH}");
    }
    // Journal before the first mutation, but only after all read-only
    // preconditions pass. Otherwise a stale marker would create a false new
    // session on top of the interrupted one.
    write_state(addr, &state)?;
    fs_write_bytes(
        addr,
        CONFIG_PATH,
        with_rw_images(&config, &[name]).as_bytes(),
        false,
    )
    .context("enable ShadowMount+ read-write mode for the image")?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    let mounted = blip(addr, &state)?;
    if mounted != state.mount_point {
        bail!(
            "ShadowMount+ remounted the image at {mounted}, expected {}",
            state.mount_point
        );
    }
    if state.restore_root_mode.is_some() {
        fs_chmod(addr, &state.mount_point, "0777", false)
            .context("make the UFS image root writable")?;
    }
    Ok(state)
}

pub fn finish(addr: &str) -> Result<ImageRwSession> {
    let state = read_state(addr)?.context("no ShadowMount+ image edit session is open")?;
    if path_exists(addr, &state.temporary_path) && !path_exists(addr, &state.image_path) {
        fs_move(addr, &state.temporary_path, &state.image_path)
            .context("recover the temporarily renamed image")?;
        wait_for_mount(addr, &state.image_path, true)?;
    }
    if let Some(mode) = &state.restore_root_mode {
        fs_chmod(addr, &state.mount_point, mode, false)
            .context("restore the UFS image root mode")?;
    }
    let config = read_text(addr, CONFIG_PATH)?;
    fs_write_bytes(
        addr,
        CONFIG_PATH,
        with_rw_images(&config, &[]).as_bytes(),
        false,
    )
    .context("remove the temporary ShadowMount+ read-write rule")?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    blip(addr, &state).context("remount the image read-only")?;
    clear_state(addr)?;
    Ok(state)
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

    const USER_CONFIG: &str = "# ShadowMount runtime config\n\
                               debug=1\n\
                               scanpath=/data/homebrew\n\
                               image_rw=MyOwnDump.exfat\n";

    #[test]
    fn adds_a_marked_block_without_touching_user_lines() {
        // A user's own `image_rw=` / `scanpath=` lines must survive an edit
        // session. Only what is between our markers is ours to rewrite.
        let out = with_rw_images(USER_CONFIG, &["PPSA26344.exfat".into()]);
        assert!(out.contains("scanpath=/data/homebrew"));
        assert!(out.contains("image_rw=MyOwnDump.exfat"));
        assert!(out.contains("image_rw=PPSA26344.exfat"));
        assert!(out.contains(MARKER_BEGIN) && out.contains(MARKER_END));
    }

    #[test]
    fn removing_the_block_restores_the_original_exactly() {
        // The end of every edit session. If this drifts, consoles accumulate
        // stale image_rw= lines and images stay silently writable.
        let with = with_rw_images(USER_CONFIG, &["PPSA26344.exfat".into()]);
        assert_eq!(with_rw_images(&with, &[]), USER_CONFIG);
    }

    #[test]
    fn replacing_a_block_does_not_nest_or_duplicate_it() {
        let once = with_rw_images(USER_CONFIG, &["A.exfat".into()]);
        let twice = with_rw_images(&once, &["B.exfat".into()]);
        assert_eq!(twice.matches(MARKER_BEGIN).count(), 1);
        assert!(!twice.contains("image_rw=A.exfat"));
        assert!(twice.contains("image_rw=B.exfat"));
        assert!(twice.contains("image_rw=MyOwnDump.exfat"));
    }

    #[test]
    fn several_images_can_be_writable_at_once() {
        let out = with_rw_images(USER_CONFIG, &["A.exfat".into(), "B.ffpkg".into()]);
        assert!(out.contains("image_rw=A.exfat") && out.contains("image_rw=B.ffpkg"));
    }

    #[test]
    fn stale_sessions_revert_when_idle_but_never_interrupt_a_running_game() {
        assert_eq!(stale_session_action(false, false), StaleSessionAction::None);
        assert_eq!(stale_session_action(false, true), StaleSessionAction::None);
        assert_eq!(
            stale_session_action(true, false),
            StaleSessionAction::RevertNow
        );
        assert_eq!(
            stale_session_action(true, true),
            StaleSessionAction::WarnOnly
        );
    }
}
