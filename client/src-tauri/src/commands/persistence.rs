//! JSON-file-backed persistence for send-payload history.
//!
//! Historically this module also hosted `profiles_*`, `queue_*`,
//! `history_*` (generic), and `config_*` handlers inherited from the
//! Electron port. Those were never wired up after the Tauri rewrite
//! and have been removed — user settings now flow through
//! `user_config.rs`, and the renderer's state is in Zustand.
//!
//! What remains is the send-payload history (Connection → Send
//! payload replays the last N sends). Schema is opaque JSON; the
//! renderer owns the shape.
//!
//! Store: `<app_data_dir>/send_payload_history.json`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};

/// Identifier for one of our on-disk JSON stores. Used as both the
/// on-disk filename AND the key into the per-store mutex table.
/// Making this an enum rather than a string makes adding a new store
/// a compile error unless the match arm in `store_mutex` is extended
/// and the filename is declared here — eliminating the class of bug
/// where a typo'd `store_mutex("send_payload_hisotry.json")` silently
/// panics at runtime.
#[derive(Clone, Copy)]
enum Store {
    SendPayloadHistory,
    ResumeTxids,
    UploadQueue,
    PayloadPlaylists,
}

impl Store {
    fn filename(self) -> &'static str {
        match self {
            Store::SendPayloadHistory => "send_payload_history.json",
            Store::ResumeTxids => "resume_txids.json",
            Store::UploadQueue => "upload_queue.json",
            Store::PayloadPlaylists => "payload_playlists.json",
        }
    }
}

/// Per-store coarse-grained mutex. Tauri dispatches `#[command]` async
/// fns on a thread pool, so two concurrent calls that both do
/// "load JSON → mutate in memory → atomic-write back" will race and
/// silently drop one of the updates (last-writer-wins on the whole
/// file). These Mutexes serialise the read-modify-write cycle per
/// file so interleaved `remember`/`forget` calls never lose records.
///
/// Using std Mutex (not tokio) because the critical section is a
/// synchronous file read + mutate + atomic rename — no awaits inside.
fn store_mutex(s: Store) -> &'static Mutex<()> {
    static HISTORY: OnceLock<Mutex<()>> = OnceLock::new();
    static TXIDS: OnceLock<Mutex<()>> = OnceLock::new();
    static QUEUE: OnceLock<Mutex<()>> = OnceLock::new();
    static PLAYLISTS: OnceLock<Mutex<()>> = OnceLock::new();
    match s {
        Store::SendPayloadHistory => HISTORY.get_or_init(|| Mutex::new(())),
        Store::ResumeTxids => TXIDS.get_or_init(|| Mutex::new(())),
        Store::UploadQueue => QUEUE.get_or_init(|| Mutex::new(())),
        Store::PayloadPlaylists => PLAYLISTS.get_or_init(|| Mutex::new(())),
    }
}

type JsonValue = serde_json::Value;

/// Monotonic counter for unique tmp-file suffixes. Concurrent writes to
/// the same store (e.g. two rapid `send_payload_history_add` calls from
/// a debounced save racing with a Send-payload button click) would
/// otherwise collide on the shared `<name>.json.tmp` path: both write,
/// first renames successfully, second finds tmp gone and fails with
/// ENOENT. Matches the pattern in user_config.rs and releases.rs.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Resolve a named file inside the app-data directory, creating the dir on
/// demand.
fn data_file(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    Ok(dir.join(name))
}

/// Best-effort sweep of `<basename>.json.tmp.*` orphans next to a
/// store file. A previous launch that crashed mid-write may have
/// left these behind; they're never the live store (we only rename
/// onto `path`, never read from `tmp`), so deleting any we find is
/// safe. Failures are logged but don't block the calling save.
fn sweep_tmp_orphans(dir: &std::path::Path, target: &std::path::Path) {
    let Some(target_name) = target.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    // Match `<target_name without .json>.json.tmp.*`. Strip the
    // .json suffix to get the base; e.g. `upload_queue.json` →
    // `upload_queue`. Then we want files starting with
    // `upload_queue.json.tmp.`.
    let prefix = format!("{target_name}.tmp.");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(&prefix) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                eprintln!(
                    "[persistence] could not sweep tmp orphan {}: {e}",
                    entry.path().display()
                );
            }
        }
    }
}

