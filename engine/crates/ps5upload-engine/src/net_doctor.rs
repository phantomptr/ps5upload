//! Facts for the network doctor ("Diagnose slow transfers").
//!
//! The doctor answers "why are my uploads slow?" with a short list of findings. The judging is
//! done by the client, which also holds the speed test's figures and knows what else the app is
//! running; this module supplies what only the engine can see: the link of this computer that
//! carries traffic to the console (wired, Wi-Fi, a VPN or a virtual adapter; its speed and MTU),
//! the console's own link as its helper reports it, and how long a connection to the console
//! takes to open.
//!
//! The same rule as `net_health`: never guess. What could not be read carries a `skip` code
//! and the reason, and the client says so instead of a verdict. Reading the facts runs a
//! command or two per platform; everything that interprets their output is a plain function
//! tested on every platform.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use ps5upload_core::diagnostics::NetInterface;
use serde::Serialize;

/// What kind of link carries the traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkKind {
    Wired,
    Wifi,
    /// A VPN or tunnel adapter: traffic to the console is wrapped and sent elsewhere first.
    Vpn,
    /// A bridge, a container's virtual cable or a virtual-machine switch: the physical link
    /// behind it cannot be seen from here.
    Virtual,
    Unknown,
}

/// One side's link, as far as it could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Link {
    /// The interface's name ("en0", "eth0", "Ethernet 3").
    pub iface: Option<String>,
    pub kind: LinkKind,
    /// The negotiated speed of a wired link in Mb/s, when the system reports it.
    pub link_mbps: Option<u64>,
    pub mtu: Option<u32>,
    /// Why the link could not be read; `None` when it was. A short code the client words:
    /// `mobile`, `container`, `no_route`, `unreadable`, `not_found`, `helper`.
    pub skip: Option<&'static str>,
    /// Extra words for the skip (an error text), never shown as a verdict.
    pub detail: Option<String>,
}

impl Link {
    fn skipped(code: &'static str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            iface: None,
            kind: LinkKind::Unknown,
            link_mbps: None,
            mtu: None,
            skip: Some(code),
            detail: (!detail.is_empty()).then_some(detail),
        }
    }
}

/// Everything the doctor asks of the engine.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DoctorFacts {
    /// This computer's link toward the console (in a container: the container's own).
    pub host: Link,
    /// The console's link, from its helper.
    pub console: Link,
    pub in_container: bool,
    /// A container on a bridge network with no `PS5UPLOAD_PKG_HOST_IP` pin.
    pub bridged_without_pin: bool,
    /// Middle of the connection-open times to the console, in milliseconds.
    pub reply_ms: Option<u64>,
    pub reply_samples: usize,
}

