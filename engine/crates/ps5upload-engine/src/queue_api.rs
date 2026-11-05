//! The engine's own queue: `GET/POST /api/queue`, `POST /api/queue/{id}/pause|resume|remove|move`.
//!
//! One line per console, kept in `<data dir>/engine-queue.json`, shared by every client of this
//! engine. An item is one call of an existing route (`/api/transfer/*`, `/api/link/download`,
//! `/api/pkg/install`) followed through `/api/jobs/{id}`, so there is no second way to move bytes.
//!
//! Rules a client can rely on:
//! - In order, one at a time per console; different consoles run side by side.
//! - An item that was running when the engine stopped comes back `interrupted`, never restarted:
//!   `resume` is the only way it runs again. Items that were waiting carry on.
//! - A failed item is not retried on its own. Its failure may have come after the bytes landed
//!   (an install refusal), and running it again by itself is how a queue re-uploads a game.
//! - The queue waits while anything else uses the console (a job another client started, an
//!   install, a client's [`hold`](hold_handler)) and while the console's helper does not answer,
//!   so a console that is off holds the line instead of failing every item in it. Nothing here
//!   sends the helper or wakes a console.
//! - A link's token and an archive's password stay in memory: they are not written to disk or
//!   shown. An item that needs one after a restart comes back `interrupted`.
//!
//! A client running its own queue (the app) takes a hold with `POST /api/queue/hold` while it works
//! on a console. The hold is refused while a queue item runs there, and the queue starts nothing
//! there while it lasts, so the two take turns.

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};
use tower::ServiceExt;

use crate::event_bus::bus;
use crate::pkg_install::PkgInstallStateHandle;

const FILE: &str = "engine-queue.json";
/// Waiting items one console may have.
const MAX_WAITING: usize = 1000;
/// Finished items kept per console; the oldest go first.
const MAX_FINISHED: usize = 200;
/// An item's request, as sent. The whole file is written on each change, so a list of 100k files
/// belongs in a direct `/api/transfer/file-list` call.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_LABEL_CHARS: usize = 200;
const MAX_HOLDER_CHARS: usize = 100;
const DEFAULT_HOLD_S: u64 = 30;
const MAX_HOLD_S: u64 = 300;
const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What an item does: one route of the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ItemKind {
    #[serde(rename = "upload_file")]
    UploadFile,
    #[serde(rename = "upload_dir")]
    UploadDir,
    #[serde(rename = "upload_zip")]
    UploadZip,
    #[serde(rename = "upload_7z")]
    Upload7z,
    #[serde(rename = "upload_rar")]
    UploadRar,
    #[serde(rename = "upload_file_list")]
    UploadFileList,
    #[serde(rename = "link_download")]
    LinkDownload,
    #[serde(rename = "download")]
    Download,
    #[serde(rename = "install")]
    Install,
}

/// The kinds, by the names a client uses.
const KIND_NAMES: &str = "upload_file, upload_dir, upload_zip, upload_7z, upload_rar, \
                          upload_file_list, link_download, download, install";

impl ItemKind {
    fn parse(name: &str) -> Option<Self> {
        serde_json::from_value(json!(name)).ok()
    }

