//! Hardware monitoring over FTX2.
//!
//! Three RPCs — HW_INFO / GET_TEMPS / GET_POWER_INFO:
//!   - [`hw_info`] returns static info (model, serial, OS, RAM, CPU count).
//!     Cached forever on the payload side since it never changes.
//!   - [`hw_temps`] returns live CPU/SoC temperature, CPU frequency, and
//!     SoC power draw. 1-second payload-side cache, 5-second throttle on
//!     the power API specifically.
//!   - [`hw_power`] returns uptime derived from `kern.boottime`.
//!
//! Wire format is newline-separated `key=value` text (not JSON) to keep
//! the payload parser trivial. This module turns that into typed Rust
//! structs so the engine HTTP surface can return clean JSON to clients.
//!
//! Browser launch ([`app_launch_browser`]) fits here because it shares
//! the Sony-API theme even if it's not strictly monitoring.
use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Static hardware info. `physmem` is in bytes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwInfo {
    pub model: String,
    pub serial: String,
    pub has_wlan_bt: bool,
    pub has_optical_out: bool,
    pub hw_model: String,
    pub hw_machine: String,
    pub os: String,
    pub ncpu: u32,
    pub physmem: u64,
    /// Sony's packed firmware version word, read from kernel data
    /// memory via the SDK's `kernel_get_fw_version()` helper. 0 when
    /// kernel R/W is unavailable on the running payload (no kstuff
    /// loaded, or payload doesn't have the required perms) — in that
    /// case fall back to the kern.version string parse on the
    /// desktop side. Precise enough to distinguish FW points within
    /// a family (e.g. 9.60 vs 9.6010).
    #[serde(default)]
    pub kernel_fw_version: u32,
}

/// Live sensor readings. Values of 0 mean "API not available on this
/// firmware" rather than "actually zero" -- there's no thermal state
/// where any of these are legitimately 0 during normal operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwTemps {
    pub cpu_temp: i32,
    pub soc_temp: i32,
    pub cpu_freq_mhz: i64,
    pub soc_clock_mhz: i64,
    pub soc_power_mw: u32,
    /// Average CPU usage across cores, 0–100 %. `-1` = unavailable (the
    /// API isn't exported on this firmware, or an older payload that
    /// doesn't send the field). Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub cpu_usage_pct: i32,
    /// Current fan duty, 0–100 % (approximate — the raw scale is
    /// firmware-dependent). `-1` = unavailable. Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub fan_duty_pct: i32,
    /// Raw Sony "basic product shape" code (distinguishes hardware
    /// families). `-1` = unavailable. The desktop maps known codes to a
    /// label and otherwise shows the number; the model string remains the
    /// primary identifier. Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub product_shape: i32,
}

fn default_sensor_unavailable() -> i32 {
    -1
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwPower {
    pub operating_time_sec: u64,
    pub operating_time_hours: u64,
    pub operating_time_minutes: u64,
    pub boot_count: u64,
    pub power_consumption_mw: u32,
    /// System load average (1, 5, 15 minute windows) via the SDK's
    /// `getloadavg()` — added to prospero-libc in the 2026-05
    /// ps5-payload-dev/sdk update. Stored as fractional values
    /// (e.g. 0.42 = 42% of one core average); the payload sends
    /// integer centi-units on the wire and the engine divides by
    /// 100 here so consumers get a normal `f32`. Negative value
    /// means the getloadavg call failed on this firmware (the
    /// payload encodes that as -1.00).
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_1m: f32,
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_5m: f32,
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_15m: f32,
}

fn default_loadavg_unavailable() -> f32 {
    -1.0
}

/// Parse a newline-separated "key=value" body into a lookup closure.
/// Unknown keys are ignored; missing keys default per field. The
/// payload's text format has no escape rules (values are simple
/// identifiers or integers), so a naive split is sufficient.
fn parse_kv<'a>(body: &'a [u8]) -> impl Fn(&str) -> Option<&'a str> {
    let text = std::str::from_utf8(body).unwrap_or("");
    move |key: &str| -> Option<&'a str> {
        for line in text.lines() {
            if let Some(eq) = line.find('=') {
                let (k, v) = line.split_at(eq);
                if k == key {
                    return Some(&v[1..]); // skip the '='
                }
            }
        }
        None
    }
}

