//! Console clean-up (spec 2026-10-22-console-cleanup-design.md): the helper scans the console
//! for what can safely be removed and removes the items the user picked. The engine only
//! forwards the calls; the helper decides what is listed and re-checks every item before it
//! removes it (`payload/src/storage_cleanup.c`, `cleanup_rules.c`).
//!
//! Every call returns the helper's JSON body. A refusal comes back as an error whose token is
//! one of `busy`, `unknown_scan`, `scan_expired`, `scan_not_done`, `unknown_apply`,
//! `install_running`, `bad_request`; an older helper answers `ERR_UNKNOWN_METHOD`
//! ([`crate::console_files::is_unsupported`]).

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mgmt::{self, m};

fn call_json(addr: &str, method: mgmt::Method, body: &Value) -> Result<Value> {
    let r = mgmt::call(addr, method, &serde_json::to_vec(body)?).and_then(|bytes| {
        serde_json::from_slice(&bytes).with_context(|| format!("{} reply", method.label))
    });
    match &r {
        Ok(v) => crate::clog!(
            Debug,
            "cleanup",
            "{} {addr} {body}: {}",
            method.label,
            summary(v)
        ),
        Err(e) => crate::clog!(
            Debug,
            "cleanup",
            "{} {addr} {body}: failed: {e:#}",
            method.label
        ),
    }
    r
}

/// A reply in a line: its state and counts, never the item list itself (paths of every file).
fn summary(v: &Value) -> String {
    let mut out = Vec::new();
    if let Some(o) = v.as_object() {
        for (k, val) in o {
            match val {
                Value::Array(a) => out.push(format!("{k}=[{}]", a.len())),
                Value::Object(_) => out.push(format!("{k}={{..}}")),
                Value::String(s) if s.len() > 80 => out.push(format!("{k}=<{} chars>", s.len())),
                other => out.push(format!("{k}={other}")),
            }
        }
    }
    out.join(" ")
}

/// Starts a scan: `{"ok":true,"scan_id":N}`.
pub fn scan_start(addr: &str) -> Result<Value> {
    call_json(addr, m::CLEANUP_SCAN, &serde_json::json!({}))
}

/// The scan's state, phase and (once done) its items.
pub fn scan_status(addr: &str, scan_id: u32) -> Result<Value> {
    call_json(
        addr,
        m::CLEANUP_STATUS,
        &serde_json::json!({ "scan_id": scan_id }),
    )
}

/// Removes `item_ids` of scan `scan_id`: `{"ok":true,"apply_id":N}`.
pub fn apply_start(addr: &str, scan_id: u32, item_ids: &[u32]) -> Result<Value> {
    call_json(
        addr,
        m::CLEANUP_APPLY,
        &serde_json::json!({ "scan_id": scan_id, "item_ids": item_ids }),
    )
}

/// What the apply has removed, skipped and failed so far.
pub fn apply_status(addr: &str, apply_id: u32) -> Result<Value> {
    call_json(
        addr,
        m::CLEANUP_APPLY_STATUS,
        &serde_json::json!({ "apply_id": apply_id }),
    )
}

/// Stops a scan, or an apply after its current item.
pub fn cancel(addr: &str, id: u32) -> Result<Value> {
    call_json(addr, m::CLEANUP_CANCEL, &serde_json::json!({ "id": id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_the_schema_numbers() {
        assert_eq!(m::CLEANUP_SCAN.id, 160);
        assert_eq!(m::CLEANUP_STATUS.id, 161);
        assert_eq!(m::CLEANUP_APPLY.id, 162);
        assert_eq!(m::CLEANUP_APPLY_STATUS.id, 163);
        assert_eq!(m::CLEANUP_CANCEL.id, 164);
    }
}
