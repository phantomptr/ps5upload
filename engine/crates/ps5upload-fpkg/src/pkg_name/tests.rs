//! A rename, on packages this crate writes: the name reads back, the package still verifies in
//! full, and nothing but the planned bytes moved.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;
use crate::build::{self, BuildRequest};
use crate::crypto::DEFAULT_PASSCODE;
use crate::source::SourceTree;

const CONTENT_ID: &str = "UP0000-PPSA01234_00-TESTGAME00000000";
const MARKED: &str = "Rise of the Example-example.invalid-abc";

/// A folder of its own, removed when dropped (a failed test too).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = crate::test_tmp::temp_path(format!(
            "fpkg-name-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A `param.json` as a Publishing Tools package has it: indented, CRLF line ends, the name in
/// three languages (two the same, one its own).
fn pretty_param() -> String {
    format!(
        "{{\r\n  \"applicationCategoryType\": 0,\r\n  \"attributePub\": 0,\r\n  \"contentId\": \
         \"{CONTENT_ID}\",\r\n  \"contentVersion\": \"01.001.000\",\r\n  \"localizedParameters\": \
         {{\r\n    \"defaultLanguage\": \"en-US\",\r\n    \"en-US\": {{\r\n      \"titleName\": \
         \"{MARKED}\"\r\n    }},\r\n    \"fr-FR\": {{\r\n      \"titleName\": \"{MARKED}\"\r\n    \
         }},\r\n    \"ja-JP\": {{\r\n      \"titleName\": \"テストゲーム\"\r\n    }}\r\n  }},\r\n  \
         \"titleId\": \"PPSA01234\"\r\n}}\r\n"
    )
}

/// [`pretty_param`] with a top-level `titleName` as well, which is the one this crate's
/// builder writes into the package's manifest (`pfsimage.xml`).
fn pretty_param_with_a_top_name() -> String {
    pretty_param().replace(
        "  \"titleId\"",
        &format!("  \"titleName\": \"{MARKED}\",\r\n  \"titleId\""),
    )
}

fn compact_param() -> String {
    format!(
        "{{\"applicationCategoryType\":0,\"attributePub\":0,\"contentId\":\"{CONTENT_ID}\",\
         \"contentVersion\":\"01.001.000\",\"localizedParameters\":{{\"defaultLanguage\":\"en-US\",\
         \"en-US\":{{\"titleName\":\"{MARKED}\"}}}},\"titleId\":\"PPSA01234\"}}"
    )
}

/// A small PS5 package built from a folder with `param`, in `dir`.
fn ps5_package(dir: &Path, param: &str) -> PathBuf {
    let source = dir.join("game");
    let put = |path: &str, data: &[u8]| {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    };
    put(
        "eboot.bin",
        &(0..4096u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    );
    put("data/large.bin", &vec![0xAB; 300 * 1024]);
    put("sce_sys/param.json", param.as_bytes());
    put("sce_sys/icon0.png", &[0x89; 2048]);
    put("sce_sys/icon0.dds", &[0x44; 4096]);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        ..BuildRequest::new(&source, &out)
    };
    let report = build::build(&request, &mut |_| {}).unwrap();
    assert!(report.verify.ok(), "{}", report.verify);
    report.path
}

/// What the engine does through its undo journal, plainly: the plan's bytes into the file.
fn apply(path: &Path, plan: &Plan) {
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    for p in &plan.patches {
        f.seek(SeekFrom::Start(p.offset)).unwrap();
        f.write_all(&p.data).unwrap();
    }
    f.sync_all().unwrap();
}

/// Every byte outside the plan's patches is as it was, and the length too.
fn only_the_patches_changed(before: &[u8], after: &[u8], plan: &Plan) {
    assert_eq!(before.len(), after.len(), "the package keeps its length");
    let mut planned = vec![false; before.len()];
    for p in &plan.patches {
        planned[p.offset as usize..p.offset as usize + p.data.len()].fill(true);
    }
    let stray = (0..before.len()).find(|i| before[*i] != after[*i] && !planned[*i]);
    assert_eq!(stray, None, "a byte outside the plan changed");
}

