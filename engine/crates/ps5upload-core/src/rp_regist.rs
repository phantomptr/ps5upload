//! Remote Play registration — how a console hands us a wake credential.
//!
//! Ported from playactor (ISC), cross-checked against chiaki. chiaki is
//! AGPL-3.0 and this project is GPL-3.0, so chiaki was read to confirm
//! behaviour and never copied; the algorithm and key tables here come from
//! the ISC implementation.
//!
//! Registration is the only way to obtain the `user-credential` that a DDP
//! WAKEUP needs (see [`crate::ddp`]). It needs two things: the console's
//! PSN account id, and an 8-digit PIN. Every other client makes a person
//! fetch both by hand — a browser sign-in for the account id, and the
//! console's Settings → System → Remote Play → Link Device screen for the
//! PIN. We run code on the console, so [`crate::remoteplay`] reads the
//! account id out of the registry and asks Sony's own API for a PIN, and
//! neither ever reaches the user.
//!
//! The exchange is one HTTP POST on TCP 9295 whose body is
//! 480 bytes of padding followed by an AES-128-CFB encrypted record. The
//! console derives the decryption key from the PIN and from two offsets it
//! reads out of that padding, so the padding is part of the protocol
//! rather than filler. The reply is encrypted the same way and carries the
//! regist key.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use aes::cipher::KeyIvInit;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::rp_regist_keys::{PS5_AERO_KEY, PS5_INIT_KEY};

type Aes128CfbEnc = cfb_mode::Encryptor<aes::Aes128>;
type Aes128CfbDec = cfb_mode::Decryptor<aes::Aes128>;

/// Registration listens here — not on the discovery port.
pub const REGIST_PORT: u16 = 9295;

/// The PS5 registration path. PS4 uses `/sie/ps4/...` or, before system
/// software 8.0, `/sce/rp/regist`; we only ever talk to a PS5.
const REGIST_PATH: &str = "/sie/ps5/rp/sess/rgst";

/// Remote Play protocol version for PS5. Sent as an `RP-Version` header;
/// a mismatch is rejected with reason `80108b11`.
const RP_VERSION: &str = "1.0";

/// Identifies us as the Windows Remote Play client. The console checks it.
const CLIENT_TYPE: &str = "dabfa2ec873de5839bee8d3f4c0239c4282c07c25c6077a2931afcf0adc0d34f";

/// HMAC key the IV is derived with, PS5 flavour.
const HMAC_KEY_PS5: [u8; 16] = [
    0x46, 0x46, 0x87, 0xb3, 0x49, 0xca, 0x8c, 0xe8, 0x59, 0xc5, 0x27, 0x0f, 0x5d, 0x7a, 0x69, 0xd6,
];

const NONCE_LEN: usize = 16;
const PADDING_BYTES: usize = 480;

/// Everything the console tells us when registration succeeds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    /// Hex, as sent. [`credential_from_regist_key`] turns it into the
    /// number a WAKEUP carries.
    pub regist_key: String,
    /// Streaming session key. We do not stream, but it is stored so a
    /// future session feature does not need to re-pair.
    pub rp_key: String,
    pub mac: String,
    pub nickname: String,
}

/// The `user-credential` a WAKEUP carries, derived from a regist key.
///
/// The conversion is genuinely strange, and both reference implementations
/// do exactly this: read the hex into bytes, read *those bytes* as ASCII,
/// then parse that text as a hexadecimal number. playactor's author left a
/// "this is so bizarre, but here it is" next to it.
/// NUL-pad a key the console reported as bare text out to the 16-byte form.
///
/// `PS5-RegistKey` arrives as the ASCII characters only (e.g. 16 hex chars for
/// 8 bytes), but a session key is always 16 bytes; `rp_session` truncates at
/// the first NUL when it needs the text back. Anything already 32 chars (or
/// longer, or not hex) is returned unchanged so this can never corrupt a value
/// that was already in the right shape.
fn pad_session_key_hex(hex: &str) -> String {
    const WANT: usize = 32; // 16 bytes
    let t = hex.trim();
    if t.len() >= WANT || t.is_empty() || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return t.to_string();
    }
    let mut out = String::with_capacity(WANT);
    out.push_str(t);
    while out.len() < WANT {
        out.push('0');
    }
    out
}

