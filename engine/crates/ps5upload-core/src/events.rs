//! The one door every engine crate reports an event through (bug-report spec §1.1). The engine
//! installs a sink at startup (its journal); with no sink installed, `emit` is a no-op, so tests
//! and tools pay nothing.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const MSG_MAX: usize = 1024;
const DETAIL_MAX: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cat {
    Connection,
    Helper,
    Transfer,
    Install,
    Api,
    App,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Src {
    Engine,
    App,
    Helper,
}

/// One journal record; the client's `EventRecord` has the same shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub ts: u64,
    pub src: Src,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub console: Option<String>,
    pub cat: Cat,
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_ts: Option<u64>,
    /// The engine's event number (the `id:` of `/api/events`), set when the event enters the
    /// journal; `/api/event-journal?since_seq=` resumes from it. None on app-side records.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub seq: Option<u64>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn trim_utf8(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// The console's address without its port, brackets kept: `10.0.0.2:9120` -> `10.0.0.2`,
/// `[fe80::1]:9120` -> `[fe80::1]`. A bare IPv6 literal (several colons, no brackets) has no port
/// to lose. AVA1 has one port, so a `:port` from an older client or a saved address means nothing.
/// This is the form that takes a port again: the session pool's key.
pub fn console_addr(addr: &str) -> String {
    let a = addr.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return match rest.find(']') {
            Some(i) => format!("[{}]", &rest[..i]),
            None => a.to_string(),
        };
    }
    match a.split_once(':') {
        Some((host, port)) if !port.contains(':') => host.to_string(),
        _ => a.to_string(),
    }
}

/// The console's bare host, as a peer's IP prints: [`console_addr`] without the brackets.
/// `192.0.2.100:9120` -> `192.0.2.100`, `[fe80::1]:9120` -> `fe80::1`.
pub fn console_host(addr: &str) -> String {
    let a = console_addr(addr);
    match a.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']').unwrap_or(rest).to_string(),
        None => a,
    }
}

impl Event {
    /// Secrets out of its message and detail (`secret_text`): where events are kept or sent.
    pub fn without_secrets(mut self) -> Self {
        if let std::borrow::Cow::Owned(clean) = crate::secret_text::without_secrets(&self.msg) {
            self.msg = clean;
        }
        if let Some(detail) = self.detail.as_mut() {
            crate::secret_text::value_without_secrets(detail);
        }
        self
    }

    /// An engine event stamped now. `msg` is cut to 1 KiB; a `detail` over 2 KiB is dropped.
    pub fn new(
        cat: Cat,
        level: Level,
        code: &str,
        console: Option<&str>,
        msg: String,
        detail: Option<serde_json::Value>,
    ) -> Self {
        let detail = detail.filter(|d| {
            serde_json::to_string(d)
                .map(|s| s.len() <= DETAIL_MAX)
                .unwrap_or(false)
        });
        Event {
            ts: now_ms(),
            src: Src::Engine,
            console: console.map(console_host),
            cat,
            level,
            code: (!code.is_empty()).then(|| code.to_string()),
            msg: trim_utf8(msg, MSG_MAX),
            detail,
            count: None,
            last_ts: None,
            seq: None,
        }
    }
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();

/// Installs the process-wide sink. The first call wins: the engine installs its journal once.
pub fn set_sink(f: impl Fn(Event) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(f));
}

pub fn emit_event(e: Event) {
    if let Some(s) = SINK.get() {
        s(e)
    }
}

pub fn emit(cat: Cat, level: Level, code: &str, console: Option<&str>, msg: impl Into<String>) {
    emit_event(Event::new(cat, level, code, console, msg.into(), None));
}

pub fn emit_detail(
    cat: Cat,
    level: Level,
    code: &str,
    console: Option<&str>,
    msg: impl Into<String>,
    detail: serde_json::Value,
) {
    emit_event(Event::new(
        cat,
        level,
        code,
        console,
        msg.into(),
        Some(detail),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_keeps_no_secret() {
        let e = Event::new(
            Cat::Api,
            Level::Error,
            "x",
            None,
            "GET https://u:hunter2@h/x?token=abc123 failed".into(),
            Some(serde_json::json!({"rp_key": "abc123", "why": "password=hunter2"})),
        )
        .without_secrets();
        let text = serde_json::to_string(&e).unwrap();
        assert!(
            !text.contains("hunter2") && !text.contains("abc123"),
            "{text}"
        );
    }
    use std::sync::{Arc, Mutex};

    #[test]
    fn emit_reaches_sink_with_trimmed_fields() {
        let got: Arc<Mutex<Vec<Event>>> = Arc::default();
        let g = got.clone();
        set_sink(move |e| g.lock().unwrap().push(e));
        emit(
            Cat::Connection,
            Level::Warn,
            "conn_lost",
            Some("10.0.0.9:9120"),
            "x".repeat(5000),
        );
        let v = got.lock().unwrap();
        let e = v.last().unwrap();
        assert_eq!(e.console.as_deref(), Some("10.0.0.9"));
        assert_eq!(e.code.as_deref(), Some("conn_lost"));
        assert!(e.msg.len() <= 1024);
        assert!(matches!(e.src, Src::Engine));
        assert!(e.ts > 1_700_000_000_000);
    }

    #[test]
    fn oversized_detail_is_dropped() {
        let big = serde_json::json!({ "s": "y".repeat(4096) });
        let e = Event::new(Cat::Api, Level::Error, "x", None, "m".into(), Some(big));
        assert!(e.detail.is_none());
    }

    #[test]
    fn serializes_lowercase_and_skips_none() {
        let e = Event::new(
            Cat::Install,
            Level::Info,
            "install_start",
            None,
            "m".into(),
            None,
        );
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""cat":"install""#) && s.contains(r#""level":"info""#));
        assert!(!s.contains("console") && !s.contains("count"));
    }

    #[test]
    fn console_host_strips_port_and_brackets() {
        assert_eq!(console_host("192.0.2.100:9120"), "192.0.2.100");
        assert_eq!(console_host("[fe80::1]:9120"), "fe80::1");
        assert_eq!(console_host("ps5.local"), "ps5.local");
        assert_eq!(console_host("fe80::1"), "fe80::1");
        assert_eq!(console_host("[::1]:"), "::1");
        assert_eq!(console_host("[2001:db8::1]:foo"), "2001:db8::1");
        assert_eq!(console_host(" 10.0.0.2:9120 "), "10.0.0.2");
        assert_eq!(console_host(""), "");
    }

    #[test]
    fn console_addr_keeps_brackets_for_the_port_it_will_get_back() {
        for (typed, want) in [
            ("10.0.0.2", "10.0.0.2"),
            ("10.0.0.2:9021", "10.0.0.2"),
            ("ps5.lan:9120", "ps5.lan"),
            ("[::1]", "[::1]"),
            ("[::1]:9021", "[::1]"),
            ("fe80::1", "fe80::1"),
            (":1234", ""),
        ] {
            assert_eq!(console_addr(typed), want, "{typed}");
        }
    }

    #[test]
    fn multibyte_msg_is_cut_on_a_char_boundary() {
        let e = Event::new(Cat::App, Level::Info, "x", None, "多".repeat(600), None);
        assert!(e.msg.len() <= 1024);
        assert!(e.msg.chars().all(|c| c == '多'));
    }
}
