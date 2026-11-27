//! "Something wrong?" on the game page: the usual reasons a game does not show on the PS5's home
//! screen or does not start, checked in order, each one `pass`, `problem` or `unknown` (the app
//! could not tell). Read-only: every fact comes from a helper read, and nothing on the console
//! changes.
//!
//! The checks, in the order they are shown:
//!   1. `fake_package`: a game installed from a fake package needs kstuff, and on firmware above
//!      11.60 kstuff 1.15 or newer. The helper sees kstuff only as a process name and never its
//!      version, so this is a hint: a running kstuff on such firmware is `unknown`, not `pass`.
//!   2. `mount_layer`: a game run from a disk image needs ShadowMount+ to mount it; a registered
//!      folder needs its folder attached (now, or by ShadowMount+ when the game starts).
//!   3. `package_size`: a fake package past its firmware's size ceiling installs and never starts
//!      (`ps5upload_fpkg::package_limit`, the Workshop's rule).
//!   4. `database`: the game is in the PS5's app.db (the home screen's list); one that is not has
//!      lost its tile (Rebuild Database drops fake packages, #961).
//!   5. `permissions`: a game folder whose files or folders other users can't read or run. The
//!      PS5 starts a game as a user of its own, and refuses one whose files lack execute for
//!      everyone (CE-107750-0, payload/src/main.c). Packages and disk images carry their own modes
//!      (exFAT shows every file as 0777, a `.ffpkg` ships 0555), so only folders are checked.
//!   6. `free_space`: the console's own storage nearly full (saves and system data go there).

use axum::{extract::Query, http::StatusCode, response::IntoResponse, response::Response, Json};
use ps5upload_core::kraken::Mounter;
use ps5upload_fpkg::package_limit::{self, Firmware, ParamSizes};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const GIB: u64 = 1 << 30;

/// The newest firmware where any kstuff that loads fake packages starts a PS5 game.
const OLD_KSTUFF_LAST: Firmware = Firmware::new(11, 60);
/// The newest firmware a fake PS5 game is known to start on (kstuff 1.15).
const KNOWN_LAST: Firmware = Firmware::new(13, 60);
/// Below this much free on the console's own storage a game can't write its save: a problem.
const INTERNAL_FULL: u64 = GIB;
/// Below this, low: how much a game needs there is not known, so it is only "can't tell".
const INTERNAL_LOW: u64 = 5 * GIB;
/// The most entries of a game folder looked at (its top level and `sce_module`).
const MAX_ENTRIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Pass,
    Problem,
    /// The app can't tell from what it could read.
    Unknown,
}

/// How the game is on the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    /// Installed from a package (`/user/app/<id>/app.pkg`).
    Package,
    /// A game folder registered on the console.
    Folder,
    /// Registered from a disk image (`.exfat`, `.ffpkg`, ...).
    Image,
    /// None of these was found (or the helper could not say).
    Unknown,
}

/// One check's verdict. `reason` is a stable code (the app words each one itself; `message` is
/// the English), `params` the values the words use, `fix` what the app can do about it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DoctorCheck {
    pub id: &'static str,
    pub status: DoctorStatus,
    pub reason: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// `fix_permissions` (make the folder 0777), `restore` (Restore to home screen),
    /// `register` (register the game again), `start_shadowmount` (load ShadowMount+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<&'static str>,
}

fn check(
    id: &'static str,
    status: DoctorStatus,
    reason: &'static str,
    message: String,
) -> DoctorCheck {
    DoctorCheck {
        id,
        status,
        reason,
        message,
        params: Map::new(),
        fix: None,
    }
}

impl DoctorCheck {
    fn param(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.params.insert(key.to_string(), value.into());
        self
    }
    fn fix(mut self, fix: &'static str) -> Self {
        self.fix = Some(fix);
        self
    }
}

/// What a look at a game folder's modes found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderModes {
    /// Not looked at (not a folder) or the folder itself could not be read.
    NotRead,
    /// The folder has no `eboot.bin`.
    NoEboot,
    /// `checked` entries looked at; `bad` the ones other users can't read (or run), with mode.
    Read {
        checked: usize,
        bad: Vec<(String, u32)>,
    },
}

/// Everything the checks decide from. `None` is "could not be read".
#[derive(Debug, Clone)]
pub struct Facts {
    pub kind: GameKind,
    /// The folder or image a registered game runs from.
    pub source: Option<String>,
    pub firmware: Option<Firmware>,
    /// The title is a PS5 game (`PPSA`): the 11.60 / kstuff 1.15 rule is about those.
    pub ps5_game: bool,
    /// Whether the helper has kernel access (it reads the firmware and the process list with it).
    pub kernel_access: Option<bool>,
    /// The console's process names, lowercased.
    pub procs: Option<Vec<String>>,
    /// A package install's header: `fake`, `retail` or `ps4`.
    pub pkg_kind: Option<String>,
    pub pkg_size: Option<u64>,
    /// What the game's param.json reserves; `None` when it could not be read.
    pub param_sizes: Option<ParamSizes>,
    pub mounter: Mounter,
    /// A folder game's folder is attached at `/system_ex/app/<id>` now.
    pub attached: Option<bool>,
    pub in_appdb: Option<bool>,
    /// The helper's Restore check state (#961), for a package that is not on the home screen.
    pub restore_state: Option<String>,
    pub folder_modes: FolderModes,
    /// Usable bytes on the console's own storage (`/user`).
    pub free_internal: Option<u64>,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            kind: GameKind::Unknown,
            source: None,
            firmware: None,
            ps5_game: false,
            kernel_access: None,
            procs: None,
            pkg_kind: None,
            pkg_size: None,
            param_sizes: None,
            mounter: Mounter::Unknown,
            attached: None,
            in_appdb: None,
            restore_state: None,
            folder_modes: FolderModes::NotRead,
            free_internal: None,
        }
    }
}

/// Every check, in order.
pub fn evaluate(f: &Facts) -> Vec<DoctorCheck> {
    vec![
        fake_package(f),
        mount_layer(f),
        package_size(f),
        database(f),
        permissions(f),
        free_space(f),
    ]
}

