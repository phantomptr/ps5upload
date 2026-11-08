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

#[derive(Default)]
pub struct Metrics {
    pub hosts: AtomicU64,
    pub reclaims: AtomicU64,
    pub joins: AtomicU64,
    pub signals: AtomicU64,
    pub rooms_expired: AtomicU64,
    pub stun_requests: AtomicU64,
    pub stun_ignored: AtomicU64,
    pub stun_limited: AtomicU64,
    errors: [AtomicU64; ErrorKind::ALL.len()],
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

    /// Requests refused by a limit (rate, size of the hub, rooms or connections per client).
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
    }

    /// A number that grows whenever any counter does: the summary skips an interval in which it
    /// did not.
    pub fn activity(&self) -> u64 {
        [
            &self.hosts,
            &self.reclaims,
            &self.joins,
            &self.signals,
            &self.rooms_expired,
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
            "hub_rooms_expired_total",
            "Rooms removed after their host stayed away.",
            &self.rooms_expired,
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
        s
    }
}
