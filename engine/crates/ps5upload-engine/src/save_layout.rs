//! Where save backups go inside a place that holds them (the backup folder on this computer, a
//! folder on a network share, `ps5upload/save-backups` on a PS5 drive): by game, as always, or by
//! user (#886).
//!
//! ```text
//! By game:  <TITLE>/<TITLE>_user<8 hex>_<stamp>.zip
//! By user:  <name>_<8 hex>/Encrypted/PS5/<TITLE>[ - <game>]/<TITLE>_user<8 hex>_<stamp>.zip
//!           <name>_<8 hex>/Encrypted/PS4/<TITLE>[ - <game>]/…     (PS4 saves on the PS5)
//!           <name>_<8 hex>/Decrypted/PS5/<TITLE>[ - <game>]/…     (decrypted exports)
//! ```
//!
//! The file names are the same in both layouts, so a backup says whose and which game it is
//! wherever it sits. The user's folder ends in the account ID, so it is unique and is found again
//! by that ID after the user is renamed (a folder already there is used, whatever name it shows).
//! A game's folder already there is used too, with or without the game's name.
//!
//! Nothing is ever moved: backups made before stay where they are, and listing, restoring and
//! keeping "Versions to keep" look in both layouts of a place. Only files named the way this app
//! names its backups (`ours`) are ever deleted. An emulator's save (the Switch, PlayStation and
//! PlayStation 2 emulators') belongs to none of the console's users and stays by game.
//!
//! The functions here see a place only through `list` (a folder's entries, by path relative to
//! the place: "" is the place itself), so the same rules hold on a disk here, a server and a PS5
//! drive.

use ps5upload_core::save_archive::{auto_backup_stem, backup_tag};
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};

use crate::save_backup::parse_backup_name;

/// How backups are arranged in a place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// `<TITLE>/<file>` (what every version before 12.6.16 wrote).
    #[default]
    ByGame,
    /// `<name>_<8 hex>/Encrypted/<PS4|PS5>/<TITLE>[ - <game>]/<file>`.
    ByUser,
}

/// A place's layout and whether a new game folder carries the game's name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arrange {
    pub layout: Layout,
    pub game_names: bool,
}

/// The two kinds of files in a by-user folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Backups as the console keeps them: sealed, they restore on this PS5.
    Encrypted,
    /// Decrypted exports ("Save as a file for a friend or another PS5"): they open on any PS5.
    Decrypted,
}

impl Kind {
    pub fn dir(self) -> &'static str {
        match self {
            Kind::Encrypted => "Encrypted",
            Kind::Decrypted => "Decrypted",
        }
    }
}

/// Which console's format a save is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// A PS4 game's save on the PS5 (`/user/home/<id>/savedata/<CUSA…>`).
    Ps4,
    /// A PS5 game's save (`/user/home/<id>/savedata_prospero/<PPSA…>`).
    Ps5,
}

impl Platform {
    pub fn dir(self) -> &'static str {
        match self {
            Platform::Ps4 => "PS4",
            Platform::Ps5 => "PS5",
        }
    }
}

/// The format of a game's save, from what the console's save list says (`kind` "ps4" or "ps5"),
/// else from its folder; None for an emulator's save, which stays by game.
pub fn platform_of(e: &SaveEntry) -> Option<Platform> {
    if crate::emulator_saves::is_emulator_save(e)
        || crate::emulator_saves::kind_of_path(&e.path).is_some()
    {
        return None;
    }
    match e.kind.as_str() {
        "ps4" => Some(Platform::Ps4),
        "ps5" => Some(Platform::Ps5),
        _ if e.path.contains("/savedata_prospero/") => Some(Platform::Ps5),
        _ if e.path.contains("/savedata/") => Some(Platform::Ps4),
        _ => None,
    }
}

/// Most characters of a user's name kept in a folder name.
pub const NAME_CHARS: usize = 40;
/// Most characters of a game's name kept in a folder name: with the title ID, the folders and the
/// file, a backup's path stays well inside Windows' 260 and a UFS drive's 255 bytes a name.
pub const GAME_CHARS: usize = 50;

