//! Legacy body <-> typed AVA1 body conversions for the management methods that are typed
//! (SPEC.md section 7.3). Everything else travels as `MgmtText` with the body unchanged.
//!
//! Callers in `ps5upload-core` keep sending and reading the legacy JSON. These pure
//! functions are the whole mapping, so they are tested without a network.

use anyhow::Result;
use ava1::gen::{self, FsEntry, FsFreeSpace, FsListResult, FsStat, NodeStatus};
use serde_json::{json, Value};

/// A refusal that is the caller's own fault (a body the legacy handler would have
/// refused), carrying the legacy token as its cause.
pub(crate) fn bad_request(label: &str, cause: &str) -> anyhow::Error {
    ps5upload_core::mgmt::MgmtError {
        label: label.to_string(),
        status: gen::ERR_PROTOCOL,
        cause: cause.to_string(),
    }
    .into()
}

/// The legacy name of an entry kind (`FsEntry.kind`, `FsStat.kind`).
pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        gen::ENTRY_FILE => "file",
        gen::ENTRY_DIR => "dir",
        gen::ENTRY_LINK => "link",
        gen::ENTRY_OTHER => "other",
        _ => "unknown",
    }
}

fn parse(body: &[u8], label: &str) -> Result<Value> {
    serde_json::from_slice(body).map_err(|_| bad_request(label, "body_malformed"))
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `{"path","offset","limit"}` -> `FsList`.
pub fn fs_list_request(body: &[u8], label: &str) -> Result<gen::FsList> {
    let v = parse(body, label)?;
    Ok(gen::FsList {
        path: str_field(&v, "path"),
        offset: v
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        limit: v
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(256)
            .min(u16::MAX as u64) as u16,
    })
}

/// `FsListResult` -> the legacy `{"path","entries","truncated","total_scanned","returned"}`.
/// `path` and the returned count are not on the wire (SPEC 7.5): the request and the entry
/// count supply them.
pub fn fs_list_reply(path: &str, r: &FsListResult) -> Value {
    let entries: Vec<Value> = r
        .entries
        .iter()
        .map(|e: &FsEntry| {
            json!({
                "name": e.name,
                "kind": kind_name(e.kind),
                "size": e.size,
                "mtime": e.mtime.unwrap_or(0),
            })
        })
        .collect();
    json!({
        "path": path,
        "returned": entries.len(),
        "entries": entries,
        "truncated": r.more != 0,
        "total_scanned": r.total_scanned,
    })
}

/// `{"path"}` -> `FsPath`.
pub fn fs_path_request(body: &[u8], label: &str) -> Result<gen::FsPath> {
    Ok(gen::FsPath {
        path: str_field(&parse(body, label)?, "path"),
    })
}

/// `FsStat` -> `{"kind","size","mtime","mode","dev"}`.
pub fn fs_stat_reply(s: &FsStat) -> Value {
    json!({
        "kind": kind_name(s.kind),
        "size": s.size,
        "mtime": s.mtime,
        "mode": s.mode,
        "dev": s.dev,
    })
}

/// `FsFreeSpace` -> `{"usable_bytes","free_bytes","total_bytes","reserve_bytes","dev"}`.
pub fn fs_freespace_reply(f: &FsFreeSpace) -> Value {
    json!({
        "usable_bytes": f.usable,
        "free_bytes": f.free,
        "total_bytes": f.total,
        "reserve_bytes": f.reserve,
        "dev": f.dev,
    })
}

/// `{"path"}` -> `FsMkdir` with the legacy behaviour: parents created, mode 0777.
pub fn fs_mkdir_request(body: &[u8], label: &str) -> Result<gen::FsMkdir> {
    Ok(gen::FsMkdir {
        path: str_field(&parse(body, label)?, "path"),
        mode: 0o777,
        parents: 1,
    })
}

/// `{"from","to"}` -> `FsRename`. The legacy move was a plain `rename(2)`, which replaces.
pub fn fs_rename_request(body: &[u8], label: &str) -> Result<gen::FsRename> {
    let v = parse(body, label)?;
    Ok(gen::FsRename {
        from: str_field(&v, "from"),
        to: str_field(&v, "to"),
        overwrite: 1,
    })
}

/// `{"path","mode":"0777","recursive":0|1}`. `None` for a recursive chmod, which runs as a
/// `job.run` op (Task 5); a recursive chmod runs as a job.
pub fn fs_chmod_request(body: &[u8], label: &str) -> Result<Option<gen::FsChmod>> {
    let v = parse(body, label)?;
    let recursive = match v.get("recursive") {
        Some(Value::Bool(b)) => *b,
        Some(n) => n.as_i64().unwrap_or(0) != 0,
        None => false,
    };
    if recursive {
        return Ok(None);
    }
    let mode = u32::from_str_radix(v.get("mode").and_then(Value::as_str).unwrap_or("0"), 8)
        .map_err(|_| bad_request(label, "fs_chmod_bad_mode"))?;
    Ok(Some(gen::FsChmod {
        path: str_field(&v, "path"),
        mode,
    }))
}

/// What an `fs.read` call asked for.
pub struct ReadAsk {
    pub path: String,
    pub offset: u64,
    /// Bytes wanted in total (already clamped to the per-call ceiling).
    pub limit: u64,
    pub flags: u32,
}

/// The legacy `FsRead` answered at most this much (`FS_READ_MAX_BYTES`, runtime.c); the
/// wrapper keeps that ceiling so a caller's limit means what it did.
pub const LEGACY_READ_MAX: u64 = 2 * 1024 * 1024;

/// `{"path","offset","limit","unsafe"}`.
pub fn fs_read_request(body: &[u8], label: &str) -> Result<ReadAsk> {
    let v = parse(body, label)?;
    let unsafe_read = v.get("unsafe").and_then(Value::as_bool).unwrap_or(false);
    Ok(ReadAsk {
        path: str_field(&v, "path"),
        offset: v.get("offset").and_then(Value::as_u64).unwrap_or(0),
        limit: v
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(LEGACY_READ_MAX),
        flags: if unsafe_read { gen::FSR_UNSAFE } else { 0 },
    })
}

/// What an `fs.write` call asked for.
pub struct WriteAsk {
    pub path: String,
    pub data: Vec<u8>,
    pub create_only: bool,
}

/// The legacy `FsWriteBytes` ceiling (`FS_WRITE_BYTES_MAX`).
pub const LEGACY_WRITE_MAX: usize = 256 * 1024;

/// `{"path","bytes":"<base64>","mode":"create"|"overwrite"}`.
pub fn fs_write_request(body: &[u8], label: &str) -> Result<WriteAsk> {
    use base64::Engine as _;
    let v = parse(body, label)?;
    let b64 = v
        .get("bytes")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(label, "bytes_required"))?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| bad_request(label, "bad_base64"))?;
    if data.len() > LEGACY_WRITE_MAX {
        return Err(bad_request(label, "too_large"));
    }
    let path = str_field(&v, "path");
    if path.is_empty() {
        return Err(bad_request(label, "path_required"));
    }
    Ok(WriteAsk {
        path,
        data,
        create_only: v.get("mode").and_then(Value::as_str) == Some("create"),
    })
}

