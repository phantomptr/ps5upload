// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! The few `tkproto.TakionMessage` messages the stream needs, encoded and
//! decoded by hand (proto2 wire format) rather than through a protobuf code
//! generator. Field numbers from chiaki-ng's `lib/protobuf/takion.proto`
//! (AGPL-3.0).

use anyhow::{bail, Result};

pub mod kind {
    pub const BIG: u64 = 0;
    pub const BANG: u64 = 1;
    pub const HEARTBEAT: u64 = 3;
    pub const CORRUPTFRAME: u64 = 5;
    pub const DISCONNECT: u64 = 8;
    pub const STREAMINFO: u64 = 13;
    pub const STREAMINFOACK: u64 = 14;
    pub const CONNECTIONQUALITY: u64 = 16;
    pub const CONTROLLERCONNECTION: u64 = 21;
    pub const IDRREQUEST: u64 = 25;
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_key(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, ((field as u64) << 3) | wire as u64);
}

fn put_u(out: &mut Vec<u8>, field: u32, v: u64) {
    put_key(out, field, 0);
    put_varint(out, v);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, v: &[u8]) {
    put_key(out, field, 2);
    put_varint(out, v.len() as u64);
    out.extend_from_slice(v);
}

/// A message carrying only its type (HEARTBEAT, STREAMINFOACK, IDRREQUEST…).
pub fn bare(kind: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_u(&mut out, 1, kind);
    out
}

pub struct Big<'a> {
    pub client_version: u32,
    pub session_key: &'a str,
    pub launch_spec_b64: &'a str,
    pub ecdh_pub_key: &'a [u8],
    pub ecdh_sig: &'a [u8],
}

pub fn big(b: &Big) -> Vec<u8> {
    let mut p = Vec::new();
    put_u(&mut p, 1, b.client_version as u64);
    put_bytes(&mut p, 2, b.session_key.as_bytes());
    put_bytes(&mut p, 3, b.launch_spec_b64.as_bytes());
    put_bytes(&mut p, 4, &[0, 0, 0, 0]);
    put_bytes(&mut p, 5, b.ecdh_pub_key);
    put_bytes(&mut p, 6, b.ecdh_sig);
    let mut out = Vec::new();
    put_u(&mut out, 1, kind::BIG);
    put_bytes(&mut out, 2, &p);
    out
}

/// "A DualShock 4 is connected", which the console expects after STREAMINFO; or a DualSense,
/// so the console sends adaptive triggers and haptics (chiaki
/// `stream_connection_send_controller_connection`, `ControllerType` 2 / 6 in takion.proto).
pub fn controller_connection(dualsense: bool) -> Vec<u8> {
    let mut p = Vec::new();
    put_u(&mut p, 2, 1); // connected
    put_u(&mut p, 3, if dualsense { 6 } else { 2 }); // DUALSENSE / DUALSHOCK4
    let mut out = Vec::new();
    put_u(&mut out, 1, kind::CONTROLLERCONNECTION);
    put_bytes(&mut out, 22, &p);
    out
}

/// Our own STREAMINFO, carrying only the microphone's audio header (chiaki
/// `stream_connection_enable_microphone`; see mic.rs).
pub fn stream_info_mic(audio_header: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    put_bytes(&mut p, 2, audio_header);
    let mut out = Vec::new();
    put_u(&mut out, 1, kind::STREAMINFO);
    put_bytes(&mut out, 15, &p);
    out
}

/// Frames `start..=end` were lost (chiaki `stream_connection_send_corrupt_frame`).
pub fn corrupt_frame(start: u16, end: u16) -> Vec<u8> {
    let mut p = Vec::new();
    put_u(&mut p, 1, start as u64);
    put_u(&mut p, 2, end as u64);
    let mut out = Vec::new();
    put_u(&mut out, 1, kind::CORRUPTFRAME);
    put_bytes(&mut out, 6, &p);
    out
}

