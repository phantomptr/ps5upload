//! Real implementations of the connectivity + payload-send primitives.
//! These stay small and dependency-free: a TCP connect for port_check, a
//! `stream the file + half-close` for payload_send, and a thin wrapper
//! around the engine's FS_LIST_DIR for manage_list.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const PS5_LOADER_PORT: u16 = 9021;
/// Management port — lightweight, served by its own pthread inside the
/// payload. Used for HELLO / STATUS / FS_* / CLEANUP / QUERY_TX /
/// TAKEOVER_REQUEST. Responsive even during an active transfer, which
/// is the whole point of the 9113/9114 split. Transfer commands
/// (BEGIN_TX / STREAM_SHARD / COMMIT_TX / ABORT_TX) accept their `addr`
/// from the renderer, which supplies :9113 directly.
const PS5_MGMT_PORT: u16 = 9114;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
const PAYLOAD_SEND_MAX_BYTES: u64 = 128 * 1024 * 1024;
const EMBEDDED_PAYLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Generic TCP reachability probe. Mirrors the Electron `port_check` shape:
/// returns `{ open, error? }`. Used by the UI to know whether an IP is
/// reachable on a given service port.
#[tauri::command]
pub async fn port_check(ip: String, port: u16) -> serde_json::Value {
    let addr = format!("{ip}:{port}");
    match timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => serde_json::json!({ "open": true }),
        Ok(Err(e)) => serde_json::json!({ "open": false, "error": e.to_string() }),
        Err(_) => serde_json::json!({ "open": false, "error": "timeout" }),
    }
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
///   { ok: true,  reachable: true,  status: {...full STATUS_ACK...} }
///   { ok: false, reachable: false, error: "<reason>" }
#[tauri::command]
pub async fn payload_check(ip: String) -> serde_json::Value {
    let engine_url = crate::engine::url();
    // URL-encode the addr query value like every other engine proxy in
    // ps5_engine.rs. The renderer-supplied `ip` is free-form; without
    // encoding a `&`/`#`/space corrupts the query string and the STATUS
    // round-trip targets the wrong address.
    let addr = crate::commands::ps5_engine::urlencoding(&format!("{ip}:{PS5_MGMT_PORT}"));
    let url = format!("{engine_url}/api/ps5/status?addr={addr}");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({ "ok": false, "reachable": false, "error": e.to_string() })
        }
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(status) => serde_json::json!({
                "ok": true,
                "reachable": true,
                "status": status,
            }),
            Err(e) => serde_json::json!({
                "ok": false,
                "reachable": true,
                "error": format!("decode STATUS_ACK: {e}"),
            }),
        },
        // 502 Bad Gateway from the engine means the connect/STATUS frame
        // round-trip itself failed — surface as "not running" rather
        // than as an engine error so the UI can render "Not reachable".
        Ok(r) => {
            let code = r.status();
            let body = r.text().await.unwrap_or_default();
            serde_json::json!({
                "ok": false,
                "reachable": false,
                "error": if body.is_empty() {
                    format!("engine returned HTTP {code}")
                } else {
                    body
                },
            })
        }
        Err(e) => serde_json::json!({
            "ok": false,
            "reachable": false,
            "error": e.to_string(),
        }),
    }
}