fn fake_package(f: &Facts) -> DoctorCheck {
    const ID: &str = "fake_package";
    if f.kind != GameKind::Package {
        return check(
            ID,
            DoctorStatus::Pass,
            "not_a_package",
            "Not installed from a package, so fake-package support doesn't matter for it.".into(),
        );
    }
    if f.pkg_kind.as_deref() == Some("retail") {
        return check(
            ID,
            DoctorStatus::Pass,
            "retail",
            "Installed from a retail package, not a fake one.".into(),
        );
    }
    let Some(fw) = f.firmware else {
        return check(
            ID,
            DoctorStatus::Unknown,
            "firmware_unknown",
            "The console's firmware could not be read, so the app can't tell which kstuff it \
             needs."
                .into(),
        );
    };
    let fw_text = fw.to_string();
    let Some(procs) = &f.procs else {
        let (reason, why) = if f.kernel_access == Some(false) {
            (
                "no_kernel_access",
                "The helper has no kernel access, so it can't see whether kstuff is running.",
            )
        } else {
            (
                "procs_unknown",
                "The console's process list could not be read, so the app can't tell whether \
                 kstuff is running.",
            )
        };
        return check(ID, DoctorStatus::Unknown, reason, why.into()).param("firmware", fw_text);
    };
    let kstuff = procs.iter().any(|p| p.contains("kstuff"));
    if !kstuff {
        let (reason, need) = if f.ps5_game && fw > OLD_KSTUFF_LAST {
            (
                "kstuff_not_running_new",
                " (on this firmware, kstuff 1.15 or newer)",
            )
        } else {
            ("kstuff_not_running", "")
        };
        return check(
            ID,
            DoctorStatus::Problem,
            reason,
            format!(
                "No kstuff was found running. A game installed from a fake package starts only \
                 with kstuff loaded{need}. Load it, then start the game again. If your \
                 jailbreak runs it under another name, this can be wrong."
            ),
        )
        .param("firmware", fw_text);
    }
    if !f.ps5_game || fw <= OLD_KSTUFF_LAST {
        return check(
            ID,
            DoctorStatus::Pass,
            "kstuff_running",
            "kstuff is running.".into(),
        )
        .param("firmware", fw_text);
    }
    if fw > KNOWN_LAST {
        return check(
            ID,
            DoctorStatus::Unknown,
            "firmware_newer_than_known",
            format!(
                "kstuff is running, but no kstuff is known to start fake PS5 games on firmware \
                 {fw_text} (the newest known is {KNOWN_LAST})."
            ),
        )
        .param("firmware", fw_text)
        .param("known", KNOWN_LAST.to_string());
    }
    check(
        ID,
        DoctorStatus::Unknown,
        "kstuff_version_unknown",
        format!(
            "kstuff is running. On firmware {fw_text} a fake PS5 game needs kstuff 1.15 or newer, \
             and the app can't read which version is loaded. If the game stays on its loading \
             screen, load the newest kstuff."
        ),
    )
    .param("firmware", fw_text)
}

fn mounter_name(m: Mounter) -> &'static str {
    match m {
        Mounter::Shadowmount => "ShadowMount+",
        Mounter::Kraken => "Kraken Mount",
        Mounter::Both => "ShadowMount+ and Kraken Mount",
        Mounter::None | Mounter::Unknown => "",
    }
}

fn mount_layer(f: &Facts) -> DoctorCheck {
    const ID: &str = "mount_layer";
    match f.kind {
        GameKind::Package => check(
            ID,
            DoctorStatus::Pass,
            "not_needed",
            "Installed from a package: nothing needs to mount it.".into(),
        ),
        GameKind::Unknown => check(
            ID,
            DoctorStatus::Unknown,
            "kind_unknown",
            "The app could not tell how this game is on the console.".into(),
        ),
        GameKind::Image => match f.mounter {
            Mounter::Shadowmount | Mounter::Both => check(
                ID,
                DoctorStatus::Pass,
                "mounter_running",
                format!(
                    "{} is running to mount the game's disk image.",
                    mounter_name(f.mounter)
                ),
            )
            .param("mounter", mounter_name(f.mounter)),
            Mounter::Kraken => check(
                ID,
                DoctorStatus::Problem,
                "kraken_no_images",
                "This game runs from a disk image, and Kraken Mount, which is running, does not \
                 mount disk images. Stop Kraken Mount and start ShadowMount+."
                    .into(),
            ),
            Mounter::None => check(
                ID,
                DoctorStatus::Problem,
                "no_mounter",
                "This game runs from a disk image, and nothing that mounts disk images is \
                 running. Start ShadowMount+, then start the game again."
                    .into(),
            )
            .fix("start_shadowmount"),
            Mounter::Unknown => check(
                ID,
                DoctorStatus::Unknown,
                "mounter_unknown",
                "The app could not tell whether ShadowMount+ is running.".into(),
            ),
        },
        GameKind::Folder => {
            if f.attached == Some(true) {
                return check(
                    ID,
                    DoctorStatus::Pass,
                    "attached",
                    "The game's folder is attached.".into(),
                );
            }
            match (f.mounter, f.attached) {
                (Mounter::Shadowmount | Mounter::Both | Mounter::Kraken, _) => check(
                    ID,
                    DoctorStatus::Pass,
                    "mounter_attaches",
                    format!(
                        "{} is running to attach the game's folder when it starts.",
                        mounter_name(f.mounter)
                    ),
                )
                .param("mounter", mounter_name(f.mounter)),
                (Mounter::None, Some(false)) => check(
                    ID,
                    DoctorStatus::Problem,
                    "folder_not_attached",
                    "The game's folder is not attached, and nothing that attaches it when the \
                     game starts is running. Start ShadowMount+, or register the folder again."
                        .into(),
                )
                .fix("register"),
                _ => check(
                    ID,
                    DoctorStatus::Unknown,
                    "attach_unknown",
                    "The app could not tell whether the game's folder is attached.".into(),
                ),
            }
        }
    }
}

