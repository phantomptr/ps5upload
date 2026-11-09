//! Hub settings. Every limit has a default that suits one small VPS; `main` reads overrides from
//! `HUB_*` environment variables (see [`Config::from_env`]) so the systemd unit can set them in
//! an environment file.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// The guest page's origin.
pub const GUEST_PAGE_ORIGIN: &str = "https://ps5upload.phantomptr.com";

/// The PS5Upload app's own pages (Tauri's WebView): `tauri://localhost` on macOS and iOS,
/// `http://tauri.localhost` on Android and Windows (`https://` when the app opts into it). The
/// app joins an internet invite from its page, so these are allowed by default next to the guest
/// page. No website can have these origins, so this does not open the hub to other sites.
pub const APP_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// An address block (`10.0.0.0/8`, `fdaa::/16`); a bare address is a /32 or /128.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let m = mask32(self.prefix);
                u32::from(a) & m == u32::from(b) & m
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let m = mask128(self.prefix);
                u128::from(a) & m == u128::from(b) & m
            }
            _ => false,
        }
    }
}

fn mask32(p: u8) -> u32 {
    if p == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(p.min(32)))
    }
}

fn mask128(p: u8) -> u128 {
    if p == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(p.min(128)))
    }
}

impl std::str::FromStr for Cidr {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.trim().parse().map_err(|_| ())?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.trim().parse::<u8>().map_err(|_| ())?,
            None => max,
        };
        if prefix > max {
            return Err(());
        }
        Ok(Cidr { addr, prefix })
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Where the WebSocket/HTTP server listens. Behind Caddy: a loopback address.
    pub http_listen: SocketAddr,
    /// A separate listener for `/metrics` (and `/health`). Set: `/metrics` is served only there,
    /// not on `http_listen` (on Fly.io the public port carries `/ws`, and Fly scrapes this one
    /// over its private network). Unset: `/metrics` is on `http_listen` and the reverse proxy
    /// keeps it private.
    pub metrics_listen: Option<SocketAddr>,
    /// UDP addresses for the optional STUN server; empty (the default) runs none, and clients
    /// use public STUN servers. `[::]:3478` is dual stack on Linux (IPv4 arrives as IPv4-mapped
    /// IPv6); an address that cannot be bound is counted in the start-up log and skipped.
    pub stun_listen: Vec<SocketAddr>,
    /// Browser origins allowed to open `/ws`. A request without an `Origin` header (the engine)
    /// is always allowed.
    pub allowed_origins: Vec<String>,
    /// Peers whose client-IP header is believed (the reverse proxy), as addresses or CIDR
    /// blocks. Empty: the header is ignored and the TCP peer address is the client.
    pub trusted_proxies: Vec<Cidr>,
    /// The header a trusted proxy sets; its last (rightmost) entry is the client.
    pub client_ip_header: String,

    pub max_guests_per_room: usize,
    pub max_signals_per_guest: u32,
    pub max_signals_per_host: u32,
    pub max_joins_per_ip: u32,
    pub max_rooms: usize,
    pub max_rooms_per_ip: usize,
    pub max_stun_per_ip: u32,
    /// WebSocket connections open at once, in all (refused with HTTP 503 past it).
    pub max_connections: usize,
    /// WebSocket connections open at once from one client (refused with HTTP 429 past it).
    pub max_connections_per_ip: usize,
    /// A connection must say `host` or `join` this soon after it opens, or it is closed.
    pub hello_timeout: Duration,
    /// The window the per-minute limits count over.
    pub rate_window: Duration,
    /// A room with no host for this long is removed.
    pub room_idle_ttl: Duration,
    /// WebSocket ping cadence. A peer heard from within the last interval is not pinged (the
    /// engine pings the hub itself), so a quiet peer is pinged at most every two intervals.
    pub ping_interval: Duration,
    /// A peer that sent nothing (not even a pong) for this long is dropped. Must be more than
    /// twice `ping_interval`.
    pub peer_timeout: Duration,
    /// How often expired rooms and rate counters are swept.
    pub sweep_interval: Duration,
    /// Largest accepted text frame.
    pub max_frame_bytes: usize,
    /// Largest `data` of a signal, as serialized JSON.
    pub max_signal_data_bytes: usize,
    /// How many IPs the rate tables track before refusing new ones until the next sweep.
    pub max_tracked_ips: usize,
    /// How often a one-line count summary is logged (zero: never). Nothing is logged for an
    /// interval in which nothing happened.
    pub summary_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            http_listen: "127.0.0.1:8787".parse().expect("static address"),
            metrics_listen: None,
            stun_listen: Vec::new(),
            allowed_origins: std::iter::once(GUEST_PAGE_ORIGIN)
                .chain(APP_ORIGINS)
                .map(str::to_string)
                .collect(),
            trusted_proxies: Vec::new(),
            client_ip_header: "x-forwarded-for".to_string(),
            max_guests_per_room: 8,
            max_signals_per_guest: 20,
            max_signals_per_host: 200,
            max_joins_per_ip: 30,
            max_rooms: 5_000,
            // A home behind carrier-grade NAT shares its IPv4 with others, and an engine that
            // restarts opens new rooms while its old ones wait out their reclaim time.
            max_rooms_per_ip: 8,
            max_stun_per_ip: 60,
            // Fly's hard_limit is 2000 too; the hub enforces it itself anywhere else.
            max_connections: 2_000,
            max_connections_per_ip: 32,
            hello_timeout: Duration::from_secs(10),
            rate_window: Duration::from_secs(60),
            room_idle_ttl: Duration::from_secs(120),
            ping_interval: Duration::from_secs(20),
            peer_timeout: Duration::from_secs(60),
            sweep_interval: Duration::from_secs(30),
            max_frame_bytes: 16 * 1024,
            max_signal_data_bytes: 12 * 1024,
            max_tracked_ips: 100_000,
            summary_interval: Duration::from_secs(3600),
        }
    }
}