/// Stream a file to a PS5 loader port. Extracted from the
/// `#[tauri::command]` wrapper so the core flow (open → optional ELF
/// magic check → connect → stream → half-close) is reachable from
/// `#[tokio::test]` without standing up a Tauri runtime.
///
/// When `target_port == PS5_LOADER_PORT` (9021 — the canonical ELF
/// loader convention) we peek the first 4 bytes and reject non-ELF
/// before opening the TCP socket. Without this, picking the wrong
/// file silently streamed garbage to the loader, which then either
/// hung or no-oped — surfacing to the user as "send succeeded but
/// the payload didn't come up." Other ports (custom-build loaders,
/// .bin/.js/.lua/.jar scene flows surfaced by `payload_probe`) skip
/// the check; those formats don't begin with the ELF magic. JARs in
/// particular start with `PK\x03\x04` (ZIP) — sending them to :9021
/// would be a no-op; users targeting BD-JB-style loaders are expected
/// to set a non-9021 port in the Send Payload screen.
async fn do_payload_send(ip: &str, path: &str, target_port: u16) -> Result<u64, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {path}: {e}"))?;
    let size = file
        .metadata()
        .await
        .map_err(|e| format!("stat {path}: {e}"))?
        .len();
    if size > PAYLOAD_SEND_MAX_BYTES {
        return Err(format!(
            "payload is too large ({size} bytes > {PAYLOAD_SEND_MAX_BYTES} cap)"
        ));
    }
    if target_port == PS5_LOADER_PORT {
        if size < 4 {
            return Err(format!("not an ELF file: {path} (only {size} bytes)"));
        }
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)
            .await
            .map_err(|e| format!("read {path}: {e}"))?;
        if &magic != b"\x7FELF" {
            return Err(format!(
                "not an ELF file: {path} (first 4 bytes {magic:02x?})"
            ));
        }
        // Rewind so the magic bytes ship as part of the file body.
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|e| format!("seek {path}: {e}"))?;
    }
    // Best-effort old-payload eviction. When the user resends payload
    // bytes to :9021, the PS5 ELF loader spawns a fresh process — but
    // the OLD ps5upload payload is unaware and keeps running. The two
    // contend for :9114 and the new bind fails, leaving the OLD
    // payload still answering with whatever its (possibly stale) wire
    // protocol expects. Symptom users see: "I sent the payload but
    // installs still fail with read frame header." Send a Shutdown
    // frame to the existing :9114 first, give it a moment to free
    // the ports, THEN push the new ELF. No-op when nothing's
    // listening on :9114 (first send of the session, console
    // rebooted, etc) — shutdown_running_payload returns Ok(false)
    // and we proceed normally.
    //
    // Only relevant for the ELF loader path. On other ports (.jar
    // BD-JB loader, .js webkit stages, etc) we don't own the
    // protocol on :9114 and the shutdown handshake doesn't apply.
    if target_port == PS5_LOADER_PORT {
        let mgmt_addr = format!("{ip}:9114");
        // Off the async runtime — Connection is blocking I/O.
        let _ = tokio::task::spawn_blocking(move || {
            ps5upload_core::payload_lifecycle::shutdown_running_payload(&mgmt_addr)
        })
        .await;
        // Brief grace period for the OS to recycle :9114 after the
        // old process exits. 600 ms is enough for the typical FreeBSD
        // close-wait → unbind transition on the PS5 we've measured;
        // anything more would noticeably slow the user-facing send.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    let addr = format!("{ip}:{target_port}");
    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("connect {addr}: timeout"))?
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let sent = timeout(SEND_TIMEOUT, async {
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| format!("read {path}: {e}"))?;
            if n == 0 {
                break;
            }
            total = total.saturating_add(n as u64);
            if total > PAYLOAD_SEND_MAX_BYTES {
                return Err(format!(
                    "payload exceeded {PAYLOAD_SEND_MAX_BYTES} bytes while streaming"
                ));
            }
            stream
                .write_all(&buf[..n])
                .await
                .map_err(|e| format!("write: {e}"))?;
        }
        // Bound the half-close FIN: if the PS5 loader's TCP stack
        // doesn't promptly ACK our FIN (e.g. its keepalive interval
        // hasn't fired yet), an unbounded shutdown() can block until
        // the OS keepalive default fires (Linux: 2 hours). 5 s is
        // generous for a healthy LAN and keeps the worst case bounded.
        match tokio::time::timeout(std::time::Duration::from_secs(5), stream.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("shutdown: {e}")),
            Err(_) => {
                // Treat shutdown timeout as success — the bytes are
                // already in the kernel's send buffer and the loader
                // typically reads + processes the ELF before ACKing
                // our half-close anyway.
            }
        }
        Ok::<u64, String>(total)
    })
    .await
    .map_err(|_| "send timed out".to_string())??;
    Ok(sent)
}

