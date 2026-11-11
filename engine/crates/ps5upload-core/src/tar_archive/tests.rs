//! Tar and `.zst` fixtures are built here: the `tar` crate writes the archives, and each
//! codec's own encoder compresses them (zstd, flate2, lzma-rust2, bzip2). A few zstd frames
//! are written by hand where an exact header matters (a 2 GiB window, a content size).

use super::*;
use std::io::Write;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5tar-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(n);
    v
}

fn header(kind: tar::EntryType, size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(1_700_000_000);
    h
}

const PARAM: &[u8] = br#"{"titleId":"PPSA01234","contentId":"EP0000-PPSA01234_00-GAME000000000000","applicationCategoryType":0,"localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Tar Game"}}}"#;

/// The long name needs a GNU `L` header (over 100 bytes).
fn long_name() -> String {
    format!("game/{}/deep.bin", "d".repeat(120))
}

/// A tar with what real archives carry, and the files it should unpack to.
fn sample_tar() -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let mut b = tar::Builder::new(Vec::new());
    let mut want = BTreeMap::new();
    let mut dir = header(tar::EntryType::Directory, 0);
    b.append_data(&mut dir, "./", io::empty()).unwrap();
    let mut dir = header(tar::EntryType::Directory, 0);
    b.append_data(&mut dir, "game/", io::empty()).unwrap();
    let mut add = |b: &mut tar::Builder<Vec<u8>>, path: &str, data: Vec<u8>| {
        let mut h = header(tar::EntryType::Regular, data.len() as u64);
        b.append_data(&mut h, path, &data[..]).unwrap();
        want.insert(path.trim_start_matches("./").to_string(), data);
    };
    add(&mut b, "game/sce_sys/param.json", PARAM.to_vec());
    add(&mut b, "./game/a.bin", noise(1, 300_000));
    add(&mut b, &long_name(), noise(2, 70_000));
    add(&mut b, "game/empty.txt", Vec::new());
    // A PAX header naming the next file (a name no ustar field holds).
    let pax_name = format!("game/pax/{}.dat", "p".repeat(150));
    b.append_pax_extensions([("path", pax_name.as_bytes())])
        .unwrap();
    let data = noise(3, 5000);
    let mut h = header(tar::EntryType::Regular, data.len() as u64);
    h.set_path("game/pax/short.dat").unwrap();
    h.set_cksum();
    b.append(&h, &data[..]).unwrap();
    want.insert(pax_name, data);
    // Links and a FIFO: left out and counted.
    let mut l = header(tar::EntryType::Symlink, 0);
    b.append_link(&mut l, "game/link", "a.bin").unwrap();
    let mut l = header(tar::EntryType::Link, 0);
    b.append_link(&mut l, "game/hard", "game/a.bin").unwrap();
    let mut f = header(tar::EntryType::Fifo, 0);
    b.append_data(&mut f, "game/fifo", io::empty()).unwrap();
    // The same path again: the later copy is what extracting leaves.
    let newer = noise(4, 1234);
    let mut h = header(tar::EntryType::Regular, newer.len() as u64);
    b.append_data(&mut h, "game/a.bin", &newer[..]).unwrap();
    want.insert("game/a.bin".into(), newer);
    (b.into_inner().unwrap(), want)
}

fn compress(codec: Codec, data: &[u8]) -> Vec<u8> {
    match codec {
        Codec::None => data.to_vec(),
        Codec::Zstd => {
            // Streamed (no content size in the header, as `tar --zstd` writes), with a checksum.
            let mut e = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
            e.include_checksum(true).unwrap();
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        Codec::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        Codec::Xz => {
            let mut w =
                lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1))
                    .unwrap();
            w.write_all(data).unwrap();
            w.finish().unwrap()
        }
        Codec::Bzip2 => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
    }
}

const CODECS: [(Codec, &str); 5] = [
    (Codec::None, "a.tar"),
    (Codec::Zstd, "a.tar.zst"),
    (Codec::Gzip, "a.tgz"),
    (Codec::Xz, "a.tar.xz"),
    (Codec::Bzip2, "a.tbz2"),
];

