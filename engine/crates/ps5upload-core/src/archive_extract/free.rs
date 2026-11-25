//! Unpacking an archive while its own space is given back, so a game fits on a drive that has
//! no room for the archive and the unpacked game side by side.
//!
//! This destroys the archive, so it is asked for by name and never the default. What each
//! kind of archive allows ([`FreeMode`]):
//!
//! - **`.zip`** ([`FreeMode::Entries`]): every file in a zip is compressed on its own and the
//!   list of them is at the end. The list is read into memory, then the files are unpacked from
//!   the last one in the archive to the first, and the archive is cut short behind each one.
//! - **A multi-part `.rar`** ([`FreeMode::Parts`]): the parts are read in order, first to
//!   last, and a part is deleted once every file that starts in it (or before it) is unpacked
//!   and the reader has moved on to a later part.
//! - **Everything else** ([`FreeMode::End`]: a `.7z`, a one-file `.rar`, a tar or `.zst`): the
//!   data is one stream read from the front, and a file can't be cut at the front. Nothing is
//!   freed while it unpacks; the archive is deleted the moment the unpack has finished and
//!   been checked, before whatever is made from it starts.
//!
//! The rules it keeps:
//!
//! - [`plan`] checks everything that can be checked without unpacking, and refuses with a
//!   plain reason: the place (a network or read-only drive, a link, a file with a second
//!   name), the listing (a password, a method that can't be unpacked, entries that overlap,
//!   two files that would land on one name), the game (exactly one), the parts (all there,
//!   each one's files where the set says they are).
//! - Before the first byte goes, [`consume`] reads the whole archive through its decoder
//!   once (`super::test`), so damage anywhere in it is found while it is still whole.
//! - Nothing is cut or deleted until what it held is written, compared with the size and
//!   checksum the archive lists, and flushed to the drive.
//! - The moment the first bytes of a `.zip` go, it is renamed with [`PARTIAL_SUFFIX`]: a cut
//!   archive never looks like a good one. A set's remaining parts get the suffix when the
//!   unpack stops early (a set without its first part can't be opened as it is).
//! - When it stops part way (a failure, a cancel), everything unpacked so far stays, and the
//!   error names what is left of the archive ([`ConsumeError::left`]).

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};

use super::{
    host_path, is_sidecar, kind_of, open_zip, remove_path_retrying, ArchiveKind, Sink,
    GAME_FILE_EXTS,
};
use crate::transfer::{sanitize_7z_entry, sanitize_zip_entry};

/// Added to the name of an archive that is no longer whole.
pub const PARTIAL_SUFFIX: &str = ".partial-deleted";

/// A `.zip` is cut once this much of it is unpacked and waiting to go (and at its end): a cut
/// per file would mean a drive flush per file.
pub const CUT_EVERY: u64 = 64 << 20;

/// How an archive's space is given back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeMode {
    /// File by file, as each is unpacked (a `.zip`).
    Entries,
    /// Part by part, as each is finished (a multi-part `.rar`).
    Parts,
    /// All at once, when the unpack has finished and been checked.
    End,
}

impl FreeMode {
    pub fn name(self) -> &'static str {
        match self {
            FreeMode::Entries => "entries",
            FreeMode::Parts => "parts",
            FreeMode::End => "end",
        }
    }
}

/// What the one game in an archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameKind {
    /// A game folder (one with `sce_sys/param.json` or `param.sfo`).
    Folder,
    /// A game image or package.
    Image,
}

impl GameKind {
    pub fn name(self) -> &'static str {
        match self {
            GameKind::Folder => "folder",
            GameKind::Image => "image",
        }
    }
}

/// A file as it was when it was checked: a part that changed since is not touched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Result<Stamp> {
        let m = std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
        Ok(Stamp {
            len: m.len(),
            modified: m.modified().ok(),
        })
    }
}

/// One file of a `.zip`, where it is and what the list says it holds.
#[derive(Debug, Clone)]
struct ZipStep {
    index: usize,
    /// Where its local header starts: the archive is cut here once it is unpacked.
    header_start: u64,
    rel: String,
    size: u64,
    crc: u32,
    zstd: bool,
}

/// A multi-part RAR set's files, and where each part's own begin.
#[derive(Debug, Clone)]
struct RarSteps {
    /// Every file (checked path, listed size), in the order a walk meets them.
    files: Vec<(String, u64)>,
    /// For each part: how many files start in the parts before it (0 for the first).
    starts: Vec<usize>,
}

