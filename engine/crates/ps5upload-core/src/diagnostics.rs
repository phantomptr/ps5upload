//! Diagnostics RPCs — kernel log, network interfaces, peripheral
//! control, module enumeration. All read-only or single-action; none
//! affect the payload's transfer/install pipelines.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

/// Read up to `max_bytes` of currently-buffered kernel log. Empty
/// response = nothing in the buffer (poll again later).
///
/// DESTRUCTIVE: `/dev/klog` is a consuming read, so whatever this returns is
/// gone from the console's buffer. A caller that reads a "baseline" before an
/// operation therefore DELETES the very history it wanted to diff against —
/// measured while chasing a launch crash, where a baseline read made the next
/// capture come back empty and the run read as "no crash". Read once, after
/// the thing you care about, and keep the text.
pub fn klog_read(addr: &str, max_bytes: u32) -> Result<String> {
    let body = serde_json::json!({ "max_bytes": max_bytes });
    let body = serde_json::to_vec(&body)?;
    // One reply carries at most the newest ~56 KiB; a console that had more says so in the text.
    let resp = mgmt::call(addr, m::LOG_KLOG, &body)?;
    Ok(String::from_utf8_lossy(&resp).into_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetInterface {
    pub name: String,
    pub mac: String,
    pub ipv4: String,
    pub mtu: u32,
    /// Raw flags from sceNetGetIfList. Bit set carries link state +
    /// other Sony-internal flags.
    pub flags: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetInterfaceList {
    #[serde(default)]
    pub interfaces: Vec<NetInterface>,
    #[serde(default)]
    pub err: Option<String>,
}

pub fn net_interfaces(addr: &str) -> Result<NetInterfaceList> {
    let resp = mgmt::call(addr, m::NET_INTERFACES, &[])?;
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeripheralAction {
    BdPowerOff,
    BdPowerOn,
    EjectDisc,
    UsbPortOff,
    UsbPortOn,
    /// Sound the console's beeper once; `port` is the pattern (0-3).
    Beep,
    /// Turn the front LED off.
    LedOff,
    /// Hand the front LED back to the console.
    LedOn,
    /// Front LED brightness; `port` is the level (0-2).
    LedDim,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeripheralAck {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub port: Option<i32>,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub err: Option<String>,
}

/// The longest a `shell.exec` call may take (AVA1 plan: bounded to 10 s).
pub const SHELL_MAX_SECS: u64 = 10;

/// Send a peripheral-control action. `port` only matters for
/// `UsbPortOff` / `UsbPortOn`; ignored otherwise.
pub fn peripheral_control(
    addr: &str,
    action: PeripheralAction,
    port: i32,
) -> Result<PeripheralAck> {
    let body = serde_json::json!({
        "action": match action {
            PeripheralAction::BdPowerOff => "bd_power_off",
            PeripheralAction::BdPowerOn => "bd_power_on",
            PeripheralAction::EjectDisc => "eject_disc",
            PeripheralAction::UsbPortOff => "usb_port_off",
            PeripheralAction::UsbPortOn => "usb_port_on",
            PeripheralAction::Beep => "beep",
            PeripheralAction::LedOff => "led_off",
            PeripheralAction::LedOn => "led_on",
            PeripheralAction::LedDim => "led_dim",
        },
        "port": port,
    });
    let body = serde_json::to_vec(&body)?;
    let resp = mgmt::call_keep(addr, m::PERIPH_CONTROL, "PERIPHERAL_CONTROL", &body)?;
    let parsed: PeripheralAck = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "PERIPHERAL_CONTROL failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    pub handle: i32,
    pub name: String,
    /// Base address as a hex string ("0x7f1234..."). String form
    /// because JSON numbers can't represent u64 across all renderers
    /// (JS Number is f64, can lose precision past 2^53).
    pub base: String,
    pub code_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleList {
    #[serde(default)]
    pub modules: Vec<ModuleInfo>,
    #[serde(default)]
    pub err: Option<String>,
    /// How many modules the console listed. `refused` of them have no row: Sony would not
    /// describe them, and `refused_rc` is what it said. An older helper sends none of these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_rc: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellRunResult {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Run a shell command on the PS5. `timeout_secs` bounds how long the engine waits for the answer
/// (at most `SHELL_MAX_SECS`); the helper cannot stop a built-in that is running and ignores it.
/// stdout (with stderr merged) capped at 256 KB.
pub fn shell_run(
    addr: &str,
    cmd: &str,
    session_id: Option<&str>,
    cwd: Option<&str>,
    timeout_secs: u32,
) -> Result<ShellRunResult> {
    let cwd = cwd.filter(|v| v.starts_with('/')).unwrap_or("/");
    let body = serde_json::json!({
        "cmd": cmd,
        "session_id": session_id.unwrap_or("default"),
        "cwd": cwd,
        "timeout_secs": timeout_secs,
    });
    let body = serde_json::to_vec(&body)?;
    // Bounded: the call's deadline is the shell's own timeout, never more than 10 s (the payload runs
    // an allowlisted built-in only and answers at most 32 KiB, with "truncated":true past that).
    let deadline = std::time::Duration::from_secs(u64::from(timeout_secs).clamp(1, SHELL_MAX_SECS));
    let resp = mgmt::keep_body(mgmt::call_with(
        addr,
        m::SHELL_EXEC,
        "SHELL_RUN",
        &body,
        Some(deadline),
    ))?;
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Crc32FileResult {
    #[serde(default)]
    pub crc32: Option<u32>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Compute the CRC32 of a file on the PS5. Cheap; for cryptographic
/// strength use FsHash (BLAKE3) instead.
pub fn crc32_file(addr: &str, path: &str) -> Result<Crc32FileResult> {
    let body = serde_json::json!({ "path": path });
    let body = serde_json::to_vec(&body)?;
    // A job over AVA1: checksumming a large file takes longer than a request deadline.
    // The deadline is the old read timeout times ten (a 64 GiB image at ~100 MB/s).
    match mgmt::run_op(
        addr,
        mgmt::ops::CRC32,
        "CRC32_FILE",
        &body,
        &mgmt::JobCall {
            op_id: 0,
            subject: path,
            deadline: CRC32_DEADLINE,
        },
    ) {
        Ok(resp) => Ok(serde_json::from_slice(&resp)?),
        // The legacy handler answered a failure as a normal ack carrying `err`; keep that shape
        // for a job the console ran and could not finish (a missing file, a refused path).
        Err(e) => match e.downcast_ref::<mgmt::MgmtError>() {
            Some(m) if m.status != 0 => Ok(Crc32FileResult {
                crc32: None,
                size: None,
                err: Some(m.cause.clone()),
            }),
            _ => Err(e),
        },
    }
}

/// How long [`crc32_file`] waits for the console to finish.
const CRC32_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDbEntry {
    pub title_id: String,
    pub app_id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDbList {
    #[serde(default)]
    pub apps: Vec<AppDbEntry>,
    #[serde(default)]
    pub err: Option<String>,
    /// Which reader answered: `sqlite`, `sqlite-immutable` or `scan`.
    ///
    /// Worth surfacing rather than hiding. `scan` means the SQL open
    /// failed and the rows came from the byte-level b-tree reader, which
    /// recovers title ids reliably but display names only sometimes — so
    /// a caller that sees a blank name knows whether to believe it.
    #[serde(default)]
    pub source: Option<String>,
    /// One page's window (#473): the first row it holds, the row the next page starts at, and
    /// whether one follows. Absent from an older helper, which answers one unpaged reply, and
    /// from the list [`appdb_query`] returns, which is every page joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<bool>,
    /// The table's row count when the page was read (SQL reader only). A change between pages
    /// means rows moved under the walk, so one may have been skipped: the list is truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// True when app.db holds rows this list does not: the helper could not deliver them, a
    /// page failed (`err` says why), the page bound was reached, or an older helper answered
    /// a full 256 rows (its old cap, which it could not report). A truncated list is never
    /// proof that a title is absent.
    #[serde(default)]
    pub truncated: bool,
}

/// The most app.db pages [`appdb_query`] reads. A page holds up to 512 rows, so this is
/// far past any real console; it bounds the loop against a helper that never says it is done.
pub const APPDB_MAX_PAGES: usize = 32;

/// The row count an older, unpaged helper stopped at without saying so.
const APPDB_LEGACY_CAP: usize = 256;

/// Query app.db for the title_id ↔ app_id ↔ name mapping: every page of it (#473).
///
/// The helper answers a window (`{"offset":N}` in; `next`, `more` and `truncated` out); this
/// follows it until `more` is false, at most [`APPDB_MAX_PAGES`] calls. An older helper ignores
/// the window and answers once, which is then the whole answer. Whatever stops the walk early
/// sets `truncated` on the result, so callers can tell a partial list from a complete one:
/// a failure after the first page keeps the rows read so far, with the reason in `err`.
pub fn appdb_query(addr: &str) -> Result<AppDbList> {
    let mut list: Option<AppDbList> = None;
    let mut offset: u64 = 0;
    for _ in 0..APPDB_MAX_PAGES {
        let body = serde_json::json!({ "offset": offset }).to_string();
        let page = mgmt::call(addr, m::APP_DB_QUERY, body.as_bytes())
            .and_then(|resp| Ok(serde_json::from_slice::<AppDbList>(&resp)?));
        let mut page = match (page, list.as_mut()) {
            (Ok(p), _) => p,
            // Page one failing is the call failing, as it always was.
            (Err(e), None) => return Err(e),
            (Err(e), Some(l)) => {
                l.truncated = true;
                l.err = Some(format!("app.db page at row {offset}: {e:#}"));
                break;
            }
        };
        let paged = page.more.is_some() || page.next.is_some();
        let more = page.more.unwrap_or(false);
        let next = page.next.unwrap_or(offset + page.apps.len() as u64);
        let got = page.apps.len();
        let total_moved = match (list.as_ref().and_then(|l| l.total), page.total) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let page_truncated = page.truncated
            || total_moved
            || (!paged && got >= APPDB_LEGACY_CAP)
            // A page that claims more but does not move forward would loop.
            || (more && next <= offset);
        match list.as_mut() {
            None => {
                page.truncated = page_truncated;
                list = Some(page);
            }
            Some(l) => {
                l.apps.append(&mut page.apps);
                l.truncated |= page_truncated;
                if page.err.is_some() {
                    l.err = page.err.take();
                    l.truncated = true;
                }
            }
        }
        let l = list.as_mut().expect("set above");
        if !paged || !more || next <= offset || l.err.is_some() {
            l.offset = None;
            l.next = None;
            l.more = None;
            l.total = None;
            return Ok(list.expect("set above"));
        }
        offset = next;
    }
    // Here only when a later page failed or the page bound ran out with pages still to read.
    let mut l = list.expect("APPDB_MAX_PAGES > 0");
    l.truncated = true;
    l.offset = None;
    l.next = None;
    l.more = None;
    l.total = None;
    Ok(l)
}

/// One (key, value) row of appinfo.db for a title.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfoRow {
    pub key: String,
    #[serde(default)]
    pub val: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfoRows {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub rows: Vec<AppInfoRow>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Read per-title metadata out of appinfo.db, the database behind
/// Settings → Storage.
///
/// `keys` filters to a comma-separated set; `None` returns every key.
pub fn appinfo_query(addr: &str, title_id: &str, keys: Option<&str>) -> Result<AppInfoRows> {
    let body = serde_json::json!({
        "title_id": title_id,
        "keys": keys.unwrap_or(""),
    });
    // `{"ok":false,"error":..}` is a reply the caller decodes (AppInfoRows.error): see call_legacy_body.
    let resp = mgmt::call_legacy_body(
        addr,
        m::APP_INFO_QUERY,
        "APPINFO_QUERY",
        body.to_string().as_bytes(),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

/// The helper's answer to `app.icon_set` / `app.hidden_list` (#407).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HiddenIcons {
    #[serde(default)]
    pub ok: bool,
    /// Title ids whose home-screen icon is hidden in any user's icon table.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Why it failed (`err` from icon_set, `error` from hidden_list).
    #[serde(default, alias = "err")]
    pub error: Option<String>,
}

/// The titles whose home-screen icon is hidden (app.db `visible = 0`).
pub fn hidden_icons(addr: &str) -> Result<HiddenIcons> {
    let resp = mgmt::call_legacy_ok(addr, m::APP_HIDDEN_LIST, "APP_HIDDEN_LIST", &[])?;
    Ok(serde_json::from_slice(&resp)?)
}

/// Hide (`hidden = true`) or show a title's home-screen icon. Only app.db's `visible` flag
/// changes; the helper backs app.db up before its first change and refuses system apps.
pub fn set_icon_hidden(addr: &str, title_id: &str, hidden: bool) -> Result<HiddenIcons> {
    let body = serde_json::json!({
        "title_id": title_id,
        "hidden": if hidden { "1" } else { "0" },
    });
    let resp = mgmt::call_legacy_ok(
        addr,
        m::APP_ICON_SET,
        "APP_ICON_SET",
        body.to_string().as_bytes(),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetSpeedTestResult {
    pub round_trips: u32,
    pub elapsed_ms: u64,
    pub avg_rtt_us: u64,
    pub p50_rtt_us: u64,
    pub p95_rtt_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgDirectMountResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub mount_point: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Mount a `.pkg` file directly via sceFsMountGamePkg. Faster than
/// the full BGFT install for testing patches; doesn't register the
/// title in the home screen. The mount point defaults to
/// `/mnt/ps5upload/<pkg-basename>.mount` when not specified.
pub fn pkg_direct_mount(
    addr: &str,
    pkg_path: &str,
    mount_point: Option<&str>,
) -> Result<PkgDirectMountResult> {
    let body = serde_json::json!({
        "pkg_path": pkg_path,
        "mount_point": mount_point.unwrap_or(""),
    });
    let body = serde_json::to_vec(&body)?;
    // A failed mount is an error status whose cause is the failure body (code, mount_point):
    // `call_legacy_ok` gives it back as the `{"ok":false,...}` this function reports on.
    let resp = mgmt::call_legacy_ok(addr, m::FS_MOUNT_PKG, "PKG_DIRECT_MOUNT", &body)?;
    let parsed: PkgDirectMountResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "PKG_DIRECT_MOUNT failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UfsFsckResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub repair: bool,
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteBytesResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Atomic small-file write (≤256 KB) via tmp+rename. `mode="create"`
/// rejects pre-existing paths; default `"overwrite"` replaces them.
/// For files larger than 256 KB, use the regular transfer pipeline.
pub fn fs_write_bytes(
    addr: &str,
    path: &str,
    bytes: &[u8],
    create_only: bool,
) -> Result<WriteBytesResult> {
    fs_write_bytes_inner(addr, path, bytes, create_only, false)
}

/// Like [`fs_write_bytes`] for files up to 16 MiB (an `icon0.png` is often 300-700 KB and a
/// `snd0.at9` a few MB). The transport sends it as 48 KiB chunks and commits once, so the
/// target never holds a partial file.
pub fn fs_write_bytes_large(
    addr: &str,
    path: &str,
    bytes: &[u8],
    create_only: bool,
) -> Result<WriteBytesResult> {
    fs_write_bytes_inner(addr, path, bytes, create_only, true)
}

fn fs_write_bytes_inner(
    addr: &str,
    path: &str,
    bytes: &[u8],
    create_only: bool,
    large: bool,
) -> Result<WriteBytesResult> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut body = serde_json::json!({
        "path": path,
        "bytes": b64,
        "mode": if create_only { "create" } else { "overwrite" },
    });
    if large {
        body["large"] = serde_json::Value::Bool(true);
    }
    let body = serde_json::to_vec(&body)?;
    // The AVA1 payload answers a refusal with an error status and the legacy token as its
    // cause; `call_legacy_ok` turns that back into the `{"ok":false,"err":..}` body this
    // function has always parsed, so the caller-visible text does not change.
    let resp = mgmt::call_legacy_ok(addr, m::FS_WRITE, "FS_WRITE_BYTES", &body)?;
    let parsed: WriteBytesResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "FS_WRITE_BYTES failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LwfsMountResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub mount_point: Option<String>,
    #[serde(default)]
    pub title_id: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Mount a LWFS patch overlay via sceFsMountLwfs. Lets a title see
/// patched files without a full reinstall. The patch_path is a
/// `.pkg` file (LWFS-format patch). The mount_point defaults to
/// `/mnt/ps5upload/<basename>.lwfs` when not specified.
pub fn lwfs_mount(
    addr: &str,
    patch_path: &str,
    mount_point: Option<&str>,
    title_id: Option<&str>,
) -> Result<LwfsMountResult> {
    let body = serde_json::json!({
        "patch_path": patch_path,
        "mount_point": mount_point.unwrap_or(""),
        "title_id": title_id.unwrap_or(""),
    });
    let body = serde_json::to_vec(&body)?;
    let resp = mgmt::call_legacy_ok(addr, m::FS_MOUNT_LWFS, "LWFS_MOUNT", &body)?;
    let parsed: LwfsMountResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "LWFS_MOUNT failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

/// Run sceFsUfsFsck against a UFS device. `repair=false` is
/// read-only check; `true` attempts repair (caller should
/// confirm before sending true).
pub fn ufs_fsck(addr: &str, device: &str, repair: bool) -> Result<UfsFsckResult> {
    let body = serde_json::json!({
        "device": device,
        "repair": repair,
    });
    let body = serde_json::to_vec(&body)?;
    // fsck of a big volume runs for minutes: a job, polled (its reply is the same JSON).
    let resp = mgmt::run_op(
        addr,
        mgmt::ops::FSCK,
        "UFS_FSCK",
        &body,
        &mgmt::JobCall {
            op_id: 0,
            subject: device,
            deadline: FSCK_DEADLINE,
        },
    )?;
    let parsed: UfsFsckResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "UFS_FSCK failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

/// How long [`ufs_fsck`] waits for the console to finish.
const FSCK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Measure round-trip latency to the payload by issuing N empty
/// NetSpeedTest frames and timing each ACK.
pub fn net_speed_test(addr: &str, round_trips: u32) -> Result<NetSpeedTestResult> {
    let n = round_trips.clamp(1, 2048);
    let mut latencies_us: Vec<u64> = Vec::with_capacity(n as usize);
    let started = std::time::Instant::now();
    for _ in 0..n {
        let t0 = std::time::Instant::now();
        mgmt::call(addr, m::NET_SPEEDTEST, &[])?;
        latencies_us.push(t0.elapsed().as_micros() as u64);
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    latencies_us.sort_unstable();
    let avg = latencies_us.iter().sum::<u64>() / latencies_us.len().max(1) as u64;
    let p50 = latencies_us[latencies_us.len() / 2];
    let p95_idx = (latencies_us.len() as f64 * 0.95) as usize;
    let p95 = latencies_us[p95_idx.min(latencies_us.len() - 1)];
    Ok(NetSpeedTestResult {
        round_trips: n,
        elapsed_ms,
        avg_rtt_us: avg,
        p50_rtt_us: p50,
        p95_rtt_us: p95,
    })
}

/// Whether the console could open a TCP connection to `host:port`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct NetReach {
    pub ok: bool,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub errno: i32,
    #[serde(default)]
    pub err: String,
    #[serde(default)]
    pub ms: u64,
}

/// Ask the console (mgmt `addr`) to connect to `host:port`. A stream install
/// needs the PS5 to reach this engine; this finds out in seconds instead of
/// waiting out Sony's 30 s download timeout. An `Err` means the helper could
/// not be asked (an older helper answers an unknown frame with an error), not
/// that the host is unreachable.
pub fn net_reach(addr: &str, host: &str, port: u16, timeout_ms: u32) -> Result<NetReach> {
    let body = serde_json::json!({
        "host": host,
        "port": port.to_string(),
        "timeout_ms": timeout_ms.to_string(),
    })
    .to_string();
    // The probe may wait `timeout_ms` for the connect; the call's own deadline is longer.
    let deadline =
        std::time::Duration::from_millis(u64::from(timeout_ms.clamp(100, 15_000)) + 5_000);
    let resp = mgmt::call_with(
        addr,
        m::NET_REACH,
        m::NET_REACH.label,
        body.as_bytes(),
        Some(deadline),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn proc_modules(addr: &str, pid: i32) -> Result<ModuleList> {
    let body = serde_json::json!({ "pid": pid });
    let body = serde_json::to_vec(&body)?;
    let resp = mgmt::call(addr, m::PROC_MODULES, &body)?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload answers an unreadable app.db with `err` set and `apps`
    /// empty. This pins the cross-language shape, because getting it wrong
    /// is silent: serde fills both missing fields with defaults, so the
    /// generic `{"ok":false,"error":".."}` envelope decodes as "no error,
    /// zero titles" — and `pkg_install::appdb_has_title` reads that as a
    /// confident "the title is not installed", failing verification for an
    /// install that actually worked.
    #[test]
    fn unreadable_appdb_decodes_as_an_error_not_an_empty_library() {
        let list: AppDbList =
            serde_json::from_str(r#"{"err":"cannot read app.db","apps":[]}"#).unwrap();
        assert!(list.err.is_some(), "err must survive the round trip");
        assert!(list.apps.is_empty());
    }

    #[test]
    fn a_successful_list_reports_which_reader_answered() {
        let list: AppDbList = serde_json::from_str(
            r#"{"apps":[{"title_id":"CUSA00900","app_id":101,"name":"Bloodborne"}],
                "source":"sqlite"}"#,
        )
        .unwrap();
        assert!(list.err.is_none());
        assert_eq!(list.source.as_deref(), Some("sqlite"));
        assert_eq!(list.apps[0].name, "Bloodborne");
    }

    /// A payload older than this change sends no `source` at all; it must
    /// still parse rather than failing the whole response.
    #[test]
    fn an_older_payload_without_source_still_parses() {
        let list: AppDbList =
            serde_json::from_str(r#"{"apps":[{"title_id":"CUSA00900","app_id":0,"name":"x"}]}"#)
                .unwrap();
        assert_eq!(list.source, None);
        assert_eq!(list.apps.len(), 1);
    }

    #[test]
    fn appinfo_rows_decode_with_their_key_value_pairs() {
        let rows: AppInfoRows = serde_json::from_str(
            r#"{"ok":true,"title_id":"PPSA01650","rows":[
                 {"key":"CONTENT_VERSION","val":"01.00"}],"source":"sqlite"}"#,
        )
        .unwrap();
        assert!(rows.ok);
        assert_eq!(rows.rows[0].key, "CONTENT_VERSION");
        assert_eq!(rows.rows[0].val, "01.00");
    }
}

#[cfg(test)]
mod net_reach_live {
    /// Hardware check: `PS5UPLOAD_LIVE_REACH=<console mgmt addr>,<host>,<port>
    /// cargo test -p ps5upload-core net_reach_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_reach() {
        let Ok(spec) = std::env::var("PS5UPLOAD_LIVE_REACH") else {
            return;
        };
        let parts: Vec<&str> = spec.split(',').collect();
        let r = super::net_reach(parts[0], parts[1], parts[2].parse().unwrap(), 3000).unwrap();
        println!("REACH {}:{} -> {r:?}", parts[1], parts[2]);
    }
}

#[cfg(test)]
mod hidden_icons_tests {
    use super::HiddenIcons;

    #[test]
    fn both_replies_decode_with_their_reason() {
        let ok: HiddenIcons =
            serde_json::from_str(r#"{"ok":true,"hidden":["ITEM00001"]}"#).unwrap();
        assert!(ok.ok && ok.hidden == ["ITEM00001"]);
        let set_err: HiddenIcons =
            serde_json::from_str(r#"{"ok":false,"err":"NPXS40000 is a system app"}"#).unwrap();
        assert!(!set_err.ok);
        assert_eq!(set_err.error.as_deref(), Some("NPXS40000 is a system app"));
        let list_err: HiddenIcons =
            serde_json::from_str(r#"{"ok":false,"error":"cannot read app.db"}"#).unwrap();
        assert_eq!(list_err.error.as_deref(), Some("cannot read app.db"));
        let done: HiddenIcons = serde_json::from_str(r#"{"ok":true,"err":null}"#).unwrap();
        assert!(done.ok && done.error.is_none());
    }
}

/// One port held on the console (`net.ports`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsolePort {
    pub port: u16,
    /// `tcp` or `udp`.
    pub proto: String,
    /// `all` (reachable from the network), `console` (127.0.0.1 only), `lan` (the LAN address
    /// only: most often an outgoing connection) or `busy` (held; where was not checked).
    pub scope: String,
    /// What usually listens there, when the port is a well-known one; a guess by number only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// For a well-known port, whether it serves a web page. `false` keeps the app from pointing
    /// a browser at a loader or the helper (a stray HTTP request is not what they expect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<bool>,
}

/// The helper's answer to `net.ports`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolePorts {
    #[serde(default)]
    pub ports: Vec<ConsolePort>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub truncated: bool,
}

/// Who usually listens on a port, by number alone (payload defaults and Sony's own services).
pub fn known_port_name(port: u16, proto: &str) -> Option<&'static str> {
    use crate::payload_lifecycle::{INSTALLER_PORT, PS5_LOADER_PORT};
    let tcp = proto == "tcp";
    Some(match port {
        p if p == PS5_LOADER_PORT && tcp => "elfldr (payload loader)",
        // The helper's AVA1 port (ava1::gen::DEFAULT_PORT; this crate does not link ava1).
        9120 if tcp => "ps5upload helper",
        p if p == INSTALLER_PORT && tcp => "ps5upload installer",
        p if p == crate::payload_manager::PAYLOAD_MANAGER_PORT && tcp => "Payload Manager",
        p if p == crate::rp_regist::REGIST_PORT => "Remote Play",
        p if p == crate::ddp::DDP_PORT && !tcp => "PS5 discovery",
        53 if !tcp => "nanodns (DNS)",
        2121 if tcp => "ftpsrv (FTP)",
        2159 if tcp => "gdbsrv (debugger)",
        5353 if !tcp => "mDNS",
        8080 if tcp => "websrv",
        8090 if tcp => "HearBridge",
        9180 if tcp => "PS5Upload Minion",
        12800 if tcp => "DPI installer bridge",
        _ => return None,
    })
}

/// Whether a well-known port serves a web page (Payload Manager's UI, websrv, HearBridge,
/// the helper's PS5Upload Minion page).
pub fn known_port_is_web(port: u16) -> bool {
    port == crate::payload_manager::PAYLOAD_MANAGER_PORT || matches!(port, 8080 | 8090 | 9180)
}

/// The ports held on the console. The helper finds them by binding each port and closing it
/// at once, never by connecting, so no payload sees a client. `addr` is `host:port` of the
/// helper; its IPv4 address is what the LAN check binds.
pub fn console_ports(addr: &str) -> Result<ConsolePorts> {
    // A bare host (no port) resolves with a placeholder one.
    let lan = crate::net::resolve_connect_targets(addr)
        .or_else(|_| crate::net::resolve_connect_targets(&format!("{addr}:0")))
        .ok()
        .and_then(|v| v.into_iter().find(|sa| sa.is_ipv4()))
        .map(|sa| sa.ip().to_string())
        .unwrap_or_default();
    let body = serde_json::json!({ "lan": lan }).to_string();
    let resp = mgmt::call(addr, m::NET_PORTS, body.as_bytes())?;
    let mut out: ConsolePorts = serde_json::from_slice(&resp)?;
    for p in &mut out.ports {
        p.name = known_port_name(p.port, &p.proto).map(str::to_string);
        p.web = p.name.as_ref().map(|_| known_port_is_web(p.port));
    }
    Ok(out)
}

#[cfg(test)]
mod console_ports_tests {
    use super::*;

    #[test]
    fn helper_answer_parses_and_known_ports_are_named() {
        let raw = br#"{"ok":true,"ports":[{"port":9021,"proto":"tcp","scope":"all"},{"port":53,"proto":"udp","scope":"all"},{"port":41800,"proto":"tcp","scope":"console"}],"elapsed_ms":885}"#;
        let r: ConsolePorts = serde_json::from_slice(raw).unwrap();
        assert_eq!(r.ports.len(), 3);
        assert_eq!(r.elapsed_ms, 885);
        assert!(!r.truncated);
        assert_eq!(
            known_port_name(9021, "tcp"),
            Some("elfldr (payload loader)")
        );
        assert_eq!(known_port_name(53, "udp"), Some("nanodns (DNS)"));
        // By number and protocol: DNS over TCP on 53 is not nanodns.
        assert_eq!(known_port_name(53, "tcp"), None);
        assert_eq!(known_port_name(41800, "tcp"), None);
        assert!(known_port_is_web(8084));
        assert!(!known_port_is_web(9021));
    }
}
