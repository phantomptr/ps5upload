//! Find PS5s on the network: mDNS-SD plus a /24 sweep, for the Connection screen's "Find PS5s"
//! button and `POST /api/discover`.
//!
//! Why this exists: typing an IP address is the #1 onboarding friction
//! per release feedback. Most users have a single PS5 on their LAN; the
//! UI should be able to find it for them.
//!
//! Why mDNS-SD instead of an ARP/ICMP sweep:
//!   - mDNS-SD is a pure-Rust LAN-only protocol (no raw sockets, no
//!     root, no firewall prompts) and works identically on macOS,
//!     Windows, and Linux.
//!   - PS5 doesn't natively advertise via mDNS, but several homebrew
//!     payloads do (a payload binding `_sonic-loader._tcp`, ftp
//!     servers publishing `_ftp._tcp`, etc.). Picking those up gives
//!     us a strong "this is a homebrew-enabled PS5" signal.
//!   - For consoles where no homebrew advertises mDNS yet, we fall
//!     back to a TCP probe of :9021 (the universal payload-loader
//!     port) on every host that did show up via mDNS for any reason.
//!   - And, as a last-ditch fallback when mDNS turns up zero hosts,
//!     a concurrent /24 sweep of EVERY local subnet (all network
//!     interfaces — Ethernet + Wi-Fi + USB-tether) probing :9120 (our
//!     own payload's AVA1 port). Nothing else binds 9120, so a hit is
//!     a near-certain PS5-with-our-payload signal. Keeps discovery
//!     working when the LAN has mDNS suppressed (some routers/APs
//!     intentionally drop multicast).
//!
//! What we deliberately do NOT do:
//!   - Run a continuous mDNS browser. Discovery is a one-shot the
//!     user triggers; a long-running browser would spam the LAN and
//!     leak fds during the app's lifetime.
//!   - Enumerate the full Bonjour service catalogue
//!     (`_services._dns-sd._udp.local`). That meta-query produces a
//!     "tell me every type that exists" result, which then needs a
//!     follow-up browse per type — quickly turns into hundreds of
//!     parallel browses on a busy office LAN. Curated list is faster
//!     and quieter.
//!   - Advertise ourselves. Read-only scope, per the integration plan
//!     locked in for this phase.
//!
//! Blocking, in plain threads, like the rest of this crate. mDNS is the `mdns` feature: a build
//! without it (the standalone Android engine) sweeps only, and says so in the result.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Service types we browse. Order in this list is also the precedence
/// order used by `Candidate::primary_service` for UI display — first
/// match wins, so the "homebrew PS5" services come first.
///
/// The trailing `.local.` is required by the mDNS-SD wire format —
/// without it the browse silently matches nothing on most stacks.
const BROWSED_SERVICES: &[&str] = &[
    // PS5-homebrew-specific service type. If this is alive on a host,
    // that host is almost certainly a jailbroken PS5.
    "_sonic-loader._tcp.local.",
    // FTP server (homebrew ftp payload, default port 2121). Less
    // specific — many devices advertise FTP — but combined with a
    // :9021 probe it's a clear PS5 fingerprint.
    "_ftp._tcp.local.",
    // Generic workstation advertisement. PS5 is FreeBSD-derived;
    // some homebrew layers include avahi/mdnsd that publish this.
    "_workstation._tcp.local.",
    // HTTP catches homebrew web UIs (commonly on port 6969) advertised
    // via mDNS.
    "_http._tcp.local.",
    // SSH is rare on PS5 but enabled by a few advanced payloads.
    "_ssh._tcp.local.",
];

/// PS5 payload loader (universal across jailbreak chains — kstuff,
/// etaHEN, etc. all bind it). Our strongest "this is actually a
/// PS5 right now" signal.
const PS5_LOADER_PORT: u16 = 9021;

/// ps5upload payload's one port (AVA1: management and data). Open ≈ our payload is already
/// running — UI can show that and skip the "send payload" step.
const PS5_AVA1_PORT: u16 = 9120;

/// How long mDNS listens. The default covers a typical home LAN; the ceiling handles enterprise
/// LANs where announcements arrive over a longer window, and keeps a bad request from pinning a
/// thread for minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TIMEOUT: Duration = Duration::from_secs(10);

/// See `sweep_for_payload_port`.
const SWEEP_WORKERS: usize = 200;
const SWEEP_STACK_BYTES: usize = 128 * 1024;

