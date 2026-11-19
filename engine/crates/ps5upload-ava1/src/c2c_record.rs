//! The shared record of a console-to-console send (#444). The job id lives only in the queue of
//! the device that started the send, so that device writes what another device needs to find,
//! watch or take over the send into a small file on the receiving console,
//! `/data/ps5upload/c2c/<job id>.json`, with the management fs write any paired device can read.
//! The record also holds the lease: the device whose lease is fresh drives the send, and one that
//! finds the lease moved to another device stops driving it (without ending anything).
//!
//! Lease times are wall-clock milliseconds of the devices involved; a few seconds of clock skew
//! between them only shifts when a stopped device's send reads as interrupted.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ps5upload_core::diagnostics::fs_write_bytes;
use ps5upload_core::fs_ops::{
    fs_delete, fs_mkdir, fs_read, is_not_found, list_dir, ListDirOptions,
};
use serde::{Deserialize, Serialize};

use crate::c2c::Lease;

/// Where the records live on the receiving console.
pub const DIR: &str = "/data/ps5upload/c2c";
/// A lease not refreshed for this long has lapsed: its device stopped or lost the console.
pub const LEASE_MS: u64 = 45_000;
/// How often the driving device refreshes its lease (and looks whether it still holds it).
pub const REFRESH: Duration = Duration::from_secs(15);
/// Records not updated for this long are removed when a device lists them.
pub const KEEP_MS: u64 = 7 * 24 * 3600 * 1000;

pub const RUNNING: &str = "running";
pub const PAUSED: &str = "paused";
pub const FAILED: &str = "failed";

/// One send, as the receiving console keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRecord {
    pub v: u32,
    /// The job id on both consoles (hex), the queue's `txIdHex`.
    pub tx_id: String,
    /// The consoles as the driving device addresses them, and their keys (hex): another
    /// device may know them by another address.
    pub from: String,
    #[serde(default)]
    pub from_key: String,
    pub to: String,
    #[serde(default)]
    pub to_key: String,
    pub src: String,
    pub dest: String,
    /// `auto`, `direct` or `relay`, and what the sending console dials.
    pub route: String,
    #[serde(default)]
    pub direct_addr: Option<String>,
    /// The driving device's key (hex) and name.
    pub owner: String,
    pub owner_name: String,
    pub lease_until_ms: u64,
    pub updated_ms: u64,
    /// `running`, `paused` or `failed`.
    pub state: String,
    /// The route it runs on now (`direct`, `relay`), once known.
    #[serde(default)]
    pub live_route: Option<String>,
    #[serde(default)]
    pub bytes_done: u64,
    #[serde(default)]
    pub bytes_total: u64,
    #[serde(default)]
    pub error: Option<String>,
}

impl SendRecord {
    pub fn lease_fresh(&self, now_ms: u64) -> bool {
        self.lease_until_ms > now_ms
    }

    /// Old enough to remove (a send nobody resumed in a week).
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.updated_ms) > KEEP_MS
    }
}

/// How a send reads to a device that lists the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// This engine runs it now.
    Mine,
    /// Another device drives it (a fresh lease).
    RunningElsewhere,
    /// Its device paused it: both halves are ended, any device may resume it.
    Paused,
    /// It stopped without a pause (failed, or its device went away): resumable.
    Interrupted,
}

pub fn classify(r: &SendRecord, me: &str, now_ms: u64, running_here: bool) -> Class {
    if running_here {
        Class::Mine
    } else if r.state == RUNNING && r.lease_fresh(now_ms) && r.owner != me {
        Class::RunningElsewhere
    } else if r.state == PAUSED {
        Class::Paused
    } else {
        Class::Interrupted
    }
}

/// What the receiving console says about the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Running,
    Finished,
    Failed,
    /// Not listed for this device (on an older helper also another device's listed job).
    NotListed,
    /// The console could not be asked.
    Unknown,
}

impl Seen {
    pub fn of(status: &Result<Option<crate::gen::Status>>) -> Seen {
        match status {
            Ok(Some(st)) => match st.state {
                Some(1) => Seen::Finished,
                Some(0) | None => Seen::Running,
                Some(_) => Seen::Failed,
            },
            Ok(None) => Seen::NotListed,
            Err(_) => Seen::Unknown,
        }
    }
}

