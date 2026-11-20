//! The one gate every helper send passes, from every front end: the desktop app (through
//! `POST /api/payload/send`), the browser build (`POST /api/pkg/payload-restore`) and anything
//! scripted against the API.
//!
//! Sending the helper replaces the one running: its start-up stops whatever the old one ran, and
//! every device's connection drops. Sent at the wrong moment it has crashed consoles (a helper sent
//! into a console still booting, FW 11.40, three times out of three) and made two app versions
//! replace each other's helper. The client decides when an unattended send is due; this gate makes
//! sure no front end can get round the rules that keep a send safe:
//!
//! - one send per console at a time (the console's loader lock), and none within
//!   [`RESEND_COOLDOWN`] of the last one, checked under that lock;
//! - never into an install, nor over this engine's own running jobs on the console;
//! - unattended (`mode: "auto"`): never over a helper that answers unless the caller says it means
//!   to replace it (an older helper), never when that helper reports work running, never past
//!   [`AUTO_SENDS_PER_HOUR`] sends an hour (kept across restarts), never once the console
//!   restarted twice right after such a send, never with a helper of another version than the
//!   app's, never to an address that now answers as another console; and the running helper is
//!   asked to leave only if it is idle (`node.shutdown {if_idle:true}`);
//! - a person's send (`mode: "explicit"`, the default for older callers) is refused only for what
//!   a person may override, and `force: true` (their "Send anyway") overrides it.
//!
//! Every refusal says why with a stable `code` the client keys on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use ps5upload_core::payload_lifecycle::{
    self as pl, compare_versions, helper_image_version, Evict, SendFailure, SentPayload,
    PS5_LOADER_PORT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::pkg_install::PkgInstallStateHandle;

/// No second helper this soon after one was sent to the console: it is still starting (taking
/// over, arming its listeners) and answers nothing meanwhile.
pub(crate) const RESEND_COOLDOWN: Duration = Duration::from_secs(45);
/// Unattended sends one console may get in an hour; then they stop until a person sends one.
pub(crate) const AUTO_SENDS_PER_HOUR: usize = 4;
const HOUR_MS: u64 = 3_600_000;
/// Unattended sends stop once the console restarted this many times in a row right after one.
pub(crate) const RESTARTS_BEFORE_STOP: u32 = 2;
/// How long an unattended send waits for another loader operation on the console.
const AUTO_LOADER_WAIT: Duration = Duration::from_secs(15);
/// The status call right before an unattended send.
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Nobody asked: the reconnect loop, a queue's recovery, the automatic update.
    Auto,
    /// A person asked (a Send helper button, starting an upload).
    Explicit,
}

/// How a caller asks for a helper send. Every field is optional: an older client sends none and
/// gets an explicit send, as before.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SendOpts {
    /// `"auto"` or `"explicit"` (the default).
    #[serde(default)]
    pub mode: Option<String>,
    /// The person confirmed "Send anyway": the refusals a person may override are skipped.
    #[serde(default)]
    pub force: bool,
    /// Unattended only: the caller means to replace a helper that answers (an older one).
    #[serde(default)]
    pub replace: bool,
    /// The caller's app version, which the helper sent must match for an unattended send
    /// (default: this engine's version).
    #[serde(default)]
    pub app_version: Option<String>,
}

impl SendOpts {
    pub(crate) fn mode(&self) -> Mode {
        if self.mode.as_deref().map(str::trim) == Some("auto") {
            Mode::Auto
        } else {
            Mode::Explicit
        }
    }
}

/// Why a send did not go. `forceable`: a person's send may go anyway (`force`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub code: &'static str,
    pub message: String,
    pub retry_after_ms: Option<u64>,
    pub forceable: bool,
}

impl Refusal {
    fn new(code: &'static str, message: impl Into<String>, forceable: bool) -> Self {
        Refusal {
            code,
            message: message.into(),
            retry_after_ms: None,
            forceable,
        }
    }

