//! Keeping a console's Payload Manager in step with this computer's payloads ("sync with pldmgr
//! so i can autoload the right files").
//!
//! Payload Manager (itsPLK's `pldmgr.elf`, :8084) keeps its payloads under
//! `/data/pldmgr/payloads/` and loads the ones named in `/data/pldmgr/autoload.txt` at boot (0.5:
//! file names; 0.6: slots such as `local/kstuff`). Everything here goes through its own HTTP API,
//! never around it: an upload is written by Payload Manager to a temporary name in its storage
//! folder and renamed into place on the same device (0.5: `payloads/<name>.tmp` → `payloads/<stem>/
//! <name>`; 0.6: `payloads/local/.txn-*/new` → `payloads/local/<stem>`, with a journal and a SHA-256
//! re-read of the copy). Writing into its folder from outside would skip 0.6's slot metadata and
//! checksum, and a rename from anywhere else could cross devices, which panics the PS5 kernel.
//!
//! What the user picks, per console:
//!   - `ids`: payloads to keep in Payload Manager. A sync stores this computer's copy when Payload
//!     Manager lacks it or holds a different one (by SHA-256 of what we stored).
//!   - `autoload_ids`: of those, the ones Payload Manager loads at boot. A sync adds a chosen
//!     payload to the boot list once; if the user later takes it out in Payload Manager itself,
//!     it stays out. Unticking it here takes it out on the next sync.
//!
//! A sync never adds a payload the user did not choose, never touches a payload Payload Manager
//! installed from its own repository, keeps every other entry and pause of the boot list where it
//! was, and never stores into a folder that holds a payload that is not ours to replace.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::download::sha256_of_file;
use super::updates::{now_secs, write_json};
use super::{Library, LibraryError};
use crate::payload_manager::{self as pm, Autoload, StoreCheck, StoredPayload};

const CHOICES_FILE: &str = "pm-sync.json";

/// What a sync needs of a Payload Manager: [`HttpPm`] on a console, a fake in tests.
pub trait PmConsole {
    fn version(&self) -> Result<String, String>;
    fn list(&self) -> Result<Vec<StoredPayload>, String>;
    fn check(&self, filename: &str) -> Result<StoreCheck, String>;
    /// Store (or replace) `filename`. Payload Manager writes it through a temporary name and a
    /// same-folder rename of its own.
    fn store(&self, filename: &str, bytes: &[u8]) -> Result<(), String>;
    fn delete(&self, filename: &str) -> Result<(), String>;
    fn autoload_get(&self) -> Result<Autoload, String>;
    fn autoload_set(&self, enabled: bool, list: &[String]) -> Result<(), String>;
}

/// Payload Manager on a console, over its HTTP API on :8084.
pub struct HttpPm(pub String);

impl PmConsole for HttpPm {
    fn version(&self) -> Result<String, String> {
        pm::version(&self.0)
    }
    fn list(&self) -> Result<Vec<StoredPayload>, String> {
        pm::list(&self.0)
    }
    fn check(&self, filename: &str) -> Result<StoreCheck, String> {
        pm::check(&self.0, filename)
    }
    fn store(&self, filename: &str, bytes: &[u8]) -> Result<(), String> {
        pm::store(&self.0, filename, bytes)
    }
    fn delete(&self, filename: &str) -> Result<(), String> {
        pm::delete(&self.0, filename)
    }
    fn autoload_get(&self) -> Result<Autoload, String> {
        pm::autoload_get(&self.0)
    }
    fn autoload_set(&self, enabled: bool, list: &[String]) -> Result<(), String> {
        pm::autoload_set(&self.0, enabled, list)
    }
}

/// A file this app stored in a console's Payload Manager.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PmStoredRecord {
    pub payload_id: String,
    pub version: String,
    pub sha256: String,
}

/// The user's choice for one console, and what this app stored there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PmSyncChoice {
    /// Payloads to keep in Payload Manager.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Payloads Payload Manager loads at boot; each is also in `ids`.
    #[serde(default)]
    pub autoload_ids: Vec<String>,
    /// Chosen payloads a sync already put in the boot list, so taking one out in Payload
    /// Manager itself is respected.
    #[serde(default)]
    pub autoload_applied: Vec<String>,
    /// Unticked here since the last sync: the next sync takes their entry out.
    #[serde(default)]
    pub autoload_remove: Vec<String>,
    /// By file name.
    #[serde(default)]
    pub stored: BTreeMap<String, PmStoredRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ChoicesFile {
    #[serde(default)]
    consoles: BTreeMap<String, PmSyncChoice>,
}