/// An interrupted-looking send checked against the receiving console before it is shown as
/// interrupted: one whose job still runs there is running elsewhere (its lease lapsed, its
/// bytes still move, e.g. a direct send whose device closed); `None` = it finished, the record
/// only outlived it.
pub fn settle(class: Class, seen: Seen) -> Option<Class> {
    match (class, seen) {
        (_, Seen::Finished) => None,
        (Class::Interrupted, Seen::Running) => Some(Class::RunningElsewhere),
        (c, _) => Some(c),
    }
}

/// Who may drive a send whose record is `existing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// No record, or one nobody drives now.
    Free,
    /// This device's own.
    Mine,
    /// Another device drives it.
    Held { by: String },
}

pub fn claim(existing: Option<&SendRecord>, me: &str, now_ms: u64) -> Claim {
    match existing {
        None => Claim::Free,
        Some(r) if r.owner == me => Claim::Mine,
        Some(r) if r.state == RUNNING && r.lease_fresh(now_ms) => Claim::Held {
            by: r.owner_name.clone(),
        },
        Some(_) => Claim::Free,
    }
}

/// The device the lease moved to, if it moved away from `me`.
pub fn moved(existing: Option<&SendRecord>, me: &str) -> Option<String> {
    existing
        .filter(|r| r.owner != me)
        .map(|r| r.owner_name.clone())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// This engine's key as a record's owner (hex); empty without an identity.
pub fn me() -> String {
    crate::pool()
        .identity_key()
        .map(|k| ava1::hex::encode(&k))
        .unwrap_or_default()
}

/// What another device's "Taken over by …" calls this one: `PS5UPLOAD_DEVICE_NAME`, else the
/// computer's name.
pub fn device_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(n) = env("PS5UPLOAD_DEVICE_NAME").or_else(|| env("COMPUTERNAME")) {
            return n.trim().to_string();
        }
        let run = |cmd: &str, args: &[&str]| {
            let mut c = std::process::Command::new(cmd);
            c.args(args);
            // CREATE_NO_WINDOW: without it a console window flashes up under the app.
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
            c.output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let mac = if cfg!(target_os = "macos") {
            run("scutil", &["--get", "ComputerName"])
        } else {
            None
        };
        mac.or_else(|| run("hostname", &[]))
            .unwrap_or_else(|| "another device".to_string())
    })
    .clone()
}

pub fn path(tx_hex: &str) -> String {
    format!("{DIR}/{tx_hex}.json")
}

pub fn read(console: &str, tx_hex: &str) -> Result<Option<SendRecord>> {
    let bytes = match fs_read(console, &path(tx_hex), 0, 64 * 1024) {
        Ok(b) => b,
        Err(e) if is_not_found(&format!("{e:#}")) => return Ok(None),
        Err(e) => return Err(e).context("read the send record"),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_slice(&bytes).ok())
}

pub fn write(console: &str, r: &SendRecord) -> Result<()> {
    let body = serde_json::to_vec(r)?;
    fs_write_bytes(console, &path(&r.tx_id), &body, false).context("write the send record")?;
    Ok(())
}

pub fn remove(console: &str, tx_hex: &str) -> Result<()> {
    match fs_delete(console, &path(tx_hex)) {
        Err(e) if !is_not_found(&format!("{e:#}")) => Err(e).context("remove the send record"),
        _ => Ok(()),
    }
}

/// Every record on `console`; ones a week old are removed on the way.
pub fn list(console: &str) -> Result<Vec<SendRecord>> {
    let listing = match list_dir(console, DIR, ListDirOptions::default()) {
        Ok(l) => l,
        Err(e) if is_not_found(&format!("{e:#}")) => return Ok(Vec::new()),
        Err(e) => return Err(e).context("list the send records"),
    };
    let now = now_ms();
    let mut out = Vec::new();
    for e in listing.entries {
        // Dot files are the connection test's probes, not sends.
        let Some(tx) = e
            .name
            .strip_suffix(".json")
            .filter(|_| !e.name.starts_with('.'))
        else {
            continue;
        };
        match read(console, tx) {
            Ok(Some(r)) if r.expired(now) => {
                let _ = remove(console, tx);
            }
            Ok(Some(r)) => out.push(r),
            _ => {}
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.updated_ms));
    Ok(out)
}

static RUNNING_HERE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Marks a send (its job id, hex) as driven by this engine while it lives.
pub struct RunningHere(String);

