//! The streaming verifier on packages that are not what they claim.

use std::io::{Seek, SeekFrom, Write};

use super::*;
use crate::build::{self, BuildRequest};

/// A count the table has no room for is not walked: only the records that are there.
#[test]
fn a_flat_path_table_is_read_no_further_than_it_goes() {
    let mut flt = vec![0u8; 0x40 + 2 * 16];
    flt[0x40..0x48].copy_from_slice(&7u64.to_le_bytes());
    flt[0x48..0x50].copy_from_slice(&(0xAB00_0000_0000_0003u64).to_le_bytes());
    flt[0x50..0x58].copy_from_slice(&9u64.to_le_bytes());
    flt[0x58..0x60].copy_from_slice(&4u64.to_le_bytes());
    assert!(flt_holds(&flt, 2, 7, 3));
    assert!(flt_holds(&flt, usize::MAX, 9, 4));
    assert!(!flt_holds(&flt, 1, 9, 4), "past the count");
    assert!(!flt_holds(&flt, usize::MAX, 9, 5));
    assert!(!flt_holds(&flt[..0x20], usize::MAX, 7, 3), "no records");
}

/// A package whose image counts fewer inodes than the two every image starts with is reported
/// as failing its checks. It used to index past the list and take the job down.
#[test]
fn an_image_without_its_first_inodes_is_reported_not_indexed() {
    let dir = crate::test_tmp::temp_path(format!("fpkg-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let source = dir.join("game");
    let put = |path: &str, data: &[u8]| {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    };
    put("eboot.bin", &[7u8; 4096]);
    put(
        "sce_sys/param.json",
        br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.000.000","titleId":"PPSA01234"}"#,
    );
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        ..BuildRequest::new(&source, &out)
    };
    let path = build::build(&request, &mut |_| {}).unwrap().path;
    let passcode = crate::crypto::DEFAULT_PASSCODE;
    assert!(verify_streaming(&path, passcode, &mut |_, _| {})
        .unwrap()
        .ok());

    // The superblock's inode count (+0x30), set to 2.
    let mut file = PkgFile::open(&path).unwrap();
    let head = file.read_at(0, BLOCK as usize).unwrap();
    let fih = fih::parse(&head).unwrap();
    let superblock = fih.pfs_offset + crate::le64(&head, 0x20) - BLOCK;
    drop(file);
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(superblock + 0x30)).unwrap();
    f.write_all(&2u64.to_le_bytes()).unwrap();
    drop(f);

    let report = verify_streaming(&path, passcode, &mut |_, _| {}).unwrap();
    assert!(!report.ok());
    assert!(report
        .checks
        .iter()
        .any(|c| !c.ok && c.name.contains("flat-path table and root folder")));
    std::fs::remove_dir_all(&dir).ok();
}

fn check(kind: Kind, ok: bool, count: Option<(u64, u64)>) -> Check {
    Check {
        name: format!("{} check", kind.as_str()),
        ok,
        detail: String::new(),
        kind,
        count,
    }
}

fn report(checks: Vec<Check>) -> Report {
    Report {
        content_id: String::new(),
        checks,
    }
}

/// What failed checks add up to: a cut-short file before anything else (the rest of it was
/// never read), damaged data before broken tables, and the package's own information alone
/// last. Nothing when every check holds.
#[test]
fn damage_names_the_worst_thing_the_checks_found() {
    use Kind::*;
    assert_eq!(report(vec![check(Metadata, true, None)]).damage(), None);
    assert_eq!(
        report(vec![check(Data, true, Some((0, 900)))]).damage(),
        None
    );
    // Only header information: counted, and said to be only that.
    assert_eq!(
        report(vec![
            check(Metadata, false, None),
            check(Metadata, false, None),
            check(Structure, true, None),
            check(Data, true, Some((0, 900))),
        ])
        .damage(),
        Some(Damage::Metadata { failed: 2 })
    );
    // The reported case: a digest, the blocks and the manifest's checksums all fail. It is the
    // data that is damaged, 56116 blocks of 1529208.
    assert_eq!(
        report(vec![
            check(Metadata, false, None),
            check(Data, false, Some((56_116, 1_529_208))),
            check(Structure, false, None),
            check(Metadata, false, None),
        ])
        .damage(),
        Some(Damage::Data {
            bad: 56_116,
            total: 1_529_208
        })
    );
    assert_eq!(
        report(vec![
            check(Structure, false, None),
            check(Metadata, false, None)
        ])
        .damage(),
        Some(Damage::Structure { failed: 1 })
    );
    // The verifier that holds the package checks block by block: each block is one of them.
    assert_eq!(
        report(vec![
            check(Data, true, None),
            check(Data, false, None),
            check(Data, false, None),
        ])
        .damage(),
        Some(Damage::Data { bad: 2, total: 3 })
    );
    assert_eq!(
        report(vec![
            check(Data, false, Some((1, 9))),
            check(Truncated, false, Some((100, 400))),
        ])
        .damage(),
        Some(Damage::Truncated {
            have: 100,
            need: 400
        })
    );
    // Only the end of the file is missing: no sizes to give.
    assert_eq!(
        report(vec![check(Truncated, false, None)]).damage(),
        Some(Damage::Truncated { have: 0, need: 0 })
    );
    for (kind, word) in [
        (Metadata, "metadata"),
        (Structure, "structure"),
        (Data, "data"),
        (Truncated, "truncated"),
    ] {
        assert_eq!(kind.as_str(), word);
    }
}