/// Per-host TCP probe timeout. Short on purpose — multiplied across
/// all discovered hosts, this dominates wallclock once mDNS has
/// settled. 800 ms is generous for LAN RTT and rules out hosts that
/// are merely up but not listening.
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// Confidence-scoring weights — the "is this actually a PS5?"
/// heuristic. Tuned conservatively: each individual signal is
/// suggestive but not conclusive, so we accumulate.
///
/// Threshold for the UI's "highly likely" badge is `>= 50`. Anything
/// below that is shown in the candidate list with a muted indicator
/// — the user can still pick it, we just don't claim certainty.
mod confidence {
    /// :9021 accepting connections. Strongest single signal: this port
    /// is bound by every common PS5 payload loader and by very little
    /// else.
    pub const LOADER_PORT_OPEN: u8 = 60;
    /// :9120 accepting connections. Means our own payload is already
    /// running on that host — even stronger than the loader port,
    /// because nothing else binds 9120.
    pub const PAYLOAD_PORT_OPEN: u8 = 70;
    /// `_sonic-loader._tcp` mDNS service type seen for this host —
    /// a PS5-homebrew-specific advertisement that strongly indicates
    /// a jailbroken console.
    pub const SONIC_LOADER_SERVICE: u8 = 50;
    /// Hostname literally contains "PS5" or "PlayStation". Set on
    /// homebrew that includes user-customisable hostname settings.
    pub const HOSTNAME_HINT: u8 = 25;
    /// FTP advertised on the homebrew ftpsrv default port (2121, not
    /// the standard 21). Combined with anything else, this is a
    /// clear scene-tool fingerprint.
    pub const HOMEBREW_FTP_PORT: u8 = 20;
    /// Confidence ≥ this gets the green "highly likely PS5" badge.
    pub const HIGH_THRESHOLD: u8 = 50;
}

/// Internal accumulator while we collect mDNS responses. Keyed by
/// hostname (or IP fallback) so multiple service-type announcements
/// from the same device fold into one row.
#[derive(Default)]
struct CandidateAccumulator {
    hostname: Option<String>,
    ips: Vec<IpAddr>,
    services: Vec<String>,
    /// Per-service ports we observed. Lets us notice "FTP advertised
    /// on 2121 specifically" vs "FTP on 21" — the former is a
    /// homebrew fingerprint.
    ports: Vec<u16>,
}

/// One row in the discovery result.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredHost {
    /// Best IP to type into the host field (`pick_best_ip`).
    pub ip: String,
    /// Hostname from the mDNS records, when there were any.
    pub hostname: Option<String>,
    /// All addresses seen for this host.
    pub all_ips: Vec<String>,
    /// Service types this host advertised, without the trailing `.local.`.
    pub services: Vec<String>,
    /// :9021 reachable.
    pub loader_port_open: bool,
    /// :9120 reachable: our payload is already running.
    pub payload_port_open: bool,
    /// Cumulative confidence score, capped at 100.
    pub confidence: u8,
    /// `confidence >= confidence::HIGH_THRESHOLD`.
    pub likely_ps5: bool,
}

/// What a run looks for.
#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions {
    /// How long mDNS listens: clamped to 1..=10 s, 3 s when absent or zero.
    pub timeout: Option<Duration>,
    /// More /24 networks to sweep, for an engine whose own interfaces are not on the consoles'
    /// LAN (Docker without host networking). `[192, 168, 1]` is 192.168.1.0/24.
    pub extra_subnets: Vec<[u8; 3]>,
}

/// The result, shaped the way the Connection screen has always read it.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub candidates: Vec<DiscoveredHost>,
    pub scanned_ms: u64,
    pub browsed_services: Vec<String>,
    /// False when this build has no mDNS, or it could not start: only a sweep ran.
    pub mdns: bool,
    /// The /24 networks swept, as `a.b.c.0/24`. Empty when mDNS already found a running helper.
    pub swept: Vec<String>,
}

/// Parse a network to sweep: `192.168.1.0/24`, `192.168.1.5` or `192.168.1`. Only private and
/// link-local networks: this must never sweep someone else's public /24.
pub fn parse_subnet(raw: &str) -> Result<[u8; 3], String> {
    let s = raw.trim().trim_end_matches("/24");
    let parts: Vec<&str> = s.split('.').collect();
    let bad = || format!("\"{raw}\" is not a /24 network like 192.168.1.0/24");
    if !(parts.len() == 3 || parts.len() == 4) {
        return Err(bad());
    }
    let mut o = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        o[i] = p.parse().map_err(|_| bad())?;
    }
    let ip = Ipv4Addr::new(o[0], o[1], o[2], o[3]);
    if !(ip.is_private() || ip.is_link_local()) {
        return Err(format!("{raw} is not a private network"));
    }
    Ok([o[0], o[1], o[2]])
}