fn package_size(f: &Facts) -> DoctorCheck {
    const ID: &str = "package_size";
    if f.kind != GameKind::Package {
        return check(
            ID,
            DoctorStatus::Pass,
            "not_a_package",
            "Not a package: no package size limit applies.".into(),
        );
    }
    if f.pkg_kind.as_deref() == Some("retail") {
        return check(
            ID,
            DoctorStatus::Pass,
            "retail",
            "A retail package: the fake-package size limit does not apply.".into(),
        );
    }
    let Some(size) = f.pkg_size else {
        return check(
            ID,
            DoctorStatus::Unknown,
            "size_unknown",
            "The package's size could not be read.".into(),
        );
    };
    let Some(fw) = f.firmware else {
        return check(
            ID,
            DoctorStatus::Unknown,
            "firmware_unknown",
            "The console's firmware could not be read, so the app can't tell its size limit."
                .into(),
        )
        .param("size_gib", package_limit::gib(size));
    };
    let sizes = f.param_sizes.unwrap_or_default();
    let Some(limit) = package_limit::limit_for(fw, &sizes) else {
        return check(
            ID,
            DoctorStatus::Pass,
            "no_limit",
            format!("Firmware {fw} checks no package size."),
        )
        .param("firmware", fw.to_string());
    };
    let over = size > limit.max_package;
    let base = |status, reason, message| {
        check(ID, status, reason, message)
            .param("size_gib", package_limit::gib(size))
            .param("limit_gib", package_limit::gib(limit.max_package))
            .param("firmware", fw.to_string())
    };
    if over {
        // Unread sizes count as nothing reserved; on 11.xx a game may raise the ceiling to 320 GiB.
        if f.param_sizes.is_none() && limit.band == package_limit::Band::Fw1100 && size <= 320 * GIB
        {
            return base(
                DoctorStatus::Unknown,
                "may_be_over_profile",
                format!(
                    "The package is {} GiB. Firmware {fw} opens fake packages up to 256 GiB, or up \
                     to 320 GiB for a game that asks for it, and the game's settings could not be \
                     read.",
                    package_limit::gib(size)
                ),
            );
        }
        let size_gib = package_limit::gib(size);
        let limit_gib = package_limit::gib(limit.max_package);
        let message = if limit.reserved > 0 {
            format!(
                "The package is {size_gib} GiB. Firmware {fw} opens a fake package only up to {} \
                 GiB including the add-on space the game reserves ({} GiB here), so this one can \
                 be {limit_gib} GiB at most. It installs but never starts. Keep the game as a \
                 disk image instead, or use a smaller package.",
                package_limit::gib(limit.cap),
                package_limit::gib(limit.reserved)
            )
        } else {
            format!(
                "The package is {size_gib} GiB, and firmware {fw} can't open a fake package larger \
                 than {limit_gib} GiB. It installs but never starts. Keep the game as a disk image \
                 instead, or use a smaller package."
            )
        };
        let reason = if limit.reserved > 0 {
            "over_limit_reserved"
        } else {
            "over_limit"
        };
        return base(DoctorStatus::Problem, reason, message)
            .param("reserved_gib", package_limit::gib(limit.reserved))
            .param("cap_gib", package_limit::gib(limit.cap));
    }
    // Fits with nothing reserved: on 13.x a game's add-on space counts too, unknown when unread.
    if f.param_sizes.is_none() && limit.band == package_limit::Band::Fw1300 && size > 256 * GIB {
        return base(
            DoctorStatus::Unknown,
            "may_be_over",
            format!(
                "The package is {} GiB. Firmware {fw} opens fake packages up to 450 GiB including \
                 the add-on space the game reserves, and the game's settings could not be read.",
                package_limit::gib(size)
            ),
        );
    }
    base(
        DoctorStatus::Pass,
        "fits",
        format!(
            "The package is {} GiB, within the {} GiB firmware {fw} opens.",
            package_limit::gib(size),
            package_limit::gib(limit.max_package)
        ),
    )
}

fn database(f: &Facts) -> DoctorCheck {
    const ID: &str = "database";
    match f.in_appdb {
        Some(true) => check(
            ID,
            DoctorStatus::Pass,
            "listed",
            "The PS5's database lists the game.".into(),
        ),
        None => check(
            ID,
            DoctorStatus::Unknown,
            "db_unreadable",
            "The PS5's database could not be read in full.".into(),
        ),
        Some(false) => match f.kind {
            GameKind::Package => {
                let restorable =
                    matches!(f.restore_state.as_deref(), Some("restorable" | "pending"));
                let c = check(
                    ID,
                    DoctorStatus::Problem,
                    if restorable {
                        "missing_restorable"
                    } else {
                        "missing_package"
                    },
                    if restorable {
                        "The game's files are on the console, but the PS5's database doesn't list \
                         it, so it has no home-screen tile. Use Restore to home screen on this \
                         page."
                            .into()
                    } else {
                        "The game's files are on the console, but the PS5's database doesn't list \
                         it, so it has no home-screen tile. Install its package again."
                            .into()
                    },
                );
                match (&f.restore_state, restorable) {
                    (Some(s), _) => {
                        let c = c.param("restore_state", s.clone());
                        if restorable {
                            c.fix("restore")
                        } else {
                            c
                        }
                    }
                    (None, _) => c,
                }
            }
            GameKind::Folder | GameKind::Image => check(
                ID,
                DoctorStatus::Problem,
                "missing_registration",
                "The PS5's database doesn't list this game, so it has no home-screen tile. \
                 Register it again."
                    .into(),
            )
            .fix("register"),
            GameKind::Unknown => check(
                ID,
                DoctorStatus::Problem,
                "not_on_console",
                "The PS5's database doesn't list this game, and the app found no package or \
                 registered folder of it on the console."
                    .into(),
            ),
        },
    }
}

