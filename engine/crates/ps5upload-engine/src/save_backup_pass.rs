//! One save backup run, for the network-share backups (`save_backup_server`) and the automatic
//! backups to a folder here (`save_backup`): each save is copied off the console once, zipped,
//! and sent to every place it is due in (the share, the backup folder, a PS5 drive), each place
//! verified, recorded and retried on its own. The run's state goes to `save_backup_run` as it
//! goes, so the app can show it and a run cut short can be continued.
//!
//! Retries (#896): a copy that fails is tried again in the same run, after a pause, on a fresh
//! session; a PS5 that stops answering is waited for a few times before the run stops (the saves
//! left stay waiting, for Continue). A place that failed every try for two saves in a row is
//! tried once per save after that, so a share that is away costs no long waits per save.
//!
//! Everything a run needs from the console and the places is behind `Console` and `Dest`, so the
//! tests run whole passes against a fake console, an in-memory share and a fake PS5 drive.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ps5upload_core::save_archive::{cleanup_temp_dir, make_temp_dir, zip_folder_noted};
use ps5upload_core::saves::SaveEntry;

use crate::remote::{PutProgress, RemoteFs};
use crate::save_backup::{self as local, done_key, Done};
use crate::save_backup_console::{Copies, Drive, Outcome};
use crate::save_backup_run::{self as journal, Item, Place, Run, RunRef, Step};
use crate::save_folders as folders;
use crate::save_layout::{self as layout, Arrange, Kind, Layout, Owner};

// ── the console ─────────────────────────────────────────────────────────

/// What a run needs from a console. `Live` asks the engine's own routes.
pub(crate) trait Console {
    /// The console's saves (and the emulators').
    fn list_saves(&self, host: &str) -> Result<Vec<SaveEntry>, String>;
    /// The titles running, upper-case.
    fn running(&self, host: &str) -> Result<BTreeSet<String>, String>;
    /// Copies the save off the console into `tmp/<title_id>`, tidied for a backup.
    fn read_save(&self, host: &str, entry: &SaveEntry, tmp: &Path) -> Result<(), String>;
    /// Whose save it is, and what its folders are named after.
    fn owner(&self, host: &str, entry: &SaveEntry, arrange: Arrange) -> Owner;
    /// The comment of a backup zip (which PS5 and user it is from).
    fn note(&self, host: &str, user: u32) -> String;
    /// The console's drives and files, for the PS5 drive copies.
    fn drive<'a>(&'a self, host: &'a str) -> Box<dyn Drive + 'a>;
    /// What the console folder `path` holds now (`save_folders::signature`); Err when it can't
    /// be read (#899).
    fn folder_signature(&self, host: &str, path: &str) -> Result<i64, String>;
    /// Copies the console folder `path` into `tmp` (as `tmp/<its name>`).
    fn read_folder(&self, host: &str, path: &str, tmp: &Path) -> Result<(), String>;
    /// The PS5's own id, for a folder backup's note.
    fn console_id(&self, host: &str) -> Option<String>;
}

/// The console at a host, through the engine's own routes.
pub(crate) struct Live;

impl Console for Live {
    fn list_saves(&self, host: &str) -> Result<Vec<SaveEntry>, String> {
        local::list_saves(host)
    }
    fn running(&self, host: &str) -> Result<BTreeSet<String>, String> {
        crate::save_backup_server::running_titles(host)
    }
    fn read_save(&self, host: &str, entry: &SaveEntry, tmp: &Path) -> Result<(), String> {
        local::download_save(host, entry, tmp, &|| Ok(()))
    }
    fn owner(&self, host: &str, entry: &SaveEntry, arrange: Arrange) -> Owner {
        local::owner_of(host, entry, arrange)
    }
    fn note(&self, host: &str, user: u32) -> String {
        local::backup_note(local::console_identity(host).as_deref(), Some(user))
    }
    fn drive<'a>(&'a self, host: &'a str) -> Box<dyn Drive + 'a> {
        Box::new(crate::save_backup_console::Live(host))
    }
    fn folder_signature(&self, host: &str, path: &str) -> Result<i64, String> {
        let entries = folders::walk(&|p| local::folder_entries(host, p), path)?;
        Ok(folders::signature(&entries))
    }
    fn read_folder(&self, host: &str, path: &str, tmp: &Path) -> Result<(), String> {
        local::download(host, path, tmp, "folder")
    }
    fn console_id(&self, host: &str) -> Option<String> {
        local::console_identity(host)
    }
}

/// Whether `e` says the console (or the helper on it) is not answering: worth waiting for.
pub(crate) fn console_gone(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    [
        "isn't answering",
        "not answering",
        "did not answer",
        "didn't answer",
        "rest mode",
        "timed out",
        "connection refused",
        "connection reset",
        "unreachable",
        "no route to host",
        "host is down",
        "broken pipe",
    ]
    .iter()
    .any(|k| l.contains(k))
}

// ── the places ──────────────────────────────────────────────────────────

/// What one place did with a backup.
#[derive(Debug, PartialEq)]
pub(crate) enum Sent {
    /// There and checked, at this path.
    Verified(String),
    Failed(String),
    /// Not made, and not an error (a PS5 drive that is out).
    Skipped(String),
}

/// How a place hears how a copy goes.
pub(crate) struct Report<'a> {
    /// Bytes written so far (the run's record reads it while the copy goes).
    pub sent: Arc<AtomicU64>,
    /// The copy is written; reading it back now.
    pub checking: &'a dyn Fn(),
    pub cancel: Arc<AtomicBool>,
}

/// A place backups go to.
pub(crate) trait Dest {
    fn place(&self) -> Place;
    /// Where the backup `name` of `owner`'s save will be, before it is written (named in the
    /// run's record, so a copy cut short can be removed).
    fn target(&mut self, host: &str, owner: &Owner, name: &str) -> Result<String, String>;
    /// Writes the backup `zip` to `target` and checks it there. `scratch` is a local file to
    /// read it back into.
    fn send(
        &mut self,
        host: &str,
        owner: &Owner,
        zip: &Path,
        target: &str,
        scratch: &Path,
        report: &Report,
    ) -> Sent;
    /// After a failure: a session that may have broken is not used again.
    fn reset(&mut self) {}
    /// Removes `file`, a copy a run that stopped left unverified (only a backup's name).
    fn discard(&mut self, host: &str, file: &str);
    /// Where the backup `name` of a console folder goes (`rel`: `Folders/<name>`, #899).
    fn target_folder(&mut self, host: &str, rel: &str, name: &str) -> Result<String, String>;
    /// Writes a console folder's backup `zip` to `rel/name` and checks it there.
    fn send_folder(
        &mut self,
        host: &str,
        zip: &Path,
        rel: &str,
        name: &str,
        scratch: &Path,
        report: &Report,
    ) -> Sent;
}

/// What a backup is of: a save (whose, and its folders' names) or a console folder (`rel`: its
/// backups' folder, `Folders/<name>`).
pub(crate) enum What {
    Save(Owner),
    Folder(String),
}

impl What {
    fn label(&self) -> &str {
        match self {
            What::Save(o) => &o.title_id,
            What::Folder(rel) => rel,
        }
    }
}

/// Opens the server (its pooled session) and names the backup folder in it.
pub(crate) type OpenShare<'a> = Box<dyn Fn() -> Result<(Arc<dyn RemoteFs>, String), String> + 'a>;

/// The network share: the folder `folder` of a saved server, arranged `layout`, `keep` versions.
pub(crate) struct ShareDest<'a> {
    pub rt: tokio::runtime::Handle,
    /// Opens the server (the pooled session) and names the folder in it.
    pub open: OpenShare<'a>,
    /// Forgets the pooled session.
    pub forget: Box<dyn Fn() + 'a>,
    pub layout: Layout,
    pub keep: u32,
    /// The session and folder of the last `target`, and the folder the backup goes in.
    at: Option<(Arc<dyn RemoteFs>, String, String)>,
    /// The newest backup verified there.
    pub last: Option<crate::save_backup_server::Verified>,
}

impl<'a> ShareDest<'a> {
    pub fn new(
        rt: tokio::runtime::Handle,
        open: OpenShare<'a>,
        forget: Box<dyn Fn() + 'a>,
        layout: Layout,
        keep: u32,
    ) -> Self {
        ShareDest {
            rt,
            open,
            forget,
            layout,
            keep,
            at: None,
            last: None,
        }
    }
}

