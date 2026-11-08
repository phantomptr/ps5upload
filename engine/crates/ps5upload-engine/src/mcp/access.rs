//! Whether agents may change things through the MCP server, or only look.
//!
//! Read-only is `PS5UPLOAD_MCP_READ_ONLY` in the engine's environment OR the owner's switch in the
//! app (MCP & API › Connect an agent), kept in `agent-access.json` in the engine's data folder. The
//! environment always wins: with it set, the switch cannot let agents change things, and the app
//! shows it as locked.
//!
//! It applies live. `tools/list` and every `tools/call` read it when they run, and an open
//! `subscriptions/listen` stream that asked for it is told `notifications/tools/list_changed` when
//! it flips.
//!
//! * `GET /api/mcp/access` -> `{read_only, setting, locked}`: `read_only` is what is in force,
//!   `setting` the app's switch, `locked` whether the environment holds it read-only.
//! * `PUT /api/mcp/access` `{read_only: bool}` -> the same, after the change.
//!
//! An agent cannot flip it: these routes are mounted beside `/mcp`, outside the router the MCP
//! server calls in-process, and [`McpState::api_call`](super::McpState::api_call) refuses the path
//! however it is written, as `settings_set` refuses the name. That is the whole of the guard. The
//! engine's API has no password, by the owner's decision, so a program that can run commands on
//! this computer (an agent's own shell tool, a script) can still call `PUT /api/mcp/access`
//! directly. This stops agents using PS5Upload's tools to change things; it is not a lock on the
//! computer.
//!
//! Looking is not looking at everything. Whatever this switch says, the in-process answers an
//! agent reads (`McpState::api_call`, `catalog_routes::redact_for_agents`) leave out a live Play
//! Together invite's key, code and links and the phone link's QR and internet links (`GET
//! /api/join`, `GET /api/ps5/rp/pad-link`: a LAN phone with the QR key gets in without the host's
//! Allow), and a tailnet's sign-in link everywhere. Only `together_invite`, which makes an invite
//! because the user asked for one, answers with its link.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use super::McpState;

/// The route, as it is mounted.
pub(crate) const ROUTE: &str = "/api/mcp/access";
/// The setting's name, wherever a name is looked up (`settings_set`).
pub(crate) const SETTING: &str = "mcp_read_only";
const ENV: &str = "PS5UPLOAD_MCP_READ_ONLY";
const FILE: &str = "agent-access.json";

/// What a refused call says, after the tool or request it names.
pub(crate) fn refusal(locked: bool) -> String {
    let mut text = String::from(
        "this PS5Upload is set so agents can only look; change it in the app under MCP & API › \
         Connect an agent (\"Agents can change things\").",
    );
    if locked {
        text.push_str(&format!(
            " Here it is set by {ENV} in the engine's environment, which the owner has to remove \
             and then restart the engine."
        ));
    }
    text
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    read_only: bool,
}

/// The switch and the environment. Cheap to clone.
#[derive(Clone)]
pub(crate) struct AgentAccess(Arc<Inner>);

struct Inner {
    /// `PS5UPLOAD_MCP_READ_ONLY`, read once at startup.
    locked: bool,
    /// The app's switch. Writes go through `save` under its lock.
    setting: Mutex<bool>,
    /// Where the switch is kept; `None` keeps it in memory (tests, no data folder).
    file: Option<PathBuf>,
    /// What is in force, for the streams that say when it changes.
    in_force: watch::Sender<bool>,
}

impl AgentAccess {
    /// The engine's: the environment, and the switch as it was saved.
    pub(crate) fn from_environment() -> Self {
        let locked = env_says_yes(std::env::var(ENV).ok().as_deref());
        let file = if cfg!(test) {
            None
        } else {
            ps5upload_ava1::pool::data_dir().map(|d| d.join(FILE))
        };
        let setting = file.as_deref().map(load).unwrap_or_default();
        let access = Self::new(locked, setting, file);
        if locked {
            crate::log_info!("mcp: agents can only look ({ENV})");
        } else if setting {
            crate::log_info!("mcp: agents can only look (set in the app)");
        }
        access
    }

    /// For tests: `locked` stands for the environment, `setting` for the saved switch; nothing is written.
    #[cfg(test)]
    pub(crate) fn in_memory(locked: bool, setting: bool) -> Self {
        Self::new(locked, setting, None)
    }

