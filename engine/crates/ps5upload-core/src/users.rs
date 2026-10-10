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

pub fn user_delete(addr: &str, uid: i32, wipe_saves: bool) -> Result<()> {
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
    fn parses_a_successful_user_creation() {
        let r: UserCreateResult =
            serde_json::from_str(r#"{"ok":true,"uid":3,"name":"player","err":""}"#).unwrap();
        assert!(r.ok);
        assert_eq!(r.uid, 3);
        assert_eq!(r.name, "player");
    }
}
