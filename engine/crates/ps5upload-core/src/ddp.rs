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

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
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

impl DdpStatus {
    pub fn is_awake(&self) -> bool {
        self.code == 200
    }
    /// In standby and therefore wakeable. This is the state that decides
    /// whether offering a Wake button means anything.
    pub fn is_standby(&self) -> bool {
        self.code == 620
    }
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
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| anyhow!("binding a socket: {e}"))?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_broadcast(true)?;

    let pkt = format!("SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{DDP_VERSION}\n\0");
    socket
        .send_to(pkt.as_bytes(), addr)
        .map_err(|e| anyhow!("sending SRCH to {addr}: {e}"))?;

    let mut buf = [0u8; 4096];
    let (n, _from) = socket.recv_from(&mut buf).map_err(|e| {
        anyhow!(
            "no reply from {addr}: {e}. A console only answers when \
             Remote Play is enabled in its settings."
        )
    })?;
    parse_reply(&String::from_utf8_lossy(&buf[..n]))
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
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| anyhow!("binding a socket: {e}"))?;
    socket.set_broadcast(true)?;

    let pkt = format!(
        "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:m\napp-type:r\n\
         user-credential:{}\ndevice-discovery-protocol-version:{DDP_VERSION}\n",
        credential.trim()
    );
    socket
        .send_to(pkt.as_bytes(), addr)
        .map_err(|e| anyhow!("sending WAKEUP to {addr}: {e}"))?;
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
        assert!(s.is_awake() && !s.is_standby());
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
        assert!(s.is_standby() && !s.is_awake());
        assert_eq!(s.status_text, "Server Standby");
        // A colon inside the value must survive: split on the FIRST colon only.
        assert_eq!(s.running_app_name, "Black Myth: Wukong");
        assert_eq!(s.running_app_titleid, "PPSA23226");
    }

    #[test]
    fn an_unknown_code_is_reported_rather_than_guessed() {
        let s = parse_reply("HTTP/1.1 500 Kaboom\n").unwrap();
        assert_eq!(s.code, 500);
        assert!(!s.is_awake() && !s.is_standby());
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
    fn waking_without_a_credential_is_refused_up_front() {
        // The console silently ignores a credential-less WAKEUP, so refusing
        // here is the only way the caller learns what is missing.
        assert!(wake("192.168.1.50", "").is_err());
        assert!(wake("192.168.1.50", "   ").is_err());
    }
}