fn round_trip(addr: &str, req: FrameType, ack: FrameType, label: &str) -> Result<Vec<u8>> {
    round_trip_body(addr, req, &[], ack, label)
}

/// Like [`round_trip`] but sends a request body. Used by HW_TEMPS to pass
/// the `extended` selector ("1" = read the on-demand-only telemetry).
fn round_trip_body(
    addr: &str,
    req: FrameType,
    body: &[u8],
    ack: FrameType,
    label: &str,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(req, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {label}: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack {
        bail!("expected {ack:?}, got {ft:?}");
    }
    Ok(resp)
}

/// Static hardware info. Cached on the payload side, so cheap to call
/// repeatedly -- subsequent calls just return the already-formatted
/// text. The client Hardware tab calls this once at mount time.
pub fn hw_info(addr: &str) -> Result<HwInfo> {
    let body = round_trip(addr, FrameType::HwInfo, FrameType::HwInfoAck, "HW_INFO")?;
    let get = parse_kv(&body);
    Ok(HwInfo {
        model: get("model").unwrap_or("PlayStation 5").to_string(),
        serial: get("serial").unwrap_or("").to_string(),
        has_wlan_bt: get("has_wlan_bt") != Some("0"),
        has_optical_out: get("has_optical_out").is_some_and(|v| v != "0"),
        hw_model: get("hw_model").unwrap_or("").to_string(),
        hw_machine: get("hw_machine").unwrap_or("").to_string(),
        os: get("os").unwrap_or("").to_string(),
        ncpu: get("ncpu").and_then(|v| v.parse().ok()).unwrap_or(0),
        physmem: get("physmem").and_then(|v| v.parse().ok()).unwrap_or(0),
        kernel_fw_version: get("kernel_fw_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

/// Physical sanity bounds for live sensor values, mirrored from the
/// payload (`payload/src/hw_info.c`: `HW_TEMP_MIN_C` / `HW_TEMP_MAX_C` /
/// `HW_CPU_FREQ_MAX_MHZ`, plus the `< 500000` power clamp).
///
/// The current payload already drops out-of-range reads, but we
/// re-validate here as defense-in-depth: a console still running an
/// OLDER payload can emit garbage — e.g. `cpu_temp=26747` (a raw FreeBSD
/// thermal-zone value the old sysctl fallback mis-converted) or a
/// `cpu_freq_mhz` near 5.2e13 (an unbounded direct read on FW 5.10) —
/// and the UI must never render those. Out-of-range ⇒ 0 = "unavailable",
/// matching the payload's contract that 0 means "no usable reading".
pub const SENSOR_TEMP_MIN_C: i32 = 1;
pub const SENSOR_TEMP_MAX_C: i32 = 150;
pub const SENSOR_CPU_FREQ_MAX_MHZ: i64 = 6000;
pub const SENSOR_POWER_MAX_MW: u32 = 500_000;

/// Clamp each sensor field to its physical range, zeroing anything
/// implausible. Pure — unit-tested below.
fn sanitize_temps(mut t: HwTemps) -> HwTemps {
    if !(SENSOR_TEMP_MIN_C..=SENSOR_TEMP_MAX_C).contains(&t.cpu_temp) {
        t.cpu_temp = 0;
    }
    if !(SENSOR_TEMP_MIN_C..=SENSOR_TEMP_MAX_C).contains(&t.soc_temp) {
        t.soc_temp = 0;
    }
    if !(0..=SENSOR_CPU_FREQ_MAX_MHZ).contains(&t.cpu_freq_mhz) {
        t.cpu_freq_mhz = 0;
    }
    if !(0..=SENSOR_CPU_FREQ_MAX_MHZ).contains(&t.soc_clock_mhz) {
        t.soc_clock_mhz = 0;
    }
    if t.soc_power_mw > SENSOR_POWER_MAX_MW {
        t.soc_power_mw = 0;
    }
    // 0–100 % are the only valid percentages; anything else (incl. the -1
    // "unavailable" sentinel) is normalised to -1 so the UI shows a dash
    // rather than a bogus bar. product_shape is a raw code with no range to
    // clamp — pass it through untouched (-1 already means unavailable).
    if !(0..=100).contains(&t.cpu_usage_pct) {
        t.cpu_usage_pct = -1;
    }
    if !(0..=100).contains(&t.fan_duty_pct) {
        t.fan_duty_pct = -1;
    }
    t
}

/// Live sensor snapshot. Auto-polled every 5s by the Dashboard (and read
/// on demand from the Hardware tab). The payload reads CPU/SoC temp + CPU
/// clock via direct Sony APIs only (no ptrace, no auto-fired power read —
/// `sceKernelGetSocPowerConsumption` hangs the payload on FW 9.60, so
/// `soc_power_mw` is intentionally 0); see `payload/src/hw_info.c`. Every
/// value is re-validated through [`sanitize_temps`] so a stale payload
/// can't surface a nonsense reading.
/// `extended` selects which sensors the payload reads:
///   - `false` (BASIC): CPU/SoC temp + CPU clock only — the auto-poll-safe
///     calls the Dashboard's 5 s tick uses.
///   - `true` (EXTENDED): additionally read CPU usage, fan duty and product
///     shape (`"ufs"`). Requested only behind an explicit user action
///     ("Read sensors"), never on a timer.
///
/// SoC power is DELIBERATELY EXCLUDED. Hardware probe (2026-06): of the
/// four extended getters, `sceKernelGetSocPowerConsumption` is the ONLY one
/// that wedges the helper on FW 9.60 (Pro) — and it does so even with the
/// corrected ABI, so the hang is the API's, not ours; on the phat (5.10) it
/// returns a useless 0. Dangerous and worthless, so we never request it
/// ("p" is omitted from the selector). The payload still supports the `p`
/// flag for a future firmware probe, but no shipping path sends it.
pub fn hw_temps(addr: &str, extended: bool) -> Result<HwTemps> {
    // "ufs" = usage + fan + shape (NOT power — see fn doc). Each is
    // independently gated payload-side so this set is the verified-safe one.
    let req_body: &[u8] = if extended { b"ufs" } else { b"" };
    let body = round_trip_body(
        addr,
        FrameType::HwTemps,
        req_body,
        FrameType::HwTempsAck,
        "HW_TEMPS",
    )?;
    Ok(sanitize_temps(parse_hw_temps(&body)))
}

/// Pure parse of the `key=value` HW_TEMPS body into the typed struct,
/// split out from [`hw_temps`] so the wire-format decode is unit-testable
/// without a live payload. Bounds are applied separately by
/// [`sanitize_temps`].
fn parse_hw_temps(body: &[u8]) -> HwTemps {
    let get = parse_kv(body);
    HwTemps {
        cpu_temp: get("cpu_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        soc_temp: get("soc_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        cpu_freq_mhz: get("cpu_freq_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_clock_mhz: get("soc_clock_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_power_mw: get("soc_power_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        // Missing (older payload) ⇒ -1 = "unavailable", NOT 0 — 0% usage /
        // 0% duty are legitimate live readings and must stay distinct from
        // "this payload doesn't report it".
        cpu_usage_pct: get("cpu_usage_pct")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        fan_duty_pct: get("fan_duty_pct")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        product_shape: get("product_shape")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
    }
}

pub fn hw_power(addr: &str) -> Result<HwPower> {
    let body = round_trip(addr, FrameType::HwPower, FrameType::HwPowerAck, "HW_POWER")?;
    let get = parse_kv(&body);
    /* Wire format is integer centi-units (× 100). Divide here so
     * consumers downstream see a normal fractional value. Missing
     * key OR parse failure → -1.0 (the "unavailable" sentinel that
     * matches what the payload emits when getloadavg itself fails);
     * keeps "old payload doesn't send this field" and "new payload
     * couldn't read it" indistinguishable to the UI, which is the
     * right behavior — both render the same "unavailable" chip. */
    let la = |k: &str| -> f32 {
        get(k)
            .and_then(|v| v.parse::<i32>().ok())
            .map(|c| c as f32 / 100.0)
            .unwrap_or(-1.0)
    };
    Ok(HwPower {
        operating_time_sec: get("operating_time_sec")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        operating_time_hours: get("operating_time_hours")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        operating_time_minutes: get("operating_time_minutes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        boot_count: get("boot_count").and_then(|v| v.parse().ok()).unwrap_or(0),
        power_consumption_mw: get("power_consumption_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        load_avg_1m: la("load_avg_1m_centi"),
        load_avg_5m: la("load_avg_5m_centi"),
        load_avg_15m: la("load_avg_15m_centi"),
    })
}

/// "Console Storage" aggregate that matches what PS5 Settings shows.
/// Sums the three partitions Sony's UI counts (`/user effective +
/// /system_data + /system_ex`) and accounts for the reserved-pool slice
/// the FS keeps for root. Per-partition fields are also surfaced for
/// the diagnostics card. Distinct from `fs_list_volumes`: that returns
/// per-volume detail; this is the single-line summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwStorage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub user_total_bytes: u64,
    pub user_free_bytes: u64,
    pub user_reserved_bytes: u64,
    pub system_data_total_bytes: u64,
    pub system_data_free_bytes: u64,
    pub system_ex_total_bytes: u64,
    pub system_ex_free_bytes: u64,
}

pub fn hw_storage(addr: &str) -> Result<HwStorage> {
    let body = round_trip(
        addr,
        FrameType::HwStorage,
        FrameType::HwStorageAck,
        "HW_STORAGE",
    )?;
    let get = parse_kv(&body);
    let n = |k: &str| get(k).and_then(|v| v.parse().ok()).unwrap_or(0u64);
    Ok(HwStorage {
        total_bytes: n("total_bytes"),
        free_bytes: n("free_bytes"),
        used_bytes: n("used_bytes"),
        reserved_bytes: n("reserved_bytes"),
        user_total_bytes: n("user_total_bytes"),
        user_free_bytes: n("user_free_bytes"),
        user_reserved_bytes: n("user_reserved_bytes"),
        system_data_total_bytes: n("system_data_total_bytes"),
        system_data_free_bytes: n("system_data_free_bytes"),
        system_ex_total_bytes: n("system_ex_total_bytes"),
        system_ex_free_bytes: n("system_ex_free_bytes"),
    })
}

/// Safe bounds for fan-turbo threshold. Mirrored in the payload
/// (payload/include/hw_info.h) — the payload clamps identically, so
/// callers that bypass this function still get a safe outcome. We
/// enforce here mostly for clearer error surfacing.
pub const FAN_THRESHOLD_MIN_C: u8 = 45;
pub const FAN_THRESHOLD_MAX_C: u8 = 80;

/// Set the PS5's fan-turbo threshold in °C. Rings through to the
/// payload's `/dev/icc_fan` ioctl — the canonical fan-threshold
/// mechanism on PS5, confirmed working on firmwares 9.x–11.x.
/// Persists until the PS5 reboots.
///
/// Out-of-range inputs are rejected with a clear error here rather
/// than silently clamped, so the UI can surface "you asked for 30,
/// we'll use 45" explicitly rather than hide the behavior.
pub fn hw_set_fan_threshold(addr: &str, threshold_c: u8) -> Result<()> {
    if !(FAN_THRESHOLD_MIN_C..=FAN_THRESHOLD_MAX_C).contains(&threshold_c) {
        bail!(
            "threshold {threshold_c}°C is outside the safe range \
             {FAN_THRESHOLD_MIN_C}–{FAN_THRESHOLD_MAX_C}°C"
        );
    }
    let body = threshold_c.to_string();
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::HwSetFanThreshold, body.as_bytes())?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected HW_SET_FAN_THRESHOLD: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::HwSetFanThresholdAck {
        bail!("expected HW_SET_FAN_THRESHOLD_ACK, got {ft:?}");
    }
    Ok(())
}

/// Open the PS5's built-in web browser. Handy for self-hosted
/// payload-loader pages or custom browsing on the console.
pub fn app_launch_browser(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AppLaunchBrowser, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APP_LAUNCH_BROWSER: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppLaunchBrowserAck {
        bail!("expected APP_LAUNCH_BROWSER_ACK, got {:?}", ft);
    }
    Ok(())
}

// ─── Process listing ─────────────────────────────────────────────────────

/// One running process on the PS5. Kept minimal — PID and command name
/// is all the payload can surface cheaply from the allproc walk. Fields
/// are `String` rather than `&str` since this crosses the HTTP boundary
/// and needs to own its data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcEntry {
    pub pid: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcList {
    /// `false` when the payload can tell the walk didn't succeed
    /// (typically `kernel_rw_unavailable`). `procs` may still be
    /// partially populated if the failure happened mid-walk.
    pub ok: bool,
    /// Observed entries. Empty when `ok == false`.
    pub procs: Vec<ProcEntry>,
    /// Payload-reported truncation signal. `true` if the walk was cut
    /// short (cap hit or a follow-pointer failure).
    #[serde(default)]
    pub truncated: bool,
    /// Optional short error code when `ok == false`. UI can map this
    /// to a human string; raw codes match the `err_out` values in
    /// `payload/src/proc_list.c`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Snapshot of running PS5 processes. The payload returns JSON directly
/// so we pass it straight through after a minimal parse. Designed to be
/// cheap to call on a ~2–5 second refresh cadence — no UI surface uses
/// it today (latent capability for a future "processes" view), but the
/// endpoint exists because the injector path wants to pick targets by
/// name, which needs the same walk.
pub fn proc_list(addr: &str) -> Result<ProcList> {
    let body = round_trip(
        addr,
        FrameType::ProcList,
        FrameType::ProcListAck,
        "PROC_LIST",
    )?;
    // The payload emits a very small, flat shape — parse straight into
    // a serde_json::Value first so we can handle both the normal case
    // (with a trailing `{"truncated":true}` sentinel object in the
    // array) and the degenerate empty/error case without a bespoke
    // parser.
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("proc_list returned non-JSON: {e}"))?;

    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut truncated = false;
    let mut procs = Vec::new();
    if let Some(arr) = v.get("procs").and_then(|x| x.as_array()) {
        for item in arr {
            // The truncation sentinel is the only entry with a
            // `truncated` key. Everything else is `{pid, name}`.
            if item
                .get("truncated")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                truncated = true;
                continue;
            }
            let pid = item.get("pid").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                procs.push(ProcEntry { pid, name });
            }
        }
    }
    let error = v
        .get("error")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    Ok(ProcList {
        ok,
        procs,
        truncated,
        error,
    })
}

/// Recent PS5 kernel log lines (`dmesg`-style output). Returned verbatim
/// from `sysctl kern.msgbuf` on the PS5 — the kernel's in-memory printk/
/// printf history. Used by the desktop's "System log" panel to diagnose
/// "why didn't the payload load" / "what error happened silently" without
/// requiring the user to FTP/ssh into the console.
///
/// Up to ~1 MiB returned (payload-side HARD_CAP); can be empty if the
/// kernel rebuilt with a smaller buffer or the sysctl reset between
/// calls. Most firmwares surface 128 KiB – 256 KiB of recent kernel
/// output.
pub fn syslog_tail(addr: &str) -> Result<String> {
    let body = round_trip(
        addr,
        FrameType::SyslogTail,
        FrameType::SyslogTailAck,
        "SYSLOG_TAIL",
    )?;
    // Kernel printf output is plain ASCII; lossy decode covers any stray
    // non-UTF-8 byte (driver scribbles, raw register dumps) without
    // failing the whole panel render.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hw_info_body() {
        let body = b"model=CFI-1215A\nserial=ABC123\nhas_wlan_bt=1\nhas_optical_out=0\nhw_model=CFI-1215A\nhw_machine=amd64\nos=FreeBSD 11.0-RELEASE\nncpu=8\nphysmem=13421772800\n";
        let get = parse_kv(body);
        assert_eq!(get("model"), Some("CFI-1215A"));
        assert_eq!(get("ncpu"), Some("8"));
        assert_eq!(get("physmem"), Some("13421772800"));
    }

    #[test]
    fn parse_temps_with_zero_fallbacks() {
        let body =
            b"cpu_temp=65\nsoc_temp=72\ncpu_freq_mhz=3500\nsoc_clock_mhz=0\nsoc_power_mw=85000\n";
        let get = parse_kv(body);
        assert_eq!(get("cpu_temp"), Some("65"));
        assert_eq!(get("soc_power_mw"), Some("85000"));
    }

    #[test]
    fn parse_hw_temps_decodes_plausible_reading() {
        // A normal reading from a fixed payload (PS5 Pro 9.60): power is 0
        // by design (the direct power API hangs that firmware), everything
        // else is in range and must pass through untouched.
        let body = b"cpu_temp=36\nsoc_temp=34\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 36);
        assert_eq!(t.soc_temp, 34);
        assert_eq!(t.cpu_freq_mhz, 800);
        assert_eq!(t.soc_power_mw, 0);
    }

    #[test]
    fn sanitize_zeros_old_payload_garbage_temps() {
        // The exact garbage an OLD payload emitted on FW 5.10: the sysctl
        // thermal-zone fallback's raw value rendered as "26747 °C". Must be
        // dropped to 0 = "unavailable", never shown to the user.
        let body =
            b"cpu_temp=26747\nsoc_temp=44000\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 0, "26747 °C must be rejected");
        assert_eq!(t.soc_temp, 0, "44000 °C must be rejected");
        assert_eq!(t.cpu_freq_mhz, 800, "an in-range clock survives");
    }

    #[test]
    fn sanitize_zeros_garbage_cpu_freq() {
        // The unbounded direct cpu-freq read on FW 5.10 surfaced ~5.2e13 MHz.
        let body = b"cpu_temp=45\nsoc_temp=40\ncpu_freq_mhz=51874615006914\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 45);
        assert_eq!(t.cpu_freq_mhz, 0, "5.2e13 MHz must be rejected");
    }

    #[test]
    fn sanitize_clamps_boundary_values() {
        // Exactly-at-bound values are valid; one past is not.
        let lo = sanitize_temps(HwTemps {
            cpu_temp: SENSOR_TEMP_MIN_C,
            soc_temp: SENSOR_TEMP_MAX_C,
            cpu_freq_mhz: SENSOR_CPU_FREQ_MAX_MHZ,
            soc_clock_mhz: 0,
            soc_power_mw: SENSOR_POWER_MAX_MW,
            cpu_usage_pct: 0,
            fan_duty_pct: 100,
            product_shape: 3,
        });
        assert_eq!(lo.cpu_temp, SENSOR_TEMP_MIN_C);
        assert_eq!(lo.soc_temp, SENSOR_TEMP_MAX_C);
        assert_eq!(lo.cpu_freq_mhz, SENSOR_CPU_FREQ_MAX_MHZ);
        assert_eq!(lo.soc_power_mw, SENSOR_POWER_MAX_MW);
        assert_eq!(lo.cpu_usage_pct, 0, "0% usage is valid");
        assert_eq!(lo.fan_duty_pct, 100, "100% duty is valid");
        assert_eq!(lo.product_shape, 3, "raw product shape passes through");

        let hi = sanitize_temps(HwTemps {
            cpu_temp: SENSOR_TEMP_MAX_C + 1,
            soc_temp: 0, // 0 = "no reading"; stays 0 (it's below MIN, already unavailable)
            cpu_freq_mhz: SENSOR_CPU_FREQ_MAX_MHZ + 1,
            soc_clock_mhz: -1,
            soc_power_mw: SENSOR_POWER_MAX_MW + 1,
            cpu_usage_pct: 101,
            fan_duty_pct: -5,
            product_shape: -1,
        });
        assert_eq!(hi.cpu_temp, 0, "one above max temp is rejected");
        assert_eq!(hi.soc_temp, 0);
        assert_eq!(hi.cpu_freq_mhz, 0, "one above max freq is rejected");
        assert_eq!(hi.soc_clock_mhz, 0, "negative clock is rejected");
        assert_eq!(hi.soc_power_mw, 0, "one above max power is rejected");
        assert_eq!(hi.cpu_usage_pct, -1, "101% usage → unavailable");
        assert_eq!(hi.fan_duty_pct, -1, "negative duty → unavailable");
    }

    #[test]
    fn parse_hw_temps_extended_fields() {
        // A new-payload reading that carries the extra telemetry. Power is
        // now a real value, usage/duty are valid percentages, and product
        // shape is a raw code that passes through untouched.
        let body = b"cpu_temp=42\nsoc_temp=40\ncpu_freq_mhz=2560\nsoc_clock_mhz=0\nsoc_power_mw=18000\ncpu_usage_pct=37\nfan_duty_pct=55\nproduct_shape=2\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.soc_power_mw, 18000);
        assert_eq!(t.cpu_usage_pct, 37);
        assert_eq!(t.fan_duty_pct, 55);
        assert_eq!(t.product_shape, 2);
    }

    #[test]
    fn parse_hw_temps_old_payload_omits_extended_fields() {
        // An OLDER payload sends only the original five keys. The new fields
        // must read back as -1 = "unavailable" (NOT 0, which would render as
        // a real 0% reading), so the UI shows a dash.
        let body = b"cpu_temp=36\nsoc_temp=34\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 36);
        assert_eq!(t.cpu_usage_pct, -1, "missing usage ⇒ unavailable");
        assert_eq!(t.fan_duty_pct, -1, "missing duty ⇒ unavailable");
        assert_eq!(t.product_shape, -1, "missing shape ⇒ unavailable");
    }

    #[test]
    fn sanitize_passes_unavailable_zero_through() {
        // All-zero (every sensor unavailable) is a valid state and must
        // round-trip as zeros, not be mistaken for garbage.
        let t = sanitize_temps(HwTemps::default());
        assert_eq!(t.cpu_temp, 0);
        assert_eq!(t.soc_temp, 0);
        assert_eq!(t.cpu_freq_mhz, 0);
        assert_eq!(t.soc_power_mw, 0);
    }

    #[test]
    fn parse_kv_ignores_missing_keys() {
        let body = b"cpu_temp=50\n";
        let get = parse_kv(body);
        assert_eq!(get("cpu_temp"), Some("50"));
        assert_eq!(get("unknown_key"), None);
    }

    /// Parse the post-2.2.26 sysctl-based proc_list shape into the
    /// engine struct. Mirrors the live-decoder body used by
    /// `proc_list()` so a payload-shape regression here lights up
    /// the engine immediately.
    fn parse_proc_list_body(body: &[u8]) -> ProcList {
        let v: serde_json::Value = serde_json::from_slice(body).expect("valid JSON");
        let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
        let mut truncated = false;
        let mut procs = Vec::new();
        if let Some(arr) = v.get("procs").and_then(|x| x.as_array()) {
            for item in arr {
                if item
                    .get("truncated")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false)
                {
                    truncated = true;
                    continue;
                }
                let pid = item.get("pid").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                let name = item
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    procs.push(ProcEntry { pid, name });
                }
            }
        }
        let error = v
            .get("error")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        ProcList {
            ok,
            procs,
            truncated,
            error,
        }
    }

    #[test]
    fn parse_proc_list_normal() {
        let body = br#"{"ok":true,"procs":[{"pid":97,"name":"SceShellUI"},{"pid":113,"name":"payload.elf"}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(!pl.truncated);
        assert_eq!(pl.procs.len(), 2);
        assert_eq!(pl.procs[0].pid, 97);
        assert_eq!(pl.procs[0].name, "SceShellUI");
        assert_eq!(pl.procs[1].name, "payload.elf");
        assert!(pl.error.is_none());
    }

    #[test]
    fn parse_proc_list_truncated() {
        // Payload emits a sentinel object with truncated:true when the
        // walk hit the buffer cap.
        let body = br#"{"ok":true,"procs":[{"pid":1,"name":"init"},{"truncated":true}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(pl.truncated);
        // The sentinel object isn't counted as a process — only real
        // {pid,name} entries should land in `procs`.
        assert_eq!(pl.procs.len(), 1);
    }

    #[test]
    fn parse_proc_list_empty_list() {
        // Edge case: payload emits ok:true but no entries.
        let body = br#"{"ok":true,"procs":[]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(!pl.truncated);
        assert!(pl.procs.is_empty());
    }

    #[test]
    fn parse_proc_list_error_shape() {
        // sysctl probe failed shape; engine surfaces the error code so
        // UI can map "proc_list_sysctl_size_failed" → human string.
        let body = br#"{"ok":false,"error":"proc_list_sysctl_read_failed"}"#;
        let pl = parse_proc_list_body(body);
        assert!(!pl.ok);
        assert!(pl.procs.is_empty());
        assert_eq!(pl.error.as_deref(), Some("proc_list_sysctl_read_failed"),);
    }

    #[test]
    fn parse_proc_list_skips_unnamed_entry() {
        // A kinfo_proc with a NUL-only tdname json-escapes to "".
        // Defensive: we shouldn't surface those as zero-name rows.
        let body = br#"{"ok":true,"procs":[{"pid":42,"name":""},{"pid":43,"name":"valid"}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert_eq!(pl.procs.len(), 1);
        assert_eq!(pl.procs[0].pid, 43);
    }

    #[test]
    fn fan_threshold_rejects_out_of_range() {
        // Below the safe floor: running fan at turbo during idle.
        let low = hw_set_fan_threshold("127.0.0.1:0", 30);
        assert!(low.is_err());
        let msg = format!("{}", low.unwrap_err());
        assert!(msg.contains("safe range"), "unexpected: {msg}");

        // Above the safe ceiling: too close to thermal throttle.
        let high = hw_set_fan_threshold("127.0.0.1:0", 95);
        assert!(high.is_err());
    }
}
