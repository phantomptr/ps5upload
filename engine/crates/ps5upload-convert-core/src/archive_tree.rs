//! The game folder inside a `.zip`, `.7z` or `.rar`, read in place: a [`SourceTree`] whose files
//! come out of the archive as a build asks for them (`ps5upload_archive::stream`, which says how
//! and what memory it holds). The engine (`archive_tree.rs` there, which adds the password and
//! its job's cancel flag) and the console's converter build from it the same way.
//!
//! What can't be read this way is refused with [`NEEDS_UNPACK`] at the start of its message: a
//! game image or package inside an archive (its reader seeks all over it). On a computer the app
//! unpacks that archive first; on the console the converter says so.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use ps5upload_archive::stream::{self, Lane, Listing, ReadError, Reader, Stop};
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// The start of the refusal for an archive that has to be unpacked first. The app sees it and
/// takes the unpack path.
pub const NEEDS_UNPACK: &str = "archive_needs_unpack";

/// Folders the game is looked for in, as deep as an unpack is searched (`find_game`).
const MAX_DEPTH: usize = 4;

/// Junk no game wants, skipped by name at any depth (as a folder source skips it), and the
/// archive tools' own sidecars.
fn is_junk(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        ".ds_store"
            | "thumbs.db"
            | "desktop.ini"
            | "system volume information"
            | ".fseventsd"
            | ".spotlight-v100"
            | ".trashes"
            | "__macosx"
    ) || name.starts_with("._")
}

fn has_junk(path: &str) -> bool {
    path.split('/').any(is_junk)
}

/// The game in the archive: the folder holding `sce_sys/param.json` (or `param.sfo`), as
/// `""` or `dir/`. One game image or package instead is [`NEEDS_UNPACK`]; none, or more than
/// one game, is refused as an unpack would refuse it.
pub fn game_root(listing: &Listing) -> Result<String, String> {
    let mut games = BTreeSet::new();
    for f in &listing.files {
        if has_junk(&f.path) {
            continue;
        }
        let lower = f.path.to_ascii_lowercase();
        for param in ["sce_sys/param.json", "sce_sys/param.sfo"] {
            let dir = if lower == param {
                Some(String::new())
            } else {
                lower
                    .strip_suffix(param)
                    .filter(|d| d.ends_with('/'))
                    .map(|d| f.path[..d.len()].to_string())
            };
            if let Some(dir) = dir.filter(|d| d.matches('/').count() <= MAX_DEPTH) {
                games.insert(dir);
            }
        }
    }
    // A game inside another game's folder is part of it.
    let all: Vec<String> = games.iter().cloned().collect();
    games.retain(|g| !all.iter().any(|o| o != g && g.starts_with(o.as_str())));
    let images: Vec<&str> = (listing.files.iter())
        .map(|f| f.path.as_str())
        .filter(|p| !has_junk(p) && p.matches('/').count() <= MAX_DEPTH)
        .filter(|p| !games.iter().any(|g| p.starts_with(g.as_str())))
        .filter(|p| {
            p.rsplit_once('.').is_some_and(|(_, ext)| {
                crate::image_build::GAME_FILE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
            })
        })
        .collect();
    match (games.len(), images.len()) {
        (1, 0) => Ok(games.pop_first().unwrap_or_default()),
        (0, 1) => Err(format!(
            "{NEEDS_UNPACK}: the game in this archive is an image or package ({}), which is unpacked first",
            images[0]
        )),
        (0, 0) => Err(
            "no game in the archive: no folder with sce_sys/param.json, and no game image or package"
                .into(),
        ),
        _ => {
            let mut names: Vec<String> = games
                .iter()
                .map(|g| match g.trim_end_matches('/') {
                    "" => "(the archive's top folder)".to_string(),
                    g => g.to_string(),
                })
                .collect();
            names.extend(images.iter().map(|s| s.to_string()));
            Err(format!(
                "more than one game in the archive ({}); unpack it and convert one at a time",
                names.join(", ")
            ))
        }
    }
}

/// The game folder inside an archive: paths relative to it, bytes from the archive.
pub struct ArchiveTree {
    reader: Reader,
    /// The game folder's path in the archive (`""` or ending in `/`).
    prefix: String,
    files: Vec<SourceFile>,
    sizes: HashMap<String, u64>,
    empty_dirs: Vec<String>,
    describe: String,
    /// What the conversion leaves out of the archive (files beside the game folder).
    notes: Vec<String>,
}

