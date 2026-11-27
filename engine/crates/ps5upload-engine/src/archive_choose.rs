//! "Choose files" in an archive (#969): what a ZIP, 7z, RAR or tar holds, as the rows a person
//! ticks, read from its listing without unpacking anything.
//!
//! Each package (`.pkg`, `.fpkg`) is its own row wherever it is. Everything else is grouped: the
//! shallowest folder that holds no package is one row, and a loose file beside a package (a
//! readme, an `.nfo`) is its own row. The client sends the ticked rows back as `=path` entries
//! in the upload's `excludes` (see `ps5upload_core::excludes`), so a row's `path` is spelt
//! exactly as the transfer spells it: the same listing functions are used.
//!
//! A package inside a ZIP also has its header read (the first [`PKG_HEAD_BYTES`] of the entry,
//! cheap for a stored or deflated entry), so the row can say which game, which version and
//! whether it is a base, an update or DLC. 7z and RAR would have to decode everything before
//! the entry in a solid block: they are not read, and the install reads each package once it is
//! on the PS5, as before.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

/// How many rows an answer holds at most. An archive with more (thousands of loose files at its
/// top) says so with `complete: false`, and the app does not offer a choice of it.
pub(crate) const ROWS_MAX: usize = 5000;

/// How much of a package's start is read for its header (the param.sfo of a PS4 package lies
/// well inside it).
const PKG_HEAD_BYTES: u64 = 4 << 20;

/// How many packages of one archive have their header read at most.
const PKG_HEADS_MAX: usize = 64;

/// What a package's header says, when it could be read.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct PackageBrief {
    pub title: String,
    pub title_id: String,
    pub content_id: String,
    /// `gd` a game, `gp` an update, `ac` DLC (from param.sfo, else the header's content type).
    pub category: String,
    pub app_ver: String,
}

/// One row of the chooser.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ChooseRow {
    /// The path inside the archive, as the transfer spells it.
    pub path: String,
    /// `package`, `folder` or `file`.
    pub kind: &'static str,
    /// Unpacked bytes (a folder: everything in it).
    pub size: u64,
    /// Files it stands for (1 for a file or a package).
    pub files: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageBrief>,
}

/// The answer of `/api/archive/entries`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ArchiveEntries {
    /// `zip`, `7z`, `rar` or `tar`.
    pub kind: &'static str,
    pub items: Vec<ChooseRow>,
    pub file_count: u64,
    pub total_bytes: u64,
    /// False when there were more rows than [`ROWS_MAX`] (the rest are left out).
    pub complete: bool,
}

/// True for a file name the install path takes as a package.
pub(crate) fn is_package_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".pkg") || lower.ends_with(".fpkg")
}

/// The chooser's rows for a listing of `(path, size)` files (any order). Sorted by path.
pub(crate) fn chooser_rows(files: &[(String, u64)]) -> (Vec<ChooseRow>, bool) {
    // Every folder that holds a package somewhere below it: it can't be one row.
    let mut pkg_dirs: BTreeSet<&str> = BTreeSet::new();
    for (path, _) in files {
        if is_package_name(path) {
            let mut at = 0;
            while let Some(i) = path[at..].find('/') {
                pkg_dirs.insert(&path[..at + i]);
                at += i + 1;
            }
        }
    }
    let mut rows: BTreeMap<String, ChooseRow> = BTreeMap::new();
    for (path, size) in files {
        let (key, kind) = if is_package_name(path) {
            (path.as_str(), "package")
        } else {
            // The shallowest folder on the way that holds no package, else the file itself.
            let mut found = None;
            let mut at = 0;
            while let Some(i) = path[at..].find('/') {
                let dir = &path[..at + i];
                if !pkg_dirs.contains(dir) {
                    found = Some(dir);
                    break;
                }
                at += i + 1;
            }
            match found {
                Some(dir) => (dir, "folder"),
                None => (path.as_str(), "file"),
            }
        };
        let row = rows.entry(key.to_string()).or_insert_with(|| ChooseRow {
            path: key.to_string(),
            kind,
            size: 0,
            files: 0,
            package: None,
        });
        row.size = row.size.saturating_add(*size);
        row.files += 1;
    }
    let complete = rows.len() <= ROWS_MAX;
    (rows.into_values().take(ROWS_MAX).collect(), complete)
}

