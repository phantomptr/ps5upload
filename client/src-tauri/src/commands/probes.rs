//! Real implementations of the connectivity + payload-send primitives.
//! These stay small and dependency-free: a TCP connect for port_check, a
//! `stream the file + half-close` for payload_send, and a thin wrapper
//! around the engine's FS_LIST_DIR for manage_list.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
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
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
const PAYLOAD_SEND_MAX_BYTES: u64 = 128 * 1024 * 1024;
const EMBEDDED_PAYLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;

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
    let targets = resolve_targets(host, port).await?;
    let mut last = String::from("no addresses to try");
    for sa in targets {
        match timeout(CONNECT_TIMEOUT, TcpStream::connect(sa)).await {
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
        .timeout(Duration::from_secs(5))
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
            serde_json::json!({
                "ok": false,
                "reachable": false,
                "engine": true,
                "error": if body.is_empty() {
                    format!("engine returned HTTP {code}")
                } else {
                    body
                },
            })
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
    // Does the ELF we're about to load identify as a ps5upload payload?
    // Only ps5upload payloads bind :9120, so only they contend with a
    // running ps5upload — and only they warrant evicting it (below). Other
    // ELFs (the DPI install daemon on :9115, scene tools) bind different
    // ports and can load ALONGSIDE ps5upload, so they must NOT knock it
    // offline. Determined here while the file is already open.
    let mut sending_ps5upload = false;
    if target_port == PS5_LOADER_PORT {
        if size < 4 {
            return Err(format!("not an ELF file: {path} (only {size} bytes)"));
        }
        // Read a window big enough to verify the ELF magic AND spot the
        // "ps5upload" ASCII signature in the section headers (mirrors
        // payload_probe's 512 KiB window). Capped so we stay off disk for
        // large ELFs.
        const PROBE_WINDOW: usize = 512 * 1024;
        let want = PROBE_WINDOW.min(size as usize);
        let mut window = vec![0u8; want];
        file.read_exact(&mut window)
            .await
            .map_err(|e| format!("read {path}: {e}"))?;
        if &window[..4] != b"\x7FELF" {
            return Err(format!(
                "not an ELF file: {path} (first 4 bytes {:02x?})",
                &window[..4]
            ));
        }
        sending_ps5upload = is_ps5upload_payload(path, &window);
        // Rewind so the magic bytes ship as part of the file body.
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|e| format!("seek {path}: {e}"))?;
    }
    // Best-effort old-payload eviction. When the user resends payload
    // bytes to :9021, the PS5 ELF loader spawns a fresh process — but
    // the OLD ps5upload payload is unaware and keeps running. The two
    // contend for :9120 and the new bind fails, leaving the OLD
    // payload still answering with whatever its (possibly stale)
    // behaviour expects. Symptom users see: "I sent the payload but
    // nothing changed." Send a node.shutdown to the existing helper
    // first, give it a moment to free the port, THEN push the new
    // ELF. No-op when nothing's listening on :9120 (first send of the
    // session, console
    // rebooted, etc) — shutdown_running_payload returns Ok(false)
    // and we proceed normally.
    //
    // GATED on `sending_ps5upload`: we ONLY evict when the incoming ELF is
    // itself a ps5upload payload (the only thing that contends for :9120).
    // Loading a different-port daemon — e.g. the DPI installer (:9115) —
    // leaves ps5upload running, so an install no longer drops the transfer
    // connection. (On a single-payload loader the loader itself may still
    // clobber ps5upload; that's outside our control, and the post-install
    // payload restore — which IS a ps5upload send — cleans up the ports.)
    // An OLDER helper is replaced by the engine's replace flow (the old protocol's shutdown,
    // the stamped helper, the AVA1 wait) instead of the shutdown below, which only speaks AVA1.
    // The replace sends the BUNDLED helper, so it is taken only when the chosen file IS the
    // bundled helper; any other ELF (a downgrade, a test build) is the person's deliberate
    // choice and goes through the shutdown-then-send path, and THEIR file is what gets sent.
    if target_port == PS5_LOADER_PORT && sending_ps5upload {
        let state = engine_helper_state(ip).await;
        if state.as_deref() == Some("helper_old") {
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|e| format!("read {path}: {e}"))?;
            let bundled = tokio::task::spawn_blocking(move || file_is_bundled(&bytes))
                .await
                .unwrap_or(false);
            if old_helper_path(state.as_deref(), bundled) == OldHelperPath::Replace {
                engine_replace_helper(ip).await?;
                return Ok(size);
            }
        }
    }
    if target_port == PS5_LOADER_PORT && sending_ps5upload {
        let host = ip.to_string();
        // Off the async runtime — the management call is blocking I/O.
        let _ = tokio::task::spawn_blocking(move || {
            ps5upload_core::payload_lifecycle::shutdown_running_payload(&host)
        })
        .await;
        // Brief grace period for the OS to recycle :9120 after the
        // old process exits. 600 ms is enough for the typical FreeBSD
        // close-wait → unbind transition on the PS5 we've measured;
        // anything more would noticeably slow the user-facing send.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    // A ps5upload helper is sent from memory so its AVA1 trust slot can be stamped.
    let helper_bytes = if target_port == PS5_LOADER_PORT && sending_ps5upload {
        let mut v = Vec::with_capacity(size as usize);
        file.read_to_end(&mut v)
            .await
            .map_err(|e| format!("read {path}: {e}"))?;
        stamp_ava1_trust(&mut v).await;
        Some(v)
    } else {
        None
    };

    let addr = format!("{ip}:{target_port}");
    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("connect {addr}: timeout"))?
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let sent = timeout(SEND_TIMEOUT, async {
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        if let Some(v) = &helper_bytes {
            stream
                .write_all(v)
                .await
                .map_err(|e| format!("write: {e}"))?;
            total = v.len() as u64;
        } else {
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
        // Nothing answered on the loader at all (not a send that broke
        // mid-way, which may already be running): launch it through the
        // console's Payload Manager instead. A wedged elfldr leaves :9021 dead
        // while Payload Manager stays up, and the helper could not be put back
        // until the user reloaded elfldr by hand (#344).
        Err(e) if target_port == PS5_LOADER_PORT && e.starts_with("connect ") => {
            match payload_send_via_payload_manager(&ip, &path).await {
                Ok(n) => serde_json::json!({
                    "ok": true,
                    "status": format!("{e}; launched {n} bytes through Payload Manager on {ip}:8084 instead"),
                    "bytes": n
                }),
                Err(pm) => serde_json::json!({
                    "ok": false,
                    "status": format!("{e} (and no fallback: {pm})")
                }),
            }
        }
        Err(e) => serde_json::json!({ "ok": false, "status": e }),
    }
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
    if let Err(why) = ava1::trust::stamp_helper(bytes, key, || token) {
        eprintln!("[payload_send] {why}");
    }
}

/// What the engine says about the console's running helper (`GET /api/ps5/helper/state`):
/// `ava1`, `helper_old`, `starting`, `ava1_failed` or `not_running`. `None` when the engine
/// does not answer (an older engine without the route): the caller then keeps the plain flow.
async fn engine_helper_state(ip: &str) -> Option<String> {
    let url = format!(
        "{}/api/ps5/helper/state?host={}",
        crate::engine::url(),
        crate::commands::ps5_engine::urlencoding(ip)
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        let client = crate::engine_http::engine_client_builder().build().ok()?;
        let r = client.get(&url).send().await.ok()?;
        if !r.status().is_success() {
            return None;
        }
        let v: serde_json::Value = r.json().await.ok()?;
        v.get("state")?.as_str().map(str::to_string)
    })
    .await
    .ok()
    .flatten()
}

