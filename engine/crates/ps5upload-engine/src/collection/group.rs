//! Turning locations into games and games into a summary. Pure: no disk, no clock.

use std::collections::BTreeMap;

use super::{Game, Location, Summary};

/// Every full copy but the largest. Add-ons never count.
pub fn redundant_bytes(locations: &[Location]) -> u64 {
    let mut sizes: Vec<u64> = locations
        .iter()
        .filter(|l| l.is_copy())
        .map(|l| l.size_bytes)
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.iter().skip(1).sum()
}

/// A platform from a Game ID, when nothing better is known.
pub fn platform_of_id(id: &str) -> &'static str {
    if id.starts_with("PP") {
        "PS5"
    } else if id.starts_with("CU") {
        "PS4"
    } else if is_ps3_id(id) {
        "PS3"
    } else {
        "PlayStation"
    }
}

/// PS3 disc (`BLUS`, `BCES`…) and PSN (`NPUB`, `NPEA`…) title IDs. `NPXS` is a console
/// system application, not PS3.
pub fn is_ps3_id(id: &str) -> bool {
    let b = id.as_bytes();
    if b.len() < 4 {
        return false;
    }
    let region = matches!(b[2], b'U' | b'E' | b'J' | b'H' | b'K' | b'A');
    match &b[..2] {
        b"BL" | b"BC" => region && matches!(b[3], b'S' | b'M' | b'X'),
        b"NP" => region && matches!(b[3], b'A' | b'B' | b'C' | b'D' | b'X' | b'Z'),
        _ => false,
    }
}

/// Adds a location to its game, creating the game the first time.
pub fn place(games: &mut BTreeMap<String, Game>, game_id: &str, loc: Location) {
    let g = games.entry(game_id.to_string()).or_insert_with(|| Game {
        game_id: game_id.to_string(),
        title: game_id.to_string(),
        platform: platform_of_id(game_id).to_string(),
        ..Game::default()
    });
    if let Some(p) = &loc.pkg {
        // A base game (or its patch) names the title; a DLC carries its own name.
        let rank = if p.kind == "dlc" { 1 } else { 2 };
        if !p.title.is_empty() && rank > g.title_rank {
            g.title = p.title.clone();
            g.title_rank = rank;
            g.title_source = if rank == 2 { "package" } else { "dlc" }.to_string();
        }
        if !p.platform.is_empty() && g.platform == "PlayStation" {
            g.platform = p.platform.to_ascii_uppercase();
        }
    }
    let source = loc.kind.split('.').next().unwrap_or("").to_string();
    if !g.sources.contains(&source) {
        g.sources.push(source);
    }
    g.total_size_bytes += loc.size_bytes;
    g.locations.push(loc);
}

/// Sorts a game's locations newest first and fills its totals and dates.
pub fn finish(g: &mut Game) {
    g.locations.sort_by(|a, b| a.path.cmp(&b.path));
    g.locations.sort_by_key(|l| std::cmp::Reverse(l.added_ts));
    let newest = g.locations.iter().max_by_key(|l| l.added_ts);
    let oldest = g
        .locations
        .iter()
        .filter(|l| l.added_at.is_some())
        .min_by_key(|l| l.added_ts);
    g.added_at = newest.and_then(|l| l.added_at.clone());
    g.added_ts = newest.map(|l| l.added_ts).unwrap_or(0);
    g.first_added_at = oldest.and_then(|l| l.added_at.clone());
    g.modified_at = g
        .locations
        .iter()
        .filter_map(|l| l.modified_at.clone())
        .max();
    g.copies = g.locations.iter().filter(|l| l.is_copy()).count();
    g.is_duplicate = g.copies > 1;
    g.total_size_bytes = g.locations.iter().map(|l| l.size_bytes).sum();
}

pub fn summarize(games: &BTreeMap<String, Game>) -> Summary {
    let mut s = Summary {
        total_games: games.len(),
        ..Summary::default()
    };
    for g in games.values() {
        s.total_locations += g.locations.len();
        s.total_size_bytes += g.total_size_bytes;
        if g.is_duplicate {
            s.duplicates_count += 1;
            s.reclaimable_bytes += redundant_bytes(&g.locations);
        }
    }
    s
}