/// Every file a package reads back as, by path.
fn ps5_files(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut tree = crate::pkg_source::open(path, DEFAULT_PASSCODE).unwrap();
    let paths: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
    paths
        .into_iter()
        .map(|p| {
            let data = tree.read(&p).unwrap();
            (p, data)
        })
        .collect()
}

/// The package's manifest (`pfsimage.xml` in the ZIP that ends it), whose CRC in that ZIP
/// must be right.
fn manifest(path: &Path) -> String {
    let mut file = PkgFile::open(path).unwrap();
    let zip = si::read(&mut file).unwrap().unwrap();
    let m = zip
        .members
        .iter()
        .find(|m| m.name.ends_with("pfsimage.xml"))
        .expect("our packages carry a manifest");
    let xml = file.read_at(m.offset, m.size as usize).unwrap();
    assert_eq!(crc32fast::hash(&xml), m.crc, "the ZIP's own CRC of it");
    String::from_utf8(xml).unwrap()
}

fn full_verify(path: &Path) -> crate::verify::Report {
    crate::verify::verify_streaming(path, DEFAULT_PASSCODE, &mut |_, _| {}).unwrap()
}

#[test]
fn a_ps5_package_gets_a_shorter_name_in_place() {
    let dir = TempDir::new("ps5-short");
    let pkg = ps5_package(dir.path(), &pretty_param_with_a_top_name());
    let names = read(&pkg).unwrap();
    assert_eq!(names.platform, Platform::Ps5);
    assert_eq!(names.content_id, CONTENT_ID);
    assert_eq!(names.name, MARKED);
    assert_eq!(names.refusal, None);
    assert_eq!(names.other_names, 1, "ja-JP has its own");
    let keys: Vec<&str> = names.entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, ["en-US", "fr-FR", "ja-JP", ""]);

    let before = std::fs::read(&pkg).unwrap();
    let files = ps5_files(&pkg);
    let plan = plan(&pkg, "  Rise of the Example ", false).unwrap();
    assert_eq!(plan.name, "Rise of the Example", "trimmed");
    assert_eq!((plan.previous.as_str(), plan.changed), (MARKED, 3));
    assert!(!plan.patches.is_empty());
    assert!(
        plan.write_bytes() < 64 * 1024,
        "{} bytes for a rename",
        plan.write_bytes()
    );
    assert_eq!(
        std::fs::read(&pkg).unwrap(),
        before,
        "a plan writes nothing"
    );

    apply(&pkg, &plan);
    check(&pkg, &plan).unwrap();
    let after = std::fs::read(&pkg).unwrap();
    only_the_patches_changed(&before, &after, &plan);

    // The reader, the app's package reader and the full verifier.
    let names = read(&pkg).unwrap();
    assert_eq!(names.refusal, None);
    assert_eq!(names.name, "Rise of the Example");
    let read_back: Vec<&str> = names.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        read_back,
        [
            "Rise of the Example",
            "Rise of the Example",
            "テストゲーム",
            "Rise of the Example"
        ]
    );
    assert_eq!(
        ps5upload_pkg::parse_pkg(&pkg).unwrap().title,
        "Rise of the Example"
    );
    let report = full_verify(&pkg);
    assert!(report.ok(), "{report}");
    assert!(report
        .checks
        .iter()
        .any(|c| c.name == "si playgo-chunk.crc"));
    // The game's files, the image's own param.json among them, are the ones packaged.
    assert_eq!(ps5_files(&pkg), files);

    // The manifest in the ZIP that ends the package says the same.
    let xml = manifest(&pkg);
    assert!(
        xml.contains("<titleName>Rise of the Example</titleName>"),
        "{xml}"
    );
    assert!(!xml.contains(MARKED));
}