pub fn credential_from_regist_key(regist_key_hex: &str) -> Result<u64> {
    let bytes = parse_hex(regist_key_hex)
        .ok_or_else(|| anyhow!("regist key is not hex: {regist_key_hex:?}"))?;
    // The console pads the field with NULs; the meaningful part is the
    // text before the first one.
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = std::str::from_utf8(&bytes[..end])
        .with_context(|| "regist key bytes are not text".to_string())?
        .trim();
    if text.is_empty() {
        bail!("regist key decoded to an empty string");
    }
    u64::from_str_radix(text, 16)
        .with_context(|| format!("regist key {text:?} is not a hex number"))
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// The AES key for this exchange, mixed from a fixed table and the PIN.
///
/// `init_key_off` is not ours to choose: the console reads it back out of
/// the padding we send, so both sides land on the same key only if the
/// padding is transmitted unchanged.
pub fn generate_seed(pin: u32, init_key_off: usize) -> [u8; 16] {
    let mut seed = [0u8; 16];
    for (i, out) in seed.iter_mut().enumerate() {
        *out = PS5_INIT_KEY[(i * 0x20 + init_key_off) % PS5_INIT_KEY.len()];
    }
    // The PIN only ever touches the last four bytes.
    seed[0xc] ^= ((pin >> 0x18) & 0xFF) as u8;
    seed[0xd] ^= ((pin >> 0x10) & 0xFF) as u8;
    seed[0xe] ^= ((pin >> 0x08) & 0xFF) as u8;
    seed[0xf] ^= (pin & 0xFF) as u8;
    seed
}

/// Obfuscate the nonce so it can travel inside the padding.
///
/// The console runs the inverse to recover the nonce, which is how it can
/// derive the same IV without us ever sending it in the clear.
fn generate_aeropause(nonce: &[u8; NONCE_LEN], padding: &[u8]) -> [u8; NONCE_LEN] {
    let aero_key_off = (padding[0] >> 3) as usize;
    // -0x2d as a byte. PS4 uses 0x29 here.
    let wurzelbert: u8 = 0xd3;

    let mut aeropause = [0u8; NONCE_LEN];
    for (i, out) in aeropause.iter_mut().enumerate() {
        let k = PS5_AERO_KEY[i * 0x20 + aero_key_off];
        *out = (nonce[i] ^ k)
            .wrapping_add(wurzelbert)
            .wrapping_add(i as u8);
    }
    aeropause
}

/// IV for the record cipher: HMAC-SHA256 over the nonce and a counter,
/// truncated to the block size. Registration always uses counter 0.
pub fn generate_iv(nonce: &[u8; NONCE_LEN], counter: u64) -> [u8; 16] {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&HMAC_KEY_PS5)
        .expect("HMAC accepts any key length");
    mac.update(nonce);
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&digest[..16]);
    iv
}

/// The 480-byte preface, with the nonce woven into it.
///
/// Filled with `'A'`: the two offsets the console reads (`padding[0]` and
/// `padding[0x18D]`) then have known values, and neither is overwritten by
/// the two aeropause halves that land at 0xc7 and 0x191.
fn build_preface(nonce: &[u8; NONCE_LEN]) -> ([u8; PADDING_BYTES], usize) {
    let mut padding = [b'A'; PADDING_BYTES];
    let init_key_off = (padding[0x18D] & 0x1F) as usize;

    let aeropause = generate_aeropause(nonce, &padding);
    // Deliberately swapped: the second half goes first. The console
    // reassembles them in this order.
    padding[0xc7..0xc7 + 8].copy_from_slice(&aeropause[8..16]);
    padding[0x191..0x191 + 8].copy_from_slice(&aeropause[0..8]);

    (padding, init_key_off)
}

