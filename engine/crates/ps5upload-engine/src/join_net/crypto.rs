//! str0m's crypto on pure Rust: DTLS by dimpl's RustCrypto backend, SRTP, STUN's HMAC-SHA1 and the
//! certificate fingerprints' SHA-1 and SHA-256 by RustCrypto, the certificate itself by rcgen on
//! ring.
//!
//! Adapted from `str0m-rust-crypto` 0.7.0 (the one for str0m 0.25 and dimpl 0.9; MIT OR Apache-2.0, Copyright (c) the str0m authors),
//! which cannot be used as it is: it turns on dimpl's `rcgen` feature, and with it aws-lc (a C
//! library) on every target, the phones included. The engine already has ring (through rustls).
//!
//! Play Together uses data channels only; SRTP is here because the DTLS handshake still offers its
//! profiles and str0m makes the contexts once it completes.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use aes::cipher::Array;
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
use ctr::cipher::{KeyIvInit, StreamCipher};
use str0m::crypto::dtls::{
    DtlsCert, DtlsImplError, DtlsInstance, DtlsOutput, DtlsProvider, DtlsVersion, ProtocolVersion,
};
use str0m::crypto::{
    AeadAes128Gcm, AeadAes128GcmCipher, AeadAes256Gcm, AeadAes256GcmCipher, Aes128CmSha1_80Cipher,
    CryptoError, CryptoProvider, Sha1HmacProvider, Sha1Provider, Sha256Provider, SrtpProvider,
    SupportedAeadAes128Gcm, SupportedAeadAes256Gcm, SupportedAes128CmSha1_80,
};

/// The provider every peer connection uses.
pub(crate) fn provider() -> Arc<CryptoProvider> {
    static P: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| {
        static SRTP: Srtp = Srtp;
        static SHA1_HMAC: Sha1Hmac = Sha1Hmac;
        static SHA1: Sha1 = Sha1;
        static SHA256: Sha256 = Sha256;
        static DTLS: Dtls = Dtls;
        Arc::new(CryptoProvider {
            srtp_provider: &SRTP,
            sha1_hmac_provider: &SHA1_HMAC,
            sha1_provider: &SHA1,
            sha256_provider: &SHA256,
            dtls_provider: &DTLS,
        })
    })
    .clone()
}

/// A new self-signed ECDSA P-256 certificate (what browsers make for themselves). Its serial is
/// random: Firefox refuses two certificates with one serial from one issuer name.
pub(crate) fn new_cert() -> Option<DtlsCert> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).ok()?;
    let mut params = CertificateParams::new(Vec::<String>::new()).ok()?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "PS5Upload Play Together");
    params.distinguished_name = dn;
    params.is_ca = IsCa::NoCa;
    // rcgen's default validity (1975-4096): nobody checks a DTLS certificate's dates, only its
    // fingerprint, and a computer's wrong clock then never matters.
    let mut serial = [0u8; 16];
    getrandom::fill(&mut serial).ok()?;
    serial[0] &= 0x7f; // a positive integer
    params.serial_number = Some(serial.to_vec().into());
    let cert = params.self_signed(&key).ok()?;
    Some(DtlsCert {
        certificate: cert.der().to_vec(),
        private_key: key.serialize_der(),
    })
}