#[derive(Default)]
struct Collect {
    files: BTreeMap<String, Vec<u8>>,
    cur: Option<String>,
    skipped: u64,
    skip: Vec<String>,
}

impl TarVisit for Collect {
    fn want(&mut self, _i: usize, f: &TarFile) -> bool {
        f.kept && !self.skip.contains(&f.path)
    }
    fn begin(&mut self, f: &TarFile) -> io::Result<()> {
        self.cur = Some(f.path.clone());
        self.files.insert(f.path.clone(), Vec::new());
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        let k = self.cur.clone().unwrap();
        self.files.get_mut(&k).unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn end(&mut self) -> io::Result<()> {
        self.cur = None;
        Ok(())
    }
    fn skipped(&mut self, n: u64) {
        self.skipped += n;
    }
}

fn walk_all(path: &Path, layout: &TarLayout, start: usize, skip: &[&str]) -> Collect {
    let mut c = Collect {
        skip: skip.iter().map(|s| s.to_string()).collect(),
        ..Collect::default()
    };
    tar_walk(path, layout, start, &mut c, &AtomicBool::new(false)).unwrap();
    c
}

#[test]
fn names_say_tar_or_zst_and_the_bytes_say_the_codec() {
    for (name, kind) in [
        ("x.tar", Some(StreamKind::Tar)),
        ("X.TAR.ZST", Some(StreamKind::Tar)),
        ("x.tzst", Some(StreamKind::Tar)),
        ("x.tar.gz", Some(StreamKind::Tar)),
        ("x.tgz", Some(StreamKind::Tar)),
        ("x.tar.xz", Some(StreamKind::Tar)),
        ("x.txz", Some(StreamKind::Tar)),
        ("x.tar.bz2", Some(StreamKind::Tar)),
        ("x.tbz2", Some(StreamKind::Tar)),
        ("x.tbz", Some(StreamKind::Tar)),
        ("game.exfat.zst", Some(StreamKind::Zst)),
        ("x.gz", None),
        ("x.zip", None),
        ("x.tar.7z", None),
    ] {
        assert_eq!(stream_kind(Path::new(name)), kind, "{name}");
    }
    assert_eq!(
        zst_output_name(Path::new("/a/game.exfat.zst")),
        "game.exfat"
    );
    assert_eq!(zst_output_name(Path::new("/a/G.ZST")), "G");
    assert_eq!(zst_output_name(Path::new("/a/.zst")), "unpacked");
    let (tar, _) = sample_tar();
    for (codec, _) in CODECS {
        assert_eq!(sniff_codec(&compress(codec, &tar)[..512]), codec);
    }
    // A plain tar whose first file is named like a bzip2 magic is still a tar.
    let mut b = tar::Builder::new(Vec::new());
    let mut h = header(tar::EntryType::Regular, 1);
    b.append_data(&mut h, "BZh91AY", &b"x"[..]).unwrap();
    assert_eq!(sniff_codec(&b.into_inner().unwrap()), Codec::None);
}

#[test]
fn every_codec_lists_and_unpacks_the_same_tree() {
    let d = temp_dir("codecs");
    let (tar, want) = sample_tar();
    for (codec, name) in CODECS {
        let p = d.join(name);
        std::fs::write(&p, compress(codec, &tar)).unwrap();
        let l = tar_layout(&p).unwrap();
        assert_eq!(l.codec, codec, "{name}");
        assert_eq!(l.sequential(), codec != Codec::None);
        assert_eq!((l.links, l.special), (2, 1), "{name}");
        let kept: BTreeMap<String, u64> = l.kept().map(|f| (f.path.clone(), f.size)).collect();
        let sizes: BTreeMap<String, u64> = want
            .iter()
            .map(|(k, v)| (k.clone(), v.len() as u64))
            .collect();
        assert_eq!(kept, sizes, "{name}");
        assert_eq!(
            l.files.iter().filter(|f| !f.kept).count(),
            1,
            "the replaced copy"
        );
        assert!(
            l.dirs.contains(&"game".to_string()) && l.dirs.contains(&"game/sce_sys".to_string())
        );
        assert!(
            !l.dirs.iter().any(|d| d.is_empty() || d == "."),
            "{:?}",
            l.dirs
        );
        assert_eq!(l.unpacked_total(), sizes.values().sum::<u64>());
        let (root, json) = l.param_json.clone().unwrap();
        assert_eq!((root.as_str(), &json[..]), ("game", PARAM));
        let got = walk_all(&p, &l, 0, &[]);
        assert_eq!(got.files, want, "{name}");
        let inspect = inspect_from_layout(&l);
        assert_eq!(inspect.title_id.as_deref(), Some("PPSA01234"), "{name}");
        assert_eq!(inspect.game_root.as_deref(), Some("game"));
        assert_eq!(inspect.file_count, want.len() as u64);
        assert!(inspect.entry_names_complete);
    }
}

#[test]
fn a_plain_tar_starts_at_any_file_and_skips_by_seeking() {
    let d = temp_dir("plain-start");
    let mut b = tar::Builder::new(Vec::new());
    let datas: Vec<Vec<u8>> = (0..6)
        .map(|i| noise(i, 10_000 + i as usize * 777))
        .collect();
    for (i, data) in datas.iter().enumerate() {
        let mut h = header(tar::EntryType::Regular, data.len() as u64);
        // Every other one with a long name, so a start lands on an `L` header too.
        let name = if i % 2 == 0 {
            format!("f{i}")
        } else {
            format!("{}/f{i}", "n".repeat(110))
        };
        b.append_data(&mut h, &name, &data[..]).unwrap();
    }
    let p = d.join("s.tar");
    std::fs::write(&p, b.into_inner().unwrap()).unwrap();
    let l = tar_layout(&p).unwrap();
    for start in 0..6 {
        let got = walk_all(&p, &l, start, &[]);
        assert_eq!(got.files.len(), 6 - start, "start {start}");
        for (i, data) in datas.iter().enumerate().skip(start) {
            assert_eq!(
                &got.files[&l.files[i].path], data,
                "start {start}, file {i}"
            );
        }
    }
    // The data offsets are the file's own bytes in the archive.
    let raw = std::fs::read(&p).unwrap();
    for (f, data) in l.files.iter().zip(&datas) {
        let at = f.data_offset as usize;
        assert_eq!(&raw[at..at + data.len()], &data[..]);
    }
    // A skipped file of a plain tar is passed by a seek, never decoded.
    let got = walk_all(&p, &l, 0, &[&l.files[2].path.clone()]);
    assert_eq!((got.files.len(), got.skipped), (5, 0));
}

#[test]
fn a_compressed_tar_decodes_and_counts_what_it_skips() {
    let d = temp_dir("seq-skip");
    let (tar, want) = sample_tar();
    let p = d.join("a.tar.zst");
    std::fs::write(&p, compress(Codec::Zstd, &tar)).unwrap();
    let l = tar_layout(&p).unwrap();
    let got = walk_all(&p, &l, 0, &["game/sce_sys/param.json"]);
    assert_eq!(got.files.len(), want.len() - 1);
    // The skipped param.json and the replaced copy of a.bin are decoded and dropped.
    assert_eq!(got.skipped, PARAM.len() as u64 + 300_000);
}

fn tar_with(name: &str) -> Vec<u8> {
    // Header::set_path refuses `..`, so the name goes into the raw field.
    let mut h = header(tar::EntryType::Regular, 1);
    let bytes = name.as_bytes();
    h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
    h.set_cksum();
    let mut b = tar::Builder::new(Vec::new());
    b.append(&h, &b"x"[..]).unwrap();
    b.into_inner().unwrap()
}

#[test]
fn traversal_and_backslash_names_are_refused_a_leading_slash_is_dropped() {
    let d = temp_dir("unsafe");
    for (i, bad) in ["../evil", "a/../../evil", "a\\b"].iter().enumerate() {
        let p = d.join(format!("bad{i}.tar"));
        std::fs::write(&p, tar_with(bad)).unwrap();
        let e = tar_layout(&p).unwrap_err();
        assert!(
            matches!(tar_fault_of(&e), Some(TarFault::UnsafePath(_))),
            "{bad}: {e}"
        );
    }
    let p = d.join("abs.tar");
    std::fs::write(&p, tar_with("/etc/x")).unwrap();
    assert_eq!(tar_layout(&p).unwrap().files[0].path, "etc/x");
}

#[test]
fn a_sparse_file_and_case_clashes_are_refused_with_what_to_do() {
    let d = temp_dir("refuse");
    let mut b = tar::Builder::new(Vec::new());
    let mut h = header(tar::EntryType::GNUSparse, 0);
    h.as_gnu_mut().unwrap().set_real_size(0);
    b.append_data(&mut h, "disk.img", io::empty()).unwrap();
    let p = d.join("sparse.tar");
    std::fs::write(&p, b.into_inner().unwrap()).unwrap();
    let e = tar_layout(&p).unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::Unsupported(m)) if m.contains("--sparse")),
        "{e}"
    );

    let mut b = tar::Builder::new(Vec::new());
    for n in ["G/a.bin", "g/A.bin"] {
        let mut h = header(tar::EntryType::Regular, 1);
        b.append_data(&mut h, n, &b"x"[..]).unwrap();
    }
    let p = d.join("case.tar");
    std::fs::write(&p, b.into_inner().unwrap()).unwrap();
    let e = tar_layout(&p).unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::Unsupported(m)) if m.contains("case")),
        "{e}"
    );
}

