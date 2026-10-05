//! `POST /api/mgmt/call`: a generic management passthrough for the desktop shell.
//!
//! The desktop process runs about fifty management calls in-process (power, hardware,
//! processes, saves, SMP, ...). It cannot hold its own AVA1 session next to this engine's
//! (one console session per identity), so it forwards them here and the call goes over the
//! session this engine already has. The wire is `ps5upload_core::mgmt_proxy`.
//!
//! Guard: none of its own. The route sits behind the same two layers as every other `/api/*`
//! route, the peer-IP policy (`loopback_guard`: loopback, or a peer matched by
//! `PS5UPLOAD_ALLOW_IP`) and the browser Origin guard. A desktop whose Engine URL points at a
//! NAS or a Docker engine (where the peer is a LAN or bridge address) therefore keeps its
//! management commands whenever that engine admits it for the other routes. The caller's
//! timeout is honoured end to end: nothing here adds a deadline of its own.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::mgmt::MgmtTransport;
use ps5upload_core::mgmt_proxy::{self, ProxyRequest};

pub(crate) async fn mgmt_call_handler(Json(req): Json<ProxyRequest>) -> Response {
    handle(req, None).await
}

/// `transport` is a test seam; `None` uses the process's registered transport.
async fn handle(req: ProxyRequest, transport: Option<Arc<dyn MgmtTransport>>) -> Response {
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
        let r = parse(handle(req(30_000), Some(t)).await).await;
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
        let r = parse(handle(req(5_000), Some(t)).await).await;
        assert!(!r.ok);
        match r.error.unwrap() {
            ProxyError::Mgmt { label, cause, .. } => {
                assert_eq!(label, "HW_INFO");
                assert!(cause.starts_with("helper_not_ava1"), "{cause}");
            }
            o => panic!("{o:?}"),
        }
    }

    /// The route behind the engine's real guards (`loopback_guard`, `browser_origin_guard`),
    /// as `run` layers them, with the peer address faked. Returns the status of one POST.
    async fn post_as(peer: &str, allow: &str, origin: Option<&str>) -> u16 {
        use axum::extract::connect_info::MockConnectInfo;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let guard_cfg = crate::LoopbackGuardConfig {
            allowed_ips: crate::parse_allow_ips(allow).into(),
        };
        let app = axum::Router::new()
            .route(
                ps5upload_core::mgmt_proxy::ROUTE,
                axum::routing::post(mgmt_call_handler),
            )
            .layer(axum::middleware::from_fn(crate::browser_origin_guard))
            .layer(axum::middleware::from_fn_with_state(
                guard_cfg,
                crate::loopback_guard,
            ))
            .layer(MockConnectInfo(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            ));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        let body = serde_json::to_string(&req(1_000)).unwrap();
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        let msg = format!(
            "POST {} HTTP/1.1\r\nHost: {addr}\r\n{origin}Content-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            ps5upload_core::mgmt_proxy::ROUTE,
            body.len()
        );
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(msg.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let head = String::from_utf8_lossy(&out);
        head.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lan_peer_gets_the_same_answer_as_every_other_api_route() {
        // Not on the allow list: refused, like /api/ps5/*.
        for peer in ["192.168.1.20:4000", "172.17.0.1:4000", "[2001:db8::1]:4000"] {
            assert_eq!(post_as(peer, "", None).await, 403, "{peer}");
        }
        // On the allow list (a NAS, or a Docker bridge address): forwarded, not refused.
        assert_ne!(
            post_as("192.168.1.20:4000", "192.168.1.0/24", None).await,
            403
        );
        assert_ne!(post_as("172.17.0.1:4000", "172.17.0.1", None).await, 403);
        // A peer outside the range stays refused.
        assert_eq!(post_as("10.0.0.5:4000", "192.168.1.0/24", None).await, 403);
        // Loopback, including IPv4-mapped loopback, never needs the list.
        assert_ne!(post_as("127.0.0.1:4000", "", None).await, 403);
        assert_ne!(post_as("[::1]:4000", "", None).await, 403);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cross_site_browser_origin_is_still_refused_even_from_an_allowed_peer() {
        assert_eq!(
            post_as("127.0.0.1:4000", "", Some("https://evil.example")).await,
            403
        );
        assert_eq!(
            post_as(
                "192.168.1.20:4000",
                "192.168.1.0/24",
                Some("https://evil.example")
            )
            .await,
            403
        );
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
        let r = parse(handle(req(120_000), Some(t)).await).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(spy.timeout_ms.load(Ordering::SeqCst), 120_000);
    }
}
