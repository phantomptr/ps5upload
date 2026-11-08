//! A Binding-only STUN server (RFC 5389): a Binding request gets a success response carrying
//! XOR-MAPPED-ADDRESS (the address the request came from). Everything else is dropped without an
//! answer, as is a request over the per-client rate. The answer is at most 44 bytes for a request
//! of at least 20, so the server cannot be used to amplify traffic.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::limits::{canonical_ip, IpKey, RateTable};
use crate::metrics::Metrics;
use crate::Shared;

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
pub const BINDING_REQUEST: u16 = 0x0001;
pub const BINDING_SUCCESS: u16 = 0x0101;
pub const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const HEADER_LEN: usize = 20;
/// Larger datagrams are not a Binding request a browser sends.
const MAX_REQUEST: usize = 548;

/// The transaction id of a well-formed Binding request, or None.
pub fn parse_binding_request(pkt: &[u8]) -> Option<[u8; 12]> {
    if pkt.len() < HEADER_LEN || pkt.len() > MAX_REQUEST {
        return None;
    }
    let msg_type = u16::from_be_bytes([pkt[0], pkt[1]]);
    // The top two bits of every STUN message are zero; that also rules out the type check
    // matching some other protocol by chance.
    if msg_type != BINDING_REQUEST {
        return None;
    }
    let len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    if !len.is_multiple_of(4) || HEADER_LEN + len != pkt.len() {
        return None;
    }
    if u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) != MAGIC_COOKIE {
        return None;
    }
    let mut txid = [0u8; 12];
    txid.copy_from_slice(&pkt[8..20]);
    Some(txid)
}

/// The largest response: header, attribute header, an IPv6 XOR-MAPPED-ADDRESS.
pub const MAX_RESPONSE: usize = HEADER_LEN + 4 + 20;

