//! Hub settings. Every limit has a default that suits one small VPS; `main` reads overrides from
//! `HUB_*` environment variables (see [`Config::from_env`]) so the systemd unit can set them in
//! an environment file.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::throttle::{Escalation, Rule, Rules};

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
    /// Signals one guest client (an address, see `limits.rs`) may send in one room per window,
    /// all its guests there together, those that left included: joining again does not refill
    /// it. Room for a few guests from one home (or behind one carrier-grade NAT).
    pub max_signals_per_guest_client: u32,
    /// Signals a host may send per window, to all its guests together. Past it they are
    /// dropped, not forwarded; the host stays.
    pub max_signals_per_host: u32,
    pub max_joins_per_ip: u32,
    pub max_rooms: usize,
    pub max_rooms_per_ip: usize,
    pub max_stun_per_ip: u32,
    /// WebSocket connections open at once, in all (refused with HTTP 503 past it).
    pub max_connections: usize,
    /// WebSocket connections open at once from one client (refused with HTTP 429 past it).
    pub max_connections_per_ip: usize,
    /// WebSocket connections open at once from one IPv6 /48 (refused with HTTP 429 past it).
    /// A /48 holds 65,536 of the /64s the per-client limit counts one by one.
    pub max_connections_per_v6_site: usize,
    /// A connection must say `host` or `join` this soon after it opens, or it is closed.
    pub hello_timeout: Duration,
    /// The window the per-minute limits count over.
    pub rate_window: Duration,
    /// A room with no host for this long is removed.
    pub room_idle_ttl: Duration,
    /// A guest still in its room this long after it joined is removed (it gets `timeout`). A
    /// client leaves the hub once its connection to the host is up, within a minute and a half
    /// at most (a page holds the place it took early for a minute, ICE gets 15 seconds), so a
    /// guest past this has not made a session: idle sockets cannot keep a room full.
    pub guest_max_age: Duration,
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
    /// Play Rooms: cards, the list, the friends lookup, previews and knocks (`rooms.rs`).
    pub rooms: RoomsConfig,
}

/// Play Rooms settings (design: play-rooms 9.x). Every number is a default for the Fly free plan;
/// `HUB_*` variables override them (see [`Config::from_lookup`]); the switches and the blocklist
/// can also change at run time (`HUB_CONTROL_FILE`, `POST /admin/control`).
#[derive(Clone, Debug)]
pub struct RoomsConfig {
    /// `HUB_ROOMS`: cards, the list and the friends lookup. Off: none of them.
    pub enabled: bool,
    /// `HUB_PREVIEW`: room previews relayed through the hub.
    pub previews: bool,
    /// `HUB_KNOCKS`: lobby rooms (where knocks go).
    pub knocks: bool,
    /// `HUB_MIN_PROTO`: rooms below this protocol are not listed; `HUB_LATEST_PROTO` is the newest
    /// one (both go out in the list as `proto: {min, latest}`).
    pub min_proto: u32,
    pub latest_proto: u32,
    /// `HUB_APP_LATEST`: the newest PS5Upload version, for update nudges (`app: {latest}`).
    pub app_latest: Option<String>,
    /// Engines accept guests this many protocols apart ("Works with my version").
    pub proto_window: u32,
    /// `HUB_BLOCKLIST`: control lines (`tag 4F2A; ip 203.0.113.0/24; word ...`) applied at start.
    pub blocklist: String,
    /// `HUB_CONTROL_FILE`: control lines re-read whenever the file changes.
    pub control_file: Option<std::path::PathBuf>,
    /// `HUB_ADMIN_TOKEN`: enables `GET/POST /admin/control` on the private metrics listener.
    pub admin_token: Option<String>,
    /// `HUB_GEO_FILE`: IP → country ranges (CSV `start,end,CC`); unset: no countries.
    pub geo_file: Option<std::path::PathBuf>,
    /// Cards on the hub (new ones refused past it, none evicted).
    pub max_ads: usize,
    pub max_ads_per_ip: usize,
    /// Card creates per client per hour.
    pub creates_per_hour: u32,
    /// Leading zero bits the create's proof of work needs.
    pub pow_bits: u32,
    /// A card is withdrawn this long after it was created (the engine creates it again).
    pub ad_max_age: Duration,
    /// ... or when its host has not renewed it (any `ad`) for this long.
    pub ad_renew: Duration,
    pub ad_max_bytes: usize,
    /// Card updates (renewals included) per card.
    pub ad_updates: Rule,
    /// Guests a lobby room takes at once (invite rooms: `max_guests_per_room`).
    pub max_lobby_guests: usize,
    pub list_cache: Duration,
    pub list_cache_cutback: Duration,
    pub list_cache_entries: usize,
    pub page_items: usize,
    pub page_items_cutback: usize,
    pub page_bytes: usize,
    /// The deepest a list may be paged (items).
    pub max_offset: usize,
    pub preview_ttl: Duration,
    pub preview_wait: Duration,
    pub preview_max_bytes: usize,
    /// How often apps should look friends' rooms up (`every_s`), normally and in cut-back mode.
    pub status_every: Duration,
    pub status_every_cutback: Duration,
    pub status_max_ids: usize,
    pub knock_max_bytes: usize,
    /// Knocks per client per room ("5 a 10 minutes": a password retried a few times is fine).
    pub knock_client: Rule,
    /// Per-client buckets by class and when refusals escalate (`throttle.rs`).
    pub throttle: Rules,
    pub escalation: Escalation,
    /// Messages one WebSocket may send (any kind): far above any app, below a flood.
    pub conn_messages: Rule,
    /// `HUB_MEM_CUTBACK_MIB`: resident memory that starts cut-back mode;
    /// `HUB_MEM_REFUSE_MIB`: new sockets are refused (503) past it.
    pub mem_cutback_bytes: u64,
    pub mem_refuse_bytes: u64,
    /// `HUB_OUT_BUDGET_GB`: outbound bytes a month; a projection past it starts cut-back mode.
    pub out_budget_month: u64,
    /// How often cards, caches, budgets and the control file are checked.
    pub tick: Duration,
}

