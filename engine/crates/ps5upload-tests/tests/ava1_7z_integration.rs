//! AVA1 integration tests for the `.7z` upload path. They replace `transfer_7z_integration.rs`:
//! each builds a `.7z` fixture, uploads it through the Rust AVA1 job host (the decoder runs
//! forward on one thread and feeds the sender, `ps5upload_ava1::seq::SevenzSource`) and asserts
//! the files land already extracted with byte-identical content. The inspect and plan-preview
//! tests never touched the console; they are carried over unchanged.

mod ava1_common;
use ava1_common::*;

use std::path::PathBuf;

use ps5upload_ava1::upload;
use ps5upload_core::transfer::{inspect_7z, sevenz_plan_preview, TransferConfig};

/// A temp `.7z` that deletes itself on drop.
struct Tmp7z(PathBuf);
impl Drop for Tmp7z {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn tmp_path(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    crate::test_tmp::temp_path(format!(
        "ps5u_ava1_7z_{tag}_{}_{}.7z",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Builds a `.7z` from `(name, bytes)` entries (LZMA2, the codec the engine decodes).
fn build_7z(entries: &[(&str, &[u8])]) -> Tmp7z {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let path = tmp_path("b");
    let mut w = ArchiveWriter::create(&path).expect("create 7z");
    for (name, data) in entries {
        let mut e = ArchiveEntry::new();
        e.name = (*name).to_string();
        e.has_stream = !data.is_empty();
        w.push_archive_entry::<&[u8]>(e, if data.is_empty() { None } else { Some(*data) })
            .expect("push 7z entry");
    }
    w.finish().expect("finish 7z");
    Tmp7z(path)
}

async fn put_7z(
    c: &Console,
    id: u8,
    dest_root: &str,
    path: &std::path::Path,
    config: TransferConfig,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (pool, dest, path) = (c.pool.clone(), dest_root.to_string(), path.to_path_buf());
    run(120, move || {
        upload::upload_7z_in(&pool, &config, job_id(id), &dest, &path)
    })
    .await
}

/// Ports `transfer_7z_basic_lands_extracted`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_basic_lands_extracted() {
    let arc = build_7z(&[
        ("a.txt", b"file-a"),
        ("b.txt", b"file-b contents here"),
        ("sub/c.txt", b"nested-c"),
    ]);
    let c = console().await;
    put_7z(&c, 1, "data/dest", &arc.0, cfg()).await.unwrap();
    let got = landed(&c.share.join("data/dest"));
    assert_eq!(got.len(), 3);
    assert_eq!(got["a.txt"], b"file-a");
    assert_eq!(got["b.txt"], b"file-b contents here");
    assert_eq!(got["sub/c.txt"], b"nested-c");
}

/// Ports `transfer_7z_solid_multi_file_byte_correct`: a solid block is decoded in one forward
/// pass; every file is byte-exact.
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_solid_multi_file_is_byte_correct() {
    let big_a: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let big_b: Vec<u8> = (0..150_000u32).map(|i| ((i * 7) % 253) as u8).collect();
    let arc = build_7z(&[
        ("game/eboot.bin", &big_a),
        ("game/sce_sys/param.json", b"{\"titleId\":\"PPSA17905\"}"),
        ("game/data/big.pak", &big_b),
    ]);
    let c = console().await;
    put_7z(&c, 2, "data/g", &arc.0, cfg()).await.unwrap();
    let got = landed(&c.share.join("data/g"));
    assert_eq!(got.len(), 3);
    assert!(got["game/eboot.bin"] == big_a);
    assert_eq!(
        got["game/sce_sys/param.json"],
        b"{\"titleId\":\"PPSA17905\"}"
    );
    assert!(got["game/data/big.pak"] == big_b);
}

/// Ports `transfer_7z_large_entry_spans_many_shards`: one file far larger than a chunk (the
/// `.exfat` single-file case in miniature).
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_large_entry_spans_many_chunks() {
    let payload: Vec<u8> = (0..(3 * 1024 * 1024u32)).map(|i| (i % 256) as u8).collect();
    let arc = build_7z(&[("PPSA17905.exfat", &payload)]);
    let c = console().await;
    let r = put_7z(&c, 3, "data/img", &arc.0, cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, payload.len() as u64);
    assert!(std::fs::read(c.share.join("data/img/PPSA17905.exfat")).unwrap() == payload);
}

/// Ports `transfer_7z_honors_excludes`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_honors_excludes() {
    let arc = build_7z(&[
        ("keep.txt", b"keep"),
        (".DS_Store", b"junk"),
        ("nested/.DS_Store", b"junk2"),
        ("nested/real.bin", b"real"),
    ]);
    let c = console().await;
    let mut config = cfg();
    config.excludes = vec![".DS_Store".to_string()];
    put_7z(&c, 4, "data/x", &arc.0, config).await.unwrap();
    let got = landed(&c.share.join("data/x"));
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        vec!["keep.txt", "nested/real.bin"]
    );
}

/// Ports `transfer_7z_rejects_path_traversal`: a hostile entry name is refused and nothing from
/// the archive lands.
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_rejects_path_traversal() {
    let arc = build_7z(&[("../evil.txt", b"pwned")]);
    let c = console().await;
    let e = put_7z(&c, 5, "data/x", &arc.0, cfg())
        .await
        .expect_err("traversal must be rejected");
    let msg = format!("{e:#}").to_lowercase();
    assert!(
        msg.contains("unsafe")
            || msg.contains("invalid")
            || msg.contains("path")
            || msg.contains("unsupported"),
        "unexpected error: {msg}"
    );
    assert!(
        landed(&c.share).is_empty(),
        "nothing lands from a hostile archive"
    );
}

// ─── Inspect + preview (no console needed; protocol-neutral, carried over) ────

#[test]
fn inspect_7z_reports_counts_and_sizes() {
    let arc = build_7z(&[
        ("a.bin", &[0u8; 1000]),
        ("b.bin", &[1u8; 2000]),
        ("dir/c.bin", &[2u8; 3000]),
    ]);
    let info = inspect_7z(&arc.0).unwrap();
    assert_eq!(info.file_count, 3);
    assert_eq!(info.total_uncompressed, 6000);
    assert!(info.compressed_size > 0);
    assert!(info.title.is_none());
}

#[test]
fn sevenz_plan_preview_total_and_sorted_with_excludes() {
    let arc = build_7z(&[
        ("z.bin", &[0u8; 100]),
        ("a.bin", &[0u8; 200]),
        (".DS_Store", &[0u8; 999]),
    ]);
    let (total, files) = sevenz_plan_preview(&arc.0, &[".DS_Store".to_string()]).unwrap();
    assert_eq!(total, 300);
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["a.bin", "z.bin"]);
}

