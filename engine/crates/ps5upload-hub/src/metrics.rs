//! Counts only. Nothing here can hold an address, a room id or a payload: the counters are plain
//! numbers and the error kinds are a fixed list of names.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Every error the hub reports, by its wire name (`{"t":"error","why":<name>}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Not JSON, or JSON that is not an object with the expected fields.
    BadJson,
    /// `v` missing or not 1 on a `host` or `join`.
    BadVersion,
    /// Unknown `t`.
    UnknownType,
    /// A frame over the size limit.
    TooLarge,
    /// A binary frame.
    BadFrame,
    /// A room id that is not 22 base64url characters.
    BadRoom,
    /// A secret that is not 43 base64url characters.
    BadSecret,
    /// A message that does not fit the connection's role (a second `host`, a `signal` first...).
    BadState,
    /// A signal `to` the sender may not address.
    BadTarget,
    /// A signal `data` that is not an object or is over its size limit.
    BadData,
    /// The room exists and the secret does not match.
    RoomTaken,
    /// No such room, or its host is away.
    NoRoom,
    /// The room has its maximum of guests.
    RoomFull,
    /// Over a per-minute limit.
    RateLimited,
    /// The hub has its maximum of rooms (or of connections: HTTP 503 before the upgrade).
    ServerFull,
    /// This client already hosts its maximum of rooms.
    TooManyRooms,
    /// The signal's addressee has gone (not fatal).
    NoPeer,
    /// Silent past the peer timeout.
    Timeout,
    /// The browser origin is not allowed (HTTP 403 before the upgrade).
    BadOrigin,
    /// The peer did not read its messages fast enough.
    SlowPeer,
    /// This client already has its maximum of open connections (HTTP 429 before the upgrade).
    TooManyConnections,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 21] = [
        ErrorKind::BadJson,
        ErrorKind::BadVersion,
        ErrorKind::UnknownType,
        ErrorKind::TooLarge,
        ErrorKind::BadFrame,
        ErrorKind::BadRoom,
        ErrorKind::BadSecret,
        ErrorKind::BadState,
        ErrorKind::BadTarget,
        ErrorKind::BadData,
        ErrorKind::RoomTaken,
        ErrorKind::NoRoom,
        ErrorKind::RoomFull,
        ErrorKind::RateLimited,
        ErrorKind::ServerFull,
        ErrorKind::TooManyRooms,
        ErrorKind::NoPeer,
        ErrorKind::Timeout,
        ErrorKind::BadOrigin,
        ErrorKind::SlowPeer,
        ErrorKind::TooManyConnections,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::BadJson => "bad_json",
            ErrorKind::BadVersion => "bad_version",
            ErrorKind::UnknownType => "unknown_type",
            ErrorKind::TooLarge => "too_large",
            ErrorKind::BadFrame => "bad_frame",
            ErrorKind::BadRoom => "bad_room",
            ErrorKind::BadSecret => "bad_secret",
            ErrorKind::BadState => "bad_state",
            ErrorKind::BadTarget => "bad_target",
            ErrorKind::BadData => "bad_data",
            ErrorKind::RoomTaken => "room_taken",
            ErrorKind::NoRoom => "no_room",
            ErrorKind::RoomFull => "room_full",
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::ServerFull => "server_full",
            ErrorKind::TooManyRooms => "too_many_rooms",
            ErrorKind::NoPeer => "no_peer",
            ErrorKind::Timeout => "timeout",
            ErrorKind::BadOrigin => "bad_origin",
            ErrorKind::SlowPeer => "slow_peer",
            ErrorKind::TooManyConnections => "too_many_connections",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|k| *k == self)
            .expect("every kind is in ALL")
    }
}

/// Which limit refused something (`hub_limited_total{limit=...}`). The error kinds
/// (`rate_limited`, `too_many_connections`...) say only that a limit fired; this says which one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// `join` attempts per client per window.
    JoinRate,
    /// A room's table of guest clients' budgets is full (a new client waits for windows to end).
    RoomClients,
    /// A `join` from a client whose signal budget in the room is used up.
    ClientBudget,
    /// One guest's signals per window.
    GuestSignals,
    /// One client's signals in a room per window, its guests together.
    ClientSignals,
    /// A host's signals per window (dropped, not closed).
    HostSignals,
    /// Open connections per client (HTTP 429).
    ConnectionsPerClient,
    /// Open connections per IPv6 /48 (HTTP 429).
    ConnectionsPerV6Site,
    /// Open connections on the hub (HTTP 503).
    Connections,
    /// Rooms on the hub.
    Rooms,
    /// Rooms per client.
    RoomsPerClient,
}