impl Default for RoomsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            previews: true,
            knocks: true,
            min_proto: 2,
            latest_proto: 2,
            app_latest: None,
            proto_window: 2,
            blocklist: String::new(),
            control_file: None,
            admin_token: None,
            geo_file: None,
            max_ads: 500,
            max_ads_per_ip: 2,
            creates_per_hour: 10,
            pow_bits: 18,
            ad_max_age: Duration::from_secs(12 * 3600),
            // "Renew at most every 2 minutes" with 30 s for a slow network.
            ad_renew: Duration::from_secs(150),
            ad_max_bytes: 1024,
            ad_updates: Rule::new(6.0, 0.1),
            max_lobby_guests: 16,
            list_cache: Duration::from_secs(5),
            list_cache_cutback: Duration::from_secs(30),
            list_cache_entries: 512,
            page_items: 12,
            page_items_cutback: 6,
            page_bytes: 2048,
            max_offset: 480,
            preview_ttl: Duration::from_secs(10),
            preview_wait: Duration::from_secs(3),
            preview_max_bytes: 2048,
            status_every: Duration::from_secs(60),
            status_every_cutback: Duration::from_secs(300),
            status_max_ids: 16,
            knock_max_bytes: 2048,
            knock_client: Rule::per_min(5.0, 0.5),
            throttle: Rules::default(),
            escalation: Escalation::default(),
            conn_messages: Rule::new(100.0, 30.0),
            mem_cutback_bytes: 200 << 20,
            mem_refuse_bytes: 220 << 20,
            out_budget_month: 100_000_000_000,
            tick: Duration::from_secs(2),
        }
    }
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
            // One try of a guest: its offer, then every ICE candidate its browser finds, one
            // signal each. Firefox on one network found 20 for the Play Together page as it was
            // (a data channel, a video and an audio track: a set of candidates for each), so with
            // the offer it went past the old 20 and was closed before it could connect; a home
            // with IPv6 or a VPN finds twice as many. The pages now leave out the candidates the
            // engine cannot use (about half); older pages stay in use.
            max_signals_per_guest: 60,
            // Room for a home of three or four, each trying twice in the minute with such a page.
            max_signals_per_guest_client: 300,
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
            // Eight full clients' worth: room for a campus or a shared provider block, and
            // an eighth of the hub at most for one block.
            max_connections_per_v6_site: 256,
            hello_timeout: Duration::from_secs(10),
            rate_window: Duration::from_secs(60),
            room_idle_ttl: Duration::from_secs(120),
            // Twice the longest a real guest stays (about 75 s), with room for a slow network.
            guest_max_age: Duration::from_secs(180),
            ping_interval: Duration::from_secs(20),
            peer_timeout: Duration::from_secs(60),
            sweep_interval: Duration::from_secs(30),
            max_frame_bytes: 16 * 1024,
            max_signal_data_bytes: 12 * 1024,
            max_tracked_ips: 100_000,
            summary_interval: Duration::from_secs(3600),
            rooms: RoomsConfig::default(),
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
        num(
            get,
            "HUB_MAX_SIGNALS_PER_GUEST_CLIENT",
            &mut c.max_signals_per_guest_client,
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
        num(
            get,
            "HUB_MAX_CONNECTIONS_PER_V6_SITE",
            &mut c.max_connections_per_v6_site,
        )?;
        secs(get, "HUB_HELLO_SECS", &mut c.hello_timeout)?;
        secs(get, "HUB_RATE_WINDOW_SECS", &mut c.rate_window)?;
        secs(get, "HUB_ROOM_IDLE_SECS", &mut c.room_idle_ttl)?;
        secs(get, "HUB_GUEST_MAX_SECS", &mut c.guest_max_age)?;
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
        if c.guest_max_age.is_zero() {
            return Err("HUB_GUEST_MAX_SECS must be above zero".into());
        }
        rooms_from_lookup(get, &mut c.rooms)?;
        Ok(c)
    }
}