#[test]
fn a_ps5_package_gets_a_longer_name_in_every_language() {
    let dir = TempDir::new("ps5-long");
    let pkg = ps5_package(dir.path(), &pretty_param_with_a_top_name());
    let before = std::fs::read(&pkg).unwrap();
    let files = ps5_files(&pkg);
    let name = "Rise of the Example: \"Final\" & <Best> ゲーム";
    let plan = plan(&pkg, name, true).unwrap();
    assert_eq!(plan.changed, 4);
    apply(&pkg, &plan);
    check(&pkg, &plan).unwrap();
    only_the_patches_changed(&before, &std::fs::read(&pkg).unwrap(), &plan);
    let names = read(&pkg).unwrap();
    assert_eq!((names.name.as_str(), names.other_names), (name, 0));
    assert!(names.entries.iter().all(|e| e.name == name));
    assert_eq!(ps5upload_pkg::parse_pkg(&pkg).unwrap().title, name);
    let report = full_verify(&pkg);
    assert!(report.ok(), "{report}");
    assert_eq!(ps5_files(&pkg), files);
    assert!(
        manifest(&pkg).contains(
            "<titleName>Rise of the Example: &quot;Final&quot; &amp; &lt;Best&gt; ゲーム</titleName>"
        ),
        "the manifest's name grew with it"
    );

    // A second rename of the renamed package, back to a short name.
    let again = super::plan(&pkg, "Example", true).unwrap();
    assert_eq!(again.previous, name);
    apply(&pkg, &again);
    check(&pkg, &again).unwrap();
    assert!(full_verify(&pkg).ok());
    assert_eq!(read(&pkg).unwrap().name, "Example");
}

#[test]
fn a_name_that_does_not_fit_is_refused_and_nothing_is_written() {
    let dir = TempDir::new("ps5-full");
    let pkg = ps5_package(dir.path(), &compact_param());
    let before = std::fs::read(&pkg).unwrap();
    // No whitespace to give up: one byte more than the old name does not fit.
    let longer = format!("{MARKED}!");
    let e = plan(&pkg, &longer, false).unwrap_err().to_string();
    assert!(e.contains("too long for this package"), "{e}");
    // The same length, and shorter, do.
    let same = MARKED.replace("abc", "xyz");
    let p = plan(&pkg, &same, false).unwrap();
    assert_eq!(std::fs::read(&pkg).unwrap(), before);
    apply(&pkg, &p);
    check(&pkg, &p).unwrap();
    assert!(full_verify(&pkg).ok());
    assert_eq!(read(&pkg).unwrap().name, same);
}

#[test]
fn the_name_it_already_has_plans_nothing() {
    let dir = TempDir::new("ps5-same");
    let pkg = ps5_package(dir.path(), &pretty_param());
    let p = plan(&pkg, MARKED, false).unwrap();
    assert_eq!((p.changed, p.patches.len()), (0, 0));
    check(&pkg, &p).unwrap();
}

#[test]
fn names_that_can_not_be_stored_are_refused() {
    for (name, why) in [
        ("   ", "empty"),
        ("two\nlines", "control character"),
        (&"x".repeat(MAX_NAME_BYTES + 1), "at most 127"),
    ] {
        let e = clean_name(name).unwrap_err().to_string();
        assert!(e.contains(why), "{name:?}: {e}");
    }
    assert_eq!(clean_name(&"x".repeat(MAX_NAME_BYTES)).unwrap().len(), 127);
}

