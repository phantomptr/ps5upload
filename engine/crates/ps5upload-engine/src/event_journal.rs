//! The engine's event journal (bug-report spec §1.3): one JSONL file per UTC day under
//! `<data_dir>/events`, kept 7 days and 32 MB. Events reach it through the core sink
//! (`ps5upload_core::events`) and a channel to one writer thread, so no caller ever waits on the
//! disk; a failed write drops the event and counts it. Repeats of one event within a minute are
//! folded into a single line with a count.
use axum::{
    extract::Query,
    response::{IntoResponse, Response},
    Json,
};
use ps5upload_core::events::{now_ms, set_sink, Cat, Event, Src};
use serde::Deserialize;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

pub const MAX_TOTAL: u64 = 32 << 20;
pub const MAX_AGE_MS: u64 = 7 * 86_400_000;
const COLLAPSE_MS: u64 = 60_000;

/// UTC (year, month, day) from ms since the epoch (Howard Hinnant's civil-from-days; no chrono).
pub(crate) fn ymd_from_ms(ts: u64) -> (i64, u32, u32) {
    let z = (ts / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub(crate) fn day_file_name(ts: u64) -> String {
    let (y, m, d) = ymd_from_ms(ts);
    format!("events-{y:04}{m:02}{d:02}.jsonl")
}

/// One-off results: each is its own line, however close together they come.
const NEVER_FOLD: &[&str] = &[
    "install_start",
    "install_result",
    "job_done",
    "job_failed",
    "engine_start",
];

/// The message with every run of digits made one `#`: "reconnecting in 5.2s" and "… 5.97s" match.
fn shape(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut in_digits = false;
    for c in msg.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

/// Whether `b` repeats `a` (so it folds into a count): same source, kind, code, console and level,
/// and the same message but for its numbers. Helper log lines and one-off results never fold.
fn same_key(a: &Event, b: &Event) -> bool {
    a.src != Src::Helper
        && !a.code.as_deref().is_some_and(|c| NEVER_FOLD.contains(&c))
        && a.src == b.src
        && a.cat == b.cat
        && a.code == b.code
        && a.console == b.console
        && a.level == b.level
        && shape(&a.msg) == shape(&b.msg)
}

pub struct Journal {
    dir: PathBuf,
    max_total: u64,
    max_age_ms: u64,
    /// Open entries still collecting repeats, one per kind of event (two consoles polled in
    /// turn would otherwise never fold).
    pending: Vec<Event>,
    dropped: u64,
    /// When old files were last looked for. Listing the folder every second, on a phone too, was
    /// 86,400 wakeups a day to delete a file once a day.
    last_prune_ms: u64,
    /// Today's file, kept open, and its name. One write per event, not an open and a close.
    open_file: Option<(String, fs::File)>,
}

/// Old files are looked for this often, and when the day changes. (A flush at exit always does.)
const PRUNE_EVERY_MS: u64 = 10 * 60 * 1000;

/// More open entries than this and the oldest is written out.
const MAX_PENDING: usize = 64;

impl Journal {
    pub fn open(dir: PathBuf, max_total: u64, max_age_ms: u64) -> Self {
        let _ = fs::create_dir_all(&dir);
        Journal {
            dir,
            max_total,
            max_age_ms,
            pending: Vec::new(),
            dropped: 0,
            last_prune_ms: 0,
            open_file: None,
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn push(&mut self, e: Event) {
        let open = self.pending.iter().position(|p| {
            same_key(p, &e) && e.ts.saturating_sub(p.last_ts.unwrap_or(p.ts)) <= COLLAPSE_MS
        });
        if let Some(i) = open {
            let p = &mut self.pending[i];
            p.count = Some(p.count.unwrap_or(1) + 1);
            p.last_ts = Some(e.ts);
            // The line changed: a poller on `since_seq` sees it again, with the new count.
            p.seq = e.seq.or(p.seq);
            return;
        }
        // An entry whose repeats have stopped (a gap over a minute) is done.
        let stale: Vec<Event> = {
            let (done, keep): (Vec<Event>, Vec<Event>) = std::mem::take(&mut self.pending)
                .into_iter()
                .partition(|p| {
                    same_key(p, &e) || e.ts.saturating_sub(p.last_ts.unwrap_or(p.ts)) > COLLAPSE_MS
                });
            self.pending = keep;
            done
        };
        for p in stale {
            self.write(&p);
        }
        self.pending.push(e);
        if self.pending.len() > MAX_PENDING {
            let oldest = self.pending.remove(0);
            self.write(&oldest);
        }
    }

    /// Writes the pending entry once its collapse window has closed (or now, with `force`),
    /// then prunes.
    pub fn flush_due(&mut self, force: bool) {
        let now = now_ms();
        let (due, keep): (Vec<Event>, Vec<Event>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| force || now.saturating_sub(p.last_ts.unwrap_or(p.ts)) > COLLAPSE_MS);
        self.pending = keep;
        for p in due {
            self.write(&p);
        }
        let day_changed = day_file_name(now) != day_file_name(self.last_prune_ms);
        if force || day_changed || now.saturating_sub(self.last_prune_ms) >= PRUNE_EVERY_MS {
            self.last_prune_ms = now;
            self.prune();
        }
    }

    pub fn flush(&mut self) {
        self.flush_due(true)
    }

    fn write(&mut self, e: &Event) {
        let Ok(line) = serde_json::to_string(e) else {
            self.dropped += 1;
            return;
        };
        let name = day_file_name(e.ts);
        if self.open_file.as_ref().map(|(n, _)| n) != Some(&name) {
            let opened = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(&name));
            match opened {
                Ok(f) => self.open_file = Some((name, f)),
                Err(_) => {
                    self.dropped += 1;
                    return;
                }
            }
        }
        // One write for the whole line (not writeln!'s several), so a reader never sees half.
        let ok = self
            .open_file
            .as_mut()
            .is_some_and(|(_, f)| f.write_all(format!("{line}\n").as_bytes()).is_ok());
        if !ok {
            self.open_file = None;
            self.dropped += 1;
        }
    }

    fn files_oldest_first(&self) -> Vec<(PathBuf, u64)> {
        let mut v: Vec<(PathBuf, u64)> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("events-") && n.ends_with(".jsonl")
            })
            .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
            .collect();
        v.sort();
        v
    }

    fn prune(&mut self) {
        let now = now_ms();
        let oldest_keep = day_file_name(now.saturating_sub(self.max_age_ms));
        let today = day_file_name(now);
        let mut kept = Vec::new();
        for (p, n) in self.files_oldest_first() {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name < oldest_keep {
                self.close_if_open(&name);
                let _ = fs::remove_file(&p);
            } else {
                kept.push((p, n, name));
            }
        }
        let mut total: u64 = kept.iter().map(|(_, n, _)| n).sum();
        // Oldest first; today's file is never deleted, so the cap can be exceeded by today alone.
        for (p, n, name) in kept {
            if total <= self.max_total || name == today {
                break;
            }
            self.close_if_open(&name);
            let _ = fs::remove_file(&p);
            total -= n;
        }
    }

    /// A late event may still be written to a file that goes: not into one that is gone.
    fn close_if_open(&mut self, name: &str) {
        if self.open_file.as_ref().is_some_and(|(n, _)| n == name) {
            self.open_file = None;
        }
    }

    /// What a read needs from under the lock: the writer thread needs that lock for every event,
    /// and the reading itself may take a while (a week of files for a bug report).
    pub fn read_plan(&self, since: u64, until: u64, cats: Option<&[Cat]>) -> ReadPlan {
        // An entry lives in the file of its first time and can have collected repeats since: one
        // day of slack before `since`.
        let first = day_file_name(since.saturating_sub(24 * 60 * 60 * 1000));
        let last = day_file_name(until);
        let in_range = |p: &PathBuf| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string());
            name.is_some_and(|n| n >= first && n <= last)
        };
        let keep = keeper(since, until, cats);
        ReadPlan {
            files: self
                .files_oldest_first()
                .into_iter()
                .map(|(p, _)| p)
                .filter(in_range)
                .collect(),
            pending: self.pending.iter().filter(|p| keep(p)).cloned().collect(),
            since,
            until,
            cats: cats.map(<[Cat]>::to_vec),
        }
    }

    #[cfg(test)]
    pub fn read_range(&self, since: u64, until: u64, cats: Option<&[Cat]>) -> Vec<Event> {
        self.read_plan(since, until, cats).read()
    }
}