/// Times opening a connection to the console's helper port, a few times, and returns the middle
/// time and how many opened. A connection opens in the console's network stack, not in the
/// helper, so this is the network's round trip even while the helper is busy with a transfer.
pub(crate) fn reply_time(host: &str, tries: usize) -> (Option<u64>, usize) {
    // Through Tailscale when the console is away (the same path the transfers take). A local
    // mapping of the engine's own node opens at once whatever the network does, so it is no
    // measurement: none then.
    let target = match ps5upload_core::route::dial(
        &ps5upload_core::events::console_host(host),
        ps5upload_ava1::gen::DEFAULT_PORT,
        false,
    ) {
        Some(a) if a.ip().is_loopback() => return (None, 0),
        Some(a) => a,
        None => match resolve(host, ps5upload_ava1::gen::DEFAULT_PORT) {
            Some(a) => a,
            None => return (None, 0),
        },
    };
    let mut samples = Vec::new();
    for _ in 0..tries {
        let t = Instant::now();
        match TcpStream::connect_timeout(&target, Duration::from_millis(1500)) {
            Ok(s) => {
                samples.push(t.elapsed().as_millis() as u64);
                drop(s);
            }
            // A closed port answers at once on most systems, but Windows retries a refused
            // connection for about a second, so only an opened one is a measurement.
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    let n = samples.len();
    (crate::net_health::median_ms(samples), n)
}

fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    (host, port).to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

/// Gathers the facts for the console at `addr`. Blocking: it asks the helper and runs a
/// command or two on this computer.
pub(crate) fn gather(addr: &str) -> DoctorFacts {
    let host = crate::pkg_install::strip_host_port(addr);
    let console_ip = resolve(&host, 1).and_then(|a| match a.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    });
    let in_container = crate::pkg_install::in_container();

    let host_link = match console_ip {
        None => Link::skipped("no_route", format!("{host} is not an IPv4 address")),
        Some(ip) => {
            let mut l = host_link_toward(ip);
            // A container sees its own virtual cable, which says nothing about the computer's.
            if in_container && matches!(l.kind, LinkKind::Virtual | LinkKind::Unknown) {
                l.skip = Some("container");
            }
            l
        }
    };

    let console = match ps5upload_core::diagnostics::net_interfaces(addr) {
        Ok(list) if !list.interfaces.is_empty() => console_link(&list.interfaces, console_ip),
        Ok(list) => Link::skipped("helper", list.err.unwrap_or_default()),
        Err(e) => Link::skipped("helper", format!("{e:#}")),
    };

    let (reply_ms, reply_samples) = reply_time(&host, 5);
    DoctorFacts {
        host: host_link,
        console,
        in_container,
        bridged_without_pin: crate::pkg_install::bridged_container_without_pkg_host_ip(),
        reply_ms,
        reply_samples,
    }
}

// ── The console ────────────────────────────────────────────────────────────────────────────

/// What a console interface's name says about it. The PS5 names its cable port `eth0` and its
/// Wi-Fi `wlan0`; anything else is not judged.
pub(crate) fn console_iface_kind(name: &str) -> LinkKind {
    let n = name.trim().to_ascii_lowercase();
    if n.starts_with("wlan") {
        LinkKind::Wifi
    } else if n.starts_with("eth") {
        LinkKind::Wired
    } else {
        LinkKind::Unknown
    }
}

/// The console's link: the interface that holds the address this computer talks to. When the
/// address is not among them (a hostname, NAT), the only interface with an address is it.
pub(crate) fn console_link(ifaces: &[NetInterface], ip: Option<Ipv4Addr>) -> Link {
    let has_addr = |i: &&NetInterface| {
        let a = i.ipv4.trim();
        !a.is_empty() && a != "0.0.0.0" && !a.starts_with("127.")
    };
    let by_ip = ip.and_then(|ip| {
        ifaces
            .iter()
            .find(|i| i.ipv4.trim().parse::<Ipv4Addr>().ok() == Some(ip))
    });
    let addressed: Vec<&NetInterface> = ifaces.iter().filter(has_addr).collect();
    let chosen = by_ip.or(if addressed.len() == 1 {
        Some(addressed[0])
    } else {
        None
    });
    let Some(i) = chosen else {
        return Link::skipped(
            "not_found",
            "the PS5 lists no interface with the address this computer uses",
        );
    };
    let kind = console_iface_kind(&i.name);
    Link {
        iface: Some(i.name.clone()),
        kind,
        link_mbps: None,
        mtu: (i.mtu > 0).then_some(i.mtu),
        skip: (kind == LinkKind::Unknown).then_some("unreadable"),
        detail: (kind == LinkKind::Unknown).then(|| format!("interface {}", i.name)),
    }
}

// ── This computer ──────────────────────────────────────────────────────────────────────────

#[cfg(any(target_os = "android", target_os = "ios"))]
fn host_link_toward(_ip: Ipv4Addr) -> Link {
    Link::skipped("mobile", "")
}

#[cfg(target_os = "linux")]
fn host_link_toward(ip: Ipv4Addr) -> Link {
    let routes = match std::fs::read_to_string("/proc/net/route") {
        Ok(t) => t,
        Err(e) => return Link::skipped("unreadable", format!("/proc/net/route: {e}")),
    };
    let Some(iface) = linux_route_iface(&routes, ip) else {
        return Link::skipped("no_route", "");
    };
    linux_link(&iface)
}

/// Reads one Linux interface from /sys/class/net. A bridge with a single physical member is
/// judged by that member (a home server often puts its one cable in a bridge).
#[cfg(target_os = "linux")]
fn linux_link(iface: &str) -> Link {
    use std::path::Path;
    let dir = |i: &str| Path::new("/sys/class/net").join(i);
    let read = |i: &str, f: &str| {
        std::fs::read_to_string(dir(i).join(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let facts = |i: &str| LinuxIf {
        name: i.to_string(),
        wireless: dir(i).join("wireless").exists() || dir(i).join("phy80211").exists(),
        physical: dir(i).join("device").exists(),
        speed: read(i, "speed"),
        mtu: read(i, "mtu"),
    };
    let mut f = facts(iface);
    if dir(iface).join("bridge").exists() {
        let members: Vec<LinuxIf> = std::fs::read_dir(dir(iface).join("brif"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| facts(&e.file_name().to_string_lossy()))
                    .filter(|m| m.physical)
                    .collect()
            })
            .unwrap_or_default();
        if let [one] = members.as_slice() {
            // The bridge's own MTU is what this computer sends with.
            f = LinuxIf {
                mtu: f.mtu.clone(),
                ..one.clone()
            };
        }
    }
    linux_classify(&f)
}

#[cfg(target_os = "macos")]
fn host_link_toward(ip: Ipv4Addr) -> Link {
    let Some(route) = run("/sbin/route", &["-n", "get", &ip.to_string()]) else {
        return Link::skipped("unreadable", "route did not answer");
    };
    let Some(iface) = mac_route_iface(&route) else {
        return Link::skipped("no_route", "");
    };
    let ports = run("/usr/sbin/networksetup", &["-listallhardwareports"]).unwrap_or_default();
    let ifconfig = run("/sbin/ifconfig", &[&iface]).unwrap_or_default();
    mac_classify(&iface, &ports, &ifconfig)
}

#[cfg(windows)]
fn host_link_toward(ip: Ipv4Addr) -> Link {
    match crate::win_net::win::powershell(&windows_script(ip), Duration::from_secs(12)) {
        Some((out, 0)) => windows_classify(&out),
        Some((_, code)) => Link::skipped("unreadable", format!("PowerShell exited with {code}")),
        None => Link::skipped("unreadable", "PowerShell did not answer"),
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    windows
)))]
fn host_link_toward(_ip: Ipv4Addr) -> Link {
    Link::skipped("unreadable", "not read on this system")
}

/// Runs a system tool and returns what it printed, or `None` when it failed.
#[cfg(target_os = "macos")]
fn run(prog: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Names that are tunnels whatever the system: VPN clients, WireGuard, PPP.
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
fn is_tunnel_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "tun",
        "tap",
        "utun",
        "wg",
        "ppp",
        "ipsec",
        "tailscale",
        "zt",
    ]
    .iter()
    .any(|p| n.starts_with(p))
}

