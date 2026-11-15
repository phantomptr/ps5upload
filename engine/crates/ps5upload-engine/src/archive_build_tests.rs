//! Image and folder builds read straight from an archive (`archive_tree`): a small game in a
//! `.zip`, a `.7z` (solid and not), a `.rar` (solid, not, and a two-part set), each built and
//! read back from what was written; and the refusals (two games, a truncated archive, a game
//! image inside, a multi-part set's removal).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use uuid::Uuid;

use crate::fpkg_api::{exfat_build_handler, folder_build_handler, ExfatBuildReq, FolderBuildReq};
use crate::{AppState, JobState};

type Jobs = Arc<Mutex<std::collections::HashMap<Uuid, JobState>>>;

fn state() -> (AppState, Jobs) {
    let jobs: Jobs = Arc::default();
    let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
    let state = AppState {
        jobs: jobs.clone(),
        default_ps5_addr: "127.0.0.1".to_string(),
        events_tx,
    };
    (state, jobs)
}

fn root(tag: &str) -> PathBuf {
    let r = std::env::temp_dir().join(format!("ps5u-archive-build-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    std::fs::create_dir_all(&r).unwrap();
    r
}

/// A small game: files of many sizes (some over the 64 KiB chunk, one over 1 MiB), in folders.
fn game() -> Vec<(String, Vec<u8>)> {
    let bytes = |seed: usize, n: usize| -> Vec<u8> {
        (0..n)
            .map(|i| (seed * 131 + i * 7 + i / 509) as u8)
            .collect()
    };
    vec![
        (
            "sce_sys/param.json".into(),
            br#"{"titleId":"PPSA01234"}"#.to_vec(),
        ),
        ("eboot.bin".into(), b"eboot".to_vec()),
        ("sce_sys/icon0.png".into(), bytes(1, 3000)),
        ("data/a.bin".into(), bytes(2, 200_000)),
        ("data/b/c.bin".into(), bytes(3, 70_000)),
        ("data/zz.bin".into(), bytes(4, 10)),
        ("Data2/big.bin".into(), bytes(5, 1_300_000)),
        ("data/b/aa.bin".into(), bytes(6, 65)),
    ]
}

/// The game under `prefix` (`""` or `dir/`), with one empty folder and some junk.
fn entries(prefix: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = game()
        .into_iter()
        .map(|(p, d)| (format!("{prefix}{p}"), d))
        .collect();
    out.push(("__MACOSX/._junk".into(), b"junk".to_vec()));
    out
}

fn write_zip(path: &Path, files: &[(String, Vec<u8>)], empty_dir: Option<&str>) {
    let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (p, d) in files {
        zw.start_file(p.as_str(), opts).unwrap();
        zw.write_all(d).unwrap();
    }
    if let Some(d) = empty_dir {
        zw.add_directory(d, opts).unwrap();
    }
    zw.finish().unwrap();
}

fn write_7z(path: &Path, files: &[(String, Vec<u8>)], solid: bool) {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
    let mut w = ArchiveWriter::create(path).unwrap();
    if solid {
        let entries: Vec<ArchiveEntry> = files
            .iter()
            .map(|(p, _)| ArchiveEntry::new_file(p))
            .collect();
        let readers: Vec<SourceReader<std::io::Cursor<Vec<u8>>>> = files
            .iter()
            .map(|(_, d)| SourceReader::new(std::io::Cursor::new(d.clone())))
            .collect();
        w.push_archive_entries(entries, readers).unwrap();
    } else {
        for (p, d) in files {
            w.push_archive_entry(ArchiveEntry::new_file(p), Some(d.as_slice()))
                .unwrap();
        }
    }
    w.finish().unwrap();
}

// ── a RAR5 stored writer (no `rar` tool in the toolchain; as ps5upload-ava1's tests do) ──────

fn vint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

fn block(body: &[u8]) -> Vec<u8> {
    let mut hdr = Vec::new();
    vint(body.len() as u64, &mut hdr);
    hdr.extend_from_slice(body);
    let mut out = crc32(&hdr).to_le_bytes().to_vec();
    out.extend_from_slice(&hdr);
    out
}

/// One file header and its data: `part` of `whole`, `flags` the split bits (0x08 continued
/// from the previous volume, 0x10 continued in the next), `solid` for all but the first.
fn rar_file(name: &str, part: &[u8], whole: &[u8], flags: u64, solid: bool) -> Vec<u8> {
    let mut h = Vec::new();
    vint(2, &mut h); // file header
    vint(2 | flags, &mut h); // data area follows
    vint(part.len() as u64, &mut h);
    vint(4, &mut h); // data CRC32 present
    vint(whole.len() as u64, &mut h);
    vint(0o644, &mut h);
    h.extend_from_slice(&crc32(whole).to_le_bytes());
    vint(if solid { 0x40 } else { 0 }, &mut h); // stored; solid bit
    vint(1, &mut h); // unix
    vint(name.len() as u64, &mut h);
    h.extend_from_slice(name.as_bytes());
    let mut out = block(&h);
    out.extend_from_slice(part);
    out
}

fn rar_main(solid: bool, volume: Option<u64>) -> Vec<u8> {
    let mut main = Vec::new();
    vint(1, &mut main);
    vint(0, &mut main);
    let mut flags = if solid { 4 } else { 0 };
    if let Some(n) = volume {
        flags |= 1;
        if n > 0 {
            flags |= 2;
        }
    }
    vint(flags, &mut main);
    if let Some(n) = volume.filter(|&n| n > 0) {
        vint(n, &mut main);
    }
    block(&main)
}

fn rar_end(more: bool) -> Vec<u8> {
    let mut e = Vec::new();
    vint(5, &mut e);
    vint(0, &mut e);
    vint(u64::from(more), &mut e);
    block(&e)
}

fn write_rar(path: &Path, files: &[(String, Vec<u8>)], solid: bool) {
    let mut out = b"Rar!\x1a\x07\x01\x00".to_vec();
    out.extend(rar_main(solid, None));
    for (i, (p, d)) in files.iter().enumerate() {
        out.extend(rar_file(p, d, d, 0, solid && i > 0));
    }
    out.extend(rar_end(false));
    std::fs::write(path, out).unwrap();
}

/// A two-part set `<stem>.part1.rar` + `<stem>.part2.rar`, the file at `split` cut across them.
fn write_rar_parts(dir: &Path, stem: &str, files: &[(String, Vec<u8>)], split: usize) -> PathBuf {
    let sig = b"Rar!\x1a\x07\x01\x00".to_vec();
    let (mut one, mut two) = (sig.clone(), sig);
    one.extend(rar_main(false, Some(0)));
    two.extend(rar_main(false, Some(1)));
    for (i, (p, d)) in files.iter().enumerate() {
        if i < split {
            one.extend(rar_file(p, d, d, 0, false));
        } else if i == split {
            let cut = d.len() / 2;
            one.extend(rar_file(p, &d[..cut], d, 0x10, false));
            two.extend(rar_file(p, &d[cut..], d, 0x08, false));
        } else {
            two.extend(rar_file(p, d, d, 0, false));
        }
    }
    one.extend(rar_end(true));
    two.extend(rar_end(false));
    let first = dir.join(format!("{stem}.part1.rar"));
    std::fs::write(&first, one).unwrap();
    std::fs::write(dir.join(format!("{stem}.part2.rar")), two).unwrap();
    first
}

fn rar_helper_here() -> bool {
    let here = ps5upload_core::rar_helper::helper_path().is_file();
    if !here {
        eprintln!(
            "skipped: the ps5upload-unrar helper is not built (cargo build -p ps5upload-unrar)"
        );
    }
    here
}

// ── running a build ─────────────────────────────────────────────────────────────────────────

/// The status and body of the request, and the job's end (`dest` or the error) when it started.
async fn run(
    image: bool,
    body: serde_json::Value,
) -> (StatusCode, String, Option<Result<String, String>>) {
    let (state, jobs) = state();
    let resp = if image {
        let req: ExfatBuildReq = serde_json::from_value(body).unwrap();
        exfat_build_handler(State(state), Json(req))
            .await
            .into_response()
    } else {
        let req: FolderBuildReq = serde_json::from_value(body).unwrap();
        folder_build_handler(State(state), Json(req))
            .await
            .into_response()
    };
    let status = resp.status();
    let text = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&text).into_owned();
    if status != StatusCode::ACCEPTED {
        return (status, text, None);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let end = jobs.lock().unwrap().values().find_map(|j| match j {
            JobState::Done { dest, .. } => Some(Ok(dest.clone())),
            JobState::Failed { error, .. } => Some(Err(error.clone())),
            _ => None,
        });
        if let Some(end) = end {
            return (status, text, Some(end));
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the build did not end"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What was written holds the game exactly: every file's bytes and the empty folder.
fn read_back(out: &Path, empty_dir: bool) {
    let mut tree = ps5upload_fpkg::source::open(out).unwrap();
    let want: BTreeMap<String, Vec<u8>> = game().into_iter().collect();
    let got: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
    let mut got_sorted = got.clone();
    got_sorted.sort();
    // An image adds nothing for this game; the folder carries no marker once named.
    assert_eq!(
        got_sorted,
        want.keys().cloned().collect::<Vec<_>>(),
        "files of {}",
        out.display()
    );
    for (p, d) in &want {
        assert_eq!(&tree.read(p).unwrap(), d, "{p} in {}", out.display());
    }
    if empty_dir {
        assert!(
            tree.empty_dirs()
                .iter()
                .any(|d| d.trim_matches('/') == "data/empty"),
            "the empty folder in {}: {:?}",
            out.display(),
            tree.empty_dirs()
        );
    }
}

async fn build_both(archive: &Path, out: &Path, password: Option<&str>, empty_dir: bool) {
    let (status, body, end) = run(
        true,
        serde_json::json!({
            "source": archive.display().to_string(),
            "output_dir": out.display().to_string(),
            "full_verify": true,
            "archive_password": password,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}: {body}",
        archive.display()
    );
    let image = PathBuf::from(
        end.unwrap()
            .unwrap_or_else(|e| panic!("{}: {e}", archive.display())),
    );
    assert_eq!(
        image.file_name().unwrap().to_string_lossy(),
        "Game.exfat",
        "named after the archive"
    );
    read_back(&image, empty_dir);
    let (status, body, end) = run(
        false,
        serde_json::json!({
            "source": archive.display().to_string(),
            "output_dir": out.display().to_string(),
            "archive_password": password,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}: {body}",
        archive.display()
    );
    let folder = PathBuf::from(
        end.unwrap()
            .unwrap_or_else(|e| panic!("{}: {e}", archive.display())),
    );
    read_back(&folder, empty_dir);
    std::fs::remove_file(&image).unwrap();
    std::fs::remove_dir_all(&folder).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zip_becomes_an_image_and_a_folder_with_nothing_unpacked() {
    let r = root("zip");
    let out = r.join("out");
    let archive = r.join("Game.zip");
    write_zip(
        &archive,
        &entries("PPSA01234-app/"),
        Some("PPSA01234-app/data/empty/"),
    );
    build_both(&archive, &out, None, true).await;
    // Nothing but the outputs was written: no unpack folder anywhere.
    let left: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_7z_solid_or_not_becomes_an_image_and_a_folder() {
    for solid in [false, true] {
        let r = root(if solid { "7z-solid" } else { "7z" });
        let out = r.join("out");
        let archive = r.join("Game.7z");
        write_7z(&archive, &entries(""), solid);
        build_both(&archive, &out, None, false).await;
        let _ = std::fs::remove_dir_all(&r);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rar_solid_or_not_and_a_two_part_set_become_images() {
    if !rar_helper_here() {
        return;
    }
    for solid in [false, true] {
        let r = root(if solid { "rar-solid" } else { "rar" });
        let out = r.join("out");
        let archive = r.join("Game.rar");
        write_rar(&archive, &entries("PPSA01234/"), solid);
        build_both(&archive, &out, None, false).await;
        let _ = std::fs::remove_dir_all(&r);
    }
    // Two parts, the large file cut across them; named without `.part1`.
    let r = root("rar-parts");
    let out = r.join("out");
    let files = entries("");
    let split = files
        .iter()
        .position(|(p, _)| p == "Data2/big.bin")
        .unwrap();
    let first = write_rar_parts(&r, "Game", &files, split);
    build_both(&first, &out, None, false).await;
    // Its source is never moved to the Trash alone: the other part would stay.
    let (status, body, _) = run(
        true,
        serde_json::json!({
            "source": first.display().to_string(),
            "output_dir": out.display().to_string(),
            "delete_source": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("multi-part"), "{body}");
    // A missing part is named.
    std::fs::remove_file(r.join("Game.part2.rar")).unwrap();
    let (status, body, end) = run(
        true,
        serde_json::json!({
            "source": first.display().to_string(),
            "output_dir": out.display().to_string(),
        }),
    )
    .await;
    let said = match end {
        Some(Err(e)) => e,
        _ => body.clone(),
    };
    assert!(
        status == StatusCode::BAD_REQUEST || matches!(status, StatusCode::ACCEPTED),
        "{body}"
    );
    // The set's first part alone can't tell a lost last part from the end; UnRAR says so.
    assert!(
        said.contains("rar_missing_volume") || said.contains("volume"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_that_cannot_be_built_from_are_refused_before_a_job() {
    let r = root("refused");
    let out = r.join("out").display().to_string();
    let refused = |(status, body, end): (StatusCode, String, Option<Result<String, String>>),
                   want: &str| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(end.is_none());
        assert!(body.contains(want), "{want}: {body}");
    };
    // Two games in one archive.
    let two = r.join("Two.zip");
    let mut both = entries("A/");
    both.extend(game().into_iter().map(|(p, d)| (format!("B/{p}"), d)));
    write_zip(&two, &both, None);
    for image in [true, false] {
        refused(
            run(
                image,
                serde_json::json!({ "source": two.display().to_string(), "output_dir": out }),
            )
            .await,
            "more than one game",
        );
    }
    // A truncated archive (a download cut short), zip and 7z.
    let cut = r.join("Cut.zip");
    write_zip(&cut, &entries(""), None);
    let bytes = std::fs::read(&cut).unwrap();
    std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
    let (status, body, _) = run(
        true,
        serde_json::json!({ "source": cut.display().to_string(), "output_dir": out }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let cut7 = r.join("Cut.7z");
    write_7z(&cut7, &entries(""), true);
    let bytes = std::fs::read(&cut7).unwrap();
    std::fs::write(&cut7, &bytes[..bytes.len() / 2]).unwrap();
    refused(
        run(
            true,
            serde_json::json!({ "source": cut7.display().to_string(), "output_dir": out }),
        )
        .await,
        "truncated",
    );
    // A game image inside: the app unpacks that archive first.
    let img = r.join("Img.zip");
    write_zip(&img, &[("x/Game.exfat".into(), vec![0; 4096])], None);
    refused(
        run(
            true,
            serde_json::json!({ "source": img.display().to_string(), "output_dir": out }),
        )
        .await,
        crate::archive_tree::NEEDS_UNPACK,
    );
    // An update to merge: unpacked first too.
    let one = r.join("One.zip");
    write_zip(&one, &entries(""), None);
    refused(
        run(
            true,
            serde_json::json!({
                "source": one.display().to_string(), "output_dir": out,
                "update": r.join("update.pkg").display().to_string(),
            }),
        )
        .await,
        crate::archive_tree::NEEDS_UNPACK,
    );
    let _ = std::fs::remove_dir_all(&r);
}