/// See [`Journal::read_plan`].
pub struct ReadPlan {
    files: Vec<PathBuf>,
    pending: Vec<Event>,
    since: u64,
    until: u64,
    cats: Option<Vec<Cat>>,
}

impl ReadPlan {
    pub fn read(self) -> Vec<Event> {
        let keep = keeper(self.since, self.until, self.cats.as_deref());
        let mut out = Vec::new();
        for p in &self.files {
            let Ok(text) = fs::read_to_string(p) else {
                continue;
            };
            out.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<Event>(l).ok())
                    .filter(|e| keep(e))
                    // A line from before lines were cleaned stays on disk for a week.
                    .map(Event::without_secrets),
            );
        }
        out.extend(self.pending);
        out.sort_by_key(|e| e.ts); // stable: equal ts keep file order
        out
    }
}

/// Whether an event is in the time range and categories asked for.
fn keeper(since: u64, until: u64, cats: Option<&[Cat]>) -> impl Fn(&Event) -> bool + '_ {
    move |e: &Event| {
        let end = e.last_ts.unwrap_or(e.ts);
        end >= since && e.ts <= until && cats.map(|c| c.contains(&e.cat)).unwrap_or(true)
    }
}

static JOURNAL: OnceLock<Mutex<Journal>> = OnceLock::new();

