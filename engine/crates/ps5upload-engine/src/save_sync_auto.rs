//! Save sync, kept in step automatically: a game the user turned on for a pair (a user on one PS5
//! and a user on another) has its newer save copied across with no question asked, as long as
//! nothing says otherwise.
//!
//! Which side is newer comes from the same fingerprints the rest of save sync uses
//! ([`crate::save_sync`]): both sides' fingerprints are kept after each copy (the pair's sync
//! base), and a side whose fingerprint differs from that has changed since. So:
//!
//! - **One side changed, the other did not**: copied from the changed side, through the same copy
//!   as Sync selected ([`save_sync::apply`]): the save it replaces is backed up first, and put
//!   back if the copy fails. Only when neither console runs the game.
//! - **Both changed** (or never synced, so nothing tells which is newer): a conflict. Nothing is
//!   copied; both saves are backed up (tagged `syncconflict`) and the user picks which to keep
//!   (`POST /api/save-sync/resolve`), from Save sync or the prompt shown over every screen.
//! - **A console off, resting, settling or busy**: the copy waits and is made once it answers
//!   (and has been up for 90 s). A console is never woken unless the user turned that on for the
//!   pair, and then at most once in half an hour.
//! - **No save on the other side yet** (a PS5 save): never made there by itself. It waits for the
//!   user's one-time first copy (`POST /api/save-sync/first-copy`), or for the game's own first
//!   save there, which is then a pick like any never-synced save.
//! - **Can't sync**: a PS4 save, or a copy the helper there can't make safely (#809: images the
//!   other user lacks and a helper that can't make a save theirs). Said plainly; a PS4 save can't
//!   be turned on, and one that fails three times in a row stops until Sync now.
//!
//! When it runs: a game closing on either console (the after-close look of [`crate::save_sync`]),
//! every minute for a copy waiting on a console, and every 10 minutes for all of them (a game
//! played while this engine was off). In the desktop app while it runs; in the Docker engine
//! around the clock.
//!
//! The decisions are pure ([`plan`]); the console work goes through [`Env`], so all of it is
//! tested without a PS5. What a console's own page shows of it is `save_sync_console`.
//!
//! ## The gaps closed in 12.6.46, and why this way
//!
//! **A user's first save of a game on the other console.** Options weighed: (a) keep refusing and
//! ask the user to start the game there first; (b) copy it there by itself as soon as it is turned
//! on; (c) make it a one-time copy the user asks for, then keep it in step like any other.
//! The mechanism exists and is measured: Saves › Copy to another console already sends a save to
//! a user with none of the game as whole images and makes each theirs (`save_copy::copy_to_console`
//! → `restore_owned`: the helper's `save.owner` writes their account and user id into the save's
//! param.sfo and adds its row to their save database; on FW 13.60 the game then shows Continue,
//! and without that step it starts as new, #809). A same-user restore needs no database change
//! (delete + write the images works in-game), but a brand-new save does: the row is what makes the
//! console list it, which is why the plain copy path never did it. Chosen: (c). It is the one step
//! that adds a save the user did not make there, so it is never automatic: the app shows "Copy to
//! <console> (<user> has no save yet)" with the check made first ([`sync::FirstCopy`]), and keeps
//! (a)'s words where the copy can't be made: a helper too old for `save.owner`, or a user who has
//! never saved any PS5 game there (they have no save database, and making one has never been
//! tried, `save_copy::first_copy_check`). Turned on without the copy, the save waits
//! ([`AutoReason::NoSave`]) and becomes a pick once the game saves there by itself; it is never
//! overwritten without one. Not covered: a save whose param.sfo is in a protected block (never
//! seen on 13.60; the helper's block write is untested on hardware).
//!
//! **PS4 saves.** Options: (a) copy the files as they are (backup on one side, restore on the
//! other); (b) the PS5 path (open decrypted, put the files in the other user's save); (c) leave
//! them out. Nothing in the app moves a PS4 save to another user or PS5: Copy to another console
//! refuses it, `restore_owned` refuses a PS4 image (its owner is more than the account and user in
//! its param.sfo, and its key file `<name>.bin` is the console's), and Move saves from a PS4 is
//! experimental with mixed reports. (b) has never opened a PS4 image here. Chosen: (c), said
//! plainly ([`PS4_WORDS`]): a PS4 save is backed up and restored on the same PS5 and user only.
//!
//! **The console's own page.** The page talks only to its helper and save sync lives in the
//! engine, so the engine writes a small status document onto each console through the mesh's
//! documents and the helper serves it read only (`save_sync_console`). Options weighed: the page
//! asking the engine directly (it has no address for it, and a PS5 browser can't reach a computer
//! that is asleep), a new helper store (more code on the console for the same thing). Chosen: the
//! mesh documents already kept and size-limited on the console, written only when something
//! changed or every half hour.
//!
//! **Testing on one console.** The second user and console can't be made (no create-user on
//! PS5), so the decisions are tested with fake consoles; on hardware one user is paired with
//! itself under two addresses of the same console, and the kept base of one side is changed by
//! hand so the engine sees a one-sided change and runs the real copy end to end.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::save_backup::{json_error, now_ms};
use crate::save_sync::{
    self as sync, apply, check, item_key, items_of, Env, Settings, State, SyncDirection, SyncPair,
    SyncReason, SyncRow, SyncSide, SyncStatus, SyncSuggestion, SyncSuggestionState, Watch,
};

/// Every save kept in step is looked at this often, changed or not.
pub const SWEEP_MS: u64 = 10 * 60_000;
/// A console asleep is woken (when the user allows it) at most this often.
pub const WAKE_EVERY_MS: u64 = 30 * 60_000;
/// Failed copies in a row before a save stops trying until Sync now.
pub const MAX_FAILS: u32 = 3;
/// The tag of the backups taken of both sides when a conflict is found.
pub const CONFLICT_TAG: &str = "syncconflict";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoState {
    InStep,
    /// A copy, or a look, waits for a console (`side`).
    Waiting,
    /// Both changed, or never synced: the user picks.
    Conflict,
    /// This save can't be kept in step (`reason`, `detail`).
    CantSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReason {
    /// The console is off, resting or not answering.
    Offline,
    /// It has just woken, is going to rest, or is busy with a transfer or an install.
    Settling,
    /// The game (or its emulator) runs there.
    Running,
    /// Copied at the next look.
    Queued,
    /// Reading or copying failed; tried again at the next look.
    Error,
    /// A PS4 save: it can't go to another console.
    Ps4,
    /// Waiting: a PS5 save with none of the game for the user on that console yet. It goes there
    /// once the user asks for the first copy (`detail`: why that can't be made, when it can't),
    /// or the game makes its own save there (then the user picks which to keep).
    NoSave,
    /// The copy would not be safe (`detail` says why).
    Refused,
    /// The copy failed [`MAX_FAILS`] times in a row; Sync now tries again.
    Failed,
}

/// Where a save kept in step stands, kept with its sync record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoView {
    pub state: AutoState,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub kind: String,
    /// The console the state is about (`a` or `b`): the one waited for, or the one the reason
    /// names.
    #[serde(default)]
    pub side: Option<char>,
    #[serde(default)]
    pub reason: Option<AutoReason>,
    /// The words of an error or a refusal.
    #[serde(default)]
    pub detail: Option<String>,
    /// Waiting: the way the save goes once it can.
    #[serde(default)]
    pub direction: Option<SyncDirection>,
    /// Conflict: never synced before (rather than changed on both).
    #[serde(default)]
    pub first: bool,
    /// Conflict: the backups taken of both sides when it was found (file names).
    #[serde(default)]
    pub kept: Vec<String>,
    /// When this state began (engine ms).
    pub since_ms: u64,
    #[serde(default)]
    pub fails: u32,
    /// When the console waited for was last woken.
    #[serde(default)]
    pub woke_ms: Option<u64>,
    /// The last copy made while kept in step, and which way it went.
    #[serde(default)]
    pub copied_ms: Option<u64>,
    #[serde(default)]
    pub copied_direction: Option<SyncDirection>,
}

