//! This engine as one of the devices that use a console (8.0.0, `ps5upload_core::mesh`).
//!
//! * `GET  /api/mesh/me`                      -> `{id,name,kind,os,app,docker_volume}`
//! * `POST /api/mesh/me`        `{name}`      -> the same (renames this device everywhere)
//! * `GET  /api/mesh/state?addr=&since=&ev=&client=` -> the console's picture, or `{"same":true}`
//! * `POST /api/mesh/activity`  `{addr,client,items}` -> what one window of this app is doing
//! * `POST /api/mesh/lock`      `{addr,op,name,ttl?,force?,label?}`
//! * `POST /api/mesh/device`    `{addr,op,id,name?}`
//! * `POST /api/mesh/settings`  `{addr,require_approval?,prune_days?}`
//! * `GET  /api/mesh/doc?addr=&name=` / `POST /api/mesh/doc` `{addr,name,base,doc}`
//! * `POST /api/mesh/announce`  `{addr,kind,text?}`
//!
//! Each answer carries `supported`: false when the helper predates the mesh, and the app then
//! behaves as before. The engine says hello to a console before its first mesh call and again
//! every few minutes, and whenever the helper says it has not met us (a restarted helper).
//!
//! Presence: every window of this app (the desktop window, each browser on a Docker engine) posts
//! what it is doing; the engine sends the console the union, and drops a window that stopped
//! posting. On a Docker engine the number of browsers using it rides along as one more item.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::mesh::{self, Hello};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::json_err;

/// Say hello again after this long, so the console's "last seen" and our name stay current.
const HELLO_EVERY: Duration = Duration::from_secs(300);
/// A window that has not posted its activity or polled for this long is gone. Above a minute: a
/// browser throttles a hidden tab's timers to about once a minute, and a tab in the background is
/// still there.
const CLIENT_GONE: Duration = Duration::from_secs(75);
/// How often the engine looks at what the consoles should show, with or without a window.
const PRESENCE_TICK: Duration = Duration::from_secs(10);
/// After a console did not take the update, leave it this long.
const PRESENCE_RETRY: Duration = Duration::from_secs(60);

#[derive(Default)]
struct ConsoleState {
    hello_at: Option<Instant>,
    /// Per window: when it last spoke, and its items.
    clients: HashMap<String, (Instant, Vec<Value>)>,
    /// What the console last accepted from us.
    pushed: Option<Value>,
}

fn consoles() -> &'static Mutex<HashMap<String, ConsoleState>> {
    static C: OnceLock<Mutex<HashMap<String, ConsoleState>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn lock_consoles() -> std::sync::MutexGuard<'static, HashMap<String, ConsoleState>> {
    consoles().lock().unwrap_or_else(|e| e.into_inner())
}

/// The consoles this engine has greeted or been asked about since it started, sorted.
pub(crate) fn greeted_consoles() -> Vec<String> {
    let mut consoles: Vec<String> = lock_consoles().keys().cloned().collect();
    consoles.sort();
    consoles
}

/// The mesh for a bug report: what kind of device this engine is and, per console it greets,
/// how many app windows are active and whether the console took our presence. No names.
pub(crate) fn diag_summary() -> Value {
    let now = Instant::now();
    let consoles: Vec<Value> = lock_consoles()
        .values()
        .map(|c| {
            json!({
                "hello_s_ago": c.hello_at.map(|t| now.saturating_duration_since(t).as_secs()),
                "windows": c.clients.values().filter(|(t, _)| now.saturating_duration_since(*t) < CLIENT_GONE).count(),
                "presence_pushed": c.pushed.is_some(),
            })
        })
        .collect();
    json!({ "kind": kind(), "consoles": consoles })
}

// ---- who we are ----

pub fn kind() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if is_docker() {
        "docker"
    } else {
        "desktop"
    }
}

fn is_docker() -> bool {
    crate::pkg_install::in_container()
}

fn os_label() -> String {
    match std::env::consts::OS {
        "macos" => "macOS".into(),
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        "android" => "Android".into(),
        "ios" => "iOS".into(),
        other => other.into(),
    }
}

fn me_path() -> Option<PathBuf> {
    Some(crate::remote::store::data_dir()?.join("mesh-me.json"))
}