    fn route(self) -> &'static str {
        match self {
            ItemKind::UploadFile => "/api/transfer/file",
            ItemKind::UploadDir => "/api/transfer/dir",
            ItemKind::UploadZip => "/api/transfer/zip",
            ItemKind::Upload7z => "/api/transfer/7z",
            ItemKind::UploadRar => "/api/transfer/rar",
            ItemKind::UploadFileList => "/api/transfer/file-list",
            ItemKind::LinkDownload => "/api/link/download",
            ItemKind::Download => "/api/transfer/download",
            ItemKind::Install => "/api/pkg/install",
        }
    }

    /// The route's own name for the console.
    fn console_field(self) -> &'static str {
        match self {
            ItemKind::Install => "ps5_addr",
            _ => "addr",
        }
    }

    /// The route takes a `tx_id`, so a stopped run picks up where it was.
    fn resumes(self) -> bool {
        !matches!(self, ItemKind::Install | ItemKind::Download)
    }

    /// The route's own request type reads it. A mistake is refused when the item is added, not
    /// hours later when its turn comes.
    fn check(self, body: &Value) -> Result<(), String> {
        fn reads<T: serde::de::DeserializeOwned>(body: &Value) -> Result<(), String> {
            serde_json::from_value::<T>(body.clone())
                .map(drop)
                .map_err(|e| e.to_string())
        }
        match self {
            ItemKind::UploadFile => reads::<crate::TransferFileReq>(body),
            ItemKind::UploadDir => reads::<crate::TransferDirReq>(body),
            ItemKind::UploadZip => reads::<crate::TransferZipReq>(body),
            ItemKind::Upload7z => reads::<crate::Transfer7zReq>(body),
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ItemKind::UploadRar => reads::<crate::TransferRarReq>(body),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            ItemKind::UploadRar => Err("rar archives need the desktop app's engine".to_string()),
            ItemKind::UploadFileList => reads::<crate::TransferFileListReq>(body),
            ItemKind::LinkDownload => reads::<crate::LinkDownloadReq>(body),
            ItemKind::Download => reads::<crate::TransferDownloadReq>(body),
            ItemKind::Install => reads::<crate::install::InstallRequest>(body),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    /// Waits for its turn.
    Queued,
    /// Its route has been called, or is being called.
    Running,
    /// Skipped until resumed. It keeps its place.
    Paused,
    Done,
    Failed,
    /// Someone stopped its job or install directly.
    Cancelled,
    /// Was running when the engine stopped, or its job can no longer be found.
    Interrupted,
}

impl Status {
    fn waiting(self) -> bool {
        matches!(self, Status::Queued | Status::Paused)
    }

    fn finished(self) -> bool {
        !self.waiting() && self != Status::Running
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intent {
    /// Stop the running job and keep the item, paused.
    Pause,
    /// Stop the running job and drop the item.
    Remove,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Item {
    id: String,
    /// The console's host, lower case: what groups the line.
    console: String,
    kind: ItemKind,
    label: String,
    status: Status,
    /// The route's body, without any secret.
    request: Value,
    #[serde(default)]
    stop_on_failure: bool,
    added_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<u64>,
    /// The job (`/api/jobs/{id}`) or install of the current or last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    /// Runs started, this one included.
    #[serde(default)]
    runs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    /// How an install ended: installed, may_not_launch, unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<String>,
    /// What the install says about its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    /// The latest figures of the job, from `/api/jobs/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<Value>,
    /// The request had a secret, which [`Self::secrets`] holds until the engine stops.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    secret_in_memory: bool,
    #[serde(skip)]
    secrets: Secrets,
    #[serde(skip)]
    intent: Option<Intent>,
}

#[derive(Clone, Debug, Default)]
struct Secrets {
    password: Option<String>,
    url: Option<String>,
}

impl Secrets {
    fn is_empty(&self) -> bool {
        self.password.is_none() && self.url.is_none()
    }
}

/// `https://user:pw@host/a.pkg?token=x#f` without what can carry a secret, and whether it had any.
fn url_without_secret(url: &str) -> (String, bool) {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let (mut plain, mut had) = (url[..end].to_string(), end < url.len());
    if let Some(scheme_end) = plain.find("://") {
        let host_at = scheme_end + 3;
        let authority_end = plain[host_at..]
            .find('/')
            .map_or(plain.len(), |i| host_at + i);
        if let Some(at) = plain[host_at..authority_end].rfind('@') {
            plain.replace_range(host_at..=host_at + at, "");
            had = true;
        }
    }
    if had {
        plain.push_str("?...");
    }
    (plain, had)
}

/// The link's place in a request, by kind.
fn url_slot(kind: ItemKind, body: &mut Value) -> Option<&mut Value> {
    match kind {
        ItemKind::LinkDownload => body.get_mut("url"),
        ItemKind::Install => body.get_mut("source")?.get_mut("url"),
        _ => None,
    }
}

/// Takes out of a request what must not reach the disk or another client.
fn take_secrets(kind: ItemKind, body: &mut Value) -> Secrets {
    let mut secrets = Secrets::default();
    if let Some(slot) = url_slot(kind, body) {
        if let Some(url) = slot.as_str().map(str::to_string) {
            let (plain, had) = url_without_secret(&url);
            if had {
                *slot = json!(plain);
                secrets.url = Some(url);
            }
        }
    }
    if let Some(map) = body.as_object_mut() {
        match map.remove("password") {
            Some(Value::String(password)) => secrets.password = Some(password),
            Some(Value::Null) | None => {}
            // Not a password the route could read: the route's own check refuses it.
            Some(other) => {
                map.insert("password".to_string(), other);
            }
        }
    }
    secrets
}

/// The request as its route takes it.
fn with_secrets(item: &Item) -> Value {
    let mut body = item.request.clone();
    if let Some(url) = &item.secrets.url {
        if let Some(slot) = url_slot(item.kind, &mut body) {
            *slot = json!(url);
        }
    }
    if let (Some(password), Some(map)) = (&item.secrets.password, body.as_object_mut()) {
        map.insert("password".to_string(), json!(password));
    }
    body
}

/// A name for an item that was not given one: the last part of where it goes or comes from.
fn default_label(kind: ItemKind, body: &Value) -> String {
    let field = |name: &str| body.get(name).and_then(Value::as_str);
    let whole = match kind {
        ItemKind::Install => {
            let source = body.get("source").and_then(Value::as_object);
            source.and_then(|source| {
                source.values().find_map(|value| {
                    value
                        .as_str()
                        .or_else(|| value.get("path").and_then(Value::as_str))
                })
            })
        }
        ItemKind::UploadFile => field("src"),
        ItemKind::UploadDir => field("src_dir"),
        ItemKind::UploadZip => field("zip_path"),
        ItemKind::Upload7z | ItemKind::UploadRar => field("archive_path"),
        ItemKind::UploadFileList => field("dest_root"),
        ItemKind::LinkDownload => field("url"),
        ItemKind::Download => field("src_path"),
    }
    .unwrap_or("");
    let plain = whole.strip_suffix("?...").unwrap_or(whole);
    plain
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// Why a console's next item has not started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WaitingFor {
    /// A client holds the console.
    Hold,
    /// A job, install or console operation this engine did not start from the queue.
    OtherWork,
    /// A task running on the console itself (Minion's copies, a PC's on a share).
    ConsoleTask,
    /// The console's helper does not answer.
    Console,
    /// A short pause between items.
    Settle,
}

/// What `GET /api/ps5/console-tasks` answers holds a task still running.
fn tasks_say_running(answer: &Value) -> bool {
    answer
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| tasks.iter().any(|t| t["state"] == "running"))
}

#[derive(Default)]
struct Book {
    items: Vec<Item>,
    /// console → holder → when the hold lapses.
    holds: HashMap<String, HashMap<String, Instant>>,
    waiting_for: HashMap<String, WaitingFor>,
    /// Consoles with a driver task.
    drivers: HashSet<String>,
    /// When a console's last item ended.
    last_ended: HashMap<String, Instant>,
    /// Numbers the saves, so an older one never lands after a newer one.
    generation: u64,
}

impl Book {
    fn find(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    fn running_on(&self, console: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|item| item.console == console && item.status == Status::Running)
    }

    fn next_on(&self, console: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|item| item.console == console && item.status == Status::Queued)
    }

    fn holders(&mut self, console: &str) -> Vec<String> {
        let now = Instant::now();
        let Some(holds) = self.holds.get_mut(console) else {
            return Vec::new();
        };
        holds.retain(|_, until| *until > now);
        let mut holders: Vec<String> = holds.keys().cloned().collect();
        if holders.is_empty() {
            self.holds.remove(console);
        }
        holders.sort();
        holders
    }

    fn position(&self, item: &Item) -> Option<usize> {
        item.status.waiting().then(|| {
            self.items
                .iter()
                .take_while(|other| other.id != item.id)
                .filter(|other| other.console == item.console && other.status.waiting())
                .count()
        })
    }

    fn view(&self, item: &Item) -> Value {
        let mut view = json!(item);
        if let Some(position) = self.position(item) {
            view["position"] = json!(position);
        }
        view
    }

    /// `item` moves to `position` among the waiting items of its console.
    fn place(&mut self, at: usize, position: usize) {
        let item = self.items.remove(at);
        let mut seen = 0;
        let mut to = None;
        for (index, other) in self.items.iter().enumerate() {
            if other.console == item.console && other.status.waiting() {
                if seen == position {
                    to = Some(index);
                    break;
                }
                seen += 1;
            }
        }
        match to {
            Some(index) => self.items.insert(index, item),
            None => {
                // Behind the last waiting item of the console: the list's end is after it.
                self.items.push(item);
            }
        }
    }

    /// Items after a failed `stop_on_failure` item wait for someone to look.
    fn hold_later_items(&mut self, at: usize) {
        let console = self.items[at].console.clone();
        let id = self.items[at].id.clone();
        for item in self.items.iter_mut().skip(at + 1) {
            if item.console == console && item.status == Status::Queued {
                item.status = Status::Paused;
                item.code = Some("held_after_failure".to_string());
                item.error = Some(format!("Held because {id} did not finish."));
            }
        }
    }

    /// The oldest finished items of a console go past the limit.
    fn prune(&mut self, console: &str) {
        let finished = self
            .items
            .iter()
            .filter(|item| item.console == console && item.status.finished())
            .count();
        let mut over = finished.saturating_sub(MAX_FINISHED);
        self.items.retain(|item| {
            if over > 0 && item.console == console && item.status.finished() {
                over -= 1;
                return false;
            }
            true
        });
    }

    fn console_view(&mut self, console: &str) -> Value {
        let holders = self.holders(console);
        let running = self.running_on(console).map(|item| item.id.clone());
        let waiting = self
            .items
            .iter()
            .filter(|item| item.console == console && item.status == Status::Queued)
            .count();
        let waiting_for = self.waiting_for.get(console).copied();
        let state = if running.is_some() {
            "running"
        } else if waiting == 0 {
            "idle"
        } else {
            "waiting"
        };
        json!({
            "console": console,
            "state": state,
            "running_item": running,
            "queued": waiting,
            "waiting_for": if state == "waiting" { waiting_for } else { None },
            "holders": holders,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    items: Vec<Value>,
}

/// How long the queue waits between looks and between items.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    pub tick: Duration,
    /// Longer than the app's look for a turn (`client/src/api/engineQueue.ts`), so a client waiting
    /// for the console gets in between two items.
    pub settle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            settle: Duration::from_secs(3),
        }
    }
}

/// Whether a console's helper answers.
type Probe = Arc<dyn Fn(String) -> BoxFuture<'static, bool> + Send + Sync>;

async fn helper_answers(addr: String) -> bool {
    let target = ps5upload_ava1::pool::ava1_addr(&addr);
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(target)).await,
        Ok(Ok(_))
    )
}

