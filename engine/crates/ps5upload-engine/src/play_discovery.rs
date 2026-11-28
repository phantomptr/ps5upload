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
//! When the probe comes from a console in this engine's list, the answer also has `"you":
//! {"known": true, "name", "streaming", "together", "paired"}`: the console's name on the list,
//! whether this engine is Remote Playing it now, how many friends play in its Play Together, and
//! whether its PS5Upload Play is paired here. The app then picks "its" computer by itself (the one
//! streaming it first) and keeps the others for joining a friend. Anyone else never gets `you`.
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
/// Longest device name in an answer (characters), and in bytes (a name in another script is up
/// to 4 bytes a character).
const NAME_MAX: usize = 64;
const NAME_MAX_BYTES: usize = 128;

/// What the answer says to a console this engine has on its list.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct You {
    /// Its name on the list, if it has one.
    pub name: Option<String>,
    /// This engine is Remote Playing it now.
    pub streaming: bool,
    /// Friends playing in its Play Together here.
    pub together: usize,
    /// Its PS5Upload Play is paired with this engine.
    pub paired: bool,
}

/// What an answer says.
#[derive(Clone, Debug)]
pub(crate) struct Info {
    pub name: String,
    pub port: u16,
    pub version: String,
    pub id: String,
    /// Play Togethers open here whose host is playing.
    pub together: usize,
    /// Only for a probe from a console on this engine's list.
    pub you: Option<You>,
}

/// `s` without control characters, at most `chars` characters and `bytes` bytes.
fn cut(s: &str, chars: usize, bytes: usize) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|c| !c.is_control()).take(chars) {
        if out.len() + c.len_utf8() > bytes {
            break;
        }
        out.push(c);
    }
    out
}

fn body_of(info: &Info, name_chars: usize, with_you: bool) -> Vec<u8> {
    let bytes = NAME_MAX_BYTES * name_chars / NAME_MAX;
    let mut v = json!({
        "name": cut(&info.name, name_chars, bytes),
        "port": info.port,
        "version": info.version,
        "id": info.id,
        "together": info.together,
    });
    if let (true, Some(y)) = (with_you, &info.you) {
        v["you"] = json!({
            "known": true,
            "name": y.name.as_deref().map(|n| cut(n, name_chars, bytes)),
            "streaming": y.streaming,
            "together": y.together,
            "paired": y.paired,
        });
    }
    v.to_string().into_bytes()
}

/// The answer to `req`, or None: not a probe, or too short to carry the answer. The names are cut
/// until it fits; `you` goes last of all.
pub(crate) fn reply_for(req: &[u8], info: &Info) -> Option<Vec<u8>> {
    if req.len() > MAX_PROBE || !req.starts_with(PROBE) {
        return None;
    }
    for with_you in [true, false] {
        if with_you && info.you.is_none() {
            continue;
        }
        for chars in [NAME_MAX, NAME_MAX / 2, NAME_MAX / 4, 0] {
            let body = body_of(info, chars, with_you);
            if body.len() <= req.len() {
                return Some(body);
            }
        }
    }
    None
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
        let info = move |from: IpAddr| Info {
            name: crate::mesh_api::device_name(),
            port: http.port(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            id: crate::mesh_api::id_hex()
                .map(|h| h.chars().take(16).collect())
                .unwrap_or_default(),
            together: crate::join::open_playing_count(),
            you: you_for(from),
        };
        serve(sock, info, allow, || crate::join::settings().play_discovery).await;
    });
}

/// `you` for a probe from `from`: None unless it is a console on this engine's list.
fn you_for(from: IpAddr) -> Option<You> {
    let console = crate::play_app::console_of(from);
    let (name, _) = crate::consoles_api::console_brief(&console)?;
    let together = crate::join::app_view(&console, false)["guests"]
        .as_array()
        .map_or(0, Vec::len);
    Some(You {
        name,
        streaming: crate::rp_stream_api::live_video(&console).is_some(),
        together,
        paired: crate::play_app::is_paired(&console),
    })
}

