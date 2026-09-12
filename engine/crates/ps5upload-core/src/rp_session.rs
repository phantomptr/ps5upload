//! Remote Play control session — logging the console into its user after a
//! network wake.
//!
//! A DDP WAKEUP powers a console on, but leaves it at the user-select
//! screen: a network wake deliberately waits for a Remote Play session to
//! say who is connecting, which is why local auto-login does not fire. A
//! controller press logs its user in; a bare wake does not. To land on the
//! user's home screen the way the PS Remote Play app (and chiaki) do, we
//! have to establish the session's **control connection** — the point at
//! which the console signs the registered user in. The video stream that a
//! real client sets up afterwards is not needed and not built.
//!
//! Ported from playactor (ISC) and cross-checked against chiaki (AGPL, read
//! only to confirm behaviour). Two stages:
//!
//! * **session-init** — a plain, unencrypted `GET /sie/ps5/rp/sess/init` on
//!   TCP 9295 carrying the registration key in hex. The console replies with
//!   an `RP-Nonce`.
//! * **ctrl** — `GET /sie/ps5/rp/sess/ctrl` on the same port, whose
//!   `RP-Auth` / `RP-Did` / `RP-OSType` headers are AES-128-CFB encrypted
//!   with a key derived from that nonce and the console's **RP-Key**. When
//!   the console accepts it, it logs the user in.
//!
//! Both keys — the registration key and the RP-Key ("morning") — come from
//! a pairing. We do not mint them here; they are supplied by the caller,
//! harvested from an existing pairing the same way the wake credential is.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

use aes::cipher::KeyIvInit;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;

use crate::rp_regist::{generate_iv, resolve, split_http};
use crate::rp_regist_keys::{PS5_AUTH_NONCE_KEY, PS5_AUTH_SEED_KEY};

type Aes128CfbEnc = cfb_mode::Encryptor<aes::Aes128>;

/// Session and ctrl share the registration port.
const SESSION_PORT: u16 = 9295;
const KEY_SIZE: usize = 16;
const RP_VERSION: &str = "1.0";

/// The two secrets a pairing produces, both needed to open a session.
#[derive(Clone)]
pub struct SessionCreds {
    /// The 16-byte registration key (the wake credential derives from this).
    pub regist_key: [u8; KEY_SIZE],
    /// The 16-byte RP-Key — "morning" in the protocol.
    pub morning: [u8; KEY_SIZE],
}

impl SessionCreds {
    /// Build from hex strings, as the keys are stored and harvested.
    pub fn from_hex(regist_key_hex: &str, morning_hex: &str) -> Result<Self> {
        Ok(Self {
            regist_key: hex16(regist_key_hex).context("regist_key")?,
            morning: hex16(morning_hex).context("morning/rp_key")?,
        })
    }
}

fn hex16(s: &str) -> Result<[u8; KEY_SIZE]> {
    let s = s.trim();
    if s.len() != KEY_SIZE * 2 {
        bail!("expected {} hex chars, got {}", KEY_SIZE * 2, s.len());
    }
    let mut out = [0u8; KEY_SIZE];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| anyhow!("not hex: {s:?}"))?;
    }
    Ok(out)
}

/// The "ambassador" — the nonce transformed so it can key the IV.
///
/// PS5 form: each byte has 0x2d and its index subtracted, then is XORed with
/// a table entry chosen by the nonce's first byte. Mirrors playactor's
/// `transformServerNonceForAuth`.
fn ambassador(nonce: &[u8; KEY_SIZE]) -> [u8; KEY_SIZE] {
    let off = (nonce[0] >> 3) as usize * 0x70;
    let mut out = [0u8; KEY_SIZE];
    for (i, o) in out.iter_mut().enumerate() {
        let v = nonce[i].wrapping_sub(0x2d).wrapping_sub(i as u8);
        *o = v ^ PS5_AUTH_NONCE_KEY[off + i];
    }
    out
}

/// The "bright" — the AES key for the ctrl cipher, mixed from the RP-Key,
/// the nonce, and a second table chosen by the nonce's last byte. Mirrors
/// playactor's `generateAuthSeed` (PS5 branch).
fn bright(nonce: &[u8; KEY_SIZE], morning: &[u8; KEY_SIZE]) -> [u8; KEY_SIZE] {
    let off = (nonce[7] >> 3) as usize * 0x70;
    let mut out = [0u8; KEY_SIZE];
    for (i, o) in out.iter_mut().enumerate() {
        let v = morning[i].wrapping_add(0x18).wrapping_add(i as u8);
        *o = v ^ nonce[i] ^ PS5_AUTH_SEED_KEY[off + i];
    }
    out
}

