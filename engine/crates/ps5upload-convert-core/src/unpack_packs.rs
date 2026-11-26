//! A game in asset packs (LZ4 or zstd) read as the plain game again: every packed file under
//! its own name, the pack volumes and their indexes left out. What "Unpack the asset packs"
//! writes as a folder or an image, with no copy on disk in between.
//!
//! What happens to the emulator and to `ampr_emu.index` follows quer3q/ps5-dump-forge v0.5.4's
//! `--zstd-unpack` / `--lz4-unpack` (`ps5-dump-forge-core/src/lz4.rs`, `open`, `prepare` and
//! `Runtime::action`, GPL-3.0-or-later):
//!
//! - a game that reads through libSceAmpr keeps an ampr_emu as new as the one this app installs;
//!   ampr-emu-ng (it came with the zstd packs and has nothing left to read), an older ampr_emu
//!   or a build this app can't name is replaced by the emulator Convert's settings choose, and
//!   a game with none gets it;
//! - a game that doesn't read through libSceAmpr keeps whatever it has;
//! - `ampr_commands.bin` and the emulator's logs are left out (the journal's file ids follow
//!   the old index's order), and a fresh `ampr_emu.index` lists every file written.
//!
//! A package can instead leave the emulator out ([`open_without_emulator`]): an installed
//! package is a real PFS the console's own libSceAmpr reads, and Spider-Man 2 (a libSceAmpr
//! backport) played on FW 5.10 from one of ours with `fakelib/libSceAmpr.sprx` left out and an
//! `ampr_emu.index` in it. One game on one firmware, so it is the user's choice, never the
//! default.
//!
//! The engine fetches the emulator that takes the place of one that has to go. The converter on
//! the PS5 fetches nothing: when the user had the app put ampr_emu on the console
//! ([`console_ampr_emu`]) it unpacks exactly as the engine does with that file
//! ([`open_with_console_emulator`]); otherwise it has a third way ([`open_with_own_emulator`]):
//! the game's own emulator stays when the rule above keeps it, and the unpack is refused
//! ([`NEEDS_OTHER_EMULATOR`]) when the rule would have put another in, so plain files never end
//! up beside an emulator the rule does not keep there. [`assess`] says which it will be before
//! anything is built.
//!
//! Shared by the engine (`ps5upload-engine`, which re-exports it and keeps the tests that build
//! through its own image, folder and package jobs) and the converter on the PS5
//! (`ps5upload-convert`).

use std::collections::HashMap;

use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::image::{emulator_home, Lz4Unpacked, Unpacking, RUNTIME_VERSION};
use ps5upload_fpkg::ampr_pack::ng;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// What a run of the game wrote: not part of the game, and stale once the index is new.
const LEFT_OUT: [&str; 3] = ["ampr_commands.bin", "ampr_emu.log", "apr_emu.log"];

/// The emulator an unpacked game gets when its own has to go: the one Convert's settings
/// choose, fetched when it is needed.
pub struct Emulator<'a> {
    /// It is ampr-emu-ng: a game that already has ampr-emu-ng then keeps it.
    pub ng: bool,
    pub fetch: &'a dyn Fn() -> Result<Vec<u8>, String>,
}

/// What an unpack does with the game's `libSceAmpr.sprx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeAction {
    /// Not a libSceAmpr game: whatever it has stays.
    Untouched,
    /// It has an emulator that runs the plain game.
    Keep,
    /// It has none: the chosen one goes in.
    Install,
    /// The chosen one takes its place.
    Replace,
}

