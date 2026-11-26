use super::*;
use std::io::Write;
use std::sync::atomic::Ordering;

/// A folder of this test's own, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = crate::test_tmp::temp_path(format!(
            "ps5u-free-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The folder as the checks see it (macOS's /var is a link).
        Scratch(std::fs::canonicalize(&dir).unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Bytes that do not compress (`seed` picks which).
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

/// A small game: a folder with `sce_sys/param.json`, files that compress and files that do
/// not, an empty file and a folder two deep.
fn game() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "Game/sce_sys/param.json",
            b"{\"titleId\":\"PPSA00001\"}".to_vec(),
        ),
        ("Game/eboot.bin", noise(300_000, 1)),
        ("Game/data/a.bin", vec![b'a'; 200_000]),
        ("Game/data/empty.bin", Vec::new()),
        ("Game/data/sub/b.bin", noise(100_000, 2)),
    ]
}

#[derive(Default, Clone, Copy)]
struct ZipShape {
    /// Every entry carries zip64 size fields.
    zip64: bool,
    /// Written as a stream: sizes and checksums follow each file (data descriptors).
    streamed: bool,
}

fn zip_bytes(files: &[(&str, Vec<u8>)], shape: ZipShape) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    fn fill<W: Write + std::io::Seek>(
        zw: &mut zip::ZipWriter<W>,
        files: &[(&str, Vec<u8>)],
        shape: ZipShape,
    ) {
        zw.add_directory("Game/hollow/", SimpleFileOptions::default())
            .unwrap();
        for (i, (name, bytes)) in files.iter().enumerate() {
            // Stored and deflated entries side by side.
            let method = if i % 2 == 0 {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            };
            let opts = SimpleFileOptions::default()
                .compression_method(method)
                .large_file(shape.zip64);
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
    }
    if shape.streamed {
        let mut zw = zip::ZipWriter::new_stream(Vec::new());
        fill(&mut zw, files, shape);
        zw.finish().unwrap().into_inner()
    } else {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        fill(&mut zw, files, shape);
        zw.finish().unwrap().into_inner()
    }
}

/// Every file under `root`, by its path from it.
fn tree(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn want(files: &[(&str, Vec<u8>)]) -> std::collections::BTreeMap<String, Vec<u8>> {
    files
        .iter()
        .map(|(n, b)| (n.to_string(), b.clone()))
        .collect()
}

/// A cut after every file, so a small archive shows what a large one does.
const EACH: Wants = Wants {
    cut_every: 0,
    game: true,
};

fn no_stop() -> AtomicBool {
    AtomicBool::new(false)
}

/// Nothing named like a cut archive, and nothing of ours, is left beside `archive`.
fn nothing_left_beside(archive: &Path) {
    for e in std::fs::read_dir(archive.parent().unwrap())
        .unwrap()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        assert!(!is_partial_name(&name), "left behind: {name}");
        assert!(!name.starts_with(".ps5upload-free-"), "left behind: {name}");
    }
}

#[test]
fn a_zip_is_unpacked_from_its_end_and_shrinks_as_it_goes() {
    let tmp = Scratch::new("zip");
    let archive = tmp.path("Game.zip");
    let files = game();
    let bytes = zip_bytes(&files, ZipShape::default());
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    assert_eq!(plan.mode, FreeMode::Entries);
    assert_eq!(plan.game, GameKind::Folder);
    assert_eq!(plan.unpacked, 600_023);
    assert_eq!(plan.archive_bytes, bytes.len() as u64);
    // Unpacked beside the archive it never needs room for all of both.
    assert!(plan.peak_beside < plan.unpacked, "{}", plan.peak_beside);
    assert_eq!(plan.room_needed(false), plan.unpacked);

    let dest = tmp.path("out");
    let partial = tmp.path("Game.zip.partial-deleted");
    // What is left of the archive each time progress is reported.
    let mut sizes = Vec::new();
    let mut last = (0u64, 0u64);
    let mut progress = |done: u64, total: u64| {
        let len = std::fs::metadata(&archive)
            .or_else(|_| std::fs::metadata(&partial))
            .map(|m| m.len())
            .unwrap_or(0);
        sizes.push(len);
        assert!(done >= last.0, "progress went back");
        last = (done, total);
    };
    let mut found = 0;
    let out = consume(&plan, &dest, None, &mut progress, &no_stop(), &mut || {
        found += 1;
        Ok(())
    })
    .unwrap();
    assert!(out.not_removed.is_empty());
    assert_eq!(found, 1);
    assert_eq!(last, (plan.unpacked * 2, plan.unpacked * 2));
    assert_eq!(tree(&dest), want(&files));
    assert!(
        dest.join("Game/hollow").is_dir(),
        "the empty folder is made"
    );
    // It only ever got smaller, it was cut more than once, and it is gone.
    assert!(sizes.windows(2).all(|w| w[1] <= w[0]), "{sizes:?}");
    let mut steps = sizes.clone();
    steps.dedup();
    assert!(steps.len() >= 3, "cut as it went: {steps:?}");
    assert_eq!(steps[0], bytes.len() as u64);
    assert!(!archive.exists() && !partial.exists());
    nothing_left_beside(&archive);
}

