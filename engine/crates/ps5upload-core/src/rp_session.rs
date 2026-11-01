// Portions ported from playactor, Copyright (c) Daniel Leong, under the ISC license
// (LICENSES/playactor-ISC.txt); the rest of this file is part of ps5upload (GPL-3.0).
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
//! **The ordering is the whole trick, and it is not obvious.** A console
//! still in standby answers session-init and hands out a nonce, but resets
//! the ctrl connection because it has not finished booting. Retrying the pair
//! together throws that nonce away — and by the second attempt the console has
//! woken and reserved its Remote Play session for the wake, so session-init
//! returns `0x80108b10` IN_USE and never recovers. We lock ourselves out of a
//! session we were already granted. So: take the nonce ONCE, then retry only
//! the ctrl connection until the console is up. Measured on FW 9.60,
//! 2026-09-12 — init at ddp=620 returns 200 + nonce; 5 s later, awake, the
//! same request is IN_USE and stays that way for as long as you poll it.
//!
//! Verified 5/5 on FW 9.60 (wake from cold rest to signed-in, 10-17 s).
//! FW 5.10 still does NOT sign in this way: it accepts the ctrl connection,
//! stays silent, and closes it after ~30-40 s. That console most likely wants
//! the stream stage a real client sets up next (SESSION_ID -> Senkusha ->
//! Takion), which we do not build. See issue #318.
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
use sha2::{Digest, Sha256};

use crate::rp_login_pin::{
    PasscodePrompt, PinFlow, CTRL_LOGIN, LOGIN_STATE_SUCCESS as CTRL_LOGIN_STATE_SUCCESS,
    PASSCODE_CONSOLE_GAVE_UP, PASSCODE_WAIT,
};
use crate::rp_regist::{generate_iv, resolve, split_http};

/// How a Wake & sign in ended, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// The console signed the user in.
    SignedIn,
    /// The user has a login passcode and nobody was there to type it: the console is awake at
    /// its passcode / user-select screen (automatic wakes, e.g. for an upload).
    PasscodeNeeded,
}
use crate::rp_regist_keys::{PS5_AUTH_NONCE_KEY, PS5_AUTH_SEED_KEY};

type Aes128CfbEnc = cfb_mode::Encryptor<aes::Aes128>;
type Aes128CfbDec = cfb_mode::Decryptor<aes::Aes128>;