/// An archive checked for unpacking with its space given back: what [`consume`] then does.
#[derive(Debug)]
pub struct FreePlan {
    pub mode: FreeMode,
    pub kind: ArchiveKind,
    /// Every file of the archive, first to last (one, unless it is a multi-part set).
    pub parts: Vec<PathBuf>,
    /// The size of all of them.
    pub archive_bytes: u64,
    /// The size of everything unpacked.
    pub unpacked: u64,
    /// The most room the unpack takes at once beyond the archive's own, when it unpacks onto
    /// the archive's drive: what is written, less what has been given back by then.
    pub peak_beside: u64,
    /// The one game in it.
    pub game: GameKind,
    stamps: Vec<Stamp>,
    dirs: Vec<String>,
    zip: Vec<ZipStep>,
    rar: Option<RarSteps>,
    cut_every: u64,
}

impl FreePlan {
    /// The room the unpack needs on the drive it unpacks onto: all of the unpacked size when
    /// the archive is on another drive (its space comes back there, not here).
    pub fn room_needed(&self, same_drive: bool) -> u64 {
        if same_drive {
            self.peak_beside
        } else {
            self.unpacked
        }
    }
}

/// Why an unpack stopped, and what became of the archive.
#[derive(Debug)]
pub struct ConsumeError {
    pub error: anyhow::Error,
    /// `None`: the archive is as it was. `Some`: part of it is gone for good, and these files
    /// (renamed with [`PARTIAL_SUFFIX`]) are what is left of it, which may be nothing.
    /// Everything unpacked so far was kept either way.
    pub left: Option<Vec<PathBuf>>,
}

impl std::fmt::Display for ConsumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl std::error::Error for ConsumeError {}

/// A finished unpack.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Files of the archive that could not be deleted at the end (something held them): they
    /// are still on the drive, the unpack itself is complete.
    pub not_removed: Vec<PathBuf>,
}

// ─── The drive ──────────────────────────────────────────────────────────────────────────

/// Flushes one unpacked file. On macOS a full flush per file takes long enough to matter for a
/// game of many small files, so the file's data is handed to the drive here and the drive is
/// made to keep it by [`barrier`]; on Linux the barrier flushes the whole file system at once.
pub(super) fn flush_file(f: &File) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `f` is an open file for the length of the call.
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = f;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    {
        f.sync_all()
    }
}

/// Everything unpacked into `dir` so far is on the drive when this returns `Ok`: called
/// before any of the archive is cut or deleted.
fn barrier(dir: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let d = File::open(dir)?;
        // SAFETY: `d` is an open directory for the length of the call.
        if unsafe { libc::syncfs(d.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // macOS: a full flush of the drive (F_FULLFSYNC), which covers the files handed to it
        // by `flush_file`; elsewhere the folder's own entry.
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// A folder's own entries (a rename in it) flushed, where a folder can be.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Why the drive `path` is on can't have a file cut or deleted safely, or `None`: a network
/// drive (a cut there may not have happened when it says it has) or a read-only one.
fn drive_refusal(path: &Path) -> Option<String> {
    const NETWORK: &str = "it is on a network drive, where freeing space as it goes is not \
                           safe; copy it to a drive in or plugged into this computer first";
    const READ_ONLY: &str = "it is on a read-only drive";
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a NUL-terminated path and `st` a statfs to fill.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return Some("the drive it is on could not be checked".into());
        }
        let flags = st.f_flags as u64;
        if flags & libc::MNT_LOCAL as u64 == 0 {
            return Some(NETWORK.into());
        }
        if flags & libc::MNT_RDONLY as u64 != 0 {
            return Some(READ_ONLY.into());
        }
        None
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo") else {
            return Some("the drive it is on could not be checked".into());
        };
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        match mount_of(&mounts, &real) {
            Some((fstype, read_only)) => {
                if network_fs(&fstype) {
                    Some(NETWORK.into())
                } else if read_only {
                    Some(READ_ONLY.into())
                } else {
                    None
                }
            }
            None => Some("the drive it is on could not be checked".into()),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let text = real.to_string_lossy().into_owned();
        let plain = match text.strip_prefix(r"\\?\") {
            Some(rest) if rest.starts_with("UNC\\") => return Some(NETWORK.into()),
            Some(rest) => rest.to_string(),
            None if text.starts_with(r"\\") => return Some(NETWORK.into()),
            None => text,
        };
        let bytes = plain.as_bytes();
        if bytes.len() < 3 || bytes[1] != b':' {
            return Some("the drive it is on could not be checked".into());
        }
        let root: Vec<u16> = std::ffi::OsStr::new(&format!("{}:\\", &plain[..1]))
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `root` is a NUL-terminated wide string.
        match unsafe { GetDriveTypeW(root.as_ptr()) } {
            // DRIVE_REMOTE
            4 => Some(NETWORK.into()),
            // DRIVE_CDROM
            5 => Some(READ_ONLY.into()),
            // DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR
            0 | 1 => Some("the drive it is on could not be checked".into()),
            _ => None,
        }
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    {
        let _ = (path, NETWORK, READ_ONLY);
        None
    }
}

/// The file system type and read-only flag of the mount `path` is on, from the text of
/// `/proc/self/mountinfo`: the mount whose point is the longest one that holds `path`.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn mount_of(mountinfo: &str, path: &Path) -> Option<(String, bool)> {
    let mut best: Option<(usize, String, bool)> = None;
    for line in mountinfo.lines() {
        let (left, right) = line.split_once(" - ")?;
        let mut fields = left.split(' ');
        let point = unescape_mount(fields.nth(4)?);
        let read_only = fields.next()?.split(',').any(|o| o == "ro");
        let fstype = right.split(' ').next()?.to_string();
        if !path.starts_with(&point) {
            continue;
        }
        let depth = Path::new(&point).components().count();
        // A later mount on the same point covers the earlier one.
        if best.as_ref().is_none_or(|(d, _, _)| depth >= *d) {
            best = Some((depth, fstype, read_only));
        }
    }
    best.map(|(_, fstype, read_only)| (fstype, read_only))
}

/// A mount point as mountinfo writes it: a space, tab, newline or backslash is `\ooo`.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &field[i + 1..i + 4];
            if let Ok(b) = u8::from_str_radix(oct, 8) {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// File systems that are another computer's: a cut or a delete there is the server's to do.
/// `fuseblk` (NTFS and exFAT drives) is a drive of this computer; any other `fuse.*` is not
/// known to be.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn network_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "smbfs"
            | "smb2"
            | "9p"
            | "afs"
            | "ceph"
            | "glusterfs"
            | "lustre"
            | "ncpfs"
            | "davfs"
            | "vboxsf"
            | "drvfs"
            | "fuse"
    ) || fstype.starts_with("fuse.")
}

