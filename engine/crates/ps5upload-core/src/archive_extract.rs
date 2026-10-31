//! Unpack a game archive (`.zip`, `.7z`, `.rar`) into a folder, and find the game in it.
//!
//! Convert builds from a folder or an image on this computer, so an archive is unpacked
//! first. Entry names get the same zip-slip checks the archive uploads use; nothing is
//! written outside `dest`. 7z decodes single-threaded unless `PS5UPLOAD_7Z_THREADS` says
//! otherwise (multi-threaded decode can hold the whole archive in memory). RAR is
//! desktop-only, as it is for uploads.
//!
//! Passwords: only RAR's are supported. The zip and 7z decoders are built without their
//! encryption features, so a protected `.zip` / `.7z` is refused with that reason.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context, Result};

use crate::transfer::{
    host_unsafe_component, sanitize_7z_entry, sanitize_zip_entry, sevenz_check_layout,
    sevenz_decode_threads,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    SevenZ,
    Rar,
}

/// The archive kind a file name says, if any.
pub fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "zip" => Some(ArchiveKind::Zip),
        "7z" => Some(ArchiveKind::SevenZ),
        "rar" => Some(ArchiveKind::Rar),
        _ => None,
    }
}

/// Volumes an old-style RAR set can have after its `.rar`: `.r00`…`.r99`, then WinRAR (and
/// UnRAR, which follows the same rule) carries on with `.s00`…`.s99`, `.t00`…, up to `.z99`.
/// The payload's extractor uses the same limit (`archive_volume_name`).
pub const RAR_OLD_VOLUMES_MAX: u32 = 900;

/// The 0-based position of an old-style RAR volume after the set's `.rar`, from its file name:
/// `x.r00` → 0, `x.r99` → 99, `x.s00` → 100, `x.t05` → 205. Returns the stem (`x`) with it.
/// None for anything else, `.rar` included.
pub fn rar_old_volume_index(name: &str) -> Option<(&str, u32)> {
    let dot = name.rfind('.')?;
    let ext = &name.as_bytes()[dot + 1..];
    if ext.len() != 3 || !ext[1].is_ascii_digit() || !ext[2].is_ascii_digit() {
        return None;
    }
    let letter = ext[0].to_ascii_lowercase();
    if !(b'r'..=b'z').contains(&letter) {
        return None;
    }
    let n =
        u32::from(letter - b'r') * 100 + u32::from(ext[1] - b'0') * 10 + u32::from(ext[2] - b'0');
    Some((&name[..dot], n))
}

/// The name of old-style volume `index` (0-based, after the `.rar`) of the set `stem.rar`:
/// 0 → `stem.r00`, 100 → `stem.s00`. `upper` writes the letter as `R`/`S`/… for a set named
/// `STEM.RAR` (case matters on a case-sensitive disk). None past `.z99`.
pub fn rar_old_volume_name(stem: &str, index: u32, upper: bool) -> Option<String> {
    if index >= RAR_OLD_VOLUMES_MAX {
        return None;
    }
    let letter = (b'r' + (index / 100) as u8) as char;
    let letter = if upper {
        letter.to_ascii_uppercase()
    } else {
        letter
    };
    Some(format!("{stem}.{letter}{:02}", index % 100))
}

/// Remove a file or a folder tree, retrying a few times. On Windows a file that was just closed
/// can stay locked for a moment (an antivirus or the indexer opening it, a helper process still
/// exiting), so a single attempt at cleanup after a failed unpack left the files behind. A path
/// that is already gone counts as removed.
pub fn remove_path_retrying(path: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut last = Ok(());
    for attempt in 0..ATTEMPTS {
        let r = match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => Err(e),
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
        };
        match r {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => last = Err(e),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(100 << attempt));
        }
    }
    last
}

/// Image files Convert builds from directly.
const IMAGE_EXTS: [&str; 3] = ["exfat", "ffpkg", "ffpfsc"];

