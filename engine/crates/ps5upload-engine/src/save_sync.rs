//! Save sync: one user's saves kept in step across two PS5s, never overwritten on its own.
//!
//! A **pair** is a user on one console and a user on another (`save-sync.json` in `state/`, next
//! to the save-backup settings). Every save both of them have, PS5 games' and emulators', is in
//! it unless the user leaves it out; PS4 saves can't go to another console and are shown as such.
//!
//! What changed is decided by **fingerprints**, never by dates: the console resets a restored
//! save's time and the two consoles' clocks drift. A fingerprint is the SHA-256 of the save
//! folder's listing as the automatic backups read it (`save_backup::fingerprint`: every file's
//! name, size and the console's own time for it), so it is compared only with an earlier
//! fingerprint of the same save on the same console. After each copy both sides' fingerprints are
//! kept (`save-sync-state.json`); a side whose fingerprint differs from that has changed since.
//!
//! - **Check now** (`GET /api/save-sync/status?pair=`): per save, in step, one side newer, both
//!   changed (a conflict: the user picks), first time (nothing kept yet: the user picks), only on
//!   one side, or unavailable (a console off or in rest, settling after a wake, the game running,
//!   a PS4 save).
//! - **Apply** (`POST /api/save-sync/apply`): the picked saves, one at a time, each through
//!   [`save_copy::copy_to_console`] (the target backed up first, put back on failure). One apply at
//!   a time per pair of consoles. A save whose state changed since the check is refused.
//! - **After a game closes**: the same running-game look the backups to a network share use. A
//!   game closed on one side whose save changed while the other side's did not becomes a
//!   suggestion ("Send …'s save to …?"), applied only when the user says so. A console that is off
//!   or resting is never woken: the suggestion waits and is looked at again once it answers, and
//!   nothing runs in the first 90 s after a console comes back or while its helper says it is
//!   settling.
//!
//! The console work goes through [`Env`], so every decision is tested without a PS5.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::save_backup::{self as local, json_error, now_ms};

const SETTINGS_FILE: &str = "save-sync.json";
const STATE_FILE: &str = "save-sync-state.json";
/// How often consoles are asked what runs (the same minute the server backups use).
pub const TICK: Duration = Duration::from_secs(60);
/// Nothing runs on a console this soon after it answers again (back from rest or off).
pub const AFTER_WAKE_MS: u64 = 90_000;

// ── model ───────────────────────────────────────────────────────────────

/// A user on a console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSide {
    /// The console (bare IP, as the app's console list has it).
    pub host: String,
    pub user_id: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncPair {
    pub id: String,
    pub a: SyncSide,
    pub b: SyncSide,
    /// Saves left out (their keys, see [`Item::key`]). Everything else both sides have is in.
    #[serde(default)]
    pub excluded: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub pairs: Vec<SyncPair>,
}

/// The fingerprint of a side as last seen, and when it was first seen to differ from the one
/// before (this engine's clock, so approximate: it is seen at a check, not when it was written).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncSeen {
    pub fp: String,
    pub size: i64,
    #[serde(default)]
    pub changed_ms: Option<u64>,
}