/// `s` made safe as one folder name everywhere backups go (Windows, macOS, Linux, a NAS over SMB,
/// a PS5's exFAT and UFS drives): no control character, none of `\/:*?"<>|` (a NAS shows a name
/// with them as a mangled `ABC~1` one), no ™ ® ©, runs of spaces made one, no dot or space at
/// either end, at most `max` characters. "" when nothing is left.
pub fn safe_component(s: &str, max: usize) -> String {
    let mapped: String = s
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©'))
        .map(|c| {
            if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(max).collect();
    cut.trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string()
}

/// A user's folder: `<name>_<8 hex>`, the name made safe (`user` when there is none to show).
/// The ID is the account's, as the app prints it (lower-case, 8 digits, every 32-bit value).
pub fn user_dir(name: Option<&str>, uid: u32) -> String {
    let safe = name
        .map(|n| safe_component(n, NAME_CHARS))
        .unwrap_or_default();
    let safe = if safe.is_empty() {
        "user".to_string()
    } else {
        safe
    };
    format!("{safe}_{uid:08x}")
}

/// The account ID a user's folder ends with; None for any other folder.
pub fn user_of_dir(name: &str) -> Option<u32> {
    let (_, hex) = name.rsplit_once('_')?;
    (hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// A game's folder in a user's folder: `<TITLE>`, or `<TITLE> - <game>` with the game's name
/// (made safe) when one is given.
pub fn title_dir(title_id: &str, game: Option<&str>) -> String {
    match game
        .map(|g| safe_component(g, GAME_CHARS))
        .filter(|g| !g.is_empty() && !g.eq_ignore_ascii_case(title_id))
    {
        Some(g) => format!("{title_id} - {g}"),
        None => title_id.to_string(),
    }
}

/// The title ID a game's folder is for (`PPSA01234 - Game` → `PPSA01234`).
pub fn title_of_dir(name: &str) -> &str {
    name.split_once(" - ").map_or(name, |(t, _)| t)
}

/// Whether `name` is a game folder of `title_id`, with or without its name.
pub fn is_title_dir(name: &str, title_id: &str) -> bool {
    title_of_dir(name).eq_ignore_ascii_case(title_id)
}

/// Whether `name` is a backup of `title_id` named the way this app names them:
/// `<title>_user<8 hex>_<YYYY-MM-DD_HHMMSS>[_<tag>].zip`. Anything else in a folder (a file the
/// person put there, a copy cut short) is never deleted.
pub fn ours(title_id: &str, name: &str) -> bool {
    if !crate::save_backup::plain(name) {
        return false;
    }
    let Some((_, stamp)) = parse_backup_name(title_id, name) else {
        return false;
    };
    let b = stamp.as_bytes();
    let shape = b.len() >= 17
        && b[..17].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'_',
            _ => c.is_ascii_digit(),
        });
    shape
        && match &stamp[17..] {
            "" => true,
            tag => tag
                .strip_prefix('_')
                .is_some_and(|t| !t.is_empty() && t.bytes().all(|c| c.is_ascii_lowercase())),
        }
}

/// A folder's entries: `(name, is_dir, size)`.
pub type Entries = Vec<(String, bool, u64)>;
/// Lists a folder of a place by its path relative to the place ("" is the place itself).
pub type List<'a> = dyn Fn(&str) -> Result<Entries, String> + 'a;

/// `a/b`, either may be "".
pub fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{}/{b}", a.trim_end_matches('/')),
    }
}

/// The user folders of a place: `(folder, account ID)`.
fn user_dirs(list: &List) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = list("")
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, dir, _)| *dir)
        .filter_map(|(n, _, _)| user_of_dir(&n).map(|u| (n, u)))
        .collect();
    out.sort();
    out
}

