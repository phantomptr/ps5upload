//! PS5Upload Play finds this engine without the helper: a small UDP responder on the LAN.
//!
//! The app sends one datagram to UDP port [`PORT`] (19261; broadcast, or a remembered address):
//! [`PROBE`] (`PS5UPLOAD_PLAY_DISCOVER v1`), padded with anything to at least 512 bytes. The
//! engine answers with one datagram of JSON, `{"name", "port", "version", "id", "together"}`: this
//! device's name, the engine's HTTP port (the app then dials `/api/play/app` there), its version, a
//! short id that tells two engines apart, and how many Play Togethers are open here with their host
//! playing (the app's home screen then offers "Join <name>'s game": one press, and the host still
//! lets them in). Nothing else is answered.
//!
//! What keeps it from being misused:
//! * **No amplification:** the answer is never larger than the probe (a probe too short for it
//!   gets nothing), and answers are held to [`PER_SOURCE_GAP`] per address and
//!   [`REPLIES_PER_SEC`] in all.
//! * **Home networks only:** only private (10/8, 172.16/12, 192.168/16), link-local, CGNAT
//!   (100.64/10, Tailscale's too), loopback and IPv6 unique-local / link-local sources are
//!   answered, plus addresses `PS5UPLOAD_ALLOW_IP` admits.
//! * **The engine's own posture:** an engine bound to loopback only runs no responder, and it
//!   listens on the address the engine listens on. Settings › Play Together's "PS5Upload Play can
//!   find this computer" (`play_discovery`, on by default) turns the answers off.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::UdpSocket;

/// The UDP port the responder listens on.
pub(crate) const PORT: u16 = 19261;
/// What a probe starts with.
pub(crate) const PROBE: &[u8] = b"PS5UPLOAD_PLAY_DISCOVER v1";
/// Datagrams larger than this are not probes.
const MAX_PROBE: usize = 1500;
/// One answer per address this often at most, and this many a second in all.
const PER_SOURCE_GAP: Duration = Duration::from_millis(250);
const REPLIES_PER_SEC: usize = 50;
/// Longest device name in an answer (characters).
const NAME_MAX: usize = 64;

/// What an answer says.
#[derive(Clone, Debug)]
pub(crate) struct Info {
    pub name: String,
    pub port: u16,
    pub version: String,
    pub id: String,
    /// Play Togethers open here whose host is playing.
    pub together: usize,
}

/// The answer to `req`, or None: not a probe, or too short to carry the answer.
pub(crate) fn reply_for(req: &[u8], info: &Info) -> Option<Vec<u8>> {
    if req.len() > MAX_PROBE || !req.starts_with(PROBE) {
        return None;
    }
    let name: String = info
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX)
        .collect();
    let body = json!({
        "name": name,
        "port": info.port,
        "version": info.version,
        "id": info.id,
        "together": info.together,
    })
    .to_string()
    .into_bytes();
    (body.len() <= req.len()).then_some(body)
}

/// A home network's address (see the module notes), or one `allow` admits.
pub(crate) fn source_ok(ip: IpAddr, allow: &[crate::AllowRule]) -> bool {
    let home = match ip {
        IpAddr::V4(a) => v4_home(a),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4_home(v4),
            None => v6_home(a),
        },
    };
    home || crate::allow_rules_contain(allow, ip)
}

fn v4_home(a: Ipv4Addr) -> bool {
    let o = a.octets();
    a.is_private()
        || a.is_link_local()
        || a.is_loopback()
        // CGNAT 100.64.0.0/10 (Tailscale's addresses too)
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
}

fn v6_home(a: Ipv6Addr) -> bool {
    let s0 = a.segments()[0];
    a.is_loopback() || (s0 & 0xfe00) == 0xfc00 || (s0 & 0xffc0) == 0xfe80
}

/// The answers' pace: per address and in all.
#[derive(Default)]
pub(crate) struct Pace {
    last: HashMap<IpAddr, Instant>,
    second: Option<(Instant, usize)>,
}

impl Pace {
    pub(crate) fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.last.len() > 1024 {
            self.last
                .retain(|_, t| now.saturating_duration_since(*t) < PER_SOURCE_GAP);
        }
        if self
            .last
            .get(&ip)
            .is_some_and(|t| now.saturating_duration_since(*t) < PER_SOURCE_GAP)
        {
            return false;
        }
        let (start, n) = match self.second {
            Some((s, n)) if now.saturating_duration_since(s) < Duration::from_secs(1) => (s, n),
            _ => (now, 0),
        };
        if n >= REPLIES_PER_SEC {
            return false;
        }
        self.second = Some((start, n + 1));
        self.last.insert(ip, now);
        true
    }
}

