//! Tag-based backup & restore over AVA1 management.
//!
//! Snapshots are stored on the PS5 at
//! `/data/ps5upload/backups/<tag>/<unix_timestamp>/`. Each snapshot has
//! a `.manifest` (basename → original-path mapping) and flattened copies
//! of the source files. Restore reads the manifest and copies each file
//! back to its original path.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSnapshotResult {
    pub ok: bool,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub files: i32,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub err: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub tag: String,
    pub timestamp: i64,
    pub files: i32,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupList {
    pub snapshots: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRestoreResult {
    pub ok: bool,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub restored: i32,
    #[serde(default)]
    pub err: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupDeleteResult {
    pub ok: bool,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub err: String,
}

/// Tag validation: `[a-zA-Z0-9_-]`, 1–32 chars. Mirrors the payload
/// check so we reject bad input before round-tripping.
pub fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() || tag.len() > 32 {
        bail!("tag must be 1–32 characters");
    }
    for c in tag.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
            bail!("tag may only contain [a-zA-Z0-9_-]");
        }
    }
    Ok(())
}

pub fn backup_snapshot(addr: &str, tag: &str, path: &str) -> Result<BackupSnapshotResult> {
    validate_tag(tag)?;
    let body = serde_json::json!({ "tag": tag, "path": path });
    // A job over AVA1: copying a save folder is quick, a whole title's data is not.
    let resp = mgmt::run_op(
        addr,
        mgmt::ops::BACKUP_SNAPSHOT,
        "BACKUP_SNAPSHOT",
        &serde_json::to_vec(&body)?,
        &mgmt::JobCall {
            op_id: 0,
            subject: path,
            deadline: BACKUP_DEADLINE,
        },
    )?;
    let parsed: BackupSnapshotResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "snapshot failed: {}",
            if parsed.err.is_empty() {
                "unknown error"
            } else {
                &parsed.err
            }
        );
    }
    Ok(parsed)
}

/// How long a snapshot or restore may take before the engine stops waiting for it.
const BACKUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

pub fn backup_list(addr: &str, tag: &str) -> Result<BackupList> {
    let body = serde_json::json!({ "tag": tag });
    let resp = mgmt::call_keep(
        addr,
        m::BACKUP_LIST,
        "BACKUP_LIST",
        &serde_json::to_vec(&body)?,
    )?;
    let parsed: BackupList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

pub fn backup_restore(addr: &str, tag: &str, timestamp: i64) -> Result<BackupRestoreResult> {
    validate_tag(tag)?;
    let body = serde_json::json!({ "tag": tag, "timestamp": timestamp });
    let resp = mgmt::run_op(
        addr,
        mgmt::ops::BACKUP_RESTORE,
        "BACKUP_RESTORE",
        &serde_json::to_vec(&body)?,
        &mgmt::JobCall {
            op_id: 0,
            subject: tag,
            deadline: BACKUP_DEADLINE,
        },
    )?;
    let parsed: BackupRestoreResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "restore failed: {}",
            if parsed.err.is_empty() {
                "unknown error"
            } else {
                &parsed.err
            }
        );
    }
    Ok(parsed)
}

pub fn backup_delete(addr: &str, tag: &str, timestamp: i64) -> Result<()> {
    validate_tag(tag)?;
    let body = serde_json::json!({ "tag": tag, "timestamp": timestamp });
    let resp = mgmt::call_keep(
        addr,
        m::BACKUP_DELETE,
        "BACKUP_DELETE",
        &serde_json::to_vec(&body)?,
    )?;
    let parsed: BackupDeleteResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "delete failed: {}",
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

    /// The tag names a directory on the console, so anything that could
    /// climb out of the backup root has to be refused before the frame
    /// is sent — not left to the payload to catch.
    #[test]
    fn rejects_tags_that_could_escape_the_backup_directory() {
        for bad in ["..", "../etc", "a/b", "a\\b", "a/../..", "./x", "a b"] {
            assert!(validate_tag(bad).is_err(), "tag {bad:?} should be rejected");
        }
    }

    #[test]
    fn rejects_empty_and_overlong_tags() {
        assert!(validate_tag("").is_err());
        assert!(validate_tag(&"a".repeat(33)).is_err());
        assert!(
            validate_tag(&"a".repeat(32)).is_ok(),
            "32 is the documented limit"
        );
        assert!(validate_tag("a").is_ok());
    }

    #[test]
    fn rejects_control_and_non_ascii_characters() {
        for bad in ["a\nb", "a\0b", "café", "日本語", "a\tb"] {
            assert!(validate_tag(bad).is_err(), "tag {bad:?} should be rejected");
        }
    }

    #[test]
    fn accepts_the_documented_character_set() {
        for good in ["pre-install", "snapshot_1", "ABC123", "a-b_c"] {
            assert!(
                validate_tag(good).is_ok(),
                "tag {good:?} should be accepted"
            );
        }
    }

    /// The payload emits snake_case JSON and every optional field has a
    /// serde default, so a renamed key does not error — it silently
    /// yields a zero. These pin the shapes the payload actually sends.
    #[test]
    fn parses_a_successful_snapshot() {
        let r: BackupSnapshotResult = serde_json::from_str(
            r#"{"ok":true,"tag":"pre-install","timestamp":1786320000,"files":12,"bytes":4096}"#,
        )
        .unwrap();
        assert!(r.ok);
        assert_eq!(r.tag, "pre-install");
        assert_eq!(r.files, 12);
        assert_eq!(r.bytes, 4096);
        assert!(r.err.is_empty());
    }

    #[test]
    fn parses_a_refused_snapshot_without_losing_the_reason() {
        let r: BackupSnapshotResult =
            serde_json::from_str(r#"{"ok":false,"err":"store_failed"}"#).unwrap();
        assert!(!r.ok);
        assert_eq!(r.err, "store_failed");
    }

    #[test]
    fn parses_a_restore_and_an_empty_list() {
        let r: BackupRestoreResult =
            serde_json::from_str(r#"{"ok":true,"tag":"t","restored":3}"#).unwrap();
        assert!(r.ok);
        assert_eq!(r.restored, 3);

        let l: BackupList = serde_json::from_str(r#"{"snapshots":[]}"#).unwrap();
        assert!(l.snapshots.is_empty());
    }
}