struct Inner {
    path: Option<PathBuf>,
    book: Mutex<Book>,
    /// The engine's API: what items call.
    api: OnceLock<Router>,
    installs: OnceLock<PkgInstallStateHandle>,
    probe: Probe,
    timing: Timing,
    written: Mutex<u64>,
}

/// The queue. Cheap to clone.
#[derive(Clone)]
pub struct QueueState(Arc<Inner>);

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn read_stored(path: &std::path::Path) -> Vec<Item> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            crate::log_warn!("queue: cannot read {}: {e}", path.display());
            return Vec::new();
        }
    };
    let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
        // Kept for a person to look at; the queue must still start.
        let aside = path.with_extension("json.corrupt");
        crate::log_warn!(
            "queue: {} is not readable, moved to {}",
            path.display(),
            aside.display()
        );
        let _ = std::fs::rename(path, aside);
        return Vec::new();
    };
    stored
        .items
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<Item>(value) {
            Ok(item) => Some(item),
            Err(e) => {
                // From a newer engine, say.
                crate::log_warn!("queue: dropped an item this engine cannot read: {e}");
                None
            }
        })
        .collect()
}

/// What a restart means for what was on the list. True when anything changed.
fn after_restart(book: &mut Book) -> bool {
    let mut changed = false;
    for at in 0..book.items.len() {
        let was = book.items[at].status;
        let item = &mut book.items[at];
        if was == Status::Running {
            item.status = Status::Interrupted;
            item.code = Some("interrupted".to_string());
            item.error = Some(if item.kind == ItemKind::Install {
                "The engine stopped during this install, and the console may have gone on with \
                 it. Check the console first: resume runs the install again."
                    .to_string()
            } else {
                "The engine stopped during this item. Resume runs it again.".to_string()
            });
            item.finished_at_ms = Some(crate::now_ms());
            changed = true;
        } else if was.waiting() && item.secret_in_memory {
            item.status = Status::Interrupted;
            item.code = Some("secret_not_kept".to_string());
            item.error = Some(
                "The engine stopped, and this item's password or link token is not kept on disk. \
                 Add it again."
                    .to_string(),
            );
            item.finished_at_ms = Some(crate::now_ms());
            changed = true;
        }
        if was == Status::Running && book.items[at].stop_on_failure {
            book.hold_later_items(at);
        }
    }
    changed
}

impl QueueState {
    fn build(path: Option<PathBuf>, probe: Probe, timing: Timing) -> Self {
        let mut book = Book::default();
        if let Some(path) = &path {
            book.items = read_stored(path);
        }
        let changed = after_restart(&mut book);
        let queue = Self(Arc::new(Inner {
            path,
            book: Mutex::new(book),
            api: OnceLock::new(),
            installs: OnceLock::new(),
            probe,
            timing,
            written: Mutex::new(0),
        }));
        if changed {
            queue.save_now();
        }
        queue
    }

    /// The queue of the running engine: its file under the data folder.
    pub fn from_env() -> Self {
        // A test must never read or write the user's queue: tests name their own file.
        let path = if cfg!(test) {
            None
        } else {
            crate::remote::store::data_dir().map(|dir| dir.join(FILE))
        };
        let probe: Probe = Arc::new(|addr| Box::pin(helper_answers(addr)));
        Self::build(path, probe, Timing::default())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        path: Option<PathBuf>,
        answers: Arc<std::sync::atomic::AtomicBool>,
        timing: Timing,
    ) -> Self {
        let probe: Probe = Arc::new(move |_| {
            let answers = answers.clone();
            Box::pin(async move { answers.load(std::sync::atomic::Ordering::SeqCst) })
        });
        Self::build(path, probe, timing)
    }

    /// Items go through this router instead of the engine's own.
    #[cfg(test)]
    pub(crate) fn use_api(&self, api: Router) {
        let _ = self.0.api.set(api);
    }