    fn retry_after(mut self, d: Duration) -> Self {
        self.retry_after_ms = Some(d.as_millis() as u64);
        self
    }

    /// The fields every refusal answers with (`error` stays the code, as `send-check` had it).
    pub(crate) fn fields(&self) -> Value {
        json!({
            "ok": false,
            "error": self.code,
            "refused": self.code,
            "reason": self.code,
            "message": self.message,
            "forceable": self.forceable,
            "retry_after_ms": self.retry_after_ms,
        })
    }
}

/// How a gated send ended when it did not go through.
#[derive(Debug)]
pub(crate) enum GateError {
    Refused(Refusal),
    /// The send failed (loader closed, image not an ELF...): the text says how.
    Failed(String),
}

/// One console's record of unattended sends, kept across engine restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Book {
    /// When each unattended send of the last hour went (unix ms).
    #[serde(default)]
    pub auto_sends: Vec<u64>,
    /// Restarts in a row right after an unattended send.
    #[serde(default)]
    pub restarts: u32,
    /// An unattended send went and its helper has not answered yet.
    #[serde(default)]
    pub awaiting: bool,
    /// The instance that answered before that send (the new helper is another one).
    #[serde(default)]
    pub awaiting_instance: Option<u64>,
    /// The console stopped answering at all (off, restarting) while waiting.
    #[serde(default)]
    pub went_dark: bool,
    /// The console's DDP host id (its MAC) as last sent to: an address that answers as another
    /// console (two PS5s that swapped DHCP addresses) gets nothing unasked.
    #[serde(default)]
    pub host_id: Option<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unattended sends that may not go because of the console's record.
pub(crate) fn book_refusal(book: &Book, now: u64) -> Option<Refusal> {
    if book.restarts >= RESTARTS_BEFORE_STOP {
        return Some(Refusal::new(
            "console_restarted_after_sends",
            "The PS5 restarted twice right after the helper was sent to it by itself, so it is not \
             sent by itself any more. Check the console, then press Send helper.",
            false,
        ));
    }
    let recent: Vec<u64> = book
        .auto_sends
        .iter()
        .copied()
        .filter(|&t| now.saturating_sub(t) < HOUR_MS)
        .collect();
    if recent.len() >= AUTO_SENDS_PER_HOUR {
        let oldest = recent.iter().copied().min().unwrap_or(now);
        let wait = (oldest + HOUR_MS).saturating_sub(now);
        return Some(
            Refusal::new(
                "send_budget_spent",
                format!(
                    "The helper was sent {} times in the last hour without staying up, so it is not \
                     sent by itself for now. Press Send helper to send it yourself.",
                    recent.len()
                ),
                false,
            )
            .retry_after(Duration::from_millis(wait)),
        );
    }
    None
}

/// The cooldown after the last send to this console (`ago`: how long since; None: none yet).
pub(crate) fn cooldown_refusal(ago: Option<Duration>, mode: Mode, force: bool) -> Option<Refusal> {
    let ago = ago?;
    if ago >= RESEND_COOLDOWN || (mode == Mode::Explicit && force) {
        return None;
    }
    Some(
        Refusal::new(
            "recent_send",
            format!(
                "A helper was sent to this PS5 {} s ago and may still be starting. Wait a moment \
                 before sending it again.",
                ago.as_secs()
            ),
            mode == Mode::Explicit,
        )
        .retry_after(RESEND_COOLDOWN - ago),
    )
}

/// The helper's busy keys (payload helper_busy.h) in plain words.
const BUSY_WORDS: [(&str, &str); 8] = [
    ("uploads", "a transfer"),
    ("jobs", "a transfer"),
    ("install", "an install"),
    ("extract", "an archive extraction"),
    ("convert", "a conversion"),
    ("dump", "a game dump"),
    ("input", "remote controls"),
    ("pads", "virtual controllers"),
];

