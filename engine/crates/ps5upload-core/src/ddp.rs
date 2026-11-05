//! Sony's Device Discovery Protocol — console status, and waking from standby.
//!
//! Adapted from iharosi/ps5-wake (GPL-3.0, as is this project), itself
//! descended from Darryl Sokoloski's ps4-wake.
//!
//! A plain-text, HTTP-shaped protocol over UDP 9302. Two requests:
//!
//! * `SRCH` — any console answers with its identity and whether it is awake
//!   (`200`) or in standby (`620`). No credential needed.
//! * `WAKEUP` — wakes a console in standby. Needs a `user-credential` the user
//!   captures from the PS Remote Play app.
//!
//! Both require "Enable Remote Play" on the console. With it off nothing is
//! listening and every probe times out.
//!
//! Note for anyone reaching for Wake-on-LAN: a PS5 does not wake from a magic
//! packet, on any port or broadcast address. DDP is the only network path.

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// The port the console listens on. `ps5-wake`'s README says 987, but its own
/// source defaults to 9302, and 9302 is the one that answers.
pub const DDP_PORT: u16 = 9302;

/// Protocol version string every request carries.
pub const DDP_VERSION: &str = "00030010";

/// What a console said about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DdpStatus {
    /// 200 = awake, 620 = standby. Kept raw so an unknown code is reported
    /// rather than flattened into a guess.
    pub code: u16,
    pub status_text: String,
    /// The console's MAC without separators. A caller with this never needs to
    /// ask the console for its MAC by another route.
    pub host_id: String,
    pub host_name: String,
    pub host_type: String,
    pub system_version: String,
    pub running_app_name: String,
    pub running_app_titleid: String,
}

/// Parse a DDP reply. Unknown keys are ignored: Sony adds fields between
/// firmware versions, and a strict parser would reject a healthy console.
pub fn parse_reply(text: &str) -> Result<DdpStatus> {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or_default().trim();
    // "HTTP/1.1 200 Ok"
    let mut parts = head.split_whitespace();
    let _proto = parts.next();
    let code: u16 = parts
        .next()
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow!("not a DDP reply: {head:?}"))?;
    let status_text = parts.collect::<Vec<_>>().join(" ");

    let mut out = DdpStatus {
        code,
        status_text,
        ..Default::default()
    };
    for line in lines {
        let line = line.trim_end_matches('\0').trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "host-id" => out.host_id = value,
            "host-name" => out.host_name = value,
            "host-type" => out.host_type = value,
            "system-version" => out.system_version = value,
            "running-app-name" => out.running_app_name = value,
            "running-app-titleid" => out.running_app_titleid = value,
            _ => {}
        }
    }
    Ok(out)
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let bare = host.split(':').next().unwrap_or(host);
    format!("{bare}:{port}")
        .to_socket_addrs()
        .map_err(|e| anyhow!("resolving {bare}: {e}"))?
        .next()
        .ok_or_else(|| anyhow!("{bare} resolved to nothing"))
}

/// Ask a console to identify itself.
///
/// The trailing NUL is deliberate: a SRCH is sent with it and a WAKEUP without,
/// matching `ps5-wake`. The asymmetry looks like an oversight but the console
/// may depend on it, so it is preserved rather than tidied.
pub fn probe(host: &str, timeout: Duration) -> Result<DdpStatus> {
    let addr = resolve(host, DDP_PORT)?;
    let pkt = format!("SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{DDP_VERSION}\n\0");
    let sockets = send_every_way(pkt.as_bytes(), addr, true);
    if sockets.is_empty() {
        return Err(anyhow!("could not send SRCH to {addr} on any network"));
    }
    ask_again(
        sockets,
        addr,
        timeout,
        PROBE_RESEND_EVERY,
        PROBE_RESENDS,
        || send_every_way(pkt.as_bytes(), addr, true),
    )
    .ok_or_else(|| {
        anyhow!(
            "no reply from {addr}. A console only answers when \
             Remote Play is enabled in its settings."
        )
    })
}

