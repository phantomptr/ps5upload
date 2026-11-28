//! Putting a game installed from a package back on the home screen (#961).
//!
//! After Settings › Rebuild Database (Safe Mode, where no fake-package support is loaded), games
//! installed from fake packages disappear from the home screen and from Settings › Storage, while
//! their files stay (`<drive>/user/app/<id>/app.pkg`). The helper's `app.restore`
//! (`payload/src/title_restore.c`) checks a title, moves its package aside on the same drive
//! (`prepare`, after backing up app.db and appinfo.db), and after the engine has installed that
//! copy through the installer daemon, deletes the copy or puts it back (`finish`). The helper
//! never writes either database; the console's installer does, as for any install.
//!
//! A refusal from `prepare` is an `Ok` reply with `ok: false` and `err` (`restore_<state>`); an
//! older helper answers ERR_UNKNOWN_METHOD, turned into `restore_unsupported: ...` here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

/// What the helper answered. Fields a state does not use are absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestoreReply {
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(default)]
    pub title_id: String,
    /// check: restorable | registered | record_elsewhere | not_installed | pending |
    /// folder_registration | ambiguous | retail | no_room | aside_cross_device | db_unreadable |
    /// invalid_title | system_title. prepare: prepared. finish: restored | rolled_back |
    /// nothing_pending | waiting | unsettled.
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// internal | m2 | usb | extended: the drive the package is (or was) on.
    #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
    pub where_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    /// The folder the package is moved into while it is installed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aside: Option<String>,
    /// The installed package's place: `<drive>/user/app/<id>/app.pkg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// fake | ps4 | unknown (a retail PS5 package is refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The title's update, when one sits on the same drive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_volume: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_internal: Option<u64>,
    /// What the package's own drive needs free for the new copy (its size; on the console's
    /// storage 1.2x plus 1 GiB, which its reserve takes as it is written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_volume: Option<u64>,
    /// What the console's own storage needs free for the new copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_internal: Option<u64>,
    /// prepare: the copy aside to install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_aside: Option<String>,
    /// The folder with the copies of app.db and appinfo.db taken before the move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// finish: where the console installed the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_where: Option<String>,
    /// finish: the update kept aside because its place was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_kept: Option<String>,
    /// finish: the copy aside, kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<String>,
    /// clear: Sony's uninstall was asked to remove the console's unfinished copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared: Option<bool>,
    /// clear: Sony's answer to that uninstall (0 accepted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sony_rc: Option<u32>,
    /// clear refused: the title's update left in its place (the uninstall could take it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    /// clear refused: the title's add-ons on a drive (the uninstall could take them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addons: Option<String>,
}

impl RestoreReply {
    /// The check answered "this title can be put back".
    pub fn restorable(&self) -> bool {
        self.ok && self.state == "restorable"
    }

    /// A clear or rollback that has to wait for Sony to finish removing its copy: asking again
    /// later (finish with rollback) puts the package back.
    pub fn clearing(&self) -> bool {
        self.state == "unsettled" && self.reason.as_deref() == Some("clearing")
    }
}

/// The request body.
pub fn body(op: &str, title_id: &str, rollback: bool, install_ended: bool) -> serde_json::Value {
    let mut b = serde_json::json!({ "op": op, "title_id": title_id });
    if rollback {
        b["rollback"] = serde_json::Value::Bool(true);
    }
    if install_ended {
        b["install_ended"] = serde_json::Value::Bool(true);
    }
    b
}

/// How an older helper's refusal starts (its text follows the colon).
pub const UNSUPPORTED: &str = "restore_unsupported:";

/// A title id the helper accepts: 4 letters and 5 digits.
pub fn valid_title_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9
        && b[..4].iter().all(|c| c.is_ascii_alphabetic())
        && b[4..].iter().all(|c| c.is_ascii_digit())
}

/// How long clear may take: Sony's uninstall (FW 13.60 repeats it for two records, 10 s or more),
/// the helper's wait for the title's folders to go (up to 10 s) and its settle look (3 s).
const CLEAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

fn call(
    addr: &str,
    op: &str,
    title_id: &str,
    rollback: bool,
    install_ended: bool,
) -> Result<RestoreReply> {
    let id = title_id.trim().to_ascii_uppercase();
    if !valid_title_id(&id) {
        anyhow::bail!("restore_invalid_title: {title_id:?} is not a title id");
    }
    let req = serde_json::to_vec(&body(op, &id, rollback, install_ended))?;
    // A rollback after a clear settles too (3 s); clear runs Sony's uninstall.
    let timeout = (op == "clear" || rollback).then_some(CLEAR_TIMEOUT);
    let bytes =
        mgmt::call_with(addr, m::APP_RESTORE, "APP_RESTORE", &req, timeout).map_err(|e| {
            if crate::console_files::is_unsupported(&e) {
                anyhow::anyhow!(
                    "{UNSUPPORTED} the PS5 helper is an older version; send the helper again"
                )
            } else {
                e
            }
        })?;
    let r: RestoreReply = serde_json::from_slice(&bytes).context("parse APP_RESTORE reply")?;
    crate::clog!(
        Info,
        "install",
        "{op} {id} on {addr}: ok={} state={} err={:?} reason={:?}{}",
        r.ok,
        r.state,
        r.err,
        r.reason,
        match (r.cleared, r.sony_rc) {
            (Some(true), Some(rc)) => format!(" cleared (uninstall rc=0x{rc:08X})"),
            (Some(true), None) => " cleared earlier".to_string(),
            _ => String::new(),
        }
    );
    Ok(r)
}

