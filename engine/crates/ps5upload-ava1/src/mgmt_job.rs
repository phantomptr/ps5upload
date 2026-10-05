//! Long management operations as `job.run` jobs (P3 Task 5).
//!
//! A delete of a 220k-file game folder, a checksum of a 60 GiB image, an fsck or a backup
//! used to hold one socket for as long as it took (the 1-hour deadlines). Now the console
//! runs it as a job, answers `job.run` at once and this module polls `job.status` every
//! 500 ms until it ends or the caller's deadline passes (`ps5upload_core::mgmt::run_op`).
//!
//! * The caller's `op_id` is the low 8 bytes of the job id (the high 8 are a per-call nonce,
//!   so a finished job of an earlier call with the same `op_id` can never be mistaken for
//!   this one), which is what keeps `/api/ps5/fs/op-status?op_id=` and `op-cancel` working
//!   unchanged: [`op_job_id`] and [`op_id_of`] map both ways, and a registry of the calls in
//!   flight maps an `op_id` to its console and job.
//! * The wait survives a lost connection: the job runs on the console, a status poll after the
//!   reconnect finds it (owner-keyed, not session-keyed), and a `job.run` sent again is
//!   idempotent. A job the console no longer knows (it restarted) is started again, except a
//!   backup snapshot, which would then be taken twice.
//! * A failed job is the `MgmtError` callers know: `payload rejected <LABEL>: <cause>`, with the
//!   status the console reported and its own cause token.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ava1::gen::{self, JobRef, JobRun, Status};
use ava1::wire::Message;
use ava1::Ava1Error;
use ps5upload_core::mgmt::{JobCall, JobOp, JobProgress, MgmtError};

use crate::mgmt::AvaTransport;
use crate::mgmt_convert as conv;
use crate::pool::host_of;
use crate::upload::hex;

/// Between two status polls.
pub const POLL: Duration = Duration::from_millis(500);
/// The first polls come sooner (a hash of one small file is done in milliseconds): 20, 50, 100,
/// 250 ms, then [`POLL`].
const EARLY_POLLS: [Duration; 4] = [
    Duration::from_millis(20),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(250),
];
/// How long a progress query waits for the console to list the job (the run reply not yet in).
const LISTED_WAIT: Duration = Duration::from_secs(3);
/// Consecutive times the console may forget a job we started before the call gives up.
const REISSUE_LIMIT: u32 = 3;
/// One status or cancel call may take this long (they answer in milliseconds).
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// The console-side job id of operation `op_id`: a nonce in the high 8 bytes, `op_id`
/// (little endian) in the low 8.
pub fn op_job_id(op_id: u64, nonce: [u8; 8]) -> [u8; 16] {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&nonce);
    id[8..].copy_from_slice(&op_id.to_le_bytes());
    id
}

/// The `op_id` a job id carries.
pub fn op_id_of(job: &[u8; 16]) -> u64 {
    u64::from_le_bytes(job[8..].try_into().expect("8 bytes"))
}

/// What the polling loop last saw of the job (`listed` is false until the console answered `job.run`).
#[derive(Default)]
struct Shared {
    listed: bool,
    last: JobProgress,
}

struct Entry {
    host: String,
    job_id: [u8; 16],
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static R: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// Removes the registry entry on every exit: success, error, timeout, panic.
struct Registered(u64);

impl Drop for Registered {
    fn drop(&mut self) {
        if self.0 != 0 {
            registry()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
        }
    }
}

fn register(op_id: u64, e: Entry) -> Result<Registered> {
    if op_id == 0 {
        return Ok(Registered(0));
    }
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if r.contains_key(&op_id) {
        return Err(anyhow!(
            "op_id {op_id} is already running; an operation needs its own op id"
        ));
    }
    r.insert(op_id, e);
    Ok(Registered(op_id))
}

/// `(console host, job id)` of the operation running under `op_id`, if this process runs it.
pub fn lookup(op_id: u64) -> Option<(String, [u8; 16])> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&op_id)
        .map(|e| (e.host.clone(), e.job_id))
}

/// A job the console ran and ended in failure, as the error callers know.
fn failed(label: &str, st: &Status) -> anyhow::Error {
    let code = st.code.unwrap_or(gen::ERR_INTERNAL);
    let cause = st
        .current
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| conv::default_cause(code).to_string());
    MgmtError {
        label: label.to_string(),
        status: code,
        cause,
    }
    .into()
}