/// One thing a sync did or left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PmChange {
    /// Stored this computer's copy. `from` is the version it replaced, when this app stored it.
    Stored {
        payload_id: String,
        filename: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
    },
    Unchanged {
        payload_id: String,
        filename: String,
        version: String,
    },
    /// Not stored, and why (`code`: `not_downloaded`, `folder_in_use`, `repo_managed`).
    Skipped {
        payload_id: String,
        code: String,
        reason: String,
    },
    Failed {
        payload_id: String,
        error: String,
    },
    AutoloadAdded {
        payload_id: String,
        entry: String,
    },
    AutoloadRenamed {
        payload_id: String,
        from: String,
        to: String,
    },
    AutoloadRemoved {
        payload_id: String,
        entry: String,
    },
    /// An older copy this app stored under another name, removed once nothing loads it.
    RemovedOld {
        payload_id: String,
        filename: String,
    },
}

/// What a sync did on one console.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PmSyncReport {
    pub host: String,
    pub at: i64,
    /// Payload Manager answered.
    pub present: bool,
    #[serde(default)]
    pub pm_version: String,
    /// Why nothing was done (Payload Manager absent, its list unreadable, nothing chosen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub changes: Vec<PmChange>,
    #[serde(default)]
    pub autoload_before: Vec<String>,
    #[serde(default)]
    pub autoload_after: Vec<String>,
}

impl PmSyncReport {
    /// Anything stored, removed or changed in the boot list.
    pub fn changed_anything(&self) -> bool {
        self.changes.iter().any(|c| {
            !matches!(
                c,
                PmChange::Unchanged { .. } | PmChange::Skipped { .. } | PmChange::Failed { .. }
            )
        })
    }
}

/// Payload Manager's own storage, not a USB find.
fn in_pm_storage(p: &StoredPayload) -> bool {
    p.path.starts_with("/data/pldmgr/")
}

/// Installed from Payload Manager's repository: it updates those itself.
fn repo_managed(p: &StoredPayload) -> bool {
    p.slot.starts_with("repo/") || p.path.contains("/payloads/repo/")
}

/// The boot-list entry that names `p`: its slot on 0.6, else its file name.
fn autoload_key(p: &StoredPayload) -> String {
    if p.slot.is_empty() {
        p.filename.clone()
    } else {
        p.slot.clone()
    }
}

/// The copy of `filename` in Payload Manager's own storage that is ours to replace.
fn ours<'a>(listed: &'a [StoredPayload], filename: &str) -> Option<&'a StoredPayload> {
    listed
        .iter()
        .find(|p| in_pm_storage(p) && !repo_managed(p) && p.filename == filename)
}

fn file_name_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Normalise a console address to the key the choices are kept under.
fn host_key(host: &str) -> String {
    crate::events::console_host(host.trim())
}

