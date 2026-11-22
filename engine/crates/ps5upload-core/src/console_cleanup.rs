//! Console clean-up (spec 2026-10-22-console-cleanup-design.md): the helper scans the console
//! for what can safely be removed and removes the items the user picked. The engine only
//! forwards the calls; the helper decides what is listed and re-checks every item before it
//! removes it (`payload/src/storage_cleanup.c`, `cleanup_rules.c`).
//!
//! Every call returns the helper's JSON body. A refusal comes back as an error whose token is
//! one of `busy`, `unknown_scan`, `scan_expired`, `scan_not_done`, `unknown_apply`,
//! `install_running`, `bad_request`; an older helper answers `ERR_UNKNOWN_METHOD`
//! ([`crate::console_files::is_unsupported`]).
//!
//! Kraken Mount's games are never offered (#894): a package file or game folder it mounts, or
//! one sitting in a folder it scans, is the user's game, not a leftover. The helper keeps what is
//! in Kraken Mount's scan folders on its own; the engine also asks Kraken Mount's API which paths
//! it mounts, marks those items `"kept":"kraken"` (listed, never removable) and drops them
//! from an apply ([`scan_status_marked`], [`apply_start_guarded`]).

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::kraken::{self, KrakenStatus};
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

/// What Kraken Mount uses on one console: the paths its API lists, and the folders it scans
/// (one level deep) once it has run there, whether it runs now or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KrakenKeep {
    /// The `.pkg` files and game folders its library lists (every row, a duplicate or an error
    /// included: each is still the user's file).
    pub paths: Vec<String>,
    /// Its scan roots; empty when it never ran on this console.
    pub roots: Vec<String>,
}

/// `/user/data` written as `/data`, without trailing slashes.
fn norm(path: &str) -> String {
    let p = path.trim();
    let p = match p.strip_prefix("/user/data") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("/data{rest}"),
        _ => p.to_string(),
    };
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".into()
    } else {
        t.to_string()
    }
}

fn inside(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

impl KrakenKeep {
    pub fn from_status(s: &KrakenStatus) -> Self {
        // A listing of its folder that failed for another reason than "not there" may hide it:
        // keep its roots then too.
        let unsure = s.errors.iter().any(|e| e.starts_with("list "));
        let paths = s
            .games
            .iter()
            .map(|g| norm(&g.path))
            .filter(|p| p.starts_with('/') && p != "/")
            .collect();
        let roots = if s.installed || s.running || unsure {
            let r = if s.scan_roots.is_empty() {
                kraken::default_scan_roots()
            } else {
                s.scan_roots.clone()
            };
            r.iter().map(|p| norm(p)).filter(|p| p != "/").collect()
        } else {
            Vec::new()
        };
        Self { paths, roots }
    }

    /// Whether removing `path` would take a Kraken Mount game: it is one its API lists, inside
    /// one, or a folder holding one; or a `.pkg` file or folder directly in a folder it scans.
    pub fn keeps(&self, path: &str, is_dir: bool) -> bool {
        let p = norm(path);
        if !p.starts_with('/') || p == "/" {
            return false;
        }
        if self
            .paths
            .iter()
            .any(|g| *g == p || inside(&p, g) || (is_dir && inside(g, &p)))
        {
            return true;
        }
        let mountable = is_dir || p.to_ascii_lowercase().ends_with(".pkg");
        let parent = match p.rfind('/') {
            Some(0) | None => return false,
            Some(i) => &p[..i],
        };
        mountable && self.roots.iter().any(|r| r == parent)
    }
}

/// Marks the items of a scan reply that Kraken Mount uses: `kept: "kraken"`, `action: "none"`,
/// `default: false`. The ids of every kept item (the helper's own marks included).
pub fn mark_kept(reply: &mut Value, keep: &KrakenKeep) -> Vec<u32> {
    let mut ids = Vec::new();
    let Some(items) = reply.get_mut("items").and_then(Value::as_array_mut) else {
        return ids;
    };
    for it in items.iter_mut() {
        let Some(o) = it.as_object_mut() else {
            continue;
        };
        let already = o.get("kept").and_then(Value::as_str).is_some();
        let path = o.get("path").and_then(Value::as_str).unwrap_or("");
        let is_dir = o.get("is_dir").and_then(Value::as_bool).unwrap_or(false);
        if !already && !keep.keeps(path, is_dir) {
            continue;
        }
        o.insert("kept".into(), Value::from("kraken"));
        o.insert("action".into(), Value::from("none"));
        o.insert("default".into(), Value::from(false));
        if let Some(id) = o.get("id").and_then(Value::as_u64) {
            ids.push(id as u32);
        }
    }
    ids
}

/// Kraken Mount's paths and roots on the console at `addr`.
pub fn kraken_keep(addr: &str) -> KrakenKeep {
    KrakenKeep::from_status(&kraken::collect(addr, None))
}

/// [`scan_status`], with the items Kraken Mount uses marked kept once the scan has its list.
pub fn scan_status_marked(addr: &str, scan_id: u32) -> Result<Value> {
    let mut v = scan_status(addr, scan_id)?;
    if v.get("items")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        let kept = mark_kept(&mut v, &kraken_keep(addr));
        if !kept.is_empty() {
            crate::clog!(
                Info,
                "cleanup",
                "scan {scan_id} {addr}: {} item(s) kept for Kraken Mount",
                kept.len()
            );
        }
    }
    Ok(v)
}

