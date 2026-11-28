//! Play Room secrets (design: play-rooms 4.3 and 9.3 #6): the room password's hash, and knock
//! secrets sealed to the host engine's room key so the hub carries them without reading them.
//!
//! **Password.** PBKDF2-HMAC-SHA256, 210 000 iterations, a 16-byte random salt, 32 bytes out,
//! compared in constant time. Only the hash is kept (`play_room_secret.json`, mode 0600).
//!
//! **Sealed knock** (one format for the web page's WebCrypto, the apps and the engine):
//! * the host's room key is a P-256 key pair; its public key goes on the room's card and in
//!   `{"knock": "challenge"}` as `pk` = base64url (no padding) of the 65-byte uncompressed SEC1
//!   point;
//! * the engine first sends the guest a fresh 16-byte challenge `c` (base64url);
//! * the guest makes an ephemeral P-256 key pair `E`, takes the ECDH shared secret (the 32-byte
//!   x coordinate) with `pk`, and derives a 16-byte key with HKDF-SHA256 (salt = the challenge's
//!   16 bytes, info = `"ps5upload room knock v1"` followed by E's 65-byte public point);
//! * the plaintext is JSON `{"gid": <the guest's hub id>, "c": <the challenge as sent>, "pw"?:
//!   <password>, "pass"?: <friend pass>}`;
//! * `blob` = base64url( E public (65) ‖ nonce (12, random) ‖ AES-128-GCM(key, nonce, plaintext,
//!   aad = the gid's UTF-8 bytes) ).
//!
//! The engine opens a blob only for the challenge it sent that gid, once: a captured blob can't be
//! replayed to another gid or for another challenge.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine as _;
use hmac::Hmac;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

use crate::pad_link::same_key;

/// Iterations for a room password (fewer in tests: they make many).
pub(crate) const PW_ITERS: u32 = if cfg!(test) { 1000 } else { 210_000 };
/// Characters a room password needs at least, and may have at most.
pub(crate) const PW_MIN: usize = 4;
pub(crate) const PW_MAX: usize = 64;
const INFO: &[u8] = b"ps5upload room knock v1";
/// Largest sealed blob the engine opens (a password and a pass fit in far less).
pub(crate) const BLOB_MAX: usize = 1024;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
}

fn random<const N: usize>() -> Option<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).ok()?;
    Some(b)
}

/// A stored room password: salt, hash and the iterations it was made with.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PwHash {
    pub salt: String,
    pub pbkdf2_sha256: String,
    pub iters: u32,
}

fn derive(pw: &str, salt: &[u8], iters: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(pw.as_bytes(), salt, iters, &mut out)
        .expect("HMAC takes any key length");
    out
}

/// Why a new password is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PwErr {
    /// Fewer than [`PW_MIN`] or more than [`PW_MAX`] characters.
    Short,
    Long,
}

/// The hash to keep for `pw`.
pub(crate) fn hash_password(pw: &str) -> Result<PwHash, PwErr> {
    let n = pw.chars().count();
    if n < PW_MIN {
        return Err(PwErr::Short);
    }
    if n > PW_MAX {
        return Err(PwErr::Long);
    }
    let salt = random::<16>().ok_or(PwErr::Short)?;
    Ok(PwHash {
        salt: b64(&salt),
        pbkdf2_sha256: b64(&derive(pw, &salt, PW_ITERS)),
        iters: PW_ITERS,
    })
}

/// `pw` is the room's password (constant time; a broken stored hash matches nothing).
pub(crate) fn password_ok(h: &PwHash, pw: &str) -> bool {
    if pw.is_empty() || pw.chars().count() > PW_MAX || h.iters == 0 || h.iters > 10_000_000 {
        return false;
    }
    let Some(salt) = unb64(&h.salt) else {
        return false;
    };
    same_key(&b64(&derive(pw, &salt, h.iters)), &h.pbkdf2_sha256)
}

/// The host engine's room key (P-256), kept as its 32-byte scalar (base64url).
#[derive(Clone)]
pub(crate) struct RoomKey(SecretKey);

impl std::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoomKey({})", self.public())
    }
}

impl RoomKey {
    pub(crate) fn generate() -> Option<RoomKey> {
        // A uniformly random scalar: retry the (astronomically rare) out-of-range value.
        for _ in 0..8 {
            let b = random::<32>()?;
            if let Ok(k) = SecretKey::from_slice(&b) {
                return Some(RoomKey(k));
            }
        }
        None
    }

    pub(crate) fn from_b64(s: &str) -> Option<RoomKey> {
        SecretKey::from_slice(&unb64(s)?).ok().map(RoomKey)
    }

    pub(crate) fn to_b64(&self) -> String {
        b64(&self.0.to_bytes())
    }

    /// `pk`: the uncompressed public point, base64url (87 characters).
    pub(crate) fn public(&self) -> String {
        b64(self.0.public_key().to_sec1_point(false).as_bytes())
    }