/// Answers probes on `sock` while `on()` says so; `info` is asked with the probe's source.
pub(crate) async fn serve(
    sock: UdpSocket,
    info: impl Fn(IpAddr) -> Info,
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
        if !pace.allow(from.ip(), Instant::now()) {
            continue;
        }
        let Some(reply) = reply_for(req, &info(from.ip())) else {
            continue;
        };
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
            you: None,
        }
    }

    fn yours() -> Info {
        Info {
            you: Some(You {
                name: Some("Living room".into()),
                streaming: true,
                together: 2,
                paired: false,
            }),
            ..info()
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
    fn a_console_on_the_list_is_told_about_itself_and_anyone_else_is_not() {
        let v: serde_json::Value =
            serde_json::from_slice(&reply_for(&probe(512), &yours()).unwrap()).unwrap();
        assert_eq!(
            v["you"],
            json!({"known": true, "name": "Living room", "streaming": true, "together": 2, "paired": false})
        );
        // Not on the list: no `you` at all.
        let v: serde_json::Value =
            serde_json::from_slice(&reply_for(&probe(512), &info()).unwrap()).unwrap();
        assert!(v.get("you").is_none(), "{v}");
        // A console with no name on the list is still known.
        let mut nameless = yours();
        nameless.you.as_mut().unwrap().name = None;
        let v: serde_json::Value =
            serde_json::from_slice(&reply_for(&probe(512), &nameless).unwrap()).unwrap();
        assert_eq!(v["you"]["known"], true);
        assert!(v["you"]["name"].is_null());
    }

    #[test]
    fn you_is_only_for_a_console_on_this_engines_list() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        // Tests keep the shared documents in memory (each nextest test is its own process).
        let doc = crate::shared_state::read(crate::consoles_api::DOC);
        if crate::consoles_api::console_brief("127.0.0.1").is_none() {
            assert_eq!(you_for(ip), None);
        }
        crate::shared_state::write(
            crate::consoles_api::DOC,
            doc.rev,
            json!({"profiles": [{"host": "127.0.0.1", "name": "Desk PS5"}]}),
        )
        .unwrap();
        let y = you_for(ip).unwrap();
        assert_eq!(y.name.as_deref(), Some("Desk PS5"));
        assert!(!y.streaming);
        // An IPv4 console reached over a mapped address is the same console.
        assert!(you_for("::ffff:127.0.0.1".parse().unwrap()).is_some());
        assert_eq!(you_for("192.168.200.9".parse().unwrap()), None);
    }

    #[test]
    fn the_you_answer_fits_a_padded_probe_with_the_longest_names() {
        // Names that grow most in JSON: 4-byte characters, and quotes (escaped, two bytes each).
        for n in ["\u{1F3AE}".repeat(500), "\"".repeat(500), "\\".repeat(500)] {
            let mut i = yours();
            i.name = n.clone();
            i.version = "13.0.0-beta.10".into();
            i.together = 999;
            i.you.as_mut().unwrap().name = Some(n.clone());
            let r = reply_for(&probe(512), &i).unwrap();
            assert!(r.len() <= 512, "{}", r.len());
            let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
            assert_eq!(v["you"]["known"], true, "{v}");
            assert!(!v["name"].as_str().unwrap().is_empty());
            // Every size: never larger than the probe; `you` goes before nothing is answered.
            for len in [PROBE.len(), 64, 100, 150, 200, 300, 400, 512, MAX_PROBE] {
                if let Some(r) = reply_for(&probe(len), &i) {
                    assert!(r.len() <= len, "{} > {len}", r.len());
                }
            }
        }
        // A probe big enough for the plain answer but not for `you` still gets the plain one.
        let plain = reply_for(&probe(512), &info()).unwrap();
        let r = reply_for(&probe(plain.len()), &yours()).unwrap();
        assert!(r.len() <= plain.len());
        let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
        assert!(v.get("you").is_none());
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
        tokio::spawn(serve(
            sock,
            |_| info(),
            Vec::new(),
            move || on2.load(std::sync::atomic::Ordering::Relaxed),
        ));
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