/// Load a JSON file; return the given default if it doesn't exist yet.
fn load_json_or_default(path: &PathBuf, default: JsonValue) -> Result<JsonValue, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return Ok(default);
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("parse {path:?}: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(format!("read {path:?}: {e}")),
    }
}

/// Write a JSON value atomically: temp file + fsync + rename, so a
/// power-cut mid-write doesn't corrupt the store. Per-call unique tmp
/// suffix so concurrent callers don't collide on the shared path.
///
/// The fsync between write and rename matters: without it, Linux ext4
/// can reorder the rename ahead of the data write, producing a
/// zero-byte `store.json` after a crash. Windows (ReFS/NTFS) and macOS
/// (APFS) are less eager to reorder, but the cost (~1 ms per store
/// write, amortised across rare resume-txid updates) is cheap
/// insurance for the one platform that will silently corrupt.
fn write_json_atomic(path: &PathBuf, value: &JsonValue) -> Result<(), String> {
    use std::io::Write;

    // Best-effort sweep of any leftover tmp files in the same dir
    // before we add another. Without this, a crash sequence
    // (write succeeds → fsync fails → process killed) accumulates
    // `<name>.json.tmp.<seq>` orphans that nobody ever cleans up.
    // Cheap because it only scans the parent dir on each save.
    if let Some(parent) = path.parent() {
        sweep_tmp_orphans(parent, path);
    }

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{seq}"));
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("create {tmp:?}: {e}"))?;
    if let Err(e) = f.write_all(&bytes) {
        // Tmp write failed; clean it up so we don't leak a partial
        // file. Best-effort — if cleanup itself fails the orphan
        // sweep on the next save will pick it up.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {tmp:?}: {e}"));
    }
    if let Err(e) = f.sync_all() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("fsync {tmp:?}: {e}"));
    }
    drop(f);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename {tmp:?} -> {path:?}: {e}"));
    }
    Ok(())
}

// ── Send-payload history ────────────────────────────────────────────────────
//
// Each record is a small JSON object — path, port, host, status,
// timestamp — stored newest-last on disk (append order). The UI sorts
// by timestamp on load so it doesn't have to care about write order.
//
// Bounded to SEND_PAYLOAD_HISTORY_MAX entries to keep the file small;
// users typically replay the last 5-10 sends anyway. When full, the
// oldest record is dropped on each add.

const SEND_PAYLOAD_HISTORY_MAX: usize = 50;

fn empty_send_payload_history() -> JsonValue {
    serde_json::json!({
        "records": [],
        "updated_at": 0,
    })
}

#[tauri::command]
pub async fn send_payload_history_load(app: AppHandle) -> Result<JsonValue, String> {
    let path = data_file(&app, Store::SendPayloadHistory.filename())?;
    load_json_or_default(&path, empty_send_payload_history())
}