/// A real package through the streaming verifier: whole, with blocks of its data overwritten,
/// cut short, and with only its trailing install manifest gone.
#[test]
fn a_damaged_package_says_how_it_is_damaged() {
    let dir = crate::test_tmp::temp_path(format!("fpkg-verify-damage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let source = dir.join("game");
    let put = |path: &str, data: &[u8]| {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    };
    put("eboot.bin", &[7u8; 4096]);
    put("data/big.bin", &vec![0x5Au8; 2 << 20]);
    put(
        "sce_sys/param.json",
        br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.000.000","titleId":"PPSA01234"}"#,
    );
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        level: crate::kraken::Level::Stored,
        ..BuildRequest::new(&source, &out)
    };
    let path = build::build(&request, &mut |_| {}).unwrap().path;
    let passcode = crate::crypto::DEFAULT_PASSCODE;
    let verify = |path: &Path| verify_streaming(path, passcode, &mut |_, _| {}).unwrap();
    let good = verify(&path);
    assert!(good.ok() && good.is_debug());
    assert_eq!(good.damage(), None);
    // Every check has a kind, and the sweep counted every block.
    let whole = std::fs::read(&path).unwrap();
    let head = &whole[..BLOCK as usize];
    let fih = fih::parse(head).unwrap();
    let blocks = fih.pfs_size / BLOCK;
    let sweep = good.checks.iter().find(|c| c.kind == Kind::Data).unwrap();
    assert_eq!(sweep.count, Some((0, blocks)));

    // Three blocks overwritten.
    let damaged = dir.join("damaged.pkg");
    let mut bytes = whole.clone();
    for index in [2u64, 5, 11] {
        let at = (fih.pfs_offset + index * BLOCK) as usize + 7;
        for b in &mut bytes[at..at + 32] {
            *b ^= 0xA5;
        }
    }
    std::fs::write(&damaged, &bytes).unwrap();
    let r = verify(&damaged);
    assert!(!r.ok());
    assert_eq!(
        r.damage(),
        Some(Damage::Data {
            bad: 3,
            total: blocks
        })
    );

    // Cut short inside the image, and before the container: reported, not an error.
    for cut in [fih.pfs_offset + fih.pfs_size / 2, fih.cnt_offset] {
        let short = dir.join("short.pkg");
        std::fs::write(&short, &whole[..cut as usize]).unwrap();
        let r = verify(&short);
        assert!(!r.ok());
        let Some(Damage::Truncated { have, need }) = r.damage() else {
            panic!("{:?}", r.damage());
        };
        assert_eq!(have, cut);
        assert!(need > have && need <= whole.len() as u64, "{need}");
        assert_eq!(r.checks.len(), 1);
    }

    // Only the install manifest at the very end is gone.
    let manifest = si::read(&mut PkgFile::open(&path).unwrap())
        .unwrap()
        .expect("the trailing zip");
    let tailless = dir.join("tailless.pkg");
    std::fs::write(&tailless, &whole[..manifest.zip_start as usize]).unwrap();
    let r = verify(&tailless);
    assert_eq!(r.damage(), Some(Damage::Truncated { have: 0, need: 0 }));
    std::fs::remove_dir_all(&dir).ok();
}

/// A scratch folder that goes when the test ends, passed or not.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fpkg-verify-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A small debug package as Convert builds one: an SI ZIP with its playgo-chunk.crc.
fn built_package(dir: &Path) -> std::path::PathBuf {
    let source = dir.join("game");
    std::fs::create_dir_all(source.join("sce_sys")).unwrap();
    std::fs::write(source.join("eboot.bin"), [7u8; 4096]).unwrap();
    std::fs::write(source.join("big.bin"), vec![0x5Au8; 3 << 20]).unwrap();
    std::fs::write(
        source.join("sce_sys/param.json"),
        br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.000.000","titleId":"PPSA01234"}"#,
    )
    .unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        ..BuildRequest::new(&source, &out)
    };
    build::build(&request, &mut |_| {}).unwrap().path
}