impl Limit {
    pub const ALL: [Limit; 11] = [
        Limit::JoinRate,
        Limit::RoomClients,
        Limit::ClientBudget,
        Limit::GuestSignals,
        Limit::ClientSignals,
        Limit::HostSignals,
        Limit::ConnectionsPerClient,
        Limit::ConnectionsPerV6Site,
        Limit::Connections,
        Limit::Rooms,
        Limit::RoomsPerClient,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Limit::JoinRate => "join_rate",
            Limit::RoomClients => "room_clients",
            Limit::ClientBudget => "client_budget",
            Limit::GuestSignals => "guest_signals",
            Limit::ClientSignals => "client_signals",
            Limit::HostSignals => "host_signals",
            Limit::ConnectionsPerClient => "connections_per_client",
            Limit::ConnectionsPerV6Site => "connections_per_v6_site",
            Limit::Connections => "connections",
            Limit::Rooms => "rooms",
            Limit::RoomsPerClient => "rooms_per_client",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|k| *k == self)
            .expect("every limit is in ALL")
    }
}

/// A plain HTTP request that was not a WebSocket to `/ws` or a health check, by a fixed class of
/// its path (`hub_http_strays_total{class=...}`). Only the class is kept: never the path, its
/// query, or who asked. The apps and pages only ever open `/ws`; these are scanners and crawlers
/// that found the name (certificate transparency logs publish it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stray {
    /// `/` (404).
    Root,
    /// `/favicon.ico` (404).
    Favicon,
    /// `/robots.txt` (answered: 200, everything disallowed).
    Robots,
    /// `/.well-known/...` (404).
    WellKnown,
    /// `/ws` that is not a WebSocket upgrade: another method, or the upgrade headers missing.
    /// Answered as before (405, 400 or 426).
    WsNotWebSocket,
    /// Anything else (404).
    Other,
}

impl Stray {
    pub const ALL: [Stray; 6] = [
        Stray::Root,
        Stray::Favicon,
        Stray::Robots,
        Stray::WellKnown,
        Stray::WsNotWebSocket,
        Stray::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Stray::Root => "root",
            Stray::Favicon => "favicon",
            Stray::Robots => "robots",
            Stray::WellKnown => "well_known",
            Stray::WsNotWebSocket => "ws_not_websocket",
            Stray::Other => "other",
        }
    }

    /// The class of a path nothing serves.
    pub fn of_path(path: &str) -> Stray {
        match path {
            "/" | "" => Stray::Root,
            "/favicon.ico" => Stray::Favicon,
            "/robots.txt" => Stray::Robots,
            p if p.starts_with("/.well-known/") => Stray::WellKnown,
            _ => Stray::Other,
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|k| *k == self)
            .expect("every class is in ALL")
    }
}

#[derive(Default)]
pub struct Metrics {
    pub hosts: AtomicU64,
    pub reclaims: AtomicU64,
    pub joins: AtomicU64,
    pub signals: AtomicU64,
    pub host_signals_dropped: AtomicU64,
    pub rooms_expired: AtomicU64,
    /// Rooms their host closed (`leave`). Rooms opened = closed + expired + those still open.
    pub rooms_closed: AtomicU64,
    pub guests_expired: AtomicU64,
    pub stun_requests: AtomicU64,
    pub stun_ignored: AtomicU64,
    pub stun_limited: AtomicU64,
    errors: [AtomicU64; ErrorKind::ALL.len()],
    limited: [AtomicU64; Limit::ALL.len()],
    strays: [AtomicU64; Stray::ALL.len()],
}

/// The live numbers `/metrics` reports next to the counters.
pub struct Gauges {
    pub rooms: usize,
    pub hosted_rooms: usize,
    pub guests: usize,
    pub connections: usize,
}