/// A frame header by hand: `fhd` then the window descriptor (when not single segment), then the
/// content size bytes, then one raw last block of `data`, then `checksum`.
fn frame(fhd: u8, window: Option<u8>, fcs: &[u8], data: &[u8], checksum: Option<u32>) -> Vec<u8> {
    let mut v = vec![0x28, 0xB5, 0x2F, 0xFD, fhd];
    v.extend(window);
    v.extend_from_slice(fcs);
    let h = 1 | ((data.len() as u32) << 3); // last, raw
    v.extend_from_slice(&h.to_le_bytes()[..3]);
    v.extend_from_slice(data);
    if let Some(c) = checksum {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

#[test]
fn a_huge_zstd_window_is_refused_before_it_is_allocated_and_named() {
    // Window descriptor (31 - 10) << 3: window log 31, what `zstd --long=31` writes.
    let big = frame(0x00, Some(21 << 3), &[], b"x", None);
    let mut r = ZstdReader::with_max_window(&big[..], ZSTD_WINDOW_FLOOR);
    let e = r.read(&mut [0u8; 16]).unwrap_err();
    match tar_fault_of(&e) {
        Some(TarFault::TooMuchMemory(m)) => {
            assert!(m.contains("2 GiB") && m.contains("128 MiB"), "{m}");
            assert!(m.contains("PS5UPLOAD_ZSTD_MAX_WINDOW_MB=2048"), "{m}");
        }
        other => panic!("{other:?}: {e}"),
    }
    // Under the cap it decodes (a 64 MiB window, window log 26: no 2 GiB allocation in a test).
    let fits = frame(0x00, Some(16 << 3), &[], b"x", None);
    let mut out = Vec::new();
    ZstdReader::with_max_window(&fits[..], ZSTD_WINDOW_FLOOR)
        .read_to_end(&mut out)
        .unwrap();
    assert_eq!(out, b"x");
    // zstd's own windowLogMax is the second guard: with the header check out of the way (a
    // cap raised past what the decoder was told), zstd refuses the frame itself, and its
    // "Frame requires too much memory for decoding" becomes the same memory fault.
    let mut r = ZstdReader::with_max_window(&big[..], ZSTD_WINDOW_FLOOR);
    r.max_window = u64::MAX;
    let e = r.read_to_end(&mut Vec::new()).unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::TooMuchMemory(m)) if m.contains("--long")),
        "{e}"
    );
    // Exactly the cap is allowed; one step over is not.
    let at_cap = frame(0x00, Some(17 << 3), &[], b"x", None); // 128 MiB
    let mut out = Vec::new();
    ZstdReader::with_max_window(&at_cap[..], ZSTD_WINDOW_FLOOR)
        .read_to_end(&mut out)
        .unwrap();
    let over = frame(0x00, Some((17 << 3) | 1), &[], b"x", None); // 144 MiB
    let e = ZstdReader::with_max_window(&over[..], ZSTD_WINDOW_FLOOR)
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::TooMuchMemory(m)) if m.contains("144 MiB")),
        "{e}"
    );
    // Through the tar path the same refusal reaches the caller.
    let d = temp_dir("window");
    let p = d.join("w.tar.zst");
    std::fs::write(&p, &big).unwrap();
    if zstd_max_window() < ZSTD_WINDOW_CEIL {
        let e = tar_layout(&p).unwrap_err();
        assert!(
            matches!(tar_fault_of(&e), Some(TarFault::TooMuchMemory(_))),
            "{e}"
        );
    }
}