// ---- stream-less entries inside a solid block ---------------------------------------
//
// A block walk that covers fewer entries than the block spans would silently drop the last
// files, so an archive with directories between its streamed files is refused up front.

/// 7z variable-length number.
fn num(v: u64) -> Vec<u8> {
    let mut n = 0;
    while n < 8 && v >= 1u64 << (7 * (n + 1)) {
        n += 1;
    }
    let mut first = if n == 8 { 0xFFu8 } else { !(0xFFu8 >> n) };
    if n < 8 {
        first |= (v >> (8 * n)) as u8;
    }
    let mut out = vec![first];
    out.extend_from_slice(&v.to_le_bytes()[..n]);
    out
}

/// A hand-built 7z (COPY, uncompressed header, no CRCs) holding one solid folder whose
/// file list interleaves stream-less directories among the streamed files: the
/// `sevenz-rust2` writer cannot produce this valid shape, 7z the format allows it.
/// `layout` lists `Some(file index)` for a streamed file and `None` for a directory.
fn craft_interleaved(
    path: &std::path::Path,
    files: &[(String, Vec<u8>)],
    layout: &[Option<usize>],
) {
    let data: Vec<u8> = files.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    let mut h = vec![0x01, 0x04];
    h.extend([0x06]);
    h.extend(num(0));
    h.extend(num(1));
    h.push(0x09);
    h.extend(num(data.len() as u64));
    h.push(0x00);
    h.push(0x07);
    h.push(0x0B);
    h.extend(num(1));
    h.push(0x00); // not external
    h.extend(num(1)); // one coder
    h.extend([0x01, 0x00]); // simple coder, id size 1, COPY
    h.push(0x0C);
    h.extend(num(data.len() as u64));
    h.push(0x00);
    h.push(0x08);
    h.push(0x0D);
    h.extend(num(files.len() as u64));
    h.push(0x09);
    for (_, d) in &files[..files.len() - 1] {
        h.extend(num(d.len() as u64));
    }
    h.push(0x00);
    h.push(0x00); // end of streams info
    h.push(0x05);
    h.extend(num(layout.len() as u64));
    let mut bits = vec![0u8; layout.len().div_ceil(8)];
    for (i, l) in layout.iter().enumerate() {
        if l.is_none() {
            bits[i / 8] |= 0x80 >> (i % 8);
        }
    }
    h.push(0x0E);
    h.extend(num(bits.len() as u64));
    h.extend(&bits);
    let mut names = vec![0u8]; // not external
    for l in layout {
        let n = match l {
            Some(i) => files[*i].0.clone(),
            None => format!("sub{}", names.len()),
        };
        for u in n.encode_utf16().chain([0]) {
            names.extend(u.to_le_bytes());
        }
    }
    h.push(0x11);
    h.extend(num(names.len() as u64));
    h.extend(&names);
    h.push(0x00);
    h.push(0x00);
    let mut start = Vec::new();
    start.extend((data.len() as u64).to_le_bytes());
    start.extend((h.len() as u64).to_le_bytes());
    start.extend(crc32fast::hash(&h).to_le_bytes());
    let mut out = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4];
    out.extend(crc32fast::hash(&start).to_le_bytes());
    out.extend(start);
    out.extend(data);
    out.extend(h);
    std::fs::write(path, out).unwrap();
}