impl Metrics {
    pub fn error(&self, kind: ErrorKind) {
        self.errors[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn errors(&self, kind: ErrorKind) -> u64 {
        self.errors[kind.index()].load(Ordering::Relaxed)
    }

    pub fn errors_total(&self) -> u64 {
        self.errors.iter().map(|e| e.load(Ordering::Relaxed)).sum()
    }

    pub fn limited(&self, limit: Limit) {
        self.limited[limit.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn limited_by(&self, limit: Limit) -> u64 {
        self.limited[limit.index()].load(Ordering::Relaxed)
    }

    pub fn stray(&self, class: Stray) {
        self.strays[class.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn strays(&self, class: Stray) -> u64 {
        self.strays[class.index()].load(Ordering::Relaxed)
    }

    pub fn strays_total(&self) -> u64 {
        self.strays.iter().map(|e| e.load(Ordering::Relaxed)).sum()
    }

    /// Requests refused by a limit (rate, size of the hub, rooms or connections per client), and
    /// host signals dropped by the host's.
    pub fn refused_total(&self) -> u64 {
        [
            ErrorKind::RateLimited,
            ErrorKind::ServerFull,
            ErrorKind::TooManyRooms,
            ErrorKind::TooManyConnections,
        ]
        .iter()
        .map(|k| self.errors(*k))
        .sum::<u64>()
            + self.stun_limited.load(Ordering::Relaxed)
            + self.host_signals_dropped.load(Ordering::Relaxed)
    }

    /// A number that grows whenever any counter does: the summary skips an interval in which it
    /// did not. Strays are left out: scanners alone do not make an idle hub log.
    pub fn activity(&self) -> u64 {
        [
            &self.hosts,
            &self.reclaims,
            &self.joins,
            &self.signals,
            &self.host_signals_dropped,
            &self.rooms_expired,
            &self.rooms_closed,
            &self.guests_expired,
            &self.stun_requests,
            &self.stun_ignored,
            &self.stun_limited,
        ]
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum::<u64>()
            + self.errors_total()
    }

    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// Prometheus text format.
    pub fn render(&self, g: &Gauges) -> String {
        let mut s = String::new();
        let mut gauge = |name: &str, help: &str, v: u64| {
            let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}");
        };
        gauge("hub_rooms", "Rooms in memory.", g.rooms as u64);
        gauge(
            "hub_rooms_hosted",
            "Rooms with a host connected.",
            g.hosted_rooms as u64,
        );
        gauge("hub_guests", "Guests connected.", g.guests as u64);
        gauge(
            "hub_connections",
            "WebSocket connections open.",
            g.connections as u64,
        );
        let mut counter = |name: &str, help: &str, c: &AtomicU64| {
            let _ = writeln!(
                s,
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {}",
                c.load(Ordering::Relaxed)
            );
        };
        counter("hub_hosts_total", "Rooms opened.", &self.hosts);
        counter(
            "hub_reclaims_total",
            "Rooms reclaimed by their host.",
            &self.reclaims,
        );
        counter("hub_joins_total", "Guests joined.", &self.joins);
        counter("hub_signals_total", "Signals forwarded.", &self.signals);
        counter(
            "hub_host_signals_dropped_total",
            "Host signals not forwarded: over the host's limit.",
            &self.host_signals_dropped,
        );
        counter(
            "hub_rooms_expired_total",
            "Rooms removed after their host stayed away.",
            &self.rooms_expired,
        );
        counter(
            "hub_rooms_closed_total",
            "Rooms their host closed (leave).",
            &self.rooms_closed,
        );
        counter(
            "hub_guests_expired_total",
            "Guests removed for staying in a room past the longest a join takes.",
            &self.guests_expired,
        );
        counter(
            "hub_stun_requests_total",
            "STUN Binding requests answered.",
            &self.stun_requests,
        );
        counter(
            "hub_stun_ignored_total",
            "UDP datagrams that were not a STUN Binding request.",
            &self.stun_ignored,
        );
        counter(
            "hub_stun_limited_total",
            "STUN requests dropped by the rate limit.",
            &self.stun_limited,
        );
        let _ = writeln!(
            s,
            "# HELP hub_errors_total Protocol errors by kind.\n# TYPE hub_errors_total counter"
        );
        for k in ErrorKind::ALL {
            let _ = writeln!(
                s,
                "hub_errors_total{{kind=\"{}\"}} {}",
                k.as_str(),
                self.errors(k)
            );
        }
        let _ = writeln!(
            s,
            "# HELP hub_limited_total Refusals (and dropped host signals) by the limit that fired.\n\
             # TYPE hub_limited_total counter"
        );
        for l in Limit::ALL {
            let _ = writeln!(
                s,
                "hub_limited_total{{limit=\"{}\"}} {}",
                l.as_str(),
                self.limited_by(l)
            );
        }
        let _ = writeln!(
            s,
            "# HELP hub_http_strays_total Plain HTTP requests other than a WebSocket to /ws or a \
             health check, by path class (no paths kept).\n# TYPE hub_http_strays_total counter"
        );
        for c in Stray::ALL {
            let _ = writeln!(
                s,
                "hub_http_strays_total{{class=\"{}\"}} {}",
                c.as_str(),
                self.strays(c)
            );
        }
        s
    }
}