/// The unpacked size of every file, from the archive's listing (no decompression).
pub fn unpacked_size(archive: &Path, password: Option<&str>) -> Result<u64> {
    match kind_of(archive)? {
        ArchiveKind::Zip => {
            let mut z = open_zip(archive)?;
            let mut total = 0u64;
            for i in 0..z.len() {
                let e = z.by_index_raw(i)?;
                if !e.is_dir() {
                    total = total.saturating_add(e.size());
                }
            }
            Ok(total)
        }
        ArchiveKind::SevenZ => {
            let archive = read_7z_header(archive)?;
            Ok(archive
                .files
                .iter()
                .filter(|f| !f.is_directory())
                .map(|f| f.size())
                .fold(0u64, u64::saturating_add))
        }
        ArchiveKind::Rar => rar_size(archive, password),
    }
}

/// Unpack `archive` into `dest` (created if missing). `on_bytes(done, total)` reports
/// unpacked bytes; setting `cancel` stops at the next chunk with a "cancelled" error.
pub fn extract(
    archive: &Path,
    dest: &Path,
    password: Option<&str>,
    on_bytes: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let kind = kind_of(archive)?;
    let total = unpacked_size(archive, password)?;
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut sink = Sink {
        dest,
        discard: false,
        done: 0,
        total,
        on_bytes,
        cancel,
    };
    sink.check_cancel()?;
    match kind {
        ArchiveKind::Zip => extract_zip(archive, &mut sink),
        ArchiveKind::SevenZ => extract_7z(archive, &mut sink),
        ArchiveKind::Rar => extract_rar(archive, password, &mut sink),
    }
}

/// Read every file in `archive` through its decoder and throw the bytes away: each entry's
/// checksum (zip CRC-32, 7z CRC, RAR's own) is checked as it is read, so a damaged archive
/// fails here the way it would fail half way through an unpack. Nothing is written.
pub fn test(
    archive: &Path,
    password: Option<&str>,
    on_bytes: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let kind = kind_of(archive)?;
    let total = unpacked_size(archive, password)?;
    let mut sink = Sink {
        dest: Path::new("."),
        discard: true,
        done: 0,
        total,
        on_bytes,
        cancel,
    };
    sink.check_cancel()?;
    match kind {
        ArchiveKind::Zip => extract_zip(archive, &mut sink),
        ArchiveKind::SevenZ => extract_7z(archive, &mut sink),
        ArchiveKind::Rar => extract_rar(archive, password, &mut sink),
    }
}

fn kind_of(archive: &Path) -> Result<ArchiveKind> {
    archive_kind(archive)
        .ok_or_else(|| anyhow!("{} is not a .zip, .7z or .rar archive", archive.display()))
}

/// Where unpacked bytes go, counted and cancellable.
struct Sink<'a> {
    dest: &'a Path,
    /// Read and count, write nothing (a [`test`]).
    discard: bool,
    done: u64,
    total: u64,
    on_bytes: &'a mut dyn FnMut(u64, u64),
    cancel: &'a AtomicBool,
}

impl Sink<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        Ok(())
    }

    /// `rel` is already sanitised: relative, no `..`.
    fn dir(&self, rel: &str) -> Result<()> {
        let p = host_path(self.dest, rel)?;
        if self.discard {
            return Ok(());
        }
        std::fs::create_dir_all(&p).with_context(|| format!("create {}", p.display()))
    }

    fn file(&mut self, rel: &str, from: &mut dyn Read) -> Result<()> {
        let p = host_path(self.dest, rel)?;
        let mut out = if self.discard {
            None
        } else {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            Some(File::create(&p).with_context(|| format!("create {}", p.display()))?)
        };
        let mut buf = vec![0u8; 1 << 20];
        loop {
            self.check_cancel()?;
            let n = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(anyhow!("unpack {rel}: {e}")),
            };
            if let Some(out) = out.as_mut() {
                out.write_all(&buf[..n])
                    .with_context(|| format!("write {}", p.display()))?;
            }
            self.done += n as u64;
            (self.on_bytes)(self.done, self.total.max(self.done));
        }
        Ok(())
    }
}

