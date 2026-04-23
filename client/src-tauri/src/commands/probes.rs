//! Real implementations of the connectivity + payload-send primitives.
//! These stay small and dependency-free: a TCP connect for port_check, a
//! `stream the file + half-close` for payload_send, and a thin wrapper
//! around the engine's FS_LIST_DIR for manage_list.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
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
    let url = format!(
        "{engine_url}/api/ps5/status?addr={ip}:{PS5_MGMT_PORT}"
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return serde_json::json!({ "ok": false, "reachable": false, "error": e.to_string() }),
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            match r.json::<serde_json::Value>().await {
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
            }
        }
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

/// Send an ELF to the PS5 payload loader. Matches the `make send-payload`
/// behaviour: TCP connect to ip:port, stream the file, half-close the
/// write side (the loader uses EOF as the "go execute" signal).
///
/// `port` is optional — defaults to `PS5_LOADER_PORT` (9021). Pass an
/// override for scene payloads that bind a different loader port (some
/// custom builds do). The Connection screen's fast-path send always
/// uses the default; the Send-payload screen exposes a port field.
#[tauri::command]
pub async fn payload_send(
    ip: String,
    path: String,
    port: Option<u16>,
) -> serde_json::Value {
    let target_port = port.unwrap_or(PS5_LOADER_PORT);
    let result: Result<u64, String> = (async {
        let bytes = tokio::fs::read(&path).await
            .map_err(|e| format!("read {path}: {e}"))?;
        let addr = format!("{ip}:{target_port}");
        let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| format!("connect {addr}: timeout"))?
            .map_err(|e| format!("connect {addr}: {e}"))?;
        timeout(SEND_TIMEOUT, async {
            stream.write_all(&bytes).await
                .map_err(|e| format!("write: {e}"))?;
            stream.shutdown().await
                .map_err(|e| format!("shutdown: {e}"))?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|_| "send timed out".to_string())??;
        Ok(bytes.len() as u64)
    })
    .await;

    match result {
        Ok(n) => serde_json::json!({
            "ok": true,
            "status": format!("sent {n} bytes to {ip}:{target_port}"),
            "bytes": n
        }),
        Err(e) => serde_json::json!({ "ok": false, "status": e }),
    }
}

/// Locate the bundled `ps5upload.elf` the Connection screen should send.
/// Mirrors the engine-binary resolution in `engine.rs` so the lookup
/// works identically in dev (repo `payload/` dir) and packaged builds
/// (Resources, with the `_up_/_up_` escape for the `../../` prefix in
/// tauri.conf.json's `resources` list).
///
/// The bundled form is `ps5upload.elf.gz` — linuxdeploy (Linux AppImage
/// bundler) walks every ELF in the AppDir and refuses to proceed when
/// it can't resolve the PS5 payload's sprx dependencies on Linux. A
/// gzipped file shows gzip magic instead of ELF magic and is treated
/// as plain data. We decompress on first use into the app's cache
/// directory and return the decompressed path so the send path
/// (ps5upload-core::send_payload) keeps its "read file → stream bytes"
/// semantics unchanged.
///
/// Lookup order:
///   1. Bundled resource dir / payload / ps5upload.elf.gz
///   2. Bundled resource dir / _up_ / _up_ / payload / ps5upload.elf.gz
///   3. Repo-root / payload / ps5upload.elf.gz (dev + local `make payload`)
///   4. Repo-root / payload / ps5upload.elf (older dev builds, never
///      bundled but still serviceable)
fn find_bundled_payload(app: &AppHandle) -> Result<PathBuf, String> {
    let mut gz_candidates: Vec<PathBuf> = Vec::new();

    // Windows portable: payload lives at
    // `<exe-dir>/resources/payload/ps5upload.elf.gz` because
    // `--no-bundle` skips Tauri's Resources wiring and the release
    // workflow packs it in the zip alongside the exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            gz_candidates.push(
                exe_dir
                    .join("resources")
                    .join("payload")
                    .join("ps5upload.elf.gz"),
            );
        }
    }

    if let Ok(rd) = app.path().resource_dir() {
        gz_candidates.push(rd.join("payload").join("ps5upload.elf.gz"));
        gz_candidates.push(
            rd.join("_up_")
                .join("_up_")
                .join("payload")
                .join("ps5upload.elf.gz"),
        );
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from);
    if let Some(ref rr) = repo_root {
        gz_candidates.push(rr.join("payload").join("ps5upload.elf.gz"));
    }

    for gz in &gz_candidates {
        if gz.is_file() {
            return decompress_to_cache(app, gz);
        }
    }

    // Fallback: raw `.elf` in the repo checkout for devs who haven't
    // run `make payload` since the .elf.gz rule landed.
    if let Some(rr) = repo_root {
        let raw = rr.join("payload").join("ps5upload.elf");
        if raw.is_file() {
            return Ok(raw);
        }
    }

    Err(format!(
        "ps5upload.elf(.gz) not found. Searched:\n  {}\nBuild it with `make payload`.",
        gz_candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    ))
}

