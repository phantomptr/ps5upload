//! The last status check of each console, phase by phase (`GET /api/ps5/connect-timings`).
//!
//! Read on demand, never logged: one small record per console, replaced by the next check. It
//! answers "why did this console take so long to show as connected?" with numbers: was a pooled
//! session reused or a new one opened (and how long the handshake took), did a remembered
//! failure answer, did a quick connect look past it, how long `node.status` took, and whether a
//! session that had gone quiet was dropped.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

/// One console's last status check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConnectTiming {
    /// The console, without a port.
    pub console: String,
    /// When the check started, Unix ms.
    pub at_ms: u64,
    /// The whole check, in ms.
    pub total_ms: u64,
    /// The helper answered.
    pub ok: bool,
    /// The failure's token (`console_rest_mode`, `ava1_unreachable`, ...), or its text when it
    /// has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// How the console was reached: `lan`, `tailnet_direct`, `tailnet_relayed` or
    /// `tailnet_system` (set by the engine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A live session was already pooled, so nothing was opened. Absent when a remembered
    /// failure answered before a session was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reused_session: Option<bool>,
    /// A failure remembered from a moment ago answered; nothing reached the console.
    pub cached_verdict: bool,
    /// A plain TCP connect to the helper's port: the quick look past a remembered failure, or
    /// the check that a quiet session's console still answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_ms: Option<u64>,
    /// Whether that TCP connect was taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_ok: Option<bool>,
    /// Opening a new session: the TCP connect, the AVA1 handshake and the pairing check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake_ms: Option<u64>,
    /// The `node.status` call itself (the last attempt).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_ms: Option<u64>,
    /// The helper's version from the reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A pooled session had stopped answering and was dropped (then opened again, or the
    /// console was found gone).
    pub stale_session_dropped: bool,
}

/// At most this many consoles are remembered (the oldest check goes first).
const MAX_CONSOLES: usize = 64;

static LAST: Mutex<Option<HashMap<String, ConnectTiming>>> = Mutex::new(None);

/// Keeps `t` as its console's last check.
pub fn record(t: ConnectTiming) {
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if !m.contains_key(&t.console) && m.len() >= MAX_CONSOLES {
        if let Some(oldest) = m
            .values()
            .min_by_key(|v| v.at_ms)
            .map(|v| v.console.clone())
        {
            m.remove(&oldest);
        }
    }
    m.insert(t.console.clone(), t);
}

/// Changes `console`'s last check (the engine adds what only it knows: the path).
pub fn update(console: &str, f: impl FnOnce(&mut ConnectTiming)) {
    let key = crate::pool::host_of(console);
    if let Some(t) = LAST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.get_mut(&key))
    {
        f(t);
    }
}

/// `console`'s last check, if there was one.
pub fn last(console: &str) -> Option<ConnectTiming> {
    let key = crate::pool::host_of(console);
    LAST.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key).cloned())
}

/// Every console's last check, newest first.
pub fn all() -> Vec<ConnectTiming> {
    let mut v: Vec<ConnectTiming> = LAST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then(a.console.cmp(&b.console)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_replaces_the_last_one_of_its_console_and_keys_by_host() {
        record(ConnectTiming {
            console: "timings-a".into(),
            at_ms: 1,
            ok: false,
            ..Default::default()
        });
        record(ConnectTiming {
            console: "timings-a".into(),
            at_ms: 2,
            ok: true,
            ..Default::default()
        });
        update("timings-a:9120", |t| t.path = Some("lan".into()));
        let t = last("timings-a").unwrap();
        assert!(t.ok);
        assert_eq!(t.at_ms, 2);
        assert_eq!(t.path.as_deref(), Some("lan"));
        assert!(all().iter().any(|t| t.console == "timings-a"));
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("reason").is_none(), "{json}");
        assert_eq!(json["stale_session_dropped"], false);
    }
}
