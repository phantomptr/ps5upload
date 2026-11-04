//! Where the Collection keeps its state: `<data dir>/collection/` holds `settings.json`,
//! `index.json` and `covers/`. Writes are atomic (write a temp file, rename).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Library;

/// Automatic refresh intervals offered, in seconds (the source app's list).
pub const REFRESH_CHOICES: &[u64] = &[30, 60, 300, 900, 1800, 3600, 7200, 86_400];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Folders scanned, in order.
    #[serde(default)]
    pub roots: Vec<String>,
    /// Seconds between automatic scans; `None` turns automatic refresh off.
    #[serde(default = "default_refresh")]
    pub refresh_secs: Option<u64>,
    /// Remove AppleDouble `._` sidecars in every folder a scan walks.
    #[serde(default)]
    pub sweep_sidecars: bool,
    /// Where the engine has no trash (Docker, a headless server): Move to Trash deletes for
    /// good instead. Off unless the user turns it on; every delete is still confirmed.
    #[serde(default)]
    pub allow_permanent_delete: bool,
}

fn default_refresh() -> Option<u64> {
    Some(60)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            refresh_secs: default_refresh(),
            sweep_sidecars: false,
            allow_permanent_delete: false,
        }
    }
}

impl Settings {
    /// Checks and tidies settings a client sent: roots trimmed and de-duplicated, a refresh
    /// interval from the offered list.
    pub fn validated(mut self) -> Result<Self, String> {
        let mut seen = std::collections::HashSet::new();
        self.roots = self
            .roots
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty() && seen.insert(r.clone()))
            .collect();
        if let Some(s) = self.refresh_secs {
            if !REFRESH_CHOICES.contains(&s) {
                return Err(format!(
                    "refresh interval {s}s is not one of {REFRESH_CHOICES:?}"
                ));
            }
        }
        Ok(self)
    }
}

pub fn dir() -> Option<PathBuf> {
    crate::remote::store::data_dir().map(|d| d.join("collection"))
}

pub fn covers_dir() -> Option<PathBuf> {
    dir().map(|d| d.join("covers"))
}