/// The order covers are tried in: a folder is a plain file read, an image a short walk, a
/// package a seek into a file that can exceed 100 GB; a base game carries the game's own art,
/// a patch or DLC may not.
pub fn cover_rank(l: &Location) -> (u8, u8) {
    let source = match l.kind.split('.').next().unwrap_or("") {
        "folder" => 0,
        "mount" => 1,
        "pkg" => 2,
        _ => 9,
    };
    let kind = match l.pkg.as_ref().map(|p| p.kind.as_str()) {
        None | Some("base") | Some("") => 0,
        _ => 1,
    };
    (source, kind)
}

#[cfg(test)]
mod tests {
    use super::super::PkgInfo;
    use super::*;

    fn loc(path: &str, kind: &str, size: u64, pkg_kind: Option<&str>, ts: i64) -> Location {
        Location {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            kind: kind.into(),
            size_bytes: size,
            added_ts: ts,
            added_at: Some(super::super::iso_utc(ts)),
            pkg: pkg_kind.map(|k| PkgInfo {
                kind: k.into(),
                title: format!("T-{k}"),
                ..PkgInfo::default()
            }),
            ..Location::default()
        }
    }

    #[test]
    fn add_ons_are_never_copies_or_duplicates() {
        let mut games = BTreeMap::new();
        place(
            &mut games,
            "CUSA00900",
            loc("a/base.pkg", "pkg", 26, Some("base"), 3),
        );
        place(
            &mut games,
            "CUSA00900",
            loc("a/patch.pkg", "pkg", 8, Some("patch"), 5),
        );
        place(
            &mut games,
            "CUSA00900",
            loc("a/dlc.pkg", "pkg", 1, Some("dlc"), 1),
        );
        let g = games.get_mut("CUSA00900").unwrap();
        finish(g);
        assert_eq!((g.copies, g.is_duplicate), (1, false));
        assert_eq!(g.total_size_bytes, 35);
        assert_eq!(g.locations[0].path, "a/patch.pkg", "newest first");
        assert_eq!(g.added_ts, 5);
        assert_eq!(g.first_added_at, Some(super::super::iso_utc(1)));
        assert_eq!(g.title, "T-base", "the base names the game, not the DLC");
        assert_eq!(summarize(&games).reclaimable_bytes, 0);
    }

    #[test]
    fn full_copies_in_any_form_are_duplicates_and_all_but_the_largest_is_reclaimable() {
        let mut games = BTreeMap::new();
        place(
            &mut games,
            "PPSA01234",
            loc("app/X-app", "folder", 100, None, 1),
        );
        place(
            &mut games,
            "PPSA01234",
            loc("mount/X.exfat", "mount.exfat", 90, None, 2),
        );
        place(
            &mut games,
            "PPSA01234",
            loc("zip/X.zip", "zip", 40, None, 3),
        );
        place(
            &mut games,
            "PPSA01234",
            loc("fpkg/X-patch.pkg", "pkg", 7, Some("patch"), 4),
        );
        finish(games.get_mut("PPSA01234").unwrap());
        let g = &games["PPSA01234"];
        assert_eq!((g.copies, g.is_duplicate), (3, true));
        assert_eq!(g.sources, vec!["folder", "mount", "zip", "pkg"]);
        let s = summarize(&games);
        assert_eq!((s.duplicates_count, s.reclaimable_bytes), (1, 130));
    }

    #[test]
    fn an_unreadable_package_counts_as_a_copy() {
        let l = Location {
            pkg: Some(PkgInfo {
                error: Some("unreadable".into()),
                ..PkgInfo::default()
            }),
            ..Location::default()
        };
        assert!(l.is_copy());
    }

    #[test]
    fn covers_come_from_folders_then_images_then_base_packages() {
        let mut ls = [
            loc("p.pkg", "pkg", 1, Some("base"), 0),
            loc("d.pkg", "pkg", 1, Some("dlc"), 0),
            loc("i.exfat", "mount.exfat", 1, None, 0),
            loc("f-app", "folder", 1, None, 0),
        ];
        ls.sort_by_key(cover_rank);
        let order: Vec<&str> = ls.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(order, ["f-app", "i.exfat", "p.pkg", "d.pkg"]);
    }

    #[test]
    fn platforms_follow_the_id_prefix() {
        assert_eq!(platform_of_id("PPSA01234"), "PS5");
        assert_eq!(platform_of_id("CUSA00900"), "PS4");
        assert_eq!(platform_of_id("NPUB30001"), "PS3");
        assert_eq!(platform_of_id("BLES01234"), "PS3");
        assert_eq!(
            platform_of_id("NPXS39041"),
            "PlayStation",
            "a system app, not PS3"
        );
        assert_eq!(platform_of_id("WEBB00002"), "PlayStation");
    }
}
