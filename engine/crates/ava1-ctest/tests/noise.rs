#![cfg(unix)]
use ava1::hex;
use ava1::keys::{self, Handshake, Identity};
use ava1_ctest::{ffi, CHandshake};

fn v() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../protocol/ava1/vectors/noise_xx.json"
    ))
    .unwrap()
}
fn b32(v: &serde_json::Value, k: &str) -> [u8; 32] {
    hex::decode(v[k].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

#[test]
fn the_c_noise_struct_layout_matches() {
    assert_eq!(
        unsafe { ffi::ava1_test_sizeof_noise() },
        std::mem::size_of::<ffi::CNoise>()
    );
}

#[test]
fn c_reproduces_the_published_vector() {
    let v = v();
    let pro = hex::decode(v["init_prologue"].as_str().unwrap()).unwrap();
    let mut i = CHandshake::new(
        true,
        b32(&v, "init_static"),
        b32(&v, "init_ephemeral"),
        &pro,
    );
    let mut r = CHandshake::new(
        false,
        b32(&v, "resp_static"),
        b32(&v, "resp_ephemeral"),
        &pro,
    );
    for (n, m) in v["messages"].as_array().unwrap().iter().take(3).enumerate() {
        let payload = hex::decode(m["payload"].as_str().unwrap()).unwrap();
        let (w, rd) = if n % 2 == 0 {
            (&mut i, &mut r)
        } else {
            (&mut r, &mut i)
        };
        let msg = w.write(&payload).unwrap();
        assert_eq!(
            hex::encode(&msg),
            m["ciphertext"].as_str().unwrap(),
            "message {n}"
        );
        assert_eq!(rd.read(&msg).unwrap(), payload);
    }
    assert_eq!(
        hex::encode(&i.hash()),
        v["handshake_hash"].as_str().unwrap()
    );
    assert_eq!(i.split(), r.split());
}

#[test]
fn c_and_snow_complete_handshakes_with_each_other() {
    for c_is_initiator in [true, false] {
        let (a, b) = (
            keys::random_bytes::<32>().unwrap(),
            keys::random_bytes::<32>().unwrap(),
        );
        let eph = keys::random_bytes::<32>().unwrap();
        let rust_id = Identity::from_secret(b);
        let mut c = CHandshake::new(c_is_initiator, a, eph, keys::PROLOGUE);
        let mut r = if c_is_initiator {
            Handshake::responder(&rust_id)
        } else {
            Handshake::initiator(&rust_id)
        }
        .unwrap();
        for step in 0..3 {
            let c_turn = (step % 2 == 0) == c_is_initiator;
            if c_turn {
                let m = c.write(format!("c{step}").as_bytes()).unwrap();
                assert_eq!(r.read(&m).unwrap(), format!("c{step}").as_bytes());
            } else {
                let m = r.write(format!("r{step}").as_bytes()).unwrap();
                assert_eq!(c.read(&m).unwrap(), format!("r{step}").as_bytes());
            }
        }
        assert_eq!(c.remote_static(), rust_id.public());
        let keys = r.finish();
        assert_eq!(c.hash(), keys.hash);
        assert_eq!(c.split(), (keys.c2s, keys.s2c));
    }
}

#[test]
fn c_derivations_and_sealing_match_rust() {
    let dir = [0x11u8; 32];
    let mut out32 = [0u8; 32];
    let mut out16 = [0u8; 16];
    let (cn, sn, sid) = ([4u8; 16], [5u8; 16], [3u8; 16]);
    for lane in [0u16, 1, 8, 0xffff] {
        unsafe {
            ffi::ava1_lane_key(
                dir.as_ptr(),
                lane,
                cn.as_ptr(),
                sn.as_ptr(),
                out32.as_mut_ptr(),
            )
        };
        assert_eq!(out32, keys::lane_key(&dir, lane, &cn, &sn));
    }
    unsafe { ffi::ava1_control_key(dir.as_ptr(), out32.as_mut_ptr()) };
    assert_eq!(out32, keys::control_key(&dir));
    unsafe {
        ffi::ava1_join_tag(
            dir.as_ptr(),
            sid.as_ptr(),
            5,
            cn.as_ptr(),
            out16.as_mut_ptr(),
        )
    };
    assert_eq!(out16, keys::join_tag(&dir, &sid, 5, &cn));
    unsafe {
        ffi::ava1_join_ack_tag(
            dir.as_ptr(),
            sid.as_ptr(),
            5,
            cn.as_ptr(),
            sn.as_ptr(),
            out16.as_mut_ptr(),
        )
    };
    assert_eq!(out16, keys::join_ack_tag(&dir, &sid, 5, &cn, &sn));

    for len in [0usize, 1, 15, 16, 17, 4096, 70_000] {
        let body: Vec<u8> = (0..len).map(|i| i as u8).collect();
        let ad = [9u8; 12];
        let mut rust = body.clone();
        keys::seal(&dir, 42, &ad, &mut rust);
        let mut c = body.clone();
        let mut mac = [0u8; 16];
        unsafe {
            ffi::ava1_seal(
                dir.as_ptr(),
                42,
                ad.as_ptr(),
                ad.len(),
                c.as_mut_ptr(),
                c.len(),
                mac.as_mut_ptr(),
            )
        };
        c.extend_from_slice(&mac);
        assert_eq!(c, rust, "len {len}");
        let (mut ct, tag) = (rust[..len].to_vec(), rust[len..].to_vec());
        assert_eq!(
            unsafe {
                ffi::ava1_open(
                    dir.as_ptr(),
                    42,
                    ad.as_ptr(),
                    ad.len(),
                    ct.as_mut_ptr(),
                    ct.len(),
                    tag.as_ptr(),
                )
            },
            0
        );
        assert_eq!(ct, body);
        // The header is the AD: a frame whose header was altered (CRC fixed up) never opens.
        let mut bad_ad = ad;
        bad_ad[4] ^= 1;
        let mut tampered = rust[..len].to_vec();
        assert_ne!(
            unsafe {
                ffi::ava1_open(
                    dir.as_ptr(),
                    42,
                    bad_ad.as_ptr(),
                    bad_ad.len(),
                    tampered.as_mut_ptr(),
                    tampered.len(),
                    tag.as_ptr(),
                )
            },
            0
        );
        let mut forged = rust[..len].to_vec();
        assert_ne!(
            unsafe {
                ffi::ava1_open(
                    dir.as_ptr(),
                    43,
                    ad.as_ptr(),
                    ad.len(),
                    forged.as_mut_ptr(),
                    forged.len(),
                    tag.as_ptr(),
                )
            },
            0
        );
    }
}

#[test]
fn c_refuses_garbage_and_out_of_turn_messages() {
    let mut resp = CHandshake::new(false, [1; 32], [2; 32], keys::PROLOGUE);
    assert!(resp.write(b"").is_err(), "a responder cannot speak first");
    assert!(
        resp.read(&[0u8; 5]).is_err(),
        "message 1 is at least 32 bytes"
    );
    let mut init = CHandshake::new(true, [3; 32], [4; 32], keys::PROLOGUE);
    let m1 = init.write(b"").unwrap();
    let mut resp = CHandshake::new(false, [1; 32], [2; 32], keys::PROLOGUE);
    resp.read(&m1).unwrap();
    let mut m2 = resp.write(b"").unwrap();
    m2[50] ^= 1;
    assert!(init.read(&m2).is_err(), "a tampered message 2 is refused");
}

/// S1: the server's commitment, from the shared vectors, in C.
#[test]
fn c_pairing_commit_reproduces_the_vectors() {
    let h = |s: &str| hex::decode(s).unwrap();
    let mut n = 0;
    for l in include_str!("../../../../protocol/ava1/vectors/pairing.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let ns = h(f[1]);
        let mut commit = [0u8; 32];
        unsafe { ffi::ava1_pair_commit(ns.as_ptr(), commit.as_mut_ptr()) };
        assert_eq!(hex::encode(&commit), f[2], "{l}");
        n += 1;
    }
    assert!(n >= 3);
}

/// The pairing PAKE (SPEC.md 5.5): G, both public values, K and both confirmations, from the
/// shared vectors, in C. Rust checks the same file (cpace.rs), so the two agree byte for byte.
#[test]
fn c_cpace_reproduces_the_vectors() {
    let h = |s: &str| hex::decode(s).unwrap();
    let mut n = 0;
    for l in include_str!("../../../../protocol/ava1/vectors/cpace.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let (hash, xa, xb) = (h(f[1]), h(f[3]), h(f[4]));
        let code: u32 = f[2].parse().unwrap();
        let (mut g, mut ya, mut yb, mut k, mut k2) =
            ([0u8; 32], [0u8; 32], [0u8; 32], [0u8; 32], [0u8; 32]);
        let (mut mc, mut ms) = ([0u8; 32], [0u8; 32]);
        unsafe {
            ffi::ava1_cpace_generator(hash.as_ptr(), code, g.as_mut_ptr());
            assert_eq!(
                ffi::ava1_cpace_public(xa.as_ptr(), g.as_ptr(), ya.as_mut_ptr()),
                0
            );
            assert_eq!(
                ffi::ava1_cpace_public(xb.as_ptr(), g.as_ptr(), yb.as_mut_ptr()),
                0
            );
            assert_eq!(
                ffi::ava1_cpace_key(
                    hash.as_ptr(),
                    xa.as_ptr(),
                    yb.as_ptr(),
                    ya.as_ptr(),
                    yb.as_ptr(),
                    k.as_mut_ptr()
                ),
                0
            );
            assert_eq!(
                ffi::ava1_cpace_key(
                    hash.as_ptr(),
                    xb.as_ptr(),
                    ya.as_ptr(),
                    ya.as_ptr(),
                    yb.as_ptr(),
                    k2.as_mut_ptr()
                ),
                0
            );
            ffi::ava1_cpace_mac(k.as_ptr(), 0, hash.as_ptr(), mc.as_mut_ptr());
            ffi::ava1_cpace_mac(k.as_ptr(), 1, hash.as_ptr(), ms.as_mut_ptr());
        }
        assert_eq!(k, k2, "both sides agree: {l}");
        for (got, want, what) in [
            (&g, f[5], "G"),
            (&ya, f[6], "Ya"),
            (&yb, f[7], "Yb"),
            (&k, f[8], "K"),
            (&mc, f[9], "client mac"),
            (&ms, f[10], "server mac"),
        ] {
            assert_eq!(hex::encode(got), want, "{what}: {l}");
        }
        n += 1;
    }
    assert!(n >= 5);
}

/// The Elligator2 map is the one place two implementations could drift: Rust's port against
/// Monocypher's, on many inputs (the code feeds it through a hash, so these stand in for
/// every code).
#[test]
fn the_rust_elligator_map_equals_monocypher_on_many_inputs() {
    // The special inputs first: r = 0 and r = 1 (where the map's formulas are least generic),
    // r = 2^254 - 1 (the largest 254-bit value) and one with the two ignored top bits set.
    let mut special = vec![[0u8; 32], [0xffu8; 32]];
    let mut one = [0u8; 32];
    one[0] = 1;
    special.push(one);
    let mut top = [0u8; 32];
    top[31] = 0xc0; // only the ignored bits: the same r as zero
    special.push(top);
    let mut max = [0xffu8; 32];
    max[31] = 0x3f;
    special.push(max);
    for (i, hidden) in special.iter().enumerate() {
        let mut c = [0u8; 32];
        unsafe { ffi::crypto_elligator_map(c.as_mut_ptr(), hidden.as_ptr()) };
        assert_eq!(c, ava1::cpace::elligator_map(hidden), "special {i}");
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..3000u32 {
        let mut hidden = [0u8; 32];
        for c in hidden.chunks_mut(8) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            c.copy_from_slice(&seed.to_le_bytes());
        }
        if i == 0 {
            hidden = [0xff; 32];
        }
        let mut c = [0u8; 32];
        unsafe { ffi::crypto_elligator_map(c.as_mut_ptr(), hidden.as_ptr()) };
        assert_eq!(c, ava1::cpace::elligator_map(&hidden), "{i}");
    }
}

#[test]
fn c_key_derivations_reproduce_the_vectors() {
    let h = |s: &str| hex::decode(s).unwrap();
    let mut n = 0;
    for l in include_str!("../../../../protocol/ava1/vectors/keys.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let (dir, lane) = (h(f[1]), f[2].parse::<u16>().unwrap());
        let cn = h(f[3]);
        let mut out = vec![0u8; if f[0] == "lane_key" { 32 } else { 16 }];
        unsafe {
            match f[0] {
                "lane_key" => ffi::ava1_lane_key(
                    dir.as_ptr(),
                    lane,
                    cn.as_ptr(),
                    h(f[4]).as_ptr(),
                    out.as_mut_ptr(),
                ),
                "join_tag" => ffi::ava1_join_tag(
                    dir.as_ptr(),
                    h(f[5]).as_ptr(),
                    lane,
                    cn.as_ptr(),
                    out.as_mut_ptr(),
                ),
                "join_ack_tag" => ffi::ava1_join_ack_tag(
                    dir.as_ptr(),
                    h(f[5]).as_ptr(),
                    lane,
                    cn.as_ptr(),
                    h(f[4]).as_ptr(),
                    out.as_mut_ptr(),
                ),
                k => panic!("unknown vector kind {k}"),
            }
        }
        assert_eq!(hex::encode(&out), *f.last().unwrap(), "{l}");
        n += 1;
    }
    assert!(n >= 5);
}

/// A sealed frame from the Rust writer, read back by the C reader: as sent it opens; with
/// any header byte changed (CRC recomputed, so only the AD check can catch it) it does not.
#[test]
fn the_c_reader_refuses_a_frame_whose_header_was_altered() {
    use ava1::conn::FrameWriter;
    let key = [0x21u8; 32];
    let frame = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let mut w = FrameWriter::new(Vec::new());
            w.set_key(key);
            w.send(0x09, 7, b"heartbeat body").await.unwrap();
            w.into_inner()
        });
    let open =
        |f: &[u8]| unsafe { ffi::ava1_test_conn_open_frame(key.as_ptr(), f.as_ptr(), f.len()) };
    assert_eq!(open(&frame), 0, "the untouched frame opens");
    for at in [2usize, 4, 7] {
        let mut t = frame.clone();
        t[at] ^= 0x01; // type, channel
        let crc = ava1::crc32c::crc32c(&t[..12]);
        t[12..16].copy_from_slice(&crc.to_le_bytes());
        assert_ne!(open(&t), 0, "header byte {at} altered");
    }
}