/// The action for a game that reads through libSceAmpr (`ampr`) or not, by the emulator it has
/// (`current`: [`ng::classify`]'s kind and version, `None` when it has none) and whether the
/// chosen replacement is ampr-emu-ng (`ng_chosen`). ps5-dump-forge's `Runtime::action(ampr,
/// false)`; the one difference is that ampr-emu-ng stays when it is also what would go in.
pub fn runtime_action(
    ampr: bool,
    current: Option<(&str, Option<&str>)>,
    ng_chosen: bool,
) -> RuntimeAction {
    if !ampr {
        return RuntimeAction::Untouched;
    }
    match current {
        None => RuntimeAction::Install,
        Some(("ampr-emu-ng", _)) if ng_chosen => RuntimeAction::Keep,
        Some(("ampr_emu", Some(v))) if !ng::older(v, RUNTIME_VERSION) => RuntimeAction::Keep,
        Some(_) => RuntimeAction::Replace,
    }
}

/// The unpacked game: the decoded tree with the emulator and index this module decided on.
struct Plain {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    generated: HashMap<String, Vec<u8>>,
}

impl SourceTree for Plain {
    fn release(&mut self) {
        self.inner.release();
    }

    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        match self.generated.get(path) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.inner.read(path),
        }
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        match self.generated.get(path) {
            Some(bytes) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = start.saturating_add(len).min(bytes.len());
                Ok(bytes[start..end].to_vec())
            }
            None => self.inner.read_range(path, offset, len),
        }
    }
    fn describe(&self) -> String {
        format!("{} (asset packs unpacked)", self.inner.describe())
    }
}

/// What an unpack does with the game's AMPR emulator.
#[derive(Clone, Copy)]
enum EmulatorChoice<'a> {
    /// The game's own stays, whatever it is.
    Keep,
    /// The game's own stays when the rule keeps it; else the unpack is refused
    /// ([`NEEDS_OTHER_EMULATOR`]). Nothing is fetched.
    Own,
    /// The game's own goes when it only suits packs, and a libSceAmpr game with none gets this.
    Chosen(&'a Emulator<'a>),
    /// Every emulator file in `fakelib` and `fakelib2` is left out ([`is_emulator_file`]).
    LeaveOut,
}

/// The AMPR emulator's module where a game loads it from, and the copy of a game's own an
/// earlier build kept beside it (`libSceAmpr.sprx.orig`), in any letter case.
pub fn is_emulator_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    ["fakelib/", "fakelib2/"].iter().any(|dir| {
        p.strip_prefix(dir)
            .is_some_and(|name| name == "libsceampr.sprx" || name == "libsceampr.sprx.orig")
    })
}

/// `tree` with its asset packs decoded, or why it has none to decode. `emulator` is what
/// replaces an emulator that has to go; `None` (Convert's "Add the AMPR emulator" is off)
/// leaves the game's own where it is. Every record of the new `ampr_emu.index` gets `mtime`: a
/// read-back that opens the source again passes the same one and gets the same bytes.
pub fn open(
    tree: Box<dyn SourceTree>,
    emulator: Option<&Emulator>,
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    unpack(
        tree,
        emulator.map_or(EmulatorChoice::Keep, EmulatorChoice::Chosen),
        mtime,
    )
}

/// `tree` with its asset packs decoded as [`open`] decodes it, and the AMPR emulator left out:
/// every `libSceAmpr.sprx` (and `.orig` copy) in its `fakelib` and `fakelib2`, so nothing is
/// fetched. A game that reads through libSceAmpr still gets a fresh `ampr_emu.index`, as every
/// package of such a game carries one; any other game gets none. For a package only: an image
/// or folder is played from ShadowMount+'s mount, where some libSceAmpr games need an emulator.
pub fn open_without_emulator(
    tree: Box<dyn SourceTree>,
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    unpack(tree, EmulatorChoice::LeaveOut, mtime)
}

/// How [`open_with_own_emulator`]'s refusal starts: the game reads through libSceAmpr and the
/// emulator it has (or its having none) is one the rule replaces, and nothing can be fetched to
/// replace it with.
pub const NEEDS_OTHER_EMULATOR: &str = "the game's AMPR emulator can't stay beside plain files";

