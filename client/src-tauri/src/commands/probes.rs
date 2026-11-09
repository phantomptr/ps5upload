//! Real implementations of the connectivity + payload-send primitives.
//! These stay small and dependency-free: a TCP connect for port_check, a
//! thin caller of `payload_lifecycle` for payload_send, and a thin wrapper
//! around the engine's FS_LIST_DIR for manage_list.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

const PS5_LOADER_PORT: u16 = 9021;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Name resolution gets its own, longer budget — separate from the connect
/// budget above.
///
/// Folding DNS into `CONNECT_TIMEOUT` meant a console entered by hostname had
/// to resolve AND complete a TCP handshake inside 3 s. A cold cache on a name
/// the resolver has to walk a suffix search list for (Windows and a `.lan`
/// name being the reported case, #272) can spend most of that budget on
/// resolution alone, and the probe then reports "port not open" for what is
/// really a slow lookup. Resolving separately also lets the error say which
/// of the two actually failed.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const EMBEDDED_PAYLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// How long `payload_check` waits for the engine's status route. Longer than the engine's own
/// worst case (a new session, then the status call: about 5 s), so a console that is slow to
/// answer is answered by the engine, never mistaken for an engine that is not there (5 s was
/// shorter than that, and a console that had just gone into rest mode read as "engine
/// unreachable").
const STATUS_HTTP_TIMEOUT: Duration = Duration::from_secs(8);

/// Generic TCP reachability probe. Mirrors the Electron `port_check` shape:
/// returns `{ open, error? }`. Used by the UI to know whether an IP is
/// reachable on a given service port.
#[tauri::command]
pub async fn port_check(ip: String, port: u16) -> serde_json::Value {
    match connect_probe(&ip, port).await {
        Ok(()) => serde_json::json!({ "open": true }),
        Err(e) => serde_json::json!({ "open": false, "error": e }),
    }
}

/// Resolve `host` (IP literal or DNS name) into the addresses to try, IPv4
/// first. Shared with the scene-tool strip so both surfaces classify a
/// hostname failure the same way — the split where one lit up green and the
/// other said "not open" is what made #272 so hard to read.
///
/// The returned `Err` is a user-facing string: it names the host and says
/// resolution, not connection, is what failed.
pub(crate) async fn resolve_targets(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addr = format!("{host}:{port}");
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(vec![sa]);
    }
    let resolved = match timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host(addr)).await {
        Ok(Ok(it)) => it,
        Ok(Err(e)) => return Err(format!("cannot resolve \"{host}\": {e}")),
        Err(_) => {
            return Err(format!(
                "cannot resolve \"{host}\": name lookup timed out after {}s",
                RESOLVE_TIMEOUT.as_secs()
            ))
        }
    };
    let mut out: Vec<SocketAddr> = resolved.collect();
    if out.is_empty() {
        return Err(format!("cannot resolve \"{host}\": no addresses"));
    }
    // IPv4 first: the PS5's LAN listeners are IPv4 in practice, and a name
    // carrying a dead AAAA would otherwise burn the whole connect budget on
    // IPv6 before falling back.
    out.sort_by_key(|sa| !sa.is_ipv4());
    Ok(out)
}

/// Resolve then TCP-connect, reporting which step failed. `Ok(())` means the
/// port accepted a connection.
pub(crate) async fn connect_probe(host: &str, port: u16) -> Result<(), String> {
    connect_probe_within(host, port, CONNECT_TIMEOUT).await
}

/// `connect_probe` allowing `per_address` for each address the host resolves to. A hostname still
/// gets its own lookup budget, so a slow first lookup does not read as "not running" (#272).
pub(crate) async fn connect_probe_within(
    host: &str,
    port: u16,
    per_address: Duration,
) -> Result<(), String> {
    let targets = resolve_targets(host, port).await?;
    let mut last = String::from("no addresses to try");
    for sa in targets {
        match timeout(per_address, TcpStream::connect(sa)).await {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "timeout".to_string(),
        }
    }
    Err(last)
}