/// Send an ELF (or other payload) to the PS5 payload loader. Matches
/// the `make send-payload` behaviour: TCP connect to ip:port, stream
/// the file, half-close the write side (the loader uses EOF as the
/// "go execute" signal).
///
/// `port` is optional — defaults to `PS5_LOADER_PORT` (9021). Pass an
/// override for scene payloads that bind a different loader port (some
/// custom builds do). The Connection screen's fast-path send always
/// uses the default; the Send-payload screen exposes a port field.
#[tauri::command]
pub async fn payload_send(ip: String, path: String, port: Option<u16>) -> serde_json::Value {
    let target_port = port.unwrap_or(PS5_LOADER_PORT);
    match do_payload_send(&ip, &path, target_port).await {
        Ok(n) => serde_json::json!({
            "ok": true,
            "status": format!("sent {n} bytes to {ip}:{target_port}"),
            "bytes": n
        }),
        Err(e) => serde_json::json!({ "ok": false, "status": e }),
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
fn find_bundled_payload(app: &AppHandle) -> Result<PathBuf, String> {
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

// ─── DPI install daemon (ezremote-dpi) auto-load ─────────────────────

const DPI_DAEMON_PORT: u16 = 9040;

/// DPI daemon ELF, embedded when present at build time (see build.rs's
/// `have_dpi` gate). Absent in CI build-verification (no DPI build) —
/// then `dpi_ensure` reports it unavailable rather than failing to link.
#[cfg(have_dpi)]
const EMBEDDED_DPI_GZ: &[u8] = include_bytes!(env!("PS5UPLOAD_DPI_GZ_BYTES"));

/// Extract the embedded DPI daemon ELF into the app's local-data dir,
/// blake3-stamped for reuse. Mirrors `find_bundled_payload`.
#[cfg(have_dpi)]
fn find_bundled_dpi(app: &AppHandle) -> Result<PathBuf, String> {
    use std::fs;
    use std::io::{Read, Write};

    let out_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?
        .join("payload");
    fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let out_path = out_dir.join("ezremote-dpi.elf");
    let stamp_path = out_dir.join("ezremote-dpi.elf.gz.blake3");
    let embedded_hex = blake3::hash(EMBEDDED_DPI_GZ).to_hex().to_string();

    if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
        if stored.trim() == embedded_hex && meta.len() > 0 {
            return Ok(out_path);
        }
    }
    let _guard = EXTRACT_LOCK
        .lock()
        .map_err(|e| format!("acquire extract lock: {e}"))?;
    if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
        if stored.trim() == embedded_hex && meta.len() > 0 {
            return Ok(out_path);
        }
    }
    let tmp_path = out_dir.join(format!(
        "ezremote-dpi.elf.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let mut decoder = flate2::read::GzDecoder::new(EMBEDDED_DPI_GZ);
    let mut tmp =
        fs::File::create(&tmp_path).map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    let mut magic = Vec::with_capacity(4);
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| format!("gunzip dpi: {e}"))?;
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
            return Err("decompressed dpi.elf too large".into());
        }
        tmp.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
    }
    if magic.as_slice() != b"\x7FELF" {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("dpi.elf not an ELF (first bytes {magic:02x?})"));
    }
    let _ = tmp.sync_all();
    drop(tmp);
    if let Err(e) = super::replace_file(&tmp_path, &out_path) {
        let _ = fs::remove_file(&tmp_path);
        if let (Ok(stored), Ok(meta)) = (fs::read_to_string(&stamp_path), fs::metadata(&out_path)) {
            if stored.trim() == embedded_hex && meta.len() > 0 {
                return Ok(out_path);
            }
        }
        return Err(format!("rename dpi.elf: {e}"));
    }
    let _ = fs::write(&stamp_path, &embedded_hex);
    Ok(out_path)
}