#[tauri::command]
pub async fn send_payload_history_add(app: AppHandle, record: JsonValue) -> Result<bool, String> {
    let path = data_file(&app, Store::SendPayloadHistory.filename())?;
    // Serialize the read-modify-write cycle — two rapid adds would
    // otherwise both read the pre-change store and the second would
    // silently overwrite the first's record.
    let _guard = store_mutex(Store::SendPayloadHistory)
        .lock()
        .map_err(|e| format!("history store mutex poisoned: {e}"))?;
    let mut current = load_json_or_default(&path, empty_send_payload_history())?;

    // Dedupe: if an entry with the same (path, host, port) already exists,
    // replace it with the new record (which carries the fresh timestamp
    // and possibly a different status). Matching takes all three fields
    // because the same ELF can legitimately be sent to two different PS5s
    // (staging + prod) and those should stay distinct rows.
    let match_key = record_match_key(&record);
    if let Some(records) = current.get_mut("records").and_then(|v| v.as_array_mut()) {
        if let Some(key) = &match_key {
            records.retain(|existing| record_match_key(existing).as_deref() != Some(key));
        }
        records.push(record);
        while records.len() > SEND_PAYLOAD_HISTORY_MAX {
            records.remove(0);
        }
    }
    if let Some(obj) = current.as_object_mut() {
        obj.insert("updated_at".to_string(), serde_json::json!(now_unix_ms()));
    }
    write_json_atomic(&path, &current)?;
    Ok(true)
}

/// Stable identity for dedup purposes. Empty string is treated as
/// "missing field" — if any of path/host/port is missing we return None
/// so the caller falls through to the normal append path (better to
/// keep a malformed record than silently drop it).
fn record_match_key(record: &JsonValue) -> Option<String> {
    let obj = record.as_object()?;
    let path = obj.get("path")?.as_str()?;
    let host = obj.get("host")?.as_str()?;
    let port = obj.get("port")?.as_u64()?;
    if path.is_empty() || host.is_empty() {
        return None;
    }
    Some(format!("{path}|{host}|{port}"))
}

#[tauri::command]
pub async fn send_payload_history_clear(app: AppHandle) -> Result<bool, String> {
    let path = data_file(&app, Store::SendPayloadHistory.filename())?;
    let _guard = store_mutex(Store::SendPayloadHistory)
        .lock()
        .map_err(|e| format!("history store mutex poisoned: {e}"))?;
    write_json_atomic(&path, &empty_send_payload_history())?;
    Ok(true)
}

// ── Resume tx_id persistence ────────────────────────────────────────────────
//
// Cross-session shard-level resume needs a stable tx_id for the
// (host, src, dest) triple: when the user hits Resume after closing the
// app or after a long-running failure, the client looks up the tx_id it
// registered on the first attempt and passes it to the engine. The
// payload's journal keeps the partial shard state keyed by tx_id, so
// the resumed BEGIN_TX finds the interrupted entry and skips past the
// last-acked shard.
//
// Entry lifetime: 24 h. After that we assume the payload's tx table has
// evicted the entry (FIFO under PS5UPLOAD2_MAX_TX, so heavy users will
// roll through faster), and resuming would just cause a full re-upload
// anyway — might as well forget the stale tx_id client-side so the
// engine mints a fresh one.
//
// Store: `<app_data_dir>/resume_txids.json`.

const RESUME_TXID_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const RESUME_TXID_MAX_RECORDS: usize = 128;

fn empty_resume_txids() -> JsonValue {
    serde_json::json!({
        "records": [],
        "updated_at": 0,
    })
}

fn resume_txid_match_key(host: &str, src: &str, dest: &str) -> String {
    format!("{host}|{src}|{dest}")
}

/// Drop any expired records in-place and return the (possibly-modified)
/// store. Side effect isolates "is this data fresh?" from the caller's
/// concerns. Returns true iff anything was evicted so callers know
/// whether to flush back to disk.
fn resume_txid_prune_expired(store: &mut JsonValue) -> bool {
    let now = now_unix_ms();
    let cutoff = now.saturating_sub(RESUME_TXID_TTL_MS);
    let mut evicted = false;
    if let Some(records) = store.get_mut("records").and_then(|v| v.as_array_mut()) {
        let before = records.len();
        records.retain(|r| {
            r.get("last_seen_ms")
                .and_then(|v| v.as_u64())
                .map(|t| t >= cutoff)
                .unwrap_or(false)
        });
        evicted = records.len() != before;
    }
    evicted
}

#[derive(serde::Deserialize)]
pub struct ResumeTxidLookupReq {
    pub host: String,
    pub src: String,
    pub dest: String,
}

