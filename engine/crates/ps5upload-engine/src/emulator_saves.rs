//! The saves of ProsperoEden, the Switch emulator for the PS5 (PPSA99008), for the automatic save
//! backups. The emulator keeps them as plain files, not as PS5 save images, one folder per profile
//! and game: `/data/prosperoeden/user/nand/user/save/0000000000000000/<profile>/<game>/`, the
//! profile as 32 hex digits (all zeros: a game's save for the whole console) and the game as its
//! 16-hex-digit title ID. That place is fixed in the emulator: the game files folder chosen in its
//! settings moves the games, keys and firmware, never the saves.
//!
//! Each such folder that holds a file is listed among a console's saves (`kind` "switch",
//! `title_id` the game's), so it is backed up, kept and restored as a PS5 save is, with two
//! differences: its files go as they are (`backup_finalize` and `restore_prepare` shape PS5 save
//! images), and the app that writes it is the emulator, not the game.
//!
//! The PlayStation 2 emulators' memory cards and saved states (`kind` "ps2") are emulators' saves
//! too: see `ps2_saves`. What tells them all apart from a PS5 save is here.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::fs_ops::{list_dir, DirEntry, DirListing, ListDirOptions};
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The emulator's title.
pub const APP: &str = "PPSA99008";
/// `SaveEntry::kind` of its saves.
pub const KIND: &str = "switch";
/// Where it keeps every profile's saves.
pub const ROOT: &str = "/data/prosperoeden/user/nand/user/save/0000000000000000";
/// How many folders down a game's save is looked into for its newest file.
pub const DEPTH: u32 = 4;

fn hex_digits(name: &str, len: usize) -> bool {
    name.len() == len && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A game's folder: its 16-hex-digit title ID.
pub fn is_game(name: &str) -> bool {
    hex_digits(name, 16)
}

/// A profile's folder: 32 hex digits.
fn is_profile(name: &str) -> bool {
    hex_digits(name, 32)
}

/// The save is an emulator's: a Switch game's in ProsperoEden, or a PlayStation 2 emulator's cards
/// or saved states.
pub fn is_emulator_save(e: &SaveEntry) -> bool {
    e.kind == KIND || e.kind == crate::ps2_saves::KIND
}

/// `path` is a save folder of the emulator, `ROOT/<profile>/<game>`, and nothing past it.
pub fn is_save_path(path: &str) -> bool {
    path.strip_prefix(ROOT)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(profile, game)| is_profile(profile) && is_game(game))
}

/// The kind of emulator's save `path` is the folder of; None for a PS5 save's.
pub fn kind_of_path(path: &str) -> Option<&'static str> {
    if is_save_path(path) {
        Some(KIND)
    } else if crate::ps2_saves::place_of(path).is_some() {
        Some(crate::ps2_saves::KIND)
    } else {
        None
    }
}

/// The app that writes the save: ProsperoEden for its saves, the game itself for a PS5 one. (A
/// PlayStation 2 emulator's saves are under its own title already.)
pub fn app_of(e: &SaveEntry) -> &str {
    if e.kind == KIND {
        APP
    } else {
        &e.title_id
    }
}

/// A save the automatic backups take: a PS5 game's (a PlayStation 2 emulator's saves carry its
/// title), or a game's in ProsperoEden.
pub fn backs_up(e: &SaveEntry) -> bool {
    crate::looks_like_title_id(&e.title_id) || (e.kind == KIND && is_game(&e.title_id))
}

/// The profile as the user number in a backup's name: its first 7 hex digits (0 for the
/// console's own), enough to keep two profiles' backups of one game apart. Seven, not eight,
/// so the number stays positive where it is kept as an `i32`.
fn user_of(profile: &str) -> i32 {
    i32::from_str_radix(&profile[..7], 16).unwrap_or(0)
}

/// The emulator's saves on `host`: none when it is not there or has no save yet.
pub fn list(host: &str) -> Vec<SaveEntry> {
    saves_in(&|path: &str| ls(host, path))
}

/// Every entry of the folder `path` on `host`.
pub(crate) fn ls(host: &str, path: &str) -> Result<Vec<DirEntry>, String> {
    all_pages(&|offset| {
        list_dir(
            host,
            path,
            ListDirOptions {
                offset,
                ..Default::default()
            },
        )
        .map_err(|e| format!("{e:#}"))
    })
}

/// Every entry of a folder, page by page (`page(offset)`): a full page, or one cut short
/// because its buffer filled, means there may be more.
fn all_pages(page: &dyn Fn(u64) -> Result<DirListing, String>) -> Result<Vec<DirEntry>, String> {
    let limit = ListDirOptions::default().limit;
    let mut out = Vec::new();
    loop {
        let l = page(out.len() as u64)?;
        let n = l.entries.len() as u64;
        out.extend(l.entries);
        if n == 0 || (!l.truncated && n < limit) {
            return Ok(out);
        }
    }
}

