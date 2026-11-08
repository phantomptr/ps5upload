//! What Remote Play sends the console's controller, and how the last pairings went: for a bug
//! report (`rp_stream_api::diag_summary`), the log, and an agent (`GET /api/ps5/rp/controller`,
//! `GET /api/ps5/rp/pair/attempts`, MCP `rp_controller` and `rp_pair_attempts`).
//!
//! Two field reports were slow to pin down without this: L2/R2 reaching the PS5 as 0 or 255 only
//! (the page read a trigger the browser called "pressed" as pulled all the way), and pairing
//! refused with 80108b03 on a PS5 whose user was not signed in. Here each controller's last state
//! and its trigger peaks show the first at a glance ("half-press L2: did 128 arrive?"), and each
//! pairing try keeps its code, reason and what the console last said about its user and Remote
//! Play switch.
//!
//! Kept in memory only, and only read when asked: the controller's few fields are updated in place
//! as input comes (no log line, no background work); a pairing try logs one line and the last 5
//! are kept. Never a PIN, a key or an account id: a pairing's error text is scrubbed of anything
//! that could be one before it is kept or logged ([`scrub`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use ps5upload_core::rp_stream::{button, ControllerState};
use serde_json::{json, Value};

// ── Controller ─────────────────────────────────────────────────────────────────────────────

/// One controller's latest state in a session, and how far its triggers went.
#[derive(Debug, Clone)]
struct Seen {
    state: ControllerState,
    peak_l2: u8,
    peak_r2: u8,
    /// The trigger levels seen other than 0 and 255, up to [`LEVELS_KEPT`]: none after pulls
    /// means the triggers arrive as a button would.
    between: Vec<u8>,
    /// Pulls seen (a trigger going from 0 to more).
    pulls: u32,
    updates: u64,
    at: Instant,
    /// Let go (the controller left), not just at rest.
    gone: bool,
}

const LEVELS_KEPT: usize = 8;

impl Seen {
    fn new(state: ControllerState) -> Self {
        let mut s = Seen {
            state: ControllerState::default(),
            peak_l2: 0,
            peak_r2: 0,
            between: Vec::new(),
            pulls: 0,
            updates: 0,
            at: Instant::now(),
            gone: false,
        };
        s.take(state);
        s
    }

    fn take(&mut self, state: ControllerState) {
        let prev = self.state;
        for (was, now) in [(prev.l2, state.l2), (prev.r2, state.r2)] {
            if was == 0 && now > 0 {
                self.pulls = self.pulls.saturating_add(1);
            }
            if now > 0
                && now < 255
                && !self.between.contains(&now)
                && self.between.len() < LEVELS_KEPT
            {
                self.between.push(now);
            }
        }
        self.peak_l2 = self.peak_l2.max(state.l2);
        self.peak_r2 = self.peak_r2.max(state.r2);
        self.state = state;
        self.updates += 1;
        self.at = Instant::now();
        self.gone = false;
    }

    /// Whether the triggers came through as analog: true once a level between 0 and 255 came,
    /// false after a few pulls that were all 0 or 255, null before that.
    fn analog(&self) -> Value {
        if !self.between.is_empty() {
            Value::Bool(true)
        } else if self.pulls >= 3 {
            Value::Bool(false)
        } else {
            Value::Null
        }
    }

    fn json(&self) -> Value {
        json!({
            "state": state_json(&self.state),
            "peak_l2": self.peak_l2,
            "peak_r2": self.peak_r2,
            "trigger_pulls": self.pulls,
            "trigger_levels_between": self.between,
            "triggers_analog": self.analog(),
            "updates": self.updates,
            "age_ms": self.at.elapsed().as_millis() as u64,
            "gone": self.gone,
        })
    }
}

