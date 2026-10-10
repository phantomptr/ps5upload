//! Console to console (#433, SPEC.md §18): straight between the two consoles when they can
//! reach each other, else through this computer (the relay). The receiving console hands out a
//! ticket for the sending console's key (c2c.allow); the sending console dials it with that
//! ticket and pushes the job (c2c.send). The job on the receiving console is the engine's own,
//! with the same id as a relay would use, so either route resumes what the other left.
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ava1::gen::{self, C2cAllow, C2cSend, C2cTicket, JobRef, Status};
use ava1::send::{Progress, SendReport};
use ava1::wire::Message;

use crate::pool::{pool, Pool};

/// How a console-to-console job travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Straight between the consoles.
    Direct,
    /// Through this computer, and why not straight.
    Relay { reason: String },
}

/// The sending console's dial takes a TCP connect and a handshake per connection, and it walks
/// the source before it answers.
const SEND_TIMEOUT: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(500);
/// No byte acknowledged for this long ends the direct send; the relay picks it up.
const DIRECT_STALL: Duration = Duration::from_secs(120);
/// Status calls that keep failing for this long end it the same way.
const STATUS_GIVE_UP: Duration = Duration::from_secs(30);
/// The data lanes the sending console opens to the other.
const LANES: u8 = 4;

static TEST_PORT: AtomicU16 = AtomicU16::new(0);

/// Test knob: the sending console is told this port for the other console (a closed one
/// makes the direct route fail, so the fallback runs). 0 clears it.
#[doc(hidden)]
pub fn set_dial_port_for_tests(port: u16) {
    TEST_PORT.store(port, Ordering::Relaxed);
}

enum Direct {
    Done(SendReport),
    Fallback(String),
    Cancelled,
    /// The job failed for a reason the relay would meet too (no space, a bad path).
    Failed(String),
}

/// Copies `src` on `from` to `dest` on `to`: directly when it can, else through this computer.
/// `on_route` hears the route as soon as it is known (and again if it changes).
#[allow(clippy::too_many_arguments)]
pub fn ps5_to_ps5_routed(
    from: &str,
    src: &str,
    to: &str,
    dest: &str,
    job_id: [u8; 16],
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    on_route: impl Fn(&Route),
) -> Result<(SendReport, Route)> {
    ps5_to_ps5_routed_between(
        pool(),
        from,
        src,
        pool(),
        to,
        dest,
        job_id,
        progress,
        cancel,
        on_route,
    )
}

/// Pool injection for local two-console tests; the same body as production.
#[allow(clippy::too_many_arguments)]
pub fn ps5_to_ps5_routed_between(
    from_pool: &Pool,
    from: &str,
    src: &str,
    to_pool: &Pool,
    to: &str,
    dest: &str,
    job_id: [u8; 16],
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    on_route: impl Fn(&Route),
) -> Result<(SendReport, Route)> {
    let direct = crate::block_on(direct(
        from_pool, from, src, to_pool, to, dest, job_id, &progress, &cancel, &on_route,
    ));
    let reason = match direct {
        Direct::Done(r) => return Ok((r, Route::Direct)),
        Direct::Cancelled => return Err(anyhow!("transfer_cancelled")),
        Direct::Failed(why) => return Err(anyhow!("{why}")),
        Direct::Fallback(reason) => reason,
    };
    let route = Route::Relay { reason };
    on_route(&route);
    let r = crate::relay::ps5_to_ps5_between(
        from_pool, from, src, to_pool, to, dest, job_id, progress, cancel,
    )?;
    Ok((r, route))
}

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).trim().to_string()
}

/// The first IPv4 address of `addr` (`host:port`), as the sending console must dial it.
fn ipv4_of(addr: &str) -> Option<(String, u16)> {
    addr.to_socket_addrs()
        .ok()?
        .find(|a| a.is_ipv4())
        .map(|a| (a.ip().to_string(), a.port()))
}

/// Codes a relay would meet as well: the job ends instead of falling back.
fn terminal(code: u16) -> bool {
    matches!(
        code,
        gen::ERR_NO_SPACE | gen::ERR_PATH | gen::ERR_EXISTS | gen::ERR_CROSS_DEVICE
    )
}