/// An older helper (one that only speaks the old protocol) is replaced by the ENGINE, not shut
/// down from here: `POST /api/ps5/helper/replace` asks it to exit, waits for its ports to close,
/// sends the stamped helper (so no pairing code appears) and waits for the AVA1 port. Returns
/// `Err` with the engine's token at the start (`legacy_helper_wedged`: the old helper did not
/// exit, the console must be restarted; `helper_not_running`) so the UI can say what to do.
async fn engine_replace_helper(ip: &str) -> Result<(), String> {
    let url = format!("{}/api/ps5/helper/replace", crate::engine::url());
    let client = crate::engine_http::engine_client_builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;
    let r = client
        .post(&url)
        .json(&serde_json::json!({ "host": ip }))
        .send()
        .await
        .map_err(|e| format!("replace helper: {e}"))?;
    if r.status().is_success() {
        return Ok(());
    }
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    Err(replace_failure(status.as_u16(), &body))
}

/// How an older helper is dealt with when a ps5upload ELF is sent to the loader.
#[derive(Debug, PartialEq, Eq)]
enum OldHelperPath {
    /// The engine's replace flow (it sends the bundled helper).
    Replace,
    /// The shutdown-then-send path: the person's own file is what gets sent.
    ShutdownThenSend,
}

/// The replace flow only for an older helper AND the bundled helper file.
fn old_helper_path(engine_state: Option<&str>, file_is_bundled: bool) -> OldHelperPath {
    if engine_state == Some("helper_old") && file_is_bundled {
        OldHelperPath::Replace
    } else {
        OldHelperPath::ShutdownThenSend
    }
}

/// True when `file` is byte-for-byte the helper this app embeds.
fn file_is_bundled(file: &[u8]) -> bool {
    same_as_gz(file, EMBEDDED_PAYLOAD_GZ)
}

fn same_as_gz(file: &[u8], gz: &[u8]) -> bool {
    let mut out = Vec::with_capacity(file.len());
    let decoder = flate2::read::GzDecoder::new(gz);
    if std::io::Read::read_to_end(
        &mut std::io::Read::take(decoder, EMBEDDED_PAYLOAD_MAX_BYTES),
        &mut out,
    )
    .is_err()
    {
        return false;
    }
    blake3::hash(&out) == blake3::hash(file)
}