#[test]
fn the_window_cap_follows_memory_and_the_override() {
    const G: u64 = 1 << 30;
    assert_eq!(select_max_window(None, None, false), ZSTD_WINDOW_FLOOR);
    assert_eq!(select_max_window(None, Some(16 * G), false), 2 * G);
    assert_eq!(select_max_window(None, Some(8 * G), false), G);
    assert_eq!(
        select_max_window(None, Some(512 << 20), false),
        ZSTD_WINDOW_FLOOR
    );
    assert_eq!(select_max_window(None, Some(64 * G), false), 2 * G);
    assert_eq!(select_max_window(None, Some(12 * G), true), 512 << 20);
    assert_eq!(select_max_window(Some("4096"), Some(G), false), 2 * G);
    assert_eq!(
        select_max_window(Some("256"), Some(64 * G), false),
        256 << 20
    );
    assert_eq!(select_max_window(Some("junk"), Some(16 * G), false), 2 * G);
}

#[test]
fn zstd_frames_follow_one_another_and_skippable_frames_are_passed() {
    let a = compress(Codec::Zstd, &noise(5, 100_000));
    let b = compress(Codec::Zstd, b"tail");
    let mut all = a.clone();
    all.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 9, 9, 9]); // skippable, 3 bytes
    all.extend_from_slice(&b);
    let mut out = Vec::new();
    ZstdReader::new(&all[..]).read_to_end(&mut out).unwrap();
    let mut want = noise(5, 100_000);
    want.extend_from_slice(b"tail");
    assert_eq!(out, want);
    // A damaged checksum (the frame's last 4 bytes) is caught.
    let mut bad = a.clone();
    let n = bad.len();
    bad[n - 1] ^= 0xFF;
    let e = ZstdReader::new(&bad[..])
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::Corrupt(m)) if m.contains("checksum")),
        "{e}"
    );
    // A stream cut short is corrupt, not a clean end.
    let e = ZstdReader::new(&a[..a.len() / 2])
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::Corrupt(_))),
        "{e}"
    );
}