const BUTTON_NAMES: [(u32, &str); 16] = [
    (button::CROSS, "cross"),
    (button::CIRCLE, "circle"),
    (button::SQUARE, "square"),
    (button::TRIANGLE, "triangle"),
    (button::DPAD_UP, "up"),
    (button::DPAD_DOWN, "down"),
    (button::DPAD_LEFT, "left"),
    (button::DPAD_RIGHT, "right"),
    (button::L1, "l1"),
    (button::R1, "r1"),
    (button::L3, "l3"),
    (button::R3, "r3"),
    (button::OPTIONS, "options"),
    (button::SHARE, "create"),
    (button::TOUCHPAD, "touchpad"),
    (button::PS, "ps"),
];

/// A controller state as an agent reads it: the buttons by name, triggers 0-255, sticks.
pub(crate) fn state_json(s: &ControllerState) -> Value {
    let pressed: Vec<&str> = BUTTON_NAMES
        .iter()
        .filter(|(bit, _)| s.buttons & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    json!({
        "buttons": s.buttons,
        "pressed": pressed,
        "l2": s.l2,
        "r2": s.r2,
        "lx": s.lx,
        "ly": s.ly,
        "rx": s.rx,
        "ry": s.ry,
        "touch": s.touch.is_some(),
    })
}

#[derive(Default)]
struct PadDiag {
    session: u64,
    host: String,
    /// By source (the page 0, phones 1 and up, a guest, automation): see `rp_stream_api`.
    sources: BTreeMap<u64, Seen>,
    /// What the console was sent: every controller merged.
    sent: Option<Seen>,
    /// What the page says feeds its controller (see [`page_from`]).
    page_from: Option<Value>,
}

static PADS: LazyLock<Mutex<PadDiag>> = LazyLock::new(|| Mutex::new(PadDiag::default()));

fn pads_for(g: &mut PadDiag, session: u64, host: &str) {
    if g.session != session {
        *g = PadDiag {
            session,
            host: host.to_string(),
            ..PadDiag::default()
        };
    }
}

/// A controller of session `session` changed (`None`: it left); `merged` is what the console
/// gets. A few fields updated in place: nothing logged, nothing allocated once a source is known.
pub(crate) fn pad_seen(
    session: u64,
    host: &str,
    source: u64,
    state: Option<&ControllerState>,
    merged: &ControllerState,
) {
    if let Ok(mut g) = PADS.lock() {
        pad_seen_in(&mut g, session, host, source, state, merged);
    }
}

fn pad_seen_in(
    g: &mut PadDiag,
    session: u64,
    host: &str,
    source: u64,
    state: Option<&ControllerState>,
    merged: &ControllerState,
) {
    pads_for(g, session, host);
    match state {
        Some(s) => match g.sources.get_mut(&source) {
            Some(seen) => seen.take(*s),
            None => {
                g.sources.insert(source, Seen::new(*s));
            }
        },
        None => {
            if let Some(seen) = g.sources.get_mut(&source) {
                seen.take(ControllerState::default());
                seen.gone = true;
            }
        }
    }
    match g.sent.as_mut() {
        Some(seen) => seen.take(*merged),
        None => g.sent = Some(Seen::new(*merged)),
    }
}

fn short(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| s.chars().take(max).collect::<String>())
        .filter(|s| !s.is_empty())
}

/// What the page says feeds its controller: `{inputs: [...], pads: [{id, mapping, profile,
/// reader}]}` (lib/remotePlayStream.ts `padFrom`), sent only when it changes. Only those fields
/// are kept, short; nothing is logged.
pub(crate) fn page_from(session: u64, host: &str, v: &Value) {
    if let Ok(mut g) = PADS.lock() {
        page_from_in(&mut g, session, host, v);
    }
}