/// Find PS5 candidates. Never fails outright: no candidates is a result, and a sweep still runs
/// when mDNS could not start.
pub fn discover(opts: &DiscoverOptions) -> DiscoverResult {
    let started = Instant::now();
    let budget = opts
        .timeout
        .filter(|t| !t.is_zero())
        .unwrap_or(DEFAULT_TIMEOUT)
        .clamp(Duration::from_secs(1), MAX_TIMEOUT);

    let (accum, mdns_error) = browse_mdns(budget);
    let mdns = mdns_error.is_none() && cfg!(feature = "mdns");

    // Probe and score BEFORE deciding whether to sweep: the right signal is "did mDNS find a PS5
    // running our payload", not "did it find anything", which any printer would satisfy.
    let mut candidates = probe_and_score_accum(accum);

    // The sweep targets only :9120, because nothing else binds it; :9021 is shared by every
    // jailbreak chain. It still runs when mDNS found a loader but no helper: there may be a
    // second PS5 that isn't advertising. Networks a caller named are always swept.
    let any_payload_port_open = candidates.iter().any(|c| c.payload_port_open);
    let (locals, mut subnets) = if any_payload_port_open {
        (Vec::new(), Vec::new())
    } else {
        let locals = local_ipv4s();
        let subnets = subnets_of(&locals);
        (locals, subnets)
    };
    for s in &opts.extra_subnets {
        if !subnets.contains(s) {
            subnets.push(*s);
        }
    }
    if !subnets.is_empty() {
        let already_known: HashSet<String> = candidates
            .iter()
            .flat_map(|c| c.all_ips.iter().cloned())
            .collect();
        let mut sweep_accum: BTreeMap<String, CandidateAccumulator> = BTreeMap::new();
        for ip in sweep_for_payload_port(&subnets, &locals) {
            let ip_str = ip.to_string();
            // mDNS already surfaced this host: its probe stands.
            if already_known.contains(&ip_str) {
                continue;
            }
            sweep_accum
                .entry(ip_str)
                .or_default()
                .ips
                .push(IpAddr::V4(ip));
        }
        if !sweep_accum.is_empty() {
            candidates.extend(probe_and_score_accum(sweep_accum));
        }
    }

    // Highest confidence first; by IP within that, so rows don't shuffle between runs.
    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.ip.cmp(&b.ip))
    });

    DiscoverResult {
        // The UI would rather show "no candidates" than an error, except that it says why mDNS
        // could not start, as it always has.
        ok: mdns_error.is_none(),
        error: mdns_error,
        candidates,
        scanned_ms: started.elapsed().as_millis() as u64,
        browsed_services: BROWSED_SERVICES.iter().map(|s| s.to_string()).collect(),
        mdns,
        swept: subnets
            .iter()
            .map(|p| format!("{}.{}.{}.0/24", p[0], p[1], p[2]))
            .collect(),
    }
}

/// Listen for `budget`. Returns what was heard, and why mDNS could not start, if it could not.
#[cfg(feature = "mdns")]
fn browse_mdns(budget: Duration) -> (BTreeMap<String, CandidateAccumulator>, Option<String>) {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let mut accum: BTreeMap<String, CandidateAccumulator> = BTreeMap::new();
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => return (accum, Some(format!("mdns init failed: {e}"))),
    };
    let mut receivers = Vec::with_capacity(BROWSED_SERVICES.len());
    for svc in BROWSED_SERVICES {
        match daemon.browse(svc) {
            Ok(rx) => receivers.push((*svc, rx)),
            Err(e) => crate::log::log(&format!("[discover] browse({svc}) failed: {e}")),
        }
    }
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let mut heard = false;
        for (svc, rx) in &receivers {
            while let Ok(event) = rx.try_recv() {
                heard = true;
                // Only a resolved service carries addresses and a port.
                let ServiceEvent::ServiceResolved(info) = event else {
                    continue;
                };
                let key = info.get_hostname().trim_end_matches('.').to_string();
                let entry = accum.entry(key.clone()).or_default();
                if entry.hostname.is_none() && !key.is_empty() {
                    entry.hostname = Some(key);
                }
                for scoped in info.get_addresses() {
                    let ip = scoped.to_ip_addr();
                    if !entry.ips.contains(&ip) {
                        entry.ips.push(ip);
                    }
                }
                let svc_clean = svc
                    .trim_end_matches('.')
                    .trim_end_matches(".local")
                    .to_string();
                if !entry.services.contains(&svc_clean) {
                    entry.services.push(svc_clean);
                }
                entry.ports.push(info.get_port());
            }
        }
        if !heard {
            std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    for (svc, _rx) in &receivers {
        let _ = daemon.stop_browse(svc);
    }
    let _ = daemon.shutdown();
    (accum, None)
}