/// SHA-256 of `der`, as SDP and the invite link write it: `AB:CD:…` (32 bytes, upper-case hex).
pub(crate) fn fingerprint_of(der: &[u8]) -> String {
    Sha256
        .sha256(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

// ── DTLS ─────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Dtls;

impl DtlsProvider for Dtls {
    fn generate_certificate(&self) -> Option<DtlsCert> {
        new_cert()
    }

    fn new_dtls(
        &self,
        cert: &DtlsCert,
        now: Instant,
        dtls_version: DtlsVersion,
        mtu: Option<usize>,
    ) -> Result<Box<dyn DtlsInstance>, CryptoError> {
        let dimpl_cert = dimpl::DtlsCertificate {
            certificate: cert.certificate.clone(),
            private_key: cert.private_key.clone(),
        };
        // ICE has checked the return route before DTLS starts: no server cookie needed.
        let mut builder = dimpl::Config::builder().use_server_cookie(false);
        if let Some(mtu) = mtu {
            builder = builder.mtu(mtu);
        }
        let config = Arc::new(
            builder
                .build()
                .map_err(|e| CryptoError::Other(format!("dimpl config: {e}")))?,
        );
        let dtls = match dtls_version {
            DtlsVersion::Dtls12 => dimpl::Dtls::new_12(config, dimpl_cert, now),
            DtlsVersion::Dtls13 => dimpl::Dtls::new_13(config, dimpl_cert, now),
            DtlsVersion::Auto => dimpl::Dtls::new_auto(config, dimpl_cert, now),
            v => return Err(CryptoError::Other(format!("unsupported DTLS version {v}"))),
        };
        Ok(Box::new(DtlsConn(dtls)))
    }
}

struct DtlsConn(dimpl::Dtls);

impl std::fmt::Debug for DtlsConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DtlsConn")
    }
}

impl DtlsInstance for DtlsConn {
    fn set_active(&mut self, active: bool) {
        self.0.set_active(active);
    }
    fn handle_packet(&mut self, packet: &[u8]) -> Result<(), DtlsImplError> {
        self.0.handle_packet(packet)
    }
    fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> DtlsOutput<'a> {
        self.0.poll_output(buf)
    }
    fn handle_timeout(&mut self, now: Instant) -> Result<(), DtlsImplError> {
        self.0.handle_timeout(now)
    }
    fn send_application_data(&mut self, data: &[u8]) -> Result<usize, DtlsImplError> {
        self.0.send_application_data(data)
    }
    fn is_active(&self) -> bool {
        self.0.is_active()
    }
    fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.0.protocol_version()
    }
    fn is_closing(&self) -> bool {
        self.0.is_closing()
    }
    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
    fn close(&mut self) -> Result<(), DtlsImplError> {
        self.0.close()
    }
}

// ── Hashes ───────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Sha1Hmac;

impl Sha1HmacProvider for Sha1Hmac {
    fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
        use hmac::{KeyInit, Mac};
        let mut mac = <hmac::Hmac<sha1::Sha1> as KeyInit>::new_from_slice(key)
            .expect("HMAC takes a key of any size");
        for p in payloads {
            mac.update(p);
        }
        mac.finalize().into_bytes().into()
    }
}

#[derive(Debug)]
struct Sha1;

impl Sha1Provider for Sha1 {
    fn sha1(&self, data: &[u8]) -> [u8; 20] {
        use sha1::Digest;
        sha1::Sha1::digest(data).into()
    }
}

#[derive(Debug)]
struct Sha256;

impl Sha256Provider for Sha256 {
    fn sha256(&self, data: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        sha2::Sha256::digest(data).into()
    }
}

// ── SRTP ─────────────────────────────────────────────────────────────────────────────────────────

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

struct CmCipher {
    key: [u8; 16],
}

impl std::fmt::Debug for CmCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CmCipher")
    }
}

impl Aes128CmSha1_80Cipher for CmCipher {
    fn encrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        let mut c = Aes128Ctr::new(&self.key.into(), iv.into());
        output[..input.len()].copy_from_slice(input);
        c.apply_keystream(&mut output[..input.len()]);
        Ok(())
    }
    fn decrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        self.encrypt(iv, input, output)
    }
}

fn gcm_seal<C: Aead<NonceSize = U12>>(
    c: &C,
    iv: &[u8; 12],
    aad: &[u8],
    input: &[u8],
    output: &mut [u8],
) -> Result<(), CryptoError> {
    let sealed = c
        .encrypt(&Nonce::from(*iv), Payload { msg: input, aad })
        .map_err(|e| CryptoError::Other(format!("AES-GCM encrypt: {e:?}")))?;
    output[..sealed.len()].copy_from_slice(&sealed);
    Ok(())
}

