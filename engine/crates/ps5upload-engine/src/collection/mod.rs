//! The Collection: every game kept in the user's folders, on this computer's drives or on a
//! saved server, grouped by Game ID. A port of PS Game Library (its `reindex.py`) into the
//! engine, so it runs wherever the engine runs. Nothing about any one machine is built in: the
//! folders are the user's own, and the index, covers and undo logs live in the engine's data
//! folder for the platform.
//!
//! The rules are the source app's, kept exactly: items are found by what they are, never by
//! the folder they sit in (depth 6, dot-names ignored); a package, image or game folder states
//! its own identity; a game's copies are its full copies (patches and DLC are add-ons); a
//! duplicate is more than one full copy; reclaimable space is every full copy but the largest.
//! The index keeps the source app's field names, so its exports and consumers keep working.
//!
//! Where ps5upload reads more than the Python scanner could, it does: `.ffpkg` (UFS2),
//! `.ffpfs`/`.ffpfsc` (PFS) and PS3 packages describe themselves here too.

pub mod console;
pub mod group;
pub mod identify;
pub mod junk;
pub mod online;
pub mod organize;
pub mod scan;
pub mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a location is, as the source app names it (`type`); the part before the dot is what
/// the dashboard filters on.
pub const ITEM_EXTS: &[(&str, &str)] = &[
    (".exfat", "mount.exfat"),
    (".ffpkg", "mount.ffpkg"),
    (".ffpfsc", "mount.ffpfsc"),
    (".ffpfs", "mount.ffpfs"),
    (".7z", "7z"),
    (".zip", "zip"),
    (".pkg", "pkg"),
];

/// What a package (or image, or folder) says about itself. Field names are the source app's.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PkgInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
    /// `base`, `patch` or `dlc`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default)]
    pub kind_confident: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind_reason: String,
    #[serde(default = "yes")]
    pub complete: bool,
    /// Why it could not be read, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn yes() -> bool {
    true
}

/// One place a game is: a package, an image, a folder, an archive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Location {
    /// The library root this location is under.
    #[serde(default)]
    pub root: String,
    /// The top folder under the root, for display only (nothing keys on it).
    #[serde(default)]
    pub container: String,
    pub name: String,
    /// `pkg`, `folder`, `rar`, `7z`, `zip`, `mount.exfat`, `mount.ffpkg`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// Relative to the root, `/`-separated.
    pub path: String,
    #[serde(default)]
    pub absolute_path: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    /// `created` (file birth time) or `modified` (birth time not available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_source: Option<String>,
    /// Unix seconds of `added_at`, for sorting (strings from other tools may differ in form).
    #[serde(default)]
    pub added_ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg: Option<PkgInfo>,
}

impl Location {
    /// A full copy of the game, as opposed to an add-on. A package whose kind could not be read
    /// counts as a copy: a missed duplicate costs more than a spurious one.
    pub fn is_copy(&self) -> bool {
        !matches!(
            self.pkg.as_ref().map(|p| p.kind.as_str()),
            Some("patch") | Some("dlc")
        )
    }
}

/// One game: everything in the collection with its Game ID.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Game {
    pub game_id: String,
    pub title: String,
    pub platform: String,
    #[serde(default)]
    pub sources: Vec<String>,
    pub locations: Vec<Location>,
    pub total_size_bytes: u64,
    #[serde(default)]
    pub copies: usize,
    #[serde(default)]
    pub is_duplicate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub added_ts: i64,
    /// The cover file in the covers folder, when one was read out of the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_cover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// How good the title's source is: 0 the Game ID, 1 a DLC's name, 2 the game's own.
    #[serde(skip)]
    pub title_rank: u8,
    /// Where the title came from: `package` (the game itself), `dlc`, `online`, or empty (the
    /// Game ID).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title_source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub total_games: usize,
    pub total_locations: usize,
    pub total_size_bytes: u64,
    pub duplicates_count: usize,
    pub reclaimable_bytes: u64,
}

/// A folder's cached deep size, valid while its depth-1 signature holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderCacheEntry {
    pub sig: String,
    pub size: u64,
}

/// A file's cached identity, valid while its size and mtime hold and it was read by this
/// version of the readers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PkgCacheEntry {
    pub size: u64,
    pub mtime: i64,
    pub pkg: PkgInfo,
    /// [`IDENTIFY_VERSION`] when it was read; 0 for an entry from PS Game Library.
    #[serde(default)]
    pub v: u32,
}

/// Raised whenever identification changes, so cached identities are read again.
pub const IDENTIFY_VERSION: u32 = 4;

/// The whole index, written to `<data dir>/collection/index.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub version: u32,
    /// Every root scanned.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The first root, for consumers of the source app's single-root index.
    #[serde(default)]
    pub library_root: String,
    pub generated_at: String,
    pub summary: Summary,
    pub games: BTreeMap<String, Game>,
    #[serde(default, rename = "_folder_cache")]
    pub folder_cache: BTreeMap<String, FolderCacheEntry>,
    #[serde(default, rename = "_pkg_cache")]
    pub pkg_cache: BTreeMap<String, PkgCacheEntry>,
}

pub const INDEX_VERSION: u32 = 1;

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The first `XXXX12345` Game ID in `s` (four letters, five digits, on word boundaries).
pub fn game_id_in(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 9 {
        return None;
    }
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    for i in 0..=b.len() - 9 {
        let w = &b[i..i + 9];
        if w[..4].iter().all(u8::is_ascii_alphabetic)
            && w[4..].iter().all(u8::is_ascii_digit)
            && (i == 0 || !word(b[i - 1]))
            && (i + 9 == b.len() || !word(b[i + 9]))
        {
            return Some(String::from_utf8_lossy(w).to_ascii_uppercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_print_as_utc_iso() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_733_554_277), "2024-12-07T06:51:17Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn game_ids_are_found_on_word_boundaries() {
        assert_eq!(
            game_id_in("Bloodborne - CUSA00900 - v1.09.pkg"),
            Some("CUSA00900".into())
        );
        assert_eq!(game_id_in("PPSA20052-app"), Some("PPSA20052".into()));
        assert_eq!(game_id_in("xCUSA00900"), None);
        assert_eq!(game_id_in("CUSA009001"), None);
        assert_eq!(game_id_in("ppsa01234.exfat"), Some("PPSA01234".into()));
    }
}