/// May a job the console has forgotten be started again? A backup snapshot makes a new
/// snapshot each time; every other operation gives the same outcome when repeated.
fn reissuable(op: JobOp) -> bool {
    op.id != gen::JOB_OP_BACKUP_SNAPSHOT
}

impl AvaTransport {
    /// Runs `op` as a job on `console` and waits for its result (the handler's reply body).
    pub(crate) async fn run_job_async(
        &self,
        console: &str,
        op: JobOp,
        label: &str,
        body: &[u8],
        call: &JobCall<'_>,
    ) -> Result<Option<Vec<u8>>> {
        let nonce = ava1::keys::random_bytes::<8>()?;
        let job_id = op_job_id(call.op_id, nonce);
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            listed: false,
            last: JobProgress {
                kind: op.kind.to_string(),
                subject: call.subject.to_string(),
                ..JobProgress::default()
            },
        }));
        let _registered = register(
            call.op_id,
            Entry {
                host: host_of(console),
                job_id,
                cancel: cancel.clone(),
                shared: shared.clone(),
            },
        )?;
        let run = JobRun {
            job_id,
            op: op.id,
            args: body.to_vec(),
        }
        .to_bytes()?;
        let by_id = JobRef { job_id }.to_bytes()?;
        let deadline = Instant::now() + call.deadline;
        let (mut issued, mut reissues, mut polls) = (false, 0u32, 0u32);
        let mut backoff = Duration::from_millis(250);
        loop {
            let now = Instant::now();
            if now >= deadline {
                // Give up cleanly: the console would otherwise keep deleting or hashing for
                // nobody. Best effort; a job it no longer has is fine.
                if issued {
                    let _ = self
                        .rpc(console, gen::METHOD_JOB_CANCEL, label, &by_id, RPC_TIMEOUT)
                        .await;
                }
                return Err(anyhow!(
                    "{label}: no result after {:?}; the operation may still be running on the console (job {})",
                    call.deadline,
                    hex(&job_id)
                ));
            }
            let (method, req) = if issued {
                (gen::METHOD_JOB_STATUS, &by_id)
            } else {
                (gen::METHOD_JOB_RUN, &run)
            };
            let wait = (deadline - now).min(RPC_TIMEOUT);
            match self.rpc(console, method, label, req, wait).await {
                Ok(reply) => {
                    let st = Status::decode(&reply)?;
                    issued = true;
                    backoff = Duration::from_millis(250);
                    polls += 1;
                    {
                        let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                        sh.listed = true;
                        sh.last = JobProgress {
                            kind: op.kind.to_string(),
                            subject: call.subject.to_string(),
                            files_done: st.files_done as u64,
                            files_total: st.files_total as u64,
                            bytes_done: st.bytes_durable,
                            bytes_total: st.bytes_total,
                            cancel_requested: cancel.load(Ordering::Relaxed),
                        };
                    }
                    match st.state {
                        Some(1) => return Ok(Some(st.result.unwrap_or_default())),
                        Some(0) | None => {}
                        Some(_) => return Err(failed(label, &st)),
                    }
                }
                Err(e) => match e.downcast_ref::<MgmtError>() {
                    // The console forgot the job (it restarted, or the job aged out): ask
                    // again, which is safe for every operation but a snapshot.
                    Some(m) if m.status == gen::ERR_UNKNOWN_JOB && issued => {
                        reissues += 1;
                        if reissues > REISSUE_LIMIT || !reissuable(op) {
                            return Err(MgmtError {
                                label: label.to_string(),
                                status: gen::ERR_UNKNOWN_JOB,
                                cause: "job_lost".into(),
                            }
                            .into());
                        }
                        issued = false;
                        continue;
                    }
                    // A refusal (busy after its retries, a bad request, not paired).
                    Some(_) => return Err(e),
                    // The connection was lost or the call timed out: the job runs on. Ask
                    // again after a pause. A closed session is replaced by the pool itself; a
                    // live one is left alone, because it is shared with every upload and a
                    // management timeout is not evidence it is broken (final review #4:
                    // forgetting it made the next call's handshake end the uploads' session).
                    None => {
                        tokio::time::sleep(
                            backoff.min(deadline.saturating_duration_since(Instant::now())),
                        )
                        .await;
                        backoff = (backoff * 2).min(Duration::from_secs(2));
                        continue;
                    }
                },
            }
            let pause = EARLY_POLLS
                .get(polls.saturating_sub(1) as usize)
                .copied()
                .unwrap_or(POLL);
            tokio::time::sleep(pause.min(deadline.saturating_duration_since(Instant::now()))).await;
        }
    }

    /// The progress of the operation running under `op_id`; `None` when this process runs none.
    /// It is what the polling loop last saw (no extra call, so it never takes a slot or races the
    /// job's release). A query that arrives before the console answered `job.run` waits for it
    /// rather than reporting zero progress for a job that is simply not listed yet.
    pub(crate) async fn job_progress_async(
        &self,
        console: &str,
        op_id: u64,
    ) -> Result<Option<JobProgress>> {
        let (shared, cancel) = {
            let r = registry().lock().unwrap_or_else(|e| e.into_inner());
            match r.get(&op_id) {
                Some(e) if e.host == host_of(console) => (e.shared.clone(), e.cancel.clone()),
                _ => return Ok(None),
            }
        };
        let until = Instant::now() + LISTED_WAIT;
        loop {
            {
                let sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                if sh.listed || Instant::now() >= until {
                    let mut p = sh.last.clone();
                    p.cancel_requested = cancel.load(Ordering::Relaxed);
                    return Ok(Some(p));
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Asks the operation under `op_id` to stop. `false` when this process runs none.
    pub(crate) async fn job_cancel_async(&self, console: &str, op_id: u64) -> Result<bool> {
        let (job_id, cancel) = {
            let r = registry().lock().unwrap_or_else(|e| e.into_inner());
            match r.get(&op_id) {
                Some(e) if e.host == host_of(console) => (e.job_id, e.cancel.clone()),
                _ => return Ok(false),
            }
        };
        cancel.store(true, Ordering::Relaxed);
        let by_id = JobRef { job_id }.to_bytes()?;
        match self
            .rpc(
                console,
                gen::METHOD_JOB_CANCEL,
                "FS_OP_CANCEL",
                &by_id,
                RPC_TIMEOUT,
            )
            .await
        {
            Ok(_) => Ok(true),
            // The job is gone on the console (finished, or not yet started): the flag is set
            // and the polling loop sees how it ended.
            Err(e)
                if e.downcast_ref::<MgmtError>()
                    .is_some_and(|m| m.status == gen::ERR_UNKNOWN_JOB) =>
            {
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }
}

/// Whether `e` is the transport losing the connection (test helper for callers).
#[allow(dead_code)]
pub(crate) fn is_lost(e: &Ava1Error) -> bool {
    matches!(e, Ava1Error::Lost(_) | Ava1Error::Closed | Ava1Error::Io(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_ids_map_to_job_ids_both_ways() {
        let id = op_job_id(0x1122_3344_5566_7788, [9; 8]);
        assert_eq!(op_id_of(&id), 0x1122_3344_5566_7788);
        assert_eq!(&id[..8], &[9; 8]);
        assert_ne!(op_job_id(5, [1; 8]), op_job_id(5, [2; 8]));
        assert_eq!(op_id_of(&op_job_id(0, [3; 8])), 0);
        assert_eq!(op_id_of(&op_job_id(u64::MAX, [3; 8])), u64::MAX);
    }

    #[test]
    fn a_registered_call_is_found_by_its_op_id_and_forgotten_on_exit() {
        let r = register(
            424_242,
            Entry {
                host: "10.0.0.1".into(),
                job_id: op_job_id(424_242, [1; 8]),
                cancel: Arc::default(),
                shared: Arc::default(),
            },
        )
        .unwrap();
        assert_eq!(lookup(424_242).unwrap().1[8..], 424_242u64.to_le_bytes());
        // The same op id twice is refused, not silently shared.
        assert!(register(
            424_242,
            Entry {
                host: "x".into(),
                job_id: [0; 16],
                cancel: Arc::default(),
                shared: Arc::default()
            }
        )
        .is_err());
        drop(r);
        assert!(lookup(424_242).is_none());
        // op id 0 is never registered.
        let z = register(
            0,
            Entry {
                host: "x".into(),
                job_id: [0; 16],
                cancel: Arc::default(),
                shared: Arc::default(),
            },
        )
        .unwrap();
        assert!(lookup(0).is_none());
        drop(z);
    }

    #[test]
    fn only_a_snapshot_is_not_repeatable() {
        for op in ps5upload_core::mgmt::ops::ALL {
            assert_eq!(
                reissuable(*op),
                op.id != gen::JOB_OP_BACKUP_SNAPSHOT,
                "{}",
                op.label
            );
        }
    }
}
