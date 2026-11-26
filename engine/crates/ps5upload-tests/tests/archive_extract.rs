//! Convert's archive inputs: unpack a .zip / .7z / .rar to a folder and find the game in it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ps5upload_core::archive_extract::{
    archive_kind, extract, find_game, unpacked_size, ArchiveKind,
};

fn scratch(name: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let d = crate::test_tmp::temp_path(format!(
        "ps5upload-extract-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const PARAM: &[u8] = br#"{"titleId":"PPSA01234"}"#;

fn game(prefix: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        (format!("{prefix}sce_sys/param.json"), PARAM.to_vec()),
        (format!("{prefix}eboot.bin"), vec![7u8; 70_000]),
        (format!("{prefix}data/deep/a.bin"), b"deep".to_vec()),
    ]
}

fn zip_of(dir: &Path, entries: &[(String, Vec<u8>)]) -> PathBuf {
    let p = dir.join("game.zip");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        w.start_file(name.as_str(), opts).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap();
    p
}

fn sevenz_of(dir: &Path, entries: &[(String, Vec<u8>)]) -> PathBuf {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let p = dir.join("game.7z");
    let mut w = ArchiveWriter::create(&p).unwrap();
    for (name, data) in entries {
        let mut e = ArchiveEntry::new();
        e.name = name.clone();
        e.has_stream = !data.is_empty();
        w.push_archive_entry::<&[u8]>(e, Some(&data[..])).unwrap();
    }
    w.finish().unwrap();
    p
}

fn run(archive: &Path, dest: &Path, password: Option<&str>) -> anyhow::Result<(u64, u64)> {
    let never = AtomicBool::new(false);
    let mut last = (0, 0);
    extract(archive, dest, password, &mut |d, t| last = (d, t), &never)?;
    Ok(last)
}

#[test]
fn kinds_by_name() {
    assert_eq!(archive_kind(Path::new("/a/G.ZIP")), Some(ArchiveKind::Zip));
    assert_eq!(
        archive_kind(Path::new("/a/g.7z")),
        Some(ArchiveKind::SevenZ)
    );
    assert_eq!(
        archive_kind(Path::new("/a/g.part1.rar")),
        Some(ArchiveKind::Rar)
    );
    assert_eq!(archive_kind(Path::new("/a/g.exfat")), None);
}

#[test]
fn a_zip_unpacks_to_the_game_with_progress() {
    let dir = scratch("zip");
    let entries = game("");
    let arc = zip_of(&dir, &entries);
    let total: u64 = entries.iter().map(|(_, b)| b.len() as u64).sum();
    assert_eq!(unpacked_size(&arc, None).unwrap(), total);
    let out = dir.join("out");
    let (done, t) = run(&arc, &out, None).unwrap();
    assert_eq!((done, t), (total, total));
    for (name, bytes) in &entries {
        assert_eq!(&std::fs::read(out.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(find_game(&out).unwrap(), out);
}

#[test]
fn a_game_wrapped_in_a_folder_is_found_inside_it() {
    let dir = scratch("wrapped");
    let arc = sevenz_of(&dir, &game("My Game (EU)/"));
    let out = dir.join("out");
    run(&arc, &out, None).unwrap();
    assert_eq!(find_game(&out).unwrap(), out.join("My Game (EU)"));
}

#[test]
fn an_image_in_an_archive_is_the_game() {
    let dir = scratch("image");
    let arc = zip_of(
        &dir,
        &[
            ("dump/PPSA01234.exfat".to_string(), vec![1u8; 4096]),
            ("dump/readme.txt".to_string(), b"hi".to_vec()),
        ],
    );
    let out = dir.join("out");
    run(&arc, &out, None).unwrap();
    assert_eq!(find_game(&out).unwrap(), out.join("dump/PPSA01234.exfat"));
}

#[test]
fn no_game_or_two_games_is_refused_with_the_reason() {
    let dir = scratch("none");
    let out = dir.join("none");
    std::fs::create_dir_all(out.join("x")).unwrap();
    std::fs::write(out.join("x/readme.txt"), b"hi").unwrap();
    assert!(find_game(&out).unwrap_err().to_string().contains("no game"));

    let two = dir.join("two");
    for g in ["a", "b"] {
        std::fs::create_dir_all(two.join(g).join("sce_sys")).unwrap();
        std::fs::write(two.join(g).join("sce_sys/param.json"), PARAM).unwrap();
    }
    let e = find_game(&two).unwrap_err().to_string();
    assert!(e.contains("more than one game"), "{e}");
}

#[test]
fn an_entry_escaping_the_folder_is_refused() {
    let dir = scratch("slip");
    let arc = zip_of(&dir, &[("../evil.txt".to_string(), b"x".to_vec())]);
    let out = dir.join("out");
    assert!(run(&arc, &out, None).is_err());
    assert!(!dir.join("evil.txt").exists());
}

#[test]
fn cancel_stops_the_unpack() {
    let dir = scratch("cancel");
    let arc = zip_of(&dir, &game(""));
    let stop = AtomicBool::new(true);
    let e = extract(&arc, &dir.join("out"), None, &mut |_, _| {}, &stop).unwrap_err();
    assert!(e.to_string().contains("cancel"), "{e}");
}

#[cfg(not(target_os = "android"))]
#[test]
fn a_password_rar_needs_its_password() {
    let arc =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ps5upload-core/testdata/rar/crypted.rar");
    let dir = scratch("rar");
    let e = run(&arc, &dir.join("a"), None).unwrap_err().to_string();
    assert!(e.contains("rar_password_required"), "{e}");
    let e = run(&arc, &dir.join("b"), Some("nope"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("rar_password_wrong"), "{e}");
    run(&arc, &dir.join("c"), Some("unrar")).unwrap();
    assert_eq!(
        std::fs::read(dir.join("c/.gitignore")).unwrap(),
        b"target\nCargo.lock\n"
    );
}

// Test scratch paths removed when the test process exits (engine/test-support/test_tmp.rs).
#[path = "../../../test-support/test_tmp.rs"]
mod test_tmp;
