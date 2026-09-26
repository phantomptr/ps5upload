use ps5upload_fpkg::crypto::{sha3, XtsKeys, DEFAULT_PASSCODE};
use ps5upload_fpkg::verify::verify_package;
use ps5upload_fpkg::{cnt, fih, keys, outer, si, xts::Xts, PkgFile, BLOCK};

fn sample_dir() -> std::path::PathBuf {
    std::env::var("PS5UPLOAD_SAMPLE_PKGS")
        .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into())
        .into()
}

/// One TLV payload of the metric blob: `{tag reversed, u32 1, u64 len, payload}`.
fn tlv<'a>(blob: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let mut at = 0usize;
    while at + 16 <= blob.len() {
        let len = u64::from_le_bytes(blob[at + 8..at + 16].try_into().unwrap()) as usize;
        if blob[at..at + 4] == [tag[3], tag[2], tag[1], tag[0]] {
            return blob.get(at + 16..at + 16 + len);
        }
        at += 16 + len;
    }
    None
}

/// The metric blob's `obdg` table is one `SHA3-256` per stored block of the inner image, in
/// order — measured on Sony's own `webbrowser.pkg`, whose first entry is exactly the digest
/// of its stored image's first 64 KiB block and whose table runs to the image's block count
/// (`FIH 0x90`, which the other samples agree with: 5, 3, 1 and 15736 entries).
#[test]
fn the_metric_blob_carries_a_digest_per_stored_block() {
    let path = sample_dir().join("webbrowser.pkg");
    let Ok(mut pkg) = PkgFile::open(&path) else {
        eprintln!("skip: {} not present", path.display());
        return;
    };
    let head = pkg.read_at(0, fih::HEADER_LEN).unwrap();
    let parsed = fih::parse(&head).unwrap();
    let container = cnt::read(&mut pkg, parsed.cnt_offset).unwrap();
    let image = outer::open(&mut pkg, &parsed, &container, DEFAULT_PASSCODE).unwrap();
    let stored = image.file_data(&image.dinodes()[3]);

    let si = si::read(&mut pkg).unwrap().expect("the sample has an SI");
    let member = si
        .members
        .iter()
        .find(|m| m.name.ends_with("naps_meta_18.dat"))
        .expect("the sample's metric blob");
    let mut blob = pkg.read_at(member.offset, member.size as usize).unwrap();
    Xts::new(&XtsKeys {
        tweak: keys::NAPS_META_18_TWEAK_KEY,
        data: keys::NAPS_META_18_DATA_KEY,
    })
    .decrypt(keys::NAPS_META_18_TWEAK_SECTOR, &mut blob);

    let obdg = tlv(&blob, b"obdg").expect("obdg in the metric blob");
    let blocks = stored.len().div_ceil(BLOCK as usize);
    assert_eq!(obdg.len(), blocks * 32, "one digest per stored block");
    for i in 0..blocks {
        let want = sha3(&stored[i * BLOCK as usize..(i + 1) * BLOCK as usize]);
        assert_eq!(&obdg[i * 32..i * 32 + 32], &want, "block {i} digest");
    }
}

#[test]
fn every_check_passes_on_real_debug_samples() {
    let mut checked = 0;
    for name in [
        "webbrowser.pkg",
        "EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg",
        "Crimson.Desert.DLC.Unlocker-DUPLEX.pkg",
    ] {
        let path = sample_dir().join(name);
        if !path.exists() {
            eprintln!("skip: {} not present", path.display());
            continue;
        }
        let report = verify_package(&path, DEFAULT_PASSCODE).unwrap();
        println!("{report}");
        assert!(report.ok(), "{name} failed:\n{report}");
        assert!(report.checks.len() >= 10, "{name}: too few checks ran");
        checked += 1;
    }
    eprintln!("verified {checked} real sample(s)");
}

#[test]
fn a_single_flipped_byte_fails_verification() {
    let src = sample_dir().join("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg");
    let Ok(mut bytes) = std::fs::read(&src) else {
        eprintln!("skip: {} not present", src.display());
        return;
    };
    // Inside outer block 3 (the inode table): its imagedigs check must fail.
    bytes[0x10000 + 3 * 0x10000 + 100] ^= 0x01;
    let tmp = std::env::temp_dir().join(format!("fpkg-tamper-{}.pkg", std::process::id()));
    std::fs::write(&tmp, &bytes).unwrap();
    let report = verify_package(&tmp, DEFAULT_PASSCODE).unwrap();
    std::fs::remove_file(&tmp).ok();
    assert!(!report.ok());
    assert!(report
        .checks
        .iter()
        .any(|c| !c.ok && c.name.starts_with("outer block 3")));
}