#[test]
fn an_official_package_is_refused_with_the_reason() {
    let dir = TempDir::new("ps5-retail");
    let pkg = ps5_package(dir.path(), &pretty_param());
    let mut bytes = std::fs::read(&pkg).unwrap();
    bytes[5] = 0x80; // the finalized-image header's "signed" byte: retail
    std::fs::write(&pkg, &bytes).unwrap();
    let names = read(&pkg).unwrap();
    let refusal = names.refusal.expect("refused");
    assert_eq!(refusal.code, "retail");
    assert!(refusal.reason.contains("official package"), "{refusal:?}");
    let e = plan(&pkg, "Anything", false).unwrap_err().to_string();
    assert!(e.contains("publisher's signature"), "{e}");
    assert_eq!(std::fs::read(&pkg).unwrap(), bytes);
}

#[test]
fn a_package_whose_digests_do_not_hold_is_refused() {
    let dir = TempDir::new("ps5-damaged");
    let pkg = ps5_package(dir.path(), &pretty_param());
    let clean = std::fs::read(&pkg).unwrap();
    let cnt_at = u64::from_le_bytes(clean[0x58..0x60].try_into().unwrap()) as usize;
    // The body digest, the package digest and one byte of an entry: each alone is enough.
    for at in [cnt_at + 0x160, cnt_at + 0xFE0, cnt_at + 0x2010] {
        let mut bytes = clean.clone();
        bytes[at] ^= 1;
        std::fs::write(&pkg, &bytes).unwrap();
        let refusal = read(&pkg).unwrap().refusal.expect("refused");
        assert_eq!(refusal.code, "unsupported");
        assert!(refusal.reason.contains("does not hold"), "{refusal:?}");
        assert!(plan(&pkg, "Anything", false).is_err());
    }
    // A wrong block checksum outside the container is found when the change is planned.
    std::fs::write(&pkg, &clean).unwrap();
    let mut file = PkgFile::open(&pkg).unwrap();
    let zip = si::read(&mut file).unwrap().unwrap();
    let crc = zip
        .members
        .iter()
        .find(|m| m.name.ends_with("/playgo-chunk.crc"))
        .unwrap();
    let mut bytes = clean.clone();
    bytes[crc.offset as usize + (cnt_at / BLOCK as usize) * 4] ^= 1;
    std::fs::write(&pkg, &bytes).unwrap();
    assert_eq!(read(&pkg).unwrap().refusal, None);
    let e = plan(&pkg, "Anything", false).unwrap_err().to_string();
    assert!(e.contains("block checksums"), "{e}");
}

#[test]
fn a_change_written_in_part_does_not_check_out() {
    let dir = TempDir::new("ps5-part");
    let pkg = ps5_package(dir.path(), &pretty_param());
    let clean = std::fs::read(&pkg).unwrap();
    let whole = plan(&pkg, "Example", false).unwrap();
    for skip in 0..whole.patches.len() {
        std::fs::write(&pkg, &clean).unwrap();
        let mut part = whole.clone();
        part.patches.remove(skip);
        apply(&pkg, &part);
        let e = check(&pkg, &whole).unwrap_err().to_string();
        assert!(
            e.contains("does not check out"),
            "without patch {skip}: {e}"
        );
    }
    std::fs::write(&pkg, &clean).unwrap();
    apply(&pkg, &whole);
    check(&pkg, &whole).unwrap();
}

#[test]
fn what_is_not_a_package_and_a_delta_update_are_told_apart() {
    let dir = TempDir::new("not");
    let p = dir.path().join("x.pkg");
    std::fs::write(&p, b"PK\x03\x04 not a package at all").unwrap();
    let e = read(&p).unwrap_err().to_string();
    assert!(e.contains("not a PS4 or PS5 package"), "{e}");
    std::fs::write(&p, [crate::lih::MAGIC.as_slice(), &[0u8; 64][..]].concat()).unwrap();
    let refusal = read(&p).unwrap().refusal.expect("refused");
    assert_eq!(refusal.code, "patch");
    assert!(plan(&p, "Anything", false).is_err());
}

// ─── PS4 ────────────────────────────────────────────────────────────────────────────────

