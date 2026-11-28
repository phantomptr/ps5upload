//! Where to dial a console. The home network always comes first: its own (LAN) address. Only
//! when that does not answer and the console is on Tailscale: a port on 127.0.0.1 that the
//! engine's own tailnet node carries to the console's tailnet address (`ps5upload-tailnet`), or,
//! with the system's Tailscale app on this device, the console's tailnet address itself.
//!
//! The engine installs a [`Router`] at start; every place that dials the console asks
//! [`dial`] first: the helper's AVA1 port (`ps5upload-ava1` pool), Remote Play's session
//! (TCP 9295) and stream (UDP 9296). AVA1 still checks the console's pinned key, so a path
//! through the tailnet can never stand in for another console.
//!
//! At home, a console with a USB Ethernet adapter the helper drives (`crate::usbnet`,
//! experimental) may have its AVA1 port dialled at the adapter's address instead, while this
//! computer reaches it there. That is still the home network: [`Via`] stays `Lan` for every
//! home-only check, and [`link_of`] says which of the console's links a connection used.

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

/// A console's USB Ethernet adapter as the router knows it (`crate::usbnet`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbLink {
    /// Where its AVA1 port answers while it serves; None when it does not serve now.
    pub addr: Option<SocketAddr>,
    /// The link's speed (Mb/s; 0 unknown).
    pub mbps: u32,
    pub chip: String,
}

/// Which of the console's own links a connection went through: the label of a transfer and
/// of a status check. Never a home-network check (that is [`Via`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// The console's own (built-in) network address.
    Console,
    /// Its USB Ethernet adapter, at `mbps` (0 unknown).
    Usb { mbps: u32 },
}

pub trait Router: Send + Sync {
    /// `Some(local)` to dial `local` in place of `host:port`; `None` for the console itself.
    fn dial(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr>;
    fn via(&self, host: &str) -> Via;
    /// A connection to `to` that [`Router::dial`] gave for `host:port` worked (`ok`) or failed
    /// (could not be opened, or ended under its user): a path that fails is let go at once.
    fn dialed(&self, _host: &str, _port: u16, _to: SocketAddr, _ok: bool) {}
    /// The console's USB adapter, once it has reported one; None for a console without.
    fn usb(&self, _host: &str) -> Option<UsbLink> {
        None
    }
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

/// A connection that [`dial_str`] routed (`to`, not the console's own address) worked or not.
/// Unparsable addresses are ignored.
pub fn dialed(host: &str, port: u16, to: &str, ok: bool) {
    let (Some(r), Ok(to)) = (current(), to.parse::<SocketAddr>()) else {
        return;
    };
    r.dialed(host, port, to, ok);
}

/// The console's USB adapter, once it has reported one.
pub fn usb(host: &str) -> Option<UsbLink> {
    current()?.usb(host)
}

/// Which link a connection that dialled `dialed` ("ip:port") went through. None for a console
/// that never reported a USB adapter: then it has one link and there is nothing to say.
pub fn link_of(host: &str, dialed: &str) -> Option<Link> {
    let u = usb(host)?;
    Some(match (u.addr, dialed.parse::<SocketAddr>()) {
        (Some(a), Ok(d)) if a == d => Link::Usb { mbps: u.mbps },
        _ => Link::Console,
    })
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
        fn usb(&self, host: &str) -> Option<UsbLink> {
            (host == "192.0.2.252").then(|| UsbLink {
                addr: Some("198.51.100.9:9120".parse().unwrap()),
                mbps: 2500,
                chip: "RTL8156B".into(),
            })
        }
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
        // The link label: only for a console with an adapter, and USB only at its address.
        assert_eq!(link_of("192.0.2.251", "192.0.2.251:9120"), None);
        assert_eq!(
            link_of("192.0.2.252", "198.51.100.9:9120"),
            Some(Link::Usb { mbps: 2500 })
        );
        assert_eq!(
            link_of("192.0.2.252", "192.0.2.252:9120"),
            Some(Link::Console)
        );
        assert!(!via("192.0.2.252").is_tailnet());
    }
}