/// Where `rel` (a sanitised archive path) lands under `dest`. The sanitisers keep `..`,
/// NUL and backslashes out, but `Path::join` has its own ideas on Windows: `C:/x` and
/// `C:evil` are drive paths that replace `dest`, and a reserved device name or a
/// trailing dot names something else. Refused everywhere, like the download side, so
/// an archive unpacks the same way on every host.
fn host_path(dest: &Path, rel: &str) -> Result<PathBuf> {
    // Device names, ':' and a trailing dot or space only misbehave on Windows; elsewhere they
    // are ordinary names (`aux.dat` is a real game file), so only a Windows unpack refuses them.
    if cfg!(windows) {
        for seg in rel.split('/') {
            if let Some(why) = host_unsafe_component(seg) {
                bail!("the archive has an entry path this computer cannot write, {why}: {rel:?}");
            }
        }
    }
    let p = dest.join(rel);
    let inside = p
        .strip_prefix(dest)
        .is_ok_and(|r| r.components().all(|c| matches!(c, Component::Normal(_))));
    if !inside {
        bail!("the archive has an unsafe entry path: {rel:?}");
    }
    Ok(p)
}

fn open_zip(archive: &Path) -> Result<zip::ZipArchive<std::io::BufReader<File>>> {
    let f = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    zip::ZipArchive::new(std::io::BufReader::new(f))
        .map_err(|e| anyhow!("read zip {}: {e}", archive.display()))
}

fn extract_zip(archive: &Path, sink: &mut Sink) -> Result<()> {
    let mut z = open_zip(archive)?;
    for i in 0..z.len() {
        let mut e = match z.by_index(i) {
            Ok(e) => e,
            Err(zip::result::ZipError::UnsupportedArchive(why)) if why.contains("assword") => {
                bail!("this .zip is password-protected, which Convert can't open; re-pack it without a password, or as .rar")
            }
            Err(e) => return Err(anyhow!("read zip entry {i}: {e}")),
        };
        // zip 9 returns an error when the declared encoding can't decode the bytes; fall back to a
        // lossy read so one odd name doesn't stop the whole archive.
        let decoded = e
            .name()
            .map(|n| n.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(e.name_raw()).into_owned());
        let name = crate::transfer::zip_entry_name(e.name_raw(), &decoded);
        let Some(rel) = sanitize_zip_entry(&name) else {
            if e.is_dir() && name.trim_matches('/').is_empty() {
                continue;
            }
            bail!("the archive has an unsafe entry path: {name:?}");
        };
        if e.is_dir() {
            sink.dir(&rel)?;
        } else {
            sink.file(&rel, &mut e)?;
        }
    }
    Ok(())
}

fn read_7z_header(archive: &Path) -> Result<sevenz_rust2::Archive> {
    let mut src = std::io::BufReader::new(
        File::open(archive).with_context(|| format!("open {}", archive.display()))?,
    );
    let header = sevenz_rust2::Archive::read(&mut src, &sevenz_rust2::Password::from(""))
        .map_err(|e| seven_err(archive, e))?;
    // The same refusal the upload path makes: the per-block walk below would silently
    // skip the last files of such a block, and Convert would report a short tree as done.
    sevenz_check_layout(&header)?;
    Ok(header)
}

fn seven_err(archive: &Path, e: sevenz_rust2::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.to_ascii_lowercase().contains("password") || msg.to_ascii_lowercase().contains("aes") {
        anyhow!("this .7z is password-protected, which Convert can't open; re-pack it without a password, or as .rar")
    } else {
        anyhow!("read 7z {}: {msg}", archive.display())
    }
}