/// The interface Linux routes `ip` through: the longest matching prefix in /proc/net/route
/// (lowest metric on a tie). The table's addresses are the kernel's in-memory words printed
/// in hex.
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn linux_route_iface(table: &str, ip: Ipv4Addr) -> Option<String> {
    let target = u32::from_ne_bytes(ip.octets());
    let mut best: Option<(u32, u32, String)> = None; // (prefix length, metric, iface)
    for line in table.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 {
            continue;
        }
        let (Ok(dest), Ok(flags), Ok(metric), Ok(mask)) = (
            u32::from_str_radix(f[1], 16),
            u32::from_str_radix(f[3], 16),
            f[6].parse::<u32>(),
            u32::from_str_radix(f[7], 16),
        ) else {
            continue;
        };
        const RTF_UP: u32 = 0x1;
        if flags & RTF_UP == 0 || target & mask != dest & mask {
            continue;
        }
        let len = mask.count_ones();
        let better = match &best {
            None => true,
            Some((l, m, _)) => len > *l || (len == *l && metric < *m),
        };
        if better {
            best = Some((len, metric, f[0].to_string()));
        }
    }
    best.map(|(_, _, i)| i)
}

/// What /sys/class/net says about one interface.
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) struct LinuxIf {
    pub name: String,
    pub wireless: bool,
    /// Backed by a device (a real network card), not a bridge, veth or tunnel.
    pub physical: bool,
    /// The `speed` file's text (Mb/s; `-1` or absent when unknown).
    pub speed: Option<String>,
    pub mtu: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn linux_classify(f: &LinuxIf) -> Link {
    let kind = if f.wireless {
        LinkKind::Wifi
    } else if is_tunnel_name(&f.name) {
        LinkKind::Vpn
    } else if f.physical {
        LinkKind::Wired
    } else {
        LinkKind::Virtual
    };
    let speed = f
        .speed
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&s| s > 0 && s < 1_000_000)
        .map(|s| s as u64);
    Link {
        iface: Some(f.name.clone()),
        kind,
        // A Wi-Fi card's figure is a radio rate, not comparable with a cable's.
        link_mbps: if kind == LinkKind::Wired { speed } else { None },
        mtu: f.mtu.as_deref().and_then(|m| m.parse().ok()),
        skip: None,
        detail: None,
    }
}