    /// Gives the queue the engine's API and starts what was waiting.
    pub(crate) fn attach(&self, api: Router, installs: PkgInstallStateHandle) {
        let _ = self.0.api.set(api);
        let _ = self.0.installs.set(installs);
        if tokio::runtime::Handle::try_current().is_ok() {
            let consoles: HashSet<String> = self
                .book()
                .items
                .iter()
                .filter(|item| item.status == Status::Queued)
                .map(|item| item.console.clone())
                .collect();
            for console in consoles {
                self.kick(&console);
            }
        }
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.0.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snapshot(&self) -> Option<(u64, Vec<u8>)> {
        self.0.path.as_ref()?;
        let mut book = self.book();
        book.generation += 1;
        let items = book.items.iter().map(|item| json!(item)).collect();
        let bytes = serde_json::to_vec_pretty(&Stored { version: 1, items }).ok()?;
        Some((book.generation, bytes))
    }

    fn write_snapshot(inner: &Inner, generation: u64, bytes: &[u8]) {
        let Some(path) = &inner.path else { return };
        let mut written = inner.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written >= generation {
            return;
        }
        match write_atomic(path, bytes) {
            Ok(()) => *written = generation,
            Err(e) => crate::log_warn!(
                "queue: {}",
                crate::state_io::describe("cannot save the queue", path, &e)
            ),
        }
    }

    fn save_now(&self) {
        if let Some((generation, bytes)) = self.snapshot() {
            Self::write_snapshot(&self.0, generation, &bytes);
        }
    }

    async fn save(&self) {
        let Some((generation, bytes)) = self.snapshot() else {
            return;
        };
        let inner = self.0.clone();
        let _ = tokio::task::spawn_blocking(move || {
            Self::write_snapshot(&inner, generation, &bytes);
        })
        .await;
    }

    /// Tells `/api/events`.
    fn announce(&self, action: &'static str, id: &str) {
        let body = {
            let book = self.book();
            let Some(at) = book.find(id) else { return };
            json!({ "action": action, "item": book.view(&book.items[at]) })
        };
        bus().publish_json("queue", &body);
    }

    fn set_waiting_for(&self, console: &str, why: Option<WaitingFor>) {
        let changed = {
            let mut book = self.book();
            let old = match why {
                Some(why) => book.waiting_for.insert(console.to_string(), why),
                None => book.waiting_for.remove(console),
            };
            (old != why).then(|| book.console_view(console))
        };
        if let Some(view) = changed {
            bus().publish_json("queue", &json!({ "action": "console", "console": view }));
        }
    }

    /// Makes sure a console's line is being worked on.
    fn kick(&self, console: &str) {
        if !self.book().drivers.insert(console.to_string()) {
            return;
        }
        let queue = self.clone();
        let console = console.to_string();
        tokio::spawn(async move {
            let mut ending = DriverEnd {
                queue: queue.clone(),
                console: console.clone(),
                finished: false,
            };
            queue.drive(&console).await;
            ending.finished = true;
        });
    }

    async fn drive(&self, console: &str) {
        loop {
            match self.next_step(console).await {
                Step::Stop => return,
                Step::Wait(why) => {
                    self.set_waiting_for(console, why);
                    tokio::time::sleep(self.0.timing.tick).await;
                }
                Step::Run(id) => {
                    self.set_waiting_for(console, None);
                    self.announce("started", &id);
                    self.save().await;
                    self.run_item(&id).await;
                }
            }
        }
    }

    /// Asked only as an item is about to start, and while one waits. A helper too old to say, or
    /// that doesn't answer this, stops nothing: the probe above is what says it is there.
    async fn a_task_runs_on(&self, console: &str) -> bool {
        let path = format!(
            "/api/ps5/console-tasks?addr={}",
            crate::mcp::encode_component(console)
        );
        let (status, answer) = self.call(Method::GET, &path, None).await;
        status.is_success() && tasks_say_running(&answer)
    }

    fn console_is_in_use(&self, console: &str) -> bool {
        crate::helper_stop::is_busy(console)
            || self
                .0
                .installs
                .get()
                .is_some_and(|installs| installs.jobs.active_for(console))
    }

    async fn next_step(&self, console: &str) -> Step {
        let head = {
            let mut book = self.book();
            let Some(head) = book.next_on(console).map(|item| item.id.clone()) else {
                // Under the lock: an item added from now on starts a new driver.
                book.drivers.remove(console);
                book.waiting_for.remove(console);
                return Step::Stop;
            };
            if !book.holders(console).is_empty() {
                return Step::Wait(Some(WaitingFor::Hold));
            }
            let settled = book
                .last_ended
                .get(console)
                .is_none_or(|ended| ended.elapsed() >= self.0.timing.settle);
            if !settled {
                return Step::Wait(Some(WaitingFor::Settle));
            }
            head
        };
        if self.console_is_in_use(console) {
            return Step::Wait(Some(WaitingFor::OtherWork));
        }
        if !(self.0.probe)(console.to_string()).await {
            return Step::Wait(Some(WaitingFor::Console));
        }
        if self.a_task_runs_on(console).await {
            return Step::Wait(Some(WaitingFor::ConsoleTask));
        }
        let mut book = self.book();
        // The probe took a moment: the line or the holds may have changed.
        if book.next_on(console).map(|item| item.id.as_str()) != Some(head.as_str())
            || !book.holders(console).is_empty()
        {
            return Step::Wait(None);
        }
        let Some(at) = book.find(&head) else {
            return Step::Wait(None);
        };
        let item = &mut book.items[at];
        item.status = Status::Running;
        item.started_at_ms = Some(crate::now_ms());
        item.finished_at_ms = None;
        item.job_id = None;
        item.error = None;
        item.code = None;
        item.verdict = None;
        item.hint = None;
        item.progress = None;
        item.runs += 1;
        Step::Run(head)
    }

    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> (StatusCode, Value) {
        let Some(api) = self.0.api.get() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "the engine's API is not ready" }),
            );
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1");
        let request_body = match body {
            Some(json) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let Ok(mut request) = builder.body(request_body) else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "the request could not be made" }),
            );
        };
        // As a caller on this computer, which a route that asks is told.
        let peer = SocketAddr::from(([127, 0, 0, 1], 0));
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = api
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
        let status = response.status();
        let bytes = to_bytes(response.into_body(), MAX_REPLY_BYTES)
            .await
            .unwrap_or_default();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Ends an item and says so. An item someone asked to remove goes away instead.
    async fn finish(&self, id: &str, end: End) {
        let (removed, console) = {
            let mut book = self.book();
            let Some(at) = book.find(id) else { return };
            let intent = book.items[at].intent.take();
            let console = book.items[at].console.clone();
            book.last_ended.insert(console.clone(), Instant::now());
            let stopped = end.status == Status::Cancelled;
            // However the run ended: the client was told it is going, and a run that got to its end
            // before the cancel reached it, or a route that refused, changes nothing about that.
            let removed = intent == Some(Intent::Remove);
            if removed {
                let view = book.view(&book.items[at]);
                book.items.remove(at);
                bus().publish_json("queue", &json!({ "action": "removed", "item": view }));
            } else {
                let item = &mut book.items[at];
                item.status = if stopped && intent == Some(Intent::Pause) {
                    Status::Paused
                } else {
                    end.status
                };
                item.finished_at_ms = Some(crate::now_ms());
                item.error = end.error;
                item.code = end.code;
                item.verdict = end.verdict;
                item.hint = end.hint;
                let holds = item.stop_on_failure
                    && matches!(item.status, Status::Failed | Status::Interrupted);
                if holds {
                    book.hold_later_items(at);
                }
            }
            book.prune(&console);
            (removed, console)
        };
        if !removed {
            self.announce("ended", id);
        }
        crate::log_info!("queue: {console} item {id} ended (removed: {removed})");
        self.save().await;
    }

    async fn run_item(&self, id: &str) {
        let start = {
            let book = self.book();
            book.find(id).map(|at| {
                let item = &book.items[at];
                (item.kind, with_secrets(item))
            })
        };
        let Some((kind, body)) = start else { return };
        let (status, reply) = self.call(Method::POST, kind.route(), Some(&body)).await;
        if kind == ItemKind::Install
            && status == StatusCode::CONFLICT
            && reply.get("error").and_then(Value::as_str) == Some("busy")
        {
            // Another install took the console first: back in line, ahead of the rest.
            {
                let mut book = self.book();
                if let Some(at) = book.find(id) {
                    let item = &mut book.items[at];
                    item.status = Status::Queued;
                    item.started_at_ms = None;
                    item.runs = item.runs.saturating_sub(1);
                    let console = item.console.clone();
                    book.last_ended.insert(console, Instant::now());
                }
            }
            self.announce("requeued", id);
            self.save().await;
            return;
        }
        if !status.is_success() {
            self.finish(id, End::refused(status, &reply)).await;
            return;
        }
        let field = if kind == ItemKind::Install {
            "job"
        } else {
            "job_id"
        };
        let Some(job) = reply.get(field).and_then(Value::as_str).map(str::to_string) else {
            let end = End::failed(
                format!(
                    "{} answered without saying which job it started.",
                    kind.route()
                ),
                "no_job_id".to_string(),
            );
            self.finish(id, end).await;
            return;
        };
        {
            let mut book = self.book();
            if let Some(at) = book.find(id) {
                book.items[at].job_id = Some(job.clone());
            }
        }
        self.announce("job_started", id);
        self.save().await;
        let end = self.follow(id, kind, &job).await;
        self.finish(id, end).await;
    }

    /// Follows the job to its end.
    async fn follow(&self, id: &str, kind: ItemKind, job: &str) -> End {
        let path = format!("/api/jobs/{job}");
        let mut stop_sent = false;
        loop {
            let wants_stop = {
                let book = self.book();
                book.find(id)
                    .is_some_and(|at| book.items[at].intent.is_some())
            };
            if wants_stop && !stop_sent {
                let cancel = format!("/api/jobs/{job}/cancel");
                self.call(Method::POST, &cancel, None).await;
                stop_sent = true;
            }
            let (status, state) = self.call(Method::GET, &path, None).await;
            if !status.is_success() {
                return End {
                    status: Status::Interrupted,
                    error: Some(format!("Job {job} can no longer be found.")),
                    code: Some("job_lost".to_string()),
                    verdict: None,
                    hint: None,
                };
            }
            let end = if kind == ItemKind::Install {
                install_end(&state)
            } else {
                job_end(&state)
            };
            // The last figures too: a job can end between two looks.
            self.note_progress(id, kind, &state);
            if let Some(end) = end {
                return end;
            }
            tokio::time::sleep(self.0.timing.tick).await;
        }
    }

    fn note_progress(&self, id: &str, kind: ItemKind, state: &Value) {
        let names: &[&str] = if kind == ItemKind::Install {
            &["phase", "metrics"]
        } else {
            &[
                "bytes_sent",
                "total_bytes",
                "resumed_bytes",
                "files_sent",
                "files_count",
                "phase",
                "stage",
            ]
        };
        let mut progress = serde_json::Map::new();
        for name in names {
            if let Some(value) = state.get(*name) {
                progress.insert((*name).to_string(), value.clone());
            }
        }
        let mut book = self.book();
        if let Some(at) = book.find(id) {
            book.items[at].progress = Some(Value::Object(progress));
        }
    }
}

