//! A game archive (`.zip`, `.7z`, `.rar`, a multi-part RAR set from its first part) read in
//! place: the reader is `ps5upload_archive::stream` (shared with the converter that runs on the
//! PS5, which says how it works and what memory it holds), with what only the engine has added:
//! `.rar` through the separate `ps5upload-unrar` program (`crate::transfer::rar_walk`), and zip
//! entries compressed with zstd (`crate::tar_archive::ZipZstdEntry`).

use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

pub use ps5upload_archive::stream::*;

/// The files and folders of the archive at `path` (see `ps5upload_archive::stream::list`), with
/// the engine's `.rar` and zstd readers in place.
pub fn list(path: &Path, password: Option<&str>) -> Result<Listing> {
    set_extras(Box::new(EngineExtras));
    ps5upload_archive::stream::list(path, password)
}

struct EngineExtras;

impl Extras for EngineExtras {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn rar_list(
        &self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<(Vec<Entry>, Vec<String>, bool)> {
        let layout = crate::transfer::rar_layout(path, password, &[])?;
        let files = layout
            .files
            .into_iter()
            .map(|(path, size)| Entry { path, size })
            .collect();
        Ok((files, layout.dirs, layout.solid))
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn rar_list(&self, _: &Path, _: Option<&str>) -> Result<(Vec<Entry>, Vec<String>, bool)> {
        anyhow::bail!("RAR archives can't be opened on a phone; use .zip or .7z")
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn rar_walk(
        &self,
        path: &Path,
        password: Option<&str>,
        sink: &mut dyn RarSink,
        halt: &AtomicBool,
    ) -> std::result::Result<(), RarFail> {
        use crate::transfer::{RarWalkError, RarWalkSink};
        struct Adapter<'a>(&'a mut dyn RarSink);
        impl RarWalkSink for Adapter<'_> {
            fn visit(&mut self, ordinal: u64, path: &str) -> bool {
                self.0.visit(ordinal, path)
            }
            fn want(&mut self, ordinal: u64, path: &str, size: u64) -> bool {
                self.0.want(ordinal, path, size)
            }
            fn begin(&mut self, path: &str) -> std::io::Result<()> {
                self.0.begin(path)
            }
            fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
                self.0.data(bytes)
            }
            fn end(&mut self) -> std::io::Result<()> {
                self.0.end()
            }
            fn skipped(&mut self, n: u64) {
                self.0.skipped(n)
            }
        }
        match crate::transfer::rar_walk(path, password, &[], 0, &mut Adapter(sink), halt) {
            Ok(()) => Ok(()),
            Err(RarWalkError::Cancelled) => Err(RarFail::Cancelled),
            Err(RarWalkError::Sink(_)) => Err(RarFail::Sink),
            Err(RarWalkError::Failed { message, .. }) => Err(RarFail::Failed(message)),
        }
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn rar_walk(
        &self,
        _: &Path,
        _: Option<&str>,
        _: &mut dyn RarSink,
        _: &AtomicBool,
    ) -> std::result::Result<(), RarFail> {
        Err(RarFail::Failed(
            "RAR archives can't be opened on a phone; use .zip or .7z".into(),
        ))
    }

    fn zip_zstd<'a>(
        &self,
        raw: Box<dyn Read + 'a>,
        crc: u32,
        size: u64,
    ) -> Result<Box<dyn Read + 'a>> {
        Ok(Box::new(crate::tar_archive::ZipZstdEntry::new(
            raw, crc, size,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    /// A zip entry compressed with zstd reads through the engine's decoder; the shared reader
    /// alone refuses it.
    #[test]
    fn zstd_zip_entries_read_through_the_engine() {
        let d = std::env::temp_dir().join(format!("ps5u-archive-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let packed = zstd::stream::encode_all(&data[..], 3).unwrap();
        let path = d.join("z.zip");
        // A stored entry's bytes rewritten as method 93 with zstd's stream: what 7-Zip ZS writes.
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("g/a.bin", opts).unwrap();
        zw.write_all(&packed).unwrap();
        zw.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let crc = crc32fast::hash(&data);
        patch_zip(&mut bytes, crc, data.len() as u32);
        std::fs::write(&path, bytes).unwrap();
        let listing = list(&path, None).unwrap();
        let mut r = Reader::new(&path, None, listing, Arc::new(|| false));
        assert_eq!(r.read_at("g/a.bin", 0, data.len()).unwrap(), data);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Sets the one entry's method to 93, its CRC and unpacked size, in both headers.
    fn patch_zip(b: &mut [u8], crc: u32, size: u32) {
        let local = 0;
        assert_eq!(&b[local..local + 4], b"PK\x03\x04");
        b[local + 8..local + 10].copy_from_slice(&93u16.to_le_bytes());
        b[local + 14..local + 18].copy_from_slice(&crc.to_le_bytes());
        b[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
        let central = b.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        b[central + 10..central + 12].copy_from_slice(&93u16.to_le_bytes());
        b[central + 16..central + 20].copy_from_slice(&crc.to_le_bytes());
        b[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
    }

    /// A RAR's password: the right one reads, a wrong one or none is told apart (the app asks
    /// again), for content and header encryption. Needs the `ps5upload-unrar` helper built.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn rar_passwords_right_wrong_and_missing() {
        if !crate::rar_helper::helper_path().is_file() {
            eprintln!("skipped: the ps5upload-unrar helper is not built");
            return;
        }
        let fixture = |n: &str| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/rar")
                .join(n)
        };
        let read = |path: &Path, pw: Option<&str>| -> Result<Vec<u8>, String> {
            let listing = list(path, pw).map_err(|e| format!("{e:#}"))?;
            let mut r = Reader::new(path, pw, listing, Arc::new(|| false));
            r.read_at(".gitignore", 0, 100).map_err(|e| e.to_string())
        };
        let content = fixture("crypted.rar");
        assert_eq!(
            read(&content, Some("unrar")).unwrap(),
            b"target\nCargo.lock\n"
        );
        let e = read(&content, Some("wrong")).unwrap_err();
        assert!(e.contains("rar_password_wrong"), "{e}");
        let e = read(&content, None).unwrap_err();
        assert!(e.contains("rar_password_required"), "{e}");
        let headers = fixture("comment-hpw-password.rar");
        assert_eq!(
            read(&headers, Some("password")).unwrap(),
            b"target\nCargo.lock\n"
        );
        let e = read(&headers, None).unwrap_err();
        assert!(e.contains("rar_password_required"), "{e}");
        let e = read(&headers, Some("nope")).unwrap_err();
        assert!(e.contains("rar_password"), "{e}");
    }
}