/// What the helper's status says is running on the console (its `busy` object, 12.6.15 and
/// later), in plain words; None when nothing, or an older helper that does not say.
pub(crate) fn busy_cause(status: &Value) -> Option<String> {
    let busy = status.get("busy")?.as_object()?;
    let on = |k: &str| match busy.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) > 0,
        _ => false,
    };
    let mut what = Vec::new();
    for (key, words) in BUSY_WORDS {
        if on(key) && !what.contains(&words) {
            what.push(words);
        }
    }
    (!what.is_empty()).then(|| what.join(", "))
}

/// A helper answered the status call right before the send: whether that stops the send.
/// `image_version`: the version of the helper being sent, when it says.
pub(crate) fn status_refusal(
    status: &Value,
    mode: Mode,
    opts: &SendOpts,
    image_version: Option<&str>,
) -> Option<Refusal> {
    if mode == Mode::Explicit && opts.force {
        return None;
    }
    if let Some(what) = busy_cause(status) {
        return Some(Refusal::new(
            "console_busy",
            format!(
                "The PS5 is busy ({what}). Sending the helper restarts it and stops that. Send it \
                 when that has finished."
            ),
            mode == Mode::Explicit,
        ));
    }
    if mode == Mode::Explicit {
        return None;
    }
    if !opts.replace {
        return Some(Refusal::new(
            "helper_answers",
            "A helper is running on the PS5 and answers, so none is sent over it by itself.",
            false,
        ));
    }
    let running = status.get("version").and_then(Value::as_str);
    match (running, image_version) {
        (None, _) => Some(Refusal::new(
            "helper_version_unknown",
            "The helper running on the PS5 does not say its version, so it is not replaced by \
             itself.",
            false,
        )),
        (Some(r), Some(img)) => match compare_versions(r, img) {
            None => Some(Refusal::new(
                "helper_version_unknown",
                format!("The PS5's helper says it is version \"{r}\", which is not a version."),
                false,
            )),
            Some(o) if o != std::cmp::Ordering::Less => Some(Refusal::new(
                "already_current",
                format!("The PS5 already runs helper v{r}."),
                false,
            )),
            Some(_) => None,
        },
        (Some(_), None) => None,
    }
}

/// An unattended send's helper is not the app's version (a stale build): never sent unasked.
pub(crate) fn version_refusal(image_version: Option<&str>, app_version: &str) -> Option<Refusal> {
    let v = image_version?;
    (v != app_version).then(|| {
        Refusal::new(
            "helper_version_mismatch",
            format!(
                "This app carries helper v{v} but is v{app_version}, so the helper is not sent by \
                 itself. Reinstall the app; Send helper still sends it."
            ),
            false,
        )
    })
}

/// A status answer reached the console's record: after an unattended send, the new helper's first
/// answer says whether the console restarted in between. True when the record changed.
pub(crate) fn note_status_ok(book: &mut Book, status: &Value) -> bool {
    if !book.awaiting {
        return false;
    }
    let instance = status.get("instance_id").and_then(Value::as_u64);
    if book.awaiting_instance.is_some() && instance == book.awaiting_instance {
        return false; // the helper the send replaces still answers
    }
    // "stale": the record it found is from before the console last started. After the console
    // went dark following our send, that is a restart the send may have caused.
    let restarted = status.get("prior_instance").and_then(Value::as_str) == Some("stale");
    if restarted && book.went_dark {
        book.restarts += 1;
    } else {
        book.restarts = 0;
    }
    book.awaiting = false;
    book.awaiting_instance = None;
    book.went_dark = false;
    true
}

/// A status call found nothing answering at all (off, restarting) while waiting on a send.
pub(crate) fn note_dark(book: &mut Book) -> bool {
    if book.awaiting && !book.went_dark {
        book.went_dark = true;
        return true;
    }
    false
}

