//! `ps5upload-hub [--listen <addr>] [--stun <addr>]...`
//!
//! Settings come from `HUB_*` environment variables (`config.rs`); the flags override the two
//! addresses. STUN is off unless `--stun` (or `HUB_STUN_LISTEN`) names at least one address.

use std::sync::Arc;
use std::time::Duration;

use ps5upload_hub::{Config, Hub};

const USAGE: &str = "\
usage: ps5upload-hub [--listen <addr:port>] [--stun <addr:port>]...

  --listen  HTTP/WebSocket listen address (default 127.0.0.1:8787, env HUB_HTTP_LISTEN)
  --stun    also answer STUN Binding requests on this UDP address; repeatable
            (default: off, env HUB_STUN_LISTEN, comma separated)

Limits and the rest: HUB_* environment variables, see PROTOCOL.md and deploy/README.md.";

fn main() {
    let mut config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => fail(&e),
    };
    let mut args = std::env::args().skip(1);
    let mut stun_from_flags = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => {
                config.http_listen = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--listen needs an address:port"));
            }
            "--stun" => stun_from_flags.push(
                args.next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--stun needs an address:port")),
            ),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("ps5upload-hub {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => fail("unknown argument (see --help)"),
        }
    }
    if !stun_from_flags.is_empty() {
        config.stun_listen = stun_from_flags;
    }

    // One thread: the hub runs on one shared vCPU, does no blocking work and holds its one lock
    // only for map updates, so a second worker would only add a thread and work stealing.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|_| fail("cannot start the runtime"));
    rt.block_on(async move {
        let log: ps5upload_hub::LogSink = Arc::new(|line: &str| eprintln!("{line}"));
        let hub = match Hub::start(config, log).await {
            Ok(h) => h,
            Err(e) => fail(&format!("cannot listen: {}", e.kind())),
        };
        wait_for_signal().await;
        hub.stop(Duration::from_secs(5)).await;
    });
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("ps5upload-hub: {msg}");
    std::process::exit(2);
}