fn gcm_open<C: Aead<NonceSize = U12>>(
    c: &C,
    iv: &[u8; 12],
    aads: &[&[u8]],
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, CryptoError> {
    let joined;
    let aad = if aads.len() == 1 {
        aads[0]
    } else {
        joined = aads.concat();
        &joined
    };
    let plain = c
        .decrypt(&Nonce::from(*iv), Payload { msg: input, aad })
        .map_err(|e| CryptoError::Other(format!("AES-GCM decrypt: {e:?}")))?;
    output[..plain.len()].copy_from_slice(&plain);
    Ok(plain.len())
}

struct Gcm128(Aes128Gcm);
struct Gcm256(Aes256Gcm);

impl std::fmt::Debug for Gcm128 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gcm128")
    }
}

impl std::fmt::Debug for Gcm256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gcm256")
    }
}

impl AeadAes128GcmCipher for Gcm128 {
    fn encrypt(
        &mut self,
        iv: &[u8; AeadAes128Gcm::IV_LEN],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        gcm_seal(&self.0, iv, aad, input, output)
    }
    fn decrypt(
        &mut self,
        iv: &[u8; AeadAes128Gcm::IV_LEN],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError> {
        gcm_open(&self.0, iv, aads, input, output)
    }
}

impl AeadAes256GcmCipher for Gcm256 {
    fn encrypt(
        &mut self,
        iv: &[u8; AeadAes256Gcm::IV_LEN],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        gcm_seal(&self.0, iv, aad, input, output)
    }
    fn decrypt(
        &mut self,
        iv: &[u8; AeadAes256Gcm::IV_LEN],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError> {
        gcm_open(&self.0, iv, aads, input, output)
    }
}

#[derive(Debug)]
struct Cm;
#[derive(Debug)]
struct Aead128;
#[derive(Debug)]
struct Aead256;

impl SupportedAes128CmSha1_80 for Cm {
    fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn Aes128CmSha1_80Cipher> {
        Box::new(CmCipher { key })
    }
}

impl SupportedAeadAes128Gcm for Aead128 {
    fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn AeadAes128GcmCipher> {
        Box::new(Gcm128(Aes128Gcm::new(&key.into())))
    }
}

impl SupportedAeadAes256Gcm for Aead256 {
    fn create_cipher(&self, key: [u8; 32], _encrypt: bool) -> Box<dyn AeadAes256GcmCipher> {
        Box::new(Gcm256(Aes256Gcm::new(&key.into())))
    }
}

#[derive(Debug)]
struct Srtp;

/// One AES-ECB block of `input` then one of PKCS#7 padding (what SRTP's key derivation asks).
fn ecb_round<C>(c: &C, input: &[u8], output: &mut [u8])
where
    C: aes::cipher::BlockCipherEncrypt
        + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>,
{
    assert!(input.len() == 16 && output.len() >= 32);
    let mut b1 = Array::try_from(&input[..16]).expect("AES block length");
    c.encrypt_block(&mut b1);
    output[..16].copy_from_slice(&b1);
    let mut b2 = Array::from([0x10u8; 16]);
    c.encrypt_block(&mut b2);
    output[16..32].copy_from_slice(&b2);
}

impl SrtpProvider for Srtp {
    fn aes_128_cm_sha1_80(&self) -> &'static dyn SupportedAes128CmSha1_80 {
        &Cm
    }
    fn aead_aes_128_gcm(&self) -> &'static dyn SupportedAeadAes128Gcm {
        &Aead128
    }
    fn aead_aes_256_gcm(&self) -> &'static dyn SupportedAeadAes256Gcm {
        &Aead256
    }
    fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        use aes::cipher::KeyInit;
        ecb_round(
            &aes::Aes128::new(key.try_into().expect("AES-128 key length")),
            input,
            output,
        );
    }
    fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        use aes::cipher::KeyInit;
        ecb_round(
            &aes::Aes256::new(key.try_into().expect("AES-256 key length")),
            input,
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_certificate_has_a_colon_hex_sha256_fingerprint() {
        let c = new_cert().unwrap();
        let fp = fingerprint_of(&c.certificate);
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp
            .split(':')
            .all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit())));
        assert_ne!(fp, fingerprint_of(&new_cert().unwrap().certificate));
    }
}
