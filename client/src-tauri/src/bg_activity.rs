//! Background watcher (#457): notices from the host, not the WebView, that work is running or
//! has just ended.
//!
//! A hidden window throttles the renderer's timers (WebView2, WebKitGTK, a hidden WKWebView can
//! be suspended) and Android freezes a backgrounded app's process outright, so the renderer is
//! the wrong place to decide "a job finished, tell the person and start the next one". This
//! task polls the engine's `GET /api/activity` every second instead and:
//!
//! - on Android, runs a foreground service (`android_bg`) while anything is active, which keeps
//!   the process and the in-process engine alive, with an ongoing progress notification;
//! - posts a complete / failed notification when the app is in the background;
//! - emits `bg-job-finished` (and a `bg-tick` heartbeat while work runs) so the renderer's queue
//!   advances without waiting on a throttled timer.
//!
//! "Active" is an engine job running, or a renderer hold (queued items waiting their turn)
//! refreshed within the last 5 minutes; the service stops 30 s after nothing is active.
//! The decisions live in [`step`], which is pure and unit-tested.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

const POLL: Duration = Duration::from_secs(1);
/// A renderer hold counts this long after its last refresh (the renderer refreshes every 60 s):
/// a frozen renderer cannot keep the service up forever.
const HOLD_TTL_MS: u64 = 5 * 60 * 1000;
/// Nothing active for this long stops the service. Covers the gap between queue items.
const IDLE_GRACE_MS: u64 = 30 * 1000;
/// While work is active, Android hears from the watcher at least this often, even with the same
/// text: its wake lock has a timeout and is renewed by these, and a stall or a long wait has no
/// new text to send.
const KEEPALIVE_MS: u64 = 60 * 1000;
/// Weight of the newest sample in the speed average.
const EMA_ALPHA: f64 = 0.3;

// ── Engine snapshot ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ActiveJob {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub bytes_done: u64,
    #[serde(default)]
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct FinishedJob {
    pub seq: u64,
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Snapshot {
    pub seq: u64,
    #[serde(default)]
    pub active: Vec<ActiveJob>,
    #[serde(default)]
    pub finished: Vec<FinishedJob>,
}

// ── Strings (the renderer sends its language's; English until it does) ──────

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct BgStrings {
    pub uploading: String,
    pub downloading: String,
    pub installing: String,
    pub working: String,
    pub jobs_running: String,
    pub waiting: String,
    pub progress_of: String,
    pub done_upload: String,
    pub done_download: String,
    pub done_install: String,
    pub done_job: String,
    pub failed_upload: String,
    pub failed_download: String,
    pub failed_install: String,
    pub failed_job: String,
    pub channel_progress: String,
    pub channel_results: String,
}

impl Default for BgStrings {
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        Self {
            uploading: s("Uploading {name}"),
            downloading: s("Downloading {name}"),
            installing: s("Installing {name}"),
            working: s("Working on {name}"),
            jobs_running: s("{n} jobs running"),
            waiting: s("Waiting for the next item"),
            progress_of: s("{done} of {total}"),
            done_upload: s("Upload complete"),
            done_download: s("Download complete"),
            done_install: s("Installed"),
            done_job: s("Finished"),
            failed_upload: s("Upload failed"),
            failed_download: s("Download failed"),
            failed_install: s("Install failed"),
            failed_job: s("Failed"),
            channel_progress: s("Transfers in progress"),
            channel_results: s("Finished transfers"),
        }
    }
}

// ── The pure core ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub title: String,
    pub text: String,
    /// `None` when the total is unknown (indeterminate bar).
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    StartFg(Progress),
    Update(Progress),
    Finished(FinishedJob),
    StopFg,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    bytes: u64,
    at_ms: u64,
    bps: f64,
}

#[derive(Debug, Default)]
pub struct State {
    /// The engine seq already seen; `None` until the first poll.
    pub seq: Option<u64>,
    /// The service (and progress notification) is up.
    pub fg: bool,
    idle_since_ms: Option<u64>,
    last: Option<Progress>,
    /// When the service was last told anything.
    last_sent_ms: u64,
    rates: HashMap<String, Rate>,
    pub strings: BgStrings,
}

