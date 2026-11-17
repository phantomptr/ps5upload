//! Editing a game image IN PLACE (the image browser's "Apply in place"), instead of writing an
//! edited copy. Ported from PS5 Dump Forge's edit job (quer3q/ps5-dump-forge commit 8295556,
//! `crates/ps5-dump-forge-core/src/edit_job.rs`: scan, preflight, `patch::apply`, a check of
//! the image through its reader, commit; any failure after the apply rolls it back), trimmed to
//! what PS5Upload's edits are (`ps5upload_fpkg::browse::Edit`).
//!
//! * **Which images**: an uncompressed `.exfat`, `.ffpkg` (UFS2) or `.ffpfs` (PFS) on this
//!   computer. A `.ffpfsc`, a package and LZ4/zstd packs are only ever saved as an edited copy.
//!   The format editors (`ps5_dump_forge_{exfat,ufs2,pfs}::edit`) plan every change against the
//!   image's own free space: the image never grows, and a change that does not fit is refused
//!   with the reason, before anything is written (the UI then offers the edited copy).
//! * **Crash safety** (`ps5_dump_forge_patch`): the old bytes of everything the edit overwrites
//!   go to `<image>.forge-undo` beside it first (synced, with its folder); then the new data
//!   into free space, then the metadata. Once the image reads back right the journal is
//!   deleted; on any failure the journal restores it. A journal a crash left is settled the
//!   next time the image is opened here ([`before_open`]): restored when complete, deleted when
//!   it was cut short (nothing had been overwritten yet), kept (and the image refused) when
//!   damaged. The same journal format as Forge's, so either app recovers the other's. A
//!   `<image>.forge-undo.id` beside it says which image the journal is for: a journal is never
//!   restored into another image of the same length put in the crashed one's place (the open
//!   is refused, both files kept).
//! * **Exclusive**: one in-place edit per image in this engine ([`busy`]: builds, saves and the
//!   browser leave it alone meanwhile), no other Workshop job reading it, and an OS lock on the
//!   file while it is written.
//! * **Checked**: before the journal goes, every changed file (put or moved) is hashed from
//!   where it comes from; after the apply, the format editor reopens the image (its own
//!   consistency checks: FAT/bitmap/up-case, UFS2's group totals, PFS's path table), the
//!   image's reader lists exactly the expected files and folders, and every changed file
//!   hashes the same. `full_verify` hashes every file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use ps5_dump_forge_patch::{self as patch, Applied, Data, Op, Recovery, Syncs, Write};
use ps5upload_fpkg::browse::{self, Edit, Edited, Filesystem};
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// How the journal and the image are made durable on a desktop: std's `sync_all`, and the
/// folder synced on Unix.
const SYNCS: Syncs = Syncs::STD;

/// Room beyond the journal's own size its folder must have: what the filesystem spends on the
/// new file (Forge's `JOURNAL_MARGIN`).
const JOURNAL_MARGIN: u64 = 1 << 20;
/// The journal's framing (`ps5_dump_forge_patch`'s layout): header and trailer, then per entry.
const JOURNAL_FRAME: u64 = 30 + 12;
const JOURNAL_ENTRY: u64 = 16;
/// Reads while hashing, in pieces of this many bytes.
const PIECE: usize = 8 << 20;

/// The stages of an in-place edit job, in order.
pub(crate) const STAGES: [&str; 3] = ["check", "write", "verify"];

/// The image formats edited in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Exfat,
    Ufs2,
    Pfs,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Exfat => "exFAT",
            Kind::Ufs2 => "UFS2 (.ffpkg)",
            Kind::Pfs => "PFS (.ffpfs)",
        }
    }
}

/// Why an edit can't go in place. `busy`: the image is in use (HTTP 409); else the format, the
/// changes or the room refuse it (the edited copy is the way).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub reason: String,
    pub busy: bool,
}

impl Refusal {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            busy: false,
        }
    }

    fn busy(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            busy: true,
        }
    }
}

// ─── Images being edited ────────────────────────────────────────────────────────────────

/// Who uses an image in this engine: the in-place edits running (each image claimed once) and
/// the builds reading a source on this computer (a count per path: two builds may read one
/// source). One lock, so an edit's claim and a build's registration are one decision: neither
/// starts while the other holds the image.
///
/// Nothing that locks this may run while its guard is held: a [`Claim`] or [`Reading`] is
/// only built, and so only ever dropped, after the guard is released (a `Drop` under it would
/// lock it again and hang).
#[derive(Default)]
struct Users {
    claims: HashSet<PathBuf>,
    readers: HashMap<PathBuf, usize>,
}

fn users() -> &'static Mutex<Users> {
    static USERS: OnceLock<Mutex<Users>> = OnceLock::new();
    USERS.get_or_init(Default::default)
}

fn lock_users() -> std::sync::MutexGuard<'static, Users> {
    users().lock().unwrap_or_else(|e| e.into_inner())
}

/// The path an image is known by here: resolved, so two spellings are one image.
fn key(image: &Path) -> PathBuf {
    std::fs::canonicalize(image).unwrap_or_else(|_| image.to_path_buf())
}

/// One path is the other or inside it: a build of a folder reads an image in it, and an edit
/// of that image changes what the build reads.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn edited_message(image: &Path) -> String {
    format!(
        "{} is being edited in place; try again once that finishes",
        image.display()
    )
}

/// An image claimed for one in-place edit, released when dropped.
pub(crate) struct Claim(PathBuf);

impl Claim {
    /// The claim, or why not: another in-place edit of `image` runs in this engine, or a
    /// build reads it (or a folder holding it).
    pub(crate) fn take(image: &Path) -> Result<Claim, String> {
        let k = key(image);
        let refused = {
            let mut users = lock_users();
            if users.claims.contains(&k) {
                Some(format!("{} is being edited in place", k.display()))
            } else if let Some(read) = users.readers.keys().find(|r| overlaps(&k, r)) {
                Some(format!(
                    "a running job is reading {}: wait for it to finish",
                    read.display()
                ))
            } else {
                users.claims.insert(k.clone());
                None
            }
        };
        match refused {
            Some(why) => Err(why),
            None => Ok(Claim(k)),
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        lock_users().claims.remove(&self.0);
    }
}

/// A job reading a source on this computer, from before its first read to its end: an
/// in-place edit of it (or of an image in it) can't be claimed meanwhile. Released when
/// dropped.
pub(crate) struct Reading(Option<PathBuf>);

impl Reading {
    /// Registers a job reading `source`, or says why it can't read it now: an in-place edit of
    /// it (or of an image inside it) runs. A source on a PS5 or a server is not registered (no
    /// edit here changes it).
    pub(crate) fn take(source: &Path) -> Result<Reading, String> {
        if ps5upload_fpkg::remote_source::is_remote(source) {
            return Ok(Reading(None));
        }
        let k = key(source);
        let refused = {
            let mut users = lock_users();
            match users.claims.iter().find(|c| overlaps(c, &k)) {
                Some(c) => Some(edited_message(c)),
                None => {
                    *users.readers.entry(k.clone()).or_insert(0) += 1;
                    None
                }
            }
        };
        match refused {
            Some(why) => Err(why),
            None => Ok(Reading(Some(k))),
        }
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        let Some(k) = self.0.take() else {
            return;
        };
        let mut users = lock_users();
        if let Some(n) = users.readers.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                users.readers.remove(&k);
            }
        }
    }
}

/// Why `path` can't be read or written by another job now: an in-place edit of it (or of an
/// image inside it) runs. `None` when nothing edits it.
#[cfg(test)]
fn busy(path: &Path) -> Option<String> {
    let k = key(path);
    let users = lock_users();
    users
        .claims
        .iter()
        .find(|e| e.starts_with(&k))
        .map(|e| edited_message(e))
}

/// Before an image is opened here (browsed, edited, built from): refused while an in-place
/// edit of it runs, and an undo journal an interrupted edit left beside it is settled first
/// (restored, or deleted when it was cut short), and logged. A damaged journal is kept and the
/// open refused: the image may be part-edited.
pub(crate) fn before_open(image: &Path) -> Result<Recovery, String> {
    open_for_reading(image).map(|(recovery, _reading)| recovery)
}

/// [`before_open`] for a job that goes on reading `image` after the open (an upload of it): the
/// [`Reading`] it returns keeps an edit from starting until it is dropped. The image is
/// registered as read before its journal is settled, so an edit can't be claimed between the
/// busy check and the recovery and have its live journal "recovered" under it.
pub(crate) fn open_for_reading(image: &Path) -> Result<(Recovery, Reading), String> {
    let reading = Reading::take(image)?;
    let recovery = settle(image)?;
    Ok((recovery, reading))
}

