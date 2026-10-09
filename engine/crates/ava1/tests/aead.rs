//! The frame AEAD (`keys::seal`/`keys::open`): RFC 8439 vectors, byte-for-byte agreement
//! with the RustCrypto `chacha20poly1305` crate it replaced, and a throughput probe.
use ava1::hex;
use ava1::keys::{self, MAC_LEN};
use ava1::wire::SplitMix;
use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};

fn h(s: &str) -> Vec<u8> {
    hex::decode(&s.split_whitespace().collect::<String>()).unwrap()
}

/// RFC 8439 §2.8.2.
#[test]
fn rfc8439_aead_vector() {
    let key: [u8; 32] = std::array::from_fn(|i| 0x80 + i as u8);
    let nonce: [u8; 12] = h("07000000 4041424344454647").try_into().unwrap();
    let ad = h("50515253c0c1c2c3c4c5c6c7");
    let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for \
               the future, sunscreen would be it.";
    let ct = h(
        "d31a8d34648e60db7b86afbc53ef7ec2 a4aded51296e08fea9e2b5a736ee62d6
                3dbea45e8ca9671282fafb69da92728b 1a71de0a9e060b2905d6a5b67ecd3b36
                92ddbd7f2d778b8c9803aee328091b58 fab324e4fad675945585808b4831d7bc
                3ff4def08e4b7a9de576d26586cec64b 6116",
    );
    let tag = h("1ae10b594f09e26a7e902ecbd0600691");
    let mut buf = pt.to_vec();
    keys::seal_nonce(&key, &nonce, &ad, &mut buf);
    assert_eq!(hex::encode(&buf[..pt.len()]), hex::encode(&ct));
    assert_eq!(hex::encode(&buf[pt.len()..]), hex::encode(&tag));
    assert!(keys::open_nonce(&key, &nonce, &ad, &mut buf));
    assert_eq!(&buf[..], &pt[..]);
}

fn reference_seal(key: &[u8; 32], n: u64, ad: &[u8], buf: &mut Vec<u8>) {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&n.to_le_bytes());
    let tag = ChaCha20Poly1305::new(&Key::from(*key))
        .encrypt_inout_detached(&Nonce::from(nonce), ad, buf.as_mut_slice().into())
        .unwrap();
    buf.extend_from_slice(&tag);
}

fn check(rng: &mut SplitMix, len: usize, flips: usize) {
    let mut key = [0u8; 32];
    rng.fill(&mut key);
    let n = rng.next_u64();
    let mut ad = vec![0u8; rng.below(40) as usize];
    rng.fill(&mut ad);
    let mut pt = vec![0u8; len];
    rng.fill(&mut pt);
    let mut ours = pt.clone();
    keys::seal(&key, n, &ad, &mut ours);
    let mut theirs = pt.clone();
    reference_seal(&key, n, &ad, &mut theirs);
    assert!(ours == theirs, "len {len}: sealed bytes differ");
    let mut back = ours.clone();
    assert!(keys::open(&key, n, &ad, &mut back), "len {len}");
    assert!(back == pt, "len {len}: open did not round-trip");
    // A flipped bit anywhere (body, MAC, AD) is refused.
    for _ in 0..flips {
        let mut bad = ours.clone();
        let mut bad_ad = ad.clone();
        let at = rng.below((bad.len() + bad_ad.len()) as u64) as usize;
        let bit = 1u8 << rng.below(8);
        if at < bad.len() {
            bad[at] ^= bit;
        } else {
            bad_ad[at - bad.len()] ^= bit;
        }
        assert!(
            !keys::open(&key, n, &bad_ad, &mut bad),
            "len {len}: flip at {at} accepted"
        );
    }
    let mut wrong_n = ours.clone();
    assert!(!keys::open(&key, n ^ 1, &ad, &mut wrong_n));
}

#[test]
fn matches_the_previous_implementation_at_every_small_length() {
    let mut rng = SplitMix(1);
    for len in 0..=1100 {
        check(&mut rng, len, 4);
    }
}

#[test]
fn matches_the_previous_implementation_up_to_16_mib() {
    let mut rng = SplitMix(2);
    for len in [
        4095,
        4096,
        4097,
        65_535,
        65_536 + 7,
        1 << 20,
        (1 << 20) + 63,
        3_000_001,
        (16 << 20) - 1,
        16 << 20,
    ] {
        check(&mut rng, len, 2);
    }
}

#[test]
fn a_short_buffer_never_opens() {
    for len in 0..MAC_LEN {
        let mut b = vec![0u8; len];
        assert!(!keys::open(&[0; 32], 0, &[], &mut b));
    }
}

/// `cargo test --release -p ava1 --test aead -- --ignored --nocapture`
#[test]
#[ignore]
fn throughput() {
    const MIB: usize = 1 << 20;
    let rounds = 256u64;
    let key = [0x11u8; 32];
    let mut buf = vec![0x5au8; MIB];
    let t = std::time::Instant::now();
    for i in 0..rounds {
        keys::seal(&key, i, &[], &mut buf);
        buf.truncate(MIB);
    }
    let seal = (rounds as f64 * MIB as f64) / t.elapsed().as_micros().max(1) as f64;
    let mut sealed = vec![0x5au8; MIB];
    keys::seal(&key, 0, &[], &mut sealed);
    let t = std::time::Instant::now();
    for _ in 0..rounds {
        buf.clear();
        buf.extend_from_slice(&sealed);
        assert!(keys::open(&key, 0, &[], &mut buf));
    }
    let open = (rounds as f64 * MIB as f64) / t.elapsed().as_micros().max(1) as f64;
    println!("seal {seal:.0} MB/s, open {open:.0} MB/s (1 MiB frames, one core)");
}
