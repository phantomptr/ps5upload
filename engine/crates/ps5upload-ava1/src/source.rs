//! The `ava1::source::Source` views this crate's uploads read through: the engine's
//! `SourceFs` (local disk, or a saved SMB/FTP/SFTP server) and the explicit file list.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use ava1::source::{ReadAt, SeekReader, Source, SourceMeta};
use ps5upload_core::source_fs::SourceFs;

/// A source through the engine's `SourceFs`: local disk by default, a saved remote
/// server when the config carries one.
pub struct FsSource {
    fs: Arc<dyn SourceFs>,
    root: PathBuf,
}

impl FsSource {
    pub fn new(fs: Arc<dyn SourceFs>, root: PathBuf) -> Self {
        Self { fs, root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }
}

impl Source for FsSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Ok(Box::new(SeekReader::new(self.fs.open(&self.path(rel))?)))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        let mut out = Vec::new();
        for (p, is_dir) in self.fs.read_dir(&self.path(rel))? {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("a name is not UTF-8: {}", p.display()),
                    )
                })?
                .to_string();
            let len = if is_dir { 0 } else { self.fs.metadata(&p)?.len };
            // 0 = the backend reports no mtime (SPEC.md §11.4: skip-existing then
            // falls back to a content check, see `upload::apply_existing_policy`).
            let mtime = if is_dir {
                0
            } else {
                self.fs.mtime(&p).unwrap_or(0)
            };
            out.push((
                name,
                SourceMeta {
                    size: len,
                    mtime,
                    mode: if is_dir { 0o755 } else { 0o644 },
                    is_dir,
                },
            ));
        }
        Ok(out)
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        let m = self.fs.metadata(&self.path(rel))?;
        Ok(SourceMeta {
            size: m.len,
            mtime: if m.is_dir {
                0
            } else {
                self.fs.mtime(&self.path(rel)).unwrap_or(0)
            },
            mode: if m.is_dir { 0o755 } else { 0o644 },
            is_dir: m.is_dir,
        })
    }
}

/// Explicit (relative path → local file) pairs: the file-list upload.
pub struct ListSource {
    map: HashMap<String, PathBuf>,
}

impl ListSource {
    pub fn new(pairs: Vec<(String, PathBuf)>) -> Self {
        Self {
            map: pairs.into_iter().collect(),
        }
    }
}

impl Source for ListSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        let p = self
            .map
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_string()))?;
        Ok(Box::new(SeekReader::new(std::fs::File::open(p)?)))
    }

    fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a list source is never walked",
        ))
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        let p = self
            .map
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_string()))?;
        let m = std::fs::metadata(p)?;
        // A real mtime: these are local files, and the manifest is built from here.
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Ok(SourceMeta {
            size: m.len(),
            mtime,
            mode: 0o644,
            is_dir: false,
        })
    }
}

/// A folder source whose listed names differ from the files read: `(source path, console
/// path)` pairs, both relative to the folder, differing only in the last component. The
/// manifest, the skip-existing comparison and the console all see the console name.
pub struct RenamedSource {
    inner: Arc<dyn Source>,
    /// Source path -> console name (last component).
    to: HashMap<String, String>,
    /// Console path -> source path.
    back: HashMap<String, String>,
}

impl RenamedSource {
    pub fn new(inner: Arc<dyn Source>, renames: &[(String, String)]) -> Self {
        let last = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        Self {
            inner,
            to: renames.iter().map(|(f, t)| (f.clone(), last(t))).collect(),
            back: renames
                .iter()
                .map(|(f, t)| (t.clone(), f.clone()))
                .collect(),
        }
    }

    fn source_path<'a>(&'a self, rel: &'a str) -> &'a str {
        self.back.get(rel).map_or(rel, String::as_str)
    }
}

impl Source for RenamedSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        self.inner.open(self.source_path(rel))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        let mut out = self.inner.list(rel)?;
        for (name, _) in &mut out {
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if let Some(new) = self.to.get(&path) {
                *name = new.clone();
            }
        }
        Ok(out)
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        self.inner.stat(self.source_path(rel))
    }

    fn close(&self) {
        self.inner.close()
    }
}

/// A folder downloaded with `escape_names` (`download::to_local_with_in`), read under the
/// console's own names: each kept name (`host_names::escape_name`) is listed as the name it
/// stands for and read from the file that holds it. A name that is not one this app keeps is
/// refused, so two files never stand for one console name (#729).
pub struct KeptNamesSource {
    inner: Arc<dyn Source>,
}

impl KeptNamesSource {
    pub fn new(inner: Arc<dyn Source>) -> Self {
        Self { inner }
    }

    /// The file a console path is kept in.
    fn kept(rel: &str) -> io::Result<String> {
        if rel.is_empty() {
            return Ok(String::new());
        }
        ps5upload_core::host_names::escape_path(rel)
            .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))
    }
}

impl Source for KeptNamesSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        self.inner.open(&Self::kept(rel)?)
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        let mut out = self.inner.list(&Self::kept(rel)?)?;
        for (name, _) in &mut out {
            *name = ps5upload_core::host_names::unescape_name(name)
                .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))?;
        }
        Ok(out)
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        self.inner.stat(&Self::kept(rel)?)
    }

    fn close(&self) {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava1::source::LocalSource;

    #[test]
    fn a_renamed_module_is_listed_under_its_new_name_and_read_from_the_old_file() {
        let d = std::env::temp_dir().join(format!("p5a-renamed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Media/Modules")).unwrap();
        std::fs::write(d.join("eboot.bin"), b"eboot").unwrap();
        std::fs::write(d.join("Media/Modules/Il2cppUserAssemblies.prx"), b"module").unwrap();
        std::fs::write(d.join("Media/Modules/PS5Util.prx"), b"util").unwrap();
        let src = RenamedSource::new(
            Arc::new(LocalSource::new(d.clone())),
            &[(
                "Media/Modules/Il2cppUserAssemblies.prx".into(),
                "Media/Modules/Il2CppUserAssemblies.prx".into(),
            )],
        );
        let m = ava1::manifest::walk(&src, &|_| false).unwrap();
        let paths: Vec<&str> = m.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "Media",
                "Media/Modules",
                "Media/Modules/Il2CppUserAssemblies.prx",
                "Media/Modules/PS5Util.prx",
                "eboot.bin"
            ]
        );
        let new = "Media/Modules/Il2CppUserAssemblies.prx";
        assert_eq!(src.stat(new).unwrap().size, 6);
        let mut buf = [0u8; 6];
        let n = src.open(new).unwrap().read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"module");
        let _ = std::fs::remove_dir_all(&d);
    }
}