impl Dest for ShareDest<'_> {
    fn place(&self) -> Place {
        Place::Share
    }

    fn target(&mut self, _host: &str, owner: &Owner, name: &str) -> Result<String, String> {
        let (fs, folder) = (self.open)()?;
        let rel = self.rt.block_on(crate::save_backup_server::remote_place(
            fs.as_ref(),
            &folder,
            self.layout,
            Kind::Encrypted,
            owner,
        ))?;
        let target = format!("{}/{name}", layout::join(&folder, &rel));
        self.at = Some((fs, folder, rel));
        Ok(target)
    }

    fn send(
        &mut self,
        _host: &str,
        owner: &Owner,
        zip: &Path,
        _target: &str,
        scratch: &Path,
        report: &Report,
    ) -> Sent {
        let Some((fs, folder, rel)) = self.at.clone() else {
            return Sent::Failed("no place on the server".into());
        };
        let Some(name) = zip.file_name().and_then(|n| n.to_str()) else {
            return Sent::Failed("unexpected backup name".into());
        };
        let sent = Arc::clone(&report.sent);
        let progress = PutProgress {
            add: Box::new(move |n| {
                sent.fetch_add(n, Ordering::Relaxed);
            }),
            cancel: Arc::clone(&report.cancel),
        };
        let r = self
            .rt
            .block_on(crate::save_backup_server::store_verified_with(
                fs.as_ref(),
                zip,
                &folder,
                &rel,
                &owner.title_id,
                owner.user,
                name,
                self.keep,
                scratch,
                &progress,
                report.checking,
            ));
        match r {
            Ok(v) => {
                let at = format!("{}/{}", layout::join(&folder, &rel), v.file);
                self.last = Some(v);
                Sent::Verified(at)
            }
            Err(e) => Sent::Failed(e),
        }
    }

    fn reset(&mut self) {
        self.at = None;
        (self.forget)();
    }

    fn target_folder(&mut self, _host: &str, rel: &str, name: &str) -> Result<String, String> {
        let (fs, folder) = (self.open)()?;
        let target = format!("{}/{name}", layout::join(&folder, rel));
        self.at = Some((fs, folder, rel.to_string()));
        Ok(target)
    }

    fn send_folder(
        &mut self,
        _host: &str,
        zip: &Path,
        rel: &str,
        name: &str,
        scratch: &Path,
        report: &Report,
    ) -> Sent {
        let Some((fs, folder, _)) = self.at.clone() else {
            return Sent::Failed("no place on the server".into());
        };
        let sent = Arc::clone(&report.sent);
        let progress = PutProgress {
            add: Box::new(move |n| {
                sent.fetch_add(n, Ordering::Relaxed);
            }),
            cancel: Arc::clone(&report.cancel),
        };
        match self.rt.block_on(folders::store_on_server(
            fs.as_ref(),
            zip,
            &folder,
            rel,
            name,
            self.keep,
            scratch,
            &progress,
            report.checking,
        )) {
            Ok(at) => Sent::Verified(at),
            Err(e) => Sent::Failed(e),
        }
    }

    fn discard(&mut self, _host: &str, file: &str) {
        let Some(name) = file.rsplit('/').next() else {
            return;
        };
        if !is_backup_name(name) && !folders::ours(name) {
            return;
        }
        let Ok((fs, _)) = (self.open)() else {
            return;
        };
        self.rt.block_on(async {
            let _ = fs.remove_file(file).await;
            let _ = fs.remove_file(&format!("{file}.ps5upload-tmp")).await;
            let _ = fs.remove_file(&format!("{file}.sha256")).await;
        });
    }
}

/// A name this app gives its backups (`<title>_user<8 hex>_<stamp>[_<tag>].zip`).
pub(crate) fn is_backup_name(name: &str) -> bool {
    let Some((title, _)) = name.split_once("_user") else {
        return false;
    };
    layout::ours(title, name)
}

/// A drive in the PS5 (`Copies`), on the console each save came from.
pub(crate) struct DriveDest<'a> {
    pub copies: Copies,
    pub console: &'a dyn Console,
    /// The layout the copies follow.
    pub layout: Layout,
}

impl Dest for DriveDest<'_> {
    fn place(&self) -> Place {
        Place::Drive
    }

    fn target(&mut self, host: &str, owner: &Owner, name: &str) -> Result<String, String> {
        let c = self.console.drive(host);
        crate::save_backup_console::copy_target(
            c.as_ref(),
            self.copies.drive(),
            owner,
            self.layout,
            name,
        )
    }

    fn send(
        &mut self,
        host: &str,
        owner: &Owner,
        zip: &Path,
        _target: &str,
        _scratch: &Path,
        _report: &Report,
    ) -> Sent {
        // A write on the console: no other save operation runs there meanwhile.
        let _op = local::save_op(&[host]);
        let c = self.console.drive(host);
        match self.copies.copy(c.as_ref(), host, owner, zip) {
            Outcome::Copied(p) => Sent::Verified(p),
            Outcome::Skipped(why) => Sent::Skipped(why),
            Outcome::Failed(why) => Sent::Failed(why),
        }
    }

    fn discard(&mut self, host: &str, file: &str) {
        let name = file.rsplit('/').next().unwrap_or_default();
        if !(is_backup_name(name) || folders::ours(name))
            || crate::save_backup_console::root_of(self.copies.drive())
                .map(|r| !file.starts_with(&format!("{r}/")))
                .unwrap_or(true)
        {
            return;
        }
        let _op = local::save_op(&[host]);
        let _ = self.console.drive(host).delete(file);
    }

    fn target_folder(&mut self, _host: &str, rel: &str, name: &str) -> Result<String, String> {
        let root = crate::save_backup_console::root_of(self.copies.drive())?;
        Ok(format!("{}/{name}", layout::join(&root, rel)))
    }

    fn send_folder(
        &mut self,
        host: &str,
        zip: &Path,
        rel: &str,
        name: &str,
        _scratch: &Path,
        _report: &Report,
    ) -> Sent {
        let c = self.console.drive(host);
        let out = match self.copies.unready(c.as_ref(), host) {
            Some(why) => Sent::Skipped(why),
            None => {
                // A write on the console: no other save operation runs there meanwhile.
                let _op = local::save_op(&[host]);
                match crate::save_backup_console::root_of(self.copies.drive()).and_then(|root| {
                    folders::store_on_drive(c.as_ref(), &root, rel, name, zip, self.copies.keep())
                }) {
                    Ok(p) => Sent::Verified(p),
                    Err(e) => Sent::Failed(e),
                }
            }
        };
        self.copies.count(match &out {
            Sent::Verified(_) => Ok(()),
            Sent::Skipped(why) => Err((false, why.clone())),
            Sent::Failed(why) => Err((true, why.clone())),
        });
        out
    }
}

/// The backup folder on this computer, arranged `arrange`, `keep` versions of each save.
pub(crate) struct LocalDest {
    pub dir: String,
    pub keep: u32,
    pub layout: Layout,
}

impl Dest for LocalDest {
    fn place(&self) -> Place {
        Place::Local
    }

    fn target(&mut self, _host: &str, owner: &Owner, name: &str) -> Result<String, String> {
        if self.dir.trim().is_empty() {
            return Err("automatic backup: no folder chosen".into());
        }
        let list = local::local_list(&self.dir);
        let rel = layout::place(&list, self.layout, Kind::Encrypted, owner)?;
        Ok(local::local_path(&self.dir, &rel)
            .join(name)
            .to_string_lossy()
            .into_owned())
    }

    fn send(
        &mut self,
        _host: &str,
        owner: &Owner,
        zip: &Path,
        target: &str,
        _scratch: &Path,
        report: &Report,
    ) -> Sent {
        let dest = PathBuf::from(target);
        let Some(folder) = dest.parent() else {
            return Sent::Failed("no folder for the backup".into());
        };
        if let Err(e) = std::fs::create_dir_all(folder) {
            return Sent::Failed(format!("create {}: {e}", folder.display()));
        }
        // Under a temporary name until it is whole and checked: a half copy never takes the
        // backup's name, nor pushes a good version out.
        let tmp = dest.with_extension("zip.ps5upload-tmp");
        let size = std::fs::metadata(zip).map(|m| m.len()).unwrap_or(0);
        if let Err(e) = std::fs::copy(zip, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Sent::Failed(format!("write {}: {e}", tmp.display()));
        }
        report.sent.store(size, Ordering::Relaxed);
        (report.checking)();
        if let Err(e) = crate::save_backup_server::check_zip(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Sent::Failed(format!("the copy here does not check out: {e}"));
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            let _ = std::fs::remove_file(&tmp);
            return Sent::Failed(format!("write {}: {e}", dest.display()));
        }
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        local::prune_local(
            &self.dir,
            &owner.title_id,
            owner.user,
            ps5upload_core::save_archive::backup_tag(&name),
            self.keep,
            &dest,
        );
        Sent::Verified(target.to_string())
    }

    fn discard(&mut self, _host: &str, file: &str) {
        let p = PathBuf::from(file);
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !(is_backup_name(&name) || folders::ours(&name)) || !p.starts_with(&self.dir) {
            return;
        }
        let _ = std::fs::remove_file(p.with_extension("zip.ps5upload-tmp"));
    }

    fn target_folder(&mut self, _host: &str, rel: &str, name: &str) -> Result<String, String> {
        if self.dir.trim().is_empty() {
            return Err("automatic backup: no folder chosen".into());
        }
        Ok(local::local_path(&self.dir, rel)
            .join(name)
            .to_string_lossy()
            .into_owned())
    }

    fn send_folder(
        &mut self,
        _host: &str,
        zip: &Path,
        rel: &str,
        name: &str,
        _scratch: &Path,
        report: &Report,
    ) -> Sent {
        (report.checking)();
        match folders::store_here(&self.dir, rel, name, zip, self.keep) {
            Ok(p) => {
                report.sent.store(
                    std::fs::metadata(zip).map(|m| m.len()).unwrap_or(0),
                    Ordering::Relaxed,
                );
                Sent::Verified(p)
            }
            Err(e) => Sent::Failed(e),
        }
    }
}

// ── retries ─────────────────────────────────────────────────────────────

/// The pauses before each try again.
#[derive(Clone, Debug)]
pub(crate) struct Retry {
    /// Reading a save off a PS5 that is not answering.
    pub read: Vec<Duration>,
    /// Sending a backup to a place.
    pub send: Vec<Duration>,
}

impl Retry {
    pub fn live() -> Self {
        Retry {
            read: vec![
                Duration::from_secs(15),
                Duration::from_secs(45),
                Duration::from_secs(120),
            ],
            send: vec![
                Duration::from_secs(5),
                Duration::from_secs(20),
                Duration::from_secs(60),
            ],
        }
    }

    /// No pauses (tests): the same tries, at once.
    #[cfg(test)]
    pub fn instant() -> Self {
        Retry {
            read: vec![Duration::ZERO; 3],
            send: vec![Duration::ZERO; 3],
        }
    }

    /// One try each (tests: how a run went before #896).
    #[cfg(test)]
    pub fn once() -> Self {
        Retry {
            read: Vec::new(),
            send: Vec::new(),
        }
    }
}

/// Waits `d`, or less when `cancel` is set (false then).
fn pause(d: Duration, cancel: &AtomicBool) -> bool {
    let until = std::time::Instant::now() + d;
    while std::time::Instant::now() < until {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep((until - std::time::Instant::now()).min(Duration::from_millis(250)));
    }
    !cancel.load(Ordering::SeqCst)
}