/// Whether two paths are on one drive (file system), so that space freed at one is room at
/// the other. Unknown counts as not.
pub fn same_drive(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let root = |p: &Path| {
            std::fs::canonicalize(p)
                .ok()
                .and_then(|p| p.components().next().map(|c| c.as_os_str().to_os_string()))
        };
        match (root(a), root(b)) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
            _ => false,
        }
    }
}

/// Why this one file can't be cut or deleted here, or `Ok`: it has to be a plain file with one
/// name, on a drive of this computer that can be written, in a folder that can be written.
fn check_place(path: &Path) -> Result<()> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("read {}", path.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "{} is a link to the archive, not the archive itself; choose the archive file",
            path.display()
        );
    }
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            bail!(
                "{} has a second name on this drive (a hard link): freeing it would damage \
                 that one too",
                path.display()
            );
        }
    }
    if meta.permissions().readonly() {
        bail!("{} is read-only", path.display());
    }
    if let Some(why) = drive_refusal(path) {
        bail!("{}: {why}", path.display());
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| anyhow!("{} can't be changed: {e}", path.display()))?;
    // Its folder has to take a rename and a delete: tried with a file of our own.
    let dir = path.parent().unwrap_or(Path::new("."));
    let probe = dir.join(format!(".ps5upload-free-{}", std::process::id()));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    match made {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) => bail!("the folder {} can't be written: {e}", dir.display()),
    }
    Ok(())
}

// ─── The listing ────────────────────────────────────────────────────────────────────────