/// Session and ctrl share the registration port.
pub(crate) const SESSION_PORT: u16 = 9295;
pub(crate) const KEY_SIZE: usize = 16;
pub(crate) const RP_VERSION: &str = "1.0";

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
pub(crate) fn ambassador(nonce: &[u8; KEY_SIZE]) -> [u8; KEY_SIZE] {
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
pub(crate) fn bright(nonce: &[u8; KEY_SIZE], morning: &[u8; KEY_SIZE]) -> [u8; KEY_SIZE] {
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
pub(crate) fn auth_encrypt(
    bright: &[u8; 16],
    ambassador: &[u8; 16],
    counter: u64,
    data: &[u8],
) -> Vec<u8> {
    let iv = generate_iv(ambassador, counter);
    let mut buf = data.to_vec();
    Aes128CfbEnc::new(bright.into(), &iv.into()).encrypt(&mut buf);
    buf
}

pub(crate) fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A 32-byte device id: a fixed prefix and suffix around random bytes. The
/// console does not check the random part; the shape is what matters.
pub(crate) fn make_did(regist_key: &[u8; KEY_SIZE]) -> [u8; 32] {
    let mut did = [0u8; 32];
    did[..10].copy_from_slice(&[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]);
    // The middle 16 bytes identify THIS client to the console, and they must be
    // stable: a real Remote Play client has one device id for the life of its
    // registration. They used to be freshly random per connection, so every
    // attempt introduced itself as a brand-new device — and a console that has
    // reserved its Remote Play session for the device it woke for then refuses
    // the newcomer as "already in use" (0x80108b10), which is exactly the
    // wake-then-sign-in failure on FW 9.60.
    //
    // Deriving them from the registration key gives one id per console with
    // nothing to persist, and it changes only if the console is re-registered.
    let digest = Sha256::digest(regist_key);
    did[10..26].copy_from_slice(&digest[..16]);
    // did[26..32] stays zero — the suffix.
    did
}

/// Stage 1: the unencrypted session request. Returns the 16-byte nonce.
pub(crate) fn session_init(host: &str, regist_key: &[u8; KEY_SIZE]) -> Result<[u8; KEY_SIZE]> {
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
fn ctrl_connect(
    host: &str,
    creds: &SessionCreds,
    nonce: &[u8; KEY_SIZE],
    hold: Duration,
    prompt: Option<&mut dyn PasscodePrompt>,
) -> Result<SignIn> {
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
    let did = make_did(&creds.regist_key);
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
    // the auth; the sign-in itself is confirmed over the ctrl stream below.
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

    // The HTTP 200 means the auth was accepted, but the console only *keeps*
    // the user signed in while the control session is live: it sends periodic
    // heartbeats and tears the session down — reverting to user-select — if
    // the client stops answering them. So the connection has to be held and
    // its heartbeats answered for long enough for the sign-in to commit.
    //
    // Everything needed is in the clear: each ctrl message is `[u32 size][u16
    // type][2 bytes][payload]` with only the type read here, and a heartbeat
    // reply is an empty message — just its 8-byte header, no encryption. So
    // the session is maintained without the streaming crypto layer.
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(raw.len());
    // Our counter after the five request fields: a passcode reply goes out on it.
    let mut counter_local = counter;
    let result = hold_control_session(
        &mut stream,
        raw[header_end..].to_vec(),
        Keys {
            bright: &brt,
            ambassador: &amb,
            counter_local: &mut counter_local,
        },
        hold,
        prompt,
        PASSCODE_WAIT,
    );
    // Close the control session cleanly. Dropping the socket closes it too, but
    // an explicit graceful shutdown gives the console an unambiguous "the client
    // is leaving" FIN instead of a half-open session it must time out — a
    // candidate mitigation for a console that otherwise stays in a headless
    // Remote-Play state after sign-in (#318). Harmless on the paths that already
    // work: a well-behaved client closes this way regardless.
    let _ = stream.shutdown(Shutdown::Both);
    result
}

/// The console's "this session is properly established" marker.
///
/// We used to ignore it, and that was the bug: the old design assumed simply
/// opening the ctrl connection signed the user in, so it held for a fixed 12 s
/// and hoped. Measured on hardware (FW 5.10): ctrl established, keys accepted,
/// no LOGIN ever arrived, nobody signed in. A real client treats SESSION_ID as
/// the point the session is READY (cf. pyremoteplay, GPL-3.0) and only then
/// moves on to the stream. Waiting for this signal instead of a timer is the
/// difference between "we connected" and "the console accepted the session".
const CTRL_SESSION_ID: u16 = 0x0033;
const CTRL_HEARTBEAT_REQ: u16 = 0x00fe;
const CTRL_HEARTBEAT_REP: u16 = 0x01fe;

/// Decrypt a received ctrl payload. The console encrypts with the same key
/// and IV scheme as our request fields, on its own counter that advances once
/// per payload-bearing message it sends, starting at zero.
pub(crate) fn auth_decrypt(
    bright: &[u8; 16],
    ambassador: &[u8; 16],
    counter: u64,
    data: &[u8],
) -> Vec<u8> {
    let iv = generate_iv(ambassador, counter);
    let mut buf = data.to_vec();
    Aes128CfbDec::new(bright.into(), &iv.into()).decrypt(&mut buf);
    buf
}

/// The ctrl cipher, and our side's counter (a passcode reply is encrypted on it).
struct Keys<'a> {
    bright: &'a [u8; 16],
    ambassador: &'a [u8; 16],
    counter_local: &'a mut u64,
}

/// Keep the control session alive for `hold`, answering heartbeats, and
/// confirm the sign-in.
///
/// The console holds the user signed in only while this session is live: it
/// heartbeats, and reverts to user-select if the client stops replying. So
/// the connection is held and its heartbeats answered long enough for the
/// sign-in to commit and stick after we disconnect — a video stream is not
/// required.
///
/// The console also announces the outcome: a `LOGIN` message whose one-byte
/// (encrypted) payload is the login state. Decrypting it turns "we held the
/// session" into "the console reported the user signed in". A `LOGIN_PIN_REQ`
/// means the account has a login passcode: with a `prompt` the user is asked
/// for it (heartbeats answered meanwhile, re-asked after a wrong one, see
/// rp_login_pin); without one the console is left awake at that screen.
fn hold_control_session(
    stream: &mut TcpStream,
    initial: Vec<u8>,
    keys: Keys<'_>,
    hold: Duration,
    mut prompt: Option<&mut dyn PasscodePrompt>,
    passcode_wait: Duration,
) -> Result<SignIn> {
    let Keys {
        bright,
        ambassador,
        counter_local,
    } = keys;
    let mut buf = initial;
    let mut tmp = [0u8; 2048];
    // The console's send counter starts at 1, not 0: it advances once over the
    // ctrl auth exchange before sending its first message. Verified on
    // hardware — decrypting the LOGIN message at 0 gives garbage, at 1 gives
    // the real state byte (0 = signed in).
    let mut remote_counter = 1u64;
    let mut deadline = std::time::Instant::now() + hold;
    // Once the console reports success, hold a short grace answering heartbeats
    // so the sign-in commits and sticks after we disconnect, then stop — no
    // need to wait out the full timeout.
    let grace_after_login = Duration::from_secs(3);
    let mut login_at: Option<std::time::Instant> = None;
    // Either signal means the console took the session: LOGIN is it announcing
    // a sign-in transition, SESSION_ID is it declaring the session live. A
    // console that woke already signed in has no transition to announce, so
    // LOGIN alone is not something we can wait for.
    let mut established_at: Option<std::time::Instant> = None;
    let mut flow = PinFlow::new(passcode_wait);
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok();

    loop {
        // Drain every complete message currently buffered.
        while buf.len() >= 8 {
            let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            // Ctrl control messages are tiny; a huge length means the stream
            // has desynced (usually a decryption/counter mismatch), so stop
            // rather than wait forever for bytes that will not come.
            if size > 8192 {
                bail!("control stream desynced (implausible message size {size})");
            }
            if buf.len() < 8 + size {
                break; // partial message — wait for the rest
            }
            let msg_type = u16::from_be_bytes([buf[4], buf[5]]);

            // Decrypt the payload if there is one, keeping the console's
            // counter in step so later messages decrypt correctly.
            let payload = if size > 0 {
                let p = auth_decrypt(bright, ambassador, remote_counter, &buf[8..8 + size]);
                remote_counter += 1;
                p
            } else {
                Vec::new()
            };
            buf.drain(0..8 + size);

            // The passcode exchange, when someone can type it.
            if let Some(p) = prompt.as_deref_mut() {
                if flow.on_message(msg_type, &payload, p)? {
                    // Asked (again): any earlier LOGIN success was not for this passcode.
                    login_at = None;
                    continue;
                }
            } else if msg_type == crate::rp_login_pin::CTRL_LOGIN_PIN_REQ {
                // Nobody to ask (an automatic wake): the console is up, at the passcode screen.
                return Ok(SignIn::PasscodeNeeded);
            }

            match msg_type {
                CTRL_LOGIN => {
                    if payload.first() == Some(&CTRL_LOGIN_STATE_SUCCESS) {
                        login_at.get_or_insert_with(std::time::Instant::now);
                    } else {
                        bail!(
                            "the console rejected the sign-in (login state {:?})",
                            payload.first()
                        );
                    }
                }
                CTRL_SESSION_ID => {
                    established_at.get_or_insert_with(std::time::Instant::now);
                }
                CTRL_HEARTBEAT_REQ => {
                    // Reply keeps the session (and the sign-in) alive. Empty
                    // body, so just the header — no encryption, no counter.
                    let mut hdr = [0u8; 8];
                    hdr[4..6].copy_from_slice(&CTRL_HEARTBEAT_REP.to_be_bytes());
                    stream
                        .write_all(&hdr)
                        .map_err(|e| anyhow!("answering a heartbeat: {e}"))?;
                    stream.flush().ok();
                }
                _ => {}
            }
        }

        // Done as soon as the sign-in is confirmed and has been held through
        // its grace period; otherwise keep going until the overall deadline.
        // Once the console asked for a passcode, only its LOGIN success says the user
        // signed in: a SESSION_ID before the ask says nothing about the passcode, and
        // nothing is confirmed while the user is still typing.
        let confirmed = if flow.involved() {
            login_at
        } else {
            login_at.or(established_at)
        };
        if let Some(at) = confirmed.filter(|_| !flow.asking()) {
            if at.elapsed() >= grace_after_login {
                return Ok(SignIn::SignedIn);
            }
        }
        if flow.asking() {
            // The user is typing: the console waits, and so do we (up to the passcode wait).
            if let Some(p) = prompt.as_deref_mut() {
                if flow.poll(stream, bright, ambassador, counter_local, p)? {
                    // A fresh wait for the console's verdict.
                    deadline = std::time::Instant::now() + hold;
                }
            }
        } else if std::time::Instant::now() >= deadline {
            // No LOGIN arrived. We genuinely do not know whether the user was
            // signed in, so say exactly that — an earlier revision of this
            // returned Ok() here, which reported "signed in" for consoles that
            // were still sitting on user-select. A claim we cannot back is
            // worse than an honest "could not confirm".
            //
            // Not a wrong RP-Key, whatever the old message said: a bad key is
            // refused at ctrl connect with a 403, long before we hold the
            // session. Reaching here means the keys authenticated.
            bail!(
                "the console accepted the control session (so the keys are right) but \
                 never declared it live — no SESSION_ID and no LOGIN arrived in {}s, so \
                 the sign-in did not happen.",
                hold.as_secs()
            );
        }
        match stream.read(&mut tmp) {
            // Once a passcode is involved, a retry would only ask again: final.
            Ok(0) if flow.involved() => bail!(PASSCODE_CONSOLE_GAVE_UP),
            Ok(0) => bail!("the console closed the control session before sign-in settled"),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Quiet gap between heartbeats — keep holding until the deadline.
            }
            Err(_) if flow.involved() => bail!(PASSCODE_CONSOLE_GAVE_UP),
            Err(e) => return Err(anyhow!("reading the control session: {e}")),
        }
    }
}

/// Wake-then-login: open a control session so the console lands on its user.
///
/// Runs after the wake, against an awake (or waking) console. Needs both the
/// registration key and the RP-Key. Establishing the ctrl connection is the
/// side effect that matters — the user is signed in; we do not stream.
/// How long to hold the control session so the sign-in commits and sticks
/// after we disconnect. Long enough to answer a couple of heartbeats.
/// Upper bound on waiting for the console to declare the session live. We
/// return as soon as SESSION_ID (or LOGIN) lands plus its grace, so this only
/// bounds the failure case.
const SIGN_IN_HOLD_DEFAULT_S: u64 = 30;

/// How long to wait for the console to declare the session live. Overridable
/// only so a console that behaves differently can be characterised without a
/// rebuild; the default is what ships.
fn sign_in_hold() -> Duration {
    Duration::from_secs(
        std::env::var("PS5UPLOAD_SIGNIN_HOLD_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SIGN_IN_HOLD_DEFAULT_S),
    )
}

/// Wait until the console is answering on the session port, then sign in.
///
/// After a network wake the console boots for a while before it accepts a
/// session request; polling `login_session` is how we bridge that. Returns
/// once the sign-in succeeds, or errors if the console never becomes ready
/// or refuses the session within `timeout`.
/// Retry pacing for a waking console: a steady 2 s, deliberately NOT backing
/// off. A console accepts session-init only during a narrow window while it
/// boots; measured 2026-09-12, polling every 2 s caught it (sign-in in 12 s)
/// while an exponential backoff (2→4→8→16→20 s) stepped straight over it and
/// failed the whole 90 s budget every time. Gentler is not better here.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(2);

/// Is this refusal "the console is not ready yet" rather than "this will never
/// work"?
///
/// A waking console goes through several distinct not-ready states before it
/// will sign anyone in, and each looks different:
///   * connection-level errors — the network stack is not up yet;
///   * `80108b10` (IN_USE) — the wake briefly reserves the Remote Play
///     session and the console releases it a few seconds later;
///   * `80108bff` — session-init succeeds and hands out a nonce, but the ctrl
///     connection is refused while the console finishes booting. Measured on
///     hardware: the same console with the same keys signed in normally once
///     it had settled. This one was missing, so the first refusal after a wake
///     ended the attempt and told the user their RP-Key was wrong.
///
/// A rejected sign-in, or anything after the passcode was asked for (cancelled, timed out,
/// too many wrong ones), is final: retrying cannot help, and pretending otherwise just delays a
/// real answer (and would ask again).
fn is_transient_session_error(msg: &str) -> bool {
    msg.contains("connecting")
        || msg.contains("Connection refused")
        || msg.contains("reading")
        || msg.contains("timed out")
        || msg.contains("sent no response")
        || msg.contains("80108b10")
        || msg.contains("80108bff")
        // A console that has not finished booting resets the ctrl connection
        // rather than answering it. Measured: attempt #1 against a standby
        // console returns this in under a second.
        || msg.contains("Connection reset")
        || msg.contains("sent no response")
}

/// `prompt`: who types the user's login passcode if the console asks for one; None (an
/// automatic wake) leaves the console awake at that screen and returns `PasscodeNeeded`.
pub fn login_session_when_ready(
    host: &str,
    creds: &SessionCreds,
    timeout: Duration,
    mut prompt: Option<&mut dyn PasscodePrompt>,
) -> Result<SignIn> {
    let deadline = std::time::Instant::now() + timeout;
    let mut backoff = RETRY_FIRST;

    // Get the nonce ONCE and keep it.
    //
    // A console still in standby answers session-init and hands out a nonce,
    // but resets the ctrl connection because it is not up yet. The old loop
    // retried `login_session` — init AND ctrl — so it threw that nonce away and
    // asked for another, by which time the console had woken and reserved its
    // Remote Play session for the wake: every retry then got 0x80108b10
    // IN_USE. We locked ourselves out of a session we had already been granted.
    //
    // Measured 2026-09-12 on FW 9.60: init at ddp=620 returns 200 + nonce;
    // 5 s later, awake, the same request is IN_USE and stays that way.
    // So: one init, then retry only the ctrl connection until the console has
    // finished booting.
    let mut nonce = None;
    loop {
        let err = match nonce {
            None => match session_init(host, &creds.regist_key) {
                Ok(n) => {
                    nonce = Some(n);
                    continue;
                }
                Err(e) => e,
            },
            Some(n) => match ctrl_connect(
                host,
                creds,
                &n,
                sign_in_hold(),
                prompt.as_mut().map(|p| &mut **p as &mut dyn PasscodePrompt),
            ) {
                Ok(r) => return Ok(r),
                Err(e) => e,
            },
        };
        let msg = format!("{err:#}");
        if !is_transient_session_error(&msg) || std::time::Instant::now() >= deadline {
            // A console whose Remote Play stayed busy for the whole budget is
            // not going to free it on its own. Measured on hardware: once it
            // wedges, two consecutive full budgets both failed and only a
            // console restart cleared it, after which sign-in took 5 s. Say
            // that, instead of repeating a code the user cannot act on.
            if msg.contains("80108b10") {
                // Measured, not guessed (2026-09-12, FW 9.60): once the
                // console reaches this state it does NOT recover on its own —
                // it was still refusing after 10 minutes of polling. A restart
                // released it immediately and the next sign-in took 5 s. So
                // say "restart", not "wait a bit".
                bail!(
                    "the console reserved its Remote Play session for the wake and has not \
                     released it (waited {}s), so it will not sign anyone in. Use \
                     \"Cancel Remote Play\" on this console, or put it back into rest and \
                     wake it again. See issue #318.",
                    timeout.as_secs()
                );
            }
            return Err(err);
        }
        // Back off rather than hammer. A fixed 2 s gap meant ~45 session-init
        // requests per attempt at a console that was already refusing them,
        // which is exactly the pressure that leaves Remote Play wedged.
        std::thread::sleep(backoff.min(RETRY_MAX));
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rp_login_pin::test_support::{console_msg, read_pin, Typist, AMB, BRT};
    use crate::rp_login_pin::{
        Passcode, CTRL_LOGIN_PIN_REQ, LOGIN_STATE_PIN_INCORRECT, PASSCODE_CANCELLED,
        PASSCODE_TIMEOUT, PASSCODE_TOO_MANY,
    };

    /// Hold a sign-in session against a fake console thread. Returns the outcome and our ctrl
    /// counter after (5 = nothing sent).
    fn sign_in_against(
        user: Option<&mut Typist>,
        passcode_wait: Duration,
        console: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (Result<SignIn>, u64) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            console(c);
        });
        let mut s = TcpStream::connect(addr).unwrap();
        let mut ours = 5u64;
        let r = hold_control_session(
            &mut s,
            Vec::new(),
            Keys {
                bright: &BRT,
                ambassador: &AMB,
                counter_local: &mut ours,
            },
            Duration::from_secs(5),
            user.map(|u| u as &mut dyn PasscodePrompt),
            passcode_wait,
        );
        drop(s);
        t.join().unwrap();
        (r, ours)
    }

    /// The console's side after a passcode it took: signed in, then live.
    fn accept(c: &mut TcpStream, n: &mut u64) {
        c.write_all(&console_msg(CTRL_LOGIN, &[0], n)).unwrap();
        c.write_all(&console_msg(
            CTRL_SESSION_ID,
            &[4, b'a', b'b', b'c', b'd'],
            n,
        ))
        .unwrap();
        // Held through the grace: the engine closes ctrl itself.
        let mut b = [0u8; 64];
        while c.read(&mut b).map(|n| n > 0).unwrap_or(false) {}
    }

    #[test]
    fn wake_sign_in_asks_for_the_passcode_and_signs_in() {
        let mut user = Typist::new(vec![None, None, Some(Passcode::Digits("2468".into()))]);
        let (r, ours) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            // Heartbeats are answered while the user types.
            c.write_all(&console_msg(CTRL_HEARTBEAT_REQ, &[], &mut n))
                .unwrap();
            assert_eq!(read_pin(&mut c, 5), b"2468");
            accept(&mut c, &mut n);
        });
        assert_eq!(r.unwrap(), SignIn::SignedIn);
        assert_eq!(user.asked, vec![false]);
        assert_eq!(ours, 6);
    }

    #[test]
    fn a_session_id_before_the_passcode_ask_is_not_a_sign_in() {
        // The console declares the session live, then asks for the passcode. The user types
        // for longer than the sign-in grace: we must keep asking, not report "signed in".
        let mut answers: Vec<Option<Passcode>> = (0..16).map(|_| None).collect();
        answers.push(Some(Passcode::Digits("2468".into())));
        let mut user = Typist::new(answers);
        let (r, ours) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(
                CTRL_SESSION_ID,
                &[4, b'a', b'b', b'c', b'd'],
                &mut n,
            ))
            .unwrap();
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            assert_eq!(read_pin(&mut c, 5), b"2468");
            accept(&mut c, &mut n);
        });
        assert_eq!(r.unwrap(), SignIn::SignedIn);
        assert_eq!(ours, 6);
    }

    #[test]
    fn a_sent_passcode_without_login_success_is_not_a_sign_in() {
        let mut user = Typist::new(vec![Some(Passcode::Digits("2468".into()))]);
        let (r, _) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(
                CTRL_SESSION_ID,
                &[4, b'a', b'b', b'c', b'd'],
                &mut n,
            ))
            .unwrap();
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            assert_eq!(read_pin(&mut c, 5), b"2468");
            // No verdict: the console never says LOGIN success.
            let mut b = [0u8; 64];
            while c.read(&mut b).map(|n| n > 0).unwrap_or(false) {}
        });
        assert!(r.is_err(), "reported {r:?} with no LOGIN success");
    }

    #[test]
    fn a_wrong_wake_passcode_asks_again() {
        let mut user = Typist::new(vec![
            Some(Passcode::Digits("0000".into())),
            Some(Passcode::Digits("1357".into())),
        ]);
        let (r, ours) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            assert_eq!(read_pin(&mut c, 5), b"0000");
            let mut both = console_msg(CTRL_LOGIN, &[LOGIN_STATE_PIN_INCORRECT], &mut n);
            both.extend(console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n));
            c.write_all(&both).unwrap();
            assert_eq!(read_pin(&mut c, 6), b"1357");
            accept(&mut c, &mut n);
        });
        assert_eq!(r.unwrap(), SignIn::SignedIn);
        assert_eq!(user.asked, vec![false, true]);
        assert_eq!(ours, 7);
    }

    #[test]
    fn cancelling_the_wake_passcode_sends_nothing() {
        let mut user = Typist::new(vec![Some(Passcode::Cancel)]);
        let (r, ours) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap_or(0), 0);
        });
        assert!(format!("{:#}", r.unwrap_err()).contains(PASSCODE_CANCELLED));
        assert_eq!(ours, 5);
    }

    #[test]
    fn an_unanswered_wake_passcode_times_out() {
        let mut user = Typist::default();
        let (r, _) = sign_in_against(Some(&mut user), Duration::from_millis(300), |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap_or(0), 0);
        });
        assert!(format!("{:#}", r.unwrap_err()).contains(PASSCODE_TIMEOUT));
    }

    #[test]
    fn five_wrong_wake_passcodes_stop_the_asking() {
        let mut user = Typist::new(
            (0..6)
                .map(|_| Some(Passcode::Digits("9999".into())))
                .collect(),
        );
        let (r, ours) = sign_in_against(Some(&mut user), PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            for i in 0..5u64 {
                assert_eq!(read_pin(&mut c, 5 + i), b"9999");
                c.write_all(&console_msg(
                    CTRL_LOGIN,
                    &[LOGIN_STATE_PIN_INCORRECT],
                    &mut n,
                ))
                .unwrap();
            }
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap_or(0), 0);
        });
        assert!(format!("{:#}", r.unwrap_err()).contains(PASSCODE_TOO_MANY));
        assert_eq!(user.asked, vec![false, true, true, true, true]);
        assert_eq!(ours, 10);
    }

    #[test]
    fn an_automatic_wake_stops_at_the_passcode_screen() {
        let (r, ours) = sign_in_against(None, PASSCODE_WAIT, |mut c| {
            let mut n = 1u64;
            c.write_all(&console_msg(CTRL_LOGIN_PIN_REQ, &[], &mut n))
                .unwrap();
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap_or(0), 0);
        });
        assert_eq!(r.unwrap(), SignIn::PasscodeNeeded);
        assert_eq!(ours, 5);
    }

    #[test]
    fn the_device_id_is_stable_per_console() {
        // It used to be random per connection, so every sign-in attempt looked
        // like a different device to the console — the suspected cause of a
        // console refusing the second client with 0x80108b10 after it had
        // reserved its Remote Play session for the wake.
        let a = [0x11u8; KEY_SIZE];
        let b = [0x22u8; KEY_SIZE];
        assert_eq!(make_did(&a), make_did(&a), "same console, same id");
        assert_ne!(make_did(&a), make_did(&b), "different consoles differ");
        // The fixed prefix/suffix the console expects must survive.
        let did = make_did(&a);
        assert_eq!(
            &did[..10],
            &[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]
        );
        assert_eq!(&did[26..], &[0u8; 6]);
    }

    #[test]
    fn the_ctrl_message_types_match_a_real_client() {
        // Pinned against a working GPL-3.0 implementation (pyremoteplay). The
        // one that used to be missing is SESSION_ID: without it we waited on a
        // timer instead of the console's own "session is live" signal, and a
        // console that never sent LOGIN looked like a failure while actually
        // just never being asked properly.
        assert_eq!(CTRL_LOGIN_PIN_REQ, 0x0004);
        assert_eq!(CTRL_LOGIN, 0x0005);
        assert_eq!(CTRL_SESSION_ID, 0x0033);
        assert_eq!(CTRL_HEARTBEAT_REQ, 0x00fe);
        assert_eq!(CTRL_HEARTBEAT_REP, 0x01fe);
    }

    #[test]
    fn a_still_booting_console_is_retried_not_blamed_on_the_key() {
        // Measured on hardware 2026-09-12: after a network wake the console
        // answers session-init but refuses the ctrl connection with 80108bff
        // for the first few seconds. It was not in the retry set, so the very
        // first refusal ended the attempt and told the user their RP-Key was
        // wrong. The same console, same keys, signed in fine moments later.
        assert!(is_transient_session_error(
            "the console refused the control session (HTTP 403): 80108bff"
        ));
        // Already covered, and must stay covered.
        assert!(is_transient_session_error(
            "session init refused (HTTP 403): 80108b10"
        ));
        assert!(is_transient_session_error(
            "connecting for ctrl: Connection refused"
        ));
        // A login passcode is genuinely final — retrying cannot help.
        assert!(!is_transient_session_error(
            "the account has a login passcode, so the console is waiting for it"
        ));
        // So is an outright rejected sign-in.
        assert!(!is_transient_session_error(
            "the console rejected the sign-in (login state Some(2))"
        ));
    }

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
    fn creds_reject_mis_sized_hex() {
        assert!(SessionCreds::from_hex("00", "00").is_err());
        assert!(SessionCreds::from_hex(&"a".repeat(32), &"b".repeat(32)).is_ok());
    }
}
