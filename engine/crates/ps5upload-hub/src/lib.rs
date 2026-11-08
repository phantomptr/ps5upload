//! The Play Together hub: a small public server that introduces a host's engine to a guest's
//! browser so they can open a WebRTC connection to each other. It forwards signalling messages
//! (SDP and ICE, opaque to the hub) over WebSockets and answers STUN Binding requests. Media
//! never passes through it, and it keeps nothing on disk. `PROTOCOL.md` is the wire contract.
//!
//! Privacy rule: nothing the hub logs or reports can hold a client address, a room id or a
//! payload. [`Shared::log`] only takes `&'static str` names and numbers, so a log line cannot
//! carry anything a client sent.

pub mod config;
pub mod hub;
pub mod limits;
pub mod metrics;
pub mod stun;

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use config::Config;
use limits::RateTable;
use metrics::Metrics;

/// Where log lines go. `main` prints them to stderr (the journal); the tests collect them.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A log field's value: a number or a fixed name. Deliberately no `String`.
#[derive(Clone, Copy, Debug)]
pub enum Field {
    N(u64),
    S(&'static str),
}

/// Everything the tasks share.
pub struct Shared {
    pub config: Config,
    pub metrics: Metrics,
    pub(crate) state: Mutex<hub::State>,
    pub(crate) shutdown: watch::Sender<bool>,
    pub(crate) next_conn: AtomicU64,
    pub(crate) connections: AtomicUsize,
    log: LogSink,
}

impl Shared {
    /// One log line: an event name and named numbers or fixed names.
    pub fn log(&self, event: &'static str, fields: &[(&'static str, Field)]) {
        let mut line = format!("hub: {event}");
        for (k, v) in fields {
            match v {
                Field::N(n) => line.push_str(&format!(" {k}={n}")),
                Field::S(s) => line.push_str(&format!(" {k}={s}")),
            }
        }
        (self.log)(&line);
    }

    pub fn metrics_text(&self) -> String {
        self.metrics.render(&hub::gauges(self))
    }
}

fn serve(listener: TcpListener, app: axum::Router, shared: &Shared) -> JoinHandle<()> {
    let mut rx = shared.shutdown.subscribe();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = rx.wait_for(|v| *v).await;
        })
        .await;
    })
}

/// A running hub.
pub struct Hub {
    pub http_addr: SocketAddr,
    pub metrics_addr: Option<SocketAddr>,
    pub stun_addrs: Vec<SocketAddr>,
    pub shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
}

impl Hub {
    /// Binds the HTTP listener and the STUN sockets and starts serving. A STUN address that
    /// cannot be bound is skipped (and logged by its index, not its address); the HTTP listener
    /// must bind.
    pub async fn start(config: Config, log: LogSink) -> io::Result<Hub> {
        let listener = TcpListener::bind(config.http_listen).await?;
        let http_addr = listener.local_addr()?;
        let metrics_listener = match config.metrics_listen {
            Some(a) => Some(TcpListener::bind(a).await?),
            None => None,
        };
        let metrics_addr = match &metrics_listener {
            Some(l) => Some(l.local_addr()?),
            None => None,
        };
        let mut stun_socks = Vec::new();
        let mut stun_failed = 0u64;
        for addr in &config.stun_listen {
            match UdpSocket::bind(addr).await {
                Ok(s) => stun_socks.push(s),
                Err(_) => stun_failed += 1,
            }
        }
        let stun_addrs = stun_socks
            .iter()
            .filter_map(|s| s.local_addr().ok())
            .collect::<Vec<_>>();
        let (shutdown, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            state: Mutex::new(hub::State::new(&config)),
            metrics: Metrics::default(),
            shutdown,
            next_conn: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
            log,
            config,
        });
        let stun_table = Arc::new(Mutex::new(RateTable::new(shared.config.max_tracked_ips)));

        let mut tasks = Vec::new();
        for s in stun_socks {
            tasks.push(tokio::spawn(stun::serve(
                s,
                shared.clone(),
                stun_table.clone(),
            )));
        }
        tasks.push(tokio::spawn(hub::sweeper(shared.clone(), stun_table)));
        let app = hub::router(shared.clone(), metrics_listener.is_none());
        tasks.push(serve(listener, app, &shared));
        if let Some(l) = metrics_listener {
            tasks.push(serve(l, hub::metrics_router(shared.clone()), &shared));
        }
        shared.log(
            "started",
            &[
                ("http_port", Field::N(http_addr.port().into())),
                ("stun_sockets", Field::N(stun_addrs.len() as u64)),
                ("stun_bind_failed", Field::N(stun_failed)),
            ],
        );
        Ok(Hub {
            http_addr,
            metrics_addr,
            stun_addrs,
            shared,
            tasks,
        })
    }

    /// Asks everything to stop: every WebSocket gets a close frame (1001, going away).
    pub fn shutdown(&self) {
        let _ = self.shared.shutdown.send(true);
    }

    /// Stops and waits for the tasks and the connections, at most `grace` in all (Fly's
    /// `kill_timeout` must be longer).
    pub async fn stop(self, grace: Duration) {
        let deadline = tokio::time::Instant::now() + grace;
        self.shutdown();
        let shared = self.shared.clone();
        let all = futures_util::future::join_all(self.tasks);
        let _ = tokio::time::timeout_at(deadline, all).await;
        // Connection tasks close on the same signal; give them the rest of the grace period.
        while shared
            .connections
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        shared.log("stopped", &[]);
    }
}