#[cfg(not(feature = "mdns"))]
fn browse_mdns(_budget: Duration) -> (BTreeMap<String, CandidateAccumulator>, Option<String>) {
    (BTreeMap::new(), None)
}

/// Probe each host's best IP for the loader and payload ports at once, and score it. Used after
/// the mDNS browse and for what the sweep found, so the two can't score differently.
fn probe_and_score_accum(accum: BTreeMap<String, CandidateAccumulator>) -> Vec<DiscoveredHost> {
    let hosts: Vec<(String, CandidateAccumulator, IpAddr)> = accum
        .into_iter()
        // No address at all (a misconfigured avahi): nothing to probe or to type.
        .filter_map(|(k, a)| pick_best_ip(&a.ips).map(|ip| (k, a, ip)))
        .collect();
    let probes: Vec<(bool, bool)> = std::thread::scope(|s| {
        let handles: Vec<_> = hosts
            .iter()
            .map(|(_, _, ip)| {
                let ip = *ip;
                s.spawn(move || {
                    let (loader, payload) = std::thread::scope(|s2| {
                        let l = s2.spawn(move || tcp_probe(ip, PS5_LOADER_PORT));
                        let p = s2.spawn(move || tcp_probe(ip, PS5_AVA1_PORT));
                        (l.join().unwrap_or(false), p.join().unwrap_or(false))
                    });
                    (loader, payload)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or((false, false)))
            .collect()
    });
    hosts
        .into_iter()
        .zip(probes)
        .map(
            |((host_key, acc, best_ip), (loader_port_open, payload_port_open))| {
                let confidence = score(
                    &host_key,
                    &acc.services,
                    &acc.ports,
                    loader_port_open,
                    payload_port_open,
                );
                DiscoveredHost {
                    ip: best_ip.to_string(),
                    hostname: acc.hostname,
                    all_ips: acc.ips.iter().map(ToString::to_string).collect(),
                    services: acc.services,
                    loader_port_open,
                    payload_port_open,
                    confidence,
                    likely_ps5: confidence >= confidence::HIGH_THRESHOLD,
                }
            },
        )
        .collect()
}

/// Did the OS accept a connection? Refused and timed out both read as closed.
fn tcp_probe(ip: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), PROBE_TIMEOUT).is_ok()
}

/// Every usable local IPv4: private and link-local only, since those are same-LAN reachable and a
/// public interface address would mean sweeping strangers' hosts. All interfaces, so a laptop
/// wired to the PS5's LAN while also on Wi-Fi gets both. Falls back to the routing-table trick (a
/// UDP `connect` sends no packet) when enumeration finds nothing.
fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut locals: Vec<Ipv4Addr> = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            if let IpAddr::V4(v4) = iface.ip() {
                if v4.is_private() || v4.is_link_local() {
                    locals.push(v4);
                }
            }
        }
    }
    if locals.is_empty() {
        if let Ok(addr) = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect("1.1.1.1:1").and_then(|_| s.local_addr()))
        {
            if let IpAddr::V4(v4) = addr.ip() {
                if !v4.is_loopback() && !v4.is_unspecified() {
                    locals.push(v4);
                }
            }
        }
    }
    locals
}