/// A failure that trying again can't change.
fn permanent(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("unexpected backup")
        || l.contains("the backup made here")
        || l.contains("not a backup of")
        || l.contains("no folder chosen")
}

// ── one save ────────────────────────────────────────────────────────────

/// How a save's backup ended.
#[derive(Debug, PartialEq)]
pub(crate) enum End {
    /// Done with it (verified, failed or skipped in each place: see the record).
    Done,
    /// The PS5 stopped answering while it was read: the save is waiting again.
    ConsoleGone(String),
    Cancelled,
}

/// Everything a run's saves are sent through.
pub(crate) struct Ctx<'a, 'b> {
    pub console: &'a dyn Console,
    pub run: &'a RunRef<'b>,
    pub retry: &'a Retry,
    pub arrange: Arrange,
    pub dests: Vec<&'a mut dyn Dest>,
    /// Saves in a row each place failed every try for: past two, one try per save.
    pub streak: BTreeMap<Place, u32>,
    /// Called once a place has verified a save: records it.
    pub on_verified: &'a mut dyn FnMut(&str, &SaveEntry, Place),
}

/// The name of a backup of `entry` made now: `<title>_user<8 hex>_<stamp>.zip`.
pub(crate) fn backup_name(entry: &SaveEntry) -> String {
    format!(
        "{}{}.zip",
        ps5upload_core::save_archive::auto_backup_stem(&entry.title_id, entry.user_id as u32),
        local::stamp(local::now_ms())
    )
}

impl Ctx<'_, '_> {
    fn dest_index(&self, place: Place) -> Option<usize> {
        self.dests.iter().position(|d| d.place() == place)
    }

    /// Backs up the run's save `i` (`entry`, on `host`) to each place it still needs.
    pub fn back_up(&mut self, i: usize, host: &str, entry: &SaveEntry) -> End {
        let run = self.run;
        let Some(item) = run.item(i) else {
            return End::Done;
        };
        let mut due = item.left_places();
        // The PS5 drive last: it goes over the console, after the places that don't.
        due.sort_by_key(|p| *p == Place::Drive);
        if due.is_empty() {
            return End::Done;
        }
        // A copy a run that stopped left unverified goes before the save is copied again.
        for p in &due {
            if let Some(file) = item.dests.get(p).and_then(|d| d.file.clone()) {
                if let Some(di) = self.dest_index(*p) {
                    self.dests[di].discard(host, &file);
                }
                run.dest(i, *p, |d| d.file = None);
            }
        }
        if run.cancelled() {
            return End::Cancelled;
        }
        let prefix = if entry.kind == "folder" {
            "folder"
        } else {
            entry.title_id.as_str()
        };
        let tmp = match make_temp_dir(prefix) {
            Ok(t) => t,
            Err(e) => {
                run.failed(i, &e);
                return End::Done;
            }
        };
        let end = self.back_up_in(i, host, entry, &due, &tmp);
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
        if end == End::Done {
            if let Some(it) = run.item(i) {
                let step = match it.outcome() {
                    journal::Outcome::Verified => Step::Verified,
                    journal::Outcome::Skipped => Step::Skipped,
                    journal::Outcome::Failed => Step::Failed,
                    journal::Outcome::Left => Step::Waiting,
                };
                if it.step != Step::Failed {
                    run.step(i, step);
                }
            }
        }
        end
    }

    fn back_up_in(
        &mut self,
        i: usize,
        host: &str,
        entry: &SaveEntry,
        due: &[Place],
        tmp: &Path,
    ) -> End {
        let run = self.run;
        let cancel = run.cancel_flag();
        // A console folder (#899) or a save.
        let folder = (entry.kind == "folder").then(|| {
            entry
                .path
                .strip_prefix("folder:")
                .unwrap_or(&entry.path)
                .to_string()
        });
        let name = match &folder {
            Some(_) => folders::backup_name(None),
            None => backup_name(entry),
        };
        let zip = tmp.join(&name);
        // A folder's backups are kept per PS5 (two PS5s may have the same folder): where they go
        // needs to know which PS5 this is.
        let folder_at = match &folder {
            Some(path) => match self
                .console
                .console_id(host)
                .ok_or_else(|| folders::UNKNOWN_CONSOLE.to_string())
                .and_then(|id| folders::folder_rel_on(path, &id).map(|rel| (id, rel)))
            {
                Ok(at) => Some(at),
                Err(e) => {
                    run.failed(i, &e);
                    return End::Done;
                }
            },
            None => None,
        };
        // Off the console and zipped, waiting for a PS5 that stops answering a few times.
        let mut tries = 0usize;
        loop {
            run.step(i, Step::Reading);
            run.read_try(i);
            let read = {
                let _op = local::save_op(&[host]);
                let _ = std::fs::remove_file(&zip);
                match &folder {
                    Some(path) => folders::leaf_of(path).and_then(|leaf| {
                        let _ = std::fs::remove_dir_all(tmp.join(&leaf));
                        self.console.read_folder(host, path, tmp)?;
                        let id = folder_at.as_ref().map(|(id, _)| id.as_str());
                        folders::zip_copy(tmp, &leaf, &zip, path, id)
                    }),
                    None => {
                        let _ = std::fs::remove_dir_all(tmp.join(&entry.title_id));
                        self.console.read_save(host, entry, tmp).and_then(|()| {
                            let note = self.console.note(host, entry.user_id as u32);
                            zip_folder_noted(tmp, &entry.title_id, &zip, &note)
                                .map_err(|e| format!("zip: {e:#}"))
                        })
                    }
                }
            };
            match read {
                Ok(()) => break,
                Err(e) if console_gone(&e) => {
                    run.step(i, Step::Waiting);
                    match self.retry.read.get(tries) {
                        Some(d) if pause(*d, &cancel) => tries += 1,
                        Some(_) => return End::Cancelled,
                        None => return End::ConsoleGone(e),
                    }
                }
                Err(e) => {
                    crate::log_warn!("save backup: {} on {host}: {e}", entry.title_id);
                    run.failed(i, &e);
                    return End::Done;
                }
            }
        }
        let what = match &folder_at {
            Some((_, rel)) => What::Folder(rel.clone()),
            None => What::Save(self.console.owner(host, entry, self.arrange)),
        };
        let scratch = tmp.join("readback.zip");
        for place in due {
            if let Some(end) = self.send_to(i, host, &what, *place, &zip, &scratch) {
                return end;
            }
            if run
                .item(i)
                .and_then(|it| it.dests.get(place).map(|d| d.step))
                == Some(Step::Verified)
            {
                (self.on_verified)(host, entry, *place);
            }
        }
        End::Done
    }

    /// Sends the backup `zip` to `place`, trying again after a pause. Some when the run ends.
    fn send_to(
        &mut self,
        i: usize,
        host: &str,
        what: &What,
        place: Place,
        zip: &Path,
        scratch: &Path,
    ) -> Option<End> {
        let run = self.run;
        let cancel = run.cancel_flag();
        let Some(di) = self.dest_index(place) else {
            run.dest(i, place, |d| {
                d.step = Step::Skipped;
                d.error = Some("not chosen".into());
            });
            return None;
        };
        let name = zip
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tries_allowed = if self.streak.get(&place).copied().unwrap_or(0) >= 2 {
            0
        } else {
            self.retry.send.len()
        };
        let mut tries = 0usize;
        loop {
            if run.cancelled() {
                return Some(End::Cancelled);
            }
            run.step(i, Step::Copying);
            run.dest(i, place, |d| {
                d.step = Step::Copying;
                d.tries += 1;
            });
            let dest = &mut self.dests[di];
            let target = match what {
                What::Save(owner) => dest.target(host, owner, &name),
                What::Folder(rel) => dest.target_folder(host, rel, &name),
            };
            let sent = match target {
                Err(e) => Sent::Failed(e),
                Ok(target) => {
                    run.dest(i, place, |d| {
                        d.file = Some(target.clone());
                        d.error = None;
                    });
                    let sent = Arc::new(AtomicU64::new(0));
                    let size = std::fs::metadata(zip).map(|m| m.len()).unwrap_or(0);
                    run.watch(i, place, Arc::clone(&sent), size);
                    let checking = || {
                        run.step(i, Step::Checking);
                        run.dest(i, place, |d| d.step = Step::Checking);
                    };
                    let report = Report {
                        sent,
                        checking: &checking,
                        cancel: Arc::clone(&cancel),
                    };
                    let out = match what {
                        What::Save(owner) => dest.send(host, owner, zip, &target, scratch, &report),
                        What::Folder(rel) => {
                            dest.send_folder(host, zip, rel, &name, scratch, &report)
                        }
                    };
                    run.unwatch();
                    out
                }
            };
            match sent {
                Sent::Verified(file) => {
                    self.streak.insert(place, 0);
                    crate::log_info!(
                        "save backup: {} on {host} to {place:?} (try {}): verified",
                        what.label(),
                        tries + 1
                    );
                    run.dest(i, place, |d| {
                        d.step = Step::Verified;
                        d.file = Some(file);
                        d.error = None;
                    });
                    return None;
                }
                Sent::Skipped(why) => {
                    run.dest(i, place, |d| {
                        d.step = Step::Skipped;
                        d.file = None;
                        d.error = Some(why);
                    });
                    return None;
                }
                Sent::Failed(why) => {
                    if run.cancelled() {
                        run.dest(i, place, |d| {
                            d.step = Step::Waiting;
                            d.file = None;
                        });
                        return Some(End::Cancelled);
                    }
                    crate::log_warn!(
                        "save backup: {} on {host} to {place:?} (try {}): {why}",
                        what.label(),
                        tries + 1
                    );
                    self.dests[di].reset();
                    if tries < tries_allowed && !permanent(&why) {
                        run.dest(i, place, |d| {
                            d.step = Step::Waiting;
                            d.file = None;
                            d.error = Some(why.clone());
                        });
                        if !pause(self.retry.send[tries], &cancel) {
                            return Some(End::Cancelled);
                        }
                        tries += 1;
                        continue;
                    }
                    *self.streak.entry(place).or_default() += 1;
                    run.dest(i, place, |d| {
                        d.step = Step::Failed;
                        d.file = None;
                        d.error = Some(why);
                    });
                    return None;
                }
            }
        }
    }
}