/// Read-only: can this title be put back, and why not.
pub fn check(addr: &str, title_id: &str) -> Result<RestoreReply> {
    call(addr, "check", title_id, false, false)
}

/// Back up the databases and move the package aside. `ok: false` with `err` is a refusal
/// (nothing was changed).
pub fn prepare(addr: &str, title_id: &str) -> Result<RestoreReply> {
    call(addr, "prepare", title_id, false, false)
}

/// After the install: `install_ended` when it is known to have ended (its job reached a
/// verdict, or the user saw the game start), `rollback` when it ended without putting the game
/// back (implies ended).
pub fn finish(
    addr: &str,
    title_id: &str,
    rollback: bool,
    install_ended: bool,
) -> Result<RestoreReply> {
    call(
        addr,
        "finish",
        title_id,
        rollback,
        install_ended || rollback,
    )
}

/// The install of the copy aside failed after the console took it: Sony's uninstall removes the
/// console's unfinished copy (and the failed download in its queue, which blocks every later
/// install), then the package goes back. Refused (`unsettled` with a `reason`) while the title's
/// update or add-ons are in place or the copy aside is missing; `clearing` until Sony has finished
/// removing its copy (then [`finish`] with `rollback` puts the package back).
pub fn clear(addr: &str, title_id: &str) -> Result<RestoreReply> {
    call(addr, "clear", title_id, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_number_is_the_schemas() {
        assert_eq!(m::APP_RESTORE.id, 202);
        assert_eq!(m::APP_RESTORE.label, "APP_RESTORE");
    }

    #[test]
    fn body_sends_flags_only_when_set() {
        assert_eq!(
            body("check", "PPSA01234", false, false),
            serde_json::json!({"op":"check","title_id":"PPSA01234"})
        );
        assert_eq!(
            body("finish", "PPSA01234", true, true),
            serde_json::json!({"op":"finish","title_id":"PPSA01234","rollback":true,"install_ended":true})
        );
    }

    #[test]
    fn title_ids_are_checked_before_asking() {
        assert!(valid_title_id("PPSA01234"));
        assert!(valid_title_id("cusa00900"));
        for bad in [
            "",
            "PPSA0123",
            "PPSA012345",
            "PPSA0123A",
            "../x",
            "PPSA 1234",
        ] {
            assert!(!valid_title_id(bad), "{bad}");
        }
        let e = check("127.0.0.1:1", "../etc").unwrap_err().to_string();
        assert!(e.starts_with("restore_invalid_title"), "{e}");
    }

    #[test]
    fn replies_parse_with_every_shape() {
        let r: RestoreReply = serde_json::from_str(
            r#"{"ok":true,"title_id":"PPSA01234","state":"restorable","where":"m2","volume":"/mnt/ext1",
                "aside":"/mnt/ext1/ps5upload/restore/PPSA01234","pkg":"/mnt/ext1/user/app/PPSA01234/app.pkg",
                "size":4096,"kind":"fake","patch":"/mnt/ext1/user/patch/PPSA01234/patch.pkg","patch_size":10,
                "free_volume":1,"free_internal":2}"#,
        )
        .unwrap();
        assert!(r.restorable());
        assert_eq!(r.where_.as_deref(), Some("m2"));
        assert_eq!(r.size, Some(4096));
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["where"], "m2");
        assert!(back.get("err").is_none());

        let r: RestoreReply = serde_json::from_str(
            r#"{"ok":false,"err":"restore_registered","title_id":"PPSA01234","state":"registered"}"#,
        )
        .unwrap();
        assert!(!r.restorable());
        assert_eq!(r.err.as_deref(), Some("restore_registered"));

        let r: RestoreReply = serde_json::from_str(
            r#"{"ok":true,"title_id":"PPSA01234","state":"unsettled","reason":"clearing","cleared":true,"sony_rc":0,"kept":"/x"}"#,
        )
        .unwrap();
        assert!(r.clearing());
        assert_eq!((r.cleared, r.sony_rc), (Some(true), Some(0)));
        let r: RestoreReply = serde_json::from_str(
            r#"{"ok":true,"state":"unsettled","reason":"uninstall_refused","cleared":true,"sony_rc":2159155970}"#,
        )
        .unwrap();
        assert!(!r.clearing());
        assert_eq!(r.sony_rc, Some(0x80B2_1B02));

        let r: RestoreReply =
            serde_json::from_str(r#"{"ok":true,"state":"system_title"}"#).unwrap();
        assert_eq!(r.state, "system_title");
        assert!(r.title_id.is_empty());
    }
}