/// `tree` with its asset packs decoded as [`open`] decodes it, and the game's own emulator kept
/// where the unpack rule keeps it: an ampr_emu as new as the one this app installs, or whatever a
/// game that doesn't read through libSceAmpr has. Where the rule would add or replace one (a
/// libSceAmpr game with ampr-emu-ng, an older ampr_emu, a build this app can't name, or none),
/// the unpack is refused with an error that starts with [`NEEDS_OTHER_EMULATOR`]: nothing is
/// fetched here, and plain files are never left beside an emulator the rule does not keep. For
/// the converter on the PS5; there the user leaves the emulator out
/// ([`open_without_emulator`]) or makes the package on a computer.
pub fn open_with_own_emulator(
    tree: Box<dyn SourceTree>,
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    unpack(tree, EmulatorChoice::Own, mtime)
}

/// The folder, in the converter's job folder (`/data/ps5upload/convert` on the PS5), that holds
/// the emulators the desktop app puts on the console: ampr-emu-ng for zstd packs, and, when the
/// user asks for it, ampr_emu for unpacked asset packs ([`console_ampr_emu_folder`]).
pub const CONSOLE_RUNTIME_FOLDER: &str = "runtime";
/// The emulator's file in its folder there.
pub const CONSOLE_EMULATOR_FILE: &str = "libSceAmpr.sprx";
/// The largest file there read as an emulator (ampr_emu is about 1 MiB).
const CONSOLE_EMULATOR_MAX: u64 = 16 << 20;

/// ampr_emu `version`'s folder in [`CONSOLE_RUNTIME_FOLDER`]: its name is the version marker the
/// converter checks the file's own version against.
pub fn console_ampr_emu_folder(version: &str) -> String {
    format!("ampr_emu-{version}")
}

/// The ampr_emu the app put on the console, as the converter there finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleEmulator {
    pub version: String,
    pub bytes: Vec<u8>,
}

/// The newest usable ampr_emu in `runtime_dir` ([`CONSOLE_RUNTIME_FOLDER`]): a
/// `ampr_emu-<version>/libSceAmpr.sprx` whose module says it is ampr_emu of that same version,
/// as new as [`RUNTIME_VERSION`] or newer. None when there is none (nothing was put there, or
/// what is there is damaged or another build).
pub fn console_ampr_emu(runtime_dir: &std::path::Path) -> Option<ConsoleEmulator> {
    let mut best: Option<ConsoleEmulator> = None;
    for entry in std::fs::read_dir(runtime_dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(version) = name
            .to_str()
            .and_then(|n| n.strip_prefix("ampr_emu-"))
            .filter(|v| {
                !v.is_empty()
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            })
        else {
            continue;
        };
        if ng::older(version, RUNTIME_VERSION)
            || best
                .as_ref()
                .is_some_and(|b| !ng::older(&b.version, version))
        {
            continue;
        }
        let file = entry.path().join(CONSOLE_EMULATOR_FILE);
        let size = std::fs::metadata(&file).map_or(0, |m| m.len());
        if size == 0 || size > CONSOLE_EMULATOR_MAX {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        match ng::classify(&bytes) {
            ("ampr_emu", Some(v), _) if v == version => {
                best = Some(ConsoleEmulator {
                    version: version.to_string(),
                    bytes,
                });
            }
            (kind, v, _) => crate::log_at!(
                Warn,
                "convert",
                "{} is {kind} {}, not ampr_emu {version}: not used",
                file.display(),
                v.unwrap_or_default()
            ),
        }
    }
    best
}

/// `tree` with its asset packs decoded as the desktop app decodes them with "Add the AMPR
/// emulator" on and ampr_emu chosen: [`open`] with `emulator` (ampr_emu from the console's own
/// disk, [`console_ampr_emu`]) as the one that goes in where the game's own has to go. For the
/// converter on the PS5; the package it writes holds the same files as the desktop app's.
pub fn open_with_console_emulator(
    tree: Box<dyn SourceTree>,
    emulator: &[u8],
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    let fetch = || Ok(emulator.to_vec());
    open(
        tree,
        Some(&Emulator {
            ng: false,
            fetch: &fetch,
        }),
        mtime,
    )
}

/// What an unpack of a game would be, read from its pack indexes, its eboot's imports and its
/// emulator: never the packed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assessment {
    /// The sum of the plain files' sizes (the packs, their indexes, the old `ampr_emu.index`
    /// and a run's journal and logs left out): about what the unpacked game holds.
    pub plain_bytes: u64,
    /// Whether [`open_with_own_emulator`] goes ahead: the game's own emulator stays under the
    /// unpack rule, so nothing has to be fetched.
    pub own_emulator_suits: bool,
}

