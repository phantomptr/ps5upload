#![cfg(unix)]
use ava1::manifest::{walk, Entry, Manifest};
use ava1::source::LocalSource;
use ava1::wire::Message;
use ava1_ctest::*;

fn e(path: &str, kind: u8, size: u64) -> Entry {
    Entry {
        kind,
        mode: 0o644,
        size,
        mtime: 1_700_000_000,
        path: path.into(),
        root: None,
    }
}

#[test]
fn c_rebuilds_rust_pages_with_the_same_hash() {
    let entries: Vec<Entry> = (0..3000)
        .map(|i| {
            e(
                &format!("d{}/f{i:05}{}", i % 7, "x".repeat(i % 200)),
                ava1::gen::ENTRY_FILE,
                i as u64 * 3,
            )
        })
        .collect();
    let m = Manifest { entries };
    let pages: Vec<Vec<u8>> = m
        .pages([5; 16])
        .iter()
        .map(|p| p.to_bytes().unwrap())
        .collect();
    let (rc, hash, n, bytes) = c_mstore_from_pages(&pages);
    assert_eq!(rc, 0);
    assert_eq!(
        (hash, n, bytes),
        (m.hash(), m.entries.len() as u32, m.bytes())
    );
}

#[test]
fn c_manifest_refuses_escaping_paths() {
    for bad in ["../x", "/abs", "a/../b", "a//b", "a/./b", "a/", ""] {
        let m = Manifest {
            entries: vec![e("ok", 0, 1), e(bad, 0, 1)],
        };
        // Encode by hand: Manifest::pages would not refuse, the receiver must.
        let pages: Vec<Vec<u8>> = m
            .pages([1; 16])
            .iter()
            .map(|p| p.to_bytes().unwrap())
            .collect();
        let (rc, ..) = c_mstore_from_pages(&pages);
        assert_eq!(rc, -20, "{bad:?} must be refused with AVA1_E_BADPATH");
    }
}