/// Every folder of a place that may hold `kind` files of `title_id`, relative to the place: the
/// by-game `<TITLE>` (for `Encrypted`), then each user's `<kind>/<PS5|PS4>/<TITLE>[ - <game>]`.
pub fn title_dirs(list: &List, kind: Kind, title_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if kind == Kind::Encrypted {
        out.push(title_id.to_string());
    }
    for (u, _) in user_dirs(list) {
        for p in [Platform::Ps5, Platform::Ps4] {
            let base = format!("{u}/{}/{}", kind.dir(), p.dir());
            let mut games: Vec<String> = list(&base)
                .unwrap_or_default()
                .into_iter()
                .filter(|(n, dir, _)| *dir && is_title_dir(n, title_id))
                .map(|(n, _, _)| join(&base, &n))
                .collect();
            games.sort();
            out.extend(games);
        }
    }
    out
}

/// One backup file found in a place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its folder, relative to the place.
    pub dir: String,
    pub name: String,
    pub size: u64,
}

impl Found {
    /// Its path relative to the place.
    pub fn rel(&self) -> String {
        join(&self.dir, &self.name)
    }
}

/// Every file of `title_id`'s backups in a place, in both layouts (any name a backup could have;
/// a caller keeps the ones it wants).
pub fn backup_files(list: &List, title_id: &str) -> Vec<Found> {
    let mut out = Vec::new();
    for dir in title_dirs(list, Kind::Encrypted, title_id) {
        for (name, is_dir, size) in list(&dir).unwrap_or_default() {
            if !is_dir {
                out.push(Found {
                    dir: dir.clone(),
                    name,
                    size,
                });
            }
        }
    }
    out
}

/// The backup `file` of `title_id` in a place, in either layout.
pub fn find(list: &List, title_id: &str, file: &str) -> Option<Found> {
    backup_files(list, title_id)
        .into_iter()
        .find(|f| f.name == file)
}

/// Every game folder of a place with the title it is for: the by-game `<TITLE>` folders and each
/// user's `Encrypted/<PS5|PS4>/<TITLE>[ - <game>]`.
pub fn all_title_dirs(list: &List) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, dir, _) in list("").unwrap_or_default() {
        if dir && user_of_dir(&name).is_none() && crate::save_backup::plain(&name) {
            out.push((name.clone(), name));
        }
    }
    for (u, _) in user_dirs(list) {
        for p in [Platform::Ps5, Platform::Ps4] {
            let base = format!("{u}/{}/{}", Kind::Encrypted.dir(), p.dir());
            for (n, dir, _) in list(&base).unwrap_or_default() {
                let title = title_of_dir(&n).to_string();
                if dir && crate::save_backup::plain(&title) {
                    out.push((join(&base, &n), title));
                }
            }
        }
    }
    out
}

/// Which backups to delete so `keep` of this save's (`title_id`, `user`) backups of kind `tag`
/// remain, counted across every folder of the place in both layouts (`files`): the oldest, and
/// only files named the way this app names its backups.
pub fn to_prune(
    files: &[Found],
    title_id: &str,
    user: u32,
    tag: Option<&str>,
    keep: u32,
) -> Vec<Found> {
    let stem = auto_backup_stem(title_id, user);
    let mut mine: Vec<&Found> = files
        .iter()
        .filter(|f| ours(title_id, &f.name) && f.name.starts_with(&stem))
        .filter(|f| backup_tag(&f.name) == tag)
        .collect();
    mine.sort_by(|a, b| a.name.cmp(&b.name).then(a.dir.cmp(&b.dir)));
    let drop = mine.len().saturating_sub(keep.max(1) as usize);
    mine.into_iter().take(drop).cloned().collect()
}

/// Whose save a new file is for, and what its folders are named after.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owner {
    pub title_id: String,
    pub user: u32,
    /// None: an emulator's save (always by game).
    pub platform: Option<Platform>,
    /// The user's name, when the console says.
    pub name: Option<String>,
    /// The game's name, when known and wanted in its folder.
    pub game: Option<String>,
}