/// Full payload probe. Before, this was a shallow TCP reachability
/// check — but the UI wants version + uptime so the Status pill can
/// say "Running v2.0.0 for 3m". We now route through the engine's
/// /api/ps5/status (a real STATUS frame round-trip) and return the
/// decoded JSON to the renderer. The UI's engine-status tick already
/// uses the same endpoint, so we keep behaviour consistent between
/// the explicit Check button and the 5s poll.
///
/// Response shape:
///   { ok: true,  reachable: true,  engine: true,  status: {...STATUS_ACK...} }
///   { ok: false, reachable: false, engine: <bool>, error: "<reason>" }
///
/// `engine` says whether the ENGINE answered at all, which is not the same
/// question as `reachable` (whether the CONSOLE answered). The renderer needs
/// both: a console verdict of "down" is only trustworthy when the engine
/// rendered it. Without this bit an engine outage (engine dead, restarting, or
/// too busy to answer in 5s) was reported as "the console is down" — a verdict
/// nothing can ever contradict, because the engine is the only thing that
/// probes the console. In the 2026-09-14 post-mortem that unmovable DOWN
/// armed the renderer's auto-redeploy loop, which pushed a fresh ELF at :9021
/// every ~32s for six minutes on both consoles, killing each helper as it came
/// up until the consoles stopped responding.
#[tauri::command]
pub async fn payload_check(ip: String) -> serde_json::Value {
    let engine_url = crate::engine::url();
    // URL-encode the addr query value like every other engine proxy in
    // ps5_engine.rs. The renderer-supplied `ip` is free-form; without
    // encoding a `&`/`#`/space corrupts the query string and the STATUS
    // round-trip targets the wrong address.
    let addr = crate::commands::ps5_engine::urlencoding(&ip);
    let url = format!("{engine_url}/api/ps5/status?addr={addr}");
    let client = match crate::engine_http::engine_client_builder()
        .timeout(STATUS_HTTP_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        // Our own HTTP client wouldn't build. Nothing was asked of the
        // console, so it gets no vote.
        Err(e) => {
            return serde_json::json!({
                "ok": false,
                "reachable": false,
                "engine": false,
                "error": e.to_string(),
            })
        }
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(status) => serde_json::json!({
                "ok": true,
                "reachable": true,
                "engine": true,
                "status": status,
            }),
            Err(e) => serde_json::json!({
                "ok": false,
                "reachable": true,
                "engine": true,
                "error": format!("decode STATUS_ACK: {e}"),
            }),
        },
        // 502 Bad Gateway from the engine means the engine DID answer and its
        // connect/STATUS frame round-trip to the console failed — surface as
        // "not running" rather than as an engine error so the UI can render
        // "Not reachable". This is the only shape that earns a console verdict.
        Ok(r) => {
            let code = r.status();
            let body = r.text().await.unwrap_or_default();
            status_failure(code.as_u16(), &body)
        }
        // Connection refused / timed out / reset: the engine side of the
        // proxy failed, so the console was never consulted.
        Err(e) => serde_json::json!({
            "ok": false,
            "reachable": false,
            "engine": false,
            "error": crate::engine_http::error_chain(&e),
        }),
    }
}

/// The reply for an engine that answered the status probe with a failure. `error` stays the
/// raw body (the renderer matches tokens in it); `reason` (the engine's token, e.g.
/// `console_rest_mode`) and `power` (`awake` / `rest` / `unreachable`) are passed through
/// from a JSON body `{ error, reason, power }` when the engine sent them.
fn status_failure(code: u16, body: &str) -> serde_json::Value {
    let mut out = serde_json::json!({
        "ok": false,
        "reachable": false,
        "engine": true,
        "error": if body.is_empty() {
            format!("engine returned HTTP {code}")
        } else {
            crate::engine_http::plain_error_text(body)
        },
    });
    if let Ok(serde_json::Value::Object(parsed)) = serde_json::from_str(body) {
        for key in ["reason", "power"] {
            if let Some(v @ serde_json::Value::String(_)) = parsed.get(key) {
                out[key] = v.clone();
            }
        }
    }
    out
}