/// The kind an archive's name says, for the chooser (a lone `.zst` has nothing to choose).
pub(crate) fn chooser_kind(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some("zip")
    } else if lower.ends_with(".7z") {
        Some("7z")
    } else if lower.ends_with(".rar") {
        Some("rar")
    } else if ps5upload_core::tar_archive::stream_kind(Path::new(path))
        == Some(ps5upload_core::tar_archive::StreamKind::Tar)
    {
        Some("tar")
    } else {
        None
    }
}

/// Every file in the archive, `(path, size)`, spelt as the transfer spells it.
pub(crate) fn archive_files(
    path: &str,
    password: Option<&str>,
) -> anyhow::Result<(&'static str, Vec<(String, u64)>)> {
    let p = Path::new(path);
    let Some(kind) = chooser_kind(path) else {
        anyhow::bail!("not a .zip, .7z, .rar or tar archive: {path}");
    };
    let files = match kind {
        "zip" => ps5upload_core::transfer::zip_plan_preview(p, &[])?.1,
        "7z" => ps5upload_core::transfer::sevenz_plan_preview(p, &[])?.1,
        "tar" => {
            let layout = ps5upload_core::tar_archive::tar_layout(p)?;
            ps5upload_core::tar_archive::tar_plan(&layout, &[]).1
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        _ => ps5upload_core::transfer::rar_layout(p, password, &[])?.files,
        #[cfg(any(target_os = "android", target_os = "ios"))]
        _ => {
            let _ = password;
            anyhow::bail!("RAR is not supported on this build")
        }
    };
    Ok((kind, files))
}

/// The chooser's answer for the archive at `path`.
pub(crate) fn archive_entries(
    path: &str,
    password: Option<&str>,
) -> anyhow::Result<ArchiveEntries> {
    let (kind, files) = archive_files(path, password)?;
    let (mut items, complete) = chooser_rows(&files);
    if kind == "zip" {
        let wanted: Vec<String> = items
            .iter()
            .filter(|r| r.kind == "package")
            .take(PKG_HEADS_MAX)
            .map(|r| r.path.clone())
            .collect();
        let mut heads = zip_package_briefs(Path::new(path), &wanted);
        for row in items.iter_mut() {
            row.package = heads.remove(&row.path);
        }
    }
    Ok(ArchiveEntries {
        kind,
        file_count: files.len() as u64,
        total_bytes: files.iter().fold(0u64, |n, (_, s)| n.saturating_add(*s)),
        items,
        complete,
    })
}

/// The first bytes of an entry, served as the whole entry would be: reads past them fail, so a
/// header field that lies further in is left empty rather than guessed.
struct Prefix {
    buf: Vec<u8>,
    pos: u64,
}

impl Read for Prefix {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let at = usize::try_from(self.pos).unwrap_or(usize::MAX);
        if at >= self.buf.len() {
            if out.is_empty() {
                return Ok(0);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "past the bytes read from the archive",
            ));
        }
        let n = out.len().min(self.buf.len() - at);
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Prefix {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let base = |b: u64, d: i64| {
            b.checked_add_signed(d).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before the start")
            })
        };
        self.pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => base(self.pos, d)?,
            // The real end is not known here; a seek from it lands past the bytes held.
            SeekFrom::End(d) => base(u64::MAX / 2, d)?,
        };
        Ok(self.pos)
    }
}

/// What a package's first bytes say, or `None` when they are no package header.
pub(crate) fn brief_from_head(head: Vec<u8>, size: u64) -> Option<PackageBrief> {
    let mut r = Prefix { buf: head, pos: 0 };
    let meta = ps5upload_pkg::parse_pkg_from(&mut r, size, Path::new("")).ok()?;
    if meta.content_id.is_empty() && meta.title_id.is_empty() {
        return None;
    }
    let category = if meta.category.is_empty() && meta.content_type != 0 {
        ps5upload_pkg::kind::category_from_header(meta.content_type, meta.content_flags).to_string()
    } else {
        meta.category
    };
    let title_id = if meta.title_id.is_empty() {
        ps5upload_pkg::kind::title_id_of_content_id(&meta.content_id)
    } else {
        meta.title_id
    };
    Some(PackageBrief {
        title: meta.title,
        title_id,
        content_id: meta.content_id,
        category,
        app_ver: meta.app_ver,
    })
}

