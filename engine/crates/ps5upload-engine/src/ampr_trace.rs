//! Measuring how a game reads its files, to pack it the way it plays (the LZ4 asset packs'
//! profile). drakmor's ampr_emu ships a debug build that journals every APR read to
//! `/app0/ampr_commands.bin`; ShadowMount+ mounts a game's `fakelib2/` over `fakelib/`, so
//! putting that build there (with the rest of `fakelib/` beside it) traces the game without
//! touching what it ships. Sessions are collected here, and `ampr_pack::profile_gen` turns
//! them into the profile Convert's LZ4 option uses.
//!
//! Routes (all take the console as `addr`, the game's folder on it as `game_path`, and the
//! title id the sessions and profile are filed under):
//!   GET  /api/ampr/trace?title_id=                       what is recorded, and the profile
//!   POST /api/ampr/trace/start   {addr, game_path, title_id}   put the debug build in place
//!   POST /api/ampr/trace/stop    {addr, game_path, title_id}   take it out again
//!   POST /api/ampr/trace/collect {addr, game_path, title_id}   copy a session's journal here
//!   POST /api/ampr/trace/generate {title_id}                   build the profile
//!   GET  /api/ampr/profile?title_id=                           the profile's text
//!   DELETE /api/ampr/profile?title_id=                         forget sessions and profile
//!
//! A `fakelib2/` the game shipped with (a backport's) is never touched: only one carrying
//! our marker file is removed by `stop`.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use ps5upload_core::fs_ops;
use ps5upload_fpkg::ampr_pack::profile_gen;

/// The debug build of ampr_emu 0.4.2.1 (drakmor/ampr_emu release asset), fetched on first
/// use and kept under the data dir; its digest pins what we put on a console.
const DEBUG_RUNTIME_URL: &str =
    "https://github.com/drakmor/ampr_emu/releases/download/0.4.2.1/libSceAmpr.sprx-0.4.2.1-test-debug-pack";
const DEBUG_RUNTIME_SHA256: &str =
    "b44a986f2fa9903e74a34cbbd4681e899cd99196613652cd073ed11f2ca947c2";
const DEBUG_RUNTIME_VERSION: &str = "0.4.2.1 test-debug-pack";
/// What a `fakelib2/` we made carries, so `stop` knows it may remove it.
const MARKER: &str = ".ps5upload-trace";
const MARKER_TEXT: &str =
    "fakelib2 placed by ps5upload to measure this game's reads (ampr_emu debug build). Stop measuring in ps5upload to remove it.\n";
const JOURNAL: &str = "ampr_commands.bin";
const INDEX: &str = "ampr_emu.index";
/// One console read per call; the helper caps a read anyway, so the loop asks again.
const READ_CHUNK: u64 = 1 << 20;

#[derive(Deserialize)]
pub(crate) struct TitleQuery {
    title_id: String,
}

#[derive(Deserialize)]
pub(crate) struct TraceReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    game_path: String,
    title_id: String,
}

#[derive(Deserialize)]
pub(crate) struct GenerateReq {
    title_id: String,
}

/// What is on disk for a title, as the UI shows it.
#[derive(Serialize, Default)]
struct State_ {
    /// `start` ran and `stop` has not: the debug build should be on the console.
    tracing: bool,
    host: Option<String>,
    game_path: Option<String>,
    started_at: Option<u64>,
}

#[derive(Serialize)]
struct Session {
    id: String,
    collected_at: u64,
    journal_bytes: u64,
}

#[derive(Serialize)]
struct Profile {
    generated_at: u64,
    sessions: usize,
    summary: serde_json::Value,
}

fn bad(code: StatusCode, e: impl Into<String>) -> Response {
    crate::json_err(code, e).into_response()
}

fn title_ok(title_id: &str) -> Result<String, String> {
    let t = title_id.trim().to_ascii_uppercase();
    if t.is_empty()
        || t.len() > 16
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("{title_id:?} is not a title id"));
    }
    Ok(t)
}

fn data_dir() -> Result<PathBuf, String> {
    ps5upload_ava1::pool::data_dir().ok_or_else(|| "no data directory (no HOME)".to_string())
}

