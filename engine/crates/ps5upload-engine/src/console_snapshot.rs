//! What the engine last read about each console, title by title, and when.
//!
//! The game page shows every saved console, not only the connected one, so what a read found is
//! kept: the Collection's per-console check records each title in full, the installed-apps list
//! records what is installed (and so what no longer is). Kept by the console's IP, like the queue,
//! in `state/console-snapshots.json`, and it outlives an engine restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::collection::console::ConsoleTitle;

/// One title on one console, as last read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TitleFacts {
    /// The name the console reported ("" when unknown).
    #[serde(default)]
    pub title: String,
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
    /// None: never read (the installed-apps list does not look).
    #[serde(default)]
    pub patch_installed: Option<bool>,
    #[serde(default)]
    pub dlc_labels: Option<Vec<String>>,
    #[serde(default)]
    pub registered_from: Option<String>,
    /// Unix seconds of the read that last touched this title.
    pub read_at: u64,
}

impl TitleFacts {
    /// The facts as `collection::console::state_for` takes them.
    pub fn console_title(&self) -> ConsoleTitle {
        ConsoleTitle {
            installed: self.installed,
            version: self.version.clone(),
            patch_installed: self.patch_installed.unwrap_or(false),
            dlc_labels: self.dlc_labels.clone().unwrap_or_default(),
            registered_from: self.registered_from.clone(),
        }
    }
}

/// Host (bare IP) → title ID (upper case) → facts.
pub type Snapshots = BTreeMap<String, BTreeMap<String, TitleFacts>>;

/// The console's IP without the port: `1.2.3.4:9114` → `1.2.3.4`, `[::1]:9114` → `::1`.
pub fn host_key(addr: &str) -> String {
    let a = addr.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    match a.rsplit_once(':') {
        // One colon: host:port. More: a bare IPv6 address, kept whole.
        Some((h, _)) if !h.contains(':') => h.to_string(),
        _ => a.to_string(),
    }
}

/// Records a full read of one title (installed, version, update, DLC).
pub fn merge_detailed(
    s: &mut Snapshots,
    host: &str,
    title_id: &str,
    t: &ConsoleTitle,
    title: &str,
    now: u64,
) {
    let facts = s
        .entry(host_key(host))
        .or_default()
        .entry(title_id.to_ascii_uppercase())
        .or_default();
    if !title.is_empty() {
        facts.title = title.to_string();
    }
    facts.installed = t.installed;
    facts.version = t.version.clone();
    facts.patch_installed = Some(t.patch_installed);
    facts.dlc_labels = Some(t.dlc_labels.clone());
    facts.registered_from = t.registered_from.clone();
    facts.read_at = now;
}

/// Records a console's installed list: each listed title is installed (details it does not
/// read are kept). When the list is `complete`, every other recorded title of that console is
/// no longer installed; a list read with a part missing (app.db unreadable) removes nothing.
pub fn merge_full_list(
    s: &mut Snapshots,
    host: &str,
    installed: &[(String, String, Option<String>)],
    complete: bool,
    now: u64,
) {
    let titles = s.entry(host_key(host)).or_default();
    let listed: std::collections::HashSet<String> = installed
        .iter()
        .map(|(id, _, _)| id.to_ascii_uppercase())
        .collect();
    for (id, facts) in titles.iter_mut() {
        if complete && !listed.contains(id) && facts.installed {
            *facts = TitleFacts {
                title: std::mem::take(&mut facts.title),
                installed: false,
                read_at: now,
                ..TitleFacts::default()
            };
        }
    }
    for (id, name, from) in installed {
        let facts = titles.entry(id.to_ascii_uppercase()).or_default();
        if !facts.installed {
            // Newly seen installed: details read while it was not are stale.
            facts.version = None;
            facts.patch_installed = None;
            facts.dlc_labels = None;
        }
        facts.installed = true;
        if !name.is_empty() && !name.eq_ignore_ascii_case(id) {
            facts.title = name.clone();
        }
        facts.registered_from = from.clone();
        facts.read_at = now;
    }
}