/// The ids of `item_ids` that may be removed: the kept ones left out.
pub fn without_kept(item_ids: &[u32], kept: &[u32]) -> Vec<u32> {
    item_ids
        .iter()
        .copied()
        .filter(|id| !kept.contains(id))
        .collect()
}

/// [`apply_start`] for the items Kraken Mount does not use, checked against the scan and Kraken
/// Mount now. Refused with `kraken` when every item asked for is one of its games.
pub fn apply_start_guarded(addr: &str, scan_id: u32, item_ids: &[u32]) -> Result<Value> {
    let mut scan = scan_status(addr, scan_id)?;
    let kept = mark_kept(&mut scan, &kraken_keep(addr));
    let ids = without_kept(item_ids, &kept);
    if ids.len() < item_ids.len() {
        crate::clog!(
            Warn,
            "cleanup",
            "apply of scan {scan_id} {addr}: {} item(s) left out, Kraken Mount uses them",
            item_ids.len() - ids.len()
        );
    }
    if ids.is_empty() {
        bail!(mgmt::MgmtError {
            label: m::CLEANUP_APPLY.label.into(),
            status: 1,
            cause: r#"{"ok":false,"err":"kraken"}"#.into(),
        });
    }
    apply_start(addr, scan_id, &ids)
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

    fn game(path: &str, status: &str) -> kraken::KrakenGame {
        kraken::KrakenGame {
            title_id: "PPSA01234".into(),
            path: path.into(),
            kind: "pkg".into(),
            status: status.into(),
            ..Default::default()
        }
    }

    #[test]
    fn kraken_keeps_what_its_api_lists_and_what_sits_in_its_roots() {
        let st = KrakenStatus {
            installed: true,
            running: true,
            api: true,
            games: vec![
                game("/mnt/usb0/homebrew/Game.pkg", "ready"),
                game("/user/data/mine/Other.pkg", "duplicate"),
                game("/mnt/usb1/homebrew/PPSA05555-app", "ready"),
            ],
            scan_roots: kraken::default_scan_roots(),
            ..Default::default()
        };
        let k = KrakenKeep::from_status(&st);
        // listed by its API, however the path is written
        assert!(k.keeps("/mnt/usb0/homebrew/Game.pkg", false));
        assert!(k.keeps("/data/mine/Other.pkg", false));
        // inside a game folder it mounts, and a folder holding one
        assert!(k.keeps("/mnt/usb1/homebrew/PPSA05555-app/sce_sys/param.json", false));
        assert!(k.keeps("/mnt/usb1/homebrew", true));
        // in a root it scans, one level deep: a package or a folder, not other files
        assert!(k.keeps("/data/homebrew/Later.pkg", false));
        assert!(k.keeps("/mnt/usb2/LOUD.PKG", false));
        assert!(k.keeps("/mnt/usb2/etaHEN/games/PPSA07777", true));
        assert!(!k.keeps("/mnt/usb2/old/Game.pkg", false));
        assert!(!k.keeps("/mnt/usb2/Game.pkg.part", false));
        assert!(!k.keeps("/mnt/usb2/big.exfat", false));
        assert!(!k.keeps("/data/ps5upload/pkg_temp/x", true));
        assert!(!k.keeps("/user/patch/PPSA01234", true));
    }

    #[test]
    fn its_roots_count_while_it_is_stopped_but_not_where_it_never_ran() {
        let stopped = KrakenStatus {
            installed: true,
            scan_roots: vec!["/mnt/usb0/games".into()],
            ..Default::default()
        };
        let k = KrakenKeep::from_status(&stopped);
        assert!(k.keeps("/mnt/usb0/games/Game.pkg", false));
        // its own scanpath lines replace the default list
        assert!(!k.keeps("/data/homebrew/Game.pkg", false));

        let never = KrakenStatus {
            scan_roots: kraken::default_scan_roots(),
            ..Default::default()
        };
        assert!(!KrakenKeep::from_status(&never).keeps("/mnt/usb0/Game.pkg", false));

        // its folder could not be listed: assume it is there
        let unsure = KrakenStatus {
            errors: vec!["list /data/krakenmount: timed out".into()],
            ..Default::default()
        };
        assert!(KrakenKeep::from_status(&unsure).keeps("/mnt/usb0/Game.pkg", false));
    }

    #[test]
    fn kept_items_are_marked_and_left_out_of_an_apply() {
        let mut reply = serde_json::json!({
            "ok": true, "state": "done",
            "items": [
                {"id": 1, "cat": "C", "path": "/data/homebrew/Game.pkg", "is_dir": false,
                 "default": true, "action": "delete"},
                {"id": 2, "cat": "C", "path": "/data/ps5upload/downloads/x.pkg", "is_dir": false,
                 "default": true, "action": "delete"},
                {"id": 3, "cat": "G", "path": "/mnt/usb0/homebrew/PPSA01234-app", "is_dir": true,
                 "default": false, "action": "none", "kept": "kraken"},
            ]
        });
        let keep = KrakenKeep {
            paths: vec![],
            roots: vec!["/data/homebrew".into()],
        };
        let kept = mark_kept(&mut reply, &keep);
        assert_eq!(kept, vec![1, 3]);
        let first = &reply["items"][0];
        assert_eq!(first["kept"], "kraken");
        assert_eq!(first["action"], "none");
        assert_eq!(first["default"], false);
        assert!(reply["items"][1].get("kept").is_none());
        assert_eq!(reply["items"][1]["action"], "delete");
        assert_eq!(without_kept(&[1, 2, 3], &kept), vec![2]);
        assert!(without_kept(&[1], &kept).is_empty());
    }
}
