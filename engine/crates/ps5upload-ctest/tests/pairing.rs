//! The engine's pairing seam (`Pool::pairing_status` / `Pool::confirm_pairing`) against the
//! real C server: the six-digit code the user compares, the confirm, and a closed window.
#![cfg(unix)]
use std::time::{Duration, Instant};

use ava1::Ava1Error;
use ps5upload_ava1::{ConsoleMatch, Pairing, Pool};
use ps5upload_ctest::*;

mod common;
use common::{dir, SECRET};

fn opts(pairing_s: u32) -> ffi::TestOpts {
    ffi::TestOpts {
        pairing_s,
        ping_ms: 100,
        dead_ms: 5_000,
        handshake_ms: 2_000,
        ..Default::default()
    }
}

/// The server shows its code once it has checked our Auth; the connect can return first.
fn wait_for_console_code(srv: &CServer) -> u32 {
    let t = Instant::now();
    while srv.pair_requests().0 == 0 && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
    }
    srv.pair_requests().1
}

#[tokio::test(flavor = "multi_thread")]
async fn the_code_the_console_shows_confirms_the_pairing() {
    let d = dir("pool-pair");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());

    let first = pool.pairing_status("192.0.2.9").await.unwrap();
    let Pairing::Code { peer_name } = first else {
        panic!("expected a code prompt, got {first:?}")
    };
    // The code exists only on the console's screen: the user types what it shows.
    let code = wait_for_console_code(&srv);
    assert_eq!(peer_name, "C test server");

    // Asking again while the dialog is open must not open a second handshake: the code on
    // the console would change under the user's eyes.
    let attempts = pool.attempts();
    let again = pool.pairing_status("192.0.2.9").await.unwrap();
    assert_eq!(
        again,
        Pairing::Code {
            peer_name: "C test server".into()
        }
    );
    assert_eq!(pool.attempts(), attempts, "the pending session is reused");

    pool.confirm_pairing("192.0.2.9", code).await.unwrap();
    assert!(
        !srv.pairing_open(),
        "a successful pairing closes the window"
    );
    assert_eq!(
        pool.pairing_status("192.0.2.9").await.unwrap(),
        Pairing::Paired
    );
    // And an ordinary session works now, with no code.
    pool.session("192.0.2.9")
        .await
        .unwrap()
        .node_info()
        .await
        .unwrap();
    let peers = std::fs::read_to_string(d.join("srv-peers")).unwrap();
    assert!(peers.lines().count() == 1, "{peers}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_window_is_reported_as_closed_not_as_an_error() {
    let d = dir("pool-pair-closed");
    // pairing_s = 0: the window is shut, and the peers file already has a device in it.
    let (_other, _) = common::paired_client(&d.join("srv-peers"));
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(0));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    assert_eq!(
        pool.pairing_status("192.0.2.9").await.unwrap(),
        Pairing::Closed
    );
    // Nothing is pending, so a confirm has nothing to confirm.
    assert!(matches!(
        pool.confirm_pairing("192.0.2.9", 0).await,
        Err(Ava1Error::NotPaired)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_already_trusts_us_needs_no_code() {
    let d = dir("pool-pair-known");
    // Teach each side the other's key, the way a launch from the app does.
    let ava = d.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = ava1::keys::Identity::load_or_create(&ava.join("identity")).unwrap();
    ava1::peers::PeerStore::load(&d.join("srv-peers"))
        .unwrap()
        .add(me.public(), "engine")
        .unwrap();
    ava1::peers::PeerStore::load(&ava.join("peers"))
        .unwrap()
        .add(
            ava1::keys::Identity::from_secret(SECRET).public(),
            "C test server",
        )
        .unwrap();
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(0));
    let pool = Pool::new(ava).with_addr(srv.addr());
    assert_eq!(
        pool.pairing_status("192.0.2.9").await.unwrap(),
        Pairing::Paired
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confirm_with_nothing_pending_re_reads_the_state() {
    let d = dir("pool-pair-late");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    // Nothing pending and the window open: the confirm is answered with a fresh code, not "closed".
    let first = pool.confirm_or_status("192.0.2.9", 0).await.unwrap();
    assert!(matches!(first, Pairing::Code { .. }), "{first:?}");
    let shown = wait_for_console_code(&srv);
    // Now it is pending: a confirm pairs.
    assert_eq!(
        pool.confirm_or_status("192.0.2.9", shown).await.unwrap(),
        Pairing::Paired
    );
    // A second (late or concurrent) confirm finds it paired: accepted, never closed.
    assert_eq!(
        pool.confirm_or_status("192.0.2.9", shown).await.unwrap(),
        Pairing::Paired
    );
}

// ---- passkey entry through the engine's pool (SPEC.md §5.5) ----

use std::sync::Arc;

async fn start_pairing(pool: &Pool) -> String {
    match pool.pairing_status("192.0.2.9").await.unwrap() {
        Pairing::Code { peer_name, .. } => peer_name,
        other => panic!("expected a code, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_typed_code_is_refused_by_the_console_and_starts_over_with_a_new_code() {
    let d = dir("pool-wrongcode");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    start_pairing(&pool).await;
    let first = wait_for_console_code(&srv);
    let wrong = (first + 1) % 1_000_000;
    // This side cannot tell a typo from a guess: only the console knows the code, so the
    // console refuses it; the retry is a fresh handshake with a new code on the screen.
    let r = pool.confirm_or_status("192.0.2.9", wrong).await.unwrap();
    assert!(matches!(r, Pairing::WrongCode { .. }), "{r:?}");
    let t = Instant::now();
    while srv.pair_requests().0 < 2 && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(srv.pair_requests().0, 2, "a second pop-up with a new code");
    let second = srv.pair_requests().1;
    assert!(!srv.pairing_open() || second < 1_000_000);
    assert_eq!(
        pool.confirm_or_status("192.0.2.9", second).await.unwrap(),
        Pairing::Paired
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dismissing_the_dialog_gives_the_unconfirmed_place_back() {
    let d = dir("pool-cancel");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    start_pairing(&pool).await; // takes one of the console's two places
    let other = Pool::new(d.join("ava2")).with_addr(srv.addr());
    start_pairing(&other).await; // takes the other
    let third = Pool::new(d.join("ava3")).with_addr(srv.addr());
    let full = third.pairing_status("192.0.2.9").await;
    assert!(
        matches!(&full, Err(Ava1Error::Refused { code, .. }) if *code == ava1::gen::ERR_BUSY),
        "{full:?}"
    );
    assert!(pool.cancel_pairing("192.0.2.9").await);
    assert!(!pool.cancel_pairing("192.0.2.9").await, "nothing left");
    let t = Instant::now();
    loop {
        match third.pairing_status("192.0.2.9").await {
            Ok(Pairing::Code { .. }) => break,
            r => assert!(t.elapsed() < Duration::from_secs(5), "{r:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // And the poll that follows a dismissal does not knock again.
    assert!(matches!(
        pool.confirm_pairing("192.0.2.9", 1).await,
        Err(Ava1Error::NotPaired)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_different_console_at_a_pinned_address_can_be_forgotten_and_paired() {
    let d = dir("pool-forget");
    let ava = d.join("ava");
    {
        let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
        let pool = Pool::new(ava.clone()).with_addr(srv.addr());
        start_pairing(&pool).await;
        let code = wait_for_console_code(&srv);
        pool.confirm_pairing("192.0.2.9", code).await.unwrap();
    }
    // A different console (another key) now answers at the same address.
    let srv2 = CServer::start_with([0x77; 32], &d.join("srv2-peers"), opts(60));
    let pool = Pool::new(ava).with_addr(srv2.addr());
    assert_eq!(
        pool.pairing_status("192.0.2.9").await.unwrap(),
        Pairing::WrongConsole
    );
    assert_eq!(
        pool.pairing_status("192.0.2.9").await.unwrap(),
        Pairing::WrongConsole,
        "no way out without forgetting"
    );
    assert!(pool.forget_console_key("192.0.2.9").await);
    assert!(!pool.forget_console_key("192.0.2.9").await, "already gone");
    start_pairing(&pool).await;
    let code = wait_for_console_code(&srv2);
    pool.confirm_pairing("192.0.2.9", code).await.unwrap();
    pool.session("192.0.2.9")
        .await
        .unwrap()
        .node_info()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn polling_an_unpaired_console_for_a_minute_opens_at_most_two_handshakes() {
    use ps5upload_ava1::console::require_in;
    let d = dir("pool-poll");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    // The status poll is 10 s and the not-paired memory 30 s; scaled 1:100 here.
    let pool = Arc::new(
        Pool::new(d.join("ava"))
            .with_addr(srv.addr())
            .with_not_paired_ttl(Duration::from_millis(300)),
    );
    let p = pool.clone();
    let began = std::time::Instant::now();
    tokio::task::spawn_blocking(move || {
        for _ in 0..11 {
            let _ = require_in(&p, "192.0.2.9", ava1::gen::CAP_DATA_PLANE);
            std::thread::sleep(Duration::from_millis(50));
        }
    })
    .await
    .unwrap();
    // One handshake per not-paired memory (300 ms) at most, never one per poll. The window is
    // ~600 ms (two), longer on a loaded machine, where a third is the memory working, not a bug.
    let allowed = 1 + (began.elapsed().as_millis() / 300) as usize;
    assert!(
        pool.attempts() <= allowed.min(10),
        "{} handshakes in {:?} (allowed {allowed})",
        pool.attempts(),
        began.elapsed()
    );
    assert!(pool.attempts() >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_poll_never_knocks_while_the_dialog_is_waiting_on_a_code() {
    use ps5upload_ava1::console::require_in;
    let d = dir("pool-poll-pending");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Arc::new(
        Pool::new(d.join("ava"))
            .with_addr(srv.addr())
            .with_not_paired_ttl(Duration::from_millis(1)),
    );
    start_pairing(&pool).await;
    let before = pool.attempts();
    let p = pool.clone();
    tokio::task::spawn_blocking(move || {
        for _ in 0..5 {
            let _ = require_in(&p, "192.0.2.9", ava1::gen::CAP_DATA_PLANE);
            std::thread::sleep(Duration::from_millis(20));
        }
    })
    .await
    .unwrap();
    assert_eq!(
        pool.attempts(),
        before,
        "the pending handshake is the only one"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn peeking_at_pairing_never_opens_a_handshake() {
    let d = dir("pool-peek");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    assert_eq!(pool.peek_pairing("192.0.2.9").await, None);
    assert_eq!(pool.attempts(), 0, "a read never dials the console");
    assert_eq!(srv.pair_requests().0, 0, "and shows nothing on it");
    start_pairing(&pool).await;
    let before = pool.attempts();
    assert!(matches!(
        pool.peek_pairing("192.0.2.9").await,
        Some(Pairing::Code { .. })
    ));
    assert_eq!(pool.attempts(), before);
}

/// Pairs `pool` with the console at 192.0.2.9 (the address the pin is kept under).
async fn pair_at_9(pool: &Pool, srv: &CServer) {
    start_pairing(pool).await;
    let code = wait_for_console_code(srv);
    pool.confirm_pairing("192.0.2.9", code).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_paired_console_at_a_new_address_is_recognised_as_the_same_one() {
    let d = dir("pool-same-moved");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    pair_at_9(&pool, &srv).await;
    // DHCP moved it: the same console now answers at .10 (the override routes both to srv).
    assert_eq!(
        pool.same_console("192.0.2.9", "192.0.2.10").await,
        ConsoleMatch::Same
    );
    assert_eq!(
        srv.pair_requests().0,
        1,
        "checking never asks for a new code"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_console_at_the_new_address_is_not_taken_for_the_paired_one() {
    let d = dir("pool-same-other");
    let ava = d.join("ava");
    {
        let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
        pair_at_9(&Pool::new(ava.clone()).with_addr(srv.addr()), &srv).await;
    }
    let other = CServer::start_with([0x77; 32], &d.join("srv2-peers"), opts(60));
    let pool = Pool::new(ava).with_addr(other.addr());
    assert_eq!(
        pool.same_console("192.0.2.9", "192.0.2.10").await,
        ConsoleMatch::Different
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_pin_or_an_answer_it_cannot_tell() {
    let d = dir("pool-same-unknown");
    let srv = CServer::start_with(SECRET, &d.join("srv-peers"), opts(60));
    let pool = Pool::new(d.join("ava")).with_addr(srv.addr());
    // Never paired at the old address: nothing to compare against.
    assert_eq!(
        pool.same_console("192.0.2.9", "192.0.2.10").await,
        ConsoleMatch::Unknown
    );
    pair_at_9(&pool, &srv).await;
    let addr = srv.addr();
    drop(srv);
    // Paired, but nothing answers at the new address.
    let pool = Pool::new(d.join("ava")).with_addr(addr);
    assert_eq!(
        pool.same_console("192.0.2.9", "192.0.2.10").await,
        ConsoleMatch::Unknown
    );
}
