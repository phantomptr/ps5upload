//! Limits on the engine's public sockets (Play Rooms, owner rule: "limits never stop humans"):
//! the guest socket, the route probe, the room preview, the signalling relay and PS5Upload
//! Play's channel — what the home network, Tailscale and the internet can reach.
//!
//! Token buckets per client (an IPv4 address, or an IPv6 /64: `join::limit_key`), sized for
//! people: a burst well above what a person does, then a steady refill. Past one the answer is
//! "slow down" (HTTP 429 with `Retry-After`, which our clients wait out silently). Only a client
//! that keeps going after being told (refused for [`FLOOD_FOR`] or more) is blocked for a while:
//! 2 minutes, doubling on repeats up to 30, forgotten after a clean hour. This computer
//! (loopback) is never limited. Memory is bounded: past [`MAX_CLIENTS`] the idle entries go.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Clients tracked per limit at most.
pub(crate) const MAX_CLIENTS: usize = 4096;
/// Refused this long without stopping: a flood, blocked for a while.
pub(crate) const FLOOD_FOR: Duration = Duration::from_secs(30);
const BLOCK_FIRST: Duration = Duration::from_secs(120);
const BLOCK_MAX: Duration = Duration::from_secs(30 * 60);
const CLEAN_FOR: Duration = Duration::from_secs(3600);
/// No refusal for this long ends a run of refusals (a person who waited as told).
const CALM_FOR: Duration = Duration::from_secs(10);

/// A bucket's size and refill.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rate {
    pub burst: f64,
    /// Seconds per token.
    pub every_s: f64,
}

/// What a request gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    /// Wait this many seconds (at least 1).
    Slow(u64),
    /// A flood: blocked for this many seconds.
    Blocked(u64),
}

#[derive(Debug)]
struct Client {
    tokens: f64,
    at: Instant,
    /// Since when refused without a pause of [`CALM_FOR`] (a flood that keeps going), and the
    /// last refusal.
    refused_since: Option<Instant>,
    refused_at: Option<Instant>,
    blocked_until: Option<Instant>,
    blocks: u32,
    last_block: Option<Instant>,
}

/// One limit (a route or a socket kind).
pub(crate) struct Limit {
    rate: Rate,
    clients: Mutex<Option<HashMap<IpAddr, Client>>>,
}

impl Limit {
    pub(crate) const fn new(rate: Rate) -> Limit {
        Limit {
            rate,
            clients: Mutex::new(None),
        }
    }

    /// One request from `ip` now.
    pub(crate) fn check(&self, ip: IpAddr, now: Instant) -> Verdict {
        if ip.is_loopback() {
            return Verdict::Ok;
        }
        let key = crate::join::limit_key(ip);
        let r = self.rate;
        let mut g = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() >= MAX_CLIENTS && !m.contains_key(&key) {
            // Keep the ones that are blocked or still owe tokens.
            m.retain(|_, c| {
                c.blocked_until.is_some_and(|t| t > now)
                    || c.tokens + now.saturating_duration_since(c.at).as_secs_f64() / r.every_s
                        < r.burst
            });
            if m.len() >= MAX_CLIENTS {
                return Verdict::Slow(1);
            }
        }
        let c = m.entry(key).or_insert(Client {
            tokens: r.burst,
            at: now,
            refused_since: None,
            refused_at: None,
            blocked_until: None,
            blocks: 0,
            last_block: None,
        });
        if let Some(t) = c.blocked_until {
            if t > now {
                return Verdict::Blocked(t.duration_since(now).as_secs().max(1));
            }
            c.blocked_until = None;
            c.refused_since = None;
        }
        if c.last_block
            .is_some_and(|t| now.saturating_duration_since(t) >= CLEAN_FOR)
        {
            c.blocks = 0;
            c.last_block = None;
        }
        let dt = now.saturating_duration_since(c.at).as_secs_f64();
        c.tokens = (c.tokens + dt / r.every_s).min(r.burst);
        c.at = now;
        // Calm for a while since the last refusal: the run of refusals is over.
        if c.refused_at
            .is_some_and(|t| now.saturating_duration_since(t) >= CALM_FOR)
        {
            c.refused_since = None;
        }
        if c.tokens >= 1.0 {
            c.tokens -= 1.0;
            return Verdict::Ok;
        }
        c.refused_at = Some(now);
        let since = *c.refused_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= FLOOD_FOR {
            let d = BLOCK_FIRST
                .saturating_mul(1u32 << c.blocks.min(4))
                .min(BLOCK_MAX);
            c.blocks = c.blocks.saturating_add(1);
            c.last_block = Some(now);
            c.blocked_until = Some(now + d);
            crate::log_at!(
                Info,
                "together",
                "limit: a client kept going after being told to slow down: blocked for {} s",
                d.as_secs()
            );
            return Verdict::Blocked(d.as_secs());
        }
        Verdict::Slow((((1.0 - c.tokens) * r.every_s).ceil() as u64).max(1))
    }