#[test]
fn a_failed_c_handshake_stays_failed_and_yields_no_keys() {
    let mut init = CHandshake::new(true, [3; 32], [4; 32], keys::PROLOGUE);
    assert!(init.try_split().is_err(), "no keys before the handshake");
    let m1 = init.write(b"").unwrap();
    let mut resp = CHandshake::new(false, [1; 32], [2; 32], keys::PROLOGUE);
    resp.read(&m1).unwrap();
    let good = resp.write(b"").unwrap();
    assert!(
        resp.try_split().is_err(),
        "no keys after two of three messages"
    );
    let mut bad = good.clone();
    bad[50] ^= 1;
    assert!(init.read(&bad).is_err());
    // Sticky: the untampered message is refused too — the failed read already mixed the
    // forged bytes into the hash — and nothing can be written or split afterwards.
    assert!(init.read(&good).is_err());
    assert!(init.write(b"").is_err());
    assert!(init.try_split().is_err());
}

#[test]
fn low_order_keys_are_refused_by_c_and_rust() {
    let low: [u8; 32] = {
        let mut p = [0u8; 32];
        p[0] = 1; // u = 1, order 1 on the Montgomery curve
        p
    };
    // C responder, hostile initiator: message 1 carries a low-order ephemeral key. The
    // ee DH (done when the responder writes message 2) would be all zero.
    let mut resp = CHandshake::new(false, [1; 32], [2; 32], keys::PROLOGUE);
    let mut m1 = low.to_vec();
    m1.extend_from_slice(b"hi");
    let _ = resp.read(&m1);
    assert!(resp.write(b"").is_err());
    assert!(resp.try_split().is_err());
    // The same message to the Rust responder.
    let id = Identity::from_secret([7; 32]);
    let mut r = Handshake::responder(&id).unwrap();
    assert!(matches!(r.read(&m1), Err(ava1::Ava1Error::WeakKey)));
    // A hostile responder whose static key is low-order: the Rust initiator refuses
    // message 2, and so does the C initiator.
    for rust_initiator in [true, false] {
        let mut hostile = CHandshake::new(false, [1; 32], [2; 32], keys::PROLOGUE);
        hostile.set_static_public(low);
        if rust_initiator {
            let mut i = Handshake::initiator(&id).unwrap();
            hostile.read(&i.write(b"").unwrap()).unwrap();
            let m2 = hostile.write(b"").unwrap();
            assert!(i.read(&m2).is_err());
        } else {
            let mut i = CHandshake::new(true, [3; 32], [4; 32], keys::PROLOGUE);
            hostile.read(&i.write(b"").unwrap()).unwrap();
            let m2 = hostile.write(b"").unwrap();
            assert!(i.read(&m2).is_err());
            assert!(i.try_split().is_err());
        }
    }
}