/// One field of the ctrl request: AES-128-CFB with the bright key and an IV
/// derived from the ambassador and this field's counter. Each field uses the
/// next counter, so the console can reproduce each IV in turn.
fn auth_encrypt(bright: &[u8; 16], ambassador: &[u8; 16], counter: u64, data: &[u8]) -> Vec<u8> {
    let iv = generate_iv(ambassador, counter);
    let mut buf = data.to_vec();
    Aes128CfbEnc::new(bright.into(), &iv.into()).encrypt(&mut buf);
    buf
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A 32-byte device id: a fixed prefix and suffix around random bytes. The
/// console does not check the random part; the shape is what matters.
fn make_did() -> [u8; 32] {
    let mut did = [0u8; 32];
    did[..10].copy_from_slice(&[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]);
    // 16 random bytes between the 10-byte prefix and the 6-byte (zero) suffix.
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut did[10..26]);
    }
    // did[26..32] stays zero — the suffix.
    did
}

/// Stage 1: the unencrypted session request. Returns the 16-byte nonce.
fn session_init(host: &str, regist_key: &[u8; KEY_SIZE]) -> Result<[u8; KEY_SIZE]> {
    // Registration keys are hex-encoded over their significant bytes — the
    // field is NUL-padded and the console wants only the text before the pad.
    let end = regist_key.iter().position(|b| *b == 0).unwrap_or(KEY_SIZE);
    let regist_key_hex: String = regist_key[..end]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let addr = resolve(host, SESSION_PORT)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| anyhow!("connecting for session init: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let req = format!(
        "GET /sie/ps5/rp/sess/init HTTP/1.1\r\n\
         Host: {host}:{SESSION_PORT}\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: close\r\n\
         Content-Length: 0\r\n\
         RP-Registkey: {regist_key_hex}\r\n\
         Rp-Version: {RP_VERSION}\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| anyhow!("sending session init: {e}"))?;
    stream.flush().ok();
    let _ = stream.shutdown(Shutdown::Write);

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| anyhow!("reading session init reply: {e}"))?;
    let reply = split_http(&raw)?;
    if reply.code != 200 {
        let reason = reply
            .headers
            .iter()
            .find(|(k, _)| k == "rp-application-reason")
            .map(|(_, v)| v.as_str())
            .unwrap_or("no reason given");
        bail!("session init refused (HTTP {}): {reason}", reply.code);
    }
    let nonce_b64 = reply
        .headers
        .iter()
        .find(|(k, _)| k == "rp-nonce")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| anyhow!("session init reply had no RP-Nonce"))?;
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(nonce_b64.trim())
        .map_err(|e| anyhow!("RP-Nonce not base64: {e}"))?;
    if nonce.len() != KEY_SIZE {
        bail!("RP-Nonce was {} bytes, expected {KEY_SIZE}", nonce.len());
    }
    let mut out = [0u8; KEY_SIZE];
    out.copy_from_slice(&nonce);
    Ok(out)
}