/// `<size>-<modified ns>` of a saved cover, or None when it cannot be read.
pub fn cover_stamp(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let ns = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(format!("{}-{ns}", m.len()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_settings_at(dir: &Path) -> Settings {
    std::fs::read(dir.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings_at(dir: &Path, s: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("settings.json"), &bytes)
}

pub fn load_index_at(dir: &Path) -> Option<Library> {
    let bytes = std::fs::read(dir.join("index.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_index_at(dir: &Path, lib: &Library) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(lib).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("index.json"), &bytes)
}

// ── Exports (the source app's `libexport.py`, field for field) ─────────────────

/// `1.5 GiB`, `2.0 GiB`, `0 B`: the engine's one size formatter.
pub fn format_bytes(n: u64) -> String {
    ps5upload_core::units::iec_bytes(n)
}

/// `Sep 22, 2026`, or `Unknown`.
pub fn format_date(iso: Option<&str>) -> String {
    // `get`: an index can come from another tool, and a date from it may not be ASCII.
    let Some((y, m, d)) = iso.and_then(|s| Some((s.get(..4)?, s.get(5..7)?, s.get(8..10)?))) else {
        return "Unknown".into();
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match (m.parse::<usize>(), d.parse::<u32>()) {
        (Ok(m), Ok(d)) if (1..=12).contains(&m) => format!("{} {d}, {y}", months[m - 1]),
        _ => "Unknown".into(),
    }
}

/// The index as consumers see it: without the scanner's caches.
pub fn public_json(lib: &Library) -> serde_json::Value {
    let mut v = serde_json::to_value(lib).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| !k.starts_with('_'));
    }
    v
}

fn csv_field(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "\"\""))
}

pub fn to_csv(lib: &Library) -> String {
    let header = [
        "Game ID",
        "Title",
        "Platform",
        "Location Count",
        "Total Size Bytes",
        "Total Size Formatted",
        "Added",
        "First Added",
        "Is Duplicate",
        "Locations (path | size | added)",
    ];
    let mut rows = vec![header
        .iter()
        .map(|h| csv_field(h))
        .collect::<Vec<_>>()
        .join(",")];
    for g in lib.games.values() {
        let locs = g
            .locations
            .iter()
            .map(|l| {
                format!(
                    "{} | {} | {}",
                    l.path,
                    l.size_bytes,
                    l.added_at.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let cells = [
            g.game_id.clone(),
            if g.title.is_empty() {
                g.game_id.clone()
            } else {
                g.title.clone()
            },
            g.platform.clone(),
            g.locations.len().to_string(),
            g.total_size_bytes.to_string(),
            format_bytes(g.total_size_bytes),
            g.added_at.clone().unwrap_or_default(),
            g.first_added_at.clone().unwrap_or_default(),
            if g.is_duplicate { "Yes" } else { "No" }.to_string(),
            locs,
        ];
        rows.push(
            cells
                .iter()
                .map(|c| csv_field(c))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    // A byte-order mark so Excel reads UTF-8 titles correctly.
    format!("\u{feff}{}", rows.join("\r\n"))
}

pub fn to_markdown(lib: &Library, generated: &str) -> String {
    let s = &lib.summary;
    let cell = |v: &str| v.replace('|', "\\|");
    let mut md = String::from("# Game Collection Report\n\n");
    md += &format!("**Generated**: {generated}\n\n");
    md += &format!("**Total Unique Titles**: {}\n\n", s.total_games);
    md += &format!("**Total Locations**: {}\n\n", s.total_locations);
    md += &format!(
        "**Total Storage**: {}\n\n",
        format_bytes(s.total_size_bytes)
    );
    md += &format!(
        "**Duplicates**: {} titles ({} reclaimable)\n\n",
        s.duplicates_count,
        format_bytes(s.reclaimable_bytes)
    );
    md += "| Game ID | Title | Platform | Copies | Added | Total Size | Locations |\n";
    md += "|---|---|---|---|---|---|---|\n";
    for g in lib.games.values() {
        let locs = g
            .locations
            .iter()
            .map(|l| {
                format!(
                    "`{}` ({}, added {})",
                    cell(&l.path),
                    format_bytes(l.size_bytes),
                    format_date(l.added_at.as_deref())
                )
            })
            .collect::<Vec<_>>()
            .join("<br>");
        md += &format!(
            "| **{}** | {} | {} | {} | {} | {} | {} |\n",
            cell(&g.game_id),
            cell(if g.title.is_empty() {
                &g.game_id
            } else {
                &g.title
            }),
            cell(&g.platform),
            g.locations.len(),
            format_date(g.added_at.as_deref()),
            format_bytes(g.total_size_bytes),
            locs
        );
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_dates_print_like_the_source_app() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 << 30), "2.0 GiB");
        assert_eq!(format_date(Some("2026-09-22T10:00:00Z")), "Sep 22, 2026");
        assert_eq!(format_date(None), "Unknown");
    }

    #[test]
    fn a_date_from_another_tool_never_panics() {
        for odd in [
            "",
            "2026",
            "2026-09-2",
            "二〇二六年九月二十二日",
            "2026-é9-22",
            "20é6-09-22xx",
        ] {
            assert_eq!(format_date(Some(odd)), "Unknown", "{odd}");
        }
    }

    /// An index written before split sets, unreadable folders and the lookups' memory. Each of
    /// those fields is new since: it must read as empty, and a save must not grow it for nothing.
    #[test]
    fn an_older_index_still_loads() {
        let dir = std::env::temp_dir().join(format!("ps5u-old-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.json"),
            br#"{"version":1,"roots":["/g"],"library_root":"/g","generated_at":"2026-10-01T00:00:00Z",
              "summary":{"total_games":1,"total_locations":1,"total_size_bytes":26,"duplicates_count":0,"reclaimable_bytes":0},
              "games":{"CUSA00900":{"game_id":"CUSA00900","title":"Bloodborne","platform":"PS4","locations":[
                {"root":"/g","container":"fpkg","name":"b.pkg","type":"pkg","path":"fpkg/b.pkg","absolute_path":"/g/fpkg/b.pkg",
                 "size_bytes":26,"pkg":{"kind":"base","title":"Bloodborne","complete":true}}],"total_size_bytes":26}},
              "missing_roots":["/gone"],
              "_folder_cache":{"/g|x":{"sig":"s","size":1}},
              "_pkg_cache":{"/g|fpkg/b.pkg":{"size":26,"mtime":5,"pkg":{"kind":"base"},"v":6}}}"#,
        )
        .unwrap();
        let lib = load_index_at(&dir).expect("an older index loads");
        assert_eq!(
            lib.games["CUSA00900"].locations[0].parts,
            Vec::<String>::new()
        );
        assert!(lib.unreadable.is_empty());
        assert!(lib.online.is_empty());
        assert_eq!(lib.missing_roots, vec!["/gone".to_string()]);
        assert_eq!(lib.pkg_cache["/g|fpkg/b.pkg"].v, 6);
        // Written back, it holds nothing it did not need and reads the same again.
        save_index_at(&dir, &lib).unwrap();
        let bytes = std::fs::read(dir.join("index.json")).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("\"parts\"")
                && !text.contains("\"unreadable\"")
                && !text.contains("_online")
        );
        assert_eq!(load_index_at(&dir).unwrap(), lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_new_date_is_no_change_worth_saving() {
        let mut a = Library {
            version: 1,
            generated_at: "t1".into(),
            ..Library::default()
        };
        let mut b = a.clone();
        b.generated_at = "t2".into();
        assert!(a.same_index(&b));
        b.unreadable.push("/x".into());
        assert!(!a.same_index(&b));
        b.unreadable.clear();
        a.online.misses.insert("PPSA00001".into(), 5);
        assert!(!a.same_index(&b));
    }

    /// The memory of the title sites is the scanner's, not something consumers read.
    #[test]
    fn the_public_index_leaves_the_scanners_memory_out() {
        let mut lib = Library::default();
        lib.online.misses.insert("PPSA00001".into(), 5);
        let v = public_json(&lib);
        assert!(v.get("_online").is_none());
        assert!(serde_json::to_value(&lib).unwrap().get("_online").is_some());
    }

    #[test]
    fn settings_are_tidied_and_checked() {
        let s = Settings {
            roots: vec![" /a ".into(), "/a".into(), "".into(), "/b".into()],
            refresh_secs: Some(300),
            sweep_sidecars: false,
            allow_permanent_delete: false,
        }
        .validated()
        .unwrap();
        assert_eq!(s.roots, vec!["/a", "/b"]);
        assert!(Settings {
            refresh_secs: Some(7),
            ..Settings::default()
        }
        .validated()
        .is_err());
        assert!(Settings {
            refresh_secs: None,
            ..Settings::default()
        }
        .validated()
        .is_ok());
    }

    #[test]
    fn exports_list_each_game_with_its_copies() {
        let src = br#"{"version":1,"library_root":"/Volumes/S/games","generated_at":"x",
          "summary":{"total_games":0,"total_locations":0,"total_size_bytes":0,"duplicates_count":0,"reclaimable_bytes":0},
          "games":{"CUSA00900":{"game_id":"CUSA00900","title":"Bloodborne","platform":"PS4","locations":[
            {"container":"fpkg","name":"b.pkg","type":"pkg","path":"fpkg/b.pkg","size_bytes":26,"pkg":{"kind":"base","title":"Bloodborne"}},
            {"container":"fpkg","name":"p.pkg","type":"pkg","path":"fpkg/p.pkg","size_bytes":8,"pkg":{"kind":"patch"}}],
            "total_size_bytes":34}}}"#;
        let mut lib: Library = serde_json::from_slice(src).unwrap();
        for g in lib.games.values_mut() {
            super::super::group::finish(g);
        }
        lib.summary = super::super::group::summarize(&lib.games);
        let csv = to_csv(&lib);
        assert!(csv.starts_with('\u{feff}') && csv.contains("\"Bloodborne\""));
        assert!(to_markdown(&lib, "now").contains("| **CUSA00900** | Bloodborne | PS4 | 2 |"));
    }
}