/// A send went (or broke after the connection was up: it may run all the same).
pub(crate) fn note_sent(book: &mut Book, mode: Mode, now: u64, instance_before: Option<u64>) {
    match mode {
        Mode::Explicit => {
            book.auto_sends.clear();
            book.restarts = 0;
            book.awaiting = false;
            book.awaiting_instance = None;
            book.went_dark = false;
        }
        Mode::Auto => {
            book.auto_sends.retain(|&t| now.saturating_sub(t) < HOUR_MS);
            book.auto_sends.push(now);
            book.awaiting = true;
            book.awaiting_instance = instance_before;
            book.went_dark = false;
        }
    }
}

fn store_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None; // tests never touch the person's data folder
    }
    ps5upload_ava1::pool::data_dir().map(|d| d.join("helper_sends.json"))
}

fn books() -> MutexGuard<'static, HashMap<String, Book>> {
    static M: OnceLock<Mutex<HashMap<String, Book>>> = OnceLock::new();
    M.get_or_init(|| {
        let loaded = store_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn save(map: &HashMap<String, Book>) {
    let Some(path) = store_path() else { return };
    if let Ok(bytes) = serde_json::to_vec_pretty(map) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn with_book<T>(host: &str, f: impl FnOnce(&mut Book) -> (T, bool)) -> T {
    let mut map = books();
    let book = map
        .entry(crate::helper_stop::console_key(host))
        .or_default();
    let (out, changed) = f(book);
    if changed {
        save(&map);
    }
    out
}

/// A copy of a console's record (the status route, tests).
pub(crate) fn book_of(host: &str) -> Book {
    books()
        .get(&crate::helper_stop::console_key(host))
        .cloned()
        .unwrap_or_default()
}

/// The status route answered for `console`.
pub(crate) fn after_status_ok(console: &str, status: &Value) {
    with_book(console, |b| ((), note_status_ok(b, status)));
}

/// The status route found the console not answering at all (`power`: what DDP said).
pub(crate) fn after_status_failed(console: &str, power: Option<ps5upload_core::ddp::Power>) {
    if power == Some(ps5upload_core::ddp::Power::Unreachable) {
        with_book(console, |b| ((), note_dark(b)));
    }
}

fn last_sends() -> MutexGuard<'static, HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// How long ago a helper was last sent to this console by this engine (None: not this run).
pub(crate) fn sent_ago(host: &str) -> Option<Duration> {
    last_sends()
        .get(&crate::helper_stop::console_key(host))
        .map(|t| t.elapsed())
}

/// Test seam: a helper was sent to `host` just now.
#[cfg(test)]
pub(crate) fn tests_note_sent(host: &str) {
    last_sends().insert(crate::helper_stop::console_key(host), Instant::now());
}

fn console_host(host: &str) -> String {
    crate::install::console_id(host.trim())
}

/// The console's DDP host id, asked quickly (None in tests or when it does not answer).
fn host_id_of(host: &str) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    ps5upload_core::ddp::probe(host, Duration::from_millis(800))
        .ok()
        .map(|s| s.host_id)
        .filter(|id| !id.is_empty())
}

/// The refusals that need no lock and nothing from the console.
pub(crate) fn early_refusal(
    installs: &PkgInstallStateHandle,
    host: &str,
    image: &[u8],
    opts: &SendOpts,
) -> Option<Refusal> {
    let mode = opts.mode();
    let console = console_host(host);
    if installs.jobs.active_for(&console) {
        return Some(Refusal::new(
            "pkg_install_busy",
            "An install is running on this PS5. Send the helper once it has finished.",
            false,
        ));
    }
    if crate::helper_stop::is_busy(&console) && !(mode == Mode::Explicit && opts.force) {
        return Some(Refusal::new(
            "helper_busy",
            "A transfer or file operation is running on this PS5, and sending the helper stops it.",
            mode == Mode::Explicit,
        ));
    }
    if mode == Mode::Auto {
        let app = opts
            .app_version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(env!("CARGO_PKG_VERSION"));
        let image_version = helper_image_version(image);
        if let Some(r) = version_refusal(image_version.as_deref(), app) {
            crate::log_error!(
                "helper send: {console}: refusing an unattended send: {}",
                r.message
            );
            return Some(r);
        }
        if let Some(r) = book_refusal(&book_of(&console), now_ms()) {
            return Some(r);
        }
    }
    None
}

/// Sends `image` (a helper) to `host`'s loader through the gate. Blocking.
///
/// `stamp` writes this engine's AVA1 trust into the image (`ava1_api::stamped_helper`).
pub(crate) fn send_helper(
    installs: &PkgInstallStateHandle,
    host: &str,
    label: &str,
    image: &[u8],
    opts: &SendOpts,
    stamp: &mut dyn FnMut(&mut Vec<u8>),
) -> Result<SentPayload, GateError> {
    let mode = opts.mode();
    let console = console_host(host);
    if let Some(r) = early_refusal(installs, &console, image, opts) {
        return Err(GateError::Refused(r));
    }
    // One loader operation per console: an elfldr swap, an install's daemon send, another send.
    let lock = crate::elfldr_guard::loader_lock(&console);
    let _loader = match mode {
        Mode::Explicit => lock.lock().unwrap_or_else(|e| e.into_inner()),
        Mode::Auto => {
            let until = Instant::now() + AUTO_LOADER_WAIT;
            loop {
                match lock.try_lock() {
                    Ok(g) => break g,
                    Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        return Err(GateError::Refused(Refusal::new(
                            "loader_busy",
                            "The PS5's loader is in use (an install or another send).",
                            false,
                        )))
                    }
                }
            }
        }
    };
    // Under the lock, so two callers cannot both read "none sent lately" and both send.
    if let Some(r) = cooldown_refusal(sent_ago(&console), mode, opts.force) {
        return Err(GateError::Refused(r));
    }
    // What answers now, right before the send (another device's send may have landed meanwhile).
    let image_version = helper_image_version(image);
    let mut instance_before = None;
    if mode == Mode::Auto || !opts.force {
        let status = ps5upload_core::mgmt::call_with(
            &console,
            ps5upload_core::mgmt::m::NODE_STATUS,
            ps5upload_core::mgmt::m::NODE_STATUS.label,
            b"",
            Some(STATUS_TIMEOUT),
        )
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        if let Some(status) = status {
            instance_before = status.get("instance_id").and_then(Value::as_u64);
            if let Some(r) = status_refusal(&status, mode, opts, image_version.as_deref()) {
                return Err(GateError::Refused(r));
            }
        }
    }
    let host_id = host_id_of(&console);
    if mode == Mode::Auto {
        let known = book_of(&console).host_id;
        if let (Some(was), Some(now)) = (known.as_deref(), host_id.as_deref()) {
            if !was.eq_ignore_ascii_case(now) {
                return Err(GateError::Refused(Refusal::new(
                    "different_console",
                    "Another PS5 answers at this address now (its network address changed), so the \
                     helper is not sent to it by itself. Check the address, then press Send helper.",
                    false,
                )));
            }
        }
    }
    let evict = if mode == Mode::Explicit && opts.force {
        Evict::Always
    } else {
        Evict::IfIdle
    };
    let sent = pl::send_payload_image_with(&console, PS5_LOADER_PORT, label, image, stamp, evict);
    let went = matches!(&sent, Ok(_) | Err(SendFailure::Broke(_)));
    if went {
        last_sends().insert(crate::helper_stop::console_key(&console), Instant::now());
        with_book(&console, |b| {
            note_sent(b, mode, now_ms(), instance_before);
            if host_id.is_some() && (mode == Mode::Explicit || b.host_id.is_none()) {
                b.host_id = host_id.clone();
            }
            ((), true)
        });
        crate::log_info!(
            "helper send: {console} ({}{}): {}",
            if mode == Mode::Auto {
                "unattended"
            } else {
                "asked"
            },
            if opts.force { ", sent anyway" } else { "" },
            match &sent {
                Ok(s) => s.status.clone(),
                Err(e) => format!("broke after connecting: {e}"),
            }
        );
    }
    match sent {
        Ok(s) => Ok(s),
        Err(SendFailure::Refused(why)) => {
            let what = why
                .split_once("\"busy\":\"")
                .and_then(|(_, rest)| rest.split('"').next())
                .and_then(|k| BUSY_WORDS.iter().find(|(key, _)| *key == k))
                .map_or("work running", |(_, words)| words);
            Err(GateError::Refused(Refusal::new(
                "console_busy",
                format!(
                    "The PS5 is busy ({what}). Sending the helper restarts it and stops that. Send \
                     it when that has finished."
                ),
                mode == Mode::Explicit,
            )))
        }
        Err(e) => Err(GateError::Failed(e.message().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto() -> SendOpts {
        SendOpts {
            mode: Some("auto".into()),
            ..Default::default()
        }
    }

    #[test]
    fn older_callers_get_an_explicit_send() {
        assert_eq!(SendOpts::default().mode(), Mode::Explicit);
        assert_eq!(auto().mode(), Mode::Auto);
        let o: SendOpts = serde_json::from_str(r#"{"mode":"weird"}"#).unwrap();
        assert_eq!(o.mode(), Mode::Explicit);
    }

    #[test]
    fn the_cooldown_holds_auto_always_and_a_person_until_they_insist() {
        let just = Some(Duration::from_secs(10));
        let r = cooldown_refusal(just, Mode::Auto, true).unwrap();
        assert_eq!(r.code, "recent_send");
        assert!(!r.forceable);
        assert_eq!(r.retry_after_ms, Some(35_000));
        let r = cooldown_refusal(just, Mode::Explicit, false).unwrap();
        assert!(r.forceable);
        assert!(cooldown_refusal(just, Mode::Explicit, true).is_none());
        assert!(cooldown_refusal(Some(RESEND_COOLDOWN), Mode::Auto, false).is_none());
        assert!(cooldown_refusal(None, Mode::Auto, false).is_none());
    }

    #[test]
    fn four_unattended_sends_an_hour_then_none_until_a_person_sends() {
        let mut b = Book::default();
        let t0 = 10 * HOUR_MS;
        for i in 0..AUTO_SENDS_PER_HOUR as u64 {
            assert!(book_refusal(&b, t0 + i).is_none());
            note_sent(&mut b, Mode::Auto, t0 + i * 60_000, None);
        }
        let r = book_refusal(&b, t0 + 5 * 60_000).unwrap();
        assert_eq!(r.code, "send_budget_spent");
        assert_eq!(r.retry_after_ms, Some(HOUR_MS - 5 * 60_000));
        // An hour after the first, one slot is free again.
        assert!(book_refusal(&b, t0 + HOUR_MS).is_none());
        // A person's send starts the budget over.
        note_sent(&mut b, Mode::Explicit, t0 + 6 * 60_000, None);
        assert!(book_refusal(&b, t0 + 6 * 60_000).is_none());
        assert!(b.auto_sends.is_empty());
    }

    #[test]
    fn the_record_survives_a_restart_as_json() {
        let mut b = Book::default();
        note_sent(&mut b, Mode::Auto, 5, Some(7));
        b.host_id = Some("D4F7D5D36123".into());
        let back: Book = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
        // An older file without the newer fields still reads.
        let old: Book = serde_json::from_str(r#"{"auto_sends":[1,2]}"#).unwrap();
        assert_eq!(old.auto_sends, vec![1, 2]);
        assert!(!old.awaiting);
    }

    #[test]
    fn two_restarts_right_after_unattended_sends_stop_them() {
        let mut b = Book::default();
        let stale = json!({"instance_id": 2, "prior_instance": "stale"});
        for round in 1..=2u32 {
            note_sent(&mut b, Mode::Auto, round as u64, Some(1));
            // The old helper still answering changes nothing.
            assert!(!note_status_ok(&mut b, &json!({"instance_id": 1})));
            assert!(note_dark(&mut b));
            assert!(note_status_ok(&mut b, &stale));
            assert_eq!(b.restarts, round);
        }
        let r = book_refusal(&b, 10).unwrap();
        assert_eq!(r.code, "console_restarted_after_sends");
        // A person's send clears it.
        note_sent(&mut b, Mode::Explicit, 11, None);
        assert!(book_refusal(&b, 11).is_none());
    }

    #[test]
    fn a_console_that_booted_before_the_send_is_not_a_restart() {
        // The usual case: the console was switched on, the helper sent at boot finds the record of
        // the helper before the restart ("stale"), but the console never went dark after the send.
        let mut b = Book::default();
        for _ in 0..3 {
            note_sent(&mut b, Mode::Auto, 1, None);
            assert!(note_status_ok(
                &mut b,
                &json!({"instance_id": 9, "prior_instance": "stale"})
            ));
        }
        assert_eq!(b.restarts, 0);
        // Dark, then a clean start: no restart either, and the count starts over.
        b.restarts = 1;
        note_sent(&mut b, Mode::Auto, 2, None);
        note_dark(&mut b);
        note_status_ok(
            &mut b,
            &json!({"instance_id": 3, "prior_instance": "clean"}),
        );
        assert_eq!(b.restarts, 0);
        // Not waiting on a send: nothing changes.
        assert!(!note_dark(&mut b));
        assert!(!note_status_ok(&mut b, &json!({"prior_instance": "stale"})));
    }

    #[test]
    fn busy_work_reads_in_plain_words() {
        let s = json!({"busy": {"uploads": 2, "install": true, "convert": 0, "input": 1}});
        assert_eq!(
            busy_cause(&s).as_deref(),
            Some("a transfer, an install, remote controls")
        );
        assert_eq!(
            busy_cause(&json!({"busy": {"uploads": 0, "install": false}})),
            None
        );
        // An older helper has no busy object.
        assert_eq!(busy_cause(&json!({"version": "12.6.14"})), None);
    }

    #[test]
    fn an_answering_helper_is_replaced_unattended_only_when_older_and_idle() {
        let idle_old = json!({"version": "12.6.14", "busy": {}});
        let o = auto();
        let r = status_refusal(&idle_old, Mode::Auto, &o, Some("12.6.15")).unwrap();
        assert_eq!(r.code, "helper_answers", "not asked to replace it");
        let replace = SendOpts {
            replace: true,
            ..auto()
        };
        assert!(status_refusal(&idle_old, Mode::Auto, &replace, Some("12.6.15")).is_none());
        for v in ["12.6.15", "12.7.0"] {
            let r = status_refusal(
                &json!({ "version": v }),
                Mode::Auto,
                &replace,
                Some("12.6.15"),
            )
            .unwrap();
            assert_eq!(r.code, "already_current", "{v}");
        }
        let r = status_refusal(
            &json!({"version": "x"}),
            Mode::Auto,
            &replace,
            Some("12.6.15"),
        )
        .unwrap();
        assert_eq!(r.code, "helper_version_unknown");
        let r = status_refusal(&json!({}), Mode::Auto, &replace, Some("12.6.15")).unwrap();
        assert_eq!(r.code, "helper_version_unknown");
        let busy = json!({"version": "12.6.14", "busy": {"extract": 1}});
        let r = status_refusal(&busy, Mode::Auto, &replace, Some("12.6.15")).unwrap();
        assert_eq!(r.code, "console_busy");
        assert!(!r.forceable);
    }

    #[test]
    fn a_person_is_asked_about_a_busy_console_and_may_insist() {
        let busy = json!({"version": "12.6.15", "busy": {"dump": true}});
        let r = status_refusal(&busy, Mode::Explicit, &SendOpts::default(), None).unwrap();
        assert_eq!(r.code, "console_busy");
        assert!(r.forceable);
        assert!(r.message.contains("a game dump"), "{}", r.message);
        let force = SendOpts {
            force: true,
            ..Default::default()
        };
        assert!(status_refusal(&busy, Mode::Explicit, &force, None).is_none());
        // A person may replace an answering, idle helper of any version.
        assert!(status_refusal(
            &json!({"version": "12.7.0"}),
            Mode::Explicit,
            &SendOpts::default(),
            Some("12.6.15")
        )
        .is_none());
    }

    #[test]
    fn a_helper_of_another_version_is_never_sent_unattended() {
        let r = version_refusal(Some("12.6.14"), "12.6.15").unwrap();
        assert_eq!(r.code, "helper_version_mismatch");
        assert!(version_refusal(Some("12.6.15"), "12.6.15").is_none());
        // A helper without the marker (an older build, a test stand-in) is not judged.
        assert!(version_refusal(None, "12.6.15").is_none());
    }

    #[test]
    fn a_refusal_carries_its_code_and_whether_a_person_may_override_it() {
        let r = cooldown_refusal(Some(Duration::from_secs(1)), Mode::Explicit, false).unwrap();
        let v = r.fields();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "recent_send");
        assert_eq!(v["refused"], "recent_send");
        assert_eq!(v["forceable"], true);
        assert!(v["retry_after_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn an_install_or_a_running_job_stops_the_send_early() {
        let installs: PkgInstallStateHandle = Default::default();
        let host = "192.0.2.91";
        let img = b"\x7FELF stand-in";
        assert!(early_refusal(&installs, host, img, &SendOpts::default()).is_none());
        let busy = crate::helper_stop::enter(host);
        let r = early_refusal(&installs, host, img, &SendOpts::default()).unwrap();
        assert_eq!(r.code, "helper_busy");
        assert!(r.forceable);
        let r = early_refusal(&installs, host, img, &auto()).unwrap();
        assert!(!r.forceable);
        let force = SendOpts {
            force: true,
            ..Default::default()
        };
        assert!(early_refusal(&installs, host, img, &force).is_none());
        drop(busy);
        // A stale build is not sent unattended, with any app version the caller names.
        let mut stale = b"\x7FELF..PS5UPLOAD_HELPER_VERSION=1.0.0;".to_vec();
        stale.extend_from_slice(&[0; 16]);
        let r = early_refusal(&installs, host, &stale, &auto()).unwrap();
        assert_eq!(r.code, "helper_version_mismatch");
        let named = SendOpts {
            app_version: Some("1.0.0".into()),
            ..auto()
        };
        assert!(early_refusal(&installs, host, &stale, &named).is_none());
    }

    /// Two unattended sends at once: the loader lock serialises them and the second, reading the
    /// first one's send under the lock, is refused (a loader on this machine stands in).
    #[test]
    fn two_sends_at_once_send_one_helper() {
        use std::io::Read;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let host = format!("127.0.0.1:{port}");
        // The gate sends to :9021; this test drives the pieces it is made of instead.
        let console = console_host(&host);
        let lock = crate::elfldr_guard::loader_lock(&console);
        let accepted = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let lock = lock.clone();
            let console = console.clone();
            let sends = sends.clone();
            handles.push(std::thread::spawn(move || {
                let _g = lock.lock().unwrap();
                if cooldown_refusal(sent_ago(&console), Mode::Auto, false).is_some() {
                    return;
                }
                let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                std::io::Write::write_all(&mut s, b"\x7FELF").unwrap();
                sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                last_sends().insert(crate::helper_stop::console_key(&console), Instant::now());
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(accepted.join().unwrap(), b"\x7FELF");
    }
}
