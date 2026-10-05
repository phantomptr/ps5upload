//! Console-local copy and move over AVA1 (`job.copy`, `job.status`, `job.cancel`).
//! Blocking: call from `spawn_blocking` (the engine's `ps5_fs_copy` / `ps5_fs_move`).
//!
//! The console runs the job; the engine only asks for it and watches. The rules are the
//! payload's (cca7b988): a job is owned by the device that issued it, so status and
//! cancel from anyone else answer `ERR_UNKNOWN_JOB`; a `job.copy` for an id the console
//! already has (same owner, same parameters) answers with that job instead of starting a
//! second one, which is what makes re-issuing after a lost connection safe; a failed
//! job is retired and restarted by a re-issue; and a move reports "running" until the
//! source has been deleted, so `state = 1` is the only point at which a move is done.
//! The op registry below backs the endpoints the client already polls
//! (`/api/ps5/fs/op-status`, `op-cancel`) for the ids this module owns.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ava1::gen::{self, JobCopy, JobRef, Status};
use ava1::wire::Message;
use ps5upload_core::fs_ops::FsOpSnapshot;

use crate::pool::{pool, Pool};
use crate::upload::{hex, refusal, wait, SessionGate, UploadFailure, STALL_LIMIT};

const POLL: Duration = Duration::from_millis(500);
/// `ERR_BUSY` is "another job holds this destination" or "the failed copy is still
/// closing": the second is momentary, the first is not, so bound the wait.
const BUSY_TRIES: u32 = 20;
/// Consecutive re-issues that find the console no longer knowing the job.
const REISSUE_LIMIT: u32 = 5;

struct Op {
    snap: FsOpSnapshot,
    cancel: Arc<AtomicBool>,
}

fn ops() -> &'static Mutex<HashMap<u64, Op>> {
    static OPS: OnceLock<Mutex<HashMap<u64, Op>>> = OnceLock::new();
    OPS.get_or_init(Mutex::default)
}

/// Removes the registry entry on every exit: success, error, cancel, panic.
struct Registered(u64);

impl Drop for Registered {
    fn drop(&mut self) {
        ops().lock().unwrap().remove(&self.0);
    }
}