/// One poll's decisions. The first poll only records `seq`, so a launch never re-announces
/// endings from before it; a `seq` lower than the one seen means the engine restarted, and
/// counting starts again from its value.
pub fn step(state: &mut State, snap: &Snapshot, hold: bool, now_ms: u64) -> Vec<Action> {
    let mut out = Vec::new();
    match state.seq {
        Some(seen) if snap.seq >= seen => {
            for f in snap.finished.iter().filter(|f| f.seq > seen) {
                out.push(Action::Finished(f.clone()));
            }
        }
        _ => {}
    }
    state.seq = Some(snap.seq);

    update_rates(&mut state.rates, &snap.active, now_ms);
    let active = !snap.active.is_empty() || hold;
    if active {
        state.idle_since_ms = None;
        let p = format_progress(&snap.active, &state.rates, &state.strings);
        if !state.fg {
            state.fg = true;
            state.last_sent_ms = now_ms;
            out.push(Action::StartFg(p.clone()));
            state.last = Some(p);
        } else if state.last.as_ref() != Some(&p)
            || now_ms.saturating_sub(state.last_sent_ms) >= KEEPALIVE_MS
        {
            state.last_sent_ms = now_ms;
            out.push(Action::Update(p.clone()));
            state.last = Some(p);
        }
    } else if state.fg {
        match state.idle_since_ms {
            None => {
                state.idle_since_ms = Some(now_ms);
                // The last job's 99% would otherwise sit there through the grace period.
                let p = format_progress(&[], &state.rates, &state.strings);
                if state.last.as_ref() != Some(&p) {
                    out.push(Action::Update(p.clone()));
                    state.last = Some(p);
                }
            }
            Some(t) if now_ms.saturating_sub(t) >= IDLE_GRACE_MS => {
                state.fg = false;
                state.idle_since_ms = None;
                state.last = None;
                out.push(Action::StopFg);
            }
            Some(_) => {}
        }
    }
    out
}

/// Per-job speed, an exponential moving average of bytes over time between polls. Jobs that
/// are no longer running are forgotten.
fn update_rates(rates: &mut HashMap<String, Rate>, active: &[ActiveJob], now_ms: u64) {
    rates.retain(|id, _| active.iter().any(|j| &j.id == id));
    for j in active {
        match rates.get_mut(&j.id) {
            None => {
                rates.insert(
                    j.id.clone(),
                    Rate {
                        bytes: j.bytes_done,
                        at_ms: now_ms,
                        bps: 0.0,
                    },
                );
            }
            Some(r) => {
                let dt = now_ms.saturating_sub(r.at_ms);
                if dt == 0 {
                    continue;
                }
                let sample = j.bytes_done.saturating_sub(r.bytes) as f64 * 1000.0 / dt as f64;
                r.bps = if r.bps == 0.0 {
                    sample
                } else {
                    EMA_ALPHA * sample + (1.0 - EMA_ALPHA) * r.bps
                };
                r.bytes = j.bytes_done;
                r.at_ms = now_ms;
            }
        }
    }
}