fn permissions(f: &Facts) -> DoctorCheck {
    const ID: &str = "permissions";
    match f.kind {
        GameKind::Package => {
            return check(
                ID,
                DoctorStatus::Pass,
                "in_package",
                "Installed from a package: its files are inside the package.".into(),
            )
        }
        GameKind::Image => {
            return check(
                ID,
                DoctorStatus::Pass,
                "in_image",
                "Runs from a disk image, which gives the console the access it needs.".into(),
            )
        }
        GameKind::Unknown => {
            return check(
                ID,
                DoctorStatus::Unknown,
                "kind_unknown",
                "No game folder of this title was found to check.".into(),
            )
        }
        GameKind::Folder => {}
    }
    match &f.folder_modes {
        FolderModes::NotRead => check(
            ID,
            DoctorStatus::Unknown,
            "not_read",
            "The game's folder could not be read.".into(),
        ),
        FolderModes::NoEboot => check(
            ID,
            DoctorStatus::Problem,
            "no_eboot",
            "The game's folder has no eboot.bin, so there is nothing to start. Send the game \
             again."
                .into(),
        ),
        FolderModes::Read { checked, bad } if bad.is_empty() => check(
            ID,
            DoctorStatus::Pass,
            "ok",
            format!("The {checked} files and folders checked can be read and run by the console."),
        )
        .param("checked", *checked as u64),
        FolderModes::Read { checked, bad } => {
            let (path, mode) = &bad[0];
            check(
                ID,
                DoctorStatus::Problem,
                "unreadable",
                format!(
                    "{} of the {checked} files and folders checked can't be read or run by the \
                     console (for example {path}, mode {mode:04o}). Fix permissions makes every \
                     file in the game's folder readable and runnable.",
                    bad.len()
                ),
            )
            .param("count", bad.len() as u64)
            .param("checked", *checked as u64)
            .param("path", path.clone())
            .param("mode", format!("{mode:04o}"))
            .fix("fix_permissions")
        }
    }
}

fn free_space(f: &Facts) -> DoctorCheck {
    const ID: &str = "free_space";
    let Some(free) = f.free_internal else {
        return check(
            ID,
            DoctorStatus::Unknown,
            "free_unknown",
            "The free space on the console's storage could not be read.".into(),
        );
    };
    let gb = format!("{:.1}", free as f64 / 1e9);
    if free < INTERNAL_FULL {
        return check(
            ID,
            DoctorStatus::Problem,
            "internal_full",
            format!(
                "Only {gb} GB is free on the console's storage. Games keep their saves and data \
                 there: free some space."
            ),
        )
        .param("free_gb", gb);
    }
    if free < INTERNAL_LOW {
        return check(
            ID,
            DoctorStatus::Unknown,
            "internal_low",
            format!(
                "Only {gb} GB is free on the console's storage. A game needs some room there for \
                 its saves; how much is not known."
            ),
        )
        .param("free_gb", gb);
    }
    check(
        ID,
        DoctorStatus::Pass,
        "ok",
        format!("{gb} GB is free on the console's storage."),
    )
    .param("free_gb", gb)
}

// ─── Reading the facts ──────────────────────────────────────────────────────

/// The firmware from the helper's kernel word (BCD: `0x13000000` is 13.00), else from the kernel
/// version text (`.../releases/13.00: ...`).
pub fn firmware_from(kernel_word: u32, os: &str) -> Option<Firmware> {
    let bcd = |b: u32| -> Option<u8> {
        let (hi, lo) = (b >> 4, b & 0xF);
        (hi < 10 && lo < 10).then_some((hi * 10 + lo) as u8)
    };
    if kernel_word != 0 {
        if let (Some(major), Some(minor)) = (
            bcd((kernel_word >> 24) & 0xFF),
            bcd((kernel_word >> 16) & 0xFF),
        ) {
            if major > 0 {
                return Some(Firmware::new(major, minor));
            }
        }
    }
    let text = os.split_once("releases/").map(|(_, r)| r).unwrap_or(os);
    // The first "NN.NN" in what is left.
    let b = text.as_bytes();
    for i in 0..b.len() {
        let major_len = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=2).contains(&major_len) || (i > 0 && b[i - 1].is_ascii_digit()) {
            continue;
        }
        let dot = i + major_len;
        if b.get(dot) != Some(&b'.') {
            continue;
        }
        let minor = &b[dot + 1..];
        if minor.len() >= 2 && minor[0].is_ascii_digit() && minor[1].is_ascii_digit() {
            if minor.get(2).is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            let major: u8 = text[i..dot].parse().ok()?;
            let minor: u8 = text[dot + 1..dot + 3].parse().ok()?;
            if major > 0 {
                return Some(Firmware::new(major, minor));
            }
        }
    }
    None
}

/// What a package's first bytes say: `fake` (a PS5 package signed for debug), `retail`, or `ps4`
/// (its header does not say), as the helper reads it (payload/src/title_restore.c).
pub fn pkg_kind(head: &[u8]) -> Option<&'static str> {
    if head.len() < 6 {
        return None;
    }
    if head[..4] == [0x7F, b'F', b'I', b'H'] {
        return Some(if head[5] == 0 { "fake" } else { "retail" });
    }
    if head[..4] == [0x7F, b'C', b'N', b'T'] {
        return Some("ps4");
    }
    None
}

/// Whether a game folder entry is fine for the console: folders and executables need read and
/// execute for everyone, other files read.
pub fn mode_ok(is_dir: bool, executable: bool, mode: u32) -> bool {
    let other = mode & 0o007;
    if is_dir || executable {
        other & 0o5 == 0o5
    } else {
        other & 0o4 != 0
    }
}

fn is_executable_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "eboot.bin" || n.ends_with(".prx") || n.ends_with(".sprx") || n.ends_with(".self")
}

/// A disk image a game is registered from.
pub fn is_image_source(src: &str, image_backed: bool) -> bool {
    let s = src.to_ascii_lowercase();
    image_backed
        || s.starts_with("/mnt/shadowmnt")
        || [".exfat", ".ffpkg", ".ffpfs", ".ffpfsc", ".img"]
            .iter()
            .any(|ext| s.ends_with(ext))
}