/// Settles a journal left beside `image` ([`patch::recover`]), logged. Only for a file that is
/// no image being edited in place right now (the caller checks, or holds the [`Claim`]).
fn settle(image: &Path) -> Result<Recovery, String> {
    let journal = patch::journal_path(image);
    if std::fs::symlink_metadata(&journal).is_err() {
        return Ok(Recovery::Clean);
    }
    let settled = patch::recover(image, SYNCS);
    match &settled {
        Ok(Recovery::Restored) => crate::engine_log::record(
            "warn",
            format!(
                "in-place edit: an earlier edit of {} was interrupted; its undo journal restored \
                 the image as it was",
                image.display()
            ),
        ),
        Ok(Recovery::Discarded) => crate::engine_log::record(
            "info",
            format!(
                "in-place edit: an earlier edit of {} was interrupted before it changed anything; \
                 its incomplete undo journal was deleted",
                image.display()
            ),
        ),
        Ok(Recovery::Clean) => {}
        Err(e) => crate::engine_log::record(
            "warn",
            format!(
                "in-place edit: the undo journal beside {} could not be settled: {e}",
                image.display()
            ),
        ),
    }
    settled.map_err(|e| {
        format!(
            "{}: an earlier in-place edit's undo journal could not be settled ({e}); keep both \
             files",
            image.display()
        )
    })
}

// ─── Planning ───────────────────────────────────────────────────────────────────────────

/// A file on this computer an edit puts into the image, as it was when the edit was planned.
#[derive(Debug, Clone)]
struct Host {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

/// The image's size and modification time, to tell it was not changed in between.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> io::Result<Stamp> {
        let m = std::fs::metadata(path)?;
        Ok(Stamp {
            len: m.len(),
            modified: m.modified().ok(),
        })
    }
}

/// What the format editor's plan takes in the image: what it allocates and what was free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Room {
    pub needed: u64,
    pub free: u64,
}

/// An in-place edit, planned: nothing written yet.
pub(crate) struct Prepared {
    pub kind: Kind,
    image: PathBuf,
    stamp: Stamp,
    writes: Vec<Write>,
    hosts: Vec<Host>,
    /// The image with the edits applied: where the files to check come from.
    expected: Edited,
    /// What the image's reader must list afterwards (with a fresh `ampr_emu.index`).
    listing: Vec<SourceFile>,
    empty_dirs: BTreeSet<String>,
    /// A fresh `ampr_emu.index` the edit puts in.
    index: Option<Vec<u8>>,
    /// What [`apply`] reads back in full (every file with `full_verify`).
    changed: Vec<String>,
    pub journal_bytes: u64,
    pub room: Option<Room>,
    /// An undo journal settled on the way in.
    pub recovery: Recovery,
}

impl Prepared {
    /// Bytes the apply writes into the image.
    pub(crate) fn write_bytes(&self) -> u64 {
        self.writes.iter().map(|w| w.data.len()).sum()
    }

    /// Bytes the checks hash, before and after.
    pub(crate) fn check_bytes(&self, full: bool) -> u64 {
        let files = &self.listing;
        if full {
            files.iter().map(|f| f.size).sum()
        } else {
            let changed: BTreeSet<&str> = self.changed.iter().map(String::as_str).collect();
            (files.iter())
                .filter(|f| changed.contains(f.path.as_str()))
                .map(|f| f.size)
                .sum()
        }
    }
}

/// The ops an edit is to the format editors (Forge's `Op`), in the same order, with the
/// files put from this computer: a folder put is every file in it (`Put`) and every empty
/// folder (`Mkdir`), as the edited copy takes it (`browse::Edited`). A rename into a folder
/// that isn't there yet gets it made first (an edited copy makes it; the editors' `Rename`
/// wants it): `folders` is every folder of the image before the edits.
fn ops_for(edits: &[Edit], mut folders: BTreeSet<String>) -> Result<(Vec<Op>, Vec<Host>), String> {
    let mut ops = Vec::new();
    let mut hosts: Vec<Host> = Vec::new();
    let mut put = |ops: &mut Vec<Op>, path: String, from: PathBuf| -> Result<(), String> {
        let m = std::fs::metadata(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        let id = u32::try_from(hosts.len()).map_err(|_| "too many files to add".to_string())?;
        let size = m.len();
        hosts.push(Host {
            path: from,
            size,
            modified: m.modified().ok(),
        });
        ops.push(Op::Put {
            path,
            len: size,
            data: Data::Source {
                id,
                offset: 0,
                len: size,
            },
        });
        Ok(())
    };
    for edit in edits {
        match edit {
            Edit::Delete { path } => {
                let path = norm(path)?;
                let inside = format!("{path}/");
                folders.retain(|f| *f != path && !f.starts_with(&inside));
                ops.push(Op::Delete { path });
            }
            Edit::Rename { from, to } => {
                let (from, to) = (norm(from)?, norm(to)?);
                let up = parent_of(&to);
                if !up.is_empty() && !folders.contains(up) {
                    ops.push(Op::Mkdir { path: up.into() });
                }
                folders.extend(ancestors_of(&to));
                ops.push(Op::Rename { from, to });
            }
            Edit::Mkdir { path } => {
                let path = norm(path)?;
                folders.extend(ancestors_of(&path));
                folders.insert(path.clone());
                ops.push(Op::Mkdir { path });
            }
            Edit::Put { path, from } => {
                let path = norm(path)?;
                let meta =
                    std::fs::metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
                folders.extend(ancestors_of(&path));
                if !meta.is_dir() {
                    put(&mut ops, path, from.clone())?;
                    continue;
                }
                let (files, empty) =
                    ps5upload_fpkg::source::scan_tree(from).map_err(|e| e.to_string())?;
                if files.is_empty() && empty.is_empty() {
                    ops.push(Op::Mkdir { path: path.clone() });
                }
                for d in empty {
                    ops.push(Op::Mkdir {
                        path: format!("{path}/{d}"),
                    });
                }
                for f in files {
                    put(&mut ops, format!("{path}/{}", f.path), from.join(&f.path))?;
                }
            }
        }
    }
    Ok((ops, hosts))
}

/// The folder `path` is in ("" at the root).
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(up, _)| up)
}

/// Every folder above `path`, outermost first.
fn ancestors_of(path: &str) -> impl Iterator<Item = String> + '_ {
    path.match_indices('/')
        .map(move |(i, _)| path[..i].to_string())
}

/// Every folder of a listing: those above its files, and its empty ones.
fn folders_of(files: &[SourceFile], empty: &[String]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = files.iter().flat_map(|f| ancestors_of(&f.path)).collect();
    for d in empty.iter().filter_map(|d| browse::normalize(d)) {
        out.extend(ancestors_of(&d));
        if !d.is_empty() {
            out.insert(d);
        }
    }
    out
}

fn norm(raw: &str) -> Result<String, String> {
    match browse::normalize(raw) {
        Some(p) if !p.is_empty() => Ok(p),
        _ => Err(format!("{raw:?} is not a path inside the image")),
    }
}

/// Whether `a` and `b` name one file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A format editor's plan: the writes, or every finding against them; and the room it counted.
type Plan = (Result<Vec<Write>, Vec<String>>, Option<Room>);

/// The format editor's plan of `ops` against the image, reading it only. A panic in a parser
/// (a damaged image the checks missed) is a refusal, never a crash: nothing is written yet.
fn plan_writes(kind: Kind, image: &Path, ops: &[Op]) -> Result<Plan, String> {
    let mut file = File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let planned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
        Kind::Exfat => {
            let (p, s) = ps5_dump_forge_exfat::edit::plan_sized(&mut file, ops);
            (
                p,
                s.map(|s| Room {
                    needed: s.needed,
                    free: s.free,
                }),
            )
        }
        Kind::Ufs2 => {
            let (p, s) = ps5_dump_forge_ufs2::edit::plan_sized(&mut file, ops);
            (
                p,
                s.map(|s| Room {
                    needed: s.needed,
                    free: s.free,
                }),
            )
        }
        // PFS keeps no free count; an in-place `.ffpfs` edit only overwrites runs. Its "one
        // run" refusal is given once, not per op (as Forge's preflight does).
        Kind::Pfs => {
            let p = ps5_dump_forge_pfs::edit::plan(&mut file, ops).map_err(|mut f| {
                let mut seen = false;
                f.retain(|f| {
                    f != ps5_dump_forge_pfs::edit::NO_ROOM || !std::mem::replace(&mut seen, true)
                });
                f
            });
            (p, None)
        }
    }));
    planned.map_err(|_| {
        format!(
            "{} could not be read for an in-place edit (its {} metadata looks damaged)",
            image.display(),
            kind.name()
        )
    })
}