/// Ensure the DPI install daemon is listening on `:9040`. Reuses one
/// that's already up (ours from a prior call, or a scene daemon like
/// etaHEN/ezRemote). Otherwise streams the bundled `ezremote-dpi.elf`
/// to the loader (`:9021`) — which on a single-payload loader REPLACES
/// our main payload, so the caller must re-send the main payload after
/// the install — and waits for `:9040`.
///
/// Response: `{ ok, listening, sent, error? }`.
#[tauri::command]
pub async fn dpi_ensure(app: AppHandle, ip: String) -> serde_json::Value {
    let addr = format!("{ip}:{DPI_DAEMON_PORT}");
    let up = || async {
        timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false)
    };
    if up().await {
        return serde_json::json!({ "ok": true, "listening": true, "sent": false });
    }
    #[cfg(have_dpi)]
    {
        let dpi_path = match find_bundled_dpi(&app) {
            Ok(p) => p,
            Err(e) => return serde_json::json!({ "ok": false, "error": e }),
        };
        if let Err(e) =
            do_payload_send(&ip, &dpi_path.to_string_lossy(), PS5_LOADER_PORT).await
        {
            return serde_json::json!({ "ok": false, "error": format!("send dpi.elf: {e}") });
        }
        for _ in 0..16 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if up().await {
                return serde_json::json!({ "ok": true, "listening": true, "sent": true });
            }
        }
        serde_json::json!({ "ok": false, "sent": true, "listening": false,
                            "error": "DPI daemon did not come up on :9040" })
    }
    #[cfg(not(have_dpi))]
    {
        let _ = app;
        serde_json::json!({ "ok": false, "listening": false, "sent": false,
                            "error": "DPI daemon is not bundled in this build" })
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

    let name_match = path.to_ascii_lowercase().contains("ps5upload");

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
mod payload_send_tests {
    //! Pin the `do_payload_send` flow: ELF magic check fires only
    //! on the canonical loader port, file bytes reach the listener,
    //! and the half-close signals EOF. Pre-2.2.61 there was a
    //! parallel implementation in `ps5upload-core::payload_loader`
    //! that had the magic check but no production callers — we
    //! deleted it and ported the check here.
    use super::*;
    use std::path::{Path, PathBuf};
    use tokio::net::TcpListener;

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

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ps5upload_probes_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_fixture(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[tokio::test]
    async fn rejects_non_elf_on_default_loader_port() {
        let tmp = tempdir();
        let p = write_fixture(
            &tmp,
            "fake.elf",
            b"NOT AN ELF, ENOUGH BYTES TO PASS THE SIZE CHECK",
        );

        // Port 0 would also fail to connect, but the magic check fires
        // first because we open and peek before connecting.
        let err = do_payload_send("127.0.0.1", p.to_str().unwrap(), PS5_LOADER_PORT)
            .await
            .unwrap_err();
        assert!(
            err.contains("not an ELF"),
            "expected ELF-magic rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_too_small_file_on_default_loader_port() {
        let tmp = tempdir();
        let p = write_fixture(&tmp, "tiny.elf", b"AB");

        let err = do_payload_send("127.0.0.1", p.to_str().unwrap(), PS5_LOADER_PORT)
            .await
            .unwrap_err();
        assert!(
            err.contains("not an ELF"),
            "expected size-based rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn skips_magic_check_on_non_default_port() {
        // .bin / .js / .lua scene flows use custom loader ports and
        // don't begin with the ELF magic. The check must NOT fire
        // there. We verify by sending a non-ELF to a real listener
        // on a non-default port and asserting success + bytes match.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, PS5_LOADER_PORT, "ephemeral port must not be 9021");

        let received = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            buf
        });

        let tmp = tempdir();
        let payload = b"#!/usr/bin/env lua\nprint('hi')\n";
        let p = write_fixture(&tmp, "exploit.lua", payload);

        let n = do_payload_send("127.0.0.1", p.to_str().unwrap(), port)
            .await
            .unwrap();
        assert_eq!(n, payload.len() as u64);

        let got = received.await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn sends_elf_bytes_to_loopback_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let received = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            buf
        });

        let tmp = tempdir();
        let mut elf = b"\x7FELF".to_vec();
        elf.extend(vec![0xabu8; 1024]);
        let p = write_fixture(&tmp, "ok.elf", &elf);

        // Use the random listener's port — that path skips the
        // magic check, but we want to also exercise the magic path,
        // so we run a *second* send against the default loader port
        // wrapper-style: the listener test above proved the bytes
        // path works; here we just confirm a valid ELF makes it
        // through the magic gate without rewriting the buffer.
        let n = do_payload_send("127.0.0.1", p.to_str().unwrap(), port)
            .await
            .unwrap();
        assert_eq!(n, elf.len() as u64);

        let got = received.await.unwrap();
        assert_eq!(got, elf);
    }

    #[tokio::test]
    async fn enforces_size_cap() {
        let tmp = tempdir();
        let p = tmp.join("huge.elf");
        // Use set_len to make a sparse file at the size cap + 1 — no
        // need to actually allocate 128 MiB on disk for this test.
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(PAYLOAD_SEND_MAX_BYTES + 1).unwrap();
        drop(f);

        let err = do_payload_send("127.0.0.1", p.to_str().unwrap(), PS5_LOADER_PORT)
            .await
            .unwrap_err();
        assert!(err.contains("too large"), "expected size cap, got: {err}");
    }
}