impl Library {
    fn read_choices(&self) -> ChoicesFile {
        std::fs::read(self.dir.join(CHOICES_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_choices(&self, f: &ChoicesFile) -> Result<(), LibraryError> {
        write_json(&self.dir, &self.dir.join(CHOICES_FILE), f)
    }

    /// What the user chose to keep in `host`'s Payload Manager.
    pub fn pm_sync_choice(&self, host: &str) -> PmSyncChoice {
        self.read_choices()
            .consoles
            .remove(&host_key(host))
            .unwrap_or_default()
    }

    /// Consoles with anything chosen.
    pub fn pm_sync_hosts(&self) -> Vec<String> {
        self.read_choices()
            .consoles
            .into_iter()
            .filter(|(_, c)| !c.ids.is_empty())
            .map(|(h, _)| h)
            .collect()
    }

    /// Every payload any console keeps in Payload Manager: an update check covers them too.
    pub fn pm_sync_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for c in self.read_choices().consoles.into_values() {
            for id in c.ids {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Set which payloads `host`'s Payload Manager keeps and loads at boot. A payload to load at
    /// boot is kept too. Ids must be the library's.
    pub fn set_pm_sync_choice(
        &self,
        host: &str,
        ids: &[String],
        autoload_ids: &[String],
    ) -> Result<PmSyncChoice, LibraryError> {
        let host = host_key(host);
        if host.is_empty() {
            return Err(LibraryError::new("bad_host", "no console address"));
        }
        for id in ids.iter().chain(autoload_ids) {
            self.resolve_entry(id)?;
        }
        let _g = super::updates::choices_lock();
        let mut file = self.read_choices();
        let c = file.consoles.entry(host).or_default();
        let mut new_ids: Vec<String> = Vec::new();
        for id in ids.iter().chain(autoload_ids) {
            if !new_ids.contains(id) {
                new_ids.push(id.clone());
            }
        }
        let mut new_auto: Vec<String> = Vec::new();
        for id in autoload_ids {
            if !new_auto.contains(id) {
                new_auto.push(id.clone());
            }
        }
        for id in &c.autoload_ids {
            if !new_auto.contains(id)
                && c.autoload_applied.contains(id)
                && !c.autoload_remove.contains(id)
            {
                c.autoload_remove.push(id.clone());
            }
        }
        c.autoload_remove.retain(|id| !new_auto.contains(id));
        c.autoload_applied.retain(|id| new_auto.contains(id));
        c.ids = new_ids;
        c.autoload_ids = new_auto;
        let out = c.clone();
        self.write_choices(&file)?;
        Ok(out)
    }

    /// Bring `host`'s Payload Manager in step with the user's choice (see the module notes).
    /// Payload Manager not answering is a report with `present: false`, not an error: the
    /// caller's own store and autoload are unaffected. The report is kept in the update state.
    pub fn sync_payload_manager(&self, host: &str, pm: &dyn PmConsole) -> PmSyncReport {
        let key = host_key(host);
        let mut report = PmSyncReport {
            host: key.clone(),
            at: now_secs(),
            ..PmSyncReport::default()
        };
        self.sync_into(&key, pm, &mut report);
        let kept = report.clone();
        let _ = self.edit_update_state(move |s| {
            s.pm_syncs.insert(kept.host.clone(), kept);
        });
        report
    }

    fn sync_into(&self, host: &str, pm: &dyn PmConsole, report: &mut PmSyncReport) {
        match pm.version() {
            Ok(v) => {
                report.present = true;
                report.pm_version = v;
            }
            Err(e) => {
                report.message = Some(format!(
                    "Payload Manager isn't running on this PS5 ({e}). Only this computer's \
                     payloads were updated."
                ));
                return;
            }
        }
        let mut choice = self.pm_sync_choice(host);
        if choice.ids.is_empty() && choice.autoload_remove.is_empty() {
            report.message = Some("No payloads are chosen for Payload Manager.".into());
            return;
        }
        let mut listed = match pm.list() {
            Ok(l) => l,
            Err(e) => {
                report.message = Some(e);
                return;
            }
        };

        // (payload id, the entry this app stored before under another name, the new name)
        let mut renamed: Vec<(String, StoredPayload, String)> = Vec::new();
        let mut stored_any = false;
        for id in choice.ids.clone() {
            let Some(local) = self.local_entry(&id) else {
                report.changes.push(PmChange::Skipped {
                    payload_id: id.clone(),
                    code: "not_downloaded".into(),
                    reason: "Not downloaded to this computer yet.".into(),
                });
                continue;
            };
            let filename = file_name_of(&local.path);
            let sha = match sha256_of_file(std::path::Path::new(&local.path)) {
                Ok(s) => s,
                Err(e) => {
                    report.changes.push(PmChange::Failed {
                        payload_id: id.clone(),
                        error: format!("read {}: {e}", local.path),
                    });
                    continue;
                }
            };
            // Payload Manager's repository has a payload by this name: it is that one's to
            // update, not ours.
            if listed
                .iter()
                .any(|p| in_pm_storage(p) && repo_managed(p) && p.filename == filename)
                && ours(&listed, &filename).is_none()
            {
                report.changes.push(PmChange::Skipped {
                    payload_id: id.clone(),
                    code: "repo_managed".into(),
                    reason: format!(
                        "Payload Manager installed {filename} from its own repository and updates it itself."
                    ),
                });
                continue;
            }
            let here = ours(&listed, &filename).cloned();
            let record = choice.stored.get(&filename).cloned();
            if here.is_some() && record.as_ref().is_some_and(|r| r.sha256 == sha) {
                report.changes.push(PmChange::Unchanged {
                    payload_id: id.clone(),
                    filename,
                    version: local.version.clone(),
                });
                continue;
            }
            // Earlier copies this app stored for the same payload under another name.
            let old_names: Vec<String> = choice
                .stored
                .iter()
                .filter(|(f, r)| r.payload_id == id && **f != filename)
                .map(|(f, _)| f.clone())
                .collect();
            let old_entries: Vec<StoredPayload> = old_names
                .iter()
                .filter_map(|n| ours(&listed, n).cloned())
                .collect();
            if here.is_none() {
                // Storing into a folder Payload Manager already has removes that folder's other
                // files. Only into a new folder, or one that holds our own older copy.
                match pm.check(&filename) {
                    Ok(chk) if chk.folder_exists && !chk.file_exists => {
                        let ours_there =
                            old_entries.iter().any(|p| p.folder.ends_with(&chk.folder));
                        if !ours_there {
                            report.changes.push(PmChange::Skipped {
                                payload_id: id.clone(),
                                code: "folder_in_use".into(),
                                reason: format!(
                                    "Payload Manager would store {filename} in its \"{}\" folder, which holds other files. Store it once from the Payload Manager tab to confirm.",
                                    chk.folder
                                ),
                            });
                            continue;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        report.changes.push(PmChange::Failed {
                            payload_id: id.clone(),
                            error: e,
                        });
                        continue;
                    }
                }
            }
            let bytes = match std::fs::read(&local.path) {
                Ok(b) if b.starts_with(b"\x7FELF") => b,
                Ok(_) => {
                    report.changes.push(PmChange::Failed {
                        payload_id: id.clone(),
                        error: format!("{} is not an ELF", local.path),
                    });
                    continue;
                }
                Err(e) => {
                    report.changes.push(PmChange::Failed {
                        payload_id: id.clone(),
                        error: format!("read {}: {e}", local.path),
                    });
                    continue;
                }
            };
            if let Err(e) = pm.store(&filename, &bytes) {
                report.changes.push(PmChange::Failed {
                    payload_id: id.clone(),
                    error: e,
                });
                continue;
            }
            stored_any = true;
            let from = record
                .map(|r| r.version)
                .or_else(|| {
                    old_names
                        .first()
                        .and_then(|n| choice.stored.get(n))
                        .map(|r| r.version.clone())
                })
                .filter(|v| !v.is_empty());
            report.changes.push(PmChange::Stored {
                payload_id: id.clone(),
                filename: filename.clone(),
                from,
                to: local.version.clone(),
            });
            choice.stored.insert(
                filename.clone(),
                PmStoredRecord {
                    payload_id: id.clone(),
                    version: local.version.clone(),
                    sha256: sha,
                },
            );
            for old in old_entries {
                renamed.push((id.clone(), old, filename.clone()));
            }
        }
        if stored_any {
            match pm.list() {
                Ok(l) => listed = l,
                Err(e) => report.message = Some(e),
            }
        }

        // The boot list: rename what we replaced, add what was chosen and not yet added, take
        // out what was unticked. Every other entry and pause stays where it was.
        let mut autoload_ok = false;
        let mut newly_applied: Vec<String> = Vec::new();
        match pm.autoload_get() {
            Ok(al) => {
                report.autoload_before = al.list.clone();
                let mut list = al.list.clone();
                for (id, old, new_name) in &renamed {
                    let Some(new) = ours(&listed, new_name) else {
                        continue;
                    };
                    let (from, to) = (autoload_key(old), autoload_key(new));
                    if from == to || !list.contains(&from) {
                        continue;
                    }
                    if list.contains(&to) {
                        list.retain(|e| e != &from);
                    } else {
                        for e in list.iter_mut().filter(|e| **e == from) {
                            *e = to.clone();
                        }
                    }
                    report.changes.push(PmChange::AutoloadRenamed {
                        payload_id: id.clone(),
                        from,
                        to,
                    });
                }
                let current_entry = |id: &str| -> Option<String> {
                    let local = self.local_entry(id)?;
                    ours(&listed, &file_name_of(&local.path)).map(autoload_key)
                };
                for id in choice.autoload_remove.clone() {
                    let mut keys: Vec<String> = choice
                        .stored
                        .iter()
                        .filter(|(_, r)| r.payload_id == id)
                        .filter_map(|(f, _)| ours(&listed, f).map(autoload_key))
                        .collect();
                    keys.extend(current_entry(&id));
                    for k in keys {
                        if list.contains(&k) {
                            list.retain(|e| e != &k);
                            report.changes.push(PmChange::AutoloadRemoved {
                                payload_id: id.clone(),
                                entry: k,
                            });
                        }
                    }
                }
                for id in &choice.autoload_ids {
                    if choice.autoload_applied.contains(id) {
                        continue;
                    }
                    let Some(k) = current_entry(id) else {
                        continue;
                    };
                    if !list.contains(&k) {
                        list.push(k.clone());
                        report.changes.push(PmChange::AutoloadAdded {
                            payload_id: id.clone(),
                            entry: k,
                        });
                    }
                    newly_applied.push(id.clone());
                }
                if list == al.list {
                    autoload_ok = true;
                } else {
                    match pm.autoload_set(al.enabled, &list) {
                        Ok(()) => autoload_ok = true,
                        Err(e) => {
                            // Nothing was written: what it would have done is still to do.
                            report.changes.retain(|c| {
                                !matches!(
                                    c,
                                    PmChange::AutoloadAdded { .. }
                                        | PmChange::AutoloadRemoved { .. }
                                        | PmChange::AutoloadRenamed { .. }
                                )
                            });
                            report.changes.push(PmChange::Failed {
                                payload_id: String::new(),
                                error: e,
                            });
                            list = al.list.clone();
                        }
                    }
                }
                report.autoload_after = list;
            }
            Err(e) => {
                report.message = Some(format!("Couldn't read the autoload list: {e}"));
            }
        }

        // An older copy under another name goes only once nothing loads it any more.
        if autoload_ok {
            for (id, old, _) in &renamed {
                if report.autoload_after.contains(&autoload_key(old)) {
                    continue;
                }
                if ours(&listed, &old.filename).is_none() {
                    choice.stored.remove(&old.filename);
                    continue;
                }
                match pm.delete(&old.filename) {
                    Ok(()) => {
                        choice.stored.remove(&old.filename);
                        report.changes.push(PmChange::RemovedOld {
                            payload_id: id.clone(),
                            filename: old.filename.clone(),
                        });
                    }
                    Err(e) => report.changes.push(PmChange::Failed {
                        payload_id: id.clone(),
                        error: e,
                    }),
                }
            }
        }

        let _g = super::updates::choices_lock();
        let mut file = self.read_choices();
        let c = file.consoles.entry(host.to_string()).or_default();
        c.stored = choice.stored;
        // The user may have changed the choice while this ran: keep theirs, add what we did.
        if autoload_ok {
            for id in newly_applied {
                if c.autoload_ids.contains(&id) && !c.autoload_applied.contains(&id) {
                    c.autoload_applied.push(id);
                }
            }
            c.autoload_remove
                .retain(|id| !choice.autoload_remove.contains(id));
        }
        let _ = self.write_choices(&file);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::library;
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap as Map;

    const KSTUFF: &str = "kstuff-echostretch";
    const SMP: &str = "shadowmountplus";
    const HOST: &str = "192.0.2.7";

    /// Payload Manager 0.6 on a console, as files: `/data/pldmgr/payloads/local/<stem>/<name>`
    /// and its boot list. An upload goes to a temporary name in the slot's parent folder and is
    /// renamed into place; a rename whose two folders differ is what panics a PS5, so the fake
    /// fails the test if it ever sees one.
    #[derive(Default)]
    struct FakePm {
        absent: bool,
        files: RefCell<Map<String, Vec<u8>>>,
        repo: RefCell<Vec<StoredPayload>>,
        autoload: RefCell<Autoload>,
        renames: RefCell<Vec<(String, String)>>,
        fail_store: bool,
        uploads: RefCell<Vec<String>>,
        autoload_writes: RefCell<usize>,
    }

    fn stem(name: &str) -> &str {
        name.strip_suffix(".elf").unwrap_or(name)
    }

    fn parent(path: &str) -> &str {
        path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
    }

    impl FakePm {
        fn slot_path(name: &str) -> String {
            format!("/data/pldmgr/payloads/local/{}/{name}", stem(name))
        }
        fn rename(&self, from: &str, to: &str) {
            // A slot folder replaced as a whole: the staged one sits in the same parent.
            assert_eq!(
                parent(from),
                parent(to),
                "rename across folders: {from} -> {to}"
            );
            self.renames.borrow_mut().push((from.into(), to.into()));
        }
        fn with_autoload(self, enabled: bool, list: &[&str]) -> Self {
            *self.autoload.borrow_mut() = Autoload {
                enabled,
                delay_s: 5,
                list: list.iter().map(|s| s.to_string()).collect(),
            };
            self
        }
        fn put(&self, name: &str, bytes: &[u8]) {
            self.files
                .borrow_mut()
                .insert(Self::slot_path(name), bytes.to_vec());
        }
        fn list_now(&self) -> Vec<StoredPayload> {
            let mut out: Vec<StoredPayload> = self
                .files
                .borrow()
                .keys()
                .map(|path| {
                    let filename = path.rsplit('/').next().unwrap().to_string();
                    StoredPayload {
                        slot: format!("local/{}", stem(&filename)),
                        folder: format!("local/{}", stem(&filename)),
                        path: path.clone(),
                        filename,
                        name: String::new(),
                        version: String::new(),
                        source: String::new(),
                        install_source: "upload".into(),
                    }
                })
                .collect();
            out.extend(self.repo.borrow().iter().cloned());
            out
        }
    }

    impl PmConsole for FakePm {
        fn version(&self) -> Result<String, String> {
            if self.absent {
                Err("no Payload Manager on :8084 (connection refused)".into())
            } else {
                Ok("0.6.1".into())
            }
        }
        fn list(&self) -> Result<Vec<StoredPayload>, String> {
            Ok(self.list_now())
        }
        fn check(&self, filename: &str) -> Result<StoreCheck, String> {
            let folder = format!("local/{}", stem(filename));
            let files = self.files.borrow();
            Ok(StoreCheck {
                folder_exists: files
                    .keys()
                    .any(|p| p.starts_with(&format!("/data/pldmgr/payloads/{folder}/"))),
                file_exists: files.contains_key(&Self::slot_path(filename)),
                folder,
            })
        }
        fn store(&self, filename: &str, bytes: &[u8]) -> Result<(), String> {
            self.uploads.borrow_mut().push(filename.into());
            if self.fail_store {
                return Err("Payload Manager refused the upload (HTTP 500: disk full)".into());
            }
            let target_dir = parent(&Self::slot_path(filename)).to_string();
            // Staged as a sibling of the slot folder, then renamed over it.
            self.rename(&format!("{}/.txn-new", parent(&target_dir)), &target_dir);
            self.files
                .borrow_mut()
                .retain(|p, _| !p.starts_with(&format!("{target_dir}/")));
            self.put(filename, bytes);
            Ok(())
        }
        fn delete(&self, filename: &str) -> Result<(), String> {
            self.files
                .borrow_mut()
                .remove(&Self::slot_path(filename))
                .map(|_| ())
                .ok_or_else(|| format!("{filename} is not stored"))
        }
        fn autoload_get(&self) -> Result<Autoload, String> {
            Ok(self.autoload.borrow().clone())
        }
        fn autoload_set(&self, enabled: bool, list: &[String]) -> Result<(), String> {
            *self.autoload_writes.borrow_mut() += 1;
            let delay_s = self.autoload.borrow().delay_s;
            *self.autoload.borrow_mut() = Autoload {
                enabled,
                delay_s,
                list: list.to_vec(),
            };
            Ok(())
        }
    }

    fn elf(fill: u8, len: usize) -> Vec<u8> {
        let mut v = b"\x7FELF".to_vec();
        v.resize(len, fill);
        v
    }

    fn cache(lib: &Library, id: &str, name: &str, version: &str, bytes: &[u8]) {
        let dir = lib.dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for f in std::fs::read_dir(&dir).unwrap().flatten() {
            if f.path().extension().and_then(|e| e.to_str()) == Some("elf") {
                std::fs::remove_file(f.path()).unwrap();
            }
        }
        std::fs::write(dir.join(name), bytes).unwrap();
        std::fs::write(dir.join("VERSION"), version).unwrap();
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn payload_manager_absent_is_said_and_nothing_is_touched() {
        let (_d, _h, lib) = library("pm-absent");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm {
            absent: true,
            ..FakePm::default()
        };
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(!r.present);
        assert!(r.message.unwrap().contains("isn't running"));
        assert!(r.changes.is_empty());
        assert!(pm.uploads.borrow().is_empty());
        // The report is kept for the screen.
        assert!(!lib.update_state().pm_syncs[HOST].present);
        // This computer's copy is untouched.
        assert_eq!(lib.local_entry(KSTUFF).unwrap().version, "v1");
    }

    #[test]
    fn a_sync_stores_the_chosen_payloads_and_only_adds_the_chosen_ones_to_the_boot_list() {
        let (_d, _h, lib) = library("pm-first");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        cache(&lib, SMP, "shadowmountplus.elf", "v3", &elf(2, 32));
        // Both kept in Payload Manager; only kstuff loads at boot.
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF, SMP]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm::default().with_autoload(true, &["local/etahen", "!2000"]);
        pm.put("etahen.elf", b"\x7FELFuser");

        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(r.present, "{r:?}");
        assert_eq!(*pm.uploads.borrow(), ["kstuff.elf", "shadowmountplus.elf"]);
        assert_eq!(
            pm.autoload.borrow().list,
            ["local/etahen", "!2000", "local/kstuff"],
            "the user's entries and pause stay; only the chosen one is added"
        );
        assert!(pm.autoload.borrow().enabled);
        assert!(r.changes.contains(&PmChange::AutoloadAdded {
            payload_id: KSTUFF.into(),
            entry: "local/kstuff".into()
        }));
        assert!(!r
            .changes
            .iter()
            .any(|c| matches!(c, PmChange::AutoloadAdded { payload_id, .. } if payload_id == SMP)));
        // The user's own payload is never touched.
        assert_eq!(
            pm.files.borrow()["/data/pldmgr/payloads/local/etahen/etahen.elf"],
            b"\x7FELFuser"
        );

        // A second sync with nothing new does nothing at all.
        let writes = *pm.autoload_writes.borrow();
        let again = lib.sync_payload_manager(HOST, &pm);
        assert!(!again.changed_anything(), "{again:?}");
        assert_eq!(pm.uploads.borrow().len(), 2);
        assert_eq!(*pm.autoload_writes.borrow(), writes);
    }

    #[test]
    fn an_update_replaces_the_copy_in_place_and_keeps_its_boot_slot() {
        let (_d, _h, lib) = library("pm-update");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm::default().with_autoload(true, &[]);
        lib.sync_payload_manager(HOST, &pm);
        // The user moves it first in Payload Manager's own list.
        pm.autoload_set(true, &ids(&["local/kstuff", "local/etahen"]))
            .unwrap();

        cache(&lib, KSTUFF, "kstuff.elf", "v2", &elf(9, 48));
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(r.changes.contains(&PmChange::Stored {
            payload_id: KSTUFF.into(),
            filename: "kstuff.elf".into(),
            from: Some("v1".into()),
            to: "v2".into(),
        }));
        assert_eq!(
            pm.files.borrow()["/data/pldmgr/payloads/local/kstuff/kstuff.elf"],
            elf(9, 48)
        );
        assert_eq!(pm.autoload.borrow().list, ["local/kstuff", "local/etahen"]);
        // Every write on the console was a same-folder rename.
        for (from, to) in pm.renames.borrow().iter() {
            assert_eq!(parent(from), parent(to));
        }
    }

    #[test]
    fn a_new_file_name_takes_over_the_old_entry_and_the_old_copy_goes() {
        let (_d, _h, lib) = library("pm-rename");
        // A custom repo whose asset name carries its version.
        let id = lib
            .add_custom_repo(&super::super::AddRepo {
                host: "github.com".into(),
                owner: "me".into(),
                name: "thing".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let id = id.as_str();
        cache(&lib, id, "thing_v1.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[id]), &ids(&[id]))
            .unwrap();
        let pm = FakePm::default().with_autoload(true, &["local/etahen"]);
        lib.sync_payload_manager(HOST, &pm);
        assert_eq!(
            pm.autoload.borrow().list,
            ["local/etahen", "local/thing_v1"]
        );

        cache(&lib, id, "thing_v2.elf", "v2", &elf(2, 32));
        let r = lib.sync_payload_manager(HOST, &pm);
        assert_eq!(
            pm.autoload.borrow().list,
            ["local/etahen", "local/thing_v2"]
        );
        assert!(r.changes.contains(&PmChange::AutoloadRenamed {
            payload_id: id.into(),
            from: "local/thing_v1".into(),
            to: "local/thing_v2".into(),
        }));
        assert!(r.changes.contains(&PmChange::RemovedOld {
            payload_id: id.into(),
            filename: "thing_v1.elf".into(),
        }));
        assert!(!pm
            .files
            .borrow()
            .contains_key("/data/pldmgr/payloads/local/thing_v1/thing_v1.elf"));
    }

    #[test]
    fn a_failed_upload_leaves_the_console_as_it_was() {
        let (_d, _h, lib) = library("pm-fail");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm {
            fail_store: true,
            ..FakePm::default()
        }
        .with_autoload(true, &["local/etahen"]);
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(matches!(r.changes[0], PmChange::Failed { .. }), "{r:?}");
        assert!(pm.files.borrow().is_empty());
        assert_eq!(pm.autoload.borrow().list, ["local/etahen"]);
        assert_eq!(*pm.autoload_writes.borrow(), 0);
        assert!(lib.pm_sync_choice(HOST).stored.is_empty());
        assert!(lib.pm_sync_choice(HOST).autoload_applied.is_empty());
    }

    #[test]
    fn a_folder_holding_someone_elses_files_is_left_alone() {
        let (_d, _h, lib) = library("pm-folder");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &[]).unwrap();
        let pm = FakePm::default().with_autoload(false, &[]);
        // The user's own file in the folder kstuff.elf would land in.
        pm.files.borrow_mut().insert(
            "/data/pldmgr/payloads/local/kstuff/kstuff-old-build.elf".into(),
            b"\x7FELFmine".to_vec(),
        );
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(matches!(&r.changes[0], PmChange::Skipped { code, .. } if code == "folder_in_use"));
        assert!(pm.uploads.borrow().is_empty());
    }

    #[test]
    fn a_repository_payload_is_payload_managers_to_update() {
        let (_d, _h, lib) = library("pm-repo");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm::default().with_autoload(true, &["repo/kstuff@main"]);
        pm.repo.borrow_mut().push(StoredPayload {
            path: "/data/pldmgr/payloads/repo/kstuff@main/kstuff.elf".into(),
            slot: "repo/kstuff@main".into(),
            filename: "kstuff.elf".into(),
            folder: "repo/kstuff@main".into(),
            name: "kstuff".into(),
            version: "1.6".into(),
            source: "itsPLK".into(),
            install_source: "repository".into(),
        });
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(matches!(&r.changes[0], PmChange::Skipped { code, .. } if code == "repo_managed"));
        assert!(pm.uploads.borrow().is_empty());
        assert_eq!(pm.autoload.borrow().list, ["repo/kstuff@main"]);
    }

    #[test]
    fn taken_out_in_payload_manager_stays_out_and_unticked_here_comes_out() {
        let (_d, _h, lib) = library("pm-remove");
        cache(&lib, KSTUFF, "kstuff.elf", "v1", &elf(1, 32));
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &ids(&[KSTUFF]))
            .unwrap();
        let pm = FakePm::default().with_autoload(true, &[]);
        lib.sync_payload_manager(HOST, &pm);
        assert_eq!(pm.autoload.borrow().list, ["local/kstuff"]);

        // Taken out in Payload Manager itself: a later sync does not put it back.
        pm.autoload_set(true, &[]).unwrap();
        lib.sync_payload_manager(HOST, &pm);
        assert!(pm.autoload.borrow().list.is_empty());

        // Back in by hand, then unticked here: the next sync takes it out.
        pm.autoload_set(true, &ids(&["!500", "local/kstuff"]))
            .unwrap();
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &[]).unwrap();
        let r = lib.sync_payload_manager(HOST, &pm);
        assert_eq!(pm.autoload.borrow().list, ["!500"]);
        assert!(r.changes.contains(&PmChange::AutoloadRemoved {
            payload_id: KSTUFF.into(),
            entry: "local/kstuff".into()
        }));
        assert!(lib.pm_sync_choice(HOST).autoload_remove.is_empty());
        // Still kept in Payload Manager.
        assert!(pm
            .files
            .borrow()
            .contains_key("/data/pldmgr/payloads/local/kstuff/kstuff.elf"));
    }

    #[test]
    fn a_payload_not_downloaded_is_skipped_and_unknown_ids_are_refused() {
        let (_d, _h, lib) = library("pm-missing");
        lib.set_pm_sync_choice(HOST, &ids(&[KSTUFF]), &[]).unwrap();
        let pm = FakePm::default().with_autoload(true, &[]);
        let r = lib.sync_payload_manager(HOST, &pm);
        assert!(
            matches!(&r.changes[0], PmChange::Skipped { code, .. } if code == "not_downloaded")
        );
        assert_eq!(
            lib.set_pm_sync_choice(HOST, &ids(&["../etc"]), &[])
                .unwrap_err()
                .code,
            "unknown_payload"
        );
        // A boot-load choice is kept too; the host is stored without its port.
        let c = lib
            .set_pm_sync_choice("192.0.2.7:9021", &[], &ids(&[SMP]))
            .unwrap();
        assert_eq!(c.ids, [SMP]);
        assert_eq!(lib.pm_sync_choice(HOST).autoload_ids, [SMP]);
    }
}
