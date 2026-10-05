//! Management calls over loopback HTTP: the wire shared by the engine's `POST /api/mgmt/call`
//! route and the desktop shell's forwarding transport.
//!
//! The desktop process cannot hold its own AVA1 console session next to the sidecar engine's
//! (the console keeps one session per identity, so the two would evict each other). Its
//! management calls therefore go to the engine, which runs them over the session it already
//! has. This module is the request/response shape and the two conversions that keep errors
//! intact across the hop: [`execute`] (engine side: run a request on the registered transport)
//! and [`ProxyResponse::into_result`] (caller side: rebuild the same `MgmtError`/anyhow error).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, JobCall, JobProgress, Method, MgmtError, MgmtTransport};

/// The route path on the engine.
pub const ROUTE: &str = "/api/mgmt/call";

/// The call never reached the console through the engine: the engine was unreachable, refused
/// the hop (a 403, a non-success status) or its reply could not be decoded. Distinct from
/// [`MgmtError`] (the console answered) and from a dropped console connection, so that a
/// Reboot/Shutdown/Standby does not report success for a command nobody ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardError(pub String);

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ForwardError {}

/// What the caller asks the engine to run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProxyRequest {
    Call {
        addr: String,
        method: u16,
        label: String,
        body_b64: String,
        timeout_ms: u64,
    },
    RunJob {
        addr: String,
        job_op: u8,
        label: String,
        body_b64: String,
        op_id: u64,
        subject: String,
        deadline_ms: u64,
    },
    JobProgress {
        addr: String,
        op_id: u64,
    },
    JobCancel {
        addr: String,
        op_id: u64,
    },
}

impl ProxyRequest {
    /// How long the engine may take to answer: the caller's own bound.
    pub fn bound(&self) -> Duration {
        match self {
            Self::Call { timeout_ms, .. } => Duration::from_millis(*timeout_ms),
            Self::RunJob { deadline_ms, .. } => Duration::from_millis(*deadline_ms),
            _ => mgmt::DEFAULT_TIMEOUT,
        }
    }
}

/// A successful answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyResult {
    Bytes { body_b64: String },
    Progress { progress: Option<JobProgress> },
    Cancel { found: bool },
}

/// A failure, typed so the caller can rebuild the original error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyError {
    /// The payload (or the transport) refused: `payload rejected <label>: <cause>`.
    Mgmt {
        label: String,
        status: u16,
        cause: String,
    },
    /// Anything else, as the engine's error chain text.
    Other { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ProxyResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProxyError>,
}

impl ProxyResponse {
    fn good(r: ProxyResult) -> Self {
        Self {
            ok: true,
            result: Some(r),
            error: None,
        }
    }