pub fn disconnect(reason: &str) -> Vec<u8> {
    let mut p = Vec::new();
    put_bytes(&mut p, 1, reason.as_bytes());
    let mut out = Vec::new();
    put_u(&mut out, 1, kind::DISCONNECT);
    put_bytes(&mut out, 10, &p);
    out
}

#[derive(Debug, Clone)]
pub enum Value {
    Varint(u64),
    Bytes(Vec<u8>),
    Fixed(u64),
}

/// Every field of one message, in order, without interpreting it.
pub fn fields(mut buf: &[u8]) -> Result<Vec<(u32, Value)>> {
    fn varint(buf: &mut &[u8]) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some((&b, rest)) = buf.split_first() else {
                bail!("protobuf varint runs past the end");
            };
            *buf = rest;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("protobuf varint too long")
    }
    let mut out = Vec::new();
    while !buf.is_empty() {
        let key = varint(&mut buf)?;
        let field = (key >> 3) as u32;
        let v = match key & 7 {
            0 => Value::Varint(varint(&mut buf)?),
            1 => {
                if buf.len() < 8 {
                    bail!("protobuf fixed64 runs past the end");
                }
                let v = u64::from_le_bytes(buf[..8].try_into().unwrap());
                buf = &buf[8..];
                Value::Fixed(v)
            }
            2 => {
                let n = varint(&mut buf)? as usize;
                if buf.len() < n {
                    bail!("protobuf field {field} runs past the end");
                }
                let v = buf[..n].to_vec();
                buf = &buf[n..];
                Value::Bytes(v)
            }
            5 => {
                if buf.len() < 4 {
                    bail!("protobuf fixed32 runs past the end");
                }
                let v = u32::from_le_bytes(buf[..4].try_into().unwrap()) as u64;
                buf = &buf[4..];
                Value::Fixed(v)
            }
            w => bail!("protobuf wire type {w} not supported"),
        };
        out.push((field, v));
    }
    Ok(out)
}

pub fn get_u(f: &[(u32, Value)], field: u32) -> Option<u64> {
    f.iter().find_map(|(n, v)| match v {
        Value::Varint(x) if *n == field => Some(*x),
        _ => None,
    })
}

pub fn get_bytes(f: &[(u32, Value)], field: u32) -> Option<&[u8]> {
    f.iter().find_map(|(n, v)| match v {
        Value::Bytes(x) if *n == field => Some(x.as_slice()),
        _ => None,
    })
}

pub fn all_bytes(f: &[(u32, Value)], field: u32) -> impl Iterator<Item = &[u8]> {
    f.iter().filter_map(move |(n, v)| match v {
        Value::Bytes(x) if *n == field => Some(x.as_slice()),
        _ => None,
    })
}

#[derive(Debug)]
pub struct Bang {
    pub version_accepted: bool,
    pub encrypted_key_accepted: bool,
    pub ecdh_pub_key: Vec<u8>,
    pub ecdh_sig: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
    /// The codec parameter sets (SPS/PPS, plus VPS for HEVC), Annex-B.
    pub video_header: Vec<u8>,
}

#[derive(Debug)]
pub struct StreamInfo {
    pub resolutions: Vec<Resolution>,
    pub audio_header: Vec<u8>,
}

/// The console's own view of the link, sent now and then while streaming (chiaki-ng's
/// `ConnectionQualityPayload`, which it only logs). `target_bitrate` is what its encoder aims
/// for: it falls when our congestion reports show loss. The units are not documented: the
/// values are passed on raw, for the logs.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct ConnectionQuality {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_loss: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss: Option<u64>,
}

pub enum Message {
    Bang(Bang),
    StreamInfo(StreamInfo),
    Disconnect(String),
    ConnectionQuality(ConnectionQuality),
    Other(u64),
}

fn get_fixed(f: &[(u32, Value)], field: u32) -> Option<u64> {
    f.iter().find_map(|(n, v)| match v {
        Value::Fixed(x) if *n == field => Some(*x),
        _ => None,
    })
}