/// Decompress `ps5upload.elf.gz` into the app's local-data cache dir
/// and return the extracted path. Skips the decompress if the cached
/// copy is already present and matches the gz's mtime — the payload is
/// rebuilt rarely, so we avoid a ~50 ms gunzip on every Send.
fn decompress_to_cache(app: &AppHandle, gz_path: &std::path::Path) -> Result<PathBuf, String> {
    use std::fs;
    use std::io::{Read, Write};

    let cache_root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("resolving app_local_data_dir: {e}"))?;
    let out_dir = cache_root.join("payload");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let out_path = out_dir.join("ps5upload.elf");

    // Quick skip: if the extracted .elf is newer than the source .gz,
    // trust the cache. Mtime comparison handles the "build produced
    // a new .gz" case; the .elf is regenerated on next send.
    let should_extract = match (fs::metadata(gz_path), fs::metadata(&out_path)) {
        (Ok(gz_meta), Ok(elf_meta)) => match (gz_meta.modified(), elf_meta.modified()) {
            (Ok(gz_m), Ok(elf_m)) => gz_m > elf_m,
            _ => true,
        },
        _ => true,
    };
    if !should_extract {
        return Ok(out_path);
    }

    let raw = fs::read(gz_path).map_err(|e| format!("read {}: {e}", gz_path.display()))?;
    let mut decoder = flate2::read::GzDecoder::new(&raw[..]);
    let mut decompressed = Vec::with_capacity(raw.len() * 3);
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| format!("gunzip {}: {e}", gz_path.display()))?;

    // Sanity: must be ELF after decompression — catches corrupt .gz
    // or accidentally-bundled non-payload bytes before we send
    // garbage to the PS5 loader.
    if decompressed.len() < 4 || &decompressed[..4] != b"\x7FELF" {
        return Err(format!(
            "decompressed payload is not an ELF (first 4 bytes {:02x?})",
            &decompressed[..decompressed.len().min(4)]
        ));
    }

    let mut f = fs::File::create(&out_path)
        .map_err(|e| format!("create {}: {e}", out_path.display()))?;
    f.write_all(&decompressed)
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;
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
///   invalid_ext    — extension isn't .elf/.bin/.js/.lua
///   detected       — filename or file contents contain the "ps5upload"
///                    signature; this is our payload
///   no_signature   — accepted extension but doesn't look like ours
/// The i18n table in desktop/src/i18n.ts maps those codes to human strings.
///
/// Accepted extensions span the scene's common payload shapes:
///   - .elf     : native PS5 payload, loaded by :9021 elfldr
///   - .bin     : raw blobs (some kernel patches ship as .bin)
///   - .js      : browser-stage JS exploits
///   - .lua     : etaHEN / scripting-runtime plugins
/// The probe doesn't gate — it just labels. The UI tells the user
/// what kind of file they picked; the actual loader on the PS5 side
/// is responsible for accepting or rejecting it.
#[tauri::command]
pub async fn payload_probe(path: String) -> serde_json::Value {
    let p = PathBuf::from(&path);
    let ext = p.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "elf" | "bin" | "js" | "lua") {
        return serde_json::json!({
            "is_ps5upload": false,
            "code": "payload_probe_invalid_ext",
        });
    }

    let name_match = path.to_ascii_lowercase().contains("ps5upload");

    // Only read the first 512 KiB — plenty to spot the ASCII signature at
    // the ELF's section headers, and keeps us off disk for big files.
    const PROBE_WINDOW: usize = 512 * 1024;
    let bytes = match tokio::fs::read(&p).await {
        Ok(b) => b,
        Err(_) => {
            // Treat unreadable as "not ours" rather than failing; the UI
            // shows the code's localised message either way.
            return serde_json::json!({
                "is_ps5upload": false,
                "code": "payload_probe_no_signature",
            });
        }
    };
    let window = &bytes[..bytes.len().min(PROBE_WINDOW)];
    let sig_match = memmem_ascii(window, b"ps5upload") || memmem_ascii(window, b"PS5UPLOAD");
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