/// The folder, relative to the place, a new `kind` file of `owner` goes in. By game (or an
/// emulator's save): `<TITLE>`. By user: the user's folder already there (found by its account
/// ID, so a renamed user keeps theirs), else `<name>_<8 hex>`; in it `<kind>/<PS5|PS4>/`, then the
/// game's folder already there, else `<TITLE>` or `<TITLE> - <game>`. Err for a decrypted file
/// that has no user folder to go in.
pub fn place(list: &List, layout: Layout, kind: Kind, owner: &Owner) -> Result<String, String> {
    let platform = match (layout, owner.platform) {
        (Layout::ByUser, Some(p)) => p,
        _ if kind == Kind::Encrypted => return Ok(owner.title_id.clone()),
        _ => return Err("only a PS4 or PS5 game's save has a Decrypted folder".into()),
    };
    let user = user_dirs(list)
        .into_iter()
        .find(|(_, u)| *u == owner.user)
        .map(|(d, _)| d)
        .unwrap_or_else(|| user_dir(owner.name.as_deref(), owner.user));
    let base = format!("{user}/{}/{}", kind.dir(), platform.dir());
    let mut games: Vec<String> = list(&base)
        .unwrap_or_default()
        .into_iter()
        .filter(|(n, dir, _)| *dir && is_title_dir(n, &owner.title_id))
        .map(|(n, _, _)| n)
        .collect();
    games.sort();
    let game = games
        .into_iter()
        .next()
        .unwrap_or_else(|| title_dir(&owner.title_id, owner.game.as_deref()));
    Ok(join(&base, &game))
}

/// The tag of a decrypted copy made with each automatic backup (`…_<stamp>_decrypted.zip`): its
/// own versions to keep, apart from the backups'.
pub const DECRYPTED_TAG: &str = "decrypted";

/// The decrypted files of `title_id` in a place (each user's `Decrypted/<PS5|PS4>/<game>`).
pub fn decrypted_files(list: &List, title_id: &str) -> Vec<Found> {
    let mut out = Vec::new();
    for dir in title_dirs(list, Kind::Decrypted, title_id) {
        for (name, is_dir, size) in list(&dir).unwrap_or_default() {
            if !is_dir {
                out.push(Found {
                    dir: dir.clone(),
                    name,
                    size,
                });
            }
        }
    }
    out
}

/// What the decrypted copies made with the automatic backups did, for the Saves screen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecryptedStatus {
    /// Unix ms of the last one tried.
    pub at: u64,
    pub made: u32,
    pub failed: u32,
    /// Why the last one failed; None when they all went.
    pub note: Option<String>,
}

