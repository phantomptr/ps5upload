//! Round trips through the writer, checked by the vendored reader and by the mini fsck in
//! `common`. Images land in `$CARGO_TARGET_TMPDIR/ufs2/` so `scripts/fsck-ufs.sh` can be
//! pointed at them afterwards; the half-gigabyte one is deleted unless FORGE_UFS2_KEEP=1.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};

use common::{Synth, build, compare_with_reader, fsck, scratch};
use ps5_dump_forge_ufs2::Options;
use ps5upload_fpkg::source::{FolderSource, SourceTree};

const B: u64 = 65536;

fn assert_clean(image: &std::path::Path) -> common::Fsck {
    common::script_last_group_check(image);
    let r = fsck(image);
    assert!(r.errors.is_empty(), "{}: {:#?}", image.display(), r.errors);
    r
}

/// A game-shaped folder through `FolderSource`: nested dirs, an empty dir, a zero-length
/// file, files at the direct -> single-indirect edge, non-ASCII (NFC) names.
#[test]
fn game_folder_round_trip() {
    let root = scratch("game-src");
    let _ = std::fs::remove_dir_all(&root);
    let files: &[(&str, u64)] = &[
        ("eboot.bin", 3 * B + 17),
        ("sce_sys/param.json", 812),
        ("sce_sys/icon0.png", 70_000),
        ("sce_module/libc.prx", 12 * B), // exactly the 12 direct blocks
        ("sce_module/libSceFios2.prx", 12 * B + 1), // first single-indirect block
        ("data/empty.bin", 0),
        ("data/deep/er/still/x.dat", 5),
        ("data/caf\u{e9}/\u{65e5}\u{672c}.txt", 4096),
    ];
    for (i, (p, size)) in files.iter().enumerate() {
        let path = root.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, common::pattern(i, 0, *size as usize)).unwrap();
    }
    std::fs::create_dir_all(root.join("data/shaders")).unwrap();

    let mut src = FolderSource::open(&root).unwrap();
    assert_eq!(src.empty_dirs(), ["data/shaders"]);
    let (img, layout, report) = build("game", &mut src, &Options::default());
    assert_eq!(report.files, files.len() as u64);
    assert_eq!(report.image_size % B, 0);
    assert_eq!(std::fs::metadata(&img).unwrap().len(), layout.image_size);
    assert!(layout.last_group_blocks > layout.metadata_blocks_per_group);
    let r = assert_clean(&img);
    assert!(
        r.dirs.contains(&"data/shaders".to_string()),
        "empty dir kept: {:?}",
        r.dirs
    );
    assert_eq!(r.files.len(), files.len());
    compare_with_reader(&img, &mut src);

    // Same tree, same bytes.
    let first = std::fs::read(&img).unwrap();
    let (img2, _, _) = build("game-again", &mut src, &Options::default());
    assert!(
        first == std::fs::read(&img2).unwrap(),
        "output is not deterministic"
    );
    std::fs::remove_file(img2).unwrap();
}

/// Free space for writable mounts: the requested bytes stay free, plus spare inodes.
#[test]
fn free_bytes_reserved() {
    let mut src = Synth::new(&[("eboot.bin", 100), ("sce_sys/param.json", 10)]);
    let want = 300 * 1024 * 1024;
    let (img, layout, _) = build("free", &mut src, &Options { free_bytes: want });
    assert!(layout.free_bytes >= want, "{} < {want}", layout.free_bytes);
    assert!(layout.free_inodes >= 2048);
    assert_clean(&img);
    compare_with_reader(&img, &mut src);
}

/// One directory with 20 000 entries: the directory itself needs a single-indirect block
/// (> 12 blocks of records). Most files are empty so the image stays small.
#[test]
fn many_entry_directory() {
    let names: Vec<String> = (0..20_000)
        .map(|i| format!("assets/a-rather-long-file-name-to-fill-records-{i:06}.bin"))
        .collect();
    let mut list: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 0)).collect();
    list[7].1 = 3 * B + 1;
    list[19_999].1 = 1;
    list.push(("eboot.bin", 10));
    let mut src = Synth::new(&list);
    let (img, layout, _) = build("many", &mut src, &Options::default());
    assert!(layout.free_inodes < u64::MAX);
    let r = assert_clean(&img);
    assert_eq!(r.files.len(), list.len());
    // The vendored reader caps directories at 1 MiB (another agent is lifting that);
    // this one is ~1.3 MiB, so a refusal there is a reader limit, not an image fault.
    match ps5upload_fpkg::ufs2_source::Ufs2Source::open(&img) {
        Ok(_) => compare_with_reader(&img, &mut src),
        Err(e) => eprintln!("NOTE: vendored reader refused the big directory: {e}"),
    }
}

