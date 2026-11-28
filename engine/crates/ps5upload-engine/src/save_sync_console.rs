//! Save sync on the console's own page: what the saves kept in step on a console stand at, put on
//! that console for its page to show (read only).
//!
//! The page talks only to its helper, and save sync lives in this engine, so the engine writes a
//! small document onto each console of a pair through the mesh's documents (`MESH_DOC_PUT`, the
//! same store Mesh Drive keeps its drives in), and the helper's `/api/save-sync` reads it back.
//! One document, [`DOC`], per console; each engine with pairs keeps its own entry in it (keyed by
//! the start of its device key), so the desktop app and a Docker engine never write over each
//! other. An entry holds only that console's side: its user, the other console and its user, and
//! where each game stands, in codes the page says in its own language.
//!
//! Written when what a console's entry says changes, and at least every [`REFRESH_MS`] so the page
//! can tell an engine that stopped (it shows when the entry was written). Nothing is written to a
//! console that did not answer, and a failed write is tried at the next look. An entry not
//! written for [`FORGET_MS`] is dropped by the next engine that writes.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::save_sync::{item_key, Settings, State, SyncDirection};
use crate::save_sync_auto::{to_side, AutoReason, AutoState};

/// The document's name on the console.
pub const DOC: &str = "save-sync";
/// An unchanged entry is written again this often.
pub const REFRESH_MS: u64 = 30 * 60_000;
/// Another engine's entry not written for this long is dropped.
pub const FORGET_MS: u64 = 30 * 24 * 3_600_000;
/// The most games one entry lists (a document is at most 160 KiB on the console).
pub const MAX_GAMES: usize = 200;

/// One save kept in step, seen from one console.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConsoleGame {
    pub title_id: String,
    /// This console's user (8 hex digits, as its home folder).
    pub user: String,
    /// The other console: its name in the app, else its address.
    pub other: String,
    pub other_user: String,
    pub state: AutoState,
    pub reason: Option<AutoReason>,
    /// The state is about this console (it is the one waited for, or the one with no save yet);
    /// false: about the other one.
    pub here: bool,
    /// The last copy made between the two, and whether it came to this console.
    pub copied_ms: Option<u64>,
    pub copied_here: Option<bool>,
}

/// What `host`'s entry says: every save kept in step with one side on it.
pub fn games_for(
    settings: &Settings,
    state: &State,
    host: &str,
    name_of: &dyn Fn(&str) -> String,
) -> Vec<ConsoleGame> {
    let key = crate::helper_stop::console_key(host);
    let mut out = Vec::new();
    for p in &settings.pairs {
        for (me, other, c) in [(&p.a, &p.b, 'a'), (&p.b, &p.a, 'b')] {
            if crate::helper_stop::console_key(&me.host) != key {
                continue;
            }
            for k in &p.auto {
                let Some(v) = state
                    .items
                    .get(&item_key(&p.id, k))
                    .and_then(|i| i.auto.as_ref())
                else {
                    continue;
                };
                let title_id = if v.title_id.is_empty() {
                    crate::save_sync::title_of_key(k).to_string()
                } else {
                    v.title_id.clone()
                };
                out.push(ConsoleGame {
                    title_id,
                    user: format!("{:08x}", me.user_id),
                    other: name_of(&other.host),
                    other_user: format!("{:08x}", other.user_id),
                    state: v.state,
                    reason: v.reason,
                    here: v.side == Some(c),
                    copied_ms: v.copied_ms,
                    copied_here: v.copied_direction.map(|d: SyncDirection| to_side(d) == c),
                });
            }
        }
    }
    out.sort_by(|x, y| x.title_id.cmp(&y.title_id).then(x.user.cmp(&y.user)));
    out.truncate(MAX_GAMES);
    out
}