#[cfg(test)]
mod status_failure_tests {
    use super::status_failure;

    #[test]
    fn reason_and_power_pass_through_from_the_engine_body() {
        let body = r#"{"error":"payload rejected NODE_STATUS: console_rest_mode: The PS5 is in rest mode.","reason":"console_rest_mode","power":"rest"}"#;
        let v = status_failure(502, body);
        assert_eq!(v["ok"], false);
        assert_eq!(v["reachable"], false);
        assert_eq!(v["engine"], true);
        assert_eq!(v["reason"], "console_rest_mode");
        assert_eq!(v["power"], "rest");
        assert_eq!(v["error"], body, "error stays the raw body");
    }

    #[test]
    fn an_older_engine_or_a_plain_body_has_no_reason_or_power() {
        let v = status_failure(502, r#"{"error":"helper_not_ava1: gone"}"#);
        assert!(v.get("reason").is_none() && v.get("power").is_none(), "{v}");
        let v = status_failure(502, "not json");
        assert_eq!(v["error"], "not json");
        assert!(v.get("reason").is_none());
        let v = status_failure(503, "");
        assert_eq!(v["error"], "engine returned HTTP 503");
        // A reason only, without power (the engine did not ask DDP).
        let v = status_failure(502, r#"{"error":"x","reason":"helper_not_ava1"}"#);
        assert_eq!(v["reason"], "helper_not_ava1");
        assert!(v.get("power").is_none());
    }
}

/// Send an ELF (or other payload) to the PS5 payload loader, the way
/// `payload_lifecycle::send_payload_file` does it for every front end: TCP connect to ip:port,
/// stream the file, half-close the write side (the loader uses EOF as the "go execute" signal),
/// and through Payload Manager when :9021 did not answer at all.
///
/// `port` is optional and defaults to 9021. Pass an override for scene payloads that bind a
/// different loader port. The Connection screen's fast-path send always uses the default; the
/// Send-payload screen exposes a port field.
#[tauri::command]
pub async fn payload_send(ip: String, path: String, port: Option<u16>) -> serde_json::Value {
    let target_port = port.unwrap_or(PS5_LOADER_PORT);
    let handle = tokio::runtime::Handle::current();
    let sent = tokio::task::spawn_blocking(move || {
        ps5upload_core::payload_lifecycle::send_payload_file(
            &ip,
            &path,
            target_port,
            &mut |bytes| handle.block_on(stamp_ava1_trust(bytes)),
        )
    })
    .await;
    match sent {
        Ok(Ok(s)) => serde_json::json!({ "ok": true, "status": s.status, "bytes": s.bytes }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "status": e }),
        Err(e) => serde_json::json!({ "ok": false, "status": format!("send task: {e}") }),
    }
}

/// Whether a send the user started may go now, asked of the engine: the loader serves one
/// connection at a time and the engine is the one using it (an elfldr swap, an install's daemon
/// send), and a helper image stops the helper a running job needs. This socket is the app's own,
/// so it can't take the engine's lock; the check waits for it to be free instead. The app's own
/// sends of the helper (the recovery a queue runs) do not come here.
///
/// `{ok: true}` to go ahead, also when the engine can't be asked: the send itself says what is
/// wrong then. `{ok: false, status}` with the engine's reason otherwise.
#[tauri::command]
pub async fn payload_send_check(ip: String, path: String, port: Option<u16>) -> serde_json::Value {
    if port.unwrap_or(PS5_LOADER_PORT) != PS5_LOADER_PORT {
        return serde_json::json!({ "ok": true });
    }
    let helper = file_is_helper(&path).await;
    let url = format!("{}/api/payload/send-check", crate::engine::url());
    let Ok(client) = crate::engine_http::engine_client_builder()
        .timeout(Duration::from_secs(20))
        .build()
    else {
        return serde_json::json!({ "ok": true });
    };
    let body = serde_json::json!({ "host": ip, "helper": helper });
    let Ok(resp) = client.post(url).json(&body).send().await else {
        return serde_json::json!({ "ok": true });
    };
    if resp.status() != reqwest::StatusCode::CONFLICT {
        return serde_json::json!({ "ok": true });
    }
    let reason = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "loader_busy".to_string());
    serde_json::json!({ "ok": false, "status": reason })
}

