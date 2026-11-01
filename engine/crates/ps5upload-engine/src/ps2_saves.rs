//! The saves of the PlayStation 2 emulators for the PS5, for the automatic save backups: Crux2
//! (PPSA99249) and PS5SX2 (PPSA99203), both ports of PCSX2. Each keeps them in two folders of its
//! data folder, every game's in the same ones:
//! - its memory cards, as files (`<serial>.ps2` with a card for each game, `Mcd001.ps2`…) or, for
//!   a folder card, a folder of them;
//! - its saved states (`<serial> (<CRC>).<slot>.p2s`, the ones it makes by itself and the copies
//!   PCSX2 keeps of a slot's last state, as they are).
//!
//! Each folder is one save, listed among a console's saves under the emulator's own title ID
//! (`kind` "ps2"), so it is backed up when the emulator closes, kept and restored as a PS5 save is,
//! its files as they are. The emulators have no profiles: the user number tells the two apart (0
//! the cards, 1 the saved states), so each keeps its own backups and goes back on its own.
//!
//! Where each keeps its data folder:
//! - Crux2: `/data/crux2` when that folder is there, else a `crux2-data` folder at the top of a USB
//!   drive, else the title's own folder, wherever it was installed (`OutsideFolder` and
//!   `FindTitleFolder` in its ps5/frontend/ps5_platform.cpp); the cards in `memcards`, the states in
//!   `sstates`, PCSX2's own names.
//! - PS5SX2: `/data/PCSX2`, the cards in `memcards` and the states in `savestates` when those
//!   folders are there, else loose in `/data/PCSX2` itself (`OrbisDir` in its
//!   ps5/coreorbis/orbis-shims/orbis_paths.cpp; as installed from the Prospero Store, it has
//!   neither). That folder also holds its BIOS, games, covers and caches, so there a save is only
//!   its own files, and its path says which: `/data/PCSX2/*.ps2` the cards, `/data/PCSX2/*.p2s`
//!   the states.

use ps5upload_core::fs_ops::DirEntry;
use ps5upload_core::saves::SaveEntry;

use crate::emulator_saves::{measure, DEPTH};

/// `SaveEntry::kind` of their saves.
pub const KIND: &str = "ps2";

/// The folder of the memory cards in a data folder, and its save's user number.
const CARDS: &str = "memcards";
pub const CARDS_USER: i32 = 0;
/// The saved states' user number.
pub const STATES_USER: i32 = 1;

/// The last part of a loose save's path, for each user number.
const LOOSE: [(&str, i32); 2] = [("*.ps2", CARDS_USER), ("*.p2s", STATES_USER)];

/// One of the emulators: its title, its name, and where its saves may be.
pub struct Emulator {
    pub app: &'static str,
    pub name: &'static str,
    /// Its saved states' folder in a data folder.
    states: &'static str,
    /// Without a save's folder, that save is loose in the data folder.
    loose: bool,
    /// Its data folders outside the title's, the first one there used.
    outside: &'static [&'static str],
    /// Where its title folder may be installed, the first one there used when no data folder
    /// outside it is.
    installed: &'static [&'static str],
}

impl Emulator {
    /// Its save folders in a data folder, each with its save's user number.
    fn folders(&self) -> [(&'static str, i32); 2] {
        [(CARDS, CARDS_USER), (self.states, STATES_USER)]
    }

    fn has_data_folder(&self, path: &str) -> bool {
        self.outside.contains(&path) || self.installed.contains(&path)
    }
}

pub const CRUX2: Emulator = Emulator {
    app: "PPSA99249",
    name: "Crux2",
    states: "sstates",
    loose: false,
    outside: &[
        "/data/crux2",
        "/mnt/usb0/crux2-data",
        "/mnt/usb1/crux2-data",
        "/mnt/usb2/crux2-data",
        "/mnt/usb3/crux2-data",
        "/mnt/usb4/crux2-data",
        "/mnt/usb5/crux2-data",
        "/mnt/usb6/crux2-data",
        "/mnt/usb7/crux2-data",
    ],
    installed: &[
        "/data/homebrew/PPSA99249",
        "/user/app/PPSA99249",
        "/mnt/ext0/homebrew/PPSA99249",
        "/mnt/ext1/homebrew/PPSA99249",
        "/mnt/usb0/homebrew/PPSA99249",
        "/mnt/usb1/homebrew/PPSA99249",
    ],
};

pub const PS5SX2: Emulator = Emulator {
    app: "PPSA99203",
    name: "PS5SX2",
    states: "savestates",
    loose: true,
    outside: &["/data/PCSX2"],
    installed: &[],
};

pub const EMULATORS: [&Emulator; 2] = [&CRUX2, &PS5SX2];

/// Where a save of an emulator is on the console.
pub struct Place<'a> {
    pub emulator: &'static Emulator,
    /// The save's user number: [`CARDS_USER`] or [`STATES_USER`].
    pub user: i32,
    /// The folder its entries are in.
    pub folder: &'a str,
    /// It is only some of the folder's entries (see [`Place::holds`]), not the whole folder.
    pub loose: bool,
}

