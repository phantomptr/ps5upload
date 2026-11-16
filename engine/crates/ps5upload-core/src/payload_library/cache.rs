//! The local copies of payloads: files the user got earlier (older versions of the app downloaded
//! them; the app no longer downloads payloads). A send uses a local copy or a file the user picks.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{Library, LibraryError};

/// The error for a payload that has no local copy, or not the version asked for.
pub const NOT_ON_THIS_COMPUTER: &str = "not_on_this_computer";

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The URL without its query and fragment.
fn url_path(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or("")
}

/// SHA-256 of a file on disk.
pub(super) fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// The file name a payload is cached under, which is also the name Store in Payload Manager
/// gives it: the asset hint when it is a plain name; with no hint (a custom repo), the release
/// asset's own file name (`<name>.elf`, or a `.zip`'s stem); else `<id>.elf`. A hint and an asset
/// name are outside text, so anything that could name a path (`/`, `..`, `:`, `%`) falls back to
/// the id instead of being joined.
pub(super) fn cached_elf_name(id: &str, asset_name_hint: &str, asset_url: &str) -> String {
    let plain_base = |name: &str| -> Option<String> {
        let base = name
            .trim()
            .trim_end_matches(".elf")
            .trim_end_matches(".zip")
            .trim_matches(|c: char| !c.is_alphanumeric());
        let plain = !base.is_empty()
            && base
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !base.contains("..");
        plain.then(|| format!("{base}.elf"))
    };
    if !asset_name_hint.trim().is_empty() {
        return plain_base(asset_name_hint).unwrap_or_else(|| format!("{id}.elf"));
    }
    let asset_file = url_path(asset_url).rsplit('/').next().unwrap_or("");
    let lower = asset_file.to_ascii_lowercase();
    if lower.ends_with(".elf") || lower.ends_with(".zip") {
        if let Some(name) = plain_base(asset_file) {
            return name;
        }
    }
    format!("{id}.elf")
}

/// Remove every `.elf` in a payload's cache folder except `keep`: a copy cached under an earlier
/// name would be picked up just as well as the new one.
pub(super) fn remove_other_elfs(dir: &Path, keep: &str) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) == Some("elf")
            && f.file_name().to_str() != Some(keep)
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

impl Library {
    /// A payload's file ready to send: the local copy, when it is the version asked for (or when
    /// no version was asked for). Nothing is ever downloaded: without a copy the error says where
    /// the user gets the file.
    pub fn local_copy(&self, id: &str, tag: Option<&str>) -> Result<PathBuf, LibraryError> {
        self.resolve_entry(id)?;
        let local = self.local_entry(id);
        if let Some(local) = &local {
            if tag.is_none() || tag == Some(local.version.as_str()) {
                return Ok(PathBuf::from(&local.path));
            }
        }
        let info = self.catalog().into_iter().find(|p| p.id == id);
        let name = info
            .as_ref()
            .map(|p| p.display_name.clone())
            .unwrap_or_else(|| id.to_string());
        let page = info
            .as_ref()
            .map(|p| p.homepage.clone())
            .filter(|h| !h.is_empty())
            .map(|h| format!(" ({h})"))
            .unwrap_or_default();
        let what = match (tag, &local) {
            (Some(t), Some(l)) if !l.version.is_empty() => {
                format!("{name} {t} isn't on this computer (it has {})", l.version)
            }
            (Some(t), _) => format!("{name} {t} isn't on this computer"),
            (None, _) => format!("{name} isn't on this computer"),
        };
        Err(LibraryError::new(
            NOT_ON_THIS_COMPUTER,
            format!(
                "{what}. ps5upload no longer downloads payloads: get it from its project \
                 page{page} and send the file."
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{library, seed, TempDir};
    use super::*;

    const KSTUFF_ID: &str = "kstuff-echostretch";

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_new_copy_leaves_only_its_own_elf() {
        let d = TempDir::new("elf-cache");
        for name in ["custom-0a.elf", "kstuff.elf", "VERSION", "notes.txt"] {
            std::fs::write(d.0.join(name), b"x").unwrap();
        }
        remove_other_elfs(&d.0, "kstuff.elf");
        assert_eq!(files_in(&d.0), ["VERSION", "kstuff.elf", "notes.txt"]);
    }

    #[test]
    fn a_custom_hint_cannot_name_a_path() {
        let n = |hint: &str| cached_elf_name("custom-0a", hint, "");
        assert_eq!(n("kstuff"), "kstuff.elf");
        assert_eq!(n("elfldr-ps5.elf"), "elfldr-ps5.elf");
        assert_eq!(n("Tool-"), "Tool.elf");
        assert_eq!(n(""), "custom-0a.elf");
        // Leading and trailing punctuation is trimmed; only interior path text falls back.
        assert_eq!(n("..\\x"), "x.elf");
        for hint in ["a/../../b", "C:evil", "sub/dir", "a..b", "a\\b"] {
            assert_eq!(n(hint), "custom-0a.elf", "{hint}");
        }
    }

    #[test]
    fn a_custom_repo_payload_keeps_its_release_asset_name() {
        let n = |url: &str| cached_elf_name("custom-3fa9c21b04de", "", url);
        let gh = "https://github.com/someone/thing/releases/download/v1.2";
        assert_eq!(n(&format!("{gh}/MyPayload_v1.2.elf")), "MyPayload_v1.2.elf");
        assert_eq!(n(&format!("{gh}/MyPayload-1.2.zip")), "MyPayload-1.2.elf");
        assert_eq!(n(&format!("{gh}/thing.elf?raw=1")), "thing.elf");
        assert_eq!(
            cached_elf_name("custom-0a", "kstuff", &format!("{gh}/kstuff_v1.elf")),
            "kstuff.elf"
        );
        for bad in [
            "My%20Payload.elf",
            "a..b.elf",
            "notes.txt",
            "",
            "https://x/",
        ] {
            assert_eq!(
                n(&format!("{gh}/{bad}")),
                "custom-3fa9c21b04de.elf",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_local_copy_is_used_and_nothing_is_fetched() {
        let (_d, http, lib) = library("local-copy");
        let p = seed(&lib, KSTUFF_ID, "kstuff.elf", b"\x7FELF-one", "v1");
        assert_eq!(lib.local_copy(KSTUFF_ID, None).unwrap(), p);
        assert_eq!(lib.local_copy(KSTUFF_ID, Some("v1")).unwrap(), p);
        assert!(
            http.asked.lock().unwrap().is_empty(),
            "never asks the network"
        );
    }

    #[test]
    fn another_version_is_not_on_this_computer() {
        let (_d, http, lib) = library("local-copy-ver");
        seed(&lib, KSTUFF_ID, "kstuff.elf", b"\x7FELF-one", "v1");
        let e = lib.local_copy(KSTUFF_ID, Some("v2")).unwrap_err();
        assert_eq!(e.code, NOT_ON_THIS_COMPUTER);
        assert!(e.message.contains("v2 isn't on this computer (it has v1)"));
        assert!(e.message.contains("no longer downloads payloads"));
        assert!(http.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_missing_copy_says_where_to_get_it() {
        let (_d, http, lib) = library("local-copy-missing");
        let e = lib.local_copy("hush", None).unwrap_err();
        assert_eq!(e.code, NOT_ON_THIS_COMPUTER);
        assert!(e.message.starts_with("Hush isn't on this computer."), "{e}");
        assert!(e.message.contains("https://github.com/"), "{e}");
        assert!(http.asked.lock().unwrap().is_empty());
        assert_eq!(
            lib.local_copy("nope", None).unwrap_err().code,
            "unknown_payload"
        );
    }
}