enum Step {
    Stop,
    Wait(Option<WaitingFor>),
    Run(String),
}

/// A driver that does not reach its end (a panic, the runtime shutting down) gives its console up
/// and leaves its running item with nobody following it: that item is interrupted, as after a
/// crash. One that ended normally gave the console up in [`QueueState::next_step`], and a new driver
/// may already own it.
struct DriverEnd {
    queue: QueueState,
    console: String,
    finished: bool,
}

impl Drop for DriverEnd {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut orphaned = None;
        {
            let mut book = self.queue.book();
            book.drivers.remove(&self.console);
            let found = book
                .items
                .iter()
                .position(|item| item.console == self.console && item.status == Status::Running);
            if let Some(at) = found {
                let item = &mut book.items[at];
                item.status = Status::Interrupted;
                item.code = Some("interrupted".to_string());
                item.error = Some("The queue stopped following this item.".to_string());
                item.finished_at_ms = Some(crate::now_ms());
                item.intent = None;
                orphaned = Some(item.id.clone());
            }
        }
        if let Some(id) = orphaned {
            self.queue.announce("ended", &id);
            self.queue.save_now();
        }
    }
}

/// How a run ended.
struct End {
    status: Status,
    error: Option<String>,
    code: Option<String>,
    verdict: Option<String>,
    hint: Option<String>,
}

impl End {
    fn with_status(status: Status) -> Self {
        End {
            status,
            error: None,
            code: None,
            verdict: None,
            hint: None,
        }
    }

    fn failed(error: String, code: String) -> Self {
        End {
            error: Some(error),
            code: Some(code),
            ..End::with_status(Status::Failed)
        }
    }

    /// The route refused to start.
    fn refused(status: StatusCode, reply: &Value) -> Self {
        let error = reply
            .get("error")
            .and_then(Value::as_str)
            .map_or_else(|| format!("the route answered {status}"), str::to_string);
        let code = reply
            .get("code")
            .or_else(|| reply.get("reason"))
            .and_then(Value::as_str)
            .map_or_else(
                || crate::api_errors::code_for_status(status),
                str::to_string,
            );
        End::failed(error, code)
    }
}

/// A transfer job's end, from its snapshot. `None` while it runs.
fn job_end(state: &Value) -> Option<End> {
    let field = |name: &str| state.get(name).and_then(Value::as_str);
    match field("status")? {
        "done" => Some(End::with_status(Status::Done)),
        "failed" => {
            let error = field("error").unwrap_or("the job failed");
            if error == "transfer_cancelled" {
                return Some(End {
                    code: Some("cancelled".to_string()),
                    ..End::with_status(Status::Cancelled)
                });
            }
            Some(End::failed(
                field("error_detail").unwrap_or(error).to_string(),
                field("error_reason").unwrap_or("job_failed").to_string(),
            ))
        }
        _ => None,
    }
}

/// An install's end, from its status. `None` while it runs.
fn install_end(state: &Value) -> Option<End> {
    let field = |name: &str| state.get(name).and_then(Value::as_str);
    let hint = field("hint").map(str::to_string);
    let verdict = field("verdict").map(str::to_string);
    // An update that did not apply ends in phase `done` with verdict `failed`: it failed.
    let phase = match field("phase")? {
        "done" if verdict.as_deref() == Some("failed") => "failed",
        phase => phase,
    };
    match phase {
        "done" => Some(End {
            verdict,
            hint,
            ..End::with_status(Status::Done)
        }),
        "failed" => {
            let reason = field("reason").unwrap_or("install_failed");
            if reason == "cancelled" {
                return Some(End {
                    code: Some("cancelled".to_string()),
                    hint,
                    ..End::with_status(Status::Cancelled)
                });
            }
            Some(End {
                error: Some(
                    hint.clone()
                        .unwrap_or_else(|| "the install failed".to_string()),
                ),
                code: Some(reason.to_string()),
                verdict,
                hint,
                ..End::with_status(Status::Failed)
            })
        }
        _ => None,
    }
}

// ─── Routes ──────────────────────────────────────────────────────────────────

fn fail(status: StatusCode, code: &str, error: impl Into<String>) -> Response {
    (status, Json(json!({ "error": error.into(), "code": code }))).into_response()
}

