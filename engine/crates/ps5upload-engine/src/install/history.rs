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

/// `<dir>/<sanitized-console-id>.jsonl`; the id is `ps5_addr` with every char
/// outside `[A-Za-z0-9._-]` replaced by `_`.
pub fn console_file(dir: &Path, ps5_addr: &str) -> PathBuf {
    let id: String = ps5_addr
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{id}.jsonl"))
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
/// entry rather than an error; a corrupt line is skipped, never fatal.
pub fn read_recent(dir: &Path, ps5_addr: &str, limit: usize) -> Vec<HistoryEntry> {
    let path = console_file(dir, ps5_addr);
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out: Vec<HistoryEntry> = content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
        .collect();
    out.reverse(); // newest first
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
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!("ps5u-hist-{}-{}", std::process::id(), n));
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
    fn console_id_is_filesystem_safe() {
        let dir = std::path::Path::new("/tmp");
        let p = console_file(dir, "192.168.0.100:9114");
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "192.168.0.100_9114.jsonl"
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
