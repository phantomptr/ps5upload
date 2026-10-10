//! A game folder as a packed game: what an image writer is given in place of the folder.
//!
//! [`prepare`] packs the folder's files into volumes in a spool folder and returns an
//! [`Overlay`]: the folder minus every packed original, plus a regenerated `ampr_emu.index`
//! (listing the original files, which is what `ampr_emu` resolves), the manifest and its
//! sidecars, and the volumes. A folder emptied by packing is kept as an empty folder.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::config::{pack_output_glob, Config};
use super::{glob, pack, Control, Output};
use crate::ampr_index;
use crate::source::{SourceFile, SourceTree};
use crate::{format_err, Error, Result};

/// Files an earlier AMPR build or run left, which a fresh index and pack set replace.
fn generated(path: &str, config: &Config, pack_glob: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let index = config.index_name.to_ascii_lowercase();
    lower == ampr_index::NAME
        || lower == "ampr_commands.bin"
        || lower == "apr_emu.log"
        || lower == index
        || lower == format!("{index}.crc")
        || lower == format!("{index}.runtime")
        || glob::matches_any(path, &[pack_glob])
}

/// What the image holds instead of the folder.
pub struct Overlay<'a> {
    source: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    memory: HashMap<String, Vec<u8>>,
    volumes: HashMap<String, PathBuf>,
    open: Option<(String, std::fs::File)>,
    label: String,
}

