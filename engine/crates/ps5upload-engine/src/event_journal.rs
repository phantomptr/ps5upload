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
fn ymd_from_ms(ts: u64) -> (i64, u32, u32) {
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

/// Helper stderr lines are never folded: two identical-looking lines are two log lines.
fn same_key(a: &Event, b: &Event) -> bool {
    a.src != Src::Helper
        && a.src == b.src
        && a.cat == b.cat
        && a.code == b.code
        && a.console == b.console
}

pub struct Journal {
    dir: PathBuf,
    max_total: u64,
    max_age_ms: u64,
    pending: Option<Event>,
    dropped: u64,
}

impl Journal {
    pub fn open(dir: PathBuf, max_total: u64, max_age_ms: u64) -> Self {
        let _ = fs::create_dir_all(&dir);
        Journal {
            dir,
            max_total,
            max_age_ms,
            pending: None,
            dropped: 0,
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn push(&mut self, e: Event) {
        if let Some(p) = self.pending.as_mut() {
            let last = p.last_ts.unwrap_or(p.ts);
            if same_key(p, &e) && e.ts.saturating_sub(last) <= COLLAPSE_MS {
                p.count = Some(p.count.unwrap_or(1) + 1);
                p.last_ts = Some(e.ts);
                return;
            }
        }
        if let Some(prev) = self.pending.replace(e) {
            self.write(&prev);
        }
    }

    /// Writes the pending entry once its collapse window has closed (or now, with `force`),
    /// then prunes.
    pub fn flush_due(&mut self, force: bool) {
        let due = self
            .pending
            .as_ref()
            .map(|p| force || now_ms().saturating_sub(p.last_ts.unwrap_or(p.ts)) > COLLAPSE_MS)
            .unwrap_or(false);
        if due {
            if let Some(p) = self.pending.take() {
                self.write(&p);
            }
        }
        self.prune();
    }

    #[cfg(test)]
    pub fn flush(&mut self) {
        self.flush_due(true)
    }

    fn write(&mut self, e: &Event) {
        let Ok(line) = serde_json::to_string(e) else {
            self.dropped += 1;
            return;
        };
        let path = self.dir.join(day_file_name(e.ts));
        let ok = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"))
            .is_ok();
        if !ok {
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
            let _ = fs::remove_file(&p);
            total -= n;
        }
    }

    pub fn read_range(&self, since: u64, until: u64, cats: Option<&[Cat]>) -> Vec<Event> {
        let keep = |e: &Event| {
            let end = e.last_ts.unwrap_or(e.ts);
            end >= since && e.ts <= until && cats.map(|c| c.contains(&e.cat)).unwrap_or(true)
        };
        let mut out = Vec::new();
        for (p, _) in self.files_oldest_first() {
            let Ok(text) = fs::read_to_string(&p) else {
                continue;
            };
            out.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<Event>(l).ok())
                    .filter(|e| keep(e)),
            );
        }
        if let Some(p) = self.pending.as_ref().filter(|p| keep(p)) {
            out.push(p.clone());
        }
        out.sort_by_key(|e| e.ts); // stable: equal ts keep file order
        out
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
        let _ = tx.try_send(e);
    });
    let _ = std::thread::Builder::new()
        .name("event-journal".into())
        .spawn(move || loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(e) => {
                    if let Ok(mut g) = j.lock() {
                        g.push(e)
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(mut g) = j.lock() {
                        g.flush_due(false)
                    }
                }
                Err(_) => break,
            }
        });
}

#[derive(Deserialize)]
pub struct EventsQuery {
    pub since: u64,
    pub until: Option<u64>,
    pub cat: Option<String>,
}

pub(crate) fn parse_cats(s: &str) -> Vec<Cat> {
    s.split(',')
        .filter_map(|c| {
            serde_json::from_value(serde_json::Value::String(c.trim().to_string())).ok()
        })
        .collect()
}

/// `GET /api/event-journal?since=<ms>&until=<ms>&cat=a,b` -> `{"events":[...],"dropped":N}`, oldest first.
pub async fn events_handler(Query(q): Query<EventsQuery>) -> Response {
    let Some(j) = JOURNAL.get() else {
        return Json(serde_json::json!({ "events": [], "dropped": 0 })).into_response();
    };
    let cats = q.cat.as_deref().map(parse_cats);
    let until = q.until.unwrap_or(u64::MAX);
    let since = q.since;
    let (events, dropped) = tokio::task::spawn_blocking(move || {
        let g = j.lock().unwrap_or_else(|e| e.into_inner());
        (g.read_range(since, until, cats.as_deref()), g.dropped())
    })
    .await
    .unwrap_or_default();
    Json(serde_json::json!({ "events": events, "dropped": dropped })).into_response()
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
