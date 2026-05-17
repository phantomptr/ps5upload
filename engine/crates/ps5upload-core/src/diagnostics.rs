//! Diagnostics RPCs — kernel log, network interfaces, peripheral
//! control, module enumeration. All read-only or single-action; none
//! affect the payload's transfer/install pipelines.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Read up to `max_bytes` of currently-buffered kernel log. Empty
/// response = nothing in the buffer (poll again later).
pub fn klog_read(addr: &str, max_bytes: u32) -> Result<String> {
    let body = serde_json::json!({ "max_bytes": max_bytes });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::KlogRead, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected KLOG_READ: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::KlogReadAck {
        bail!("expected KLOG_READ_ACK, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::NetInterfaces, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected NET_INTERFACES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::NetInterfacesAck {
        bail!("expected NET_INTERFACES_ACK, got {ft:?}");
    }
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
        },
        "port": port,
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PeripheralControl, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PERIPHERAL_CONTROL: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PeripheralControlAck {
        bail!("expected PERIPHERAL_CONTROL_ACK, got {ft:?}");
    }
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

/// Run a shell command on the PS5. `timeout_secs` clamps the
/// payload-side wait. stdout (with stderr merged) capped at 256 KB.
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ShellExec, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected SHELL_RUN: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ShellExecAck {
        bail!("expected SHELL_RUN ack, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::Crc32File, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected CRC32_FILE: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::Crc32FileAck {
        bail!("expected CRC32_FILE_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

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
}

/// Query app.db for the title_id ↔ app_id ↔ name mapping.
pub fn appdb_query(addr: &str) -> Result<AppDbList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AppDbQuery, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APPDB_QUERY: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppDbQueryAck {
        bail!("expected APPDB_QUERY_ACK, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PkgDirectMount, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PKG_DIRECT_MOUNT: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PkgDirectMountAck {
        bail!("expected PKG_DIRECT_MOUNT_ACK, got {ft:?}");
    }
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
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let body = serde_json::json!({
        "path": path,
        "bytes": b64,
        "mode": if create_only { "create" } else { "overwrite" },
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::FsWriteBytes, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_WRITE_BYTES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsWriteBytesAck {
        bail!("expected FS_WRITE_BYTES_ACK, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::LwfsMount, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LWFS_MOUNT: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::LwfsMountAck {
        bail!("expected LWFS_MOUNT_ACK, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::UfsFsck, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected UFS_FSCK: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::UfsFsckAck {
        bail!("expected UFS_FSCK_ACK, got {ft:?}");
    }
    let parsed: UfsFsckResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "UFS_FSCK failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

/// Measure round-trip latency to the payload by issuing N empty
/// NetSpeedTest frames and timing each ACK.
pub fn net_speed_test(addr: &str, round_trips: u32) -> Result<NetSpeedTestResult> {
    let n = round_trips.clamp(1, 2048);
    let mut latencies_us: Vec<u64> = Vec::with_capacity(n as usize);
    let mut c = Connection::connect(addr)?;
    let started = std::time::Instant::now();
    for _ in 0..n {
        let t0 = std::time::Instant::now();
        c.send_frame(FrameType::NetSpeedTest, &[])?;
        let (hdr, _resp) = c.recv_frame()?;
        let ft = hdr.frame_type().unwrap_or(FrameType::Error);
        if ft != FrameType::NetSpeedTestAck {
            bail!("expected NET_SPEED_TEST_ACK, got {ft:?}");
        }
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

pub fn proc_modules(addr: &str, pid: i32) -> Result<ModuleList> {
    let body = serde_json::json!({ "pid": pid });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ProcModules, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PROC_MODULES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ProcModulesAck {
        bail!("expected PROC_MODULES_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}