#[test]
fn a_lone_zst_is_its_one_file_sized_from_the_headers_or_by_decoding() {
    let d = temp_dir("single");
    // Single segment, one-byte content size (FHD 0x20), plus a second frame with a 2-byte size
    // (FHD 0x60 with window descriptor absent as single segment: 0x60) of 300 bytes.
    let mut f = frame(0x20, None, &[5], b"hello", None);
    let body = noise(6, 300);
    f.extend(frame(
        0x60,
        None,
        &((300 - 256) as u16).to_le_bytes(),
        &body,
        None,
    ));
    let p = d.join("save.bin.zst");
    std::fs::write(&p, &f).unwrap();
    assert_eq!(zst_content_size(&p).unwrap(), Some(305));
    let l = tar_layout(&p).unwrap();
    assert!(l.single && l.files.len() == 1);
    assert_eq!(
        (l.files[0].path.as_str(), l.files[0].size),
        ("save.bin", 305)
    );
    let got = walk_all(&p, &l, 0, &[]);
    let mut want = b"hello".to_vec();
    want.extend_from_slice(&body);
    assert_eq!(got.files["save.bin"], want);
    // A streamed zstd encode writes no content size: the file is decoded once to learn it.
    let data = noise(7, 200_000);
    let p = d.join("g.exfat.zst");
    std::fs::write(&p, compress(Codec::Zstd, &data)).unwrap();
    assert_eq!(zst_content_size(&p).unwrap(), None);
    let l = tar_layout(&p).unwrap();
    assert_eq!(
        (l.files[0].path.as_str(), l.files[0].size),
        ("g.exfat", 200_000)
    );
    assert_eq!(walk_all(&p, &l, 0, &[]).files["g.exfat"], data);
    // Not zstd at all.
    let p = d.join("fake.zst");
    std::fs::write(&p, b"plain text").unwrap();
    assert!(matches!(
        tar_fault_of(&tar_layout(&p).unwrap_err()),
        Some(TarFault::Corrupt(_))
    ));
}

