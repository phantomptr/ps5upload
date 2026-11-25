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
    let dir = std::env::temp_dir().join(format!("fpkg-verify-{}", std::process::id()));
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