/// The one game among `files` (checked archive paths), as [`super::find_game`] finds it in
/// the unpacked folder: a game folder however deep it is wrapped, or a single game image.
/// None, or more than one, is refused here, while the archive is still whole.
fn game_in_names<'a>(files: impl Iterator<Item = &'a str>) -> Result<GameKind> {
    const MAX_DEPTH: usize = 4;
    let files: Vec<&str> = files.filter(|p| !p.split('/').any(is_sidecar)).collect();
    let mut games: BTreeSet<String> = BTreeSet::new();
    for p in &files {
        for param in ["sce_sys/param.json", "sce_sys/param.sfo"] {
            let dir = if *p == param {
                Some("")
            } else {
                p.strip_suffix(param).filter(|d| d.ends_with('/'))
            };
            if let Some(dir) = dir.filter(|d| d.matches('/').count() <= MAX_DEPTH) {
                games.insert(dir.to_string());
            }
        }
    }
    // A game inside another game's folder is part of it.
    let all: Vec<String> = games.iter().cloned().collect();
    games.retain(|g| !all.iter().any(|o| o != g && g.starts_with(o.as_str())));
    let images: Vec<&str> = files
        .iter()
        .copied()
        .filter(|p| p.matches('/').count() <= MAX_DEPTH)
        .filter(|p| !games.iter().any(|g| p.starts_with(g.as_str())))
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            name.rsplit_once('.')
                .is_some_and(|(_, ext)| GAME_FILE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .collect();
    match (games.len(), images.len()) {
        (1, 0) => Ok(GameKind::Folder),
        (0, 1) => Ok(GameKind::Image),
        (0, 0) => bail!(
            "no game in the archive: no folder with sce_sys/param.json, and no game image or package"
        ),
        _ => {
            let mut names: Vec<String> = games
                .iter()
                .map(|g| match g.trim_end_matches('/') {
                    "" => "(the archive's top folder)".to_string(),
                    g => g.to_string(),
                })
                .collect();
            names.extend(images.iter().map(|s| s.to_string()));
            bail!(
                "more than one game in the archive ({}); unpack it and convert one at a time",
                names.join(", ")
            )
        }
    }
}

/// Refuses a listing that can't be written out as it stands: a path this computer can't
/// hold, two entries that land on one name, or a file where another entry needs a folder.
/// Found half way through, any of these would stop the unpack with part of the archive gone.
/// Names are compared without case: most drives a game is unpacked onto ignore it.
fn check_names<'a>(
    files: impl Iterator<Item = &'a str>,
    dirs: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let probe = Path::new(if cfg!(windows) { r"C:\x" } else { "/x" });
    let mut seen: HashMap<String, &str> = HashMap::new();
    let files: Vec<&str> = files.collect();
    for f in &files {
        host_path(probe, f)?;
        if let Some(other) = seen.insert(f.to_lowercase(), f) {
            bail!(
                "two files in the archive would land on one name ({other} and {f}); unpack \
                 it the usual way instead"
            );
        }
    }
    let clash = |folder: &str, by: &str| -> Result<()> {
        if let Some(file) = seen.get(&folder.to_lowercase()) {
            bail!(
                "the archive has both a file and a folder named {file} (the folder holds \
                 {by}); unpack it the usual way instead"
            );
        }
        Ok(())
    };
    for f in &files {
        let mut at = *f;
        while let Some((parent, _)) = at.rsplit_once('/') {
            clash(parent, f)?;
            at = parent;
        }
    }
    for d in dirs {
        host_path(probe, d)?;
        let mut at = d;
        loop {
            clash(at, d)?;
            match at.rsplit_once('/') {
                Some((parent, _)) => at = parent,
                None => break,
            }
        }
    }
    Ok(())
}

const ZIP_PASSWORD: &str = "this .zip is password-protected, which Convert can't open; re-pack \
                            it without a password, or as .rar";

/// A zip entry's name as the unpack writes it: `None` for the archive's own top folder.
fn zip_rel<R: Read>(e: &zip::read::ZipFile<'_, R>) -> Result<Option<String>> {
    let decoded = e
        .name()
        .map(|n| n.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(e.name_raw()).into_owned());
    let name = crate::transfer::zip_entry_name(e.name_raw(), &decoded);
    match sanitize_zip_entry(&name) {
        Some(rel) => Ok(Some(rel)),
        None if e.is_dir() && name.trim_matches('/').is_empty() => Ok(None),
        None => bail!("the archive has an unsafe entry path: {name:?}"),
    }
}

fn base_plan(archive: &Path, kind: ArchiveKind) -> Result<FreePlan> {
    check_place(archive)?;
    let stamp = Stamp::of(archive)?;
    Ok(FreePlan {
        mode: FreeMode::End,
        kind,
        parts: vec![archive.to_path_buf()],
        archive_bytes: stamp.len,
        unpacked: 0,
        peak_beside: 0,
        game: GameKind::Folder,
        stamps: vec![stamp],
        dirs: Vec::new(),
        zip: Vec::new(),
        rar: None,
        cut_every: CUT_EVERY,
    })
}

/// What [`plan`] checks beyond the archive itself.
#[derive(Debug, Clone, Copy)]
struct Wants {
    cut_every: u64,
    /// Exactly one game in it (off only for tests on archives that hold none).
    game: bool,
}

/// Checks the archive at `archive` (a multi-part set's first part) for unpacking with its
/// space given back, changing nothing. `Err` is a plain reason it can't be done.
pub fn plan(archive: &Path, password: Option<&str>) -> Result<FreePlan> {
    plan_with(
        archive,
        password,
        Wants {
            cut_every: CUT_EVERY,
            game: true,
        },
    )
}

fn plan_with(archive: &Path, password: Option<&str>, wants: Wants) -> Result<FreePlan> {
    let kind = kind_of(archive)?;
    let mut plan = base_plan(archive, kind)?;
    plan.cut_every = wants.cut_every;
    match kind {
        ArchiveKind::Zip => plan_zip(archive, &mut plan, wants)?,
        ArchiveKind::Rar => plan_rar(archive, password, &mut plan, wants)?,
        ArchiveKind::SevenZ => {
            let header = super::read_7z_header(archive)?;
            let mut files = Vec::new();
            let mut dirs = Vec::new();
            for e in &header.files {
                let Some(rel) = sanitize_7z_entry(e.name()) else {
                    bail!("the archive has an unsafe entry path: {:?}", e.name());
                };
                if e.is_directory() {
                    dirs.push(rel);
                } else {
                    plan.unpacked = plan.unpacked.saturating_add(e.size());
                    files.push(rel);
                }
            }
            listed(&mut plan, &files, dirs, wants)?;
        }
        ArchiveKind::Tar => {
            let layout = crate::tar_archive::tar_layout(archive)
                .map_err(|e| anyhow!("read {}: {e}", archive.display()))?;
            plan.unpacked = layout.unpacked_total();
            let files: Vec<String> = (layout.files.iter())
                .filter(|f| f.kept)
                .map(|f| f.path.clone())
                .collect();
            listed(&mut plan, &files, layout.dirs.clone(), wants)?;
        }
    }
    if plan.mode == FreeMode::End {
        // Nothing comes back until the unpack is over.
        plan.peak_beside = plan.unpacked;
    }
    Ok(plan)
}