/// `route -n get` on macOS: the `interface:` line.
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn mac_route_iface(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("interface:")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// The hardware port `networksetup -listallhardwareports` names for `device` ("Wi-Fi",
/// "Ethernet", "USB 10/100/1000 LAN").
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn mac_hardware_port(text: &str, device: &str) -> Option<String> {
    let mut port: Option<String> = None;
    for l in text.lines() {
        let l = l.trim();
        if let Some(p) = l.strip_prefix("Hardware Port:") {
            port = Some(p.trim().to_string());
        } else if let Some(d) = l.strip_prefix("Device:") {
            if d.trim() == device {
                return port;
            }
        }
    }
    None
}

/// From `ifconfig <iface>` on macOS: the MTU, and the negotiated speed from the media line
/// ("media: autoselect (1000baseT <full-duplex>)"; "(100baseTX ...)"; "(10Gbase-T ...)").
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn mac_ifconfig(text: &str) -> (Option<u32>, Option<u64>) {
    let mtu = text
        .split_whitespace()
        .skip_while(|w| *w != "mtu")
        .nth(1)
        .and_then(|m| m.parse().ok());
    let media = text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("media:")?;
        let inside = rest.split_once('(').map(|(_, r)| r).unwrap_or(rest);
        inside.split_whitespace().find_map(media_mbps)
    });
    (mtu, media)
}

/// "1000baseT" -> 1000, "10Gbase-T" -> 10000, "2500Base-T" -> 2500; anything else is none.
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
fn media_mbps(word: &str) -> Option<u64> {
    let w = word.trim_start_matches('(').to_ascii_lowercase();
    let digits: String = w.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u64 = digits.parse().ok()?;
    let rest = &w[digits.len()..];
    if rest.starts_with("gbase") {
        Some(n * 1000)
    } else if rest.starts_with("base") {
        Some(n)
    } else {
        None
    }
}

#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn mac_classify(iface: &str, ports: &str, ifconfig: &str) -> Link {
    let (mtu, mbps) = mac_ifconfig(ifconfig);
    let port = mac_hardware_port(ports, iface);
    let kind = match port.as_deref().map(str::to_ascii_lowercase) {
        Some(p) if p.contains("wi-fi") || p.contains("airport") => LinkKind::Wifi,
        Some(p) if p.contains("bridge") => LinkKind::Virtual,
        Some(p) if p.contains("ethernet") || p.contains("lan") => LinkKind::Wired,
        _ if is_tunnel_name(iface) => LinkKind::Vpn,
        _ if iface.starts_with("bridge") || iface.starts_with("vmenet") => LinkKind::Virtual,
        // A port macOS does not list (an iPhone over USB, a dongle it calls something else):
        // a speed on the media line still says it is a cable.
        _ if mbps.is_some() => LinkKind::Wired,
        _ => LinkKind::Unknown,
    };
    Link {
        iface: Some(iface.to_string()),
        kind,
        link_mbps: if kind == LinkKind::Wired { mbps } else { None },
        mtu,
        skip: (kind == LinkKind::Unknown).then_some("unreadable"),
        detail: (kind == LinkKind::Unknown).then(|| format!("interface {iface}")),
    }
}