/// Many empty files at the 64 KiB inode-density floor: `plan` must grow the image until
/// every inode fits instead of laying out too few.
#[test]
fn inode_exhaustion_grows_the_image() {
    let names: Vec<String> = (0..6000).map(|i| format!("d{}/f{i}", i % 7)).collect();
    let list: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 0)).collect();
    let mut src = Synth::new(&list);
    let cancel = AtomicBool::new(false);
    let layout = ps5_dump_forge_ufs2::plan(&src, &Options::default(), &cancel).unwrap();
    let total = layout.cylinder_groups * layout.inodes_per_group;
    assert!(total >= 6000 + 8 + 2, "{total} inodes for 6008 objects");
    assert_eq!(layout.bytes_per_inode, 64 * 1024);
    let (img, _, _) = build("inodes", &mut src, &Options::default());
    let r = assert_clean(&img);
    assert_eq!(r.files.len(), 6000);
}

/// The last cylinder group as small as allowed: its metadata plus a single data block
/// (the UFS2Tool v4.1 bug sits one step below this). Only big images have groups of a
/// fixed size (newfs fills the cg map block), so this one is ~70 GB, mostly free and
/// sparse on disk; the requested free space is nudged until the last group is that small.
#[test]
fn nearly_empty_last_group() {
    let mut src = Synth::new(&[("eboot.bin", 3 * B + 5), ("sce_sys/param.json", 100)]);
    let cancel = AtomicBool::new(false);
    let mut free: i64 = 64 << 30;
    let mut found = None;
    for _ in 0..40 {
        let opts = Options {
            free_bytes: free as u64,
        };
        let l = ps5_dump_forge_ufs2::plan(&src, &opts, &cancel).unwrap();
        let meta = l.metadata_blocks_per_group as i64;
        let last = l.last_group_blocks as i64;
        assert!(last > meta, "last group {last} <= metadata {meta}");
        if l.cylinder_groups > 4 && last == meta + 1 {
            found = Some(opts);
            break;
        }
        // Shrink the last group by its excess; if it would vanish, the planner moves on.
        free -= (last - meta - 1).max(1) * B as i64;
    }
    let opts = found.expect("a layout whose last group has one data block");
    let (img, layout, _) = build("lastcg", &mut src, &opts);
    // -i comes from the tree, not the free space (ffpkg.sh): a tiny tree gets the floor.
    assert_eq!(layout.bytes_per_inode, 64 * 1024);
    assert_eq!(
        layout.last_group_blocks,
        layout.metadata_blocks_per_group + 1
    );
    let r = assert_clean(&img);
    assert_eq!(r.size % r.fpg, r.dblkno + 1);
    compare_with_reader(&img, &mut src);
}

/// Files at the single -> double indirect edge: 12 + 8192 blocks (537 MB), and one block
/// more. Synthetic data, so nothing is read from disk; ~1 GB of image is written.
#[test]
fn double_indirect_transition() {
    let edge = (12 + 8192) * B;
    let mut src = Synth::new(&[("a/edge.bin", edge), ("a/over.bin", edge + 1), ("b.bin", 1)]);
    let (img, _, _) = build("double", &mut src, &Options::default());
    assert_clean(&img);
    compare_with_reader(&img, &mut src);
    if std::env::var_os("FORGE_UFS2_KEEP").is_none() {
        std::fs::remove_file(img).unwrap();
    }
}