pub fn journal_dir() -> PathBuf {
    crate::remote::store::data_dir()
        .unwrap_or_else(|| PathBuf::from(".ps5upload"))
        .join("events")
}

/// Opens the journal and makes it the core event sink. Called once from `run()`.
pub fn install() {
    let j = JOURNAL.get_or_init(|| Mutex::new(Journal::open(journal_dir(), MAX_TOTAL, MAX_AGE_MS)));
    let (tx, rx) = std::sync::mpsc::sync_channel::<Event>(4096);
    set_sink(move |e| {
        let mut e = e.without_secrets();
        // The same number on the live feed (`journal` events) and in the journal, so
        // `?since_seq=` and `Last-Event-ID` are one cursor.
        crate::event_bus::bus().publish_with(Some("journal"), |seq| {
            e.seq = Some(seq);
            serde_json::to_string(&e).unwrap_or_default()
        });
        let _ = tx.try_send(e);
    });
    let _ = std::thread::Builder::new()
        .name("event-journal".into())
        .spawn(move || {
            writer_loop(
                rx,
                Duration::from_secs(1),
                |e| {
                    if let Ok(mut g) = j.lock() {
                        g.push(e)
                    }
                },
                || {
                    if let Ok(mut g) = j.lock() {
                        g.flush_due(false)
                    }
                },
            )
        });
}

