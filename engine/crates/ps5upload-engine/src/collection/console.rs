//! A game in the collection against what one console has: installed or not, at which version,
//! and what the collection could bring it (the game, a newer update, missing DLC).
//!
//! `state_for` is pure: the console facts come in as a [`ConsoleTitle`], read once per title.

use ps5upload_pkg::kind::version_cmp;
use serde::Serialize;

use super::{Game, Location};

/// What the console has for one Game ID.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConsoleTitle {
    /// The console knows the title (it has `/user/app/<id>`).
    pub installed: bool,
    /// Its installed version (`APP_VER`), when it could be read.
    pub version: Option<String>,
    /// An update is installed (`/user/patch/<id>`).
    pub patch_installed: bool,
    /// Folder names under `/user/addcont/<id>`: each DLC's label (the end of its content ID).
    pub dlc_labels: Vec<String>,
    /// Registered from a folder or an image (not a package install).
    pub registered_from: Option<String>,
    /// The version could not be read (the console did not answer): `version` means nothing.
    pub version_unread: bool,
    /// The update and DLC folders could not be read: `patch_installed` and `dlc_labels` mean
    /// nothing.
    pub extras_unread: bool,
}

/// A package in the collection the console could take.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Offer {
    pub path: String,
    pub name: String,
    pub version: String,
    pub content_id: String,
    pub title: String,
    pub size_bytes: u64,
    /// `gd`, `gp` or `ac`, which orders installs base → update → DLC.
    pub category: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GameConsoleState {
    pub game_id: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_from: Option<String>,
    /// The base package to install, when the game is not on the console.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Offer>,
    /// A newer update than the console has (or an update when the console has none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<Offer>,
    pub dlc_missing: Vec<Offer>,
    /// The collection holds the game only as a folder, image or archive (no base package):
    /// it reaches the console by upload, not by package install.
    pub non_package_copy: bool,
}

fn offer(l: &Location, category: &str) -> Offer {
    let p = l.pkg.clone().unwrap_or_default();
    Offer {
        path: l.absolute_path.clone(),
        name: l.name.clone(),
        version: p.version,
        content_id: p.content_id,
        title: p.title,
        size_bytes: l.size_bytes,
        category: category.to_string(),
    }
}

fn is_pkg_of(l: &Location, kind: &str) -> bool {
    l.kind == "pkg"
        && l.pkg
            .as_ref()
            .is_some_and(|p| p.kind == kind && p.error.is_none() && p.complete)
}

/// The DLC's label: the part of its content ID after the last `-`.
pub fn dlc_label(content_id: &str) -> &str {
    content_id.rsplit('-').next().unwrap_or("")
}