/// A PS4 fake package written by this crate from the synthetic fixture's game, so every
/// digest in it is filled (the fixture itself has its digests zeroed).
fn ps4_package(dir: &Path) -> PathBuf {
    let bytes = crate::ps4_pkg::tests::fixture("synthetic");
    let len = bytes.len() as u64;
    let mut game = crate::ps4_pkg::Ps4Package::open(
        Box::new(std::io::Cursor::new(bytes)),
        len,
        DEFAULT_PASSCODE,
        "fixture".into(),
    )
    .unwrap();
    let path = dir.join("game.pkg");
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    crate::ps4_pkg_write::write(
        &mut game,
        &mut out,
        &crate::ps4_pkg_write::Options {
            time: Some(1_600_000_000),
        },
        &mut |_, _, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    path
}

fn ps4_files(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).unwrap();
    let len = file.metadata().unwrap().len();
    let mut p = crate::ps4_pkg::Ps4Package::open(
        Box::new(file),
        len,
        DEFAULT_PASSCODE,
        path.display().to_string(),
    )
    .unwrap();
    let paths: Vec<String> = p.files().iter().map(|f| f.path.clone()).collect();
    paths
        .into_iter()
        .map(|f| {
            let data = p.read(&f).unwrap();
            (f, data)
        })
        .collect()
}

#[test]
fn a_ps4_package_is_renamed_in_place() {
    let dir = TempDir::new("ps4");
    let pkg = ps4_package(dir.path());
    let names = read(&pkg).unwrap();
    assert_eq!(names.platform, Platform::Ps4);
    assert_eq!(names.refusal, None);
    assert_eq!(names.name, "Test PS4 Game");
    assert_eq!(names.entries[0].key, "TITLE");

    let before = std::fs::read(&pkg).unwrap();
    let files = ps4_files(&pkg);
    let plan = plan(&pkg, "A Game With Another, Rather Longer Name", false).unwrap();
    assert_eq!(plan.platform, Platform::Ps4);
    assert_eq!(plan.changed, 1);
    apply(&pkg, &plan);
    check(&pkg, &plan).unwrap();
    only_the_patches_changed(&before, &std::fs::read(&pkg).unwrap(), &plan);

    let names = read(&pkg).unwrap();
    assert_eq!(names.refusal, None, "every digest holds again");
    assert_eq!(names.name, "A Game With Another, Rather Longer Name");
    assert_eq!(
        ps5upload_pkg::parse_pkg(&pkg).unwrap().title,
        "A Game With Another, Rather Longer Name"
    );
    // Every file but param.sfo is the same, and param.sfo differs only in its name.
    let after = ps4_files(&pkg);
    assert_eq!(after.len(), files.len());
    for ((path, was), (path_now, now)) in files.iter().zip(&after) {
        assert_eq!(path, path_now);
        if path == "sce_sys/param.sfo" {
            let (a, b) = (
                ps5upload_pkg::sfo_params(was).unwrap(),
                ps5upload_pkg::sfo_params(now).unwrap(),
            );
            assert_eq!(a.len(), b.len());
            for ((key, x), (_, y)) in a.iter().zip(&b) {
                assert_eq!(x == y, key != "TITLE", "{key}");
            }
        } else {
            assert!(was == now, "{path} changed");
        }
    }

    // And back to a short one.
    let again = super::plan(&pkg, "Short", false).unwrap();
    apply(&pkg, &again);
    check(&pkg, &again).unwrap();
    assert_eq!(read(&pkg).unwrap().name, "Short");
}