/// Whether the file at `path` is a ps5upload helper, from the same window `payload_send` reads.
async fn file_is_helper(path: &str) -> bool {
    const PROBE_WINDOW: u64 = 512 * 1024;
    let Ok(file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut window = Vec::new();
    if file
        .take(PROBE_WINDOW)
        .read_to_end(&mut window)
        .await
        .is_err()
    {
        return false;
    }
    ps5upload_core::payload_lifecycle::is_ps5upload_payload(path, &window)
}

/// This engine's AVA1 public key and the launch token that goes with it (SPEC.md §5.2),
/// or `None` if the engine is unreachable or silent.
async fn fetch_ava1_identity(url: &str) -> Option<([u8; 32], Option<[u8; 16]>)> {
    // A wedged engine must not hang the send: give up after 2 s and send unstamped.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let client = crate::engine_http::engine_client_builder().build().ok()?;
        let v: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
        let k = ava1::hex::decode(v.get("public_key")?.as_str()?)?;
        let key = <[u8; 32]>::try_from(k).ok()?;
        // Absent when the engine does not count this caller as local: the token is the
        // engine's own secret, and it stamps its own sends without us.
        let token = v
            .get("launch_token")
            .and_then(|t| t.as_str())
            .and_then(ava1::hex::decode)
            .and_then(|t| <[u8; 16]>::try_from(t).ok());
        Some((key, token))
    })
    .await
    .ok()
    .flatten()
}

/// Stamps this engine's AVA1 key — and a launch token, when the engine gives one — into a
/// ps5upload helper ELF, so the console trusts this engine without pairing and this engine
/// trusts the console it just launched without a pairing code either (SPEC.md §5.1, §5.2).
/// An unreachable engine or an ELF without a slot (an older build) is sent unchanged; the
/// console then opens its pairing window instead.
async fn stamp_ava1_trust(bytes: &mut [u8]) {
    let url = format!("{}/api/ava1/identity", crate::engine::url());
    let fetched = fetch_ava1_identity(&url).await;
    let key = fetched.as_ref().map(|(k, _)| k);
    let token = fetched.and_then(|(_, t)| t);
    // The same step the engine's own helper sends take (ava1_api::stamped_helper).
    if let Err(why) = ava1::trust::stamp_program(bytes, key, || token) {
        eprintln!("[payload_send] {why}");
    }
}

/// PS5 payload embedded at compile time via `include_bytes!`. The
/// build script sets `PS5UPLOAD_PAYLOAD_GZ_BYTES` to the absolute path
/// of `payload/ps5upload.elf.gz`; embedding makes the desktop exe
/// self-contained across platforms. We ship the gzipped form (not the
/// raw ELF) because linuxdeploy walks every ELF in the AppDir and
/// aborts when it can't resolve the payload's PS5 sprx deps — gzip
/// magic (`\x1f\x8b`) isn't ELF magic so the bundler skips it. At
/// runtime we decompress once into the app's local-data dir and reuse
/// the extracted `.elf` on subsequent sends.
///
/// Embedded on EVERY platform including mobile: the payload is just
/// bytes streamed to the PS5 over the network, identical regardless of
/// host OS. (Only the engine *binary* — a host executable — is excluded
/// from the mobile build; that one runs in-process instead.)
const EMBEDDED_PAYLOAD_GZ: &[u8] = include_bytes!(env!("PS5UPLOAD_PAYLOAD_GZ_BYTES"));