fn saved_name() -> Option<String> {
    let text = std::fs::read_to_string(me_path()?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let n = v.get("name")?.as_str()?.trim().to_string();
    (!n.is_empty()).then_some(n)
}

/// The computer's own name, without starting a program on a phone.
fn host_name() -> Option<String> {
    let clean = |s: String| {
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    if let Ok(n) = std::env::var("PS5UPLOAD_DEVICE_NAME") {
        if let Some(n) = clean(n) {
            return Some(n);
        }
    }
    if cfg!(target_os = "android") || cfg!(target_os = "ios") {
        return None;
    }
    if cfg!(target_os = "windows") {
        return std::env::var("COMPUTERNAME").ok().and_then(clean);
    }
    if cfg!(target_os = "macos") {
        if let Ok(o) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
        {
            if let Some(n) = clean(String::from_utf8_lossy(&o.stdout).into_owned()) {
                return Some(n);
            }
        }
    }
    if is_docker() {
        return None; // the container's host name is its id: say "Docker" instead
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(clean)
}

fn default_name() -> String {
    match kind() {
        "android" => "Android device".into(),
        "ios" => "iPhone or iPad".into(),
        "docker" => host_name().unwrap_or_else(|| "Docker".into()),
        _ => host_name().unwrap_or_else(|| format!("{} computer", os_label())),
    }
}

fn name() -> String {
    saved_name().unwrap_or_else(default_name)
}

/// This device's name, as the consoles list it.
pub(crate) fn device_name() -> String {
    name()
}

fn hello_info() -> Hello {
    Hello {
        name: name(),
        kind: kind().into(),
        os: os_label(),
        app: env!("CARGO_PKG_VERSION").into(),
    }
}

pub(crate) fn id_hex() -> Option<String> {
    let id = crate::ava1_api::identity()?;
    Some(id.public().iter().map(|b| format!("{b:02x}")).collect())
}

/// Docker only: whether the engine's data folder (/data, or wherever PS5UPLOAD_DATA_DIR points)
/// is on a volume the host keeps (a named volume or a folder), not the anonymous one Docker makes
/// when none was given (a new one, so a new device, on every update). None outside Docker or when
/// it cannot be told.
fn docker_volume_kept() -> Option<bool> {
    if kind() != "docker" {
        return None;
    }
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let dir = crate::remote::store::data_dir()?;
    Some(kept_mount(&info, &dir.to_string_lossy()))
}

/// Whether the mount holding `dir` (the longest mount point it is under) is a kept volume.
fn kept_mount(mountinfo: &str, dir: &str) -> bool {
    let under = |mp: &str| mp == "/" || dir == mp || dir.starts_with(&format!("{mp}/"));
    let best = mountinfo
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            (f.len() > 4 && under(f[4])).then(|| (f[4], f[3]))
        })
        .max_by_key(|(mp, _)| mp.len());
    match best {
        // The container's own disk: gone with the container.
        None | Some(("/", _)) => false,
        // Field 4 is the source path inside its filesystem. An anonymous volume is
        // .../volumes/<64 hex>/_data.
        Some((_, root)) => {
            !(root
                .split('/')
                .any(|p| p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()))
                && root.ends_with("/_data"))
        }
    }
}

#[cfg(test)]
mod volume_tests {
    use super::kept_mount;

    const INFO: &str = "1 0 0:1 / / rw - overlay overlay rw\n\
        2 1 0:2 /Users/me/ps5u/config /config rw - virtiofs x rw\n\
        3 1 0:3 /var/lib/docker/volumes/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/_data /data rw - ext4 y rw\n\
        4 1 0:4 /var/lib/docker/volumes/ps5u/_data /named rw - ext4 z rw";

    #[test]
    fn the_mount_holding_the_data_folder_decides() {
        assert!(kept_mount(INFO, "/config"));
        assert!(kept_mount(INFO, "/config/.ps5upload"));
        assert!(!kept_mount(INFO, "/data")); // anonymous volume
        assert!(!kept_mount(INFO, "/data/.ps5upload"));
        assert!(kept_mount(INFO, "/named/x"));
        assert!(!kept_mount(INFO, "/elsewhere")); // the container's own disk
        assert!(!kept_mount(INFO, "/configx")); // not under /config
    }
}