fn traces_dir(title_id: &str) -> Result<PathBuf, String> {
    Ok(data_dir()?.join("ampr-traces").join(title_id))
}

fn profile_path(title_id: &str) -> Result<PathBuf, String> {
    Ok(data_dir()?
        .join("ampr-profiles")
        .join(format!("{title_id}.toml")))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_state(title_id: &str) -> State_ {
    let Ok(dir) = traces_dir(title_id) else {
        return State_::default();
    };
    std::fs::read(dir.join("state.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| State_ {
            tracing: v["tracing"].as_bool().unwrap_or(false),
            host: v["host"].as_str().map(String::from),
            game_path: v["game_path"].as_str().map(String::from),
            started_at: v["started_at"].as_u64(),
        })
        .unwrap_or_default()
}

fn write_state(title_id: &str, s: &State_) -> Result<(), String> {
    let dir = traces_dir(title_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let v = serde_json::json!({
        "tracing": s.tracing, "host": s.host, "game_path": s.game_path, "started_at": s.started_at,
    });
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_vec_pretty(&v).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())
}

fn sessions(title_id: &str) -> Vec<Session> {
    let Ok(dir) = traces_dir(title_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ts) = name.parse::<u64>() else {
                continue;
            };
            let journal = e.path().join(JOURNAL);
            let Ok(meta) = std::fs::metadata(&journal) else {
                continue;
            };
            if !e.path().join(INDEX).is_file() {
                continue;
            }
            out.push(Session {
                id: name,
                collected_at: ts,
                journal_bytes: meta.len(),
            });
        }
    }
    out.sort_by_key(|s| s.collected_at);
    out
}

fn profile(title_id: &str) -> Option<Profile> {
    let path = profile_path(title_id).ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    let summary: serde_json::Value = std::fs::read(path.with_extension("json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::Value::Null);
    Some(Profile {
        generated_at: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs()),
        sessions: summary["traces"].as_u64().unwrap_or(0) as usize,
        summary,
    })
}

fn status_json(title_id: &str) -> serde_json::Value {
    let s = read_state(title_id);
    serde_json::json!({
        "ok": true,
        "title_id": title_id,
        "state": s,
        "sessions": sessions(title_id),
        "profile": profile(title_id),
        "runtime": DEBUG_RUNTIME_VERSION,
    })
}

/// GET /api/ampr/trace?title_id=
pub(crate) async fn status(
    State(_s): State<crate::AppState>,
    Query(q): Query<TitleQuery>,
) -> Response {
    match title_ok(&q.title_id) {
        Ok(t) => Json(status_json(&t)).into_response(),
        Err(e) => bad(StatusCode::BAD_REQUEST, e),
    }
}

/// The debug build from the cache, or fetched and checked against its digest.
fn debug_runtime() -> Result<Vec<u8>, String> {
    fetch_pinned(
        DEBUG_RUNTIME_URL,
        DEBUG_RUNTIME_SHA256,
        "libSceAmpr.sprx-0.4.2.1-test-debug-pack",
        "the ampr_emu debug build",
    )
}

/// The release ampr_emu the engine installs into an image whose game has none or an old one.
pub(crate) fn release_runtime() -> Result<Vec<u8>, String> {
    use ps5upload_fpkg::ampr_pack::image as img;
    fetch_pinned(
        img::RUNTIME_URL,
        img::RUNTIME_SHA256,
        "libSceAmpr.sprx-0.4.2.1",
        "ampr_emu 0.4.2.1",
    )
}

/// The emulator a game gets: ampr-emu-ng when asked for by name, else drakmor's ampr_emu.
pub(crate) fn emulator(build: Option<&str>) -> Result<Vec<u8>, String> {
    match build {
        Some("ampr-emu-ng") => ng_runtime(),
        _ => release_runtime(),
    }
}

/// ampr-emu-ng's release, which reads zstd packs.
pub(crate) fn ng_runtime() -> Result<Vec<u8>, String> {
    use crate::zstd_packs::{NG_SHA256, NG_URL, NG_VERSION};
    fetch_pinned(
        NG_URL,
        NG_SHA256,
        &format!("ampr-emu-ng-{NG_VERSION}-libSceAmpr.sprx"),
        "ampr-emu-ng",
    )
}

/// A file from the data dir's cache, fetched once and only kept or used when its SHA-256
/// matches `sha`: what goes onto a console or into an image is exactly what was reviewed.
fn fetch_pinned(url: &str, sha: &str, cache_name: &str, what: &str) -> Result<Vec<u8>, String> {
    let path = data_dir()?.join("cache").join(cache_name);
    crate::zstd_runtime::fetch_checked(&crate::zstd_runtime::http_get, url, sha, &path, what)
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

fn exists(addr: &str, path: &str) -> bool {
    fs_ops::fs_stat(addr, path).is_ok()
}

/// What `start` does on the console, blocking.
fn start_on_console(addr: &str, game_path: &str) -> Result<(), String> {
    crate::require_console_ready(addr).map_err(|e| e.to_string())?;
    if !exists(addr, &join(game_path, "eboot.bin")) {
        return Err(format!(
            "{game_path} has no eboot.bin: it is not a game folder"
        ));
    }
    let fakelib2 = join(game_path, "fakelib2");
    if exists(addr, &fakelib2) {
        if exists(addr, &join(&fakelib2, MARKER)) {
            return Ok(()); // already measuring
        }
        return Err(
            "this game has its own fakelib2 folder (a backport's); measuring would replace it, \
             so it is left alone"
                .into(),
        );
    }
    let runtime = debug_runtime()?;
    fs_ops::fs_mkdir(addr, &fakelib2).map_err(|e| format!("mkdir fakelib2: {e}"))?;
    // The marker goes in first: the folder is ours from here, so a copy that fails
    // halfway leaves something `stop` may still remove, never a folder it refuses to touch.
    ps5upload_core::diagnostics::fs_write_bytes(
        addr,
        &join(&fakelib2, MARKER),
        MARKER_TEXT.as_bytes(),
        false,
    )
    .map_err(|e| format!("writing the marker: {e}"))?;
    match fill_fakelib2(addr, game_path, &fakelib2, &runtime) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best effort: a half-built folder would load as the game's libraries.
            let _ = fs_ops::fs_delete_with_timeout(
                addr,
                &fakelib2,
                Some(std::time::Duration::from_secs(300)),
            );
            Err(e)
        }
    }
}