#[test]
fn a_truncated_archive_is_corrupt_for_every_codec() {
    let d = temp_dir("truncated");
    let (tar, _) = sample_tar();
    for (codec, name) in CODECS {
        let full = compress(codec, &tar);
        // Cut inside a.bin's data (the plain tar's is at ~2 KiB; compressed: half way).
        let cut = if codec == Codec::None {
            4096
        } else {
            full.len() / 2
        };
        let p = d.join(name);
        std::fs::write(&p, &full[..cut]).unwrap();
        let e = tar_layout(&p).unwrap_err();
        assert!(
            matches!(tar_fault_of(&e), Some(TarFault::Corrupt(_))),
            "{name}: {e} ({:?})",
            e.kind()
        );
    }
}

#[test]
fn the_listing_is_cached_until_the_file_changes() {
    let d = temp_dir("cache");
    let (tar, _) = sample_tar();
    let p = d.join("c.tar.gz");
    std::fs::write(&p, compress(Codec::Gzip, &tar)).unwrap();
    let a = tar_layout(&p).unwrap();
    assert!(Arc::ptr_eq(&a, &tar_layout(&p).unwrap()));
    assert!(Arc::ptr_eq(&a, &cached_layout(&p).unwrap()));
    let mut b = tar::Builder::new(Vec::new());
    let mut h = header(tar::EntryType::Regular, 2);
    b.append_data(&mut h, "other", &b"yz"[..]).unwrap();
    std::fs::write(&p, compress(Codec::Gzip, &b.into_inner().unwrap())).unwrap();
    let c = tar_layout(&p).unwrap();
    assert_eq!(c.files.len(), 1);
    assert_ne!(a.identity, c.identity);
}