/// Starts the responder beside the engine listening at `http`: on the same address, unless that
/// is loopback only (then none). Best effort: a port another engine holds is logged and skipped.
pub(crate) fn start(http: SocketAddr, allow: Vec<crate::AllowRule>) {
    if http.ip().is_loopback() {
        crate::log_at!(
            Info,
            "together",
            "play discovery: off (the engine listens on this computer only)"
        );
        return;
    }
    tokio::spawn(async move {
        let at = SocketAddr::new(http.ip(), PORT);
        let sock = match UdpSocket::bind(at).await {
            Ok(s) => s,
            Err(e) => {
                crate::log_at!(
                    Info,
                    "together",
                    "play discovery: UDP {at} not available ({e}); PS5Upload Play can still use a typed address"
                );
                return;
            }
        };
        crate::log_at!(Info, "together", "play discovery: answering on UDP {at}");
        let info = move || Info {
            name: crate::mesh_api::device_name(),
            port: http.port(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            id: crate::mesh_api::id_hex()
                .map(|h| h.chars().take(16).collect())
                .unwrap_or_default(),
            together: crate::join::open_playing_count(),
        };
        serve(sock, info, allow, || crate::join::settings().play_discovery).await;
    });
}

/// Answers probes on `sock` while `on()` says so.
pub(crate) async fn serve(
    sock: UdpSocket,
    info: impl Fn() -> Info,
    allow: Vec<crate::AllowRule>,
    on: impl Fn() -> bool,
) {
    let mut buf = vec![0u8; MAX_PROBE + 1];
    let mut pace = Pace::default();
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                // A reply bounced (ICMP unreachable on some systems) is not the end.
                crate::log_at!(Debug, "together", "play discovery: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let req = &buf[..n];
        if !req.starts_with(PROBE) || !source_ok(from.ip(), &allow) || !on() {
            continue;
        }
        let Some(reply) = reply_for(req, &info()) else {
            continue;
        };
        if !pace.allow(from.ip(), Instant::now()) {
            continue;
        }
        let _ = sock.send_to(&reply, from).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        Info {
            name: "Desk".into(),
            port: 19113,
            version: "13.0.0".into(),
            id: "0123456789abcdef".into(),
            together: 1,
        }
    }

    fn probe(len: usize) -> Vec<u8> {
        let mut p = PROBE.to_vec();
        p.resize(len.max(PROBE.len()), b' ');
        p
    }

    #[test]
    fn a_padded_probe_gets_the_engines_name_port_version_and_id() {
        let r = reply_for(&probe(512), &info()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
        assert_eq!(
            v,
            json!({"name": "Desk", "port": 19113, "version": "13.0.0", "id": "0123456789abcdef", "together": 1})
        );
        assert!(r.len() <= 512);
    }

    #[test]
    fn the_answer_is_never_larger_than_the_probe() {
        // The bare probe is too short to carry the answer: nothing.
        assert!(reply_for(PROBE, &info()).is_none());
        let long_name = Info {
            name: "n".repeat(500),
            ..info()
        };
        for len in [PROBE.len(), 64, 100, 200, 400, 512, MAX_PROBE] {
            if let Some(r) = reply_for(&probe(len), &long_name) {
                assert!(r.len() <= len, "{} > {len}", r.len());
            }
        }
        // The name is cut, so a 512-byte probe always gets an answer.
        assert!(reply_for(&probe(512), &long_name).is_some());
        // Not a probe, or too large to be one.
        assert!(reply_for(&[b'x'; 600], &info()).is_none());
        assert!(reply_for(&probe(MAX_PROBE + 1), &info()).is_none());
        assert!(reply_for(b"PS5UPLOAD_PLAY_DISCOVER v2                ", &info()).is_none());
    }

    #[test]
    fn only_home_network_sources_are_answered() {
        let ok = |s: &str| source_ok(s.parse().unwrap(), &[]);
        for s in [
            "192.168.1.50",
            "10.0.0.2",
            "172.16.4.4",
            "172.31.255.1",
            "169.254.3.3",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.50",
        ] {
            assert!(ok(s), "{s}");
        }
        for s in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "100.63.255.255",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(!ok(s), "{s}");
        }
        // …unless PS5UPLOAD_ALLOW_IP admits it.
        let allow = crate::parse_allow_ips("203.0.113.0/24");
        assert!(source_ok("203.0.113.9".parse().unwrap(), &allow));
    }

    #[test]
    fn answers_are_paced_per_address_and_in_all() {
        let mut p = Pace::default();
        let t = Instant::now();
        let a: IpAddr = "192.168.1.2".parse().unwrap();
        assert!(p.allow(a, t));
        assert!(!p.allow(a, t + Duration::from_millis(100)));
        assert!(p.allow(a, t + PER_SOURCE_GAP));
        let mut p = Pace::default();
        let n = (0..200u8)
            .filter(|i| p.allow(IpAddr::from([10, 0, 0, *i]), t))
            .count();
        assert_eq!(n, REPLIES_PER_SEC);
        assert!(p.allow(IpAddr::from([10, 0, 1, 1]), t + Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn a_live_responder_answers_a_padded_probe_and_nothing_else() {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = sock.local_addr().unwrap();
        let on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let on2 = on.clone();
        tokio::spawn(serve(sock, info, Vec::new(), move || {
            on2.load(std::sync::atomic::Ordering::Relaxed)
        }));
        let app = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 2048];
        let wait = |d: u64| Duration::from_millis(d);
        // Unpadded: no answer.
        app.send_to(PROBE, at).await.unwrap();
        assert!(tokio::time::timeout(wait(300), app.recv_from(&mut buf))
            .await
            .is_err());
        // Padded: the answer.
        app.send_to(&probe(512), at).await.unwrap();
        let (n, _) = tokio::time::timeout(wait(2000), app.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(v["port"], 19113);
        // Turned off in the settings: no answer.
        on.store(false, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(PER_SOURCE_GAP).await;
        app.send_to(&probe(512), at).await.unwrap();
        assert!(tokio::time::timeout(wait(300), app.recv_from(&mut buf))
            .await
            .is_err());
    }
}