/// SRCH is one UDP datagram: on Wi-Fi it, or its answer, can simply be lost, and one lost packet
/// must not read as "the PS5 does not answer". So it goes again this often, this many more times,
/// all within the caller's wait.
const PROBE_RESEND_EVERY: Duration = Duration::from_millis(500);
const PROBE_RESENDS: u32 = 2;

/// Waits for the console's answer within `timeout`, asking again with `send` every `every`.
fn ask_again(
    mut sockets: Vec<UdpSocket>,
    addr: SocketAddr,
    timeout: Duration,
    every: Duration,
    resends: u32,
    mut send: impl FnMut() -> Vec<UdpSocket>,
) -> Option<DdpStatus> {
    let end = std::time::Instant::now() + timeout;
    for _ in 0..resends {
        if end.saturating_duration_since(std::time::Instant::now()) <= every {
            break;
        }
        if let Some(s) = first_reply(&sockets, addr, every) {
            return Some(s);
        }
        sockets.extend(send());
    }
    first_reply(
        &sockets,
        addr,
        end.saturating_duration_since(std::time::Instant::now()),
    )
}

/// Sends `pkt` to `addr` every way this machine can reach it: from the default route, and
/// from each local interface on the console's subnet (unicast, plus that subnet's broadcast
/// when `broadcast` or the unicast send failed). Returns the sockets that sent something.
///
/// Why: a machine on Wi-Fi and Ethernet at once routes by its default interface, and a PS5 in
/// rest mode was measured not answering over Wi-Fi at all ("Host is down", no ARP reply) while
/// answering 620 on the same LAN over Ethernet. Sending only by the default route made both
/// consoles look switched off.
fn send_every_way(pkt: &[u8], addr: SocketAddr, broadcast: bool) -> Vec<UdpSocket> {
    let mut out = Vec::new();
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.send_to(pkt, addr).is_ok() {
            out.push(s);
        }
    }
    if let SocketAddr::V4(v4) = addr {
        for (local, bcast) in local_ipv4_on_subnet_of(*v4.ip()) {
            let Ok(s) = UdpSocket::bind((local, 0)) else {
                continue;
            };
            let unicast = s.send_to(pkt, addr).is_ok();
            let mut sent = unicast;
            if broadcast || !unicast {
                let _ = s.set_broadcast(true);
                sent |= s.send_to(pkt, (bcast, DDP_PORT)).is_ok();
            }
            if sent {
                out.push(s);
            }
        }
    }
    out
}

/// Waits up to `timeout` for the console at `addr` to answer on any of `sockets`, and parses
/// the first reply. Only the console's own reply counts, not another console's answer to a
/// broadcast or a stray datagram.
fn first_reply(sockets: &[UdpSocket], addr: SocketAddr, timeout: Duration) -> Option<DdpStatus> {
    let until = std::time::Instant::now() + timeout;
    for s in sockets {
        s.set_nonblocking(true).ok()?;
    }
    let mut buf = [0u8; 4096];
    loop {
        for s in sockets {
            while let Ok((n, from)) = s.recv_from(&mut buf) {
                if from.ip() == addr.ip() {
                    if let Ok(st) = parse_reply(&String::from_utf8_lossy(&buf[..n])) {
                        return Some(st);
                    }
                }
            }
        }
        if std::time::Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// This machine's IPv4 addresses on the same subnet as `dst`, with that subnet's broadcast
/// address, one per interface that is up. Empty where interfaces cannot be listed (Windows):
/// the default route alone is then used, as before.
#[cfg(unix)]
fn local_ipv4_on_subnet_of(dst: Ipv4Addr) -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we only read and then free once.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, not yet freed.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let up = ifa.ifa_flags & (libc::IFF_UP as libc::c_uint) != 0;
        let loopback = ifa.ifa_flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0;
        if !up || loopback || ifa.ifa_addr.is_null() || ifa.ifa_netmask.is_null() {
            continue;
        }
        // SAFETY: both pointers are non-null and point at sockaddrs owned by the list.
        let (fam, addr, mask) = unsafe {
            (
                (*ifa.ifa_addr).sa_family as i32,
                std::ptr::read_unaligned(ifa.ifa_addr as *const libc::sockaddr_in),
                std::ptr::read_unaligned(ifa.ifa_netmask as *const libc::sockaddr_in),
            )
        };
        if fam != libc::AF_INET {
            continue;
        }
        let local = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
        let mask = Ipv4Addr::from(u32::from_be(mask.sin_addr.s_addr));
        if let Some(b) = same_subnet_broadcast(local, mask, dst) {
            if !out.iter().any(|(l, _)| *l == local) {
                out.push((local, b));
            }
        }
    }
    // SAFETY: `head` came from a successful getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(head) };
    out
}

