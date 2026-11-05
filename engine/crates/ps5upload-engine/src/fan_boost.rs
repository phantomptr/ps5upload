//! Fan boost: a cooler fan target for a while, always undone.
//!
//! The helper keeps a fan target across reboots (it saves it and puts it back whenever the
//! system resets it), so a boost left on by accident would stay on for good. The engine
//! therefore keeps its own record of every boost, with the target it replaced, in
//! `<data>/fan-boost.json`, and undoes it when it is turned off, when that console's Remote Play
//! session ends, when it runs out (30 minutes), and, should the app have closed in between, as
//! soon as the console answers again. A boost never makes the fan quieter than it was.
//!
//! - `GET  /api/ps5/fan-boost?addr=` → `{on, until_ms, boost_c, previous_c}`
//! - `POST /api/ps5/fan-boost {addr, on}` → the same

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{extract::Query, http::StatusCode, Json};
use ps5upload_core::hw::{hw_set_fan_threshold, hw_temps, FAN_RESTORE_CONSOLE};
use serde::{Deserialize, Serialize};

/// The boost's target (°C): the fan works to keep the console at this.
pub const BOOST_C: u8 = 55;
/// How long a boost lasts unless turned off first.
pub const BOOST_FOR: Duration = Duration::from_secs(30 * 60);
/// How often expired or not-yet-undone boosts are looked at.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// One console's boost.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Boost {
    /// The target it replaced: a pinned °C, or 0 for the console's own control.
    pub previous_c: u8,
    pub until_ms: u64,
    /// It ran out or was turned off, but setting the previous target back failed (the console
    /// was away): retried until it works.
    #[serde(default)]
    pub undo_pending: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn path() -> Option<PathBuf> {
    Some(crate::remote::store::data_dir()?.join("fan-boost.json"))
}

fn lock() -> &'static Mutex<BTreeMap<String, Boost>> {
    static L: OnceLock<Mutex<BTreeMap<String, Boost>>> = OnceLock::new();
    L.get_or_init(|| {
        let map = path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Mutex::new(map)
    })
}

fn save(map: &BTreeMap<String, Boost>) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(map) {
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// The console's host, the record's key (any port stripped).
fn key(addr: &str) -> String {
    ps5upload_core::events::console_host(addr)
}

/// What to set for a boost given the target in force now (`pinned`, 0 = the console's own):
/// `None` when that is already as cool as a boost or cooler (nothing to do).
pub fn boost_target(pinned: u8) -> Option<u8> {
    if pinned != FAN_RESTORE_CONSOLE && pinned <= BOOST_C {
        None
    } else {
        Some(BOOST_C)
    }
}

/// Undo `b` on `host`: put the previous target back. Err when the console did not take it.
fn undo(host: &str, b: &Boost) -> anyhow::Result<()> {
    hw_set_fan_threshold(host, b.previous_c)
}

/// Turn a boost on (blocking: talks to the console).
fn start(host: &str) -> Result<Boost, String> {
    {
        let map = lock().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = map.get(host).filter(|b| !b.undo_pending) {
            return Ok(b.clone());
        }
    }
    let t = hw_temps(host, false).map_err(|e| format!("reading the fan target: {e}"))?;
    let pinned = u8::try_from(t.fan_pinned_c.max(0)).unwrap_or(0);
    let Some(target) = boost_target(pinned) else {
        return Err(format!(
            "the fan is already set cooler ({pinned}°C) than a boost ({BOOST_C}°C)"
        ));
    };
    hw_set_fan_threshold(host, target).map_err(|e| format!("setting the fan: {e}"))?;
    let b = Boost {
        previous_c: pinned,
        until_ms: now_ms() + BOOST_FOR.as_millis() as u64,
        undo_pending: false,
    };
    let mut map = lock().lock().unwrap_or_else(|e| e.into_inner());
    // A boost still waiting to be undone keeps the target from before it: that is the real one.
    let previous_c = map
        .get(host)
        .filter(|old| old.undo_pending)
        .map(|old| old.previous_c)
        .unwrap_or(pinned);
    let b = Boost { previous_c, ..b };
    map.insert(host.to_string(), b.clone());
    save(&map);
    crate::log_info!("fan_boost: on for {host} ({target}°C, was {previous_c})");
    Ok(b)
}

/// Turn a boost off (blocking). No boost: nothing to do.
fn stop(host: &str) -> Result<(), String> {
    let Some(b) = lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(host)
        .cloned()
    else {
        return Ok(());
    };
    let r = undo(host, &b);
    let mut map = lock().lock().unwrap_or_else(|e| e.into_inner());
    match r {
        Ok(()) => {
            map.remove(host);
            save(&map);
            crate::log_info!("fan_boost: off for {host} (back to {})", b.previous_c);
            Ok(())
        }
        Err(e) => {
            if let Some(x) = map.get_mut(host) {
                x.undo_pending = true;
            }
            save(&map);
            crate::log_warn!("fan_boost: undo for {host} failed, will retry: {e}");
            Err(format!("setting the fan back: {e} (it will be retried)"))
        }
    }
}

/// A Remote Play session with `host` ended: its boost ends with it.
pub fn session_ended(host: &str) {
    let host = key(host);
    if lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&host)
    {
        let _ = stop(&host);
    }
}