/// A built request, with the key material the reply must be read back with.
pub struct RegistRequest {
    pub body: Vec<u8>,
    /// Kept because the console encrypts its reply with the same pair.
    pub seed: [u8; 16],
    pub iv: [u8; 16],
}

/// Build the request body for a PIN and account id.
///
/// Split out from the network so it can be tested against the reference
/// implementation's vectors.
pub fn build_request_body(
    account_id_b64: &str,
    pin: u32,
    nonce: &[u8; NONCE_LEN],
) -> RegistRequest {
    let (padding, init_key_off) = build_preface(nonce);
    let seed = generate_seed(pin, init_key_off);
    let iv = generate_iv(nonce, 0);

    let record = format!("Client-Type: {CLIENT_TYPE}\r\nNp-AccountId: {account_id_b64}\r\n");
    let mut encrypted = record.into_bytes();
    Aes128CfbEnc::new(&seed.into(), &iv.into()).encrypt(&mut encrypted);

    let mut body = Vec::with_capacity(PADDING_BYTES + encrypted.len());
    body.extend_from_slice(&padding);
    body.extend_from_slice(&encrypted);
    RegistRequest { body, seed, iv }
}

/// Announce ourselves on 9295 before registering.
///
/// Not optional: without it the console rejects the registration. It is a
/// bare `SRC3` datagram answered with `RES3`. A failure to bind 9295
/// locally is tolerated — some hosts will not allow it, and the console
/// still answers a request from an ephemeral port.
fn regist_search(host: &str, timeout: Duration) -> Result<()> {
    let addr = resolve(host, REGIST_PORT)?;
    let socket = UdpSocket::bind(("0.0.0.0", REGIST_PORT))
        .or_else(|_| UdpSocket::bind("0.0.0.0:0"))
        .map_err(|e| anyhow!("binding a socket for the registration search: {e}"))?;
    socket.set_read_timeout(Some(timeout))?;
    socket
        .send_to(b"SRC3", addr)
        .map_err(|e| anyhow!("sending SRC3 to {addr}: {e}"))?;

    let mut buf = [0u8; 1024];
    let (n, _) = socket.recv_from(&mut buf).map_err(|e| {
        anyhow!(
            "the console did not answer the registration search on {addr}: {e}. \
             It must be awake, with Remote Play enabled."
        )
    })?;
    if !buf[..n].starts_with(b"RES3") {
        bail!(
            "unexpected answer to the registration search: {:?}",
            String::from_utf8_lossy(&buf[..n.min(32)])
        );
    }
    Ok(())
}

pub(crate) fn resolve(host: &str, port: u16) -> Result<std::net::SocketAddr> {
    let bare = host.split(':').next().unwrap_or(host);
    format!("{bare}:{port}")
        .to_socket_addrs()
        .map_err(|e| anyhow!("resolving {bare}: {e}"))?
        .next()
        .ok_or_else(|| anyhow!("{bare} resolved to nothing"))
}

/// A raw HTTP reply, taken apart.
pub(crate) struct HttpReply {
    pub(crate) code: u16,
    /// Header names lower-cased, so lookups do not have to guess the case
    /// the console used.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

/// Split a raw HTTP response into its status code, headers and body.
pub(crate) fn split_http(raw: &[u8]) -> Result<HttpReply> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("no header/body break in the console's reply"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = raw[split + 4..].to_vec();

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow!("not an HTTP reply: {status_line:?}"))?;

    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Ok(HttpReply {
        code,
        headers,
        body,
    })
}

/// Turn the console's refusal code into something a person can act on.
fn reason_text(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "80108b09" => "the PIN was wrong or has expired",
        // Hit when a pending registration was already finalised on the
        // console — polling Remote Play status probes
        // sceRemoteplayConfirmDeviceRegist, which does exactly that. Retrying
        // from a clean state is the fix, so say so.
        "80108b03" => "a previous pairing attempt is still pending — try again",
        "80108b02" => "the console rejected the PSN account",
        "80108b10" => "Remote Play is already in use",
        "80108b15" => "Remote Play crashed on the console",
        "80108b11" => "the console wants a different Remote Play version",
        _ => "the console refused the registration",
    }
}