impl Config {
    /// Defaults overridden by `HUB_*` variables. A variable that does not parse is an error
    /// naming the variable (never its value, which could be an address).
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut c = Config::default();
        fn num<T: std::str::FromStr>(
            get: &dyn Fn(&str) -> Option<String>,
            key: &str,
            into: &mut T,
        ) -> Result<(), String> {
            if let Some(v) = get(key) {
                *into = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("{key}: not a number"))?;
            }
            Ok(())
        }
        fn secs(
            get: &dyn Fn(&str) -> Option<String>,
            key: &str,
            into: &mut Duration,
        ) -> Result<(), String> {
            let mut s = into.as_secs();
            num(get, key, &mut s)?;
            *into = Duration::from_secs(s);
            Ok(())
        }
        fn list(v: &str) -> impl Iterator<Item = &str> {
            v.split(',').map(str::trim).filter(|s| !s.is_empty())
        }
        let get: &dyn Fn(&str) -> Option<String> = &get;
        if let Some(v) = get("HUB_HTTP_LISTEN") {
            c.http_listen = v
                .trim()
                .parse()
                .map_err(|_| "HUB_HTTP_LISTEN: not a socket address")?;
        }
        if let Some(v) = get("HUB_METRICS_LISTEN").filter(|v| !v.trim().is_empty()) {
            c.metrics_listen = Some(
                v.trim()
                    .parse()
                    .map_err(|_| "HUB_METRICS_LISTEN: not a socket address")?,
            );
        }
        if let Some(v) = get("HUB_STUN_LISTEN") {
            c.stun_listen = list(&v)
                .map(|s| s.parse())
                .collect::<Result<_, _>>()
                .map_err(|_| "HUB_STUN_LISTEN: not a list of socket addresses")?;
        }
        if let Some(v) = get("HUB_ALLOWED_ORIGINS") {
            c.allowed_origins = list(&v).map(str::to_string).collect();
        }
        if let Some(v) = get("HUB_TRUSTED_PROXIES") {
            c.trusted_proxies = list(&v)
                .map(|s| s.parse())
                .collect::<Result<_, _>>()
                .map_err(|_| "HUB_TRUSTED_PROXIES: not a list of IP addresses or CIDR blocks")?;
        }
        if let Some(v) = get("HUB_CLIENT_IP_HEADER") {
            c.client_ip_header = v.trim().to_ascii_lowercase();
        }
        num(get, "HUB_MAX_GUESTS_PER_ROOM", &mut c.max_guests_per_room)?;
        num(
            get,
            "HUB_MAX_SIGNALS_PER_GUEST",
            &mut c.max_signals_per_guest,
        )?;
        num(get, "HUB_MAX_SIGNALS_PER_HOST", &mut c.max_signals_per_host)?;
        num(get, "HUB_MAX_JOINS_PER_IP", &mut c.max_joins_per_ip)?;
        num(get, "HUB_MAX_ROOMS", &mut c.max_rooms)?;
        num(get, "HUB_MAX_ROOMS_PER_IP", &mut c.max_rooms_per_ip)?;
        num(get, "HUB_MAX_STUN_PER_IP", &mut c.max_stun_per_ip)?;
        num(get, "HUB_MAX_TRACKED_IPS", &mut c.max_tracked_ips)?;
        num(get, "HUB_MAX_CONNECTIONS", &mut c.max_connections)?;
        num(
            get,
            "HUB_MAX_CONNECTIONS_PER_IP",
            &mut c.max_connections_per_ip,
        )?;
        secs(get, "HUB_HELLO_SECS", &mut c.hello_timeout)?;
        secs(get, "HUB_RATE_WINDOW_SECS", &mut c.rate_window)?;
        secs(get, "HUB_ROOM_IDLE_SECS", &mut c.room_idle_ttl)?;
        secs(get, "HUB_PING_SECS", &mut c.ping_interval)?;
        secs(get, "HUB_PEER_TIMEOUT_SECS", &mut c.peer_timeout)?;
        secs(get, "HUB_SUMMARY_SECS", &mut c.summary_interval)?;
        if c.ping_interval.is_zero() || c.peer_timeout <= c.ping_interval * 2 {
            return Err("HUB_PEER_TIMEOUT_SECS must be more than twice HUB_PING_SECS".into());
        }
        if c.hello_timeout.is_zero() {
            return Err("HUB_HELLO_SECS must be above zero".into());
        }
        if c.rate_window.is_zero() {
            return Err("HUB_RATE_WINDOW_SECS must be above zero".into());
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr() {
        let c: Cidr = "10.0.0.0/8".parse().unwrap();
        assert!(c.contains("10.9.8.7".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        let all: Cidr = "::/0".parse().unwrap();
        assert!(all.contains("2001:db8::1".parse().unwrap()));
        assert!(!all.contains("1.2.3.4".parse().unwrap()));
        let one: Cidr = "127.0.0.1".parse().unwrap();
        assert!(one.contains("127.0.0.1".parse().unwrap()));
        assert!(!one.contains("127.0.0.2".parse().unwrap()));
        assert!("1.2.3.4/33".parse::<Cidr>().is_err());
    }

    #[test]
    fn env_overrides() {
        let c = Config::from_lookup(|k| match k {
            "HUB_MAX_GUESTS_PER_ROOM" => Some("4".into()),
            "HUB_TRUSTED_PROXIES" => Some("127.0.0.1, ::1".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(c.max_guests_per_room, 4);
        assert_eq!(c.trusted_proxies.len(), 2);
        assert!(c.stun_listen.is_empty());
        assert!(Config::from_lookup(|k| (k == "HUB_MAX_ROOMS").then(|| "x".into())).is_err());
        // A peer must get two ping intervals before it counts as silent.
        assert!(
            Config::from_lookup(|k| (k == "HUB_PEER_TIMEOUT_SECS").then(|| "40".into())).is_err()
        );
        assert!(
            Config::from_lookup(|k| (k == "HUB_PEER_TIMEOUT_SECS").then(|| "41".into())).is_ok()
        );
    }
}
