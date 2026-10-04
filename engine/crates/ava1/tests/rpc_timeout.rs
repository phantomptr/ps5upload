//! An RPC the peer never answers is bounded (final review: engine #6).
mod common;

use std::time::{Duration, Instant};

use ava1::session::connect;
use ava1::Ava1Error;
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn an_rpc_the_peer_never_answers_times_out() {
    // A peer whose handler holds the reply for longer than the bound.
    let (s_id, c_id) = (
        ava1::keys::Identity::generate().unwrap(),
        std::sync::Arc::new(ava1::keys::Identity::generate().unwrap()),
    );
    let mut sp = ava1::peers::PeerStore::in_memory();
    sp.add(c_id.public(), "client").unwrap();
    let mut cp = ava1::peers::PeerStore::in_memory();
    cp.add(s_id.public(), "server").unwrap();
    let slow: ava1::server::RpcHandler = Box::new(|_, _| {
        std::thread::sleep(Duration::from_secs(4));
        ava1::session::RpcReply {
            status: 0,
            body: Vec::new(),
        }
    });
    let ctx = ava1::server::ServerCtx::new(s_id, "slow", sp, slow).with_timing(fast());
    let (addr, _ctx) = start(ctx).await;
    let s = connect(
        &addr.to_string(),
        c_id,
        std::sync::Arc::new(std::sync::Mutex::new(cp)),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    let t = Instant::now();
    let e = s
        .rpc_within(999, &[], Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(matches!(e, Ava1Error::Timeout), "{e}");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}