/// The names checked and the game found, for a plan whose files are `files`.
fn listed(plan: &mut FreePlan, files: &[String], dirs: Vec<String>, wants: Wants) -> Result<()> {
    check_names(
        files.iter().map(String::as_str),
        dirs.iter().map(String::as_str),
    )?;
    if wants.game {
        plan.game = game_in_names(files.iter().map(String::as_str))?;
    }
    plan.dirs = dirs;
    Ok(())
}

fn plan_zip(archive: &Path, plan: &mut FreePlan, wants: Wants) -> Result<()> {
    let mut z = open_zip(archive)?;
    let dir_start = z.central_directory_start();
    let mut raw = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut steps = Vec::new();
    let mut dirs = Vec::new();
    // Where every entry's bytes are: (header start, data end).
    let mut extents: Vec<(u64, u64)> = Vec::with_capacity(z.len());
    for index in 0..z.len() {
        let e = z
            .by_index_raw(index)
            .map_err(|e| anyhow!("read zip entry {index}: {e}"))?;
        if e.encrypted() {
            bail!("{ZIP_PASSWORD}");
        }
        let method = e.compression();
        let zstd = method == zip::CompressionMethod::ZSTD;
        if !zstd
            && method != zip::CompressionMethod::Stored
            && method != zip::CompressionMethod::Deflated
        {
            bail!(
                "this .zip holds a file packed in a way that can't be unpacked here ({method}); \
                 re-pack it as a plain .zip"
            );
        }
        let header_start = e.header_start();
        let data_start = local_data_start(&mut raw, header_start)
            .with_context(|| format!("read zip entry {index}"))?;
        let data_end = data_start
            .checked_add(e.compressed_size())
            .ok_or_else(|| anyhow!("zip entry {index} has an impossible size"))?;
        extents.push((header_start, data_end));
        let Some(rel) = zip_rel(&e)? else { continue };
        if e.is_dir() {
            dirs.push(rel);
            continue;
        }
        plan.unpacked = plan.unpacked.saturating_add(e.size());
        steps.push(ZipStep {
            index,
            header_start,
            rel,
            size: e.size(),
            crc: e.crc32(),
            zstd,
        });
    }
    // Cutting behind one file must take nothing an earlier one needs: every entry's bytes
    // come after the entry before it, and all of them before the list at the end.
    extents.sort_unstable();
    for pair in extents.windows(2) {
        if pair[0].1 > pair[1].0 {
            bail!("this .zip has files that share bytes in the archive; unpack it the usual way instead");
        }
    }
    if extents.last().is_some_and(|last| last.1 > dir_start) {
        bail!("this .zip's file list overlaps its files; unpack it the usual way instead");
    }
    let files: Vec<String> = steps.iter().map(|s| s.rel.clone()).collect();
    listed(plan, &files, dirs, wants)?;
    // Last in the archive first.
    steps.sort_by_key(|s| std::cmp::Reverse(s.header_start));
    // The room it takes beyond the archive's own: what is written, less what was cut by then.
    let (mut len, mut written, mut peak) = (plan.archive_bytes, 0u64, 0u64);
    for (n, s) in steps.iter().enumerate() {
        written = written.saturating_add(s.size);
        peak = peak.max((written.saturating_add(len)).saturating_sub(plan.archive_bytes));
        if n + 1 == steps.len() || len.saturating_sub(s.header_start) >= plan.cut_every {
            len = s.header_start;
        }
    }
    plan.peak_beside = peak;
    plan.mode = FreeMode::Entries;
    plan.zip = steps;
    Ok(())
}