fn me_json() -> Value {
    json!({
        "id": id_hex(),
        "name": name(),
        "kind": kind(),
        "os": os_label(),
        "app": env!("CARGO_PKG_VERSION"),
        "docker_volume": docker_volume_kept(),
    })
}

pub async fn me_get() -> Response {
    let v = tokio::task::spawn_blocking(me_json)
        .await
        .unwrap_or(Value::Null);
    Json(v).into_response()
}

#[derive(Deserialize)]
pub struct MeSet {
    name: String,
}

pub async fn me_set(Json(req): Json<MeSet>) -> Response {
    let n: String = req.name.trim().chars().take(60).collect();
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let path = me_path().ok_or_else(|| anyhow::anyhow!("no data directory"))?;
        if n.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, serde_json::to_vec(&json!({ "name": n }))?)?;
        }
        // Every console hears the new name on our next call.
        for c in lock_consoles().values_mut() {
            c.hello_at = None;
        }
        Ok(me_json())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    answer_result(r)
}

// ---- calls ----

fn unsupported() -> Value {
    json!({ "supported": false })
}

fn with_supported(v: Option<Value>) -> Value {
    match v {
        None => unsupported(),
        Some(Value::Object(mut o)) => {
            o.insert("supported".into(), true.into());
            Value::Object(o)
        }
        Some(other) => json!({ "supported": true, "value": other }),
    }
}

/// Hello if due, then `f`; a helper that has not met us (it restarted) gets a hello and the call
/// once more. `Ok(None)`: the helper predates the mesh.
pub(crate) fn call(
    addr: &str,
    f: impl Fn() -> anyhow::Result<Option<Value>>,
) -> anyhow::Result<Option<Value>> {
    let due = lock_consoles()
        .get(addr)
        .and_then(|c| c.hello_at)
        .is_none_or(|t| t.elapsed() >= HELLO_EVERY);
    if due && !hello(addr)? {
        return Ok(None);
    }
    match f() {
        Err(e) if mesh::needs_hello(&e) => {
            if !hello(addr)? {
                return Ok(None);
            }
            f()
        }
        r => r,
    }
}

/// Says hello on a session that just opened (ps5upload_ava1::pool::on_new_session): any work
/// opens one, not only mesh calls, and a console lists a device that never said hello as an
/// app from before 8.0.0. Best effort: an old helper or a dropped session is tried again later.
pub(crate) fn greet(addr: &str) {
    if let Err(e) = hello(addr) {
        crate::log_debug!("mesh hello to {addr} on a new session: {e:#}");
    }
}

/// false: the helper predates the mesh.
fn hello(addr: &str) -> anyhow::Result<bool> {
    let r = mesh::hello(addr, &hello_info())?;
    let mut map = lock_consoles();
    let c = map.entry(addr.to_string()).or_default();
    c.hello_at = Some(Instant::now());
    // A restarted helper forgot our presence: send it again.
    c.pushed = None;
    Ok(r.is_some())
}

/// A transfer this engine runs against a console: what its window would say if it had one.
struct NotedJob {
    host: String,
    kind: &'static str,
    title: String,
}

fn noted_jobs() -> &'static Mutex<HashMap<uuid::Uuid, NotedJob>> {
    static NOTED: OnceLock<Mutex<HashMap<uuid::Uuid, NotedJob>>> = OnceLock::new();
    NOTED.get_or_init(Default::default)
}

/// A transfer started. It keeps running when the tab that asked for it closes, and the other
/// devices should still see it: the helper would otherwise be restarted under it.
pub(crate) fn note_job(job: uuid::Uuid, addr: &str, kind: &'static str, dest: &str) {
    let title: String = dest
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    noted_jobs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            job,
            NotedJob {
                host: ps5upload_core::events::console_host(addr),
                kind,
                title,
            },
        );
}

type JobMap = std::sync::Arc<Mutex<HashMap<uuid::Uuid, crate::JobState>>>;

fn job_map() -> &'static OnceLock<JobMap> {
    static JOBS: OnceLock<JobMap> = OnceLock::new();
    &JOBS
}

