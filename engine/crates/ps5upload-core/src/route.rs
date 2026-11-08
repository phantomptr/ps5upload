//! Where to dial a console. The home network always comes first: its own (LAN) address. Only
//! when that does not answer and the console is on Tailscale: a port on 127.0.0.1 that the
//! engine's own tailnet node carries to the console's tailnet address (`ps5upload-tailnet`), or,
//! with the system's Tailscale app on this device, the console's tailnet address itself.
//!
//! The engine installs a [`Router`] at start; every place that dials the console asks
//! [`dial`] first: the helper's AVA1 port (`ps5upload-ava1` pool), Remote Play's session
//! (TCP 9295) and stream (UDP 9296). AVA1 still checks the console's pinned key, so a path
//! through the tailnet can never stand in for another console.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

/// How a console is reached right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Via {
    /// Its own address (the LAN).
    Lan,
    /// The tailnet, peer-to-peer (`latency_ms` from the node's disco pings).
    TailnetDirect { latency_ms: Option<f64> },
    /// The tailnet through a relay (DERP).
    TailnetRelayed,
    /// The tailnet through the system's own Tailscale app on this device (its 100.x address,
    /// dialled directly; the path inside it is the app's to choose).
    System,
}

impl Via {
    /// Not the home network.
    pub fn is_tailnet(&self) -> bool {
        *self != Via::Lan
    }
}

/// What to say when something only works on the home network and the console is reached
/// through Tailscale now. `what` is the thing that cannot ("Sending a payload").
pub fn home_only(what: &str) -> String {
    format!(
        "{what} needs your home network. This PS5 is reached through Tailscale right now, \
         which only carries Remote Play and PS5Upload's own connection."
    )
}

pub trait Router: Send + Sync {
    /// `Some(local)` to dial `local` in place of `host:port`; `None` for the console itself.
    fn dial(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr>;
    fn via(&self, host: &str) -> Via;
}

static ROUTER: RwLock<Option<Arc<dyn Router>>> = RwLock::new(None);

pub fn install(r: Arc<dyn Router>) {
    *ROUTER.write().unwrap_or_else(|e| e.into_inner()) = Some(r);
}

fn current() -> Option<Arc<dyn Router>> {
    ROUTER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Where to dial `host:port` (`host` without a port, as `events::console_host` gives it).
pub fn dial(host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
    current()?.dial(host, port, udp)
}

pub fn via(host: &str) -> Via {
    current().map_or(Via::Lan, |r| r.via(host))
}

/// `host:port` for a dial, through the router when it says so: the AVA1 pool's form.
pub fn dial_str(addr: &str, port: u16) -> String {
    let host = crate::events::console_host(addr);
    match dial(&host, port, false) {
        Some(a) => a.to_string(),
        None => addr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl Router for Fake {
        fn dial(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
            (host == "192.0.2.250")
                .then(|| SocketAddr::from(([127, 0, 0, 1], port + if udp { 1000 } else { 0 })))
        }
        fn via(&self, host: &str) -> Via {
            if host == "192.0.2.250" {
                Via::TailnetRelayed
            } else {
                Via::Lan
            }
        }
    }

    #[test]
    fn routes_only_what_the_router_takes() {
        install(Arc::new(Fake));
        assert_eq!(
            dial("192.0.2.250", 9295, false),
            Some("127.0.0.1:9295".parse().unwrap())
        );
        assert_eq!(
            dial("192.0.2.250", 9296, true),
            Some("127.0.0.1:10296".parse().unwrap())
        );
        assert_eq!(dial("192.0.2.251", 9295, false), None);
        assert_eq!(dial_str("192.0.2.250:9120", 9120), "127.0.0.1:9120");
        assert_eq!(dial_str("192.0.2.251:9120", 9120), "192.0.2.251:9120");
        assert_eq!(via("192.0.2.250"), Via::TailnetRelayed);
    }
}