/// Scan and preflight of an in-place edit of `image` (the caller holds its [`Claim`]): a
/// journal left by an interrupted edit is settled, the image's kind checked, the edits applied
/// to its tree (every edit must apply, as for an edited copy), the format editor's plan made
/// and room beside the image for the undo journal checked. Writes nothing but a recovery.
pub(crate) fn prepare(image: &Path, edits: &[Edit]) -> Result<Prepared, Refusal> {
    if edits.is_empty() {
        return Err(Refusal::new("there are no changes to apply"));
    }
    let recovery = settle(image).map_err(Refusal::new)?;
    let stamp = Stamp::of(image).map_err(|e| Refusal::new(format!("{}: {e}", image.display())))?;
    let opened = browse::open(image).map_err(|e| Refusal::new(e.to_string()))?;
    let kind =
        match (opened.filesystem, opened.compressed) {
            (_, true) => {
                return Err(Refusal::new(
                    "a compressed image (.ffpfsc) can't be changed in place: save an edited copy \
                 instead",
                ))
            }
            (Filesystem::Exfat, false) => Kind::Exfat,
            (Filesystem::Ufs2, false) => Kind::Ufs2,
            (Filesystem::Pfs, false) => Kind::Pfs,
            (Filesystem::Pkg, false) => return Err(Refusal::new(
                "a package can't be edited here: unpack it to a folder, change that, and build a \
                 new package with Convert",
            )),
        };
    let mut tree = opened.tree;
    // A game in asset packs keeps its packs' own file list (and ampr_emu.index lists what is
    // inside them): it is only saved as an edited copy.
    match ps5upload_fpkg::source::packed_kind(&mut *tree) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(Refusal::new(
                "a game in LZ4 or zstd asset packs can't be changed in place: save an edited \
                 copy instead",
            ))
        }
        Err(e) => return Err(Refusal::new(format!("reading its pack index: {e}"))),
    }
    let before = listing_without_index(tree.files());
    let folders = folders_of(tree.files(), tree.empty_dirs());
    let mut expected = Edited::for_filesystem(tree, edits, opened.filesystem)
        .map_err(|e| Refusal::new(e.to_string()))?;
    let mut changed = expected.changed_files();
    let (mut ops, hosts) = ops_for(edits, folders).map_err(Refusal::new)?;
    // A libSceAmpr game's ampr_emu.index must list every file at its size (#449), as an edited
    // copy's is regenerated: a fresh one goes in when the edits change the file set, a size, or
    // the index itself (Forge's `fresh_index`).
    let index = fresh_ampr_index(&mut expected, &before, &ops).map_err(Refusal::new)?;
    let mut listing: Vec<SourceFile> = expected.files().to_vec();
    if let Some(bytes) = &index {
        let name = ps5upload_fpkg::ampr_index::NAME;
        ops.push(Op::Put {
            path: name.into(),
            len: bytes.len() as u64,
            data: Data::Bytes(bytes.clone()),
        });
        listing.retain(|f| f.path != name);
        listing.push(SourceFile {
            path: name.into(),
            size: bytes.len() as u64,
        });
        listing.sort_by(|a, b| a.path.cmp(&b.path));
        changed.push(name.into());
    }
    let empty_dirs: BTreeSet<String> = expected.empty_dirs().iter().cloned().collect();
    // An add of the image itself (or of a folder holding it) would read it while it changes.
    if let Some(h) = hosts.iter().find(|h| same_file(&h.path, image)) {
        return Err(Refusal::new(format!(
            "{} is the image being edited",
            h.path.display()
        )));
    }
    let (planned, room) = plan_writes(kind, image, &ops).map_err(Refusal::new)?;
    let writes = planned.map_err(|f| {
        Refusal::new(format!(
            "these changes can't go into the image in place: {}",
            f.join("; ")
        ))
    })?;
    let journal_bytes = (writes.iter())
        .filter(|w| w.live && !w.data.is_empty())
        .fold(JOURNAL_FRAME, |sum, w| {
            sum.saturating_add(JOURNAL_ENTRY + w.data.len())
        });
    let dir = match image.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if let Some(free) = ps5upload_fpkg::build::free_bytes(&dir) {
        let need = journal_bytes.saturating_add(JOURNAL_MARGIN);
        if free < need {
            return Err(Refusal::new(format!(
                "the undo journal needs {} MB beside the image, {} has {} MB free",
                need.div_ceil(1_000_000),
                dir.display(),
                free / 1_000_000
            )));
        }
    }
    Ok(Prepared {
        kind,
        image: image.to_path_buf(),
        stamp,
        writes,
        hosts,
        expected,
        listing,
        empty_dirs,
        index,
        changed,
        journal_bytes,
        room,
        recovery,
    })
}

/// Every (path, size) of a listing but `ampr_emu.index`.
fn listing_without_index(files: &[SourceFile]) -> BTreeSet<(String, u64)> {
    let name = ps5upload_fpkg::ampr_index::NAME;
    (files.iter())
        .filter(|f| !f.path.eq_ignore_ascii_case(name))
        .map(|f| (f.path.clone(), f.size))
        .collect()
}

/// A fresh `ampr_emu.index` for the edited tree, when the game reads through libSceAmpr (its
/// `eboot.bin` imports it, or it ships an index) and the edits change its file set or a size,
/// or name the index itself; `None` otherwise (the index, if any, is right as it is).
fn fresh_ampr_index(
    edited: &mut Edited,
    before: &BTreeSet<(String, u64)>,
    ops: &[Op],
) -> Result<Option<Vec<u8>>, String> {
    let name = ps5upload_fpkg::ampr_index::NAME;
    let named = |p: &str| p.eq_ignore_ascii_case(name);
    let touched = ops.iter().any(|op| match op {
        Op::Delete { path } | Op::Mkdir { path } | Op::Put { path, .. } => named(path),
        Op::Rename { from, to } => named(from) || named(to),
    });
    let after = listing_without_index(edited.files());
    if !touched && after == *before {
        return Ok(None);
    }
    let had = edited.files().iter().any(|f| named(&f.path));
    if !had && !ps5upload_fpkg::source::imports_ampr(edited, "eboot.bin") {
        return Ok(None);
    }
    let files: Vec<SourceFile> = (edited.files().iter())
        .filter(|f| !named(&f.path))
        .cloned()
        .collect();
    let mtime = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let listed = ps5upload_fpkg::ampr_index::listing(&files);
    ps5upload_fpkg::ampr_index::build(&listed, mtime)
        .map(Some)
        .map_err(|e| format!("{name}: {e}"))
}

// ─── Applying ───────────────────────────────────────────────────────────────────────────

/// The host files an apply copies ([`Data::Source`] ids index `files`), each checked to be the
/// file the edit was planned with (size and modification time) before it is read.
struct Hosts<'a> {
    files: &'a [Host],
    open: Option<(u32, File)>,
}

impl Hosts<'_> {
    fn read(&mut self, id: u32, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let host = (self.files.get(id as usize))
            .ok_or_else(|| io::Error::other(format!("no host file {id}")))?;
        let with_path =
            |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", host.path.display()));
        if offset.saturating_add(buf.len() as u64) > host.size {
            return Err(io::Error::other(format!(
                "{}: a read past its {} bytes",
                host.path.display(),
                host.size
            )));
        }
        if self.open.as_ref().is_none_or(|(at, _)| *at != id) {
            self.open = None;
            self.open = Some((id, File::open(&host.path).map_err(with_path)?));
        }
        let Some((_, file)) = self.open.as_mut() else {
            return Err(io::Error::other("no open host file"));
        };
        let m = file.metadata().map_err(with_path)?;
        if m.len() != host.size || m.modified().ok() != host.modified {
            return Err(io::Error::other(format!(
                "{} changed after it was added (its size or modification time); add it again",
                host.path.display()
            )));
        }
        file.seek(SeekFrom::Start(offset)).map_err(with_path)?;
        file.read_exact(buf).map_err(with_path)
    }
}

/// What a finished in-place edit did.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub files: u64,
    pub written: u64,
    pub checked: u64,
    pub journal_bytes: u64,
    pub notes: Vec<String>,
    /// The edit is done, but something after it did not go as it should (said in plain words).
    pub warning: Option<String>,
}

impl Outcome {
    /// The finished job's `commit_ack`: what was written and checked, and `warning` when there
    /// is one.
    pub(crate) fn ack(&self) -> serde_json::Value {
        let mut ack = serde_json::json!({
            "in_place": true,
            "written_bytes": self.written,
            "checked_bytes": self.checked,
            "journal_bytes": self.journal_bytes,
        });
        if let (Some(w), Some(o)) = (&self.warning, ack.as_object_mut()) {
            o.insert("warning".into(), w.clone().into());
        }
        ack
    }
}

/// Where an apply stands, for the job's progress: (stage, done, total).
pub(crate) type Progress<'a> = &'a mut dyn FnMut(&str, u64, u64);

/// Test hooks: where an apply stops as if the engine died there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum Stop {
    /// A normal run.
    Never,
    /// Inside `patch::apply` (the journal, the free-space writes, a live write).
    #[cfg(test)]
    At(patch::Step),
    /// Applied and verified, the journal not yet deleted.
    #[cfg(test)]
    BeforeCommit,
    /// A normal run whose sync of the journal's folder after its removal fails.
    #[cfg(test)]
    DirSyncFails,
}