/// Return the most-recent tx_id_hex for (host, src, dest), or null if no
/// non-expired record exists. Prunes expired entries as a side effect so
/// the store doesn't bloat.
#[tauri::command]
pub async fn resume_txid_lookup(
    app: AppHandle,
    req: ResumeTxidLookupReq,
) -> Result<JsonValue, String> {
    let path = data_file(&app, Store::ResumeTxids.filename())?;
    // Hold the store lock across the prune-on-read path too. Without
    // it, a lookup racing a concurrent remember could re-write the
    // pre-prune store over the new record.
    let _guard = store_mutex(Store::ResumeTxids)
        .lock()
        .map_err(|e| format!("resume-txid store mutex poisoned: {e}"))?;
    let mut store = load_json_or_default(&path, empty_resume_txids())?;
    let pruned = resume_txid_prune_expired(&mut store);
    let key = resume_txid_match_key(&req.host, &req.src, &req.dest);
    let tx_id_hex = store
        .get("records")
        .and_then(|v| v.as_array())
        .and_then(|rs| {
            rs.iter().rev().find_map(|r| {
                let rk = r.get("key").and_then(|v| v.as_str())?;
                if rk == key {
                    r.get("tx_id_hex")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        });
    if pruned {
        // Pruning failure is non-fatal for the lookup itself — the
        // user still gets back the live tx_id — but the next launch
        // will repeat the prune work and silently fail again. Log
        // so the pattern surfaces in support requests.
        if let Err(e) = write_json_atomic(&path, &store) {
            eprintln!("[resume-txid] post-prune save failed: {e} (will retry next time)");
        }
    }
    Ok(match tx_id_hex {
        Some(hex) => serde_json::json!({ "tx_id_hex": hex }),
        None => serde_json::json!({ "tx_id_hex": null }),
    })
}

#[derive(serde::Deserialize)]
pub struct ResumeTxidRememberReq {
    pub host: String,
    pub src: String,
    pub dest: String,
    pub tx_id_hex: String,
    pub mode: Option<String>,
}

/// Upsert the tx_id for (host, src, dest). Called when the client starts
/// an upload and needs to remember its client-generated tx_id for a
/// possible future resume. Also bumps `last_seen_ms` so Resume is
/// available even days after the initial attempt (subject to the 24 h
/// TTL from last contact, not original creation).
#[tauri::command]
pub async fn resume_txid_remember(
    app: AppHandle,
    req: ResumeTxidRememberReq,
) -> Result<bool, String> {
    if req.tx_id_hex.is_empty() {
        return Err("tx_id_hex is required".to_string());
    }
    let path = data_file(&app, Store::ResumeTxids.filename())?;
    let _guard = store_mutex(Store::ResumeTxids)
        .lock()
        .map_err(|e| format!("resume-txid store mutex poisoned: {e}"))?;
    let mut store = load_json_or_default(&path, empty_resume_txids())?;
    let _ = resume_txid_prune_expired(&mut store);
    let key = resume_txid_match_key(&req.host, &req.src, &req.dest);
    let now = now_unix_ms();
    let new_record = serde_json::json!({
        "key": key,
        "host": req.host,
        "src": req.src,
        "dest": req.dest,
        "tx_id_hex": req.tx_id_hex,
        "mode": req.mode.unwrap_or_else(|| "reconcile".to_string()),
        "created_at_ms": now,
        "last_seen_ms": now,
    });
    if let Some(records) = store.get_mut("records").and_then(|v| v.as_array_mut()) {
        // Dedup: same (host, src, dest) → replace. This keeps the tx_id
        // stable across BEGIN_TX retries issued from subsequent app
        // launches; only a user-driven Override resets it (via forget()).
        records.retain(|r| r.get("key").and_then(|v| v.as_str()) != Some(&key));
        records.push(new_record);
        // Bound on total records so the store doesn't grow without
        // limit across many (src, dest) combinations. Oldest-first
        // eviction matches send-payload-history's behavior.
        while records.len() > RESUME_TXID_MAX_RECORDS {
            records.remove(0);
        }
    }
    if let Some(obj) = store.as_object_mut() {
        obj.insert("updated_at".to_string(), serde_json::json!(now));
    }
    write_json_atomic(&path, &store)?;
    Ok(true)
}

#[derive(serde::Deserialize)]
pub struct ResumeTxidForgetReq {
    pub host: String,
    pub src: String,
    pub dest: String,
}

/// Remove the tx_id record for (host, src, dest). Called on successful
/// Done (tx is committed on the payload side, no value in remembering
/// it) and on Override (user is explicitly starting over — keeping the
/// old tx_id would be confusing).
#[tauri::command]
pub async fn resume_txid_forget(
    app: AppHandle,
    req: ResumeTxidForgetReq,
) -> Result<bool, String> {
    let path = data_file(&app, Store::ResumeTxids.filename())?;
    let _guard = store_mutex(Store::ResumeTxids)
        .lock()
        .map_err(|e| format!("resume-txid store mutex poisoned: {e}"))?;
    let mut store = load_json_or_default(&path, empty_resume_txids())?;
    let key = resume_txid_match_key(&req.host, &req.src, &req.dest);
    let mut dirty = resume_txid_prune_expired(&mut store);
    if let Some(records) = store.get_mut("records").and_then(|v| v.as_array_mut()) {
        let before = records.len();
        records.retain(|r| r.get("key").and_then(|v| v.as_str()) != Some(&key));
        if records.len() != before {
            dirty = true;
        }
    }
    if dirty {
        if let Some(obj) = store.as_object_mut() {
            obj.insert("updated_at".to_string(), serde_json::json!(now_unix_ms()));
        }
        write_json_atomic(&path, &store)?;
    }
    Ok(true)
}

// ── Upload queue (whole-document store) ─────────────────────────────────────
//
// The renderer owns the queue shape (see client/src/state/uploadQueue.ts).
// We persist the entire queue as one JSON document on every mutation —
// queues are tiny (≤ a few hundred items, each item < 1 KiB) so partial
// updates aren't worth the complexity of per-record indexing here.
//
// Per-store mutex serialises rapid mutations (e.g. user reorders 3
// items in a row) against concurrent saves.

#[tauri::command]
pub async fn upload_queue_load(app: AppHandle) -> Result<JsonValue, String> {
    let path = data_file(&app, Store::UploadQueue.filename())?;
    load_json_or_default(&path, serde_json::json!({}))
}

#[tauri::command]
pub async fn upload_queue_save(app: AppHandle, doc: JsonValue) -> Result<bool, String> {
    let path = data_file(&app, Store::UploadQueue.filename())?;
    let _guard = store_mutex(Store::UploadQueue)
        .lock()
        .map_err(|e| format!("upload-queue store mutex poisoned: {e}"))?;
    write_json_atomic(&path, &doc)?;
    Ok(true)
}

// ── Payload playlists (whole-document store) ────────────────────────────────
//
// Same shape rationale as upload_queue. Renderer maintains a list of
// named playlists (each = ordered (payload_path, sleep_ms) steps); we
// just round-trip the JSON.

#[tauri::command]
pub async fn payload_playlists_load(app: AppHandle) -> Result<JsonValue, String> {
    let path = data_file(&app, Store::PayloadPlaylists.filename())?;
    load_json_or_default(&path, serde_json::json!({}))
}

#[tauri::command]
pub async fn payload_playlists_save(app: AppHandle, doc: JsonValue) -> Result<bool, String> {
    let path = data_file(&app, Store::PayloadPlaylists.filename())?;
    let _guard = store_mutex(Store::PayloadPlaylists)
        .lock()
        .map_err(|e| format!("payload-playlists store mutex poisoned: {e}"))?;
    write_json_atomic(&path, &doc)?;
    Ok(true)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