impl SourceTree for Overlay<'_> {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self
            .files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.size)
            .ok_or_else(|| Error::Format(format!("{path} is not in the packed image")))?;
        self.read_range(path, 0, size as usize)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(bytes) = self.memory.get(path) {
            let at = (offset as usize).min(bytes.len());
            return Ok(bytes[at..(at + len).min(bytes.len())].to_vec());
        }
        if let Some(file) = self.volumes.get(path) {
            if self.open.as_ref().is_none_or(|(p, _)| p != path) {
                self.open = Some((path.to_string(), std::fs::File::open(file)?));
            }
            let (_, f) = self.open.as_mut().unwrap();
            f.seek(SeekFrom::Start(offset))?;
            let mut buf = Vec::with_capacity(len);
            f.by_ref().take(len as u64).read_to_end(&mut buf)?;
            return Ok(buf);
        }
        self.source.read_range(path, offset, len)
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// Pack `source` by `config` into `spool` (which must not exist yet) and return the tree the
/// image is written from, with the pack report. Every file is listed in the regenerated
/// `ampr_emu.index` at `mtime`.
pub fn prepare<'a>(
    source: &'a mut dyn SourceTree,
    config: &Config,
    spool: &Path,
    mtime: i64,
    control: &mut Control,
) -> Result<(Overlay<'a>, Output)> {
    let pack_glob = pack_output_glob(&config.pack_pattern)?;
    if source.files().iter().any(|f| {
        f.path.eq_ignore_ascii_case(&config.index_name) || glob::matches_any(&f.path, &[&pack_glob])
    }) {
        return format_err(
            "the folder already carries AMPR asset packs; its packed files are not there to \
             pack again",
        );
    }
    let logical: Vec<ampr_index::Entry> = source
        .files()
        .iter()
        .filter(|f| !generated(&f.path, config, &pack_glob))
        .map(|f| ampr_index::Entry {
            path: f.path.clone(),
            size: f.size,
            mtime,
        })
        .collect();
    let index = ampr_index::build_entries(&logical).map_err(Error::Format)?;
    let entries = ampr_index::parse(&index)
        .ok_or_else(|| Error::Format("the generated index does not read back".into()))?;
    std::fs::create_dir(spool).map_err(|e| Error::Format(format!("{}: {e}", spool.display())))?;
    let output = match pack::build(&entries, source, config, spool, control) {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_dir_all(spool);
            return Err(e);
        }
    };

    let sizes: HashMap<&str, u64> = source
        .files()
        .iter()
        .map(|f| (f.path.as_str(), f.size))
        .collect();
    let mut files: Vec<SourceFile> = output
        .loose
        .iter()
        .map(|p| SourceFile {
            path: p.clone(),
            size: sizes[p.as_str()],
        })
        .collect();
    let mut memory = HashMap::new();
    files.push(SourceFile {
        path: ampr_index::NAME.to_string(),
        size: index.len() as u64,
    });
    memory.insert(ampr_index::NAME.to_string(), index);
    for (name, bytes) in output.small_files() {
        files.push(SourceFile {
            path: name.clone(),
            size: bytes.len() as u64,
        });
        memory.insert(name, bytes.to_vec());
    }
    let mut volumes = HashMap::new();
    for (name, path) in &output.volumes {
        files.push(SourceFile {
            path: name.clone(),
            size: std::fs::metadata(path)?.len(),
        });
        volumes.insert(name.clone(), path.clone());
    }

    // Folders packing emptied stay, as empty folders: only the innermost need naming.
    let mut kept: BTreeSet<String> = BTreeSet::new();
    let source_empty: Vec<String> = source
        .empty_dirs()
        .iter()
        .map(|d| d.trim_matches('/').to_string())
        .collect();
    for d in &source_empty {
        kept.insert(d.clone());
    }
    for path in files
        .iter()
        .map(|f| f.path.as_str())
        .chain(source_empty.iter().map(|d| d.as_str()))
    {
        let mut p = path;
        while let Some((dir, _)) = p.rsplit_once('/') {
            kept.insert(dir.to_string());
            p = dir;
        }
    }
    let mut emptied: BTreeSet<String> = BTreeSet::new();
    for p in &output.packed {
        let mut p = p.as_str();
        while let Some((dir, _)) = p.rsplit_once('/') {
            if !kept.contains(dir) {
                emptied.insert(dir.to_string());
            }
            p = dir;
        }
    }
    let mut empty_dirs = source_empty;
    for d in &emptied {
        let prefix = format!("{d}/");
        if !emptied.iter().any(|o| o.starts_with(&prefix)) {
            empty_dirs.push(d.clone());
        }
    }
    let label = format!("{} with LZ4 asset packs", source.describe());
    Ok((
        Overlay {
            source,
            files,
            empty_dirs,
            memory,
            volumes,
            open: None,
            label,
        },
        output,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image holds the loose files and the pack set, not the packed originals; a folder
    /// packing emptied stays (only the innermost is named), and the folder's own empty folders
    /// are kept as they were.
    #[test]
    fn the_overlay_swaps_packed_files_for_the_pack_set() {
        let root = std::env::temp_dir().join(format!(
            "ampr-overlay-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("game");
        for (p, d) in [
            ("eboot.bin", vec![1u8; 100]),
            ("d/a.bin", vec![b'a'; 70_000]),
            ("x/keep.txt", b"keep".to_vec()),
            ("x/y/z.bin", vec![b'z'; 1000]),
            // A stale index and a run's trace are replaced, never carried.
            ("ampr_emu.index", b"stale".to_vec()),
            ("ampr_commands.bin", b"trace".to_vec()),
        ] {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, d).unwrap();
        }
        std::fs::create_dir_all(src.join("d/shaders")).unwrap();
        let mut tree = crate::source::FolderSource::open(&src).unwrap();
        let config = super::super::default_profile(9, 16);
        let spool = root.join("spool");
        let (mut overlay, out) =
            prepare(&mut tree, &config, &spool, 7, &mut Control::default()).unwrap();
        let mut names: Vec<String> = overlay.files().iter().map(|f| f.path.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "ampr_assets-000.pak",
                "ampr_assets.index",
                "ampr_assets.index.crc",
                "ampr_emu.index",
                "eboot.bin",
                "x/keep.txt"
            ]
        );
        let mut empty = overlay.empty_dirs().to_vec();
        empty.sort();
        assert_eq!(empty, ["d/shaders", "x/y"]);
        assert_eq!(out.packed.len(), 2);
        let index = ampr_index::parse(&overlay.read("ampr_emu.index").unwrap()).unwrap();
        let mut listed: Vec<&str> = index.iter().map(|e| e.path.as_str()).collect();
        listed.sort();
        assert_eq!(listed, ["d/a.bin", "eboot.bin", "x/keep.txt", "x/y/z.bin"]);
        assert!(index.iter().all(|e| e.mtime == 7));
        // A volume reads back whole through the overlay.
        let pak = overlay.read("ampr_assets-000.pak").unwrap();
        assert_eq!(
            pak,
            std::fs::read(spool.join("ampr_assets-000.pak")).unwrap()
        );
        drop(overlay);

        // A folder that already carries packs is refused: its packed originals are gone.
        std::fs::write(src.join("ampr_assets.index"), b"x").unwrap();
        let mut tree = crate::source::FolderSource::open(&src).unwrap();
        let e = prepare(
            &mut tree,
            &config,
            &root.join("spool2"),
            7,
            &mut Control::default(),
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("already carries"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