impl AutoView {
    fn new(state: AutoState, row: &SyncRow, prev: Option<&AutoView>, now: u64) -> Self {
        let same = prev.filter(|p| p.state == state);
        AutoView {
            state,
            title_id: row.title_id.clone(),
            kind: row.kind.clone(),
            side: None,
            reason: None,
            detail: None,
            direction: None,
            first: false,
            kept: same.map(|p| p.kept.clone()).unwrap_or_default(),
            since_ms: same.map_or(now, |p| p.since_ms),
            fails: prev.map_or(0, |p| p.fails),
            woke_ms: prev.and_then(|p| p.woke_ms),
            copied_ms: prev.and_then(|p| p.copied_ms),
            copied_direction: prev.and_then(|p| p.copied_direction),
        }
    }

    /// A copy went across (whoever asked for it): in step.
    pub fn copied(&mut self, now: u64, dir: SyncDirection) {
        self.state = AutoState::InStep;
        self.side = None;
        self.reason = None;
        self.detail = None;
        self.direction = None;
        self.first = false;
        self.kept.clear();
        self.since_ms = now;
        self.fails = 0;
        self.copied_ms = Some(now);
        self.copied_direction = Some(dir);
    }
}

/// What to do with a save kept in step, from a fresh check of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    InStep,
    Copy(SyncDirection),
    Conflict {
        first: bool,
    },
    Wait {
        side: char,
        reason: AutoReason,
        direction: Option<SyncDirection>,
    },
    CantSync {
        side: Option<char>,
        reason: AutoReason,
    },
    /// Nothing new can be told (a console did not answer and nothing changed on the other): as
    /// it was.
    Keep,
}

fn other(c: char) -> char {
    if c == 'a' {
        'b'
    } else {
        'a'
    }
}

/// The way a save goes from side `c`.
pub fn from_side(c: char) -> SyncDirection {
    if c == 'a' {
        SyncDirection::AToB
    } else {
        SyncDirection::BToA
    }
}

/// The side a copy in `d` goes to.
pub fn to_side(d: SyncDirection) -> char {
    match d {
        SyncDirection::AToB => 'b',
        SyncDirection::BToA => 'a',
    }
}

/// The decision, pure: what `row` (a check made now) means for a save kept in step that stood at
/// `prev`.
pub fn plan(row: &SyncRow, prev: Option<&AutoView>) -> Plan {
    let in_step_now = row.reason.is_none() && row.status == SyncStatus::InStep;
    // A conflict stays until the user picks (or both are the same as the base again), and a save
    // that failed too often waits for Sync now.
    if let Some(p) = prev {
        let stuck = p.state == AutoState::Conflict
            || (p.state == AutoState::CantSync && p.reason == Some(AutoReason::Failed));
        if stuck && !in_step_now {
            return Plan::Keep;
        }
    }
    let changed = |c: char| {
        if c == 'a' {
            row.a_changed
        } else {
            row.b_changed
        }
    };
    let one_way = match (row.a_changed, row.b_changed) {
        (Some(true), Some(true)) => None,
        (Some(true), _) => Some(SyncDirection::AToB),
        (_, Some(true)) => Some(SyncDirection::BToA),
        _ => None,
    };
    let side = row.reason_side.unwrap_or('a');
    match row.reason {
        Some(SyncReason::Ps4) => Plan::CantSync {
            side: None,
            reason: AutoReason::Ps4,
        },
        Some(SyncReason::Running) => Plan::Wait {
            side,
            reason: AutoReason::Running,
            direction: one_way,
        },
        Some(r @ (SyncReason::Offline | SyncReason::Settling)) => {
            let reason = if r == SyncReason::Offline {
                AutoReason::Offline
            } else {
                AutoReason::Settling
            };
            // The side that answered changed: it goes across once the other can take it.
            if changed(other(side)) == Some(true) {
                return Plan::Wait {
                    side,
                    reason,
                    direction: Some(from_side(other(side))),
                };
            }
            match prev {
                // Still waiting for the same thing. A save waiting for its first save there has no
                // copy to make, so a console away is never woken for it.
                Some(p) if p.state == AutoState::Waiting => Plan::Wait {
                    side,
                    reason,
                    direction: p.direction.filter(|_| p.reason != Some(AutoReason::NoSave)),
                },
                None => Plan::Wait {
                    side,
                    reason,
                    direction: None,
                },
                _ => Plan::Keep,
            }
        }
        Some(SyncReason::Error) => Plan::Wait {
            side,
            reason: AutoReason::Error,
            direction: one_way,
        },
        None => match row.status {
            SyncStatus::InStep => Plan::InStep,
            SyncStatus::ANewer => Plan::Copy(SyncDirection::AToB),
            SyncStatus::BNewer => Plan::Copy(SyncDirection::BToA),
            SyncStatus::BothChanged => Plan::Conflict { first: false },
            SyncStatus::FirstTime => Plan::Conflict { first: true },
            SyncStatus::OnlyOnA | SyncStatus::OnlyOnB => {
                let missing = if row.status == SyncStatus::OnlyOnA {
                    'b'
                } else {
                    'a'
                };
                if row.kind == "ps5" {
                    // Never made there by itself: it waits for the user's one-time first copy,
                    // or for the game's own first save there (then the user picks).
                    Plan::Wait {
                        side: missing,
                        reason: AutoReason::NoSave,
                        direction: None,
                    }
                } else {
                    // An emulator's save goes into the same place there.
                    Plan::Copy(from_side(other(missing)))
                }
            }
            SyncStatus::Unavailable | SyncStatus::Excluded => Plan::Keep,
        },
    }
}

/// The words for a PS4 save, which is never kept in step (see the module docs).
pub const PS4_WORDS: &str = "PS4 saves can't be kept in step: PS5Upload copies a PS4 save only back to the same user on the same PS5 (Restore). Moving one to another user or PS5 isn't supported.";

/// Whether a save may be turned on at all, from a check of it: never a PS4 save. A PS5 save the
/// other user has none of yet may: it waits for the one-time first copy or the game's own first
/// save there. Err: the words for the user.
pub fn can_turn_on(row: &SyncRow) -> Result<(), String> {
    if row.kind == "ps4" || row.reason == Some(SyncReason::Ps4) {
        return Err(PS4_WORDS.into());
    }
    Ok(())
}

fn sides(pair: &SyncPair, dir: SyncDirection) -> (&SyncSide, &SyncSide) {
    match dir {
        SyncDirection::AToB => (&pair.a, &pair.b),
        SyncDirection::BToA => (&pair.b, &pair.a),
    }
}

fn side_of(pair: &SyncPair, c: char) -> &SyncSide {
    if c == 'a' {
        &pair.a
    } else {
        &pair.b
    }
}

/// Asks `env` what a copy of `key` in `dir` would refuse, changing nothing.
fn preflight(env: &dyn Env, pair: &SyncPair, key: &str, dir: SyncDirection) -> Result<(), String> {
    let (from, to) = sides(pair, dir);
    let src_saves = items_of(&env.saves(&from.host)?, from.user_id);
    let dst_saves = items_of(&env.saves(&to.host)?, to.user_id);
    let src = src_saves
        .get(key)
        .ok_or_else(|| "That save is no longer there.".to_string())?;
    env.preflight(from, to, src, dst_saves.get(key))
}

/// Backs up both sides of a conflict; the backups made, and the first error if one failed.
fn keep_both(env: &dyn Env, pair: &SyncPair, key: &str) -> (Vec<String>, Option<String>) {
    let mut kept = Vec::new();
    let mut err = None;
    for side in [&pair.a, &pair.b] {
        let entry: Option<SaveEntry> = env
            .saves(&side.host)
            .ok()
            .and_then(|s| items_of(&s, side.user_id).remove(key));
        let Some(entry) = entry else { continue };
        match env.keep(side, &entry) {
            Ok(name) => kept.push(name),
            Err(e) => {
                crate::log_warn!(
                    "save sync: {key} on {} not backed up for the conflict: {e}",
                    side.host
                );
                err.get_or_insert(e);
            }
        }
    }
    (kept, err)
}