#[test]
fn c_and_rust_walk_a_tree_identically() {
    let d = TempDir::new(format!("ava1-cwalk-{}", std::process::id()));
    for p in ["a/b/c", "a2", "a/b2", "z"] {
        std::fs::create_dir_all(d.join(p)).unwrap();
    }
    for (p, n) in [
        ("a/b/c/f1", 10),
        ("a/x", 0),
        ("a2/y", 3),
        ("top", 7),
        ("a/b2/é", 1),
    ] {
        std::fs::write(d.join(p), vec![1u8; n]).unwrap();
    }
    let m = walk(&LocalSource::new(d.to_path_buf()), &|_: &str| false).unwrap();
    let (rc, hash, n) = c_mstore_walk(&d);
    assert_eq!(rc, 0);
    assert_eq!((hash, n), (m.hash(), m.entries.len() as u32));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn c_follow_and_rust_agree_on_a_tree_with_a_directory_symlink() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new(format!("ava1-cwalk-sym-{}", std::process::id()));
    std::fs::create_dir_all(d.join("a2")).unwrap();
    std::fs::write(d.join("a2/f"), b"abc").unwrap();
    std::fs::write(d.join("top"), b"x").unwrap();
    symlink(d.join("a2"), d.join("a")).unwrap();
    // Rust follows the link: a (dir), a/f, a2, a2/f, top.
    let m = walk(&LocalSource::new(d.to_path_buf()), &|_: &str| false).unwrap();
    assert_eq!(m.entries.len(), 5);
    // AVA1_WALK_FOLLOW (the download sender's mode) is the parity contract.
    let (rc, hash, n) = c_mstore_walk_ex(&d, 1);
    assert_eq!(rc, 0);
    assert_eq!((hash, n), (m.hash(), m.entries.len() as u32));
    // The default mode skips the directory symlink (and its subtree): the copy's mode.
    let (rc0, _, n0) = c_mstore_walk(&d);
    assert_eq!((rc0, n0), (0, 3));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_dangling_symlink_fails_both_c_modes_and_rust() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new(format!("ava1-cwalk-dangle-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    symlink(d.join("gone"), d.join("dead")).unwrap();
    // Rust's walk fails on a dangling link; the C walk must too, in both modes (C1).
    assert!(walk(&LocalSource::new(d.to_path_buf()), &|_: &str| false).is_err());
    let (rc, ..) = c_mstore_walk(&d);
    assert_ne!(
        rc, 0,
        "the default mode must not silently drop a dangling link"
    );
    let (rc2, ..) = c_mstore_walk_ex(&d, 1);
    assert_ne!(
        rc2, 0,
        "the follow mode must not silently drop a dangling link"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_symlink_cycle_fails_the_c_follow_walk_and_rust() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new(format!("ava1-cwalk-cycle-{}", std::process::id()));
    std::fs::create_dir_all(d.join("sub")).unwrap();
    symlink(&d, d.join("sub/loop")).unwrap();
    assert!(walk(&LocalSource::new(d.to_path_buf()), &|_: &str| false).is_err());
    // ELOOP or a path-length error — an error, never a spin (C1).
    let (rc, ..) = c_mstore_walk_ex(&d, 1);
    assert_ne!(rc, 0, "a cycle must end the follow walk, never spin");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn c_threads_start_with_their_own_stack() {
    // Not a smoke test: the C side reports the stack it actually ran on and fails unless
    // it is within [200 KiB, 256 KiB + 4 KiB] (64 KiB of ceiling on macOS, whose pthreads
    // pad the reported allocation) — the default pthread stack (512 KiB on macOS, 8 MiB on Linux) must not pass. See ruling 4.
    assert_eq!(c_thread_smoke().0, 0);
}

const FILE: u8 = ava1::gen::ENTRY_FILE;
const DIR: u8 = ava1::gen::ENTRY_DIR;

#[test]
fn c_path_rules_match_the_spec() {
    assert!(c_path_ok(b"a/b.txt"));
    assert!(c_path_ok("é/ü".as_bytes()));
    assert!(!c_path_ok(b"a\0b"), "NUL");
    assert!(c_path_ok(&[b'a'; 1024]));
    assert!(!c_path_ok(&[b'a'; 1025]), "1025 bytes");
    assert!(!c_path_ok(b"."), "bare dot");
    assert!(!c_path_ok(b".."), "bare dotdot");
    assert!(!c_path_ok(b""));
    assert!(!c_path_ok(&[0xff, 0xfe]), "not UTF-8");
}

#[test]
fn c_store_add_checks_ids_kinds_and_sizes() {
    let gap = c_add_one(5, FILE, 1, b"x", 0);
    assert_eq!(gap.rc, -11, "file_id gap is AVA1_E_PROTO");
    let kind = c_add_one(1, 7, 1, b"x", 0);
    assert_eq!(kind.rc, -11, "bad kind is AVA1_E_PROTO");
    let bad = c_add_one(1, FILE, 1, b"../x", 0);
    assert_eq!(bad.rc, -20);
    let dir = c_add_one(1, DIR, 99, b"d", 4);
    assert_eq!(
        (dir.rc, dir.stored_size, dir.bytes),
        (0, 0, 4),
        "dir size zeroed"
    );
    let ok = c_add_one(1, FILE, 7, b"f", 4);
    assert_eq!((ok.rc, ok.stored_size, ok.bytes), (0, 7, 11));
    assert!(ok.path_bounds_ok, "path(id >= n) is NULL, path(0) is not");
}

#[test]
fn c_store_refuses_a_total_that_overflows() {
    let r = c_add_one(1, FILE, 1, b"f", u64::MAX);
    assert_eq!(r.rc, -11);
    let ok = c_add_one(1, FILE, 1, b"f", u64::MAX - 1);
    assert_eq!((ok.rc, ok.bytes), (0, u64::MAX));
}

#[test]
fn c_store_is_capped_and_entries_stay_small() {
    assert!(
        c_ment_size() <= 32,
        "sizeof(ava1_ment_t) = {}",
        c_ment_size()
    );
    let (reserve, add, cap) = c_mstore_cap();
    assert_eq!(reserve, -11, "reserve past AVA1_MAX_ENTRIES");
    assert_eq!(add, -11, "add past AVA1_MAX_ENTRIES");
    assert_eq!(
        cap, 223_000,
        "reserve allocates exactly what it was asked for"
    );
}

fn rooted_manifest() -> Manifest {
    let mut entries: Vec<Entry> = (0..500)
        .map(|i| e(&format!("d{}/f{i}", i % 3), FILE, i as u64 + 1))
        .collect();
    for (i, en) in entries.iter_mut().enumerate() {
        if i % 4 == 0 {
            en.root = Some([i as u8 ^ 0x5a; 32]);
        }
    }
    entries.insert(0, e("dir", DIR, 0));
    Manifest { entries }
}

#[test]
fn c_blob_and_pages_round_trip_with_roots_and_decode_in_rust() {
    let m = rooted_manifest();
    let pages: Vec<Vec<u8>> = m
        .pages([5; 16])
        .iter()
        .map(|p| p.to_bytes().unwrap())
        .collect();
    let r = c_mstore_roundtrip(&pages);
    assert_eq!(r.rc, 0);
    assert_eq!(r.nroots, 125);
    assert_eq!((r.hash, r.hash2), (m.hash(), m.hash()));

    // C pages -> Rust from_pages: same entries (roots included), same hash.
    let rp: Vec<ava1::gen::ManifestPage> = r
        .pages
        .iter()
        .map(|b| ava1::gen::ManifestPage::decode(b).unwrap())
        .collect();
    let back = Manifest::from_pages(rp).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.hash(), m.hash());

    // C blob -> Rust: the blob is page.entries; a one-page wrapper decodes it.
    let mut body = (r.blob.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&r.blob);
    let entries = ava1::wire::Reader::new(&body)
        .records::<ava1::gen::ManifestEntry>()
        .unwrap();
    let page = ava1::gen::ManifestPage {
        job_id: [0; 16],
        entries,
    };
    let from_blob = Manifest::from_pages([page]).unwrap();
    assert_eq!(from_blob.hash(), m.hash());
    assert_eq!(from_blob, m);
}

#[test]
fn c_page_keeps_next_when_the_page_does_not_fit() {
    let (rc_small, next_small, rc_big, next_big) = c_page_next();
    assert_ne!(rc_small, 0);
    assert_eq!(next_small, 0, "*next committed on a failed encode");
    assert_eq!((rc_big, next_big), (0, 3));
}

#[test]
fn c_data_config_is_clamped_and_start_is_not_repeated() {
    let (c, second, first) = c_data_clamp(0, 0, 0);
    assert_eq!((c, first), ([4, 2, 16], 0));
    assert_ne!(
        second, 0,
        "a second start must not spawn another housekeeping thread"
    );
    let (c, ..) = c_data_clamp(200, 100, 250);
    assert_eq!(c, [16, 16, 16], "max clamps to the 16-slot worker array");
    let (c, ..) = c_data_clamp(9, 6, 3);
    assert_eq!(c, [3, 3, 3], "min <= start <= max");
    let (c, ..) = c_data_clamp(1, 5, 8);
    assert_eq!(c, [5, 5, 8], "start below min rises to min");
}
