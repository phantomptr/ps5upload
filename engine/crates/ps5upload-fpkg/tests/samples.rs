use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::verify::verify_package;

fn sample_dir() -> std::path::PathBuf {
    std::env::var("PS5UPLOAD_SAMPLE_PKGS")
        .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into())
        .into()
}

#[test]
fn every_check_passes_on_real_debug_samples() {
    let mut checked = 0;
    for name in ["webbrowser.pkg", "EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg"] {
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