/// Hashes every file of `paths` in `tree` (blake3), in pieces.
fn hash_files(
    tree: &mut dyn SourceTree,
    paths: &[(String, u64)],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<BTreeMap<String, [u8; 32]>, String> {
    let mut out = BTreeMap::new();
    for (path, size) in paths {
        let mut h = blake3::Hasher::new();
        let mut at = 0u64;
        while at < *size {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let n = (*size - at).min(PIECE as u64) as usize;
            let piece = tree
                .read_range(path, at, n)
                .map_err(|e| format!("{path}: {e}"))?;
            if piece.len() != n {
                return Err(format!(
                    "{path}: read {} bytes at {at}, wanted {n}",
                    piece.len()
                ));
            }
            h.update(&piece);
            at += n as u64;
            progress(n as u64);
        }
        out.insert(path.clone(), *h.finalize().as_bytes());
    }
    Ok(out)
}

/// An applied edit not yet kept: dropped without [`Pending::commit`] (an error or a panic on
/// the way) it rolls the image back (Forge's `Pending`).
struct Pending {
    applied: Option<Applied>,
}

impl Pending {
    fn rollback(mut self) -> io::Result<()> {
        match self.applied.take() {
            Some(applied) => applied.rollback(SYNCS),
            None => Ok(()),
        }
    }

    /// Keeps the edit: the undo journal is deleted, then its folder synced. A journal that
    /// can't be deleted is still complete, so the image is restored from it (an error). A
    /// failed folder sync after the delete can't be undone: the edit is done, with a warning
    /// (`Ok(Some(..))`) saying so in plain words.
    fn commit(
        mut self,
        image: &Path,
        dir_sync: fn(&Path) -> io::Result<()>,
    ) -> Result<Option<String>, String> {
        let Some(applied) = self.applied.take() else {
            return Ok(None);
        };
        let no_dir = Syncs {
            file: SYNCS.file,
            dir: |_| Ok(()),
        };
        if let Err(e) = applied.commit(no_dir) {
            return Err(match patch::recover(image, SYNCS) {
                Ok(_) => format!("{e}; the image was restored"),
                Err(r) => format!(
                    "{e}, and restoring the image failed ({r}): its undo journal stays, and the \
                     next time the image is opened here it is restored"
                ),
            });
        }
        let folder = match image.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        Ok(dir_synced(folder, dir_sync(folder)))
    }
}

/// The warning a failed sync of `folder` after the journal's removal leaves: the edit is in
/// the image and checked; only the removal of its undo journal may not have reached the disk.
fn dir_synced(folder: &Path, synced: io::Result<()>) -> Option<String> {
    synced.err().map(|e| {
        format!(
            "the changes are in the image and were checked, but the computer could not confirm \
             that its undo journal's removal was saved to disk ({}: {e}). If the computer \
             crashes before that happens, the next time the image is opened here it is put \
             back as it was before this edit",
            folder.display()
        )
    })
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(applied) = self.applied.take() {
            let _ = applied.rollback(SYNCS);
        }
    }
}

/// Writes a [`prepare`]d edit into the image and checks it; see the module docs. The caller
/// holds the image's [`Claim`]. On any failure after the first write the image is restored
/// from the journal before this returns, and the error says so.
pub(crate) fn apply(
    prepared: Prepared,
    full_verify: bool,
    cancel: &AtomicBool,
    progress: Progress,
) -> Result<Outcome, String> {
    apply_stopping(prepared, full_verify, cancel, progress, Stop::Never)
}

fn apply_stopping(
    prepared: Prepared,
    full_verify: bool,
    cancel: &AtomicBool,
    progress: Progress,
    stop: Stop,
) -> Result<Outcome, String> {
    let Prepared {
        kind,
        image,
        stamp,
        writes,
        hosts,
        mut expected,
        listing,
        empty_dirs,
        index,
        changed,
        journal_bytes,
        ..
    } = prepared;
    // What the image must read back as, hashed now from where each file comes from (the image
    // as it is, the added files; a fresh index from its bytes): a write that damaged one is
    // caught.
    let index_name = ps5upload_fpkg::ampr_index::NAME;
    let check: Vec<(String, u64)> = {
        let changed: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
        (listing.iter())
            .filter(|f| full_verify || changed.contains(f.path.as_str()))
            .map(|f| (f.path.clone(), f.size))
            .collect()
    };
    let from_tree: Vec<(String, u64)> = (check.iter())
        .filter(|(p, _)| index.is_none() || p != index_name)
        .cloned()
        .collect();
    let check_total: u64 = check.iter().map(|(_, s)| s).sum();
    let mut done = 0u64;
    progress("check", 0, check_total);
    let mut want = hash_files(&mut expected, &from_tree, cancel, &mut |n| {
        done += n;
        progress("check", done, check_total);
    })?;
    if let Some(bytes) = &index {
        want.insert(index_name.into(), *blake3::hash(bytes).as_bytes());
    }
    // The image's read handle is not needed past here.
    drop(expected);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled; nothing was written".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&image)
        .map_err(|e| format!("opening {} to edit it: {e}", image.display()))?;
    // Another program (or another PS5Upload) writing it would wreck the edit.
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(format!(
                "{} is open in another program; close it and try again",
                image.display()
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(format!("locking {}: {e}", image.display()))
        }
    }
    if Stamp::of(&image).ok().as_ref() != Some(&stamp) {
        return Err(format!(
            "{} changed after the edit was planned; nothing was written",
            image.display()
        ));
    }
    let written: u64 = writes.iter().map(|w| w.data.len()).sum();
    progress("write", 0, written.max(1));
    let mut source = Hosts {
        files: &hosts,
        open: None,
    };
    // The added files' bytes are what take the time: reported as they are read in, at most
    // every 64 MiB or 250 ms.
    let total_write = written.max(1);
    let (mut read_in, mut shown, mut shown_at) = (0u64, 0u64, std::time::Instant::now());
    let mut read_source = |id, offset, buf: &mut [u8]| {
        source.read(id, offset, buf)?;
        read_in = read_in.saturating_add(buf.len() as u64).min(total_write);
        if read_in - shown >= 64 << 20
            || shown_at.elapsed() >= std::time::Duration::from_millis(250)
        {
            (shown, shown_at) = (read_in, std::time::Instant::now());
            progress("write", read_in, total_write);
        }
        Ok(())
    };
    let applied = match stop {
        Stop::Never => patch::apply(&mut file, &image, &writes, &mut read_source, cancel, SYNCS),
        #[cfg(test)]
        Stop::BeforeCommit | Stop::DirSyncFails => {
            patch::apply(&mut file, &image, &writes, &mut read_source, cancel, SYNCS)
        }
        #[cfg(test)]
        Stop::At(step) => patch::apply_until_step(
            &mut file,
            &image,
            &writes,
            &mut read_source,
            cancel,
            SYNCS,
            step,
        ),
    };
    let applied = applied.map_err(|e| {
        if e.kind() == io::ErrorKind::Interrupted {
            "cancelled; the image is as it was".to_string()
        } else {
            format!("editing {}: {e}", image.display())
        }
    })?;
    #[cfg(test)]
    if let Stop::At(_) = stop {
        // Past the last live write: the engine "dies" with the journal still there (an
        // `Applied` dropped without commit or rollback leaves it for `recover`).
        drop(applied);
        return Err("simulated crash".into());
    }
    // From here every way out but a commit restores the image, a panic included.
    let pending = Pending {
        applied: Some(applied),
    };
    // Everything is written and synced: the OS lock goes before the checks. On Windows it is
    // mandatory, and the checks (the format editor's plan, the image's reader) and a recovery
    // open the image again by its path, which a held lock would refuse: every edit would roll
    // back. The in-place claim stays until the job ends, so nothing in this engine touches the
    // image meanwhile. Should the unlock fail, the checks fail and the edit is rolled back
    // through the handle the apply kept.
    let _ = file.unlock();
    progress("write", written.max(1), written.max(1));
    let checked = verify(
        kind,
        &image,
        &listing,
        &empty_dirs,
        &check,
        &want,
        cancel,
        progress,
    )
    .and_then(|()| {
        // A cancel seen during the last read still undoes the edit.
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        Ok(())
    });
    if let Err(e) = checked {
        let what = if cancel.load(Ordering::Relaxed) {
            "cancelled; the image was restored"
        } else {
            "the edit did not verify; the image was restored"
        };
        return Err(match pending.rollback() {
            Ok(()) => format!("{what}: {e}"),
            Err(r) => format!(
                "the edit was not kept ({e}), and restoring the image failed ({r}): its undo \
                 journal {} stays, and the next time the image is opened here it is restored",
                patch::journal_path(&image).display()
            ),
        });
    }
    #[cfg(test)]
    if stop == Stop::BeforeCommit {
        // The engine "dies" here: the journal stays (no rollback, no commit).
        let mut pending = pending;
        drop(pending.applied.take());
        return Err("simulated crash before the commit".into());
    }
    #[cfg(not(test))]
    let dir_sync = SYNCS.dir;
    #[cfg(test)]
    let dir_sync: fn(&Path) -> io::Result<()> = if stop == Stop::DirSyncFails {
        |_| Err(io::Error::other("simulated sync failure"))
    } else {
        SYNCS.dir
    };
    let warning = pending.commit(&image, dir_sync)?;
    drop(file);
    let mut notes = vec![format!(
        "edited {} in place ({}): {written} bytes written, {} files read back, a {journal_bytes} \
         byte undo journal",
        image.display(),
        kind.name(),
        check.len()
    )];
    if full_verify {
        notes.push("every file read back".into());
    }
    Ok(Outcome {
        files: listing.len() as u64,
        written,
        checked: check_total,
        journal_bytes,
        notes,
        warning,
    })
}

/// Tests elsewhere in the engine: `edits` written into `image` and verified, then the engine
/// "dies" before the undo journal is deleted, leaving the image edited and the journal beside it
/// (what the next open restores).
#[cfg(test)]
pub(crate) fn crash_before_commit(image: &Path, edits: &[Edit]) {
    let _claim = Claim::take(image).expect("the image is free");
    let p = prepare(image, edits).unwrap_or_else(|r| panic!("{r:?}"));
    let e = apply_stopping(
        p,
        false,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
        Stop::BeforeCommit,
    )
    .unwrap_err();
    assert!(e.contains("simulated crash"), "{e}");
    assert!(patch::journal_path(image).exists());
}