/// Percent to the nearest ten: a percent every few seconds would be a write to the console for
/// each, in the middle of the transfer it describes.
fn rough_pct(sent: u64, total: u64) -> Option<u64> {
    (total > 0).then(|| (sent.saturating_mul(10) / total).min(10) * 10)
}

/// The presence items for the transfers this engine runs against `addr`. Finished jobs are
/// forgotten here.
fn engine_job_items(addr: &str) -> Vec<Value> {
    let Some(jobs) = job_map().get() else {
        return Vec::new();
    };
    let host = ps5upload_core::events::console_host(addr);
    let jobs = jobs.lock().unwrap_or_else(|e| e.into_inner());
    let mut noted = noted_jobs().lock().unwrap_or_else(|e| e.into_inner());
    noted.retain(|id, _| matches!(jobs.get(id), Some(crate::JobState::Running { .. })));
    noted
        .iter()
        .filter(|(_, n)| n.host == host)
        .filter_map(|(id, n)| match jobs.get(id) {
            Some(crate::JobState::Running {
                bytes_sent,
                total_bytes,
                ..
            }) => Some(json!({
                "kind": n.kind,
                "title": n.title,
                "pct": rough_pct(*bytes_sent, *total_bytes),
            })),
            _ => None,
        })
        .collect()
}

/// The union of every live window's items (plus, on Docker, how many browsers use this engine), if
/// it differs from what the console has. With no window left, what this engine itself is running.
fn presence_due(addr: &str, now: Instant) -> Option<Value> {
    let own_jobs = engine_job_items(addr);
    let mut map = lock_consoles();
    let c = map.entry(addr.to_string()).or_default();
    c.clients
        .retain(|_, (t, _)| now.duration_since(*t) < CLIENT_GONE);
    let mut items: Vec<Value> = c.clients.values().flat_map(|(_, i)| i.clone()).collect();
    if c.clients.is_empty() {
        items.extend(own_jobs);
    }
    if kind() == "docker" && !c.clients.is_empty() {
        items.push(json!({ "kind": "browsers", "count": c.clients.len() }));
    }
    // The console keeps 1.5 KiB per device: the newest items first, cut to fit.
    let mut v = Value::Array(items);
    while serde_json::to_vec(&v).map_or(0, |b| b.len()) > 1400 {
        match &mut v {
            Value::Array(a) if !a.is_empty() => {
                a.pop();
            }
            _ => break,
        }
    }
    if c.pushed.as_ref() == Some(&v) {
        None
    } else {
        Some(v)
    }
}

fn push_presence(addr: &str) -> anyhow::Result<()> {
    if let Some(items) = presence_due(addr, Instant::now()) {
        if call(addr, || mesh::activity(addr, &items))?.is_some() {
            lock_consoles().entry(addr.to_string()).or_default().pushed = Some(items);
        }
    }
    Ok(())
}

/// The consoles to keep current: those this engine has greeted.
fn greeted() -> Vec<String> {
    lock_consoles()
        .iter()
        .filter(|(_, c)| c.hello_at.is_some())
        .map(|(a, _)| a.clone())
        .collect()
}

/// The presence used to move only when a window called: with none left, the console went on
/// showing the last list ("Docker: Remote Play" long after it ended), and nothing ever said an
/// upload was still running with its tab closed. Sends nothing unless the list changed.
pub(crate) fn spawn_presence(jobs: JobMap) {
    let _ = job_map().set(jobs);
    tokio::spawn(async {
        let mut wait_until: HashMap<String, Instant> = HashMap::new();
        loop {
            tokio::time::sleep(PRESENCE_TICK).await;
            for addr in greeted() {
                let now = Instant::now();
                if wait_until.get(&addr).is_some_and(|t| now < *t) {
                    continue;
                }
                let target = addr.clone();
                let pushed = tokio::task::spawn_blocking(move || push_presence(&target))
                    .await
                    .is_ok_and(|r| r.is_ok());
                if pushed {
                    wait_until.remove(&addr);
                } else {
                    wait_until.insert(addr, now + PRESENCE_RETRY);
                }
            }
        }
    });
}