/// Stage 2: the encrypted ctrl connection. Establishing it is what signs the
/// registered user in on the console.
fn ctrl_connect(host: &str, creds: &SessionCreds, nonce: &[u8; KEY_SIZE]) -> Result<()> {
    let amb = ambassador(nonce);
    let brt = bright(nonce, &creds.morning);

    // Each field consumes the next counter, in this order.
    let mut counter = 0u64;
    let mut next = |data: &[u8]| {
        let enc = auth_encrypt(&brt, &amb, counter, data);
        counter += 1;
        b64(&enc)
    };

    let auth_b64 = next(&creds.regist_key);
    let did = make_did();
    let did_b64 = next(&did);
    // OSType is a NUL-terminated string; the terminator is part of the input.
    let ostype = b"Win10.0.0\0";
    let ostype_b64 = next(ostype);
    // PS5 carries a start-bitrate (all zero = "let the console decide") and a
    // streaming type. 2 = H.265, which every PS5 accepts.
    let bitrate_b64 = next(&[0u8; 4]);
    let streaming_type_b64 = next(&2u32.to_le_bytes());

    let addr = resolve(host, SESSION_PORT)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| anyhow!("connecting for ctrl: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let req = format!(
        "GET /sie/ps5/rp/sess/ctrl HTTP/1.1\r\n\
         Host: {host}:{SESSION_PORT}\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: keep-alive\r\n\
         Content-Length: 0\r\n\
         RP-Auth: {auth_b64}\r\n\
         RP-Version: {RP_VERSION}\r\n\
         RP-Did: {did_b64}\r\n\
         RP-ControllerType: 3\r\n\
         RP-ClientType: 11\r\n\
         RP-OSType: {ostype_b64}\r\n\
         RP-ConPath: 1\r\n\
         RP-StartBitrate: {bitrate_b64}\r\n\
         RP-StreamingType: {streaming_type_b64}\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| anyhow!("sending ctrl request: {e}"))?;
    stream.flush().ok();

    // The console answers the ctrl GET with an HTTP status before the
    // connection turns into the binary ctrl protocol. 200 means it accepted
    // the auth and the session is up — which is the moment it logs the user
    // in. We only need that far, so read just the header and close.
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) => return Err(anyhow!("reading ctrl reply: {e}")),
        }
    }
    if raw.is_empty() {
        bail!("the console accepted the ctrl connection but sent no response");
    }
    let reply = split_http(&raw)?;
    if reply.code != 200 {
        let reason = reply
            .headers
            .iter()
            .find(|(k, _)| k == "rp-application-reason")
            .map(|(_, v)| v.as_str())
            .unwrap_or("no reason given");
        bail!(
            "the console refused the control session (HTTP {}): {reason}. \
             A wrong RP-Key is the usual cause.",
            reply.code
        );
    }

    // The HTTP 200 already means the auth was accepted and the session is
    // up. Read a little of the ctrl stream that follows to classify the
    // outcome precisely: the 8-byte message headers (size + type) are in the
    // clear, so a LOGIN_PIN_REQ (0x4) — meaning the account has a login
    // passcode and the console is waiting for it, which we cannot supply —
    // is distinguishable from an ordinary established session.
    //
    // The body may already hold bytes read alongside the header; keep
    // reading briefly for more.
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(raw.len());
    let mut stream_bytes = raw[header_end..].to_vec();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 2048];
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    while std::time::Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stream_bytes.extend_from_slice(&buf[..n]),
            Err(_) => break, // timeout — the console has gone quiet, which is fine
        }
    }

    if ctrl_stream_needs_login_pin(&stream_bytes) {
        bail!(
            "the console signed in but the account has a login passcode, so it \
             is waiting for a PIN we cannot provide. Remove the passcode \
             (Settings › Users and Accounts › Login Settings) to wake straight \
             to this user."
        );
    }
    Ok(())
}

/// Scan the plaintext ctrl message headers for a LOGIN_PIN_REQ (0x4).
///
/// Each ctrl message is `[u32 big-endian payload size][u16 big-endian type]
/// [2 bytes][payload]`. Only the type is needed, and it is not encrypted.
fn ctrl_stream_needs_login_pin(body: &[u8]) -> bool {
    const CTRL_LOGIN_PIN_REQ: u16 = 0x4;
    let mut off = 0;
    while off + 8 <= body.len() {
        let size =
            u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]) as usize;
        let msg_type = u16::from_be_bytes([body[off + 4], body[off + 5]]);
        if msg_type == CTRL_LOGIN_PIN_REQ {
            return true;
        }
        off += 8 + size;
    }
    false
}

/// Wake-then-login: open a control session so the console lands on its user.
///
/// Runs after the wake, against an awake (or waking) console. Needs both the
/// registration key and the RP-Key. Establishing the ctrl connection is the
/// side effect that matters — the user is signed in; we do not stream.
pub fn login_session(host: &str, creds: &SessionCreds) -> Result<()> {
    let nonce = session_init(host, &creds.regist_key)?;
    ctrl_connect(host, creds, &nonce)
}