#[cfg(not(unix))]
fn local_ipv4_on_subnet_of(_dst: Ipv4Addr) -> Vec<(Ipv4Addr, Ipv4Addr)> {
    Vec::new()
}

/// The broadcast address of `local`/`mask` when `dst` is on that subnet; `None` otherwise, or
/// for a mask too wide or too narrow to be a LAN (/0, /31, /32).
#[cfg(any(unix, test))]
fn same_subnet_broadcast(local: Ipv4Addr, mask: Ipv4Addr, dst: Ipv4Addr) -> Option<Ipv4Addr> {
    let (l, m, d) = (u32::from(local), u32::from(mask), u32::from(dst));
    if m == 0 || m.count_zeros() < 2 || (l & m) != (d & m) || l == d {
        return None;
    }
    Some(Ipv4Addr::from(l | !m))
}

/// `EHOSTDOWN` ("Host is down"): the console gave no ARP reply. std has no `ErrorKind` for it.
/// Seen on a Mac whose default route was Wi-Fi while both consoles slept, so callers treat it
/// like a timeout (ask DDP why), not as a broken helper.
pub fn host_is_down(io: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        io.raw_os_error() == Some(libc::EHOSTDOWN)
    }
    #[cfg(not(unix))]
    {
        let _ = io;
        false
    }
}

/// The DDP code of a console in rest mode (`HTTP/1.1 620 Server Standby`).
pub const CODE_STANDBY: u16 = 620;
/// The DDP code of an awake console (`HTTP/1.1 200 Ok`).
pub const CODE_AWAKE: u16 = 200;

/// Where a console stands, power-wise, as the engine reports it (`power` in a status failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Power {
    /// DDP answered 200.
    Awake,
    /// DDP answered 620: rest mode with its network features on.
    Rest,
    /// Nothing answered, neither DDP nor TCP: off, on another network, or in rest mode with
    /// its network features off (the three look the same from here).
    Unreachable,
}

impl Power {
    /// The wire spelling: `"awake"`, `"rest"`, `"unreachable"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Power::Awake => "awake",
            Power::Rest => "rest",
            Power::Unreachable => "unreachable",
        }
    }
}

/// The budget of [`standby_probe`] on a failure path: short, because it only runs after a
/// connection already failed, and a sleeping console must not slow the answer much.
pub const STANDBY_PROBE_BUDGET: Duration = Duration::from_millis(800);
/// How many SRCH datagrams [`standby_probe`] sends within its budget (UDP can drop one).
pub const STANDBY_PROBE_SENDS: u32 = 2;

/// Asks a console for its DDP status with `sends` SRCH datagrams spread over `budget`.
/// `Some(code)` is the first reply's code (200 awake, 620 rest mode); `None` is no reply at
/// all, or a host that does not resolve. Never an error: callers use it only to explain a
/// failure they already have. Blocking.
pub fn standby_probe(host: &str, budget: Duration, sends: u32) -> Option<u16> {
    let addr = resolve(host, DDP_PORT).ok()?;
    let pkt = format!("SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{DDP_VERSION}\n\0");
    let sends = sends.max(1);
    let slice = (budget / sends).max(Duration::from_millis(1));
    for _ in 0..sends {
        let sockets = send_every_way(pkt.as_bytes(), addr, true);
        if sockets.is_empty() {
            return None;
        }
        if let Some(s) = first_reply(&sockets, addr, slice) {
            return Some(s.code);
        }
    }
    None
}