/// The flags and offset of chunk `index` of `total` chunks. One chunk is the whole file in
/// one call (neither `AT_OFFSET` nor `COMMIT`: the commit is implied); more chunks go at
/// their offsets and the last one commits (SPEC 7.5).
pub fn write_chunk_flags(create_only: bool, index: usize, total: usize) -> u32 {
    let base = if create_only {
        gen::FSW_CREATE
    } else {
        gen::FSW_OVERWRITE
    };
    if total <= 1 {
        return base;
    }
    let mut f = base | gen::FSW_AT_OFFSET;
    if index + 1 == total {
        f |= gen::FSW_COMMIT;
    }
    f
}

/// `NodeStatus` -> the legacy STATUS JSON the client and `health.rs` read.
///
/// `ucred_elevated` is a JSON boolean again, `prior_instance` is present only when the
/// payload sent it (an older payload had no such field). The old transaction fields
/// (`runtime_port`, `shutdown`, `takeover_requested`, `active_transactions`,
/// `last_tx_seq`, `recovered_transactions`) no longer exist; nothing reads them.
pub fn node_status_json(s: &NodeStatus) -> Value {
    let mut v = json!({
        "version": s.version,
        "ps5_kernel": s.ps5_kernel,
        "instance_id": s.instance_id,
        "startup_reason": s.startup_reason,
        "started_at_unix": s.started_at_unix,
        "command_count": s.command_count,
        "ucred_elevated": s.ucred_elevated != 0,
        "max_transfer_streams": s.max_transfer_streams,
        "fan_threshold": s.fan_threshold,
        "fan_reapply_sec": s.fan_reapply_sec,
    });
    if let Some(p) = &s.prior_instance {
        v["prior_instance"] = json!(p);
    }
    v
}