/// [`Assessment`] of `tree`, or why it has no packs to decode (or they don't decode).
pub fn assess(tree: Box<dyn SourceTree>) -> Result<Assessment, String> {
    let game = decode(tree)?;
    let own = runtime_action(game.ampr, game.current_kind(), false);
    let plain_bytes = game
        .tree
        .files()
        .iter()
        .filter(|f| {
            !LEFT_OUT.iter().any(|n| f.path.eq_ignore_ascii_case(n))
                && !f.path.eq_ignore_ascii_case(ampr_index::NAME)
        })
        .fold(0u64, |sum, f| sum.saturating_add(f.size));
    Ok(Assessment {
        plain_bytes,
        own_emulator_suits: matches!(own, RuntimeAction::Untouched | RuntimeAction::Keep),
    })
}

/// A game with its packs decoded, and what it has of an emulator.
struct Decoded {
    tree: Box<dyn SourceTree>,
    /// The pack formats decoded: `LZ4`, `zstd`.
    kinds: Vec<&'static str>,
    /// Whether its eboot imports libSceAmpr.
    ampr: bool,
    /// Where its emulator is, when it has one.
    module: Option<String>,
    /// [`ng::classify`] of that emulator.
    current: Option<(&'static str, Option<String>, bool)>,
}

impl Decoded {
    /// The emulator's kind and version, as [`runtime_action`] takes them.
    fn current_kind(&self) -> Option<(&str, Option<&str>)> {
        self.current.as_ref().map(|c| (c.0, c.1.as_deref()))
    }
}

/// The emulator `c` ([`ng::classify`]) by name, for the log and for a refusal.
fn named(c: &(&'static str, Option<String>, bool)) -> String {
    match &c.1 {
        Some(v) => format!("{} {v}", c.0),
        None => "an emulator this app doesn't know".to_string(),
    }
}

fn decode(tree: Box<dyn SourceTree>) -> Result<Decoded, String> {
    let mut kinds = Vec::new();
    let mut tree = match Lz4Unpacked::open(tree).map_err(|e| format!("the LZ4 packs: {e}"))? {
        Unpacking::Decoded(decoded) => {
            kinds.push("LZ4");
            decoded as Box<dyn SourceTree>
        }
        Unpacking::NotPacked(plain) => plain,
    };
    if ps5_dump_forge_zstd::reader::detect(&*tree) {
        tree = Box::new(
            ps5_dump_forge_zstd::reader::unpack(tree)
                .map_err(|e| format!("the zstd packs: {e}"))?,
        );
        kinds.push("zstd");
    }
    if kinds.is_empty() {
        return Err(
            "this game is not in asset packs (it has no ampr_assets.index or ampr_zstd.index \
             pack set): there is nothing to unpack"
                .to_string(),
        );
    }
    let ampr = ps5upload_fpkg::source::imports_ampr(&mut *tree, "eboot.bin");
    let module = ng::module_path(&*tree);
    let current = match &module {
        Some(path) => Some(
            ps5upload_fpkg::source::read_small(&mut *tree, path, 64 << 20)
                .map(|bytes| ng::classify(&bytes))
                .map_err(|e| format!("{path}: {e}"))?,
        ),
        None => None,
    };
    Ok(Decoded {
        tree,
        kinds,
        ampr,
        module,
        current,
    })
}

fn unpack(
    tree: Box<dyn SourceTree>,
    choice: EmulatorChoice,
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    let game = decode(tree)?;
    let own = runtime_action(game.ampr, game.current_kind(), false);
    let Decoded {
        tree,
        kinds,
        ampr,
        module,
        current,
    } = game;
    if matches!(choice, EmulatorChoice::Own)
        && matches!(own, RuntimeAction::Install | RuntimeAction::Replace)
    {
        let has = match (&module, &current) {
            (Some(path), Some(c)) => format!("it has {} at {path}", named(c)),
            _ => "it has none".to_string(),
        };
        // The facts last: a long detail is shortened in the middle, and these are what a
        // report needs.
        return Err(format!(
            "{NEEDS_OTHER_EMULATOR}. Put the AMPR emulator on this PS5 from the app, leave it \
             out, or make the package with PS5Upload on a computer. The game reads its files \
             through AMPR and {has}; ampr_emu {RUNTIME_VERSION} or newer is needed, and none is \
             on this PS5"
        ));
    }
    let mut generated: HashMap<String, Vec<u8>> = HashMap::new();
    let mut replaced: Option<String> = None;
    let leave_out = matches!(choice, EmulatorChoice::LeaveOut);
    let action = match choice {
        EmulatorChoice::Chosen(e) => {
            runtime_action(ampr, current.as_ref().map(|c| (c.0, c.1.as_deref())), e.ng)
        }
        // The user turned emulators off (the game's own stays, whatever it is), the game's
        // own is one the rule keeps (checked above), or the package leaves them out: nothing
        // goes in.
        EmulatorChoice::Keep | EmulatorChoice::Own | EmulatorChoice::LeaveOut => {
            RuntimeAction::Untouched
        }
    };
    match (action, choice) {
        (RuntimeAction::Install, EmulatorChoice::Chosen(e)) => {
            let bytes = (e.fetch)().map_err(|why| {
                format!(
                    "this game reads its files through AMPR and has no emulator, and one could \
                     not be fetched: {why}. Connect to the internet, or turn off \"Add the AMPR \
                     emulator\" in Convert's options"
                )
            })?;
            let at = emulator_home(&*tree);
            crate::log_at!(
                Info,
                "convert",
                "unpack packs: the game has no emulator; one goes in as {at}"
            );
            generated.insert(at.clone(), bytes);
            replaced = Some(at);
        }
        (RuntimeAction::Replace, EmulatorChoice::Chosen(e)) => {
            let at = module.clone().unwrap_or_else(|| emulator_home(&*tree));
            let was = current.as_ref().map(named).unwrap_or_default();
            match (e.fetch)() {
                Ok(bytes) => {
                    crate::log_at!(
                        Info,
                        "convert",
                        "unpack packs: {was} at {at} is replaced by {}",
                        if e.ng { "ampr-emu-ng" } else { "ampr_emu" }
                    );
                    generated.insert(at.clone(), bytes);
                    replaced = Some(at);
                }
                // The game ran with the one it has: better kept than no game at all.
                Err(why) => crate::log_at!(
                    Warn,
                    "convert",
                    "unpack packs: {was} at {at} stays, the emulator to replace it with could \
                     not be fetched: {why}"
                ),
            }
        }
        _ => {
            if let (Some(path), Some(c)) = (&module, &current) {
                crate::log_at!(
                    Info,
                    "convert",
                    "unpack packs: {} at {path} {}",
                    named(c),
                    if leave_out { "is left out" } else { "stays" }
                );
            }
        }
    }
    let mut left_out = Vec::new();
    let mut files: Vec<SourceFile> = Vec::with_capacity(tree.files().len() + 2);
    for f in tree.files() {
        if LEFT_OUT.iter().any(|n| f.path.eq_ignore_ascii_case(n))
            || (leave_out && is_emulator_file(&f.path))
        {
            left_out.push(f.path.clone());
        } else if !f.path.eq_ignore_ascii_case(ampr_index::NAME)
            && !replaced
                .as_deref()
                .is_some_and(|m| f.path.eq_ignore_ascii_case(m))
        {
            files.push(f.clone());
        }
    }
    if let Some(at) = &replaced {
        files.push(SourceFile {
            path: at.clone(),
            size: generated.get(at).map_or(0, |b| b.len() as u64),
        });
    }
    // With no emulator left, only a libSceAmpr game keeps an index (the one a package of it
    // always carries).
    let indexed = !leave_out || ampr;
    if indexed {
        let index = ampr_index::build(&ampr_index::listing(&files), mtime)?;
        files.push(SourceFile {
            path: ampr_index::NAME.to_string(),
            size: index.len() as u64,
        });
        generated.insert(ampr_index::NAME.to_string(), index);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    crate::log_at!(
        Info,
        "convert",
        "unpack packs: {} packs decoded, {} plain files{}{}",
        kinds.join(" and "),
        files.len(),
        if indexed {
            format!(", a new {} lists them", ampr_index::NAME)
        } else {
            String::new()
        },
        if left_out.is_empty() {
            String::new()
        } else {
            format!("; left out {}", left_out.join(", "))
        }
    );
    Ok(Box::new(Plain {
        inner: tree,
        files,
        generated,
    }))
}

/// The ways only the converter on the PS5 uses. What the engine's builds make of an unpack (the
/// rule itself, a folder, an image, a package, the emulator fetched) is tested in
/// `ps5upload-engine`'s `unpack_packs`.
#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::FolderSource;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// A scratch folder that goes when the test ends, passed or failed.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = crate::test_tmp::temp_path(format!(
                "ps5upload-core-unpack-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A game whose eboot imports libSceAmpr (`ampr`) or not.
    fn game(root: &Path, ampr: bool) -> PathBuf {
        let src = root.join("PPSA99994-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data")).unwrap();
        let mut eboot = vec![0u8; 100_000];
        if ampr {
            eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        }
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99994\"}",
        )
        .unwrap();
        std::fs::write(src.join("data/table.bin"), "asset 7 ".repeat(60_000)).unwrap();
        src
    }