/// Wait until the console is answering on the session port, then sign in.
///
/// After a network wake the console boots for a while before it accepts a
/// session request; polling `login_session` is how we bridge that. Returns
/// once the sign-in succeeds, or errors if the console never becomes ready
/// or refuses the session within `timeout`.
pub fn login_session_when_ready(host: &str, creds: &SessionCreds, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let err = match login_session(host, creds) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        // Some refusals are final (wrong key, a login passcode) and some are
        // just "not ready yet". A connection-level failure is the console
        // still booting. And `80108b10` (IN_USE) right after a wake is
        // transient too: the wake briefly reserves the Remote Play session,
        // and the console clears it a few seconds later — measured, the manual
        // flow that waited longer never hit it. Retry both; a genuine
        // rejection, or running out of time, stops us and surfaces the reason.
        let msg = format!("{err:#}");
        let transient = msg.contains("connecting")
            || msg.contains("Connection refused")
            || msg.contains("reading")
            || msg.contains("timed out")
            || msg.contains("sent no response")
            || msg.contains("80108b10"); // IN_USE — still settling
        if !transient || std::time::Instant::now() >= deadline {
            return Err(err);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed nonce/morning to pin the crypto so a table or transform change
    // is caught here rather than by a silent console refusal. The values are
    // the algorithm's own output, checked for stability and structure; the
    // real cross-check is a live ctrl connection, which only succeeds if the
    // console agrees byte-for-byte.
    const NONCE: [u8; 16] = [
        0xae, 0x92, 0xe7, 0x64, 0x88, 0x26, 0x51, 0xef, 0x89, 0x01, 0x8c, 0xfa, 0x69, 0x6c, 0x69,
        0x38,
    ];
    const MORNING: [u8; 16] = [
        0x13, 0x95, 0xc8, 0xcc, 0x7e, 0xca, 0x16, 0xfe, 0x98, 0x2e, 0xb2, 0x2e, 0x52, 0x7b, 0xa3,
        0xda,
    ];

    #[test]
    fn ambassador_and_bright_are_deterministic_and_distinct() {
        let a = ambassador(&NONCE);
        let b = bright(&NONCE, &MORNING);
        assert_eq!(a, ambassador(&NONCE));
        assert_eq!(b, bright(&NONCE, &MORNING));
        // They are derived differently; a collision would mean a ported-wrong
        // table or transform.
        assert_ne!(a, b);
    }

    #[test]
    fn bright_depends_on_the_rp_key() {
        let mut other = MORNING;
        other[0] ^= 0xff;
        assert_ne!(bright(&NONCE, &MORNING), bright(&NONCE, &other));
    }

    #[test]
    fn the_table_offset_tracks_the_nonce() {
        // A different top bit of nonce[0] selects a different ambassador
        // table row, so the result must move.
        let mut n2 = NONCE;
        n2[0] ^= 0x80;
        assert_ne!(ambassador(&NONCE), ambassador(&n2));
    }

    #[test]
    fn each_field_encrypts_under_its_own_counter() {
        let amb = ambassador(&NONCE);
        let brt = bright(&NONCE, &MORNING);
        let a = auth_encrypt(&brt, &amb, 0, &[0u8; 16]);
        let b = auth_encrypt(&brt, &amb, 1, &[0u8; 16]);
        assert_eq!(a.len(), 16);
        // Same plaintext, different counter → different ciphertext (different IV).
        assert_ne!(a, b);
    }

    #[test]
    fn the_device_id_has_the_fixed_frame() {
        let did = make_did();
        assert_eq!(
            &did[..10],
            &[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]
        );
        assert_eq!(&did[26..], &[0u8; 6]);
    }

    #[test]
    fn detects_a_login_pin_request_in_the_ctrl_stream() {
        // One ctrl message: size=0, type=0x0004 (LOGIN_PIN_REQ), 2 pad bytes.
        let pin_req = [0u8, 0, 0, 0, 0x00, 0x04, 0, 0];
        assert!(ctrl_stream_needs_login_pin(&pin_req));
        // A different type (0x33 SESSION_ID) is not a pin request.
        let session_id = [0u8, 0, 0, 0, 0x00, 0x33, 0, 0];
        assert!(!ctrl_stream_needs_login_pin(&session_id));
        // Empty / truncated streams do not false-positive.
        assert!(!ctrl_stream_needs_login_pin(&[]));
        assert!(!ctrl_stream_needs_login_pin(&[0, 0, 0]));
    }

    #[test]
    fn creds_reject_mis_sized_hex() {
        assert!(SessionCreds::from_hex("00", "00").is_err());
        assert!(SessionCreds::from_hex(&"a".repeat(32), &"b".repeat(32)).is_ok());
    }
}