/// `1.2 GB`, `38 MB`: SI units like the engine's MB/s, one decimal below 10.
pub fn fmt_bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n.max(0.0);
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// The progress notification: "Uploading <label>" / "42% · 1.2 GB of 2.9 GB · 38 MB/s" for
/// one job, "<n> jobs running" with the summed progress for several, "Waiting for the next
/// item" while only a renderer hold is active.
pub fn format_progress(
    active: &[ActiveJob],
    rates: &HashMap<String, Rate>,
    s: &BgStrings,
) -> Progress {
    let title = match active {
        [] => s.waiting.clone(),
        [one] => {
            let tmpl = match one.kind.as_str() {
                "upload" => &s.uploading,
                "download" => &s.downloading,
                "install" => &s.installing,
                _ => &s.working,
            };
            tmpl.replace("{name}", one.label.trim()).trim().to_string()
        }
        many => s.jobs_running.replace("{n}", &many.len().to_string()),
    };
    if active.is_empty() {
        return Progress {
            title,
            text: String::new(),
            percent: None,
        };
    }
    let done: u64 = active.iter().map(|j| j.bytes_done).sum();
    // A job with no known total makes the sum meaningless as a fraction.
    let total: Option<u64> = active
        .iter()
        .map(|j| (j.bytes_total > 0).then_some(j.bytes_total))
        .sum();
    let bps: f64 = active
        .iter()
        .filter_map(|j| rates.get(&j.id))
        .map(|r| r.bps)
        .sum();
    let percent = total.map(|t| ((done.min(t) as f64 / t as f64) * 100.0).floor() as u8);
    let mut parts = Vec::new();
    if let Some(p) = percent {
        parts.push(format!("{p}%"));
    }
    match total {
        Some(t) => parts.push(
            s.progress_of
                .replace("{done}", &fmt_bytes(done as f64))
                .replace("{total}", &fmt_bytes(t as f64)),
        ),
        None if done > 0 => parts.push(fmt_bytes(done as f64)),
        None => {}
    }
    if bps >= 1.0 {
        parts.push(format!("{}/s", fmt_bytes(bps)));
    }
    Progress {
        title,
        text: parts.join(" · "),
        percent,
    }
}

/// Whether a job's ending is worth a notification: not one the person stopped, and not an
/// unnamed job (the client names everything it started; the rest is the app's own plumbing).
pub fn should_notify(f: &FinishedJob) -> bool {
    !f.cancelled && !f.label.trim().is_empty()
}

/// Title and body of a complete / failed notification.
pub fn finished_text(f: &FinishedJob, s: &BgStrings) -> (String, String) {
    let title = match (f.ok, f.kind.as_str()) {
        (true, "upload") => &s.done_upload,
        (true, "download") => &s.done_download,
        (true, "install") => &s.done_install,
        (true, _) => &s.done_job,
        (false, "upload") => &s.failed_upload,
        (false, "download") => &s.failed_download,
        (false, "install") => &s.failed_install,
        (false, _) => &s.failed_job,
    };
    let mut body = f.label.trim().to_string();
    if let Some(e) = f.error.as_deref().filter(|e| !f.ok && !e.is_empty()) {
        body = format!("{body}\n{e}");
    }
    (title.clone(), body)
}

// ── Renderer inputs ──────────────────────────────────────────────────────────

/// When the renderer last said work is queued or running (ms), `None` = released.
static HOLD: Mutex<Option<u64>> = Mutex::new(None);
/// The page is showing (renderer `visibilitychange`). Android's only foreground signal.
static VISIBLE: AtomicBool = AtomicBool::new(true);
/// The person's "Mirror notifications to the system" setting.
static NOTIFY: AtomicBool = AtomicBool::new(true);
static STRINGS: OnceLock<RwLock<BgStrings>> = OnceLock::new();