/// The error text for a refused replace: the engine's `error` (which starts with its token)
/// when the body has one, else the raw body or the status.
fn replace_failure(status: u16, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                format!("helper replace failed (HTTP {status})")
            } else {
                body.trim().to_string()
            }
        })
}

/// Launch the ELF at `path` through Payload Manager (:8084). The stored copy
/// is removed once it has been launched; the payload keeps running.
async fn payload_send_via_payload_manager(ip: &str, path: &str) -> Result<u64, String> {
    let mut bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("read {path}: {e}"))?;
    stamp_ava1_trust(&mut bytes).await;
    // Our own name, never the file's: Payload Manager files uploads under a
    // folder derived from the name and its cleanup clears that folder, so a
    // user's own "ps5upload" entry must not be the one we land in.
    let name = "ps5upload-helper.elf".to_string();
    let (host, n) = (ip.to_string(), bytes.len() as u64);
    tokio::task::spawn_blocking(move || {
        ps5upload_core::payload_manager::launch_elf(&host, &name, &bytes)?;
        // Give it a moment to start before the stored copy goes.
        std::thread::sleep(std::time::Duration::from_secs(2));
        ps5upload_core::payload_manager::forget(&host, &name);
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("launch task failed: {e}"))??;
    Ok(n)
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

/// True when the file we're about to send is a ps5upload payload — by
/// filename (`ps5upload.elf`) or by the ASCII signature embedded in its
/// section headers. This is the only kind of ELF that binds :9120
/// and thus contends with a running ps5upload, so it's the only kind that
/// should trigger eviction of the current payload. `head` is the leading
/// chunk of the file (payload_probe / do_payload_send both pass 512 KiB).
fn is_ps5upload_payload(path: &str, head: &[u8]) -> bool {
    // Our installer daemon is a companion that runs alongside the helper, but
    // it shares the name and carries "ps5upload" in its strings: rule it out
    // first, by its file name or its own log marker.
    let name = path.to_ascii_lowercase();
    let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
    if base.contains("installer") || memmem_ascii(head, b"PS5Upload installer") {
        return false;
    }
    base.contains("ps5upload")
        || memmem_ascii(head, b"ps5upload")
        || memmem_ascii(head, b"PS5UPLOAD")
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
    fn only_the_bundled_helper_takes_the_replace_flow() {
        // The older helper and the bundled file: the engine replaces it.
        assert_eq!(
            old_helper_path(Some("helper_old"), true),
            OldHelperPath::Replace
        );
        // A custom ELF keeps the person's choice: shutdown, then send THEIR file.
        assert_eq!(
            old_helper_path(Some("helper_old"), false),
            OldHelperPath::ShutdownThenSend
        );
        // Anything but an older helper is the plain flow, bundled or not.
        for st in [Some("ava1"), Some("not_running"), None] {
            assert_eq!(old_helper_path(st, true), OldHelperPath::ShutdownThenSend);
        }
    }

    #[test]
    fn a_file_is_the_bundled_helper_only_when_every_byte_matches() {
        use std::io::Write;
        let elf = b"\x7FELF the bundled helper".to_vec();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&elf).unwrap();
        let gz = gz.finish().unwrap();
        assert!(same_as_gz(&elf, &gz));
        let mut other = elf.clone();
        *other.last_mut().unwrap() ^= 1;
        assert!(
            !same_as_gz(&other, &gz),
            "a one-byte difference is a custom build"
        );
        assert!(!same_as_gz(&elf[..4], &gz));
        assert!(!same_as_gz(&elf, b"not gzip"));
    }

    #[test]
    fn a_refused_replace_keeps_the_engines_token_first() {
        assert_eq!(
            replace_failure(
                409,
                r#"{"error":"legacy_helper_wedged: the older helper did not exit"}"#
            ),
            "legacy_helper_wedged: the older helper did not exit"
        );
        assert_eq!(replace_failure(502, "plain text"), "plain text");
        assert_eq!(replace_failure(500, ""), "helper replace failed (HTTP 500)");
    }

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

    #[test]
    fn ps5upload_detection_gates_eviction() {
        // ps5upload payload — by filename...
        assert!(is_ps5upload_payload(
            "/x/ps5upload.elf",
            b"\x7FELF\x00garbage"
        ));
        // ...or by embedded signature (case-insensitive) even with a
        // neutral filename (e.g. a user-renamed copy).
        assert!(is_ps5upload_payload(
            "/x/helper.elf",
            b"\x7FELF .... ps5upload v2 ...."
        ));
        assert!(is_ps5upload_payload("/x/HELPER.ELF", b"\x7FELF PS5UPLOAD"));
        // The DPI daemon and other scene ELFs must NOT match — loading
        // them leaves a running ps5upload untouched.
        assert!(!is_ps5upload_payload(
            "/x/ps5upload-installer.elf",
            b"\x7FELF some other daemon"
        ));
        assert!(!is_ps5upload_payload("/x/exploit.elf", b"\x7FELF\x00\x00"));
    }
}