/// Copies the game's fakelib (the emulator aside) into the new fakelib2 and writes the debug
/// emulator there.
fn fill_fakelib2(
    addr: &str,
    game_path: &str,
    fakelib2: &str,
    runtime: &[u8],
) -> Result<(), String> {
    // Everything the game's fakelib holds comes along, so only the emulator changes.
    let fakelib = join(game_path, "fakelib");
    if exists(addr, &fakelib) {
        let mut offset = 0u64;
        loop {
            let listing = fs_ops::list_dir(
                addr,
                &fakelib,
                fs_ops::ListDirOptions { offset, limit: 256 },
            )
            .map_err(|e| format!("listing fakelib: {e}"))?;
            for entry in &listing.entries {
                if entry.kind == "dir" {
                    continue;
                }
                if entry.name.eq_ignore_ascii_case("libSceAmpr.sprx") {
                    continue;
                }
                let from = join(&fakelib, &entry.name);
                let to = join(fakelib2, &entry.name);
                ps5upload_ava1::copy::console_copy(
                    addr,
                    &from,
                    &to,
                    crate::next_fs_op_id(),
                    false,
                    true,
                )
                .map_err(|e| format!("copying fakelib/{}: {e}", entry.name))?;
            }
            offset += listing.entries.len() as u64;
            if !listing.truncated || listing.entries.is_empty() {
                break;
            }
        }
    }
    ps5upload_core::diagnostics::fs_write_bytes_large(
        addr,
        &join(fakelib2, "libSceAmpr.sprx"),
        runtime,
        false,
    )
    .map_err(|e| format!("writing the debug emulator: {e}"))?;
    Ok(())
}