/// The game folder in the archive at `path` (already `listing`), read in place. Its
/// `param.json` (or `param.sfo`) is read now, so a wrong password or a damaged archive is
/// refused before any job starts. `stop` ends a read waiting on the archive; `lane`, when
/// given, runs on the archive's own thread before each piece (the engine's thread priority).
pub fn open_listed(
    path: &Path,
    password: Option<&str>,
    listing: Listing,
    stop: Stop,
    lane: Option<Lane>,
) -> Result<ArchiveTree, String> {
    let kind = listing.kind;
    let prefix = game_root(&listing)?;
    let mut files = Vec::new();
    let mut parents = BTreeSet::new();
    for f in &listing.files {
        let Some(rel) = f.path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rel.is_empty() || has_junk(rel) {
            continue;
        }
        let mut at = rel;
        while let Some((parent, _)) = at.rsplit_once('/') {
            parents.insert(parent.to_string());
            at = parent;
        }
        files.push(SourceFile {
            path: rel.to_string(),
            size: f.size,
        });
    }
    // Folders with nothing in them, which a file list cannot say (a game may look for one).
    let mut dirs = BTreeSet::new();
    for d in &listing.dirs {
        let Some(rel) = d.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rel.is_empty() || has_junk(rel) {
            continue;
        }
        let mut at = rel;
        while let Some((parent, _)) = at.rsplit_once('/') {
            parents.insert(parent.to_string());
            at = parent;
        }
        dirs.insert(rel.to_string());
    }
    let empty_dirs = dirs.difference(&parents).cloned().collect();
    let notes = left_out_note(&listing, &prefix).into_iter().collect();
    let sizes = files.iter().map(|f| (f.path.clone(), f.size)).collect();
    let describe = format!(
        "{} (.{} archive{}, read in place{})",
        path.display(),
        kind.name(),
        if listing.solid { ", solid" } else { "" },
        match prefix.trim_end_matches('/') {
            "" => String::new(),
            p => format!(", game in {p}"),
        }
    );
    let reader = Reader::new(path, password, listing, stop);
    let reader = match lane {
        Some(lane) => reader.with_lane(lane),
        None => reader,
    };
    let mut tree = ArchiveTree {
        reader,
        prefix,
        files,
        sizes,
        empty_dirs,
        describe,
        notes,
    };
    // The password and the data checked now: the game's own small param file.
    let param = ["sce_sys/param.json", "sce_sys/param.sfo"]
        .into_iter()
        .find_map(|want| {
            (tree.files.iter())
                .find(|f| f.path.eq_ignore_ascii_case(want))
                .map(|f| f.path.clone())
        });
    if let Some(param) = param {
        ps5upload_fpkg::source::read_small(
            &mut tree,
            &param,
            ps5upload_fpkg::source::MAX_PARAM_BYTES,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(tree)
}

/// [`open_listed`] of the archive at `path`, listed by `ps5upload_archive::stream::list` (`.zip`
/// and `.7z`; a `.rar` only where the program installed its RAR reader).
pub fn open(
    path: &Path,
    password: Option<&str>,
    stop: Stop,
    lane: Option<Lane>,
) -> Result<ArchiveTree, String> {
    let listing = stream::list(path, password).map_err(|e| format!("{e:#}"))?;
    open_listed(path, password, listing, stop, lane)
}

/// What the archive holds beside its game folder (`prefix`), junk and the folders around the
/// game aside: the conversion leaves it out, so the archive is not the game alone (and is not
/// moved to the Trash afterwards). `None` when there is nothing else.
pub fn left_out_note(listing: &Listing, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let outside = |p: &str| !p.starts_with(prefix) && !has_junk(p);
    let files: Vec<&str> = (listing.files.iter())
        .map(|f| f.path.as_str())
        .filter(|p| outside(p))
        .collect();
    // A folder the game folder is in is only its wrapping.
    let dirs = (listing.dirs.iter())
        .map(|d| d.trim_end_matches('/'))
        .filter(|d| !d.is_empty() && outside(d) && !prefix.starts_with(&format!("{d}/")))
        .count();
    let first = files.first().copied().or_else(|| {
        (listing.dirs.iter())
            .map(|d| d.trim_end_matches('/'))
            .find(|d| !d.is_empty() && outside(d) && !prefix.starts_with(&format!("{d}/")))
    })?;
    Some(format!(
        "the archive holds more than the game folder {}: {} other file(s) and {dirs} other \
         folder(s) (the first: {first}), which are not in what was made from it",
        prefix.trim_end_matches('/'),
        files.len()
    ))
}

impl ArchiveTree {
    /// Walks over the archive so far (a new one each time a read goes back before where the
    /// last one was).
    pub fn passes(&self) -> u64 {
        self.reader.sessions()
    }

    /// Whether a walk over the archive (and its decoder) is held now.
    pub fn reading(&self) -> bool {
        self.reader.reading()
    }
}

impl Drop for ArchiveTree {
    fn drop(&mut self) {
        // How many walks the build's read order took: one is ideal; many on a solid archive
        // is what makes a build from it slow.
        if self.reader.sessions() > 0 {
            crate::log_at!(
                Info,
                "convert",
                "{}: {} pass(es) over the archive",
                self.describe,
                self.reader.sessions()
            );
        }
    }
}

fn read_error(path: &str, e: ReadError) -> ps5upload_fpkg::Error {
    match e {
        ReadError::Cancelled => ps5upload_fpkg::Error::Cancelled,
        ReadError::Failed(m) => ps5upload_fpkg::Error::Format(format!("{path}: {m}")),
    }
}

impl SourceTree for ArchiveTree {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        let size = self.sizes.get(path).copied().ok_or_else(|| {
            ps5upload_fpkg::Error::Format(format!("{path} is not in the archive's game folder"))
        })?;
        let len = usize::try_from(size).map_err(|_| {
            ps5upload_fpkg::Error::Format(format!("{path} is too large to read whole"))
        })?;
        self.read_range(path, 0, len)
    }

    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        let entry = format!("{}{path}", self.prefix);
        self.reader
            .read_at(&entry, offset, len)
            .map_err(|e| read_error(path, e))
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }

    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }

    fn release(&mut self) {
        self.reader.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing_of(paths: &[&str]) -> Listing {
        // A stored zip of empty-ish files, listed the real way.
        use std::io::Write;
        let dir = crate::test_tmp::temp_path(format!(
            "ps5u-archive-tree-{}-{}",
            std::process::id(),
            paths.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let z = dir.join("t.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for p in paths {
            zw.start_file(*p, opts).unwrap();
            zw.write_all(b"{}").unwrap();
        }
        zw.finish().unwrap();
        let l = stream::list(&z, None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        l
    }

    #[test]
    fn the_game_folder_is_found_however_it_is_wrapped() {
        let l = listing_of(&["sce_sys/param.json", "eboot.bin"]);
        assert_eq!(game_root(&l).unwrap(), "");
        let l = listing_of(&[
            "[SITE] X/PPSA1-app/sce_sys/param.json",
            "[SITE] X/readme.txt",
        ]);
        assert_eq!(game_root(&l).unwrap(), "[SITE] X/PPSA1-app/");
        // A game inside the game (a sample in its data) is part of it.
        let l = listing_of(&["g/sce_sys/param.json", "g/data/sub/sce_sys/param.json"]);
        assert_eq!(game_root(&l).unwrap(), "g/");
        let e = game_root(&listing_of(&[
            "a/sce_sys/param.json",
            "b/sce_sys/param.json",
        ]))
        .unwrap_err();
        assert!(e.contains("more than one game"), "{e}");
        let e = game_root(&listing_of(&["x/Game.exfat"])).unwrap_err();
        assert!(e.starts_with(NEEDS_UNPACK), "{e}");
        let e = game_root(&listing_of(&["readme.txt"])).unwrap_err();
        assert!(e.contains("no game"), "{e}");
        // macOS's sidecars are no game.
        let l = listing_of(&["g/sce_sys/param.json", "__MACOSX/g/sce_sys/param.json"]);
        assert_eq!(game_root(&l).unwrap(), "g/");
        // Junk beside the game is no note; the folder around it neither.
        assert_eq!(left_out_note(&l, "g/"), None);
        let l = listing_of(&["sce_sys/param.json", "eboot.bin"]);
        assert_eq!(left_out_note(&l, ""), None);
        // Anything else beside the game folder is left out, and said.
        let l = listing_of(&[
            "[SITE] X/PPSA1-app/sce_sys/param.json",
            "[SITE] X/readme.txt",
        ]);
        let note = left_out_note(&l, "[SITE] X/PPSA1-app/").expect("a note");
        assert!(
            note.contains("1 other file(s)") && note.contains("[SITE] X/readme.txt"),
            "{note}"
        );
        let l = listing_of(&["g/sce_sys/param.json", "update/sce_sys/x.bin"]);
        let note = left_out_note(&l, "g/").expect("a note");
        assert!(note.contains("update/sce_sys/x.bin"), "{note}");
    }
}