/// Best effort: stops the sending console's direct job (its watcher then cancels it on the other
/// console too). A job that is gone already is fine.
async fn cancel_on(pool: &Pool, console: &str, job_id: [u8; 16]) {
    if let (Ok(s), Ok(body)) = (pool.session(console).await, JobRef { job_id }.to_bytes()) {
        let _ = s.rpc(gen::METHOD_JOB_CANCEL, &body).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn direct(
    from_pool: &Pool,
    from: &str,
    src: &str,
    to_pool: &Pool,
    to: &str,
    dest: &str,
    job_id: [u8; 16],
    progress: &Progress,
    cancel: &AtomicBool,
    on_route: &impl Fn(&Route),
) -> Direct {
    let sa = match from_pool.session(from).await {
        Ok(s) => s,
        Err(e) => return Direct::Fallback(format!("the sending console did not answer: {e}")),
    };
    let sb = match to_pool.session(to).await {
        Ok(s) => s,
        Err(e) => return Direct::Fallback(format!("the receiving console did not answer: {e}")),
    };
    let allow = C2cAllow {
        job_id,
        key: sa.peer_key(),
        root: dest.into(),
    };
    let Ok(body) = allow.to_bytes() else {
        return Direct::Fallback("the destination path is too long for a direct send".into());
    };
    let token = match sb.rpc(gen::METHOD_C2C_ALLOW, &body).await {
        Ok(r) if r.status == gen::STATUS_OK => match C2cTicket::decode(&r.body) {
            Ok(t) => t.token,
            Err(e) => return Direct::Fallback(format!("the receiving console's ticket: {e}")),
        },
        Ok(r) if r.status == gen::ERR_UNKNOWN_METHOD => {
            return Direct::Fallback("the receiving console's helper is older than 6.8".into())
        }
        Ok(r) => {
            return Direct::Fallback(format!(
                "the receiving console refused a direct send: {}",
                text(&r.body)
            ))
        }
        Err(e) => return Direct::Fallback(format!("the receiving console did not answer: {e}")),
    };
    let Some((host, mut port)) = ipv4_of(&to_pool.addr_for(to)) else {
        return Direct::Fallback("the receiving console has no IPv4 address to dial".into());
    };
    let test_port = TEST_PORT.load(Ordering::Relaxed);
    if test_port != 0 {
        port = test_port;
    }
    let send = C2cSend {
        job_id,
        host: host.clone(),
        port,
        key: sb.peer_key(),
        token,
        src: src.into(),
        dest: dest.into(),
        flags: 0,
    };
    let Ok(body) = send.to_bytes() else {
        return Direct::Fallback("the paths are too long for a direct send".into());
    };
    match sa
        .rpc_within(gen::METHOD_C2C_SEND, &body, SEND_TIMEOUT)
        .await
    {
        Ok(r) if r.status == gen::STATUS_OK => {}
        Ok(r) if r.status == gen::ERR_UNKNOWN_METHOD => {
            return Direct::Fallback("the sending console's helper is older than 6.8".into())
        }
        Ok(r) if terminal(r.status) => return Direct::Failed(text(&r.body)),
        Ok(r) => {
            return Direct::Fallback(format!(
                "the consoles could not connect to each other ({})",
                text(&r.body)
            ))
        }
        Err(e) => {
            // It may have started anyway: the relay must not run beside it.
            cancel_on(from_pool, from, job_id).await;
            return Direct::Fallback(format!("the sending console did not answer: {e}"));
        }
    }
    on_route(&Route::Direct);
    let reference = JobRef { job_id }.to_bytes().unwrap_or_default();
    let (mut last_bytes, mut last_move) = (0u64, Instant::now());
    let mut status_ok_at = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancel_on(from_pool, from, job_id).await;
            return Direct::Cancelled;
        }
        tokio::time::sleep(POLL).await;
        let reply = match from_pool.session(from).await {
            Ok(s) => s.rpc(gen::METHOD_JOB_STATUS, &reference).await,
            Err(e) => Err(e),
        };
        let st = match reply {
            Ok(r) if r.status == gen::STATUS_OK => match Status::decode(&r.body) {
                Ok(st) => st,
                Err(_) => continue,
            },
            Ok(r) if r.status == gen::ERR_UNKNOWN_JOB => {
                return Direct::Fallback("the sending console lost the direct send".into())
            }
            _ => {
                if status_ok_at.elapsed() > STATUS_GIVE_UP {
                    cancel_on(from_pool, from, job_id).await;
                    return Direct::Fallback("the sending console stopped answering".into());
                }
                continue;
            }
        };
        status_ok_at = Instant::now();
        progress
            .bytes_total
            .store(st.bytes_total, Ordering::Relaxed);
        progress
            .files_total
            .store(st.files_total as u64, Ordering::Relaxed);
        progress
            .bytes_sent
            .store(st.bytes_received.min(st.bytes_total), Ordering::Relaxed);
        progress.lanes.store(LANES, Ordering::Relaxed);
        if st.bytes_received != last_bytes {
            last_bytes = st.bytes_received;
            last_move = Instant::now();
        }
        match st.state {
            Some(1) => {
                progress.bytes_sent.store(st.bytes_total, Ordering::Relaxed);
                progress
                    .bytes_durable
                    .store(st.bytes_durable.max(st.bytes_total), Ordering::Relaxed);
                progress
                    .files_durable
                    .store(st.files_done as u64, Ordering::Relaxed);
                return Direct::Done(SendReport {
                    status: gen::STATUS_OK,
                    message: None,
                    files: st.files_done,
                    bytes: st.bytes_total,
                    resent: 0,
                    max_lanes: LANES,
                    bottleneck: 0,
                    sequential: false,
                });
            }
            Some(0) | None => {}
            Some(_) => {
                let why = st
                    .current
                    .unwrap_or_else(|| "the direct send failed".into());
                return match st.code {
                    Some(c) if c == gen::ERR_CANCELLED && cancel.load(Ordering::Relaxed) => {
                        Direct::Cancelled
                    }
                    Some(c) if terminal(c) => Direct::Failed(why),
                    _ => Direct::Fallback(format!("the direct send stopped: {why}")),
                };
            }
        }
        if last_move.elapsed() > DIRECT_STALL {
            cancel_on(from_pool, from, job_id).await;
            return Direct::Fallback("the direct send stopped moving".into());
        }
    }
}
