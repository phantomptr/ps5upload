//! mDNS-SD discovery for the Connection screen's "Find PS5s on the
//! network" button.
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

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::time::timeout;

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

/// ps5upload payload's management port. Open ≈ our payload is already
/// running — UI can show that and skip the "send payload" step.
const PS5_MGMT_PORT: u16 = 9114;

/// Max wallclock for a single discovery run, in seconds. The default
/// (3s) covers a typical home LAN; the 10s ceiling handles enterprise
/// LANs where mDNS announcements arrive over a longer window. We
/// clamp on the Rust side so a renderer bug can't pin a thread for
/// minutes.
const DEFAULT_TIMEOUT_SECS: u64 = 3;
const MAX_TIMEOUT_SECS: u64 = 10;

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
    /// :9114 accepting connections. Means our own payload is already
    /// running on that host — even stronger than the loader port,
    /// because nothing else binds 9114.
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

/// One row in the discovery result, serialized to the renderer.
#[derive(Serialize)]
pub struct DiscoveredHost {
    /// Best IP for the user to type into the host field. Picked by
    /// `pick_best_ip`: prefers IPv4, prefers private/link-local
    /// ranges (the user is on the same LAN), avoids loopback.
    ip: String,
    /// Hostname from the mDNS PTR/SRV records. May be `<host>.local`
    /// or a friendlier form depending on what the device publishes.
    hostname: Option<String>,
    /// All IPv4/IPv6 addresses we saw for this host. Useful when
    /// `ip` is wrong (e.g. dual-WAN PS5) — UI surfaces them as
    /// "alternates."
    all_ips: Vec<String>,
    /// Service types this host advertised, for UI explainer text
    /// (e.g. "sonic-loader, FTP"). Stripped of the leading underscore
    /// and trailing `.local.` for compactness.
    services: Vec<String>,
    /// :9021 reachable. Drives the loader-port confidence boost and
    /// the green "ready to receive payload" indicator.
    loader_port_open: bool,
    /// :9114 reachable — our payload is already running. UI can skip
    /// the Connection screen entirely and route to Upload directly
    /// when the user picks a host with this set.
    payload_port_open: bool,
    /// Cumulative confidence score, capped at 100. UI uses this to
    /// pick the indicator colour (green / amber / muted).
    confidence: u8,
    /// `confidence >= confidence::HIGH_THRESHOLD`. Pre-computed so
    /// the renderer doesn't have to know the threshold.
    likely_ps5: bool,
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

/// Discover PS5 candidates on the LAN. Tauri command — invoked from
/// the Connection screen's "Find PS5s" button.
///
/// `timeout_secs` clamped to [1, MAX_TIMEOUT_SECS]; 0 or absent → default.
///
/// Response shape (always `ok=true` for non-fatal failures — the UI
/// would rather show "no candidates" than an error):
/// ```json
/// {
///   "ok": true,
///   "candidates": [DiscoveredHost, ...],
///   "scanned_ms": 3014,
///   "browsed_services": ["_sonic-loader._tcp.local.", ...]
/// }
/// ```
#[tauri::command]
pub async fn discover_ps5(timeout_secs: Option<u64>) -> serde_json::Value {
    let started = Instant::now();
    let budget_secs = timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    let budget = Duration::from_secs(budget_secs);

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            return serde_json::json!({
                "ok": false,
                "error": format!("mdns init failed: {e}"),
                "candidates": [],
                "scanned_ms": started.elapsed().as_millis() as u64,
                "browsed_services": BROWSED_SERVICES,
            });
        }
    };

    let mut receivers = Vec::with_capacity(BROWSED_SERVICES.len());
    for svc in BROWSED_SERVICES {
        match daemon.browse(svc) {
            Ok(rx) => receivers.push((*svc, rx)),
            Err(e) => {
                eprintln!("[discover] browse({svc}) failed: {e}");
            }
        }
    }

    let mut accum: BTreeMap<String, CandidateAccumulator> = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;

        // Poll all receivers concurrently; whichever yields first
        // contributes the next event. We can't easily `select!` over
        // a Vec, so do a short-poll round-robin with a small per-step
        // timeout. The crate's `recv_async` is a oneshot per call so
        // this stays cheap.
        let mut got_event_this_round = false;
        for (svc, rx) in &receivers {
            // Per-receiver timeout: short, so other receivers get
            // a chance. Capped by overall remaining budget.
            let step = Duration::from_millis(100).min(remaining);
            match timeout(step, rx.recv_async()).await {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    got_event_this_round = true;
                    let key = info.get_hostname().trim_end_matches('.').to_string();
                    let entry = accum.entry(key.clone()).or_default();
                    if entry.hostname.is_none() && !key.is_empty() {
                        entry.hostname = Some(key);
                    }
                    for ip in info.get_addresses() {
                        if !entry.ips.contains(ip) {
                            entry.ips.push(*ip);
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
                Ok(Ok(_)) => {
                    // ServiceFound / SearchStarted / SearchStopped /
                    // ServiceRemoved: not actionable on their own.
                    // We act only on ServiceResolved (which carries
                    // IPs and ports).
                    got_event_this_round = true;
                }
                Ok(Err(_)) => {
                    // Channel closed. Daemon dropped this browse.
                    // Drop our handle on the next iteration.
                }
                Err(_) => {
                    // Per-step timeout; try the next receiver.
                }
            }
        }
        // If nothing happened across all receivers in a round, sleep
        // a bit so we don't burn CPU while waiting for late responses.
        if !got_event_this_round {
            let nap = Duration::from_millis(50).min(remaining);
            tokio::time::sleep(nap).await;
        }
    }

    // Stop all browses and shut down the daemon.
    for (svc, _rx) in &receivers {
        let _ = daemon.stop_browse(svc);
    }
    let _ = daemon.shutdown();

    // Promote accumulators to candidates: TCP-probe each one for
    // :9021 + :9114 in parallel, score them, and sort.
    let mut candidates = Vec::with_capacity(accum.len());
    let mut probe_tasks = Vec::with_capacity(accum.len());

    for (host_key, acc) in accum {
        let best_ip = match pick_best_ip(&acc.ips) {
            Some(ip) => ip,
            // No IPv4/IPv6 addresses at all — the announcement was
            // hostname-only (rare, but we've seen it on misconfigured
            // avahi). Skip — we can't probe or hand the user a
            // typeable address.
            None => continue,
        };
        let probe_ip = best_ip.to_string();
        let task = tokio::spawn(async move {
            let (loader_open, payload_open) = tokio::join!(
                tcp_probe(&probe_ip, PS5_LOADER_PORT),
                tcp_probe(&probe_ip, PS5_MGMT_PORT),
            );
            (loader_open, payload_open)
        });
        probe_tasks.push((host_key, acc, best_ip, task));
    }

    for (host_key, acc, best_ip, task) in probe_tasks {
        let (loader_port_open, payload_port_open) = task.await.unwrap_or((false, false));

        let confidence = score(
            &host_key,
            &acc.services,
            &acc.ports,
            loader_port_open,
            payload_port_open,
        );

        candidates.push(DiscoveredHost {
            ip: best_ip.to_string(),
            hostname: acc.hostname,
            all_ips: acc.ips.iter().map(ToString::to_string).collect(),
            services: acc.services,
            loader_port_open,
            payload_port_open,
            confidence,
            likely_ps5: confidence >= confidence::HIGH_THRESHOLD,
        });
    }

    candidates.sort_by(|a, b| {
        // Highest confidence first; tie-break by IP for stable order
        // across reruns (so the UI doesn't shuffle rows on refresh).
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.ip.cmp(&b.ip))
    });

    serde_json::json!({
        "ok": true,
        "candidates": candidates,
        "scanned_ms": started.elapsed().as_millis() as u64,
        "browsed_services": BROWSED_SERVICES,
    })
}

/// Cheap TCP-connect probe. We don't speak any protocol — purely "did
/// the OS accept a SYN" — which matches the existing
/// `companions.rs::companion_probe` shape exactly. ECONNREFUSED and
/// timeout both → false; the caller doesn't care which.
async fn tcp_probe(ip: &str, port: u16) -> bool {
    let addr = format!("{ip}:{port}");
    matches!(
        timeout(PROBE_TIMEOUT, TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
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
}