/// Register with a console and come back with its regist key.
///
/// `account_id_b64` and `pin` both come from the payload — see
/// [`crate::remoteplay`]. The console must be awake with the PIN still
/// live; PINs expire, so this is not something to retry much later.
pub fn register(host: &str, account_id_b64: &str, pin: u32) -> Result<Registration> {
    if account_id_b64.trim().is_empty() {
        bail!("an account id is required to register");
    }
    // Fail here rather than let the console reject an 8-byte field that is
    // the wrong size, which it reports only as a generic refusal.
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(account_id_b64.trim())
        .map_err(|e| anyhow!("account id is not base64: {e}"))?;
    if decoded.len() != 8 {
        bail!("account id should decode to 8 bytes, got {}", decoded.len());
    }

    regist_search(host, Duration::from_secs(5))?;

    let mut nonce = [0u8; NONCE_LEN];
    getrandom_bytes(&mut nonce)?;
    let req_built = build_request_body(account_id_b64.trim(), pin, &nonce);

    let addr = resolve(host, REGIST_PORT)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| anyhow!("connecting to {addr}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut req = format!(
        "POST {REGIST_PATH} HTTP/1.1\r\n\
         HOST: 10.0.2.15\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\
         RP-Version: {RP_VERSION}\r\n\r\n",
        req_built.body.len()
    )
    .into_bytes();
    req.extend_from_slice(&req_built.body);
    stream
        .write_all(&req)
        .map_err(|e| anyhow!("sending the registration request: {e}"))?;
    stream.flush().ok();
    let _ = stream.shutdown(Shutdown::Write);

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| anyhow!("reading the console's reply: {e}"))?;

    let reply = split_http(&raw)?;
    if reply.code >= 300 {
        // Sony's reason code says what actually went wrong; without it the
        // status alone is nearly useless for diagnosis, so fall back to
        // echoing the headers rather than inventing a cause.
        let reason = match reply
            .headers
            .iter()
            .find(|(k, _)| k == "rp-application-reason")
        {
            Some((_, v)) => format!("{} (reason {v})", reason_text(v)),
            None => format!(
                "the console refused the registration and gave no reason. It answered with: {}",
                reply
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        };
        bail!("registration failed (HTTP {}): {reason}", reply.code);
    }
    if reply.body.is_empty() {
        bail!("the console accepted the request but sent nothing back");
    }

    let mut plain = reply.body;
    Aes128CfbDec::new(&req_built.seed.into(), &req_built.iv.into()).decrypt(&mut plain);
    parse_registration(&plain)
}

/// Read the decrypted reply.
pub fn parse_registration(plain: &[u8]) -> Result<Registration> {
    let text = String::from_utf8_lossy(plain);
    let mut out = Registration::default();
    for line in text.split("\r\n") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "PS5-RegistKey" => out.regist_key = value,
            "RP-Key" => out.rp_key = value,
            "PS5-Mac" => out.mac = value,
            "PS5-Nickname" => out.nickname = value,
            _ => {}
        }
    }
    if out.regist_key.is_empty() {
        // Decryption produces plausible-looking garbage when the PIN is
        // wrong, so say that rather than "missing field".
        bail!(
            "no regist key in the console's reply — usually a wrong or expired PIN \
             (the reply did not decrypt to anything readable)"
        );
    }
    Ok(out)
}

