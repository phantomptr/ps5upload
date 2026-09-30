//! The engine client must not go through a proxy from the environment.
//!
//! A user report: with a proxy variable set, every request to the engine
//! failed with "error sending request for url (http://127.0.0.1:19113/…)"
//! and the engine logged none of them. reqwest honours `http_proxy` /
//! `ALL_PROXY` and does not exempt 127.0.0.1. This is its own test binary
//! because it sets process-wide environment variables.

use std::io::{Read, Write};
use std::net::TcpListener;

use ps5upload_desktop_lib::engine_http::engine_client_builder;

/// A one-shot HTTP server on loopback that answers 200 to whatever arrives.
fn serve_once() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        }
    });
    port
}

#[tokio::test]
async fn engine_requests_ignore_proxy_variables() {
    // A proxy nobody is listening on, as when a VPN tool exported the
    // variable and was then closed, plus the SOCKS form this build of
    // reqwest cannot use at all.
    let dead = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::env::set_var("http_proxy", format!("http://127.0.0.1:{dead}"));
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{dead}"));
    std::env::set_var("ALL_PROXY", format!("socks5://127.0.0.1:{dead}"));
    std::env::remove_var("NO_PROXY");
    std::env::remove_var("no_proxy");

    // Control: a default client really is broken by these variables, so
    // the assertion below is testing something.
    let port = serve_once();
    let url = format!("http://127.0.0.1:{port}/api/jobs");
    assert!(
        reqwest::Client::new().get(&url).send().await.is_err(),
        "a default client should have been sent to the dead proxy"
    );

    let port = serve_once();
    let url = format!("http://127.0.0.1:{port}/api/jobs");
    let resp = engine_client_builder()
        .build()
        .unwrap()
        .get(&url)
        .send()
        .await
        .expect("engine client must reach loopback directly");
    assert!(resp.status().is_success());
}
