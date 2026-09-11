//! Wake-on-LAN for a console that is asleep.
//!
//! This has to run on the HOST. Every other "power" action in ps5upload is a
//! request to the payload, but the payload is not running in rest mode — it is
//! killed when the console suspends (measured on both 5.10 and 9.60). So the
//! one power transition the app could never offer was the one people ask for:
//! turning the thing back on.
//!
//! A magic packet needs the console's MAC, which we can only learn while it is
//! awake. The caller is expected to have recorded it during a previous
//! session; there is no way to discover it from a sleeping machine.
//!
//! This only works when the user has enabled "Enable turning on PS5 from
//! network" on the console. Nothing here can detect that setting, so a wake
//! that silently does nothing is an expected outcome and the UI must say so
//! rather than reporting success.

use std::net::{IpAddr, SocketAddr, UdpSocket};

use anyhow::{anyhow, Result};

/// Ports a magic packet is conventionally sent to.
///
/// 9 (discard) is the near-universal convention. 7 (echo) is the older one and
/// some NICs still listen there. Sending both costs two datagrams and removes
/// a whole class of "it just doesn't work on my network" reports.
pub const WOL_PORTS: [u16; 2] = [9, 7];

/// Parse `a1:b2:c3:d4:e5:f6` (or `-` separated, or bare hex) into six bytes.
pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let cleaned: String = mac
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' '))
        .collect();
    if cleaned.len() != 12 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("not a MAC address: {mac}"));
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow!("not a MAC address: {mac}"))?;
    }
    Ok(out)
}

/// Build the 102-byte magic packet: six `0xFF` bytes then the MAC 16 times.
pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(6 + 16 * 6);
    pkt.extend_from_slice(&[0xFF; 6]);
    for _ in 0..16 {
        pkt.extend_from_slice(&mac);
    }
    pkt
}

/// The broadcast address to aim at, given the console's last known IP.
///
/// A global broadcast (255.255.255.255) is dropped by many routers and by
/// macOS without extra privileges, so prefer the /24 directed broadcast for
/// the console's own subnet — which is what a home network almost always is.
/// Falls back to the global address when the host is not a plain IPv4.
pub fn broadcast_for(host: &str) -> String {
    let bare = host.split(':').next().unwrap_or(host);
    if let Ok(IpAddr::V4(v4)) = bare.parse::<IpAddr>() {
        let o = v4.octets();
        return format!("{}.{}.{}.255", o[0], o[1], o[2]);
    }
    "255.255.255.255".to_string()
}

/// Send the magic packet. Returns how many datagrams actually went out.
///
/// Sending is best-effort per port: a firewall that blocks one does not mean
/// the other failed, and reporting a hard error when one of two succeeded
/// would send the user looking for a problem they do not have.
pub fn wake(mac: &str, last_known_host: &str) -> Result<usize> {
    let parsed = parse_mac(mac)?;
    let packet = magic_packet(parsed);
    let target = broadcast_for(last_known_host);

    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| anyhow!("binding a socket: {e}"))?;
    socket
        .set_broadcast(true)
        .map_err(|e| anyhow!("enabling broadcast: {e}"))?;

    let mut sent = 0usize;
    let mut last_err: Option<String> = None;
    for port in WOL_PORTS {
        let addr: SocketAddr = match format!("{target}:{port}").parse() {
            Ok(a) => a,
            Err(e) => {
                last_err = Some(e.to_string());
                continue;
            }
        };
        match socket.send_to(&packet, addr) {
            Ok(_) => sent += 1,
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    if sent == 0 {
        return Err(anyhow!(
            "could not send a wake packet to {target}: {}",
            last_err.unwrap_or_else(|| "no route".into())
        ));
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_separators_people_actually_paste() {
        let want = [0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6];
        assert_eq!(parse_mac("a1:b2:c3:d4:e5:f6").unwrap(), want);
        assert_eq!(parse_mac("A1-B2-C3-D4-E5-F6").unwrap(), want);
        assert_eq!(parse_mac("a1b2c3d4e5f6").unwrap(), want);
    }

    #[test]
    fn refuses_something_that_is_not_a_mac() {
        // Worth refusing loudly: a wake sent to a garbage address fails
        // silently on the wire, and the user would blame the console.
        assert!(parse_mac("").is_err());
        assert!(parse_mac("a1:b2:c3:d4:e5").is_err());
        assert!(parse_mac("zz:b2:c3:d4:e5:f6").is_err());
        assert!(parse_mac("192.168.1.50").is_err());
    }

    #[test]
    fn the_packet_is_the_standard_102_bytes() {
        let p = magic_packet([1, 2, 3, 4, 5, 6]);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        // The MAC repeats exactly sixteen times after the header.
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &[1, 2, 3, 4, 5, 6]);
        }
    }

    #[test]
    fn aims_at_the_consoles_own_subnet() {
        // A global broadcast is dropped by plenty of home routers; the /24
        // directed broadcast is what actually reaches the console.
        assert_eq!(broadcast_for("192.168.86.99"), "192.168.86.255");
        assert_eq!(broadcast_for("192.168.86.99:9113"), "192.168.86.255");
        assert_eq!(broadcast_for("10.0.1.5"), "10.0.1.255");
        // Not an IPv4 literal — fall back rather than guess.
        assert_eq!(broadcast_for("ps5.local"), "255.255.255.255");
        assert_eq!(broadcast_for(""), "255.255.255.255");
    }
}