/// The document with this engine's entry put in (`id`), others' kept unless they were not
/// written for [`FORGET_MS`]. `doc`: what the console holds now (None: nothing yet).
pub fn merged(doc: Option<&Value>, id: &str, entry: Value, now: u64) -> Value {
    let mut engines: Map<String, Value> = doc
        .and_then(|d| d.get("engines"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    engines.retain(|k, v| {
        k != id
            && v.get("at_ms")
                .and_then(Value::as_u64)
                .is_some_and(|t| now.saturating_sub(t) < FORGET_MS)
    });
    engines.insert(id.to_string(), entry);
    json!({ "v": 1, "engines": engines })
}

fn digest(games: &[ConsoleGame]) -> String {
    let text = serde_json::to_string(games).unwrap_or_default();
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether `host`'s entry is due: it changed since the last write, or that was long ago.
pub fn due(last: Option<&(String, u64)>, digest: &str, now: u64) -> bool {
    match last {
        None => true,
        Some((d, at)) => d != digest || now.saturating_sub(*at) >= REFRESH_MS,
    }
}

/// What was last written to each console (digest of its games, when).
fn written() -> &'static Mutex<BTreeMap<String, (String, u64)>> {
    static W: OnceLock<Mutex<BTreeMap<String, (String, u64)>>> = OnceLock::new();
    W.get_or_init(Default::default)
}

/// Writes each answering console's entry that is due (see the module docs). Blocking: from the
/// background look's thread, after it let go of the consoles.
pub fn publish(settings: &Settings, state: &State, answered: &BTreeMap<String, bool>, now: u64) {
    let mut hosts: Vec<String> = Vec::new();
    for p in &settings.pairs {
        for h in [&p.a.host, &p.b.host] {
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
    }
    publish_to(&hosts, settings, state, answered, now);
}

/// [`publish`] to `hosts` (a console no pair has any more gets an entry with no games).
fn publish_to(
    hosts: &[String],
    settings: &Settings,
    state: &State,
    answered: &BTreeMap<String, bool>,
    now: u64,
) {
    let Some(id) = crate::mesh_api::id_hex() else {
        return;
    };
    let id: String = id.chars().take(16).collect();
    let name_of = |h: &str| {
        crate::consoles_api::console_brief(h)
            .and_then(|(n, _)| n)
            .unwrap_or_else(|| ps5upload_core::events::console_host(h))
    };
    for host in hosts.iter().map(String::as_str) {
        if answered.get(host) != Some(&true) {
            continue;
        }
        let games = games_for(settings, state, host, &name_of);
        let d = digest(&games);
        let last = written()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(host)
            .cloned();
        if !due(last.as_ref(), &d, now) {
            continue;
        }
        let entry = json!({
            "name": crate::mesh_api::device_name(),
            "at_ms": now,
            "games": games,
        });
        match write(host, &id, entry, now) {
            Ok(true) => {
                written()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(host.to_string(), (d, now));
            }
            // A helper without documents: nothing to show there, asked again later.
            Ok(false) => {
                written()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(host.to_string(), (d, now));
            }
            Err(e) => crate::log_debug!("save sync: status for {host}'s page not written: {e}"),
        }
    }
}

/// A pair was removed: its consoles' entries are written again now, in the background (with no
/// games left there, the page's card goes). Best effort: a console that does not answer keeps the
/// old entry until the next write, and the page says how old it is.
pub fn after_removal(hosts: Vec<String>, settings: Settings, state: State) {
    if hosts.is_empty() {
        return;
    }
    {
        let mut w = written().lock().unwrap_or_else(|e| e.into_inner());
        for h in &hosts {
            w.remove(h);
        }
    }
    std::thread::spawn(move || {
        let answered: BTreeMap<String, bool> = hosts.iter().map(|h| (h.clone(), true)).collect();
        publish_to(&hosts, &settings, &state, &answered, crate::save_backup::now_ms());
    });
}

/// Read, merge, write against the revision read; again when another device wrote in between.
/// false: the helper has no documents.
fn write(host: &str, id: &str, entry: Value, now: u64) -> Result<bool, String> {
    let addr = crate::console_addr(host);
    for _ in 0..4 {
        let got = crate::mesh_api::call(&addr, || ps5upload_core::mesh::doc_get(&addr, DOC))
            .map_err(|e| format!("{e:#}"))?;
        let Some(got) = got else { return Ok(false) };
        if got.get("ok").and_then(Value::as_bool) == Some(false) {
            // doc_unreadable and the like: never written over.
            return Err(got
                .get("err")
                .and_then(Value::as_str)
                .unwrap_or("refused")
                .to_string());
        }
        let rev = got.get("rev").and_then(Value::as_u64).unwrap_or(0);
        let doc = merged(got.get("doc"), id, entry.clone(), now);
        let put = crate::mesh_api::call(&addr, || {
            ps5upload_core::mesh::doc_put(&addr, DOC, rev, &doc)
        })
        .map_err(|e| format!("{e:#}"))?;
        let Some(put) = put else { return Ok(false) };
        if put.get("saved").and_then(Value::as_bool) == Some(true) {
            return Ok(true);
        }
    }
    Err("the console's save sync status kept changing".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save_sync::tests::pair;
    use crate::save_sync::SyncItemState;
    use crate::save_sync_auto::AutoView;

    fn view(state: AutoState, reason: Option<AutoReason>, side: Option<char>) -> AutoView {
        AutoView {
            state,
            title_id: "PPSA01234".into(),
            kind: "ps5".into(),
            side,
            reason,
            detail: Some("not shown on the console".into()),
            direction: None,
            first: false,
            kept: vec![],
            since_ms: 1,
            fails: 0,
            woke_ms: None,
            copied_ms: Some(50),
            copied_direction: Some(SyncDirection::AToB),
        }
    }

    fn setup(v: AutoView) -> (Settings, State) {
        let mut p = pair();
        p.auto = vec!["PPSA01234".into(), "PPSA09999".into()];
        let mut st = State::default();
        st.items.insert(
            item_key(&p.id, "PPSA01234"),
            SyncItemState {
                auto: Some(v),
                ..Default::default()
            },
        );
        (Settings { pairs: vec![p] }, st)
    }

    #[test]
    fn each_console_sees_its_own_side_of_each_game() {
        let (s, st) = setup(view(
            AutoState::Waiting,
            Some(AutoReason::Offline),
            Some('b'),
        ));
        let named = |h: &str| format!("PS5 at {h}");
        let a = games_for(&s, &st, "10.0.0.1", &named);
        // Only games with a view yet; never the engine's own words (detail).
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].user, "00000001");
        assert_eq!(a[0].other, "PS5 at 10.0.0.2");
        assert_eq!(a[0].other_user, "00000002");
        assert!(!a[0].here, "waiting for the other one");
        assert_eq!(a[0].copied_here, Some(false));
        let b = games_for(&s, &st, "10.0.0.2", &named);
        assert!(b[0].here);
        assert_eq!(b[0].copied_here, Some(true));
        assert_eq!(b[0].other, "PS5 at 10.0.0.1");
        // An address with a port is the same console.
        assert_eq!(games_for(&s, &st, "10.0.0.2:9113", &named), b);
        assert!(games_for(&s, &st, "10.0.0.3", &named).is_empty());
        let text = serde_json::to_string(&b).unwrap();
        assert!(!text.contains("not shown"));
        assert!(text.contains("\"state\":\"waiting\""));
        assert!(text.contains("\"reason\":\"offline\""));
    }

    #[test]
    fn an_entry_is_written_when_it_changes_or_every_half_hour() {
        assert!(due(None, "x", 1));
        let last = ("x".to_string(), 1000);
        assert!(!due(Some(&last), "x", 1000 + REFRESH_MS - 1));
        assert!(due(Some(&last), "x", 1000 + REFRESH_MS));
        assert!(due(Some(&last), "y", 1001));
    }

    #[test]
    fn each_engine_keeps_its_own_entry_and_old_ones_go() {
        let now = 10 * FORGET_MS;
        let doc = json!({ "v": 1, "engines": {
            "mine": { "at_ms": now - 5, "games": [1] },
            "docker": { "at_ms": now - 10, "games": [2] },
            "gone": { "at_ms": now - FORGET_MS, "games": [3] },
            "odd": { "games": [4] },
        }});
        let out = merged(
            Some(&doc),
            "mine",
            json!({ "at_ms": now, "games": [] }),
            now,
        );
        let e = out["engines"].as_object().unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e["mine"]["games"], json!([]));
        assert_eq!(e["docker"]["games"], json!([2]));
        // Nothing there yet, or not a document this version knows: just this entry.
        let fresh = merged(None, "mine", json!({ "at_ms": 1 }), 1);
        assert_eq!(fresh["engines"].as_object().unwrap().len(), 1);
        assert_eq!(fresh["v"], 1);
    }

    #[test]
    fn a_long_list_fits_the_console() {
        let mut p = pair();
        let mut st = State::default();
        for n in 0..(MAX_GAMES + 50) {
            let k = format!("PPSA{n:05}");
            p.auto.push(k.clone());
            let mut v = view(AutoState::InStep, None, None);
            v.title_id = k.clone();
            st.items.insert(
                item_key(&p.id, &k),
                SyncItemState {
                    auto: Some(v),
                    ..Default::default()
                },
            );
        }
        let s = Settings { pairs: vec![p] };
        let g = games_for(&s, &st, "10.0.0.1", &|h| h.to_string());
        assert_eq!(g.len(), MAX_GAMES);
        let doc = merged(
            None,
            "0123456789abcdef",
            json!({ "at_ms": 1, "name": "x".repeat(63), "games": g }),
            1,
        );
        assert!(serde_json::to_string(&doc).unwrap().len() < 160 * 1024);
    }
}