    /// Opens a guest's sealed knock for `gid` and the `challenge` sent to it (base64url).
    pub(crate) fn open(&self, blob: &str, gid: &str, challenge: &str) -> Option<Sealed> {
        if blob.len() > BLOB_MAX * 2 {
            return None;
        }
        let raw = unb64(blob)?;
        if raw.len() < 65 + 12 + 16 {
            return None;
        }
        let (epk, rest) = raw.split_at(65);
        let (nonce, ct) = rest.split_at(12);
        let their = PublicKey::from_sec1_bytes(epk).ok()?;
        let shared = p256::ecdh::diffie_hellman(self.0.to_nonzero_scalar(), their.as_affine());
        let key = knock_key(shared.raw_secret_bytes().as_slice(), challenge, epk)?;
        let plain = Aes128Gcm::new_from_slice(&key)
            .ok()?
            .decrypt(
                &Nonce::try_from(nonce).ok()?,
                Payload {
                    msg: ct,
                    aad: gid.as_bytes(),
                },
            )
            .ok()?;
        let v: serde_json::Value = serde_json::from_slice(&plain).ok()?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        if s("gid").as_deref() != Some(gid) || !same_key(&s("c").unwrap_or_default(), challenge) {
            return None;
        }
        Some(Sealed {
            pw: s("pw"),
            pass: s("pass"),
        })
    }
}

fn knock_key(shared: &[u8], challenge: &str, epk: &[u8]) -> Option<[u8; 16]> {
    let salt = unb64(challenge).filter(|c| c.len() == 16)?;
    let mut info = INFO.to_vec();
    info.extend_from_slice(epk);
    let mut key = [0u8; 16];
    hkdf::Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(&info, &mut key)
        .ok()?;
    Some(key)
}

/// What a sealed knock carried.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sealed {
    pub pw: Option<String>,
    pub pass: Option<String>,
}

/// A fresh knock challenge (16 random bytes, base64url).
pub(crate) fn challenge() -> Option<String> {
    random::<16>().map(|b| b64(&b))
}

/// What a guest does (the web page and the apps do the same; here for tests and the engine's
/// own bridge): seals `pw`/`pass` for `gid` to the room key `pk` with `challenge`.
#[cfg(test)]
pub(crate) fn seal(
    pk: &str,
    gid: &str,
    challenge: &str,
    pw: Option<&str>,
    pass: Option<&str>,
) -> Option<String> {
    let host = PublicKey::from_sec1_bytes(&unb64(pk)?).ok()?;
    let eph = RoomKey::generate()?;
    let epk = eph.0.public_key().to_sec1_point(false);
    let shared = p256::ecdh::diffie_hellman(eph.0.to_nonzero_scalar(), host.as_affine());
    let key = knock_key(
        shared.raw_secret_bytes().as_slice(),
        challenge,
        epk.as_bytes(),
    )?;
    let mut body = serde_json::json!({"gid": gid, "c": challenge});
    if let Some(p) = pw {
        body["pw"] = p.into();
    }
    if let Some(p) = pass {
        body["pass"] = p.into();
    }
    let nonce = random::<12>()?;
    let ct = Aes128Gcm::new_from_slice(&key)
        .ok()?
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: body.to_string().as_bytes(),
                aad: gid.as_bytes(),
            },
        )
        .ok()?;
    let mut out = epk.as_bytes().to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Some(b64(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_check_in_constant_time() {
        assert_eq!(hash_password("abc"), Err(PwErr::Short));
        assert_eq!(hash_password(&"x".repeat(65)), Err(PwErr::Long));
        let h = hash_password("couch42").unwrap();
        assert!(password_ok(&h, "couch42"));
        assert!(!password_ok(&h, "couch43"));
        assert!(!password_ok(&h, ""));
        let h2 = hash_password("couch42").unwrap();
        assert_ne!(h.salt, h2.salt, "a new salt each time");
        let broken = PwHash {
            salt: "!!".into(),
            ..h.clone()
        };
        assert!(!password_ok(&broken, "couch42"));
        // A hash made with another iteration count still checks with its own.
        let mut slow = h.clone();
        slow.iters = 2000;
        assert!(!password_ok(&slow, "couch42"));
    }

    #[test]
    fn a_sealed_knock_opens_only_for_its_gid_and_challenge() {
        let k = RoomKey::generate().unwrap();
        let pk = k.public();
        assert_eq!(pk.len(), 87);
        let c = challenge().unwrap();
        let blob = seal(&pk, "gid1", &c, Some("couch42"), Some("pass1")).unwrap();
        assert_eq!(
            k.open(&blob, "gid1", &c),
            Some(Sealed {
                pw: Some("couch42".into()),
                pass: Some("pass1".into())
            })
        );
        // Another gid, another challenge, another key: nothing.
        assert_eq!(k.open(&blob, "gid2", &c), None);
        assert_eq!(k.open(&blob, "gid1", &challenge().unwrap()), None);
        let other = RoomKey::generate().unwrap();
        assert_eq!(other.open(&blob, "gid1", &c), None);
        // A flipped byte fails the tag.
        let mut raw = unb64(&blob).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert_eq!(k.open(&b64(&raw), "gid1", &c), None);
        assert_eq!(k.open("short", "gid1", &c), None);
        // The key round-trips through its stored form.
        let back = RoomKey::from_b64(&k.to_b64()).unwrap();
        assert_eq!(back.public(), pk);
        assert!(back.open(&blob, "gid1", &c).is_some());
    }
}