    fn bad(e: &anyhow::Error) -> Self {
        let error = match e.downcast_ref::<MgmtError>() {
            Some(m) => ProxyError::Mgmt {
                label: m.label.clone(),
                status: m.status,
                cause: m.cause.clone(),
            },
            None => ProxyError::Other {
                message: format!("{e:#}"),
            },
        };
        Self {
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// The caller's view: the same `Result` the in-process transport would have produced
    /// (`Ok(Some(..))`; a refusal is the same `MgmtError`).
    pub fn into_result(self) -> Result<ProxyResult> {
        if self.ok {
            return self
                .result
                .ok_or_else(|| anyhow!("management proxy answered ok with no result"));
        }
        match self.error {
            Some(ProxyError::Mgmt {
                label,
                status,
                cause,
            }) => Err(MgmtError {
                label,
                status,
                cause,
            }
            .into()),
            Some(ProxyError::Other { message }) => Err(anyhow!(message)),
            None => Err(anyhow!("management proxy answered not-ok with no error")),
        }
    }
}

pub fn encode_body(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub fn decode_body(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| anyhow!("bad base64 body: {e}"))
}

fn find_method(id: u16) -> Result<Method> {
    mgmt::m::ALL
        .iter()
        .copied()
        .find(|m| m.id == id)
        .ok_or_else(|| anyhow!("unknown management method {id}"))
}

fn find_job_op(id: u8) -> Result<mgmt::JobOp> {
    mgmt::ops::ALL
        .iter()
        .copied()
        .find(|o| o.id == id)
        .ok_or_else(|| anyhow!("unknown job op {id}"))
}

/// Engine side: runs `req` on the process's registered transport. Blocking (the transport
/// blocks): call from `spawn_blocking`.
pub fn execute(req: ProxyRequest) -> ProxyResponse {
    match run(req) {
        Ok(r) => ProxyResponse::good(r),
        Err(e) => ProxyResponse::bad(&e),
    }
}

/// [`execute`] over an explicit transport (a test seam: it is used for this thread's calls only).
pub fn execute_with(t: Arc<dyn MgmtTransport>, req: ProxyRequest) -> ProxyResponse {
    let _g = mgmt::scoped_transport(t);
    execute(req)
}

fn run(req: ProxyRequest) -> Result<ProxyResult> {
    match req {
        ProxyRequest::Call {
            addr,
            method,
            label,
            body_b64,
            timeout_ms,
        } => {
            let body = decode_body(&body_b64)?;
            let reply = mgmt::call_with(
                &addr,
                find_method(method)?,
                &label,
                &body,
                Some(Duration::from_millis(timeout_ms)),
            )?;
            Ok(ProxyResult::Bytes {
                body_b64: encode_body(&reply),
            })
        }
        ProxyRequest::RunJob {
            addr,
            job_op,
            label,
            body_b64,
            op_id,
            subject,
            deadline_ms,
        } => {
            let body = decode_body(&body_b64)?;
            let reply = mgmt::run_op(
                &addr,
                find_job_op(job_op)?,
                &label,
                &body,
                &JobCall {
                    op_id,
                    subject: &subject,
                    deadline: Duration::from_millis(deadline_ms),
                },
            )?;
            Ok(ProxyResult::Bytes {
                body_b64: encode_body(&reply),
            })
        }
        ProxyRequest::JobProgress { addr, op_id } => {
            let p = mgmt::op_progress(&addr, op_id)?
                .ok_or_else(|| mgmt::helper_not_ava1("JOB_STATUS"))?;
            Ok(ProxyResult::Progress { progress: p })
        }
        ProxyRequest::JobCancel { addr, op_id } => {
            let f = mgmt::op_cancel(&addr, op_id)?
                .ok_or_else(|| mgmt::helper_not_ava1("JOB_CANCEL"))?;
            Ok(ProxyResult::Cancel { found: f })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl MgmtTransport for Echo {
        fn call(
            &self,
            _a: &str,
            m: Method,
            _l: &str,
            body: &[u8],
            t: Duration,
        ) -> Result<Option<Vec<u8>>> {
            if m.id == mgmt::m::HW_INFO.id {
                return Ok(Some(
                    [body, b"|", t.as_millis().to_string().as_bytes()].concat(),
                ));
            }
            Err(MgmtError {
                label: "X".into(),
                status: 7,
                cause: "no".into(),
            }
            .into())
        }
    }

    fn call_req(method: u16) -> ProxyRequest {
        ProxyRequest::Call {
            addr: "c".into(),
            method,
            label: "L".into(),
            body_b64: encode_body(b"hi"),
            timeout_ms: 90_000,
        }
    }

    #[test]
    fn a_call_keeps_its_body_and_timeout() {
        let r = execute_with(Arc::new(Echo), call_req(mgmt::m::HW_INFO.id));
        let json = serde_json::to_string(&r).unwrap();
        let r: ProxyResponse = serde_json::from_str(&json).unwrap();
        match r.into_result().unwrap() {
            ProxyResult::Bytes { body_b64 } => {
                assert_eq!(decode_body(&body_b64).unwrap(), b"hi|90000")
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_refusal_rebuilds_the_same_mgmt_error() {
        let r = execute_with(Arc::new(Echo), call_req(mgmt::m::FS_STAT.id));
        let e = r.into_result().unwrap_err();
        let m = e.downcast_ref::<MgmtError>().unwrap();
        assert_eq!(
            (m.label.as_str(), m.status, m.cause.as_str()),
            ("X", 7, "no")
        );
    }

    #[test]
    fn a_transport_that_does_not_serve_is_helper_not_ava1() {
        struct None_;
        impl MgmtTransport for None_ {
            fn call(
                &self,
                _: &str,
                _: Method,
                _: &str,
                _: &[u8],
                _: Duration,
            ) -> Result<Option<Vec<u8>>> {
                Ok(None)
            }
        }
        let e = execute_with(Arc::new(None_), call_req(mgmt::m::HW_INFO.id))
            .into_result()
            .unwrap_err();
        assert!(e.to_string().contains("helper_not_ava1"), "{e}");
        assert!(e.downcast_ref::<MgmtError>().is_some());
    }

    #[test]
    fn an_unknown_method_is_a_plain_error() {
        let e = execute_with(Arc::new(Echo), call_req(60000))
            .into_result()
            .unwrap_err();
        assert!(e.to_string().contains("unknown management method"));
    }
}