/// Every minute: end boosts that ran out, and retry undos that failed.
pub fn spawn_sweeper() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            let due: Vec<String> = {
                let now = now_ms();
                lock()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter(|(_, b)| b.undo_pending || b.until_ms <= now)
                    .map(|(h, _)| h.clone())
                    .collect()
            };
            for host in due {
                let _ = tokio::task::spawn_blocking(move || stop(&host)).await;
            }
        }
    });
}

fn view(host: &str) -> serde_json::Value {
    let map = lock().lock().unwrap_or_else(|e| e.into_inner());
    match map.get(host).filter(|b| !b.undo_pending) {
        Some(b) => serde_json::json!({
            "on": true, "until_ms": b.until_ms, "boost_c": BOOST_C, "previous_c": b.previous_c,
        }),
        None => serde_json::json!({ "on": false, "boost_c": BOOST_C }),
    }
}

#[derive(Deserialize)]
pub struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: String,
}

#[derive(Deserialize)]
pub struct SetReq {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    on: bool,
}

pub async fn get(Query(q): Query<AddrQuery>) -> Json<serde_json::Value> {
    Json(view(&key(&q.addr)))
}

pub async fn post(
    Json(r): Json<SetReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let host = key(&r.addr);
    let h = host.clone();
    let res = tokio::task::spawn_blocking(move || {
        if r.on {
            start(&h).map(|_| ())
        } else {
            stop(&h)
        }
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|x| x);
    match res {
        Ok(()) => Ok(Json(view(&host))),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": e })),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boost_never_makes_the_fan_quieter() {
        // The console's own control, or a warmer pinned target: boost to 55 °C.
        assert_eq!(boost_target(FAN_RESTORE_CONSOLE), Some(BOOST_C));
        assert_eq!(boost_target(70), Some(BOOST_C));
        // Already as cool or cooler: nothing to boost.
        assert_eq!(boost_target(55), None);
        assert_eq!(boost_target(48), None);
    }

    #[test]
    fn a_record_survives_a_restart() {
        let b = Boost {
            previous_c: 0,
            until_ms: 42,
            undo_pending: true,
        };
        let json =
            serde_json::to_vec(&BTreeMap::from([("10.0.0.9".to_string(), b.clone())])).unwrap();
        let back: BTreeMap<String, Boost> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.get("10.0.0.9"), Some(&b));
        // An older record without the pending flag reads as not pending.
        let old: Boost = serde_json::from_str(r#"{"previous_c":60,"until_ms":1}"#).unwrap();
        assert!(!old.undo_pending);
    }

    #[test]
    fn the_key_is_the_bare_host() {
        assert_eq!(key("192.168.1.5:9120"), "192.168.1.5");
        assert_eq!(key("192.168.1.5"), "192.168.1.5");
    }
}
