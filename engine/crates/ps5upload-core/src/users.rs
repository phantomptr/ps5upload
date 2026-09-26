//! User account enumeration over FTX2 USER_LIST.
//!
//! Returns the list of user accounts on the console, with the
//! foreground (currently logged-in) user marked. Read-only.
//!
//! Useful for the Library tab when investigating per-user save data
//! and registered titles — Sony stores those keyed by user_id, so
//! seeing which users exist is the entry point.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAccount {
    pub id: i32,
    pub name: String,
    /// True for the currently-active foreground user.
    pub foreground: bool,
    /// Sony API error from sceUserServiceGetUserName for this user.
    /// 0 = success; non-zero means we have the id but no name (rare —
    /// happens on temporary guest accounts).
    #[serde(default)]
    pub err_name: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserList {
    /// User id of the foreground (active) user, or -1 when no one
    /// is logged in / the API call failed.
    pub foreground: i32,
    /// Sony API error from sceUserServiceGetForegroundUser. Non-zero
    /// means foreground may be stale.
    #[serde(default)]
    pub err_fg: i32,
    /// Sony API error from sceUserServiceGetLoginUserIdList.
    #[serde(default)]
    pub err_list: i32,
    /// Logged-in users (Sony's API only enumerates currently logged-in,
    /// not all profiles ever created).
    pub users: Vec<UserAccount>,
}

pub fn user_list(addr: &str) -> Result<UserList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::UserList, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected USER_LIST: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::UserListAck {
        bail!("expected USER_LIST_ACK, got {ft:?}");
    }
    let parsed: UserList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

// ── User create / delete ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCreateResult {
    pub ok: bool,
    pub uid: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub err: String,
}

pub fn user_create(addr: &str, name: &str) -> Result<UserCreateResult> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "name": name });
    c.send_frame(FrameType::UserCreate, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected USER_CREATE: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::UserCreateAck {
        bail!("expected USER_CREATE_ACK, got {ft:?}");
    }
    let parsed: UserCreateResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "user create failed: {}",
            if parsed.err.is_empty() {
                "unknown error"
            } else {
                &parsed.err
            }
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDeleteResult {
    pub ok: bool,
    pub uid: i32,
    #[serde(default)]
    pub err: String,
}

pub fn user_delete(addr: &str, uid: i32, wipe_saves: bool) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "uid": uid, "wipe_saves": wipe_saves });
    c.send_frame(FrameType::UserDelete, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected USER_DELETE: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::UserDeleteAck {
        bail!("expected USER_DELETE_ACK, got {ft:?}");
    }
    let parsed: UserDeleteResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "user delete failed: {}",
            if parsed.err.is_empty() {
                "unknown error"
            } else {
                &parsed.err
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list carries three separate Sony error codes. They default to
    /// zero, so a missing one reads as success — the UI must be able to
    /// tell "no users" from "the call failed".
    #[test]
    fn parses_a_user_list_and_its_error_codes() {
        let l: UserList = serde_json::from_str(
            r#"{"foreground":1,"err_fg":0,"err_list":0,"users":[
                {"id":1,"name":"player","foreground":true,"err_name":0}
            ]}"#,
        )
        .unwrap();
        assert_eq!(l.foreground, 1);
        assert_eq!(l.users.len(), 1);
        assert!(l.users[0].foreground);
    }

    /// -1 is the documented "nobody logged in / call failed" value, and
    /// it must survive as -1 rather than becoming a default 0, which
    /// would name user zero.
    #[test]
    fn preserves_the_no_foreground_user_sentinel() {
        let l: UserList =
            serde_json::from_str(r#"{"foreground":-1,"err_fg":5,"users":[]}"#).unwrap();
        assert_eq!(l.foreground, -1);
        assert_eq!(l.err_fg, 5);
        assert!(l.users.is_empty());
    }

    /// A user whose name lookup failed still has a usable id; the UI
    /// shows the id rather than dropping the account.
    #[test]
    fn keeps_a_user_whose_name_lookup_failed() {
        let l: UserList = serde_json::from_str(
            r#"{"foreground":1,"users":[{"id":7,"name":"","foreground":false,"err_name":2}]}"#,
        )
        .unwrap();
        assert_eq!(l.users[0].id, 7);
        assert_eq!(l.users[0].err_name, 2);
    }

    /// The payload always emits `uid`, using -1 when creation failed, so
    /// `uid` is deliberately required here — a reply without it is
    /// malformed rather than a refusal. Written the other way round at
    /// first, and the parse error is what showed the real contract.
    #[test]
    fn parses_a_refused_user_creation_with_the_sentinel_uid() {
        let r: UserCreateResult =
            serde_json::from_str(r#"{"ok":false,"uid":-1,"name":"","err":"invalid_name"}"#)
                .unwrap();
        assert!(!r.ok);
        assert_eq!(r.uid, -1);
        assert_eq!(r.err, "invalid_name");
    }

    #[test]
    fn parses_a_successful_user_creation() {
        let r: UserCreateResult =
            serde_json::from_str(r#"{"ok":true,"uid":3,"name":"player","err":""}"#).unwrap();
        assert!(r.ok);
        assert_eq!(r.uid, 3);
        assert_eq!(r.name, "player");
    }
}