/// Where an entry's data starts, from its local header at `header_start` (its name and extra
/// field can differ in length from the list's copy).
fn local_data_start(raw: &mut File, header_start: u64) -> Result<u64> {
    let mut head = [0u8; 30];
    raw.seek(SeekFrom::Start(header_start))?;
    raw.read_exact(&mut head)
        .map_err(|_| anyhow!("its header is past the end of the archive"))?;
    if head[..4] != [b'P', b'K', 3, 4] {
        bail!("no file header where the archive's list says one is");
    }
    let name = u64::from(u16::from_le_bytes([head[26], head[27]]));
    let extra = u64::from(u16::from_le_bytes([head[28], head[29]]));
    Ok(header_start + 30 + name + extra)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn plan_rar(_: &Path, _: Option<&str>, _: &mut FreePlan, _: Wants) -> Result<()> {
    bail!("RAR archives can't be opened on a phone; use .zip or .7z")
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn plan_rar(
    archive: &Path,
    password: Option<&str>,
    plan: &mut FreePlan,
    wants: Wants,
) -> Result<()> {
    let parts = rar_parts(archive)?;
    let (unpacked, files) = crate::transfer::rar_plan_entries(archive, password, &[])?;
    let dirs = crate::transfer::rar_dirs(archive, password)?;
    plan.unpacked = unpacked;
    let names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
    listed(plan, &names, dirs, wants)?;
    if parts.len() == 1 {
        return Ok(());
    }
    // Where each part's own files begin in the set's list: a part listed on its own gives the
    // files that start in it and in the parts after it.
    let mut starts = vec![0usize];
    let mut stamps = Vec::new();
    for part in &parts {
        check_place(part)?;
        stamps.push(Stamp::of(part)?);
    }
    for part in &parts[1..] {
        let (_, own) = crate::transfer::rar_plan_entries(part, password, &[])?;
        let belongs = own.len() <= files.len() && files[files.len() - own.len()..] == own[..];
        let start = files.len().saturating_sub(own.len());
        if !belongs || start < *starts.last().unwrap_or(&0) {
            bail!(
                "{} does not look like a part of this set (its files are not where the set \
                 lists them); unpack it the usual way instead",
                part.display()
            );
        }
        starts.push(start);
    }
    plan.archive_bytes = stamps.iter().map(|s| s.len).sum();
    // The room it takes beyond the set's own: what is written, less the parts gone by then.
    let (mut left, mut written, mut peak, mut gone) = (plan.archive_bytes, 0u64, 0u64, 0usize);
    for (j, (_, size)) in files.iter().enumerate() {
        while gone + 1 < parts.len() && starts[gone + 1] <= j {
            left = left.saturating_sub(stamps[gone].len);
            gone += 1;
        }
        if *size < UNKNOWN_SIZE {
            written = written.saturating_add(*size);
        }
        peak = peak.max((written.saturating_add(left)).saturating_sub(plan.archive_bytes));
    }
    plan.peak_beside = peak;
    plan.mode = FreeMode::Parts;
    plan.parts = parts;
    plan.stamps = stamps;
    plan.rar = Some(RarSteps { files, starts });
    Ok(())
}

/// At or over this, a RAR entry's listed size is UnRAR's "not known".
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const UNKNOWN_SIZE: u64 = 1 << 62;

/// Every file of the RAR set whose first part is `first`, in order, by the two ways sets are
/// named: `x.part1.rar`, `x.part2.rar`… and `x.rar`, `x.r00`, `x.r01`… A part other than the
/// first is refused.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn rar_parts(first: &Path) -> Result<Vec<PathBuf>> {
    let name = first
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("{} has no readable name", first.display()))?;
    let lower = name.to_ascii_lowercase();
    let mut parts = vec![first.to_path_buf()];
    let Some(stem) = lower.strip_suffix(".rar") else {
        return Ok(parts);
    };
    if let Some((before, digits)) = stem.rsplit_once(".part") {
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            if digits.parse::<u32>().ok() != Some(1) {
                bail!("{name} is not the first part of its set; choose the first part");
            }
            let width = digits.len();
            // The name's own spelling of each piece is kept.
            let head = &name[..before.len() + ".part".len()];
            let ext = &name[name.len() - ".rar".len()..];
            for n in 2u32.. {
                let next = first.with_file_name(format!("{head}{n:0width$}{ext}"));
                if !next.is_file() {
                    break;
                }
                parts.push(next);
            }
            return Ok(parts);
        }
    }
    let stem = &name[..stem.len()];
    let upper = name.ends_with(".RAR");
    for i in 0..super::RAR_OLD_VOLUMES_MAX {
        let Some(next) = super::rar_old_volume_name(stem, i, upper) else {
            break;
        };
        let next = first.with_file_name(next);
        if !next.is_file() {
            break;
        }
        parts.push(next);
    }
    Ok(parts)
}

// ─── The unpack ─────────────────────────────────────────────────────────────────────────

