//! Which way an internet guest's connection goes ("one invite": the same link on the same Wi-Fi,
//! on Tailscale, or across the internet). Two str0m peers on a simulated network: the engine's
//! side set up as rtc.rs does it (its host candidates through [`super::rtc::add_host_candidates`],
//! a STUN candidate, the answer), and a guest that makes the offer and controls ICE as the browser
//! does. The guest's own host addresses are left out of its offer, as Chrome hides them behind
//! mDNS `.local` names the engine ignores: the engine learns the guest's LAN address only from its
//! checks (a peer-reflexive candidate), which is what makes the LAN pair possible at all.
//!
//! The network: a home network (192.168.1.0/24), a tailnet (100.100.0.0/16), and routers with
//! endpoint-independent mappings (the guest's and the engine's, or one router for both with
//! hairpinning). Every way takes the same time, so only ICE's priorities choose.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

const ENGINE_LAN: &str = "192.168.1.10:5000";
const ENGINE_TAIL: &str = "100.100.0.1:5000";
const GUEST_LAN: &str = "192.168.1.20:7000";
const GUEST_TAIL: &str = "100.100.0.2:7000";
/// The guest away from home: its own network and router.
const GUEST_AWAY: &str = "10.9.0.5:7000";

/// Where each side is.
#[derive(Clone, Copy)]
struct Place {
    /// The guest is on the engine's home network.
    same_lan: bool,
    /// Both are on the tailnet.
    tailnet: bool,
}

/// A router mapping: inside (a socket) ↔ outside.
struct Map {
    inside: SocketAddr,
    outside: SocketAddr,
}

struct Net {
    place: Place,
    maps: Vec<Map>,
    /// (when, to the engine?, from, to, bytes)
    q: Vec<(Instant, bool, SocketAddr, SocketAddr, Vec<u8>)>,
}

const HOP: Duration = Duration::from_millis(3);

fn a(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn on_lan(x: SocketAddr) -> bool {
    matches!(x.ip(), std::net::IpAddr::V4(v) if v.octets()[..3] == [192, 168, 1])
}

fn on_tail(x: SocketAddr) -> bool {
    matches!(x.ip(), std::net::IpAddr::V4(v) if v.octets()[..2] == [100, 100])
}

impl Net {
    /// Where a packet from `src` to `dst` arrives (side, its source then, its destination), or
    /// None when no way leads there.
    fn route(&self, src: SocketAddr, dst: SocketAddr) -> Option<(bool, SocketAddr, SocketAddr)> {
        let engine_owns = |x: SocketAddr| x == a(ENGINE_LAN) || x == a(ENGINE_TAIL);
        if on_lan(src) && on_lan(dst) {
            return self.place.same_lan.then_some((engine_owns(dst), src, dst));
        }
        if on_tail(src) && on_tail(dst) {
            return self.place.tailnet.then_some((engine_owns(dst), src, dst));
        }
        // Out through the sender's router to a mapped address (hairpinning when it is its own).
        let out = self.maps.iter().find(|m| m.inside == src)?.outside;
        let m = self.maps.iter().find(|m| m.outside == dst)?;
        Some((engine_owns(m.inside), out, m.inside))
    }

    fn send(&mut self, now: Instant, src: SocketAddr, dst: SocketAddr, data: Vec<u8>) {
        if let Some((to_engine, s, d)) = self.route(src, dst) {
            self.q.push((now + HOP, to_engine, s, d, data));
        }
    }
}

struct Outcome {
    /// The engine's selected pair (its socket, the guest's address) and the path rtc.rs names.
    pair: (SocketAddr, SocketAddr),
    path: &'static str,
    /// The engine's host candidates' priorities in its answer, by address.
    host_prios: Vec<(SocketAddr, u32)>,
}

/// A guest at `place` connects to an engine on the home network that also runs Tailscale.
fn connect(place: Place) -> Outcome {
    let t0 = Instant::now();
    let engine_public = a("203.0.113.1:6000");
    let guest_base = if place.same_lan {
        a(GUEST_LAN)
    } else {
        a(GUEST_AWAY)
    };
    // One router for both at home (hairpinning), the guest's own when away.
    let guest_public = if place.same_lan {
        a("203.0.113.1:8000")
    } else {
        a("198.51.100.2:8000")
    };
    let mut net = Net {
        place,
        maps: vec![
            Map {
                inside: a(ENGINE_LAN),
                outside: engine_public,
            },
            Map {
                inside: guest_base,
                outside: guest_public,
            },
        ],
        q: Vec::new(),
    };

    // The guest (the browser): its home-network address first, as Chrome ranks its Wi-Fi above
    // a VPN adapter, then its tailnet one; both hidden from the offer.
    let mut g = Rtc::builder()
        .set_crypto_provider(super::crypto::provider())
        .build(t0);
    g.add_local_candidate(Candidate::host(guest_base, "udp").unwrap());
    if place.tailnet {
        g.add_local_candidate(Candidate::host(a(GUEST_TAIL), "udp").unwrap());
    }
    g.add_local_candidate(Candidate::server_reflexive(guest_public, guest_base, "udp").unwrap());
    let mut api = g.sdp_api();
    let _cid = api.add_channel("join".into());
    let (offer, pending) = api.apply().unwrap();
    let offer: String = offer
        .to_sdp_string()
        .lines()
        .filter(|l| !(l.starts_with("a=candidate") && l.contains(" typ host")))
        .map(|l| format!("{l}\r\n"))
        .collect();
    assert!(!offer.contains("192.168.1.20") && !offer.contains("100.100.0.2"));

    // The engine, as rtc.rs on_offer makes it: its sockets in the order the system lists them
    // (by address: the tailnet's first), then the STUN candidate.
    let (cert, _) = super::rtc::cert().unwrap();
    let mut h = Rtc::builder()
        .set_crypto_provider(super::crypto::provider())
        .set_dtls_cert(cert.clone())
        .set_stats_interval(Some(Duration::from_millis(200)))
        .build(t0);
    super::rtc::add_host_candidates(&mut h, [a(ENGINE_TAIL), a(ENGINE_LAN)]);
    h.add_local_candidate(
        Candidate::server_reflexive(engine_public, a(ENGINE_LAN), "udp").unwrap(),
    );
    let answer = h
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer).unwrap())
        .unwrap()
        .to_sdp_string();
    let host_prios = answer
        .lines()
        .filter(|l| l.starts_with("a=candidate") && l.contains(" typ host"))
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (
                format!("{}:{}", f[4], f[5]).parse().unwrap(),
                f[3].parse().unwrap(),
            )
        })
        .collect();
    g.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();

    let mut now = t0;
    let mut open = false;
    let mut pair = None;
    while now < t0 + Duration::from_secs(30) {
        let mut next = now + Duration::from_millis(50);
        for (rtc, engine) in [(&mut h, true), (&mut g, false)] {
            loop {
                match rtc.poll_output().unwrap() {
                    Output::Timeout(t) => {
                        next = next.min(t);
                        break;
                    }
                    Output::Transmit(t) => {
                        net.send(now, t.source, t.destination, t.contents.to_vec())
                    }
                    Output::Event(Event::ChannelOpen(..)) if engine => open = true,
                    Output::Event(Event::PeerStats(s)) if engine && open => {
                        if let Some(c) = s.selected_candidate_pair {
                            pair = Some((c.local.addr, c.remote.addr));
                        }
                    }
                    Output::Event(_) => {}
                }
            }
        }
        if let Some(p) = pair {
            return Outcome {
                pair: p,
                path: super::rtc::path_of(p.0, p.1),
                host_prios,
            };
        }
        if let Some(t) = net.q.iter().map(|p| p.0).min() {
            next = next.min(t);
        }
        now = next.max(now);
        let (due, rest): (Vec<_>, Vec<_>) = net.q.drain(..).partition(|p| p.0 <= now);
        net.q = rest;
        for (_, to_engine, source, destination, data) in due {
            let rtc = if to_engine { &mut h } else { &mut g };
            let input = Input::Receive(
                now,
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination,
                    contents: data.as_slice().try_into().unwrap(),
                },
            );
            if rtc.accepts(&input) {
                rtc.handle_input(input).unwrap();
            }
        }
        h.handle_input(Input::Timeout(now)).unwrap();
        g.handle_input(Input::Timeout(now)).unwrap();
    }
    panic!("never connected");
}