pub fn state_for(game: &Game, t: &ConsoleTitle) -> GameConsoleState {
    let newest = |kind: &str| {
        game.locations
            .iter()
            .filter(|l| is_pkg_of(l, kind))
            .max_by(|a, b| {
                let va = a.pkg.as_ref().map(|p| p.version.as_str()).unwrap_or("");
                let vb = b.pkg.as_ref().map(|p| p.version.as_str()).unwrap_or("");
                version_cmp(va, vb).then(a.added_ts.cmp(&b.added_ts))
            })
    };
    let base = (!t.installed)
        .then(|| newest("base").map(|l| offer(l, "gd")))
        .flatten();
    let update = newest("patch").and_then(|l| {
        if !t.installed {
            // With the game: it goes in after the base, in the same run.
            return base.as_ref().map(|_| offer(l, "gp"));
        }
        let v = l.pkg.as_ref().map(|p| p.version.as_str()).unwrap_or("");
        match t.version.as_deref() {
            Some(have) if !v.is_empty() => {
                (version_cmp(v, have) == std::cmp::Ordering::Greater).then(|| offer(l, "gp"))
            }
            // Without the console's version, offer it only when no update is installed.
            _ => (!t.patch_installed).then(|| offer(l, "gp")),
        }
    });
    let dlc_missing = game
        .locations
        .iter()
        .filter(|l| is_pkg_of(l, "dlc"))
        .filter(|l| {
            let label = l
                .pkg
                .as_ref()
                .map(|p| dlc_label(&p.content_id))
                .unwrap_or("");
            !label.is_empty() && !t.dlc_labels.iter().any(|d| d.eq_ignore_ascii_case(label))
        })
        .map(|l| offer(l, "ac"))
        .collect();
    let has_base_pkg = game.locations.iter().any(|l| is_pkg_of(l, "base"));
    GameConsoleState {
        game_id: game.game_id.clone(),
        installed: t.installed,
        installed_version: t.version.clone(),
        registered_from: t.registered_from.clone(),
        base,
        update,
        dlc_missing,
        non_package_copy: !has_base_pkg
            && game
                .locations
                .iter()
                .any(|l| l.is_copy() && l.kind != "pkg"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::PkgInfo;
    use super::*;

    fn pkg(kind: &str, version: &str, cid: &str) -> Location {
        Location {
            kind: "pkg".into(),
            name: format!("{kind}-{version}.pkg"),
            absolute_path: format!("/lib/{kind}-{version}-{cid}.pkg"),
            size_bytes: 10,
            pkg: Some(PkgInfo {
                kind: kind.into(),
                version: version.into(),
                content_id: cid.into(),
                complete: true,
                ..PkgInfo::default()
            }),
            ..Location::default()
        }
    }

    fn bloodborne() -> Game {
        Game {
            game_id: "CUSA00900".into(),
            locations: vec![
                pkg("base", "1.00", "UP9000-CUSA00900_00-BLOODBORNE000000"),
                pkg("patch", "1.03", "UP9000-CUSA00900_00-BLOODBORNE000000"),
                pkg("patch", "1.09", "UP9000-CUSA00900_00-BLOODBORNE000000"),
                pkg("dlc", "1.00", "UP9000-CUSA00900_00-SPEXPANSIONDLC03"),
                pkg("dlc", "1.00", "UP9000-CUSA00900_00-SPDLCMESSENGER00"),
            ],
            ..Game::default()
        }
    }

    #[test]
    fn a_game_not_on_the_console_offers_its_base_then_its_newest_update_and_dlc() {
        let s = state_for(&bloodborne(), &ConsoleTitle::default());
        assert!(!s.installed);
        assert_eq!(s.base.as_ref().unwrap().category, "gd");
        assert_eq!(s.update.as_ref().unwrap().version, "1.09");
        assert_eq!(s.dlc_missing.len(), 2);
    }

    #[test]
    fn an_older_console_version_is_offered_the_newer_update_and_only_missing_dlc() {
        let t = ConsoleTitle {
            installed: true,
            version: Some("1.03".into()),
            patch_installed: true,
            dlc_labels: vec!["spexpansiondlc03".into()],
            registered_from: None,
            ..ConsoleTitle::default()
        };
        let s = state_for(&bloodborne(), &t);
        assert!(s.base.is_none());
        assert_eq!(s.update.as_ref().unwrap().version, "1.09");
        let missing: Vec<&str> = s
            .dlc_missing
            .iter()
            .map(|o| dlc_label(&o.content_id))
            .collect();
        assert_eq!(missing, ["SPDLCMESSENGER00"]);
    }

    #[test]
    fn an_up_to_date_console_is_offered_nothing() {
        let t = ConsoleTitle {
            installed: true,
            version: Some("1.09".into()),
            patch_installed: true,
            dlc_labels: vec!["SPEXPANSIONDLC03".into(), "SPDLCMESSENGER00".into()],
            registered_from: None,
            ..ConsoleTitle::default()
        };
        let s = state_for(&bloodborne(), &t);
        assert!(s.update.is_none() && s.dlc_missing.is_empty() && s.base.is_none());
    }

    #[test]
    fn an_unknown_console_version_never_guesses_past_an_installed_update() {
        let mut t = ConsoleTitle {
            installed: true,
            version: None,
            patch_installed: true,
            ..ConsoleTitle::default()
        };
        assert!(state_for(&bloodborne(), &t).update.is_none());
        t.patch_installed = false;
        assert!(state_for(&bloodborne(), &t).update.is_some());
    }

    #[test]
    fn a_game_kept_only_as_a_folder_or_image_reaches_the_console_by_upload() {
        let g = Game {
            game_id: "PPSA01234".into(),
            locations: vec![Location {
                kind: "mount.exfat".into(),
                ..Location::default()
            }],
            ..Game::default()
        };
        let s = state_for(&g, &ConsoleTitle::default());
        assert!(s.non_package_copy && s.base.is_none());
    }

    #[test]
    fn incomplete_or_unreadable_packages_are_never_offered() {
        let mut g = bloodborne();
        for l in &mut g.locations {
            if let Some(p) = l.pkg.as_mut() {
                p.complete = false;
            }
        }
        let s = state_for(&g, &ConsoleTitle::default());
        assert!(s.base.is_none() && s.update.is_none() && s.dlc_missing.is_empty());
    }
}
