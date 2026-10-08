//! Tree → `.ffpfs` → `PfsSource`, and tree → inner image → `.ffpfsc` → `open_ffpfsc`, comparing
//! paths, sizes and bytes; the on-disk fields MkPFS lays out; name and limit refusals; and a
//! reader fed damaged images.

use std::fs::File;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ps5_dump_forge_pfs::{
    BLOCK, Layout, Options, PfsSource, Stream, WrapOptions, container_size_max, open_ffpfsc, plan,
    wrap, write,
};
use ps5upload_fpkg::source::{SourceFile, SourceTree};
use ps5upload_fpkg::{Error, Result};

const TIME: i64 = 1_700_000_000;
const B: usize = BLOCK as usize;

/// A fresh directory under the system temp dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ps5-dump-forge-pfs-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Bytes that differ per file and per offset, so a misplaced block shows up.
fn pattern(path: &str, offset: u64, len: usize) -> Vec<u8> {
    let seed = path
        .bytes()
        .fold(0x9E37_79B9u32, |h, b| h.rotate_left(5) ^ u32::from(b));
    (0..len as u64)
        .map(|i| {
            let x = (offset + i).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(seed);
            (x >> 29) as u8
        })
        .collect()
}

/// A tree held in memory: names no macOS volume holds side by side, and huge sizes.
struct MemTree {
    files: Vec<SourceFile>,
    empty: Vec<String>,
}

impl MemTree {
    fn new(files: &[(&str, u64)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|&(p, size)| SourceFile {
                    path: p.to_string(),
                    size,
                })
                .collect(),
            empty: Vec::new(),
        }
    }
}

impl SourceTree for MemTree {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self.files.iter().find(|f| f.path == path).unwrap().size;
        self.read_range(path, 0, size as usize)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let size = self.files.iter().find(|f| f.path == path).unwrap().size;
        let len = len.min(size.saturating_sub(offset) as usize);
        Ok(pattern(path, offset, len))
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty
    }

    fn describe(&self) -> String {
        "memory".into()
    }
}

fn opts() -> Options {
    Options { time: Some(TIME) }
}

/// Plans and writes `tree` into memory.
fn build(tree: &mut MemTree) -> (Layout, Vec<u8>) {
    let cancel = AtomicBool::new(false);
    let layout = plan(tree, &opts(), &cancel).unwrap();
    let mut out = Cursor::new(Vec::new());
    let mut last = (0, 0);
    let report = write(tree, &layout, &mut out, &cancel, &mut |d, t| last = (d, t)).unwrap();
    let total: u64 = tree.files.iter().map(|f| f.size).sum();
    assert_eq!(last, (total, total), "progress ends at the file-data total");
    assert_eq!(report.image_size, layout.image_size);
    assert_eq!(report.files, tree.files.len() as u64);
    let img = out.into_inner();
    assert_eq!(img.len() as u64, layout.image_size);
    (layout, img)
}