fn seen(addr: &str, client: Option<&str>) {
    if let Some(cl) = client.filter(|c| !c.is_empty() && c.len() <= 64) {
        let mut map = lock_consoles();
        let c = map.entry(addr.to_string()).or_default();
        let e = c
            .clients
            .entry(cl.to_string())
            .or_insert_with(|| (Instant::now(), Vec::new()));
        e.0 = Instant::now();
    }
}

async fn answer(f: impl FnOnce() -> anyhow::Result<Value> + Send + 'static) -> Response {
    let r = tokio::task::spawn_blocking(f)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    answer_result(r)
}

fn answer_result(r: anyhow::Result<Value>) -> Response {
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
pub struct StateQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    since: Option<u64>,
    ev: Option<u64>,
    client: Option<String>,
}

pub async fn state(Query(q): Query<StateQuery>) -> Response {
    let Some(addr) = crate::required_console_addr(q.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        seen(&addr, q.client.as_deref());
        // Presence first, so the answer already shows it.
        let _ = push_presence(&addr);
        let r = call(&addr, || {
            mesh::state(&addr, q.since.unwrap_or(0), q.ev.unwrap_or(0))
        })?;
        Ok(with_supported(r))
    })
    .await
}

#[derive(Deserialize)]
pub struct ActivityReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    client: String,
    #[serde(default)]
    items: Vec<Value>,
}

pub async fn activity(Json(req): Json<ActivityReq>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        if req.client.is_empty() || req.client.len() > 64 {
            anyhow::bail!("bad client id");
        }
        lock_consoles()
            .entry(addr.clone())
            .or_default()
            .clients
            .insert(
                req.client,
                (Instant::now(), req.items.into_iter().take(16).collect()),
            );
        push_presence(&addr)?;
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

#[derive(Deserialize)]
pub struct LockReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    op: String,
    name: String,
    ttl: Option<u64>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    label: String,
    /// The window asking (a browser of this engine): its locks are its own.
    #[serde(default)]
    owner: String,
}

pub async fn lock(Json(req): Json<LockReq>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        let r = call(&addr, || {
            mesh::lock(
                &addr,
                &req.op,
                &req.name,
                req.ttl.unwrap_or(60),
                req.force,
                &req.label,
                &req.owner,
            )
        })?;
        Ok(with_supported(r))
    })
    .await
}

#[derive(Deserialize)]
pub struct DeviceReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    op: String,
    id: String,
    name: Option<String>,
}

pub async fn device(Json(req): Json<DeviceReq>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        let r = call(&addr, || {
            mesh::device(&addr, &req.op, &req.id, req.name.as_deref())
        })?;
        Ok(with_supported(r))
    })
    .await
}

#[derive(Deserialize)]
pub struct SettingsReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    require_approval: Option<bool>,
    prune_days: Option<u64>,
}

pub async fn settings(Json(req): Json<SettingsReq>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        let r = call(&addr, || {
            mesh::settings(&addr, req.require_approval, req.prune_days)
        })?;
        Ok(with_supported(r))
    })
    .await
}

#[derive(Deserialize)]
pub struct DocQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    name: String,
}

pub async fn doc_get(Query(q): Query<DocQuery>) -> Response {
    let Some(addr) = crate::required_console_addr(q.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        Ok(with_supported(call(&addr, || {
            mesh::doc_get(&addr, &q.name)
        })?))
    })
    .await
}

#[derive(Deserialize)]
pub struct DocPut {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    name: String,
    base: u64,
    doc: Value,
}

pub async fn doc_put(Json(req): Json<DocPut>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    if !req.doc.is_object() {
        return json_err(StatusCode::BAD_REQUEST, "doc must be an object").into_response();
    }
    answer(move || {
        let r = call(&addr, || {
            mesh::doc_put(&addr, &req.name, req.base, &req.doc)
        })?;
        Ok(with_supported(r))
    })
    .await
}

#[derive(Deserialize)]
pub struct AnnounceReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    kind: String,
    #[serde(default)]
    text: String,
}