/// Wakes the console a copy waits for, when the pair allows it and it was not woken lately.
fn maybe_wake(env: &dyn Env, pair: &SyncPair, v: &mut AutoView, now: u64) {
    if !pair.wake || v.state != AutoState::Waiting || v.reason != Some(AutoReason::Offline) {
        return;
    }
    let (Some(side), Some(dir)) = (v.side, v.direction) else {
        return;
    };
    if to_side(dir) != side {
        return;
    }
    if v.woke_ms
        .is_some_and(|t| now.saturating_sub(t) < WAKE_EVERY_MS)
    {
        return;
    }
    let host = &side_of(pair, side).host;
    v.woke_ms = Some(now);
    match env.wake(host) {
        Ok(()) => crate::log_info!("save sync: woke {host} to take {}", v.title_id),
        Err(e) => {
            crate::log_warn!("save sync: could not wake {host} for {}: {e}", v.title_id);
            v.detail = Some(format!("Couldn't wake it: {e}"));
        }
    }
}

/// One save kept in step, after a check of it (`row`): copies it, waits, or marks a conflict,
/// and keeps where it stands in `state`. `force` (Sync now): a save that failed too often tries
/// again.
pub fn step(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    row: &SyncRow,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
    force: bool,
) -> AutoView {
    let ik = item_key(&pair.id, &row.key);
    let mut prev = state.items.get(&ik).and_then(|i| i.auto.clone());
    if force {
        if let Some(p) = prev.as_mut() {
            p.fails = 0;
            if p.state == AutoState::CantSync {
                p.state = AutoState::Waiting;
                p.reason = Some(AutoReason::Queued);
            }
        }
    }
    let prev = prev.as_ref();
    let mut next = match plan(row, prev) {
        Plan::Keep => match prev {
            Some(p) => p.clone(),
            None => {
                let mut v = AutoView::new(AutoState::Waiting, row, None, now);
                v.reason = Some(AutoReason::Queued);
                v
            }
        },
        Plan::InStep => {
            let mut v = AutoView::new(AutoState::InStep, row, prev, now);
            v.fails = 0;
            v
        }
        Plan::Wait {
            side,
            reason,
            direction,
        } => {
            let mut v = AutoView::new(AutoState::Waiting, row, prev, now);
            v.side = Some(side);
            v.reason = Some(reason);
            v.direction = direction;
            if reason == AutoReason::Error {
                v.detail = row.last_error.clone();
            }
            if reason == AutoReason::NoSave {
                v.detail = row.first_copy.as_ref().and_then(|f| f.why.clone());
            }
            maybe_wake(env, pair, &mut v, now);
            v
        }
        Plan::CantSync { side, reason } => {
            let mut v = AutoView::new(AutoState::CantSync, row, prev, now);
            v.side = side;
            v.reason = Some(reason);
            v
        }
        Plan::Conflict { first } => {
            let mut v = AutoView::new(AutoState::Conflict, row, prev, now);
            v.first = first;
            let fresh = prev.is_none_or(|p| p.state != AutoState::Conflict);
            if fresh {
                // Both kept before anyone picks: whichever is chosen, the other stays in the
                // backups (the copy that follows backs up the one it replaces too).
                if !first {
                    let (kept, err) = keep_both(env, pair, &row.key);
                    v.kept = kept;
                    v.detail = err.map(|e| format!("Couldn't back up both saves: {e}"));
                }
                crate::log_info!(
                    "save sync: {} changed on both {} and {} ({}): the user picks",
                    row.title_id,
                    pair.a.host,
                    pair.b.host,
                    if first { "never synced" } else { "conflict" }
                );
                // The prompt over every screen asks which to keep (the first sync is picked where
                // it was turned on).
                if !first {
                    sync::put_suggestion(
                        state,
                        SyncSuggestion {
                            pair: pair.id.clone(),
                            key: row.key.clone(),
                            title_id: row.title_id.clone(),
                            kind: row.kind.clone(),
                            direction: SyncDirection::AToB,
                            state: SyncSuggestionState::Conflict,
                            at_ms: now,
                        },
                    );
                }
            }
            v
        }
        Plan::Copy(dir) => match preflight(env, pair, &row.key, dir) {
            Err(e) => {
                let mut v = AutoView::new(AutoState::CantSync, row, prev, now);
                v.side = Some(to_side(dir));
                v.reason = Some(AutoReason::Refused);
                v.detail = Some(e);
                v
            }
            Ok(()) => match apply(env, pair, state, &row.key, dir, now, settle_from) {
                Ok(()) => {
                    let mut v = AutoView::new(AutoState::InStep, row, prev, now);
                    // `apply` logged the copy.
                    v.copied(now, dir);
                    v
                }
                Err(e) => {
                    let fails = prev.map_or(0, |p| p.fails) + 1;
                    let state = if fails >= MAX_FAILS {
                        AutoState::CantSync
                    } else {
                        AutoState::Waiting
                    };
                    let mut v = AutoView::new(state, row, prev, now);
                    v.fails = fails;
                    v.side = Some(to_side(dir));
                    v.direction = Some(dir);
                    v.reason = Some(if fails >= MAX_FAILS {
                        AutoReason::Failed
                    } else {
                        AutoReason::Error
                    });
                    v.detail = Some(e);
                    v
                }
            },
        },
    };
    if next.title_id.is_empty() {
        next.title_id = row.title_id.clone();
    }
    // A conflict that is over (picked elsewhere, or the same again) takes its prompt with it.
    if next.state != AutoState::Conflict {
        state.suggestions.retain(|s| {
            !(s.pair == pair.id && s.key == row.key && s.state == SyncSuggestionState::Conflict)
        });
    }
    let moved = prev.is_none_or(|p| p.state != next.state || p.reason != next.reason);
    if moved {
        crate::log_info!(
            "save sync: {} ({}) now {:?}{}",
            row.title_id,
            pair.id,
            next.state,
            next.reason.map(|r| format!(" ({r:?})")).unwrap_or_default()
        );
    }
    state.items.entry(ik).or_default().auto = Some(next.clone());
    next
}

/// Whether the save of `key` is due a look in this pass.
fn due(v: Option<&AutoView>, sweep: bool, answered: &dyn Fn(char) -> bool) -> bool {
    match v {
        None => true,
        Some(v) if v.state == AutoState::Waiting => match (v.reason, v.side) {
            // Looked at once the console waited for answers again.
            (Some(AutoReason::Offline), Some(side)) => answered(side),
            // Waiting for a first save there: the game closing there is looked at anyway (the
            // after-close look), the rest with the sweep.
            (Some(AutoReason::NoSave), _) => sweep,
            _ => true,
        },
        Some(_) => sweep,
    }
}

/// The look over saves kept in step, after the after-close look in [`sync::watch_once`]: the ones
/// waiting, those never looked at, and every [`SWEEP_MS`] all of them. `handled`: looked at
/// already in this pass (a game closed).
pub fn pass(
    env: &dyn Env,
    settings: &Settings,
    state: &mut State,
    watch: &mut Watch,
    now: u64,
    busy: &dyn Fn(&str) -> bool,
    handled: &BTreeSet<(String, String)>,
) {
    for pair in &settings.pairs {
        if pair.auto.is_empty() {
            continue;
        }
        let up = |c: char| watch.answered.get(&side_of(pair, c).host) == Some(&true);
        // A console asleep with a copy waiting for it: woken, if the pair allows it.
        for key in &pair.auto {
            let ik = item_key(&pair.id, key);
            if let Some(v) = state.items.get_mut(&ik).and_then(|i| i.auto.as_mut()) {
                if v.side.is_some_and(|s| !up(s)) {
                    maybe_wake(env, pair, v, now);
                }
            }
        }
        if !up('a') && !up('b') {
            continue;
        }
        let sweep = watch
            .last_sweep
            .get(&pair.id)
            .is_none_or(|t| now.saturating_sub(*t) >= SWEEP_MS);
        let keys: BTreeSet<String> = pair
            .auto
            .iter()
            .filter(|k| !handled.contains(&(pair.id.clone(), (*k).clone())))
            .filter(|k| {
                let v = state
                    .items
                    .get(&item_key(&pair.id, k))
                    .and_then(|i| i.auto.as_ref());
                due(v, sweep, &up)
            })
            .cloned()
            .collect();
        if sweep {
            watch.last_sweep.insert(pair.id.clone(), now);
        }
        if keys.is_empty() {
            continue;
        }
        let settle = |h: &str| watch.too_soon(h, now) || busy(h);
        let rows = check(env, pair, state, now, &|k, _| keys.contains(k), &settle);
        for row in &rows {
            step(env, pair, state, row, now, &settle, false);
        }
    }
}