impl Place<'_> {
    /// The folder's entry `name` is part of the save.
    pub fn holds(&self, name: &str) -> bool {
        !self.loose || is_loose_save(self.user, name)
    }
}

/// `name` is, loose in a data folder, part of the save of that user number: a card (or folder
/// card) for the cards, a saved state (or PCSX2's copy of a slot's last one) for the states.
fn is_loose_save(user: i32, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    match user {
        CARDS_USER => name.ends_with(".ps2"),
        STATES_USER => name.ends_with(".p2s") || name.ends_with(".p2s.backup"),
        _ => false,
    }
}

/// Where the save whose path is `path` is: a save folder of an emulator (one of the places it may
/// be, exactly), or, for one that keeps them loose, `<data folder>/*.ps2` or `/*.p2s`.
pub fn place_of(path: &str) -> Option<Place<'_>> {
    let (data, last) = path.rsplit_once('/')?;
    let emulator = EMULATORS.into_iter().find(|e| e.has_data_folder(data))?;
    if let Some((_, user)) = emulator.folders().into_iter().find(|(f, _)| *f == last) {
        return Some(Place {
            emulator,
            user,
            folder: path,
            loose: false,
        });
    }
    let (_, user) = LOOSE.into_iter().find(|(l, _)| *l == last)?;
    emulator.loose.then_some(Place {
        emulator,
        user,
        folder: data,
        loose: true,
    })
}

/// `dir` holds the title, as the emulator tells it: its `eboot.bin` and `sce_sys/param.json`.
/// (The console's own `/user/app/<title>` holds the second only.)
fn is_title_folder(ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>, dir: &str) -> bool {
    let has = |path: &str, name: &str| {
        ls(path).is_ok_and(|l| l.iter().any(|e| e.kind == "file" && e.name == name))
    };
    has(dir, "eboot.bin") && has(&format!("{dir}/sce_sys"), "param.json")
}

/// The data folder `emulator` uses on the console: the first of its data folders that is there,
/// else the first folder it is installed in. None when neither is.
fn data_folder(
    ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>,
    emulator: &Emulator,
) -> Option<&'static str> {
    emulator
        .outside
        .iter()
        .find(|d| ls(d).is_ok())
        .or_else(|| emulator.installed.iter().find(|d| is_title_folder(ls, d)))
        .copied()
}

/// The emulators' saves on `host`: for each one, its cards and its saved states, each when it has
/// a file.
pub fn list(host: &str) -> Vec<SaveEntry> {
    saves_in(&|path: &str| crate::emulator_saves::ls(host, path))
}

/// [`list`], with `ls` giving the entries of a folder on the console.
pub fn saves_in(ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>) -> Vec<SaveEntry> {
    let mut out = Vec::new();
    for emulator in EMULATORS {
        let Some(data) = data_folder(ls, emulator) else {
            continue;
        };
        // The data folder's own entries, for an emulator that may keep its saves loose in it.
        let top = if emulator.loose {
            match ls(data) {
                Ok(top) => top,
                Err(_) => continue,
            }
        } else {
            Vec::new()
        };
        for ((folder, user), (loose, _)) in emulator.folders().into_iter().zip(LOOSE) {
            let in_folder =
                !emulator.loose || top.iter().any(|e| e.kind == "dir" && e.name == folder);
            let path = if in_folder {
                format!("{data}/{folder}")
            } else {
                format!("{data}/{loose}")
            };
            // A folder not read whole waits for the next look, and a save with no file is no save
            // yet.
            let measured = if in_folder {
                measure(ls, &path, 0, DEPTH)
            } else {
                measure_loose(ls, data, &top, user)
            };
            let Ok((size, mtime, files)) = measured else {
                continue;
            };
            if files == 0 {
                continue;
            }
            out.push(SaveEntry {
                title_id: emulator.app.into(),
                user_id: user,
                path,
                size: size as i64,
                mtime,
                kind: KIND.into(),
            });
        }
    }
    out
}