pub async fn announce(Json(req): Json<AnnounceReq>) -> Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        let r = call(&addr, || mesh::announce(&addr, &req.kind, &req.text))?;
        Ok(with_supported(r))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_is_the_union_of_live_windows_and_only_sent_when_it_changed() {
        let addr = "test-presence-1";
        let now = Instant::now();
        {
            let mut map = lock_consoles();
            let c = map.entry(addr.into()).or_default();
            c.clients.insert(
                "a".into(),
                (now, vec![json!({"kind":"upload","title":"A"})]),
            );
            c.clients.insert(
                "b".into(),
                (
                    now - CLIENT_GONE - Duration::from_secs(1),
                    vec![json!({"kind":"upload","title":"B"})],
                ),
            );
        }
        let v = presence_due(addr, now).expect("first push");
        let s = v.to_string();
        assert!(s.contains("\"A\"") && !s.contains("\"B\""), "{s}");
        lock_consoles().get_mut(addr).unwrap().pushed = Some(v);
        assert!(presence_due(addr, now).is_none());
    }

    #[test]
    fn a_tab_a_browser_has_throttled_to_once_a_minute_is_still_there() {
        let addr = "test-presence-hidden";
        let now = Instant::now();
        lock_consoles()
            .entry(addr.into())
            .or_default()
            .clients
            .insert(
                "hidden".into(),
                (
                    now - Duration::from_secs(60),
                    vec![json!({"kind":"upload","title":"H"})],
                ),
            );
        assert!(presence_due(addr, now)
            .expect("a push")
            .to_string()
            .contains("\"H\""));
    }

    #[test]
    fn a_noted_transfer_shows_when_no_window_is_left_and_only_then() {
        let addr = "192.0.2.77";
        let id = uuid::Uuid::new_v4();
        note_job(id, addr, "Upload", "/data/homebrew/Game.pkg/");
        let jobs: JobMap = Default::default();
        jobs.lock().unwrap().insert(
            id,
            crate::JobState::Running {
                stage: None,
                started_at_ms: 0,
                bytes_sent: 55,
                total_bytes: 100,
                files: Default::default(),
                skipped_files: 0,
                skipped_bytes: 0,
                files_processing: 0,
                files_finalized: 0,
                files_finalizing_total: 0,
                bytes_finalized: 0,
            },
        );
        let _ = job_map().set(jobs.clone());
        // Another test may have set the map first: then there is nothing of ours to look at.
        if !std::sync::Arc::ptr_eq(job_map().get().unwrap(), &jobs) {
            return;
        }
        let now = Instant::now();
        let shown = presence_due(addr, now).expect("the job is shown");
        assert_eq!(
            shown,
            json!([{"kind": "Upload", "title": "Game.pkg", "pct": 50}])
        );
        // A window speaks for itself.
        lock_consoles()
            .entry(addr.into())
            .or_default()
            .clients
            .insert(
                "w".into(),
                (now, vec![json!({"kind":"Upload","title":"mine"})]),
            );
        let with_window = presence_due(addr, now).unwrap().to_string();
        assert!(!with_window.contains("Game.pkg"), "{with_window}");
    }

    #[test]
    fn a_rough_percent_only_moves_in_tens() {
        assert_eq!(rough_pct(0, 100), Some(0));
        assert_eq!(rough_pct(19, 100), Some(10));
        assert_eq!(rough_pct(100, 100), Some(100));
        assert_eq!(rough_pct(5, 0), None);
    }

    #[test]
    fn presence_is_cut_to_what_the_console_keeps() {
        let addr = "test-presence-2";
        let now = Instant::now();
        let many: Vec<Value> = (0..16)
            .map(|i| json!({"kind":"upload","title": "x".repeat(150), "n": i}))
            .collect();
        lock_consoles()
            .entry(addr.into())
            .or_default()
            .clients
            .insert("a".into(), (now, many));
        let v = presence_due(addr, now).unwrap();
        assert!(serde_json::to_vec(&v).unwrap().len() <= 1400);
        assert!(!v.as_array().unwrap().is_empty());
    }

    #[test]
    fn an_older_helper_reads_as_unsupported() {
        assert_eq!(with_supported(None), json!({"supported": false}));
        assert_eq!(
            with_supported(Some(json!({"ok": true}))),
            json!({"ok": true, "supported": true})
        );
    }
}