/// The edited image against what it must be: the format editor reopens it (its consistency
/// checks), its reader lists exactly `listing` and `empty_dirs`, and each file of `check`
/// hashes as `want`.
#[allow(clippy::too_many_arguments)]
fn verify(
    kind: Kind,
    image: &Path,
    listing: &[SourceFile],
    empty_dirs: &BTreeSet<String>,
    check: &[(String, u64)],
    want: &BTreeMap<String, [u8; 32]>,
    cancel: &AtomicBool,
    progress: Progress,
) -> Result<(), String> {
    let total: u64 = check.iter().map(|(_, s)| s).sum();
    progress("verify", 0, total);
    // The editor's own read of the metadata: a plan of no change opens and cross-checks it.
    match plan_writes(kind, image, &[])? {
        (Ok(_), _) => {}
        (Err(f), _) => {
            return Err(format!(
                "the {} metadata does not check out: {}",
                kind.name(),
                f.join("; ")
            ))
        }
    }
    let mut tree = browse::open(image)
        .map_err(|e| format!("reading the edited image: {e}"))?
        .tree;
    let got: BTreeMap<&str, u64> = (tree.files().iter())
        .map(|f| (f.path.as_str(), f.size))
        .collect();
    let wanted: BTreeMap<&str, u64> = listing.iter().map(|f| (f.path.as_str(), f.size)).collect();
    if got != wanted {
        let missing: Vec<&str> = (wanted.iter())
            .filter(|(p, s)| got.get(*p) != Some(s))
            .map(|(p, _)| *p)
            .take(5)
            .collect();
        let extra: Vec<&str> = (got.keys())
            .filter(|p| !wanted.contains_key(*p))
            .copied()
            .take(5)
            .collect();
        return Err(format!(
            "the image lists other files than expected (missing or resized: {missing:?}; \
             unexpected: {extra:?})"
        ));
    }
    let dirs: BTreeSet<String> = (tree.empty_dirs().iter())
        .filter_map(|d| browse::normalize(d))
        .filter(|d| !d.is_empty())
        .collect();
    let want_dirs: BTreeSet<String> = (empty_dirs.iter())
        .filter_map(|d| browse::normalize(d))
        .filter(|d| !d.is_empty())
        .collect();
    if dirs != want_dirs {
        return Err(format!(
            "the image lists other empty folders than expected ({:?} instead of {:?})",
            dirs.iter().take(5).collect::<Vec<_>>(),
            want_dirs.iter().take(5).collect::<Vec<_>>()
        ));
    }
    let mut done = 0u64;
    let got = hash_files(tree.as_mut(), check, cancel, &mut |n| {
        done += n;
        progress("verify", done, total);
    })?;
    if let Some((path, _)) = want.iter().find(|(p, h)| got.get(*p) != Some(h)) {
        return Err(format!(
            "{path} reads back different from what was put there"
        ));
    }
    Ok(())
}

// ─── HTTP ───────────────────────────────────────────────────────────────────────────────

/// `POST /api/image/edit/check` — whether these edits can go into the image in place, without
/// writing anything (but settling a journal an interrupted edit left).
#[derive(serde::Deserialize)]
pub(crate) struct CheckReq {
    /// The image, on this computer.
    path: String,
    /// The changes, as `/api/exfat/build` takes them (`edits`).
    #[serde(default)]
    edits: Vec<crate::fpkg_api::ImageEditReq>,
}

#[derive(serde::Serialize, Debug, PartialEq)]
pub(crate) struct CheckResp {
    /// The changes can be applied to the image itself (`in_place: true` on the build).
    in_place: bool,
    /// Why not, in a sentence for the user (when `in_place` is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The image is in use (another edit, a build reading it): try again later.
    busy: bool,
    /// `exfat`, `ufs2` or `pfs`, when the image can be edited in place at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    filesystem: Option<&'static str>,
    /// What the changes take inside the image and what it has free, in bytes (exFAT, UFS2).
    #[serde(skip_serializing_if = "Option::is_none")]
    needed_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    free_bytes: Option<u64>,
    /// The undo journal written beside the image while the change goes in.
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_bytes: Option<u64>,
}

/// The check for `image` and `edits`, off the async runtime.
fn check_blocking(image: &Path, edits: &[Edit]) -> CheckResp {
    let refused = |r: Refusal| CheckResp {
        in_place: false,
        reason: Some(r.reason),
        busy: r.busy,
        filesystem: None,
        needed_bytes: None,
        free_bytes: None,
        journal_bytes: None,
    };
    let _claim = match Claim::take(image) {
        Ok(c) => c,
        Err(why) => return refused(Refusal::busy(why)),
    };
    match prepare(image, edits) {
        Ok(p) => CheckResp {
            in_place: true,
            reason: None,
            busy: false,
            filesystem: Some(match p.kind {
                Kind::Exfat => "exfat",
                Kind::Ufs2 => "ufs2",
                Kind::Pfs => "pfs",
            }),
            needed_bytes: p.room.map(|r| r.needed),
            free_bytes: p.room.map(|r| r.free),
            journal_bytes: Some(p.journal_bytes),
        },
        Err(r) => refused(r),
    }
}

pub(crate) async fn check_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    axum::Json(req): axum::Json<CheckReq>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let image = crate::fpkg_api::resolve_engine_path(&req.path);
    if ps5upload_fpkg::remote_source::is_remote(&image) || !image.is_absolute() {
        return crate::json_err(
            axum::http::StatusCode::BAD_REQUEST,
            "only an image on this computer can be edited in place",
        )
        .into_response();
    }
    if let Some(why) = other_job_reads(&state, &image) {
        return axum::Json(CheckResp {
            in_place: false,
            reason: Some(why),
            busy: true,
            filesystem: None,
            needed_bytes: None,
            free_bytes: None,
            journal_bytes: None,
        })
        .into_response();
    }
    let edits: Vec<Edit> = req.edits.iter().map(|e| e.edit()).collect();
    match tokio::task::spawn_blocking(move || check_blocking(&image, &edits)).await {
        Ok(resp) => axum::Json(resp).into_response(),
        Err(e) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("{e:#}"),
        )
        .into_response(),
    }
}

/// Why another running Workshop job keeps `image` from being edited: it reads the image, a
/// folder holding it, or something inside it.
fn other_job_reads(state: &crate::AppState, image: &Path) -> Option<String> {
    let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
    let runs = crate::workshop_api::live_runs(|id| {
        matches!(jobs.get(&id), Some(crate::JobState::Running { .. }))
    });
    let me = key(image);
    runs.iter()
        .find(|run| {
            let other = key(Path::new(&run.source));
            other.starts_with(&me) || me.starts_with(&other)
        })
        .map(|run| {
            format!(
                "a running job ({}) is reading {}: wait for it to finish",
                run.kind, run.source
            )
        })
}