// ── what the app and the MCP tool read ──────────────────────────────────

/// One save kept in step, for `GET /api/save-sync/games`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GameView {
    pub pair: String,
    pub key: String,
    pub a: SyncSide,
    pub b: SyncSide,
    /// The last copy between the two (any copy, kept in step or picked by hand).
    pub synced_ms: Option<u64>,
    #[serde(flatten)]
    pub view: AutoView,
}

/// Every save kept in step, from what is kept (reads no console). One never looked at yet shows
/// as waiting for the next look.
pub fn games(settings: &Settings, state: &State) -> Vec<GameView> {
    let mut out = Vec::new();
    for p in &settings.pairs {
        for key in &p.auto {
            let item = state.items.get(&item_key(&p.id, key));
            let view = item
                .and_then(|i| i.auto.clone())
                .unwrap_or_else(|| AutoView {
                    state: AutoState::Waiting,
                    title_id: sync::title_of_key(key).to_string(),
                    kind: String::new(),
                    side: None,
                    reason: Some(AutoReason::Queued),
                    detail: None,
                    direction: None,
                    first: false,
                    kept: Vec::new(),
                    since_ms: 0,
                    fails: 0,
                    woke_ms: None,
                    copied_ms: None,
                    copied_direction: None,
                });
            out.push(GameView {
                pair: p.id.clone(),
                key: key.clone(),
                a: p.a.clone(),
                b: p.b.clone(),
                synced_ms: item.and_then(|i| i.synced.as_ref()).map(|s| s.at_ms),
                view,
            });
        }
    }
    out
}

// ── HTTP ────────────────────────────────────────────────────────────────

