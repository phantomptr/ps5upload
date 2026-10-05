//! The transport half of the per-job telemetry record (review 009 #4): what AVA1 knows about
//! a job that the engine's job state does not — where its time went, the console's own
//! end-of-job line, the settle wait. Plain JSON; the engine adds the job's result and writes
//! the record. Nothing here holds an address or a path.

use std::sync::atomic::Ordering;

use ava1::send::Progress;
use serde_json::{json, Value};

/// A short, stable name for a console that is not its address: the first 8 bytes of a
/// domain-separated BLAKE3 of its public key. The same console always gets the same name, so
/// jobs can be grouped, and the name cannot be turned back into the key or the address.
pub fn console_hash(key: &[u8; 32]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"ps5upload job telemetry console v1");
    h.update(key);
    h.finalize().as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn pct(count: u32, ticks: u32) -> f64 {
    if ticks == 0 {
        return 0.0;
    }
    (f64::from(count) * 1000.0 / f64::from(ticks)).round() / 10.0
}

/// The telemetry of one job as of now: call it once the job has ended (or while it runs, for
/// a partial picture). `None` when the sender never ran (the job failed before it started).
pub fn snapshot(p: &Progress) -> Option<Value> {
    let attempts = p.attempts.load(Ordering::Relaxed);
    let t = p
        .telemetry
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if attempts == 0 && t.peer_key.is_none() {
        return None;
    }
    let s = &t.shares;
    let n = f64::from(s.ticks.max(1));
    let lanes_max = s.history.iter().map(|h| h.1).max().unwrap_or(0);
    Some(json!({
        "console": t.peer_key.as_ref().map(console_hash),
        "attempts": attempts,
        "resumed": attempts > 1,
        "shares": {
            "ticks": s.ticks,
            "credit_starved_pct": pct(s.credit_starved, s.ticks),
            "source_starved_pct": pct(s.source_starved, s.ticks),
            "receiver_bound_pct": pct(s.receiver_bound, s.ticks),
            "receiver_bottleneck": crate::progress::bottleneck_name(s.receiver_bottleneck),
        },
        "lanes_avg": (s.lanes_sum as f64 / n * 10.0).round() / 10.0,
        "lanes_max": lanes_max,
        "chunk_avg_kib": (s.chunk_sum as f64 / n / 1024.0).round(),
        "history": s.history.iter().map(|h| json!([h.0, h.1, h.2])).collect::<Vec<_>>(),
        "slow_drive_switch": s.sequential_ticks > 0,
        "settle_ms": p.settle_ms.load(Ordering::Relaxed),
        "unswept_peak": p.unswept_peak.load(Ordering::Relaxed),
        "bytes_total": p.bytes_total.load(Ordering::Relaxed),
        "bytes_sent": p.bytes_sent.load(Ordering::Relaxed),
        "bytes_durable": p.bytes_durable.load(Ordering::Relaxed),
        "files_total": p.files_total.load(Ordering::Relaxed),
        "files_durable": p.files_durable.load(Ordering::Relaxed),
        "resent_bytes": p.resent_bytes.load(Ordering::Relaxed),
        "skipped_files": p.skipped_files.load(Ordering::Relaxed),
        "skipped_bytes": p.skipped_bytes.load(Ordering::Relaxed),
        "console_line": t.console_line,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava1::governor::{Class, Decision, Mode, Sample};

    fn with_ticks(p: &Progress, ticks: &[(bool, bool, u8, u8, bool)]) {
        let mut t = p.telemetry.lock().unwrap();
        for &(credit, source, rbn, lanes, seq) in ticks {
            t.shares.observe(
                &Sample {
                    secs: 1.0,
                    lanes,
                    credit_starved: credit,
                    source_starved: source,
                    receiver_bottleneck: rbn,
                    ..Default::default()
                },
                &Decision {
                    lanes,
                    chunk: 4 << 20,
                    bundle: 0,
                    bottleneck: ava1::gen::BN_NETWORK,
                    mode: Mode::Mixed,
                    prefer: Class::Stream,
                    sequential: seq,
                },
            );
        }
    }

    #[test]
    fn a_job_that_never_ran_has_no_telemetry() {
        assert_eq!(snapshot(&Progress::default()), None);
    }

    #[test]
    fn the_snapshot_carries_the_shares_the_settle_and_the_console_line() {
        let p = Progress::default();
        p.attempts.store(2, Ordering::Relaxed);
        p.settle_ms.store(1500, Ordering::Relaxed);
        p.unswept_peak.store(40, Ordering::Relaxed);
        p.bytes_durable.store(900, Ordering::Relaxed);
        p.files_total.store(12, Ordering::Relaxed);
        with_ticks(
            &p,
            &[
                (true, false, ava1::gen::BN_DISK, 4, false),
                (true, false, ava1::gen::BN_DISK, 4, true),
                (false, true, ava1::gen::BN_DISK, 6, true),
                (false, false, ava1::gen::BN_DISK, 6, true),
            ],
        );
        {
            let mut t = p.telemetry.lock().unwrap();
            t.console_line = Some("apply: 12 files, 3 fsync waits".into());
            t.peer_key = Some([7u8; 32]);
        }
        let v = snapshot(&p).unwrap();
        assert_eq!(v["resumed"], true);
        assert_eq!(v["shares"]["ticks"], 4);
        assert_eq!(v["shares"]["credit_starved_pct"], 50.0);
        assert_eq!(v["shares"]["source_starved_pct"], 25.0);
        assert_eq!(v["shares"]["receiver_bound_pct"], 50.0);
        assert_eq!(v["shares"]["receiver_bottleneck"], "console drive");
        assert_eq!(v["slow_drive_switch"], true);
        assert_eq!(v["lanes_max"], 6);
        assert_eq!(v["lanes_avg"], 5.0);
        assert_eq!(v["chunk_avg_kib"], 4096.0);
        assert_eq!(v["settle_ms"], 1500);
        assert_eq!(v["unswept_peak"], 40);
        assert_eq!(
            (v["bytes_durable"].as_u64(), v["files_total"].as_u64()),
            (Some(900), Some(12))
        );
        assert_eq!(v["console_line"], "apply: 12 files, 3 fsync waits");
        assert_eq!(v["history"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn the_console_name_is_a_stable_hash_never_the_key() {
        let a = console_hash(&[1u8; 32]);
        assert_eq!(a, console_hash(&[1u8; 32]));
        assert_ne!(a, console_hash(&[2u8; 32]));
        assert_eq!(a.len(), 16);
        assert!(!a.contains("0101"), "not the key: {a}");
        let p = Progress::default();
        p.attempts.store(1, Ordering::Relaxed);
        p.telemetry.lock().unwrap().peer_key = Some([1u8; 32]);
        assert_eq!(snapshot(&p).unwrap()["console"], a);
    }
}