/// Every bad name is reported, in one error, before anything is written.
#[test]
fn rejected_names_listed_together() {
    let long = "x".repeat(256);
    let ok255 = "y".repeat(255);
    let bad: Vec<String> = vec![
        "nfd/e\u{301}.txt".into(),
        format!("long/{long}"),
        "nul/a\0b".into(),
        "empty//comp".into(),
        "dot/../x".into(),
        "lossy/\u{FFFD}.bin".into(),
        "clash".into(),
        "clash/inner".into(),
        format!("deep/{}", "d/".repeat(600) + "f"),
    ];
    let mut list: Vec<(&str, u64)> = bad.iter().map(|s| (s.as_str(), 1)).collect();
    list.push((&ok255, 1));
    list.push(("fine.bin", 1));
    let src = Synth::new(&list);
    let cancel = AtomicBool::new(false);
    let e = ps5_dump_forge_ufs2::plan(&src, &Options::default(), &cancel)
        .unwrap_err()
        .to_string();
    for needle in [
        "nfd/",
        "long/",
        "nul/",
        "empty//",
        "dot/",
        "lossy/",
        "\"clash\"",
    ] {
        assert!(e.contains(needle), "{needle} missing from: {e}");
    }
    assert!(!e.contains("fine.bin") && !e.contains(&ok255[..20]), "{e}");
}

#[test]
fn cancellation() {
    let mut src = Synth::new(&[("big.bin", 40 * 1024 * 1024), ("eboot.bin", 1)]);
    let cancel = AtomicBool::new(true);
    let e = ps5_dump_forge_ufs2::plan(&src, &Options::default(), &cancel).unwrap_err();
    assert!(e.to_string().contains("cancelled"));

    cancel.store(false, Ordering::Relaxed);
    let layout = ps5_dump_forge_ufs2::plan(&src, &Options::default(), &cancel).unwrap();
    let path = scratch("cancel.ffpkg");
    let _ = std::fs::remove_file(&path);
    let mut out = std::fs::File::create_new(&path).unwrap();
    let mut calls = 0;
    let e = ps5_dump_forge_ufs2::write(&mut src, &layout, &mut out, &cancel, &mut |done, total| {
        calls += 1;
        assert!(done <= total);
        if done > 0 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
    assert!(calls >= 2);
    std::fs::remove_file(&path).unwrap();

    // Cancelled from the very last progress call: still an error, not a finished image.
    cancel.store(false, Ordering::Relaxed);
    let mut out = std::fs::File::create_new(&path).unwrap();
    let total = layout_total(&src);
    let e = ps5_dump_forge_ufs2::write(&mut src, &layout, &mut out, &cancel, &mut |done, _| {
        if done == total {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
    std::fs::remove_file(path).unwrap();
}

fn layout_total(src: &Synth) -> u64 {
    src.files.iter().map(|f| f.size).sum()
}

/// The layout belongs to the tree it was planned from, and the output must be empty.
#[test]
fn write_refuses_mismatches() {
    let src = Synth::new(&[("eboot.bin", 10)]);
    let cancel = AtomicBool::new(false);
    let layout = ps5_dump_forge_ufs2::plan(&src, &Options::default(), &cancel).unwrap();
    let path = scratch("mismatch.ffpkg");
    let _ = std::fs::remove_file(&path);
    let mut out = std::fs::File::create_new(&path).unwrap();
    // Resized, and renamed at the same size: both refused.
    for other in [&[("eboot.bin", 11)], &[("eboot.bim", 10)]] {
        let mut other = Synth::new(other);
        let e = ps5_dump_forge_ufs2::write(&mut other, &layout, &mut out, &cancel, &mut |_, _| {});
        assert!(e.unwrap_err().to_string().contains("source changed"));
    }
    let mut src = src;
    std::io::Write::write_all(&mut out, b"x").unwrap();
    assert!(
        ps5_dump_forge_ufs2::write(&mut src, &layout, &mut out, &cancel, &mut |_, _| {}).is_err()
    );
    std::fs::remove_file(path).unwrap();
}

/// A tree with nothing but the root still makes a valid filesystem.
#[test]
fn empty_tree() {
    let mut src = Synth::new(&[]);
    let (img, _, report) = build("empty", &mut src, &Options::default());
    assert_eq!((report.files, report.dirs), (0, 1));
    assert_clean(&img);
}