/// Every file and empty directory of `want` is in `got`, with the same bytes.
fn assert_same(want: &mut dyn SourceTree, got: &mut dyn SourceTree) {
    let mut a: Vec<SourceFile> = want.files().to_vec();
    let mut b: Vec<SourceFile> = got.files().to_vec();
    a.sort_by(|x, y| x.path.cmp(&y.path));
    b.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(a, b);
    let mut e1 = want.empty_dirs().to_vec();
    let mut e2 = got.empty_dirs().to_vec();
    e1.sort();
    e2.sort();
    assert_eq!(e1, e2);
    for f in &a {
        let x = want.read(&f.path).unwrap();
        assert_eq!(x, got.read(&f.path).unwrap(), "{}", f.path);
        if f.size > 3 {
            let part = got.read_range(&f.path, 1, f.size as usize - 2).unwrap();
            assert_eq!(part, &x[1..x.len() - 1], "{} in part", f.path);
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    u32_at(b, at) as i32
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Inode `n`'s bytes (D32, 390 to a block).
fn inode(img: &[u8], n: usize) -> &[u8] {
    let at = B * (1 + n / 390) + (n % 390) * 0xA8;
    &img[at..at + 0xA8]
}

/// `(inode, type, name)` of the entries at `at`, up to the first empty record.
fn dirents(img: &[u8], mut at: usize) -> Vec<(u32, i32, String)> {
    let mut out = Vec::new();
    while u32_at(img, at + 12) != 0 {
        let len = u32_at(img, at + 8) as usize;
        let name = String::from_utf8(img[at + 16..at + 16 + len].to_vec()).unwrap();
        out.push((u32_at(img, at), i32_at(img, at + 4), name));
        at += u32_at(img, at + 12) as usize;
    }
    out
}

fn fpt_hash(path: &str) -> u32 {
    path.bytes().fold(0u32, |h, c| {
        h.wrapping_mul(31)
            .wrapping_add(u32::from(c.to_ascii_uppercase()))
    })
}

fn read_back(img: Vec<u8>) -> PfsSource {
    PfsSource::from_reader(Box::new(Cursor::new(img)), "test".into()).unwrap()
}

#[test]
fn tree_round_trips() {
    let mut files: Vec<(String, u64)> = vec![
        ("eboot.bin".into(), 3 * BLOCK + 17),
        ("sce_sys/param.json".into(), 812),
        ("sce_sys/icon0.png".into(), 70_000),
        ("data/empty.bin".into(), 0),
        ("data/deep/er/x.dat".into(), 5),
        ("Data-B/exact.bin".into(), BLOCK),
    ];
    // Over 390 inodes (a second inode-table block) and over 64 KiB of entries in one
    // directory (a multi-block directory).
    for i in 0..1200 {
        files.push((
            format!("many/a_rather_long_file_name_number_{i:05}.bin"),
            i % 3,
        ));
    }
    let entries: usize = files
        .iter()
        .filter_map(|(p, _)| p.strip_prefix("many/"))
        .map(|n| (n.len() + 17).next_multiple_of(8))
        .sum();
    assert!(entries > B, "the directory spans blocks");
    let refs: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let mut tree = MemTree::new(&refs);
    tree.empty = vec!["data/shaders".into(), "x/y/z".into()];
    let (layout, img) = build(&mut tree);
    assert_eq!(layout.files, files.len() as u64);
    // data, data/deep, data/deep/er, data/shaders, Data-B, many, sce_sys, x, x/y, x/y/z
    assert_eq!(layout.dirs, 10);
    let mut back = read_back(img);
    let h = back.header().clone();
    assert_eq!((h.version, h.mode, h.block_size), (2, 0x8, 0x10000));
    assert_eq!(h.inodes, 3 + 10 + files.len() as u64);
    assert_eq!(h.inode_blocks, 4);
    assert_eq!(h.ndblock * BLOCK, h.image_len);
    assert_same(&mut tree, &mut back);
}

#[test]
fn on_disk_fields_follow_mkpfs() {
    let mut tree = MemTree::new(&[
        ("sce_sys/param.json", 100),
        ("eboot.bin", 2 * BLOCK + 1),
        ("empty", 0),
    ]);
    let (layout, img) = build(&mut tree);
    // Blocks: 0 header, 1 inodes, 2 super-root, 3 FPT, 4 reserved, 5 uroot, 6 sce_sys,
    // 7..10 eboot.bin, 10 empty, 11 param.json.
    assert_eq!(layout.image_size, 12 * BLOCK);
    let h = &img[..B];
    assert_eq!(u64_at(h, 0), 2);
    assert_eq!(u64_at(h, 8), 20_130_315);
    assert_eq!(h[0x1A], 1);
    assert_eq!(u16_at(h, 0x1C), 0x8);
    assert_eq!(u32_at(h, 0x20), 0x10000);
    assert_eq!(u64_at(h, 0x28), 1);
    assert_eq!(u64_at(h, 0x30), 7);
    assert_eq!(u64_at(h, 0x38), 12);
    assert_eq!(u64_at(h, 0x40), 1);
    // The inode table's own descriptor at 0x50.
    assert_eq!(u16_at(h, 0x52), 1);
    assert_eq!(u32_at(h, 0x54), 0x10);
    assert_eq!(u64_at(h, 0x58), BLOCK);
    assert_eq!(u64_at(h, 0x60), BLOCK);
    assert_eq!(u64_at(h, 0x68), TIME as u64);
    assert_eq!(u32_at(h, 0xB0), 1);
    assert_eq!(u64_at(h, 0x50 + 0x88), 1);
    assert_eq!(u32_at(h, 0x368), 1);
    assert_eq!(u32_at(h, 0x36C), 0);

    let expect = |n: usize, mode: u16, nlink: u16, flags: u32, size: u64, blocks: u32, db0: i32| {
        let d = inode(&img, n);
        assert_eq!(u16_at(d, 0), mode, "inode {n} mode");
        assert_eq!(u16_at(d, 2), nlink, "inode {n} nlink");
        assert_eq!(u32_at(d, 4), flags, "inode {n} flags");
        assert_eq!(u64_at(d, 8), size, "inode {n} size");
        assert_eq!(u64_at(d, 16), size, "inode {n} second size");
        for t in 0..4 {
            assert_eq!(u64_at(d, 24 + 8 * t), TIME as u64);
        }
        assert_eq!(u32_at(d, 0x60), blocks, "inode {n} blocks");
        assert_eq!(i32_at(d, 0x64), db0, "inode {n} db0");
        let fill = if n == 0 { 0 } else { -1 };
        for k in 1..12 {
            assert_eq!(i32_at(d, 0x64 + 4 * k), fill, "inode {n} db{k}");
        }
        assert!(d[0x94..].iter().all(|&b| b == 0), "inode {n} ib");
    };
    let fpt_len = 4 * 8; // sce_sys, eboot.bin, empty, param.json
    expect(0, 0x416D, 1, 0x2_0010, BLOCK, 1, 2);
    expect(1, 0x816D, 1, 0x2_0010, fpt_len, 1, 3);
    expect(2, 0x416D, 4, 0x10, BLOCK, 1, 5); // uroot: 3 + one subdirectory
    expect(3, 0x416D, 2, 0x10, BLOCK, 1, 6); // sce_sys
    expect(4, 0x816D, 1, 0x10, 2 * BLOCK + 1, 3, 7); // eboot.bin
    expect(5, 0x816D, 1, 0x10, 0, 1, 10); // empty
    expect(6, 0x816D, 1, 0x10, 100, 1, 11); // sce_sys/param.json
    assert!(img[B + 7 * 0xA8..2 * B].iter().all(|&b| b == 0));

    assert_eq!(
        dirents(&img, 2 * B),
        [(1, 2, "flat_path_table".into()), (2, 3, "uroot".into())]
    );
    assert!(
        img[4 * B..5 * B].iter().all(|&b| b == 0),
        "block 4 is reserved"
    );
    assert_eq!(
        dirents(&img, 5 * B),
        [
            (2, 4, ".".into()),
            (2, 5, "..".into()),
            (3, 3, "sce_sys".into()),
            (4, 2, "eboot.bin".into()),
            (5, 2, "empty".into()),
        ]
    );
    assert_eq!(
        dirents(&img, 6 * B),
        [
            (3, 4, ".".into()),
            (2, 5, "..".into()),
            (6, 2, "param.json".into())
        ]
    );
    // The path table: sorted by hash, directories flagged.
    let mut want = vec![
        (fpt_hash("/sce_sys"), 3 | 0x2000_0000),
        (fpt_hash("/eboot.bin"), 4),
        (fpt_hash("/empty"), 5),
        (fpt_hash("/sce_sys/param.json"), 6),
    ];
    want.sort();
    let got: Vec<(u32, u32)> = (0..4)
        .map(|i| (u32_at(&img, 3 * B + 8 * i), u32_at(&img, 3 * B + 8 * i + 4)))
        .collect();
    assert_eq!(got, want);
    assert_eq!(img[7 * B..7 * B + 16], pattern("eboot.bin", 0, 16)[..]);
}

/// `A_` and `B@` share a hash: MkPFS inserts a collision resolver at inode 2, renumbers
/// everything after it, and points the path table at the resolver's records.
#[test]
fn fpt_collisions_go_to_the_resolver() {
    assert_eq!(fpt_hash("A_"), 2110);
    assert_eq!(fpt_hash("B@"), 2110);
    assert_eq!(fpt_hash("/A_"), fpt_hash("/B@"));
    let mut tree = MemTree::new(&[("A_", 3), ("B@", 4), ("eboot.bin", 5)]);
    let (_, img) = build(&mut tree);
    assert_eq!(
        u64_at(&img, 0x30),
        7,
        "SR, FPT, resolver, uroot, three files"
    );
    assert_eq!(
        dirents(&img, 2 * B),
        [
            (1, 2, "flat_path_table".into()),
            (2, 2, "collision_resolver".into()),
            (3, 3, "uroot".into())
        ]
    );
    // The resolver inode: internal, its records' length, after the FPT block.
    let r = inode(&img, 2);
    let records = 2 * (3 + 17usize).next_multiple_of(8) + 0x18;
    assert_eq!((u16_at(r, 0), u32_at(r, 4)), (0x816D, 0x2_0010));
    assert_eq!(u64_at(r, 8), records as u64);
    assert_eq!(i32_at(r, 0x64), 4);
    // uroot moved to inode 3, block 5; the files follow as 4, 5, 6.
    assert_eq!(i32_at(inode(&img, 3), 0x64), 5);
    assert_eq!(
        dirents(&img, 5 * B),
        [
            (3, 4, ".".into()),
            (3, 5, "..".into()),
            (4, 2, "A_".into()),
            (5, 2, "B@".into()),
            (6, 2, "eboot.bin".into()),
        ]
    );
    let mut want = vec![(fpt_hash("/A_"), 0x8000_0000), (fpt_hash("/eboot.bin"), 6)];
    want.sort();
    let got: Vec<(u32, u32)> = (0..2)
        .map(|i| (u32_at(&img, 3 * B + 8 * i), u32_at(&img, 3 * B + 8 * i + 4)))
        .collect();
    assert_eq!(got, want);
    assert_eq!(u64_at(inode(&img, 1), 8), 16);
    // The records: full paths, then a 0x18-byte terminator.
    let at = 4 * B;
    assert_eq!(
        dirents(&img, at),
        [(4, 2, "/A_".into()), (5, 2, "/B@".into())]
    );
    assert!(img[at + 48..at + 48 + 0x18].iter().all(|&b| b == 0));
    assert_eq!(&img[at + 16..at + 19], b"/A_");
    let mut back = read_back(img);
    assert_same(&mut tree, &mut back);
}

#[test]
fn bad_names_are_all_listed() {
    let mut tree = MemTree::new(&[
        ("eboot.bin", 1),
        ("sce_sys/A.txt", 1),
        ("sce_sys/a.txt", 1), // one name to the case-insensitive image
        ("data/caf\u{e9}.bin", 1),
        ("data/\u{65e5}/x", 1),
        ("x/eboot.bin", 1),
        ("x/eboot.bin/inner", 1), // a file and a directory at once
        ("Dir/f", 1),
        ("dir", 1), // a file colliding with a directory
    ]);
    tree.empty.push("ctl\u{1}dir".into());
    tree.empty.push("long/".to_string() + &"n".repeat(256));
    let e = plan(&tree, &opts(), &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    for needle in [
        "sce_sys/a.txt: same name as sce_sys/A.txt",
        "data/caf\u{e9}.bin: non-ASCII name",
        "data/\u{65e5}: non-ASCII name",
        "x/eboot.bin: both a file and a directory",
        "ctl\\u{1}dir: control character",
        "name longer than 255 bytes",
        "dir: same name as Dir",
    ] {
        assert!(e.contains(needle), "missing {needle:?} in:\n{e}");
    }
    assert!(e.starts_with("7 name problem(s)"), "{e}");
}

#[test]
fn d32_limits_are_refused() {
    // uroot's link count: 3 + 65533 subdirectories is over 16 bits.
    let mut tree = MemTree::new(&[("eboot.bin", 1)]);
    tree.empty = (0..65_533).map(|i| format!("d{i}")).collect();
    let e = plan(&tree, &opts(), &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(e.contains("65533 subdirectories"), "{e}");
    tree.empty.pop();
    plan(&tree, &opts(), &AtomicBool::new(false)).unwrap();

    // 2^31 blocks of 64 KiB: past a 32-bit signed block pointer.
    let tree = MemTree::new(&[("eboot.bin", 1), ("huge", (1u64 << 31) * BLOCK)]);
    let e = plan(&tree, &opts(), &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(e.contains("block pointer"), "{e}");
    let tree = MemTree::new(&[("eboot.bin", 1), ("big", (1u64 << 30) * BLOCK)]);
    let layout = plan(&tree, &opts(), &AtomicBool::new(false)).unwrap();
    assert_eq!(layout.image_size, ((1u64 << 30) + 7) * BLOCK);
}

#[test]
fn write_refuses_a_changed_tree_a_used_output_and_a_cancel() {
    let tree = MemTree::new(&[("eboot.bin", 10)]);
    let cancel = AtomicBool::new(false);
    let layout = plan(&tree, &opts(), &cancel).unwrap();
    let mut grown = MemTree::new(&[("eboot.bin", 11)]);
    let mut out = Cursor::new(Vec::new());
    let e = write(&mut grown, &layout, &mut out, &cancel, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("changed"), "{e}");

    let mut tree = MemTree::new(&[("eboot.bin", 10)]);
    let mut used = Cursor::new(vec![1u8]);
    let e = write(&mut tree, &layout, &mut used, &cancel, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("not empty"), "{e}");

    let mut big = MemTree::new(&[("big.bin", 40 << 20), ("eboot.bin", 1)]);
    let layout = plan(&big, &opts(), &cancel).unwrap();
    let mut calls = 0;
    let e = write(
        &mut big,
        &layout,
        &mut Cursor::new(Vec::new()),
        &cancel,
        &mut |d, _| {
            calls += 1;
            if d > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");
    assert_eq!(calls, 2, "the start, then one chunk");
    assert!(matches!(
        plan(&big, &opts(), &cancel).unwrap_err(),
        Error::Cancelled
    ));
}

/// A small game tree, written as `inner` into a `.ffpfsc` and read back through it.
fn wrap_round_trip(inner: &str) {
    let tmp = TempDir::new(inner);
    let mut tree = MemTree::new(&[
        ("eboot.bin", 3 * BLOCK + 17),
        ("sce_sys/param.json", 812),
        ("data/empty.bin", 0),
        ("data/deep/x.dat", 300_000),
    ]);
    tree.empty = vec!["data/shaders".into()];
    let cancel = AtomicBool::new(false);
    let name = format!("PPSA00001.{inner}");
    let path = tmp.0.join("out.ffpfsc");
    let mut out = File::create(&path).unwrap();
    let wopts = WrapOptions {
        threads: 3,
        time: Some(TIME),
        ..WrapOptions::default()
    };
    let mut fill_into = |raw: u64, writer: &mut dyn FnMut(&mut Stream<'_>) -> Result<u64>| {
        let ((), report) = wrap(&name, raw, &mut out, &wopts, &cancel, |s| {
            assert_eq!(writer(s)?, raw);
            Ok(())
        })
        .unwrap();
        report
    };
    let progress: &mut dyn FnMut(u64, u64) = &mut |_, _| {};
    let report = match inner {
        "exfat" => {
            // ps5upload's own exFAT writer (ps5upload_fpkg::exfat_write) inside the container.
            let p = ps5upload_fpkg::exfat_write::plan_exfat(&tree, None, 0).unwrap();
            fill_into(p.image_bytes, &mut |s| {
                ps5upload_fpkg::exfat_write::write_exfat(&p, &mut tree, s, progress, &|| false)
            })
        }
        "ffpkg" => {
            let l = ps5_dump_forge_ufs2::plan(&tree, &Default::default(), &cancel).unwrap();
            fill_into(l.image_size, &mut |s| {
                Ok(ps5_dump_forge_ufs2::write(&mut tree, &l, s, &cancel, progress)?.image_size)
            })
        }
        _ => {
            let l = plan(&tree, &opts(), &cancel).unwrap();
            fill_into(l.image_size, &mut |s| {
                Ok(write(&mut tree, &l, s, &cancel, progress)?.image_size)
            })
        }
    };
    drop(out);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), report.image_size);
    assert!(report.image_size <= container_size_max(report.raw_size).unwrap());
    assert!(report.compressed_blocks > 0 && report.stored_size < report.raw_size / 2);
    let (mut back, info) = open_ffpfsc(Box::new(File::open(&path).unwrap()), "t").unwrap();
    assert_eq!(info.inner_name, name);
    assert_eq!(info.raw_size, report.raw_size);
    assert_eq!(info.stored_size, report.stored_size);
    assert!(info.compressed);
    assert_eq!(info.blocks, report.raw_size.div_ceil(BLOCK));
    assert_eq!(info.blocks, report.blocks);
    assert_eq!(info.compressed_blocks, report.compressed_blocks);
    assert_eq!(info.outer.ndblock * BLOCK, report.image_size);
    assert_same(&mut tree, back.as_mut());
}

#[test]
fn ffpfsc_round_trips_with_an_exfat_inside() {
    wrap_round_trip("exfat");
}

#[test]
fn ffpfsc_round_trips_with_a_ufs2_inside() {
    wrap_round_trip("ffpkg");
}

#[test]
fn ffpfsc_round_trips_with_a_pfs_inside() {
    wrap_round_trip("ffpfs");
}

/// Wraps `data` and reads it back through the outer PFS: `(report, bytes read)`.
fn wrap_bytes(data: &[u8]) -> (ps5_dump_forge_pfs::WrapReport, Vec<u8>) {
    let tmp = TempDir::new("bytes");
    let path = tmp.0.join("b.ffpfsc");
    let mut out = File::create(&path).unwrap();
    let wopts = WrapOptions {
        time: Some(TIME),
        ..WrapOptions::default()
    };
    let cancel = AtomicBool::new(false);
    let ((), r) = wrap(
        "blob.exfat",
        data.len() as u64,
        &mut out,
        &wopts,
        &cancel,
        |s| Ok(s.write_all(data)?),
    )
    .unwrap();
    let mut outer = PfsSource::open(&path).unwrap();
    assert_eq!(outer.files().len(), 1);
    let back = outer.read("blob.exfat").unwrap();
    (r, back)
}

#[test]
fn incompressible_blocks_are_stored_raw() {
    let mut x = 0x9E37_79B9u32;
    let data: Vec<u8> = (0..5 * B + 123)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    let (r, back) = wrap_bytes(&data);
    assert_eq!(back, data);
    // The five noise blocks stay raw; the last, 123 bytes padded with zeros, compresses.
    assert_eq!((r.blocks, r.compressed_blocks), (6, 1));
    assert!(r.stored_size > BLOCK + 5 * BLOCK, "always a container");
    assert!(r.stored_size < BLOCK + 6 * BLOCK);
}

#[test]
fn compressible_blocks_shrink() {
    let mut data = Vec::new();
    for i in 0..40_000u32 {
        data.extend_from_slice(format!("line {i} of the game data\n").as_bytes());
    }
    let (r, back) = wrap_bytes(&data);
    assert_eq!(back, data);
    assert_eq!(r.compressed_blocks, r.blocks);
    assert!(r.stored_size < data.len() as u64 / 3, "{r:?}");
}

/// Past 8063 blocks the offset table outgrows its first block and pushes the data back.
#[test]
fn a_long_table_moves_the_data() {
    let tmp = TempDir::new("table");
    let path = tmp.0.join("t.ffpfsc");
    let mut out = File::create(&path).unwrap();
    let raw = 8064 * BLOCK;
    let cancel = AtomicBool::new(false);
    let wopts = WrapOptions {
        time: Some(TIME),
        ..WrapOptions::default()
    };
    let ((), r) = wrap("big.exfat", raw, &mut out, &wopts, &cancel, |s| {
        s.write_all(b"first")?;
        s.seek(SeekFrom::Start(raw - 4))?;
        s.write_all(b"last")?;
        Ok(())
    })
    .unwrap();
    drop(out);
    let img = std::fs::read(&path).unwrap();
    let c = 6 * B;
    assert_eq!(&img[c..c + 4], b"PFSC");
    assert_eq!(u64_at(&img, c + 0x20), 2 * BLOCK, "data_at");
    assert_eq!(
        u64_at(&img, c + 0x400),
        2 * BLOCK,
        "the first block's offset"
    );
    assert_eq!(r.blocks, 8064);
    let mut outer = PfsSource::open(&path).unwrap();
    assert_eq!(outer.read_range("big.exfat", 0, 5).unwrap(), b"first");
    assert_eq!(outer.read_range("big.exfat", raw - 4, 9).unwrap(), b"last");
}

/// Damaged images fail to open or read, and never panic.
#[test]
fn damaged_images_are_refused() {
    let mut tree = MemTree::new(&[
        ("eboot.bin", 2 * BLOCK + 5),
        ("sce_sys/param.json", 30),
        ("a/b/c.bin", 40),
    ]);
    let (_, img) = build(&mut tree);
    let open = |img: Vec<u8>| PfsSource::from_reader(Box::new(Cursor::new(img)), "x".into());

    let mut cut = img.clone();
    cut.truncate(img.len() - B);
    let e = open(cut).err().unwrap().to_string();
    assert!(e.contains("pointer layout"), "{e}");

    // eboot.bin is inode 7 (after SR, FPT, uroot, a, a/b, sce_sys, a/b/c.bin): point it
    // far away.
    let mut far = img.clone();
    let at = B + 7 * 0xA8 + 0x64;
    far[at..at + 4].copy_from_slice(&0x7FFF_0000i32.to_le_bytes());
    let e = open(far).err().unwrap().to_string();
    assert!(
        e.contains("eboot.bin") && e.contains("pointer layout"),
        "{e}"
    );

    // a/b listing uroot (inode 2) as its subdirectory: a cycle.
    let mut cycle = img.clone();
    let ab = u32_at(inode(&img, 4), 0x64) as usize * B;
    let entries = dirents(&img, ab);
    assert_eq!(entries[2].2, "c.bin");
    let mut at = ab;
    for _ in 0..2 {
        at += u32_at(&img, at + 12) as usize;
    }
    cycle[at..at + 8].copy_from_slice(&[2, 0, 0, 0, 3, 0, 0, 0]);
    let e = open(cycle).err().unwrap().to_string();
    assert!(e.contains("reached twice"), "{e}");

    // A sea of single-byte flips in the metadata: errors are fine, panics are not.
    let mut x = 0x1234_5678u32;
    for _ in 0..400 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let mut bad = img.clone();
        let at = (x as usize) % (11 * B);
        let block = at / B;
        // Header fields, inodes, dirents and the FPT: where the bytes mean something.
        let at = block * B + (at % B) % 0x400;
        bad[at] ^= 1 << (x >> 29);
        if let Ok(mut src) = open(bad) {
            let list = src.files().to_vec();
            for f in list {
                let _ = src.read(&f.path);
            }
        }
    }
}

#[test]
fn damaged_containers_are_refused() {
    let data: Vec<u8> = (0..3 * B as u32).map(|i| (i % 251) as u8).collect();
    let tmp = TempDir::new("dmg");
    let path = tmp.0.join("d.ffpfsc");
    let mut out = File::create(&path).unwrap();
    let wopts = WrapOptions {
        time: Some(TIME),
        ..WrapOptions::default()
    };
    wrap(
        "d.exfat",
        data.len() as u64,
        &mut out,
        &wopts,
        &AtomicBool::new(false),
        |s| Ok(s.write_all(&data)?),
    )
    .unwrap();
    drop(out);
    let img = std::fs::read(&path).unwrap();
    let c = 6 * B;
    let read = |img: Vec<u8>| -> Result<Vec<u8>> {
        let mut s = PfsSource::from_reader(Box::new(Cursor::new(img)), "c".into())?;
        s.read("d.exfat")
    };
    assert_eq!(read(img.clone()).unwrap(), data);
    // A garbled first block fails to inflate.
    let mut bad = img.clone();
    bad[c + B + 10] ^= 0xFF;
    assert!(read(bad).is_err());
    // An offset that runs backwards.
    let mut bad = img.clone();
    bad[c + 0x400 + 8..c + 0x400 + 16].copy_from_slice(&1u64.to_le_bytes());
    assert!(read(bad).is_err());
    // The table no longer ends at the stored size.
    let mut bad = img.clone();
    bad[c + 0x400 + 24..c + 0x400 + 32].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(read(bad).is_err());
    // A data length that disagrees with the file.
    let mut bad = img.clone();
    bad[c + 0x28..c + 0x30].copy_from_slice(&(9 * BLOCK).to_le_bytes());
    assert!(read(bad).is_err());
    // Not exactly one nested image in the root.
    let e = open_ffpfsc(
        Box::new(Cursor::new(build(&mut MemTree::new(&[("a", 1)])).1)),
        "n",
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        e.contains("exactly one nested image") && e.contains("[a]"),
        "{e}"
    );
}

/// The two writers that feed `.ffpfsc` write the same bytes into any `Write + Seek`, and
/// refuse an output that already holds something.
#[test]
fn inner_writers_take_any_seekable_output() {
    let mut tree = MemTree::new(&[("eboot.bin", 70_000), ("sce_sys/param.json", 10)]);
    let cancel = AtomicBool::new(false);
    let tmp = TempDir::new("generic");
    // ps5upload's exFAT writer writes front to back into any Write (see its own tests).
    let p = ps5upload_fpkg::exfat_write::plan_exfat(&tree, None, 0).unwrap();
    let mut mem = Cursor::new(Vec::new());
    ps5upload_fpkg::exfat_write::write_exfat(&p, &mut tree, &mut mem, &mut |_, _| {}, &|| false)
        .unwrap();
    let mut file = File::create(tmp.0.join("x.exfat")).unwrap();
    ps5upload_fpkg::exfat_write::write_exfat(&p, &mut tree, &mut file, &mut |_, _| {}, &|| false)
        .unwrap();
    assert_eq!(
        std::fs::read(tmp.0.join("x.exfat")).unwrap(),
        mem.into_inner()
    );

    let l = ps5_dump_forge_ufs2::plan(&tree, &Default::default(), &cancel).unwrap();
    let mut mem = Cursor::new(Vec::new());
    ps5_dump_forge_ufs2::write(&mut tree, &l, &mut mem, &cancel, &mut |_, _| {}).unwrap();
    let mut file = File::create(tmp.0.join("x.ffpkg")).unwrap();
    ps5_dump_forge_ufs2::write(&mut tree, &l, &mut file, &cancel, &mut |_, _| {}).unwrap();
    let mem = mem.into_inner();
    assert_eq!(mem.len() as u64, l.image_size);
    assert_eq!(std::fs::read(tmp.0.join("x.ffpkg")).unwrap(), mem);
    let mut used = Cursor::new(vec![0u8]);
    assert!(ps5_dump_forge_ufs2::write(&mut tree, &l, &mut used, &cancel, &mut |_, _| {}).is_err());
}

#[test]
fn reads_past_the_end_are_empty() {
    let mut tree = MemTree::new(&[("eboot.bin", 100), ("sce_sys/param.json", 30)]);
    let (_, img) = build(&mut tree);
    let mut src = read_back(img);
    assert!(src.read_range("eboot.bin", 100, 8).unwrap().is_empty());
    assert!(src.read_range("eboot.bin", u64::MAX, 1).unwrap().is_empty());
}

#[test]
fn a_second_dot_entry_is_refused() {
    let mut tree = MemTree::new(&[("eboot.bin", 100)]);
    let (_, mut img) = build(&mut tree);
    // uroot's `..` (its second record) rewritten as another `.`.
    let uroot = u32_at(inode(&img, 2), 0x64) as usize * B;
    let at = uroot + u32_at(&img, uroot + 12) as usize;
    assert_eq!(&img[at + 16..at + 18], b"..");
    img[at + 4..at + 8].copy_from_slice(&4i32.to_le_bytes());
    img[at + 8..at + 12].copy_from_slice(&1i32.to_le_bytes());
    img[at + 17] = 0;
    let e = PfsSource::from_reader(Box::new(Cursor::new(img)), "x".into())
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("exactly one '.'"), "{e}");
}

#[test]
fn a_wrapped_name_holds_no_slash() {
    let tmp = TempDir::new("slash");
    let mut out = File::create(tmp.0.join("o.ffpfsc")).unwrap();
    let cancel = AtomicBool::new(false);
    let r = wrap(
        "dir/PPSA00001.exfat",
        BLOCK,
        &mut out,
        &WrapOptions::default(),
        &cancel,
        |s| {
            s.write_all(&[0; B])?;
            Ok(())
        },
    );
    assert!(r.is_err());
}

const FPT_MISMATCH: &str = "x: the flat path table does not match the directory tree (the console looks files up through it)";

fn open_err(img: Vec<u8>) -> String {
    PfsSource::from_reader(Box::new(Cursor::new(img)), "x".into())
        .err()
        .expect("the damaged image is refused")
        .to_string()
}

/// The console looks files up through the flat path table: one that disagrees with the
/// directories, or is missing, refuses the image.
#[test]
fn a_damaged_path_table_is_refused() {
    // Inodes: SR 0, FPT 1, uroot 2, sce_sys 3, eboot.bin 4, param.json 5; the FPT at block 3.
    let mut tree = MemTree::new(&[("eboot.bin", 100), ("sce_sys/param.json", 30)]);
    let (_, img) = build(&mut tree);
    let fpt = 3 * B;
    let entry = |hash: u32| {
        (0..3)
            .map(|i| fpt + 8 * i)
            .find(|&at| u32_at(&img, at) == hash)
            .unwrap()
    };
    read_back(img.clone());

    let mut hash = img.clone();
    hash[fpt] ^= 1;
    assert_eq!(open_err(hash), FPT_MISMATCH);

    let mut ino = img.clone();
    let at = entry(fpt_hash("/eboot.bin")) + 4;
    assert_eq!(u32_at(&img, at), 4);
    ino[at..at + 4].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(open_err(ino), FPT_MISMATCH);

    // The directory flag, bit 29, on sce_sys's value and on a file's.
    for (path, value) in [("/sce_sys", 3 | 0x2000_0000), ("/eboot.bin", 4)] {
        let mut flag = img.clone();
        let at = entry(fpt_hash(path)) + 4;
        assert_eq!(u32_at(&img, at), value);
        flag[at + 3] ^= 0x20;
        assert_eq!(open_err(flag), FPT_MISMATCH, "{path}");
    }

    // The FPT's length (its inode's two sizes) one entry short.
    let mut short = img.clone();
    let at = B + 0xA8 + 8;
    for off in [at, at + 8] {
        short[off..off + 8].copy_from_slice(&16u64.to_le_bytes());
    }
    assert_eq!(open_err(short), FPT_MISMATCH);

    // The super-root's entry renamed, then removed (uroot's record moved over it).
    let mut renamed = img.clone();
    assert_eq!(&img[2 * B + 16..2 * B + 31], b"flat_path_table");
    renamed[2 * B + 30] = b'X';
    let missing = "x: the image has no flat path table (the console looks files up through it)";
    assert_eq!(open_err(renamed), missing);
    let mut removed = img.clone();
    let first = u32_at(&img, 2 * B + 12) as usize;
    let second = u32_at(&img, 2 * B + first + 12) as usize;
    removed.copy_within(2 * B + first..2 * B + first + second, 2 * B);
    removed[2 * B + second..3 * B].fill(0);
    assert_eq!(dirents(&removed, 2 * B), [(2, 3, "uroot".into())]);
    assert_eq!(open_err(removed), missing);
}

/// The hash upper-cases only on a case-insensitive image (mode bit 0x8).
#[test]
fn the_path_table_follows_the_case_mode() {
    let mut tree = MemTree::new(&[("eboot.bin", 100), ("sce_sys/param.json", 30)]);
    let (_, img) = build(&mut tree);
    let mut sensitive = img.clone();
    sensitive[0x1C] &= !0x8;
    assert_eq!(open_err(sensitive.clone()), FPT_MISMATCH);
    // The same table hashed as written, without upper-casing, is what that image needs.
    let exact = |p: &str| {
        p.bytes()
            .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(u32::from(c)))
    };
    let mut want = [
        (exact("/sce_sys"), 3 | 0x2000_0000),
        (exact("/eboot.bin"), 4u32),
        (exact("/sce_sys/param.json"), 5),
    ];
    want.sort();
    for (i, (h, v)) in want.iter().enumerate() {
        let at = 3 * B + 8 * i;
        sensitive[at..at + 4].copy_from_slice(&h.to_le_bytes());
        sensitive[at + 4..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    let mut back = PfsSource::from_reader(Box::new(Cursor::new(sensitive)), "x".into()).unwrap();
    assert_eq!(back.header().mode, 0);
    assert_same(&mut tree, &mut back);
}

/// `A_` and `B@` share a hash: the resolver's records must be what the tree encodes to, and
/// it must be there.
#[test]
fn a_damaged_collision_resolver_is_refused() {
    let mut tree = MemTree::new(&[("A_", 3), ("B@", 4), ("eboot.bin", 5)]);
    let (_, img) = build(&mut tree);
    let resolver = 4 * B;
    assert_eq!(
        dirents(&img, resolver),
        [(4, 2, "/A_".into()), (5, 2, "/B@".into())]
    );
    // A record's inode, its type, a byte of its path, and the terminator.
    for (at, flip) in [
        (resolver, 1),
        (resolver + 4, 1),
        (resolver + 17, 0x20),
        (resolver + 48, 1),
    ] {
        let mut bad = img.clone();
        bad[at] ^= flip;
        assert_eq!(open_err(bad), FPT_MISMATCH, "byte {at:#x}");
    }
    // The FPT's pointer into the resolver.
    let mut bad = img.clone();
    let at = (0..2)
        .map(|i| 3 * B + 8 * i)
        .find(|&at| u32_at(&img, at + 4) == 0x8000_0000)
        .unwrap();
    bad[at + 4] = 8;
    assert_eq!(open_err(bad), FPT_MISMATCH);
    // No resolver in the super-root.
    let mut gone = img.clone();
    let at = 2 * B + u32_at(&img, 2 * B + 12) as usize;
    assert_eq!(&img[at + 16..at + 34], b"collision_resolver");
    gone[at + 16] = b'C';
    assert_eq!(open_err(gone), FPT_MISMATCH);
}