/// The text of an error body.
pub fn cause_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

/// The legacy token for a status whose body carried no cause. A ported handler always
/// sends one; this only keeps the message readable if one does not.
pub fn default_cause(status: u16) -> &'static str {
    match status {
        gen::ERR_NOT_PAIRED => "not_paired",
        gen::ERR_PAIRING_CLOSED => "pairing_closed",
        gen::ERR_PROTOCOL => "protocol",
        gen::ERR_UNKNOWN_METHOD => "unknown_method",
        gen::ERR_INTERNAL => "internal",
        gen::ERR_BUSY => "busy",
        gen::ERR_PATH => "path_not_allowed",
        gen::ERR_NO_SPACE => "no_space",
        gen::ERR_UNKNOWN_JOB => "unknown_job",
        gen::ERR_IO => "io_error",
        gen::ERR_VERIFY => "verify_failed",
        gen::ERR_EXISTS => "exists",
        gen::ERR_CANCELLED => "cancelled",
        gen::ERR_CROSS_DEVICE => "fs_move_cross_mount",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_requests_clamp_to_the_wire_widths_and_default_like_the_payload() {
        let q = fs_list_request(br#"{"path":"/data","offset":7,"limit":999999}"#, "L").unwrap();
        assert_eq!((q.path.as_str(), q.offset, q.limit), ("/data", 7, u16::MAX));
        let q = fs_list_request(br#"{"path":"/x"}"#, "L").unwrap();
        assert_eq!((q.offset, q.limit), (0, 256));
        let e = fs_list_request(b"not json", "L").unwrap_err();
        assert_eq!(e.to_string(), "payload rejected L: body_malformed");
    }

    #[test]
    fn every_entry_kind_has_the_legacy_name() {
        let names: Vec<_> = (0..6).map(kind_name).collect();
        assert_eq!(
            names,
            ["file", "dir", "link", "other", "unknown", "unknown"]
        );
    }

    #[test]
    fn chmod_parses_octal_and_leaves_recursion_to_job_run() {
        let q = fs_chmod_request(br#"{"path":"/a","mode":"0755","recursive":0}"#, "C")
            .unwrap()
            .unwrap();
        assert_eq!((q.path.as_str(), q.mode), ("/a", 0o755));
        assert!(
            fs_chmod_request(br#"{"path":"/a","mode":"0755","recursive":1}"#, "C")
                .unwrap()
                .is_none()
        );
        assert!(fs_chmod_request(br#"{"path":"/a","mode":"9"}"#, "C").is_err());
    }

    #[test]
    fn read_limits_are_held_to_the_per_call_ceiling() {
        let a = fs_read_request(
            br#"{"path":"/a","offset":3,"limit":99999999,"unsafe":true}"#,
            "R",
        )
        .unwrap();
        assert_eq!(
            (a.offset, a.limit, a.flags),
            (3, LEGACY_READ_MAX, gen::FSR_UNSAFE)
        );
        let a = fs_read_request(br#"{"path":"/a","limit":10}"#, "R").unwrap();
        assert_eq!((a.limit, a.flags), (10, 0));
    }

    #[test]
    fn write_requests_decode_base64_and_refuse_what_the_legacy_handler_refused() {
        let w = fs_write_request(br#"{"path":"/a","bytes":"aGk=","mode":"create"}"#, "W").unwrap();
        assert_eq!((w.data.as_slice(), w.create_only), (&b"hi"[..], true));
        let cause = |b: &[u8]| fs_write_request(b, "W").err().unwrap().to_string();
        assert_eq!(
            cause(br#"{"path":"/a","bytes":"!!"}"#),
            "payload rejected W: bad_base64"
        );
        assert_eq!(
            cause(br#"{"path":"/a"}"#),
            "payload rejected W: bytes_required"
        );
        assert_eq!(
            cause(br#"{"bytes":"aGk="}"#),
            "payload rejected W: path_required"
        );
    }

    #[test]
    fn chunk_flags_follow_the_spec() {
        use gen::*;
        assert_eq!(write_chunk_flags(false, 0, 1), FSW_OVERWRITE);
        assert_eq!(write_chunk_flags(true, 0, 1), FSW_CREATE);
        assert_eq!(
            write_chunk_flags(false, 0, 3),
            FSW_OVERWRITE | FSW_AT_OFFSET
        );
        assert_eq!(
            write_chunk_flags(false, 1, 3),
            FSW_OVERWRITE | FSW_AT_OFFSET
        );
        assert_eq!(
            write_chunk_flags(true, 2, 3),
            FSW_CREATE | FSW_AT_OFFSET | FSW_COMMIT
        );
    }

    #[test]
    fn node_status_json_has_a_real_bool_and_only_a_present_prior_instance() {
        let mut s = NodeStatus {
            ucred_elevated: 1,
            ..Default::default()
        };
        let v = node_status_json(&s);
        assert_eq!(v["ucred_elevated"], serde_json::Value::Bool(true));
        assert!(v.get("prior_instance").is_none());
        s.ucred_elevated = 0;
        s.prior_instance = Some("replaced".into());
        let v = node_status_json(&s);
        assert_eq!(v["ucred_elevated"], serde_json::Value::Bool(false));
        assert_eq!(v["prior_instance"], "replaced");
    }

    /// `/api/ps5/status` is the legacy JSON rebuilt from `NodeStatus`: the keys that scripts and
    /// the client read are all there (`bench/resume-test.mjs` and `tests/install-fallback-hw.mjs`
    /// read `version` and `command_count`), and the six old transaction fields are not.
    #[test]
    fn the_status_json_keeps_the_keys_callers_read_and_drops_the_transaction_ones() {
        let v = node_status_json(&NodeStatus::default());
        for k in [
            "version",
            "ps5_kernel",
            "instance_id",
            "startup_reason",
            "started_at_unix",
            "command_count",
            "ucred_elevated",
            "max_transfer_streams",
            "fan_threshold",
            "fan_reapply_sec",
        ] {
            assert!(v.get(k).is_some(), "{k} missing");
        }
        for k in [
            "runtime_port",
            "shutdown",
            "takeover_requested",
            "active_transactions",
            "last_tx_seq",
            "recovered_transactions",
        ] {
            assert!(v.get(k).is_none(), "{k} must be gone");
        }
    }

    #[test]
    fn every_error_status_has_a_default_token() {
        assert_eq!(default_cause(gen::ERR_CROSS_DEVICE), "fs_move_cross_mount");
        assert_eq!(default_cause(gen::ERR_EXISTS), "exists");
        assert_eq!(default_cause(9999), "error");
    }
}
