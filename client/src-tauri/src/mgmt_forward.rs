//! Management calls from the desktop shell, forwarded to the sidecar engine.
//!
//! About fifty Tauri commands (power, hardware, processes, saves, SMP, diagnostics, ...)
//! call `ps5upload_core` management functions in this process. Those go through core's
//! `MgmtTransport` seam. This process must not open its own AVA1 console session: with the
//! engine's identity the two processes would evict each other's session, and with its own it
//! would need a second pairing. So this transport sends every call to the engine over loopback
//! HTTP (`POST /api/mgmt/call`, wire in `ps5upload_core::mgmt_proxy`), and the engine runs it
//! on the session it already has. Errors come back typed and are rebuilt as the same
//! `MgmtError` the in-process transport would raise, so the UI's error mapping is unchanged.
//!
//! The base URL is read per call from `engine::url()`, so it follows the engine's port
//! fallback; the client ignores proxy environment variables (`engine_http`). The HTTP timeout
//! is the call's own timeout plus a margin, never a client default.
//!
//! Desktop only. The Android/iOS build links the engine in-process, and the engine installs
//! its own AVA1 transport there.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use ps5upload_core::mgmt::{self, JobCall, JobOp, JobProgress, Method, MgmtTransport};
use ps5upload_core::mgmt_proxy::{
    decode_body, encode_body, ProxyRequest, ProxyResponse, ProxyResult, ROUTE,
};

/// Slack on top of the call's own timeout for the HTTP hop and the engine's own bookkeeping.
const MARGIN: Duration = Duration::from_secs(20);

pub struct ForwardTransport {
    base: Box<dyn Fn() -> String + Send + Sync>,
}

/// Registers the forwarding transport for this process. Call once at startup.
pub fn install() {
    mgmt::set_transport(Arc::new(ForwardTransport::new(crate::engine::url)));
}

impl ForwardTransport {
    pub fn new<S: Into<String>>(base: impl Fn() -> S + Send + Sync + 'static) -> Self {
        Self {
            base: Box::new(move || base().into()),
        }
    }

    /// One round trip. Runs on its own thread with its own runtime so it is safe from any
    /// caller: a plain thread, a `spawn_blocking` worker, or an async worker.
    fn send(&self, req: &ProxyRequest) -> Result<ProxyResult> {
        let url = format!("{}{}", (self.base)().trim_end_matches('/'), ROUTE);
        let timeout = req.bound() + MARGIN;
        let resp: ProxyResponse = std::thread::scope(|s| {
            s.spawn(|| -> Result<ProxyResponse> {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build the management forwarder runtime")?;
                rt.block_on(async {
                    let client = crate::engine_http::engine_client_builder()
                        .timeout(timeout)
                        .build()
                        .context("build the management forwarder client")?;
                    let r = client.post(&url).json(req).send().await.map_err(|e| {
                        anyhow!(
                            "engine unreachable for a management call: {}",
                            crate::engine_http::error_chain(&e)
                        )
                    })?;
                    let status = r.status();
                    if !status.is_success() {
                        let text = r.text().await.unwrap_or_default();
                        return Err(anyhow!(
                            "engine refused the management call ({status}): {text}"
                        ));
                    }
                    r.json::<ProxyResponse>()
                        .await
                        .context("decode the engine's management reply")
                })
            })
            .join()
            .unwrap_or_else(|_| Err(anyhow!("management forwarder thread panicked")))
        })?;
        resp.into_result()
    }

    fn bytes(&self, req: &ProxyRequest) -> Result<Option<Vec<u8>>> {
        match self.send(req)? {
            ProxyResult::Bytes { body_b64 } => Ok(Some(decode_body(&body_b64)?)),
            other => Err(anyhow!("unexpected management reply: {other:?}")),
        }
    }
}

