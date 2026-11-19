//! Local user accounts over AVA1 management (`user.create`, `user.delete`).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

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
    let body = serde_json::json!({ "name": name });
    let resp = mgmt::call_keep(
        addr,
        m::USER_CREATE,
        "USER_CREATE",
        &serde_json::to_vec(&body)?,
    )?;
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

/// A console user id as JSON may carry it: unsigned (0..=u32::MAX, as `user.list` and
/// `profile.info` send it) or the same 32 bits signed (an `i32`, as the saves list and older apps
/// hold it). `None` for anything else: a number that does not fit is never wrapped onto another
/// user's id.
pub fn user_id_from_i64(v: i64) -> Option<u32> {
    if (i64::from(i32::MIN)..0).contains(&v) {
        Some(v as i32 as u32)
    } else {
        u32::try_from(v).ok()
    }
}

/// Deletes console user `uid` (all 32 bits). Sent unsigned: an older helper reads an
/// id above 0x7fffffff as invalid and refuses, never as another user.
pub fn user_delete(addr: &str, uid: u32, wipe_saves: bool) -> Result<()> {
    let body = serde_json::json!({ "uid": uid, "wipe_saves": wipe_saves });
    let resp = mgmt::call_keep(
        addr,
        m::USER_DELETE,
        "USER_DELETE",
        &serde_json::to_vec(&body)?,
    )?;
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
    fn user_ids_keep_all_32_bits_and_refuse_what_does_not_fit() {
        assert_eq!(user_id_from_i64(0x179a_0cd8), Some(0x179a_0cd8));
        assert_eq!(user_id_from_i64(2_415_919_104), Some(0x9000_0000));
        assert_eq!(user_id_from_i64(-1_879_048_192), Some(0x9000_0000));
        assert_eq!(user_id_from_i64(i64::from(i32::MIN)), Some(0x8000_0000));
        assert_eq!(user_id_from_i64(i64::from(u32::MAX)), Some(u32::MAX));
        assert_eq!(user_id_from_i64(i64::from(u32::MAX) + 6), None);
        assert_eq!(user_id_from_i64(i64::from(i32::MIN) - 1), None);
    }

    /// The helper writes the id back signed (an older engine's `i32`); that still parses.
    #[test]
    fn parses_a_delete_reply_for_a_user_above_i32_max() {
        let r: UserDeleteResult =
            serde_json::from_str(r#"{"ok":true,"uid":-1879048192,"err":""}"#).unwrap();
        assert!(r.ok);
        assert_eq!(r.uid as u32, 0x9000_0000);
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