/// [`list`], with `ls` giving the entries of a folder on the console.
pub fn saves_in(ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>) -> Vec<SaveEntry> {
    let mut out = Vec::new();
    let Ok(profiles) = ls(ROOT) else {
        return out;
    };
    for p in profiles
        .iter()
        .filter(|p| p.kind == "dir" && is_profile(&p.name))
    {
        let Ok(games) = ls(&format!("{ROOT}/{}", p.name)) else {
            continue;
        };
        for g in games.iter().filter(|g| g.kind == "dir" && is_game(&g.name)) {
            let path = format!("{ROOT}/{}/{}", p.name, g.name);
            // A folder not read whole waits for the next look: its newest time could miss the
            // file the game wrote.
            let Ok((size, mtime, files)) = measure(ls, &path, g.mtime, DEPTH) else {
                continue;
            };
            // The emulator makes some games' folders before anything is saved in them: no file,
            // no save.
            if files == 0 {
                continue;
            }
            out.push(SaveEntry {
                title_id: g.name.clone(),
                user_id: user_of(&p.name),
                path,
                size: size as i64,
                mtime,
                kind: KIND.into(),
            });
        }
    }
    out
}

/// The size and number of the files under `path`, and the newest time of the folder (`mtime`) and
/// of everything in it, `depth` folders down.
pub(crate) fn measure(
    ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>,
    path: &str,
    mtime: i64,
    depth: u32,
) -> Result<(u64, i64, u32), String> {
    let (mut size, mut newest, mut files) = (0u64, mtime, 0u32);
    for e in ls(path)? {
        if e.name == "." || e.name == ".." {
            continue;
        }
        newest = newest.max(e.mtime);
        match e.kind.as_str() {
            "file" => {
                size += e.size;
                files += 1;
            }
            "dir" if depth > 0 => {
                let (s, m, f) = measure(ls, &format!("{path}/{}", e.name), e.mtime, depth - 1)?;
                size += s;
                newest = newest.max(m);
                files += f;
            }
            _ => {}
        }
    }
    Ok((size, newest, files))
}

// ── HTTP ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SwitchQuery {
    /// The console (bare IP).
    pub addr: String,
}

/// One of the emulator's saves, as the app lists it.
#[derive(Serialize)]
pub struct SwitchSave {
    #[serde(flatten)]
    pub save: SaveEntry,
    /// The emulator runs now (its saves cannot be restored meanwhile); None when the console did
    /// not say.
    pub running: Option<bool>,
}