impl MgmtTransport for ForwardTransport {
    fn call(
        &self,
        addr: &str,
        method: Method,
        label: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>> {
        self.bytes(&ProxyRequest::Call {
            addr: addr.to_string(),
            method: method.id,
            label: label.to_string(),
            body_b64: encode_body(body),
            timeout_ms: timeout.as_millis() as u64,
        })
    }

    fn run_job(
        &self,
        addr: &str,
        op: JobOp,
        label: &str,
        body: &[u8],
        call: &JobCall<'_>,
    ) -> Result<Option<Vec<u8>>> {
        self.bytes(&ProxyRequest::RunJob {
            addr: addr.to_string(),
            job_op: op.id,
            label: label.to_string(),
            body_b64: encode_body(body),
            op_id: call.op_id,
            subject: call.subject.to_string(),
            deadline_ms: call.deadline.as_millis() as u64,
        })
    }

    fn job_progress(&self, addr: &str, op_id: u64) -> Result<Option<Option<JobProgress>>> {
        match self.send(&ProxyRequest::JobProgress {
            addr: addr.to_string(),
            op_id,
        })? {
            ProxyResult::Progress { progress } => Ok(Some(progress)),
            other => Err(anyhow!("unexpected management reply: {other:?}")),
        }
    }

    fn job_cancel(&self, addr: &str, op_id: u64) -> Result<Option<bool>> {
        match self.send(&ProxyRequest::JobCancel {
            addr: addr.to_string(),
            op_id,
        })? {
            ProxyResult::Cancel { found } => Ok(Some(found)),
            other => Err(anyhow!("unexpected management reply: {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_core::mgmt::{m, MgmtError};
    use ps5upload_core::mgmt_proxy::ProxyError;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// A loopback HTTP server that answers each request with `reply(request_json)` and
    /// records the request bodies.
    fn stub(
        reply: impl Fn(&ProxyRequest) -> ProxyResponse + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<ProxyRequest>>>) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (0, 0);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                        assert!(head.starts_with("post /api/mgmt/call "), "{head}");
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let req: ProxyRequest = serde_json::from_slice(&buf[head_end..]).unwrap();
                let body = serde_json::to_vec(&reply(&req)).unwrap();
                seen2.lock().unwrap().push(req);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        (url, seen)
    }

    fn ok_bytes(b: &[u8]) -> ProxyResponse {
        ProxyResponse {
            ok: true,
            result: Some(ProxyResult::Bytes {
                body_b64: encode_body(b),
            }),
            error: None,
        }
    }

    #[test]
    fn a_call_round_trips_with_its_timeout_and_body() {
        let (url, seen) = stub(|_| ok_bytes(b"model=PS5"));
        let t = ForwardTransport::new(move || url.clone());
        let r = t
            .call(
                "10.0.0.2",
                m::HW_INFO,
                "HW_INFO",
                b"a=1",
                Duration::from_secs(300),
            )
            .unwrap();
        assert_eq!(r.unwrap(), b"model=PS5");
        let seen = seen.lock().unwrap();
        match &seen[0] {
            ProxyRequest::Call {
                addr,
                method,
                label,
                body_b64,
                timeout_ms,
            } => {
                assert_eq!(addr, "10.0.0.2");
                assert_eq!(*method, m::HW_INFO.id);
                assert_eq!(label, "HW_INFO");
                assert_eq!(decode_body(body_b64).unwrap(), b"a=1");
                assert_eq!(*timeout_ms, 300_000);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_typed_refusal_is_rebuilt_as_the_same_mgmt_error() {
        let (url, _) = stub(|_| ProxyResponse {
            ok: false,
            result: None,
            error: Some(ProxyError::Mgmt {
                label: "HW_INFO".into(),
                status: 0,
                cause: "helper_not_ava1: gone".into(),
            }),
        });
        let t = ForwardTransport::new(move || url.clone());
        let e = t
            .call("c", m::HW_INFO, "HW_INFO", b"", Duration::from_secs(5))
            .unwrap_err();
        let m = e.downcast_ref::<MgmtError>().unwrap();
        assert_eq!(m.cause, "helper_not_ava1: gone");
        assert_eq!(
            e.to_string(),
            "payload rejected HW_INFO: helper_not_ava1: gone"
        );
    }

    #[test]
    fn jobs_progress_and_cancel_round_trip() {
        let (url, _) = stub(|r| match r {
            ProxyRequest::RunJob { .. } => ok_bytes(b"{\"ok\":true}"),
            ProxyRequest::JobProgress { .. } => ProxyResponse {
                ok: true,
                result: Some(ProxyResult::Progress {
                    progress: Some(JobProgress {
                        kind: "fs_delete".into(),
                        files_done: 3,
                        ..Default::default()
                    }),
                }),
                error: None,
            },
            _ => ProxyResponse {
                ok: true,
                result: Some(ProxyResult::Cancel { found: true }),
                error: None,
            },
        });
        let t = ForwardTransport::new(move || url.clone());
        let call = JobCall {
            op_id: 9,
            subject: "/data/x",
            deadline: Duration::from_secs(3600),
        };
        let r = t
            .run_job("c", mgmt::ops::DELETE, "FS_DELETE", b"{}", &call)
            .unwrap();
        assert_eq!(r.unwrap(), b"{\"ok\":true}");
        let p = t.job_progress("c", 9).unwrap().unwrap().unwrap();
        assert_eq!((p.kind.as_str(), p.files_done), ("fs_delete", 3));
        assert_eq!(t.job_cancel("c", 9).unwrap(), Some(true));
    }

    #[test]
    fn an_unreachable_engine_is_a_plain_error() {
        let dead = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}", dead.local_addr().unwrap().port());
        drop(dead);
        let t = ForwardTransport::new(move || url.clone());
        let e = t
            .call("c", m::HW_INFO, "HW_INFO", b"", Duration::from_secs(2))
            .unwrap_err();
        assert!(e.to_string().contains("engine unreachable"), "{e}");
    }

    /// A command path that reaches the transport: `process_list_get` -> core `process_list`
    /// -> the registered transport -> the stub engine. Registers the process transport, so
    /// it is the only test here that does.
    #[tokio::test(flavor = "multi_thread")]
    async fn process_list_command_reaches_the_forwarding_transport() {
        let (url, seen) = stub(|_| ok_bytes(br#"{"procs":[{"pid":7,"name":"x","comm":"x"}]}"#));
        mgmt::set_transport(Arc::new(ForwardTransport::new(move || url.clone())));
        let v = crate::commands::process_mgr::process_list_get("10.0.0.9".into())
            .await
            .unwrap();
        assert_eq!(v["processes"][0]["pid"], 7);
        let seen = seen.lock().unwrap();
        match &seen[0] {
            ProxyRequest::Call { addr, method, .. } => {
                assert_eq!(addr, "10.0.0.9");
                assert_eq!(*method, m::PROC_PROCESS_LIST.id);
            }
            o => panic!("{o:?}"),
        }
    }
}