/// `GET /api/save-sync/games`: every save kept in step and where it stands (in step, waiting
/// for a console, a conflict to pick, or can't sync and why). Reads no console: cheap enough for
/// the phone app to ask often.
pub async fn get_games() -> Response {
    let g = sync::lock();
    Json(json!({
        "games": games(&g.settings, &g.state),
        "applying": g.applying,
        "last_sweep": g.watch.last_sweep,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct AutoBody {
    pub pair: String,
    pub key: String,
    pub on: bool,
}

fn busy_now() -> Response {
    json_error(
        StatusCode::CONFLICT,
        "Save sync is busy with these consoles right now. Try again in a moment.",
    )
}

/// Runs `f` over a copy of the shared state with the pair's consoles held, then keeps what it
/// changed. None: the consoles stayed busy.
fn with_pair<T>(pair: &SyncPair, f: impl FnOnce(&mut State) -> T) -> Option<T> {
    let _hold = sync::Applying::begin_within(pair, Duration::from_secs(30))?;
    let mut state = sync::lock().state.clone();
    let base = state.suggestions.clone();
    let out = f(&mut state);
    let mut g = sync::lock();
    sync::merge_shared(&mut g, state, &base, &pair.id, true);
    sync::persist_state(&g.state);
    Some(out)
}

/// `PUT /api/save-sync/auto` `{pair, key, on}`: keep one save in step automatically, or stop.
/// Turning it on checks the save first (both consoles read, nothing copied): a PS4 save, a PS5
/// save the other user has none of yet, or a copy the helper can't make safely is refused with
/// the reason. A save that is newer on one side is sent at the next look; one never synced, or
/// changed on both, asks which to keep.
pub async fn put_auto(Json(b): Json<AutoBody>) -> Response {
    let Some(pair) = sync::find_pair(&b.pair) else {
        return sync::no_pair();
    };
    let key = b.key.trim().to_string();
    if key.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "which save?");
    }
    if !b.on {
        let mut g = sync::lock();
        if let Some(p) = g.settings.pairs.iter_mut().find(|p| p.id == pair.id) {
            p.auto.retain(|k| k != &key);
        }
        if let Some(i) = g.state.items.get_mut(&item_key(&pair.id, &key)) {
            i.auto = None;
        }
        if let Err(e) = crate::save_backup::save(sync::SETTINGS_FILE, &g.settings) {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
        sync::persist_state(&g.state);
        crate::log_info!("save sync: {key} ({}) no longer kept in step", pair.id);
        return Json(json!({ "ok": true, "auto": Value::Null })).into_response();
    }
    let r = tokio::task::spawn_blocking(move || {
        with_pair(&pair, |state| {
            let view = turn_on(&sync::Live, &pair, state, &key, now_ms(), &sync::settle_now)?;
            keep_turned_on(&pair.id, &key)?;
            Ok::<_, String>(view)
        })
    })
    .await;
    match r {
        Ok(Some(Ok(view))) => Json(json!({ "ok": true, "auto": view })).into_response(),
        Ok(Some(Err(e))) => json_error(StatusCode::CONFLICT, e),
        Ok(None) => busy_now(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The pair's settings keep `key` in step (and no longer leave it out).
fn keep_turned_on(pair: &str, key: &str) -> Result<(), String> {
    let mut g = sync::lock();
    let Some(p) = g.settings.pairs.iter_mut().find(|p| p.id == pair) else {
        return Err("That sync pair was removed.".into());
    };
    if !p.auto.iter().any(|k| k == key) {
        p.auto.push(key.to_string());
    }
    p.excluded.retain(|k| k != key);
    crate::save_backup::save(sync::SETTINGS_FILE, &g.settings).map_err(|e| e.to_string())
}

/// Turning one save on (see [`put_auto`]): checked, and where it stands kept in `state` (the
/// caller keeps it in the pair's settings). Nothing is copied here; a copy due goes at the next
/// look.
pub fn turn_on(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    key: &str,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
) -> Result<AutoView, String> {
    let rows = check(env, pair, state, now, &|k, _| k == key, settle_from);
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| "That save is not on either console.".to_string())?;
    can_turn_on(&row)?;
    // A copy has to be safe both ways, so each side is asked where both have the save.
    if row.reason.is_none() && row.a.is_some() && row.b.is_some() {
        for dir in [SyncDirection::AToB, SyncDirection::BToA] {
            preflight(env, pair, key, dir)?;
        }
    }
    let mut view = match plan(&row, None) {
        Plan::Copy(dir) => {
            let mut v = AutoView::new(AutoState::Waiting, &row, None, now);
            v.reason = Some(AutoReason::Queued);
            v.side = Some(to_side(dir));
            v.direction = Some(dir);
            v
        }
        Plan::Conflict { first } => {
            let mut v = AutoView::new(AutoState::Conflict, &row, None, now);
            v.first = first;
            v
        }
        Plan::InStep => AutoView::new(AutoState::InStep, &row, None, now),
        Plan::Wait {
            side,
            reason,
            direction,
        } => {
            let mut v = AutoView::new(AutoState::Waiting, &row, None, now);
            v.side = Some(side);
            v.reason = Some(reason);
            v.direction = direction;
            if reason == AutoReason::NoSave {
                v.detail = row.first_copy.as_ref().and_then(|f| f.why.clone());
            }
            v
        }
        Plan::CantSync { side, reason } => {
            let mut v = AutoView::new(AutoState::CantSync, &row, None, now);
            v.side = side;
            v.reason = Some(reason);
            v
        }
        Plan::Keep => {
            let mut v = AutoView::new(AutoState::Waiting, &row, None, now);
            v.reason = Some(AutoReason::Queued);
            v
        }
    };
    if view.title_id.is_empty() {
        view.title_id = sync::title_of_key(key).to_string();
    }
    state.items.entry(item_key(&pair.id, key)).or_default().auto = Some(view.clone());
    crate::log_info!(
        "save sync: {} ({}) kept in step from now on: {:?}",
        view.title_id,
        pair.id,
        view.state
    );
    Ok(view)
}

#[derive(Debug, Deserialize)]
pub struct KeyBody {
    pub pair: String,
    pub key: String,
}

/// `POST /api/save-sync/sync-now` `{pair, key}`: looks at one save kept in step now and does what
/// the background look would: copies the newer one, or says what it waits for, or that both
/// changed. A save that stopped after failing tries again. Never overwrites a conflict.
pub async fn post_sync_now(Json(b): Json<KeyBody>) -> Response {
    let Some(pair) = sync::find_pair(&b.pair) else {
        return sync::no_pair();
    };
    let key = b.key.trim().to_string();
    if !pair.auto.contains(&key) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Turn on keeping this save in step first.",
        );
    }
    let r = tokio::task::spawn_blocking(move || {
        with_pair(&pair, |state| {
            sync_now(&sync::Live, &pair, state, &key, now_ms(), &sync::settle_now)
        })
    })
    .await;
    match r {
        Ok(Some(Some(view))) => Json(json!({ "ok": true, "auto": view })).into_response(),
        Ok(Some(None)) => json_error(
            StatusCode::NOT_FOUND,
            "That save is not on either console any more.",
        ),
        Ok(None) => busy_now(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Sync now (see [`post_sync_now`]). None: the save is on neither console.
pub fn sync_now(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    key: &str,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
) -> Option<AutoView> {
    let rows = check(env, pair, state, now, &|k, _| k == key, settle_from);
    let row = rows.into_iter().next()?;
    Some(step(env, pair, state, &row, now, settle_from, true))
}

#[derive(Debug, Deserialize)]
pub struct ResolveBody {
    pub pair: String,
    pub key: String,
    /// The side whose save is kept: `a` or `b` (as in the pair). It is copied over the other,
    /// which is backed up first.
    pub keep: String,
}

/// `POST /api/save-sync/resolve` `{pair, key, keep: "a"|"b"}`: the user's pick in a conflict (or
/// the first sync of a save): the kept side is copied over the other, the other backed up first.
/// Refused when the save is no longer a conflict (it changed since: check again).
pub async fn post_resolve(Json(b): Json<ResolveBody>) -> Response {
    let Some(pair) = sync::find_pair(&b.pair) else {
        return sync::no_pair();
    };
    let dir = match b.keep.trim() {
        "a" => SyncDirection::AToB,
        "b" => SyncDirection::BToA,
        _ => return json_error(StatusCode::BAD_REQUEST, "keep is \"a\" or \"b\""),
    };
    let key = b.key.trim().to_string();
    let r = tokio::task::spawn_blocking(move || {
        with_pair(&pair, |state| {
            resolve(
                &sync::Live,
                &pair,
                state,
                &key,
                dir,
                now_ms(),
                &sync::settle_now,
            )
        })
    })
    .await;
    match r {
        Ok(Some(Ok(view))) => Json(json!({ "ok": true, "auto": view })).into_response(),
        Ok(Some(Err(e))) => json_error(StatusCode::CONFLICT, e),
        Ok(None) => busy_now(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The pick (see [`post_resolve`]): the save as it stands after, if it is kept in step.
pub fn resolve(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    key: &str,
    dir: SyncDirection,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
) -> Result<Option<AutoView>, String> {
    apply(env, pair, state, key, dir, now, settle_from)?;
    let (from, to) = sides(pair, dir);
    crate::log_info!(
        "save sync: {key} picked from {} over {} ({})",
        from.host,
        to.host,
        pair.id
    );
    Ok(state
        .items
        .get(&item_key(&pair.id, key))
        .and_then(|i| i.auto.clone()))
}

#[derive(Debug, Deserialize)]
pub struct FirstCopyBody {
    pub pair: String,
    pub key: String,
    /// Keep the save in step from then on (the switch turned on with the copy).
    #[serde(default)]
    pub keep_in_step: bool,
}

/// `POST /api/save-sync/first-copy` `{pair, key, keep_in_step?}`: the one-time first copy of a PS5
/// save that only one side has, to the other user, made theirs there (see
/// [`sync::apply_first`]). Refused with the reason when the console there can't make it theirs
/// or the save is on both sides by now. After it the two are in step, and with `keep_in_step`
/// kept so.
pub async fn post_first_copy(Json(b): Json<FirstCopyBody>) -> Response {
    let Some(pair) = sync::find_pair(&b.pair) else {
        return sync::no_pair();
    };
    let key = b.key.trim().to_string();
    if key.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "which save?");
    }
    let keep = b.keep_in_step;
    let r = tokio::task::spawn_blocking(move || {
        with_pair(&pair, |state| {
            let view = first_copy(
                &sync::Live,
                &pair,
                state,
                &key,
                keep,
                now_ms(),
                &sync::settle_now,
            )?;
            if keep {
                keep_turned_on(&pair.id, &key)?;
            }
            Ok::<_, String>(view)
        })
    })
    .await;
    match r {
        Ok(Some(Ok(view))) => Json(json!({ "ok": true, "auto": view })).into_response(),
        Ok(Some(Err(e))) => json_error(StatusCode::CONFLICT, e),
        Ok(None) => busy_now(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The first copy (see [`post_first_copy`]): from the side that has the save. The save as it
/// stands after, if it is kept in step (or `keep`: from now on).
pub fn first_copy(
    env: &dyn Env,
    pair: &SyncPair,
    state: &mut State,
    key: &str,
    keep: bool,
    now: u64,
    settle_from: &dyn Fn(&str) -> bool,
) -> Result<Option<AutoView>, String> {
    let rows = check(env, pair, state, now, &|k, _| k == key, settle_from);
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| "That save is not on either console.".to_string())?;
    let dir = match row.status {
        SyncStatus::OnlyOnA => SyncDirection::AToB,
        SyncStatus::OnlyOnB => SyncDirection::BToA,
        _ if row.reason.is_some() => {
            // Offline, running, a PS4 save: apply says it in its words.
            SyncDirection::AToB
        }
        _ => {
            return Err(
                "Both users have a save of this game now: use Sync instead of a first copy.".into(),
            )
        }
    };
    if keep {
        // Turned on with the copy: the copy that follows marks it in step.
        let slot = state.items.entry(item_key(&pair.id, key)).or_default();
        if slot.auto.is_none() {
            let mut v = AutoView::new(AutoState::Waiting, &row, None, now);
            v.reason = Some(AutoReason::Queued);
            v.direction = Some(dir);
            v.side = Some(to_side(dir));
            slot.auto = Some(v);
        }
    }
    let r = sync::apply_first(env, pair, state, key, dir, now, settle_from);
    if r.is_err() && keep {
        // Not turned on by a copy that did not happen (unless it was on before).
        if !pair.auto.iter().any(|k| k == key) {
            if let Some(i) = state.items.get_mut(&item_key(&pair.id, key)) {
                i.auto = None;
            }
        }
    }
    r?;
    let (from, to) = sides(pair, dir);
    crate::log_info!(
        "save sync: {key} copied for the first time from {} to {} (user {:08x}, who had none) ({})",
        from.host,
        to.host,
        to.user_id,
        pair.id
    );
    Ok(state
        .items
        .get(&item_key(&pair.id, key))
        .and_then(|i| i.auto.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save_sync::tests::{fake, never, pair, save, Fake};
    use crate::save_sync::AFTER_WAKE_MS;
    use SyncDirection::{AToB, BToA};

    const K: &str = "PPSA01234";
    const A: &str = "10.0.0.1";
    const B: &str = "10.0.0.2";

    fn row(status: SyncStatus) -> SyncRow {
        SyncRow {
            key: K.into(),
            title_id: K.into(),
            kind: "ps5".into(),
            status,
            reason: None,
            reason_side: None,
            a: None,
            b: None,
            synced_ms: None,
            last_error: None,
            auto: None,
            first_copy: None,
            a_changed: None,
            b_changed: None,
            apps: vec![K.into()],
        }
    }

    fn blocked(reason: SyncReason, side: char, a: Option<bool>, b: Option<bool>) -> SyncRow {
        SyncRow {
            reason: Some(reason),
            reason_side: Some(side),
            a_changed: a,
            b_changed: b,
            ..row(SyncStatus::Unavailable)
        }
    }

    fn view(state: AutoState) -> AutoView {
        AutoView::new(state, &row(SyncStatus::InStep), None, 1)
    }

    #[test]
    fn the_newer_side_is_copied_and_both_changed_is_a_conflict() {
        assert_eq!(plan(&row(SyncStatus::InStep), None), Plan::InStep);
        assert_eq!(plan(&row(SyncStatus::ANewer), None), Plan::Copy(AToB));
        assert_eq!(plan(&row(SyncStatus::BNewer), None), Plan::Copy(BToA));
        assert_eq!(
            plan(&row(SyncStatus::BothChanged), None),
            Plan::Conflict { first: false }
        );
        // Never synced: nothing says which is newer, so the user picks.
        assert_eq!(
            plan(&row(SyncStatus::FirstTime), None),
            Plan::Conflict { first: true }
        );
    }

    #[test]
    fn a_game_running_on_either_console_waits() {
        // Running on B, changed on A: waits for B, then goes A to B.
        assert_eq!(
            plan(
                &blocked(SyncReason::Running, 'b', Some(true), Some(false)),
                None
            ),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::Running,
                direction: Some(AToB)
            }
        );
        // Changed on both while running: no way yet.
        assert_eq!(
            plan(
                &blocked(SyncReason::Running, 'a', Some(true), Some(true)),
                None
            ),
            Plan::Wait {
                side: 'a',
                reason: AutoReason::Running,
                direction: None
            }
        );
    }

    #[test]
    fn a_change_waits_for_a_console_that_is_away() {
        let away = blocked(SyncReason::Offline, 'b', Some(true), None);
        assert_eq!(
            plan(&away, Some(&view(AutoState::InStep))),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::Offline,
                direction: Some(AToB)
            }
        );
        // Nothing changed on the side that answered: as it was.
        let quiet = blocked(SyncReason::Offline, 'b', Some(false), None);
        assert_eq!(plan(&quiet, Some(&view(AutoState::InStep))), Plan::Keep);
        // A copy already waiting keeps its way.
        let mut waiting = view(AutoState::Waiting);
        waiting.direction = Some(BToA);
        assert_eq!(
            plan(&quiet, Some(&waiting)),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::Offline,
                direction: Some(BToA)
            }
        );
        // Just woken: waits too, as settling.
        assert_eq!(
            plan(
                &blocked(SyncReason::Settling, 'b', Some(true), Some(false)),
                None
            ),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::Settling,
                direction: Some(AToB)
            }
        );
    }

    #[test]
    fn a_conflict_stays_until_picked_and_a_failed_save_until_sync_now() {
        let conflict = view(AutoState::Conflict);
        assert_eq!(plan(&row(SyncStatus::ANewer), Some(&conflict)), Plan::Keep);
        assert_eq!(
            plan(&row(SyncStatus::BothChanged), Some(&conflict)),
            Plan::Keep
        );
        assert_eq!(
            plan(&row(SyncStatus::InStep), Some(&conflict)),
            Plan::InStep
        );
        let mut failed = view(AutoState::CantSync);
        failed.reason = Some(AutoReason::Failed);
        assert_eq!(plan(&row(SyncStatus::ANewer), Some(&failed)), Plan::Keep);
    }

    #[test]
    fn what_cant_be_moved_safely_is_said_and_never_turned_on() {
        let ps4 = SyncRow {
            kind: "ps4".into(),
            reason: Some(SyncReason::Ps4),
            ..row(SyncStatus::Unavailable)
        };
        assert_eq!(
            plan(&ps4, None),
            Plan::CantSync {
                side: None,
                reason: AutoReason::Ps4
            }
        );
        assert!(can_turn_on(&ps4).unwrap_err().contains("PS4"));
        // A PS5 save the other user has none of: never made there by itself. It waits for the
        // user's first copy, or the game's own first save there.
        assert_eq!(
            plan(&row(SyncStatus::OnlyOnA), None),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::NoSave,
                direction: None
            }
        );
        // The other console going to rest meanwhile is never woken for it: nothing to send yet.
        let mut no_save = view(AutoState::Waiting);
        no_save.reason = Some(AutoReason::NoSave);
        no_save.direction = Some(AToB);
        assert_eq!(
            plan(
                &blocked(SyncReason::Offline, 'b', None, None),
                Some(&no_save)
            ),
            Plan::Wait {
                side: 'b',
                reason: AutoReason::Offline,
                direction: None
            }
        );
        assert!(can_turn_on(&row(SyncStatus::OnlyOnA)).is_ok());
        assert!(can_turn_on(&row(SyncStatus::OnlyOnB)).is_ok());
        // An emulator's save goes into the same place there.
        let emu = SyncRow {
            kind: "ps2".into(),
            ..row(SyncStatus::OnlyOnB)
        };
        assert_eq!(plan(&emu, None), Plan::Copy(BToA));
        assert!(can_turn_on(&emu).is_ok());
        assert!(can_turn_on(&row(SyncStatus::BothChanged)).is_ok());
    }

    // ── with fake consoles ───────────────────────────────────────────────

    /// A pair with `K` kept in step, synced once A to B (the base).
    fn setup() -> (Fake, SyncPair, Settings, State, Watch) {
        let f = fake(&[K], &[K]);
        let mut p = pair();
        p.auto = vec![K.into()];
        let mut st = State::default();
        apply(&f, &p, &mut st, K, AToB, 1, &never).unwrap();
        let settings = Settings {
            pairs: vec![p.clone()],
        };
        (f, p, settings, st, Watch::default())
    }

    fn look(f: &Fake, s: &Settings, st: &mut State, w: &mut Watch, now: u64) {
        crate::save_sync::watch_once(f, s, st, w, now, &|_| false);
    }

    fn auto_of(st: &State, p: &SyncPair) -> AutoView {
        st.items[&item_key(&p.id, K)].auto.clone().unwrap()
    }

    /// The game started on `host` (seen running) and closed with a new save.
    fn play(f: &Fake, host: &str, s: &Settings, st: &mut State, w: &mut Watch, now: u64) {
        f.with(host, |c| {
            c.running.insert(K.into());
        });
        look(f, s, st, w, now);
        f.with(host, |c| c.running.clear());
        f.write(host, K);
    }

    #[test]
    fn a_game_closed_on_one_console_is_copied_to_the_other_by_itself() {
        let (f, p, s, mut st, mut w) = setup();
        play(&f, A, &s, &mut st, &mut w, 10);
        look(&f, &s, &mut st, &mut w, 20);
        assert_eq!(f.copies.borrow().len(), 2, "the base, then the close");
        assert_eq!(f.copies.borrow()[1], format!("{K} {A} -> {B}"));
        let v = auto_of(&st, &p);
        assert_eq!(v.state, AutoState::InStep);
        assert_eq!(v.copied_direction, Some(AToB));
        assert_eq!(v.copied_ms, Some(20));
        assert!(st.suggestions.is_empty(), "kept in step: no question asked");
        // The base moved with the copy: a look after finds both in step, copies nothing.
        look(&f, &s, &mut st, &mut w, 20 + SWEEP_MS);
        assert_eq!(f.copies.borrow().len(), 2);
        // Played on B next: goes back to A.
        play(&f, B, &s, &mut st, &mut w, 30 + SWEEP_MS);
        look(&f, &s, &mut st, &mut w, 40 + SWEEP_MS);
        assert_eq!(f.copies.borrow()[2], format!("{K} {B} -> {A}"));
        assert_eq!(auto_of(&st, &p).copied_direction, Some(BToA));
    }

    #[test]
    fn a_copy_for_a_console_away_waits_and_goes_once_it_has_been_back_a_while() {
        let (f, p, s, mut st, mut w) = setup();
        play(&f, A, &s, &mut st, &mut w, 10);
        f.with(B, |c| c.offline = true);
        look(&f, &s, &mut st, &mut w, 30);
        let v = auto_of(&st, &p);
        assert_eq!(v.state, AutoState::Waiting);
        assert_eq!(
            (v.side, v.reason, v.direction),
            (Some('b'), Some(AutoReason::Offline), Some(AToB))
        );
        // Never woken: the pair does not allow it.
        assert!(f.wakes.borrow().is_empty());
        look(&f, &s, &mut st, &mut w, 40);
        assert_eq!(f.copies.borrow().len(), 1);
        // Back, but only just: still waiting.
        f.with(B, |c| c.offline = false);
        look(&f, &s, &mut st, &mut w, 50);
        assert_eq!(auto_of(&st, &p).reason, Some(AutoReason::Settling));
        assert_eq!(f.copies.borrow().len(), 1);
        look(&f, &s, &mut st, &mut w, 50 + AFTER_WAKE_MS);
        assert_eq!(f.copies.borrow().len(), 2);
        assert_eq!(auto_of(&st, &p).state, AutoState::InStep);
    }

    #[test]
    fn a_console_asleep_is_woken_only_when_the_pair_allows_it_and_not_often() {
        let (f, mut p, _, mut st, mut w) = setup();
        p.wake = true;
        let s = Settings {
            pairs: vec![p.clone()],
        };
        play(&f, A, &s, &mut st, &mut w, 20);
        f.with(B, |c| c.offline = true);
        look(&f, &s, &mut st, &mut w, 30);
        assert_eq!(*f.wakes.borrow(), vec![B.to_string()]);
        look(&f, &s, &mut st, &mut w, 90);
        look(&f, &s, &mut st, &mut w, 30 + WAKE_EVERY_MS - 1);
        assert_eq!(f.wakes.borrow().len(), 1, "once in half an hour");
        look(&f, &s, &mut st, &mut w, 30 + WAKE_EVERY_MS);
        assert_eq!(f.wakes.borrow().len(), 2);
        // Nothing to send to it: never woken.
        let (f, mut p, _, mut st, mut w) = setup();
        p.wake = true;
        let s = Settings { pairs: vec![p] };
        look(&f, &s, &mut st, &mut w, 10);
        f.with(B, |c| c.offline = true);
        look(&f, &s, &mut st, &mut w, 10 + SWEEP_MS);
        assert!(f.wakes.borrow().is_empty());
    }

    #[test]
    fn changed_on_both_keeps_both_and_asks_and_never_overwrites() {
        let (f, p, s, mut st, mut w) = setup();
        play(&f, A, &s, &mut st, &mut w, 10);
        f.write(B, K);
        look(&f, &s, &mut st, &mut w, 20);
        let v = auto_of(&st, &p);
        assert_eq!(v.state, AutoState::Conflict);
        assert!(!v.first);
        assert_eq!(
            *f.kept.borrow(),
            vec![format!("{K} {A}"), format!("{K} {B}")]
        );
        assert_eq!(v.kept.len(), 2);
        assert_eq!(st.suggestions.len(), 1);
        assert_eq!(st.suggestions[0].state, SyncSuggestionState::Conflict);
        assert_eq!(f.copies.borrow().len(), 1, "nothing copied");
        // Later looks change nothing and back up nothing again.
        play(&f, A, &s, &mut st, &mut w, 30);
        look(&f, &s, &mut st, &mut w, 30 + SWEEP_MS);
        assert_eq!(auto_of(&st, &p).state, AutoState::Conflict);
        assert_eq!(f.kept.borrow().len(), 2);
        assert_eq!(f.copies.borrow().len(), 1);
        // Sync now does not overwrite it either.
        let v = sync_now(&f, &p, &mut st, K, 40 + SWEEP_MS, &never).unwrap();
        assert_eq!(v.state, AutoState::Conflict);
        assert_eq!(f.copies.borrow().len(), 1);
        // The user keeps B's: copied over A, in step, the prompt gone.
        let v = resolve(&f, &p, &mut st, K, BToA, 50 + SWEEP_MS, &never)
            .unwrap()
            .unwrap();
        assert_eq!(v.state, AutoState::InStep);
        assert_eq!(f.copies.borrow()[1], format!("{K} {B} -> {A}"));
        assert!(st.suggestions.is_empty());
        look(&f, &s, &mut st, &mut w, 60 + 2 * SWEEP_MS);
        assert_eq!(auto_of(&st, &p).state, AutoState::InStep);
        assert_eq!(f.copies.borrow().len(), 2);
    }

    #[test]
    fn the_game_running_on_the_target_holds_the_copy_until_it_closes() {
        let (f, p, s, mut st, mut w) = setup();
        look(&f, &s, &mut st, &mut w, 10);
        for host in [A, B] {
            f.with(host, |c| {
                c.running.insert(K.into());
            });
        }
        look(&f, &s, &mut st, &mut w, 20);
        f.with(A, |c| c.running.clear());
        f.write(A, K);
        look(&f, &s, &mut st, &mut w, 30);
        let v = auto_of(&st, &p);
        assert_eq!(
            (v.state, v.side, v.reason),
            (AutoState::Waiting, Some('b'), Some(AutoReason::Running))
        );
        assert_eq!(f.copies.borrow().len(), 1);
        // Closed on B without a change: A's goes across.
        f.with(B, |c| c.running.clear());
        look(&f, &s, &mut st, &mut w, 40);
        assert_eq!(f.copies.borrow().len(), 2);
        assert_eq!(auto_of(&st, &p).state, AutoState::InStep);
    }

    #[test]
    fn closed_on_the_target_with_its_own_change_is_a_conflict() {
        let (f, p, s, mut st, mut w) = setup();
        look(&f, &s, &mut st, &mut w, 10);
        for host in [A, B] {
            f.with(host, |c| {
                c.running.insert(K.into());
            });
        }
        look(&f, &s, &mut st, &mut w, 20);
        f.with(A, |c| c.running.clear());
        f.write(A, K);
        look(&f, &s, &mut st, &mut w, 30);
        f.with(B, |c| c.running.clear());
        f.write(B, K);
        look(&f, &s, &mut st, &mut w, 40);
        assert_eq!(auto_of(&st, &p).state, AutoState::Conflict);
        assert_eq!(f.copies.borrow().len(), 1);
    }

    #[test]
    fn a_copy_the_helper_cant_make_safely_is_not_made() {
        let (f, p, s, mut st, mut w) = setup();
        f.with(B, |c| {
            c.refuse = Some("this helper can't change a save's owner".into())
        });
        play(&f, A, &s, &mut st, &mut w, 10);
        look(&f, &s, &mut st, &mut w, 20);
        let v = auto_of(&st, &p);
        assert_eq!(
            (v.state, v.reason, v.side),
            (AutoState::CantSync, Some(AutoReason::Refused), Some('b'))
        );
        assert!(v.detail.unwrap().contains("owner"));
        assert_eq!(f.copies.borrow().len(), 1);
        // And it can't be turned on like that.
        let e = turn_on(&f, &pair(), &mut State::default(), K, 30, &never).unwrap_err();
        assert!(e.contains("owner"));
    }

    #[test]
    fn a_copy_that_keeps_failing_stops_until_sync_now() {
        let (f, p, s, mut st, mut w) = setup();
        f.with(B, |c| c.fail_copy = Some("lost the connection".into()));
        play(&f, A, &s, &mut st, &mut w, 10);
        look(&f, &s, &mut st, &mut w, 20);
        let v = auto_of(&st, &p);
        assert_eq!(
            (v.state, v.reason, v.fails),
            (AutoState::Waiting, Some(AutoReason::Error), 1)
        );
        look(&f, &s, &mut st, &mut w, 30);
        look(&f, &s, &mut st, &mut w, 40);
        let v = auto_of(&st, &p);
        assert_eq!(
            (v.state, v.reason, v.fails),
            (AutoState::CantSync, Some(AutoReason::Failed), MAX_FAILS)
        );
        assert_eq!(v.detail.as_deref(), Some("lost the connection"));
        // No more tries by itself.
        look(&f, &s, &mut st, &mut w, 40 + SWEEP_MS);
        assert_eq!(auto_of(&st, &p).fails, MAX_FAILS);
        // Sync now, once the cause is gone.
        f.with(B, |c| c.fail_copy = None);
        let v = sync_now(&f, &p, &mut st, K, 50 + SWEEP_MS, &never).unwrap();
        assert_eq!(v.state, AutoState::InStep);
        assert_eq!(f.copies.borrow().len(), 2);
    }

    #[test]
    fn a_change_made_while_nobody_watched_is_found_by_the_sweep() {
        let (f, p, s, mut st, mut w) = setup();
        look(&f, &s, &mut st, &mut w, 10);
        // Played with this engine off: no close seen.
        f.write(A, K);
        look(&f, &s, &mut st, &mut w, 20);
        assert_eq!(f.copies.borrow().len(), 1, "not before the sweep is due");
        look(&f, &s, &mut st, &mut w, 10 + SWEEP_MS);
        assert_eq!(f.copies.borrow().len(), 2);
        assert_eq!(auto_of(&st, &p).copied_direction, Some(AToB));
    }

    #[test]
    fn turning_on_checks_first_and_copies_nothing() {
        // Never synced: the user picks the first time.
        let f = fake(&[K], &[K]);
        let p = pair();
        let mut st = State::default();
        let v = turn_on(&f, &p, &mut st, K, 1, &never).unwrap();
        assert_eq!((v.state, v.first), (AutoState::Conflict, true));
        assert!(st.suggestions.is_empty(), "picked where it was turned on");
        assert!(f.copies.borrow().is_empty() && f.kept.borrow().is_empty());
        // Newer on one side: queued for the next look.
        apply(&f, &p, &mut st, K, AToB, 2, &never).unwrap();
        f.write(B, K);
        let v = turn_on(&f, &p, &mut st, K, 3, &never).unwrap();
        assert_eq!(
            (v.state, v.reason, v.direction),
            (AutoState::Waiting, Some(AutoReason::Queued), Some(BToA))
        );
        assert_eq!(f.copies.borrow().len(), 1);
        // A PS5 save only on one side waits for its first save there; nothing is copied.
        let f = fake(&[K], &[]);
        let v = turn_on(&f, &p, &mut State::default(), K, 1, &never).unwrap();
        assert_eq!(
            (v.state, v.reason, v.side),
            (AutoState::Waiting, Some(AutoReason::NoSave), Some('b'))
        );
        assert!(f.copies.borrow().is_empty());
        // A PS4 save is refused.
        let f = Fake::default();
        f.with(A, |c| c.saves = vec![save("CUSA01234", 1, "ps4", 10)]);
        f.with(B, |c| c.saves = vec![save("CUSA01234", 2, "ps4", 10)]);
        assert!(
            turn_on(&f, &p, &mut State::default(), "CUSA01234", 1, &never)
                .unwrap_err()
                .contains("PS4")
        );
    }

    // ── a first save on the other side ──────────────────────────────────

    #[test]
    fn a_first_copy_makes_the_save_there_once_and_keeps_it_in_step_after() {
        let f = fake(&[K], &[]);
        let mut p = pair();
        let mut st = State::default();
        // The check says it can be made there.
        let rows = check(&f, &p, &mut st, 1, &|k, _| k == K, &never);
        assert_eq!(rows[0].status, SyncStatus::OnlyOnA);
        assert_eq!(rows[0].first_copy.as_ref().map(|c| c.ok), Some(true));
        // Sync's own copy never makes it.
        assert!(apply(&f, &p, &mut st, K, AToB, 2, &never)
            .unwrap_err()
            .contains("Start the game once there"));
        assert!(f.copies.borrow().is_empty());
        // The first copy, with keeping in step turned on with it.
        let v = first_copy(&f, &p, &mut st, K, true, 3, &never)
            .unwrap()
            .unwrap();
        assert_eq!(*f.copies.borrow(), vec![format!("{K} {A} -> {B}")]);
        assert_eq!(
            (v.state, v.copied_direction),
            (AutoState::InStep, Some(AToB))
        );
        // Both sides' fingerprints are the base now: in step, nothing more to copy.
        p.auto = vec![K.into()];
        let s = Settings {
            pairs: vec![p.clone()],
        };
        let mut w = Watch::default();
        look(&f, &s, &mut st, &mut w, 10);
        assert_eq!(f.copies.borrow().len(), 1);
        assert_eq!(auto_of(&st, &p).state, AutoState::InStep);
        // Played on B afterwards: back to A by itself, like any save kept in step.
        play(&f, B, &s, &mut st, &mut w, 20);
        look(&f, &s, &mut st, &mut w, 30);
        assert_eq!(f.copies.borrow()[1], format!("{K} {B} -> {A}"));
        // Once both have it, a first copy is refused.
        assert!(first_copy(&f, &p, &mut st, K, false, 40, &never)
            .unwrap_err()
            .contains("Both users"));
    }

    #[test]
    fn a_first_copy_the_console_there_cant_make_is_refused_and_said() {
        let f = fake(&[K], &[]);
        f.with(B, |c| {
            c.refuse_first = Some(crate::save_copy::NO_SAVE_DATABASE.into())
        });
        let p = pair();
        let mut st = State::default();
        let rows = check(&f, &p, &mut st, 1, &|k, _| k == K, &never);
        let fc = rows[0].first_copy.clone().unwrap();
        assert!(!fc.ok);
        assert_eq!(fc.code.as_deref(), Some("no_saved_data"));
        assert!(fc.why.unwrap().contains("never saved a PS5 game"));
        let e = first_copy(&f, &p, &mut st, K, true, 2, &never).unwrap_err();
        assert!(e.contains("never saved a PS5 game"));
        assert!(f.copies.borrow().is_empty());
        // Not turned on by a copy that did not happen.
        assert!(st
            .items
            .get(&item_key(&p.id, K))
            .and_then(|i| i.auto.as_ref())
            .is_none());
        // Turned on anyway: it waits, and says why the copy can't be made.
        let v = turn_on(&f, &p, &mut st, K, 3, &never).unwrap();
        assert_eq!(v.reason, Some(AutoReason::NoSave));
        assert!(v.detail.unwrap().contains("never saved a PS5 game"));
    }

    #[test]
    fn waiting_for_a_first_save_becomes_a_pick_once_the_game_saves_there() {
        let f = fake(&[K], &[]);
        let mut p = pair();
        p.auto = vec![K.into()];
        let s = Settings {
            pairs: vec![p.clone()],
        };
        let mut st = State::default();
        let mut w = Watch::default();
        turn_on(&f, &p, &mut st, K, 1, &never).unwrap();
        look(&f, &s, &mut st, &mut w, 10);
        assert_eq!(auto_of(&st, &p).reason, Some(AutoReason::NoSave));
        // Not looked at every minute: only with the sweep, or the game closing there.
        f.with(B, |c| c.saves.push(save(K, 2, "ps5", 5)));
        look(&f, &s, &mut st, &mut w, 70);
        assert_eq!(auto_of(&st, &p).reason, Some(AutoReason::NoSave));
        look(&f, &s, &mut st, &mut w, 10 + SWEEP_MS);
        let v = auto_of(&st, &p);
        // Never synced: the user picks which to keep; nothing is overwritten.
        assert_eq!((v.state, v.first), (AutoState::Conflict, true));
        assert!(f.copies.borrow().is_empty());
    }

    #[test]
    fn the_games_list_shows_every_save_kept_in_step() {
        let (f, p, s, mut st, mut w) = setup();
        let g = games(&s, &st);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].view.state, AutoState::Waiting);
        assert_eq!(g[0].view.reason, Some(AutoReason::Queued));
        assert_eq!(g[0].synced_ms, Some(1));
        look(&f, &s, &mut st, &mut w, 10);
        let g = games(&s, &st);
        assert_eq!(g[0].view.state, AutoState::InStep);
        let v = serde_json::to_value(&g[0]).unwrap();
        assert_eq!(v["state"], "in_step");
        assert_eq!(v["pair"], p.id.as_str());
        assert_eq!(v["title_id"], K);
        // A save not kept in step is not listed.
        let mut other = s.clone();
        other.pairs[0].auto.clear();
        assert!(games(&other, &st).is_empty());
    }
}