#[test]
fn a_ps4_package_that_is_not_a_fake_one_or_does_not_hold_is_refused() {
    let dir = TempDir::new("ps4-refused");
    let pkg = ps4_package(dir.path());
    let clean = std::fs::read(&pkg).unwrap();

    // Its image no longer opens with the all-zero passcode's keys: as an official one reads.
    let pfs_at = crate::be64(&clean, 0x410) as usize;
    let mut bytes = clean.clone();
    bytes[pfs_at + 0x380] ^= 1; // the superblock's signature
    std::fs::write(&pkg, &bytes).unwrap();
    let refusal = read(&pkg).unwrap().refusal.expect("refused");
    assert_eq!(refusal.code, "retail");
    let e = plan(&pkg, "Anything", false).unwrap_err().to_string();
    assert!(e.contains("not a fake package"), "{e}");

    // A digest that does not hold.
    let mut bytes = clean.clone();
    bytes[0x160] ^= 1;
    std::fs::write(&pkg, &bytes).unwrap();
    let refusal = read(&pkg).unwrap().refusal.expect("refused");
    assert_eq!(refusal.code, "unsupported");
    assert!(refusal.reason.contains("the body digest"), "{refusal:?}");
}

/// The synthetic fixture is another writer's package (Alembic's, LibOrbisPkg's bytes): its
/// digests follow the same rules, so it is renamed as ours are.
#[test]
fn a_ps4_package_another_writer_made_is_renamed() {
    let dir = TempDir::new("ps4-other");
    let pkg = dir.path().join("fixture.pkg");
    let before = crate::ps4_pkg::tests::fixture("synthetic");
    std::fs::write(&pkg, &before).unwrap();
    let files = ps4_files(&pkg);
    let names = read(&pkg).unwrap();
    assert_eq!(
        (names.name.as_str(), names.refusal),
        ("Test PS4 Game", None)
    );
    let plan = plan(&pkg, "Renamed", false).unwrap();
    apply(&pkg, &plan);
    check(&pkg, &plan).unwrap();
    only_the_patches_changed(&before, &std::fs::read(&pkg).unwrap(), &plan);
    assert_eq!(read(&pkg).unwrap().name, "Renamed");
    let after = ps4_files(&pkg);
    for ((path, was), (_, now)) in files.iter().zip(&after) {
        assert_eq!(was == now, path != "sce_sys/param.sfo", "{path}");
    }
}

// ─── The texts ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_param_json_keeps_its_length_and_everything_but_the_names() {
    let text = pretty_param();
    let doc = scan_param(text.as_bytes()).unwrap();
    assert_eq!(doc.default_language.as_deref(), Some("en-US"));
    assert_eq!(doc.default_index(), Some(0));
    assert_eq!(text.as_bytes()[doc.close], b'}');
    assert_eq!(doc.close, text.rfind('}').unwrap());

    // Shorter: the spaces go in before the closing brace.
    let short = edit_param(text.as_bytes(), &doc, &[0, 1], "X").unwrap();
    assert_eq!(short.len(), text.len());
    let short = String::from_utf8(short).unwrap();
    assert!(short.ends_with("}\r\n"), "{short:?}");
    assert_eq!(short.matches("\"titleName\": \"X\"").count(), 2);
    assert!(short.contains("テストゲーム"));

    // Longer: whitespace between tokens comes out, from the end.
    let name = format!("{MARKED} and \"so\" \\ on");
    let long = edit_param(text.as_bytes(), &doc, &[0, 1, 2], &name).unwrap();
    assert_eq!(long.len(), text.len());
    let v: serde_json::Value = serde_json::from_slice(&long).unwrap();
    for language in ["en-US", "fr-FR", "ja-JP"] {
        assert_eq!(
            v["localizedParameters"][language]["titleName"],
            name.as_str()
        );
    }
    assert_eq!(v["contentId"], CONTENT_ID);

    // More than its whitespace holds.
    let e = edit_param(text.as_bytes(), &doc, &[0, 1, 2], &"y".repeat(127))
        .unwrap_err()
        .to_string();
    assert!(e.contains("too long for this package"), "{e}");
}