fn page_from_in(g: &mut PadDiag, session: u64, host: &str, v: &Value) {
    const INPUTS: [&str; 6] = [
        "controller",
        "keys",
        "touch",
        "phone",
        "controls",
        "touchpad",
    ];
    let inputs: Vec<&str> = v
        .get("inputs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| INPUTS.iter().find(|k| **k == s).copied())
                .collect()
        })
        .unwrap_or_default();
    let pads: Vec<Value> = v
        .get("pads")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(4)
                .map(|p| {
                    json!({
                        "id": short(p.get("id"), 96),
                        "mapping": short(p.get("mapping"), 16),
                        "profile": short(p.get("profile"), 16),
                        "reader": short(p.get("reader"), 16),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    pads_for(g, session, host);
    g.page_from = Some(json!({ "inputs": inputs, "pads": pads }));
}

/// The controllers of the running session, or of the last one when `running` is not its id.
/// `label` names a source (`page`, `phone 1`, `guest`, `automation`).
pub(crate) fn controller_json(running: Option<u64>, label: fn(u64) -> String) -> Value {
    match PADS.lock() {
        Ok(g) => controller_json_in(&g, running, label),
        Err(_) => json!({ "session": null, "running": false, "sources": [] }),
    }
}

fn controller_json_in(g: &PadDiag, running: Option<u64>, label: fn(u64) -> String) -> Value {
    let live = running == Some(g.session);
    if g.session == 0 {
        return json!({ "session": null, "running": false, "sources": [] });
    }
    let sources: Vec<Value> = g
        .sources
        .iter()
        .map(|(id, s)| {
            let mut v = s.json();
            v["source"] = label(*id).into();
            if *id == 0 {
                v["from"] = g.page_from.clone().unwrap_or(Value::Null);
            }
            v
        })
        .collect();
    json!({
        "session": g.session,
        "host": g.host,
        "running": live,
        "sent": g.sent.as_ref().map(Seen::json),
        "sources": sources,
    })
}

// ── Pairing ────────────────────────────────────────────────────────────────────────────────

/// What the console last said about Remote Play and its user, by console (remoteplay readiness).
#[derive(Debug, Clone)]
struct ConsoleSeen {
    at_unix_ms: u64,
    v: Value,
}

static READINESS: LazyLock<Mutex<BTreeMap<String, ConsoleSeen>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

static ATTEMPTS: LazyLock<Mutex<VecDeque<Value>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

/// The pairing tries kept.
const ATTEMPTS_KEPT: usize = 5;

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn host_key(host: &str) -> String {
    crate::console_snapshot::host_key(host)
}

/// The console answered a readiness check: kept (its switches and how a user was found, never the
/// account id) for the next pairing try's record.
pub(crate) fn readiness_seen(addr: &str, r: &ps5upload_core::remoteplay::RemotePlayReadiness) {
    let v = json!({
        "remote_play_on": r.service_enabled != 0,
        "user_allowed": r.user_enabled != 0,
        "user_found": r.account_uid >= 0,
        "user_in_front": r.foreground_uid >= 0,
        "user_found_via": r.account_via,
        "account_type": r.account_type,
        "registry_readable": r.registry_err == 0,
    });
    if let Ok(mut g) = READINESS.lock() {
        g.insert(
            host_key(addr),
            ConsoleSeen {
                at_unix_ms: unix_ms(),
                v,
            },
        );
    }
}

/// `text` with anything that could be a PIN or a key taken out: runs of 6 or more digits, and of
/// 16 or more hex digits. Codes (80108b03), ports and addresses stay.
pub(crate) fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_hexdigit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_hexdigit() {
                i += 1;
            }
            let run = &chars[start..i];
            let digits_only = run.iter().all(char::is_ascii_digit);
            if run.len() >= 16 || (digits_only && run.len() >= 6) {
                out.push('…');
            } else {
                out.extend(run);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The console's refusal code in a pairing error (`reason 80108b03`), lower case.
pub(crate) fn refusal_code(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("80108b")?;
    let code: String = lower[at..].chars().take(8).collect();
    (code.len() == 8 && code.chars().all(|c| c.is_ascii_hexdigit())).then_some(code)
}

/// A pairing try ended. `with`: "helper" (one press) or "pin" (the PIN the PS5 shows). `error`:
/// None when it paired. Logged and kept, scrubbed.
pub(crate) fn pair_attempt(host: &str, with: &str, error: Option<&str>) {
    let record = attempt_record(host, with, error);
    if let Ok(mut g) = ATTEMPTS.lock() {
        keep(&mut g, record);
    }
}

/// One pairing try as kept and logged.
fn attempt_record(host: &str, with: &str, error: Option<&str>) -> Value {
    let console = READINESS
        .lock()
        .ok()
        .and_then(|g| g.get(&host_key(host)).cloned())
        .map(|c| {
            let mut v = c.v;
            v["seen_unix_ms"] = c.at_unix_ms.into();
            v
        });
    let reason = error.map(scrub);
    let code = error.and_then(refusal_code);
    let record = json!({
        "at_unix_ms": unix_ms(),
        "host": host,
        "with": with,
        "ok": error.is_none(),
        "code": code,
        "reason": reason,
        "console": console,
    });
    match &reason {
        None => crate::log_at!(Info, "rp", "pair: {host}: paired (with {with})"),
        Some(r) => crate::log_at!(
            Warn,
            "rp",
            "pair: {host}: refused (with {with}){}: {r}; console last seen: {}",
            code.as_ref().map(|c| format!(" {c}")).unwrap_or_default(),
            record["console"]
        ),
    }
    record
}

fn keep(g: &mut VecDeque<Value>, record: Value) {
    if g.len() >= ATTEMPTS_KEPT {
        g.pop_front();
    }
    g.push_back(record);
}

/// The last pairing tries, newest last.
pub(crate) fn pair_attempts() -> Vec<Value> {
    ATTEMPTS
        .lock()
        .map(|g| g.iter().cloned().collect())
        .unwrap_or_default()
}

/// `GET /api/ps5/rp/pair/attempts` — the last pairing tries (at most 5, newest last): when, which
/// console, with the helper or the PS5's PIN, whether it paired, the console's refusal code and
/// reason, and what the console last said about its user and Remote Play switch. Never a PIN or
/// a key.
pub(crate) async fn pair_attempts_handler() -> axum::Json<Value> {
    axum::Json(json!({ "attempts": pair_attempts() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(n: u64) -> String {
        if n == 0 {
            "page".into()
        } else {
            format!("phone {n}")
        }
    }

    fn st(l2: u8, r2: u8) -> ControllerState {
        ControllerState {
            l2,
            r2,
            ..ControllerState::default()
        }
    }

    #[test]
    fn a_half_pulled_trigger_shows_as_half_and_analog() {
        let mut g = PadDiag::default();
        pad_seen_in(&mut g, 7, "ps5", 0, Some(&st(128, 0)), &st(128, 0));
        pad_seen_in(&mut g, 7, "ps5", 0, Some(&st(0, 0)), &st(0, 0));
        let v = controller_json_in(&g, Some(7), label);
        assert_eq!(v["session"], 7);
        assert_eq!(v["running"], true);
        let page = &v["sources"][0];
        assert_eq!(page["source"], "page");
        assert_eq!(page["peak_l2"], 128);
        assert_eq!(page["triggers_analog"], true);
        assert_eq!(v["sent"]["peak_l2"], 128);
        // Another session starts afresh.
        pad_seen_in(&mut g, 8, "ps5", 0, Some(&st(0, 0)), &st(0, 0));
        assert_eq!(
            controller_json_in(&g, Some(8), label)["sources"][0]["peak_l2"],
            0
        );
        assert_eq!(controller_json_in(&g, None, label)["running"], false);
    }

    #[test]
    fn triggers_that_only_reach_0_or_255_are_called_out() {
        let mut g = PadDiag::default();
        for _ in 0..3 {
            pad_seen_in(&mut g, 2, "ps5", 0, Some(&st(255, 0)), &st(255, 0));
            pad_seen_in(&mut g, 2, "ps5", 0, Some(&st(0, 0)), &st(0, 0));
        }
        let v = controller_json_in(&g, None, label);
        assert_eq!(v["sources"][0]["triggers_analog"], false);
        assert_eq!(v["sources"][0]["trigger_pulls"], 3);
        // A controller that leaves is let go, and says so.
        pad_seen_in(&mut g, 2, "ps5", 1, Some(&st(0, 40)), &st(0, 40));
        pad_seen_in(&mut g, 2, "ps5", 1, None, &st(0, 0));
        let v = controller_json_in(&g, Some(2), label);
        let phone = v["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["source"] == "phone 1")
            .unwrap()
            .clone();
        assert_eq!(phone["gone"], true);
        assert_eq!(phone["peak_r2"], 40);
        assert_eq!(phone["state"]["r2"], 0);
    }

    #[test]
    fn nothing_seen_yet_reads_as_no_session() {
        let v = controller_json_in(&PadDiag::default(), None, label);
        assert_eq!(v["session"], Value::Null);
        assert_eq!(v["sources"], json!([]));
    }

    #[test]
    fn the_page_says_what_feeds_its_controller_and_nothing_else_is_kept() {
        let mut g = PadDiag::default();
        page_from_in(
            &mut g,
            3,
            "ps5",
            &json!({
                "inputs": ["controller", "keys", "secret"],
                "pads": [{"id": "DualSense (app)", "mapping": "standard", "profile": "standard",
                          "reader": "native", "extra": "x"}],
                "other": 1,
            }),
        );
        pad_seen_in(&mut g, 3, "ps5", 0, Some(&st(1, 0)), &st(1, 0));
        let v = controller_json_in(&g, Some(3), label);
        let from = &v["sources"][0]["from"];
        assert_eq!(from["inputs"], json!(["controller", "keys"]));
        assert_eq!(from["pads"][0]["reader"], "native");
        assert!(from["pads"][0].get("extra").is_none());
        assert!(from.get("other").is_none());
    }

    #[test]
    fn buttons_are_named() {
        let v = state_json(&ControllerState {
            buttons: button::CROSS | button::R1,
            ..ControllerState::default()
        });
        assert_eq!(v["pressed"], json!(["cross", "r1"]));
    }

    #[test]
    fn a_pairing_try_keeps_its_code_and_what_the_console_said_never_a_pin_or_key() {
        let host = "192.0.2.77";
        let r: ps5upload_core::remoteplay::RemotePlayReadiness = serde_json::from_value(json!({
            "service_enabled": 1,
            "user_enabled": 1,
            "account_uid": -1,
            "foreground_uid": -1,
            "account_via": "none",
            "account_id_b64": "AAAAAAAAAAA=",
        }))
        .unwrap();
        readiness_seen(host, &r);
        let err = "registration failed (HTTP 403): Remote Play isn't ready (reason 80108b03); \
                   PIN 12345678 key 0123456789abcdef0123";
        let rec = attempt_record(host, "helper", Some(err));
        assert_eq!(rec["code"], "80108b03");
        assert_eq!(rec["ok"], false);
        assert_eq!(rec["with"], "helper");
        assert_eq!(rec["console"]["user_found"], false);
        assert_eq!(rec["console"]["remote_play_on"], true);
        let text = rec.to_string();
        assert!(!text.contains("12345678"), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(!text.contains("AAAAAAAAAAA"), "{text}");
        assert!(text.contains("HTTP 403"), "{text}");
        // A console never checked: nothing to say about it.
        assert_eq!(
            attempt_record("192.0.2.78", "pin", None)["console"],
            Value::Null
        );
    }

    #[test]
    fn scrub_keeps_codes_ports_and_addresses() {
        assert_eq!(
            scrub("connecting to 192.168.1.20:9295: reason 80108B03"),
            "connecting to 192.168.1.20:9295: reason 80108B03"
        );
        assert_eq!(scrub("the PIN 87654321 is wrong"), "the PIN … is wrong");
        assert_eq!(
            refusal_code("x (reason 80108B09)").as_deref(),
            Some("80108b09")
        );
        assert_eq!(refusal_code("timed out"), None);
    }

    #[test]
    fn at_most_ten_tries_are_kept() {
        let mut g = VecDeque::new();
        for n in 0..15 {
            keep(&mut g, json!(n));
        }
        assert_eq!(g.len(), ATTEMPTS_KEPT);
        assert_eq!(g.back(), Some(&json!(14)));
    }
}
