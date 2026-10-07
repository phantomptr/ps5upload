//! A game on another machine — a saved server, or the console — read in place.
//!
//! The engine knows how to reach those machines; this crate only needs files it can open,
//! stat and list, which [`RemoteFiles`] names. [`open_remote`] turns one into the same
//! [`SourceTree`] a local folder or image gives, so the builder reads the game once, in plan
//! order, straight off the network. The reader a backend returns should read ahead: the
//! builder asks for block-sized ranges, and one round trip each would be slow.
//!
//! `remote://` and `ps5://` paths reach [`crate::source::open`]; the engine [`register`]s the
//! opener that resolves them.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::source::{is_junk, SourceFile, SourceTree};
use crate::{format_err, Error, ReadSeek, Result};

/// Files on another machine, by `/`-separated path.
pub trait RemoteFiles: Send + Sync {
    fn open(&self, path: &str) -> std::io::Result<Box<dyn ReadSeek>>;
    /// `(size, is_dir)`.
    fn stat(&self, path: &str) -> std::io::Result<(u64, bool)>;
    /// Direct children as `(name, is_dir, size)`.
    fn list(&self, dir: &str) -> std::io::Result<Vec<(String, bool, u64)>>;
    /// Where the files are, for logs ("server NAS").
    fn label(&self) -> String;
}

/// Resolves a `remote://…` or `ps5://…` path to the files it names and the path on them.
pub type Opener = dyn Fn(&str) -> Result<(Arc<dyn RemoteFiles>, String)> + Send + Sync;

static OPENER: RwLock<Option<Box<Opener>>> = RwLock::new(None);

/// Install the opener for `remote://` and `ps5://` paths (the engine does, at startup).
pub fn register(opener: Box<Opener>) {
    *OPENER.write().unwrap_or_else(|e| e.into_inner()) = Some(opener);
}

/// A path read through the registered opener: `remote://<connection>/…` (a saved server) or
/// `ps5://<host>/…` (the console).
pub fn is_remote(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p.starts_with("remote://") || p.starts_with("ps5://"))
}

/// The tree a `remote://` or `ps5://` path names, through the registered opener.
pub fn open_path(path: &Path) -> Result<Box<dyn SourceTree>> {
    let url = path.to_string_lossy();
    let (files, inner) = {
        let guard = OPENER.read().unwrap_or_else(|e| e.into_inner());
        let Some(open) = guard.as_ref() else {
            return format_err(format!("{url}: this build cannot read saved servers"));
        };
        open(&url)?
    };
    open_remote(files, &inner)
}

/// One file a `remote://` or `ps5://` path names, and its size (a package the viewer reads).
pub fn open_file(path: &Path) -> Result<(Box<dyn ReadSeek>, u64)> {
    let url = path.to_string_lossy();
    let (files, inner) = {
        let guard = OPENER.read().unwrap_or_else(|e| e.into_inner());
        let Some(open) = guard.as_ref() else {
            return format_err(format!("{url}: this build cannot read saved servers"));
        };
        open(&url)?
    };
    let (size, is_dir) = files.stat(&inner).map_err(|e| io(&inner, e))?;
    if is_dir {
        return format_err(format!("{url} is a folder, not a file"));
    }
    Ok((files.open(&inner).map_err(|e| io(&inner, e))?, size))
}