fn no_such_item(id: &str) -> Response {
    fail(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no queue item {id}"),
    )
}

/// The console's address and the key its line goes by.
fn named_console(console: Option<String>) -> Option<(String, String)> {
    let addr = crate::required_console_addr(console)?;
    let key = crate::install::console_id(&addr).to_ascii_lowercase();
    (!key.is_empty()).then_some((addr, key))
}

fn no_console() -> Response {
    fail(
        StatusCode::BAD_REQUEST,
        "missing_console",
        "console is required: say which console this acts on",
    )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default, alias = "addr", alias = "host")]
    console: Option<String>,
}

/// GET /api/queue?console= — one console's line, or every console's.
async fn list_handler(State(queue): State<QueueState>, Query(q): Query<ListQuery>) -> Response {
    let wanted = q
        .console
        .filter(|c| !c.trim().is_empty())
        .map(|c| named_console(Some(c)));
    let wanted = match wanted {
        Some(None) => return no_console(),
        Some(Some((_, key))) => Some(key),
        None => None,
    };
    let mut book = queue.book();
    let mut consoles: Vec<String> = book.items.iter().map(|i| i.console.clone()).collect();
    consoles.extend(book.holds.keys().cloned());
    consoles.sort();
    consoles.dedup();
    if let Some(key) = &wanted {
        consoles.retain(|console| console == key);
        if consoles.is_empty() {
            consoles.push(key.clone());
        }
    }
    let items: Vec<Value> = book
        .items
        .iter()
        .filter(|item| wanted.as_ref().is_none_or(|key| &item.console == key))
        .map(|item| book.view(item))
        .collect();
    let consoles: Vec<Value> = consoles.iter().map(|c| book.console_view(c)).collect();
    Json(json!({ "items": items, "consoles": consoles })).into_response()
}

#[derive(Deserialize)]
struct AddBody {
    #[serde(default, alias = "addr", alias = "host")]
    console: Option<String>,
    kind: String,
    /// The body of the kind's route. Its console is this item's.
    #[serde(default)]
    request: Value,
    #[serde(default)]
    label: Option<String>,
    /// Among the console's waiting items, from 0. Left out, the end of the line.
    #[serde(default)]
    position: Option<usize>,
    /// If this item fails, the ones after it are paused for someone to look at.
    #[serde(default)]
    stop_on_failure: bool,
}

/// POST /api/queue — adds an item. 201 with the item.
async fn add_handler(State(queue): State<QueueState>, Json(body): Json<AddBody>) -> Response {
    let Some((addr, console)) = named_console(body.console) else {
        return no_console();
    };
    let Some(kind) = ItemKind::parse(&body.kind) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "unknown_kind",
            format!("kind cannot be {}: use one of {KIND_NAMES}", body.kind),
        );
    };
    let Value::Object(mut fields) = body.request else {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!(
                "request must be an object: the body of {} without the console",
                kind.route()
            ),
        );
    };
    // The item's console is the only one: an alias left in would make the field a duplicate.
    for alias in ["addr", "console", "host", "ps5_addr"] {
        fields.remove(alias);
    }
    fields.insert(kind.console_field().to_string(), json!(addr));
    if kind.resumes() && fields.get("tx_id").is_none_or(Value::is_null) {
        // Its own id from the start, so a pause or a retry picks up what already landed.
        fields.insert(
            "tx_id".to_string(),
            json!(uuid::Uuid::new_v4().simple().to_string()),
        );
    }
    let mut request = Value::Object(fields);
    if let Err(problem) = kind.check(&request) {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("request does not fit {}: {problem}", kind.route()),
        );
    }
    if let Some(tx_id) = request.get("tx_id").and_then(Value::as_str) {
        if let Err(problem) = crate::parse_or_random_tx_id(Some(tx_id)) {
            return fail(StatusCode::BAD_REQUEST, "bad_request", problem.to_string());
        }
    }
    if request.to_string().len() > MAX_REQUEST_BYTES {
        return fail(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            format!(
                "this request is over {} MiB, too large to keep in the queue: call {} directly",
                MAX_REQUEST_BYTES / (1024 * 1024),
                kind.route()
            ),
        );
    }
    let secrets = take_secrets(kind, &mut request);
    let label = body
        .label
        .map(|label| {
            label
                .trim()
                .chars()
                .take(MAX_LABEL_CHARS)
                .collect::<String>()
        })
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| default_label(kind, &request));
    let id = format!("q-{}", uuid::Uuid::new_v4().simple());
    let item = Item {
        id: id.clone(),
        console: console.clone(),
        kind,
        label,
        status: Status::Queued,
        request,
        stop_on_failure: body.stop_on_failure,
        added_at_ms: crate::now_ms(),
        started_at_ms: None,
        finished_at_ms: None,
        job_id: None,
        runs: 0,
        error: None,
        code: None,
        verdict: None,
        hint: None,
        progress: None,
        secret_in_memory: !secrets.is_empty(),
        secrets,
        intent: None,
    };
    let view = {
        let mut book = queue.book();
        let waiting = book
            .items
            .iter()
            .filter(|other| other.console == console && other.status.waiting())
            .count();
        if waiting >= MAX_WAITING {
            return fail(
                StatusCode::CONFLICT,
                "queue_full",
                format!("{console} already has {MAX_WAITING} items waiting"),
            );
        }
        book.items.push(item);
        if let Some(position) = body.position {
            let at = book.items.len() - 1;
            book.place(at, position);
        }
        let at = book.find(&id).unwrap_or(0);
        book.view(&book.items[at])
    };
    queue.announce("added", &id);
    queue.save().await;
    queue.kick(&console);
    (StatusCode::CREATED, Json(view)).into_response()
}

/// GET /api/queue/{id}
async fn get_handler(State(queue): State<QueueState>, Path(id): Path<String>) -> Response {
    let book = queue.book();
    match book.find(&id) {
        Some(at) => Json(book.view(&book.items[at])).into_response(),
        None => no_such_item(&id),
    }
}

/// POST /api/queue/{id}/pause — a waiting item is skipped until resumed. A running upload stops
/// the way a cancel does, what landed stays, and `resume` picks up from it. An install or a
/// download cannot pause once running.
async fn pause_handler(State(queue): State<QueueState>, Path(id): Path<String>) -> Response {
    let view = {
        let mut book = queue.book();
        let Some(at) = book.find(&id) else {
            return no_such_item(&id);
        };
        let item = &mut book.items[at];
        match item.status {
            Status::Queued => {
                item.status = Status::Paused;
            }
            Status::Paused => {}
            Status::Running if item.kind == ItemKind::Install => {
                return fail(
                    StatusCode::CONFLICT,
                    "install_cannot_pause",
                    "a running install cannot pause: the console goes on with it",
                );
            }
            Status::Running if !item.kind.resumes() => {
                return fail(
                    StatusCode::CONFLICT,
                    "cannot_pause",
                    "a running download cannot pause: remove it with cancel to stop it",
                );
            }
            Status::Running => {
                if item.intent == Some(Intent::Remove) {
                    return fail(
                        StatusCode::CONFLICT,
                        "item_stopping",
                        "this item is already being removed",
                    );
                }
                // It says `paused` once its job has stopped.
                item.intent = Some(Intent::Pause);
            }
            _ => {
                return fail(
                    StatusCode::CONFLICT,
                    "item_finished",
                    "this item has finished: there is nothing to pause",
                );
            }
        }
        book.view(&book.items[at])
    };
    queue.announce("paused", &id);
    queue.save().await;
    Json(view).into_response()
}