fn getrandom_bytes(buf: &mut [u8]) -> Result<()> {
    use std::io::Read as _;
    let mut f =
        std::fs::File::open("/dev/urandom").map_err(|e| anyhow!("opening /dev/urandom: {e}"))?;
    f.read_exact(buf)
        .map_err(|e| anyhow!("reading /dev/urandom: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keys_are_padded_to_sixteen_bytes() {
        // Hardware (Pro FW 9.60): a real PS5 reports PS5-RegistKey as the
        // ASCII text alone (16 hex chars). The client requires exactly 32 and
        // drops anything else, so an unpadded value silently disabled
        // wake-into-user while appearing to pair successfully.
        assert_eq!(
            pad_session_key_hex("3539363762626433"),
            "35393637626264330000000000000000"
        );
        // An RP-Key is already 16 bytes and must pass through untouched.
        let rp = "1395c8cc7eca16fe982eb22e527ba3da";
        assert_eq!(pad_session_key_hex(rp), rp);
        // Never mangle something that is not a plain hex key.
        assert_eq!(pad_session_key_hex(""), "");
        assert_eq!(pad_session_key_hex("not hex"), "not hex");
        // The padded output is exactly what the session parser accepts.
        assert!(crate::rp_session::SessionCreds::from_hex(
            &pad_session_key_hex("3539363762626433"),
            rp
        )
        .is_ok());
    }

    /// Vector from playactor's own test suite. If this drifts, the port of
    /// the key table or the PIN mixing is wrong.
    #[test]
    fn seed_matches_the_reference_implementation() {
        let seed = generate_seed(78703893, 0x1e);
        assert_eq!(
            seed,
            [
                0xe2, 0x9d, 0x64, 0x4c, 0x14, 0x1b, 0x9d, 0x61, 0x74, 0x31, 0xa5, 0x6d, 0x34, 0xcf,
                0xc1, 0x7f,
            ]
        );
    }

    /// Also from playactor's suite, and the reason the HMAC key is pinned.
    #[test]
    fn iv_matches_the_reference_implementation() {
        let nonce = [
            0x3e, 0x7e, 0x7a, 0x82, 0x59, 0x73, 0xad, 0xab, 0x2f, 0x69, 0x43, 0x46, 0xbd, 0x44,
            0xda, 0xb5,
        ];
        assert_eq!(
            generate_iv(&nonce, 0),
            [
                0x90, 0x44, 0x40, 0x82, 0x73, 0xf8, 0x04, 0x4d, 0xca, 0x76, 0x7b, 0x5a, 0x16, 0x39,
                0x4d, 0x64,
            ]
        );
    }

    #[test]
    fn the_pin_only_perturbs_the_last_four_bytes() {
        let a = generate_seed(0, 1);
        let b = generate_seed(12345678, 1);
        assert_eq!(a[..0xc], b[..0xc]);
        assert_ne!(a[0xc..], b[0xc..]);
    }

    #[test]
    fn credential_derivation_survives_the_round_trip() {
        // "1a2b3c4d" as ASCII, hex-encoded, is what the console sends.
        let hex: String = "1a2b3c4d".bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(credential_from_regist_key(&hex).unwrap(), 0x1a2b3c4d);
    }

    #[test]
    fn credential_derivation_ignores_the_nul_padding() {
        let mut bytes: Vec<u8> = "0badf00d".bytes().collect();
        bytes.resize(16, 0); // the field is NUL-padded on the wire
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(credential_from_regist_key(&hex).unwrap(), 0x0badf00d);
    }

    #[test]
    fn credential_derivation_refuses_rubbish_rather_than_guessing() {
        assert!(credential_from_regist_key("").is_err());
        assert!(credential_from_regist_key("zz").is_err());
        // Valid hex, but the bytes are not hex *text*.
        assert!(credential_from_regist_key("ffffffff").is_err());
    }

    #[test]
    fn the_preface_carries_the_nonce_without_disturbing_the_offsets() {
        let nonce = [7u8; NONCE_LEN];
        let (padding, init_key_off) = build_preface(&nonce);
        // Both offsets are read from bytes the aeropause must not touch.
        assert_eq!(padding[0], b'A');
        assert_eq!(padding[0x18D], b'A');
        assert_eq!(init_key_off, (b'A' & 0x1F) as usize);
        // And the aeropause really did land in the two windows.
        assert_ne!(&padding[0xc7..0xc7 + 8], b"AAAAAAAA");
        assert_ne!(&padding[0x191..0x191 + 8], b"AAAAAAAA");
    }

    #[test]
    fn the_body_is_padding_then_ciphertext() {
        let nonce = [3u8; NONCE_LEN];
        let body = build_request_body("XCDiqZluNXo=", 12345678, &nonce).body;
        assert!(body.len() > PADDING_BYTES);
        assert_eq!(&body[..8], b"AAAAAAAA");
        // The record must not be recognisable in the clear.
        let tail = String::from_utf8_lossy(&body[PADDING_BYTES..]).to_string();
        assert!(!tail.contains("Np-AccountId"));
    }

    #[test]
    fn a_record_decrypts_back_to_itself() {
        // The console uses the same key and IV in both directions, so a
        // round trip here is the same operation the reply goes through.
        let nonce = [9u8; NONCE_LEN];
        let built = build_request_body("XCDiqZluNXo=", 99887766, &nonce);
        let mut plain = built.body[PADDING_BYTES..].to_vec();
        Aes128CfbDec::new(&built.seed.into(), &built.iv.into()).decrypt(&mut plain);
        let text = String::from_utf8_lossy(&plain);
        assert!(text.contains("Np-AccountId: XCDiqZluNXo="));
        assert!(text.contains(CLIENT_TYPE));
    }

    #[test]
    fn reads_a_registration_reply() {
        let reply = "PS5-RegistKey: 3161326233633464\r\nRP-Key: 0011223344556677\r\n\
                     PS5-Mac: 5C843CA8AE72\r\nPS5-Nickname: Living Room PS5\r\n";
        let r = parse_registration(reply.as_bytes()).unwrap();
        assert_eq!(r.regist_key, "3161326233633464");
        assert_eq!(r.mac, "5C843CA8AE72");
        assert_eq!(r.nickname, "Living Room PS5");
        assert_eq!(
            credential_from_regist_key(&r.regist_key).unwrap(),
            0x1a2b3c4d
        );
    }

    #[test]
    fn a_reply_without_a_key_is_an_error_not_an_empty_registration() {
        assert!(parse_registration(b"PS5-Nickname: PS5\r\n").is_err());
        assert!(parse_registration(&[0xff; 64]).is_err());
    }

    #[test]
    fn http_splitting_finds_the_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nRP-Version: 1.0\r\nContent-Length: 3\r\n\r\nabc";
        let r = split_http(raw).unwrap();
        assert_eq!(r.code, 200);
        assert_eq!(r.body, b"abc");
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "rp-version" && v == "1.0"));
    }

    #[test]
    fn the_pending_registration_refusal_is_named() {
        // 80108b03 used to fall through to the generic "the console refused",
        // which reads as unfixable. It is fixable: retry from a clean state.
        assert_eq!(
            reason_text("80108b03"),
            "a previous pairing attempt is still pending — try again"
        );
        assert_eq!(reason_text("80108B03"), reason_text("80108b03"));
    }

    #[test]
    fn refusal_codes_become_something_actionable() {
        assert!(reason_text("80108b09").contains("PIN"));
        assert!(reason_text("80108B09").contains("PIN"));
        assert!(reason_text("deadbeef").contains("refused"));
    }
}