fn on_off(get: &dyn Fn(&str) -> Option<String>, key: &str, into: &mut bool) -> Result<(), String> {
    if let Some(v) = get(key) {
        *into = match v.trim().to_ascii_lowercase().as_str() {
            "on" | "1" | "true" | "yes" => true,
            "off" | "0" | "false" | "no" => false,
            _ => return Err(format!("{key}: not on or off")),
        };
    }
    Ok(())
}

fn rooms_from_lookup(
    get: &dyn Fn(&str) -> Option<String>,
    r: &mut RoomsConfig,
) -> Result<(), String> {
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
    let path = |key: &str| {
        get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    };
    on_off(get, "HUB_ROOMS", &mut r.enabled)?;
    on_off(get, "HUB_PREVIEW", &mut r.previews)?;
    on_off(get, "HUB_KNOCKS", &mut r.knocks)?;
    num(get, "HUB_MIN_PROTO", &mut r.min_proto)?;
    num(get, "HUB_LATEST_PROTO", &mut r.latest_proto)?;
    if let Some(v) = get("HUB_APP_LATEST").map(|v| v.trim().to_string()) {
        r.app_latest = (!v.is_empty()).then_some(v);
    }
    if let Some(v) = get("HUB_BLOCKLIST") {
        r.blocklist = v;
    }
    r.control_file = path("HUB_CONTROL_FILE").or(r.control_file.take());
    r.geo_file = path("HUB_GEO_FILE").or(r.geo_file.take());
    if let Some(v) = get("HUB_ADMIN_TOKEN").map(|v| v.trim().to_string()) {
        // A token too short to stand guessing is no token.
        if v.len() < 16 {
            return Err("HUB_ADMIN_TOKEN: at least 16 characters".into());
        }
        r.admin_token = Some(v);
    }
    num(get, "HUB_MAX_ADS", &mut r.max_ads)?;
    num(get, "HUB_MAX_ADS_PER_IP", &mut r.max_ads_per_ip)?;
    num(get, "HUB_AD_CREATES_PER_HOUR", &mut r.creates_per_hour)?;
    num(get, "HUB_AD_POW_BITS", &mut r.pow_bits)?;
    secs(get, "HUB_AD_MAX_SECS", &mut r.ad_max_age)?;
    secs(get, "HUB_AD_RENEW_SECS", &mut r.ad_renew)?;
    num(get, "HUB_LOBBY_GUESTS", &mut r.max_lobby_guests)?;
    secs(get, "HUB_LIST_CACHE_SECS", &mut r.list_cache)?;
    secs(
        get,
        "HUB_LIST_CACHE_CUTBACK_SECS",
        &mut r.list_cache_cutback,
    )?;
    num(get, "HUB_LIST_PAGE_ITEMS", &mut r.page_items)?;
    num(get, "HUB_LIST_PAGE_BYTES", &mut r.page_bytes)?;
    let mut mib = r.mem_cutback_bytes >> 20;
    num(get, "HUB_MEM_CUTBACK_MIB", &mut mib)?;
    r.mem_cutback_bytes = mib << 20;
    let mut mib = r.mem_refuse_bytes >> 20;
    num(get, "HUB_MEM_REFUSE_MIB", &mut mib)?;
    r.mem_refuse_bytes = mib << 20;
    let mut gb = r.out_budget_month / 1_000_000_000;
    num(get, "HUB_OUT_BUDGET_GB", &mut gb)?;
    r.out_budget_month = gb.saturating_mul(1_000_000_000);
    if r.pow_bits > 28 {
        return Err("HUB_AD_POW_BITS: at most 28".into());
    }
    if r.min_proto > r.latest_proto {
        return Err("HUB_MIN_PROTO must not be above HUB_LATEST_PROTO".into());
    }
    if r.page_items == 0 || r.ad_renew.is_zero() || r.ad_max_age.is_zero() {
        return Err(
            "HUB_LIST_PAGE_ITEMS, HUB_AD_RENEW_SECS and HUB_AD_MAX_SECS must be above zero".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod rooms_tests {
    use super::*;

    #[test]
    fn rooms_env_overrides_and_checks() {
        let c = Config::from_lookup(|k| match k {
            "HUB_ROOMS" => Some("off".into()),
            "HUB_MIN_PROTO" => Some("1".into()),
            "HUB_OUT_BUDGET_GB" => Some("5".into()),
            "HUB_MEM_CUTBACK_MIB" => Some("100".into()),
            "HUB_BLOCKLIST" => Some("tag 4F2A".into()),
            _ => None,
        })
        .unwrap();
        assert!(!c.rooms.enabled && c.rooms.previews);
        assert_eq!(c.rooms.min_proto, 1);
        assert_eq!(c.rooms.out_budget_month, 5_000_000_000);
        assert_eq!(c.rooms.mem_cutback_bytes, 100 << 20);
        assert_eq!(c.rooms.blocklist, "tag 4F2A");
        for (k, v) in [
            ("HUB_ROOMS", "maybe"),
            ("HUB_MIN_PROTO", "9"),
            ("HUB_ADMIN_TOKEN", "short"),
            ("HUB_AD_POW_BITS", "40"),
        ] {
            assert!(
                Config::from_lookup(|x| (x == k).then(|| v.to_string())).is_err(),
                "{k}"
            );
        }
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