/// Writes the Binding success response for a request from `from` into `out` (no allocation) and
/// returns its length: 32 bytes for IPv4, 44 for IPv6.
pub fn write_binding_response(
    txid: &[u8; 12],
    from: SocketAddr,
    out: &mut [u8; MAX_RESPONSE],
) -> usize {
    let xport = from.port() ^ (MAGIC_COOKIE >> 16) as u16;
    // The attribute value: reserved byte, family, port, address.
    let v = &mut out[HEADER_LEN + 4..];
    v[0] = 0;
    v[2..4].copy_from_slice(&xport.to_be_bytes());
    let value_len = match canonical_ip(from.ip()) {
        IpAddr::V4(v4) => {
            v[1] = 0x01;
            v[4..8].copy_from_slice(&(u32::from(v4) ^ MAGIC_COOKIE).to_be_bytes());
            8
        }
        IpAddr::V6(v6) => {
            v[1] = 0x02;
            let mut key = [0u8; 16];
            key[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
            key[4..].copy_from_slice(txid);
            for (dst, (a, k)) in v[4..20].iter_mut().zip(v6.octets().iter().zip(key.iter())) {
                *dst = a ^ k;
            }
            20
        }
    };
    let attr_len = value_len as u16;
    out[0..2].copy_from_slice(&BINDING_SUCCESS.to_be_bytes());
    out[2..4].copy_from_slice(&(4 + attr_len).to_be_bytes());
    out[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    out[8..20].copy_from_slice(txid);
    out[20..22].copy_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    out[22..24].copy_from_slice(&attr_len.to_be_bytes());
    HEADER_LEN + 4 + value_len
}

/// The Binding success response for a request from `from`, as a new vector (tests, tools).
pub fn binding_response(txid: &[u8; 12], from: SocketAddr) -> Vec<u8> {
    let mut out = [0u8; MAX_RESPONSE];
    let n = write_binding_response(txid, from, &mut out);
    out[..n].to_vec()
}

/// Decodes the XOR-MAPPED-ADDRESS of a Binding success response (what a client does; used by the
/// tests and handy for a manual check).
pub fn decode_xor_mapped_address(resp: &[u8]) -> Option<SocketAddr> {
    if resp.len() < HEADER_LEN
        || u16::from_be_bytes([resp[0], resp[1]]) != BINDING_SUCCESS
        || u32::from_be_bytes([resp[4], resp[5], resp[6], resp[7]]) != MAGIC_COOKIE
    {
        return None;
    }
    let len = u16::from_be_bytes([resp[2], resp[3]]) as usize;
    let body = resp.get(HEADER_LEN..HEADER_LEN + len)?;
    let txid = &resp[8..20];
    let mut at = 0;
    while at + 4 <= body.len() {
        let t = u16::from_be_bytes([body[at], body[at + 1]]);
        let l = u16::from_be_bytes([body[at + 2], body[at + 3]]) as usize;
        let v = body.get(at + 4..at + 4 + l)?;
        if t == ATTR_XOR_MAPPED_ADDRESS && l >= 8 {
            let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
            return match v[1] {
                0x01 => {
                    let a = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ MAGIC_COOKIE;
                    Some(SocketAddr::new(Ipv4Addr::from(a).into(), port))
                }
                0x02 if l >= 20 => {
                    let mut key = [0u8; 16];
                    key[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
                    key[4..].copy_from_slice(txid);
                    let mut o = [0u8; 16];
                    for (dst, (a, k)) in o.iter_mut().zip(v[4..20].iter().zip(key.iter())) {
                        *dst = a ^ k;
                    }
                    Some(SocketAddr::new(Ipv6Addr::from(o).into(), port))
                }
                _ => None,
            };
        }
        at += 4 + l.div_ceil(4) * 4;
    }
    None
}

/// Answers Binding requests on `sock` until the shutdown signal.
pub async fn serve(sock: UdpSocket, shared: Arc<Shared>, table: Arc<Mutex<RateTable>>) {
    let mut buf = [0u8; 1500];
    let mut resp = [0u8; MAX_RESPONSE];
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        let (n, from) = tokio::select! {
            r = sock.recv_from(&mut buf) => match r {
                Ok(v) => v,
                // ICMP errors from earlier sends surface here on some systems; keep serving.
                Err(_) => continue,
            },
            _ = shutdown.changed() => return,
        };
        let Some(txid) = parse_binding_request(&buf[..n]) else {
            Metrics::inc(&shared.metrics.stun_ignored);
            continue;
        };
        let ok = {
            let mut t = table.lock().unwrap_or_else(|e| e.into_inner());
            t.hit(
                IpKey::of(from.ip()),
                Instant::now(),
                shared.config.rate_window,
                shared.config.max_stun_per_ip,
            )
        };
        if !ok {
            Metrics::inc(&shared.metrics.stun_limited);
            continue;
        }
        Metrics::inc(&shared.metrics.stun_requests);
        let n = write_binding_response(&txid, from, &mut resp);
        let _ = sock.send_to(&resp[..n], from).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(txid: [u8; 12]) -> Vec<u8> {
        let mut p = vec![0x00, 0x01, 0x00, 0x00];
        p.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        p.extend_from_slice(&txid);
        p
    }

    #[test]
    fn v4_round_trip_and_rfc_bytes() {
        let txid = [7u8; 12];
        assert_eq!(parse_binding_request(&request(txid)), Some(txid));
        let from: SocketAddr = "192.0.2.1:32853".parse().unwrap();
        let r = binding_response(&txid, from);
        assert_eq!(r.len(), 32);
        // RFC 5769 2.2: port 32853 XOR 0x2112 = 0xa147, 192.0.2.1 XOR cookie = e1 12 a6 43.
        assert_eq!(
            &r[20..32],
            &[0, 0x20, 0, 8, 0, 1, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43]
        );
        assert_eq!(decode_xor_mapped_address(&r), Some(from));
    }

    #[test]
    fn v6_and_mapped_v4() {
        let txid = *b"abcdefghijkl";
        let from: SocketAddr = "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
            .parse()
            .unwrap();
        let r = binding_response(&txid, from);
        assert_eq!(r.len(), 44);
        assert_eq!(decode_xor_mapped_address(&r), Some(from));
        let mapped: SocketAddr = "[::ffff:198.51.100.9]:4000".parse().unwrap();
        let r = binding_response(&txid, mapped);
        assert_eq!(
            decode_xor_mapped_address(&r),
            Some("198.51.100.9:4000".parse().unwrap())
        );
    }

    #[test]
    fn rejects_non_binding() {
        let mut p = request([1; 12]);
        p[1] = 0x03; // Allocate (TURN)
        assert_eq!(parse_binding_request(&p), None);
        let mut p = request([1; 12]);
        p[4] = 0; // no magic cookie (RFC 3489)
        assert_eq!(parse_binding_request(&p), None);
        let mut p = request([1; 12]);
        p[3] = 4; // length says 4, body is empty
        assert_eq!(parse_binding_request(&p), None);
        assert_eq!(parse_binding_request(b"GET / HTTP/1.1\r\n\r\n\r\n"), None);
        assert_eq!(parse_binding_request(&[0u8; 10]), None);
    }
}