pub fn decode(buf: &[u8]) -> Result<Message> {
    let f = fields(buf)?;
    let Some(kind) = get_u(&f, 1) else {
        bail!("Takion message without a type");
    };
    Ok(match kind {
        kind::BANG => {
            let p = fields(get_bytes(&f, 3).unwrap_or_default())?;
            Message::Bang(Bang {
                encrypted_key_accepted: get_u(&p, 3).unwrap_or(0) != 0,
                version_accepted: get_u(&p, 4).unwrap_or(0) != 0,
                ecdh_pub_key: get_bytes(&p, 8).unwrap_or_default().to_vec(),
                ecdh_sig: get_bytes(&p, 9).unwrap_or_default().to_vec(),
            })
        }
        kind::STREAMINFO => {
            let p = fields(get_bytes(&f, 15).unwrap_or_default())?;
            let mut resolutions = Vec::new();
            for r in all_bytes(&p, 1) {
                let r = fields(r)?;
                resolutions.push(Resolution {
                    width: get_u(&r, 1).unwrap_or(0) as u32,
                    height: get_u(&r, 2).unwrap_or(0) as u32,
                    video_header: get_bytes(&r, 3).unwrap_or_default().to_vec(),
                });
            }
            Message::StreamInfo(StreamInfo {
                resolutions,
                audio_header: get_bytes(&p, 2).unwrap_or_default().to_vec(),
            })
        }
        kind::DISCONNECT => {
            let p = fields(get_bytes(&f, 10).unwrap_or_default())?;
            let reason = String::from_utf8_lossy(get_bytes(&p, 1).unwrap_or_default()).into_owned();
            Message::Disconnect(reason)
        }
        kind::CONNECTIONQUALITY => {
            let p = fields(get_bytes(&f, 17).unwrap_or_default())?;
            Message::ConnectionQuality(ConnectionQuality {
                target_bitrate: get_u(&p, 1).map(|v| v as u32),
                upstream_bitrate: get_u(&p, 2).map(|v| v as u32),
                upstream_loss: get_fixed(&p, 3).map(|v| f32::from_bits(v as u32)),
                rtt: get_fixed(&p, 5).map(f64::from_bits),
                loss: get_u(&p, 6),
            })
        }
        k => Message::Other(k),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_disconnect() {
        let m = disconnect("bye");
        match decode(&m).unwrap() {
            Message::Disconnect(r) => assert_eq!(r, "bye"),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn reads_the_consoles_connection_quality() {
        // TakionMessage{type CONNECTIONQUALITY (16), connection_quality_payload (17) {
        // target_bitrate 9000, upstream_loss 0.5f, rtt 3.25, loss 7}}, by hand.
        let mut q = vec![0x08];
        put_varint(&mut q, 9000);
        q.push(0x1d);
        q.extend_from_slice(&0.5f32.to_le_bytes());
        q.push(0x29);
        q.extend_from_slice(&3.25f64.to_le_bytes());
        q.extend_from_slice(&[0x30, 7]);
        let mut m = vec![0x08, 0x10, 0x8a, 0x01, q.len() as u8];
        m.extend_from_slice(&q);
        match decode(&m).unwrap() {
            Message::ConnectionQuality(c) => assert_eq!(
                c,
                ConnectionQuality {
                    target_bitrate: Some(9000),
                    upstream_bitrate: None,
                    upstream_loss: Some(0.5),
                    rtt: Some(3.25),
                    loss: Some(7),
                }
            ),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn heartbeat_is_two_bytes() {
        assert_eq!(bare(kind::HEARTBEAT), vec![0x08, 0x03]);
    }

    #[test]
    fn mic_stream_info_matches_nanopb() {
        // nanopb encoding of TakionMessage{type STREAMINFO (13),
        // stream_info_payload (15) {audio_header (2): 14 bytes}}, by hand.
        let h = super::super::mic::stream_info_audio_header();
        let mut want = vec![0x08, 0x0d, 0x7a, 0x10, 0x12, 0x0e];
        want.extend_from_slice(&h);
        assert_eq!(stream_info_mic(&h), want);
    }
}