#[test]
fn a_param_json_with_a_mark_padding_and_a_top_level_name_reads() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(
        br#"{"titleName":"Top \"one\"","list":[1,{"titleName":"not a name"},[]],"n":-1.5e3,"t":true,"localizedParameters":{"de-DE":{"x":null,"titleName":"Eins"}}}"#,
    );
    bytes.extend_from_slice(&[0; 5]);
    let doc = scan_param(&bytes).unwrap();
    let names: Vec<(&str, &str)> = doc
        .names
        .iter()
        .map(|n| (n.key.as_str(), n.value.as_str()))
        .collect();
    assert_eq!(names, [("", "Top \"one\""), ("de-DE", "Eins")]);
    assert_eq!(
        doc.default_index(),
        Some(0),
        "no default language: the top one"
    );
    // Shorter fits (it pads); the padding and the mark stay.
    let out = edit_param(&bytes, &doc, &[0, 1], "Neu").unwrap();
    assert_eq!(out.len(), bytes.len());
    assert!(out.starts_with(&[0xEF, 0xBB, 0xBF]) && out.ends_with(&[0; 5]));
    let again = scan_param(&out).unwrap();
    assert!(again.names.iter().all(|n| n.value == "Neu"));

    for bad in [
        &b"[1]"[..],
        b"{\"a\":1} x",
        b"{\"a\":\"open",
        b"{\"a\" 1}",
        b"",
    ] {
        assert!(
            scan_param(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

const MANIFEST: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package-configuration \
     version=\"1.0\">\n  <digests version=\"1.2\">\n    <param-digest>\n      0x01 0x02 0x03 0x04 \
     0x05 0x06 0x07 0x08 0x09 0x0a 0x0b 0x0c 0x0d 0x0e 0x0f 0x10\n      0x11 0x12 0x13 0x14 0x15 \
     0x16 0x17 0x18 0x19 0x1a 0x1b 0x1c 0x1d 0x1e 0x1f 0x20\n    </param-digest>\n  \
     </digests>\n  <params>\n    <contentId>X</contentId>\n    <titleName>Tom &amp; \
     Jerry</titleName>\n  </params>\n  <container nth-of-image=\"1\">\n    \
     <empty>\n    </empty>\n    <size>0x10</size>\n  </container>\n</package-configuration>\n";

#[test]
fn a_manifest_keeps_its_length() {
    let xml = MANIFEST.as_bytes();
    // Another name than the one it holds: left alone.
    assert_eq!(set_manifest_name(xml, "Someone else", "New").unwrap(), None);

    let short = set_manifest_name(xml, "Tom & Jerry", "T<J")
        .unwrap()
        .unwrap();
    assert_eq!(short.len(), xml.len());
    let short = String::from_utf8(short).unwrap();
    let padded = format!("<titleName>T&lt;J</titleName>{}\n", " ".repeat(9));
    assert!(short.contains(&padded), "{short}");

    let long = set_manifest_name(xml, "Tom & Jerry", "Tom & Jerry & Spike")
        .unwrap()
        .unwrap();
    assert_eq!(long.len(), xml.len());
    let long = String::from_utf8(long).unwrap();
    assert!(long.contains("<titleName>Tom &amp; Jerry &amp; Spike</titleName>\n"));
    // The indentation given up is the nearest after the name, and never an element's text.
    assert!(
        long.contains("</titleName>\n</params>\n<container nth-of-image=\"1\">\n"),
        "{long}"
    );
    assert!(long.contains("<empty>\n    </empty>"), "{long}");
    assert!(long.contains("      0x01 0x02"), "{long}");
    assert!(long.contains("\n    </param-digest>"), "{long}");

    let e = set_manifest_name(xml, "Tom & Jerry", &"z".repeat(120))
        .unwrap_err()
        .to_string();
    assert!(e.contains("no more room"), "{e}");
}

#[test]
fn a_manifest_digest_is_set_only_over_the_one_it_holds() {
    let mut xml = MANIFEST.as_bytes().to_vec();
    let old: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
    let new = [0xABu8; 32];
    set_manifest_digest(&mut xml, "param-digest", &old, &new).unwrap();
    assert_eq!(xml.len(), MANIFEST.len());
    let text = String::from_utf8(xml.clone()).unwrap();
    assert_eq!(text.matches("0xab").count(), 32);
    // No such element: nothing to do. Another digest than the container's: an error.
    set_manifest_digest(&mut xml, "body-digest", &old, &new).unwrap();
    let e = set_manifest_digest(&mut xml, "param-digest", &old, &new).unwrap_err();
    assert!(e.contains("is not the container's"), "{e}");
}

/// The room `read` reports is exact for plain text: a name of that many bytes fits, one more
/// does not. With and without whitespace to give up, for the default name and for every
/// language.
#[test]
fn the_room_a_ps5_package_reports_is_what_fits() {
    for (label, param) in [
        ("compact", compact_param()),
        ("pretty", pretty_param()),
        ("top", pretty_param_with_a_top_name()),
    ] {
        let dir = TempDir::new(&format!("ps5-room-{label}"));
        let pkg = ps5_package(dir.path(), &param);
        let names = read(&pkg).unwrap();
        let room = names.room.expect("a fake package says its room");
        assert!(room.default_name <= MAX_NAME_BYTES && room.all_languages <= MAX_NAME_BYTES);
        for (all, bytes) in [(false, room.default_name), (true, room.all_languages)] {
            let fits = "n".repeat(bytes);
            let p = plan(&pkg, &fits, all)
                .unwrap_or_else(|e| panic!("{label} all={all}: {bytes} bytes should fit: {e}"));
            assert!(p.changed > 0);
            if bytes < MAX_NAME_BYTES {
                let over = "n".repeat(bytes + 1);
                let e = plan(&pkg, &over, all).unwrap_err().to_string();
                assert!(
                    e.contains("too long for this package"),
                    "{label} all={all}: {e}"
                );
            }
        }
        // A name written to the limit still checks out on disk.
        let p = plan(&pkg, &"n".repeat(room.all_languages), true).unwrap();
        apply(&pkg, &p);
        check(&pkg, &p).unwrap();
        assert!(full_verify(&pkg).ok(), "{label}");
    }
    // No whitespace at all: exactly the old name's length.
    let dir = TempDir::new("ps5-room-exact");
    let pkg = ps5_package(dir.path(), &compact_param());
    let room = read(&pkg).unwrap().room.unwrap();
    assert!(room.default_name >= MARKED.len(), "{room:?}");
}

#[test]
fn the_room_a_ps4_package_reports_is_what_fits() {
    let dir = TempDir::new("ps4-room");
    let pkg = ps4_package(dir.path());
    let room = read(&pkg)
        .unwrap()
        .room
        .expect("a fake package says its room");
    for (all, bytes) in [(false, room.default_name), (true, room.all_languages)] {
        assert!(bytes > 0 && bytes <= MAX_NAME_BYTES);
        plan(&pkg, &"n".repeat(bytes), all)
            .unwrap_or_else(|e| panic!("all={all}: {bytes} bytes should fit: {e}"));
        if bytes < MAX_NAME_BYTES {
            let e = plan(&pkg, &"n".repeat(bytes + 1), all)
                .unwrap_err()
                .to_string();
            assert!(e.contains("too long for this package"), "all={all}: {e}");
        }
    }
}

#[test]
fn a_package_that_can_not_be_renamed_reports_no_room() {
    let dir = TempDir::new("no-room");
    let delta = dir.path().join("delta.pkg");
    std::fs::write(
        &delta,
        [crate::lih::MAGIC.as_slice(), &[0u8; 64][..]].concat(),
    )
    .unwrap();
    let names = read(&delta).unwrap();
    assert!(names.refusal.is_some());
    assert_eq!(names.room, None);
}