impl RunningHere {
    pub fn new(tx_hex: &str) -> RunningHere {
        RUNNING_HERE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx_hex.to_string());
        RunningHere(tx_hex.to_string())
    }
}

impl Drop for RunningHere {
    fn drop(&mut self) {
        let mut v = RUNNING_HERE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = v.iter().position(|t| *t == self.0) {
            v.swap_remove(i);
        }
    }
}

pub fn running_here(tx_hex: &str) -> bool {
    RUNNING_HERE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|t| t == tx_hex)
}

/// A console's identity key (hex), as the record keeps it; empty when it cannot be asked.
pub fn console_key(console: &str) -> String {
    crate::block_on(async {
        crate::pool()
            .session(console)
            .await
            .map(|s| ava1::hex::encode(&s.peer_key()))
            .unwrap_or_default()
    })
}

/// How a send this engine drove ended, for its record.
pub enum Outcome {
    Done,
    Paused,
    Failed(String),
}

/// Holds the lease of one send while this engine drives it: writes the record, refreshes it
/// every [`REFRESH`] with the progress, and notices when another device took the lease.
pub struct Keeper {
    console: String,
    record: SendRecord,
    lease: Arc<Lease>,
    snapshot: Snapshot,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Fills a record's live fields (route, bytes) from the running send.
pub type Snapshot = Arc<dyn Fn(&mut SendRecord) + Send + Sync>;

impl Keeper {
    /// Claims the lease and starts keeping it. `Err` = another device drives the send and this
    /// is not a take-over (`take`): the error is [`crate::c2c::taken_over`]. A console whose
    /// record cannot be read or written (an older helper) runs without one.
    pub fn start(record: SendRecord, take: bool, snapshot: Snapshot) -> Result<Keeper> {
        let console = record.to.clone();
        let me = record.owner.clone();
        let tx = record.tx_id.clone();
        if !take {
            if let Claim::Held { by } =
                claim(read(&console, &tx).ok().flatten().as_ref(), &me, now_ms())
            {
                return Err(crate::c2c::taken_over(&by));
            }
        }
        let mut record = record;
        record.v = 1;
        record.state = RUNNING.into();
        stamp(&mut record);
        if let Err(e) = fs_mkdir(&console, DIR).and_then(|_| write(&console, &record)) {
            log(&format!(
                "c2c record {tx}: {e:#} (the send runs without one)"
            ));
        }
        let lease = Arc::new({
            let (console, tx, me) = (console.clone(), tx.clone(), me.clone());
            Lease::new(move || moved(read(&console, &tx).ok().flatten().as_ref(), &me))
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (console, lease, stop, mut rec, snapshot) = (
                console.clone(),
                lease.clone(),
                stop.clone(),
                record.clone(),
                snapshot.clone(),
            );
            std::thread::Builder::new()
                .name("c2c-lease".into())
                .spawn(move || loop {
                    let until = std::time::Instant::now() + REFRESH;
                    while std::time::Instant::now() < until {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    if let Ok(now) = read(&console, &rec.tx_id) {
                        if let Some(by) = moved(now.as_ref(), &rec.owner) {
                            lease.mark_taken(by);
                            return;
                        }
                    }
                    snapshot(&mut rec);
                    stamp(&mut rec);
                    let _ = write(&console, &rec);
                })
                .ok()
        };
        Ok(Keeper {
            console,
            record,
            lease,
            snapshot,
            stop,
            thread,
        })
    }

    pub fn lease(&self) -> Arc<Lease> {
        self.lease.clone()
    }

    /// Stops keeping the lease and leaves the record as the send ended: gone when it is done,
    /// paused or failed (resumable from any device) otherwise. A send another device took over
    /// is that device's record now.
    pub fn finish(mut self, outcome: Outcome) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let tx = self.record.tx_id.clone();
        let now = read(&self.console, &tx);
        if self.lease.taken_by().is_some()
            || moved(
                now.as_ref().ok().and_then(|r| r.as_ref()),
                &self.record.owner,
            )
            .is_some()
        {
            return;
        }
        let r = match outcome {
            Outcome::Done => remove(&self.console, &tx),
            Outcome::Paused | Outcome::Failed(_) => {
                let mut rec = self.record.clone();
                (self.snapshot)(&mut rec);
                stamp(&mut rec);
                rec.lease_until_ms = 0; // nobody drives it now
                (rec.state, rec.error) = match outcome {
                    Outcome::Failed(why) => (FAILED.into(), Some(why)),
                    _ => (PAUSED.into(), None),
                };
                write(&self.console, &rec)
            }
        };
        if let Err(e) = r {
            log(&format!("c2c record {tx}: {e:#}"));
        }
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn stamp(r: &mut SendRecord) {
    let now = now_ms();
    r.updated_ms = now;
    r.lease_until_ms = now + LEASE_MS;
}

fn log(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "ava1 {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(owner: &str, state: &str, lease_until_ms: u64) -> SendRecord {
        SendRecord {
            tx_id: "ab".into(),
            owner: owner.into(),
            owner_name: format!("{owner}'s Mac"),
            state: state.into(),
            lease_until_ms,
            updated_ms: 1_000,
            ..Default::default()
        }
    }

    #[test]
    fn records_classify_by_who_drives_them_and_their_lease() {
        let now = 10_000;
        assert_eq!(
            classify(&rec("b", RUNNING, now + 1), "a", now, true),
            Class::Mine
        );
        assert_eq!(
            classify(&rec("b", RUNNING, now + 1), "a", now, false),
            Class::RunningElsewhere
        );
        // A lapsed lease: its device went away mid-send.
        assert_eq!(
            classify(&rec("b", RUNNING, now), "a", now, false),
            Class::Interrupted
        );
        assert_eq!(
            classify(&rec("b", PAUSED, 0), "a", now, false),
            Class::Paused
        );
        assert_eq!(
            classify(&rec("b", FAILED, 0), "a", now, false),
            Class::Interrupted
        );
        // This engine's own record of a send it no longer runs (it restarted): resumable.
        assert_eq!(
            classify(&rec("a", RUNNING, now + 1), "a", now, false),
            Class::Interrupted
        );
    }

    #[test]
    fn an_interrupted_send_is_checked_against_the_receiving_console() {
        assert_eq!(
            settle(Class::Interrupted, Seen::Running),
            Some(Class::RunningElsewhere)
        );
        assert_eq!(settle(Class::Interrupted, Seen::Finished), None);
        assert_eq!(
            settle(Class::Interrupted, Seen::NotListed),
            Some(Class::Interrupted)
        );
        assert_eq!(
            settle(Class::Interrupted, Seen::Unknown),
            Some(Class::Interrupted)
        );
        assert_eq!(settle(Class::Paused, Seen::Running), Some(Class::Paused));
    }

    #[test]
    fn a_fresh_lease_of_another_device_holds_the_send() {
        let now = 10_000;
        assert_eq!(claim(None, "a", now), Claim::Free);
        assert_eq!(
            claim(Some(&rec("a", RUNNING, now + 1)), "a", now),
            Claim::Mine
        );
        assert_eq!(
            claim(Some(&rec("b", RUNNING, now + 1)), "a", now),
            Claim::Held {
                by: "b's Mac".into()
            }
        );
        assert_eq!(claim(Some(&rec("b", RUNNING, now)), "a", now), Claim::Free);
        assert_eq!(
            claim(Some(&rec("b", PAUSED, now + 1)), "a", now),
            Claim::Free
        );
        assert_eq!(
            moved(Some(&rec("b", RUNNING, 0)), "a").as_deref(),
            Some("b's Mac")
        );
        assert_eq!(moved(Some(&rec("a", RUNNING, 0)), "a"), None);
        assert_eq!(moved(None, "a"), None, "a missing record is no take-over");
    }

    #[test]
    fn a_week_old_record_expires_and_a_record_round_trips() {
        let r = rec("b", PAUSED, 0);
        assert!(!r.expired(1_000 + KEEP_MS));
        assert!(r.expired(1_001 + KEEP_MS));
        let back: SendRecord = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        // Fields added later read as their defaults.
        let old: SendRecord = serde_json::from_str(
            r#"{"v":1,"tx_id":"ab","from":"f","to":"t","src":"/s","dest":"/d","route":"auto",
               "owner":"b","owner_name":"B","lease_until_ms":0,"updated_ms":1,"state":"paused"}"#,
        )
        .unwrap();
        assert_eq!(old.live_route, None);
        assert_eq!(old.bytes_done, 0);
    }
}