impl DecryptedStatus {
    pub fn record(&mut self, r: &Result<String, String>) {
        self.at = crate::save_backup::now_ms();
        match r {
            Ok(_) => self.made += 1,
            Err(e) => {
                self.failed += 1;
                self.note = Some(e.clone());
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A place in memory: file paths relative to it, folders implied by them.
    #[derive(Default)]
    pub(crate) struct Tree(pub BTreeMap<String, u64>);

    impl Tree {
        pub fn with(files: &[&str]) -> Self {
            Tree(files.iter().map(|f| (f.to_string(), 1)).collect())
        }
        pub fn list(&self, rel: &str) -> Result<Entries, String> {
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            let mut out: BTreeMap<String, (bool, u64)> = BTreeMap::new();
            for (p, size) in &self.0 {
                let Some(rest) = p.strip_prefix(&prefix) else {
                    continue;
                };
                match rest.split_once('/') {
                    Some((d, _)) => {
                        out.insert(d.to_string(), (true, 0));
                    }
                    None => {
                        out.insert(rest.to_string(), (false, *size));
                    }
                }
            }
            if out.is_empty() {
                return Err("no such folder".into());
            }
            Ok(out.into_iter().map(|(n, (d, s))| (n, d, s)).collect())
        }
    }

    const T: &str = "PPSA01234";

    fn owner(user: u32, name: Option<&str>, platform: Option<Platform>) -> Owner {
        Owner {
            title_id: T.into(),
            user,
            platform,
            name: name.map(String::from),
            game: None,
        }
    }

    #[test]
    fn names_are_safe_on_every_destination() {
        assert_eq!(safe_component("Alex", 40), "Alex");
        assert_eq!(
            safe_component("a:b*c?d\"e<f>g|h\\i/j", 40),
            "a b c d e f g h i j"
        );
        assert_eq!(safe_component("  ..trailing dot. ", 40), "trailing dot");
        assert_eq!(safe_component("tab\there\nnew", 40), "tab here new");
        assert_eq!(safe_component("Game™ Name®", 40), "Game Name");
        assert_eq!(safe_component("...", 40), "");
        assert_eq!(safe_component("ünïcödé 名前", 40), "ünïcödé 名前");
        // Cut by characters, never inside one.
        let long = "名".repeat(100);
        assert_eq!(
            safe_component(&long, NAME_CHARS).chars().count(),
            NAME_CHARS
        );
        // A cut that ends on a space or a dot does not leave it at the end.
        assert_eq!(safe_component("abc. def", 4), "abc");
    }

    #[test]
    fn a_user_folder_keeps_the_whole_account_id() {
        assert_eq!(user_dir(Some("Alex"), 0x1a2b3c4d), "Alex_1a2b3c4d");
        // IDs above 0x7fffffff stay as they are (12.6.13 kept them lossless on the wire).
        assert_eq!(user_dir(Some("Sam"), 0x9abc_def0), "Sam_9abcdef0");
        assert_eq!(user_of_dir("Sam_9abcdef0"), Some(0x9abc_def0));
        assert_eq!(user_dir(Some("x_deadbeef"), 1), "x_deadbeef_00000001");
        assert_eq!(user_of_dir("x_deadbeef_00000001"), Some(1));
        // A name with nothing safe in it, or none at all.
        assert_eq!(user_dir(Some("::"), 7), "user_00000007");
        assert_eq!(user_dir(None, 7), "user_00000007");
        // Two users whose names differ only in case never share a folder: the ID differs.
        assert_ne!(user_dir(Some("alex"), 1), user_dir(Some("ALEX"), 2));
        // Not a user folder: a game's, or a name without an 8-digit ID.
        for n in ["PPSA01234", "Alex_1a2b3c4", "Alex_1a2b3c4g", "Alex"] {
            assert_eq!(user_of_dir(n), None, "{n}");
        }
    }

    #[test]
    fn a_game_folder_may_carry_the_game_name() {
        assert_eq!(title_dir(T, None), T);
        assert_eq!(
            title_dir(T, Some("Marvel's Spider-Man 2: Deluxe™")),
            "PPSA01234 - Marvel's Spider-Man 2 Deluxe"
        );
        assert_eq!(title_dir(T, Some("")), T);
        assert_eq!(title_dir(T, Some("PPSA01234")), T);
        let long = title_dir(T, Some(&"x".repeat(300)));
        assert_eq!(long.chars().count(), T.len() + 3 + GAME_CHARS);
        assert!(is_title_dir("PPSA01234 - Game", T));
        assert!(is_title_dir("PPSA01234", T));
        assert!(!is_title_dir("PPSA012345", T));
        assert!(!is_title_dir("PPSA05678 - Game", T));
    }

    #[test]
    fn ps4_and_ps5_saves_are_told_apart_and_emulators_stay_by_game() {
        let e = |kind: &str, path: &str| SaveEntry {
            title_id: T.into(),
            user_id: 1,
            path: path.into(),
            size: 0,
            mtime: 0,
            kind: kind.into(),
        };
        assert_eq!(
            platform_of(&e("ps5", "/user/home/1/savedata_prospero/PPSA01234")),
            Some(Platform::Ps5)
        );
        assert_eq!(
            platform_of(&e("ps4", "/user/home/1/savedata/CUSA01234")),
            Some(Platform::Ps4)
        );
        // A save described only by its folder (the one a restore replaces).
        assert_eq!(
            platform_of(&e("", "/user/home/1/savedata/CUSA01234")),
            Some(Platform::Ps4)
        );
        assert_eq!(
            platform_of(&e("", "/user/home/1/savedata_prospero/PPSA01234")),
            Some(Platform::Ps5)
        );
        assert_eq!(
            platform_of(&e(crate::emulator_saves::KIND, "/data/x")),
            None
        );
        assert_eq!(platform_of(&e("", "/somewhere/else")), None);
    }

    #[test]
    fn a_new_backup_goes_by_game_or_by_user() {
        let empty = Tree::default();
        let list = |r: &str| empty.list(r);
        let o = owner(0x1a2b3c4d, Some("Alex"), Some(Platform::Ps5));
        assert_eq!(
            place(&list, Layout::ByGame, Kind::Encrypted, &o).unwrap(),
            "PPSA01234"
        );
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Encrypted, &o).unwrap(),
            "Alex_1a2b3c4d/Encrypted/PS5/PPSA01234"
        );
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Decrypted, &o).unwrap(),
            "Alex_1a2b3c4d/Decrypted/PS5/PPSA01234"
        );
        let ps4 = Owner {
            title_id: "CUSA01234".into(),
            ..owner(0x1a2b3c4d, Some("Alex"), Some(Platform::Ps4))
        };
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Encrypted, &ps4).unwrap(),
            "Alex_1a2b3c4d/Encrypted/PS4/CUSA01234"
        );
        let named = Owner {
            game: Some("Astro Bot".into()),
            ..o.clone()
        };
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Encrypted, &named).unwrap(),
            "Alex_1a2b3c4d/Encrypted/PS5/PPSA01234 - Astro Bot"
        );
        // An emulator's save stays by game; it has no decrypted folder.
        let emu = owner(0x1f1e1d1c, None, None);
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Encrypted, &emu).unwrap(),
            "PPSA01234"
        );
        assert!(place(&list, Layout::ByUser, Kind::Decrypted, &emu).is_err());
    }

    #[test]
    fn a_renamed_user_and_a_named_game_keep_their_folders() {
        let tree = Tree::with(&[
            "OldName_1a2b3c4d/Encrypted/PS5/PPSA01234 - Astro Bot/x.zip",
            "Other_00000002/Encrypted/PS5/PPSA01234/y.zip",
        ]);
        let list = |r: &str| tree.list(r);
        // Renamed to "NewName", and game names now off: the folders already there are used.
        let o = owner(0x1a2b3c4d, Some("NewName"), Some(Platform::Ps5));
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Encrypted, &o).unwrap(),
            "OldName_1a2b3c4d/Encrypted/PS5/PPSA01234 - Astro Bot"
        );
        // Their decrypted folder is new, under the same user folder.
        assert_eq!(
            place(&list, Layout::ByUser, Kind::Decrypted, &o).unwrap(),
            "OldName_1a2b3c4d/Decrypted/PS5/PPSA01234"
        );
    }

    #[test]
    fn listing_finds_both_layouts_and_only_this_game() {
        let tree = Tree::with(&[
            "PPSA01234/PPSA01234_user00000001_2026-10-01_000000.zip",
            "Alex_00000001/Encrypted/PS5/PPSA01234 - Astro Bot/PPSA01234_user00000001_2026-11-01_000000.zip",
            "Alex_00000001/Encrypted/PS5/PPSA05678/PPSA05678_user00000001_2026-11-01_000000.zip",
            "Alex_00000001/Decrypted/PS5/PPSA01234/PPSA01234 save shared 2026-11-01_000000.zip",
            "Sam_9abcdef0/Encrypted/PS4/PPSA01234/PPSA01234_user9abcdef0_2026-11-02_000000.zip",
            "notes/readme.txt",
        ]);
        let list = |r: &str| tree.list(r);
        let mut names: Vec<String> = backup_files(&list, T).into_iter().map(|f| f.name).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "PPSA01234_user00000001_2026-10-01_000000.zip",
                "PPSA01234_user00000001_2026-11-01_000000.zip",
                "PPSA01234_user9abcdef0_2026-11-02_000000.zip",
            ]
        );
        let f = find(&list, T, "PPSA01234_user00000001_2026-11-01_000000.zip").unwrap();
        assert_eq!(f.dir, "Alex_00000001/Encrypted/PS5/PPSA01234 - Astro Bot");
        assert!(find(&list, T, "nothing.zip").is_none());
        assert_eq!(decrypted_files(&list, T).len(), 1);
        let mut titles: Vec<String> = all_title_dirs(&list).into_iter().map(|(_, t)| t).collect();
        titles.sort();
        assert_eq!(
            titles,
            ["PPSA01234", "PPSA01234", "PPSA01234", "PPSA05678", "notes"]
        );
    }

    #[test]
    fn retention_counts_both_layouts_and_never_deletes_foreign_files() {
        let tree = Tree::with(&[
            "PPSA01234/PPSA01234_user00000001_2026-10-01_000000.zip",
            "PPSA01234/PPSA01234_user00000001_2026-10-02_000000.zip",
            "PPSA01234/PPSA01234_user00000001_my copy.zip",
            "PPSA01234/PPSA01234_user00000002_2026-09-01_000000.zip",
            "Alex_00000001/Encrypted/PS5/PPSA01234/PPSA01234_user00000001_2026-11-01_000000.zip",
            "Alex_00000001/Encrypted/PS5/PPSA01234/PPSA01234_user00000001_2026-11-02_000000_snap.zip",
            "Alex_00000001/Encrypted/PS5/PPSA01234/PPSA01234_user00000001_2026-11-03_000000.zip",
            "Alex_00000001/Encrypted/PS5/PPSA01234/PPSA01234_user00000001_2026-11-03_000000.zip.part",
        ]);
        let list = |r: &str| tree.list(r);
        let files = backup_files(&list, T);
        let pruned: Vec<String> = to_prune(&files, T, 1, None, 2)
            .into_iter()
            .map(|f| f.rel())
            .collect();
        // Four automatic backups of user 1 across both layouts: the two oldest go, wherever.
        assert_eq!(
            pruned,
            [
                "PPSA01234/PPSA01234_user00000001_2026-10-01_000000.zip",
                "PPSA01234/PPSA01234_user00000001_2026-10-02_000000.zip",
            ]
        );
        // Snapshots keep their own count; another user's are never touched.
        assert!(to_prune(&files, T, 1, Some("snap"), 1).is_empty());
        assert!(to_prune(&files, T, 2, None, 1).is_empty());
        // Keep 1: only ours go, never a file named by hand or cut short.
        let all: Vec<String> = to_prune(&files, T, 1, None, 1)
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|n| ours(T, n)));
    }

    #[test]
    fn only_our_names_are_ours() {
        assert!(ours(T, "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip"));
        assert!(ours(T, "PPSA01234_user1a2b3c4d_2026-11-17_101500_snap.zip"));
        assert!(ours(
            T,
            "PPSA01234_user1a2b3c4d_2026-11-17_101500_decrypted.zip"
        ));
        assert!(!ours(
            T,
            "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip.part"
        ));
        assert!(!ours(T, "PPSA01234_user1a2b3c4d_mine.zip"));
        assert!(!ours(T, "PPSA01234 save shared 2026-11-17_101500.zip"));
        assert!(!ours(T, "PPSA09999_user1a2b3c4d_2026-11-17_101500.zip"));
    }

    #[test]
    fn layout_reads_from_old_settings_as_by_game() {
        #[derive(Deserialize)]
        struct S {
            #[serde(default)]
            layout: Layout,
        }
        let s: S = serde_json::from_str("{}").unwrap();
        assert_eq!(s.layout, Layout::ByGame);
        let s: S = serde_json::from_str(r#"{"layout":"by_user"}"#).unwrap();
        assert_eq!(s.layout, Layout::ByUser);
    }
}