fn folder_modes(addr: &str, src: &str) -> FolderModes {
    use ps5upload_core::fs_ops::{fs_stat, is_not_found, list_dir, ListDirOptions};
    let src = src.trim_end_matches('/');
    let Ok(root) = fs_stat(addr, src) else {
        return FolderModes::NotRead;
    };
    if root.kind != "dir" {
        return FolderModes::NotRead;
    }
    match fs_stat(addr, &format!("{src}/eboot.bin")) {
        Err(e) if is_not_found(&format!("{e:#}")) => return FolderModes::NoEboot,
        _ => {}
    }
    let mut checked = 0usize;
    let mut bad = Vec::new();
    let mut look = |path: String, is_dir: bool, mode: u32| {
        checked += 1;
        if !mode_ok(is_dir, !is_dir && is_executable_name(&path), mode) {
            bad.push((path, mode & 0o7777));
        }
    };
    look(src.to_string(), true, root.mode);
    let opts = || ListDirOptions {
        offset: 0,
        limit: MAX_ENTRIES as u64,
    };
    // The top level (eboot.bin, sce_sys, sce_module, the data folders), then sce_sys/param.json
    // and every module.
    let mut paths: Vec<String> = Vec::new();
    if let Ok(list) = list_dir(addr, src, opts()) {
        paths.extend(list.entries.iter().map(|e| format!("{src}/{}", e.name)));
    }
    for sub in ["sce_module", "sce_sys"] {
        if let Ok(list) = list_dir(addr, &format!("{src}/{sub}"), opts()) {
            paths.extend(
                list.entries
                    .iter()
                    .filter(|e| sub == "sce_module" || e.name == "param.json")
                    .map(|e| format!("{src}/{sub}/{}", e.name)),
            );
        }
    }
    for path in paths.into_iter().take(MAX_ENTRIES * 2) {
        if let Ok(st) = fs_stat(addr, &path) {
            match st.kind.as_str() {
                "dir" => look(path, true, st.mode),
                "file" => look(path, false, st.mode),
                _ => {}
            }
        }
    }
    FolderModes::Read { checked, bad }
}

/// Reads every fact for `title_id` on the console at `addr` (the helper's address). Blocking;
/// each read that fails leaves its fact unknown.
pub fn gather(addr: &str, title_id: &str) -> Facts {
    use ps5upload_core::fs_ops::{app_list_registered, fs_exists, fs_read};
    let mut f = Facts {
        ps5_game: title_id.starts_with("PPSA"),
        ..Facts::default()
    };

    if let Ok(hw) = ps5upload_core::hw::hw_info(addr) {
        f.firmware = firmware_from(hw.kernel_fw_version, &hw.os);
        if hw.kernel_fw_version != 0 {
            f.kernel_access = Some(true);
        }
    }
    match ps5upload_core::hw::proc_list(addr) {
        Ok(list) if list.ok => {
            f.kernel_access = Some(true);
            f.procs = Some(
                list.procs
                    .iter()
                    .map(|p| p.name.to_ascii_lowercase())
                    .collect(),
            );
        }
        Ok(list) => {
            if list.error.as_deref() == Some("kernel_rw_unavailable") && f.kernel_access.is_none() {
                f.kernel_access = Some(false);
            }
        }
        Err(_) => {}
    }

    // Registered (a folder or image), else installed from a package.
    let registered = app_list_registered(addr)
        .ok()
        .map(|r| r.apps.into_iter().find(|a| a.title_id == title_id));
    let restore = ps5upload_core::title_restore::check(addr, title_id).ok();
    match registered.clone().flatten() {
        Some(app) if !app.src.is_empty() => {
            f.kind = if is_image_source(&app.src, app.image_backed) {
                GameKind::Image
            } else {
                GameKind::Folder
            };
            f.source = Some(app.src);
        }
        _ => {
            if let Some(r) = &restore {
                if r.pkg.is_some()
                    && !matches!(r.state.as_str(), "not_installed" | "folder_registration")
                {
                    f.kind = GameKind::Package;
                    f.pkg_size = r.size;
                    f.pkg_kind = r.kind.clone();
                    if f.pkg_kind.is_none() {
                        if let Some(pkg) = &r.pkg {
                            f.pkg_kind = fs_read(addr, pkg, 0, 8)
                                .ok()
                                .and_then(|h| pkg_kind(&h))
                                .map(str::to_string);
                        }
                    }
                    f.restore_state = Some(r.state.clone());
                }
            }
        }
    }

    if f.kind == GameKind::Package {
        for path in [
            format!("/user/appmeta/{title_id}/param.json"),
            format!("/user/app/{title_id}/sce_sys/param.json"),
        ] {
            if let Ok(bytes) = fs_read(addr, &path, 0, 256 * 1024) {
                if !bytes.is_empty() {
                    f.param_sizes = Some(package_limit::param_sizes(&bytes));
                    break;
                }
            }
        }
    }
    if matches!(f.kind, GameKind::Folder | GameKind::Image) {
        f.mounter = ps5upload_core::smp::collect_status(addr)
            .map(|s| s.mounter)
            .unwrap_or(Mounter::Unknown);
    }
    if f.kind == GameKind::Folder {
        f.attached = fs_exists(addr, &format!("/system_ex/app/{title_id}/eboot.bin")).ok();
        if let Some(src) = &f.source {
            f.folder_modes = folder_modes(addr, src);
        }
    }

    if let Ok(db) = ps5upload_core::diagnostics::appdb_query(addr) {
        let listed = db.apps.iter().any(|a| a.title_id == title_id);
        f.in_appdb = if listed {
            Some(true)
        } else if db.truncated || db.err.is_some() {
            None
        } else {
            Some(false)
        };
    }
    f.free_internal = ps5upload_core::volumes::free_space(addr, "/user")
        .ok()
        .map(|s| s.usable_bytes);
    f
}

#[derive(Debug, Deserialize)]
pub struct DoctorQuery {
    #[serde(alias = "console", alias = "host", alias = "ps5_addr")]
    pub addr: String,
    pub title_id: String,
}