/// Unpacks the archive of `plan` into `dest` (created if missing), giving its space back as
/// the plan's mode allows, and deletes what is left of it at the end.
///
/// `on_bytes(done, total)` counts unpacked bytes; where space is given back as it goes, the
/// archive is first read through once to check it, so the count runs to twice its size.
/// Setting `cancel` stops it at the next chunk. `found` runs once everything is unpacked: in
/// [`FreeMode::End`] before the archive is deleted, so an archive with no game in it stays.
pub fn consume(
    plan: &FreePlan,
    dest: &Path,
    password: Option<&str>,
    on_bytes: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
    found: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Outcome, ConsumeError> {
    let whole = |error: anyhow::Error| ConsumeError { error, left: None };
    for (part, stamp) in plan.parts.iter().zip(&plan.stamps) {
        if Stamp::of(part).map_err(whole)? != *stamp {
            return Err(whole(anyhow!(
                "{} changed since it was checked; nothing was unpacked",
                part.display()
            )));
        }
    }
    std::fs::create_dir_all(dest)
        .with_context(|| format!("create {}", dest.display()))
        .map_err(whole)?;
    let first = &plan.parts[0];
    let progressive = plan.mode != FreeMode::End;
    let total = if progressive {
        plan.unpacked.saturating_mul(2)
    } else {
        plan.unpacked
    };
    if progressive {
        // Read through once while it is whole: damage found later would be found with part of
        // the archive gone.
        let mut first_pass = |done: u64, _: u64| on_bytes(done.min(plan.unpacked), total);
        super::test(first, password, &mut first_pass, cancel).map_err(whole)?;
    }
    let mut sink = Sink {
        dest,
        discard: false,
        durable: true,
        done: if progressive { plan.unpacked } else { 0 },
        total,
        on_bytes,
        cancel,
    };
    sink.check_cancel().map_err(whole)?;
    for d in &plan.dirs {
        sink.dir(d).map_err(whole)?;
    }
    match plan.mode {
        FreeMode::Entries => consume_zip(plan, &mut sink, found),
        FreeMode::Parts => consume_rar_parts(plan, password, &mut sink, found),
        FreeMode::End => {
            let unpacked = match plan.kind {
                ArchiveKind::SevenZ => super::extract_7z(first, &mut sink),
                ArchiveKind::Rar => super::extract_rar(first, password, &mut sink),
                ArchiveKind::Tar => super::extract_tar(first, &mut sink),
                ArchiveKind::Zip => Err(anyhow!("a .zip is freed file by file")),
            };
            unpacked.map_err(whole)?;
            barrier(dest)
                .with_context(|| format!("flush {}", dest.display()))
                .map_err(whole)?;
            found().map_err(whole)?;
            let mut out = Outcome::default();
            if remove_path_retrying(first).is_err() {
                out.not_removed.push(first.clone());
            }
            Ok(out)
        }
    }
}

/// A name for what is left of `path` that nothing has: `path` with [`PARTIAL_SUFFIX`].
fn partial_name(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut to = path.with_file_name(format!("{name}{PARTIAL_SUFFIX}"));
    let mut n = 2;
    while to.symlink_metadata().is_ok() {
        to = path.with_file_name(format!("{name}{PARTIAL_SUFFIX}-{n}"));
        n += 1;
    }
    to
}

fn consume_zip(
    plan: &FreePlan,
    sink: &mut Sink,
    found: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Outcome, ConsumeError> {
    let whole = |error: anyhow::Error| ConsumeError { error, left: None };
    let archive = &plan.parts[0];
    let mut z = open_zip(archive).map_err(whole)?;
    let cutter = std::fs::OpenOptions::new()
        .write(true)
        .open(archive)
        .with_context(|| format!("open {} to free it", archive.display()))
        .map_err(whole)?;
    // Held for the length of the unpack: another program that locks what it reads is seen.
    if let Err(std::fs::TryLockError::WouldBlock) = cutter.try_lock() {
        return Err(whole(anyhow!(
            "{} is in use by another program",
            archive.display()
        )));
    }
    // Where what is left of the archive is now, and whether any of it is gone.
    let mut at = archive.clone();
    let mut cut = false;
    let mut len = plan.archive_bytes;
    for (n, step) in plan.zip.iter().enumerate() {
        let failed = |error: anyhow::Error, at: &Path, cut: bool| ConsumeError {
            error,
            left: cut.then(|| vec![at.to_path_buf()]),
        };
        let unpacked = if step.zstd {
            z.by_index_raw(step.index)
                .map_err(|e| anyhow!("read zip entry {}: {e}", step.index))
                .and_then(|raw| {
                    let mut from = crate::tar_archive::ZipZstdEntry::new(raw, step.crc, step.size);
                    sink.file_checked(&step.rel, &mut from, Some(step.size), Some(step.crc))
                })
        } else {
            z.by_index(step.index)
                .map_err(|e| anyhow!("read zip entry {}: {e}", step.index))
                .and_then(|mut from| {
                    sink.file_checked(&step.rel, &mut from, Some(step.size), Some(step.crc))
                })
        };
        unpacked.map_err(|e| failed(e, &at, cut))?;
        if n + 1 < plan.zip.len() && len.saturating_sub(step.header_start) < plan.cut_every {
            continue;
        }
        // Everything from this file on is unpacked: on the drive first, then gone from here.
        barrier(sink.dest)
            .with_context(|| format!("flush {}", sink.dest.display()))
            .map_err(|e| failed(e, &at, cut))?;
        if !cut {
            let to = partial_name(archive);
            std::fs::rename(archive, &to)
                .with_context(|| format!("rename {} before freeing it", archive.display()))
                .map_err(|e| failed(e, &at, cut))?;
            sync_dir(to.parent().unwrap_or(Path::new(".")));
            at = to;
            cut = true;
        }
        cutter
            .set_len(step.header_start)
            .with_context(|| format!("free the unpacked end of {}", at.display()))
            .map_err(|e| failed(e, &at, cut))?;
        len = step.header_start;
    }
    drop(z);
    drop(cutter);
    let mut out = Outcome::default();
    if remove_path_retrying(&at).is_err() {
        out.not_removed.push(at.clone());
    }
    found().map_err(|error| ConsumeError {
        error,
        left: Some(out.not_removed.clone()),
    })?;
    Ok(out)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn consume_rar_parts(
    _: &FreePlan,
    _: Option<&str>,
    _: &mut Sink,
    _: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Outcome, ConsumeError> {
    Err(ConsumeError {
        error: anyhow!("RAR archives can't be opened on a phone; use .zip or .7z"),
        left: None,
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn consume_rar_parts(
    plan: &FreePlan,
    password: Option<&str>,
    sink: &mut Sink,
    found: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Outcome, ConsumeError> {
    use crate::rar_stream::{next_entry, EntryReader};
    let Some(steps) = plan.rar.as_ref() else {
        return Err(ConsumeError {
            error: anyhow!("the set's parts were not listed"),
            left: None,
        });
    };
    let parts = &plan.parts;
    // Parts gone so far (always the first ones), and those that would not go when asked.
    let mut gone = 0usize;
    let mut held: Vec<PathBuf> = Vec::new();
    let (rx, worker) = crate::transfer::spawn_rar_worker(&parts[0], password, Vec::new());
    let walk = |sink: &mut Sink, gone: &mut usize, held: &mut Vec<PathBuf>| -> Result<()> {
        let mut j = 0usize;
        loop {
            let next = next_entry(&rx).map_err(|e| anyhow!("{e}"))?;
            // The reader is at file `j` now (or at the end), so it has left every part whose
            // own files, and all before them, are unpacked: those parts go.
            let done_before = |part: usize| match (&next, steps.starts.get(part + 1)) {
                (None, _) => true,
                (Some(_), Some(start)) => *start <= j,
                (Some(_), None) => false,
            };
            if *gone < parts.len() && done_before(*gone) {
                barrier(sink.dest).with_context(|| format!("flush {}", sink.dest.display()))?;
                while *gone < parts.len() && done_before(*gone) {
                    if remove_path_retrying(&parts[*gone]).is_err() {
                        held.push(parts[*gone].clone());
                    }
                    *gone += 1;
                }
            }
            let Some(rel) = next else {
                if j != steps.files.len() {
                    bail!("the set ended after {j} of its {} files", steps.files.len());
                }
                return Ok(());
            };
            let Some((listed, size)) = steps.files.get(j) else {
                bail!("the set holds more files than it listed");
            };
            if *listed != rel {
                bail!("the set's files came in another order than it listed ({rel}, not {listed})");
            }
            let size = (*size < UNKNOWN_SIZE).then_some(*size);
            sink.file_checked(&rel, &mut EntryReader::new(&rx), size, None)?;
            j += 1;
        }
    };
    let walked = walk(sink, &mut gone, &mut held);
    // Dropping the receiver unblocks a reader still sending (a cancel, a failure).
    drop(rx);
    let _ = worker.join();
    match walked {
        Ok(()) => {
            // A part something held when its turn came: once more, now that the reader is gone.
            held.retain(|p| remove_path_retrying(p).is_err());
            let out = Outcome { not_removed: held };
            found().map_err(|error| ConsumeError {
                error,
                left: Some(out.not_removed.clone()),
            })?;
            Ok(out)
        }
        Err(error) if gone == 0 => Err(ConsumeError { error, left: None }),
        Err(error) => {
            // The set can't be opened without its first parts: what is left is named so.
            let mut left = Vec::new();
            for part in held.iter().chain(&parts[gone..]) {
                if !part.is_file() {
                    continue;
                }
                let to = partial_name(part);
                left.push(match std::fs::rename(part, &to) {
                    Ok(()) => to,
                    Err(_) => part.clone(),
                });
            }
            Err(ConsumeError {
                error,
                left: Some(left),
            })
        }
    }
}

/// Whether `name` is what [`consume`] leaves of an archive it could not finish.
pub fn is_partial_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match lower.rfind(PARTIAL_SUFFIX) {
        Some(at) => {
            let tail = &lower[at + PARTIAL_SUFFIX.len()..];
            tail.is_empty()
                || tail
                    .strip_prefix('-')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests;