/// Both sides' fingerprints right after the last copy between them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncRecord {
    pub a: String,
    pub b: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncItemState {
    #[serde(default)]
    pub synced: Option<SyncRecord>,
    #[serde(default)]
    pub a: Option<SyncSeen>,
    #[serde(default)]
    pub b: Option<SyncSeen>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    AToB,
    BToA,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncSuggestionState {
    /// The closed side changed, the other did not: Send copies it.
    Ready,
    /// The closed side changed; the other console is off, resting or settling: looked at again
    /// once it answers.
    Waiting,
    /// Both changed since the last copy: the user picks in Save sync.
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncSuggestion {
    pub pair: String,
    pub key: String,
    pub title_id: String,
    pub kind: String,
    pub direction: SyncDirection,
    pub state: SyncSuggestionState,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// `<pair id>|<key>` → what is kept of that save.
    #[serde(default)]
    pub items: BTreeMap<String, SyncItemState>,
    #[serde(default)]
    pub suggestions: Vec<SyncSuggestion>,
}

fn item_key(pair: &str, key: &str) -> String {
    format!("{pair}|{key}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    InStep,
    ANewer,
    BNewer,
    /// Both changed since the last copy.
    BothChanged,
    /// Never copied by save sync: no way to tell which is newer.
    FirstTime,
    OnlyOnA,
    OnlyOnB,
    Unavailable,
    /// Left out of the pair.
    Excluded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// The console did not answer (off, in rest, no helper).
    Offline,
    /// The console is going to rest or has just woken.
    Settling,
    /// The game (or its emulator) runs on that console.
    Running,
    /// A PS4 save: it can't go to another console.
    Ps4,
    /// Reading the save failed (see `last_error`).
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SyncSideView {
    pub path: String,
    pub size: i64,
    /// When this engine first saw the save differ (approximate), if it has.
    pub changed_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SyncRow {
    pub key: String,
    pub title_id: String,
    pub kind: String,
    pub status: SyncStatus,
    pub reason: Option<SyncReason>,
    /// Which console the reason is about: `a` or `b`.
    pub reason_side: Option<char>,
    pub a: Option<SyncSideView>,
    pub b: Option<SyncSideView>,
    pub synced_ms: Option<u64>,
    pub last_error: Option<String>,
    /// Which side changed since the last copy, where it could be read.
    #[serde(skip)]
    pub a_changed: Option<bool>,
    #[serde(skip)]
    pub b_changed: Option<bool>,
    /// The titles whose closing means this save may have changed.
    #[serde(skip)]
    pub apps: Vec<String>,
}

// ── the console work ────────────────────────────────────────────────────

/// What save sync asks of the consoles.
pub trait Env {
    /// Every save on `host` (PS5, PS4 and the emulators'). Err: the console did not answer.
    fn saves(&self, host: &str) -> Result<Vec<SaveEntry>, String>;
    /// The titles running on `host`, upper-case.
    fn running(&self, host: &str) -> Result<BTreeSet<String>, String>;
    /// The console is going to rest or settling after a wake.
    fn settling(&self, host: &str) -> bool;
    fn fingerprint(&self, host: &str, entry: &SaveEntry) -> Result<String, String>;
    /// Copies `src` (on `from`) over `dst` on `to` (an emulator's save with none there yet: into
    /// the same place), the target backed up first.
    fn copy(
        &self,
        from: &SyncSide,
        to: &SyncSide,
        src: &SaveEntry,
        dst: Option<&SaveEntry>,
    ) -> Result<(), String>;
}

pub struct Live;

impl Env for Live {
    fn saves(&self, host: &str) -> Result<Vec<SaveEntry>, String> {
        local::list_saves(host)
    }
    fn running(&self, host: &str) -> Result<BTreeSet<String>, String> {
        crate::save_backup_server::running_titles(host)
    }
    fn settling(&self, host: &str) -> bool {
        ps5upload_core::hw::hw_power(&crate::console_addr(host))
            .map(|p| settling_phase(p.power_phase))
            .unwrap_or(false)
    }
    fn fingerprint(&self, host: &str, entry: &SaveEntry) -> Result<String, String> {
        Ok(digest(&local::fingerprint(host, entry)?))
    }
    fn copy(
        &self,
        from: &SyncSide,
        to: &SyncSide,
        src: &SaveEntry,
        dst: Option<&SaveEntry>,
    ) -> Result<(), String> {
        if src.kind == "ps5" {
            crate::save_copy::copy_to_console(
                &crate::save_copy::Live::new(&from.host),
                &crate::save_copy::Live::new(&to.host),
                &src.title_id,
                src.user_id as u32,
                to.user_id,
            )
            .map(|_| ())
            .map_err(|f| f.msg)
        } else {
            let (to_path, to_user) = dst.map_or((src.path.as_str(), src.user_id as u32), |d| {
                (d.path.as_str(), d.user_id as u32)
            });
            crate::save_copy::copy_emulator_save(&from.host, &to.host, src, to_path, to_user)
        }
    }
}

/// The helper's power phase in which nothing runs: going to rest (1) or settling after a wake
/// (2). An older helper says 0; a phase a newer one adds is not waited on.
pub fn settling_phase(phase: u8) -> bool {
    matches!(phase, 1 | 2)
}

/// The fingerprint of a save folder's listing (see the module docs).
pub fn digest(listing: &[(String, u64, i64)]) -> String {
    let mut h = Sha256::new();
    for (name, size, mtime) in listing {
        h.update(name.as_bytes());
        h.update([0]);
        h.update(size.to_le_bytes());
        h.update(mtime.to_le_bytes());
    }
    h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// ── deciding ────────────────────────────────────────────────────────────

/// One side's saves in the pair, by key: its user's PS5 and PS4 saves (by title; a PS5 save
/// before a PS4 one), and the emulators' (by kind and game; a Switch game's whatever its profile
/// folder, which differs between consoles). The console's own folders are left out.
pub fn items_of(saves: &[SaveEntry], user: u32) -> BTreeMap<String, SaveEntry> {
    let mut out: BTreeMap<String, SaveEntry> = BTreeMap::new();
    let mut sorted: Vec<&SaveEntry> = saves
        .iter()
        .filter(|e| crate::emulator_saves::backs_up(e))
        .collect();
    sorted.sort_by(|x, y| x.path.cmp(&y.path));
    for e in sorted {
        let key = if crate::emulator_saves::is_emulator_save(e) {
            if e.kind == crate::emulator_saves::KIND {
                format!("{}:{}", e.kind, e.title_id)
            } else {
                format!("{}:{}:{}", e.kind, e.title_id, e.user_id)
            }
        } else if (e.kind == "ps5" || e.kind == "ps4") && e.user_id as u32 == user {
            e.title_id.to_ascii_uppercase()
        } else {
            continue;
        };
        match out.get(&key) {
            Some(have) if have.kind == "ps5" || have.kind == e.kind => {}
            _ => {
                out.insert(key, e.clone());
            }
        }
    }
    out
}

/// What changed since the last copy, from both sides' fingerprints now.
pub fn decide(a: &str, b: &str, synced: Option<&SyncRecord>) -> SyncStatus {
    let Some(s) = synced else {
        return SyncStatus::FirstTime;
    };
    match (a != s.a, b != s.b) {
        (false, false) => SyncStatus::InStep,
        (true, false) => SyncStatus::ANewer,
        (false, true) => SyncStatus::BNewer,
        (true, true) => SyncStatus::BothChanged,
    }
}

/// Whether `dir` may be applied to a save in `status`: the changed side, or either when both
/// changed or nothing was kept yet; one side alone only from that side.
pub fn allowed(status: SyncStatus, dir: SyncDirection) -> bool {
    matches!(
        (status, dir),
        (SyncStatus::BothChanged | SyncStatus::FirstTime, _)
            | (
                SyncStatus::ANewer | SyncStatus::OnlyOnA,
                SyncDirection::AToB
            )
            | (
                SyncStatus::BNewer | SyncStatus::OnlyOnB,
                SyncDirection::BToA
            )
    )
}

/// `prev` with the fingerprint seen now: a different one is a change seen at `now`.
pub fn observe(prev: Option<&SyncSeen>, fp: &str, size: i64, now: u64) -> SyncSeen {
    match prev {
        Some(p) if p.fp == fp => SyncSeen { size, ..p.clone() },
        Some(_) => SyncSeen {
            fp: fp.to_string(),
            size,
            changed_ms: Some(now),
        },
        None => SyncSeen {
            fp: fp.to_string(),
            size,
            changed_ms: None,
        },
    }
}

/// Why a side can't be used now, if it can't.
#[derive(Clone, Debug)]
struct SideNow {
    saves: Option<BTreeMap<String, SaveEntry>>,
    blocked: Option<SyncReason>,
    running: BTreeSet<String>,
}

fn look(env: &dyn Env, side: &SyncSide, settle_from: &dyn Fn(&str) -> bool) -> SideNow {
    let Ok(saves) = env.saves(&side.host) else {
        return SideNow {
            saves: None,
            blocked: Some(SyncReason::Offline),
            running: BTreeSet::new(),
        };
    };
    let items = Some(items_of(&saves, side.user_id));
    if settle_from(&side.host) || env.settling(&side.host) {
        return SideNow {
            saves: items,
            blocked: Some(SyncReason::Settling),
            running: BTreeSet::new(),
        };
    }
    match env.running(&side.host) {
        Ok(running) => SideNow {
            saves: items,
            blocked: None,
            running,
        },
        Err(_) => SideNow {
            saves: items,
            blocked: Some(SyncReason::Offline),
            running: BTreeSet::new(),
        },
    }
}

fn apps_of(e: &SaveEntry) -> Vec<String> {
    crate::emulator_saves::titles_of(crate::emulator_saves::app_of(e))
        .into_iter()
        .map(|t| t.to_ascii_uppercase())
        .collect()
}

fn view(e: &SaveEntry, seen: Option<&SyncSeen>) -> SyncSideView {
    SyncSideView {
        path: e.path.clone(),
        size: e.size,
        changed_ms: seen.and_then(|s| s.changed_ms),
    }
}

/// Check now: every save of the pair (`only`: just the keys it takes), with what is kept of each
/// updated in `state`. `settle_from` says a console answered again too recently to be used.
pub fn check(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    now: u64,
    only: &dyn Fn(&str, &[String]) -> bool,
    settle_from: &dyn Fn(&str) -> bool,
) -> Vec<SyncRow> {
    let a = look(env, &pair.a, settle_from);
    let b = look(env, &pair.b, settle_from);
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for s in [&a, &b] {
        if let Some(items) = &s.saves {
            keys.extend(items.keys().cloned());
        }
    }
    // A console that did not answer: what is known of its saves from before.
    let prefix = format!("{}|", pair.id);
    for k in state.items.keys() {
        if let Some(key) = k.strip_prefix(&prefix) {
            if a.saves.is_none() || b.saves.is_none() {
                keys.insert(key.to_string());
            }
        }
    }
    let mut rows = Vec::new();
    for key in keys {
        let ea = a.saves.as_ref().and_then(|m| m.get(&key));
        let eb = b.saves.as_ref().and_then(|m| m.get(&key));
        let any = ea.or(eb);
        let apps = any.map(apps_of).unwrap_or_default();
        if !only(&key, &apps) {
            continue;
        }
        let ik = item_key(&pair.id, &key);
        let kept = state.items.get(&ik).cloned().unwrap_or_default();
        let title_id = any
            .map(|e| e.title_id.clone())
            .unwrap_or_else(|| key.split(':').nth(1).unwrap_or(&key).to_string());
        let mut row = SyncRow {
            key: key.clone(),
            title_id: title_id.to_ascii_uppercase(),
            kind: any.map(|e| e.kind.clone()).unwrap_or_default(),
            status: SyncStatus::Unavailable,
            reason: None,
            reason_side: None,
            a: ea.map(|e| view(e, kept.a.as_ref())),
            b: eb.map(|e| view(e, kept.b.as_ref())),
            synced_ms: kept.synced.as_ref().map(|s| s.at_ms),
            last_error: kept.last_error.clone(),
            a_changed: None,
            b_changed: None,
            apps: apps.clone(),
        };
        if pair.excluded.contains(&key) {
            row.status = SyncStatus::Excluded;
            rows.push(row);
            continue;
        }
        if row.kind == "ps4" {
            row.reason = Some(SyncReason::Ps4);
            rows.push(row);
            continue;
        }
        // The game running on either console holds it, whether or not that side has a save.
        let block = |s: &SideNow, c: char| -> Option<(SyncReason, char)> {
            if let Some(r) = s.blocked {
                return Some((r, c));
            }
            apps.iter()
                .any(|t| s.running.contains(t))
                .then_some((SyncReason::Running, c))
        };
        // Fingerprint what can be read, so a change on an answering side is still seen.
        let mut fp = |s: &SideNow, side: &SyncSide, e: Option<&SaveEntry>, c: char| {
            let e = e?;
            if s.blocked.is_some() {
                return None;
            }
            match env.fingerprint(&side.host, e) {
                Ok(f) => {
                    let prev = if c == 'a' {
                        kept.a.as_ref()
                    } else {
                        kept.b.as_ref()
                    };
                    let seen = observe(prev, &f, e.size, now);
                    let slot = state.items.entry(ik.clone()).or_default();
                    if c == 'a' {
                        slot.a = Some(seen.clone());
                    } else {
                        slot.b = Some(seen.clone());
                    }
                    Some(Ok((f, seen)))
                }
                Err(err) => Some(Err(err)),
            }
        };
        let fa = fp(&a, &pair.a, ea, 'a');
        let fb = fp(&b, &pair.b, eb, 'b');
        let synced = kept.synced.as_ref();
        if let Some(Ok((f, seen))) = &fa {
            row.a_changed = synced.map(|s| s.a != *f);
            row.a = ea.map(|e| view(e, Some(seen)));
        }
        if let Some(Ok((f, seen))) = &fb {
            row.b_changed = synced.map(|s| s.b != *f);
            row.b = eb.map(|e| view(e, Some(seen)));
        }
        if let Some((r, c)) = block(&a, 'a').or_else(|| block(&b, 'b')) {
            row.reason = Some(r);
            row.reason_side = Some(c);
            rows.push(row);
            continue;
        }
        for (f, c) in [(&fa, 'a'), (&fb, 'b')] {
            if let Some(Err(e)) = f {
                row.reason = Some(SyncReason::Error);
                row.reason_side = Some(c);
                row.last_error = Some(e.clone());
            }
        }
        if row.reason.is_some() {
            rows.push(row);
            continue;
        }
        row.status = match (&fa, &fb) {
            (Some(Ok((x, _))), Some(Ok((y, _)))) => decide(x, y, synced),
            (Some(_), None) => SyncStatus::OnlyOnA,
            (None, Some(_)) => SyncStatus::OnlyOnB,
            _ => SyncStatus::Unavailable,
        };
        rows.push(row);
    }
    rows
}

/// Copies one save of the pair in `dir`, after checking it again: refused when its state no
/// longer allows that direction (it changed since the check), a game runs, or a PS5 save has no
/// save on the other side yet. Both sides' fingerprints are kept after it.
pub fn apply(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    key: &str,
    dir: SyncDirection,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let rows = check(env, pair, state, now, &|k, _| k == key, settle_from);
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| "That save is not on either console any more.".to_string())?;
    let (from, to, src_c) = match dir {
        SyncDirection::AToB => (&pair.a, &pair.b, 'a'),
        SyncDirection::BToA => (&pair.b, &pair.a, 'b'),
    };
    let result = (|| {
        match (row.status, row.reason) {
            (SyncStatus::Excluded, _) => return Err("That save is left out of sync.".to_string()),
            (_, Some(SyncReason::Ps4)) => {
                return Err("A PS4 save can't be copied to another console.".to_string())
            }
            (_, Some(SyncReason::Running)) => {
                return Err(format!(
                    "{} is running on a console. Close it, then try again.",
                    row.title_id
                ))
            }
            (_, Some(SyncReason::Offline | SyncReason::Settling)) => {
                return Err("A console is off, resting or just woke up. Try again later.".into())
            }
            (_, Some(SyncReason::Error)) => {
                return Err(row
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Could not read the save.".into()))
            }
            _ => {}
        }
        if row.kind == "ps5" && matches!(row.status, SyncStatus::OnlyOnA | SyncStatus::OnlyOnB) {
            return Err(format!(
                "There is no save of {} on {} yet. Start the game once there and save first.",
                row.title_id, to.host
            ));
        }
        if !allowed(row.status, dir) {
            return Err("This save changed since the check. Check again.".to_string());
        }
        let src_saves = items_of(&env.saves(&from.host)?, from.user_id);
        let dst_saves = items_of(&env.saves(&to.host)?, to.user_id);
        let src = src_saves
            .get(key)
            .ok_or_else(|| "That save is no longer there.".to_string())?;
        env.copy(from, to, src, dst_saves.get(key))?;
        // Both sides again: the copy rewrote the target, and opening the source can touch it.
        let a_now = items_of(&env.saves(&pair.a.host)?, pair.a.user_id);
        let b_now = items_of(&env.saves(&pair.b.host)?, pair.b.user_id);
        let (ea, eb) = (a_now.get(key), b_now.get(key));
        let (Some(ea), Some(eb)) = (ea, eb) else {
            return Err("The save was copied but could not be read back.".to_string());
        };
        let fa = env.fingerprint(&pair.a.host, ea)?;
        let fb = env.fingerprint(&pair.b.host, eb)?;
        Ok((fa, fb, ea.size, eb.size))
    })();
    let slot = state.items.entry(item_key(&pair.id, key)).or_default();
    match result {
        Ok((fa, fb, sa, sb)) => {
            slot.a = Some(observe(slot.a.as_ref(), &fa, sa, now));
            slot.b = Some(observe(slot.b.as_ref(), &fb, sb, now));
            slot.synced = Some(SyncRecord {
                a: fa,
                b: fb,
                at_ms: now,
            });
            slot.last_error = None;
            state
                .suggestions
                .retain(|s| !(s.pair == pair.id && s.key == key));
            crate::log_info!(
                "save sync: {key} copied from {} ({}) to {} ({})",
                from.host,
                src_c,
                to.host,
                if src_c == 'a' { 'b' } else { 'a' }
            );
            Ok(())
        }
        Err(e) => {
            slot.last_error = Some(e.clone());
            crate::log_warn!(
                "save sync: {key} from {} to {} not copied: {e}",
                from.host,
                to.host
            );
            Err(e)
        }
    }
}

/// What a game closing on side `closed` makes of `row`: a suggestion, or None (nothing to send,
/// never copied by save sync, or not decidable).
pub fn suggestion_for(row: &SyncRow, closed: char) -> Option<SyncSuggestionState> {
    let (mine, theirs) = if closed == 'a' {
        (row.a_changed, row.b_changed)
    } else {
        (row.b_changed, row.a_changed)
    };
    if mine != Some(true) {
        return None;
    }
    match row.reason {
        Some(SyncReason::Offline | SyncReason::Settling) if row.reason_side != Some(closed) => {
            Some(SyncSuggestionState::Waiting)
        }
        Some(_) => None,
        None => match theirs {
            Some(false) => Some(SyncSuggestionState::Ready),
            Some(true) => Some(SyncSuggestionState::Conflict),
            None => None,
        },
    }
}

/// Keeps `s` as the one suggestion of its save (a newer one replaces it).
pub fn put_suggestion(state: &mut State, s: SyncSuggestion) {
    state
        .suggestions
        .retain(|x| !(x.pair == s.pair && x.key == s.key));
    state.suggestions.push(s);
}

/// What the after-close watch keeps between looks.
#[derive(Default)]
pub struct Watch {
    /// The titles running on each console at the last look.
    pub seen: BTreeMap<String, BTreeSet<String>>,
    /// Whether each console answered at the last look.
    pub answered: BTreeMap<String, bool>,
    /// When each console answered again after not answering (engine ms).
    pub back_at: BTreeMap<String, u64>,
    /// Games closed and not looked at yet: (pair, closed side, title).
    pub closed: BTreeSet<(String, char, String)>,
}

impl Watch {
    pub fn too_soon(&self, host: &str, now: u64) -> bool {
        self.back_at
            .get(host)
            .is_some_and(|t| now.saturating_sub(*t) < AFTER_WAKE_MS)
    }
}

/// One look of the after-close watch over every pair. `busy` says a console is busy with a
/// transfer or install (looked at on a later tick).
pub fn watch_once(
    env: &dyn Env,
    settings: &Settings,
    state: &mut State,
    watch: &mut Watch,
    now: u64,
    busy: &dyn Fn(&str) -> bool,
) {
    let mut hosts: BTreeSet<&str> = BTreeSet::new();
    for p in &settings.pairs {
        hosts.insert(&p.a.host);
        hosts.insert(&p.b.host);
    }
    for host in hosts {
        match env.running(host) {
            Ok(titles) => {
                if watch.answered.get(host) == Some(&false) {
                    watch.back_at.insert(host.to_string(), now);
                }
                watch.answered.insert(host.to_string(), true);
                let closed =
                    crate::save_backup_server::closed_titles(watch.seen.get(host), &titles);
                for t in closed {
                    for p in &settings.pairs {
                        for (side, c) in [(&p.a, 'a'), (&p.b, 'b')] {
                            if side.host == host {
                                watch.closed.insert((p.id.clone(), c, t.clone()));
                            }
                        }
                    }
                }
                watch.seen.insert(host.to_string(), titles);
            }
            Err(_) => {
                // A console that does not answer tells nothing: no game is taken for closed.
                watch.answered.insert(host.to_string(), false);
                watch.seen.remove(host);
            }
        }
    }
    // Suggestions waiting for the other console are looked at again.
    let waiting: Vec<(String, char, String)> = state
        .suggestions
        .iter()
        .filter(|s| s.state == SyncSuggestionState::Waiting)
        .map(|s| {
            let c = if s.direction == SyncDirection::AToB {
                'a'
            } else {
                'b'
            };
            (s.pair.clone(), c, format!("key:{}", s.key))
        })
        .collect();
    let mut todo: Vec<(String, char, String)> = watch.closed.iter().cloned().collect();
    todo.extend(waiting);
    for (pair_id, c, what) in todo {
        let Some(pair) = settings.pairs.iter().find(|p| p.id == pair_id) else {
            watch.closed.remove(&(pair_id, c, what));
            continue;
        };
        let mine = if c == 'a' { &pair.a } else { &pair.b };
        if watch.answered.get(&mine.host) != Some(&true)
            || watch.too_soon(&mine.host, now)
            || busy(&mine.host)
        {
            continue;
        }
        let only = |k: &str, apps: &[String]| match what.strip_prefix("key:") {
            Some(key) => k == key,
            None => apps.contains(&what),
        };
        let settle = |h: &str| watch.too_soon(h, now) || busy(h);
        let rows = check(env, pair, state, now, &only, &settle);
        let from_here = if c == 'a' {
            SyncDirection::AToB
        } else {
            SyncDirection::BToA
        };
        for row in &rows {
            match suggestion_for(row, c) {
                Some(st) => {
                    let fresh = !state
                        .suggestions
                        .iter()
                        .any(|s| s.pair == pair.id && s.key == row.key && s.state == st);
                    if fresh {
                        crate::log_info!(
                            "save sync: suggest {} from {} ({:?})",
                            row.title_id,
                            mine.host,
                            st
                        );
                    }
                    let at_ms = state
                        .suggestions
                        .iter()
                        .find(|s| s.pair == pair.id && s.key == row.key && s.state == st)
                        .map_or(now, |s| s.at_ms);
                    put_suggestion(
                        state,
                        SyncSuggestion {
                            pair: pair.id.clone(),
                            key: row.key.clone(),
                            title_id: row.title_id.clone(),
                            kind: row.kind.clone(),
                            direction: from_here,
                            state: st,
                            at_ms,
                        },
                    );
                }
                None => {
                    // Nothing to send from this side: one from this side goes (a waiting one once
                    // decided; one from the other side stays).
                    let undecided = matches!(row.reason, Some(SyncReason::Running));
                    if !undecided {
                        state.suggestions.retain(|s| {
                            !(s.pair == pair.id && s.key == row.key && s.direction == from_here)
                        });
                    }
                }
            }
        }
        // A game still running on a console: looked at again once it closes there too.
        if !rows.iter().any(|r| r.reason == Some(SyncReason::Running)) {
            watch.closed.remove(&(pair_id, c, what));
        }
    }
}

// ── persistence and the shared state ────────────────────────────────────

struct Shared {
    settings: Settings,
    state: State,
    watch: Watch,
    /// The console pairs with an apply running (sorted hosts).
    applying: BTreeSet<String>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            settings: local::load(SETTINGS_FILE),
            state: local::load(STATE_FILE),
            watch: Watch::default(),
            applying: BTreeSet::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

fn persist_state(state: &State) {
    if let Err(e) = local::save(STATE_FILE, state) {
        crate::log_warn!("save sync: {e}");
    }
}

fn hosts_key(p: &SyncPair) -> String {
    let mut h = [p.a.host.as_str(), p.b.host.as_str()];
    h.sort();
    format!("{}|{}", h[0], h[1])
}

/// Holds a pair of consoles for one apply; released when dropped.
struct Applying(String);

impl Applying {
    fn begin(k: String) -> Option<Self> {
        lock().applying.insert(k.clone()).then_some(Self(k))
    }

    /// [`Self::begin`] for `p`'s consoles, waiting up to `wait` for an after-close look (a few
    /// seconds) to end.
    fn begin_within(p: &SyncPair, wait: Duration) -> Option<Self> {
        let until = std::time::Instant::now() + wait;
        loop {
            if let Some(h) = Self::begin(hosts_key(p)) {
                return Some(h);
            }
            if std::time::Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Applying {
    fn drop(&mut self) {
        lock().applying.remove(&self.0);
    }
}

/// A pair's id: the same two users on the same two consoles always get the same one.
pub fn pair_id(a: &SyncSide, b: &SyncSide) -> String {
    let mut sides = [
        format!("{}#{:08x}", a.host, a.user_id),
        format!("{}#{:08x}", b.host, b.user_id),
    ];
    sides.sort();
    digest(&[(sides.join("~"), 0, 0)])[..12].to_string()
}

fn settle_now(h: &str) -> bool {
    lock().watch.too_soon(h, now_ms())
}

/// Runs a check over the shared state without holding the lock over the console work.
fn checked(pair: &SyncPair, only: &dyn Fn(&str, &[String]) -> bool) -> Vec<SyncRow> {
    let mut state = lock().state.clone();
    let rows = check(&Live, pair, &mut state, now_ms(), only, &settle_now);
    let mut g = lock();
    merge_items(&mut g.state, state, &pair.id, false);
    persist_state(&g.state);
    rows
}

/// Takes `from`'s records of `pair` into `into` (other pairs' records stay as `into` has them).
/// The newer of the two last copies is kept, so a check that ran beside an apply never puts an
/// older one back. `suggestions`: `from`'s suggestions of the pair replace `into`'s.
fn merge_items(into: &mut State, from: State, pair: &str, suggestions: bool) {
    let prefix = format!("{pair}|");
    for (k, mut v) in from.items {
        if !k.starts_with(&prefix) {
            continue;
        }
        if let Some(old) = into.items.get(&k).and_then(|o| o.synced.clone()) {
            if v.synced.as_ref().is_none_or(|n| n.at_ms < old.at_ms) {
                v.synced = Some(old);
            }
        }
        into.items.insert(k, v);
    }
    if suggestions {
        into.suggestions.retain(|s| s.pair != pair);
        into.suggestions
            .extend(from.suggestions.into_iter().filter(|s| s.pair == pair));
    }
}

pub fn tick() {
    let settings = lock().settings.clone();
    if settings.pairs.is_empty() {
        return;
    }
    // Holds every pair's consoles for the look, so no apply runs beside it (with one running
    // already, the look waits for the next tick).
    let keys: BTreeSet<String> = settings.pairs.iter().map(hosts_key).collect();
    let mut holds = Vec::new();
    for k in keys {
        match Applying::begin(k) {
            Some(h) => holds.push(h),
            None => return,
        }
    }
    let (mut state, mut watch) = {
        let mut g = lock();
        (g.state.clone(), std::mem::take(&mut g.watch))
    };
    watch_once(
        &Live,
        &settings,
        &mut state,
        &mut watch,
        now_ms(),
        &local::console_busy,
    );
    let mut g = lock();
    g.watch = watch;
    for p in &settings.pairs {
        merge_items(&mut g.state, state.clone(), &p.id, true);
    }
    persist_state(&g.state);
    drop(g);
    drop(holds);
}

pub fn spawn() {
    crate::ava1_api::spawn_periodic(TICK, tick);
}

// ── HTTP ────────────────────────────────────────────────────────────────

fn find_pair(id: &str) -> Option<SyncPair> {
    lock()
        .settings
        .pairs
        .iter()
        .find(|p| p.id == id.trim())
        .cloned()
}

fn no_pair() -> Response {
    json_error(StatusCode::NOT_FOUND, "no such sync pair")
}

/// `GET /api/save-sync`: the pairs, what is kept of each save, the suggestions and which pairs
/// are copying now. Reads nothing from a console.
pub async fn get_view() -> Response {
    let g = lock();
    Json(json!({
        "pairs": g.settings.pairs,
        "items": g.state.items,
        "suggestions": g.state.suggestions,
        "applying": g.applying,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SyncPairBody {
    pub a: SyncSide,
    pub b: SyncSide,
    /// Saves left out (keys from the status). Left out: as before (none for a new pair).
    #[serde(default)]
    pub excluded: Option<Vec<String>>,
}

/// `PUT /api/save-sync/pair` `{a, b, excluded?}`: adds the pair, or changes which saves it leaves
/// out.
pub async fn put_pair(Json(b): Json<SyncPairBody>) -> Response {
    let a = SyncSide {
        host: b.a.host.trim().to_string(),
        user_id: b.a.user_id,
    };
    let other = SyncSide {
        host: b.b.host.trim().to_string(),
        user_id: b.b.user_id,
    };
    if a.host.is_empty() || other.host.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "pick both consoles");
    }
    if a.host == other.host {
        return json_error(StatusCode::BAD_REQUEST, "pick two different consoles");
    }
    let id = pair_id(&a, &other);
    let mut g = lock();
    let pair = match g.settings.pairs.iter_mut().find(|p| p.id == id) {
        Some(p) => {
            if let Some(ex) = b.excluded {
                p.excluded = ex;
            }
            p.clone()
        }
        None => {
            let p = SyncPair {
                id: id.clone(),
                a,
                b: other,
                excluded: b.excluded.unwrap_or_default(),
            };
            g.settings.pairs.push(p.clone());
            p
        }
    };
    if let Err(e) = local::save(SETTINGS_FILE, &g.settings) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(json!({ "pair": pair })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SyncPairRef {
    pub pair: String,
}

/// `POST /api/save-sync/pair/remove` `{pair}`: stops keeping the pair in step (nothing on a
/// console changes).
pub async fn post_remove_pair(Json(b): Json<SyncPairRef>) -> Response {
    let mut g = lock();
    let id = b.pair.trim().to_string();
    g.settings.pairs.retain(|p| p.id != id);
    let prefix = format!("{id}|");
    g.state.items.retain(|k, _| !k.starts_with(&prefix));
    g.state.suggestions.retain(|s| s.pair != id);
    if let Err(e) = local::save(SETTINGS_FILE, &g.settings) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    persist_state(&g.state);
    Json(json!({ "ok": true })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SyncStatusQuery {
    pub pair: String,
}

/// `GET /api/save-sync/status?pair=`: Check now. Reads both consoles; wakes neither.
pub async fn get_status(Query(q): Query<SyncStatusQuery>) -> Response {
    let Some(pair) = find_pair(&q.pair) else {
        return no_pair();
    };
    let r = tokio::task::spawn_blocking(move || {
        let rows = checked(&pair, &|_, _| true);
        json!({ "pair": pair, "rows": rows, "checked_ms": now_ms() })
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct SyncApplyItem {
    pub key: String,
    pub direction: SyncDirection,
}

#[derive(Debug, Deserialize)]
pub struct SyncApplyBody {
    pub pair: String,
    pub items: Vec<SyncApplyItem>,
}

/// `POST /api/save-sync/apply` `{pair, items: [{key, direction}]}`: copies each, one at a time,
/// through Copy to another console. Answers each one's outcome; one failing does not stop the
/// rest. Refused while another apply runs between the same two consoles.
pub async fn post_apply(Json(b): Json<SyncApplyBody>) -> Response {
    let Some(pair) = find_pair(&b.pair) else {
        return no_pair();
    };
    let r = tokio::task::spawn_blocking(move || {
        let _hold = Applying::begin_within(&pair, Duration::from_secs(30))?;
        let mut out: Vec<Value> = Vec::new();
        for item in &b.items {
            let mut state = lock().state.clone();
            let r = apply(
                &Live,
                &pair,
                &mut state,
                &item.key,
                item.direction,
                now_ms(),
                &settle_now,
            );
            {
                let mut g = lock();
                merge_items(&mut g.state, state, &pair.id, true);
                persist_state(&g.state);
            }
            out.push(match r {
                Ok(()) => json!({ "key": item.key, "ok": true }),
                Err(e) => json!({ "key": item.key, "ok": false, "error": e }),
            });
        }
        Some(json!({ "results": out }))
    })
    .await;
    match r {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => json_error(
            StatusCode::CONFLICT,
            "A sync between these consoles is already running.",
        ),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct SyncDismissBody {
    pub pair: String,
    pub key: String,
}

/// `POST /api/save-sync/dismiss` `{pair, key}`: Not now. The suggestion goes; the next time the
/// game closes with a change it comes back.
pub async fn post_dismiss(Json(b): Json<SyncDismissBody>) -> Response {
    let mut g = lock();
    g.state
        .suggestions
        .retain(|s| !(s.pair == b.pair.trim() && s.key == b.key));
    persist_state(&g.state);
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn save(title: &str, user: u32, kind: &str, size: i64) -> SaveEntry {
        SaveEntry {
            title_id: title.into(),
            user_id: user as i32,
            path: format!("/user/home/{user:08x}/savedata_prospero/{title}"),
            size,
            mtime: 1,
            kind: kind.into(),
        }
    }

    #[derive(Default)]
    struct Console {
        saves: Vec<SaveEntry>,
        running: BTreeSet<String>,
        offline: bool,
        settling: bool,
        /// path → content version: the fingerprint.
        version: BTreeMap<String, u32>,
    }

    #[derive(Default)]
    struct Fake {
        consoles: RefCell<BTreeMap<String, Console>>,
        copies: RefCell<Vec<String>>,
    }

    impl Fake {
        fn with(&self, host: &str, f: impl FnOnce(&mut Console)) {
            f(self
                .consoles
                .borrow_mut()
                .entry(host.to_string())
                .or_default());
        }
        fn write(&self, host: &str, title: &str) {
            self.with(host, |c| {
                let p = c
                    .saves
                    .iter()
                    .find(|s| s.title_id == title)
                    .unwrap()
                    .path
                    .clone();
                *c.version.entry(p).or_default() += 1;
            });
        }
    }

    impl Env for Fake {
        fn saves(&self, host: &str) -> Result<Vec<SaveEntry>, String> {
            let c = self.consoles.borrow();
            let c = c.get(host).ok_or("no console")?;
            if c.offline {
                return Err("offline".into());
            }
            Ok(c.saves.clone())
        }
        fn running(&self, host: &str) -> Result<BTreeSet<String>, String> {
            let c = self.consoles.borrow();
            let c = c.get(host).ok_or("no console")?;
            if c.offline {
                return Err("offline".into());
            }
            Ok(c.running.clone())
        }
        fn settling(&self, host: &str) -> bool {
            self.consoles.borrow().get(host).is_some_and(|c| c.settling)
        }
        fn fingerprint(&self, host: &str, e: &SaveEntry) -> Result<String, String> {
            let c = self.consoles.borrow();
            let v = c[host].version.get(&e.path).copied().unwrap_or(0);
            Ok(format!("{host}:{}:{v}", e.path))
        }
        fn copy(
            &self,
            from: &SyncSide,
            to: &SyncSide,
            src: &SaveEntry,
            dst: Option<&SaveEntry>,
        ) -> Result<(), String> {
            self.copies
                .borrow_mut()
                .push(format!("{} {} -> {}", src.title_id, from.host, to.host));
            let path = dst.map_or(src.path.clone(), |d| d.path.clone());
            // The console rewrites the target: a new fingerprint there.
            self.with(&to.host, |c| *c.version.entry(path).or_default() += 10);
            Ok(())
        }
    }

    fn pair() -> SyncPair {
        let a = SyncSide {
            host: "10.0.0.1".into(),
            user_id: 1,
        };
        let b = SyncSide {
            host: "10.0.0.2".into(),
            user_id: 2,
        };
        SyncPair {
            id: pair_id(&a, &b),
            a,
            b,
            excluded: vec![],
        }
    }

    fn fake(titles_a: &[&str], titles_b: &[&str]) -> Fake {
        let f = Fake::default();
        f.with("10.0.0.1", |c| {
            c.saves = titles_a.iter().map(|t| save(t, 1, "ps5", 100)).collect()
        });
        f.with("10.0.0.2", |c| {
            c.saves = titles_b.iter().map(|t| save(t, 2, "ps5", 200)).collect()
        });
        f
    }

    fn all(_: &str, _: &[String]) -> bool {
        true
    }
    fn never(_: &str) -> bool {
        false
    }

    fn status_of(f: &Fake, p: &SyncPair, st: &mut State, key: &str) -> SyncRow {
        check(f, p, st, 1000, &all, &never)
            .into_iter()
            .find(|r| r.key == key)
            .unwrap()
    }

    #[test]
    fn the_status_follows_the_fingerprints_kept_at_the_last_copy() {
        assert_eq!(decide("x", "y", None), SyncStatus::FirstTime);
        let s = SyncRecord {
            a: "x".into(),
            b: "y".into(),
            at_ms: 1,
        };
        assert_eq!(decide("x", "y", Some(&s)), SyncStatus::InStep);
        assert_eq!(decide("x2", "y", Some(&s)), SyncStatus::ANewer);
        assert_eq!(decide("x", "y2", Some(&s)), SyncStatus::BNewer);
        assert_eq!(decide("x2", "y2", Some(&s)), SyncStatus::BothChanged);
    }

    #[test]
    fn only_the_changed_side_may_be_sent_unless_the_user_picks() {
        use SyncDirection::*;
        assert!(allowed(SyncStatus::ANewer, AToB));
        assert!(!allowed(SyncStatus::ANewer, BToA));
        assert!(allowed(SyncStatus::BNewer, BToA));
        assert!(!allowed(SyncStatus::BNewer, AToB));
        assert!(allowed(SyncStatus::BothChanged, AToB) && allowed(SyncStatus::BothChanged, BToA));
        assert!(allowed(SyncStatus::FirstTime, AToB) && allowed(SyncStatus::FirstTime, BToA));
        assert!(!allowed(SyncStatus::InStep, AToB) && !allowed(SyncStatus::InStep, BToA));
        assert!(!allowed(SyncStatus::Unavailable, AToB));
    }

    #[test]
    fn a_copy_keeps_both_fingerprints_and_a_later_write_shows_who_changed() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let p = pair();
        let mut st = State::default();
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::FirstTime
        );
        apply(
            &f,
            &p,
            &mut st,
            "PPSA01234",
            SyncDirection::AToB,
            1000,
            &never,
        )
        .unwrap();
        assert_eq!(
            f.copies.borrow().as_slice(),
            ["PPSA01234 10.0.0.1 -> 10.0.0.2"]
        );
        let kept = st.items[&item_key(&p.id, "PPSA01234")]
            .synced
            .clone()
            .unwrap();
        assert!(
            kept.b.ends_with(":10"),
            "the target's fingerprint after the copy"
        );
        // Nothing written since: in step (the target's reset time is in its kept fingerprint).
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::InStep
        );
        f.write("10.0.0.2", "PPSA01234");
        let row = status_of(&f, &p, &mut st, "PPSA01234");
        assert_eq!(row.status, SyncStatus::BNewer);
        assert_eq!(row.b.unwrap().changed_ms, Some(1000));
        // The direction no longer allowed by the state is refused.
        assert!(apply(
            &f,
            &p,
            &mut st,
            "PPSA01234",
            SyncDirection::AToB,
            2000,
            &never
        )
        .is_err());
        assert_eq!(f.copies.borrow().len(), 1);
        f.write("10.0.0.1", "PPSA01234");
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::BothChanged
        );
        // A conflict goes whichever way the user picks.
        apply(
            &f,
            &p,
            &mut st,
            "PPSA01234",
            SyncDirection::BToA,
            3000,
            &never,
        )
        .unwrap();
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::InStep
        );
    }

    #[test]
    fn unavailable_for_an_offline_or_settling_console_a_running_game_or_a_ps4_save() {
        let f = fake(&["PPSA01234", "CUSA00001"], &["PPSA01234", "CUSA00001"]);
        f.with("10.0.0.1", |c| {
            c.saves[1].kind = "ps4".into();
        });
        let p = pair();
        let mut st = State::default();
        let ps4 = status_of(&f, &p, &mut st, "CUSA00001");
        assert_eq!(
            (ps4.status, ps4.reason),
            (SyncStatus::Unavailable, Some(SyncReason::Ps4))
        );

        f.with("10.0.0.2", |c| {
            c.running.insert("PPSA01234".into());
        });
        let r = status_of(&f, &p, &mut st, "PPSA01234");
        assert_eq!(
            (r.status, r.reason, r.reason_side),
            (
                SyncStatus::Unavailable,
                Some(SyncReason::Running),
                Some('b')
            )
        );
        assert!(apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).is_err());
        f.with("10.0.0.2", |c| c.running.clear());

        f.with("10.0.0.1", |c| c.settling = true);
        let r = status_of(&f, &p, &mut st, "PPSA01234");
        assert_eq!(
            (r.reason, r.reason_side),
            (Some(SyncReason::Settling), Some('a'))
        );
        f.with("10.0.0.1", |c| c.settling = false);
        // Just back from rest (engine side): waits too.
        let r = check(&f, &p, &mut st, 1, &all, &|h| h == "10.0.0.2")
            .into_iter()
            .find(|r| r.key == "PPSA01234")
            .unwrap();
        assert_eq!(r.reason, Some(SyncReason::Settling));

        f.with("10.0.0.2", |c| c.offline = true);
        let r = status_of(&f, &p, &mut st, "PPSA01234");
        assert_eq!(
            (r.status, r.reason, r.reason_side),
            (
                SyncStatus::Unavailable,
                Some(SyncReason::Offline),
                Some('b')
            )
        );
        assert!(f.copies.borrow().is_empty());
    }

    #[test]
    fn a_save_on_one_side_only_asks_for_one_start_of_the_game_there() {
        let f = fake(&["PPSA01234"], &[]);
        let p = pair();
        let mut st = State::default();
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::OnlyOnA
        );
        let e = apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).unwrap_err();
        assert!(e.contains("Start the game once"), "{e}");
        assert!(f.copies.borrow().is_empty());
        assert!(st.items[&item_key(&p.id, "PPSA01234")].last_error.is_some());
    }

    #[test]
    fn a_save_left_out_is_not_read_nor_copied() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let mut p = pair();
        p.excluded = vec!["PPSA01234".into()];
        let mut st = State::default();
        assert_eq!(
            status_of(&f, &p, &mut st, "PPSA01234").status,
            SyncStatus::Excluded
        );
        assert!(apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).is_err());
    }

    #[test]
    fn a_users_saves_and_the_emulators_are_keyed_alike_on_both_consoles() {
        let mut sw = save("0100000000010000", 0x0123456, "switch", 5);
        sw.path = format!(
            "{}/0123456789abcdef0123456789abcdef/0100000000010000",
            crate::emulator_saves::ROOT
        );
        let saves = vec![
            save("PPSA01234", 1, "ps5", 1),
            save("PPSA01234", 1, "ps4", 1),
            save("PPSA05678", 9, "ps5", 1), // another user's
            sw,
        ];
        let items = items_of(&saves, 1);
        assert_eq!(
            items.keys().cloned().collect::<Vec<_>>(),
            ["PPSA01234", "switch:0100000000010000"]
        );
        assert_eq!(items["PPSA01234"].kind, "ps5");
    }

    #[test]
    fn a_game_closed_with_a_change_is_suggested_and_waits_for_an_off_console() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let p = pair();
        let settings = Settings {
            pairs: vec![p.clone()],
        };
        let mut st = State::default();
        apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).unwrap();
        let mut w = Watch::default();
        let busy = |_: &str| false;
        // Running on A, then closed with a new save while B is off.
        f.with("10.0.0.1", |c| {
            c.running.insert("PPSA01234".into());
        });
        watch_once(&f, &settings, &mut st, &mut w, 10_000, &busy);
        assert!(st.suggestions.is_empty());
        f.with("10.0.0.1", |c| c.running.clear());
        f.write("10.0.0.1", "PPSA01234");
        f.with("10.0.0.2", |c| c.offline = true);
        watch_once(&f, &settings, &mut st, &mut w, 20_000, &busy);
        assert_eq!(st.suggestions.len(), 1);
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Waiting);
        assert_eq!(st.suggestions[0].direction, SyncDirection::AToB);
        // B answers again: nothing in its first 90 s.
        f.with("10.0.0.2", |c| c.offline = false);
        watch_once(&f, &settings, &mut st, &mut w, 30_000, &busy);
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Waiting);
        watch_once(
            &f,
            &settings,
            &mut st,
            &mut w,
            30_000 + AFTER_WAKE_MS,
            &busy,
        );
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Ready);
        // Never applied by itself.
        assert_eq!(f.copies.borrow().len(), 1);
        // Send applies it and clears it.
        apply(
            &f,
            &p,
            &mut st,
            "PPSA01234",
            SyncDirection::AToB,
            200_000,
            &never,
        )
        .unwrap();
        assert!(st.suggestions.is_empty());
    }

    #[test]
    fn a_game_closed_on_both_sides_with_changes_is_a_conflict_and_unchanged_is_nothing() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let p = pair();
        let settings = Settings {
            pairs: vec![p.clone()],
        };
        let mut st = State::default();
        let busy = |_: &str| false;
        let mut w = Watch::default();
        // Never synced: no suggestion (the user picks the first time).
        f.with("10.0.0.1", |c| {
            c.running.insert("PPSA01234".into());
        });
        watch_once(&f, &settings, &mut st, &mut w, 1, &busy);
        f.with("10.0.0.1", |c| c.running.clear());
        f.write("10.0.0.1", "PPSA01234");
        watch_once(&f, &settings, &mut st, &mut w, 2, &busy);
        assert!(st.suggestions.is_empty());

        apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 3, &never).unwrap();
        // Closed without a change: nothing.
        f.with("10.0.0.1", |c| {
            c.running.insert("PPSA01234".into());
        });
        watch_once(&f, &settings, &mut st, &mut w, 4, &busy);
        f.with("10.0.0.1", |c| c.running.clear());
        watch_once(&f, &settings, &mut st, &mut w, 5, &busy);
        assert!(st.suggestions.is_empty());
        // Both changed: a conflict.
        f.with("10.0.0.1", |c| {
            c.running.insert("PPSA01234".into());
        });
        watch_once(&f, &settings, &mut st, &mut w, 6, &busy);
        f.with("10.0.0.1", |c| c.running.clear());
        f.write("10.0.0.1", "PPSA01234");
        f.write("10.0.0.2", "PPSA01234");
        watch_once(&f, &settings, &mut st, &mut w, 7, &busy);
        assert_eq!(st.suggestions.len(), 1);
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Conflict);
    }

    #[test]
    fn a_console_that_stops_answering_closes_no_game() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let p = pair();
        let settings = Settings {
            pairs: vec![p.clone()],
        };
        let mut st = State::default();
        apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).unwrap();
        let mut w = Watch::default();
        f.with("10.0.0.1", |c| {
            c.running.insert("PPSA01234".into());
        });
        watch_once(&f, &settings, &mut st, &mut w, 2, &|_| false);
        f.write("10.0.0.1", "PPSA01234");
        f.with("10.0.0.1", |c| c.offline = true);
        watch_once(&f, &settings, &mut st, &mut w, 3, &|_| false);
        assert!(w.closed.is_empty());
        assert!(st.suggestions.is_empty());
    }

    #[test]
    fn a_game_still_running_on_the_other_console_is_waited_for() {
        let f = fake(&["PPSA01234"], &["PPSA01234"]);
        let p = pair();
        let settings = Settings {
            pairs: vec![p.clone()],
        };
        let mut st = State::default();
        apply(&f, &p, &mut st, "PPSA01234", SyncDirection::AToB, 1, &never).unwrap();
        let mut w = Watch::default();
        let busy = |_: &str| false;
        for host in ["10.0.0.1", "10.0.0.2"] {
            f.with(host, |c| {
                c.running.insert("PPSA01234".into());
            });
        }
        watch_once(&f, &settings, &mut st, &mut w, 2, &busy);
        // Closed on A with a change; still running on B: nothing yet.
        f.with("10.0.0.1", |c| c.running.clear());
        f.write("10.0.0.1", "PPSA01234");
        watch_once(&f, &settings, &mut st, &mut w, 3, &busy);
        assert!(st.suggestions.is_empty());
        // Closed on B without a change: A's is suggested, and B's own close takes nothing away.
        f.with("10.0.0.2", |c| c.running.clear());
        watch_once(&f, &settings, &mut st, &mut w, 4, &busy);
        assert_eq!(st.suggestions.len(), 1);
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Ready);
        assert_eq!(st.suggestions[0].direction, SyncDirection::AToB);
    }

    #[test]
    fn the_same_two_users_get_the_same_pair_whichever_side_comes_first() {
        let a = SyncSide {
            host: "1.1.1.1".into(),
            user_id: 1,
        };
        let b = SyncSide {
            host: "2.2.2.2".into(),
            user_id: 2,
        };
        assert_eq!(pair_id(&a, &b), pair_id(&b, &a));
        assert_eq!(pair_id(&a, &b).len(), 12);
        let c = SyncSide {
            host: "2.2.2.2".into(),
            user_id: 3,
        };
        assert_ne!(pair_id(&a, &b), pair_id(&a, &c));
    }

    #[test]
    fn a_fingerprint_changes_with_any_file() {
        let one = digest(&[("sdimg_a".into(), 10, 5)]);
        assert_eq!(one, digest(&[("sdimg_a".into(), 10, 5)]));
        assert_ne!(one, digest(&[("sdimg_a".into(), 11, 5)]));
        assert_ne!(one, digest(&[("sdimg_a".into(), 10, 6)]));
        assert_ne!(one, digest(&[("sdimg_b".into(), 10, 5)]));
        assert!(settling_phase(1) && settling_phase(2));
        assert!(!settling_phase(0) && !settling_phase(3));
    }
}