#[test]
fn zip64_fields_data_descriptors_and_a_stub_in_front_are_all_read() {
    for (name, shape, stub) in [
        (
            "zip64",
            ZipShape {
                zip64: true,
                streamed: false,
            },
            0usize,
        ),
        (
            "streamed",
            ZipShape {
                zip64: false,
                streamed: true,
            },
            0,
        ),
        ("stub", ZipShape::default(), 4096),
    ] {
        let tmp = Scratch::new(name);
        let archive = tmp.path("Game.zip");
        let files = game();
        let mut bytes = noise(stub, 9);
        bytes.extend(zip_bytes(&files, shape));
        std::fs::write(&archive, &bytes).unwrap();
        let plan = plan_with(&archive, None, EACH).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(plan.mode, FreeMode::Entries, "{name}");
        let dest = tmp.path("out");
        consume(&plan, &dest, None, &mut |_, _| {}, &no_stop(), &mut || {
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(tree(&dest), want(&files), "{name}");
        assert!(!archive.exists(), "{name}");
        nothing_left_beside(&archive);
    }
}

/// With the usual threshold a small archive is cut once, at its end, and the plan's room is
/// what that takes: all of the unpacked size.
#[test]
fn a_small_zip_is_cut_once_and_the_plan_says_the_room_that_takes() {
    let tmp = Scratch::new("once");
    let archive = tmp.path("Game.zip");
    let files = game();
    std::fs::write(&archive, zip_bytes(&files, ZipShape::default())).unwrap();
    let plan = plan(&archive, None).unwrap();
    assert_eq!(plan.peak_beside, plan.unpacked);
    let dest = tmp.path("out");
    consume(&plan, &dest, None, &mut |_, _| {}, &no_stop(), &mut || {
        Ok(())
    })
    .unwrap();
    assert_eq!(tree(&dest), want(&files));
    assert!(!archive.exists());
}

#[test]
fn stopping_half_way_keeps_what_was_unpacked_and_renames_what_is_left() {
    let tmp = Scratch::new("stop");
    let archive = tmp.path("Game.zip");
    let files = game();
    let bytes = zip_bytes(&files, ZipShape::default());
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    let dest = tmp.path("out");
    let stop = no_stop();
    // Stopped in the second file from the end (the first, b.bin, is out and cut by then).
    let unpacked = plan.unpacked;
    let mut progress = |done: u64, _: u64| {
        if done >= unpacked + 150_000 {
            stop.store(true, Ordering::Relaxed);
        }
    };
    let err = consume(&plan, &dest, None, &mut progress, &stop, &mut || Ok(())).unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    let partial = tmp.path("Game.zip.partial-deleted");
    assert_eq!(err.left.as_deref(), Some(&[partial.clone()][..]));
    // The archive no longer has its own name, and what is left is shorter.
    assert!(!archive.exists());
    let left = std::fs::metadata(&partial).unwrap().len();
    assert!(left > 0 && left < bytes.len() as u64, "{left}");
    // What is out is whole and right; the file it stopped in is not there in part.
    let have = tree(&dest);
    let all = want(&files);
    assert!(
        have.contains_key("Game/data/sub/b.bin"),
        "{:?}",
        have.keys()
    );
    assert!(!have.contains_key("Game/data/a.bin"));
    for (name, bytes) in &have {
        assert_eq!(Some(bytes), all.get(name), "{name}");
    }
    // What is left is no archive any more: it is refused, not unpacked short.
    let again = tmp.path("Game.zip");
    std::fs::copy(&partial, &again).unwrap();
    assert!(plan_with(&again, None, EACH).is_err());
}

#[test]
fn a_stop_while_it_is_still_being_checked_leaves_the_archive_whole() {
    let tmp = Scratch::new("early");
    let archive = tmp.path("Game.zip");
    let bytes = zip_bytes(&game(), ZipShape::default());
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    let stop = no_stop();
    let mut progress = |done: u64, _: u64| {
        if done >= 100_000 {
            stop.store(true, Ordering::Relaxed);
        }
    };
    let err = consume(
        &plan,
        &tmp.path("out"),
        None,
        &mut progress,
        &stop,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(err.left.is_none(), "{err}");
    assert_eq!(std::fs::read(&archive).unwrap(), bytes);
    nothing_left_beside(&archive);
}

/// Patches every central-directory entry of a zip: `at` is the offset in the entry header.
fn patch_central(bytes: &mut [u8], at: usize, with: &dyn Fn(usize, &mut [u8])) {
    let mut n = 0;
    let mut i = 0;
    while i + 46 <= bytes.len() {
        if bytes[i..i + 4] == [b'P', b'K', 1, 2] {
            with(n, &mut bytes[i + at..]);
            n += 1;
            i += 46;
        } else {
            i += 1;
        }
    }
    assert!(n > 0);
}

#[test]
fn every_refusal_leaves_the_archive_as_it_was() {
    let files = game();
    let good = zip_bytes(&files, ZipShape::default());
    let named = |extra: &[(&'static str, Vec<u8>)]| {
        let mut all = game();
        all.extend(extra.iter().cloned());
        zip_bytes(&all, ZipShape::default())
    };
    let mut cases: Vec<(&str, Vec<u8>, &str)> = Vec::new();

    let mut locked = good.clone();
    patch_central(&mut locked, 8, &|_, b| b[0] |= 1);
    cases.push(("a password", locked, "password"));

    let mut odd = good.clone();
    patch_central(&mut odd, 10, &|n, b| {
        if n == 2 {
            b[..2].copy_from_slice(&12u16.to_le_bytes());
        }
    });
    cases.push(("a method it can't unpack", odd, "can't be unpacked here"));

    // Two entries pointing at one file's bytes.
    let mut shared = good.clone();
    let first = std::cell::Cell::new([0u8; 4]);
    patch_central(&mut shared, 42, &|n, b| {
        if n == 1 {
            first.set([b[0], b[1], b[2], b[3]]);
        }
        if n == 2 {
            b[..4].copy_from_slice(&first.get());
        }
    });
    cases.push(("files that share bytes", shared, ""));

    // A split zip: its end record says this is disk 1 of 2.
    let mut split = good.clone();
    let eocd = split.len() - 22;
    assert_eq!(split[eocd..eocd + 4], [b'P', b'K', 5, 6]);
    split[eocd + 4..eocd + 6].copy_from_slice(&1u16.to_le_bytes());
    split[eocd + 6..eocd + 8].copy_from_slice(&1u16.to_le_bytes());
    cases.push(("a split zip", split, ""));

    cases.push((
        "two games",
        named(&[("Other/sce_sys/param.json", b"{}".to_vec())]),
        "more than one game",
    ));
    cases.push((
        "no game",
        zip_bytes(&[("readme.txt", b"hello".to_vec())], ZipShape::default()),
        "no game",
    ));
    cases.push((
        "two names apart only by case",
        named(&[("Game/DATA/A.BIN", b"x".to_vec())]),
        "one name",
    ));
    cases.push((
        "a file where a folder is needed",
        named(&[("Game/data/a.bin/inside.bin", b"x".to_vec())]),
        "both a file and a folder",
    ));
    cases.push((
        "a path out of the folder",
        named(&[("../escape.bin", b"x".to_vec())]),
        "unsafe",
    ));
    cases.push(("not an archive at all", noise(5000, 3), ""));

    for (what, bytes, says) in cases {
        let tmp = Scratch::new("refuse");
        let archive = tmp.path("Game.zip");
        std::fs::write(&archive, &bytes).unwrap();
        let err = match plan_with(&archive, None, EACH) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("{what}: not refused"),
        };
        assert!(err.contains(says), "{what}: {err}");
        assert_eq!(std::fs::read(&archive).unwrap(), bytes, "{what}");
        nothing_left_beside(&archive);
        assert!(!tmp.path("out").exists(), "{what}");
    }
}

/// Damage in the middle of the archive is found by the read-through, before anything goes.
#[test]
fn a_damaged_zip_is_found_while_it_is_still_whole() {
    let tmp = Scratch::new("damage");
    let archive = tmp.path("Game.zip");
    let mut bytes = zip_bytes(&game(), ZipShape::default());
    // Inside eboot.bin's stored bytes (the second entry).
    let at = bytes.windows(9).position(|w| w == b"eboot.bin").unwrap() + 5000;
    bytes[at] ^= 0xff;
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    let err = consume(
        &plan,
        &tmp.path("out"),
        None,
        &mut |_, _| {},
        &no_stop(),
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(err.left.is_none(), "{err}");
    assert_eq!(std::fs::read(&archive).unwrap(), bytes);
    nothing_left_beside(&archive);
}

#[test]
fn an_archive_changed_after_the_check_is_not_touched() {
    let tmp = Scratch::new("changed");
    let archive = tmp.path("Game.zip");
    std::fs::write(&archive, zip_bytes(&game(), ZipShape::default())).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    let other = zip_bytes(
        &[("Game/sce_sys/param.json", b"{}".to_vec())],
        ZipShape::default(),
    );
    std::fs::write(&archive, &other).unwrap();
    let err = consume(
        &plan,
        &tmp.path("out"),
        None,
        &mut |_, _| {},
        &no_stop(),
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(
        err.left.is_none() && err.to_string().contains("changed"),
        "{err}"
    );
    assert_eq!(std::fs::read(&archive).unwrap(), other);
}

#[cfg(unix)]
#[test]
fn a_link_a_second_name_and_a_read_only_file_are_refused() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Scratch::new("place");
    let archive = tmp.path("Game.zip");
    let bytes = zip_bytes(&game(), ZipShape::default());
    std::fs::write(&archive, &bytes).unwrap();

    let link = tmp.path("Link.zip");
    std::os::unix::fs::symlink(&archive, &link).unwrap();
    let e = format!("{:#}", plan(&link, None).unwrap_err());
    assert!(e.contains("is a link"), "{e}");
    std::fs::remove_file(&link).unwrap();

    let twin = tmp.path("Twin.zip");
    std::fs::hard_link(&archive, &twin).unwrap();
    let e = format!("{:#}", plan(&archive, None).unwrap_err());
    assert!(e.contains("second name"), "{e}");
    std::fs::remove_file(&twin).unwrap();

    std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o444)).unwrap();
    let e = format!("{:#}", plan(&archive, None).unwrap_err());
    assert!(e.contains("read-only"), "{e}");
    std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(std::fs::read(&archive).unwrap(), bytes);
    plan(&archive, None).unwrap();
    nothing_left_beside(&archive);
}

fn sevenz_bytes(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    for (name, bytes) in files {
        // An empty file is listed with no data of its own, as 7-Zip writes it.
        let data = (!bytes.is_empty()).then(|| std::io::Cursor::new(bytes.clone()));
        w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), data)
            .unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// The same files in one solid block.
fn sevenz_solid_bytes(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    use sevenz_rust2::{ArchiveEntry, SourceReader};
    let mut w = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    let entries: Vec<ArchiveEntry> = files
        .iter()
        .map(|(n, _)| ArchiveEntry::new_file(n))
        .collect();
    let readers: Vec<SourceReader<std::io::Cursor<Vec<u8>>>> = files
        .iter()
        .map(|(_, b)| SourceReader::new(std::io::Cursor::new(b.clone())))
        .collect();
    w.push_archive_entries(entries, readers).unwrap();
    w.finish().unwrap().into_inner()
}

/// A `.7z` whose files are each a block of their own is unpacked from its last block
/// backwards, and cut short behind each one.
#[test]
fn a_7z_of_several_blocks_is_unpacked_from_its_end_and_shrinks_as_it_goes() {
    let tmp = Scratch::new("7z-blocks");
    let archive = tmp.path("Game.7z");
    let files = game();
    let bytes = sevenz_bytes(&files);
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    assert_eq!(plan.mode, FreeMode::Blocks);
    // The empty file has no block.
    assert_eq!(plan.pieces, 4);
    assert_eq!(plan.empty_files, ["Game/data/empty.bin"]);
    assert!(plan.peak_beside < plan.unpacked, "{}", plan.peak_beside);

    let dest = tmp.path("out");
    let partial = tmp.path("Game.7z.partial-deleted");
    let mut sizes = Vec::new();
    consume(
        &plan,
        &dest,
        None,
        &mut |_, _| {
            let len = std::fs::metadata(&archive)
                .or_else(|_| std::fs::metadata(&partial))
                .map(|m| m.len())
                .unwrap_or(0);
            sizes.push(len);
        },
        &no_stop(),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(tree(&dest), want(&files));
    assert!(sizes.windows(2).all(|w| w[1] <= w[0]), "{sizes:?}");
    sizes.dedup();
    assert!(sizes.len() >= 3, "cut as it went: {sizes:?}");
    assert!(!archive.exists() && !partial.exists());
    nothing_left_beside(&archive);

    // Stopped half way: what is out is whole, and what is left is renamed and no archive.
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    let dest = tmp.path("out2");
    let stop = no_stop();
    let unpacked = plan.unpacked;
    let err = consume(
        &plan,
        &dest,
        None,
        &mut |done, _| {
            if done >= unpacked + 150_000 {
                stop.store(true, Ordering::Relaxed);
            }
        },
        &stop,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(err.left.as_deref(), Some(&[partial.clone()][..]));
    assert!(!archive.exists());
    let left = std::fs::metadata(&partial).unwrap().len();
    assert!(left > 0 && left < bytes.len() as u64, "{left}");
    let have = tree(&dest);
    assert!(
        have.contains_key("Game/data/sub/b.bin"),
        "{:?}",
        have.keys()
    );
    for (name, bytes) in &have {
        assert_eq!(Some(bytes), want(&files).get(name), "{name}");
    }
}

/// Blocks of several files each (a solid archive packed with a block size limit): every file
/// of a block is out before the block goes.
#[test]
fn a_7z_of_two_solid_blocks_is_freed_one_block_at_a_time() {
    use sevenz_rust2::{ArchiveEntry, SourceReader};
    let tmp = Scratch::new("7z-solid-blocks");
    let archive = tmp.path("Game.7z");
    let files: Vec<(&str, Vec<u8>)> = game().into_iter().filter(|(_, b)| !b.is_empty()).collect();
    let mut w = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    for half in files.chunks(2) {
        let entries: Vec<ArchiveEntry> = half
            .iter()
            .map(|(n, _)| ArchiveEntry::new_file(n))
            .collect();
        let readers: Vec<SourceReader<std::io::Cursor<Vec<u8>>>> = half
            .iter()
            .map(|(_, b)| SourceReader::new(std::io::Cursor::new(b.clone())))
            .collect();
        w.push_archive_entries(entries, readers).unwrap();
    }
    let bytes = w.finish().unwrap().into_inner();
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan_with(&archive, None, EACH).unwrap();
    assert_eq!((plan.mode, plan.pieces), (FreeMode::Blocks, 2));
    let dest = tmp.path("out");
    let partial = tmp.path("Game.7z.partial-deleted");
    let mut sizes = Vec::new();
    consume(
        &plan,
        &dest,
        None,
        &mut |_, _| {
            let len = std::fs::metadata(&archive)
                .or_else(|_| std::fs::metadata(&partial))
                .map(|m| m.len())
                .unwrap_or(0);
            sizes.push(len);
        },
        &no_stop(),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(tree(&dest), want(&files));
    sizes.dedup();
    // Whole, then without its last block.
    assert_eq!(sizes.len(), 2, "{sizes:?}");
    assert!(sizes[1] < sizes[0]);
    assert!(!archive.exists() && !partial.exists());
}

/// With the usual threshold a small `.7z` of several blocks is cut once, at its end.
#[test]
fn a_small_7z_of_several_blocks_comes_out_whole() {
    let tmp = Scratch::new("7z-once");
    let archive = tmp.path("Game.7z");
    let files = game();
    std::fs::write(&archive, sevenz_bytes(&files)).unwrap();
    let plan = plan(&archive, None).unwrap();
    assert_eq!(plan.mode, FreeMode::Blocks);
    assert_eq!(plan.peak_beside, plan.unpacked);
    let dest = tmp.path("out");
    consume(&plan, &dest, None, &mut |_, _| {}, &no_stop(), &mut || {
        Ok(())
    })
    .unwrap();
    assert_eq!(tree(&dest), want(&files));
    assert!(!archive.exists());
}

/// A `.7z` in one block is one stream from the front: it is whole until the unpack is over
/// and the game found, then deleted at once.
#[test]
fn a_7z_in_one_block_is_deleted_only_once_everything_is_unpacked_and_the_game_found() {
    let tmp = Scratch::new("7z");
    let archive = tmp.path("Game.7z");
    let files = game();
    let bytes = sevenz_solid_bytes(&files);
    std::fs::write(&archive, &bytes).unwrap();
    let plan = plan(&archive, None).unwrap();
    assert_eq!(plan.mode, FreeMode::End);
    assert_eq!(plan.peak_beside, plan.unpacked);

    // No game found (the caller's check fails): the archive stays as it was.
    let err = consume(
        &plan,
        &tmp.path("out1"),
        None,
        &mut |_, _| {},
        &no_stop(),
        &mut || bail!("no game"),
    )
    .unwrap_err();
    assert!(err.left.is_none());
    assert_eq!(std::fs::read(&archive).unwrap(), bytes);

    let dest = tmp.path("out2");
    let mut whole_when_found = false;
    let mut last = (0, 0);
    consume(
        &plan,
        &dest,
        None,
        &mut |done, total| last = (done, total),
        &no_stop(),
        &mut || {
            whole_when_found = std::fs::read(&archive).is_ok_and(|b| b == bytes);
            Ok(())
        },
    )
    .unwrap();
    assert!(whole_when_found);
    assert_eq!(last, (plan.unpacked, plan.unpacked));
    assert_eq!(tree(&dest), want(&files));
    assert!(!archive.exists());
    nothing_left_beside(&archive);

    // Stopped on the way: whole.
    std::fs::write(&archive, &bytes).unwrap();
    let plan = super::plan(&archive, None).unwrap();
    let stop = no_stop();
    let err = consume(
        &plan,
        &tmp.path("out3"),
        None,
        &mut |done, _| {
            if done > 0 {
                stop.store(true, Ordering::Relaxed)
            }
        },
        &stop,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(err.left.is_none());
    assert_eq!(std::fs::read(&archive).unwrap(), bytes);
}

#[test]
fn the_game_is_looked_for_in_the_listing_as_it_is_in_the_folder() {
    let kind = |names: &[&str]| game_in_names(names.iter().copied());
    assert_eq!(
        kind(&["sce_sys/param.json", "eboot.bin"]).unwrap(),
        GameKind::Folder
    );
    assert_eq!(
        kind(&["[X] Y/PPSA1-app/sce_sys/param.sfo", "[X] Y/readme.txt"]).unwrap(),
        GameKind::Folder
    );
    assert_eq!(
        kind(&["x/Game.exfat", "x/readme.txt"]).unwrap(),
        GameKind::Image
    );
    assert_eq!(kind(&["Game.PKG"]).unwrap(), GameKind::Image);
    // A game inside the game, an image inside it, and macOS's sidecars are part of it or nothing.
    assert_eq!(
        kind(&[
            "g/sce_sys/param.json",
            "g/data/sub/sce_sys/param.json",
            "g/data/x.pkg",
            "__MACOSX/g/sce_sys/param.json"
        ])
        .unwrap(),
        GameKind::Folder
    );
    let e = kind(&["a/sce_sys/param.json", "b/sce_sys/param.json"])
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("more than one game") && e.contains("a, b"),
        "{e}"
    );
    let e = kind(&["g/sce_sys/param.json", "other.exfat"])
        .unwrap_err()
        .to_string();
    assert!(e.contains("more than one game"), "{e}");
    assert!(kind(&["readme.txt"])
        .unwrap_err()
        .to_string()
        .contains("no game"));
    // Deeper than an unpacked folder is searched.
    assert!(kind(&["1/2/3/4/5/sce_sys/param.json"]).is_err());
    assert!(kind(&["1/2/3/4/sce_sys/param.json"]).is_ok());
    // Not a param file: only a name ending like one.
    assert!(kind(&["notsce_sys/param.json"]).is_err());
}

#[test]
fn a_linux_mount_table_says_which_drives_are_another_computers() {
    let table = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
40 22 0:35 / /mnt/nas rw,relatime shared:20 - nfs4 10.0.0.5:/games rw,vers=4.2
41 22 0:36 / /mnt/My\\040Share rw - cifs //nas/share rw
42 22 8:17 / /media/usb ro,nosuid - fuseblk /dev/sdb1 ro
43 22 0:40 / /home/u/remote rw - fuse.sshfs u@host: rw
44 22 8:33 / /media/ext rw - fuseblk /dev/sdc1 rw
";
    let at = |p: &str| mount_of(table, Path::new(p)).unwrap();
    assert_eq!(at("/home/u/Game.zip"), ("ext4".into(), false));
    assert_eq!(at("/mnt/nas/Game.zip").0, "nfs4");
    assert_eq!(at("/mnt/My Share/x/Game.zip").0, "cifs");
    assert_eq!(at("/media/usb/Game.zip"), ("fuseblk".into(), true));
    assert_eq!(at("/home/u/remote/Game.zip").0, "fuse.sshfs");
    // A folder whose name only starts like a mount's is not on it.
    assert_eq!(at("/mnt/nasty/Game.zip").0, "ext4");
    for net in [
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "9p",
        "fuse.sshfs",
        "fuse.rclone",
        "vboxsf",
    ] {
        assert!(network_fs(net), "{net}");
    }
    for local in [
        "ext4", "btrfs", "xfs", "exfat", "ntfs3", "fuseblk", "apfs", "overlay", "zfs",
    ] {
        assert!(!network_fs(local), "{local}");
    }
}

#[test]
fn a_cut_archive_is_told_by_its_name() {
    for yes in [
        "Game.zip.partial-deleted",
        "Game.part3.rar.partial-deleted-2",
        "G.ZIP.PARTIAL-DELETED",
    ] {
        assert!(is_partial_name(yes), "{yes}");
    }
    for no in [
        "Game.zip",
        "Game.zip.partial",
        "Game.zip.partial-deleted-x",
        "partial-deleted.zip",
    ] {
        assert!(!is_partial_name(no), "{no}");
    }
    let tmp = Scratch::new("names");
    let a = tmp.path("Game.zip");
    assert_eq!(partial_name(&a), tmp.path("Game.zip.partial-deleted"));
    std::fs::write(tmp.path("Game.zip.partial-deleted"), b"x").unwrap();
    assert_eq!(partial_name(&a), tmp.path("Game.zip.partial-deleted-2"));
    assert!(same_drive(&tmp.0, &tmp.path("Game.zip.partial-deleted")));
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod rar {
    use super::*;

    /// The multi-part sets the console's extractor is tested on.
    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../payload/extractor/tests/fixtures")
    }

    /// The parts of the set named `prefix` copied into `into`, in order.
    fn copy_set(prefix: &str, into: &Path) -> Vec<PathBuf> {
        let mut names: Vec<String> = std::fs::read_dir(fixtures())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(prefix))
            .collect();
        names.sort();
        assert!(names.len() > 1, "{prefix}: {names:?}");
        names
            .iter()
            .map(|n| {
                let to = into.join(n);
                std::fs::copy(fixtures().join(n), &to).unwrap();
                to
            })
            .collect()
    }

    const ANY: Wants = Wants {
        cut_every: 0,
        game: false,
    };

    fn plain_unpack(first: &Path, into: &Path) {
        crate::archive_extract::extract(first, into, None, &mut |_, _| {}, &no_stop()).unwrap();
    }

    #[test]
    fn a_sets_parts_go_one_by_one_in_order_and_the_files_come_out_the_same() {
        for prefix in ["rar4-multi.part", "rar5-solid.part"] {
            let tmp = Scratch::new("rar");
            let parts = copy_set(prefix, &tmp.0);
            // What a plain unpack of the same set gives.
            let keep = Scratch::new("rar-keep");
            let twin = copy_set(prefix, &keep.0);
            plain_unpack(&twin[0], &keep.path("out"));
            let expect = tree(&keep.path("out"));
            assert!(!expect.is_empty());

            let plan =
                plan_with(&parts[0], None, ANY).unwrap_or_else(|e| panic!("{prefix}: {e:#}"));
            assert_eq!(plan.mode, FreeMode::Parts, "{prefix}");
            assert_eq!(plan.parts, parts, "{prefix}");
            let steps = plan.rar.as_ref().unwrap();
            assert_eq!(steps.starts.len(), parts.len());
            assert!(
                steps.starts.windows(2).all(|w| w[0] <= w[1]),
                "{:?}",
                steps.starts
            );
            assert_eq!(
                plan.archive_bytes,
                parts
                    .iter()
                    .map(|p| std::fs::metadata(p).unwrap().len())
                    .sum::<u64>()
            );

            let dest = tmp.path("out");
            // Which parts are there each time progress is reported.
            let mut seen: Vec<Vec<bool>> = Vec::new();
            let out = consume(
                &plan,
                &dest,
                None,
                &mut |_, _| {
                    let now: Vec<bool> = parts.iter().map(|p| p.exists()).collect();
                    if seen.last() != Some(&now) {
                        seen.push(now);
                    }
                },
                &no_stop(),
                &mut || Ok(()),
            )
            .unwrap_or_else(|e| panic!("{prefix}: {e}"));
            assert!(out.not_removed.is_empty(), "{prefix}");
            assert_eq!(tree(&dest), expect, "{prefix}");
            assert!(parts.iter().all(|p| !p.exists()), "{prefix}");
            // Only ever the first ones gone, never one put back.
            for now in &seen {
                let gone = now.iter().take_while(|there| !**there).count();
                assert!(now[gone..].iter().all(|there| *there), "{prefix}: {seen:?}");
            }
            assert!(seen.windows(2).all(|w| {
                w[0].iter().filter(|t| **t).count() >= w[1].iter().filter(|t| **t).count()
            }));
            nothing_left_beside(&parts[0]);
        }
    }

    #[test]
    fn a_set_stopped_half_way_keeps_its_files_and_renames_the_parts_left() {
        let tmp = Scratch::new("rar-stop");
        let parts = copy_set("rar4-multi.part", &tmp.0);
        let keep = Scratch::new("rar-stop-keep");
        let twin = copy_set("rar4-multi.part", &keep.0);
        plain_unpack(&twin[0], &keep.path("out"));
        let expect = tree(&keep.path("out"));

        let plan = plan_with(&parts[0], None, ANY).unwrap();
        let dest = tmp.path("out");
        let stop = no_stop();
        // Stopped as soon as the first part is gone.
        let err = consume(
            &plan,
            &dest,
            None,
            &mut |_, _| {
                if !parts[0].exists() {
                    stop.store(true, Ordering::Relaxed);
                }
            },
            &stop,
            &mut || Ok(()),
        );
        let Err(err) = err else {
            // A set whose one file spans every part frees nothing until its end: nothing to
            // stop in the middle of.
            assert!(plan
                .rar
                .as_ref()
                .unwrap()
                .starts
                .iter()
                .all(|s| *s == 0 || *s == plan.rar.as_ref().unwrap().files.len()));
            return;
        };
        let left = err.left.clone().expect("part of the set is gone");
        assert!(!left.is_empty());
        for p in &left {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(is_partial_name(&name) && p.is_file(), "{name}");
        }
        assert!(
            parts.iter().all(|p| !p.exists()),
            "no part keeps its own name"
        );
        for (name, bytes) in tree(&dest) {
            assert_eq!(Some(&bytes), expect.get(&name), "{name}");
        }
    }

    #[test]
    fn a_set_with_a_part_missing_or_not_chosen_by_its_first_is_refused_whole() {
        let tmp = Scratch::new("rar-refuse");
        let parts = copy_set("rar4-multi.part", &tmp.0);
        let sizes: Vec<u64> = parts
            .iter()
            .map(|p| std::fs::metadata(p).unwrap().len())
            .collect();
        let e = format!("{:#}", plan_with(&parts[1], None, ANY).unwrap_err());
        assert!(e.contains("not the first part"), "{e}");
        std::fs::remove_file(&parts[2]).unwrap();
        assert!(plan_with(&parts[0], None, ANY).is_err());
        for (p, size) in parts.iter().zip(&sizes) {
            if p != &parts[2] {
                assert_eq!(std::fs::metadata(p).unwrap().len(), *size);
            }
        }
        nothing_left_beside(&parts[0]);
    }

    #[test]
    fn a_one_file_rar_is_deleted_at_the_end() {
        let tmp = Scratch::new("rar-one");
        let archive = tmp.path("rar5.rar");
        std::fs::copy(fixtures().join("rar5.rar"), &archive).unwrap();
        let keep = Scratch::new("rar-one-keep");
        plain_unpack(&fixtures().join("rar5.rar"), &keep.path("out"));
        let plan = plan_with(&archive, None, ANY).unwrap();
        assert_eq!(plan.mode, FreeMode::End);
        let dest = tmp.path("out");
        let mut whole = false;
        consume(&plan, &dest, None, &mut |_, _| {}, &no_stop(), &mut || {
            whole = archive.is_file();
            Ok(())
        })
        .unwrap();
        assert!(whole && !archive.exists());
        assert_eq!(tree(&dest), tree(&keep.path("out")));
    }

    #[test]
    fn a_sets_parts_are_found_by_both_ways_of_naming_them() {
        let tmp = Scratch::new("rar-names");
        for n in [
            "G.part01.rar",
            "G.part02.rar",
            "G.part03.rar",
            "G.part05.rar",
            "Old.rar",
            "Old.r00",
            "Old.r01",
            "One.rar",
        ] {
            std::fs::write(tmp.path(n), b"x").unwrap();
        }
        let names = |first: &str| -> Vec<String> {
            rar_parts(&tmp.path(first))
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        // A gap ends the run (the listing then says a part is missing).
        assert_eq!(
            names("G.part01.rar"),
            ["G.part01.rar", "G.part02.rar", "G.part03.rar"]
        );
        assert_eq!(names("Old.rar"), ["Old.rar", "Old.r00", "Old.r01"]);
        assert_eq!(names("One.rar"), ["One.rar"]);
        assert!(rar_parts(&tmp.path("G.part02.rar")).is_err());
    }
}