fn strings() -> BgStrings {
    STRINGS
        .get_or_init(|| RwLock::new(BgStrings::default()))
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hold_active(now: u64) -> bool {
    HOLD.lock()
        .map(|g| g.is_some_and(|t| now.saturating_sub(t) < HOLD_TTL_MS))
        .unwrap_or(false)
}

/// The renderer has work queued or running (`true`, refreshed while it lasts) or none.
#[tauri::command]
pub fn bg_work_hold(active: bool) {
    if let Ok(mut g) = HOLD.lock() {
        *g = active.then(now_ms);
    }
}

#[derive(Debug, Deserialize)]
pub struct BgConfig {
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub notify: Option<bool>,
    #[serde(default)]
    pub strings: Option<BgStrings>,
}

/// The page's visibility, the notification setting and the notification texts in the
/// person's language. Each field is optional; absent ones are left as they were.
#[tauri::command]
pub fn bg_configure(config: BgConfig) {
    if let Some(v) = config.visible {
        VISIBLE.store(v, Ordering::Relaxed);
    }
    if let Some(n) = config.notify {
        NOTIFY.store(n, Ordering::Relaxed);
    }
    if let Some(s) = config.strings {
        if let Ok(mut g) = STRINGS
            .get_or_init(|| RwLock::new(BgStrings::default()))
            .write()
        {
            *g = s;
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BgCapabilities {
    /// The host posts job complete / failed notifications itself; the renderer must not
    /// mirror the same ones.
    pub native_job_notify: bool,
}

#[tauri::command]
pub fn bg_capabilities() -> BgCapabilities {
    BgCapabilities {
        native_job_notify: WATCHING.load(Ordering::Relaxed),
    }
}

// ── The task ─────────────────────────────────────────────────────────────────

static STARTED: OnceLock<()> = OnceLock::new();
/// The engine has answered `/api/activity`: endings will be seen and announced here. An
/// engine without the route (a remote one on an older version) leaves the page announcing them.
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Start the watcher once per process. Android can run setup again when its Activity is
/// recreated; the second call is a no-op.
pub fn spawn<R: Runtime>(app: &AppHandle<R>) {
    if STARTED.set(()).is_err() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let client = match crate::engine_http::engine_client_builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[bg] no HTTP client: {e}");
                return;
            }
        };
        let mut state = State::default();
        let mut ticker = tokio::time::interval(POLL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let url = format!(
                "{}/api/activity?since={}",
                crate::engine::url(),
                state.seq.unwrap_or(0)
            );
            let snap = match client.get(&url).send().await {
                Ok(r) if r.status().is_success() => r.json::<Snapshot>().await.ok(),
                // An engine without the route (a remote one on an older version) or one that
                // is down: nothing to report this round.
                _ => None,
            };
            let Some(snap) = snap else { continue };
            WATCHING.store(true, Ordering::Relaxed);
            state.strings = strings();
            let now = now_ms();
            let actions = step(&mut state, &snap, hold_active(now), now);
            for a in &actions {
                dispatch(&app, a, &state.strings);
            }
            if state.fg {
                let _ = app.emit("bg-tick", ());
            }
        }
    });
}

fn dispatch<R: Runtime>(app: &AppHandle<R>, action: &Action, s: &BgStrings) {
    match action {
        Action::Finished(f) => {
            let _ = app.emit("bg-job-finished", f);
            if should_notify(f) && NOTIFY.load(Ordering::Relaxed) && !app_in_foreground(app) {
                let (title, body) = finished_text(f, s);
                notify_finished(app, f, &title, &body, s);
            }
        }
        #[cfg(target_os = "android")]
        Action::StartFg(p) => crate::android_bg::start(
            &p.title,
            &p.text,
            p.percent.map_or(-1, i32::from),
            &s.channel_progress,
        ),
        #[cfg(target_os = "android")]
        Action::Update(p) => {
            crate::android_bg::update(&p.title, &p.text, p.percent.map_or(-1, i32::from))
        }
        #[cfg(target_os = "android")]
        Action::StopFg => crate::android_bg::stop(),
        // Desktop has no service to run: the engine is its own process and keep_awake.rs holds
        // the machine awake. Progress stays in the window.
        #[cfg(not(target_os = "android"))]
        Action::StartFg(_) | Action::Update(_) | Action::StopFg => {}
    }
}

/// Whether the person is looking at the app, so an OS notification would only repeat the
/// in-app one. Desktop asks the window; a phone has one app on screen, and the page reports it.
#[cfg(desktop)]
fn app_in_foreground<R: Runtime>(app: &AppHandle<R>) -> bool {
    use tauri::Manager;
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
    })
}

#[cfg(mobile)]
fn app_in_foreground<R: Runtime>(_app: &AppHandle<R>) -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

#[cfg(target_os = "android")]
fn notify_finished<R: Runtime>(
    _app: &AppHandle<R>,
    f: &FinishedJob,
    title: &str,
    body: &str,
    s: &BgStrings,
) {
    crate::android_bg::finished(notif_id(&f.id), title, body, f.ok, &s.channel_results);
}

