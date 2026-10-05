//! `POST /api/mgmt/call`: a generic management passthrough for the desktop shell.
//!
//! The desktop process runs about fifty management calls in-process (power, hardware,
//! processes, saves, SMP, ...). It cannot hold its own AVA1 session next to this engine's
//! (one console session per identity), so it forwards them here and the call goes over the
//! session this engine already has. The wire is `ps5upload_core::mgmt_proxy`.
//!
//! Because it is a passthrough with no per-method policy, it is reachable from this
//! machine only: the peer must be loopback, whatever `PS5UPLOAD_ALLOW_IP` says (the
//! general loopback guard lets allow-listed LAN peers in; this route does not). Behind
//! Docker's NAT the peer is the bridge address, so it is refused there too. The browser
//! Origin guard applies as to every route. The caller's timeout is honoured end to end:
//! nothing here adds a deadline of its own.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::mgmt::MgmtTransport;
use ps5upload_core::mgmt_proxy::{self, ProxyRequest};

pub(crate) async fn mgmt_call_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<ProxyRequest>,
) -> Response {
    handle(peer, req, None).await
}

/// `transport` is a test seam; `None` uses the process's registered transport.
async fn handle(
    peer: SocketAddr,
    req: ProxyRequest,
    transport: Option<Arc<dyn MgmtTransport>>,
) -> Response {
    if !peer.ip().to_canonical().is_loopback() {
        eprintln!("[ps5upload-engine] refusing /api/mgmt/call from {peer}: loopback only");
        return (
            StatusCode::FORBIDDEN,
            "the management passthrough is loopback only",
        )
            .into_response();
    }
    let res = tokio::task::spawn_blocking(move || match transport {
        Some(t) => mgmt_proxy::execute_with(t, req),
        None => mgmt_proxy::execute(req),
    })
    .await;
    match res {
        Ok(r) => Json(r).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("management call task failed: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use ava1::gen::{self, MgmtText};
    use ava1::host::FolderHost;
    use ava1::keys::Identity;
    use ava1::peers::PeerStore;
    use ava1::server::{self, RpcHandler, ServerCtx};
    use ava1::session::RpcReply;
    use ava1::wire::Message;
    use ps5upload_ava1::mgmt::AvaTransport;
    use ps5upload_ava1::Pool;
    use ps5upload_core::mgmt::{m, Method};
    use ps5upload_core::mgmt_proxy::{
        decode_body, encode_body, ProxyError, ProxyResponse, ProxyResult,
    };

    /// Records the timeout the route passed down, then defers to the real AVA1 transport.
    struct Spy {
        inner: AvaTransport,
        timeout_ms: AtomicU64,
    }
    impl MgmtTransport for Spy {
        fn call(
            &self,
            addr: &str,
            m: Method,
            label: &str,
            body: &[u8],
            t: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            self.timeout_ms
                .store(t.as_millis() as u64, Ordering::SeqCst);
            self.inner.call(addr, m, label, body, t)
        }
    }

    fn loopback() -> SocketAddr {
        "127.0.0.1:50000".parse().unwrap()
    }

    fn req(timeout_ms: u64) -> ProxyRequest {
        ProxyRequest::Call {
            addr: "route-console".into(),
            method: m::HW_INFO.id,
            label: "HW_INFO".into(),
            body_b64: encode_body(b"x=1"),
            timeout_ms,
        }
    }

    /// A paired loopback AVA1 console running `handler`, and the transport over its pool.
    async fn console(tag: &str, handler: RpcHandler) -> Arc<Spy> {
        let base = std::env::temp_dir().join(format!("p5-route-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ava = base.join("ava");
        std::fs::create_dir_all(&ava).unwrap();
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        let mut peers = PeerStore::in_memory();
        peers.add(me.public(), "engine").unwrap();
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, handler)
            .with_jobs(Arc::new(FolderHost {
                root: base.join("share"),
                jobs_dir: base.join("jobs"),
            }))
            .with_mgmt();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        let pool: &'static Pool = Box::leak(Box::new(Pool::new(ava).with_addr(addr)));
        Arc::new(Spy {
            inner: AvaTransport::with_pool(pool),
            timeout_ms: AtomicU64::new(0),
        })
    }

    async fn parse(r: Response) -> ProxyResponse {
        assert_eq!(r.status(), StatusCode::OK);
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    fn text(s: &str) -> RpcReply {
        RpcReply {
            status: gen::STATUS_OK,
            body: MgmtText {
                body: s.as_bytes().to_vec(),
                more: None,
            }
            .to_bytes()
            .unwrap(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forwards_a_call_to_the_console_and_returns_the_body() {
        let t = console(
            "fwd",
            Box::new(|method, body| {
                assert_eq!(method, gen::METHOD_HW_INFO);
                text(&format!(
                    "model=PS5\necho={}",
                    String::from_utf8(MgmtText::decode(body).unwrap().body).unwrap()
                ))
            }),
        )
        .await;
        let r = parse(handle(loopback(), req(30_000), Some(t)).await).await;
        match r.into_result().unwrap() {
            ProxyResult::Bytes { body_b64 } => {
                assert_eq!(decode_body(&body_b64).unwrap(), b"model=PS5\necho=x=1")
            }
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_console_without_ava1_is_helper_not_ava1() {
        // Nothing listens on this address: the transport has no session to open.
        let base = std::env::temp_dir().join(format!("p5-route-none-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let pool: &'static Pool = Box::leak(Box::new(Pool::new(base).with_addr("127.0.0.1:1")));
        let t: Arc<dyn MgmtTransport> = Arc::new(AvaTransport::with_pool(pool));
        let r = parse(handle(loopback(), req(5_000), Some(t)).await).await;
        assert!(!r.ok);
        match r.error.unwrap() {
            ProxyError::Mgmt { label, cause, .. } => {
                assert_eq!(label, "HW_INFO");
                assert!(cause.starts_with("helper_not_ava1"), "{cause}");
            }
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_non_loopback_peer_is_refused() {
        for peer in ["192.168.1.20:4000", "172.17.0.1:4000", "[2001:db8::1]:4000"] {
            let r = handle(peer.parse().unwrap(), req(1_000), None).await;
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "{peer}");
        }
        // IPv4-mapped loopback is still loopback.
        let t = console("mapped", Box::new(|_, _| text("ok=1"))).await;
        let r = handle(
            "[::ffff:127.0.0.1]:4000".parse().unwrap(),
            req(5_000),
            Some(t),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_timeout_beyond_sixty_seconds_reaches_the_transport_and_a_slow_call_completes() {
        let t = console(
            "slow",
            Box::new(|_, _| {
                std::thread::sleep(Duration::from_millis(1500));
                text("done=1")
            }),
        )
        .await;
        let spy = t.clone();
        let r = parse(handle(loopback(), req(120_000), Some(t)).await).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(spy.timeout_ms.load(Ordering::SeqCst), 120_000);
    }
}