/// `GET /api/ps5/app/doctor?addr=&title_id=`: "Something wrong?" for one game, read-only. Checks,
/// in order, the usual reasons a game does not show or start: `fake_package` (kstuff for a fake
/// package; a hint, since kstuff's version can't be read), `mount_layer` (ShadowMount+ for a disk
/// image, an attached folder for a folder game), `package_size` (a fake package past its
/// firmware's size ceiling), `database` (missing from the PS5's app.db), `permissions` (a game
/// folder's files other users can't read or run) and `free_space` (the console's own storage).
/// Each check has `status` `pass`, `problem` or `unknown` (the app can't tell), a `reason` code,
/// an English `message`, `params`, and a `fix` the app offers (`fix_permissions`, `restore`,
/// `register`, `start_shadowmount`). Also `kind` (`package`, `folder`, `image`, `unknown`),
/// `source`, `firmware` and `first_problem` (the first check that found one, else null).
pub async fn doctor_handler(Query(q): Query<DoctorQuery>) -> Response {
    let id = q.title_id.trim().to_ascii_uppercase();
    if !ps5upload_core::title_restore::valid_title_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "title_id is not a title id"})),
        )
            .into_response();
    }
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&q.addr);
    let title = id.clone();
    let facts = match tokio::task::spawn_blocking(move || gather(&mgmt, &title)).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    };
    let checks = evaluate(&facts);
    let first_problem = checks
        .iter()
        .find(|c| c.status == DoctorStatus::Problem)
        .map(|c| c.id);
    crate::log_info!(
        "app/doctor {id}: kind={:?} fw={:?} {}",
        facts.kind,
        facts.firmware.map(|f| f.to_string()),
        checks
            .iter()
            .map(|c| format!("{}={}", c.id, c.reason))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Json(json!({
        "ok": true,
        "title_id": id,
        "kind": facts.kind,
        "source": facts.source,
        "firmware": facts.firmware.map(|f| f.to_string()),
        "checks": checks,
        "first_problem": first_problem,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(fw: (u8, u8), procs: &[&str]) -> Facts {
        Facts {
            kind: GameKind::Package,
            firmware: Some(Firmware::new(fw.0, fw.1)),
            ps5_game: true,
            procs: Some(procs.iter().map(|s| s.to_string()).collect()),
            pkg_kind: Some("fake".into()),
            pkg_size: Some(50 * GIB),
            param_sizes: Some(ParamSizes::default()),
            in_appdb: Some(true),
            free_internal: Some(100 * GIB),
            ..Facts::default()
        }
    }

    fn folder(modes: FolderModes) -> Facts {
        Facts {
            kind: GameKind::Folder,
            source: Some("/data/homebrew/G".into()),
            firmware: Some(Firmware::new(13, 0)),
            mounter: Mounter::Shadowmount,
            attached: Some(false),
            in_appdb: Some(true),
            folder_modes: modes,
            free_internal: Some(100 * GIB),
            ..Facts::default()
        }
    }

    #[test]
    fn the_checks_come_in_order() {
        let ids: Vec<_> = evaluate(&package((9, 60), &["kstuff"]))
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(
            ids,
            [
                "fake_package",
                "mount_layer",
                "package_size",
                "database",
                "permissions",
                "free_space"
            ]
        );
    }

    #[test]
    fn a_healthy_package_on_old_firmware_passes_everything() {
        let checks = evaluate(&package((9, 60), &["kstuff.elf", "shellui"]));
        for c in &checks {
            assert_eq!(c.status, DoctorStatus::Pass, "{c:?}");
        }
    }

    /// The Discord report's console: FW 13.00 with kstuff running. kstuff's version can't be read,
    /// so it is a hint, never a pass.
    #[test]
    fn kstuff_on_firmware_above_11_60_is_a_hint_not_a_pass() {
        let c = fake_package(&package((13, 0), &["kstuff"]));
        assert_eq!(
            (c.status, c.reason),
            (DoctorStatus::Unknown, "kstuff_version_unknown")
        );
        assert!(c.message.contains("1.15"), "{}", c.message);
        let c = fake_package(&package((11, 60), &["kstuff"]));
        assert_eq!(c.status, DoctorStatus::Pass);
        // A PS4 package: the 11.60 rule is about PS5 games.
        let mut ps4 = package((13, 0), &["kstuff"]);
        ps4.ps5_game = false;
        assert_eq!(fake_package(&ps4).status, DoctorStatus::Pass);
        let c = fake_package(&package((14, 0), &["kstuff"]));
        assert_eq!(
            (c.status, c.reason),
            (DoctorStatus::Unknown, "firmware_newer_than_known")
        );
    }

    #[test]
    fn no_kstuff_with_a_fake_package_is_a_problem_and_unreadable_is_unknown() {
        let c = fake_package(&package((13, 0), &["shellui"]));
        assert_eq!(
            (c.status, c.reason),
            (DoctorStatus::Problem, "kstuff_not_running_new")
        );
        assert!(c.message.contains("1.15"));
        let c = fake_package(&package((9, 60), &[]));
        assert_eq!(c.reason, "kstuff_not_running");
        assert!(!c.message.contains("1.15"));

        let mut f = package((13, 0), &[]);
        f.procs = None;
        f.kernel_access = Some(false);
        let c = fake_package(&f);
        assert_eq!(
            (c.status, c.reason),
            (DoctorStatus::Unknown, "no_kernel_access")
        );
        assert!(c.message.contains("kernel access"));
        f.kernel_access = None;
        assert_eq!(fake_package(&f).reason, "procs_unknown");
        f.firmware = None;
        assert_eq!(fake_package(&f).reason, "firmware_unknown");
        // Retail and folder games don't need it.
        let mut r = package((13, 0), &[]);
        r.pkg_kind = Some("retail".into());
        assert_eq!(fake_package(&r).status, DoctorStatus::Pass);
        assert_eq!(
            fake_package(&folder(FolderModes::NotRead)).reason,
            "not_a_package"
        );
    }

    #[test]
    fn a_disk_image_needs_shadowmount() {
        let mut f = folder(FolderModes::NotRead);
        f.kind = GameKind::Image;
        for (m, status, reason) in [
            (Mounter::Shadowmount, DoctorStatus::Pass, "mounter_running"),
            (Mounter::Both, DoctorStatus::Pass, "mounter_running"),
            (Mounter::Kraken, DoctorStatus::Problem, "kraken_no_images"),
            (Mounter::None, DoctorStatus::Problem, "no_mounter"),
            (Mounter::Unknown, DoctorStatus::Unknown, "mounter_unknown"),
        ] {
            f.mounter = m;
            let c = mount_layer(&f);
            assert_eq!((c.status, c.reason), (status, reason), "{m:?}");
        }
        f.mounter = Mounter::None;
        assert_eq!(mount_layer(&f).fix, Some("start_shadowmount"));
    }

    #[test]
    fn a_folder_needs_to_be_attached_or_a_mounter_to_attach_it() {
        let mut f = folder(FolderModes::NotRead);
        f.mounter = Mounter::None;
        f.attached = Some(true);
        assert_eq!(mount_layer(&f).reason, "attached");
        f.attached = Some(false);
        let c = mount_layer(&f);
        assert_eq!(
            (c.status, c.reason),
            (DoctorStatus::Problem, "folder_not_attached")
        );
        f.attached = None;
        assert_eq!(mount_layer(&f).reason, "attach_unknown");
        f.mounter = Mounter::Shadowmount;
        assert_eq!(mount_layer(&f).reason, "mounter_attaches");
        assert_eq!(mount_layer(&package((13, 0), &[])).reason, "not_needed");
    }

    /// The Workshop's ceilings (package_limit.rs): 450 GiB less the add-on space on 13.x, 256 GiB
    /// on 7.xx to 10.xx.
    #[test]
    fn a_package_past_its_firmware_ceiling_is_a_problem() {
        let mut f = package((13, 0), &["kstuff"]);
        f.pkg_size = Some(460 * GIB);
        let c = package_size(&f);
        assert_eq!((c.status, c.reason), (DoctorStatus::Problem, "over_limit"));
        assert_eq!(c.params["limit_gib"], "450");
        f.pkg_size = Some(400 * GIB);
        assert_eq!(package_size(&f).reason, "fits");
        f.param_sizes = Some(ParamSizes {
            ac_gib: Some(100),
            app_gib: None,
        });
        let c = package_size(&f);
        assert_eq!(c.reason, "over_limit_reserved");
        assert_eq!(c.params["reserved_gib"], "100");
        // Unread settings: on 13.x a big package may be over; a small one fits whatever is reserved.
        f.param_sizes = None;
        assert_eq!(package_size(&f).reason, "may_be_over");
        f.pkg_size = Some(200 * GIB);
        assert_eq!(package_size(&f).reason, "fits");

        let mut old = package((9, 60), &["kstuff"]);
        old.pkg_size = Some(300 * GIB);
        assert_eq!(package_size(&old).reason, "over_limit");
        old.firmware = Some(Firmware::new(5, 10));
        assert_eq!(package_size(&old).reason, "no_limit");
        // 11.xx: 256 GiB, or up to 320 for a game that asks; unread is "can't tell" up to 320.
        let mut mid = package((12, 0), &["kstuff"]);
        mid.pkg_size = Some(300 * GIB);
        mid.param_sizes = None;
        assert_eq!(package_size(&mid).reason, "may_be_over_profile");
        mid.param_sizes = Some(ParamSizes {
            ac_gib: None,
            app_gib: Some(320),
        });
        assert_eq!(package_size(&mid).reason, "fits");
        mid.pkg_size = None;
        assert_eq!(package_size(&mid).status, DoctorStatus::Unknown);
        let mut r = package((13, 0), &[]);
        r.pkg_kind = Some("retail".into());
        r.pkg_size = Some(900 * GIB);
        assert_eq!(package_size(&r).status, DoctorStatus::Pass);
    }

    #[test]
    fn a_game_missing_from_the_database_says_how_to_bring_it_back() {
        let mut f = package((13, 0), &["kstuff"]);
        f.in_appdb = Some(false);
        f.restore_state = Some("restorable".into());
        let c = database(&f);
        assert_eq!(
            (c.status, c.reason, c.fix),
            (DoctorStatus::Problem, "missing_restorable", Some("restore"))
        );
        f.restore_state = Some("retail".into());
        let c = database(&f);
        assert_eq!((c.reason, c.fix), ("missing_package", None));
        let mut g = folder(FolderModes::NotRead);
        g.in_appdb = Some(false);
        let c = database(&g);
        assert_eq!(
            (c.reason, c.fix),
            ("missing_registration", Some("register"))
        );
        g.in_appdb = None;
        assert_eq!(database(&g).status, DoctorStatus::Unknown);
        let unknown = Facts {
            in_appdb: Some(false),
            ..Facts::default()
        };
        assert_eq!(database(&unknown).reason, "not_on_console");
    }

    /// 0644 files (a folder copied from Windows) and a 0700 folder are what the console can't use;
    /// 0555 (a .ffpkg's modes) and 0777 are fine.
    #[test]
    fn modes_other_users_cant_read_or_run_are_a_problem() {
        assert!(mode_ok(false, true, 0o100777));
        assert!(mode_ok(false, true, 0o555));
        assert!(!mode_ok(false, true, 0o644));
        assert!(mode_ok(false, false, 0o644));
        assert!(!mode_ok(false, false, 0o600));
        assert!(mode_ok(true, false, 0o755));
        assert!(!mode_ok(true, false, 0o700));

        let c = permissions(&folder(FolderModes::Read {
            checked: 7,
            bad: vec![("/data/homebrew/G/eboot.bin".into(), 0o644)],
        }));
        assert_eq!(
            (c.status, c.reason, c.fix),
            (DoctorStatus::Problem, "unreadable", Some("fix_permissions"))
        );
        assert_eq!(c.params["mode"], "0644");
        assert!(c.message.contains("eboot.bin"));
        let c = permissions(&folder(FolderModes::Read {
            checked: 7,
            bad: vec![],
        }));
        assert_eq!(c.status, DoctorStatus::Pass);
        assert_eq!(
            permissions(&folder(FolderModes::NoEboot)).reason,
            "no_eboot"
        );
        assert_eq!(
            permissions(&folder(FolderModes::NotRead)).status,
            DoctorStatus::Unknown
        );
        assert_eq!(
            permissions(&package((13, 0), &[])).status,
            DoctorStatus::Pass
        );
    }

    #[test]
    fn free_space_is_a_problem_only_when_nearly_full() {
        let mut f = package((13, 0), &["kstuff"]);
        f.free_internal = Some(GIB / 2);
        assert_eq!(free_space(&f).status, DoctorStatus::Problem);
        f.free_internal = Some(3 * GIB);
        assert_eq!(free_space(&f).status, DoctorStatus::Unknown);
        f.free_internal = Some(50 * GIB);
        assert_eq!(free_space(&f).status, DoctorStatus::Pass);
        f.free_internal = None;
        assert_eq!(free_space(&f).status, DoctorStatus::Unknown);
    }

    #[test]
    fn firmware_comes_from_the_kernel_word_or_the_version_text() {
        assert_eq!(firmware_from(0x1300_0000, ""), Some(Firmware::new(13, 0)));
        assert_eq!(firmware_from(0x0960_0010, ""), Some(Firmware::new(9, 60)));
        assert_eq!(
            firmware_from(
                0,
                "FreeBSD 11.0-RELEASE-p0 #1 r218215/releases/09.60: Jul 18 2023"
            ),
            Some(Firmware::new(9, 60))
        );
        assert_eq!(
            firmware_from(0, "FreeBSD 11.0-RELEASE-p0 #0 r218215/releases/13.00-00.00"),
            Some(Firmware::new(13, 0))
        );
        assert_eq!(firmware_from(0, ""), None);
        assert_eq!(firmware_from(0, "no version here"), None);
    }

    #[test]
    fn package_headers_and_image_sources_are_told_apart() {
        assert_eq!(pkg_kind(b"\x7FFIH\x00\x00\x00"), Some("fake"));
        assert_eq!(pkg_kind(b"\x7FFIH\x00\x01\x00"), Some("retail"));
        assert_eq!(pkg_kind(b"\x7FCNT\x00\x00"), Some("ps4"));
        assert_eq!(pkg_kind(b"PK"), None);
        assert!(is_image_source("/data/homebrew/G.exfat", false));
        assert!(is_image_source("/mnt/shadowmnt/G_1234abcd", false));
        assert!(is_image_source("/data/homebrew/G", true));
        assert!(!is_image_source("/data/homebrew/G", false));
    }

    #[test]
    fn the_answer_serializes_with_snake_case_codes() {
        let c = permissions(&folder(FolderModes::NoEboot));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["status"], "problem");
        assert_eq!(v["id"], "permissions");
        assert!(v.get("params").is_none());
        assert_eq!(serde_json::to_value(GameKind::Image).unwrap(), "image");
    }

    /// Facts that, between them, give every reason of every check.
    fn every_case() -> Vec<Facts> {
        let mut out = Vec::new();
        let pkg = |fw: (u8, u8), procs: &[&str]| package(fw, procs);
        // fake_package
        out.push(pkg((9, 60), &["kstuff"]));
        out.push(pkg((9, 60), &[]));
        out.push(pkg((13, 0), &[]));
        out.push(pkg((13, 0), &["kstuff"]));
        out.push(pkg((14, 0), &["kstuff"]));
        let mut f = pkg((13, 0), &[]);
        f.procs = None;
        out.push(f.clone());
        f.kernel_access = Some(false);
        out.push(f.clone());
        f.firmware = None;
        out.push(f.clone());
        f.pkg_size = None;
        out.push(f);
        let mut r = pkg((13, 0), &[]);
        r.pkg_kind = Some("retail".into());
        out.push(r);
        // package_size
        for (fw, size, sizes) in [
            ((13, 0), 460, Some(ParamSizes::default())),
            (
                (13, 0),
                400,
                Some(ParamSizes {
                    ac_gib: Some(100),
                    app_gib: None,
                }),
            ),
            ((13, 0), 400, None),
            ((12, 0), 300, None),
            ((5, 10), 300, None),
        ] {
            let mut f = pkg(fw, &["kstuff"]);
            f.pkg_size = Some(size * GIB);
            f.param_sizes = sizes;
            out.push(f);
        }
        // database
        for state in [Some("restorable"), Some("retail"), None] {
            let mut f = pkg((13, 0), &["kstuff"]);
            f.in_appdb = Some(false);
            f.restore_state = state.map(str::to_string);
            out.push(f);
        }
        let mut f = pkg((13, 0), &["kstuff"]);
        f.in_appdb = None;
        out.push(f);
        out.push(Facts {
            in_appdb: Some(false),
            ..Facts::default()
        });
        // mount_layer, permissions
        for m in [
            Mounter::Shadowmount,
            Mounter::Kraken,
            Mounter::None,
            Mounter::Unknown,
        ] {
            let mut f = folder(FolderModes::NotRead);
            f.kind = GameKind::Image;
            f.mounter = m;
            out.push(f);
        }
        for (m, attached) in [
            (Mounter::None, Some(true)),
            (Mounter::None, Some(false)),
            (Mounter::None, None),
            (Mounter::Shadowmount, Some(false)),
        ] {
            let mut f = folder(FolderModes::NoEboot);
            f.mounter = m;
            f.attached = attached;
            f.in_appdb = Some(false);
            out.push(f);
        }
        out.push(folder(FolderModes::NotRead));
        out.push(folder(FolderModes::Read {
            checked: 9,
            bad: vec![],
        }));
        out.push(folder(FolderModes::Read {
            checked: 9,
            bad: vec![("/data/homebrew/G/eboot.bin".into(), 0o644)],
        }));
        // free_space
        for free in [Some(GIB / 2), Some(3 * GIB), None] {
            let mut f = pkg((13, 0), &["kstuff"]);
            f.free_internal = free;
            out.push(f);
        }
        out
    }

    /// The app words every reason itself (`doctor_<id>_<reason>` in the client's en.ts); its
    /// English, filled in with `params`, must say what the engine's `message` says, and every
    /// such key must be a reason the engine gives.
    #[test]
    fn every_message_matches_the_apps_english() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../client/src/i18n/locales/en.ts");
        let Ok(en) = std::fs::read_to_string(&path) else {
            return; // a build without the client beside it
        };
        let text_of = |key: &str| -> Option<String> {
            let line = en.lines().find(|l| l.starts_with(&format!("{key}: ")))?;
            let value = line[key.len() + 2..].trim_end().trim_end_matches(',');
            serde_json::from_str(value).ok()
        };
        let mut seen = std::collections::BTreeSet::new();
        for facts in every_case() {
            for c in evaluate(&facts) {
                let key = format!("doctor_{}_{}", c.id, c.reason);
                let mut text = text_of(&key).unwrap_or_else(|| panic!("{key} is not in en.ts"));
                for (k, v) in &c.params {
                    let v = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    text = text.replace(&format!("{{{k}}}"), &v);
                }
                assert_eq!(text, c.message, "{key}");
                seen.insert(key);
            }
        }
        let ids = [
            "fake_package",
            "mount_layer",
            "package_size",
            "database",
            "permissions",
            "free_space",
        ];
        for line in en.lines() {
            let Some(key) = line.split(':').next() else {
                continue;
            };
            if ids
                .iter()
                .any(|id| key.starts_with(&format!("doctor_{id}_")))
            {
                assert!(seen.contains(key), "{key} is no reason the engine gives");
            }
        }
    }
}