#[cfg(not(target_os = "android"))]
fn notify_finished<R: Runtime>(
    app: &AppHandle<R>,
    _f: &FinishedJob,
    title: &str,
    body: &str,
    _s: &BgStrings,
) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("[bg] notification: {e}");
    }
}

/// A stable notification id per job, so a repeat replaces rather than stacks.
#[cfg(target_os = "android")]
fn notif_id(job: &str) -> i32 {
    let h = job.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    // Clear of the service's own id (1).
    (h & 0x3fff_ffff) as i32 + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, kind: &str, label: &str, done: u64, total: u64) -> ActiveJob {
        ActiveJob {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            bytes_done: done,
            bytes_total: total,
        }
    }

    #[test]
    fn a_stalled_job_still_renews_the_service_once_a_minute() {
        let mut st = State::default();
        let stuck = || snap(1, vec![job("a", "upload", "Game", 10, 100)], vec![]);
        // The first active poll starts the service.
        let start = step(&mut st, &stuck(), false, 0);
        assert!(matches!(start.first(), Some(Action::StartFg(_))));
        // A rate settling may change the text once; after that nothing changes.
        step(&mut st, &stuck(), false, 2_000);
        let quiet = step(&mut st, &stuck(), false, 30_000);
        assert!(quiet.is_empty(), "no new text, under a minute: {quiet:?}");
        let renewed = step(&mut st, &stuck(), false, 63_000);
        assert!(
            matches!(renewed.as_slice(), [Action::Update(_)]),
            "a minute of the same text must still reach the service: {renewed:?}"
        );
        assert!(step(&mut st, &stuck(), false, 64_000).is_empty());
    }

    fn fin(seq: u64, id: &str) -> FinishedJob {
        FinishedJob {
            seq,
            id: id.into(),
            kind: "upload".into(),
            label: "Game".into(),
            ok: true,
            ..FinishedJob::default()
        }
    }

    fn snap(seq: u64, active: Vec<ActiveJob>, finished: Vec<FinishedJob>) -> Snapshot {
        Snapshot {
            seq,
            active,
            finished,
        }
    }

    fn finished_ids(a: &[Action]) -> Vec<String> {
        a.iter()
            .filter_map(|a| match a {
                Action::Finished(f) => Some(f.id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_first_poll_only_records_seq() {
        let mut st = State::default();
        let a = step(&mut st, &snap(5, vec![], vec![fin(5, "old")]), false, 0);
        assert!(a.is_empty(), "{a:?}");
        assert_eq!(st.seq, Some(5));
    }

    #[test]
    fn later_endings_are_reported_once() {
        let mut st = State::default();
        step(&mut st, &snap(5, vec![], vec![]), false, 0);
        let a = step(
            &mut st,
            &snap(7, vec![], vec![fin(6, "a"), fin(7, "b")]),
            false,
            1000,
        );
        assert_eq!(finished_ids(&a), vec!["a", "b"]);
        // The same entries again (a poll raced the since) are not news.
        let a = step(
            &mut st,
            &snap(7, vec![], vec![fin(6, "a"), fin(7, "b")]),
            false,
            2000,
        );
        assert!(finished_ids(&a).is_empty());
    }

    #[test]
    fn an_engine_restart_resets_the_seq() {
        let mut st = State::default();
        step(&mut st, &snap(50, vec![], vec![]), false, 0);
        let a = step(
            &mut st,
            &snap(2, vec![], vec![fin(1, "x"), fin(2, "y")]),
            false,
            1000,
        );
        assert!(finished_ids(&a).is_empty());
        assert_eq!(st.seq, Some(2));
        let a = step(&mut st, &snap(3, vec![], vec![fin(3, "z")]), false, 2000);
        assert_eq!(finished_ids(&a), vec!["z"]);
    }

    #[test]
    fn a_running_job_starts_then_updates_the_service() {
        let mut st = State::default();
        let a = step(
            &mut st,
            &snap(0, vec![job("j", "upload", "Game", 0, 100)], vec![]),
            false,
            0,
        );
        assert!(matches!(a.as_slice(), [Action::StartFg(p)] if p.title == "Uploading Game"));
        // Unchanged progress: nothing to send.
        let a = step(
            &mut st,
            &snap(0, vec![job("j", "upload", "Game", 0, 100)], vec![]),
            false,
            1000,
        );
        assert!(a.is_empty(), "{a:?}");
        let a = step(
            &mut st,
            &snap(0, vec![job("j", "upload", "Game", 50, 100)], vec![]),
            false,
            2000,
        );
        assert!(matches!(a.as_slice(), [Action::Update(p)] if p.percent == Some(50)));
    }

    #[test]
    fn a_renderer_hold_alone_keeps_the_service_up() {
        let mut st = State::default();
        let a = step(&mut st, &snap(0, vec![], vec![]), true, 0);
        assert!(
            matches!(a.as_slice(), [Action::StartFg(p)] if p.title == "Waiting for the next item")
        );
        // Nothing new to say under a minute; at a minute it renews the service's wake lock.
        let a = step(&mut st, &snap(0, vec![], vec![]), true, 59_999);
        assert!(a.is_empty());
        let a = step(&mut st, &snap(0, vec![], vec![]), true, 60_000);
        assert!(matches!(a.as_slice(), [Action::Update(_)]), "{a:?}");
        assert!(st.fg);
    }

    #[test]
    fn the_service_stops_after_the_grace_period_not_before() {
        let mut st = State::default();
        step(
            &mut st,
            &snap(0, vec![job("j", "upload", "Game", 1, 2)], vec![]),
            false,
            0,
        );
        let a = step(&mut st, &snap(1, vec![], vec![fin(1, "j")]), false, 1_000);
        assert_eq!(finished_ids(&a), vec!["j"]);
        assert!(a
            .iter()
            .any(|a| matches!(a, Action::Update(p) if p.percent.is_none())));
        assert!(step(&mut st, &snap(1, vec![], vec![]), false, 30_999).is_empty());
        let a = step(&mut st, &snap(1, vec![], vec![]), false, 31_000);
        assert_eq!(a, vec![Action::StopFg]);
        assert!(!st.fg);
        // Idle and stopped: nothing more.
        assert!(step(&mut st, &snap(1, vec![], vec![]), false, 90_000).is_empty());
    }

    #[test]
    fn work_returning_in_the_grace_period_keeps_the_service() {
        let mut st = State::default();
        step(
            &mut st,
            &snap(0, vec![job("a", "upload", "A", 1, 2)], vec![]),
            false,
            0,
        );
        step(&mut st, &snap(0, vec![], vec![]), false, 1_000);
        let a = step(
            &mut st,
            &snap(0, vec![job("b", "install", "B", 0, 0)], vec![]),
            false,
            20_000,
        );
        assert!(matches!(a.as_slice(), [Action::Update(p)] if p.title == "Installing B"));
        // The idle clock restarted: 30 s after the first idle poll is not a stop.
        assert!(!step(&mut st, &snap(0, vec![], vec![]), false, 32_000).contains(&Action::StopFg));
    }

    #[test]
    fn progress_text_for_one_job_has_percent_bytes_and_speed() {
        let mut rates = HashMap::new();
        let j0 = job("j", "upload", "Astro Bot", 1_180_000_000, 2_900_000_000);
        update_rates(&mut rates, &[j0], 0);
        let j1 = job("j", "upload", "Astro Bot", 1_218_000_000, 2_900_000_000);
        update_rates(&mut rates, std::slice::from_ref(&j1), 1000);
        let p = format_progress(&[j1], &rates, &BgStrings::default());
        assert_eq!(p.title, "Uploading Astro Bot");
        assert_eq!(p.percent, Some(42));
        assert_eq!(p.text, "42% · 1.2 GB of 2.9 GB · 38 MB/s");
    }

    #[test]
    fn speed_is_a_moving_average() {
        let mut rates = HashMap::new();
        update_rates(&mut rates, &[job("j", "upload", "", 0, 0)], 0);
        update_rates(&mut rates, &[job("j", "upload", "", 10_000_000, 0)], 1000);
        update_rates(&mut rates, &[job("j", "upload", "", 10_000_000, 0)], 2000);
        // 10 MB/s, then a stalled second: 0.3 * 0 + 0.7 * 10 MB/s.
        assert!((rates["j"].bps - 7_000_000.0).abs() < 1.0);
        update_rates(&mut rates, &[], 3000);
        assert!(rates.is_empty(), "finished jobs are forgotten");
    }

    #[test]
    fn several_jobs_sum_and_an_unknown_total_is_indeterminate() {
        let rates = HashMap::new();
        let s = BgStrings::default();
        let p = format_progress(
            &[
                job("a", "upload", "A", 1_000_000, 4_000_000),
                job("b", "download", "B", 1_000_000, 4_000_000),
            ],
            &rates,
            &s,
        );
        assert_eq!(p.title, "2 jobs running");
        assert_eq!(p.percent, Some(25));
        assert_eq!(p.text, "25% · 2.0 MB of 8.0 MB");
        let p = format_progress(&[job("a", "job", "", 5_000, 0)], &rates, &s);
        assert_eq!(p.title, "Working on");
        assert_eq!(p.percent, None);
        assert_eq!(p.text, "5.0 kB");
        let p = format_progress(&[job("a", "install", "X", 0, 0)], &rates, &s);
        assert_eq!(p.text, "");
    }

    #[test]
    fn the_strings_follow_the_language_sent() {
        let s = BgStrings {
            uploading: "{name} wird hochgeladen".into(),
            progress_of: "{done} von {total}".into(),
            ..BgStrings::default()
        };
        let p = format_progress(&[job("a", "upload", "Spiel", 1, 2)], &HashMap::new(), &s);
        assert_eq!(p.title, "Spiel wird hochgeladen");
        assert_eq!(p.text, "50% · 1 B von 2 B");
    }

    #[test]
    fn bytes_read_naturally() {
        assert_eq!(fmt_bytes(0.0), "0 B");
        assert_eq!(fmt_bytes(999.0), "999 B");
        assert_eq!(fmt_bytes(1_500.0), "1.5 kB");
        assert_eq!(fmt_bytes(38_000_000.0), "38 MB");
        assert_eq!(fmt_bytes(2_900_000_000.0), "2.9 GB");
        assert_eq!(fmt_bytes(12_000_000_000_000.0), "12 TB");
    }

    #[test]
    fn cancelled_and_unnamed_jobs_are_not_announced() {
        let mut f = fin(1, "a");
        assert!(should_notify(&f));
        f.cancelled = true;
        assert!(!should_notify(&f));
        let mut g = fin(2, "b");
        g.label = "  ".into();
        assert!(!should_notify(&g));
    }

    #[test]
    fn the_notification_names_the_outcome_and_the_error() {
        let s = BgStrings::default();
        let mut f = fin(1, "a");
        assert_eq!(
            finished_text(&f, &s),
            ("Upload complete".into(), "Game".into())
        );
        f.ok = false;
        f.kind = "install".into();
        f.error = Some("The PS5 refused it".into());
        assert_eq!(
            finished_text(&f, &s),
            ("Install failed".into(), "Game\nThe PS5 refused it".into())
        );
        f.kind = "relay".into();
        assert_eq!(finished_text(&f, &s).0, "Failed");
    }

    #[test]
    fn missing_strings_fall_back_to_english() {
        let s: BgStrings = serde_json::from_str(r#"{"uploading":"Envoi de {name}"}"#).unwrap();
        assert_eq!(s.uploading, "Envoi de {name}");
        assert_eq!(s.done_install, "Installed");
    }

    #[test]
    fn a_hold_expires_without_refresh() {
        bg_work_hold(true);
        let now = now_ms();
        assert!(hold_active(now));
        assert!(!hold_active(now + HOLD_TTL_MS));
        bg_work_hold(false);
        assert!(!hold_active(now));
    }
}
