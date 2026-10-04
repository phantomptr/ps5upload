//! The payload's frame AEAD (ava1_aead.c) against the engine's (ring): every byte the same
//! in both directions, on each ChaCha20 path the host CPU can run. On x86-64 CI that
//! includes the AVX2 path; payload/ava1/test/aead_test.c adds the RFC vectors and
//! Monocypher.
#![cfg(unix)]
use ava1::keys;
use ava1::wire::SplitMix;
use ava1_ctest::ffi;
use std::ffi::CStr;

fn backend() -> String {
    unsafe { CStr::from_ptr(ffi::ava1_aead_backend()) }
        .to_string_lossy()
        .into_owned()
}

fn c_seal(key: &[u8; 32], n: u64, ad: &[u8], buf: &mut Vec<u8>) {
    let mut mac = [0u8; 16];
    unsafe {
        ffi::ava1_seal(
            key.as_ptr(),
            n,
            ad.as_ptr(),
            ad.len(),
            buf.as_mut_ptr(),
            buf.len(),
            mac.as_mut_ptr(),
        )
    };
    buf.extend_from_slice(&mac);
}

fn c_open(key: &[u8; 32], n: u64, ad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let at = sealed.len() - 16;
    let mut body = sealed[..at].to_vec();
    let rc = unsafe {
        ffi::ava1_open(
            key.as_ptr(),
            n,
            ad.as_ptr(),
            ad.len(),
            body.as_mut_ptr(),
            body.len(),
            sealed[at..].as_ptr(),
        )
    };
    if rc == 0 {
        Some(body)
    } else {
        // A refused frame is handed back exactly as it came in.
        assert_eq!(body, sealed[..at], "a refused open changed the buffer");
        None
    }
}

fn check(rng: &mut SplitMix, len: usize) {
    let mut key = [0u8; 32];
    rng.fill(&mut key);
    let n = rng.next_u64();
    let mut ad = vec![0u8; rng.below(33) as usize];
    rng.fill(&mut ad);
    let mut pt = vec![0u8; len];
    rng.fill(&mut pt);
    let mut rust = pt.clone();
    keys::seal(&key, n, &ad, &mut rust);
    let mut c = pt.clone();
    c_seal(&key, n, &ad, &mut c);
    assert!(c == rust, "{}: len {len}: C and Rust disagree", backend());
    // Each side opens the other's frame.
    assert_eq!(c_open(&key, n, &ad, &rust).as_deref(), Some(&pt[..]));
    let mut back = c.clone();
    assert!(keys::open(&key, n, &ad, &mut back) && back == pt);
    // One flipped bit anywhere is refused.
    let mut bad = rust.clone();
    let at = rng.below(bad.len() as u64) as usize;
    bad[at] ^= 1 << rng.below(8);
    assert!(
        c_open(&key, n, &ad, &bad).is_none(),
        "len {len}: flip at {at}"
    );
}

#[test]
fn c_and_rust_seal_identically_on_every_path() {
    let mut paths = vec![backend()];
    if paths[0] != "portable" {
        paths.push("portable".into());
    }
    for (i, want) in paths.iter().enumerate() {
        if i > 0 {
            unsafe { ffi::ava1_aead_allow_simd(0) };
        }
        assert_eq!(&backend(), want);
        let mut rng = SplitMix(7 + i as u64);
        for len in 0..=1100 {
            check(&mut rng, len);
        }
        for len in [
            4095,
            8191,
            8192,
            8193,
            65_543,
            1 << 20,
            (1 << 20) + 63,
            3_000_001,
            16 << 20,
        ] {
            check(&mut rng, len);
        }
        eprintln!("C {want} path agrees with Rust");
    }
    unsafe { ffi::ava1_aead_allow_simd(1) };
}

/// Review 007 #3: the C nonce layout, stated and pinned. The 12-byte nonce is four zero bytes then
/// the counter as a little-endian u64. The Rust side is sealed with those bytes spelled out by hand
/// (not through `keys::seal`), so a changed layout in either stack breaks this.
#[test]
fn the_c_nonce_is_four_zero_bytes_then_the_counter_little_endian() {
    let key = [0x5au8; 32];
    for n in [0u64, 1, 0x0102_0304_0506_0708, 1 << 32, u64::MAX - 1] {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&n.to_le_bytes());
        let mut want = b"nonce layout".to_vec();
        keys::seal_nonce(&key, &nonce, b"ad", &mut want);
        let mut got = b"nonce layout".to_vec();
        c_seal(&key, n, b"ad", &mut got);
        assert_eq!(got, want, "counter {n:#x}");
    }
}