/// Feeds events to `on_event` and calls `on_tick` (flush what is due, prune old files) about
/// once per `tick`, on a clock rather than only when the channel is idle: a steady stream of
/// events must not put the pruning off for ever.
fn writer_loop(
    rx: std::sync::mpsc::Receiver<Event>,
    tick: Duration,
    mut on_event: impl FnMut(Event),
    mut on_tick: impl FnMut(),
) {
    let mut last_tick = std::time::Instant::now();
    loop {
        match rx.recv_timeout(tick.saturating_sub(last_tick.elapsed())) {
            Ok(e) => on_event(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        if last_tick.elapsed() >= tick {
            on_tick();
            last_tick = std::time::Instant::now();
        }
    }
}

/// Writes everything still collecting repeats. Called when the engine stops, so a quit does not
/// lose the last events (the ones a report about that quit needs). Events still in the channel
/// are drained first.
pub fn flush_now() {
    if let Some(j) = JOURNAL.get() {
        // Give the writer thread a moment to take what was just sent.
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(mut g) = j.lock() {
            g.flush();
        }
    }
}

#[derive(Deserialize)]
pub struct EventsQuery {
    /// From this time (ms). Without it, from the start of what is kept.
    #[serde(default)]
    pub since: u64,
    pub until: Option<u64>,
    pub cat: Option<String>,
    /// Only events numbered after this: the `last_seq` of an earlier answer, or an `id:` from
    /// `/api/events`.
    pub since_seq: Option<u64>,
    /// At most this many events: the next ones after `since_seq`, else the newest.
    pub limit: Option<usize>,
}

/// Applies `since_seq` and `limit` to events oldest first. With `since_seq` the order is the
/// events' numbers and `limit` keeps the first ones, so a poller's cursor never skips any.
fn select(mut events: Vec<Event>, since_seq: Option<u64>, limit: Option<usize>) -> Vec<Event> {
    match since_seq {
        Some(after) => {
            events.retain(|e| e.seq.is_some_and(|s| s > after));
            events.sort_by_key(|e| e.seq);
            events.truncate(limit.unwrap_or(usize::MAX));
        }
        None => {
            if let Some(limit) = limit {
                events.drain(..events.len().saturating_sub(limit));
            }
        }
    }
    events
}

pub(crate) fn parse_cats(s: &str) -> Vec<Cat> {
    s.split(',')
        .filter_map(|c| {
            serde_json::from_value(serde_json::Value::String(c.trim().to_string())).ok()
        })
        .collect()
}

/// `GET /api/event-journal?since=<ms>&until=<ms>&cat=a,b&since_seq=<n>&limit=<n>` ->
/// `{"events":[...],"dropped":N,"last_seq":N}`, oldest first. `last_seq` is the cursor for the next
/// `since_seq`.
pub async fn events_handler(Query(q): Query<EventsQuery>) -> Response {
    let since_seq = q.since_seq;
    let Some(j) = JOURNAL.get() else {
        return Json(serde_json::json!({
            "events": [],
            "dropped": 0,
            "last_seq": since_seq.unwrap_or(0),
        }))
        .into_response();
    };
    let cats = q.cat.as_deref().map(parse_cats);
    let until = q.until.unwrap_or(u64::MAX);
    let (since, limit) = (q.since, q.limit);
    let (events, dropped) = tokio::task::spawn_blocking(move || {
        let (plan, dropped) = {
            let g = j.lock().unwrap_or_else(|e| e.into_inner());
            (g.read_plan(since, until, cats.as_deref()), g.dropped())
        };
        (select(plan.read(), since_seq, limit), dropped)
    })
    .await
    .unwrap_or_default();
    let last_seq = events
        .iter()
        .filter_map(|e| e.seq)
        .max()
        .max(since_seq)
        .unwrap_or(0);
    Json(serde_json::json!({ "events": events, "dropped": dropped, "last_seq": last_seq }))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_core::events::{Cat, Event, Level, Src};

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("evj-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ev(ts: u64, code: &str) -> Event {
        let mut e = Event::new(
            Cat::Connection,
            Level::Warn,
            code,
            Some("10.0.0.9"),
            "m".into(),
            None,
        );
        e.ts = ts;
        e
    }

    fn seq_ev(ts: u64, seq: Option<u64>) -> Event {
        let mut e = ev(ts, "x");
        e.seq = seq;
        e
    }

    #[test]
    fn since_seq_is_a_cursor_that_never_skips() {
        let all = || {
            vec![
                seq_ev(1, None),
                seq_ev(2, Some(12)),
                seq_ev(3, Some(11)),
                seq_ev(4, Some(13)),
            ]
        };
        let seqs = |v: Vec<Event>| v.iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(
            seqs(select(all(), Some(11), None)),
            vec![Some(12), Some(13)],
            "newer numbers only; lines without a number are from before"
        );
        assert_eq!(
            seqs(select(all(), Some(0), Some(2))),
            vec![Some(11), Some(12)],
            "the limit keeps the next ones, not the newest"
        );
    }

    #[test]
    fn a_plain_limit_keeps_the_newest_in_time_order() {
        let got = select(
            vec![seq_ev(1, None), seq_ev(2, None), seq_ev(3, None)],
            None,
            Some(2),
        );
        assert_eq!(got.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(select(vec![seq_ev(1, None)], None, Some(9)).len(), 1);
    }

    #[test]
    fn a_folded_repeat_takes_the_newest_number() {
        let t = TmpDir::new("foldseq");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        let mut a = ev(base, "reconnect");
        a.seq = Some(5);
        let mut b = ev(base + 1_000, "reconnect");
        b.seq = Some(9);
        j.push(a);
        j.push(b);
        let got = j.read_range(0, u64::MAX, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].count, Some(2));
        assert_eq!(got[0].seq, Some(9));
    }

    #[test]
    fn day_file_names_are_utc_dates() {
        assert_eq!(day_file_name(0), "events-19700101.jsonl");
        // 2026-10-09 01:27 UTC
        assert_eq!(day_file_name(1_791_509_234_858), "events-20261009.jsonl");
        assert_eq!(day_file_name(951_782_400_000), "events-20000229.jsonl");
    }

    #[test]
    fn repeats_within_60s_collapse_into_one_line() {
        let t = TmpDir::new("collapse");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        for i in 0..37 {
            j.push(ev(base + i * 5_000, "reconnect"));
        }
        j.push(ev(base + 37 * 5_000, "conn_ok"));
        j.flush();
        let got = j.read_range(0, u64::MAX, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].count, Some(37));
        assert_eq!(got[0].last_ts, Some(base + 36 * 5_000));
        assert_eq!(got[1].code.as_deref(), Some("conn_ok"));
    }

    fn evl(ts: u64, cat: Cat, level: Level, code: &str, msg: &str) -> Event {
        let mut e = Event::new(cat, level, code, Some("10.0.0.9"), msg.into(), None);
        e.ts = ts;
        e
    }

    #[test]
    fn a_failure_is_never_folded_into_a_success() {
        let t = TmpDir::new("levels");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(evl(
            base,
            Cat::Install,
            Level::Info,
            "install_result",
            "install A code=0x00000000",
        ));
        j.push(evl(
            base + 5_000,
            Cat::Install,
            Level::Error,
            "install_result",
            "install B code=0x80b21104",
        ));
        j.flush();
        let got = j.read_range(0, u64::MAX, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].level, Level::Error);
    }

    #[test]
    fn one_off_results_are_never_folded() {
        let t = TmpDir::new("oneoff");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(evl(
            base,
            Cat::Transfer,
            Level::Error,
            "job_failed",
            "job a failed: x",
        ));
        j.push(evl(
            base + 1_000,
            Cat::Transfer,
            Level::Error,
            "job_failed",
            "job a failed: x",
        ));
        j.flush();
        assert_eq!(j.read_range(0, u64::MAX, None).len(), 2);
    }

    #[test]
    fn repeats_that_differ_only_in_numbers_fold() {
        let t = TmpDir::new("digits");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(evl(
            base,
            Cat::Connection,
            Level::Warn,
            "reconnecting",
            "reconnecting in 5.2s: refused",
        ));
        j.push(evl(
            base + 5_000,
            Cat::Connection,
            Level::Warn,
            "reconnecting",
            "reconnecting in 5.97s: refused",
        ));
        j.push(evl(
            base + 10_000,
            Cat::Connection,
            Level::Warn,
            "reconnecting",
            "reconnecting in 5.1s: timed out",
        ));
        j.flush();
        let got = j.read_range(0, u64::MAX, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].count, Some(2));
    }

    #[test]
    fn two_consoles_polled_in_turn_still_fold() {
        let t = TmpDir::new("twoconsoles");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        for i in 0..10u64 {
            for host in ["10.0.0.1", "10.0.0.2"] {
                let mut e = Event::new(
                    Cat::Connection,
                    Level::Info,
                    "status_ok",
                    Some(host),
                    "status ok".into(),
                    None,
                );
                e.ts = base + i * 5_000;
                j.push(e);
            }
        }
        j.flush();
        let got = j.read_range(0, u64::MAX, None);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.count == Some(10)));
    }

    #[test]
    fn gap_over_60s_starts_a_new_entry() {
        let t = TmpDir::new("gap");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(ev(base, "reconnect"));
        j.push(ev(base + 61_000, "reconnect"));
        j.flush();
        assert_eq!(j.read_range(0, u64::MAX, None).len(), 2);
    }

    #[test]
    fn helper_lines_never_collapse() {
        let t = TmpDir::new("helper");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        for i in 0..3 {
            let mut e = Event::new(
                Cat::Helper,
                Level::Info,
                "helper_log",
                Some("h"),
                "[ava1] rpc method 33".into(),
                None,
            );
            e.src = Src::Helper;
            e.ts = now_ms() - 1_000 + i;
            j.push(e);
        }
        j.flush();
        assert_eq!(j.read_range(0, u64::MAX, None).len(), 3);
    }

    #[test]
    fn a_line_kept_on_disk_is_read_without_its_secret() {
        let t = TmpDir::new("secret");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let mut e = ev(now_ms() - 1_000, "a");
        e.msg = "fetch https://u:hunter2@h/x?token=abc123".into();
        j.push(e);
        j.flush();
        let read = j.read_range(0, u64::MAX, None);
        assert_eq!(read.len(), 1);
        let text = serde_json::to_string(&read).unwrap();
        assert!(
            !text.contains("hunter2") && !text.contains("abc123"),
            "{text}"
        );
    }

    #[test]
    fn range_and_category_filter() {
        let t = TmpDir::new("range");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(ev(base, "a"));
        let mut inst = Event::new(Cat::Install, Level::Error, "b", None, "m".into(), None);
        inst.ts = base + 200_000;
        j.push(inst);
        j.push(ev(base + 400_000, "c"));
        j.flush();
        assert_eq!(j.read_range(base + 100_000, base + 300_000, None).len(), 1);
        assert_eq!(j.read_range(0, u64::MAX, Some(&[Cat::Connection])).len(), 2);
    }

    #[test]
    fn a_collapsed_entry_reaching_into_the_range_is_kept() {
        let t = TmpDir::new("reach");
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        let base = now_ms() - 3_600_000;
        j.push(ev(base, "reconnect"));
        j.push(ev(base + 50_000, "reconnect"));
        j.flush();
        assert_eq!(j.read_range(base + 40_000, u64::MAX, None).len(), 1);
    }

    #[test]
    fn the_writer_ticks_on_a_clock_even_when_events_never_stop() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Event>(64);
        let feeder = std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < until {
                let _ = tx.try_send(Event::new(
                    Cat::App,
                    Level::Info,
                    "x",
                    None,
                    "m".into(),
                    None,
                ));
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let (mut events, mut ticks) = (0, 0);
        writer_loop(
            rx,
            Duration::from_millis(50),
            |_| events += 1,
            || ticks += 1,
        );
        feeder.join().unwrap();
        assert!(events > 10);
        assert!(ticks >= 3, "ticked {ticks} times under a steady stream");
    }

    #[test]
    fn prune_drops_old_files_and_keeps_under_the_size_cap() {
        let t = TmpDir::new("prune");
        std::fs::write(t.0.join("events-20200101.jsonl"), "{}\n").unwrap();
        let now = now_ms();
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        j.push(ev(now, "x"));
        j.flush();
        assert!(!t.0.join("events-20200101.jsonl").exists());

        // Yesterday's file over the cap goes; today's is never deleted.
        let yesterday = t.0.join(day_file_name(now - 86_400_000));
        std::fs::write(&yesterday, "x".repeat(400)).unwrap();
        let mut small = Journal::open(t.0.clone(), 200, 7 * 86_400_000);
        small.push(ev(now, "y"));
        small.flush();
        assert!(!yesterday.exists());
        assert!(t.0.join(day_file_name(now)).exists());
    }

    #[test]
    fn unreadable_lines_are_skipped() {
        let t = TmpDir::new("garbage");
        let now = now_ms();
        std::fs::write(t.0.join(day_file_name(now)), "not json\n").unwrap();
        let mut j = Journal::open(t.0.clone(), 32 << 20, 7 * 86_400_000);
        j.push(ev(now, "x"));
        j.flush();
        assert_eq!(j.read_range(0, u64::MAX, None).len(), 1);
    }

    #[test]
    fn a_failed_write_is_counted_not_raised() {
        let t = TmpDir::new("ro");
        let file_not_dir = t.0.join("blocker");
        std::fs::write(&file_not_dir, "").unwrap();
        let mut j = Journal::open(file_not_dir, 32 << 20, 7 * 86_400_000);
        j.push(ev(now_ms(), "x"));
        j.flush();
        assert_eq!(j.dropped(), 1);
    }

    #[test]
    fn categories_parse_from_a_comma_list() {
        assert_eq!(
            parse_cats("connection, helper,bogus"),
            vec![Cat::Connection, Cat::Helper]
        );
    }
}
