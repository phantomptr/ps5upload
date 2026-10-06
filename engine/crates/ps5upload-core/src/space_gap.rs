//! What a console's drive could not really allocate, learned from an upload that ran out of room.
//!
//! The PS5 content allocator can refuse writes while statfs still reports plenty free (a 153 GiB
//! upload stopped at 139 GiB with ~30 GB "free" never usable). The size of that pool is not
//! readable up front, and guessing it (a flat 80 GiB once) refused uploads that fit. What can be
//! known is what happened: when an upload that was admitted runs out of room part-way, the room
//! it was promised less what it wrote is room that did not exist. That gap is remembered per
//! console drive and taken off later checks and the Volumes "safe for new uploads" figure, so the
//! next upload that cannot fit is refused before it starts.
//!
//! Only a failure late in a write teaches anything: one at the very start (a preallocation that
//! does not fit) wrote nothing, which says nothing about the pool. A later upload that writes
//! more than the remembered room proves the gap smaller, and it is lowered to match.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// A failure has to have written at least this share of the room it was promised to say
/// anything about the pool.
const LEARN_MIN_SHARE: f64 = 0.5;

/// The drive key: the console host and the drive's total size (stable, and known to both the
/// volume list and the free-space probe).
fn key(host: &str, total_bytes: u64) -> String {
    format!("{}|{total_bytes}", bare_host(host))
}

/// `host` without a port: `10.0.0.2:9120` and `10.0.0.2` name the same console.
fn bare_host(host: &str) -> &str {
    let h = host.trim();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match h.rsplit_once(':') {
        Some((a, p)) if !a.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => a,
        _ => h,
    }
}

fn store_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None; // tests never touch the person's data folder
    }
    let data = std::env::var("PS5UPLOAD_DATA_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|v| !v.trim().is_empty());
    data.map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".ps5upload")))
        .map(|d| d.join("space_gaps.json"))
}

fn table() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| {
        let loaded = store_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

fn save(t: &HashMap<String, u64>) {
    let Some(p) = store_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(b) = serde_json::to_vec(t) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// The bytes this drive has been seen not to allocate, 0 when nothing was learned.
pub fn gap(host: &str, total_bytes: u64) -> u64 {
    if total_bytes == 0 {
        return 0;
    }
    table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key(host, total_bytes))
        .copied()
        .unwrap_or(0)
}

/// An admitted upload ran out of room after writing `written` of the `promised` bytes the
/// check offered. Returns the gap now remembered for the drive.
pub fn learn_from_failure(host: &str, total_bytes: u64, promised: u64, written: u64) -> u64 {
    if total_bytes == 0 || promised == 0 || (written as f64) < promised as f64 * LEARN_MIN_SHARE {
        return gap(host, total_bytes);
    }
    let missing = promised.saturating_sub(written);
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    let k = key(host, total_bytes);
    // A later, larger shortfall replaces a smaller one; a smaller one keeps the larger (both
    // happened, and the larger is the one that refuses in time).
    let now = t.get(&k).copied().unwrap_or(0).max(missing);
    t.insert(k, now);
    save(&t);
    now
}

/// An upload wrote `written` bytes with `allocatable_raw` reported before the gap was taken
/// off: if it wrote past what the gap allowed, the gap was too large and is lowered.
pub fn learn_from_success(host: &str, total_bytes: u64, allocatable_raw: u64, written: u64) {
    let k = key(host, total_bytes);
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    let Some(&g) = t.get(&k) else { return };
    let allowed = allocatable_raw.saturating_sub(g);
    if written > allowed {
        let lowered = g.saturating_sub(written - allowed);
        if lowered == 0 {
            t.remove(&k);
        } else {
            t.insert(k, lowered);
        }
        save(&t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hosts are unique per test: the table is process-wide.
    const GB: u64 = 1_000_000_000;

    #[test]
    fn a_late_failure_teaches_the_gap_the_report_measured() {
        // 179 GB promised, 149 GB written before the console refused: 30 GB never existed.
        let h = "10.0.0.201";
        assert_eq!(gap(h, 671 * GB), 0);
        assert_eq!(learn_from_failure(h, 671 * GB, 179 * GB, 149 * GB), 30 * GB);
        assert_eq!(gap(h, 671 * GB), 30 * GB);
        // another drive of the same console is not affected; a port does not change the console
        assert_eq!(gap(h, 2000 * GB), 0);
        assert_eq!(gap("10.0.0.201:9120", 671 * GB), 30 * GB);
    }

    #[test]
    fn a_failure_at_the_start_teaches_nothing() {
        let h = "10.0.0.202";
        assert_eq!(learn_from_failure(h, 671 * GB, 179 * GB, 2 * GB), 0);
        assert_eq!(gap(h, 671 * GB), 0);
    }

    #[test]
    fn a_larger_shortfall_wins_and_a_smaller_one_does_not_shrink_it() {
        let h = "10.0.0.203";
        learn_from_failure(h, 671 * GB, 100 * GB, 90 * GB);
        assert_eq!(learn_from_failure(h, 671 * GB, 100 * GB, 70 * GB), 30 * GB);
        assert_eq!(learn_from_failure(h, 671 * GB, 100 * GB, 95 * GB), 30 * GB);
    }

    #[test]
    fn an_upload_that_wrote_past_the_gap_lowers_it() {
        let h = "10.0.0.204";
        learn_from_failure(h, 671 * GB, 100 * GB, 70 * GB); // gap 30
                                                            // 100 GB reported, so 70 allowed; 85 were written fine: the gap is at most 15
        learn_from_success(h, 671 * GB, 100 * GB, 85 * GB);
        assert_eq!(gap(h, 671 * GB), 15 * GB);
        learn_from_success(h, 671 * GB, 100 * GB, 100 * GB);
        assert_eq!(gap(h, 671 * GB), 0);
    }
}