/// POST /api/queue/{id}/resume — a paused item waits for its turn again. So does a failed,
/// cancelled or interrupted one: this is the only way an item runs a second time.
async fn resume_handler(State(queue): State<QueueState>, Path(id): Path<String>) -> Response {
    let (view, console) = {
        let mut book = queue.book();
        let Some(at) = book.find(&id) else {
            return no_such_item(&id);
        };
        let item = &mut book.items[at];
        match item.status {
            Status::Paused | Status::Failed | Status::Cancelled | Status::Interrupted => {
                if item.secret_in_memory && item.secrets.is_empty() {
                    return fail(
                        StatusCode::CONFLICT,
                        "secret_not_kept",
                        "this item's password or link token was not kept over a restart: add it \
                         again",
                    );
                }
                item.status = Status::Queued;
                item.error = None;
                item.code = None;
                item.finished_at_ms = None;
            }
            Status::Queued => {}
            Status::Running => {
                if item.intent.is_some() {
                    return fail(
                        StatusCode::CONFLICT,
                        "item_stopping",
                        "this item is stopping: resume it once it says paused",
                    );
                }
            }
            Status::Done => {
                return fail(
                    StatusCode::CONFLICT,
                    "item_finished",
                    "this item has finished: add it again to run it again",
                );
            }
        }
        let console = item.console.clone();
        (book.view(&book.items[at]), console)
    };
    queue.announce("resumed", &id);
    queue.save().await;
    queue.kick(&console);
    Json(view).into_response()
}

#[derive(Default, Deserialize)]
struct RemoveBody {
    /// Also stop the item's running upload or download.
    #[serde(default)]
    cancel: bool,
}

/// POST /api/queue/{id}/remove — drops an item that is not running. A running upload or download
/// needs `cancel: true`: it stops and goes (202). A running install is never removed: the console
/// goes on with it whatever the queue says.
async fn remove_handler(
    State(queue): State<QueueState>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    // The body may be left out.
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        RemoveBody::default()
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(body) => body,
            Err(e) => {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    format!("bad body: {e}"),
                )
            }
        }
    };
    let view = {
        let mut book = queue.book();
        let Some(at) = book.find(&id) else {
            return no_such_item(&id);
        };
        if book.items[at].status == Status::Running {
            if book.items[at].kind == ItemKind::Install {
                return fail(
                    StatusCode::CONFLICT,
                    "install_running",
                    "a running install cannot be removed: POST /api/pkg/install/stop stops \
                     following it, then the item can go",
                );
            }
            if !body.cancel {
                return fail(
                    StatusCode::CONFLICT,
                    "item_running",
                    "this item is running: send {\"cancel\": true} to stop it and remove it",
                );
            }
            book.items[at].intent = Some(Intent::Remove);
            let view = book.view(&book.items[at]);
            drop(book);
            return (
                StatusCode::ACCEPTED,
                Json(json!({ "removing": true, "item": view })),
            )
                .into_response();
        }
        let view = book.view(&book.items[at]);
        book.items.remove(at);
        view
    };
    bus().publish_json("queue", &json!({ "action": "removed", "item": view }));
    queue.save().await;
    Json(json!({ "removed": true, "id": id })).into_response()
}

#[derive(Deserialize)]
struct MoveBody {
    /// Among the console's waiting items, from 0. Past the end means the end.
    position: usize,
}

/// POST /api/queue/{id}/move {position} — a new place among the waiting items.
async fn move_handler(
    State(queue): State<QueueState>,
    Path(id): Path<String>,
    Json(body): Json<MoveBody>,
) -> Response {
    let view = {
        let mut book = queue.book();
        let Some(at) = book.find(&id) else {
            return no_such_item(&id);
        };
        if !book.items[at].status.waiting() {
            return fail(
                StatusCode::CONFLICT,
                "not_waiting",
                "only an item that is waiting can move",
            );
        }
        book.place(at, body.position);
        let at = book.find(&id).unwrap_or(0);
        book.view(&book.items[at])
    };
    queue.announce("moved", &id);
    queue.save().await;
    Json(view).into_response()
}

#[derive(Deserialize)]
struct HoldBody {
    #[serde(default, alias = "addr", alias = "host")]
    console: Option<String>,
    holder: String,
    #[serde(default)]
    ttl_s: Option<u64>,
}

/// POST /api/queue/hold {console, holder, ttl_s?} — a client runs work of its own on this console:
/// the queue starts nothing there until the hold lapses or is released. Refused with 409
/// `queue_running` while a queue item runs there; the client waits and asks again. Asking again
/// also keeps a hold going.
async fn hold_handler(State(queue): State<QueueState>, Json(body): Json<HoldBody>) -> Response {
    let Some((_, console)) = named_console(body.console) else {
        return no_console();
    };
    let holder = body.holder.trim();
    if holder.is_empty() || holder.chars().count() > MAX_HOLDER_CHARS {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("holder must be 1 to {MAX_HOLDER_CHARS} characters"),
        );
    }
    let ttl = body.ttl_s.unwrap_or(DEFAULT_HOLD_S).clamp(1, MAX_HOLD_S);
    let held = {
        let mut book = queue.book();
        // A holder keeping its hold going while an item runs: only possible if its hold lapsed.
        if let Some(item) = book.running_on(&console) {
            let view = book.view(item);
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "the engine's queue is running an item on this console",
                    "code": "queue_running",
                    "item": view,
                })),
            )
                .into_response();
        }
        book.holds.entry(console.clone()).or_default().insert(
            holder.to_string(),
            Instant::now() + Duration::from_secs(ttl),
        );
        book.holders(&console)
    };
    Json(json!({ "held": true, "console": console, "ttl_s": ttl, "holders": held })).into_response()
}

#[derive(Deserialize)]
struct ReleaseBody {
    #[serde(default, alias = "addr", alias = "host")]
    console: Option<String>,
    holder: String,
}

/// POST /api/queue/release {console, holder} — the hold ends now.
async fn release_handler(
    State(queue): State<QueueState>,
    Json(body): Json<ReleaseBody>,
) -> Response {
    let Some((_, console)) = named_console(body.console) else {
        return no_console();
    };
    let released = {
        let mut book = queue.book();
        let released = book
            .holds
            .get_mut(&console)
            .is_some_and(|holds| holds.remove(body.holder.trim()).is_some());
        book.holders(&console);
        released
    };
    Json(json!({ "released": released, "console": console })).into_response()
}