/// Serialises concurrent calls to `find_bundled_payload`. Tauri serves
/// commands on an async runtime, so two parallel mounts of the
/// Connection screen (or React StrictMode's double-effect, or rapid
/// HMR navigations) can invoke this function simultaneously. Without
/// a lock both calls truncate the same `.tmp` file, the first wins
/// the rename, and the second fails with ENOENT — exact symptom:
///   "rename .../ps5upload.elf.tmp -> .../ps5upload.elf:
///    No such file or directory (os error 2)"
static EXTRACT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Extract the embedded `.elf.gz` into the app's local-data dir and
/// return the decompressed-ELF path. Caches across launches via a
/// hash stamp of the embedded gzip bytes; a new app build has a new
/// stamp and re-extracts without reading the existing `.elf` back
/// into memory.
pub(crate) fn find_bundled_payload(app: &AppHandle) -> Result<PathBuf, String> {
    use std::fs;
    use std::io::{Read, Write};

    let cache_root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("resolving app_local_data_dir: {e}"))?;
    let out_dir = cache_root.join("payload");
    fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let out_path = out_dir.join("ps5upload.elf");
    let stamp_path = out_dir.join("ps5upload.elf.gz.blake3");

    let embedded_hex = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(EMBEDDED_PAYLOAD_GZ);
        hasher.finalize().to_hex().to_string()
    };

    // Fast path: another invocation already produced a current
    // extracted file. Skip the lock entirely so the steady-state
    // Connection-screen poll is lock-free.
    if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
        if stored.trim() == embedded_hex && meta.len() > 0 {
            return Ok(out_path);
        }
    }

    // Slow path: serialize extraction so concurrent invocations don't
    // truncate each other's `.tmp` files. The whole "check + write"
    // happens under the lock; if a parallel call did the work first,
    // we re-check at the top and short-circuit.
    let _guard = EXTRACT_LOCK
        .lock()
        .map_err(|e| format!("acquire extract lock: {e}"))?;

    // Re-check under the lock — another thread may have just finished.
    if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
        if stored.trim() == embedded_hex && meta.len() > 0 {
            return Ok(out_path);
        }
    }

    // Per-invocation tmp filename so a (theoretical) concurrent caller
    // bypassing the lock — or a stale .tmp from a crashed prior run —
    // doesn't collide. PID + nanos is unique enough; we clean up our
    // own tmp on every exit path. Random bytes would be marginally
    // tighter, but PID+nanos is dependency-free.
    let tmp_suffix = format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let tmp_path = out_dir.join(format!("ps5upload.elf.{tmp_suffix}"));
    let mut decoder = flate2::read::GzDecoder::new(EMBEDDED_PAYLOAD_GZ);
    let mut tmp =
        fs::File::create(&tmp_path).map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
    let mut magic = Vec::with_capacity(4);
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| format!("gunzip embedded payload: {e}"))?;
        if n == 0 {
            break;
        }
        if magic.len() < 4 {
            let take = (4 - magic.len()).min(n);
            magic.extend_from_slice(&buf[..take]);
        }
        total = total.saturating_add(n as u64);
        if total > EMBEDDED_PAYLOAD_MAX_BYTES {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!(
                "decompressed embedded payload exceeded {EMBEDDED_PAYLOAD_MAX_BYTES} bytes"
            ));
        }
        tmp.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
    }
    if magic.as_slice() != b"\x7FELF" {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "decompressed payload is not an ELF (first bytes {:02x?})",
            magic
        ));
    }
    tmp.sync_all()
        .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
    drop(tmp);
    if let Err(e) = super::replace_file(&tmp_path, &out_path) {
        // Best-effort tmp cleanup; ignore failure (file may already be
        // gone if another invocation snuck through without the lock).
        let _ = fs::remove_file(&tmp_path);
        // Recovery: if `out_path` exists post-failure AND its stamp
        // matches our embedded hash, treat the rename failure as a
        // benign race — someone else extracted it first. Returning
        // success here means the user-facing banner only fires on
        // genuine extraction failures (disk full, permissions, …).
        if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
            if stored.trim() == embedded_hex && meta.len() > 0 {
                return Ok(out_path);
            }
        }
        return Err(format!(
            "rename {} -> {}: {e}",
            tmp_path.display(),
            out_path.display()
        ));
    }
    if let Err(e) = fs::write(&stamp_path, &embedded_hex) {
        eprintln!(
            "[bundled-payload] could not write stamp {}: {e} (payload still extracted)",
            stamp_path.display()
        );
    }
    Ok(out_path)
}