    /// `src` in zstd packs at `to` with `emulator` as its runtime (`None`: its emulator removed
    /// afterwards), and a journal as a run leaves it.
    fn zstd_packed(src: &Path, to: &Path, emulator: Option<&[u8]>) {
        let open = || -> Result<Box<dyn SourceTree>, String> {
            Ok(Box::new(
                FolderSource::open(src).map_err(|e| e.to_string())?,
            ))
        };
        let mut tree = open().unwrap();
        crate::zstd_packs::build_folder(
            3,
            None,
            emulator.unwrap_or(b"any"),
            &mut *tree,
            &open,
            to,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
            0,
        )
        .unwrap();
        if emulator.is_none() {
            std::fs::remove_file(to.join("fakelib/libSceAmpr.sprx")).unwrap();
        }
        std::fs::write(to.join("ampr_commands.bin"), b"journal").unwrap();
    }

    fn ampr_emu(version: &str) -> Vec<u8> {
        let mut m = format!("\0\0ampr_emu {version} (c) Drakmor\0").into_bytes();
        m.extend_from_slice(&[0u8; 32]);
        m
    }

    fn folder(path: &Path) -> Box<dyn SourceTree> {
        Box::new(FolderSource::open(path).unwrap())
    }

    /// A packed game to unpack: whether its eboot imports libSceAmpr, the emulator it has, and
    /// what the refusal names when its own emulator can't stay (`None`: it stays).
    struct Case<'a> {
        name: &'static str,
        ampr: bool,
        emulator: Option<&'a [u8]>,
        refused: Option<&'static str>,
    }

    /// Keeping the game's own emulator with nothing to fetch: it stays exactly where the unpack
    /// rule keeps it, beside a new index of the plain files; where the rule would add or
    /// replace one, the unpack is refused, and [`assess`] says which beforehand.
    #[test]
    fn the_games_own_emulator_stays_only_where_the_rule_keeps_it() {
        let ng: &[u8] = b"NG EMULATOR";
        let new = ampr_emu(RUNTIME_VERSION);
        let newer = ampr_emu("0.5");
        let old = ampr_emu("0.3.9");
        let case = |name, ampr, emulator, refused| Case {
            name,
            ampr,
            emulator,
            refused,
        };
        let cases = [
            case("ampr-new", true, Some(&new[..]), None),
            case("ampr-newer", true, Some(&newer[..]), None),
            case("ampr-old", true, Some(&old[..]), Some("ampr_emu 0.3.9")),
            case(
                "ampr-unknown",
                true,
                Some(ng),
                Some("an emulator this app doesn't know"),
            ),
            case("ampr-none", true, None, Some("it has none")),
            case("plain-unknown", false, Some(ng), None),
            case("plain-none", false, None, None),
        ];
        for Case {
            name,
            ampr,
            emulator,
            refused,
        } in cases
        {
            let scratch = Scratch::new(name);
            let src = game(&scratch.0, ampr);
            let packed = scratch.0.join("packed");
            zstd_packed(&src, &packed, emulator);

            let seen = assess(folder(&packed)).unwrap();
            assert_eq!(seen.own_emulator_suits, refused.is_none(), "{name}");
            // The plain files and the emulator; not the packs, their index, the old
            // ampr_emu.index or the journal.
            let plain: u64 = FolderSource::open(&src)
                .unwrap()
                .files()
                .iter()
                .map(|f| f.size)
                .sum();
            let emulator_bytes = emulator.map_or(0, |e| e.len() as u64);
            assert_eq!(seen.plain_bytes, plain + emulator_bytes, "{name}");

            let opened = open_with_own_emulator(folder(&packed), 7);
            match refused {
                Some(what) => {
                    let err = opened.err().unwrap();
                    assert!(err.starts_with(NEEDS_OTHER_EMULATOR), "{name}: {err}");
                    assert!(err.contains(what), "{name}: {err}");
                }
                None => {
                    let mut tree = opened.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(
                        tree.read("data/table.bin").unwrap(),
                        std::fs::read(src.join("data/table.bin")).unwrap(),
                        "{name}"
                    );
                    match emulator {
                        Some(bytes) => {
                            assert_eq!(tree.read("fakelib/libSceAmpr.sprx").unwrap(), bytes)
                        }
                        None => assert!(!tree.files().iter().any(|f| is_emulator_file(&f.path))),
                    }
                    // The new index lists what the tree gives, not what was inside the packs.
                    let listed = ampr_index::parse(&tree.read(ampr_index::NAME).unwrap()).unwrap();
                    let mut got: Vec<(String, u64)> =
                        listed.into_iter().map(|e| (e.path, e.size)).collect();
                    let mut want: Vec<(String, u64)> = tree
                        .files()
                        .iter()
                        .filter(|f| f.path != ampr_index::NAME)
                        .map(|f| (f.path.clone(), f.size))
                        .collect();
                    got.sort();
                    want.sort();
                    assert_eq!(got, want, "{name}");
                    assert!(
                        !want.iter().any(|(p, _)| p == "ampr_commands.bin"),
                        "{name}"
                    );
                    // What the same game gets with emulators turned off, where its own stays
                    // whatever it is: the same files.
                    let as_kept = open(folder(&packed), None, 7).unwrap();
                    assert_eq!(tree.files(), as_kept.files(), "{name}");
                }
            }
            // Left out, any of them unpacks.
            let bare = open_without_emulator(folder(&packed), 7).unwrap();
            assert!(!bare.files().iter().any(|f| is_emulator_file(&f.path)));
        }
    }

    /// What the app put on the console: only an ampr_emu whose module says the version its
    /// folder is named after, new enough for plain files, is used; the newest of them.
    #[test]
    fn the_console_emulator_is_the_newest_usable_ampr_emu_in_the_runtime_folder() {
        let scratch = Scratch::new("console-emu");
        let dir = scratch.0.join(CONSOLE_RUNTIME_FOLDER);
        assert_eq!(console_ampr_emu(&dir), None, "no folder");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(console_ampr_emu(&dir), None, "an empty folder");
        let put = |folder: &str, bytes: &[u8]| {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
            std::fs::write(dir.join(folder).join(CONSOLE_EMULATOR_FILE), bytes).unwrap();
        };
        // The zstd runtime beside it, an old ampr_emu, one whose module names another
        // version, another build under ampr_emu's name, an empty file: none of them.
        put("ampr-emu-ng-0.4.1", b"NG EMULATOR");
        put(&console_ampr_emu_folder("0.3.9"), &ampr_emu("0.3.9"));
        put(&console_ampr_emu_folder("0.5"), &ampr_emu("0.4.9"));
        put(&console_ampr_emu_folder("0.6"), b"NG EMULATOR");
        put(&console_ampr_emu_folder("0.7"), b"");
        std::fs::create_dir_all(dir.join(console_ampr_emu_folder("0.8"))).unwrap();
        assert_eq!(console_ampr_emu(&dir), None);

        let current = ampr_emu(RUNTIME_VERSION);
        put(&console_ampr_emu_folder(RUNTIME_VERSION), &current);
        assert_eq!(
            console_ampr_emu(&dir),
            Some(ConsoleEmulator {
                version: RUNTIME_VERSION.into(),
                bytes: current.clone()
            })
        );
        let newer = ampr_emu("0.4.3");
        put(&console_ampr_emu_folder("0.4.3"), &newer);
        assert_eq!(console_ampr_emu(&dir).unwrap().bytes, newer);
        assert_eq!(console_ampr_emu_folder("0.4.3"), "ampr_emu-0.4.3");
    }

    /// With ampr_emu on the console, every packed game unpacks exactly as the engine unpacks it
    /// with the same ampr_emu fetched: the same files, the same bytes, the same index.
    #[test]
    fn the_console_emulator_unpacks_as_the_engine_does() {
        let new = ampr_emu(RUNTIME_VERSION);
        let old = ampr_emu("0.3.9");
        let ng: &[u8] = b"NG EMULATOR";
        for (name, ampr, emulator) in [
            ("c-ampr-ng", true, Some(ng)),
            ("c-ampr-old", true, Some(&old[..])),
            ("c-ampr-none", true, None),
            ("c-ampr-new", true, Some(&new[..])),
            ("c-plain-ng", false, Some(ng)),
        ] {
            let scratch = Scratch::new(name);
            let src = game(&scratch.0, ampr);
            let packed = scratch.0.join("packed");
            zstd_packed(&src, &packed, emulator);
            let mut console = open_with_console_emulator(folder(&packed), &new, 7).unwrap();
            let fetch = || Ok(new.clone());
            let engine = Emulator {
                ng: false,
                fetch: &fetch,
            };
            let mut desktop = open(folder(&packed), Some(&engine), 7).unwrap();
            assert_eq!(console.files(), desktop.files(), "{name}");
            for f in desktop.files().to_vec() {
                assert_eq!(
                    console.read(&f.path).unwrap(),
                    desktop.read(&f.path).unwrap(),
                    "{name}: {}",
                    f.path
                );
            }
            if ampr {
                assert_eq!(
                    console.read("fakelib/libSceAmpr.sprx").unwrap(),
                    new,
                    "{name}: ampr_emu goes in"
                );
            }
        }
    }

    #[test]
    fn a_game_that_is_not_in_packs_has_nothing_to_assess() {
        let scratch = Scratch::new("plain");
        let src = game(&scratch.0, true);
        for err in [
            assess(folder(&src)).err().unwrap(),
            open_with_own_emulator(folder(&src), 7).err().unwrap(),
        ] {
            assert!(err.contains("nothing to unpack"), "{err}");
            assert!(!err.starts_with(NEEDS_OTHER_EMULATOR), "{err}");
        }
    }
}