/// Whether the console's app folders were read whole: the internal drive's listing answered
/// (`internal`: its entry count) with less than a full page, and some folder was found. Titles
/// only app.db knows count as installed through these folders, so a listing that failed or
/// was cut short leaves the installed list incomplete.
pub fn app_folders_whole(internal: Option<usize>, folders_found: usize) -> bool {
    matches!(internal, Some(n) if n < 512) && folders_found > 0
}

/// Forgets every console not in `hosts` (removed from the app, or at a new IP).
pub fn keep_hosts(s: &mut Snapshots, hosts: &[String]) {
    let keep: std::collections::HashSet<String> = hosts.iter().map(|h| host_key(h)).collect();
    s.retain(|h, _| keep.contains(h));
}

pub fn load_from(path: &Path) -> Snapshots {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            crate::log_warn!(
                "console snapshots at {} unreadable ({e}); starting empty",
                path.display()
            );
            Snapshots::new()
        }),
        Err(_) => Snapshots::new(),
    }
}

pub fn save_to(path: &Path, s: &Snapshots) {
    let Ok(bytes) = serde_json::to_vec(s) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// `PS5UPLOAD_STATE_DIR`, else `<data dir>/state`. Tests persist only with an explicit directory.
fn path() -> Option<PathBuf> {
    let dir = match std::env::var("PS5UPLOAD_STATE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ if cfg!(test) => return None,
        _ => crate::remote::store::data_dir()?.join("state"),
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("console-snapshots.json"))
}

fn store() -> &'static Mutex<Option<Snapshots>> {
    static STORE: OnceLock<Mutex<Option<Snapshots>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

/// Runs `f` on the saved snapshots (loaded on first use) and saves them after.
pub fn with<R>(f: impl FnOnce(&mut Snapshots) -> R) -> R {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    let file = path();
    let s = guard.get_or_insert_with(|| file.as_deref().map(load_from).unwrap_or_default());
    let out = f(s);
    if let Some(file) = file {
        save_to(&file, s);
    }
    out
}

/// A copy of the saved snapshots.
pub fn snapshot() -> Snapshots {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(|| path().as_deref().map(load_from).unwrap_or_default())
        .clone()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detailed(installed: bool, version: &str, patch: bool, dlc: &[&str]) -> ConsoleTitle {
        ConsoleTitle {
            installed,
            version: (!version.is_empty()).then(|| version.to_string()),
            patch_installed: patch,
            dlc_labels: dlc.iter().map(|d| d.to_string()).collect(),
            registered_from: None,
        }
    }

    #[test]
    fn a_detailed_read_records_every_field() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.2.3.4:9114",
            "ppsa01234",
            &detailed(true, "01.004", true, &["DLC1"]),
            "Astro",
            100,
        );
        let f = &s["1.2.3.4"]["PPSA01234"];
        assert_eq!(f.title, "Astro");
        assert!(f.installed);
        assert_eq!(f.version.as_deref(), Some("01.004"));
        assert_eq!(f.patch_installed, Some(true));
        assert_eq!(f.dlc_labels.as_deref(), Some(&["DLC1".to_string()][..]));
        assert_eq!(f.read_at, 100);
    }

    #[test]
    fn a_full_list_marks_titles_that_left_as_not_installed() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.2.3.4",
            "PPSA00001",
            &detailed(true, "1.0", false, &[]),
            "A",
            1,
        );
        merge_detailed(
            &mut s,
            "1.2.3.4",
            "PPSA00002",
            &detailed(true, "2.0", true, &["X"]),
            "B",
            1,
        );
        merge_full_list(
            &mut s,
            "1.2.3.4",
            &[("PPSA00001".into(), "A".into(), None)],
            true,
            5,
        );
        let b = &s["1.2.3.4"]["PPSA00002"];
        assert!(!b.installed);
        assert_eq!(b.version, None);
        assert_eq!(b.patch_installed, None);
        assert_eq!(b.dlc_labels, None);
        assert_eq!(b.title, "B");
        assert_eq!(b.read_at, 5);
        assert!(s["1.2.3.4"]["PPSA00001"].installed);
    }

    #[test]
    fn a_list_with_a_part_missing_removes_nothing() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "h",
            "PPSA00002",
            &detailed(true, "2.0", true, &[]),
            "B",
            1,
        );
        merge_full_list(
            &mut s,
            "h",
            &[("PPSA00001".into(), "A".into(), None)],
            false,
            5,
        );
        assert!(s["h"]["PPSA00002"].installed);
        assert!(s["h"]["PPSA00001"].installed);
    }

    #[test]
    fn a_full_list_keeps_details_it_did_not_read() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.2.3.4",
            "PPSA00001",
            &detailed(true, "01.004", true, &[]),
            "A",
            1,
        );
        merge_full_list(
            &mut s,
            "1.2.3.4",
            &[("PPSA00001".into(), "PPSA00001".into(), None)],
            true,
            9,
        );
        let a = &s["1.2.3.4"]["PPSA00001"];
        assert_eq!(a.version.as_deref(), Some("01.004"));
        assert_eq!(a.patch_installed, Some(true));
        assert_eq!(a.title, "A", "an id-as-name does not replace a real name");
        assert_eq!(a.read_at, 9);
    }

    #[test]
    fn a_newly_installed_title_has_no_stale_details() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "h",
            "PPSA00001",
            &detailed(false, "", false, &[]),
            "A",
            1,
        );
        s.get_mut("h")
            .unwrap()
            .get_mut("PPSA00001")
            .unwrap()
            .patch_installed = Some(true);
        merge_full_list(
            &mut s,
            "h",
            &[("PPSA00001".into(), "A".into(), Some("/data/a".into()))],
            true,
            2,
        );
        let a = &s["h"]["PPSA00001"];
        assert!(a.installed);
        assert_eq!(a.patch_installed, None);
        assert_eq!(a.registered_from.as_deref(), Some("/data/a"));
    }

    #[test]
    fn app_folders_read_whole_only_when_the_internal_listing_answered_in_full() {
        assert!(app_folders_whole(Some(40), 40));
        assert!(!app_folders_whole(None, 3), "internal listing failed");
        assert!(
            !app_folders_whole(Some(512), 512),
            "a full page may have more"
        );
        assert!(!app_folders_whole(Some(0), 0), "nothing found at all");
    }

    #[test]
    fn hosts_are_kept_by_ip_without_port() {
        assert_eq!(host_key("1.2.3.4:9114"), "1.2.3.4");
        assert_eq!(host_key("1.2.3.4"), "1.2.3.4");
        assert_eq!(host_key("[::1]:9114"), "::1");
        assert_eq!(host_key("fe80::1"), "fe80::1");
    }

    #[test]
    fn keep_drops_other_hosts() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.1.1.1",
            "PPSA00001",
            &detailed(true, "", false, &[]),
            "",
            1,
        );
        merge_detailed(
            &mut s,
            "2.2.2.2",
            "PPSA00001",
            &detailed(true, "", false, &[]),
            "",
            1,
        );
        keep_hosts(&mut s, &["1.1.1.1:9114".to_string()]);
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["1.1.1.1"]);
    }

    #[test]
    fn round_trip_and_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("ps5u-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("console-snapshots.json");
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.2.3.4",
            "PPSA00001",
            &detailed(true, "1.0", true, &["D"]),
            "A",
            7,
        );
        save_to(&file, &s);
        assert_eq!(load_from(&file), s);
        std::fs::write(&file, b"{not json").unwrap();
        assert!(load_from(&file).is_empty());
        assert!(load_from(&dir.join("missing.json")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