/// [`measure`] of the save of `user` loose among `top`, the entries of the folder `data`.
fn measure_loose(
    ls: &dyn Fn(&str) -> Result<Vec<DirEntry>, String>,
    data: &str,
    top: &[DirEntry],
    user: i32,
) -> Result<(u64, i64, u32), String> {
    let (mut size, mut newest, mut files) = (0u64, 0i64, 0u32);
    for e in top.iter().filter(|e| is_loose_save(user, &e.name)) {
        newest = newest.max(e.mtime);
        match e.kind.as_str() {
            "file" => {
                size += e.size;
                files += 1;
            }
            "dir" => {
                let (s, m, f) = measure(ls, &format!("{data}/{}", e.name), e.mtime, DEPTH - 1)?;
                size += s;
                newest = newest.max(m);
                files += f;
            }
            _ => {}
        }
    }
    Ok((size, newest, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const CRUX2_HOME: &str = "/mnt/ext1/homebrew/PPSA99249";

    fn entry(name: &str, kind: &str, size: u64, mtime: i64) -> DirEntry {
        DirEntry {
            name: name.into(),
            kind: kind.into(),
            size,
            mtime,
        }
    }

    /// The title installed in `dir`: its eboot.bin and sce_sys/param.json.
    fn install(fs: &mut BTreeMap<String, Vec<DirEntry>>, dir: &str) {
        fs.insert(
            dir.into(),
            vec![
                entry("eboot.bin", "file", 40_000_000, 10),
                entry("sce_sys", "dir", 0, 10),
                entry("memcards", "dir", 0, 10),
                entry("sstates", "dir", 0, 10),
            ],
        );
        fs.insert(
            format!("{dir}/sce_sys"),
            vec![entry("param.json", "file", 900, 10)],
        );
    }

    /// A console with Crux2 installed on the M.2 drive, its data in its own folder, and PS5SX2
    /// with its saves' folders.
    fn console() -> BTreeMap<String, Vec<DirEntry>> {
        let mut fs = BTreeMap::new();
        install(&mut fs, CRUX2_HOME);
        // What the console keeps of an installed app: its param.json, not the title.
        fs.insert(
            "/user/app/PPSA99249".into(),
            vec![
                entry("sce_sys", "dir", 0, 10),
                entry("icon0.png", "file", 134_877, 10),
                entry("mount.lnk", "file", 28, 10),
            ],
        );
        fs.insert(
            "/user/app/PPSA99249/sce_sys".into(),
            vec![entry("param.json", "file", 846, 10)],
        );
        fs.insert(
            format!("{CRUX2_HOME}/memcards"),
            vec![
                entry(".", "dir", 0, 0),
                entry("SLAJ-25054.ps2", "file", 8_650_752, 300),
                entry("Mcd002.ps2", "file", 8_650_752, 200),
            ],
        );
        fs.insert(
            format!("{CRUX2_HOME}/sstates"),
            vec![
                entry("SLES-52725 (0197EBD0).01.p2s", "file", 16_391_953, 900),
                entry("SLES-52725 (0197EBD0).resume.p2s", "file", 16_094_767, 950),
            ],
        );
        fs.insert(
            "/data/PCSX2".into(),
            vec![
                entry("memcards", "dir", 0, 50),
                entry("savestates", "dir", 0, 50),
                entry("bios", "dir", 0, 50),
            ],
        );
        fs.insert(
            "/data/PCSX2/memcards".into(),
            vec![
                // A folder card.
                entry("Mcd001.ps2", "dir", 0, 400),
            ],
        );
        fs.insert(
            "/data/PCSX2/memcards/Mcd001.ps2".into(),
            vec![
                entry("_pcsx2_superblock", "file", 1024, 400),
                entry("BASLUS-21050", "dir", 0, 450),
            ],
        );
        fs.insert(
            "/data/PCSX2/memcards/Mcd001.ps2/BASLUS-21050".into(),
            vec![entry("icon.sys", "file", 964, 600)],
        );
        // No state saved yet.
        fs.insert("/data/PCSX2/savestates".into(), vec![]);
        fs
    }

    /// PS5SX2 as the Prospero Store installs it: no saves' folders, its saves loose among the rest.
    fn ps5sx2_loose(fs: &mut BTreeMap<String, Vec<DirEntry>>) {
        fs.remove("/data/PCSX2/memcards");
        fs.remove("/data/PCSX2/savestates");
        fs.insert(
            "/data/PCSX2".into(),
            vec![
                entry("bios", "dir", 0, 50),
                entry("games", "dir", 0, 50),
                entry("ps5vk-shader-cache", "dir", 0, 50),
                entry("lastgame.txt", "file", 15, 700),
                entry("vulkan_shaders.bin", "file", 489_592, 800),
                entry("Mcd001.ps2", "file", 8_650_752, 500),
                entry("Mcd002.ps2", "file", 8_650_752, 400),
                entry("SLUS-20864 (DA6A2B67).01.p2s", "file", 9_000_000, 650),
                entry(
                    "SLUS-20864 (DA6A2B67).01.p2s.backup",
                    "file",
                    8_900_000,
                    640,
                ),
            ],
        );
    }

    fn ls_of(
        fs: &BTreeMap<String, Vec<DirEntry>>,
    ) -> impl Fn(&str) -> Result<Vec<DirEntry>, String> + '_ {
        move |p: &str| fs.get(p).cloned().ok_or_else(|| format!("{p}: _errno_2"))
    }

    fn place(path: &str) -> Option<(&'static str, i32, &str, bool)> {
        place_of(path).map(|p| (p.emulator.name, p.user, p.folder, p.loose))
    }

    #[test]
    fn the_cards_and_the_states_are_each_a_save_under_the_emulators_title() {
        let fs = console();
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves.len(), 3);
        let cards = &saves[0];
        assert_eq!(cards.title_id, CRUX2.app);
        assert_eq!(cards.path, format!("{CRUX2_HOME}/memcards"));
        assert_eq!((cards.user_id, cards.kind.as_str()), (CARDS_USER, KIND));
        assert_eq!(cards.size, 2 * 8_650_752);
        assert_eq!(cards.mtime, 300);
        let states = &saves[1];
        assert_eq!(states.title_id, CRUX2.app);
        assert_eq!(states.path, format!("{CRUX2_HOME}/sstates"));
        assert_eq!(states.user_id, STATES_USER);
        assert_eq!(states.size, 16_391_953 + 16_094_767);
        assert_eq!(states.mtime, 950);
        // PS5SX2 has saved no state: only its cards. The folder card is read into: its newest
        // file and both of them.
        let ps5sx2 = &saves[2];
        assert_eq!(ps5sx2.title_id, PS5SX2.app);
        assert_eq!(ps5sx2.path, "/data/PCSX2/memcards");
        assert_eq!(ps5sx2.mtime, 600);
        assert_eq!(ps5sx2.size, 1024 + 964);
        for s in &saves {
            assert!(crate::emulator_saves::backs_up(s));
            assert!(crate::emulator_saves::is_emulator_save(s));
            assert_eq!(crate::emulator_saves::app_of(s), s.title_id);
            let p = place_of(&s.path).unwrap();
            assert_eq!((p.emulator.app, p.user), (s.title_id.as_str(), s.user_id));
            assert!(!p.loose && p.folder == s.path);
        }
    }

    #[test]
    fn ps5sx2s_loose_saves_are_only_its_cards_and_states() {
        let mut fs = console();
        ps5sx2_loose(&mut fs);
        let saves = saves_in(&ls_of(&fs));
        let ps5sx2: Vec<&SaveEntry> = saves.iter().filter(|s| s.title_id == PS5SX2.app).collect();
        assert_eq!(ps5sx2.len(), 2);
        let (cards, states) = (ps5sx2[0], ps5sx2[1]);
        assert_eq!(
            (cards.path.as_str(), cards.user_id),
            ("/data/PCSX2/*.ps2", CARDS_USER)
        );
        assert_eq!(cards.size, 2 * 8_650_752);
        assert_eq!(cards.mtime, 500);
        assert_eq!(
            (states.path.as_str(), states.user_id),
            ("/data/PCSX2/*.p2s", STATES_USER)
        );
        assert_eq!(states.size, 9_000_000 + 8_900_000);
        assert_eq!(states.mtime, 650);
        // Their place is the top folder, and only their own files in it.
        let p = place_of(&cards.path).unwrap();
        assert_eq!((p.folder, p.loose), ("/data/PCSX2", true));
        assert!(p.holds("Mcd001.ps2") && p.holds("MCD002.PS2"));
        assert!(!p.holds("bios") && !p.holds("vulkan_shaders.bin") && !p.holds("x.p2s"));
        let p = place_of(&states.path).unwrap();
        assert!(p.holds("SLUS-20864 (DA6A2B67).01.p2s") && p.holds("a.p2s.backup"));
        assert!(!p.holds("Mcd001.ps2") && !p.holds("games") && !p.holds("lastgame.txt"));
        // A saves' folder made later is where the emulator then keeps them: the loose cards
        // are no longer the save.
        let top = fs.get_mut("/data/PCSX2").unwrap();
        top.push(entry("memcards", "dir", 0, 900));
        fs.insert(
            "/data/PCSX2/memcards".into(),
            vec![entry("Mcd001.ps2", "file", 8_650_752, 950)],
        );
        let saves = saves_in(&ls_of(&fs));
        let paths: Vec<&str> = saves
            .iter()
            .filter(|s| s.title_id == PS5SX2.app)
            .map(|s| s.path.as_str())
            .collect();
        assert_eq!(paths, ["/data/PCSX2/memcards", "/data/PCSX2/*.p2s"]);
    }

    #[test]
    fn crux2s_data_folder_outside_the_title_comes_first() {
        let mut fs = console();
        fs.insert("/data/crux2".into(), vec![entry("memcards", "dir", 0, 50)]);
        fs.insert(
            "/data/crux2/memcards".into(),
            vec![entry("Mcd001.ps2", "file", 8_650_752, 700)],
        );
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves[0].path, "/data/crux2/memcards");
        assert_eq!(saves[0].mtime, 700);
        // Its saves are only there: the title folder's older ones are not the save any more.
        assert_eq!(saves[1].title_id, PS5SX2.app);
        fs.insert("/data/crux2/memcards".into(), vec![]);
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].title_id, PS5SX2.app);
    }

    #[test]
    fn crux2_is_found_where_it_was_installed() {
        let mut fs = console();
        let card = fs.remove(&format!("{CRUX2_HOME}/memcards")).unwrap();
        fs.remove(CRUX2_HOME);
        fs.remove(&format!("{CRUX2_HOME}/sce_sys"));
        // A folder left without the whole title in it is not where Crux2 is.
        fs.insert(
            "/data/homebrew/PPSA99249".into(),
            vec![entry("eboot.bin", "file", 40_000_000, 10)],
        );
        install(&mut fs, "/mnt/usb0/homebrew/PPSA99249");
        fs.insert("/mnt/usb0/homebrew/PPSA99249/memcards".into(), card);
        let saves = saves_in(&ls_of(&fs));
        assert_eq!(saves[0].path, "/mnt/usb0/homebrew/PPSA99249/memcards");
    }

    #[test]
    fn no_emulator_or_no_file_is_no_save() {
        let nothing = |p: &str| -> Result<Vec<DirEntry>, String> { Err(format!("{p}: _errno_2")) };
        assert!(saves_in(&nothing).is_empty());
        let mut fs = console();
        // PS5SX2 there with nothing saved yet; Crux2's cards folder empty, and its states
        // folder not made.
        fs.insert("/data/PCSX2".into(), vec![entry("bios", "dir", 0, 50)]);
        fs.insert(format!("{CRUX2_HOME}/memcards"), vec![]);
        fs.remove(&format!("{CRUX2_HOME}/sstates"));
        assert!(saves_in(&ls_of(&fs)).is_empty());
    }

    #[test]
    fn only_a_save_of_an_emulator_has_a_place() {
        assert_eq!(
            place("/data/PCSX2/memcards"),
            Some(("PS5SX2", CARDS_USER, "/data/PCSX2/memcards", false))
        );
        assert_eq!(
            place("/data/PCSX2/savestates"),
            Some(("PS5SX2", STATES_USER, "/data/PCSX2/savestates", false))
        );
        assert_eq!(
            place("/data/PCSX2/*.ps2"),
            Some(("PS5SX2", CARDS_USER, "/data/PCSX2", true))
        );
        assert_eq!(
            place("/data/PCSX2/*.p2s"),
            Some(("PS5SX2", STATES_USER, "/data/PCSX2", true))
        );
        assert_eq!(
            place("/mnt/usb3/crux2-data/memcards"),
            Some(("Crux2", CARDS_USER, "/mnt/usb3/crux2-data/memcards", false))
        );
        assert_eq!(
            place(&format!("{CRUX2_HOME}/sstates")).map(|p| (p.0, p.1)),
            Some(("Crux2", STATES_USER))
        );
        for not in [
            "/data/PCSX2",
            "/data/PCSX2/",
            "/data/PCSX2/memcards/Mcd001.ps2",
            "/data/PCSX2/Mcd001.ps2",
            "/data/PCSX2/*",
            "/data/PCSX2/*.bin",
            "/data/PCSX2/sstates",
            "/data/crux2/savestates",
            "/data/crux2/*.ps2",
            "/data/PCSX2/bios",
            "/data/PCSX2/../crux2/memcards",
            "/data/PCSX2memcards",
            "/mnt/ext1/homebrew/PPSA99249/inis",
            "/mnt/ext1/homebrew/PPSA01234/memcards",
            "/user/home/1a2b3c4d/savedata_prospero/PPSA99249",
        ] {
            assert!(place_of(not).is_none(), "{not}");
        }
    }
}
