//! A STUN binding request (RFC 8489) from the ICE socket itself: the server's answer is this
//! engine's address as the internet sees it (a server-reflexive candidate). Only what that needs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// A binding request with transaction id `tx`.
pub(crate) fn request(tx: [u8; 12]) -> Vec<u8> {
    let mut b = Vec::with_capacity(20);
    b.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend_from_slice(&MAGIC.to_be_bytes());
    b.extend_from_slice(&tx);
    b
}

/// The transaction id of a binding success answer, if `b` is one.
pub(crate) fn response_tx(b: &[u8]) -> Option<[u8; 12]> {
    if b.len() < 20
        || u16::from_be_bytes([b[0], b[1]]) != BINDING_SUCCESS
        || u32::from_be_bytes([b[4], b[5], b[6], b[7]]) != MAGIC
    {
        return None;
    }
    b[8..20].try_into().ok()
}

/// The mapped address in a binding success answer (XOR-MAPPED-ADDRESS, else MAPPED-ADDRESS).
pub(crate) fn mapped(b: &[u8]) -> Option<SocketAddr> {
    let tx = response_tx(b)?;
    let len = u16::from_be_bytes([b[2], b[3]]) as usize;
    let body = b.get(20..20 + len)?;
    let mut plain = None;
    let mut i = 0;
    while i + 4 <= body.len() {
        let t = u16::from_be_bytes([body[i], body[i + 1]]);
        let l = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let v = body.get(i + 4..i + 4 + l)?;
        match t {
            XOR_MAPPED_ADDRESS => return addr(v, Some(&tx)),
            MAPPED_ADDRESS => plain = addr(v, None),
            _ => {}
        }
        i += 4 + l.div_ceil(4) * 4;
    }
    plain
}

fn addr(v: &[u8], xor_tx: Option<&[u8; 12]>) -> Option<SocketAddr> {
    if v.len() < 4 {
        return None;
    }
    let cookie = MAGIC.to_be_bytes();
    let mut port = u16::from_be_bytes([v[2], v[3]]);
    if xor_tx.is_some() {
        port ^= (MAGIC >> 16) as u16;
    }
    let ip = match (v[1], v.len()) {
        (0x01, 8) => {
            let mut o: [u8; 4] = v[4..8].try_into().ok()?;
            if xor_tx.is_some() {
                for (k, x) in o.iter_mut().enumerate() {
                    *x ^= cookie[k];
                }
            }
            IpAddr::V4(Ipv4Addr::from(o))
        }
        (0x02, 20) => {
            let mut o: [u8; 16] = v[4..20].try_into().ok()?;
            if let Some(tx) = xor_tx {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&cookie);
                key[4..].copy_from_slice(tx);
                for (k, x) in o.iter_mut().enumerate() {
                    *x ^= key[k];
                }
            }
            IpAddr::V6(Ipv6Addr::from(o))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// A binding success answer saying `who` (what a STUN server sends; the tests' fake server).
#[cfg(test)]
pub(crate) fn answer(tx: [u8; 12], who: SocketAddr) -> Vec<u8> {
    let SocketAddr::V4(w) = who else {
        panic!("IPv4 only")
    };
    let mut b = Vec::new();
    b.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    b.extend_from_slice(&12u16.to_be_bytes());
    b.extend_from_slice(&MAGIC.to_be_bytes());
    b.extend_from_slice(&tx);
    b.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes());
    b.extend_from_slice(&[0, 1]);
    b.extend_from_slice(&(w.port() ^ (MAGIC >> 16) as u16).to_be_bytes());
    let ip = u32::from(*w.ip()) ^ MAGIC;
    b.extend_from_slice(&ip.to_be_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_answer_gives_the_outside_address() {
        let tx = [7u8; 12];
        let req = request(tx);
        assert_eq!((req.len(), &req[8..]), (20, &tx[..]));
        let who: SocketAddr = "203.0.113.9:40123".parse().unwrap();
        let a = answer(tx, who);
        assert_eq!(response_tx(&a), Some(tx));
        assert_eq!(mapped(&a), Some(who));
        // a request is no answer, a truncated answer has no address
        assert_eq!(response_tx(&req), None);
        assert_eq!(mapped(&a[..a.len() - 2]), None);
    }

    #[test]
    fn rfc5769_ipv6_xor_mapped_address() {
        // RFC 5769 2.3, without the software/integrity/fingerprint attributes' meaning.
        let tx = [
            0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
        ];
        let mut b = vec![0x01, 0x01, 0x00, 0x18, 0x21, 0x12, 0xa4, 0x42];
        b.extend_from_slice(&tx);
        b.extend_from_slice(&[
            0x00, 0x20, 0x00, 0x14, 0x00, 0x02, 0xa1, 0x47, 0x01, 0x13, 0xa9, 0xfa, 0xa5, 0xd3,
            0xf1, 0x79, 0xbc, 0x25, 0xf4, 0xb5, 0xbe, 0xd2, 0xb9, 0xd9,
        ]);
        assert_eq!(
            mapped(&b),
            Some(
                "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
                    .parse()
                    .unwrap()
            )
        );
    }
}