/// The PowerShell that finds the adapter Windows routes `ip` through and reads it.
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn windows_script(ip: Ipv4Addr) -> String {
    format!(
        "$ErrorActionPreference='Stop'\n\
         $r = Find-NetRoute -RemoteIPAddress '{ip}' | Where-Object {{ $_.InterfaceIndex }} | Select-Object -First 1\n\
         $a = Get-NetAdapter -InterfaceIndex $r.InterfaceIndex -ErrorAction SilentlyContinue\n\
         $i = Get-NetIPInterface -InterfaceIndex $r.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue\n\
         [pscustomobject]@{{ alias = $r.InterfaceAlias; desc = $a.InterfaceDescription; \
         media = $a.PhysicalMediaType; speed = $a.ReceiveLinkSpeed; hw = $a.HardwareInterface; \
         mtu = $i.NlMtu }} | ConvertTo-Json -Compress\n"
    )
}

/// Reads the script's JSON. `speed` is in bits per second; `media` is the adapter's physical
/// medium ("802.3", "Native 802.11").
#[cfg_attr(not(test), allow(dead_code))] // this platform's reader may not use it; tested everywhere
pub(crate) fn windows_classify(json: &str) -> Link {
    let v: serde_json::Value =
        match serde_json::from_str(json.trim_start_matches('\u{feff}').trim()) {
            Ok(v) => v,
            Err(e) => return Link::skipped("unreadable", e.to_string()),
        };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let alias = s("alias");
    let desc = s("desc").to_ascii_lowercase();
    let media = s("media").to_ascii_lowercase();
    let hw = v.get("hw").and_then(|x| x.as_bool());
    let speed_mbps = v
        .get("speed")
        .and_then(|x| x.as_u64())
        .filter(|&b| b > 0)
        .map(|b| b / 1_000_000);
    let mtu = v
        .get("mtu")
        .and_then(|x| x.as_u64())
        .and_then(|m| u32::try_from(m).ok());
    let tunnel = [
        "vpn",
        "wireguard",
        "tap-",
        "tunnel",
        "openvpn",
        "tailscale",
        "zerotier",
    ]
    .iter()
    .any(|w| desc.contains(w));
    let kind = if media.contains("802.11") || media.contains("wireless") {
        LinkKind::Wifi
    } else if tunnel {
        LinkKind::Vpn
    } else if desc.contains("hyper-v") || desc.contains("virtual") || hw == Some(false) {
        LinkKind::Virtual
    } else if media.contains("802.3") {
        LinkKind::Wired
    } else {
        LinkKind::Unknown
    };
    Link {
        iface: (!alias.is_empty()).then_some(alias.clone()),
        kind,
        link_mbps: if kind == LinkKind::Wired {
            speed_mbps
        } else {
            None
        },
        mtu,
        skip: (kind == LinkKind::Unknown).then_some("unreadable"),
        detail: (kind == LinkKind::Unknown).then(|| format!("adapter {alias}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, ipv4: &str, mtu: u32) -> NetInterface {
        NetInterface {
            name: name.into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            ipv4: ipv4.into(),
            mtu,
            flags: 0,
        }
    }

    #[test]
    fn the_console_link_is_the_interface_holding_the_address_in_use() {
        let list = [
            iface("lo0", "127.0.0.1", 16384),
            iface("eth0", "0.0.0.0", 1500),
            iface("wlan0", "192.168.1.50", 1500),
        ];
        let l = console_link(&list, Some(Ipv4Addr::new(192, 168, 1, 50)));
        assert_eq!(l.kind, LinkKind::Wifi);
        assert_eq!(l.iface.as_deref(), Some("wlan0"));
        assert_eq!(l.mtu, Some(1500));
        assert_eq!(l.skip, None);
        // Addressed by another name: the one interface with an address is it.
        let l = console_link(&list, Some(Ipv4Addr::new(10, 0, 0, 9)));
        assert_eq!(l.kind, LinkKind::Wifi);
    }

    #[test]
    fn a_console_link_that_cannot_be_told_apart_is_skipped_not_guessed() {
        let two = [
            iface("eth0", "192.168.1.50", 1500),
            iface("wlan0", "192.168.1.51", 1500),
        ];
        let l = console_link(&two, Some(Ipv4Addr::new(10, 0, 0, 9)));
        assert_eq!(l.skip, Some("not_found"));
        let odd = [iface("bge0", "192.168.1.50", 1500)];
        let l = console_link(&odd, Some(Ipv4Addr::new(192, 168, 1, 50)));
        assert_eq!(l.kind, LinkKind::Unknown);
        assert_eq!(l.skip, Some("unreadable"));
        assert_eq!(console_iface_kind("eth0"), LinkKind::Wired);
        assert_eq!(console_iface_kind("WLAN0"), LinkKind::Wifi);
    }

    const ROUTES: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
        docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n\
        tun0\t0000000A\t00000000\t0000\t0\t0\t0\t000000FF\t0\t0\t0\n";

    #[test]
    fn linux_routes_by_the_longest_prefix_then_the_lowest_metric() {
        if cfg!(target_endian = "big") {
            return; // the fixture is a little-endian kernel's table
        }
        let on_lan = linux_route_iface(ROUTES, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(on_lan.as_deref(), Some("eth0"));
        let docker = linux_route_iface(ROUTES, Ipv4Addr::new(172, 17, 0, 2));
        assert_eq!(docker.as_deref(), Some("docker0"));
        // Default route: eth0's metric 100 beats wlan0's 600.
        let away = linux_route_iface(ROUTES, Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(away.as_deref(), Some("eth0"));
        // A route that is down is not used.
        let down = linux_route_iface(ROUTES, Ipv4Addr::new(10, 0, 0, 5));
        assert_eq!(down.as_deref(), Some("eth0"));
        assert_eq!(
            linux_route_iface("Iface\n", Ipv4Addr::new(1, 2, 3, 4)),
            None
        );
    }

    #[test]
    fn linux_interfaces_are_judged_by_what_sysfs_shows() {
        let cable = linux_classify(&LinuxIf {
            name: "enp3s0".into(),
            physical: true,
            speed: Some("100".into()),
            mtu: Some("1500".into()),
            ..Default::default()
        });
        assert_eq!(cable.kind, LinkKind::Wired);
        assert_eq!(cable.link_mbps, Some(100));
        assert_eq!(cable.mtu, Some(1500));
        let wifi = linux_classify(&LinuxIf {
            name: "wlp2s0".into(),
            physical: true,
            wireless: true,
            speed: Some("866".into()),
            ..Default::default()
        });
        assert_eq!(wifi.kind, LinkKind::Wifi);
        assert_eq!(wifi.link_mbps, None, "a radio rate is not a cable's speed");
        let veth = linux_classify(&LinuxIf {
            name: "eth0".into(),
            speed: Some("10000".into()),
            ..Default::default()
        });
        assert_eq!(veth.kind, LinkKind::Virtual);
        assert_eq!(veth.link_mbps, None);
        let unplugged = linux_classify(&LinuxIf {
            name: "eno1".into(),
            physical: true,
            speed: Some("-1".into()),
            ..Default::default()
        });
        assert_eq!(unplugged.link_mbps, None);
        let vpn = linux_classify(&LinuxIf {
            name: "wg0".into(),
            ..Default::default()
        });
        assert_eq!(vpn.kind, LinkKind::Vpn);
    }

    const PORTS: &str = "\nHardware Port: Ethernet\nDevice: en0\nEthernet Address: aa\n\n\
        Hardware Port: Wi-Fi\nDevice: en1\nEthernet Address: bb\n\n\
        Hardware Port: Thunderbolt Bridge\nDevice: bridge0\nEthernet Address: cc\n\n\
        Hardware Port: USB 10/100/1000 LAN\nDevice: en7\nEthernet Address: dd\n";

    #[test]
    fn macos_names_the_port_and_reads_the_media_line() {
        let route = "   route to: 192.168.1.50\ndestination: 192.168.1.0\n  interface: en7\n      flags: <UP,DONE>\n";
        assert_eq!(mac_route_iface(route).as_deref(), Some("en7"));
        let ifc = "en7: flags=8863<UP,BROADCAST> mtu 1500\n\tether dd\n\tmedia: autoselect (100baseTX <full-duplex>)\n\tstatus: active\n";
        let l = mac_classify("en7", PORTS, ifc);
        assert_eq!(l.kind, LinkKind::Wired);
        assert_eq!(l.link_mbps, Some(100));
        assert_eq!(l.mtu, Some(1500));
        let wifi = "en1: flags=8863<UP> mtu 1500\n\tmedia: autoselect\n\tstatus: active\n";
        let l = mac_classify("en1", PORTS, wifi);
        assert_eq!(l.kind, LinkKind::Wifi);
        assert_eq!(l.link_mbps, None);
        let jumbo = "en0: flags=8863<UP> mtu 9000\n\tmedia: autoselect (10Gbase-T <full-duplex>)\n";
        assert_eq!(mac_ifconfig(jumbo), (Some(9000), Some(10_000)));
        assert_eq!(
            mac_ifconfig("x: mtu 1500\n\tmedia: autoselect (2500Base-T <full-duplex>)\n").1,
            Some(2500)
        );
        assert_eq!(
            mac_classify("utun3", PORTS, "utun3: mtu 1380\n").kind,
            LinkKind::Vpn
        );
        assert_eq!(mac_classify("bridge0", PORTS, "").kind, LinkKind::Virtual);
        let unknown = mac_classify("en9", PORTS, "en9: mtu 1500\n\tmedia: autoselect\n");
        assert_eq!(unknown.skip, Some("unreadable"));
    }

    #[test]
    fn windows_reads_the_adapter_the_route_uses() {
        let cable = r#"{"alias":"Ethernet 3","desc":"Realtek PCIe GbE Family Controller","media":"802.3","speed":100000000,"hw":true,"mtu":1500}"#;
        let l = windows_classify(cable);
        assert_eq!(l.kind, LinkKind::Wired);
        assert_eq!(l.link_mbps, Some(100));
        assert_eq!(l.iface.as_deref(), Some("Ethernet 3"));
        let wifi = "\u{feff}{\"alias\":\"Wi-Fi\",\"desc\":\"Intel(R) Wi-Fi 6 AX201\",\"media\":\"Native 802.11\",\"speed\":1201000000,\"hw\":true,\"mtu\":1500}";
        let l = windows_classify(wifi);
        assert_eq!(l.kind, LinkKind::Wifi);
        assert_eq!(l.link_mbps, None);
        let hyperv = r#"{"alias":"vEthernet (External)","desc":"Hyper-V Virtual Ethernet Adapter","media":"802.3","speed":10000000000,"hw":false,"mtu":1500}"#;
        assert_eq!(windows_classify(hyperv).kind, LinkKind::Virtual);
        let vpn = r#"{"alias":"Corp","desc":"WireGuard Tunnel","media":"Unspecified","speed":100000000000,"hw":false,"mtu":1420}"#;
        assert_eq!(windows_classify(vpn).kind, LinkKind::Vpn);
        assert_eq!(windows_classify("not json").skip, Some("unreadable"));
        // The address is validated before it is written into the script.
        assert!(windows_script(Ipv4Addr::new(192, 168, 1, 50)).contains("'192.168.1.50'"));
    }
}