fn stop_on_console(addr: &str, game_path: &str) -> Result<bool, String> {
    crate::require_console_ready(addr).map_err(|e| e.to_string())?;
    let fakelib2 = join(game_path, "fakelib2");
    if !exists(addr, &fakelib2) {
        return Ok(false);
    }
    if !exists(addr, &join(&fakelib2, MARKER)) {
        return Err("this game's fakelib2 was not placed by ps5upload, so it is left alone".into());
    }
    fs_ops::fs_delete_with_timeout(addr, &fakelib2, Some(std::time::Duration::from_secs(300)))
        .map_err(|e| format!("removing fakelib2: {e}"))?;
    Ok(true)
}

/// A whole file from the console, read in chunks (the helper caps one read).
fn read_whole(addr: &str, path: &str, size: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(size as usize);
    let mut at = 0u64;
    while at < size {
        let chunk = fs_ops::fs_read(addr, path, at, READ_CHUNK.min(size - at))
            .map_err(|e| format!("reading {path} at {at}: {e}"))?;
        if chunk.is_empty() {
            return Err(format!("{path} ended at {at} of {size} bytes"));
        }
        at += chunk.len() as u64;
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn collect_from_console(
    addr: &str,
    game_path: &str,
    title_id: &str,
) -> Result<serde_json::Value, String> {
    crate::require_console_ready(addr).map_err(|e| e.to_string())?;
    let journal = join(game_path, JOURNAL);
    let index = join(game_path, INDEX);
    let js = fs_ops::fs_stat(addr, &journal)
        .map_err(|_| "no ampr_commands.bin in the game folder yet: start measuring, play the game, quit it, then collect".to_string())?;
    if js.size < 96 {
        return Err("ampr_commands.bin is empty: the game has not run with the debug emulator since it was placed (ShadowMount+ loads fakelib2 at the game's next start)".into());
    }
    let is = fs_ops::fs_stat(addr, &index).map_err(|_| {
        "the game folder has no ampr_emu.index, which the journal is read against".to_string()
    })?;
    let jb = read_whole(addr, &journal, js.size)?;
    let ib = read_whole(addr, &index, is.size)?;
    if ps5upload_fpkg::ampr_index::parse(&ib).is_none() {
        return Err("the game's ampr_emu.index is not a readable AMPRIDX3 index".into());
    }
    let ts = now();
    let dir = traces_dir(title_id)?.join(ts.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join(JOURNAL), &jb).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(INDEX), &ib).map_err(|e| e.to_string())?;
    // The emulator starts a fresh journal at the next launch; clearing it here means the
    // next collect never re-reads this session.
    let _ = fs_ops::fs_delete(addr, &journal);
    Ok(serde_json::json!({ "session": ts.to_string(), "journal_bytes": js.size }))
}

fn generate_profile(title_id: &str) -> Result<serde_json::Value, String> {
    let dir = traces_dir(title_id)?;
    let specs: Vec<profile_gen::TraceSpec> = sessions(title_id)
        .into_iter()
        .map(|s| profile_gen::TraceSpec {
            name: s.id.clone(),
            commands: dir.join(&s.id).join(JOURNAL),
            index: dir.join(&s.id).join(INDEX),
        })
        .collect();
    if specs.is_empty() {
        return Err("no sessions recorded yet".into());
    }
    let generated = profile_gen::generate(&specs)?;
    let path = profile_path(title_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &generated.toml).map_err(|e| e.to_string())?;
    let summary = serde_json::to_value(&generated.summary).unwrap_or(serde_json::Value::Null);
    std::fs::write(
        path.with_extension("json"),
        serde_json::to_vec_pretty(&summary).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(summary)
}

fn console_and_game(req: &TraceReq) -> Result<(String, String, String), Box<Response>> {
    let Some(addr) = crate::required_console_addr(req.addr.clone()) else {
        return Err(Box::new(crate::missing_console_addr()));
    };
    let title = title_ok(&req.title_id).map_err(|e| Box::new(bad(StatusCode::BAD_REQUEST, e)))?;
    let game = req.game_path.trim().trim_end_matches('/').to_string();
    if !game.starts_with('/') || game.contains("/../") {
        return Err(Box::new(bad(
            StatusCode::BAD_REQUEST,
            format!("{game:?} is not a folder on the console"),
        )));
    }
    Ok((addr, game, title))
}

/// POST /api/ampr/trace/start
pub(crate) async fn start(
    State(_s): State<crate::AppState>,
    Json(req): Json<TraceReq>,
) -> Response {
    let (addr, game, title) = match console_and_game(&req) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let host = ps5upload_core::events::console_host(&addr);
    let r = tokio::task::spawn_blocking(move || {
        start_on_console(&addr, &game)?;
        write_state(
            &title,
            &State_ {
                tracing: true,
                host: Some(host),
                game_path: Some(game),
                started_at: Some(now()),
            },
        )?;
        Ok::<_, String>(status_json(&title))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(StatusCode::BAD_GATEWAY, e),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/ampr/trace/stop
pub(crate) async fn stop(State(_s): State<crate::AppState>, Json(req): Json<TraceReq>) -> Response {
    let (addr, game, title) = match console_and_game(&req) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let r = tokio::task::spawn_blocking(move || {
        let removed = stop_on_console(&addr, &game)?;
        let mut s = read_state(&title);
        s.tracing = false;
        write_state(&title, &s)?;
        let mut v = status_json(&title);
        v["removed"] = serde_json::Value::Bool(removed);
        Ok::<_, String>(v)
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(StatusCode::BAD_GATEWAY, e),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/ampr/trace/collect
pub(crate) async fn collect(
    State(_s): State<crate::AppState>,
    Json(req): Json<TraceReq>,
) -> Response {
    let (addr, game, title) = match console_and_game(&req) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let r = tokio::task::spawn_blocking(move || {
        let got = collect_from_console(&addr, &game, &title)?;
        let mut v = status_json(&title);
        v["collected"] = got;
        Ok::<_, String>(v)
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(StatusCode::BAD_GATEWAY, e),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/ampr/trace/generate
pub(crate) async fn generate(
    State(_s): State<crate::AppState>,
    Json(req): Json<GenerateReq>,
) -> Response {
    let title = match title_ok(&req.title_id) {
        Ok(t) => t,
        Err(e) => return bad(StatusCode::BAD_REQUEST, e),
    };
    let r = tokio::task::spawn_blocking(move || {
        generate_profile(&title)?;
        Ok::<_, String>(status_json(&title))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(StatusCode::BAD_REQUEST, e),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /api/ampr/profile?title_id= — `{exists, toml, profile}`.
pub(crate) async fn profile_get(
    State(_s): State<crate::AppState>,
    Query(q): Query<TitleQuery>,
) -> Response {
    let title = match title_ok(&q.title_id) {
        Ok(t) => t,
        Err(e) => return bad(StatusCode::BAD_REQUEST, e),
    };
    let toml = profile_path(&title)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok());
    Json(serde_json::json!({
        "ok": true,
        "exists": toml.is_some(),
        "toml": toml,
        "profile": profile(&title),
    }))
    .into_response()
}

/// DELETE /api/ampr/profile?title_id= — forgets the sessions and the profile (the console is
/// not touched: use stop for that).
pub(crate) async fn profile_delete(
    State(_s): State<crate::AppState>,
    Query(q): Query<TitleQuery>,
) -> Response {
    let title = match title_ok(&q.title_id) {
        Ok(t) => t,
        Err(e) => return bad(StatusCode::BAD_REQUEST, e),
    };
    if let Ok(p) = profile_path(&title) {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("json"));
    }
    if let Ok(dir) = traces_dir(&title) {
        let state = read_state(&title);
        let _ = std::fs::remove_dir_all(&dir);
        if state.tracing {
            // Still measuring on the console: keep that fact.
            let _ = write_state(&title, &state);
        }
    }
    Json(status_json(&title)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_ids_are_checked() {
        assert_eq!(title_ok(" ppsa01234 ").unwrap(), "PPSA01234");
        assert!(title_ok("../x").is_err());
        assert!(title_ok("").is_err());
    }

    #[test]
    fn join_keeps_one_slash() {
        assert_eq!(
            join("/data/homebrew/X/", "fakelib2"),
            "/data/homebrew/X/fakelib2"
        );
    }
}