fn extract_7z(archive: &Path, sink: &mut Sink) -> Result<()> {
    let mut reader = sevenz_rust2::ArchiveReader::new(
        std::io::BufReader::new(
            File::open(archive).with_context(|| format!("open {}", archive.display()))?,
        ),
        sevenz_rust2::Password::from(""),
    )
    .map_err(|e| seven_err(archive, e))?;
    sevenz_check_layout(reader.archive())?;
    reader.set_thread_count(sevenz_decode_threads());
    // The closure's error type is 7z's; ours are carried out beside it.
    let mut failure: Option<anyhow::Error> = None;
    let walked = reader.for_each_entries(|entry, rd| {
        let step = (|| -> Result<()> {
            let Some(rel) = sanitize_7z_entry(entry.name()) else {
                bail!("the archive has an unsafe entry path: {:?}", entry.name());
            };
            if entry.is_directory() {
                std::io::copy(rd, &mut std::io::sink())?;
                sink.dir(&rel)
            } else {
                sink.file(&rel, rd)
            }
        })();
        match step {
            Ok(()) => Ok(true),
            Err(e) => {
                failure = Some(e);
                Ok(false)
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    walked.map_err(|e| seven_err(archive, e))?;
    // Every file the header lists must have been written: a walk that ends short of the
    // listing is a decoder gap, never a finished unpack.
    if sink.done != sink.total {
        bail!(
            "unpacked {} of the {} bytes the 7z lists; the archive's layout is not supported, re-pack it with 7-Zip or extract it first",
            sink.done,
            sink.total
        );
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn rar_size(archive: &Path, password: Option<&str>) -> Result<u64> {
    Ok(crate::transfer::rar_plan_entries(archive, password, &[])?.0)
}

#[cfg(target_os = "android")]
fn rar_size(_: &Path, _: Option<&str>) -> Result<u64> {
    bail!("RAR archives can't be opened on Android; use .zip or .7z")
}

#[cfg(not(target_os = "android"))]
fn extract_rar(archive: &Path, password: Option<&str>, sink: &mut Sink) -> Result<()> {
    use crate::rar_stream::{next_entry, EntryReader};
    for dir in crate::transfer::rar_dirs(archive, password)? {
        sink.dir(&dir)?;
    }
    let (rx, worker) = crate::transfer::spawn_rar_worker(archive, password, Vec::new());
    let result = (|| -> Result<()> {
        while let Some(rel) = next_entry(&rx).map_err(|e| anyhow!("{e}"))? {
            sink.file(&rel, &mut EntryReader::new(&rx))?;
        }
        Ok(())
    })();
    // Dropping the receiver unblocks a worker still sending (cancel, failure).
    drop(rx);
    let _ = worker.join();
    result
}

#[cfg(target_os = "android")]
fn extract_rar(_: &Path, _: Option<&str>, _: &mut Sink) -> Result<()> {
    bail!("RAR archives can't be opened on Android; use .zip or .7z")
}

/// Folders a find never descends into: archive tools' sidecars.
fn is_sidecar(name: &str) -> bool {
    name == "__MACOSX" || name.starts_with("._") || name.eq_ignore_ascii_case(".DS_Store")
}

/// The game in an unpacked archive: a game folder (one with `sce_sys/param.json` or
/// `param.sfo`), however deep it is wrapped, or a single game image. None, or more than one,
/// is an error that says so.
pub fn find_game(root: &Path) -> Result<PathBuf> {
    const MAX_DEPTH: u32 = 4;
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let sys = dir.join("sce_sys");
        if sys.join("param.json").is_file() || sys.join("param.sfo").is_file() {
            found.push(dir);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_sidecar(&name) {
                continue;
            }
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if ft.is_file()
                && path
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| IMAGE_EXTS.contains(&x.to_ascii_lowercase().as_str()))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!(
            "no game in the archive: no folder with sce_sys/param.json, and no .exfat, .ffpkg or .ffpfsc image"
        ),
        _ => {
            let names: Vec<String> = found
                .iter()
                .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                .collect();
            bail!(
                "more than one game in the archive ({}); unpack it and convert one at a time",
                names.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{host_path, rar_old_volume_index, rar_old_volume_name, remove_path_retrying};
    use std::path::Path;

    /// WinRAR's old naming runs `.rar`, `.r00`…`.r99`, then `.s00`…`.s99`, `.t00`…: every
    /// letter continues the count, so a 300-volume set is not three sets.
    #[test]
    fn old_style_rar_volumes_continue_past_r99() {
        assert_eq!(rar_old_volume_index("g.r00"), Some(("g", 0)));
        assert_eq!(rar_old_volume_index("g.r99"), Some(("g", 99)));
        assert_eq!(rar_old_volume_index("g.s00"), Some(("g", 100)));
        assert_eq!(rar_old_volume_index("G.S42"), Some(("G", 142)));
        assert_eq!(rar_old_volume_index("g.t00"), Some(("g", 200)));
        assert_eq!(rar_old_volume_index("g.z99"), Some(("g", 899)));
        for not in [
            "g.rar",
            "g.q00",
            "g.r0",
            "g.r100",
            "g.part1.rar",
            "g.7z.001",
            "r00",
        ] {
            assert_eq!(rar_old_volume_index(not), None, "{not}");
        }
        assert_eq!(rar_old_volume_name("g", 0, false).as_deref(), Some("g.r00"));
        assert_eq!(
            rar_old_volume_name("g", 99, false).as_deref(),
            Some("g.r99")
        );
        assert_eq!(
            rar_old_volume_name("g", 100, false).as_deref(),
            Some("g.s00")
        );
        assert_eq!(
            rar_old_volume_name("g", 205, true).as_deref(),
            Some("g.T05")
        );
        assert_eq!(rar_old_volume_name("g", 900, false), None);
        // The name and the index are each other's inverse over the whole range.
        for i in 0..900 {
            let n = rar_old_volume_name("x", i, false).unwrap();
            assert_eq!(rar_old_volume_index(&n), Some(("x", i)));
        }
    }

    #[test]
    fn remove_path_retrying_removes_trees_files_and_absent_paths() {
        let base = std::env::temp_dir().join(format!("ps5u-rmretry-{}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(tree.join("a/b")).unwrap();
        std::fs::write(tree.join("a/b/f.bin"), b"x").unwrap();
        let file = base.join("one.r00");
        std::fs::write(&file, b"y").unwrap();
        remove_path_retrying(&tree).unwrap();
        remove_path_retrying(&file).unwrap();
        assert!(!tree.exists() && !file.exists());
        remove_path_retrying(&base.join("never-was")).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A sanitised archive path still has to land under `dest` on this host: a drive or
    /// stream name, a reserved device name or a trailing dot would not.
    #[test]
    fn host_path_keeps_every_entry_under_dest() {
        let dest = Path::new("/tmp/unpack");
        for ok in ["a/b.txt", "sce_sys/param.json", "CONSOLE/x", "price$"] {
            let p = host_path(dest, ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(p.starts_with(dest), "{ok} -> {}", p.display());
        }
        if !cfg!(windows) {
            // Ordinary names off Windows: unpacked as they are.
            for ok in ["aux.dat", "a/NUL.txt", "x.", "s:stream"] {
                assert!(host_path(dest, ok).unwrap().starts_with(dest), "{ok}");
            }
            return;
        }
        for bad in [
            "C:/x",
            "C:evil",
            "a/C:/x",
            "CON",
            "a/NUL.txt",
            "x.",
            "a/x ",
            "s:stream",
        ] {
            let e = host_path(dest, bad).unwrap_err().to_string();
            assert!(e.contains("cannot write") && e.contains(bad), "{bad}: {e}");
        }
    }
}