/// On the same Wi-Fi the connection stays on it, though the engine never heard the guest's LAN
/// address (Chrome's mDNS name): its checks made it a peer-reflexive candidate, and the pair with
/// the engine's home-network address outranks its tailnet one and the routers' addresses.
#[test]
fn on_the_same_wifi_the_lan_pair_wins_over_tailscale_and_the_internet() {
    let o = connect(Place {
        same_lan: true,
        tailnet: true,
    });
    assert_eq!(o.pair, (a(ENGINE_LAN), a(GUEST_LAN)), "{:?}", o.pair);
    assert_eq!(o.path, "lan");
    // The home network's host candidate first, whatever order the sockets came in.
    let prio = |x: &str| o.host_prios.iter().find(|(s, _)| *s == a(x)).unwrap().1;
    assert!(prio(ENGINE_LAN) > prio(ENGINE_TAIL), "{:?}", o.host_prios);
}

/// Away from home with Tailscale on both: the tailnet, over the routers' addresses.
#[test]
fn away_on_tailscale_the_tailnet_pair_wins_over_the_internet() {
    let o = connect(Place {
        same_lan: false,
        tailnet: true,
    });
    assert_eq!(o.pair, (a(ENGINE_TAIL), a(GUEST_TAIL)), "{:?}", o.pair);
    assert_eq!(o.path, "tailscale");
}

/// Away from home without Tailscale: straight through both routers.
#[test]
fn away_without_tailscale_it_goes_over_the_internet() {
    let o = connect(Place {
        same_lan: false,
        tailnet: false,
    });
    assert_eq!(o.pair.1, a("198.51.100.2:8000"), "{:?}", o.pair);
    assert_eq!(o.path, "internet");
}

#[test]
fn addresses_rank_home_network_then_tailscale_then_public() {
    use super::rtc::{path_of, reach_rank};
    let r = |s: &str| reach_rank(s.parse().unwrap());
    for home in [
        "192.168.0.2",
        "10.1.2.3",
        "172.20.0.1",
        "169.254.3.4",
        "fd00::1",
        "fe80::1",
    ] {
        assert_eq!(r(home), 0, "{home}");
    }
    for tail in ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
        assert_eq!(r(tail), 1, "{tail}");
    }
    for public in ["100.63.0.1", "100.128.0.1", "8.8.8.8", "2001:db8::1"] {
        assert_eq!(r(public), 2, "{public}");
    }
    assert_eq!(path_of(a(ENGINE_LAN), a(GUEST_LAN)), "lan");
    assert_eq!(path_of(a(ENGINE_TAIL), a(GUEST_TAIL)), "tailscale");
    assert_eq!(path_of(a(ENGINE_LAN), a("198.51.100.2:8000")), "internet");
    // Hairpinned through the home router: the guest's address is the router's outside one.
    assert_eq!(path_of(a(ENGINE_LAN), a("203.0.113.1:8000")), "internet");
}