    fn new(locked: bool, setting: bool, file: Option<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            locked,
            setting: Mutex::new(setting),
            file,
            in_force: watch::channel(locked || setting).0,
        }))
    }

    /// Agents may only look: the environment or the switch says so.
    pub(crate) fn read_only(&self) -> bool {
        self.0.locked || self.setting()
    }

    /// The environment holds it read-only; the switch cannot change that.
    pub(crate) fn locked(&self) -> bool {
        self.0.locked
    }

    fn setting(&self) -> bool {
        *self.0.setting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes the switch and keeps it. In force at once; the streams that listen are told when
    /// what is in force changed.
    pub(crate) fn set(&self, read_only: bool) -> Result<(), String> {
        let mut setting = self.0.setting.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = &self.0.file {
            save(file, read_only)
                .map_err(|e| crate::state_io::describe("Could not keep the setting", file, &e))?;
        }
        *setting = read_only;
        let now = self.0.locked || read_only;
        drop(setting);
        self.0.in_force.send_if_modified(|was| {
            let changed = *was != now;
            *was = now;
            changed
        });
        crate::log_info!(
            "mcp: agents {} (set in the app{})",
            if read_only {
                "can only look"
            } else {
                "can change things"
            },
            if self.0.locked {
                format!("; {ENV} still holds it read-only")
            } else {
                String::new()
            }
        );
        Ok(())
    }

    /// Says when what is in force changes.
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.0.in_force.subscribe()
    }

    fn status(&self) -> AgentAccessState {
        AgentAccessState {
            read_only: self.read_only(),
            setting: self.setting(),
            locked: self.locked(),
        }
    }
}

/// What `/api/mcp/access` answers.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct AgentAccessState {
    /// In force now: agents can only look.
    read_only: bool,
    /// The app's switch ("Agents can change things" is its opposite).
    setting: bool,
    /// `PS5UPLOAD_MCP_READ_ONLY` in the engine's environment holds it read-only, whatever the
    /// switch says.
    locked: bool,
}

fn env_says_yes(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
    })
}

/// The saved switch; a missing or unreadable file is the default, full access.
fn load(file: &Path) -> bool {
    match std::fs::read(file) {
        Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) => saved.read_only,
            Err(e) => {
                crate::log_warn!(
                    "mcp: {} unreadable ({e}): agents can change things",
                    file.display()
                );
                false
            }
        },
        Err(_) => false,
    }
}

/// Written whole through a temporary file, so a crash never leaves half of it.
fn save(file: &Path, read_only: bool) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_file_name(format!(
        "{FILE}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec_pretty(&Saved { read_only }).map_err(std::io::Error::other)?;
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// `GET /api/mcp/access`: what is in force, the app's switch, and whether the environment locks it.
pub(crate) async fn get(State(state): State<McpState>) -> Json<AgentAccessState> {
    Json(state.access().status())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentAccessChange {
    /// `true`: agents can only look. `false`: they can change things too.
    read_only: bool,
}

/// `PUT /api/mcp/access` `{read_only}`: the app's switch. With the environment set, it is kept but
/// agents still only look until the environment changes.
pub(crate) async fn put(
    State(state): State<McpState>,
    Json(change): Json<AgentAccessChange>,
) -> Response {
    let access = state.access().clone();
    let saved = tokio::task::spawn_blocking(move || access.set(change.read_only)).await;
    match saved {
        Ok(Ok(())) => Json(state.access().status()).into_response(),
        Ok(Err(message)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": message })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Could not change the setting: {e}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_takes_the_usual_yes() {
        for yes in ["1", "true", "TRUE", " yes "] {
            assert!(env_says_yes(Some(yes)), "{yes}");
        }
        for no in ["0", "", "false", "off"] {
            assert!(!env_says_yes(Some(no)), "{no}");
        }
        assert!(!env_says_yes(None));
    }

    #[test]
    fn the_environment_wins_over_the_switch() {
        let access = AgentAccess::in_memory(true, false);
        assert!(access.read_only());
        assert!(access.locked());
        access.set(false).unwrap();
        assert!(access.read_only());
        assert_eq!(
            access.status(),
            AgentAccessState {
                read_only: true,
                setting: false,
                locked: true
            }
        );
    }

    #[test]
    fn the_switch_applies_at_once_and_says_so_when_it_changes() {
        let access = AgentAccess::in_memory(false, false);
        let mut changes = access.subscribe();
        assert!(!access.read_only());
        access.set(true).unwrap();
        assert!(access.read_only());
        assert!(changes.has_changed().unwrap());
        assert!(*changes.borrow_and_update());
        // The same value again is no change.
        access.set(true).unwrap();
        assert!(!changes.has_changed().unwrap());
        access.set(false).unwrap();
        assert!(!*changes.borrow_and_update());
    }

    #[test]
    fn the_switch_is_kept_in_its_file() {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-agent-access-{}-{}",
            std::process::id(),
            SEQ_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let file = dir.join(FILE);
        assert!(!load(&file));
        let access = AgentAccess::new(false, false, Some(file.clone()));
        access.set(true).unwrap();
        assert!(load(&file));
        access.set(false).unwrap();
        assert!(!load(&file));
        std::fs::write(&file, b"not json").unwrap();
        assert!(!load(&file));
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }

    static SEQ_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn a_refusal_says_where_to_change_it() {
        assert!(refusal(false).contains("agents can only look"));
        assert!(refusal(false).contains("MCP & API"));
        assert!(!refusal(false).contains(ENV));
        assert!(refusal(true).contains(ENV));
    }
}