pub fn router<S: Clone + Send + Sync + 'static>(queue: QueueState) -> Router<S> {
    Router::new()
        .route("/api/queue", get(list_handler).post(add_handler))
        .route("/api/queue/hold", post(hold_handler))
        .route("/api/queue/release", post(release_handler))
        .route("/api/queue/{id}", get(get_handler))
        .route("/api/queue/{id}/pause", post(pause_handler))
        .route("/api/queue/{id}/resume", post(resume_handler))
        .route("/api/queue/{id}/remove", post(remove_handler))
        .route("/api/queue/{id}/move", post(move_handler))
        .with_state(queue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_link_can_hide_stays_out_of_the_kept_request() {
        let (plain, had) = url_without_secret("https://user:pw@cdn.example/a/b.pkg?token=x#f");
        assert_eq!(plain, "https://cdn.example/a/b.pkg?...");
        assert!(had);
        assert_eq!(
            url_without_secret("http://nas.local/b.pkg"),
            ("http://nas.local/b.pkg".to_string(), false)
        );
        // An @ in the path names no user.
        assert_eq!(
            url_without_secret("http://h/@me/b.pkg"),
            ("http://h/@me/b.pkg".to_string(), false)
        );
    }

    #[test]
    fn secrets_go_back_in_when_the_route_is_called() {
        let mut body = json!({
            "source": { "url": "https://h/a.pkg?sig=1" },
            "ps5_addr": "10.0.0.2",
        });
        let secrets = take_secrets(ItemKind::Install, &mut body);
        assert_eq!(body["source"]["url"], "https://h/a.pkg?...");
        assert_eq!(default_label(ItemKind::Install, &body), "a.pkg");
        let item = test_item(ItemKind::Install, body, secrets);
        assert_eq!(
            with_secrets(&item)["source"]["url"],
            "https://h/a.pkg?sig=1"
        );
        let stored = serde_json::to_string(&item).unwrap();
        assert!(!stored.contains("sig=1"), "{stored}");

        let mut body =
            json!({ "archive_path": "/a.rar", "dest_root": "/d", "password": "hunter2" });
        let secrets = take_secrets(ItemKind::UploadRar, &mut body);
        assert!(body.get("password").is_none());
        let item = test_item(ItemKind::UploadRar, body, secrets);
        assert_eq!(with_secrets(&item)["password"], "hunter2");
        let stored = serde_json::to_string(&item).unwrap();
        assert!(!stored.contains("hunter2"), "{stored}");
    }

    fn test_item(kind: ItemKind, request: Value, secrets: Secrets) -> Item {
        Item {
            id: "q-1".into(),
            console: "10.0.0.2".into(),
            kind,
            label: String::new(),
            status: Status::Queued,
            request,
            stop_on_failure: false,
            added_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            job_id: None,
            runs: 0,
            error: None,
            code: None,
            verdict: None,
            hint: None,
            progress: None,
            secret_in_memory: !secrets.is_empty(),
            secrets,
            intent: None,
        }
    }

    #[test]
    fn ends_read_from_a_job_and_an_install() {
        assert!(job_end(&json!({ "status": "running" })).is_none());
        assert_eq!(
            job_end(&json!({ "status": "done" })).unwrap().status,
            Status::Done
        );
        let failed = job_end(&json!({
            "status": "failed",
            "error": "raw",
            "error_reason": "ava1_refused_7",
            "error_detail": "no room",
        }))
        .unwrap();
        assert_eq!(failed.status, Status::Failed);
        assert_eq!(failed.code.as_deref(), Some("ava1_refused_7"));
        assert_eq!(failed.error.as_deref(), Some("no room"));
        assert_eq!(
            job_end(&json!({ "status": "failed", "error": "transfer_cancelled" }))
                .unwrap()
                .status,
            Status::Cancelled
        );

        assert!(install_end(&json!({ "phase": "deliver" })).is_none());
        let done = install_end(&json!({ "phase": "done", "verdict": "may_not_launch" })).unwrap();
        assert_eq!(done.status, Status::Done);
        assert_eq!(done.verdict.as_deref(), Some("may_not_launch"));
        let failed = install_end(
            &json!({ "phase": "failed", "verdict": "failed", "reason": "stalled", "hint": "h" }),
        )
        .unwrap();
        assert_eq!(failed.status, Status::Failed);
        assert_eq!(failed.code.as_deref(), Some("stalled"));
        assert_eq!(failed.error.as_deref(), Some("h"));
        assert_eq!(
            install_end(&json!({ "phase": "failed", "reason": "cancelled" }))
                .unwrap()
                .status,
            Status::Cancelled
        );
    }

    #[test]
    fn a_restart_interrupts_what_ran_and_keeps_what_waited() {
        let mut running = test_item(ItemKind::UploadFile, json!({}), Secrets::default());
        running.status = Status::Running;
        running.stop_on_failure = true;
        let mut queued = test_item(ItemKind::UploadFile, json!({}), Secrets::default());
        queued.id = "q-2".into();
        let mut with_token = test_item(ItemKind::Install, json!({}), Secrets::default());
        with_token.id = "q-3".into();
        with_token.console = "10.0.0.3".into();
        with_token.secret_in_memory = true;
        let mut book = Book {
            items: vec![running, queued, with_token],
            ..Book::default()
        };
        assert!(after_restart(&mut book));
        assert_eq!(book.items[0].status, Status::Interrupted);
        // Behind a stop_on_failure item that did not finish: held, not run.
        assert_eq!(book.items[1].status, Status::Paused);
        assert_eq!(book.items[2].status, Status::Interrupted);
        assert_eq!(book.items[2].code.as_deref(), Some("secret_not_kept"));
    }

    #[test]
    fn placing_counts_only_this_consoles_waiting_items() {
        let item = |id: &str, console: &str, status: Status| {
            let mut item = test_item(ItemKind::UploadFile, json!({}), Secrets::default());
            item.id = id.into();
            item.console = console.into();
            item.status = status;
            item
        };
        let mut book = Book {
            items: vec![
                item("done", "a", Status::Done),
                item("a1", "a", Status::Queued),
                item("b1", "b", Status::Queued),
                item("a2", "a", Status::Paused),
                item("a3", "a", Status::Queued),
            ],
            ..Book::default()
        };
        let at = book.find("a3").unwrap();
        book.place(at, 0);
        let order: Vec<&str> = book.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, ["done", "a3", "a1", "b1", "a2"]);
        let at = book.find("a3").unwrap();
        book.place(at, 99);
        let waiting = |book: &Book| {
            book.items
                .iter()
                .filter(|i| i.console == "a" && i.status.waiting())
                .map(|i| i.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(waiting(&book), ["a1", "a2", "a3"]);
        let a2 = book.find("a2").unwrap();
        assert_eq!(book.position(&book.items[a2]), Some(1));
    }
}