/// Resolve the bundled `ps5upload.elf` path for the Connection screen.
/// The send flow then hands that path back through `payload_send(ip, path)`,
/// so the existing half-close + timeout logic stays in one place.
///
/// Also includes file size + mtime so the UI can show a quick "this is
/// the exact ELF we'll send" indicator — saves the "did my rebuild
/// get picked up?" round trip during payload development. Mtime is
/// seconds since Unix epoch; the renderer formats it.
#[tauri::command]
pub async fn payload_bundled_path(app: AppHandle) -> serde_json::Value {
    match find_bundled_payload(&app) {
        Ok(p) => {
            let (size, mtime) = match std::fs::metadata(&p) {
                Ok(md) => {
                    let mt = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (md.len(), mt)
                }
                Err(_) => (0u64, 0i64),
            };
            serde_json::json!({
                "ok": true,
                "path": p.to_string_lossy(),
                "size": size,
                "mtime": mtime,
            })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

/// Probe a local payload file before sending. Response shape matches
/// the legacy `shared/payload-file-utils.js::probePayloadFile` contract
/// that `App.tsx PayloadProbeResult` is declared against:
///   { is_ps5upload: boolean, code: 'payload_probe_<reason>' }
/// where <reason> is one of:
///   invalid_ext    — extension isn't .elf/.bin/.js/.lua/.jar
///   detected       — filename or file contents contain the "ps5upload"
///                    signature; this is our payload
///   no_signature   — accepted extension but doesn't look like ours
/// The i18n table in desktop/src/i18n.ts maps those codes to human strings.
///
/// Accepted extensions span the scene's common payload shapes:
///   - .elf     : native PS5 payload, loaded by :9021 elfldr
///   - .bin     : raw blobs (some kernel patches ship as .bin)
///   - .js      : browser-stage JS exploits
///   - .lua     : scripting-runtime plugins
///   - .jar     : BD-JB / BDJ-runtime payloads (Andy Nguyen's BD-JB
///                chain and follow-ups; user-supplied JAR-aware
///                loaders typically listen on a non-9021 port)
/// The probe doesn't gate — it just labels. The UI tells the user
/// what kind of file they picked; the actual loader on the PS5 side
/// is responsible for accepting or rejecting it.
#[tauri::command]
pub async fn payload_probe(path: String) -> serde_json::Value {
    let p = PathBuf::from(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "elf" | "bin" | "js" | "lua" | "jar") {
        return serde_json::json!({
            "is_ps5upload": false,
            "code": "payload_probe_invalid_ext",
        });
    }

    // The file name only: the app's own data dir carries "ps5upload" in its
    // name, which would make every cached catalogue payload look like ours.
    let name_match = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase().contains("ps5upload"))
        .unwrap_or(false);

    // Only read the first 512 KiB — plenty to spot the ASCII signature at
    // the ELF's section headers, and keeps us off disk for big files.
    const PROBE_WINDOW: usize = 512 * 1024;
    // Distinct error codes so the UI can tell "we couldn't read the
    // file" from "the file isn't ours". The previous shape collapsed
    // both into "no_signature", which sent users debugging the wrong
    // problem (e.g. a permissions issue showed up as "this isn't a
    // ps5upload binary, are you sure you picked the right file?").
    let mut file = match tokio::fs::File::open(&p).await {
        Ok(f) => f,
        Err(e) => {
            return serde_json::json!({
                "is_ps5upload": false,
                "code": "payload_probe_read_error",
                "error": format!("open: {e}"),
            });
        }
    };
    let mut window = vec![0u8; PROBE_WINDOW];
    let n = match file.read(&mut window).await {
        Ok(0) => {
            return serde_json::json!({
                "is_ps5upload": false,
                "code": "payload_probe_too_small",
            });
        }
        Ok(n) => n,
        Err(e) => {
            return serde_json::json!({
                "is_ps5upload": false,
                "code": "payload_probe_read_error",
                "error": format!("read: {e}"),
            });
        }
    };
    window.truncate(n);
    let sig_match = memmem_ascii(&window, b"ps5upload") || memmem_ascii(&window, b"PS5UPLOAD");
    if name_match || sig_match {
        serde_json::json!({
            "is_ps5upload": true,
            "code": "payload_probe_detected",
        })
    } else {
        serde_json::json!({
            "is_ps5upload": false,
            "code": "payload_probe_no_signature",
        })
    }
}

/// Ultra-tiny substring search — avoids pulling in the `memchr` crate for
/// a one-shot check per file. O(n·m) but m is 9 and we cap n at 512 KiB.
fn memmem_ascii(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod ava1_fetch_tests {
    #[tokio::test]
    async fn a_silent_engine_does_not_hang_the_fetch() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/ava1/identity", l.local_addr().unwrap());
        let _hold = tokio::spawn(async move {
            let mut keep = Vec::new();
            while let Ok(c) = l.accept().await {
                keep.push(c);
            }
        });
        let t = std::time::Instant::now();
        assert!(super::fetch_ava1_identity(&url).await.is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(3));
    }
}

#[cfg(test)]
mod payload_send_tests {
    //! The send itself is tested in `ps5upload_core::payload_lifecycle`. These two need what is
    //! embedded in this app.
    use super::*;
    use ps5upload_core::payload_lifecycle::is_ps5upload_payload;

    /// The embedded PS5 payload must decompress to a real ELF — this is
    /// exactly what "Send payload / helper" streams to the console. This
    /// guards the class of bug where a build embeds an empty/placeholder
    /// gz: the mobile build briefly stubbed `EMBEDDED_PAYLOAD_GZ` to
    /// `&[]`, which surfaced to users as a gunzip failure on send. Since
    /// every target now `include_bytes!`s the same file, this one test
    /// covers desktop and mobile alike.
    #[test]
    fn embedded_payload_decompresses_to_elf() {
        use std::io::Read;
        assert!(
            !EMBEDDED_PAYLOAD_GZ.is_empty(),
            "embedded payload gz is empty — build.rs must embed \
             payload/ps5upload.elf.gz on EVERY target (incl. android/ios)",
        );
        let mut elf = Vec::new();
        flate2::read::GzDecoder::new(EMBEDDED_PAYLOAD_GZ)
            .read_to_end(&mut elf)
            .expect("embedded payload gz must gunzip cleanly");
        assert!(
            elf.starts_with(&[0x7f, b'E', b'L', b'F']),
            "decompressed payload must be an ELF (magic 7f 45 4c 46); got {:02x?}",
            elf.get(..4),
        );
        // Sanity floor: the real payload is ~1 MB. Anything tiny means a
        // stub slipped through the embed.
        assert!(
            elf.len() > 100_000,
            "decompressed payload suspiciously small ({} bytes) — likely a stub",
            elf.len(),
        );
    }

    #[test]
    fn the_real_installer_daemon_never_evicts_the_helper() {
        // The shipped daemon carries "ps5upload" in its debug paths, so the
        // generic signature match took it for the helper — sending it would
        // have evicted the running ps5upload. Checked under its own name and
        // a neutral one.
        let elf = include_bytes!("../../../../payload/installer/ps5upload-installer.elf");
        let head = &elf[..elf.len().min(512 * 1024)];
        assert!(!is_ps5upload_payload("/x/ps5upload-installer.elf", head));
        assert!(!is_ps5upload_payload("/x/daemon.elf", head));
    }
}
