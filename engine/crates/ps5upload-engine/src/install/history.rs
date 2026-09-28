//! Per-console install history (spec 2 §7): a durable JSONL append log so a
//! past install is diagnosable and the client can show a "recent installs"
//! list. History is data, not control — nothing keys install behaviour off it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::install::status::{Metrics, Route, Verdict};

pub const HISTORY_CAP: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub job: String,
    /// Epoch seconds, matching `InstallStatus` timestamps.
    pub at: u64,
    pub source_kind: String,
    pub content_id: String,
    pub title_id: Option<String>,
    pub route: Option<Route>,
    pub verdict: Option<Verdict>,
    pub code: u32,
    pub metrics: Metrics,
}

/// The console's identity for history: its host, without a port. Installs
/// arrive as `ip:9114` but readers (the bug bundle, the UI) pass the bare
/// host, so keying by the full address made every bare-host read empty.
fn host_key(ps5_addr: &str) -> &str {
    let a = ps5_addr.trim();
    if let Some(rest) = a.strip_prefix('[') {
        // [v6]:port or [v6]
        return rest.split(']').next().unwrap_or(rest);
    }
    match a.rsplit_once(':') {
        // exactly one colon and a numeric tail → host:port
        Some((host, port)) if !host.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            host
        }
        _ => a,
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `<dir>/<sanitized-host>.jsonl` — every char outside `[A-Za-z0-9._-]`
/// becomes `_`.
pub fn console_file(dir: &Path, ps5_addr: &str) -> PathBuf {
    dir.join(format!("{}.jsonl", sanitize(host_key(ps5_addr))))
}

fn parse_lines(path: &Path) -> Vec<HistoryEntry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<HistoryEntry> = content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
        .collect();
    out.reverse(); // newest first within a file
    out
}

/// Append one entry as a JSON line, then truncate the file to the newest
/// `cap` lines. The truncation rewrites via a temp sibling + `rename` in the
/// same directory (same device — no cross-device rename).
pub fn append(dir: &Path, ps5_addr: &str, entry: &HistoryEntry, cap: usize) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = console_file(dir, ps5_addr);
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{line}")?;
    }
    // Truncate to the newest `cap` lines if we've grown past it.
    let content = fs::read_to_string(&path)?;
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    if lines.len() > cap {
        let keep = &lines[lines.len() - cap..];
        let tmp = path.with_extension("jsonl.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            for l in keep {
                writeln!(f, "{l}")?;
            }
            f.sync_all().ok();
        }
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// Newest first, up to `limit`. A missing file or a corrupt line yields no
/// entry rather than an error; a corrupt line is skipped, never fatal. Also
/// merges legacy `<host>_<port>.jsonl` files written before history was keyed
/// by host alone.
pub fn read_recent(dir: &Path, ps5_addr: &str, limit: usize) -> Vec<HistoryEntry> {
    let host = sanitize(host_key(ps5_addr));
    let mut out = parse_lines(&dir.join(format!("{host}.jsonl")));
    if let Ok(rd) = fs::read_dir(dir) {
        let prefix = format!("{host}_");
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let legacy = name
                .strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(".jsonl"))
                .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
            if legacy {
                out.extend(parse_lines(&e.path()));
            }
        }
    }
    // Stable: equal timestamps keep newest-first file order.
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::status::{Metrics, Route, Verdict};

    /// A unique temp dir without pulling in the `tempfile` crate (not in the
    /// lockfile). Cleaned up on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> Self {
            // A counter as well as the clock: macOS time ticks in microseconds, so two tests
            // starting together got the same directory and one deleted it under the other.
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!(
                "ps5u-hist-{}-{}-{}",
                std::process::id(),
                n,
                seq
            ));
            fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(job: &str) -> HistoryEntry {
        HistoryEntry {
            job: job.into(),
            at: 1_790_000_000,
            source_kind: "remote".into(),
            content_id: "UP4433-PPSA17221_00".into(),
            title_id: Some("PPSA17221".into()),
            route: Some(Route::Stream),
            verdict: Some(Verdict::Installed),
            code: 0,
            metrics: Metrics::default(),
        }
    }

    #[test]
    fn console_file_is_keyed_by_host_and_filesystem_safe() {
        let dir = std::path::Path::new("/tmp");
        // The port is not part of the console's identity: installs arrive as
        // "ip:9114" while readers (bug bundle, UI) pass the bare host.
        for addr in ["192.168.0.100:9114", "192.168.0.100", "192.168.0.100:9113"] {
            let p = console_file(dir, addr);
            assert_eq!(
                p.file_name().unwrap().to_str().unwrap(),
                "192.168.0.100.jsonl"
            );
        }
    }

    #[test]
    fn written_with_a_port_is_read_by_bare_host() {
        // Measured on the Phat: history was written under ..._9114.jsonl and
        // the bare-host read (the bug bundle's) always came back empty.
        let d = TmpDir::new();
        append(d.path(), "192.168.86.99:9114", &entry("a"), HISTORY_CAP).unwrap();
        let got = read_recent(d.path(), "192.168.86.99", 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].job, "a");
    }

    #[test]
    fn a_legacy_host_port_file_is_still_read() {
        // Files already on disk from before the fix keep showing up, merged
        // newest-first with the new host-keyed file.
        let d = TmpDir::new();
        let mut old = entry("old");
        old.at = 1_790_000_000;
        std::fs::write(
            d.path().join("192.168.86.99_9114.jsonl"),
            format!("{}\n", serde_json::to_string(&old).unwrap()),
        )
        .unwrap();
        let mut new = entry("new");
        new.at = 1_790_000_100;
        append(d.path(), "192.168.86.99:9114", &new, HISTORY_CAP).unwrap();
        let got = read_recent(d.path(), "192.168.86.99", 10);
        assert_eq!(
            got.iter().map(|e| e.job.as_str()).collect::<Vec<_>>(),
            ["new", "old"]
        );
    }

    #[test]
    fn append_then_read_newest_first() {
        let d = TmpDir::new();
        append(d.path(), "10.0.0.2:9114", &entry("a"), HISTORY_CAP).unwrap();
        append(d.path(), "10.0.0.2:9114", &entry("b"), HISTORY_CAP).unwrap();
        let got = read_recent(d.path(), "10.0.0.2:9114", 10);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].job, "b");
        assert_eq!(got[1].job, "a");
    }

    #[test]
    fn cap_keeps_only_newest() {
        let d = TmpDir::new();
        for i in 0..5 {
            append(d.path(), "h", &entry(&format!("j{i}")), 3).unwrap();
        }
        let got = read_recent(d.path(), "h", 100);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].job, "j4");
        assert_eq!(got[2].job, "j2");
    }

    #[test]
    fn missing_file_reads_empty_and_corrupt_line_is_skipped() {
        let d = TmpDir::new();
        assert!(read_recent(d.path(), "absent", 10).is_empty());
        let f = console_file(d.path(), "h");
        std::fs::write(&f, "not json\n").unwrap();
        assert!(read_recent(d.path(), "h", 10).is_empty());
    }

    #[test]
    fn different_consoles_do_not_mix() {
        let d = TmpDir::new();
        append(d.path(), "a:9114", &entry("x"), HISTORY_CAP).unwrap();
        append(d.path(), "b:9114", &entry("y"), HISTORY_CAP).unwrap();
        assert_eq!(read_recent(d.path(), "a:9114", 10).len(), 1);
        assert_eq!(read_recent(d.path(), "b:9114", 10)[0].job, "y");
    }
}