// ── the record of what each place has ───────────────────────────────────

/// What each place holds of each save: the save's time when it was last verified there.
pub(crate) trait Records {
    fn done(&self, place: Place) -> Done;
    fn mark(&self, place: Place, key: &str, mtime: i64);
}

/// Records kept in memory (tests).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemRecords(pub std::sync::Mutex<BTreeMap<Place, Done>>);

#[cfg(test)]
impl Records for MemRecords {
    fn done(&self, place: Place) -> Done {
        self.0
            .lock()
            .unwrap()
            .get(&place)
            .cloned()
            .unwrap_or_default()
    }
    fn mark(&self, place: Place, key: &str, mtime: i64) {
        self.0
            .lock()
            .unwrap()
            .entry(place)
            .or_default()
            .insert(key.to_string(), mtime);
    }
}

// ── planning ────────────────────────────────────────────────────────────

/// Whether `e` changed since its time in `done`.
pub(crate) fn changed(host: &str, e: &SaveEntry, done: &Done) -> bool {
    e.mtime > done.get(&done_key(host, &e.path)).copied().unwrap_or(0)
}

/// Adds `items` (each with its save) to the run; the indices with their saves.
pub(crate) fn add_items(
    run: &RunRef<'_>,
    items: Vec<(Item, SaveEntry)>,
) -> Vec<(usize, SaveEntry)> {
    let (items, entries): (Vec<Item>, Vec<SaveEntry>) = items.into_iter().unzip();
    run.add(items).zip(entries).collect()
}

/// Continue or Retry: the last run's saves again, each in the places it is not verified in yet
/// (`only`: these saves only, by id; their failed places too). Saves done carry over as they are,
/// so the run still shows the whole. The saves are looked up again on their consoles (one gone
/// from the console is dropped); a console that does not answer leaves its saves waiting.
pub(crate) fn carry_over(
    console: &dyn Console,
    run: &RunRef<'_>,
    last: &Run,
    only: Option<&BTreeSet<String>>,
) -> Vec<(usize, String, SaveEntry)> {
    let mut lists: BTreeMap<String, Result<Vec<SaveEntry>, String>> = BTreeMap::new();
    let mut todo = Vec::new();
    for old in &last.items {
        let redo = match only {
            Some(ids) => ids.contains(&old.id),
            None => old.outcome() == journal::Outcome::Left,
        };
        let mut it = old.clone();
        if !redo {
            run.add(vec![it]);
            continue;
        }
        it.step = Step::Waiting;
        it.error = None;
        it.tries = 0;
        for d in it.dests.values_mut() {
            if !matches!(d.step, Step::Verified | Step::Skipped) {
                d.step = Step::Waiting;
                d.error = None;
                d.pct = None;
                d.tries = 0;
            }
        }
        let list = lists
            .entry(it.host.clone())
            .or_insert_with(|| console.list_saves(&it.host));
        let entry = match list {
            // A console folder (#899): looked at again as it is now.
            Ok(_) if it.kind == "folder" => match console.folder_signature(&it.host, &it.path) {
                Ok(sig) => Some(folders::entry(&it.path, sig)),
                // Only "not found" is a folder that is gone: one that could not be read now (a
                // dropped link, a busy helper) stays left, with why, for the next Continue.
                Err(e) if !ps5upload_core::fs_ops::is_not_found(&e) => {
                    it.error = Some(e);
                    run.add(vec![it]);
                    continue;
                }
                Err(_) => None,
            },
            Ok(saves) => saves.iter().find(|e| e.path == it.path).cloned(),
            Err(e) => {
                run.unreached(&it.host, e);
                None
            }
        };
        match entry {
            Some(e) => {
                it.mtime = e.mtime;
                if it.kind != "folder" {
                    it.size = e.size.max(0) as u64;
                }
                let host = it.host.clone();
                let idx = run.add(vec![it]).start;
                todo.push((idx, host, e));
            }
            None if list.is_ok() => {
                // The save (or folder) is gone from the console: nothing to back up.
                it.step = Step::Skipped;
                it.error = Some(if it.kind == "folder" {
                    "the folder is no longer on the PS5".into()
                } else {
                    "the save is no longer on the PS5".into()
                });
                for d in it.dests.values_mut() {
                    if d.step == Step::Waiting {
                        d.step = Step::Skipped;
                    }
                }
                run.add(vec![it]);
            }
            None => {
                run.add(vec![it]);
            }
        }
    }
    todo
}

/// Backs up the run's saves `todo` through `ctx`. A console that stops answering keeps its saves
/// left waiting; the others go on. Some(reason) when saves were left that way.
pub(crate) fn execute(
    ctx: &mut Ctx<'_, '_>,
    todo: &[(usize, String, SaveEntry)],
    keep_going: &dyn Fn() -> bool,
) -> Option<(String, String)> {
    let mut gone: BTreeMap<String, String> = BTreeMap::new();
    for (i, host, entry) in todo {
        if ctx.run.cancelled() || !keep_going() {
            break;
        }
        if gone.contains_key(host) {
            continue;
        }
        match ctx.back_up(*i, host, entry) {
            End::Done => {}
            End::Cancelled => break,
            End::ConsoleGone(e) => {
                crate::log_warn!("save backup: {host} stopped answering: {e}");
                ctx.run.unreached(host, &e);
                gone.insert(host.clone(), e);
            }
        }
    }
    gone.into_iter()
        .next()
        .map(|(_, e)| ("console_gone".to_string(), e))
}

// ── a network-share pass ────────────────────────────────────────────────

/// What a network-share pass works on.
pub(crate) enum Work<'w> {
    /// These consoles, each with the game just closed there (None: every changed save). `settle`:
    /// a save written in the last few minutes waits.
    Hosts(&'w [(String, Option<String>)], bool),
    /// The last run again: what it left (Continue), or these saves of it (Retry).
    Carry(&'w Run, Option<&'w BTreeSet<String>>),
}

/// What a network-share pass needs, live or in a test.
pub(crate) struct SharePass<'a> {
    pub s: &'a crate::save_backup_server::ServerSettings,
    pub console: &'a dyn Console,
    pub records: &'a dyn Records,
    pub retry: &'a Retry,
    pub rt: tokio::runtime::Handle,
    /// Opens the server and names the folder in it.
    pub open: &'a dyn Fn() -> Result<(Arc<dyn RemoteFs>, String), String>,
    /// Forgets the pooled session.
    pub forget: &'a dyn Fn(),
    /// False when the run should stop (the server folder was changed part way).
    pub keep_going: &'a dyn Fn() -> bool,
    /// After a save is verified on the share (a decrypted copy beside it).
    pub after_share: &'a dyn Fn(&str, &SaveEntry),
}

/// How a network-share pass went.
pub(crate) struct ShareOut {
    pub summary: journal::Summary,
    /// The consoles that answered.
    pub answered: BTreeSet<String>,
    /// Console → the titles whose saves are not on the share yet (failed or left).
    pub missing: BTreeMap<String, BTreeSet<String>>,
    /// The copies to the PS5 drive, when one is chosen.
    pub copies: Option<crate::save_backup_console::CopyStatus>,
    /// The newest backup verified on the share.
    pub last_verified: Option<crate::save_backup_server::Verified>,
}

/// The chosen console folders of `host` that changed since `main`'s record (#899), as run items;
/// with a PS5 drive (`copies`), each due there too: since its own record when `drive_kept`, else
/// whenever it is due in `main`. A folder that is not on this console is not due.
pub(crate) fn plan_folders(
    console: &dyn Console,
    host: &str,
    list: &[String],
    main: Place,
    records: &dyn Records,
    copies: Option<&mut Copies>,
    drive_kept: bool,
) -> Vec<(Item, SaveEntry)> {
    let done = records.done(main);
    let drive_done = records.done(Place::Drive);
    let drive = console.drive(host);
    let mut copies = copies;
    let mut out = Vec::new();
    for path in list {
        // Checked again here: a setting read from disk may predate a rule.
        if folders::folder_path_ok(path).is_err() {
            continue;
        }
        let Ok(sig) = console.folder_signature(host, path) else {
            continue;
        };
        let e = folders::entry(path, sig);
        let key = done_key(host, &e.path);
        let mut places = Vec::new();
        if done.get(&key) != Some(&sig) {
            places.push(main);
        }
        let mut away = None;
        if let Some(c) = copies.as_deref_mut() {
            let due = if drive_kept {
                drive_done.get(&key) != Some(&sig)
            } else {
                !places.is_empty()
            };
            if due {
                match c.unready(drive.as_ref(), host) {
                    None => places.push(Place::Drive),
                    Some(why) => {
                        c.note_skipped(drive.as_ref(), host);
                        away = Some(why);
                    }
                }
            }
        }
        if places.is_empty() && away.is_none() {
            continue;
        }
        let mut item = journal::new_item(host, &e, key, &places);
        item.path = path.clone();
        if let Some(why) = away {
            item.dests.insert(
                Place::Drive,
                journal::DestStatus {
                    step: Step::Skipped,
                    error: Some(why),
                    ..Default::default()
                },
            );
            if places.is_empty() {
                item.step = Step::Skipped;
            }
        }
        out.push((item, e));
    }
    out
}