/// `GET /api/save-backup/switch?addr=` — the emulator's saves on that console, each with whether
/// the emulator runs now: `{saves, running}`. None (and `running` null) when it is not installed
/// or has saved nothing.
pub async fn get_switch(Query(q): Query<SwitchQuery>) -> Response {
    let host = q.addr.trim().to_string();
    if host.is_empty() {
        return crate::save_backup::json_error(StatusCode::BAD_REQUEST, "addr is required");
    }
    let r = tokio::task::spawn_blocking(move || {
        let saves = list(&host);
        // Only asked when there is something to restore.
        let running = if saves.is_empty() {
            None
        } else {
            crate::save_backup::title_running(&host, APP).ok()
        };
        (saves, running)
    })
    .await;
    match r {
        Ok((saves, running)) => Json(json!({
            "running": running,
            "saves": saves
                .into_iter()
                .map(|save| SwitchSave { save, running })
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => crate::save_backup::json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const P1: &str = "1F1E1D1C1B1A19181716151413121110";
    const DEVICE: &str = "00000000000000000000000000000000";
    const GAME: &str = "0100F2C0115B6000";

    fn entry(name: &str, kind: &str, size: u64, mtime: i64) -> DirEntry {
        DirEntry {
            name: name.into(),
            kind: kind.into(),
            size,
            mtime,
        }
    }

    /// A console's folders, as the payload lists them.
    fn console() -> BTreeMap<String, Vec<DirEntry>> {
        let mut fs = BTreeMap::new();
        fs.insert(
            ROOT.to_string(),
            vec![
                entry(".", "dir", 0, 0),
                entry(P1, "dir", 0, 100),
                entry(DEVICE, "dir", 0, 100),
                entry("cache", "dir", 0, 100),
                entry("notes.txt", "file", 5, 100),
            ],
        );
        fs.insert(
            format!("{ROOT}/{P1}"),
            vec![
                entry(GAME, "dir", 0, 200),
                entry("not-a-game", "dir", 0, 900),
            ],
        );
        fs.insert(
            format!("{ROOT}/{P1}/{GAME}"),
            vec![
                entry("save.dat", "file", 10, 300),
                entry("slots", "dir", 0, 250),
            ],
        );
        fs.insert(
            format!("{ROOT}/{P1}/{GAME}/slots"),
            vec![entry("1.bin", "file", 7, 500)],
        );
        fs.insert(format!("{ROOT}/{DEVICE}"), vec![entry(GAME, "dir", 0, 400)]);
        fs.insert(
            format!("{ROOT}/{DEVICE}/{GAME}"),
            vec![entry("system.dat", "file", 3, 350)],
        );
        fs
    }

    fn ls_of(
        fs: &BTreeMap<String, Vec<DirEntry>>,
    ) -> impl Fn(&str) -> Result<Vec<DirEntry>, String> + '_ {
        move |p: &str| fs.get(p).cloned().ok_or_else(|| format!("{p}: _errno_2"))
    }

    #[test]
    fn each_profiles_game_is_a_save_with_its_newest_time() {
        let fs = console();
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves.len(), 2);
        let mine = &saves[0];
        assert_eq!(mine.title_id, GAME);
        assert_eq!(mine.path, format!("{ROOT}/{P1}/{GAME}"));
        assert_eq!(mine.user_id, 0x1F1_E1D1);
        assert_eq!(mine.kind, KIND);
        // The file in the subfolder is the newest, and both files count.
        assert_eq!(mine.mtime, 500);
        assert_eq!(mine.size, 17);
        // The console's own save of the game: user 0, its folder's time.
        assert_eq!(saves[1].user_id, 0);
        assert_eq!(saves[1].mtime, 400);
        assert!(saves.iter().all(|s| backs_up(s) && is_save_path(&s.path)));
    }

    #[test]
    fn no_emulator_or_a_folder_that_fails_lists_nothing_of_it() {
        let nothing = |p: &str| -> Result<Vec<DirEntry>, String> { Err(format!("{p}: _errno_2")) };
        assert!(saves_in(&nothing).is_empty());
        let mut fs = console();
        fs.remove(&format!("{ROOT}/{P1}/{GAME}/slots"));
        let saves = saves_in(&ls_of(&fs));
        // The profile's game could not be read whole: only the console's own save is listed.
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].user_id, 0);
    }

    #[test]
    fn an_empty_save_folder_is_no_save() {
        let mut fs = console();
        // The emulator made the game's folder, with one of its own inside, and saved nothing.
        fs.insert(
            format!("{ROOT}/{DEVICE}/{GAME}"),
            vec![entry("slots", "dir", 0, 400)],
        );
        fs.insert(format!("{ROOT}/{DEVICE}/{GAME}/slots"), vec![]);
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].path, format!("{ROOT}/{P1}/{GAME}"));
    }

    /// The app reads a listed save as the console's saves list gives one, plus `running`.
    #[test]
    fn a_listed_save_carries_whether_the_emulator_runs() {
        let fs = console();
        let save = saves_in(&ls_of(&fs)).remove(0);
        let v = serde_json::to_value(SwitchSave {
            save,
            running: Some(true),
        })
        .unwrap();
        assert_eq!(v["title_id"], GAME);
        assert_eq!(v["kind"], KIND);
        assert_eq!(v["user_id"], 0x1F1_E1D1);
        assert_eq!(v["running"], true);
    }

    #[test]
    fn a_big_folder_is_read_page_by_page() {
        let all: Vec<DirEntry> = (0..300)
            .map(|i| entry(&format!("f{i}"), "file", 1, i))
            .collect();
        let got = all_pages(&|offset| {
            let entries: Vec<DirEntry> = all
                .iter()
                .skip(offset as usize)
                .take(256)
                .cloned()
                .collect();
            Ok(DirListing {
                path: ROOT.into(),
                returned: entries.len() as u64,
                entries,
                truncated: false,
                total_scanned: 300,
            })
        })
        .unwrap();
        assert_eq!(got.len(), 300);
        assert_eq!(got[299].name, "f299");
    }

    #[test]
    fn save_paths_and_the_app_that_writes_them() {
        assert!(is_save_path(&format!("{ROOT}/{P1}/{GAME}")));
        assert!(!is_save_path(&format!("{ROOT}/{P1}/{GAME}/slots")));
        assert!(!is_save_path(&format!("{ROOT}/{P1}/../{GAME}")));
        assert!(!is_save_path(&format!("{ROOT}/{P1}")));
        assert!(!is_save_path(
            "/user/home/1a2b3c4d/savedata_prospero/PPSA01234"
        ));
        let ps5 = SaveEntry {
            title_id: "PPSA01234".into(),
            user_id: 1,
            path: "/user/home/1/savedata_prospero/PPSA01234".into(),
            size: 1,
            mtime: 1,
            kind: "ps5".into(),
        };
        assert_eq!(app_of(&ps5), "PPSA01234");
        let switch = SaveEntry {
            title_id: GAME.into(),
            path: format!("{ROOT}/{P1}/{GAME}"),
            kind: KIND.into(),
            ..ps5.clone()
        };
        assert_eq!(app_of(&switch), APP);
        assert!(backs_up(&ps5) && backs_up(&switch));
        // A Switch title ID is not a PS5 one: only the emulator's saves carry it.
        assert!(!backs_up(&SaveEntry {
            kind: "ps5".into(),
            ..switch.clone()
        }));
    }
}