/// #951: a full check reads the package twice, its blocks and then the PlayGo chunk CRCs. Both
/// passes count in one total, so the progress reaches its total only when both are done, and
/// the second pass reports as it goes and says it is the second.
#[test]
fn progress_counts_the_playgo_crc_pass_and_reaches_its_total_only_at_the_end() {
    let t = Scratch::new("phases");
    let path = built_package(&t.0);
    let mut file = PkgFile::open(&path).unwrap();
    let zip_start = si::read(&mut file).unwrap().expect("an SI ZIP").zip_start;
    drop(file);

    let cancel = std::sync::atomic::AtomicBool::new(false);
    // Every report, with the pass it came in.
    let events = std::cell::RefCell::new(Vec::<(Option<Phase>, u64, u64)>::new());
    let current = std::cell::Cell::new(None::<Phase>);
    let report = verify_streaming_phased(
        &path,
        crate::crypto::DEFAULT_PASSCODE,
        &cancel,
        &mut |done, total| events.borrow_mut().push((current.get(), done, total)),
        &mut |p| current.set(Some(p)),
    )
    .unwrap();
    assert!(report.ok(), "{report}");
    let events = events.into_inner();
    let (_, _, total) = *events.last().unwrap();
    assert!(events.iter().all(|e| e.2 == total), "one total throughout");
    assert!(
        events.windows(2).all(|w| w[0].1 <= w[1].1),
        "never goes back"
    );
    assert_eq!(events.last().unwrap().1, total, "done at the end");
    let blocks: Vec<_> = events
        .iter()
        .filter(|e| e.0 == Some(Phase::Blocks))
        .collect();
    let crc: Vec<_> = events
        .iter()
        .filter(|e| e.0 == Some(Phase::PlaygoCrc))
        .collect();
    assert!(!blocks.is_empty() && !crc.is_empty());
    // The block pass ends a whole CRC pass short of the total.
    assert_eq!(total - blocks.last().unwrap().1, zip_start);
    assert!(
        blocks.iter().all(|e| e.1 < total),
        "never 100% before the CRC pass"
    );
    // The CRC pass reports from where the blocks left off, more than once (every 256 blocks
    // and at its end), and reaches the total only with its last report.
    assert!(crc.len() >= 2, "{crc:?}");
    assert!(crc[..crc.len() - 1].iter().all(|e| e.1 < total));
}

/// #951: Cancel is heard during the PlayGo CRC pass, not only during the block sweep.
#[test]
fn cancel_stops_the_playgo_crc_pass() {
    let t = Scratch::new("crc-cancel");
    let path = built_package(&t.0);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let reached = std::cell::Cell::new(false);
    let err = verify_streaming_phased(
        &path,
        crate::crypto::DEFAULT_PASSCODE,
        &cancel,
        &mut |_, _| {},
        &mut |p| {
            if p == Phase::PlaygoCrc {
                reached.set(true);
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .unwrap_err();
    assert!(reached.get());
    assert!(err.to_string().contains("cancelled"), "{err}");
}

/// The table read with progress is the plain table, and says how far it got.
#[test]
fn the_crc_table_with_progress_matches_the_plain_one() {
    let t = Scratch::new("crc-table");
    let path = t.0.join("blob");
    let bytes: Vec<u8> = (0..BLOCK * 600 + 5).map(|i| (i * 13 % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let mut f = PkgFile::open(&path).unwrap();
    let zip_start = BLOCK * 600;
    let mut seen = Vec::new();
    let with = si::chunk_crc_table_with(&mut f, zip_start, None, &mut |n| seen.push(n)).unwrap();
    assert_eq!(with, si::chunk_crc_table(&mut f, zip_start).unwrap());
    assert_eq!(seen, [0, 256 * BLOCK, 512 * BLOCK, zip_start]);
}