/// The saves of `host` due on the share or the PS5 drive (`copies`), as run items; with
/// `with_folders`, the chosen console folders too (#899).
pub(crate) fn plan_share_host(
    p: &SharePass,
    copies: Option<&mut Copies>,
    host: &str,
    title: Option<&str>,
    settle: bool,
    with_folders: bool,
) -> Result<Vec<(Item, SaveEntry)>, String> {
    use crate::save_backup_server::{due_saves, Pick};
    let saves = p.console.list_saves(host)?;
    // A game running now is still writing its save; one closed is not.
    let running = p.console.running(host).unwrap_or_default();
    let done = p.records.done(Place::Share);
    let drive_done = p.records.done(Place::Drive);
    let mut copies = copies;
    let pick = Pick {
        users: &p.s.users,
        title,
        settle,
        running: &running,
        drive_done: copies.is_some().then_some(&drive_done),
    };
    let now = (local::now_ms() / 1000) as i64;
    let drive = p.console.drive(host);
    let mut out = Vec::new();
    for e in due_saves(host, &saves, &done, now, &pick) {
        let mut places = Vec::new();
        if changed(host, e, &done) {
            places.push(Place::Share);
        }
        let mut away = None;
        if let Some(c) = copies.as_deref_mut() {
            if changed(host, e, &drive_done) {
                // A drive that is away is noted, and the save is not read for it alone.
                match c.unready(drive.as_ref(), host) {
                    None => places.push(Place::Drive),
                    Some(why) => {
                        c.note_skipped(drive.as_ref(), host);
                        away = Some(why);
                    }
                }
            }
        }
        if places.is_empty() && away.is_none() {
            continue;
        }
        let mut item = journal::new_item(host, e, done_key(host, &e.path), &places);
        if let Some(why) = away {
            item.dests.insert(
                Place::Drive,
                journal::DestStatus {
                    step: Step::Skipped,
                    error: Some(why),
                    ..Default::default()
                },
            );
            if places.is_empty() {
                item.step = Step::Skipped;
            }
        }
        out.push((item, e.clone()));
    }
    drop(drive);
    if with_folders {
        out.extend(plan_folders(
            p.console,
            host,
            &p.s.folders,
            Place::Share,
            p.records,
            copies,
            true,
        ));
    }
    Ok(out)
}