/// `POST /api/exfat/build` with `in_place: true`: plans the edit now (a refusal is the
/// response, with the reason), then applies it as a Workshop job. The image is claimed from
/// before the plan to the job's end.
pub(crate) async fn start(
    state: crate::AppState,
    image: PathBuf,
    edits: Vec<Edit>,
    full_verify: bool,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if let Some(why) = other_job_reads(&state, &image) {
        return crate::json_err(StatusCode::CONFLICT, why).into_response();
    }
    let planned = tokio::task::spawn_blocking(move || {
        // Refused while a build reads the image: builds register before their first read, in
        // the same set, so a build and an edit never both start.
        let claim = Claim::take(&image).map_err(Refusal::busy)?;
        prepare(&image, &edits).map(|p| (claim, p))
    })
    .await;
    let (claim, prepared) = match planned {
        Ok(Ok(p)) => p,
        Ok(Err(r)) => {
            let code = if r.busy {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            return crate::json_err(code, format!("not applied in place: {}", r.reason))
                .into_response();
        }
        Err(e) => {
            return crate::json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                .into_response()
        }
    };
    let job_id = uuid::Uuid::new_v4();
    let image = prepared.image.clone();
    crate::workshop_api::note_run(job_id, "edit", &image);
    let cancel = crate::register_workshop_job(job_id);
    let started_at_ms = crate::now_ms();
    let (checked, written) = (prepared.check_bytes(full_verify), prepared.write_bytes());
    let total = checked * 2 + written;
    crate::set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        crate::JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: total,
            files: Default::default(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
        },
    );
    if prepared.recovery != Recovery::Clean {
        crate::log_at!(
            Info,
            "convert",
            "in-place edit of {}: an earlier interrupted edit was settled first ({:?})",
            image.display(),
            prepared.recovery
        );
    }
    crate::fpkg_api::spawn_workshop_job(move || {
        let _claim = claim;
        let mut fail = crate::JobFailOnDropGuard::new(
            state.jobs.clone(),
            state.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        let mut last = std::time::Instant::now();
        let mut report = |id: &str, done: u64, stage_total: u64| {
            // The stages before this one, for the job's one bar: hashing, writing, reading back.
            let base = match id {
                "check" => 0,
                "write" => checked,
                _ => checked + written,
            };
            let now = std::time::Instant::now();
            if done != 0 && done < stage_total && now.duration_since(last).as_millis() < 250 {
                return;
            }
            last = now;
            let index = STAGES.iter().position(|s| *s == id).unwrap_or(0) as u32;
            crate::set_job(
                &state.jobs,
                &state.events_tx,
                job_id,
                crate::JobState::Running {
                    stage: Some(crate::JobStage {
                        id: id.to_string(),
                        index,
                        count: STAGES.len() as u32,
                        done,
                        total: stage_total,
                    }),
                    started_at_ms,
                    bytes_sent: base + done,
                    total_bytes: total,
                    files: Default::default(),
                    skipped_files: 0,
                    skipped_bytes: 0,
                    files_processing: 0,
                    files_finalized: 0,
                    files_finalizing_total: 0,
                    bytes_finalized: 0,
                },
            );
        };
        let ended = apply(prepared, full_verify, &cancel, &mut report);
        let completed_at_ms = crate::now_ms();
        let elapsed_ms = completed_at_ms.saturating_sub(started_at_ms);
        // The browser's listing of the image is stale now.
        crate::image_browse::forget(&image);
        let state_now = match ended {
            Ok(o) => {
                for note in &o.notes {
                    crate::engine_log::record("info", format!("in-place edit: {note}"));
                }
                if let Some(w) = &o.warning {
                    crate::engine_log::record(
                        "warn",
                        format!("in-place edit of {}: {w}", image.display()),
                    );
                }
                crate::JobState::Done {
                    started_at_ms,
                    completed_at_ms,
                    elapsed_ms,
                    tx_id_hex: String::new(),
                    bytes_sent: o.written,
                    dest: image.display().to_string(),
                    files_sent: o.files,
                    skipped_files: 0,
                    skipped_bytes: 0,
                    commit_ack: Some(o.ack()),
                }
            }
            Err(error) => {
                crate::engine_log::record(
                    "warn",
                    format!("in-place edit of {} failed: {error}", image.display()),
                );
                crate::JobState::Failed {
                    started_at_ms,
                    completed_at_ms,
                    elapsed_ms,
                    error,
                    error_reason: None,
                    error_detail: None,
                    error_console: None,
                    error_space: None,
                }
            }
        };
        crate::set_job(&state.jobs, &state.events_tx, job_id, state_now);
        fail.mark_succeeded();
    });
    (
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "job_id": job_id.to_string() })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::FolderSource;
    use std::sync::atomic::AtomicUsize;

    /// A folder under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ps5upload-inplace-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const AMPR_EBOOT: &[u8] = b"\x7fELF fake eboot importing libSceAmpr.sprx";

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// A small game folder; `eboot` its eboot.bin.
    fn game(root: &Path, eboot: &[u8]) -> PathBuf {
        let g = root.join("PPSA99999");
        put(&g, "eboot.bin", eboot);
        put(&g, "sce_sys/param.json", b"{\"titleId\":\"PPSA99999\"}");
        put(&g, "data/sub/a.bin", b"hello");
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        put(&g, "data/big.bin", &big);
        std::fs::create_dir_all(g.join("data/empty")).unwrap();
        g
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Fmt {
        Exfat,
        Ufs2,
        Pfs,
    }

    /// `src` written as an image of `fmt` by the writer PS5Upload's own builds use: its exFAT
    /// writer (`exfat_write`), the vendored Forge UFS2 and PFS writers.
    fn image(src: &Path, dir: &Path, fmt: Fmt) -> PathBuf {
        let cancel = AtomicBool::new(false);
        let mut tree = FolderSource::open(src).unwrap();
        let out = match fmt {
            Fmt::Exfat => dir.join("game.exfat"),
            Fmt::Ufs2 => dir.join("game.ffpkg"),
            Fmt::Pfs => dir.join("game.ffpfs"),
        };
        match fmt {
            Fmt::Exfat => {
                ps5upload_fpkg::exfat_write::build_exfat(
                    &mut tree,
                    &out,
                    None,
                    &mut |_, _| {},
                    &|| false,
                )
                .unwrap();
            }
            Fmt::Ufs2 => {
                let layout =
                    ps5_dump_forge_ufs2::plan(&tree, &Default::default(), &cancel).unwrap();
                let mut f = File::create(&out).unwrap();
                ps5_dump_forge_ufs2::write(&mut tree, &layout, &mut f, &cancel, &mut |_, _| {})
                    .unwrap();
            }
            Fmt::Pfs => {
                let layout = ps5_dump_forge_pfs::plan(&tree, &Default::default(), &cancel).unwrap();
                let mut f = File::create(&out).unwrap();
                ps5_dump_forge_pfs::write(&mut tree, &layout, &mut f, &cancel, &mut |_, _| {})
                    .unwrap();
            }
        }
        out
    }

    /// Every file of the image with its bytes, and its empty folders, through its reader.
    fn snapshot(image: &Path) -> (BTreeMap<String, Vec<u8>>, BTreeSet<String>) {
        let mut tree = browse::open(image).unwrap().tree;
        let paths: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
        let files = paths
            .into_iter()
            .map(|p| {
                let b = tree.read(&p).unwrap();
                (p, b)
            })
            .collect();
        let dirs = tree.empty_dirs().iter().cloned().collect();
        (files, dirs)
    }

    /// The editor's own check: the volume reopens, consistent and clean.
    fn consistent(image: &Path, fmt: Fmt) {
        let kind = match fmt {
            Fmt::Exfat => Kind::Exfat,
            Fmt::Ufs2 => Kind::Ufs2,
            Fmt::Pfs => Kind::Pfs,
        };
        let (planned, _) = plan_writes(kind, image, &[]).unwrap();
        planned.unwrap_or_else(|f| panic!("{fmt:?}: {f:?}"));
    }

    fn no_progress() -> impl FnMut(&str, u64, u64) {
        |_, _, _| {}
    }

    const PATCH: &[u8] = b"a patched eboot, shorter than the old one";

    /// Every kind of change, with its host files in `root`.
    fn edits(root: &Path) -> Vec<Edit> {
        put(root, "host/patch.bin", PATCH);
        put(root, "host/added/deep/new.txt", b"new file");
        std::fs::create_dir_all(root.join("host/added/hollow")).unwrap();
        vec![
            Edit::Put {
                path: "eboot.bin".into(),
                from: root.join("host/patch.bin"),
            },
            Edit::Put {
                path: "data/added".into(),
                from: root.join("host/added"),
            },
            Edit::Rename {
                from: "data/sub/a.bin".into(),
                to: "data/moved/hello.bin".into(),
            },
            Edit::Mkdir {
                path: "data/made".into(),
            },
            Edit::Delete {
                path: "data/empty".into(),
            },
            Edit::Delete {
                path: "data/big.bin".into(),
            },
        ]
    }

    /// The image after [`edits`]: every file and every empty folder.
    fn assert_edited(image: &Path) {
        let (files, dirs) = snapshot(image);
        let got: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let want: Vec<(&str, &[u8])> = vec![
            ("data/added/deep/new.txt", b"new file"),
            ("data/moved/hello.bin", b"hello"),
            ("eboot.bin", PATCH),
            ("sce_sys/param.json", b"{\"titleId\":\"PPSA99999\"}"),
        ];
        assert_eq!(got, want);
        let got_dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        assert_eq!(got_dirs, ["data/added/hollow", "data/made", "data/sub"]);
    }

    /// exFAT (PS5Upload's own writer) and UFS2: every kind of change goes in, reads back, and
    /// leaves no journal; the filesystem checks out; the image never grows.
    #[test]
    fn edits_go_into_the_image_itself() {
        for fmt in [Fmt::Exfat, Fmt::Ufs2] {
            let tmp = Scratch::new("apply");
            let src = game(&tmp.0, &[7u8; 70_000]);
            let img = image(&src, &tmp.0, fmt);
            let len = std::fs::metadata(&img).unwrap().len();
            let p = prepare(&img, &edits(&tmp.0)).unwrap_or_else(|r| panic!("{fmt:?}: {r:?}"));
            assert!(p.room.is_some_and(|r| r.needed <= r.free), "{fmt:?}");
            let out = apply(p, false, &AtomicBool::new(false), &mut no_progress())
                .unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
            assert_eq!(out.files, 4);
            assert!(!patch::journal_path(&img).exists());
            assert_eq!(std::fs::metadata(&img).unwrap().len(), len, "never grows");
            assert_edited(&img);
            consistent(&img, fmt);
            // And again, on the edited image, every file read back.
            let more = vec![Edit::Delete {
                path: "data/moved".into(),
            }];
            let p = prepare(&img, &more).unwrap();
            apply(p, true, &AtomicBool::new(false), &mut no_progress()).unwrap();
            let (files, _) = snapshot(&img);
            assert!(!files.contains_key("data/moved/hello.bin"));
            consistent(&img, fmt);
        }
    }

    /// PFS takes deletes, renames and a replacement that fits the file's run; a new file is
    /// refused (a .ffpfs keeps every file in one run) and the image left as it was.
    #[test]
    fn pfs_takes_what_fits_its_runs() {
        let tmp = Scratch::new("pfs");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Pfs);
        put(&tmp.0, "host/eboot.bin", &[9u8; 60_000]);
        let ok = vec![
            Edit::Delete {
                path: "data/big.bin".into(),
            },
            Edit::Rename {
                from: "data/sub/a.bin".into(),
                to: "data/sub/b.bin".into(),
            },
            Edit::Put {
                path: "eboot.bin".into(),
                from: tmp.0.join("host/eboot.bin"),
            },
        ];
        let p = prepare(&img, &ok).unwrap_or_else(|r| panic!("{r:?}"));
        apply(p, false, &AtomicBool::new(false), &mut no_progress()).unwrap();
        let (files, _) = snapshot(&img);
        assert_eq!(files["eboot.bin"], vec![9u8; 60_000]);
        assert_eq!(files["data/sub/b.bin"], b"hello");
        assert!(!files.contains_key("data/big.bin") && !files.contains_key("data/sub/a.bin"));
        consistent(&img, Fmt::Pfs);
        let before = std::fs::read(&img).unwrap();
        put(&tmp.0, "host/new.bin", b"new");
        let add = vec![Edit::Put {
            path: "data/new.bin".into(),
            from: tmp.0.join("host/new.bin"),
        }];
        let r = prepare(&img, &add).err().expect("refused");
        assert!(!r.busy && r.reason.contains("one run"), "{r:?}");
        assert!(std::fs::read(&img).unwrap() == before);
        assert!(!patch::journal_path(&img).exists());
    }

    /// A change bigger than the image's free space is refused with the room it needs, and the
    /// image file is not touched (not even its modification time), nor a journal made.
    #[test]
    fn a_change_that_does_not_fit_is_refused_untouched() {
        for fmt in [Fmt::Exfat, Fmt::Ufs2] {
            let tmp = Scratch::new("nofit");
            let src = game(&tmp.0, &[7u8; 70_000]);
            let img = image(&src, &tmp.0, fmt);
            let len = std::fs::metadata(&img).unwrap().len();
            put(
                &tmp.0,
                "host/huge.bin",
                &vec![1u8; len as usize + (4 << 20)],
            );
            let before = std::fs::read(&img).unwrap();
            let stamp = Stamp::of(&img).unwrap();
            let huge = vec![Edit::Put {
                path: "data/huge.bin".into(),
                from: tmp.0.join("host/huge.bin"),
            }];
            let r = prepare(&img, &huge).err().expect("refused");
            assert!(!r.busy, "{r:?}");
            assert!(r.reason.contains("MB free"), "{fmt:?}: {}", r.reason);
            assert!(std::fs::read(&img).unwrap() == before);
            assert_eq!(Stamp::of(&img).unwrap(), stamp);
            assert!(!patch::journal_path(&img).exists());
            // The check says the same, and that it is no busy image.
            let c = check_blocking(&img, &huge);
            assert!(!c.in_place && !c.busy && c.reason.is_some());
        }
    }

    /// The apply cut at every journal step (the journal half written, after the free-space
    /// writes, after each live write, all written but the journal not deleted): the next open
    /// settles the journal and the image reads back exactly as it was, consistent; the same
    /// edit run whole reads back fully new.
    #[test]
    fn a_crash_at_every_step_leaves_the_image_old_or_new() {
        for fmt in [Fmt::Exfat, Fmt::Ufs2] {
            let tmp = Scratch::new("crash");
            let src = game(&tmp.0, &[7u8; 70_000]);
            let pristine = image(&src, &tmp.0, fmt);
            let original = snapshot(&pristine);
            let edits = edits(&tmp.0);
            let live = prepare(&pristine, &edits)
                .unwrap()
                .writes
                .iter()
                .filter(|w| w.live && !w.data.is_empty())
                .count();
            assert!(live > 2, "{fmt:?}: {live} live writes");
            let mut stops = vec![
                Stop::At(patch::Step::JournalHalf),
                Stop::At(patch::Step::Free),
                Stop::At(patch::Step::LiveHalf),
            ];
            stops.extend((0..=live).map(|n| Stop::At(patch::Step::LiveAfter(n))));
            stops.push(Stop::BeforeCommit);
            let bytes = std::fs::read(&pristine).unwrap();
            let ext = pristine.extension().unwrap().to_str().unwrap();
            let img = tmp.0.join(format!("cut.{ext}"));
            for stop in stops {
                std::fs::write(&img, &bytes).unwrap();
                let p = prepare(&img, &edits).unwrap();
                let e = apply_stopping(p, false, &AtomicBool::new(false), &mut no_progress(), stop)
                    .unwrap_err();
                assert!(e.contains("simulated crash"), "{fmt:?} {stop:?}: {e}");
                assert!(patch::journal_path(&img).exists(), "{fmt:?} {stop:?}");
                // The next open settles it.
                let r = before_open(&img).unwrap();
                let want = match stop {
                    Stop::At(patch::Step::JournalHalf) => Recovery::Discarded,
                    _ => Recovery::Restored,
                };
                assert_eq!(r, want, "{fmt:?} {stop:?}");
                assert!(!patch::journal_path(&img).exists());
                assert_eq!(
                    snapshot(&img),
                    original,
                    "{fmt:?} {stop:?}: not the old image"
                );
                consistent(&img, fmt);
                assert_eq!(before_open(&img).unwrap(), Recovery::Clean);
            }
            // Whole: fully new.
            std::fs::write(&img, &bytes).unwrap();
            let p = prepare(&img, &edits).unwrap();
            apply(p, false, &AtomicBool::new(false), &mut no_progress()).unwrap();
            assert_edited(&img);
        }
    }

    /// A file added from this computer that changes after the plan fails the apply before any
    /// live write: the image is as it was and the journal gone.
    #[test]
    fn a_host_file_changed_after_the_plan_writes_nothing_live() {
        let tmp = Scratch::new("host");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Exfat);
        let original = snapshot(&img);
        put(&tmp.0, "host/new.bin", &[5u8; 100_000]);
        let edits = vec![Edit::Put {
            path: "data/new.bin".into(),
            from: tmp.0.join("host/new.bin"),
        }];
        let p = prepare(&img, &edits).unwrap();
        put(&tmp.0, "host/new.bin", &[6u8; 90_000]);
        let e = apply(p, false, &AtomicBool::new(false), &mut no_progress()).unwrap_err();
        assert!(e.contains("changed"), "{e}");
        assert!(!patch::journal_path(&img).exists());
        assert_eq!(snapshot(&img), original);
        consistent(&img, Fmt::Exfat);
    }

    /// The OS lock is held while the edit is written and released before the checks, which
    /// open the image again by its path: on Windows a held lock is mandatory and would refuse
    /// them, and every edit would roll back. A second handle can lock the image as the checks
    /// start (a held `flock` refuses it here as Windows refuses the read).
    #[test]
    fn the_os_lock_is_released_before_the_checks_read_the_image() {
        let tmp = Scratch::new("unlock");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Exfat);
        let p = prepare(&img, &edits(&tmp.0)).unwrap();
        let mut locked_at_write = None;
        let mut locked_at_verify = None;
        let mut probe = |stage: &str, _: u64, _: u64| {
            let slot = match stage {
                "write" if locked_at_write.is_none() => &mut locked_at_write,
                "verify" if locked_at_verify.is_none() => &mut locked_at_verify,
                _ => return,
            };
            let other = File::open(&img).unwrap();
            let free = other.try_lock().is_ok();
            let _ = other.unlock();
            *slot = Some(!free);
        };
        apply(p, false, &AtomicBool::new(false), &mut probe).unwrap();
        assert_eq!(locked_at_write, Some(true), "held while written");
        assert_eq!(locked_at_verify, Some(false), "released before the checks");
        assert_edited(&img);
    }

    /// The folder sync after the journal's removal failing can't undo the edit, which is in
    /// the image and checked: the edit is done, with a warning in plain words (the job's
    /// `commit_ack.warning`), not failed.
    #[test]
    fn a_failed_folder_sync_after_the_commit_is_done_with_a_warning() {
        let tmp = Scratch::new("dirsync");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Ufs2);
        let p = prepare(&img, &edits(&tmp.0)).unwrap();
        let o = apply_stopping(
            p,
            false,
            &AtomicBool::new(false),
            &mut no_progress(),
            Stop::DirSyncFails,
        )
        .unwrap_or_else(|e| panic!("reported as failed: {e}"));
        let w = o.warning.clone().expect("a warning");
        assert!(w.contains("changes are in the image"), "{w}");
        assert_eq!(o.ack()["warning"], w.as_str());
        assert!(!patch::journal_path(&img).exists());
        assert_edited(&img);
        // A normal run has none.
        let more = vec![Edit::Mkdir {
            path: "more".into(),
        }];
        let o = apply(
            prepare(&img, &more).unwrap(),
            false,
            &AtomicBool::new(false),
            &mut no_progress(),
        )
        .unwrap();
        assert!(o.warning.is_none() && o.ack().get("warning").is_none());
    }

    /// A cancel before the first write leaves the image untouched.
    #[test]
    fn a_cancel_writes_nothing() {
        let tmp = Scratch::new("cancel");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Ufs2);
        let before = std::fs::read(&img).unwrap();
        let p = prepare(&img, &edits(&tmp.0)).unwrap();
        let e = apply(p, false, &AtomicBool::new(true), &mut no_progress()).unwrap_err();
        assert!(e.contains("cancelled"), "{e}");
        assert!(std::fs::read(&img).unwrap() == before);
        assert!(!patch::journal_path(&img).exists());
    }

    /// A damaged journal is kept and the image refused (it may be part-edited); a journal cut
    /// short is deleted and the image opens.
    #[test]
    fn a_damaged_journal_refuses_the_open() {
        let tmp = Scratch::new("damaged");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Exfat);
        let before = std::fs::read(&img).unwrap();
        let journal = patch::journal_path(&img);
        // Whole length, trailer marker in place, but the CRC wrong.
        let mut bogus = b"FORGEUNDO1".to_vec();
        bogus.extend_from_slice(&(before.len() as u64).to_le_bytes());
        bogus.extend_from_slice(&64u64.to_le_bytes());
        bogus.extend_from_slice(&0u32.to_le_bytes());
        bogus.resize(64 - 12, 0xAB);
        bogus.extend_from_slice(b"FORGEEND");
        bogus.extend_from_slice(&[0, 0, 0, 0]);
        std::fs::write(&journal, &bogus).unwrap();
        let e = before_open(&img).unwrap_err();
        assert!(e.contains("keep both"), "{e}");
        assert!(journal.exists());
        assert!(prepare(&img, &edits(&tmp.0)).is_err());
        assert!(std::fs::read(&img).unwrap() == before);
        // Cut short (no trailer, shorter than its header says): deleted.
        std::fs::write(&journal, &bogus[..20]).unwrap();
        assert_eq!(before_open(&img).unwrap(), Recovery::Discarded);
        assert!(!journal.exists());
        assert!(std::fs::read(&img).unwrap() == before);
    }

    /// An image being edited is refused to a second edit, a check and an open; released, it is
    /// free again.
    #[test]
    fn a_claimed_image_is_busy() {
        let tmp = Scratch::new("busy");
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Exfat);
        let claim = Claim::take(&img).unwrap();
        assert!(Claim::take(&img).is_err());
        assert!(busy(&img).is_some());
        // The folder holding it is busy too (a build of the folder would read it).
        assert!(busy(&tmp.0).is_some());
        assert!(before_open(&img).is_err());
        let c = check_blocking(&img, &edits(&tmp.0));
        assert!(!c.in_place && c.busy);
        drop(claim);
        assert!(busy(&img).is_none());
        assert!(check_blocking(&img, &edits(&tmp.0)).in_place);
    }

    /// Runs `body` on a thread and fails if it hasn't finished within `secs`: a hang is a
    /// failure, not a test run that never ends.
    fn within(secs: u64, body: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            body();
            let _ = tx.send(());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
            Ok(()) => worker.join().unwrap(),
            // The body panicked: its panic is the failure.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(p) = worker.join() {
                    std::panic::resume_unwind(p);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("did not finish within {secs} s (a deadlock?)")
            }
        }
    }

    /// A second claim of a claimed image returns at once (it used to build a `Claim` under the
    /// set's lock and drop it there, which locked it again and hung), and every check that
    /// takes the lock still answers while a claim is held.
    #[test]
    fn a_second_claim_does_not_hang() {
        within(10, || {
            let tmp = Scratch::new("reclaim");
            let img = tmp.0.join("game.exfat");
            std::fs::write(&img, b"x").unwrap();
            let held = Claim::take(&img).unwrap();
            for _ in 0..3 {
                assert!(Claim::take(&img).is_err());
            }
            assert!(busy(&img).is_some());
            assert!(before_open(&img).is_err());
            assert!(check_blocking(&img, &[]).busy);
            drop(held);
            assert!(busy(&img).is_none());
            let again = Claim::take(&img);
            assert!(again.is_ok());
        });
    }

    /// A build registered as reading an image (or the folder holding it) refuses an in-place
    /// edit's claim, and a claimed image refuses a build's registration, whichever comes first;
    /// released, the other goes ahead. Two builds may read one image.
    #[test]
    fn a_reader_and_an_edit_exclude_each_other_either_order() {
        within(10, || {
            let tmp = Scratch::new("readers");
            let img = tmp.0.join("game.exfat");
            std::fs::write(&img, b"x").unwrap();
            // A build first.
            let read = Reading::take(&img).unwrap();
            let second = Reading::take(&img).unwrap();
            let e = Claim::take(&img).err().expect("refused while read");
            assert!(e.contains("reading"), "{e}");
            assert!(check_blocking(&img, &[]).busy);
            drop(read);
            assert!(Claim::take(&img).is_err(), "one build still reads it");
            drop(second);
            let folder = Reading::take(&tmp.0).unwrap();
            assert!(Claim::take(&img).is_err(), "its folder is read");
            drop(folder);
            // An edit first.
            let claim = Claim::take(&img).unwrap();
            let e = Reading::take(&img).err().expect("refused while edited");
            assert!(e.contains("being edited in place"), "{e}");
            assert!(Reading::take(&tmp.0).is_err(), "an image in it is edited");
            drop(claim);
            drop(Reading::take(&img).unwrap());
            assert!(Claim::take(&img).is_ok());
        });
    }

    /// An upload (or a browse) opens the image as a reader, registered before its journal is
    /// settled: an edit can't start in between, nor while the upload goes on; and an open is
    /// refused while an edit runs.
    #[test]
    fn an_open_for_reading_and_an_edit_exclude_each_other() {
        within(10, || {
            let tmp = Scratch::new("open-reading");
            let img = tmp.0.join("game.exfat");
            std::fs::write(&img, b"x").unwrap();
            let (recovery, reading) = open_for_reading(&img).unwrap();
            assert_eq!(recovery, Recovery::Clean);
            assert!(Claim::take(&img).is_err(), "an upload of it runs");
            drop(reading);
            let claim = Claim::take(&img).unwrap();
            let e = open_for_reading(&img).err().expect("refused while edited");
            assert!(e.contains("being edited in place"), "{e}");
            assert!(before_open(&img).is_err());
            drop(claim);
            assert_eq!(before_open(&img).unwrap(), Recovery::Clean);
            // A browse's open leaves no reader behind.
            assert!(Claim::take(&img).is_ok());
        });
    }

    /// A build and an edit starting at the same moment: never both, many times over.
    #[test]
    fn a_reader_and_an_edit_racing_never_both_start() {
        within(30, || {
            let tmp = Scratch::new("race");
            let img = tmp.0.join("game.exfat");
            std::fs::write(&img, b"x").unwrap();
            for _ in 0..300 {
                let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
                let (g, p) = (gate.clone(), img.clone());
                let reader = std::thread::spawn(move || {
                    g.wait();
                    Reading::take(&p).ok()
                });
                gate.wait();
                let claim = Claim::take(&img).ok();
                let read = reader.join().unwrap();
                assert!(
                    claim.is_none() || read.is_none(),
                    "a build and an edit both started"
                );
                assert!(claim.is_some() || read.is_some(), "one of them starts");
            }
        });
    }

    /// Not an image this edits in place: refused, the file untouched.
    #[test]
    fn other_kinds_and_bad_edits_are_refused() {
        let tmp = Scratch::new("kinds");
        for name in ["game.ffpfsc", "game.pkg", "game.zip"] {
            let p = tmp.0.join(name);
            std::fs::write(&p, b"not an image").unwrap();
            let mkdir = [Edit::Mkdir { path: "x".into() }];
            let r = prepare(&p, &mkdir).err().expect("refused");
            assert!(!r.busy, "{name}");
            assert_eq!(std::fs::read(&p).unwrap(), b"not an image");
            assert!(!patch::journal_path(&p).exists());
        }
        let src = game(&tmp.0, &[7u8; 70_000]);
        let img = image(&src, &tmp.0, Fmt::Exfat);
        assert!(prepare(&img, &[]).is_err());
        // An edit that does not apply (nothing at that path).
        let nope = [Edit::Delete {
            path: "nope".into(),
        }];
        let r = prepare(&img, &nope).err().expect("refused");
        assert!(r.reason.contains("not in the image"), "{r:?}");
        // The image added into itself.
        let me = [Edit::Put {
            path: "self.bin".into(),
            from: img.clone(),
        }];
        let r = prepare(&img, &me).err().expect("refused");
        assert!(r.reason.contains("is the image being edited"), "{r:?}");
    }

    /// A libSceAmpr game whose file set changes gets a fresh ampr_emu.index listing the edited
    /// files, as an edited copy does (#449); a change that keeps the file set keeps it.
    #[test]
    fn an_ampr_game_gets_a_fresh_index() {
        let tmp = Scratch::new("ampr");
        let src = game(&tmp.0, AMPR_EBOOT);
        let img = image(&src, &tmp.0, Fmt::Ufs2);
        put(&tmp.0, "host/new.bin", b"added");
        let edits = vec![Edit::Put {
            path: "data/new.bin".into(),
            from: tmp.0.join("host/new.bin"),
        }];
        let p = prepare(&img, &edits).unwrap();
        assert!(p.index.is_some());
        apply(p, false, &AtomicBool::new(false), &mut no_progress()).unwrap();
        let (files, _) = snapshot(&img);
        let index = &files[ps5upload_fpkg::ampr_index::NAME];
        let listed = String::from_utf8_lossy(index);
        assert!(
            listed.contains("data/new.bin"),
            "the index lists the new file"
        );
        consistent(&img, Fmt::Ufs2);
        let mkdir = [Edit::Mkdir {
            path: "more".into(),
        }];
        assert!(prepare(&img, &mkdir).unwrap().index.is_none());
    }
}