/// What a successful pairing leaves us with.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PairResult {
    /// The number a WAKEUP carries. This is the whole point.
    pub credential: String,
    /// Session credentials returned by the same registration handshake.
    /// Keeping these in the result lets the client configure Wake & sign in
    /// without asking the user to copy secrets out of Chiaki manually.
    ///
    /// Both are the 16-byte form as 32 hex characters — what
    /// [`crate::rp_session::SessionCreds::from_hex`] parses and what the UI
    /// stores. The console reports the regist key as the ASCII text alone
    /// (16 hex chars), so it is NUL-padded here: an unpadded value fails the
    /// client's 32-hex check and the sign-in keys are silently dropped.
    pub regist_key: String,
    pub rp_key: String,
    pub nickname: String,
    pub mac: String,
    /// Which account the console paired with, and how it was chosen —
    /// passed through from readiness so the UI can name it.
    pub account_id_b64: String,
    pub account_via: String,
}

/// Pair with a console end to end, with nothing asked of the user.
///
/// The console supplies both halves itself: [`crate::remoteplay`] reads the
/// PSN account id out of its registry and has Sony's own API mint a PIN, so
/// there is no browser sign-in and no trip to the Link Device screen. The
/// console must be awake with the payload running — a sleeping console
/// cannot do any of this, which is fine, because pairing happens once and
/// waking is what it buys.
///
/// `mgmt_addr` is the payload's management port; `host` is the console's
/// address, which the registration and wake traffic use on their own ports.
pub fn pair_with_console(mgmt_addr: &str, host: &str) -> Result<PairResult> {
    let mut readiness = crate::remoteplay::remoteplay_readiness(mgmt_addr)
        .context("asking the console whether it can pair")?;

    // Check preconditions here rather than let registration fail with a
    // generic refusal — each of these has a different fix.
    if !readiness.registry_ok() {
        bail!(
            "could not read the console's settings (registry error {})",
            readiness.registry_err
        );
    }
    if readiness.symbols_ok == 0 {
        bail!("this firmware does not expose Remote Play to the payload");
    }
    if readiness.account_uid <= 0 || !readiness.activated() {
        bail!(
            "no signed-in user with an activated PSN account — sign in on the console, \
             then try again"
        );
    }
    // Remote Play has to be on before a pairing can happen. We can turn it
    // on, and someone who started setup has already asked for exactly that,
    // so do it instead of sending them into the console's menus and making
    // them come back. Two scopes because FW 10.00 split the system-wide
    // service from per-user permission; a console can have the service on
    // while the account in use is not permitted, which pairs fine and then
    // refuses every session.
    if !readiness.service_on() {
        readiness = crate::remoteplay::remoteplay_enable(mgmt_addr, "service")
            .context("turning Remote Play on")?;
    }
    if readiness.needs_per_user() && !readiness.user_on() {
        readiness = crate::remoteplay::remoteplay_enable(mgmt_addr, "user")
            .context("allowing this user to use Remote Play")?;
    }
    // `remoteplay_enable` re-reads the console rather than reporting a bare
    // ok, so this is the console's own answer, not our assumption.
    if !readiness.service_on() {
        bail!("Remote Play is off and the console would not turn it on");
    }

    let account_id = readiness.account_id_b64.clone();

    // Clear any session left over from an abandoned attempt before asking
    // for a PIN. An outstanding one makes Sony's own Initialize fail with
    // 0x80FC0003 on the next try, and the console stays that way until
    // something resets it — measured on both consoles here, where a second
    // pairing attempt could never succeed without this.
    let _ = crate::remoteplay::remoteplay_cancel(mgmt_addr);

    // The PIN comes back in the ack. It must NOT be fetched by polling
    // status: that probes sceRemoteplayConfirmDeviceRegist, which finalises
    // the pending registration on the console, and the handshake below is
    // then refused (measured: HTTP 403, reason 80108b03).
    let snap = crate::remoteplay::remoteplay_request(mgmt_addr, None)
        .context("asking the console for a pairing PIN")?;

    let pin = snap.pin.trim().to_string();
    if pin.is_empty() {
        bail!("the console did not return a pairing PIN");
    }
    let pin_num: u32 = pin
        .parse()
        .map_err(|_| anyhow!("the console produced a PIN we cannot read: {pin:?}"))?;

    let reg = register(host, &account_id, pin_num)?;
    let credential = credential_from_regist_key(&reg.regist_key)?;

    // Best effort: the PIN has been consumed, so clear the waiting state
    // rather than leave the console counting down.
    let _ = crate::remoteplay::remoteplay_cancel(mgmt_addr);

    Ok(PairResult {
        credential: credential.to_string(),
        regist_key: pad_session_key_hex(&reg.regist_key),
        rp_key: pad_session_key_hex(&reg.rp_key),
        nickname: reg.nickname,
        mac: reg.mac,
        account_id_b64: account_id,
        account_via: readiness.account_via,
    })
}