/// Review 006 #1: the C connection's counters stay in lockstep across frame kinds and the
/// nonce ceiling refuses to seal or open (payload/ava1/ava1_conn.c).
#[test]
fn the_c_connection_counts_every_frame_and_stops_at_the_nonce_ceiling() {
    let key = [0x6bu8; 32];
    assert_eq!(
        unsafe { ffi::ava1_test_conn_nonce_ceiling(key.as_ptr()) },
        0
    );
}

/// Review 006 #4 (checklist A): negative frame vectors against the C reader. The same wire bytes
/// the Rust reader refuses (`ava1::conn` tests) are refused here, with the same count of frames
/// opened first: a frame cut anywhere ends the stream (`AVA1_E_CLOSED`), and reordered, dropped
/// or repeated frames fail the tag (`AVA1_E_TAG`).
#[test]
fn the_c_reader_refuses_truncated_and_misordered_frames() {
    use ava1::conn::FrameWriter;
    let key = [0x32u8; 32];
    let bodies: [&[u8]; 3] = [b"first frame", b"second frame!", b"third"];
    let wire = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let mut w = FrameWriter::new(Vec::new());
            w.set_key(key);
            for body in bodies {
                w.send(0x20, 1, body).await.unwrap();
            }
            w.into_inner()
        });
    // A sealed frame is a 16-byte header, the body and a 16-byte MAC.
    let lens: Vec<usize> = bodies.iter().map(|b| 16 + b.len() + 16).collect();
    assert_eq!(lens.iter().sum::<usize>(), wire.len());
    let read = |bytes: &[u8]| {
        let mut opened = 0u32;
        let rc = unsafe {
            ffi::ava1_test_conn_read_all(key.as_ptr(), bytes.as_ptr(), bytes.len(), &mut opened)
        };
        (opened, rc)
    };
    const E_TAG: i32 = -10;
    const E_CLOSED: i32 = -12;
    assert_eq!(read(&wire), (3, E_CLOSED), "an intact stream ends at EOF");
    for cut in 0..wire.len() {
        let whole = lens
            .iter()
            .scan(0, |at, n| {
                *at += n;
                Some(*at)
            })
            .filter(|end| *end <= cut)
            .count() as u32;
        assert_eq!(read(&wire[..cut]), (whole, E_CLOSED), "cut at {cut}");
    }
    let f = |i: usize| -> &[u8] {
        let start: usize = lens[..i].iter().sum();
        &wire[start..start + lens[i]]
    };
    for (what, bytes, opened) in [
        ("swapped", [f(1), f(0), f(2)].concat(), 0),
        ("first dropped", [f(1), f(2)].concat(), 0),
        ("middle dropped", [f(0), f(2)].concat(), 1),
        ("repeated", [f(0), f(0), f(1)].concat(), 1),
    ] {
        assert_eq!(read(&bytes), (opened, E_TAG), "{what}");
    }
}