#[test]
fn a_listing_stops_when_cancelled_and_reports_progress() {
    let d = temp_dir("cancel");
    let (tar, _) = sample_tar();
    let p = d.join("x.tar.xz");
    std::fs::write(&p, compress(Codec::Xz, &tar)).unwrap();
    let cancel = AtomicBool::new(true);
    let e = tar_layout_watched(
        &p,
        TarWatch {
            cancel: Some(&cancel),
            ..TarWatch::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.to_string(), TAR_LISTING_CANCELLED);
    assert!(cached_layout(&p).is_none());
    let live = LiveNotes::default();
    tar_layout_watched(
        &p,
        TarWatch {
            live: Some(&live),
            ..TarWatch::default()
        },
    )
    .unwrap();
    // Back to "sending" once listed.
    assert_eq!(live.phase.load(Ordering::Relaxed), 0);
}

#[test]
fn a_walk_over_a_changed_archive_fails_instead_of_misnaming_bytes() {
    let d = temp_dir("drift");
    let mut b = tar::Builder::new(Vec::new());
    for (n, data) in [("a", b"aaaa"), ("b", b"bbbb")] {
        let mut h = header(tar::EntryType::Regular, 4);
        b.append_data(&mut h, n, &data[..]).unwrap();
    }
    let p = d.join("d.tar.gz");
    std::fs::write(&p, compress(Codec::Gzip, &b.into_inner().unwrap())).unwrap();
    let mut l = (*tar_layout(&p).unwrap()).clone();
    l.files[1].path = "z".into();
    let mut c = Collect::default();
    let e = tar_walk(&p, &l, 0, &mut c, &AtomicBool::new(false)).unwrap_err();
    assert!(
        matches!(tar_fault_of(&e), Some(TarFault::Corrupt(m)) if m.contains("changed")),
        "{e}"
    );
    // A cancel ends a walk at once.
    let l = tar_layout(&p).unwrap();
    let e = tar_walk(&p, &l, 0, &mut Collect::default(), &AtomicBool::new(true)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::Interrupted);
}

/// A zip of zstd (method 93) entries, by hand: this build's `zip` crate cannot write them.
fn zstd_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let comp = compress(Codec::Zstd, data);
        let crc = crc32fast::hash(data);
        let off = out.len() as u32;
        let common = |v: &mut Vec<u8>| {
            for x in [63u16, 0x0800, 93, 0, 0x21] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            for x in [crc, comp.len() as u32, data.len() as u32] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x033Fu16.to_le_bytes());
        common(&mut central);
        central.extend_from_slice(&[0; 6]);
        central.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Convert's unpack and the Upload card's preview read zstd zip entries and tars too.
#[test]
fn convert_unpacks_tars_and_zstd_zips_and_the_zip_preview_reads_a_zstd_param_json() {
    use crate::archive_extract::{archive_kind, extract, find_game, ArchiveKind};
    let d = temp_dir("convert");
    let (tar, want) = sample_tar();
    let p = d.join("game.tar.zst");
    std::fs::write(&p, compress(Codec::Zstd, &tar)).unwrap();
    assert_eq!(archive_kind(&p), Some(ArchiveKind::Tar));
    let out = d.join("out");
    let mut last = (0, 0);
    extract(
        &p,
        &out,
        None,
        &mut |a, b| last = (a, b),
        &AtomicBool::new(false),
    )
    .unwrap();
    for (k, v) in &want {
        assert_eq!(&std::fs::read(out.join(k)).unwrap(), v, "{k}");
    }
    assert_eq!(last.0, want.values().map(|v| v.len() as u64).sum::<u64>());
    assert_eq!(find_game(&out).unwrap(), out.join("game"));

    let big = noise(9, 200_000);
    let z = d.join("z.zip");
    std::fs::write(
        &z,
        zstd_zip(&[("g/sce_sys/param.json", PARAM), ("g/big.bin", &big)]),
    )
    .unwrap();
    let out = d.join("zout");
    extract(&z, &out, None, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(std::fs::read(out.join("g/big.bin")).unwrap(), big);
    let inspect = crate::transfer::inspect_zip(&z).unwrap();
    assert_eq!(inspect.title_id.as_deref(), Some("PPSA01234"));
    assert_eq!(inspect.game_root.as_deref(), Some("g"));
    // A damaged entry fails its CRC-32.
    let mut bad = zstd_zip(&[("a.bin", &big)]);
    let n = bad.len();
    bad[n - 22 - 46 - 5 - 10] ^= 1; // inside the compressed data, before the central directory
    std::fs::write(&z, &bad).unwrap();
    assert!(extract(
        &z,
        &d.join("bad"),
        None,
        &mut |_, _| {},
        &AtomicBool::new(false)
    )
    .is_err());
}

/// Decode speed of real archives: the listing, then a full walk.
/// `PS5UPLOAD_TAR_BENCH=a.tar.zst:b.tgz cargo test --release -p ps5upload-core --lib tar_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn tar_bench() {
    struct Count(u64);
    impl TarVisit for Count {
        fn want(&mut self, _: usize, f: &TarFile) -> bool {
            f.kept
        }
        fn begin(&mut self, _: &TarFile) -> io::Result<()> {
            Ok(())
        }
        fn data(&mut self, b: &[u8]) -> io::Result<()> {
            self.0 += b.len() as u64;
            Ok(())
        }
        fn end(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let paths = std::env::var("PS5UPLOAD_TAR_BENCH").unwrap_or_default();
    for p in paths.split(':').filter(|p| !p.is_empty()) {
        let p = Path::new(p);
        let t = std::time::Instant::now();
        let l = tar_layout(p).unwrap();
        let list_s = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let mut c = Count(0);
        tar_walk(p, &l, 0, &mut c, &AtomicBool::new(false)).unwrap();
        let walk_s = t.elapsed().as_secs_f64();
        let mib = c.0 as f64 / (1 << 20) as f64;
        println!(
            "{}: {} files, {mib:.0} MiB; list {list_s:.2}s ({:.0} MiB/s), walk {walk_s:.2}s ({:.0} MiB/s)",
            p.display(),
            l.files.len(),
            mib / list_s,
            mib / walk_s
        );
    }
}