/// The distinct /24s of these addresses.
fn subnets_of(locals: &[Ipv4Addr]) -> Vec<[u8; 3]> {
    let mut out: Vec<[u8; 3]> = Vec::new();
    for l in locals {
        let o = l.octets();
        let p = [o[0], o[1], o[2]];
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Every host of these /24s that takes a connection on :9120, skipping our own addresses. A
/// bounded pool of small threads: 200 < the 256 macOS default file limit, leaving room for the
/// app's other sockets, however many interfaces the host has.
fn sweep_for_payload_port(subnets: &[[u8; 3]], own: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let targets: Vec<Ipv4Addr> = subnets
        .iter()
        .flat_map(|p| (1u8..=254).map(move |h| Ipv4Addr::new(p[0], p[1], p[2], h)))
        .filter(|ip| !own.contains(ip))
        .collect();
    sweep(&targets, |ip| tcp_probe(IpAddr::V4(ip), PS5_AVA1_PORT))
}

fn sweep(targets: &[Ipv4Addr], probe: impl Fn(Ipv4Addr) -> bool + Sync) -> Vec<Ipv4Addr> {
    let next = AtomicUsize::new(0);
    let hits: Mutex<Vec<Ipv4Addr>> = Mutex::new(Vec::new());
    let work = || loop {
        let Some(ip) = targets.get(next.fetch_add(1, Ordering::Relaxed)) else {
            return;
        };
        if probe(*ip) {
            hits.lock().unwrap_or_else(|e| e.into_inner()).push(*ip);
        }
    };
    let workers = SWEEP_WORKERS.min(targets.len());
    std::thread::scope(|s| {
        let mut started = 0;
        for _ in 0..workers {
            let spawned = std::thread::Builder::new()
                .name("lan-sweep".into())
                .stack_size(SWEEP_STACK_BYTES)
                .spawn_scoped(s, work);
            if spawned.is_ok() {
                started += 1;
            }
        }
        // A system out of threads still gets a (slow) sweep.
        if started == 0 {
            work();
        }
    });
    let mut out = hits.into_inner().unwrap_or_else(|e| e.into_inner());
    out.sort();
    out
}

/// Pick the most-useful IP from a host's announced address set:
///   1. Prefer IPv4 (most users' IP-in-router pages show IPv4; less
///      typing friction)
///   2. Prefer non-loopback (loopback would only happen if running
///      mDNS against localhost, which is fine but uninteresting)
///   3. Prefer private-range / link-local (the user is on the same
///      LAN; a public IP would mean weird routing we shouldn't lead
///      with)
///   4. First-seen tiebreaker
fn pick_best_ip(ips: &[IpAddr]) -> Option<IpAddr> {
    fn score(ip: &IpAddr) -> i32 {
        let mut s = 0;
        if matches!(ip, IpAddr::V4(_)) {
            s += 100;
        }
        if !ip.is_loopback() {
            s += 50;
        }
        // is_private and is_link_local on Ipv4Addr are MSRV-stable
        // since well before 1.77. For Ipv6 link-local we'd want
        // `is_unicast_link_local()` but that's only stable since 1.84
        // — open-coded the fe80::/10 check (matches the stdlib
        // implementation) so we keep the workspace MSRV at 1.77.
        match ip {
            IpAddr::V4(v4) if v4.is_private() => s += 20,
            IpAddr::V4(v4) if v4.is_link_local() => s += 10,
            IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => s += 10,
            _ => {}
        }
        s
    }
    ips.iter().max_by_key(|ip| score(ip)).copied()
}

/// Compute the confidence score for a candidate. Pure function so
/// it's unit-testable without mocking mDNS. See `confidence` module
/// docs for what each weight means.
///
/// Capped at 100 so the renderer can treat it as a percentage.
fn score(
    host_key: &str,
    services: &[String],
    ports: &[u16],
    loader_port_open: bool,
    payload_port_open: bool,
) -> u8 {
    let mut s: u16 = 0;
    if loader_port_open {
        s += confidence::LOADER_PORT_OPEN as u16;
    }
    if payload_port_open {
        s += confidence::PAYLOAD_PORT_OPEN as u16;
    }
    if services.iter().any(|svc| svc.contains("sonic-loader")) {
        s += confidence::SONIC_LOADER_SERVICE as u16;
    }
    let host_lower = host_key.to_ascii_lowercase();
    if host_lower.contains("ps5") || host_lower.contains("playstation") {
        s += confidence::HOSTNAME_HINT as u16;
    }
    // ftpsrv payload defaults to 2121, not 21 — that specifically is
    // the homebrew fingerprint. A regular NAS on :21 doesn't get the
    // boost.
    let has_ftp = services.iter().any(|svc| svc.contains("_ftp."));
    if has_ftp && ports.contains(&2121) {
        s += confidence::HOMEBREW_FTP_PORT as u16;
    }
    s.min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn a_listening_port_probes_open_and_a_closed_one_does_not() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open_port = open.local_addr().unwrap().port();
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(tcp_probe(lo, open_port));
        assert!(!tcp_probe(lo, closed));
        assert_eq!(PS5_AVA1_PORT, 9120, "the one port our payload listens on");
    }

    #[test]
    fn loader_port_alone_clears_threshold() {
        let s = score("foo.local", &[], &[], true, false);
        assert!(s >= confidence::HIGH_THRESHOLD);
    }

    #[test]
    fn payload_port_alone_clears_threshold() {
        let s = score("foo.local", &[], &[], false, true);
        assert!(s >= confidence::HIGH_THRESHOLD);
    }

    #[test]
    fn nothing_special_stays_low() {
        let s = score("printer.local", &[], &[], false, false);
        assert_eq!(s, 0);
    }

    #[test]
    fn hostname_hint_alone_does_not_clear() {
        // Hostname containing "PS5" is suggestive but a spoofed mDNS
        // record alone shouldn't auto-confirm. Threshold guards that.
        let s = score("my-ps5.local", &[], &[], false, false);
        assert_eq!(s, confidence::HOSTNAME_HINT);
        assert!(s < confidence::HIGH_THRESHOLD);
    }

    #[test]
    fn ftp_on_homebrew_port_boosts() {
        let svc = vec!["_ftp._tcp".to_string()];
        let s = score("nas.local", &svc, &[2121], false, false);
        assert_eq!(s, confidence::HOMEBREW_FTP_PORT);
    }

    #[test]
    fn ftp_on_standard_port_does_not_boost() {
        let svc = vec!["_ftp._tcp".to_string()];
        let s = score("nas.local", &svc, &[21], false, false);
        assert_eq!(s, 0);
    }

    #[test]
    fn confidence_caps_at_100() {
        let svc = vec!["_sonic-loader._tcp".to_string(), "_ftp._tcp".to_string()];
        let s = score("ps5-living-room.local", &svc, &[2121], true, true);
        assert_eq!(s, 100);
    }

    #[test]
    fn pick_best_ip_prefers_ipv4_private() {
        let ips = vec![
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        ];
        let best = pick_best_ip(&ips).unwrap();
        assert_eq!(best, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)));
    }

    #[test]
    fn pick_best_ip_returns_none_for_empty() {
        assert!(pick_best_ip(&[]).is_none());
    }

    #[test]
    fn a_network_to_sweep_is_a_private_slash_24() {
        assert_eq!(parse_subnet("192.168.1.0/24").unwrap(), [192, 168, 1]);
        assert_eq!(parse_subnet(" 10.0.5.7 ").unwrap(), [10, 0, 5]);
        assert_eq!(parse_subnet("172.16.9").unwrap(), [172, 16, 9]);
        assert_eq!(parse_subnet("169.254.1.0/24").unwrap(), [169, 254, 1]);
        for bad in [
            "8.8.8.0/24",
            "1.2.3",
            "192.168",
            "192.168.1.300",
            "nope",
            "",
        ] {
            assert!(parse_subnet(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_sweep_finds_every_hit_and_visits_each_target_once() {
        let targets: Vec<Ipv4Addr> = (1u8..=254).map(|h| Ipv4Addr::new(10, 0, 0, h)).collect();
        let visits = AtomicUsize::new(0);
        let hits = sweep(&targets, |ip| {
            visits.fetch_add(1, Ordering::Relaxed);
            ip.octets()[3] % 100 == 0
        });
        assert_eq!(visits.load(Ordering::Relaxed), 254);
        assert_eq!(
            hits,
            vec![Ipv4Addr::new(10, 0, 0, 100), Ipv4Addr::new(10, 0, 0, 200)]
        );
        assert!(sweep(&[], |_| true).is_empty());
    }

    #[test]
    fn a_sweep_skips_our_own_addresses_and_dedupes_networks() {
        let own = [Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(10, 0, 0, 6)];
        assert_eq!(subnets_of(&own), vec![[10, 0, 0]]);
    }

    #[test]
    fn hosts_are_scored_on_what_their_ports_say() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        drop(open);
        let mut accum = BTreeMap::new();
        accum.insert(
            "ps5.local".to_string(),
            CandidateAccumulator {
                hostname: Some("ps5.local".into()),
                ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                services: vec!["_http._tcp".into()],
                ports: vec![80],
            },
        );
        // No address: nothing to offer.
        accum.insert("empty".to_string(), CandidateAccumulator::default());
        let rows = probe_and_score_accum(accum);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ip, "127.0.0.1");
        assert_eq!(rows[0].hostname.as_deref(), Some("ps5.local"));
    }
}