fn register(op_id: u64, kind: &str, from: &str, to: &str) -> Result<(Registered, Arc<AtomicBool>)> {
    let mut map = ops().lock().unwrap();
    if map.contains_key(&op_id) {
        return Err(anyhow!(
            "op_id {op_id} is already running; a copy needs its own op id"
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    map.insert(
        op_id,
        Op {
            snap: FsOpSnapshot {
                found: true,
                op_id,
                kind: kind.into(),
                from: from.into(),
                to: to.into(),
                total_bytes: 0,
                bytes_copied: 0,
                cancel_requested: false,
            },
            cancel: cancel.clone(),
        },
    );
    Ok((Registered(op_id), cancel))
}

/// Publishes the console's progress under `op_id` (a no-op for an id this module does
/// not own). Public as the tests' seam.
pub fn record_status(op_id: u64, st: &Status) {
    if let Some(op) = ops().lock().unwrap().get_mut(&op_id) {
        op.snap.total_bytes = st.bytes_total;
        op.snap.bytes_copied = st.bytes_durable;
    }
}

/// The snapshot for an op this module runs; `None` for any other id, so the caller falls
/// through to the management job query (which is how the console's own jobs keep working).
pub fn op_snapshot(op_id: u64) -> Option<FsOpSnapshot> {
    ops().lock().unwrap().get(&op_id).map(|o| o.snap.clone())
}

/// Asks an op this module runs to stop. `false` for an id it does not own.
pub fn op_cancel(op_id: u64) -> bool {
    match ops().lock().unwrap().get_mut(&op_id) {
        Some(op) => {
            op.snap.cancel_requested = true;
            op.cancel.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// A fresh 128-bit nonce for one `console_copy_in` call. The engine's `op_id` is a
/// counter that restarts with the process, so it cannot keep a later run's id apart
/// from an earlier run's finished job still listed on the console.
fn new_nonce() -> Result<[u8; 16]> {
    Ok(ava1::keys::random_bytes::<16>()?)
}

/// The console-side job id. Stable across one call's own retries (a re-issue must find
/// the job it started: the caller draws `nonce` once) and unique per call: it hashes
/// the kind and the overwrite choice, so a copy and a move of one path pair never alias
/// a job directory or a status entry, AND the per-call nonce, so a user who copies the
/// same pair again after deleting the destination (or after an engine restart reset
/// `op_id`) does not get the console's still-listed finished job back as an instant,
/// empty "success".
pub fn copy_job_id(
    nonce: &[u8; 16],
    from: &str,
    to: &str,
    move_source: bool,
    overwrite: bool,
) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(b"ps5upload copy v1\0");
    h.update(nonce);
    h.update(&[u8::from(move_source), u8::from(overwrite)]);
    h.update(&(from.len() as u64).to_le_bytes());
    h.update(from.as_bytes());
    h.update(to.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

/// `JF_OVERWRITE` is sent exactly when the caller asked to overwrite; without it the
/// console refuses an existing destination with `ERR_EXISTS` before writing anything.
fn copy_flags(move_source: bool, overwrite: bool) -> u32 {
    let mut f = 0;
    if move_source {
        f |= gen::JF_MOVE;
    }
    if overwrite {
        f |= gen::JF_OVERWRITE;
    }
    f
}

pub fn console_copy(
    console: &str,
    from: &str,
    to: &str,
    op_id: u64,
    move_source: bool,
    overwrite: bool,
) -> Result<()> {
    console_copy_in(pool(), console, from, to, op_id, move_source, overwrite)
}

/// Sleeps `d`, waking early (and reporting it) when `cancel` is set.
async fn nap(d: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cancel.load(Ordering::Relaxed)
}

/// The longest one copy RPC waits for the console's answer.
const COPY_RPC_TIMEOUT: Duration = Duration::from_secs(30);

enum RpcStop {
    Cancelled,
    Failed(ava1::Ava1Error),
}

/// One RPC that ends on the user's cancel (polled) or after `COPY_RPC_TIMEOUT` instead of
/// waiting on a console that holds the reply for ever.
async fn rpc_cancellable(
    session: &ava1::session::Session,
    method: u16,
    body: &[u8],
    cancel: &AtomicBool,
) -> Result<ava1::session::RpcReply, RpcStop> {
    rpc_cancellable_within(session, method, body, cancel, COPY_RPC_TIMEOUT).await
}

async fn rpc_cancellable_within(
    session: &ava1::session::Session,
    method: u16,
    body: &[u8],
    cancel: &AtomicBool,
    within: Duration,
) -> Result<ava1::session::RpcReply, RpcStop> {
    let call = session.rpc_within(method, body, within);
    let stop = async {
        while !cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::select! {
        r = call => r.map_err(RpcStop::Failed),
        _ = stop => Err(RpcStop::Cancelled),
    }
}

pub fn console_copy_in(
    pool: &Pool,
    console: &str,
    from: &str,
    to: &str,
    op_id: u64,
    move_source: bool,
    overwrite: bool,
) -> Result<()> {
    console_copy_with(
        pool,
        &ConsoleCleanup,
        console,
        from,
        to,
        op_id,
        move_source,
        overwrite,
    )
}

/// What a cancelled copy leaves on the console, and how to remove it (R5, #369).
///
/// A copy to a destination that did not exist is staged: the console makes `<to>` as an empty
/// folder (its lock on the name) and writes into `<to>.ava-part`, renaming that over `<to>`
/// only when every file is durable. `job.cancel` keeps the console's journal (a later resume
/// may use it) but a user who pressed Stop wants the half-copy gone, and the empty `<to>` would
/// make the next try refuse with "already exists". The source is never touched here: only the
/// destination's own leftovers are named.
pub trait CancelCleanup: Sync {
    /// Whether `to` is absent on the console right now (asked before the copy starts, and only
    /// when the caller allowed overwriting: without that flag the console refuses an existing
    /// destination, so the answer is known).
    fn dest_absent(&self, console: &str, to: &str) -> bool;
    /// Removes what the cancelled copy left at `to`. Best effort: a failure is logged, not
    /// returned, because the user's cancel has already happened.
    fn clean(&self, console: &str, to: &str, dest_was_absent: bool);
}

/// The paths a cancelled copy to `to` may remove, in order: its staging sibling, then `to`
/// itself, and `to` only when the copy created it (it was absent at the start) and it is still
/// an empty folder (a finished rename leaves it full, and then it is the user's copy).
pub fn cancel_leftovers(to: &str, dest_was_absent: bool, dest_is_empty_dir: bool) -> Vec<String> {
    let to = to.trim_end_matches('/');
    if to.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("{to}.ava-part")];
    if dest_was_absent && dest_is_empty_dir {
        out.push(to.to_string());
    }
    out
}

struct ConsoleCleanup;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(60);

impl CancelCleanup for ConsoleCleanup {
    fn dest_absent(&self, console: &str, to: &str) -> bool {
        match ps5upload_core::fs_ops::fs_stat(console, to) {
            Ok(_) => false,
            Err(e) => ps5upload_core::fs_ops::is_not_found(&format!("{e:#}")),
        }
    }

    fn clean(&self, console: &str, to: &str, dest_was_absent: bool) {
        use ps5upload_core::fs_ops::{
            fs_delete_with_timeout, list_dir_with_timeout, ListDirOptions,
        };
        let empty = dest_was_absent
            && list_dir_with_timeout(
                console,
                to,
                ListDirOptions::default(),
                Some(CLEANUP_TIMEOUT),
            )
            .map(|l| l.entries.is_empty() && !l.truncated)
            .unwrap_or(false);
        for p in cancel_leftovers(to, dest_was_absent, empty) {
            // The staging sibling is usually absent for a copy cancelled before its first
            // byte, and the engine has no logger here (a write to a dead stderr panics): a
            // failure leaves the console's own 7-day sweep to collect the leftovers.
            let _ = fs_delete_with_timeout(console, &p, Some(CLEANUP_TIMEOUT));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn console_copy_with(
    pool: &Pool,
    cleanup: &dyn CancelCleanup,
    console: &str,
    from: &str,
    to: &str,
    op_id: u64,
    move_source: bool,
    overwrite: bool,
) -> Result<()> {
    let dest_was_absent = !overwrite || cleanup.dest_absent(console, to);
    let r = copy_job(pool, console, from, to, op_id, move_source, overwrite);
    if matches!(&r, Err(e) if e.to_string() == "cancelled") {
        // Cancelled, and the console has been told to stop (and waited on, below).
        cleanup.clean(console, to, dest_was_absent);
    }
    r
}

fn copy_job(
    pool: &Pool,
    console: &str,
    from: &str,
    to: &str,
    op_id: u64,
    move_source: bool,
    overwrite: bool,
) -> Result<()> {
    let kind = if move_source { "move" } else { "copy" };
    let (_registered, cancel) = register(op_id, kind, from, to)?;
    // Once per call: the retries below re-issue this same id.
    let id = copy_job_id(&new_nonce()?, from, to, move_source, overwrite);
    let flags = copy_flags(move_source, overwrite);
    let issue = JobCopy {
        job_id: id,
        src: from.into(),
        dest: to.into(),
        flags,
    }
    .to_bytes()?;
    let jobref = JobRef { job_id: id }.to_bytes()?;
    crate::block_on(async {
        SessionGate::identity(pool)?;
        let mut backoff = Duration::from_millis(250);
        let mut gate = SessionGate::default();
        // Whether the console is known to hold the job for this session. Cleared by any
        // transport loss and by ERR_UNKNOWN_JOB (the console restarted or reaped it):
        // issuing again is idempotent, and resumes from the console's own journal.
        let mut issued = false;
        let (mut busy, mut reissues) = (0u32, 0u32);
        let (mut last_at, mut last_durable) = (Instant::now(), 0u64);
        loop {
            if cancel.load(Ordering::Relaxed) {
                // Best effort: the user's stop must not wait on a console that is gone.
                if let Ok(Ok(s)) =
                    tokio::time::timeout(Duration::from_secs(5), pool.session(console)).await
                {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        s.rpc(gen::METHOD_JOB_CANCEL, &jobref),
                    )
                    .await;
                    // The console unlists the job and stops its threads; clearing its leftovers
                    // while they still write would race them. Wait (bounded) until it says it
                    // no longer knows the job.
                    for _ in 0..20 {
                        match tokio::time::timeout(
                            Duration::from_secs(2),
                            s.rpc(gen::METHOD_JOB_STATUS, &jobref),
                        )
                        .await
                        {
                            Ok(Ok(r)) if r.status == gen::ERR_UNKNOWN_JOB => break,
                            Ok(Ok(_)) => {}
                            _ => break,
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
                return Err(anyhow!("cancelled"));
            }
            let session = match pool.session(console).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate.failed(&e) {
                        return Err(failure.into());
                    }
                    issued = false;
                    wait(&mut backoff, &e.to_string()).await;
                    continue;
                }
            };
            gate.connected();
            let (method, body) = if issued {
                (gen::METHOD_JOB_STATUS, &jobref)
            } else {
                (gen::METHOD_JOB_COPY, &issue)
            };
            let reply = match rpc_cancellable(&session, method, body, &cancel).await {
                // The user's stop while a call hangs: the top of the loop cancels the job.
                Err(RpcStop::Cancelled) => continue,
                Ok(r) => r,
                Err(RpcStop::Failed(e)) => {
                    pool.forget(console).await;
                    // The console held the job and the link dropped: it is back (or about to
                    // be), so the first retry is prompt rather than the top of the ladder.
                    if issued {
                        backoff = crate::upload::BACKOFF_FLOOR;
                    }
                    issued = false;
                    wait(&mut backoff, &e.to_string()).await;
                    continue;
                }
            };
            match reply.status {
                gen::STATUS_OK => {}
                gen::ERR_UNKNOWN_JOB if issued => {
                    reissues += 1;
                    if reissues > REISSUE_LIMIT {
                        return Err(UploadFailure {
                            reason: "ava1_copy_lost".into(),
                            detail: "the console keeps forgetting this copy".into(),
                        }
                        .into());
                    }
                    issued = false;
                    continue;
                }
                gen::ERR_BUSY if !issued && busy < BUSY_TRIES => {
                    busy += 1;
                    nap(POLL, &cancel).await;
                    continue;
                }
                gen::ERR_EXISTS => {
                    // The client keys its "already there" prompts on this token (the
                    // payload's own word for it).
                    return Err(UploadFailure {
                        reason: "ava1_exists".into(),
                        detail: format!("fs_copy_dest_exists: {to} already exists"),
                    }
                    .into());
                }
                status => {
                    return Err(refusal(status, format!("{kind} {from} -> {to}")).into());
                }
            }
            let st = Status::decode(&reply.body)?;
            record_status(op_id, &st);
            if !issued {
                issued = true;
                backoff = Duration::from_millis(250);
            }
            match st.state {
                Some(1) => return Ok(()),
                Some(0) | None => {}
                Some(_) => {
                    // The console ran the job and it failed; it is terminal. A re-issue
                    // would retire and restart it, which is the user's decision.
                    let why = st
                        .current
                        .clone()
                        .unwrap_or_else(|| "the console reported a failure".into());
                    return Err(UploadFailure {
                        reason: "ava1_copy_failed".into(),
                        detail: format!("the console could not {kind} {from} to {to}: {why}"),
                    }
                    .into());
                }
            }
            // A stalled copy ends the job. Once every byte is durable the console may
            // still be renaming or (for a move) deleting a large tree, which moves no
            // bytes, so the stall clock only runs while bytes are outstanding.
            if st.bytes_durable > last_durable || st.bytes_durable >= st.bytes_total {
                (last_at, last_durable) = (Instant::now(), st.bytes_durable);
            } else if last_at.elapsed() > STALL_LIMIT {
                return Err(anyhow!(
                    "no copy progress for {STALL_LIMIT:?}; the job {} is still on the console",
                    hex(&id)
                ));
            }
            nap(POLL, &cancel).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_ids_separate_kind_overwrite_and_operation() {
        let n = [1u8; 16];
        let base = copy_job_id(&n, "/a", "/b", false, false);
        assert_eq!(base, copy_job_id(&n, "/a", "/b", false, false));
        assert_ne!(base, copy_job_id(&n, "/a", "/b", true, false));
        assert_ne!(base, copy_job_id(&n, "/a", "/b", false, true));
        assert_ne!(base, copy_job_id(&[2u8; 16], "/a", "/b", false, false));
        assert_ne!(base, copy_job_id(&n, "/b", "/a", false, false));
        // The length prefix keeps ("/a","b/c") and ("/ab","/c") apart.
        assert_ne!(
            copy_job_id(&n, "/a", "b/c", false, false),
            copy_job_id(&n, "/ab", "/c", false, false)
        );
    }

    // Two calls with the same operation id and paths must not share a console job.
    #[test]
    fn two_calls_with_the_same_op_and_paths_get_different_job_ids() {
        let a = copy_job_id(&new_nonce().unwrap(), "/a", "/b", false, false);
        let b = copy_job_id(&new_nonce().unwrap(), "/a", "/b", false, false);
        assert_ne!(a, b);
    }

    #[test]
    fn a_cancelled_copy_removes_its_staging_and_only_an_empty_folder_it_made() {
        // Created by the copy and still empty: both go (the next try must not hit "exists").
        assert_eq!(
            cancel_leftovers("/data/g", true, true),
            ["/data/g.ava-part", "/data/g"]
        );
        // Created by the copy but the rename already landed (the folder is full): keep it.
        assert_eq!(
            cancel_leftovers("/data/g", true, false),
            ["/data/g.ava-part"]
        );
        // The destination was the user's before the copy began: never remove it.
        assert_eq!(
            cancel_leftovers("/data/g", false, true),
            ["/data/g.ava-part"]
        );
        assert_eq!(
            cancel_leftovers("/data/g/", false, false),
            ["/data/g.ava-part"]
        );
        // No path at all names nothing (never `.ava-part` at the root).
        assert!(cancel_leftovers("", true, true).is_empty());
        assert!(cancel_leftovers("/", true, true).is_empty());
    }

    #[test]
    fn overwrite_and_move_map_to_their_wire_flags() {
        assert_eq!(copy_flags(false, false), 0);
        assert_eq!(copy_flags(false, true), gen::JF_OVERWRITE);
        assert_eq!(copy_flags(true, false), gen::JF_MOVE);
        assert_eq!(copy_flags(true, true), gen::JF_MOVE | gen::JF_OVERWRITE);
    }
}