/// A network-share pass into `run`: plans the saves (`work`), backs each up to the share and the
/// PS5 drive, and ends the run.
pub(crate) fn share_pass(p: &SharePass, run: RunRef<'_>, work: Work) -> ShareOut {
    let mut copies = Copies::new(&p.s.console_drive, p.s.drive_keep(), p.s.arrange());
    let mut answered = BTreeSet::new();
    let todo: Vec<(usize, String, SaveEntry)> = match work {
        Work::Hosts(list, settle) => {
            let mut todo = Vec::new();
            let mut folders_planned = BTreeSet::new();
            for (host, title) in list {
                // A console's folders once per run, whichever games were closed there.
                let with_folders = folders_planned.insert(host.clone());
                match plan_share_host(
                    p,
                    copies.as_mut(),
                    host,
                    title.as_deref(),
                    settle,
                    with_folders,
                ) {
                    Ok(items) => {
                        answered.insert(host.clone());
                        for (i, e) in add_items(&run, items) {
                            todo.push((i, host.clone(), e));
                        }
                    }
                    Err(e) => {
                        crate::log_warn!("save backup to server: {host}: {e}");
                        run.unreached(host, &e);
                    }
                }
            }
            todo
        }
        Work::Carry(last, only) => {
            let todo = carry_over(p.console, &run, last, only);
            answered.extend(todo.iter().map(|(_, h, _)| h.clone()));
            todo
        }
    };
    let mut share = ShareDest::new(
        p.rt.clone(),
        Box::new(p.open),
        Box::new(p.forget),
        p.s.layout,
        p.s.keep,
    );
    let mut drive = copies.map(|copies| DriveDest {
        copies,
        console: p.console,
        layout: p.s.layout,
    });
    let stopped = {
        let mut dests: Vec<&mut dyn Dest> = vec![&mut share];
        if let Some(d) = drive.as_mut() {
            dests.push(d);
        }
        let mut on_verified = |host: &str, e: &SaveEntry, place: Place| {
            p.records.mark(place, &done_key(host, &e.path), e.mtime);
            if place == Place::Share {
                (p.after_share)(host, e);
            }
        };
        let mut ctx = Ctx {
            console: p.console,
            run: &run,
            retry: p.retry,
            arrange: p.s.arrange(),
            dests,
            streak: BTreeMap::new(),
            on_verified: &mut on_verified,
        };
        execute(&mut ctx, &todo, p.keep_going)
    };
    let mut missing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for i in 0..run.count() {
        let Some(it) = run.item(i) else { continue };
        // A console folder has no game to wait for: it is looked at again in every pass (its
        // record says it is not backed up), so it never goes on the after-close retry list,
        // where it went as a game with no title.
        if it.kind == "folder" {
            continue;
        }
        let on_share = it
            .dests
            .get(&Place::Share)
            .is_none_or(|d| matches!(d.step, Step::Verified | Step::Skipped));
        if !on_share || it.step == Step::Failed {
            missing
                .entry(it.host.clone())
                .or_default()
                .insert(it.title_id.to_ascii_uppercase());
        }
    }
    let summary = run.finish(stopped);
    ShareOut {
        summary,
        answered,
        missing,
        copies: drive.map(|d| d.copies.status).filter(|c| c.at > 0),
        last_verified: share.last,
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct RunsQuery {
    /// `share` (backups to a network share) or `local` (the automatic backups).
    pub which: String,
}

fn bad_which() -> axum::response::Response {
    local::json_error(
        axum::http::StatusCode::BAD_REQUEST,
        "which must be share or local",
    )
}

/// `GET /api/save-backup/runs?which=share|local` — the last run whole (every save and its state
/// in each place), how far it is, and the last runs in short: `{run, summary, history, now}`.
pub async fn get_runs(
    axum::extract::Query(q): axum::extract::Query<RunsQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match journal::Which::parse(&q.which) {
        // Off the async workers: the record's lock is held while a pass writes the file.
        Some(w) => match tokio::task::spawn_blocking(move || journal::global().view(w)).await {
            Ok(v) => axum::Json(v).into_response(),
            Err(e) => {
                local::json_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        },
        None => bad_which(),
    }
}

#[derive(serde::Deserialize)]
pub struct RunsBody {
    pub which: String,
    /// Retry: these saves of the last run (`Item::id`); none: Continue (what it left).
    #[serde(default)]
    pub ids: Option<Vec<String>>,
}

/// `POST /api/save-backup/runs/cancel` — `{which}`: the run stops after the step it is on (a
/// copy being written is stopped and removed). `{cancelled}`: false when none was running.
pub async fn post_cancel(axum::Json(b): axum::Json<RunsBody>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match journal::Which::parse(&b.which) {
        Some(w) => axum::Json(serde_json::json!({ "cancelled": journal::global().cancel(w) }))
            .into_response(),
        None => bad_which(),
    }
}

/// `POST /api/save-backup/runs/continue` — `{which, ids?}`: Continue the last run (each save in
/// the places it is not verified in yet; a copy it left unverified is removed and made again), or
/// with `ids` Retry those saves. In the background; answers the run's view.
pub async fn post_continue(axum::Json(b): axum::Json<RunsBody>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(w) = journal::Which::parse(&b.which) else {
        return bad_which();
    };
    let j = journal::global();
    match j.last(w) {
        None => {
            return local::json_error(
                StatusCode::BAD_REQUEST,
                "there is no earlier backup to continue",
            )
        }
        Some(r) if r.state == journal::RunState::Running => {
            return local::json_error(StatusCode::CONFLICT, "a backup is running")
        }
        Some(_) => {}
    }
    if w == journal::Which::Share && !crate::save_backup_server::configured() {
        return local::json_error(
            StatusCode::BAD_REQUEST,
            "choose a server folder for the backups first",
        );
    }
    let ids: Option<BTreeSet<String>> = b.ids.map(|v| v.into_iter().collect());
    tokio::task::spawn_blocking(move || {
        let r = match w {
            journal::Which::Share => crate::save_backup_server::run_again(ids),
            journal::Which::Local => local::run_again(ids),
        };
        if let Err(e) = r {
            crate::log_warn!("save backup: {e}");
        }
    });
    (StatusCode::ACCEPTED, axum::Json(j.view(w))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::MemFs;
    use crate::save_backup_console::tests::{vol, with_m2, Fake};
    use crate::save_backup_run::{Journal, Outcome, RunState, Which};
    use crate::save_backup_server::ServerSettings;
    use std::cell::{Cell, RefCell};

    /// A folder's files: (path in it, bytes).
    type Files = Vec<(String, Vec<u8>)>;

    /// A PS5 in memory: its saves (each one file of `bytes`), reads that fail as a console that
    /// is not answering, and a drive (`Fake`).
    pub(crate) struct FakeConsole {
        pub saves: Vec<SaveEntry>,
        pub bytes: BTreeMap<String, Vec<u8>>,
        /// Save path → reads that fail before one goes through.
        pub flaky: RefCell<BTreeMap<String, u32>>,
        /// After this many reads every call fails (the console crashed); None: never.
        pub crash_after: Cell<Option<usize>>,
        pub reads: Cell<usize>,
        pub drive: Fake,
        /// Console folders (#899): path → its files (path in it, bytes).
        pub folders: RefCell<BTreeMap<String, Files>>,
        /// The PS5's own id; None: it can't be told.
        pub id: RefCell<Option<String>>,
        /// What looking at any folder fails with now (None: it works).
        pub folder_err: RefCell<Option<String>>,
    }

    const GONE: &str =
        "The PS5 isn't answering: it's off, on another network, or in rest mode with its network features off.";

    impl FakeConsole {
        fn crashed(&self) -> bool {
            self.crash_after
                .get()
                .is_some_and(|n| self.reads.get() >= n)
        }
    }

    /// The fake drive, lent to a run.
    struct DriveRef<'a>(&'a Fake);

    impl Drive for DriveRef<'_> {
        fn volumes(&self) -> Result<Vec<ps5upload_core::volumes::Volume>, String> {
            self.0.volumes()
        }
        fn list(&self, dir: &str) -> Result<Vec<(String, bool, u64)>, String> {
            self.0.list(dir)
        }
        fn mkdir(&self, dir: &str) -> Result<(), String> {
            self.0.mkdir(dir)
        }
        fn upload(&self, src: &Path, dest: &str) -> Result<(), String> {
            self.0.upload(src, dest)
        }
        fn hash(&self, path: &str) -> Result<String, String> {
            self.0.hash(path)
        }
        fn delete(&self, path: &str) -> Result<(), String> {
            self.0.delete(path)
        }
        fn download(&self, src: &str, dest_dir: &Path) -> Result<(), String> {
            self.0.download(src, dest_dir)
        }
    }

    impl Console for FakeConsole {
        fn list_saves(&self, _host: &str) -> Result<Vec<SaveEntry>, String> {
            if self.crashed() {
                return Err(GONE.into());
            }
            Ok(self.saves.clone())
        }
        fn running(&self, _host: &str) -> Result<BTreeSet<String>, String> {
            Ok(BTreeSet::new())
        }
        fn read_save(&self, _host: &str, e: &SaveEntry, tmp: &Path) -> Result<(), String> {
            if self.crashed() {
                return Err(GONE.into());
            }
            if let Some(n) = self.flaky.borrow_mut().get_mut(&e.path).filter(|n| **n > 0) {
                *n -= 1;
                return Err(GONE.into());
            }
            self.reads.set(self.reads.get() + 1);
            let dir = tmp.join(&e.title_id);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("sdimg_SAVEDATA00"), &self.bytes[&e.path])
                .map_err(|e| e.to_string())
        }
        fn owner(&self, _host: &str, e: &SaveEntry, arrange: Arrange) -> Owner {
            let by_user = arrange.layout == Layout::ByUser;
            Owner {
                title_id: e.title_id.clone(),
                user: e.user_id as u32,
                platform: Some(layout::Platform::Ps5),
                name: by_user.then(|| format!("Player{}", e.user_id)),
                game: None,
            }
        }
        fn note(&self, _host: &str, _user: u32) -> String {
            String::new()
        }
        fn drive<'a>(&'a self, _host: &'a str) -> Box<dyn Drive + 'a> {
            Box::new(DriveRef(&self.drive))
        }
        fn folder_signature(&self, _host: &str, path: &str) -> Result<i64, String> {
            if self.crashed() {
                return Err(GONE.into());
            }
            if let Some(e) = self.folder_err.borrow().clone() {
                return Err(e);
            }
            let f = self.folders.borrow();
            let files = f.get(path).ok_or("fs_list_dir_failed_errno_2")?;
            let entries: Vec<(String, u64, i64)> = files
                .iter()
                .map(|(r, b)| {
                    let h = blake3::hash(b);
                    let mut m = [0u8; 8];
                    m.copy_from_slice(&h.as_bytes()[..8]);
                    (r.clone(), b.len() as u64, i64::from_le_bytes(m))
                })
                .collect();
            Ok(folders::signature(&entries))
        }
        fn read_folder(&self, _host: &str, path: &str, tmp: &Path) -> Result<(), String> {
            if self.crashed() {
                return Err(GONE.into());
            }
            let leaf = folders::leaf_of(path)?;
            for (rel, b) in self.folders.borrow().get(path).ok_or("no such folder")? {
                let to = tmp.join(&leaf).join(rel);
                std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(to, b).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        fn console_id(&self, _host: &str) -> Option<String> {
            self.id.borrow().clone()
        }
    }

    /// Bytes that do not compress (a save's encrypted image does not), from `seed`.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// The reporter's console (#896): 11 users, 6 games each (66 saves); most saves small, one
    /// game's save large for each of the first `big` users.
    fn console(big: usize, big_bytes: usize) -> FakeConsole {
        let mut saves = Vec::new();
        let mut bytes = BTreeMap::new();
        for u in 0..11u32 {
            let user = 0x1000_0000 + u;
            for g in 0..6u32 {
                let title = format!("PPSA{:05}", 1000 + g);
                let path = format!("/user/home/{user:08x}/savedata_prospero/{title}");
                let len = if g == 0 && (u as usize) < big {
                    big_bytes
                } else {
                    64 * 1024 + (u as usize * 6 + g as usize) * 4096
                };
                bytes.insert(path.clone(), noise((u * 100 + g) as u64, len));
                saves.push(SaveEntry {
                    title_id: title,
                    user_id: user as i32,
                    path,
                    size: len as i64,
                    mtime: 1_700_000_000,
                    kind: "ps5".into(),
                });
            }
        }
        FakeConsole {
            saves,
            bytes,
            flaky: RefCell::default(),
            crash_after: Cell::new(None),
            reads: Cell::new(0),
            drive: with_m2(),
            folders: RefCell::default(),
            id: RefCell::new(Some(PS5_ID.into())),
            folder_err: RefCell::default(),
        }
    }

    /// The fake PS5's own id.
    const PS5_ID: &str = "F8461C0A0B01";

    fn settings() -> ServerSettings {
        ServerSettings {
            mode: crate::save_backup_server::Mode::AfterClose,
            target: "remote://nas/backups".into(),
            keep: 2,
            console_drive: "/mnt/ext0".into(),
            layout: Layout::ByUser,
            ..ServerSettings::default()
        }
    }

    const HOST: &str = "10.89.0.1";

    /// One network-share pass of `work` over `c`, the share `fs` (folder `/b`), into a new run
    /// of `journal`.
    fn pass_over(
        c: &FakeConsole,
        fs: &MemFs,
        records: &MemRecords,
        retry: &Retry,
        journal: &Journal,
        trigger: &str,
        work: Work,
    ) -> ShareOut {
        pass_with(&settings(), c, fs, records, retry, journal, trigger, work)
    }

    #[allow(clippy::too_many_arguments)]
    fn pass_with(
        s: &ServerSettings,
        c: &FakeConsole,
        fs: &MemFs,
        records: &MemRecords,
        retry: &Retry,
        journal: &Journal,
        trigger: &str,
        work: Work,
    ) -> ShareOut {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server: Arc<dyn RemoteFs> = Arc::new(fs.share());
        let open = || Ok((Arc::clone(&server), "/b".to_string()));
        let forget = || {};
        let keep_going = || true;
        let after = |_: &str, _: &SaveEntry| {};
        let p = SharePass {
            s,
            console: c,
            records,
            retry,
            rt: rt.handle().clone(),
            open: &open,
            forget: &forget,
            keep_going: &keep_going,
            after_share: &after,
        };
        let run = journal.begin(Which::Share, trigger).unwrap();
        share_pass(&p, run, work)
    }

    /// The backups on the share, by save (title and user), without the checksum files.
    fn on_share(fs: &MemFs) -> BTreeSet<String> {
        fs.paths()
            .into_iter()
            .filter(|p| p.ends_with(".zip"))
            .filter_map(|p| p.rsplit('/').next().map(save_of))
            .collect()
    }

    fn on_drive(c: &FakeConsole) -> BTreeSet<String> {
        c.drive
            .files
            .borrow()
            .keys()
            .filter(|p| p.ends_with(".zip"))
            .filter_map(|p| p.rsplit('/').next().map(save_of))
            .collect()
    }

    /// `PPSA01000_user10000000_<stamp>.zip` → `PPSA01000_user10000000`.
    fn save_of(name: &str) -> String {
        name.splitn(3, '_').take(2).collect::<Vec<_>>().join("_")
    }

    fn all_hosts() -> Vec<(String, Option<String>)> {
        vec![(HOST.to_string(), None)]
    }

    /// #896: on one uninterrupted Back up now, every save that reached the PS5 drive reached the
    /// share too. The NAS link drops now and then (once every 24 MB written here); the big saves
    /// meet those drops. Before the fix a share copy that failed once was final for the run (and,
    /// with "After a game is closed", never tried again until that game was closed again), while
    /// the drive copy of the same read went through and was recorded: the drive ended up with
    /// every save and the share without the big ones. The first half shows that (one try, as
    /// before); the second, the same run with the retries.
    #[test]
    fn issue_896_every_save_on_the_ps5_drive_reaches_the_share_too() {
        for (retry, fixed) in [(Retry::once(), false), (Retry::instant(), true)] {
            let c = console(6, 9 << 20);
            let fs = MemFs::new(&[]);
            fs.drop_every_bytes(24 << 20);
            let records = MemRecords::default();
            let journal = Journal::open(None);
            let hosts = all_hosts();
            let out = pass_over(
                &c,
                &fs,
                &records,
                &retry,
                &journal,
                "manual",
                Work::Hosts(&hosts, false),
            );
            let share = on_share(&fs);
            let drive = on_drive(&c);
            assert_eq!(drive.len(), 66, "the drive gets every save either way");
            let sum = &out.summary;
            let per = |p: Place| sum.places.get(&p).cloned().unwrap_or_default();
            assert_eq!(per(Place::Drive).verified, 66);
            if !fixed {
                // How it went before: the share misses the saves whose copy met a drop.
                assert!(
                    share.len() < 66,
                    "one try: the share is short ({})",
                    share.len()
                );
                assert_eq!(per(Place::Share).verified as usize, share.len());
                assert_eq!(per(Place::Share).failed as usize, 66 - share.len());
                // …and the run now says which, where, and why.
                let first = per(Place::Share).first_error.unwrap();
                assert!(first.contains("connection reset"), "{first}");
                let run = journal.last(Which::Share).unwrap();
                let failed: Vec<&Item> = run
                    .items
                    .iter()
                    .filter(|it| it.dests[&Place::Share].step == Step::Failed)
                    .collect();
                assert_eq!(failed.len(), 66 - share.len());
                // The missing ones are the big ones (9 MB, against 64-332 KB).
                assert!(failed.iter().all(|it| it.size == 9 << 20));
                for it in failed {
                    assert_eq!(it.dests[&Place::Drive].step, Step::Verified);
                }
                // Not on the share yet: tried again (the after-close retry, or Continue).
                assert!(out.missing[HOST].contains("PPSA01000"));
                continue;
            }
            assert_eq!(share, drive, "the share has every save the drive has");
            assert_eq!(per(Place::Share).verified, 66);
            assert_eq!(sum.state, RunState::Done);
            assert_eq!((sum.verified, sum.failed, sum.left), (66, 0, 0));
            assert!(out.missing.is_empty());
            // The run tells it met drops and went on: some copies took a second try.
            let run = journal.last(Which::Share).unwrap();
            assert!(run.items.iter().any(|it| it.dests[&Place::Share].tries > 1));
            // Each save was read off the console once.
            assert_eq!(c.reads.get(), 66);
            // Both places recorded it: the next pass has nothing to do.
            assert_eq!(records.done(Place::Share).len(), 66);
            assert_eq!(records.done(Place::Drive).len(), 66);
            let journal2 = Journal::open(None);
            let again = pass_over(
                &c,
                &fs,
                &records,
                &retry,
                &journal2,
                "manual",
                Work::Hosts(&hosts, false),
            );
            assert_eq!(again.summary.total, 0);
        }
    }

    /// A PS5 that does not answer for a moment is waited for; one that stays away stops the run
    /// with what is left (not 59 saves each failed in turn, #895), and is not counted against the
    /// share. Continue then does only what is left.
    #[test]
    fn a_ps5_that_stops_answering_is_waited_for_then_the_run_stops_and_continues() {
        let c = console(0, 0);
        let fs = MemFs::new(&[]);
        let records = MemRecords::default();
        let journal = Journal::open(None);
        let hosts = all_hosts();
        // Two saves don't answer twice each: waited for, then backed up.
        c.flaky.borrow_mut().insert(c.saves[3].path.clone(), 2);
        c.flaky.borrow_mut().insert(c.saves[4].path.clone(), 2);
        // The console goes away after 7 saves.
        c.crash_after.set(Some(7));
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "manual",
            Work::Hosts(&hosts, false),
        );
        let sum = &out.summary;
        assert_eq!(sum.state, RunState::Stopped);
        assert_eq!(sum.reason_code.as_deref(), Some("console_gone"));
        assert!(sum.reason.as_deref().unwrap().contains("isn't answering"));
        assert_eq!((sum.verified, sum.failed, sum.left), (7, 0, 59));
        let share = sum.places[&Place::Share].clone();
        assert_eq!((share.verified, share.failed, share.left), (7, 0, 59));
        let run = journal.last(Which::Share).unwrap();
        assert_eq!(run.items[3].tries, 3, "two waits, then it answered");
        assert!(run.unreached.contains_key(HOST));
        assert_eq!(run.items[7].step, Step::Waiting);
        // The console is back: Continue does the 59 left, not the 7 done.
        c.crash_after.set(None);
        let reads = c.reads.get();
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "continue",
            Work::Carry(&run, None),
        );
        assert_eq!(c.reads.get() - reads, 59);
        assert_eq!(out.summary.state, RunState::Done);
        assert_eq!(out.summary.total, 66, "the run still shows the whole");
        assert_eq!(out.summary.verified, 66);
        assert_eq!(on_share(&fs).len(), 66);
        assert_eq!(on_drive(&c).len(), 66);
    }

    /// The engine stops in the middle of a run (closed, restarted): the record says how far it
    /// got, and Continue after a restart removes the copy it left half-written, backs up only
    /// what was left, and leaves what was verified alone.
    #[test]
    fn a_run_continues_after_an_engine_restart() {
        let dir = std::env::temp_dir().join(format!("ps5u-pass-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("save-backup-runs.json");
        let c = console(0, 0);
        let fs = MemFs::new(&[]);
        let records = MemRecords::default();
        let half;
        {
            let journal = Journal::open(Some(file.clone()));
            // The share goes away after 40 writes: the run is cut there, as when the engine
            // stops. (The run's end is never recorded: the journal is dropped mid-run below.)
            let run = journal.begin(Which::Share, "manual").unwrap();
            let s = settings();
            let rt = tokio::runtime::Runtime::new().unwrap();
            let server: Arc<dyn RemoteFs> = Arc::new(fs.share());
            let open = || Ok((Arc::clone(&server), "/b".to_string()));
            let forget = || {};
            let after = |_: &str, _: &SaveEntry| {};
            let p = SharePass {
                s: &s,
                console: &c,
                records: &records,
                retry: &Retry::once(),
                rt: rt.handle().clone(),
                open: &open,
                forget: &forget,
                keep_going: &|| true,
                after_share: &after,
            };
            let mut copies = Copies::new(&s.console_drive, s.drive_keep(), s.arrange());
            let items = plan_share_host(&p, copies.as_mut(), HOST, None, false, true).unwrap();
            let todo: Vec<(usize, String, SaveEntry)> = add_items(&run, items)
                .into_iter()
                .map(|(i, e)| (i, HOST.to_string(), e))
                .collect();
            let mut share = ShareDest::new(
                rt.handle().clone(),
                Box::new(&open),
                Box::new(&forget),
                s.layout,
                s.keep,
            );
            let mut drive = DriveDest {
                copies: copies.unwrap(),
                console: &c,
                layout: s.layout,
            };
            let mut on_verified = |h: &str, e: &SaveEntry, place: Place| {
                records.mark(place, &done_key(h, &e.path), e.mtime);
            };
            let mut ctx = Ctx {
                console: &c,
                run: &run,
                retry: &Retry::once(),
                arrange: s.arrange(),
                dests: vec![&mut share, &mut drive],
                streak: BTreeMap::new(),
                on_verified: &mut on_verified,
            };
            for (i, h, e) in &todo[..10] {
                assert_eq!(ctx.back_up(*i, h, e), End::Done);
            }
            // The 11th is being written when the engine stops: a half copy on the share.
            let (i, _, e) = &todo[10];
            let owner = c.owner(HOST, e, s.arrange());
            let name = backup_name(e);
            let target = ctx.dests[0].target(HOST, &owner, &name).unwrap();
            run.step(*i, Step::Copying);
            run.dest(*i, Place::Share, |d| {
                d.step = Step::Copying;
                d.file = Some(target.clone());
            });
            fs.add(&[(target.as_str(), b"half a zip")]);
            half = target;
            drop(ctx);
        }
        // The engine starts again.
        let journal = Journal::open(Some(file.clone()));
        let last = journal.last(Which::Share).unwrap();
        assert_eq!(last.state, RunState::Interrupted);
        assert_eq!(last.reason_code.as_deref(), Some("interrupted"));
        let sum = last.summary();
        assert_eq!((sum.verified, sum.left), (10, 56));
        assert_eq!(
            last.items[10].dests[&Place::Share].file.as_deref(),
            Some(half.as_str())
        );
        let reads = c.reads.get();
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "continue",
            Work::Carry(&last, None),
        );
        assert_eq!(out.summary.state, RunState::Done);
        assert_eq!(out.summary.verified, 66);
        assert_eq!(
            c.reads.get() - reads,
            56,
            "only what was left is read again"
        );
        // The half copy is gone (a backup made in the same second takes its name, whole).
        assert_ne!(fs.bytes(&half).as_deref(), Some(&b"half a zip"[..]));
        assert_eq!(on_share(&fs).len(), 66);
        // Nothing else of ours on the share: no half copy, no second copy of a save.
        let zips = fs
            .paths()
            .into_iter()
            .filter(|p| p.ends_with(".zip"))
            .count();
        assert_eq!(zips, 66);
        // The history has both runs.
        let h = journal.history(Which::Share);
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].state, RunState::Interrupted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A share copy that keeps failing is tried a few times, each after a pause, then marked
    /// failed with why; a share that failed every try for two saves in a row is tried once per
    /// save after that, and the PS5 drive still gets each save. Retry then does those saves
    /// alone.
    #[test]
    fn a_share_that_is_away_costs_few_tries_and_retry_does_the_failed_ones() {
        let c = console(0, 0);
        let fs = MemFs::new(&[]);
        fs.set_down(true);
        let records = MemRecords::default();
        let journal = Journal::open(None);
        let hosts = all_hosts();
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "manual",
            Work::Hosts(&hosts, false),
        );
        let sum = &out.summary;
        assert_eq!(sum.places[&Place::Share].failed, 66);
        assert_eq!(sum.places[&Place::Drive].verified, 66);
        let run = journal.last(Which::Share).unwrap();
        let tries: Vec<u32> = run
            .items
            .iter()
            .map(|it| it.dests[&Place::Share].tries)
            .collect();
        assert_eq!(&tries[..3], &[4, 4, 1], "four tries twice, then one");
        assert!(tries[2..].iter().all(|t| *t == 1));
        assert!(run.items[0].dests[&Place::Share]
            .error
            .as_deref()
            .unwrap()
            .contains("connection reset"));
        // The server is back: Retry two of the saves.
        fs.set_down(false);
        let ids: BTreeSet<String> = run.items[..2].iter().map(|it| it.id.clone()).collect();
        let reads = c.reads.get();
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "retry",
            Work::Carry(&run, Some(&ids)),
        );
        assert_eq!(c.reads.get() - reads, 2);
        assert_eq!(out.summary.places[&Place::Share].verified, 2);
        // The drive, which had them, did not get them again.
        assert_eq!(out.summary.places[&Place::Drive].verified, 66);
        assert_eq!(*c.drive.uploads.borrow(), 66);
        assert_eq!(on_share(&fs).len(), 2);
    }

    /// A cancel stops the run after the step it is on: what is done stays done, the rest waits
    /// (Continue picks it up).
    #[test]
    fn a_cancelled_run_keeps_what_it_did() {
        let c = console(0, 0);
        let fs = MemFs::new(&[]);
        let records = MemRecords::default();
        let journal = Journal::open(None);
        // Cancel once 5 saves were read.
        c.crash_after.set(None);
        let hosts = all_hosts();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server: Arc<dyn RemoteFs> = Arc::new(fs.share());
        let open = || Ok((Arc::clone(&server), "/b".to_string()));
        let forget = || {};
        let after = |_: &str, _: &SaveEntry| {
            if c.reads.get() == 5 {
                journal.cancel(Which::Share);
            }
        };
        let s = settings();
        let p = SharePass {
            s: &s,
            console: &c,
            records: &records,
            retry: &Retry::instant(),
            rt: rt.handle().clone(),
            open: &open,
            forget: &forget,
            keep_going: &|| true,
            after_share: &after,
        };
        let run = journal.begin(Which::Share, "manual").unwrap();
        let out = share_pass(&p, run, Work::Hosts(&hosts, false));
        assert_eq!(out.summary.state, RunState::Cancelled);
        assert_eq!(out.summary.reason_code.as_deref(), Some("cancelled"));
        // The fifth was on the share when the cancel came; its drive copy waits.
        assert_eq!(out.summary.verified, 4);
        assert_eq!(out.summary.places[&Place::Share].verified, 5);
        assert_eq!(out.summary.left, 62);
        let last = journal.last(Which::Share).unwrap();
        assert_eq!(last.items[4].outcome(), Outcome::Left);
        assert_eq!(last.items[4].left_places(), vec![Place::Drive]);
    }

    /// A PS5 drive that is not plugged in: each save shows it skipped there, with why, and the
    /// share still gets them.
    #[test]
    fn a_drive_that_is_out_is_shown_skipped() {
        let mut c = console(0, 0);
        c.drive = Fake {
            vols: vec![vol("/data", "/dev/ssd0.user")],
            ..Fake::default()
        };
        let fs = MemFs::new(&[]);
        let records = MemRecords::default();
        let journal = Journal::open(None);
        let hosts = all_hosts();
        let out = pass_over(
            &c,
            &fs,
            &records,
            &Retry::instant(),
            &journal,
            "manual",
            Work::Hosts(&hosts, false),
        );
        let d = &out.summary.places[&Place::Drive];
        assert_eq!((d.skipped, d.failed, d.verified), (66, 0, 0));
        assert_eq!(out.summary.verified, 66);
        let run = journal.last(Which::Share).unwrap();
        assert!(run.items[0].dests[&Place::Drive]
            .error
            .as_deref()
            .unwrap()
            .contains("not plugged in"));
    }

    const GRIM: &str =
        "/data/prospero-win/prefixes/grim-dawn/drive_c/users/prospero/Documents/My Games/Grim Dawn";

    /// #899: a chosen console folder goes with the saves, to the share and the PS5 drive, in
    /// `Folders/<name>/` whatever the layout; again only when it changed, keeping its versions.
    #[test]
    fn a_chosen_console_folder_is_backed_up_with_the_saves() {
        for how in [Layout::ByGame, Layout::ByUser] {
            let c = console(0, 0);
            c.folders.borrow_mut().insert(
                GRIM.into(),
                vec![
                    ("Save/main/player.gdc".into(), b"level 10".to_vec()),
                    ("settings/options.txt".into(), b"a".to_vec()),
                ],
            );
            // A folder chosen for another console is not on this one: not due, no error.
            let s = ServerSettings {
                layout: how,
                folders: vec![GRIM.into(), "/data/not/here".into()],
                ..settings()
            };
            let fs = MemFs::new(&[]);
            let records = MemRecords::default();
            let journal = Journal::open(None);
            let hosts = all_hosts();
            let go = |trigger: &str| {
                pass_with(
                    &s,
                    &c,
                    &fs,
                    &records,
                    &Retry::instant(),
                    &journal,
                    trigger,
                    Work::Hosts(&hosts, false),
                )
            };
            let out = go("manual");
            assert_eq!(out.summary.total, 67, "{how:?}");
            assert_eq!(out.summary.verified, 67);
            let rel = folders::folder_rel_on(GRIM, PS5_ID).unwrap();
            assert!(rel.starts_with(&format!("Folders/PS5-{PS5_ID}/")), "{rel}");
            let dir = format!("/b/{rel}");
            let on_share = |fs: &MemFs| -> Vec<String> {
                fs.paths()
                    .into_iter()
                    .filter(|p| p.starts_with(&format!("{dir}/")) && p.ends_with(".zip"))
                    .collect()
            };
            assert_eq!(on_share(&fs).len(), 1, "{how:?}");
            let zip = fs.bytes(&on_share(&fs)[0]).unwrap();
            let local = std::env::temp_dir()
                .join(format!("ps5u-folder-{}-{how:?}.zip", std::process::id()));
            std::fs::write(&local, &zip).unwrap();
            assert_eq!(folders::noted_path(&local).as_deref(), Some(GRIM));
            let _ = std::fs::remove_file(&local);
            let drive_dir = format!("/mnt/ext0/ps5upload/save-backups/{rel}/");
            assert_eq!(
                c.drive
                    .files
                    .borrow()
                    .keys()
                    .filter(|p| p.starts_with(&drive_dir))
                    .count(),
                1
            );
            let run = journal.last(Which::Share).unwrap();
            let it = run.items.iter().find(|i| i.kind == "folder").unwrap();
            assert_eq!(it.path, GRIM);
            assert_eq!(it.id, format!("{HOST}|folder:{GRIM}"));
            // Nothing changed: nothing to do.
            let journal2 = Journal::open(None);
            let again = pass_with(
                &s,
                &c,
                &fs,
                &records,
                &Retry::instant(),
                &journal2,
                "manual",
                Work::Hosts(&hosts, false),
            );
            assert_eq!(again.summary.total, 0);
            // It changed twice: backed up each time, two versions kept (Versions to keep: 2).
            for n in [11, 12] {
                std::thread::sleep(Duration::from_millis(1100));
                c.folders.borrow_mut().get_mut(GRIM).unwrap()[0].1 =
                    format!("level {n}").into_bytes();
                let out = go("manual");
                assert_eq!(out.summary.total, 1);
                assert_eq!(out.summary.verified, 1);
            }
            assert_eq!(on_share(&fs).len(), 2, "{how:?}");
        }
    }

    /// Two PS5s with the same folder keep their backups apart (and prune their own); a PS5 that
    /// can't be told has its folder fail with why, never written where another's go.
    #[test]
    fn a_folder_of_two_ps5s_is_kept_apart_per_ps5() {
        let fs = MemFs::new(&[]);
        let s = ServerSettings {
            folders: vec![GRIM.into()],
            keep: 1,
            ..settings()
        };
        let hosts = all_hosts();
        let mut dirs = Vec::new();
        for id in ["F8461C0A0B01", "F8461C0A0B02"] {
            let c = console(0, 0);
            *c.id.borrow_mut() = Some(id.into());
            c.folders.borrow_mut().insert(
                GRIM.into(),
                vec![("Save/a.gdc".into(), id.as_bytes().to_vec())],
            );
            let out = pass_with(
                &s,
                &c,
                &fs,
                &MemRecords::default(),
                &Retry::instant(),
                &Journal::open(None),
                "manual",
                Work::Hosts(&hosts, false),
            );
            assert_eq!(out.summary.verified, out.summary.total);
            dirs.push(format!("/b/{}/", folders::folder_rel_on(GRIM, id).unwrap()));
        }
        assert_ne!(dirs[0], dirs[1]);
        for d in &dirs {
            let zips: Vec<String> = fs
                .paths()
                .into_iter()
                .filter(|p| p.starts_with(d) && p.ends_with(".zip"))
                .collect();
            // Versions to keep: 1, each PS5's own: the second PS5 pruned none of the first's.
            assert_eq!(zips.len(), 1, "{d}: {zips:?}");
        }
        // Not one written to the old shared place.
        let old = format!("/b/{}/", folders::folder_rel(GRIM));
        assert!(!fs.paths().iter().any(|p| p.starts_with(&old)));

        let c = console(0, 0);
        *c.id.borrow_mut() = None;
        c.folders
            .borrow_mut()
            .insert(GRIM.into(), vec![("Save/a.gdc".into(), b"x".to_vec())]);
        let journal = Journal::open(None);
        pass_with(
            &s,
            &c,
            &MemFs::new(&[]),
            &MemRecords::default(),
            &Retry::instant(),
            &journal,
            "manual",
            Work::Hosts(&hosts, false),
        );
        let run = journal.last(Which::Share).unwrap();
        let it = run.items.iter().find(|i| i.kind == "folder").unwrap();
        assert_eq!(it.step, crate::save_backup_run::Step::Failed);
        assert_eq!(it.error.as_deref(), Some(folders::UNKNOWN_CONSOLE));
    }

    /// Continue: a folder that could not be looked at now (the link dropped) stays left, with
    /// why; only one that is not there any more is skipped as gone.
    #[test]
    fn continue_skips_a_folder_only_when_it_is_gone() {
        let c = console(0, 0);
        c.folders
            .borrow_mut()
            .insert(GRIM.into(), vec![("a".into(), b"x".to_vec())]);
        let journal = Journal::open(None);
        let run = journal.begin(Which::Share, "manual").unwrap();
        run.add(vec![journal::new_item(
            HOST,
            &folders::entry(GRIM, 1),
            format!("{HOST}|{}", folders::item_path(GRIM)),
            &[Place::Share],
        )]);
        run.finish(Some(("console_gone".into(), "gone".into())));
        let last = journal.last(Which::Share).unwrap();
        assert_eq!(last.items[0].outcome(), Outcome::Left);

        *c.folder_err.borrow_mut() = Some("connection reset by peer".into());
        let again = journal.begin(Which::Share, "continue").unwrap();
        assert!(carry_over(&c, &again, &last, None).is_empty());
        let it = again.item(0).unwrap();
        assert_eq!(it.outcome(), Outcome::Left);
        assert_eq!(it.error.as_deref(), Some("connection reset by peer"));
        again.finish(None);

        *c.folder_err.borrow_mut() = None;
        c.folders.borrow_mut().clear();
        let again = journal.begin(Which::Share, "continue").unwrap();
        assert!(carry_over(&c, &again, &last, None).is_empty());
        let it = again.item(0).unwrap();
        assert_eq!(it.step, Step::Skipped);
        assert_eq!(
            it.error.as_deref(),
            Some("the folder is no longer on the PS5")
        );
        again.finish(None);
    }
}