    /// [`Self::check`] as a response to send instead (None: go on).
    pub(crate) fn gate(&self, ip: IpAddr) -> Option<Response> {
        match self.check(ip, Instant::now()) {
            Verdict::Ok => None,
            Verdict::Slow(s) | Verdict::Blocked(s) => Some(slow_down(s)),
        }
    }
}

/// HTTP 429 `{"why": "slow_down", "retry_s": n}` with `Retry-After`.
pub(crate) fn slow_down(retry_s: u64) -> Response {
    let mut r = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"why": "slow_down", "retry_s": retry_s})),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&retry_s.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// The guest socket's upgrades (`/api/join/ws`): a whole family on one router reconnecting after
/// a Wi-Fi blip is far below it.
pub(crate) static GUEST_WS: Limit = Limit::new(Rate {
    burst: 40.0,
    every_s: 1.0,
});
/// Route probes (`/api/join/probe`): a page probes a few times a minute.
pub(crate) static PROBE: Limit = Limit::new(Rate {
    burst: 30.0,
    every_s: 2.0,
});
/// PS5Upload Play's channel (`/api/play/app`) and the bridge.
pub(crate) static APP: Limit = Limit::new(Rate {
    burst: 30.0,
    every_s: 2.0,
});
/// The room preview, before its own per-address gap (join_room.rs `PreviewLimit`).
pub(crate) static PREVIEW: Limit = Limit::new(Rate {
    burst: 10.0,
    every_s: 6.0,
});
/// The LAN discovery probe (UDP, play_discovery.rs): answers per client.
pub(crate) static DISCOVERY: Limit = Limit::new(Rate {
    burst: 20.0,
    every_s: 1.0,
});

#[cfg(test)]
mod tests {
    use super::*;

    const R: Rate = Rate {
        burst: 5.0,
        every_s: 2.0,
    };

    #[test]
    fn a_person_never_notices_and_a_burst_only_slows_down() {
        let l = Limit::new(R);
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let t = Instant::now();
        for _ in 0..5 {
            assert_eq!(l.check(ip, t), Verdict::Ok);
        }
        assert_eq!(l.check(ip, t), Verdict::Slow(2));
        // Waiting what it was told works.
        assert_eq!(l.check(ip, t + Duration::from_secs(2)), Verdict::Ok);
        // Another address is not affected; loopback never limited.
        assert_eq!(l.check("192.168.1.10".parse().unwrap(), t), Verdict::Ok);
        for _ in 0..100 {
            assert_eq!(l.check("127.0.0.1".parse().unwrap(), t), Verdict::Ok);
        }
    }

    #[test]
    fn only_a_flood_that_keeps_going_is_blocked_and_it_grows() {
        let l = Limit::new(R);
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let t = Instant::now();
        for _ in 0..5 {
            l.check(ip, t);
        }
        // Refused for under 30 s: still only "slow down".
        assert!(matches!(
            l.check(ip, t + Duration::from_millis(100)),
            Verdict::Slow(_)
        ));
        let mut v = Verdict::Ok;
        let mut s = 0.0;
        while s < 31.0 {
            v = l.check(ip, t + Duration::from_secs_f64(0.2 + s));
            // Each request takes the token that came back: keep hammering faster than refill.
            s += 0.1;
        }
        assert!(matches!(v, Verdict::Blocked(_)), "{v:?}");
        let after = t + Duration::from_secs(32);
        assert!(matches!(l.check(ip, after), Verdict::Blocked(_)));
        // The block ends; a calm client is fine again.
        let later = after + BLOCK_FIRST + Duration::from_secs(10);
        assert_eq!(l.check(ip, later), Verdict::Ok);
    }

    #[test]
    fn ipv6_counts_by_its_64() {
        let l = Limit::new(R);
        let t = Instant::now();
        for i in 0..5 {
            let ip: IpAddr = format!("2001:db8:1:2::{i}").parse().unwrap();
            assert_eq!(l.check(ip, t), Verdict::Ok);
        }
        assert!(matches!(
            l.check("2001:db8:1:2::99".parse().unwrap(), t),
            Verdict::Slow(_)
        ));
    }

    #[test]
    fn slow_down_carries_retry_after() {
        let r = slow_down(7);
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()[header::RETRY_AFTER], "7");
    }
}