/// Ports `transfer_7z_refuses_a_block_with_directories_between_its_files`: refused with its own
/// reason, the preview refuses it too, nothing lands; the same files with the directories last
/// are fine.
#[tokio::test(flavor = "multi_thread")]
async fn upload_7z_refuses_a_block_with_directories_between_its_files() {
    let files: Vec<(String, Vec<u8>)> = (0..4)
        .map(|i| (format!("f{i}"), vec![i as u8 + 1; 1_000]))
        .collect();
    let p = tmp_path("layout");
    craft_interleaved(
        &p,
        &files,
        &[Some(0), None, Some(1), None, Some(2), Some(3)],
    );
    let c = console().await;
    let e = put_7z(&c, 6, "data/dest", &p, cfg()).await.unwrap_err();
    let f = e
        .downcast_ref::<upload::UploadFailure>()
        .unwrap_or_else(|| panic!("a typed failure, got {e:#}"));
    assert_eq!(f.reason, "ava1_7z_unsupported_layout");
    assert!(f.detail.contains("layout is not supported"), "{}", f.detail);
    assert!(
        sevenz_plan_preview(&p, &[]).is_err(),
        "the preview refuses it too"
    );
    assert!(
        landed(&c.share).is_empty(),
        "nothing was sent to the console"
    );
    craft_interleaved(
        &p,
        &files,
        &[Some(0), Some(1), Some(2), Some(3), None, None],
    );
    put_7z(&c, 7, "data/dest", &p, cfg()).await.unwrap();
    let got = landed(&c.share.join("data/dest"));
    assert_eq!(got.len(), 4);
    for (n, d) in &files {
        assert_eq!(&got[n], d, "{n}");
    }
    let _ = std::fs::remove_file(&p);
}

// Test scratch paths removed when the test process exits (engine/test-support/test_tmp.rs).
use ava1_common::test_tmp;