/// Wake a console in standby.
///
/// `credential` is the `user-credential` from the PS Remote Play app's own
/// traffic; there is no way to derive it here, and a console ignores a WAKEUP
/// carrying the wrong one. Sending is fire-and-forget — the console never
/// acknowledges — so a successful return means the datagram left the host and
/// nothing more.
pub fn wake(host: &str, credential: &str) -> Result<()> {
    if credential.trim().is_empty() {
        return Err(anyhow!(
            "a user-credential is required to wake a console; capture it from \
             the PS Remote Play app"
        ));
    }
    let addr = resolve(host, DDP_PORT)?;

    // Byte-for-byte what chiaki-ng sends, which is the client verified to
    // wake these consoles. Two details matter and both were wrong before:
    // `model:w` (not `m`), and the trailing NUL — chiaki transmits the
    // formatted string *plus* its terminator (`len + 1` bytes). Without the
    // NUL the console's standby responder reads past the datagram and stops
    // answering discovery entirely, rather than waking.
    let pkt = format!(
        "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:w\napp-type:r\n\
         user-credential:{}\ndevice-discovery-protocol-version:{DDP_VERSION}\n\0",
        credential.trim()
    );
    // Every way the console can be reached, like a SRCH (see `send_every_way`). The
    // credential goes to the subnet's broadcast only from an interface whose unicast send
    // failed: it stays off the rest of the LAN whenever it can.
    if send_every_way(pkt.as_bytes(), addr, false).is_empty() {
        return Err(anyhow!(
            "could not send WAKEUP to {addr}: no network on this computer reaches it"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real reply from a PS5.
    const AWAKE: &str = "HTTP/1.1 200 Ok\nhost-id:D4F7D5D36123\nhost-type:PS5\n\
host-name:Living Room PS5\nhost-request-port:997\n\
device-discovery-protocol-version:00030010\nsystem-version:09600004\n";

    #[test]
    fn reads_a_real_awake_reply() {
        let s = parse_reply(AWAKE).unwrap();
        assert_eq!(s.code, 200);
        assert_eq!(s.status_text, "Ok");
        assert_eq!(s.host_name, "Living Room PS5");
        assert_eq!(s.host_type, "PS5");
        assert_eq!(s.system_version, "09600004");
    }

    #[test]
    fn the_host_id_is_the_mac_without_separators() {
        // Discovery already carries the MAC, and works whether or not the
        // payload is running — so nothing else needs to look it up.
        let s = parse_reply(AWAKE).unwrap();
        assert_eq!(s.host_id, "D4F7D5D36123");
        assert_eq!(s.host_id.len(), 12);
    }

    #[test]
    fn reads_a_standby_reply_and_the_running_game() {
        let s = parse_reply(
            "HTTP/1.1 620 Server Standby\nhost-id:5C843CA8AE72\nhost-type:PS5\n\
             running-app-name:Black Myth: Wukong\nrunning-app-titleid:PPSA23226\n",
        )
        .unwrap();
        assert_eq!(s.code, 620);
        assert_eq!(s.status_text, "Server Standby");
        // A colon inside the value must survive: split on the FIRST colon only.
        assert_eq!(s.running_app_name, "Black Myth: Wukong");
        assert_eq!(s.running_app_titleid, "PPSA23226");
    }

    #[test]
    fn an_unknown_code_is_reported_rather_than_guessed() {
        let s = parse_reply("HTTP/1.1 500 Kaboom\n").unwrap();
        assert_eq!(s.code, 500);
    }

    #[test]
    fn tolerates_unknown_keys_and_trailing_nuls() {
        // Sony adds fields between firmware versions.
        let s = parse_reply("HTTP/1.1 200 Ok\nsomething-new:42\nhost-name:PS5\0\n").unwrap();
        assert_eq!(s.host_name, "PS5");
        assert_eq!(s.code, 200);
    }

    #[test]
    fn refuses_garbage_instead_of_inventing_a_status() {
        assert!(parse_reply("").is_err());
        assert!(parse_reply("hello there\n").is_err());
    }

    #[test]
    fn the_console_subnet_picks_the_interface_and_its_broadcast() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let m24 = ip("255.255.255.0");
        assert_eq!(
            same_subnet_broadcast(ip("192.0.2.199"), m24, ip("192.0.2.99")),
            Some(ip("192.0.2.255"))
        );
        // Another subnet, the console itself, and point-to-point masks are not routes to it.
        assert_eq!(
            same_subnet_broadcast(ip("10.0.0.2"), m24, ip("192.0.2.99")),
            None
        );
        assert_eq!(
            same_subnet_broadcast(ip("192.0.2.99"), m24, ip("192.0.2.99")),
            None
        );
        for m in ["255.255.255.255", "255.255.255.254", "0.0.0.0"] {
            assert_eq!(
                same_subnet_broadcast(ip("192.0.2.1"), ip(m), ip("192.0.2.99")),
                None
            );
        }
        assert_eq!(
            same_subnet_broadcast(ip("10.1.2.3"), ip("255.255.0.0"), ip("10.1.200.4")),
            Some(ip("10.1.255.255"))
        );
    }

    #[test]
    fn a_srch_reaches_a_local_responder_and_only_its_reply_counts() {
        // A fake console on loopback answers like a sleeping PS5.
        let console = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = console.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 512];
            let (_, from) = console.recv_from(&mut b).unwrap();
            console
                .send_to(b"HTTP/1.1 620 Server Standby\nhost-type:PS5\n", from)
                .unwrap();
        });
        let socks = send_every_way(b"SRCH * HTTP/1.1\n\0", addr, true);
        assert!(!socks.is_empty());
        let st = first_reply(&socks, addr, Duration::from_secs(2)).unwrap();
        assert_eq!(st.code, CODE_STANDBY);
        t.join().unwrap();
    }

    /// One lost datagram: the first SRCH gets no answer, the next does.
    #[test]
    fn a_lost_search_is_asked_again_within_the_wait() {
        let console = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = console.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 512];
            let _lost = console.recv_from(&mut b).unwrap();
            let (_, from) = console.recv_from(&mut b).unwrap();
            console
                .send_to(b"HTTP/1.1 200 Ok\nhost-type:PS5\n", from)
                .unwrap();
        });
        let send = || {
            let s = UdpSocket::bind("127.0.0.1:0").unwrap();
            s.send_to(b"SRCH", addr).unwrap();
            vec![s]
        };
        let st = ask_again(
            send(),
            addr,
            Duration::from_secs(2),
            Duration::from_millis(100),
            2,
            send,
        )
        .expect("the second one is answered");
        assert_eq!(st.code, 200);
        t.join().unwrap();
    }

    #[test]
    fn nothing_answering_still_ends_when_the_wait_does() {
        let quiet = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = quiet.local_addr().unwrap();
        let t0 = std::time::Instant::now();
        let mut sends = 0;
        let r = ask_again(
            Vec::new(),
            addr,
            Duration::from_millis(250),
            Duration::from_millis(100),
            2,
            || {
                sends += 1;
                Vec::new()
            },
        );
        assert!(r.is_none());
        assert_eq!(sends, 2);
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn waking_without_a_credential_is_refused_up_front() {
        // The console silently ignores a credential-less WAKEUP, so refusing
        // here is the only way the caller learns what is missing.
        assert!(wake("192.168.1.50", "").is_err());
        assert!(wake("192.168.1.50", "   ").is_err());
    }
}