/// Header briefs of the packages `wanted` (paths as the listing spells them) inside a ZIP. A
/// package whose header can't be read is left out; nothing fails the listing.
pub(crate) fn zip_package_briefs(
    zip_path: &Path,
    wanted: &[String],
) -> BTreeMap<String, PackageBrief> {
    let mut out = BTreeMap::new();
    if wanted.is_empty() {
        return out;
    }
    let Ok(f) = std::fs::File::open(zip_path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(f)) else {
        return out;
    };
    let wanted: BTreeSet<&str> = wanted.iter().map(String::as_str).collect();
    for i in 0..zip.len() {
        if out.len() == wanted.len() {
            break;
        }
        let rel = {
            let Ok(entry) = zip.by_index_raw(i) else {
                continue;
            };
            let decoded = entry
                .name()
                .map(|n| n.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(entry.name_raw()).into_owned());
            let full = ps5upload_core::transfer::zip_entry_name(entry.name_raw(), &decoded);
            match ps5upload_core::transfer::sanitize_zip_entry(&full) {
                Some(rel) if wanted.contains(rel.as_str()) && !out.contains_key(&rel) => rel,
                _ => continue,
            }
        };
        let Ok(entry) = zip.by_index(i) else {
            continue;
        };
        let size = entry.size();
        let mut head = Vec::new();
        if entry.take(PKG_HEAD_BYTES).read_to_end(&mut head).is_err() {
            continue;
        }
        if let Some(brief) = brief_from_head(head, size) {
            out.insert(rel, brief);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    fn rows(list: &[(&str, u64)]) -> Vec<(String, &'static str, u64, u64)> {
        chooser_rows(&files(list))
            .0
            .into_iter()
            .map(|r| (r.path, r.kind, r.size, r.files))
            .collect()
    }

    #[test]
    fn packages_are_rows_and_a_folder_without_one_is_a_row() {
        let got = rows(&[
            ("Game/base.pkg", 100),
            ("Game/update/patch.PKG", 10),
            ("Game/readme.txt", 1),
            ("Game/update/notes/a.txt", 2),
            ("Game/update/notes/b.txt", 3),
            ("Extras/art/a.png", 4),
            ("Extras/b.png", 5),
            ("top.nfo", 6),
        ]);
        assert_eq!(
            got,
            vec![
                ("Extras".into(), "folder", 9, 2),
                ("Game/base.pkg".into(), "package", 100, 1),
                ("Game/readme.txt".into(), "file", 1, 1),
                ("Game/update/notes".into(), "folder", 5, 2),
                ("Game/update/patch.PKG".into(), "package", 10, 1),
                ("top.nfo".into(), "file", 6, 1),
            ]
        );
    }

    #[test]
    fn a_game_folder_is_one_row() {
        let got = rows(&[
            ("PPSA01234/eboot.bin", 10),
            ("PPSA01234/sce_sys/param.json", 1),
            ("PPSA05678-app/eboot.bin", 20),
        ]);
        assert_eq!(
            got,
            vec![
                ("PPSA01234".into(), "folder", 11, 2),
                ("PPSA05678-app".into(), "folder", 20, 1),
            ]
        );
    }

    #[test]
    fn an_fpkg_is_a_package_too() {
        assert!(is_package_name("x/Game.FPKG"));
        assert!(!is_package_name("x/Game.pkg.0"));
        assert_eq!(rows(&[("a.fpkg", 1)])[0].1, "package");
    }

    #[test]
    fn too_many_rows_say_the_listing_is_cut() {
        let many: Vec<(String, u64)> = (0..ROWS_MAX + 3).map(|i| (format!("f{i}"), 1)).collect();
        let (rows, complete) = chooser_rows(&many);
        assert_eq!(rows.len(), ROWS_MAX);
        assert!(!complete);
    }

    #[test]
    fn the_kind_follows_the_name() {
        assert_eq!(chooser_kind("/x/a.ZIP"), Some("zip"));
        assert_eq!(chooser_kind("/x/a.part1.rar"), Some("rar"));
        assert_eq!(chooser_kind("/x/a.tar.gz"), Some("tar"));
        assert_eq!(chooser_kind("/x/a.7z"), Some("7z"));
        assert_eq!(chooser_kind("/x/a.exfat.zst"), None);
        assert_eq!(chooser_kind("/x/a.pkg"), None);
    }

    /// A PS4 package's first bytes: the magic, the content id and a content type, no SFO.
    fn fake_pkg_head(content_id: &str, content_type: u32) -> Vec<u8> {
        let mut h = vec![0u8; 0x1000];
        h[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes()); // \x7FCNT
        h[0x40..0x40 + content_id.len()].copy_from_slice(content_id.as_bytes());
        h[0x74..0x78].copy_from_slice(&content_type.to_be_bytes());
        h
    }

    #[test]
    fn a_zip_lists_rows_and_reads_its_package_headers() {
        use std::io::Write;
        let dir = crate::test_tmp::temp_path(format!("ps5upload-choose-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("bundle.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).expect("create"));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let base = fake_pkg_head("UP0001-CUSA01234_00-GAME000000000000", 0x1A);
        let dlc = fake_pkg_head("UP0001-CUSA01234_00-DLC0000000000001", 0x1B);
        zw.start_file("Game/base.pkg", stored).unwrap();
        zw.write_all(&base).unwrap();
        zw.start_file("Game/dlc.pkg", deflated).unwrap();
        zw.write_all(&dlc).unwrap();
        zw.start_file("Game/notapkg.pkg", stored).unwrap();
        zw.write_all(b"plain text").unwrap();
        zw.start_file("readme.txt", stored).unwrap();
        zw.write_all(b"hi").unwrap();
        zw.finish().unwrap();

        let got = archive_entries(path.to_str().unwrap(), None).expect("listed");
        assert_eq!(got.kind, "zip");
        assert_eq!(got.file_count, 4);
        assert!(got.complete);
        let by: BTreeMap<&str, &ChooseRow> =
            got.items.iter().map(|r| (r.path.as_str(), r)).collect();
        let b = by["Game/base.pkg"].package.as_ref().expect("base header");
        assert_eq!(b.title_id, "CUSA01234");
        assert_eq!(b.category, "gd");
        let d = by["Game/dlc.pkg"].package.as_ref().expect("dlc header");
        assert_eq!(d.category, "ac");
        assert!(by["Game/notapkg.pkg"].package.is_none());
        assert_eq!(by["readme.txt"].kind, "file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_7z_lists_rows_without_reading_headers() {
        let dir = crate::test_tmp::temp_path(format!("ps5upload-choose7z-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("b.7z");
        let mut w = sevenz_rust2::ArchiveWriter::create(&path).expect("create 7z");
        for (name, data) in [
            ("Game/base.pkg", &b"0123456789"[..]),
            ("Game/patch.pkg", &b"abc"[..]),
            ("info.txt", &b"x"[..]),
        ] {
            let mut e = sevenz_rust2::ArchiveEntry::new();
            e.name = name.to_string();
            e.has_stream = true;
            w.push_archive_entry::<&[u8]>(e, Some(data))
                .expect("push 7z entry");
        }
        w.finish().expect("finish 7z");
        let got = archive_entries(path.to_str().unwrap(), None).expect("listed");
        assert_eq!(got.kind, "7z");
        let paths: Vec<(&str, &str)> = got
            .items
            .iter()
            .map(|r| (r.path.as_str(), r.kind))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("Game/base.pkg", "package"),
                ("Game/patch.pkg", "package"),
                ("info.txt", "file")
            ]
        );
        assert!(got.items.iter().all(|r| r.package.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