fn io(path: &str, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(e.kind(), format!("{path}: {e}")))
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// A folder or image at `path` on `files`.
pub fn open_remote(files: Arc<dyn RemoteFiles>, path: &str) -> Result<Box<dyn SourceTree>> {
    let (size, is_dir) = files.stat(path).map_err(|e| io(path, e))?;
    if is_dir {
        return Ok(Box::new(RemoteFolder::open(files, path)?));
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let label = format!("{ext} {path} on {}", files.label());
    let reader = || files.open(path).map_err(|e| io(path, e));
    match ext.as_str() {
        "exfat" => exfat_tree(reader()?, size, label),
        "ffpkg" | "ufs2" => Ok(Box::new(crate::ufs2_source::Ufs2Source::from_reader(
            reader()?,
            label,
        )?)),
        "ffpfsc" => crate::source::ffpfsc_tree(&reader, &label),
        "ffpfs" => crate::pfs_source::open(reader()?, label),
        _ => format_err(format!(
            "{path} is neither a folder nor a supported image (.exfat, .ffpkg, .ffpfs, .ffpfsc)"
        )),
    }
}

pub(crate) fn exfat_tree(
    reader: Box<dyn ReadSeek>,
    len: u64,
    label: String,
) -> Result<Box<dyn SourceTree>> {
    let volume = crate::exfat::ExFat::from_file(crate::PkgFile::from_reader(reader, len), &label)?;
    Ok(Box::new(crate::exfat::ExFatSource::from_volume(
        volume, label,
    )?))
}

/// A game folder on another machine.
struct RemoteFolder {
    files: Arc<dyn RemoteFiles>,
    root: String,
    list: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    /// The file read last, kept open: the builder reads each file front to back.
    open: Option<(String, Box<dyn ReadSeek>)>,
}

impl RemoteFolder {
    fn open(files: Arc<dyn RemoteFiles>, root: &str) -> Result<Self> {
        let mut list = Vec::new();
        let mut empty = Vec::new();
        walk(files.as_ref(), root, "", &mut list, &mut empty, 0)?;
        list.sort_by(|a, b| a.path.cmp(&b.path));
        empty.sort();
        Ok(Self {
            files,
            root: root.trim_end_matches('/').to_string(),
            list,
            empty_dirs: empty,
            open: None,
        })
    }

    fn size_of(&self, path: &str) -> Result<u64> {
        self.list
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map(|i| self.list[i].size)
            .map_err(|_| Error::Format(format!("{path} is not in {}", self.root)))
    }

    fn reader(&mut self, path: &str) -> Result<&mut Box<dyn ReadSeek>> {
        if self.open.as_ref().is_none_or(|(p, _)| p != path) {
            let full = join(&self.root, path);
            let r = self.files.open(&full).map_err(|e| io(&full, e))?;
            self.open = Some((path.to_string(), r));
        }
        Ok(&mut self.open.as_mut().expect("just opened").1)
    }
}

/// Walks no deeper than this: a game tree is shallow, a link loop on a server is not.
const MAX_DEPTH: u32 = 64;

/// Like the local walk: junk skipped, directories left empty recorded. Returns whether `dir`
/// kept anything.
fn walk(
    files: &dyn RemoteFiles,
    root: &str,
    rel: &str,
    out: &mut Vec<SourceFile>,
    empty: &mut Vec<String>,
    depth: u32,
) -> Result<bool> {
    if depth > MAX_DEPTH {
        return format_err(format!("{root}/{rel} nests deeper than {MAX_DEPTH} levels"));
    }
    let dir = if rel.is_empty() {
        root.to_string()
    } else {
        join(root, rel)
    };
    let mut kept = false;
    for (name, is_dir, size) in files.list(&dir).map_err(|e| io(&dir, e))? {
        if is_junk(&name) || name == "." || name == ".." || name.contains('/') {
            continue;
        }
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if is_dir {
            if !walk(files, root, &child, out, empty, depth + 1)? {
                empty.push(child);
            }
        } else {
            out.push(SourceFile { path: child, size });
        }
        kept = true;
    }
    Ok(kept)
}

impl SourceTree for RemoteFolder {
    fn files(&self) -> &[SourceFile] {
        &self.list
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self.size_of(path)?;
        let len = usize::try_from(size)
            .map_err(|_| Error::Format(format!("{path} is too large to read whole")))?;
        self.read_range(path, 0, len)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let size = self.size_of(path)?;
        let want = (size.saturating_sub(offset)).min(len as u64) as usize;
        let r = self.reader(path)?;
        r.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            match r.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    // A failed reader is not reused.
                    self.open = None;
                    return Err(io(path, e));
                }
            }
        }
        if filled < want {
            return format_err(format!(
                "{path}: the server ended the file after {} of {want} bytes at {offset}",
                filled
            ));
        }
        Ok(buf)
    }

    fn describe(&self) -> String {
        format!("folder {} on {}", self.root, self.files.label())
    }
}
